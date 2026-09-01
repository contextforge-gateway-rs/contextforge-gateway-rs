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

#[cfg(test)]
mod test {

    const GATEWAY_AUDIENCE: &str = "audience";
    const GATEWAY_ISSUER: &str = "issuer";

    use std::sync::{Arc, Once};

    use async_trait::async_trait;
    use axum::{Router, body::Body, middleware, response::Response, routing::get};
    use chrono::Duration;
    use contextforge_data_plane_apis::{User, user_store::UserConfig};
    use http::{HeaderMap, HeaderValue, Request, StatusCode};
    use jsonwebtoken::{Algorithm, DecodingKey, EncodingKey, Header, encode};
    use tower::ServiceExt;
    use uuid::Uuid;

    use crate::{
        Config,
        authorization::{AuthorizationClaims, AuthorizationService, Scopes},
        common::ContextForgeDataPlaneAppState,
        layers::claims_id::claims_layer,
        user_config_store::{ConfigStoreError, UserConfigStore},
    };

    #[derive(Debug)]
    pub struct Noop;

    #[async_trait]
    impl AuthorizationService for Noop {
        async fn authorize(&self, _: &HeaderValue) -> Option<AuthorizationClaims> {
            None
        }
    }

    static CRYPTO: Once = Once::new();
    const HMAC_SECRET: &[u8] = b"my-test-key-but-now-longer-than-32-bytes";

    fn now_epoch_seconds() -> u64 {
        std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).expect("Time went backwards").as_secs()
    }

    fn active_test_claims() -> AuthorizationClaims {
        let now = now_epoch_seconds();
        let user_id = "11111111-1111-1111-1111-111111111111".to_owned();

        AuthorizationClaims {
            iss: GATEWAY_ISSUER.to_owned(),
            sub: user_id.clone(),
            aud: GATEWAY_AUDIENCE.to_owned(),
            exp: now + Duration::hours(1).num_seconds().cast_unsigned(),
            iat: Some(now),
            jti: Uuid::new_v4().to_string(),
            token_use: Some("api".to_owned()),
            teams: Some(vec!["team_awesome".to_owned()]),
            user: Some(
                crate::authorization::User::builder()
                    .tenant_id("team_awesome".to_owned())
                    .user_id(user_id.clone())
                    .build(),
            ),
            scopes: Some(
                Scopes::builder()
                    .server_id(Some("my_id".to_owned()))
                    .ip_restrictions(vec!["192.169.1.0/24".to_owned()])
                    .permissions(vec!["tools.read".to_owned(), "servers.use".to_owned()])
                    .time_restrictions(None)
                    .build(),
            ),
            ..Default::default()
        }
    }

    fn get_hmac_token_for_claims(claims: &AuthorizationClaims) -> String {
        let key = EncodingKey::from_secret(HMAC_SECRET);
        let header = Header::new(Algorithm::HS256);

        encode::<AuthorizationClaims>(&header, claims, &key).expect("Expecting this to work")
    }

    struct MockedUserConfigStore;
    #[async_trait]
    impl UserConfigStore for MockedUserConfigStore {
        async fn get_config<'a>(&self, _: &'a User) -> Result<UserConfig, ConfigStoreError> {
            Err(ConfigStoreError::InvalidConnection)
        }

        async fn set_config<'a>(&self, _: &'a User, _: &'a UserConfig) -> Result<(), ConfigStoreError> {
            Err(ConfigStoreError::InvalidConnection)
        }
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 1)]
    #[allow(clippy::items_after_statements)]
    async fn claim_test_valid_hmac() {
        CRYPTO.call_once(|| {
            _ = rustls::crypto::ring::default_provider().install_default();
        });

        async fn handle(_: HeaderMap) -> Response {
            Response::builder().status(StatusCode::OK).body(Body::empty()).expect("Expecting this to work")
        }

        let token = get_hmac_token_for_claims(&active_test_claims());

        let decoding_key = DecodingKey::from_secret(HMAC_SECRET);

        let state = ContextForgeDataPlaneAppState {
            authorization_service: Arc::new(Noop),
            config_store: Arc::new(MockedUserConfigStore {}),
            config: Config::default(),
        };
        let http_requst = Request::builder()
            .header("Authorization", format!("Bearer {token}"))
            .method("GET")
            .body(Body::empty())
            .expect("This should work");

        let app =
            Router::new().route("/", get(handle)).layer(middleware::from_fn_with_state(state.clone(), claims_layer));

        let res = app.oneshot(http_requst).await.unwrap();
        assert_eq!(res.status(), StatusCode::OK);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 1)]
    #[allow(clippy::items_after_statements)]
    async fn claim_test_missing_scopes_is_allowed() {
        CRYPTO.call_once(|| {
            _ = rustls::crypto::ring::default_provider().install_default();
        });

        async fn handle(_: HeaderMap) -> Response {
            Response::builder().status(StatusCode::OK).body(Body::empty()).expect("Expecting this to work")
        }

        let mut claims = active_test_claims();
        claims.scopes = None;
        let token = get_hmac_token_for_claims(&claims);

        let decoding_key = DecodingKey::from_secret(HMAC_SECRET);

        let state = ContextForgeDataPlaneAppState {
            authorization_service: Arc::new(Noop {}),
            config_store: Arc::new(MockedUserConfigStore {}),
            config: Config::default(),
        };
        let http_requst = Request::builder()
            .header("Authorization", format!("Bearer {token}"))
            .method("GET")
            .body(Body::empty())
            .expect("This should work");

        let app =
            Router::new().route("/", get(handle)).layer(middleware::from_fn_with_state(state.clone(), claims_layer));

        let res = app.oneshot(http_requst).await.unwrap();
        assert_eq!(res.status(), StatusCode::OK);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 1)]
    #[allow(clippy::items_after_statements)]
    async fn claim_test_missing_token_use_and_full_name_is_allowed() {
        CRYPTO.call_once(|| {
            _ = rustls::crypto::ring::default_provider().install_default();
        });
        let user_id = "11111111-1111-1111-1111-111111111111".to_owned();

        async fn handle(_: HeaderMap) -> Response {
            Response::builder().status(StatusCode::OK).body(Body::empty()).expect("Expecting this to work")
        }

        let mut claims = active_test_claims();
        claims.token_use = None;

        claims.user = Some(
            crate::authorization::User::builder().tenant_id("team_awesome".to_owned()).user_id(user_id.clone()).build(),
        );
        let token = get_hmac_token_for_claims(&claims);

        let decoding_key = DecodingKey::from_secret(HMAC_SECRET);

        let state = ContextForgeDataPlaneAppState {
            authorization_service: Arc::new(Noop {}),
            config_store: Arc::new(MockedUserConfigStore {}),
            config: Config::default(),
        };
        let http_requst = Request::builder()
            .header("Authorization", format!("Bearer {token}"))
            .method("GET")
            .body(Body::empty())
            .expect("This should work");

        let app =
            Router::new().route("/", get(handle)).layer(middleware::from_fn_with_state(state.clone(), claims_layer));

        let res = app.oneshot(http_requst).await.unwrap();
        assert_eq!(res.status(), StatusCode::OK);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 1)]
    #[allow(clippy::items_after_statements)]
    async fn claim_test_expired_token() {
        CRYPTO.call_once(|| {
            _ = rustls::crypto::ring::default_provider().install_default();
        });

        async fn handle(_: HeaderMap) -> Response {
            Response::builder().status(StatusCode::OK).body(Body::empty()).expect("Expecting this to work")
        }

        let mut claims = active_test_claims();
        claims.exp = 0;
        let token = get_hmac_token_for_claims(&claims);

        let state = ContextForgeDataPlaneAppState {
            authorization_service: Arc::new(Noop {}),
            config_store: Arc::new(MockedUserConfigStore {}),
            config: Config::default(),
        };
        let http_requst = Request::builder()
            .header("Authorization", format!("Bearer {token}"))
            .method("GET")
            .body(Body::empty())
            .expect("This should work");

        let app =
            Router::new().route("/", get(handle)).layer(middleware::from_fn_with_state(state.clone(), claims_layer));

        let res = app.oneshot(http_requst).await.unwrap();
        assert_eq!(res.status(), StatusCode::UNAUTHORIZED);
    }
}
