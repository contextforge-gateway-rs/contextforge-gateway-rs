use std::{collections::HashMap, sync::Arc};

use contextforge_gateway_rs_cpex::{GatewayPluginRuntimeHandle, RuntimeHookState};
use rmcp::{
    ClientHandler, Peer, RoleClient, RoleServer,
    model::{
        CallToolRequestParams, CallToolResult, ClientRequest, InitializeRequestParams, ProgressNotificationParam,
        ProgressToken, Request, ResourceUpdatedNotificationParam, ServerResult,
    },
    serde::{Serialize, de::DeserializeOwned},
    service::{NotificationContext, PeerRequestOptions, RequestHandle, ServiceError},
};
use tokio::sync::{Mutex, RwLock};
use tokio_util::sync::CancellationToken;
use tracing::{debug, warn};

use super::mcp_gateway::prefixed_name;

#[derive(Clone)]
pub(crate) struct GatewayBackendClient {
    backend_name: String,
    namespace_identifiers: bool,
    initialize_request: InitializeRequestParams,
    plugin_runtime: Option<GatewayPluginRuntimeHandle>,
    in_flight_calls: Arc<RwLock<HashMap<ProgressToken, Arc<InFlightToolCall>>>>,
    resource_subscriptions: Arc<Mutex<HashMap<String, Peer<RoleServer>>>>,
}

#[derive(Debug)]
struct InFlightToolCall {
    downstream_progress_token: ProgressToken,
    tool_name: String,
    post_state: Option<RuntimeHookState>,
    downstream: Peer<RoleServer>,
}

impl GatewayBackendClient {
    pub(crate) fn new(
        backend_name: String,
        namespace_identifiers: bool,
        initialize_request: InitializeRequestParams,
        plugin_runtime: Option<GatewayPluginRuntimeHandle>,
    ) -> Self {
        Self {
            backend_name,
            namespace_identifiers,
            initialize_request,
            plugin_runtime,
            in_flight_calls: Arc::default(),
            resource_subscriptions: Arc::default(),
        }
    }

    /// Starts a backend tool call while preventing an immediate progress
    /// notification from overtaking publication of its token mapping.
    #[expect(clippy::too_many_arguments, reason = "tracking requires the request and its downstream call metadata")]
    pub(crate) async fn start_tool_call(
        &self,
        peer: &Peer<RoleClient>,
        request: CallToolRequestParams,
        downstream_progress_token: Option<ProgressToken>,
        tool_name: String,
        downstream: Peer<RoleServer>,
        post_state: Option<RuntimeHookState>,
    ) -> Result<RequestHandle<RoleClient>, ServiceError> {
        debug!("track_tool_call {tool_name} {downstream_progress_token:?} {post_state:?}");
        let Some(downstream_progress_token) = downstream_progress_token else {
            return start_backend_tool_call(peer, request).await;
        };

        // RMCP publishes the request before returning its generated progress
        // token. Holding the write guard across that enqueue makes progress
        // lookups wait until the generated-to-downstream mapping is visible.
        let mut calls = self.in_flight_calls.write().await;
        let handle = start_backend_tool_call(peer, request).await?;
        let backend_progress_token = handle.progress_token.clone();
        let call = Arc::new(InFlightToolCall { downstream_progress_token, tool_name, post_state, downstream });
        calls.insert(backend_progress_token, call);
        Ok(handle)
    }

    pub(crate) async fn stop_tracking_tool_call(&self, backend_progress_token: &ProgressToken) {
        debug!("stop_tracking_tool_call {backend_progress_token:?}");
        let mut calls = self.in_flight_calls.write().await;
        calls.remove(backend_progress_token);
    }

    async fn progress_call(&self, progress_token: &ProgressToken) -> Option<Arc<InFlightToolCall>> {
        let calls = self.in_flight_calls.read().await;
        calls.get(progress_token).cloned()
    }

    pub(crate) async fn track_resource_subscription(&self, resource_uri: &str, downstream: Peer<RoleServer>) {
        debug!("track_resource_subscription backend {} uri {resource_uri}", self.backend_name);
        let mut subscriptions = self.resource_subscriptions.lock().await;
        subscriptions.insert(resource_uri.to_owned(), downstream);
    }

    pub(crate) async fn stop_tracking_resource_subscription(&self, resource_uri: &str) {
        debug!("stop_tracking_resource_subscription backend {} uri {resource_uri}", self.backend_name);
        let mut subscriptions = self.resource_subscriptions.lock().await;
        subscriptions.remove(resource_uri);
    }

    async fn resource_subscription(&self, resource_uri: &str) -> Option<Peer<RoleServer>> {
        let subscriptions = self.resource_subscriptions.lock().await;
        subscriptions.get(resource_uri).cloned()
    }

    async fn stream_event_post_hook<T>(&self, call: &InFlightToolCall, event: T) -> Option<T>
    where
        T: Serialize + DeserializeOwned,
    {
        let Some(plugin_runtime) = &self.plugin_runtime else {
            return Some(event);
        };
        match plugin_runtime.after_stream_event(&call.tool_name, event, call.post_state.clone()).await {
            Ok(event) => event,
            Err(error) => {
                warn!("call_tool: plugin rejected backend notification: {error:?}");
                None
            },
        }
    }
}

impl std::fmt::Debug for GatewayBackendClient {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("GatewayBackendClient")
            .field("initialize_request", &self.initialize_request)
            .finish_non_exhaustive()
    }
}

impl ClientHandler for GatewayBackendClient {
    fn get_info(&self) -> InitializeRequestParams {
        self.initialize_request.clone()
    }

    async fn on_progress(&self, mut progress: ProgressNotificationParam, _context: NotificationContext<RoleClient>) {
        let Some(call) = self.progress_call(&progress.progress_token).await else {
            debug!(
                "call_tool: dropping backend progress notification with unknown token {:?}",
                progress.progress_token
            );
            return;
        };
        progress.progress_token.clone_from(&call.downstream_progress_token);
        debug!("Processing Progress Notification {progress:?} {call:?}");
        let Some(progress) = self.stream_event_post_hook(&call, progress).await else {
            return;
        };
        if let Err(error) = call.downstream.notify_progress(progress).await {
            warn!("call_tool: unable to forward backend progress notification downstream: {error:?}");
        }
    }

    async fn on_resource_updated(
        &self,
        mut params: ResourceUpdatedNotificationParam,
        _context: NotificationContext<RoleClient>,
    ) {
        let Some(downstream) = self.resource_subscription(&params.uri).await else {
            debug!("resource_updated: dropping backend notification for unsubscribed uri {}", params.uri);
            return;
        };

        params.uri = resource_uri_for_downstream(&self.backend_name, params.uri, self.namespace_identifiers);
        if let Err(error) = downstream.notify_resource_updated(params).await {
            warn!("resource_updated: unable to forward backend notification downstream: {error:?}");
        }
    }
}

fn resource_uri_for_downstream(backend_name: &str, uri: String, namespace_identifiers: bool) -> String {
    if namespace_identifiers { prefixed_name(backend_name, &uri) } else { uri }
}

/// Starts a backend tool call and exposes RMCP's generated progress token so
/// notifications can be mapped back to the downstream token.
pub(crate) async fn start_backend_tool_call(
    peer: &Peer<RoleClient>,
    request: CallToolRequestParams,
) -> Result<RequestHandle<RoleClient>, ServiceError> {
    peer.send_cancellable_request(
        ClientRequest::CallToolRequest(Request::new(request)),
        PeerRequestOptions::no_options(),
    )
    .await
}

/// Awaits a started backend tool call while relaying downstream cancellation.
pub(crate) async fn call_backend_tool(
    mut handle: RequestHandle<RoleClient>,
    cancellation: CancellationToken,
) -> Result<CallToolResult, ServiceError> {
    let response = tokio::select! {
        response = &mut handle.rx => Some(response),
        () = cancellation.cancelled() => None,
    };

    let Some(response) = response else {
        let reason = "tool call cancelled by the downstream client".to_owned();
        if let Err(error) = handle.cancel(Some(reason.clone())).await {
            warn!("call_tool: unable to relay cancellation to the backend: {error:?}");
        }
        return Err(ServiceError::Cancelled { reason: Some(reason) });
    };
    match response.map_err(|_| ServiceError::TransportClosed)?? {
        ServerResult::CallToolResult(result) => Ok(result),
        _ => Err(ServiceError::UnexpectedResponse),
    }
}

#[cfg(test)]
mod tests {
    use super::resource_uri_for_downstream;

    #[test]
    fn single_backend_resource_update_preserves_uri() {
        assert_eq!("test://resource", resource_uri_for_downstream("backend-id", "test://resource".to_owned(), false));
    }

    #[test]
    fn multi_backend_resource_update_prefixes_uri() {
        assert_eq!(
            "backend-id-test://resource",
            resource_uri_for_downstream("backend-id", "test://resource".to_owned(), true)
        );
    }
}
