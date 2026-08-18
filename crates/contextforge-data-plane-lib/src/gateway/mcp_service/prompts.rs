use contextforge_data_plane_cpex::PromptPreFetchResult;
use rmcp::{
    ErrorData, RoleServer,
    model::{ErrorCode, GetPromptRequestParams, GetPromptResponse, ListPromptsResult, PaginatedRequestParams},
    service::RequestContext,
};
use tracing::info;

use super::McpService;
use crate::gateway::{
    identifier_routing::{backend_forward_error, resolve_tool_route},
    list_aggregation::{decode_gateway_cursor, fan_out_list, merge_prompts},
    mcp_call_validator::AuthorizedCallValidator,
    mcp_service::initialization::connect_backend_for_request,
    session_manager::SessionManager,
    session_store::UserSessionStore,
};

pub(super) async fn list_prompts<T>(
    mcp_service: &McpService<T>,
    request: Option<PaginatedRequestParams>,
    cx: RequestContext<RoleServer>,
) -> Result<ListPromptsResult, ErrorData>
where
    T: UserSessionStore + Send + Sync + 'static,
{
    let mcp_call_validator = AuthorizedCallValidator::new("list_prompts", &cx);
    let (virtual_host, session_id, claims) = mcp_call_validator.validate()?;
    let namespace_identifiers = virtual_host.backends.len() > 1;

    let session_manager = SessionManager::new(virtual_host, session_id, claims.sub.as_str(), &mcp_service.transports);
    let all_transports: Vec<_> = session_manager.borrow_transports().await;

    let gateway_cursor = decode_gateway_cursor(request.as_ref().and_then(|r| r.cursor.as_deref()), "list_prompts")?;
    let backend_transports: Vec<_> = if request.as_ref().and_then(|r| r.cursor.as_ref()).is_some() {
        all_transports.into_iter().filter(|b| gateway_cursor.backends.contains_key(&b.name)).collect()
    } else {
        all_transports
    };

    let responses = fan_out_list(
        backend_transports,
        "list_prompts",
        |response: &ListPromptsResult| response.prompts.len(),
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
                service.list_prompts(backend_req).await
            }
        },
    )
    .await;

    let (prompts, next_cursor) = merge_prompts(responses, namespace_identifiers, &gateway_cursor, "list_prompts");
    let mut result = ListPromptsResult::with_all_items(prompts);
    result.next_cursor = next_cursor;
    Ok(result)
}

pub(super) async fn get_prompt<T>(
    mcp_service: &McpService<T>,
    request: GetPromptRequestParams,
    cx: RequestContext<RoleServer>,
) -> Result<GetPromptResponse, ErrorData>
where
    T: UserSessionStore + Send + Sync + 'static,
{
    let mcp_call_validator = AuthorizedCallValidator::new("get_prompt", &cx);
    let (virtual_host, _claims) = mcp_call_validator.validate_stateless()?;
    let backend_names: Vec<&str> = virtual_host.backends.keys().map(String::as_str).collect();
    let Some((backend_name, prompt_name)) = resolve_tool_route(virtual_host, &request.name, &backend_names) else {
        return Err(ErrorData {
            code: ErrorCode::INVALID_PARAMS,
            message: "Routing problem... promtp not found".into(),
            data: None,
        });
    };

    let backend_name = backend_name.to_owned();
    let prompt_name = prompt_name.to_owned();

    let backend = virtual_host.backends.get(&backend_name).ok_or_else(|| ErrorData {
        code: ErrorCode::INVALID_PARAMS,
        message: "Routing problem... backend not found".into(),
        data: None,
    })?;

    let service_name = backend_name.clone();
    let backend_service =
        connect_backend_for_request(mcp_service, &backend_name, backend, virtual_host.backends.len() > 1, &cx).await?;

    let pre_result = if let Some(plugin_runtime) = &mcp_service.plugin_runtime {
        plugin_runtime.before_get_prompt(&request, &prompt_name, &service_name).await?
    } else {
        PromptPreFetchResult::unchanged()
    };
    let mut routed_request = request;
    pre_result.arguments.apply_to_request(&mut routed_request, &prompt_name);
    let response = backend_service
        .get_prompt(routed_request)
        .await
        .map_err(|error| backend_forward_error("get_prompt", &service_name, &error))?;
    info!("get_prompt: backend {service_name} returned {} messages", response.messages.len());
    let response = if let Some(plugin_runtime) = &mcp_service.plugin_runtime {
        plugin_runtime.after_get_prompt(&prompt_name, response, pre_result.state).await?
    } else {
        response
    };
    Ok(response.into())
}
