use cpex::cpex_core::cmf::{
    ContentPart, Message, MessagePayload, PromptRequest, PromptResult, Role, ToolCall, ToolResult,
};
use rmcp::model::{CallToolRequestParams, CallToolResult, ContentBlock, GetPromptRequestParams, GetPromptResult};
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
                    arguments: request.arguments.clone().unwrap_or_default().into_iter().collect(),
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
    content.extend(
        response
            .messages
            .iter()
            .filter_map(|message| message.content.as_text())
            .map(|text| ContentPart::Text { text: text.text.clone() }),
    );

    MessagePayload {
        message: Message { schema_version: "2.0".to_owned(), role: Role::Assistant, content, channel: None },
    }
}

pub(crate) fn prompt_result_response(
    mut original: GetPromptResult,
    payload: &MessagePayload,
) -> Option<GetPromptResult> {
    let mut texts = payload.message.content.iter().filter_map(|part| match part {
        ContentPart::Text { text } => Some(text),
        _ => None,
    });

    for message in &mut original.messages {
        if message.content.as_text().is_none() {
            continue;
        }
        message.content = ContentBlock::text(texts.next()?.clone());
    }

    if texts.next().is_some() {
        return None;
    }
    Some(original)
}

#[cfg(test)]
mod tests {
    use rmcp::model::{PromptMessage, Role as McpRole};

    use super::*;

    #[test]
    fn prompt_result_response_rejects_added_text() {
        let original = GetPromptResult::new(vec![PromptMessage::new_text(McpRole::User, "review of weather")]);
        let mut payload = prompt_result_payload(&original, "review", "prompt-1");
        payload.message.content.push(ContentPart::Text { text: "extra".to_owned() });

        assert!(prompt_result_response(original, &payload).is_none());
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
