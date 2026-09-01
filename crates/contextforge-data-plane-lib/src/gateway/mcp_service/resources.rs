use rmcp::{
    ErrorData, RoleServer,
    model::{
        ErrorCode, ReadResourceRequestParams, ReadResourceResponse, SubscribeRequestParams, UnsubscribeRequestParams,
    },
    service::RequestContext,
};
use tracing::info;

use super::McpService;
use crate::gateway::{
    mcp_call_validator::AuthorizedCallValidator, mcp_service::initialization::connect_backend_for_request,
    routing_error::backend_forward_error,
};

pub(super) async fn read_resource(
    mcp_service: &McpService,
    request: ReadResourceRequestParams,
    cx: RequestContext<RoleServer>,
) -> Result<ReadResourceResponse, ErrorData> {
    let mcp_call_validator = AuthorizedCallValidator::new("read_resource", &cx);
    let (virtual_host, _claims) = mcp_call_validator.validate_stateless()?;
    let dowstream_name = request.uri.clone();

    let Some(route) = virtual_host.resources.get(&dowstream_name) else {
        return Err(ErrorData {
            code: ErrorCode::INVALID_PARAMS,
            message: "Routing problem... resource not found".into(),
            data: None,
        });
    };

    let backend_name = route.backend_name.clone();
    let resource_uri = route.upstream_name.clone();

    let backend = virtual_host.backends.get(&backend_name).ok_or_else(|| ErrorData {
        code: ErrorCode::INVALID_PARAMS,
        message: "Routing problem... backend not found".into(),
        data: None,
    })?;

    let mut backend_service = connect_backend_for_request(mcp_service, &backend_name, backend, &cx).await?;
    let mut routed_request = request;

    routed_request.uri = resource_uri.clone();
    let response = backend_service.read_resource(routed_request).await;
    if let Err(error) = backend_service.close().await {
        tracing::warn!("read_resource: backend cleanup failed backend_name = {backend_name} error = {error:?}");
    }
    let response = response.map_err(|error| backend_forward_error("read_resource", &backend_name, &error))?;

    info!("read_resource: backend {backend_name} returned {} contents", response.contents.len());

    Ok(response.into())
}

#[allow(clippy::unused_async)]
pub(super) async fn subscribe(
    _: &McpService,
    _: SubscribeRequestParams,
    _: RequestContext<RoleServer>,
) -> Result<(), ErrorData> {
    Err(ErrorData {
        code: ErrorCode::INVALID_REQUEST,
        message: "Fan out not supported at the moment. Go to control plane".into(),
        data: None,
    })
}

#[allow(clippy::unused_async)]
pub(super) async fn unsubscribe(
    _: &McpService,
    _: UnsubscribeRequestParams,
    _: RequestContext<RoleServer>,
) -> Result<(), ErrorData> {
    Err(ErrorData {
        code: ErrorCode::INVALID_REQUEST,
        message: "Fan out not supported at the moment. Go to control plane".into(),
        data: None,
    })
}
