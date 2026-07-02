use std::time::Duration;

use axum::{body::Body, extract::State, middleware::Next, response::Response};
use contextforge_gateway_rs_apis::{User, user_store::UserConfig};
use http::{StatusCode, header};
//use openid::Claims;
use tracing::{debug, info, warn};

use crate::{
    common::{ContextForgeClaims, ContextForgeGatewayAppState},
    user_config_store::{ConfigStoreError, UserConfigStore},
};

const USER_CONFIG_MISS_RETRY_ATTEMPTS: usize = 30;
#[cfg(not(test))]
const USER_CONFIG_MISS_RETRY_DELAY: Duration = Duration::from_millis(100);
#[cfg(test)]
const USER_CONFIG_MISS_RETRY_DELAY: Duration = Duration::ZERO;
const NO_DATAPLANE_CONFIG_BODY: &str = "No dataplane config for subject";

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
        match get_config_with_miss_retry(state.config_store.as_ref(), &User::new(&subject), &subject, &method, &path)
            .await
        {
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
                    .status(StatusCode::FORBIDDEN)
                    .header(header::CONTENT_TYPE, "text/plain")
                    .body(Body::from(NO_DATAPLANE_CONFIG_BODY))
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

async fn get_config_with_miss_retry(
    config_store: &(dyn UserConfigStore + Send + Sync),
    user_key: &User<'_>,
    subject: &str,
    method: &http::Method,
    path: &str,
) -> Result<UserConfig, ConfigStoreError> {
    let mut misses = 0;
    loop {
        match config_store.get_config(user_key).await {
            Err(ConfigStoreError::NoDataForKey) if misses < USER_CONFIG_MISS_RETRY_ATTEMPTS => {
                misses += 1;
                debug!(
                    "user_config_store_layer - user config missing, retrying Redis lookup subject = {subject} method = {method} path = {path} attempt = {misses}"
                );
                tokio::time::sleep(USER_CONFIG_MISS_RETRY_DELAY).await;
            },
            result => return result,
        }
    }
}

#[cfg(test)]
mod tests {
    use std::{
        collections::HashMap,
        sync::{
            Arc,
            atomic::{AtomicUsize, Ordering},
        },
    };

    use async_trait::async_trait;
    use axum::{Router, body::to_bytes, middleware, routing::get};
    use contextforge_gateway_rs_apis::user_store::VirtualHost;
    use http::Request;
    use tower::ServiceExt;

    use super::*;
    use crate::{
        Config,
        common::{self, JwtTokenDecoders, Scopes},
        const_values::{CONTEXT_FORGE_GATEWAY_AUDIENCE, CONTEXT_FORGE_GATEWAY_ISSUER},
    };

    struct DelayedUserConfigStore {
        misses_before_success: usize,
        calls: AtomicUsize,
    }

    impl DelayedUserConfigStore {
        fn new(misses_before_success: usize) -> Self {
            Self { misses_before_success, calls: AtomicUsize::new(0) }
        }

        fn calls(&self) -> usize {
            self.calls.load(Ordering::SeqCst)
        }
    }

    #[async_trait]
    impl UserConfigStore for DelayedUserConfigStore {
        async fn get_config<'a>(&self, _: &'a User) -> Result<UserConfig, ConfigStoreError> {
            let calls = self.calls.fetch_add(1, Ordering::SeqCst);
            if calls < self.misses_before_success {
                return Err(ConfigStoreError::NoDataForKey);
            }

            Ok(UserConfig {
                virtual_hosts: HashMap::from([("known".to_owned(), VirtualHost { backends: HashMap::new() })]),
            })
        }

        async fn set_config<'a>(&self, _: &'a User, _: &'a UserConfig) -> Result<(), ConfigStoreError> {
            Err(ConfigStoreError::CantWriteData)
        }
    }

    fn claims() -> ContextForgeClaims {
        ContextForgeClaims {
            iss: CONTEXT_FORGE_GATEWAY_ISSUER.to_owned(),
            sub: "user@example.com".to_owned(),
            aud: CONTEXT_FORGE_GATEWAY_AUDIENCE.to_owned(),
            exp: u64::MAX,
            iat: None,
            jti: "test-token".to_owned(),
            token_use: "api".to_owned(),
            teams: None,
            user: common::User::builder()
                .email("user@example.com".to_owned())
                .auth_provider("api_token".to_owned())
                .full_name("Test User".to_owned())
                .is_admin(false)
                .build(),
            scopes: Some(
                Scopes::builder()
                    .server_id(None)
                    .permissions(Vec::new())
                    .ip_restrictions(Vec::new())
                    .time_restrictions(None)
                    .build(),
            ),
        }
    }

    fn state(config_store: Arc<dyn UserConfigStore + Send + Sync>) -> ContextForgeGatewayAppState {
        ContextForgeGatewayAppState {
            jwt_token_decoding_keys: JwtTokenDecoders { rs: None, hmac_sha: None },
            config_store,
            config: Config::default(),
        }
    }

    fn request() -> Request<Body> {
        let mut request = Request::builder().uri("/").body(Body::empty()).expect("request should build");
        request.extensions_mut().insert(claims());
        request
    }

    async fn handler() -> StatusCode {
        StatusCode::NO_CONTENT
    }

    #[tokio::test]
    async fn retries_user_config_lookup_after_missing_subject() {
        let config_store = Arc::new(DelayedUserConfigStore::new(1));
        let state_config_store: Arc<DelayedUserConfigStore> = Arc::clone(&config_store);
        let state_config_store: Arc<dyn UserConfigStore + Send + Sync> = state_config_store;
        let app = Router::new()
            .route("/", get(handler))
            .layer(middleware::from_fn_with_state(state(state_config_store), user_config_store_layer));

        let response = app.oneshot(request()).await.expect("response should be returned");

        assert_eq!(response.status(), StatusCode::NO_CONTENT);
        assert_eq!(config_store.calls(), 2);
    }

    #[tokio::test]
    async fn returns_forbidden_after_retry_exhaustion() {
        let config_store = Arc::new(DelayedUserConfigStore::new(usize::MAX));
        let state_config_store: Arc<DelayedUserConfigStore> = Arc::clone(&config_store);
        let state_config_store: Arc<dyn UserConfigStore + Send + Sync> = state_config_store;
        let app = Router::new()
            .route("/", get(handler))
            .layer(middleware::from_fn_with_state(state(state_config_store), user_config_store_layer));

        let response = app.oneshot(request()).await.expect("response should be returned");

        assert_eq!(response.status(), StatusCode::FORBIDDEN);
        assert_eq!(config_store.calls(), USER_CONFIG_MISS_RETRY_ATTEMPTS + 1);

        let body =
            to_bytes(response.into_body(), NO_DATAPLANE_CONFIG_BODY.len()).await.expect("body should be readable");
        assert_eq!(&body[..], NO_DATAPLANE_CONFIG_BODY.as_bytes());
    }
}
