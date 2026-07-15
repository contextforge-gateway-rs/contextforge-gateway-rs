use rmcp::{
    ErrorData, RoleServer,
    model::{GetPromptRequestParams, GetPromptResponse, ListPromptsResult, PaginatedRequestParams},
    service::RequestContext,
};
use tracing::info;

use super::McpService;
use crate::gateway::{
    identifier_routing::{backend_forward_error, route_identifier_to_backend},
    list_aggregation::{fan_out_list, merge_prompts},
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
    let backend_transports: Vec<_> = session_manager.borrow_transports().await;

    let responses = fan_out_list(
        backend_transports,
        "list_prompts",
        |response: &ListPromptsResult| response.prompts.len(),
        |service| {
            let request = request.clone();
            async move { service.list_prompts(request).await }
        },
    )
    .await;

    Ok(ListPromptsResult::with_all_items(merge_prompts(responses, namespace_identifiers)))
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
        "Routing problem... wrong prompt name",
    )
    .await?;

    let mut routed_request = request;
    routed_request.name = prompt_name;
    let response = service
        .get_prompt(routed_request)
        .await
        .map_err(|error| backend_forward_error("get_prompt", &service_name, &error))?;
    info!("get_prompt: backend {service_name} returned {} messages", response.messages.len());
    Ok(response.into())
}
