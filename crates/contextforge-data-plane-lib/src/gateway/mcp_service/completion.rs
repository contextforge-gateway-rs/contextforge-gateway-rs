use rmcp::{
    ErrorData, RoleServer,
    model::{CompleteRequestParams, CompleteResult, ErrorCode, Reference},
    service::RequestContext,
};
use tracing::info;

use crate::gateway::{
    mcp_call_validator::AuthorizedCallValidator, mcp_service::initialization::connect_backend_for_request,
    routing_error::backend_forward_error,
};

use super::McpService;

#[allow(clippy::unused_async)]
pub(super) async fn complete(
    mcp_service: &McpService,
    request: CompleteRequestParams,
    cx: RequestContext<RoleServer>,
) -> Result<CompleteResult, ErrorData> {
    let mcp_call_validator = AuthorizedCallValidator::new("complete", &cx);
    let (virtual_host, _claims) = mcp_call_validator.validate_stateless()?;

    let route = match &request.r#ref {
        Reference::Prompt(prompt) => virtual_host.prompts.get(&prompt.name),
        Reference::Resource(resource) => {
            virtual_host.resource_templates.get(&resource.uri).or_else(|| virtual_host.resources.get(&resource.uri))
        },
        _ => None,
    };
    let Some(route) = route else {
        return Err(ErrorData {
            code: ErrorCode::INVALID_PARAMS,
            message: "Routing problem... completion not found".into(),
            data: None,
        });
    };

    let backend_name = route.backend_name.clone();
    let completion_name = route.upstream_name.clone();

    let backend = virtual_host.backends.get(&backend_name).ok_or_else(|| ErrorData {
        code: ErrorCode::INVALID_PARAMS,
        message: "Routing problem... backend not found".into(),
        data: None,
    })?;

    let service_name = backend_name.clone();
    let mut backend_service = connect_backend_for_request(mcp_service, &backend_name, backend, &cx).await?;

    let mut routed_request = request;
    match &mut routed_request.r#ref {
        Reference::Prompt(prompt) => prompt.name = completion_name,
        Reference::Resource(resource) => resource.uri = completion_name,
        _ => {},
    }

    let response = backend_service.complete(routed_request).await;

    if let Err(error) = backend_service.close().await {
        tracing::warn!("complete: backend cleanup failed backend_name = {service_name} error = {error:?}");
    }

    let response = response.map_err(|error| backend_forward_error("complete", &service_name, &error))?;

    info!("complete: backend {service_name} returned {} contents", response.completion.values.len());

    Ok(response)
}
