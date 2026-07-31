use std::collections::HashMap;

use cpex::cpex_core::cmf::{
    ContentPart, Message, MessagePayload, PromptRequest, PromptResult, Resource, ResourceReference, ResourceType, Role,
    ToolCall, ToolResult,
};
use rmcp::{
    ErrorData,
    model::{
        ArgumentInfo, CallToolRequestParams, CallToolResult, CompleteResult, ContentBlock, ErrorCode,
        GetPromptRequestParams, GetPromptResult, Prompt, PromptMessage, ReadResourceResult, Resource as McpResource,
        ResourceContents, ResourceTemplate, Role as McpRole,
    },
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

/// Builds the `cmf.prompt_pre_fetch` payload. Mirrors [`tool_call_payload`]: the hook runs after
/// routing, so `prompt_name` is already backend-local and `backend_name` carries the namespace.
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
                    arguments: request.arguments.clone().unwrap_or_default().into_iter().collect(),
                    server_id: Some(backend_name.to_owned()),
                },
            }],
            channel: None,
        },
    }
}

/// Builds the `cmf.prompt_post_fetch` payload.
///
/// CMF `PromptResult` is typed rather than free-form JSON, so MCP messages are mapped into CMF
/// messages one-for-one. Text blocks are exposed as [`ContentPart::Text`]; other block kinds
/// (image, audio, embedded resource, resource link) become a message with empty content. That
/// keeps index alignment with the original response without claiming to expose content the
/// mapping cannot write back.
pub(crate) fn prompt_result_payload(
    prompt_name: &str,
    result: &GetPromptResult,
    prompt_request_id: &str,
) -> MessagePayload {
    MessagePayload {
        message: Message {
            schema_version: "2.0".to_owned(),
            role: Role::Assistant,
            content: vec![ContentPart::PromptResult {
                content: PromptResult {
                    prompt_request_id: prompt_request_id.to_owned(),
                    prompt_name: prompt_name.to_owned(),
                    messages: result.messages.iter().map(prompt_message_to_cmf).collect(),
                    content: None,
                    is_error: false,
                    error_message: None,
                },
            }],
            channel: None,
        },
    }
}

fn prompt_message_to_cmf(message: &PromptMessage) -> Message {
    let content = match &message.content {
        ContentBlock::Text(text) => vec![ContentPart::Text { text: text.text.clone() }],
        _ => Vec::new(),
    };
    Message {
        schema_version: "2.0".to_owned(),
        role: match message.role {
            McpRole::User => Role::User,
            McpRole::Assistant => Role::Assistant,
        },
        content,
        channel: None,
    }
}

/// Builds the `cmf.prompt_pre_fetch` payload for `prompts/list`.
///
/// A listing has no single prompt and no single backend, so the request carries an **empty name**
/// and no `server_id`. Plugins discriminate a listing from a `prompts/get` on that empty name.
pub(crate) fn prompt_list_request_payload(cursor: Option<&str>, prompt_request_id: &str) -> MessagePayload {
    let arguments = cursor
        .map(|cursor| HashMap::from([("cursor".to_owned(), Value::String(cursor.to_owned()))]))
        .unwrap_or_default();
    MessagePayload {
        message: Message {
            schema_version: "2.0".to_owned(),
            role: Role::User,
            content: vec![ContentPart::PromptRequest {
                content: PromptRequest {
                    prompt_request_id: prompt_request_id.to_owned(),
                    name: String::new(),
                    arguments,
                    server_id: None,
                },
            }],
            channel: None,
        },
    }
}

/// Builds the `cmf.prompt_post_fetch` payload for `prompts/list`, over the merged and namespaced
/// listing the client is about to receive.
///
/// Exposure is read-only: MCP `Prompt` is metadata (`title`, `arguments`, `icons`, `_meta`) with no
/// CMF equivalent, so a modified payload cannot be written back without silently dropping fields.
/// Each prompt contributes its exposed name, plus its description as text when it has one.
pub(crate) fn prompt_list_result_payload(prompts: &[Prompt], prompt_request_id: &str) -> MessagePayload {
    let mut content = Vec::with_capacity(prompts.len());
    for prompt in prompts {
        content.push(ContentPart::PromptRequest {
            content: PromptRequest {
                prompt_request_id: prompt_request_id.to_owned(),
                name: prompt.name.clone(),
                arguments: HashMap::new(),
                server_id: None,
            },
        });
        if let Some(description) = &prompt.description {
            content.push(ContentPart::Text { text: description.clone() });
        }
    }
    MessagePayload {
        message: Message { schema_version: "2.0".to_owned(), role: Role::Assistant, content, channel: None },
    }
}

/// Builds the `cmf.resource_pre_fetch` payload for the resource listings — `resources/list` and
/// `resources/templates/list`.
///
/// Like the prompt listing, a resource listing has no single resource and no single backend, so the
/// reference carries an **empty URI**. Plugins discriminate a listing on that empty URI. The
/// pagination cursor is not exposed: CMF `ResourceReference` has no field for request arguments,
/// and overloading `selector` would misrepresent what that field means.
pub(crate) fn resource_list_request_payload(resource_request_id: &str) -> MessagePayload {
    MessagePayload {
        message: Message {
            schema_version: "2.0".to_owned(),
            role: Role::User,
            content: vec![ContentPart::ResourceRef {
                content: ResourceReference {
                    resource_request_id: resource_request_id.to_owned(),
                    uri: String::new(),
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

/// Builds the `cmf.resource_post_fetch` payload for `resources/templates/list`, over the merged and
/// namespaced listing the client is about to receive.
///
/// Read-only for the same reason as the prompt listing: MCP `ResourceTemplate` metadata (`title`,
/// `mime_type`, `icons`, `annotations`, `_meta`) has no CMF equivalent to be rebuilt from. Each
/// template contributes its URI template and name, plus its description as text when it has one.
pub(crate) fn resource_template_list_result_payload(
    templates: &[ResourceTemplate],
    resource_request_id: &str,
) -> MessagePayload {
    let mut content = Vec::with_capacity(templates.len());
    for template in templates {
        content.push(ContentPart::ResourceRef {
            content: ResourceReference {
                resource_request_id: resource_request_id.to_owned(),
                uri: template.uri_template.clone(),
                name: Some(template.name.clone()),
                resource_type: ResourceType::Uri,
                range_start: None,
                range_end: None,
                selector: None,
            },
        });
        if let Some(description) = &template.description {
            content.push(ContentPart::Text { text: description.clone() });
        }
    }
    MessagePayload {
        message: Message { schema_version: "2.0".to_owned(), role: Role::Assistant, content, channel: None },
    }
}

/// CMF annotation key carrying the backend a resource operation routes to.
///
/// Neither CMF resource type has a backend field the way `ToolCall.namespace` and
/// `PromptRequest.server_id` do, so the backend is exposed through `Resource.annotations` — the
/// open metadata map CMF provides for exactly this kind of attribution.
pub(crate) const RESOURCE_BACKEND_ANNOTATION: &str = "backend";

/// Builds the `cmf.resource_pre_fetch` payload for the routed resource paths — `resources/read`,
/// `resources/subscribe`, and `resources/unsubscribe`. The hook runs after routing, so
/// `resource_uri` is backend-local and `backend_name` carries the namespace, matching the
/// `call_tool` contract.
///
/// `subscribe` and `unsubscribe` have no matching post hook: both return an empty
/// acknowledgement, so a post hook would receive nothing to inspect or rewrite.
pub(crate) fn resource_request_payload(
    resource_uri: &str,
    backend_name: &str,
    resource_request_id: &str,
) -> MessagePayload {
    MessagePayload {
        message: Message {
            schema_version: "2.0".to_owned(),
            role: Role::User,
            content: vec![ContentPart::Resource {
                content: Resource {
                    resource_request_id: resource_request_id.to_owned(),
                    uri: resource_uri.to_owned(),
                    resource_type: ResourceType::Uri,
                    annotations: HashMap::from([(
                        RESOURCE_BACKEND_ANNOTATION.to_owned(),
                        Value::String(backend_name.to_owned()),
                    )]),
                    ..Resource::default()
                },
            }],
            channel: None,
        },
    }
}

/// Builds the completion pre-hook payload for a prompt reference. The argument being completed is
/// merged into the prompt arguments alongside any already-resolved context arguments.
///
/// Read-only: the argument value is a partial user keystroke, so rewriting it would return
/// completions for text the user never typed. Denial is supported.
pub(crate) fn prompt_completion_payload(
    prompt_name: &str,
    backend_name: &str,
    argument: &ArgumentInfo,
    context: Option<&HashMap<String, String>>,
    prompt_request_id: &str,
) -> MessagePayload {
    let mut arguments = completion_arguments(context);
    arguments.insert(argument.name.clone(), Value::String(argument.value.clone()));
    MessagePayload {
        message: Message {
            schema_version: "2.0".to_owned(),
            role: Role::User,
            content: vec![ContentPart::PromptRequest {
                content: PromptRequest {
                    prompt_request_id: prompt_request_id.to_owned(),
                    name: prompt_name.to_owned(),
                    arguments,
                    server_id: Some(backend_name.to_owned()),
                },
            }],
            channel: None,
        },
    }
}

/// Builds the completion pre-hook payload for a resource or template reference. CMF resource types
/// have no argument map, so the completion argument and context ride in `Resource.annotations`
/// alongside the backend. Read-only, same as the prompt form.
pub(crate) fn resource_completion_payload(
    resource_uri: &str,
    backend_name: &str,
    argument: &ArgumentInfo,
    context: Option<&HashMap<String, String>>,
    resource_request_id: &str,
) -> MessagePayload {
    let annotations = HashMap::from([
        (RESOURCE_BACKEND_ANNOTATION.to_owned(), Value::String(backend_name.to_owned())),
        (
            COMPLETION_ARGUMENT_ANNOTATION.to_owned(),
            serde_json::json!({ "name": argument.name, "value": argument.value }),
        ),
        (COMPLETION_CONTEXT_ANNOTATION.to_owned(), Value::Object(completion_arguments(context).into_iter().collect())),
    ]);
    MessagePayload {
        message: Message {
            schema_version: "2.0".to_owned(),
            role: Role::User,
            content: vec![ContentPart::Resource {
                content: Resource {
                    resource_request_id: resource_request_id.to_owned(),
                    uri: resource_uri.to_owned(),
                    resource_type: ResourceType::Uri,
                    annotations,
                    ..Resource::default()
                },
            }],
            channel: None,
        },
    }
}

/// CMF annotation keys carrying the completion argument and its resolved context on resource
/// references, which have no argument map of their own.
pub(crate) const COMPLETION_ARGUMENT_ANNOTATION: &str = "completion_argument";
pub(crate) const COMPLETION_CONTEXT_ANNOTATION: &str = "completion_context";

fn completion_arguments(context: Option<&HashMap<String, String>>) -> HashMap<String, Value> {
    context
        .map(|context| context.iter().map(|(name, value)| (name.clone(), Value::String(value.clone()))).collect())
        .unwrap_or_default()
}

/// Builds the completion post-hook payload for a prompt reference: the reference itself, followed
/// by one text part per completion value.
pub(crate) fn prompt_completion_result_payload(
    prompt_name: &str,
    values: &[String],
    prompt_request_id: &str,
) -> MessagePayload {
    let mut content = vec![ContentPart::PromptResult {
        content: PromptResult {
            prompt_request_id: prompt_request_id.to_owned(),
            prompt_name: prompt_name.to_owned(),
            messages: Vec::new(),
            content: None,
            is_error: false,
            error_message: None,
        },
    }];
    content.extend(values.iter().map(|value| ContentPart::Text { text: value.clone() }));
    MessagePayload {
        message: Message { schema_version: "2.0".to_owned(), role: Role::Assistant, content, channel: None },
    }
}

/// Builds the completion post-hook payload for a resource or template reference.
pub(crate) fn resource_completion_result_payload(
    resource_uri: &str,
    values: &[String],
    resource_request_id: &str,
) -> MessagePayload {
    let mut content = vec![ContentPart::Resource {
        content: Resource {
            resource_request_id: resource_request_id.to_owned(),
            uri: resource_uri.to_owned(),
            resource_type: ResourceType::Uri,
            ..Resource::default()
        },
    }];
    content.extend(values.iter().map(|value| ContentPart::Text { text: value.clone() }));
    MessagePayload {
        message: Message { schema_version: "2.0".to_owned(), role: Role::Assistant, content, channel: None },
    }
}

/// Applies a plugin-modified completion payload back onto the backend result.
///
/// Only the completion values are written back, and the count must match what the plugin was
/// given, so the backend's `total` and `has_more` stay accurate.
pub(crate) fn completion_result_response(
    mut original: CompleteResult,
    payload: &MessagePayload,
) -> Result<CompleteResult, ErrorData> {
    let values: Vec<String> = payload
        .message
        .content
        .iter()
        .filter_map(|part| match part {
            ContentPart::Text { text } => Some(text.clone()),
            _ => None,
        })
        .collect();
    if values.len() != original.completion.values.len() {
        return Err(invalid_payload("Plugin modified completion payload value count"));
    }
    original.completion.values = values;
    Ok(original)
}

/// Builds the `cmf.resource_post_fetch` payload for `resources/list`, over the merged and
/// namespaced listing. Read-only for the same reason as the template listing: MCP `Resource`
/// listing entries are metadata with no CMF equivalent to be rebuilt from.
pub(crate) fn resource_list_result_payload(resources: &[McpResource], resource_request_id: &str) -> MessagePayload {
    let mut content = Vec::with_capacity(resources.len());
    for resource in resources {
        content.push(ContentPart::ResourceRef {
            content: ResourceReference {
                resource_request_id: resource_request_id.to_owned(),
                uri: resource.uri.clone(),
                name: Some(resource.name.clone()),
                resource_type: ResourceType::Uri,
                range_start: None,
                range_end: None,
                selector: None,
            },
        });
        if let Some(description) = &resource.description {
            content.push(ContentPart::Text { text: description.clone() });
        }
    }
    MessagePayload {
        message: Message { schema_version: "2.0".to_owned(), role: Role::Assistant, content, channel: None },
    }
}

/// Builds the `cmf.resource_post_fetch` payload for `resources/read`, one CMF resource per returned
/// content item.
///
/// Text contents are exposed in full and can be rewritten. Blob contents are exposed as a reference
/// — URI and MIME type — but their bytes are withheld: MCP carries blobs base64-encoded while CMF
/// wants raw bytes, and cloning arbitrary binary payloads through the plugin pipeline on the hot
/// path is a memory hazard. Blob contents are never mutated.
pub(crate) fn read_resource_result_payload(contents: &[ResourceContents], resource_request_id: &str) -> MessagePayload {
    let content = contents
        .iter()
        .map(|item| ContentPart::Resource { content: resource_contents_to_cmf(item, resource_request_id) })
        .collect();
    MessagePayload {
        message: Message { schema_version: "2.0".to_owned(), role: Role::Assistant, content, channel: None },
    }
}

fn resource_contents_to_cmf(contents: &ResourceContents, resource_request_id: &str) -> Resource {
    match contents {
        ResourceContents::TextResourceContents { uri, mime_type, text, .. } => Resource {
            resource_request_id: resource_request_id.to_owned(),
            uri: uri.clone(),
            resource_type: ResourceType::Uri,
            content: Some(text.clone()),
            mime_type: mime_type.clone(),
            ..Resource::default()
        },
        ResourceContents::BlobResourceContents { uri, mime_type, .. } => Resource {
            resource_request_id: resource_request_id.to_owned(),
            uri: uri.clone(),
            resource_type: ResourceType::Blob,
            mime_type: mime_type.clone(),
            ..Resource::default()
        },
        _ => Resource { resource_request_id: resource_request_id.to_owned(), ..Resource::default() },
    }
}

/// Applies a plugin-modified `resources/read` payload back onto the backend response.
///
/// Only text content is written back, and the content count must match what the plugin was given.
/// `uri`, `mimeType`, `_meta`, and blob bytes always come from the backend response, so a plugin
/// that only observes gets a byte-identical passthrough.
pub(crate) fn read_resource_response(
    mut original: ReadResourceResult,
    payload: &MessagePayload,
) -> Result<ReadResourceResult, ErrorData> {
    let resources = payload.message.get_resources();
    if resources.len() != original.contents.len() {
        return Err(invalid_payload("Plugin modified resource payload content count"));
    }

    for (resource, original_contents) in resources.iter().zip(original.contents.iter_mut()) {
        match original_contents {
            ResourceContents::TextResourceContents { text: original_text, .. } => match &resource.content {
                Some(text) => original_text.clone_from(text),
                // Text contents are always exposed, so `None` means the plugin cleared them.
                // Restoring the backend text would fail open on redaction.
                None => return Err(invalid_payload("Plugin removed text from a resource content")),
            },
            // Blob bytes are never exposed, so having no text back is expected.
            _ if resource.content.is_none() => {},
            _ => return Err(invalid_payload("Plugin added text to a non-text resource content")),
        }
    }

    Ok(original)
}

pub(crate) fn resource_uri(payload: &MessagePayload) -> Option<String> {
    payload.message.get_resources().first().map(|resource| resource.uri.clone())
}

pub(crate) fn prompt_request_arguments(payload: &MessagePayload) -> Option<Map<String, Value>> {
    payload
        .message
        .get_prompt_requests()
        .first()
        .map(|request| request.arguments.clone().into_iter().collect::<Map<String, Value>>())
}

/// Applies a plugin-modified prompt payload back onto the backend response.
///
/// Only text content is written back. The message count must match the payload the plugin was
/// given; anything else is a structural change the gateway cannot apply safely. `description`,
/// `_meta`, message roles, and non-text blocks are always taken from the original backend
/// response, so a plugin that only observes gets a byte-identical passthrough.
pub(crate) fn prompt_result_response(
    mut original: GetPromptResult,
    payload: &MessagePayload,
) -> Result<GetPromptResult, ErrorData> {
    let prompt_results = payload.message.get_prompt_results();
    let Some(result) = prompt_results.first() else {
        return Err(invalid_payload("Plugin modified prompt payload without a prompt result"));
    };
    if result.messages.len() != original.messages.len() {
        return Err(invalid_payload("Plugin modified prompt payload message count"));
    }

    for (message, original_message) in result.messages.iter().zip(original.messages.iter_mut()) {
        let text = message.content.iter().find_map(|part| match part {
            ContentPart::Text { text } => Some(text),
            _ => None,
        });
        match &mut original_message.content {
            ContentBlock::Text(original_text) => match text {
                Some(text) => original_text.text.clone_from(text),
                // A text block is always exposed as a text part, so its absence means the plugin
                // removed it. Falling back to the backend text here would return content a
                // redaction plugin explicitly stripped, so this fails closed instead.
                None => return Err(invalid_payload("Plugin removed text from a prompt message")),
            },
            // Non-text blocks are never exposed, so having no text back is expected.
            _ if text.is_none() => {},
            _ => return Err(invalid_payload("Plugin added text to a non-text prompt message")),
        }
    }

    Ok(original)
}

fn invalid_payload(message: &'static str) -> ErrorData {
    ErrorData { code: ErrorCode::INVALID_PARAMS, message: message.into(), data: None }
}

#[cfg(test)]
mod tests {
    use super::*;

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
