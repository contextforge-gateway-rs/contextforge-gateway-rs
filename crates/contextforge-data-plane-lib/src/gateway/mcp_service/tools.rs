use contextforge_data_plane_cpex::ToolPreCallResult;
use rmcp::{
    ErrorData, RoleServer,
    model::{CallToolRequestParams, CallToolResponse, ErrorCode, ListToolsResult, PaginatedRequestParams},
    service::RequestContext,
};
use tracing::{info, warn};

use super::McpService;
use crate::gateway::{
    backend_client::call_backend_tool,
    identifier_routing::{backend_forward_error, resolve_tool_route},
    list_aggregation::{decode_gateway_cursor, fan_out_list, merge_tools},
    mcp_call_validator::AuthorizedCallValidator,
    mcp_service::initialization::connect_backend_for_request,
    session_manager::SessionManager,
};

pub(super) async fn list_tools(
    mcp_service: &McpService,
    request: Option<PaginatedRequestParams>,
    cx: RequestContext<RoleServer>,
) -> Result<ListToolsResult, ErrorData>
{
    let mcp_call_validator = AuthorizedCallValidator::new("list_tools", &cx);
    let (virtual_host, session_id, claims) = mcp_call_validator.validate()?;
    let session_manager = SessionManager::new(virtual_host, session_id, claims.sub.as_str(), &mcp_service.transports);
    let all_transports: Vec<_> = session_manager.borrow_transports().await;

    let gateway_cursor = decode_gateway_cursor(request.as_ref().and_then(|r| r.cursor.as_deref()), "list_tools")?;
    // On resume, skip backends already exhausted in the prior page.
    // ponytail: topology changes between pages silently drop removed backends;
    //           add a cursor version field if reconfiguration stability matters.
    let backend_transports: Vec<_> = if request.as_ref().and_then(|r| r.cursor.as_ref()).is_some() {
        all_transports.into_iter().filter(|b| gateway_cursor.backends.contains_key(&b.name)).collect()
    } else {
        all_transports
    };

    let responses = fan_out_list(
        backend_transports,
        "list_tools",
        |response: &ListToolsResult| response.tools.len(),
        |name, service| {
            let cursor = gateway_cursor.backends.get(&name).cloned();
            let req = request.clone();
            async move {
                let backend_req = match cursor {
                    Some(c) => {
                        let mut r = req.unwrap_or_default();
                        r.cursor = Some(c);
                        Some(r)
                    },
                    None => req,
                };
                service.list_tools(backend_req).await
            }
        },
    )
    .await;

    let (tools, next_cursor) = merge_tools(responses, virtual_host, &gateway_cursor, "list_tools");
    let mut result = ListToolsResult::with_all_items(tools);
    result.next_cursor = next_cursor;
    Ok(result)
}

pub(super) async fn call_tool(
    mcp_service: &McpService,
    request: CallToolRequestParams,
    cx: RequestContext<RoleServer>,
) -> Result<CallToolResponse, ErrorData>
{
    let mcp_call_validator = AuthorizedCallValidator::new("call_tool", &cx);
    let (virtual_host, _claims) = mcp_call_validator.validate_stateless()?;
    let backend_names: Vec<&str> = virtual_host.backends.keys().map(String::as_str).collect();
    let Some((backend_name, tool_name)) = resolve_tool_route(virtual_host, &request.name, &backend_names) else {
        return Err(ErrorData {
            code: ErrorCode::INVALID_PARAMS,
            message: "Routing problem... tool not found".into(),
            data: None,
        });
    };
    let backend_name = backend_name.to_owned();
    let tool_name = tool_name.to_owned();
    let backend = virtual_host.backends.get(&backend_name).ok_or_else(|| ErrorData {
        code: ErrorCode::INVALID_PARAMS,
        message: "Routing problem... backend not found".into(),
        data: None,
    })?;
    let service_name = backend_name.clone();
    let mut backend_service =
        connect_backend_for_request(mcp_service, &backend_name, backend, virtual_host.backends.len() > 1, &cx).await?;

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
    if let Err(error) = backend_service.close().await {
        warn!("call_tool: backend cleanup failed backend_name = {service_name} error = {error:?}");
    }

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
