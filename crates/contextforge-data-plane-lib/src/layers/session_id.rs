use std::sync::Arc;

use axum::{body::Body, extract::State, http::Request, middleware::Next, response::Response};
use http::{Method, StatusCode, header};
use tracing::info;

use crate::{
    common::ContextForgeClaims,
    const_values::{MCP_SESSION_ID, MOCK_SESSION_ID},
    gateway::{BackendTransports, UserSession, UserSessionStore},
};

#[derive(Debug, Clone)]
pub struct SessionId {
    value: String,
}

impl SessionId {
    pub(crate) fn mock() -> Self {
        Self { value: MOCK_SESSION_ID.to_owned() }
    }

    pub fn value(&self) -> &String {
        &self.value
    }
}

#[derive(Clone)]
pub struct SessionIdState {
    pub user_session_store: Arc<dyn UserSessionStore>,
    pub backend_transports: BackendTransports,
}

pub async fn session_id_layer(State(state): State<SessionIdState>, mut request: Request<Body>, next: Next) -> Response {
    let session_id = request
        .headers()
        .get(MCP_SESSION_ID)
        .and_then(|session_id| session_id.to_str().ok())
        .map(|_| MOCK_SESSION_ID.to_owned());

    if let Some(session_id) = &session_id {
        info!(component = "Session", operation = "attach_session", "MCP session attached to request");
        request.extensions_mut().insert(SessionId { value: session_id.clone() });
    }

    match request.method() {
        &Method::DELETE => {
            let subject = request.extensions().get::<ContextForgeClaims>().map(|claims| claims.sub.clone());
            let rmcp_response = next.run(request).await;
            if !rmcp_response.status().is_success() {
                return rmcp_response;
            }

            let Some(session_id) = session_id else {
                return rmcp_response;
            };
            let Some(subject) = subject else {
                return response(StatusCode::BAD_REQUEST);
            };
            let user_session = UserSession::new(subject.clone(), Arc::from(session_id.as_str()));
            let remove_result = state.user_session_store.remove_session(&user_session).await;
            state.backend_transports.remove_session(&subject, &session_id).await;

            if remove_result.is_err() {
                return response(StatusCode::INTERNAL_SERVER_ERROR);
            }

            rmcp_response
        },
        _ => next.run(request).await,
    }
}

fn response(status: StatusCode) -> Response {
    Response::builder()
        .status(status)
        .header(header::CONTENT_TYPE, "text/plain")
        .body(Body::empty())
        .expect("response should build")
}
