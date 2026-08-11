use axum::{body::Body, extract::State, middleware::Next, response::Response};
use contextforge_data_plane_apis::User;
use http::{StatusCode, header};
//use openid::Claims;
use tracing::{debug, error, info, warn};

use crate::{
    common::{ContextForgeClaims, ContextForgeDataPlaneAppState},
    user_config_store::ConfigStoreError,
};

pub async fn user_config_store_layer(
    State(state): State<ContextForgeDataPlaneAppState>,
    mut request: http::Request<axum::body::Body>,
    next: Next,
) -> Response {
    let method = request.method().clone();
    let path = request.uri().path().to_owned();
    let maybe_claims = request.extensions().get::<ContextForgeClaims>();
    if let Some(claims) = maybe_claims {
        let subject = claims.sub.clone();
        debug!(
            component = "UserConfig",
            operation = "load",
            method = %method,
            path,
            "loading user config"
        );
        match state.config_store.get_config(&User::new(&subject)).await {
            Ok(user_config) => {
                let virtual_hosts = user_config.virtual_hosts.len();
                info!(component = "UserConfig", operation = "load", virtual_hosts, "user config loaded");
                request.extensions_mut().insert(user_config);
                next.run(request).await
            },

            Err(ConfigStoreError::NoDataForKey) => {
                debug!(
                    component = "UserConfig",
                    operation = "load",
                    method = %method,
                    path,
                    "user config was not found"
                );
                Response::builder()
                    .status(StatusCode::BAD_REQUEST)
                    .header(header::CONTENT_TYPE, "text/plain")
                    .body(Body::from("Problem occurred retrieving the configuration"))
                    .expect("Expecting this to work")
            },

            Err(error) => {
                error!(
                    component = "UserConfig",
                    operation = "load",
                    method = %method,
                    path,
                    error_code = "CFDP-USER-CONFIG-LOAD",
                    root_cause = %error,
                    impact_scope = "request",
                    retryable = true,
                    http_status = 500_u16,
                    error = %error,
                    "user config lookup failed"
                );
                Response::builder()
                    .status(StatusCode::INTERNAL_SERVER_ERROR)
                    .header(header::CONTENT_TYPE, "text/plain")
                    .body(Body::from("Problem occurred retrieving the configuration"))
                    .expect("Expecting this to work")
            },
        }
    } else {
        warn!(
            component = "Authorization",
            operation = "load_user_config",
            method = %method,
            path,
            "request has no authorization claims"
        );
        Response::builder()
            .status(StatusCode::BAD_REQUEST)
            .header(header::CONTENT_TYPE, "text/plain")
            .body(Body::from("No claims in the token"))
            .expect("Expecting this to work")
    }
}
