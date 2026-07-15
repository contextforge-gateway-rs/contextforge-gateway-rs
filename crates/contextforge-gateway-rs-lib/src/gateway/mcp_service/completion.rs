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

    let identifier = match &request.r#ref {
        Reference::Prompt(prompt) => prompt.name.as_str(),
        Reference::Resource(resource) => resource.uri.as_str(),
        _ => return Err(ErrorData::invalid_params("Unsupported completion reference", None)),
    };

    let (service_name, service, routed_identifier) = route_identifier_to_backend(
        &session_manager,
        "complete",
        identifier,
        "Routing problem... wrong completion reference",
    )
    .await?;

    let mut routed_request = request;
    match &mut routed_request.r#ref {
        Reference::Prompt(prompt) => prompt.name = routed_identifier,
        Reference::Resource(resource) => resource.uri = routed_identifier,
        _ => return Err(ErrorData::invalid_params("Unsupported completion reference", None)),
    }
    let response = service
        .complete(routed_request)
        .await
        .map_err(|error| backend_forward_error("complete", &service_name, &error))?;
    info!("complete: backend {service_name} returned {} values", response.completion.values.len());
    Ok(response)
}
