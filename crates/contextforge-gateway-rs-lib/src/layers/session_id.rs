use std::sync::Arc;

use axum::{body::Body, extract::State, http::Request, middleware::Next, response::Response};
use http::{Method, StatusCode, header};
use tracing::info;

use crate::{
    common::ContextForgeClaims,
    const_values::MCP_SESSION_ID,
    gateway::{BackendTransportCleanup, UserSession, UserSessionStore},
    layers::virtual_host_id::VirtualHostId,
};

#[derive(Debug, Clone)]
pub struct SessionId {
    value: String,
}

impl SessionId {
    pub fn value(&self) -> &String {
        &self.value
    }
}

#[derive(Clone)]
pub struct SessionIdState {
    pub user_session_store: Arc<dyn UserSessionStore>,
    pub backend_transport_cleanup: BackendTransportCleanup,
}

pub async fn session_id_layer(State(state): State<SessionIdState>, mut request: Request<Body>, next: Next) -> Response {
    let session_id =
        request.headers().get(MCP_SESSION_ID).and_then(|session_id| session_id.to_str().ok()).map(str::to_owned);

    if request.method() != Method::DELETE {
        if let Some(session_id) = session_id {
            info!("MCP Session ID {session_id}");
            request.extensions_mut().insert(SessionId { value: session_id });
        }
        return next.run(request).await;
    }

    let Some(session_id) = session_id else {
        return response(StatusCode::BAD_REQUEST);
    };
    let Some(claims) = request.extensions().get::<ContextForgeClaims>() else {
        return response(StatusCode::BAD_REQUEST);
    };
    let Some(_virtual_host_id) = request.extensions().get::<VirtualHostId>() else {
        return response(StatusCode::BAD_REQUEST);
    };

    let user_session = UserSession::new(claims.sub.clone(), Arc::from(session_id.as_str()));
    match state.user_session_store.get_session(&user_session).await {
        Ok(Some(_)) => {},
        Ok(None) => return response(StatusCode::NOT_FOUND),
        Err(_) => return response(StatusCode::INTERNAL_SERVER_ERROR),
    }

    request.extensions_mut().insert(SessionId { value: session_id.clone() });
    let rmcp_response = next.run(request).await;
    if !rmcp_response.status().is_success() {
        return rmcp_response;
    }

    let remove_result = state.user_session_store.remove_session(&user_session).await;
    state.backend_transport_cleanup.remove_session(&session_id).await;

    if remove_result.is_err() {
        return response(StatusCode::INTERNAL_SERVER_ERROR);
    }

    rmcp_response
}

fn response(status: StatusCode) -> Response {
    Response::builder()
        .status(status)
        .header(header::CONTENT_TYPE, "text/plain")
        .body(Body::empty())
        .expect("response should build")
}
