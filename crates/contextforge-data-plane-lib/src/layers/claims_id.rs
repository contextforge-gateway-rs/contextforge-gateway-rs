use axum::{
    extract::{Request, State},
    middleware::Next,
    response::Response,
};
use http::{StatusCode, header};

use crate::common::ContextForgeDataPlaneAppState;

fn unauthorized_response(message: &str) -> Response {
    Response::builder()
        .status(StatusCode::UNAUTHORIZED)
        .header(header::CONTENT_TYPE, "text/plain")
        .body(message.to_owned().into())
        .expect("Expecting this to work")
}

pub async fn claims_layer(
    State(state): State<ContextForgeDataPlaneAppState>,
    request: http::Request<axum::body::Body>,
    next: Next,
) -> Response {
    let (mut parts, body) = request.into_parts();

    let Some(authorization) = parts.headers.get("Authorization") else { return unauthorized_response("No header") };

    let Some(claims) = state.authorization_service.authorize(authorization).await else {
        return unauthorized_response("Invalid token");
    };

    parts.extensions.insert(claims.clone());
    let request = Request::from_parts(parts, body);
    next.run(request).await
}
