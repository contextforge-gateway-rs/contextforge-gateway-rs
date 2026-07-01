use axum::{body::Body, extract::State, middleware::Next, response::Response};
use contextforge_gateway_rs_apis::User;
use http::{StatusCode, header};
//use openid::Claims;
use tracing::{debug, info, warn};

use crate::{
    common::{ContextForgeClaims, ContextForgeGatewayAppState},
    user_config_store::ConfigStoreError,
};

pub async fn user_config_store_layer(
    State(state): State<ContextForgeGatewayAppState>,
    mut request: http::Request<axum::body::Body>,
    next: Next,
) -> Response {
    let method = request.method().clone();
    let path = request.uri().path().to_owned();
    let maybe_claims = request.extensions().get::<ContextForgeClaims>();
    if let Some(claims) = maybe_claims {
        let subject = claims.sub.clone();
        debug!(
            "user_config_store_layer - getting user config for request subject = {subject} method = {method} path = {path}"
        );
        match state.config_store.get_config(&User::new(&subject)).await {
            Ok(user_config) => {
                let virtual_hosts = user_config.virtual_hosts.len();
                info!(
                    "user_config_store_layer - loaded user config subject = {subject} virtual_hosts = {virtual_hosts}"
                );
                request.extensions_mut().insert(user_config);
                next.run(request).await
            },

            Err(ConfigStoreError::NoDataForKey) => {
                debug!(
                    "user_config_store_layer - user config lookup returned no data subject = {subject} method = {method} path = {path}"
                );
                Response::builder()
                    .status(StatusCode::BAD_REQUEST)
                    .header(header::CONTENT_TYPE, "text/plain")
                    .body(Body::from("Problem occurred retrieving the configuration"))
                    .expect("Expecting this to work")
            },

            Err(error) => {
                debug!(
                    "user_config_store_layer - user config lookup failed subject = {subject} method = {method} path = {path} error = {error}"
                );
                Response::builder()
                    .status(StatusCode::INTERNAL_SERVER_ERROR)
                    .header(header::CONTENT_TYPE, "text/plain")
                    .body(Body::from("Problem occurred retrieving the configuration"))
                    .expect("Expecting this to work")
            },
        }
    } else {
        warn!("user_config_store_layer - no claims found in request extensions method = {method} path = {path}");
        Response::builder()
            .status(StatusCode::BAD_REQUEST)
            .header(header::CONTENT_TYPE, "text/plain")
            .body(Body::from("No claims in the token"))
            .expect("Expecting this to work")
    }
}
