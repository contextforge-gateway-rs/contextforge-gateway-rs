use std::{
    collections::HashMap,
    sync::{Arc, Mutex as StdMutex},
};

use contextforge_gateway_rs_cpex::{GatewayPluginRuntimeHandle, RuntimeHookState};
use rmcp::{
    ClientHandler, Peer, RoleClient, RoleServer,
    model::{
        CallToolRequestParams, CallToolResult, ClientRequest, InitializeRequestParams, Meta, ProgressNotificationParam,
        ProgressToken, Request, ServerResult,
    },
    serde::{Serialize, de::DeserializeOwned},
    service::{NotificationContext, PeerRequestOptions, ServiceError},
};
use tokio_util::sync::CancellationToken;
use tracing::{debug, warn};

/// Client handler for a backend MCP connection. Progress notifications streamed
/// by the backend are run through the tool post hooks of the in-flight tool call
/// they belong to and forwarded to the downstream peer of the gateway session.
#[derive(Clone)]
pub(crate) struct GatewayBackendClient {
    initialize_request: InitializeRequestParams,
    downstream: Peer<RoleServer>,
    plugin_runtime: Option<GatewayPluginRuntimeHandle>,
    in_flight_calls: Arc<StdMutex<InFlightCalls>>,
}

/// Tool calls currently in flight on this backend session. Progress
/// notifications are routed strictly by the downstream progress token, as MCP
/// requires backends to echo the token the client sent.
#[derive(Default)]
struct InFlightCalls {
    by_progress_token: HashMap<ProgressToken, Arc<InFlightToolCall>>,
}

struct InFlightToolCall {
    progress_token: Option<ProgressToken>,
    tool_name: String,
    post_state: Option<RuntimeHookState>,
}

/// Keeps a tool call registered with the backend client; dropping the guard
/// stops notification forwarding for the call.
pub(crate) struct InFlightToolCallGuard {
    calls: Arc<StdMutex<InFlightCalls>>,
    call: Arc<InFlightToolCall>,
}

impl Drop for InFlightToolCallGuard {
    fn drop(&mut self) {
        if let Ok(mut calls) = self.calls.lock() {
            if let Some(token) = &self.call.progress_token
                && calls.by_progress_token.get(token).is_some_and(|call| Arc::ptr_eq(call, &self.call))
            {
                calls.by_progress_token.remove(token);
            }
        }
    }
}

impl GatewayBackendClient {
    pub(crate) fn new(
        initialize_request: InitializeRequestParams,
        downstream: Peer<RoleServer>,
        plugin_runtime: Option<GatewayPluginRuntimeHandle>,
    ) -> Self {
        Self { initialize_request, downstream, plugin_runtime, in_flight_calls: Arc::default() }
    }

    pub(crate) fn track_tool_call(
        &self,
        tool_name: String,
        progress_token: Option<ProgressToken>,
        post_state: Option<RuntimeHookState>,
    ) -> InFlightToolCallGuard {
        let call = Arc::new(InFlightToolCall { progress_token, tool_name, post_state });
        let mut calls = self.in_flight_calls.lock().expect("in-flight tool call lock poisoned");
        if let Some(token) = &call.progress_token {
            calls.by_progress_token.entry(token.clone()).or_insert_with(|| Arc::clone(&call));
        }
        drop(calls);
        InFlightToolCallGuard { calls: Arc::clone(&self.in_flight_calls), call }
    }

    fn progress_call(&self, progress_token: &ProgressToken) -> Option<Arc<InFlightToolCall>> {
        let calls = self.in_flight_calls.lock().expect("in-flight tool call lock poisoned");
        calls.by_progress_token.get(progress_token).cloned()
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
        // MCP requires backends to send progress only for tokens the caller
        // supplied, so notifications with unknown tokens are dropped.
        let Some(call) = self.progress_call(&progress.progress_token) else {
            debug!(
                "call_tool: dropping backend progress notification with unknown token {:?}",
                progress.progress_token
            );
            return;
        };
        let Some(progress) = self.stream_event_post_hook(&call, progress).await else {
            return;
        };
        if let Err(error) = self.downstream.notify_progress(progress).await {
            warn!("call_tool: unable to forward backend progress notification downstream: {error:?}");
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
