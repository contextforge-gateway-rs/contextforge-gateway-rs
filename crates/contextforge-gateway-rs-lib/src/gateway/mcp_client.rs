use std::sync::Arc;

use http::request;
use rmcp::{
    ClientHandler, Peer, RoleClient, RoleServer,
    model::{ClientCapabilities, ClientInfo, Implementation, NumberOrString, ProgressNotificationParam, ProgressToken},
    service::{NotificationContext, ServiceRole},
    transport::streamable_http_client::StreamableHttpClient,
};
use sse_stream::{Sse, SseStream};
use tracing::{debug, info, warn};

use crate::gateway::mcp_gateway::SetPeer;

// Progress-aware client handler
#[derive(Debug, Clone)]
pub struct ProgressAwareClient {
    client: reqwest::Client,
    peer: Option<Peer<RoleServer>>,
}

impl ProgressAwareClient {
    pub fn new(client: reqwest::Client) -> Self {
        Self { client, peer: None }
    }

    fn start_tracking(&self) {}

    fn stop_tracking(&self) {}
}

impl ClientHandler for ProgressAwareClient {
    async fn on_progress(&self, mut params: ProgressNotificationParam, _context: NotificationContext<RoleClient>) {
        let progress = params.progress;
        let message = params.message.clone();
        params.progress_token = ProgressToken(NumberOrString::String(Arc::from("blah")));
        info!("Got notification {params:?}");
        if let Some(peer) = self.peer.as_ref() {
            match peer.notify_progress(params).await {
                Ok(_) => {
                    debug!("Processed record: {progress} {message:?}");
                },
                Err(e) => {
                    warn!("Can't send notification {e:?}");
                },
            }
        }
    }

    fn get_info(&self) -> ClientInfo {
        ClientInfo::new(ClientCapabilities::default(), Implementation::new("mcp-gateway", "1.0.0"))
    }
}

impl SetPeer for ProgressAwareClient {
    fn set_peer(&mut self, peer: Peer<RoleServer>) {
        self.peer = Some(peer);
    }
}

impl StreamableHttpClient for ProgressAwareClient {
    type Error = reqwest::Error;

    fn post_message(
        &self,
        uri: Arc<str>,
        message: rmcp::model::ClientJsonRpcMessage,
        session_id: Option<Arc<str>>,
        auth_header: Option<String>,
        custom_headers: std::collections::HashMap<http::HeaderName, http::HeaderValue>,
    ) -> impl Future<
        Output = Result<
            rmcp::transport::streamable_http_client::StreamableHttpPostResponse,
            rmcp::transport::streamable_http_client::StreamableHttpError<Self::Error>,
        >,
    > + Send
    + '_ {
        self.client.post_message(uri, message, session_id, auth_header, custom_headers)
    }

    fn delete_session(
        &self,
        uri: Arc<str>,
        session_id: Arc<str>,
        auth_header: Option<String>,
        custom_headers: std::collections::HashMap<http::HeaderName, http::HeaderValue>,
    ) -> impl Future<Output = Result<(), rmcp::transport::streamable_http_client::StreamableHttpError<Self::Error>>>
    + Send
    + '_ {
        self.client.delete_session(uri, session_id, auth_header, custom_headers)
    }

    fn get_stream(
        &self,
        uri: Arc<str>,
        session_id: Arc<str>,
        last_event_id: Option<String>,
        auth_header: Option<String>,
        custom_headers: std::collections::HashMap<http::HeaderName, http::HeaderValue>,
    ) -> impl Future<
        Output = Result<
            futures::prelude::stream::BoxStream<
                'static,
                Result<Sse, rmcp::transport::streamable_http_client::SseError>,
            >,
            rmcp::transport::streamable_http_client::StreamableHttpError<Self::Error>,
        >,
    > + Send
    + '_ {
        self.client.get_stream(uri, session_id, last_event_id, auth_header, custom_headers)
    }
}
