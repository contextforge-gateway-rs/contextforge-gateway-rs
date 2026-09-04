use axum::{
    extract::{Request, State},
    middleware::Next,
    response::Response,
};

use crate::{common::ContextForgeDataPlaneAppState, errors::unauthorized_response};

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
