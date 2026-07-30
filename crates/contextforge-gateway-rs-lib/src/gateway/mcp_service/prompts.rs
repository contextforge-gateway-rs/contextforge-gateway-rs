use contextforge_gateway_rs_cpex::PromptPreFetchResult;
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

    // A listing has no single backend, so the pre hook runs once before fan-out as a deny gate and
    // the post hook sees the merged, namespaced result. Both are read-only for this path.
    let post_state = match &mcp_service.plugin_runtime {
        Some(plugin_runtime) => {
            let cursor = request.as_ref().and_then(|request| request.cursor.as_deref());
            plugin_runtime.before_list_prompts(cursor).await?
        },
        None => None,
    };

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

    let prompts = merge_prompts(responses, namespace_identifiers);
    if let (Some(plugin_runtime), Some(post_state)) = (&mcp_service.plugin_runtime, post_state) {
        plugin_runtime.after_list_prompts(&prompts, Some(post_state)).await?;
    }

    Ok(ListPromptsResult::with_all_items(prompts))
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

    // Hooks run after routing, so plugins see the backend-local prompt name and the backend that
    // owns it, matching the `call_tool` contract.
    let pre_result = if let Some(plugin_runtime) = &mcp_service.plugin_runtime {
        plugin_runtime.before_get_prompt(&request, &prompt_name, &service_name).await?
    } else {
        PromptPreFetchResult::unchanged()
    };
    let post_state = pre_result.state;
    let mut routed_request = request;
    pre_result.arguments.apply_to_request(&mut routed_request, &prompt_name);

    let response = service
        .get_prompt(routed_request)
        .await
        .map_err(|error| backend_forward_error("get_prompt", &service_name, &error))?;
    let response = match (&mcp_service.plugin_runtime, post_state) {
        (Some(plugin_runtime), Some(post_state)) => {
            plugin_runtime.after_get_prompt(&prompt_name, response, Some(post_state)).await?
        },
        _ => response,
    };
    info!("get_prompt: backend {service_name} returned {} messages", response.messages.len());
    Ok(response.into())
}
