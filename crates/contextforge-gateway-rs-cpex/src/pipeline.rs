use cpex::cpex_core::cmf::MessagePayload;
use cpex::cpex_core::executor::PipelineResult;
use rmcp::{
    ErrorData,
    model::{CallToolResult, CompleteResult, ErrorCode, GetPromptResult, ReadResourceResult},
    serde::de::DeserializeOwned,
};
use tracing::warn;

use crate::{
    PromptArgumentsUpdate, ResourceUriUpdate, ToolArgumentsUpdate,
    cmf::{
        completion_result_response, prompt_request_arguments, prompt_result_response, read_resource_response,
        resource_uri, tool_call_arguments, tool_result_content, tool_result_response,
    },
};

pub(crate) fn modified_message_payload(result: &PipelineResult) -> Option<&MessagePayload> {
    result.modified_payload.as_ref().and_then(|payload| payload.as_any().downcast_ref::<MessagePayload>())
}

pub(crate) fn effective_pre_args(
    original_args: Option<&serde_json::Map<String, serde_json::Value>>,
    pre_result: &PipelineResult,
) -> Result<ToolArgumentsUpdate, ErrorData> {
    let Some(modified_payload) = modified_message_payload(pre_result) else {
        return Ok(ToolArgumentsUpdate::Unchanged);
    };

    let Some(arguments) = tool_call_arguments(modified_payload) else {
        return Err(ErrorData {
            code: ErrorCode::INVALID_PARAMS,
            message: "Plugin modified tool payload without a tool call".into(),
            data: None,
        });
    };

    if original_args == Some(&arguments) || (original_args.is_none() && arguments.is_empty()) {
        Ok(ToolArgumentsUpdate::Unchanged)
    } else {
        Ok(ToolArgumentsUpdate::Replace(Some(arguments.clone())))
    }
}

pub(crate) fn effective_post_result(original: CallToolResult, result: &PipelineResult) -> CallToolResult {
    match modified_message_payload(result) {
        Some(payload) => tool_result_response(original, payload),
        None => original,
    }
}

pub(crate) fn effective_pre_prompt_args(
    original_args: Option<&serde_json::Map<String, serde_json::Value>>,
    pre_result: &PipelineResult,
) -> Result<PromptArgumentsUpdate, ErrorData> {
    let Some(modified_payload) = modified_message_payload(pre_result) else {
        return Ok(PromptArgumentsUpdate::Unchanged);
    };

    let Some(arguments) = prompt_request_arguments(modified_payload) else {
        return Err(ErrorData {
            code: ErrorCode::INVALID_PARAMS,
            message: "Plugin modified prompt payload without a prompt request".into(),
            data: None,
        });
    };

    if original_args == Some(&arguments) || (original_args.is_none() && arguments.is_empty()) {
        Ok(PromptArgumentsUpdate::Unchanged)
    } else {
        Ok(PromptArgumentsUpdate::Replace(Some(arguments)))
    }
}

pub(crate) fn effective_post_prompt_result(
    original: GetPromptResult,
    result: &PipelineResult,
) -> Result<GetPromptResult, ErrorData> {
    match modified_message_payload(result) {
        Some(payload) => prompt_result_response(original, payload),
        None => Ok(original),
    }
}

pub(crate) fn effective_post_completion_result(
    original: CompleteResult,
    result: &PipelineResult,
) -> Result<CompleteResult, ErrorData> {
    match modified_message_payload(result) {
        Some(payload) => completion_result_response(original, payload),
        None => Ok(original),
    }
}

pub(crate) fn effective_post_read_resource(
    original: ReadResourceResult,
    result: &PipelineResult,
) -> Result<ReadResourceResult, ErrorData> {
    match modified_message_payload(result) {
        Some(payload) => read_resource_response(original, payload),
        None => Ok(original),
    }
}

pub(crate) fn effective_pre_resource_uri(
    original_uri: &str,
    pre_result: &PipelineResult,
) -> Result<ResourceUriUpdate, ErrorData> {
    let Some(modified_payload) = modified_message_payload(pre_result) else {
        return Ok(ResourceUriUpdate::Unchanged);
    };

    let Some(uri) = resource_uri(modified_payload) else {
        return Err(ErrorData {
            code: ErrorCode::INVALID_PARAMS,
            message: "Plugin modified resource payload without a resource".into(),
            data: None,
        });
    };

    if uri == original_uri { Ok(ResourceUriUpdate::Unchanged) } else { Ok(ResourceUriUpdate::Replace(uri)) }
}

pub(crate) fn effective_post_json<T>(original: T, result: &PipelineResult) -> Result<T, ErrorData>
where
    T: DeserializeOwned,
{
    let Some(payload) = modified_message_payload(result) else {
        return Ok(original);
    };
    let Some(content) = tool_result_content(payload) else {
        return Err(ErrorData {
            code: ErrorCode::INVALID_PARAMS,
            message: "Plugin modified stream event payload without a tool result".into(),
            data: None,
        });
    };
    serde_json::from_value::<T>(content).map_err(|error| ErrorData {
        code: ErrorCode::INVALID_PARAMS,
        message: format!("Plugin modified stream event payload with invalid JSON: {error}").into(),
        data: None,
    })
}

/// Maps a plugin denial onto an MCP error. `denied` names the denied operation for the client-facing
/// message, e.g. `"tool call"` or `"prompt fetch"`.
pub(crate) fn plugin_denied_error(result: PipelineResult, denied: &str) -> ErrorData {
    let code = result
        .violation
        .and_then(|violation| {
            warn!("Plugin denied {denied}: code={} plugin={:?}", violation.code, violation.plugin_name);
            violation.proto_error_code.and_then(|code| i32::try_from(code).ok()).map(ErrorCode)
        })
        .unwrap_or(ErrorCode::INVALID_REQUEST);

    ErrorData { code, message: format!("Plugin denied {denied}").into(), data: None }
}

pub(crate) fn log_pipeline_errors(hook: &'static str, result: &PipelineResult) {
    for error in &result.errors {
        warn!(
            hook,
            plugin = error.plugin_name,
            code = error.code.as_deref().unwrap_or(""),
            proto_error_code = error.proto_error_code,
            "CPEX plugin soft error"
        );
    }
}
