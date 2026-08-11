use std::collections::HashMap;

use cpex::cpex_core::cmf::{
    AudioSource, ContentPart, ImageSource, Message, MessagePayload, PromptRequest, PromptResult,
    Resource as CmfResource, ResourceReference, ResourceType, Role, ToolCall, ToolResult,
};
use rmcp::model::{
    CallToolRequestParams, CallToolResult, ContentBlock, GetPromptRequestParams, GetPromptResult, PromptMessage,
    Resource as McpResource, ResourceContents, Role as McpRole,
};
use serde_json::{Map, Value};

pub(crate) fn tool_call_payload(
    request: &CallToolRequestParams,
    tool_name: &str,
    backend_name: &str,
    tool_call_id: &str,
) -> MessagePayload {
    MessagePayload {
        message: Message {
            schema_version: "2.0".to_owned(),
            role: Role::Assistant,
            content: vec![ContentPart::ToolCall {
                content: ToolCall {
                    tool_call_id: tool_call_id.to_owned(),
                    name: tool_name.to_owned(),
                    arguments: request.arguments.clone().unwrap_or_default().into_iter().collect(),
                    namespace: Some(backend_name.to_owned()),
                },
            }],
            channel: None,
        },
    }
}

pub(crate) fn tool_result_payload(tool_name: &str, response: &CallToolResult, tool_call_id: &str) -> MessagePayload {
    tool_json_result_payload(
        tool_name,
        serde_json::to_value(response).unwrap_or(Value::Null),
        response.is_error.unwrap_or(false),
        tool_call_id,
    )
}

pub(crate) fn tool_json_result_payload(
    tool_name: &str,
    content: Value,
    is_error: bool,
    tool_call_id: &str,
) -> MessagePayload {
    MessagePayload {
        message: Message {
            schema_version: "2.0".to_owned(),
            role: Role::Tool,
            content: vec![ContentPart::ToolResult {
                content: ToolResult {
                    tool_call_id: tool_call_id.to_owned(),
                    tool_name: tool_name.to_owned(),
                    content,
                    is_error,
                },
            }],
            channel: None,
        },
    }
}

pub(crate) fn tool_result_content(payload: &MessagePayload) -> Option<Value> {
    payload.message.get_tool_results().first().map(|tool_result| tool_result.content.clone())
}

pub(crate) fn tool_call_arguments(payload: &MessagePayload) -> Option<Map<String, Value>> {
    payload
        .message
        .get_tool_calls()
        .first()
        .map(|tool_call| tool_call.arguments.clone().into_iter().collect::<Map<String, Value>>())
}

pub(crate) fn tool_result_response(original: CallToolResult, payload: &MessagePayload) -> CallToolResult {
    let mut result = payload.message.get_tool_results().first().map_or(original, |tool_result| {
        serde_json::from_value::<CallToolResult>(tool_result.content.clone()).map_or_else(
            |_| {
                if tool_result.is_error {
                    raw_error_tool_result(tool_result.content.clone())
                } else {
                    raw_success_tool_result(tool_result.content.clone())
                }
            },
            |mut result| {
                result.is_error = Some(tool_result.is_error);
                result
            },
        )
    });

    let text = payload.message.get_text_content();
    if !text.is_empty() {
        result.content.push(ContentBlock::text(text));
    }

    result
}

fn raw_success_tool_result(value: Value) -> CallToolResult {
    if let Value::String(text) = value {
        CallToolResult::success(vec![ContentBlock::text(text)])
    } else {
        CallToolResult::structured(value)
    }
}

fn raw_error_tool_result(value: Value) -> CallToolResult {
    if let Value::String(text) = value {
        CallToolResult::error(vec![ContentBlock::text(text)])
    } else {
        CallToolResult::structured_error(value)
    }
}

pub(crate) fn prompt_request_payload(
    request: &GetPromptRequestParams,
    prompt_name: &str,
    backend_name: &str,
    prompt_request_id: &str,
) -> MessagePayload {
    MessagePayload {
        message: Message {
            schema_version: "2.0".to_owned(),
            role: Role::User,
            content: vec![ContentPart::PromptRequest {
                content: PromptRequest {
                    prompt_request_id: prompt_request_id.to_owned(),
                    name: prompt_name.to_owned(),
                    arguments: request.arguments.clone().map(HashMap::from_iter).unwrap_or_default(),
                    server_id: Some(backend_name.to_owned()),
                },
            }],
            channel: None,
        },
    }
}

pub(crate) fn prompt_request_arguments(payload: &MessagePayload) -> Option<Map<String, Value>> {
    payload
        .message
        .get_prompt_requests()
        .first()
        .map(|request| request.arguments.clone().into_iter().collect::<Map<String, Value>>())
}

pub(crate) fn prompt_result_payload(
    response: &GetPromptResult,
    prompt_name: &str,
    prompt_request_id: &str,
) -> MessagePayload {
    let messages =
        response.messages.iter().map(|message| cmf_prompt_message(message, prompt_request_id)).collect::<Vec<_>>();

    MessagePayload {
        message: Message {
            schema_version: "2.0".to_owned(),
            role: Role::Assistant,
            content: vec![ContentPart::PromptResult {
                content: PromptResult {
                    prompt_request_id: prompt_request_id.to_owned(),
                    prompt_name: prompt_name.to_owned(),
                    messages,
                    content: None,
                    is_error: false,
                    error_message: None,
                },
            }],
            channel: None,
        },
    }
}

fn prompt_result(payload: &MessagePayload) -> Option<&PromptResult> {
    let results = payload.message.get_prompt_results();
    let [result] = results.as_slice() else { return None };
    Some(*result)
}

pub(crate) fn prompt_result_rejection(payload: &MessagePayload) -> Option<String> {
    let result = prompt_result(payload)?;
    result
        .is_error
        .then(|| result.error_message.clone().unwrap_or_else(|| "Plugin rejected the rendered prompt".to_owned()))
}

// `None` means refuse: falling back to the backend's original would undo a plugin's redaction.
pub(crate) fn prompt_result_response(
    mut original: GetPromptResult,
    payload: &MessagePayload,
) -> Option<GetPromptResult> {
    let result = prompt_result(payload)?;
    if result.messages.len() != original.messages.len() {
        return None;
    }

    for (message, edited) in original.messages.iter_mut().zip(&result.messages) {
        let projected = cmf_prompt_message(message, &result.prompt_request_id);
        if serde_json::to_value(&projected).ok()? == serde_json::to_value(edited).ok()? {
            continue;
        }
        *message = mcp_prompt_message(edited)?;
    }

    Some(original)
}

fn cmf_prompt_message(message: &PromptMessage, prompt_request_id: &str) -> Message {
    Message {
        schema_version: "2.0".to_owned(),
        role: match message.role {
            McpRole::Assistant => Role::Assistant,
            McpRole::User => Role::User,
        },
        content: cmf_content_part(&message.content, prompt_request_id).into_iter().collect(),
        channel: None,
    }
}

fn cmf_content_part(block: &ContentBlock, prompt_request_id: &str) -> Option<ContentPart> {
    let part = match block {
        ContentBlock::Text(text) => ContentPart::Text { text: text.text.clone() },
        ContentBlock::Image(image) => ContentPart::Image {
            content: ImageSource {
                source_type: "base64".to_owned(),
                data: image.data.clone(),
                media_type: Some(image.mime_type.clone()),
            },
        },
        ContentBlock::Audio(audio) => ContentPart::Audio {
            content: AudioSource {
                source_type: "base64".to_owned(),
                data: audio.data.clone(),
                media_type: Some(audio.mime_type.clone()),
                duration_ms: None,
            },
        },
        ContentBlock::Resource(resource) => {
            let (uri, mime_type, content) = match &resource.resource {
                ResourceContents::TextResourceContents { uri, mime_type, text, .. } => {
                    (uri.clone(), mime_type.clone(), Some(text.clone()))
                },
                ResourceContents::BlobResourceContents { uri, mime_type, .. } => (uri.clone(), mime_type.clone(), None),
                _ => return None,
            };
            ContentPart::Resource {
                content: CmfResource {
                    resource_request_id: prompt_request_id.to_owned(),
                    uri,
                    name: None,
                    description: None,
                    resource_type: ResourceType::Uri,
                    content,
                    blob: None,
                    mime_type,
                    size_bytes: None,
                    annotations: HashMap::new(),
                    version: None,
                },
            }
        },
        ContentBlock::ResourceLink(link) => ContentPart::ResourceRef {
            content: ResourceReference {
                resource_request_id: prompt_request_id.to_owned(),
                uri: link.uri.clone(),
                name: Some(link.name.clone()),
                resource_type: ResourceType::Uri,
                range_start: None,
                range_end: None,
                selector: None,
            },
        },
        _ => return None,
    };

    Some(part)
}

fn mcp_prompt_message(message: &Message) -> Option<PromptMessage> {
    let role = match message.role {
        Role::Assistant => McpRole::Assistant,
        Role::User => McpRole::User,
        _ => return None,
    };

    let [part] = message.content.as_slice() else { return None };
    let content = match part {
        ContentPart::Text { text } => ContentBlock::text(text.clone()),
        ContentPart::Image { content } => {
            ContentBlock::image(content.data.clone(), content.media_type.clone().unwrap_or_default())
        },
        ContentPart::Audio { content } => {
            ContentBlock::audio(content.data.clone(), content.media_type.clone().unwrap_or_default())
        },
        ContentPart::Resource { content } => ContentBlock::resource(ResourceContents::TextResourceContents {
            uri: content.uri.clone(),
            mime_type: content.mime_type.clone(),
            text: content.content.clone()?,
            meta: None,
        }),
        ContentPart::ResourceRef { content } => {
            ContentBlock::ResourceLink(McpResource::new(content.uri.clone(), content.name.clone().unwrap_or_default()))
        },
        _ => return None,
    };

    Some(PromptMessage::new(role, content))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn text_prompt() -> GetPromptResult {
        GetPromptResult::new(vec![PromptMessage::new_text(McpRole::User, "review of weather")])
    }

    fn prompt_result_mut(payload: &mut MessagePayload) -> &mut PromptResult {
        payload
            .message
            .content
            .iter_mut()
            .find_map(|part| match part {
                ContentPart::PromptResult { content } => Some(content),
                _ => None,
            })
            .expect("payload carries a prompt result")
    }

    fn edited_messages(payload: &mut MessagePayload) -> &mut Vec<Message> {
        &mut prompt_result_mut(payload).messages
    }

    #[test]
    fn prompt_result_response_rejects_added_message() {
        let original = text_prompt();
        let mut payload = prompt_result_payload(&original, "review", "prompt-1");
        let extra = edited_messages(&mut payload).first().cloned().expect("one message");
        edited_messages(&mut payload).push(extra);

        assert!(prompt_result_response(original, &payload).is_none());
    }

    #[test]
    fn prompt_result_response_rejects_extra_prompt_result() {
        let original = text_prompt();
        let mut payload = prompt_result_payload(&original, "review", "prompt-1");
        let duplicate = payload.message.content[0].clone();
        payload.message.content.push(duplicate);

        assert!(prompt_result_response(original, &payload).is_none());
    }

    #[test]
    fn prompt_result_rejection_reports_the_plugin_error_message() {
        let original = text_prompt();
        let mut payload = prompt_result_payload(&original, "review", "prompt-1");
        let result = prompt_result_mut(&mut payload);
        result.is_error = true;
        result.error_message = Some("blocked by policy".to_owned());

        assert_eq!(Some("blocked by policy".to_owned()), prompt_result_rejection(&payload));
    }

    #[test]
    fn prompt_result_rejection_falls_back_when_the_plugin_gives_no_message() {
        let original = text_prompt();
        let mut payload = prompt_result_payload(&original, "review", "prompt-1");
        prompt_result_mut(&mut payload).is_error = true;

        assert_eq!(Some("Plugin rejected the rendered prompt".to_owned()), prompt_result_rejection(&payload));
    }

    #[test]
    fn prompt_result_rejection_is_absent_for_a_normal_result() {
        let original = text_prompt();
        let payload = prompt_result_payload(&original, "review", "prompt-1");

        assert_eq!(None, prompt_result_rejection(&payload));
    }

    #[test]
    fn prompt_result_response_rejects_removed_message() {
        let original = text_prompt();
        let mut payload = prompt_result_payload(&original, "review", "prompt-1");
        edited_messages(&mut payload).clear();

        assert!(prompt_result_response(original, &payload).is_none());
    }

    #[test]
    fn prompt_result_response_rejects_unmappable_role() {
        let original = text_prompt();
        let mut payload = prompt_result_payload(&original, "review", "prompt-1");
        edited_messages(&mut payload)[0].role = Role::System;

        assert!(prompt_result_response(original, &payload).is_none());
    }

    #[test]
    fn prompt_result_response_preserves_unmodified_messages() {
        let original = text_prompt();
        let payload = prompt_result_payload(&original, "review", "prompt-1");

        let result = prompt_result_response(original.clone(), &payload).expect("unmodified payload applies");

        assert_eq!(
            serde_json::to_value(&original).expect("original serializes"),
            serde_json::to_value(&result).expect("result serializes")
        );
    }

    #[test]
    fn prompt_result_response_round_trips_embedded_resource() {
        let original = GetPromptResult::new(vec![PromptMessage::new(
            McpRole::User,
            ContentBlock::resource(ResourceContents::text("token=secret", "file:///app.env")),
        )]);
        let mut payload = prompt_result_payload(&original, "review", "prompt-1");

        let ContentPart::Resource { content } = &mut edited_messages(&mut payload)[0].content[0] else {
            panic!("embedded resource reaches the plugin as a CMF resource part");
        };
        assert_eq!(Some("token=secret"), content.content.as_deref());
        content.content = Some("token=[REDACTED]".to_owned());

        let result = prompt_result_response(original, &payload).expect("resource edit applies");

        let ContentBlock::Resource(resource) = &result.messages[0].content else {
            panic!("expected an embedded resource");
        };
        let ResourceContents::TextResourceContents { text, uri, .. } = &resource.resource else {
            panic!("expected text resource contents");
        };
        assert_eq!("token=[REDACTED]", text);
        assert_eq!("file:///app.env", uri);
    }

    #[test]
    fn tool_result_response_uses_cmf_error_flag_for_nested_mcp_result() {
        let original = CallToolResult::success(vec![ContentBlock::text("original")]);
        let nested = CallToolResult::success(vec![ContentBlock::text("changed")]);
        let mut payload = tool_result_payload("sum", &nested, "call-1");
        let ContentPart::ToolResult { content } = &mut payload.message.content[0] else {
            panic!("expected tool result");
        };
        content.is_error = true;

        let result = tool_result_response(original, &payload);

        assert_eq!(Some(true), result.is_error);
    }
}
