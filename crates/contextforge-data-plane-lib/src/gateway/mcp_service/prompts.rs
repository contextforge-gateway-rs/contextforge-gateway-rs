use contextforge_data_plane_observability::PerformanceTimer;
use rmcp::{
    ErrorData, RoleServer,
    model::{GetPromptRequestParams, GetPromptResponse, ListPromptsResult, PaginatedRequestParams},
    service::RequestContext,
};
use tracing::info;

use super::McpService;
use crate::gateway::{
    identifier_routing::{backend_forward_error, route_identifier_to_backend},
    list_aggregation::{decode_gateway_cursor, fan_out_list, merge_prompts},
    mcp_call_validator::AuthorizedCallValidator,
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
    let (virtual_host, session_id, claims) = mcp_call_validator.validate()?;
    let session_manager = SessionManager::new(virtual_host, session_id, claims.sub.as_str(), &mcp_service.transports);

    let (service_name, service, prompt_name) = route_identifier_to_backend(
        &session_manager,
        "get_prompt",
        &request.name,
        "Routing problem... invalid prompt name",
    )
    .await?;

    let mut routed_request = request;
    routed_request.name = prompt_name;
    let mut timer = PerformanceTimer::external_call("Routing", "get_prompt");
    let response = service.get_prompt(routed_request).await;
    timer.record_result(&response);
    let response = response.map_err(|error| backend_forward_error("get_prompt", &service_name, &error))?;
    info!(
        component = "Routing",
        operation = "get_prompt",
        backend_name = service_name,
        message_count = response.messages.len(),
        "backend prompt request completed"
    );
    Ok(response.into())
}
