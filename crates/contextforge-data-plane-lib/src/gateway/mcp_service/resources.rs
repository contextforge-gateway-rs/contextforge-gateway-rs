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
    identifier_routing::{backend_forward_error, resolve_tool_route, route_identifier_to_backend},
    mcp_call_validator::AuthorizedCallValidator,
    mcp_service::initialization::connect_backend_for_request,
    session_manager::SessionManager,
};

pub(super) async fn read_resource(
    mcp_service: &McpService,
    request: ReadResourceRequestParams,
    cx: RequestContext<RoleServer>,
) -> Result<ReadResourceResponse, ErrorData> {
    let mcp_call_validator = AuthorizedCallValidator::new("read_resource", &cx);
    let (virtual_host, _claims) = mcp_call_validator.validate_stateless()?;
    let backend_names: Vec<&str> = virtual_host.backends.keys().map(String::as_str).collect();
    let Some((backend_name, resource_uri)) = resolve_tool_route(virtual_host, &request.uri, &backend_names) else {
        return Err(ErrorData {
            code: ErrorCode::INVALID_PARAMS,
            message: "Routing problem... resource not found".into(),
            data: None,
        });
    };
    let backend_name = backend_name.to_owned();
    let resource_uri = resource_uri.to_owned();

    let backend = virtual_host.backends.get(&backend_name).ok_or_else(|| ErrorData {
        code: ErrorCode::INVALID_PARAMS,
        message: "Routing problem... backend not found".into(),
        data: None,
    })?;

    let service_name = backend_name.clone();
    let mut backend_service =
        connect_backend_for_request(mcp_service, &backend_name, backend, virtual_host.backends.len() > 1, &cx).await?;

    let mut routed_request = request;
    routed_request.uri = resource_uri;

    let response = backend_service.read_resource(routed_request).await;
    if let Err(error) = backend_service.close().await {
        tracing::warn!("read_resource: backend cleanup failed backend_name = {service_name} error = {error:?}");
    }
    let response = response.map_err(|error| backend_forward_error("read_resource", &service_name, &error))?;

    info!("read_resource: backend {service_name} returned {} contents", response.contents.len());

    Ok(response.into())
}

#[expect(deprecated, reason = "temporary RMCP v3 compatibility; subscriptions/listen migration is deferred")]
pub(super) async fn subscribe(
    mcp_service: &McpService,
    request: SubscribeRequestParams,
    cx: RequestContext<RoleServer>,
) -> Result<(), ErrorData> {
    let mcp_call_validator = AuthorizedCallValidator::new("subscribe", &cx);
    let (virtual_host, session_id, claims) = mcp_call_validator.validate()?;
    let session_manager = SessionManager::new(virtual_host, session_id, claims.sub.as_str(), &mcp_service.transports);

    let (service_name, service, resource_uri) = route_identifier_to_backend(
        &session_manager,
        "subscribe",
        &request.uri,
        "Routing problem... wrong resource name",
    )
    .await?;

    let mut routed_request = request;
    routed_request.uri = resource_uri.clone();
    service.service().track_resource_subscription(&resource_uri, cx.peer.clone()).await;

    if let Err(error) = service.subscribe(routed_request).await {
        service.service().stop_tracking_resource_subscription(&resource_uri).await;
        return Err(backend_forward_error("subscribe", &service_name, &error));
    }
    info!("subscribe: backend {service_name} completed");
    Ok(())
}

#[expect(deprecated, reason = "temporary RMCP v3 compatibility; subscriptions/listen migration is deferred")]
pub(super) async fn unsubscribe(
    mcp_service: &McpService,
    request: UnsubscribeRequestParams,
    cx: RequestContext<RoleServer>,
) -> Result<(), ErrorData> {
    let mcp_call_validator = AuthorizedCallValidator::new("unsubscribe", &cx);
    let (virtual_host, session_id, claims) = mcp_call_validator.validate()?;
    let session_manager = SessionManager::new(virtual_host, session_id, claims.sub.as_str(), &mcp_service.transports);

    let (service_name, service, resource_uri) = route_identifier_to_backend(
        &session_manager,
        "unsubscribe",
        &request.uri,
        "Routing problem... wrong resource name",
    )
    .await?;

    let mut routed_request = request;
    routed_request.uri = resource_uri.clone();
    service
        .unsubscribe(routed_request)
        .await
        .map_err(|error| backend_forward_error("unsubscribe", &service_name, &error))?;
    service.service().stop_tracking_resource_subscription(&resource_uri).await;
    info!("unsubscribe: backend {service_name} completed");
    Ok(())
}
