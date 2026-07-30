use contextforge_gateway_rs_cpex::{CompletionRequest, CompletionTarget};
use rmcp::{
    ErrorData, RoleServer,
    model::{CompleteRequestParams, CompleteResult, Reference},
    service::RequestContext,
};
use tracing::info;

use super::McpService;
use crate::gateway::{
    identifier_routing::{backend_forward_error, route_identifier_to_backend},
    mcp_call_validator::AuthorizedCallValidator,
    session_manager::SessionManager,
    session_store::UserSessionStore,
};

pub(super) async fn complete<T>(
    mcp_service: &McpService<T>,
    request: CompleteRequestParams,
    cx: RequestContext<RoleServer>,
) -> Result<CompleteResult, ErrorData>
where
    T: UserSessionStore + Send + Sync + 'static,
{
    let mcp_call_validator = AuthorizedCallValidator::new("complete", &cx);
    let (virtual_host, session_id, claims) = mcp_call_validator.validate()?;
    let session_manager = SessionManager::new(virtual_host, session_id, claims.sub.as_str(), &mcp_service.transports);

    // CPEX has no completion hook, so a completion runs the hooks of whatever is being completed.
    let (identifier, target) = match &request.r#ref {
        Reference::Prompt(prompt) => (prompt.name.as_str(), CompletionTarget::Prompt),
        Reference::Resource(resource) => (resource.uri.as_str(), CompletionTarget::Resource),
        _ => return Err(ErrorData::invalid_params("Unsupported completion reference", None)),
    };

    let (service_name, service, routed_identifier) = route_identifier_to_backend(
        &session_manager,
        "complete",
        identifier,
        "Routing problem... wrong completion reference",
    )
    .await?;

    // Hooks run after routing, so plugins see the backend-local identifier and the backend that
    // owns it. The pre hook is deny-only: the argument value is a partial keystroke, so rewriting
    // it would return completions for text the user never typed.
    let post_state = match &mcp_service.plugin_runtime {
        Some(plugin_runtime) => {
            let context = request.context.as_ref().and_then(|context| context.arguments.as_ref());
            plugin_runtime
                .before_complete(&CompletionRequest {
                    target,
                    identifier: &routed_identifier,
                    backend_name: &service_name,
                    argument: &request.argument,
                    context,
                })
                .await?
        },
        None => None,
    };

    let mut routed_request = request;
    match &mut routed_request.r#ref {
        Reference::Prompt(prompt) => prompt.name.clone_from(&routed_identifier),
        Reference::Resource(resource) => resource.uri.clone_from(&routed_identifier),
        _ => return Err(ErrorData::invalid_params("Unsupported completion reference", None)),
    }
    let response = service
        .complete(routed_request)
        .await
        .map_err(|error| backend_forward_error("complete", &service_name, &error))?;
    let response = match (&mcp_service.plugin_runtime, post_state) {
        (Some(plugin_runtime), Some(post_state)) => {
            plugin_runtime.after_complete(target, &routed_identifier, response, Some(post_state)).await?
        },
        _ => response,
    };
    info!("complete: backend {service_name} returned {} values", response.completion.values.len());
    Ok(response)
}
