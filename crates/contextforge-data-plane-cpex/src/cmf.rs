use std::collections::HashMap;

use base64::{Engine as _, prelude::BASE64_STANDARD};
use cpex::cpex_core::cmf::{
    AudioSource, ContentPart, ImageSource, Message, MessagePayload, PromptRequest, PromptResult,
    Resource as CmfResource, ResourceReference, ResourceType, Role, ToolCall, ToolResult, constants::SCHEMA_VERSION,
};
use rmcp::model::{
    CallToolRequestParams, CallToolResult, ContentBlock, GetPromptRequestParams, GetPromptResult, PromptMessage,
    ReadResourceResult, Resource as McpResource, ResourceContents, Role as McpRole,
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
            schema_version: SCHEMA_VERSION.to_owned(),
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

pub(crate) fn resource_request_payload(resource_uri: &str, resource_request_id: &str) -> MessagePayload {
    MessagePayload {
        message: Message {
            schema_version: SCHEMA_VERSION.to_owned(),
            role: Role::User,
            content: vec![ContentPart::ResourceRef {
                content: ResourceReference {
                    resource_request_id: resource_request_id.to_owned(),
                    uri: resource_uri.to_owned(),
                    name: None,
                    resource_type: ResourceType::Uri,
                    range_start: None,
                    range_end: None,
                    selector: None,
                },
            }],
            channel: None,
        },
    }
}

pub(crate) fn resource_result_payload(
    response: &ReadResourceResult,
    resource_request_id: &str,
) -> Option<MessagePayload> {
    let content = response
        .contents
        .iter()
        .map(|content| {
            cmf_resource_content(content, resource_request_id).map(|content| ContentPart::Resource { content })
        })
        .collect::<Option<Vec<_>>>()?;
    Some(MessagePayload {
        message: Message { schema_version: SCHEMA_VERSION.to_owned(), role: Role::Assistant, content, channel: None },
    })
}

fn cmf_resource_content(content: &ResourceContents, resource_request_id: &str) -> Option<CmfResource> {
    let (uri, mime_type, text, blob) = match content {
        ResourceContents::TextResourceContents { uri, mime_type, text, .. } => {
            (uri.clone(), mime_type.clone(), Some(text.clone()), None)
        },
        ResourceContents::BlobResourceContents { uri, mime_type, blob, .. } => {
            (uri.clone(), mime_type.clone(), None, Some(BASE64_STANDARD.decode(blob).ok()?))
        },
        _ => return None,
    };
    Some(CmfResource {
        resource_request_id: resource_request_id.to_owned(),
        uri,
        resource_type: ResourceType::Uri,
        content: text,
        blob,
        mime_type,
        ..Default::default()
    })
}

pub(crate) fn resource_result_response(
    mut original: ReadResourceResult,
    payload: &MessagePayload,
) -> Option<ReadResourceResult> {
    // Resource post hooks replace each resource's content, not the read envelope.
    if payload.message.content.len() != original.contents.len() {
        return None;
    }
    for (original, modified) in original.contents.iter_mut().zip(&payload.message.content) {
        let ContentPart::Resource { content } = modified else { return None };
        let meta = match original {
            ResourceContents::TextResourceContents { meta, .. }
            | ResourceContents::BlobResourceContents { meta, .. } => meta.clone(),
            _ => return None,
        };
        *original = match (&content.content, &content.blob) {
            (Some(text), _) => ResourceContents::TextResourceContents {
                uri: content.uri.clone(),
                mime_type: content.mime_type.clone(),
                text: text.clone(),
                meta,
            },
            (None, Some(bytes)) => {
                let blob = match original {
                    ResourceContents::BlobResourceContents { blob, .. }
                        if BASE64_STANDARD.decode(blob.as_bytes()).ok().as_ref() == Some(bytes) =>
                    {
                        blob.clone()
                    },
                    _ => BASE64_STANDARD.encode(bytes),
                };
                ResourceContents::BlobResourceContents {
                    uri: content.uri.clone(),
                    mime_type: content.mime_type.clone(),
                    blob,
                    meta,
                }
            },
            _ => return None,
        };
    }
    Some(original)
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
            schema_version: SCHEMA_VERSION.to_owned(),
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
            schema_version: SCHEMA_VERSION.to_owned(),
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

pub(crate) fn prompt_request_arguments(
    payload: &MessagePayload,
    prompt_name: &str,
    backend_name: &str,
    prompt_request_id: &str,
) -> Option<Map<String, Value>> {
    let requests = payload.message.get_prompt_requests();
    let [request] = requests.as_slice() else { return None };
    if request.name != prompt_name
        || request.prompt_request_id != prompt_request_id
        || request.server_id.as_deref() != Some(backend_name)
    {
        return None;
    }
    Some(request.arguments.clone().into_iter().collect::<Map<String, Value>>())
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
    prompt_name: &str,
    prompt_request_id: &str,
) -> Option<GetPromptResult> {
    let result = prompt_result(payload)?;
    if result.prompt_name != prompt_name
        || result.prompt_request_id != prompt_request_id
        || result.content.is_some()
        || result.error_message.is_some()
    {
        return None;
    }
    if result.messages.len() != original.messages.len() {
        return None;
    }

    for (message, edited) in original.messages.iter_mut().zip(&result.messages) {
        let projected = cmf_prompt_message(message, prompt_request_id);
        if serde_json::to_value(&projected).ok()? == serde_json::to_value(edited).ok()? {
            continue;
        }

        let rebuilt = mcp_prompt_message(edited)?;
        if serde_json::to_value(cmf_prompt_message(&rebuilt, prompt_request_id)).ok()?
            != serde_json::to_value(edited).ok()?
        {
            return None;
        }
        *message = rebuilt;
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

// MCP inlines image and audio bytes as base64, so a CMF source CMF can express but MCP cannot —
// a URL reference — has to be refused rather than written into a field that means something else.
fn inline_media_data<'a>(source_type: &str, data: &'a str) -> Option<&'a str> {
    (source_type == "base64").then_some(data)
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
            ContentBlock::image(inline_media_data(&content.source_type, &content.data)?, content.media_type.clone()?)
        },
        ContentPart::Audio { content } => {
            ContentBlock::audio(inline_media_data(&content.source_type, &content.data)?, content.media_type.clone()?)
        },
        ContentPart::Resource { content } => ContentBlock::resource(ResourceContents::TextResourceContents {
            uri: content.uri.clone(),
            mime_type: content.mime_type.clone(),
            text: content.content.clone()?,
            meta: None,
        }),
        ContentPart::ResourceRef { content } => {
            ContentBlock::ResourceLink(McpResource::new(content.uri.clone(), content.name.clone()?))
        },
        _ => return None,
    };

    Some(PromptMessage::new(role, content))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resource_result_response_applies_text_changes() {
        let original =
            ReadResourceResult::new(vec![ResourceContents::text("AWS_ACCESS_KEY_ID=secret", "file:///password.env")]);
        let mut payload = resource_result_payload(&original, "resource-1").expect("resource result is supported");
        let ContentPart::Resource { content } = &mut payload.message.content[0] else {
            panic!("expected resource content");
        };
        content.content = Some("AWS_ACCESS_KEY_ID=[redacted]".to_owned());

        let result = resource_result_response(original, &payload).expect("text edit applies");

        let ResourceContents::TextResourceContents { text, uri, .. } = &result.contents[0] else {
            panic!("expected text resource");
        };
        assert_eq!("AWS_ACCESS_KEY_ID=[redacted]", text);
        assert_eq!("file:///password.env", uri);
    }

    #[test]
    fn resource_result_response_decodes_and_applies_blob_changes() {
        let wire_blob = BASE64_STANDARD.encode(b"AWS_ACCESS_KEY_ID=secret");
        let original = ReadResourceResult::new(vec![
            ResourceContents::blob(wire_blob.clone(), "file:///password.bin")
                .with_mime_type("application/octet-stream"),
        ]);
        let mut payload = resource_result_payload(&original, "resource-1").expect("valid blob is supported");
        let ContentPart::Resource { content } = &mut payload.message.content[0] else {
            panic!("expected resource content");
        };
        assert_eq!(Some(b"AWS_ACCESS_KEY_ID=secret".as_slice()), content.blob.as_deref());
        content.blob = Some(b"AWS_ACCESS_KEY_ID=[redacted]".to_vec());

        let result = resource_result_response(original, &payload).expect("blob edit applies");

        let ResourceContents::BlobResourceContents { blob, uri, .. } = &result.contents[0] else {
            panic!("expected blob resource");
        };
        assert_eq!(b"AWS_ACCESS_KEY_ID=[redacted]", BASE64_STANDARD.decode(blob).expect("valid base64").as_slice());
        assert_eq!("file:///password.bin", uri);
        assert_ne!(&wire_blob, blob);
    }

    #[test]
    fn resource_result_response_preserves_unchanged_blob_wire_value() {
        let wire_blob = BASE64_STANDARD.encode(b"unchanged");
        let original = ReadResourceResult::new(vec![ResourceContents::blob(&wire_blob, "file:///image.bin")]);
        let payload = resource_result_payload(&original, "resource-1").expect("valid blob is supported");

        let result = resource_result_response(original, &payload).expect("unchanged blob applies");

        let ResourceContents::BlobResourceContents { blob, .. } = &result.contents[0] else {
            panic!("expected blob resource");
        };
        assert_eq!(&wire_blob, blob);
    }

    #[test]
    fn resource_result_payload_rejects_invalid_base64_blob() {
        let original = ReadResourceResult::new(vec![ResourceContents::blob("not base64!", "file:///image.bin")]);

        assert!(resource_result_payload(&original, "resource-1").is_none());
    }

    #[test]
    fn resource_result_allows_mime_uri_and_content_type_changes() {
        let original: ReadResourceResult = serde_json::from_value(serde_json::json!({
            "_meta": {"response": "preserved"},
            "contents": [
                {"uri": "file:///a", "mimeType": "text/plain", "text": "original", "_meta": {"item": 1}},
                {"uri": "file:///b", "mimeType": "application/octet-stream", "blob": "YmluYXJ5", "_meta": {"item": 2}}
            ]
        }))
        .expect("valid resource response");
        let mut payload = resource_result_payload(&original, "resource-1").expect("resource payload");
        for (index, part) in payload.message.content.iter_mut().enumerate() {
            let ContentPart::Resource { content } = part else { panic!("resource content") };
            content.uri = format!("file:///changed-{index}");
            if index == 0 {
                content.content = None;
                content.blob = Some(b"binary edit".to_vec());
                content.mime_type = Some("application/octet-stream".to_owned());
            } else {
                content.blob = None;
                content.content = Some("text edit".to_owned());
                content.mime_type = Some("text/plain".to_owned());
            }
        }
        let actual = serde_json::to_value(resource_result_response(original, &payload).expect("valid changes apply"))
            .expect("response serializes");
        assert_eq!(serde_json::json!({"response": "preserved"}), actual["_meta"]);
        assert_eq!("file:///changed-0", actual["contents"][0]["uri"]);
        assert_eq!("application/octet-stream", actual["contents"][0]["mimeType"]);
        assert_eq!(BASE64_STANDARD.encode(b"binary edit"), actual["contents"][0]["blob"]);
        assert_eq!(1, actual["contents"][0]["_meta"]["item"]);
        assert_eq!("file:///changed-1", actual["contents"][1]["uri"]);
        assert_eq!("text/plain", actual["contents"][1]["mimeType"]);
        assert_eq!("text edit", actual["contents"][1]["text"]);
        assert_eq!(2, actual["contents"][1]["_meta"]["item"]);
    }

    #[test]
    fn resource_result_ignores_cmf_fields_that_are_not_mcp_content() {
        let original = ReadResourceResult::new(vec![ResourceContents::text("original", "file:///a")]);
        let mut payload = resource_result_payload(&original, "resource-1").expect("resource payload");
        payload.message.schema_version = "plugin value".to_owned();
        payload.message.role = Role::User;
        payload.message.channel = Some(cpex::cpex_core::cmf::Channel::Analysis);
        let ContentPart::Resource { content } = &mut payload.message.content[0] else { panic!("resource content") };
        content.resource_request_id = "plugin value".to_owned();
        content.name = Some("display name".to_owned());
        content.description = Some("description".to_owned());
        content.size_bytes = Some(8);
        content.version = Some("v2".to_owned());
        content.annotations.insert("note".to_owned(), serde_json::json!("annotation"));
        content.content = Some("redacted".to_owned());
        let result = resource_result_response(original, &payload).expect("MCP content remains usable");
        let ResourceContents::TextResourceContents { text, .. } = &result.contents[0] else { panic!("text content") };
        assert_eq!("redacted", text);
    }

    #[test]
    fn resource_result_prefers_text_when_both_content_fields_are_present() {
        let original = ReadResourceResult::new(vec![ResourceContents::blob("YmluYXJ5", "file:///a")]);
        let mut payload = resource_result_payload(&original, "resource-1").expect("resource payload");
        let ContentPart::Resource { content } = &mut payload.message.content[0] else { panic!("resource content") };
        content.content = Some("text replacement".to_owned());
        let result =
            resource_result_response(original, &payload).expect("text takes precedence, as in the built-in serializer");
        let ResourceContents::TextResourceContents { text, .. } = &result.contents[0] else { panic!("text resource") };
        assert_eq!("text replacement", text);
    }

    #[test]
    fn resource_result_rejects_content_without_a_valid_mcp_representation() {
        let original = ReadResourceResult::new(vec![ResourceContents::text("original", "file:///a")]);
        let mut payload = resource_result_payload(&original, "resource-1").expect("resource payload");
        let ContentPart::Resource { content } = &mut payload.message.content[0] else { panic!("resource content") };
        content.content = None;
        assert!(resource_result_response(original, &payload).is_none());
    }

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

        assert!(prompt_result_response(original, &payload, "review", "prompt-1").is_none());
    }

    #[test]
    fn prompt_result_response_rejects_extra_prompt_result() {
        let original = text_prompt();
        let mut payload = prompt_result_payload(&original, "review", "prompt-1");
        let duplicate = payload.message.content[0].clone();
        payload.message.content.push(duplicate);

        assert!(prompt_result_response(original, &payload, "review", "prompt-1").is_none());
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

    fn review_payload() -> MessagePayload {
        let request = GetPromptRequestParams::new("review")
            .with_arguments(Map::from_iter([("topic".to_owned(), Value::from("weather"))]));
        prompt_request_payload(&request, "review", "backend-a", "prompt-1")
    }

    fn prompt_request_mut(payload: &mut MessagePayload) -> &mut PromptRequest {
        payload
            .message
            .content
            .iter_mut()
            .find_map(|part| match part {
                ContentPart::PromptRequest { content } => Some(content),
                _ => None,
            })
            .expect("payload carries a prompt request")
    }

    #[test]
    fn prompt_request_arguments_accepts_an_argument_edit() {
        let mut payload = review_payload();
        prompt_request_mut(&mut payload).arguments.insert("topic".to_owned(), Value::from("rain"));

        let arguments = prompt_request_arguments(&payload, "review", "backend-a", "prompt-1");

        assert_eq!(Some(&Value::from("rain")), arguments.as_ref().and_then(|args| args.get("topic")));
    }

    #[test]
    fn prompt_request_arguments_rejects_a_renamed_prompt() {
        let mut payload = review_payload();
        "other".clone_into(&mut prompt_request_mut(&mut payload).name);

        assert!(prompt_request_arguments(&payload, "review", "backend-a", "prompt-1").is_none());
    }

    #[test]
    fn prompt_request_arguments_rejects_a_rerouted_backend() {
        let mut payload = review_payload();
        prompt_request_mut(&mut payload).server_id = Some("backend-b".to_owned());

        assert!(prompt_request_arguments(&payload, "review", "backend-a", "prompt-1").is_none());
    }

    #[test]
    fn prompt_request_arguments_rejects_a_recorrelated_request() {
        let mut payload = review_payload();
        "prompt-2".clone_into(&mut prompt_request_mut(&mut payload).prompt_request_id);

        assert!(prompt_request_arguments(&payload, "review", "backend-a", "prompt-1").is_none());
    }

    #[test]
    fn prompt_request_arguments_rejects_extra_prompt_requests() {
        let mut payload = review_payload();
        let duplicate = payload.message.content[0].clone();
        payload.message.content.push(duplicate);

        assert!(prompt_request_arguments(&payload, "review", "backend-a", "prompt-1").is_none());
    }

    #[test]
    fn prompt_result_response_rejects_envelope_content_edit() {
        let original = text_prompt();
        let mut payload = prompt_result_payload(&original, "review", "prompt-1");
        prompt_result_mut(&mut payload).content = Some("[REDACTED]".to_owned());

        assert!(prompt_result_response(original, &payload, "review", "prompt-1").is_none());
    }

    #[test]
    fn prompt_result_response_rejects_renamed_prompt() {
        let original = text_prompt();
        let mut payload = prompt_result_payload(&original, "review", "prompt-1");
        prompt_result_mut(&mut payload).prompt_name = "other".to_owned();

        assert!(prompt_result_response(original, &payload, "review", "prompt-1").is_none());
    }

    #[test]
    fn prompt_result_response_rejects_recorrelated_result() {
        let original = text_prompt();
        let mut payload = prompt_result_payload(&original, "review", "prompt-1");
        prompt_result_mut(&mut payload).prompt_request_id = "prompt-2".to_owned();

        assert!(prompt_result_response(original, &payload, "review", "prompt-1").is_none());
    }

    #[test]
    fn prompt_result_response_rejects_error_message_without_error_flag() {
        let original = text_prompt();
        let mut payload = prompt_result_payload(&original, "review", "prompt-1");
        prompt_result_mut(&mut payload).error_message = Some("blocked".to_owned());

        assert!(prompt_result_response(original, &payload, "review", "prompt-1").is_none());
    }

    fn resource_prompt() -> GetPromptResult {
        GetPromptResult::new(vec![PromptMessage::new(
            McpRole::User,
            ContentBlock::resource(ResourceContents::text("token=secret", "file:///app.env")),
        )])
    }

    #[test]
    fn prompt_result_response_rejects_resource_type_edit() {
        let original = resource_prompt();
        let mut payload = prompt_result_payload(&original, "review", "prompt-1");
        let ContentPart::Resource { content } = &mut edited_messages(&mut payload)[0].content[0] else {
            panic!("expected a resource part");
        };
        content.resource_type = ResourceType::Database;

        assert!(prompt_result_response(original, &payload, "review", "prompt-1").is_none());
    }

    #[test]
    fn prompt_result_response_rejects_dropped_resource_metadata() {
        let original = resource_prompt();
        let mut payload = prompt_result_payload(&original, "review", "prompt-1");
        let ContentPart::Resource { content } = &mut edited_messages(&mut payload)[0].content[0] else {
            panic!("expected a resource part");
        };
        content.description = Some("annotated by policy".to_owned());

        assert!(prompt_result_response(original, &payload, "review", "prompt-1").is_none());
    }

    fn media_prompt(content: ContentBlock) -> GetPromptResult {
        GetPromptResult::new(vec![PromptMessage::new(McpRole::User, content)])
    }

    #[test]
    fn prompt_result_response_round_trips_an_image_edit() {
        let original = media_prompt(ContentBlock::image("aW1hZ2U=", "image/png"));
        let mut payload = prompt_result_payload(&original, "review", "prompt-1");
        let ContentPart::Image { content } = &mut edited_messages(&mut payload)[0].content[0] else {
            panic!("expected an image part");
        };
        content.data = "cmVkYWN0ZWQ=".to_owned();

        let result = prompt_result_response(original, &payload, "review", "prompt-1").expect("image edit applies");

        let ContentBlock::Image(image) = &result.messages[0].content else { panic!("expected an image") };
        assert_eq!("cmVkYWN0ZWQ=", image.data);
        assert_eq!("image/png", image.mime_type);
    }

    #[test]
    fn prompt_result_response_round_trips_an_audio_edit() {
        let original = media_prompt(ContentBlock::audio("YXVkaW8=", "audio/mp3"));
        let mut payload = prompt_result_payload(&original, "review", "prompt-1");
        let ContentPart::Audio { content } = &mut edited_messages(&mut payload)[0].content[0] else {
            panic!("audio reaches the plugin as a CMF audio part");
        };
        content.data = "cmVkYWN0ZWQ=".to_owned();

        let result = prompt_result_response(original, &payload, "review", "prompt-1").expect("audio edit applies");

        let ContentBlock::Audio(audio) = &result.messages[0].content else { panic!("expected audio") };
        assert_eq!("cmVkYWN0ZWQ=", audio.data);
        assert_eq!("audio/mp3", audio.mime_type);
    }

    #[test]
    fn prompt_result_response_rejects_url_sourced_audio() {
        let original = media_prompt(ContentBlock::audio("YXVkaW8=", "audio/mp3"));
        let mut payload = prompt_result_payload(&original, "review", "prompt-1");
        let ContentPart::Audio { content } = &mut edited_messages(&mut payload)[0].content[0] else {
            panic!("expected an audio part");
        };
        "url".clone_into(&mut content.source_type);
        content.data = "https://example.invalid/clip.mp3".to_owned();

        assert!(prompt_result_response(original, &payload, "review", "prompt-1").is_none());
    }

    #[test]
    fn prompt_result_response_rejects_audio_without_media_type() {
        let original = media_prompt(ContentBlock::audio("YXVkaW8=", "audio/mp3"));
        let mut payload = prompt_result_payload(&original, "review", "prompt-1");
        let ContentPart::Audio { content } = &mut edited_messages(&mut payload)[0].content[0] else {
            panic!("expected an audio part");
        };
        content.media_type = None;

        assert!(prompt_result_response(original, &payload, "review", "prompt-1").is_none());
    }

    #[test]
    fn prompt_result_response_round_trips_a_resource_link_edit() {
        let original = media_prompt(ContentBlock::ResourceLink(McpResource::new("file:///app.env", "app-env")));
        let mut payload = prompt_result_payload(&original, "review", "prompt-1");
        let ContentPart::ResourceRef { content } = &mut edited_messages(&mut payload)[0].content[0] else {
            panic!("expected a resource reference part");
        };
        content.name = Some("redacted-env".to_owned());

        let result = prompt_result_response(original, &payload, "review", "prompt-1").expect("link edit applies");

        let ContentBlock::ResourceLink(link) = &result.messages[0].content else { panic!("expected a link") };
        assert_eq!("redacted-env", link.name);
        assert_eq!("file:///app.env", link.uri);
    }

    #[test]
    fn prompt_result_response_rejects_resource_with_removed_text() {
        let original = resource_prompt();
        let mut payload = prompt_result_payload(&original, "review", "prompt-1");
        let ContentPart::Resource { content } = &mut edited_messages(&mut payload)[0].content[0] else {
            panic!("expected a resource part");
        };
        content.content = None;

        assert!(prompt_result_response(original, &payload, "review", "prompt-1").is_none());
    }

    #[test]
    fn prompt_result_response_rejects_multiple_content_parts() {
        let original = text_prompt();
        let mut payload = prompt_result_payload(&original, "review", "prompt-1");
        edited_messages(&mut payload)[0].content.push(ContentPart::Text { text: "extra".to_owned() });

        assert!(prompt_result_response(original, &payload, "review", "prompt-1").is_none());
    }

    #[test]
    fn prompt_result_response_rejects_a_cmf_only_content_part() {
        let original = text_prompt();
        let mut payload = prompt_result_payload(&original, "review", "prompt-1");
        edited_messages(&mut payload)[0].content = vec![ContentPart::Thinking { text: "reasoning".to_owned() }];

        assert!(prompt_result_response(original, &payload, "review", "prompt-1").is_none());
    }

    #[test]
    fn prompt_result_response_rejects_a_payload_without_a_prompt_result() {
        let original = text_prompt();
        let mut payload = prompt_result_payload(&original, "review", "prompt-1");
        payload.message.content.clear();

        assert!(prompt_result_response(original, &payload, "review", "prompt-1").is_none());
    }

    #[test]
    fn prompt_request_arguments_rejects_a_payload_without_a_prompt_request() {
        let mut payload = review_payload();
        payload.message.content.clear();

        assert!(prompt_request_arguments(&payload, "review", "backend-a", "prompt-1").is_none());
    }

    #[test]
    fn prompt_result_response_rejects_url_sourced_image() {
        let original =
            GetPromptResult::new(vec![PromptMessage::new(McpRole::User, ContentBlock::image("aW1hZ2U=", "image/png"))]);
        let mut payload = prompt_result_payload(&original, "review", "prompt-1");
        let ContentPart::Image { content } = &mut edited_messages(&mut payload)[0].content[0] else {
            panic!("expected an image part");
        };
        "url".clone_into(&mut content.source_type);
        content.data = "https://example.invalid/image.png".to_owned();

        assert!(prompt_result_response(original, &payload, "review", "prompt-1").is_none());
    }

    #[test]
    fn prompt_result_response_rejects_image_without_media_type() {
        let original =
            GetPromptResult::new(vec![PromptMessage::new(McpRole::User, ContentBlock::image("aW1hZ2U=", "image/png"))]);
        let mut payload = prompt_result_payload(&original, "review", "prompt-1");
        let ContentPart::Image { content } = &mut edited_messages(&mut payload)[0].content[0] else {
            panic!("expected an image part");
        };
        content.media_type = None;

        assert!(prompt_result_response(original, &payload, "review", "prompt-1").is_none());
    }

    #[test]
    fn prompt_result_response_rejects_resource_link_without_name() {
        let original = GetPromptResult::new(vec![PromptMessage::new(
            McpRole::User,
            ContentBlock::ResourceLink(McpResource::new("file:///app.env", "app-env")),
        )]);
        let mut payload = prompt_result_payload(&original, "review", "prompt-1");
        let ContentPart::ResourceRef { content } = &mut edited_messages(&mut payload)[0].content[0] else {
            panic!("expected a resource reference part");
        };
        content.name = None;

        assert!(prompt_result_response(original, &payload, "review", "prompt-1").is_none());
    }

    #[test]
    fn prompt_result_response_rejects_resource_link_range_edit() {
        let original = GetPromptResult::new(vec![PromptMessage::new(
            McpRole::User,
            ContentBlock::ResourceLink(McpResource::new("file:///app.env", "app-env")),
        )]);
        let mut payload = prompt_result_payload(&original, "review", "prompt-1");
        let ContentPart::ResourceRef { content } = &mut edited_messages(&mut payload)[0].content[0] else {
            panic!("expected a resource reference part");
        };
        content.range_start = Some(10);

        assert!(prompt_result_response(original, &payload, "review", "prompt-1").is_none());
    }

    #[test]
    fn prompt_result_response_rejects_removed_message() {
        let original = text_prompt();
        let mut payload = prompt_result_payload(&original, "review", "prompt-1");
        edited_messages(&mut payload).clear();

        assert!(prompt_result_response(original, &payload, "review", "prompt-1").is_none());
    }

    #[test]
    fn prompt_result_response_rejects_unmappable_role() {
        let original = text_prompt();
        let mut payload = prompt_result_payload(&original, "review", "prompt-1");
        edited_messages(&mut payload)[0].role = Role::System;

        assert!(prompt_result_response(original, &payload, "review", "prompt-1").is_none());
    }

    #[test]
    fn prompt_result_response_preserves_unmodified_messages() {
        let original = text_prompt();
        let payload = prompt_result_payload(&original, "review", "prompt-1");

        let result = prompt_result_response(original.clone(), &payload, "review", "prompt-1")
            .expect("unmodified payload applies");

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

        let result = prompt_result_response(original, &payload, "review", "prompt-1").expect("resource edit applies");

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
