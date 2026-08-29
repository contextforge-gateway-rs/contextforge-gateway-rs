use contextforge_data_plane_cpex::ToolPreCallResult;
use http::request::Parts;
use rmcp::{
    ErrorData, RoleServer,
    model::{CallToolRequestParams, CallToolResponse, ErrorCode, ProtocolVersion},
    service::RequestContext,
};
use tracing::{info, warn};

use super::McpService;
use crate::gateway::{
    backend_client::call_backend_tool, identifier_routing::backend_forward_error,
    mcp_call_validator::AuthorizedCallValidator, mcp_service::initialization::connect_backend_for_request,
};
use crate::mcp_standard_headers;

pub(super) async fn call_tool(
    mcp_service: &McpService,
    request: CallToolRequestParams,
    cx: RequestContext<RoleServer>,
) -> Result<CallToolResponse, ErrorData> {
    let mcp_call_validator = AuthorizedCallValidator::new("call_tool", &cx);
    let (virtual_host, _claims) = mcp_call_validator.validate_stateless()?;

    let dowstream_name = request.name.to_string();
    let Some((backend_name, tool_name)) = virtual_host.tools.get(&dowstream_name) else {
        return Err(ErrorData {
            code: ErrorCode::INVALID_PARAMS,
            message: "Routing problem... tool not found".into(),
            data: None,
        });
    };

    let backend = virtual_host.backends.get(backend_name).ok_or_else(|| ErrorData {
        code: ErrorCode::INVALID_PARAMS,
        message: "Routing problem... backend not found".into(),
        data: None,
    })?;

    if cx.protocol_version().is_some_and(|version| version >= ProtocolVersion::STANDARD_HEADERS) {
        let downstream_headers = cx
            .extensions
            .get::<Parts>()
            .map(|parts| &parts.headers)
            .ok_or_else(|| ErrorData::internal_error("Routing problem... request headers not found", None))?;
        let tool_schema = backend.tool_schemas.get(tool_name).ok_or_else(|| {
            ErrorData::header_mismatch(format!("Missing published schema for tool '{tool_name}'"), None)
        })?;
        mcp_standard_headers::validate_tool_params(downstream_headers, request.arguments.as_ref(), tool_schema)
            .map_err(|message| ErrorData::header_mismatch(message, None))?;
    }

    let backend_name = backend_name.clone();
    let pre_result = if let Some(plugin_runtime) = &mcp_service.plugin_runtime {
        plugin_runtime.before_tool_call(&request, tool_name, &backend_name).await?
    } else {
        ToolPreCallResult::unchanged()
    };
    let mut backend_service = connect_backend_for_request(mcp_service, &backend_name, backend, &cx).await?;
    let post_state = pre_result.state;
    let mut routed_request = request;
    pre_result.arguments.apply_to_request(&mut routed_request, tool_name);

    let progress_token = cx.meta.get_progress_token();
    let handle = backend_service
        .service()
        .start_tool_call(
            backend_service.peer(),
            routed_request,
            progress_token,
            tool_name.clone(),
            cx.peer.clone(),
            post_state.clone(),
        )
        .await
        .map_err(|error| backend_forward_error("call_tool", &backend_name, &error))?;
    let backend_progress_token = handle.progress_token.clone();
    let response = call_backend_tool(handle, cx.ct.clone()).await;
    backend_service.service().stop_tracking_tool_call(&backend_progress_token).await;
    if let Err(error) = backend_service.close().await {
        warn!("call_tool: backend cleanup failed backend_name = {backend_name} error = {error:?}");
    }

    let response = response.map_err(|error| backend_forward_error("call_tool", &backend_name, &error))?;
    let response = match (&mcp_service.plugin_runtime, post_state) {
        (Some(plugin_runtime), Some(post_state)) => {
            plugin_runtime.after_tool_call(tool_name, response, Some(post_state)).await?
        },
        _ => response,
    };
    info!("call_tool: backend {backend_name} completed");
    Ok(response.into())
}
