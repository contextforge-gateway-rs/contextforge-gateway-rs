use contextforge_gateway_rs_cpex::ToolPreCallResult;
use rmcp::{
    ErrorData, RoleServer,
    model::{CallToolRequestParams, CallToolResponse, ErrorCode, ListToolsResult, PaginatedRequestParams},
    service::RequestContext,
};
use tracing::info;

use super::McpService;
use crate::gateway::{
    backend_client::call_backend_tool,
    identifier_routing::{backend_forward_error, resolve_backend, resolve_tool_route},
    list_aggregation::{fan_out_list, merge_tools},
    mcp_call_validator::AuthorizedCallValidator,
    session_manager::SessionManager,
    session_store::UserSessionStore,
};

pub(super) async fn list_tools<T>(
    mcp_service: &McpService<T>,
    request: Option<PaginatedRequestParams>,
    cx: RequestContext<RoleServer>,
) -> Result<ListToolsResult, ErrorData>
where
    T: UserSessionStore + Send + Sync + 'static,
{
    let mcp_call_validator = AuthorizedCallValidator::new("list_tools", &cx);
    let (virtual_host, session_id, claims) = mcp_call_validator.validate()?;
    let session_manager = SessionManager::new(virtual_host, session_id, claims.sub.as_str(), &mcp_service.transports);
    let backend_transports: Vec<_> = session_manager.borrow_transports().await;

    let responses = fan_out_list(
        backend_transports,
        "list_tools",
        |response: &ListToolsResult| response.tools.len(),
        |service| {
            let request = request.clone();
            async move { service.list_tools(request).await }
        },
    )
    .await;

    Ok(ListToolsResult::with_all_items(merge_tools(responses, virtual_host)))
}

pub(super) async fn call_tool<T>(
    mcp_service: &McpService<T>,
    request: CallToolRequestParams,
    cx: RequestContext<RoleServer>,
) -> Result<CallToolResponse, ErrorData>
where
    T: UserSessionStore + Send + Sync + 'static,
{
    let mcp_call_validator = AuthorizedCallValidator::new("call_tool", &cx);
    let (virtual_host, session_id, claims) = mcp_call_validator.validate()?;
    let session_manager = SessionManager::new(virtual_host, session_id, claims.sub.as_str(), &mcp_service.transports);

    let backend_names = session_manager.get_backend_names();

    let Some((backend_name, tool_name)) = resolve_tool_route(virtual_host, &request.name, &backend_names) else {
        return Err(ErrorData {
            code: ErrorCode::INVALID_PARAMS,
            message: "Routing problem... tool not found".into(),
            data: None,
        });
    };
    let backend_name = backend_name.to_owned();
    let tool_name = tool_name.to_owned();

    let (service_name, backend_service) = resolve_backend(&session_manager, "call_tool", &backend_name).await?;

    let pre_result = if let Some(plugin_runtime) = &mcp_service.plugin_runtime {
        plugin_runtime.before_tool_call(&request, &tool_name, &service_name).await?
    } else {
        ToolPreCallResult::unchanged()
    };
    let post_state = pre_result.state;
    let mut routed_request = request;
    pre_result.arguments.apply_to_request(&mut routed_request, &tool_name);

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
        .map_err(|error| backend_forward_error("call_tool", &service_name, &error))?;
    let backend_progress_token = handle.progress_token.clone();
    let response = call_backend_tool(handle, cx.ct.clone()).await;
    backend_service.service().stop_tracking_tool_call(&backend_progress_token).await;

    let response = response.map_err(|error| backend_forward_error("call_tool", &service_name, &error))?;
    let response = match (&mcp_service.plugin_runtime, post_state) {
        (Some(plugin_runtime), Some(post_state)) => {
            plugin_runtime.after_tool_call(&tool_name, response, Some(post_state)).await?
        },
        _ => response,
    };
    info!("call_tool: backend {service_name} completed");
    Ok(response.into())
}
