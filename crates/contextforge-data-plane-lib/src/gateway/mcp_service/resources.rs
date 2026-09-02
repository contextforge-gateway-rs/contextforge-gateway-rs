use contextforge_data_plane_cpex::{HookTarget, ResourcePreFetchResult, ScopedMcpHook};
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
    identifier_routing::{backend_forward_error, resolve_resources_route},
    mcp_call_validator::AuthorizedCallValidator,
    mcp_service::initialization::connect_backend_for_request,
    plugin_context::{build_plugin_context, require_plugin_binding},
};

pub(super) async fn read_resource(
    mcp_service: &McpService,
    request: ReadResourceRequestParams,
    cx: RequestContext<RoleServer>,
) -> Result<ReadResourceResponse, ErrorData> {
    let mcp_call_validator = AuthorizedCallValidator::new("read_resource", &cx);
    let authorized = mcp_call_validator.validate_stateless()?;
    let virtual_host = authorized.virtual_host;
    let downstream_resource_uri = request.uri.clone();
    let backend_names: Vec<&str> = virtual_host.backends.keys().map(String::as_str).collect();
    let Some((backend_name, resource_uri)) = resolve_resources_route(virtual_host, &request.uri, &backend_names)
        .map_err(|e| ErrorData {
            code: ErrorCode::INVALID_PARAMS,
            message: format!("Routing problem... {e}").into(),
            data: None,
        })?
    else {
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
    let pre_result = if let Some(plugin_runtime) = &mcp_service.plugin_runtime {
        let plugin_names = require_plugin_binding(
            &virtual_host.plugin_bindings.revision,
            virtual_host.plugin_bindings.resource_plugins(&backend_name, &resource_uri),
        )?;
        let context = build_plugin_context(
            &authorized,
            "resources/read",
            &downstream_resource_uri,
            HookTarget::Resource { uri: resource_uri.clone(), backend: backend_name.clone() },
        );
        let scope = ScopedMcpHook::new(&virtual_host.plugin_bindings.revision, plugin_names, context);
        plugin_runtime.before_read_resource(&resource_uri, scope).await?
    } else {
        ResourcePreFetchResult::unchanged()
    };
    let mut backend_service = connect_backend_for_request(mcp_service, &backend_name, backend, &cx).await?;

    let mut routed_request = request;
    routed_request.uri = resource_uri;

    let response = backend_service.read_resource(routed_request).await;
    if let Err(error) = backend_service.close().await {
        tracing::warn!("read_resource: backend cleanup failed backend_name = {service_name} error = {error:?}");
    }
    let response = response.map_err(|error| backend_forward_error("read_resource", &service_name, &error))?;
    let response = if let Some(plugin_runtime) = &mcp_service.plugin_runtime {
        plugin_runtime.after_read_resource(response, pre_result.state).await?
    } else {
        response
    };

    info!("read_resource: backend {service_name} returned {} contents", response.contents.len());

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
