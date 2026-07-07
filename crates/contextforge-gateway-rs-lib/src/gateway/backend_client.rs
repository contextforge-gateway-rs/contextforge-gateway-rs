use std::{collections::HashMap, sync::Arc};

use contextforge_gateway_rs_cpex::{GatewayPluginRuntimeHandle, RuntimeHookState};
use rmcp::{
    ClientHandler, Peer, RoleClient, RoleServer,
    model::{
        CallToolRequestParams, CallToolResult, ClientRequest, InitializeRequestParams, Meta, ProgressNotificationParam,
        ProgressToken, Request, ResourceUpdatedNotificationParam, ServerResult,
    },
    serde::{Serialize, de::DeserializeOwned},
    service::{NotificationContext, PeerRequestOptions, ServiceError},
};
use tokio::sync::Mutex;
use tokio_util::sync::CancellationToken;
use tracing::{debug, warn};

#[derive(Clone)]
pub(crate) struct GatewayBackendClient {
    backend_name: String,
    initialize_request: InitializeRequestParams,
    plugin_runtime: Option<GatewayPluginRuntimeHandle>,
    in_flight_calls: Arc<Mutex<HashMap<ProgressToken, Arc<InFlightToolCall>>>>,
    resource_subscriptions: Arc<Mutex<HashMap<String, Peer<RoleServer>>>>,
}

#[derive(Debug)]
struct InFlightToolCall {
    progress_token: Option<ProgressToken>,
    tool_name: String,
    post_state: Option<RuntimeHookState>,
    downstream: Peer<RoleServer>,
}

impl GatewayBackendClient {
    pub(crate) fn new(
        backend_name: String,
        initialize_request: InitializeRequestParams,
        plugin_runtime: Option<GatewayPluginRuntimeHandle>,
    ) -> Self {
        Self {
            backend_name,
            initialize_request,
            plugin_runtime,
            in_flight_calls: Arc::default(),
            resource_subscriptions: Arc::default(),
        }
    }

    pub(crate) async fn track_tool_call(
        &self,
        tool_name: String,
        downstream: Peer<RoleServer>,
        progress_token: Option<ProgressToken>,
        post_state: Option<RuntimeHookState>,
    ) {
        debug!("track_tool_call {tool_name} {progress_token:?} {post_state:?}");
        let call = Arc::new(InFlightToolCall { progress_token, tool_name, post_state, downstream });
        let mut calls = self.in_flight_calls.lock().await;

        if let Some(token) = &call.progress_token {
            calls.entry(token.clone()).or_insert_with(|| Arc::clone(&call));
        }
        drop(calls);
    }

    pub(crate) async fn stop_tracking_tool_call(&self, progress_token: Option<ProgressToken>) {
        debug!("stop_tracking_tool_call {progress_token:?}");

        let mut calls = self.in_flight_calls.lock().await;

        if let Some(token) = &progress_token {
            calls.remove(token);
        }
        drop(calls);
    }

    async fn progress_call(&self, progress_token: &ProgressToken) -> Option<Arc<InFlightToolCall>> {
        let calls = self.in_flight_calls.lock().await;
        calls.get(progress_token).cloned()
    }

    pub(crate) async fn track_resource_subscription(&self, resource_uri: String, downstream: Peer<RoleServer>) {
        debug!("track_resource_subscription backend {} uri {resource_uri}", self.backend_name);
        let mut subscriptions = self.resource_subscriptions.lock().await;
        subscriptions.insert(resource_uri, downstream);
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

    async fn on_progress(&self, progress: ProgressNotificationParam, _context: NotificationContext<RoleClient>) {
        let Some(call) = self.progress_call(&progress.progress_token).await else {
            debug!(
                "call_tool: dropping backend progress notification with unknown token {:?}",
                progress.progress_token
            );
            return;
        };
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

        params.uri = format!("{}-{}", self.backend_name, params.uri);
        if let Err(error) = downstream.notify_resource_updated(params).await {
            warn!("resource_updated: unable to forward backend notification downstream: {error:?}");
        }
    }
}

/// Calls the tool on the backend, keeping the downstream progress token on
/// the request (`Peer::call_tool` would stamp an auto-generated token over it
/// at serialization) and relaying a downstream cancellation to the backend.
pub(crate) async fn call_backend_tool(
    peer: &Peer<RoleClient>,
    request: CallToolRequestParams,
    progress_token: Option<ProgressToken>,
    cancellation: CancellationToken,
) -> Result<CallToolResult, ServiceError> {
    let mut options = PeerRequestOptions::no_options();
    if let Some(progress_token) = progress_token {
        let mut meta = Meta::new();
        meta.set_progress_token(progress_token);
        options.meta = Some(meta);
    }
    let mut handle =
        peer.send_cancellable_request(ClientRequest::CallToolRequest(Request::new(request)), options).await?;
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
