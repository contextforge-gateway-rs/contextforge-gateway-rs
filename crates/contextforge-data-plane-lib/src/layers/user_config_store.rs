use axum::{extract::State, middleware::Next, response::Response};
use contextforge_data_plane_apis::User;

use tracing::{debug, info, warn};

use crate::{
    common::ContextForgeDataPlaneAppState,
    errors::{bad_request, internal_server_error},
    user_config_store::ConfigStoreError,
};

pub async fn user_config_store_layer(
    State(state): State<ContextForgeDataPlaneAppState>,
    mut request: http::Request<axum::body::Body>,
    next: Next,
) -> Response {
    let method = request.method().clone();
    let path = request.uri().path().to_owned();
    let maybe_principal = request.extensions().get::<super::AuthorizedPrincipal>();
    if let Some(principal) = maybe_principal {
        debug!(
            "user_config_store_layer - getting user config for principal {principal:?} method = {method} path = {path}"
        );
        let user = User::from(principal);
        match state.config_store.get_config(&user).await {
            Ok(user_config) => {
                let virtual_hosts = user_config.virtual_hosts.len();
                info!(
                    "user_config_store_layer - loaded user config principal = {principal:?} virtual_hosts = {virtual_hosts}"
                );
                request.extensions_mut().insert(user_config);
                next.run(request).await
            },

            Err(ConfigStoreError::NoDataForKey) => {
                debug!(
                    "user_config_store_layer - user config lookup returned no data principal = {principal:?} method = {method} path = {path}"
                );
                bad_request("Problem occurred retrieving the configuration")
            },

            Err(error) => {
                debug!(
                    "user_config_store_layer - user config lookup failed principal = {principal:?} method = {method} path = {path} error = {error}"
                );
                internal_server_error("Problem occurred retrieving the configuration")
            },
        }
    } else {
        warn!("user_config_store_layer - no claims found in request extensions method = {method} path = {path}");
        bad_request("No claims in the token")
    }
}
