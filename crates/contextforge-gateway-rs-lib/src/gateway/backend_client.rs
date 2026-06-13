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
    service::{NotificationContext, PeerRequestOptions, ServiceError},
};
use tracing::{debug, warn};

/// Client handler for a backend MCP connection. Progress notifications streamed
/// by the backend are run through the tool post hooks of the in-flight tool call
/// they belong to and forwarded to the downstream peer of the gateway session.
#[derive(Clone)]
pub(crate) struct GatewayBackendClient {
    initialize_request: InitializeRequestParams,
    downstream: Peer<RoleServer>,
    plugin_runtime: Option<GatewayPluginRuntimeHandle>,
    /// In-flight tool calls indexed by the downstream progress token. MCP
    /// requires backends to echo the token the caller supplied, so progress is
    /// routed strictly by token and notifications with unknown tokens are
    /// dropped.
    in_flight_calls: Arc<StdMutex<HashMap<ProgressToken, Arc<InFlightToolCall>>>>,
}

struct InFlightToolCall {
    tool_name: String,
    post_state: Option<RuntimeHookState>,
}

/// Keeps a tool call registered with the backend client for the duration of the
/// request; dropping the guard stops progress forwarding for the call.
pub(crate) struct InFlightToolCallGuard {
    calls: Arc<StdMutex<HashMap<ProgressToken, Arc<InFlightToolCall>>>>,
    registration: Option<(ProgressToken, Arc<InFlightToolCall>)>,
}

impl Drop for InFlightToolCallGuard {
    fn drop(&mut self) {
        let Some((token, call)) = &self.registration else { return };
        if let Ok(mut calls) = self.calls.lock()
            && calls.get(token).is_some_and(|existing| Arc::ptr_eq(existing, call))
        {
            calls.remove(token);
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

    /// Registers a tool call so its backend progress can be forwarded. Calls
    /// without a downstream progress token are not tracked, since no progress
    /// can be routed to them.
    pub(crate) fn track_tool_call(
        &self,
        tool_name: String,
        progress_token: Option<ProgressToken>,
        post_state: Option<RuntimeHookState>,
    ) -> InFlightToolCallGuard {
        let registration = progress_token.map(|token| {
            let call = Arc::new(InFlightToolCall { tool_name, post_state });
            // The first call to register a token owns it until it completes; a
            // duplicate token (malformed per spec) does not displace it.
            self.in_flight_calls
                .lock()
                .expect("in-flight tool call lock poisoned")
                .entry(token.clone())
                .or_insert_with(|| Arc::clone(&call));
            (token, call)
        });
        InFlightToolCallGuard { calls: Arc::clone(&self.in_flight_calls), registration }
    }

    fn progress_call(&self, progress_token: &ProgressToken) -> Option<Arc<InFlightToolCall>> {
        self.in_flight_calls.lock().expect("in-flight tool call lock poisoned").get(progress_token).cloned()
    }

    async fn progress_post_hook(
        &self,
        call: &InFlightToolCall,
        progress: ProgressNotificationParam,
    ) -> Option<ProgressNotificationParam> {
        let Some(plugin_runtime) = &self.plugin_runtime else {
            return Some(progress);
        };
        match plugin_runtime.after_progress_notification(&call.tool_name, progress, call.post_state.clone()).await {
            Ok(progress) => progress,
            Err(error) => {
                warn!("call_tool: plugin rejected backend progress notification: {error:?}");
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
        let Some(call) = self.progress_call(&progress.progress_token) else {
            debug!(
                "call_tool: dropping backend progress notification with unknown token {:?}",
                progress.progress_token
            );
            return;
        };
        let Some(progress) = self.progress_post_hook(&call, progress).await else {
            return;
        };
        if let Err(error) = self.downstream.notify_progress(progress).await {
            warn!("call_tool: unable to forward backend progress notification downstream: {error:?}");
        }
    }
}

/// Calls the tool on the backend, keeping the downstream progress token on the
/// request (`Peer::call_tool` would stamp an auto-generated token over it at
/// serialization).
pub(crate) async fn call_backend_tool(
    peer: &Peer<RoleClient>,
    request: CallToolRequestParams,
    progress_token: Option<ProgressToken>,
) -> Result<CallToolResult, ServiceError> {
    let mut options = PeerRequestOptions::no_options();
    if let Some(progress_token) = progress_token {
        let mut meta = Meta::new();
        meta.set_progress_token(progress_token);
        options.meta = Some(meta);
    }
    let handle = peer.send_cancellable_request(ClientRequest::CallToolRequest(Request::new(request)), options).await?;
    match handle.await_response().await? {
        ServerResult::CallToolResult(result) => Ok(result),
        _ => Err(ServiceError::UnexpectedResponse),
    }
}
