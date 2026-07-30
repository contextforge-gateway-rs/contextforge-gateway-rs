use contextforge_gateway_rs_cpex::ResourcePreFetchResult;
use rmcp::{
    ErrorData, RoleServer,
    model::{
        ListResourceTemplatesResult, ListResourcesResult, PaginatedRequestParams, ReadResourceRequestParams,
        ReadResourceResponse, SubscribeRequestParams, UnsubscribeRequestParams,
    },
    service::RequestContext,
};
use tracing::info;

use super::McpService;
use crate::gateway::{
    identifier_routing::{backend_forward_error, route_identifier_to_backend},
    list_aggregation::{decode_gateway_cursor, fan_out_list, merge_resource_templates, merge_resources},
    mcp_call_validator::AuthorizedCallValidator,
    session_manager::SessionManager,
    session_store::UserSessionStore,
};

pub(super) async fn list_resources<T>(
    mcp_service: &McpService<T>,
    request: Option<PaginatedRequestParams>,
    cx: RequestContext<RoleServer>,
) -> Result<ListResourcesResult, ErrorData>
where
    T: UserSessionStore + Send + Sync + 'static,
{
    let mcp_call_validator = AuthorizedCallValidator::new("list_resources", &cx);
    let (virtual_host, session_id, claims) = mcp_call_validator.validate()?;
    let namespace_identifiers = virtual_host.backends.len() > 1;

    let session_manager = SessionManager::new(virtual_host, session_id, claims.sub.as_str(), &mcp_service.transports);

    // A listing has no single backend, so the pre hook runs once before fan-out as a deny gate and
    // the post hook sees the merged, namespaced page. Both are read-only for this path.
    let post_state = match &mcp_service.plugin_runtime {
        Some(plugin_runtime) => plugin_runtime.before_list_resources().await?,
        None => None,
    };

    let all_transports: Vec<_> = session_manager.borrow_transports().await;

    let gateway_cursor = decode_gateway_cursor(request.as_ref().and_then(|r| r.cursor.as_deref()), "list_resources")?;
    let backend_transports: Vec<_> = if request.as_ref().and_then(|r| r.cursor.as_ref()).is_some() {
        all_transports.into_iter().filter(|b| gateway_cursor.backends.contains_key(&b.name)).collect()
    } else {
        all_transports
    };

    let responses = fan_out_list(
        backend_transports,
        "list_resources",
        |response: &ListResourcesResult| response.resources.len(),
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
                service.list_resources(backend_req).await
            }
        },
    )
    .await;

    let (resources, next_cursor) = merge_resources(responses, namespace_identifiers, &gateway_cursor, "list_resources");
    if let (Some(plugin_runtime), Some(post_state)) = (&mcp_service.plugin_runtime, post_state) {
        plugin_runtime.after_list_resources(&resources, Some(post_state)).await?;
    }

    let mut result = ListResourcesResult::with_all_items(resources);
    result.next_cursor = next_cursor;
    Ok(result)
}

pub(super) async fn read_resource<T>(
    mcp_service: &McpService<T>,
    request: ReadResourceRequestParams,
    cx: RequestContext<RoleServer>,
) -> Result<ReadResourceResponse, ErrorData>
where
    T: UserSessionStore + Send + Sync + 'static,
{
    let mcp_call_validator = AuthorizedCallValidator::new("read_resource", &cx);
    let (virtual_host, session_id, claims) = mcp_call_validator.validate()?;
    let session_manager = SessionManager::new(virtual_host, session_id, claims.sub.as_str(), &mcp_service.transports);

    let (service_name, service, resource_uri) = route_identifier_to_backend(
        &session_manager,
        "read_resource",
        &request.uri,
        "Routing problem... wrong resource name",
    )
    .await?;

    // Hooks run after routing, so plugins see the backend-local URI and the backend that owns it.
    let pre_result = match &mcp_service.plugin_runtime {
        Some(plugin_runtime) => plugin_runtime.before_read_resource(&resource_uri, &service_name).await?,
        None => ResourcePreFetchResult::unchanged(),
    };
    let post_state = pre_result.state;
    let resource_uri = pre_result.uri.apply_to(resource_uri);

    let mut routed_request = request;
    routed_request.uri = resource_uri;
    let response = service
        .read_resource(routed_request)
        .await
        .map_err(|error| backend_forward_error("read_resource", &service_name, &error))?;
    let response = match (&mcp_service.plugin_runtime, post_state) {
        (Some(plugin_runtime), Some(post_state)) => {
            plugin_runtime.after_read_resource(response, Some(post_state)).await?
        },
        _ => response,
    };
    info!("read_resource: backend {service_name} returned {} contents", response.contents.len());
    Ok(response.into())
}

pub(super) async fn list_resource_templates<T>(
    mcp_service: &McpService<T>,
    request: Option<PaginatedRequestParams>,
    cx: RequestContext<RoleServer>,
) -> Result<ListResourceTemplatesResult, ErrorData>
where
    T: UserSessionStore + Send + Sync + 'static,
{
    let mcp_call_validator = AuthorizedCallValidator::new("list_resource_templates", &cx);
    let (virtual_host, session_id, claims) = mcp_call_validator.validate()?;
    let namespace_identifiers = virtual_host.backends.len() > 1;

    let session_manager = SessionManager::new(virtual_host, session_id, claims.sub.as_str(), &mcp_service.transports);

    // A listing has no single backend, so the pre hook runs once before fan-out as a deny gate and
    // the post hook sees the merged, namespaced page. Both are read-only for this path.
    let post_state = match &mcp_service.plugin_runtime {
        Some(plugin_runtime) => plugin_runtime.before_list_resource_templates().await?,
        None => None,
    };

    let all_transports: Vec<_> = session_manager.borrow_transports().await;

    let gateway_cursor =
        decode_gateway_cursor(request.as_ref().and_then(|r| r.cursor.as_deref()), "list_resource_templates")?;
    let backend_transports: Vec<_> = if request.as_ref().and_then(|r| r.cursor.as_ref()).is_some() {
        all_transports.into_iter().filter(|b| gateway_cursor.backends.contains_key(&b.name)).collect()
    } else {
        all_transports
    };

    let responses = fan_out_list(
        backend_transports,
        "list_resource_templates",
        |response: &ListResourceTemplatesResult| response.resource_templates.len(),
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
                service.list_resource_templates(backend_req).await
            }
        },
    )
    .await;

    let (resource_templates, next_cursor) =
        merge_resource_templates(responses, namespace_identifiers, &gateway_cursor, "list_resource_templates");
    if let (Some(plugin_runtime), Some(post_state)) = (&mcp_service.plugin_runtime, post_state) {
        plugin_runtime.after_list_resource_templates(&resource_templates, Some(post_state)).await?;
    }

    let mut result = ListResourceTemplatesResult::with_all_items(resource_templates);
    result.next_cursor = next_cursor;
    Ok(result)
}

#[expect(deprecated, reason = "temporary RMCP v3 compatibility; subscriptions/listen migration is deferred")]
pub(super) async fn subscribe<T>(
    mcp_service: &McpService<T>,
    request: SubscribeRequestParams,
    cx: RequestContext<RoleServer>,
) -> Result<(), ErrorData>
where
    T: UserSessionStore + Send + Sync + 'static,
{
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

    // Hooks run after routing, so plugins see the backend-local URI and the backend that owns it.
    // There is no post hook: `subscribe` returns an empty acknowledgement. The rewritten URI is
    // used for tracking too, or a rewritten subscription would be tracked under a URI the backend
    // never notifies about.
    let resource_uri = match &mcp_service.plugin_runtime {
        Some(plugin_runtime) => {
            plugin_runtime.before_subscribe(&resource_uri, &service_name).await?.apply_to(resource_uri)
        },
        None => resource_uri,
    };

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
pub(super) async fn unsubscribe<T>(
    mcp_service: &McpService<T>,
    request: UnsubscribeRequestParams,
    cx: RequestContext<RoleServer>,
) -> Result<(), ErrorData>
where
    T: UserSessionStore + Send + Sync + 'static,
{
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

    // Same contract as `subscribe`: pre hook only, and the rewritten URI is what gets forwarded and
    // untracked, so a rewritten subscribe and unsubscribe pair resolve to the same tracking key.
    let resource_uri = match &mcp_service.plugin_runtime {
        Some(plugin_runtime) => {
            plugin_runtime.before_unsubscribe(&resource_uri, &service_name).await?.apply_to(resource_uri)
        },
        None => resource_uri,
    };

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
