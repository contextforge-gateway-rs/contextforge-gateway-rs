use std::{collections::HashMap, sync::Arc};

use contextforge_data_plane_cpex::{GatewayPluginRuntimeHandle, RuntimeHookState};
use rmcp::{
    ClientHandler, Peer, RoleClient, RoleServer,
    model::{
        CallToolRequestParams, CallToolResult, ClientRequest, InitializeRequestParams, ProgressNotificationParam,
        ProgressToken, Request, ServerResult,
    },
    serde::{Serialize, de::DeserializeOwned},
    service::{NotificationContext, PeerRequestOptions, RequestHandle, ServiceError},
};
use tokio::sync::RwLock;
use tokio_util::sync::CancellationToken;
use tracing::{debug, warn};

#[derive(Clone)]
pub(crate) struct GatewayBackendClient {
    initialize_request: InitializeRequestParams,
    plugin_runtime: Option<GatewayPluginRuntimeHandle>,
    in_flight_calls: Arc<RwLock<HashMap<ProgressToken, Arc<InFlightToolCall>>>>,
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
        initialize_request: InitializeRequestParams,
        plugin_runtime: Option<GatewayPluginRuntimeHandle>,
    ) -> Self {
        Self { initialize_request, plugin_runtime, in_flight_calls: Arc::default() }
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
        let request = ClientRequest::CallToolRequest(Request::new(request));
        let Some(downstream_progress_token) = downstream_progress_token else {
            return peer.send_cancellable_request(request, PeerRequestOptions::no_options()).await;
        };

        // RMCP publishes the request before returning its generated progress
        // token. Holding the write guard across that enqueue makes progress
        // lookups wait until the generated-to-downstream mapping is visible.
        let mut calls = self.in_flight_calls.write().await;
        let handle = peer.send_cancellable_request(request, PeerRequestOptions::no_options()).await?;
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
