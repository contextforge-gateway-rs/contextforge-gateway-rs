use crate::authorization::jwks::jwks::Jwks;
use crate::authorization::jwks::principal::DefaultPrincipalExtractor;
use crate::authorization::{AuthorizationClaims, AuthorizationError, AuthorizationService};
use async_trait::async_trait;
use jsonwebtoken::decode_header;
use std::fmt;
use std::net::IpAddr;
use std::path::{Path, PathBuf};
use std::time::Duration;
use url::Url;

const JWKS_REQUEST_TIMEOUT: Duration = Duration::from_secs(10);
const JWKS_CONNECT_TIMEOUT: Duration = Duration::from_secs(5);
const JWKS_READ_TIMEOUT: Duration = Duration::from_secs(5);

pub struct JwtAuthorizationService {
    jwks: Jwks<super::principal::DefaultPrincipalExtractor>,
}

impl JwtAuthorizationService {
    pub fn from_jwks_url(jwks_url: Url, ca_cert_path: Option<&PathBuf>) -> Result<Self, AuthorizationError> {
        let url = parse_jwks_url(jwks_url)?;
        let mut client = reqwest::Client::builder()
            .tls_backend_rustls()
            .connect_timeout(JWKS_CONNECT_TIMEOUT)
            .read_timeout(JWKS_READ_TIMEOUT)
            .timeout(JWKS_REQUEST_TIMEOUT)
            .redirect(reqwest::redirect::Policy::none())
            .user_agent(concat!("mcp-ops/", env!("CARGO_PKG_VERSION")));
        if let Some(ca_cert_path) = ca_cert_path {
            client = client.tls_certs_only(load_ca_certificates(ca_cert_path)?);
        }
        let client = client.build().map_err(AuthorizationError::JwksRequest)?;
        Ok(Self {
            jwks: Jwks::builder().client(client).url(url).principal_extractor(DefaultPrincipalExtractor {}).build(),
        })
    }

    async fn authorize_token(&self, token: &str) -> Option<AuthorizationClaims> {
        let header = decode_header(token).ok()?;
        self.jwks.validate(token, &header).await
    }
}

impl fmt::Debug for JwtAuthorizationService {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("JwtAuthorizationService")
            .field("verification_source", &"remote JWKS")
            .finish_non_exhaustive()
    }
}

#[async_trait]
impl AuthorizationService for JwtAuthorizationService {
    async fn authorize(&self, authorization_token: &http::HeaderValue) -> Option<AuthorizationClaims> {
        let token = authorization_token.as_bytes().strip_prefix(b"Bearer ")?;
        let token = str::from_utf8(token).ok()?;
        let claims = self.authorize_token(token).await;

        if claims.is_none() {
            tracing::debug!("validate_saas_jwt  SaaS JWT was rejected");
        }

        claims
    }
}

fn parse_jwks_url(url: Url) -> Result<Url, AuthorizationError> {
    let secure = url.scheme() == "https";
    let local_http = url.scheme() == "http"
        && url.host_str().is_some_and(|host| {
            host.eq_ignore_ascii_case("localhost") || host.parse::<IpAddr>().is_ok_and(|address| address.is_loopback())
        });
    if !secure && !local_http {
        return Err(AuthorizationError::InsecureJwksUrl);
    }
    Ok(url)
}

fn load_ca_certificates(path: &Path) -> Result<Vec<reqwest::Certificate>, AuthorizationError> {
    let pem = std::fs::read(path)
        .map_err(|source| AuthorizationError::ReadJwksCaCertificate { path: path.to_owned(), source })?;
    let certificates = reqwest::Certificate::from_pem_bundle(&pem)
        .map_err(|source| AuthorizationError::InvalidJwksCaCertificate { path: path.to_owned(), source })?;
    if certificates.is_empty() {
        return Err(AuthorizationError::EmptyJwksCaCertificate { path: path.to_owned() });
    }
    Ok(certificates)
}

#[cfg(test)]
mod test {
    use crate::authorization::{
        AuthorizationError,
        jwks::{
            JwtAuthorizationService,
            jwks::{JWKS_CACHE_KEY, Jwks, VerificationKey},
            jwks_authorization::{JWKS_CONNECT_TIMEOUT, JWKS_READ_TIMEOUT, JWKS_REQUEST_TIMEOUT},
            principal::DefaultPrincipalExtractor,
        },
    };
    use crate::{
        Config,
        authorization::{AuthorizationClaims, Scopes},
        common::ContextForgeDataPlaneAppState,
        layers::claims_id::claims_layer,
        user_config_store::{ConfigStoreError, UserConfigStore},
    };
    use async_trait::async_trait;
    use axum::{Router, body::Body, middleware, response::Response, routing::get};

    use contextforge_data_plane_apis::{User, user_store::UserConfig};
    use http::{HeaderMap, Request, StatusCode};
    use jsonwebtoken::{Algorithm, DecodingKey, EncodingKey, Header, encode};
    use lru_time_cache::LruCache;

    use std::sync::{Arc, Once};
    use std::{str::FromStr, time::Duration};
    use tokio::sync::RwLock;
    use tower::ServiceExt;

    use url::Url;
    use uuid::Uuid;

    const GATEWAY_AUDIENCE: &str = "audience";
    const GATEWAY_ISSUER: &str = "issuer";

    impl VerificationKey {
        pub fn new(id: Option<String>, decoding_key: DecodingKey) -> Self {
            Self { key_id: id, decoding_key }
        }
    }

    impl JwtAuthorizationService {
        pub async fn from_keys(verification_keys: Vec<VerificationKey>) -> Result<Self, AuthorizationError> {
            let url: Url = Url::from_str("http://127.0.0.1:0/").expect("this should work");
            let client = reqwest::Client::builder()
                .tls_backend_rustls()
                .connect_timeout(JWKS_CONNECT_TIMEOUT)
                .read_timeout(JWKS_READ_TIMEOUT)
                .timeout(JWKS_REQUEST_TIMEOUT)
                .redirect(reqwest::redirect::Policy::none())
                .user_agent(concat!("mcp-ops/", env!("CARGO_PKG_VERSION")));

            let client = client.build().map_err(AuthorizationError::JwksRequest)?;

            let cache = RwLock::new(LruCache::with_expiry_duration(Duration::from_hours(100)));
            let mut guard = cache.write().await;
            guard.insert(JWKS_CACHE_KEY.to_owned(), verification_keys);
            drop(guard);

            Ok(Self {
                jwks: Jwks::builder()
                    .cache(cache)
                    .client(client)
                    .url(url)
                    .principal_extractor(DefaultPrincipalExtractor {})
                    .build(),
            })
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
            exp: now + Duration::from_hours(1).as_secs(),
            nbf: Some(now - Duration::from_mins(1).as_secs()),
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
            tenant_id: "tenant".to_owned(),
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
    #[test_log::test]
    async fn claim_test_valid_hmac() {
        CRYPTO.call_once(|| {
            _ = rustls::crypto::ring::default_provider().install_default();
        });

        async fn handle(_: HeaderMap) -> Response {
            Response::builder().status(StatusCode::OK).body(Body::empty()).expect("Expecting this to work")
        }

        let token = get_hmac_token_for_claims(&active_test_claims());

        let decoding_key = DecodingKey::from_secret(HMAC_SECRET);
        let verfication_key = VerificationKey::new(Some("HS256".to_owned()), decoding_key);

        let state = ContextForgeDataPlaneAppState {
            authorization_service: Arc::new(
                JwtAuthorizationService::from_keys(vec![verfication_key]).await.expect("this should work"),
            ),
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
        let verfication_key = VerificationKey::new(Some("HS256".to_owned()), decoding_key);
        let state = ContextForgeDataPlaneAppState {
            authorization_service: Arc::new(
                JwtAuthorizationService::from_keys(vec![verfication_key]).await.expect("this should work"),
            ),
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
        let verfication_key = VerificationKey::new(Some("HS256".to_owned()), decoding_key);

        let state = ContextForgeDataPlaneAppState {
            authorization_service: Arc::new(
                JwtAuthorizationService::from_keys(vec![verfication_key]).await.expect("this should work"),
            ),
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

        let decoding_key = DecodingKey::from_secret(HMAC_SECRET);
        let verfication_key = VerificationKey::new(Some("HS256".to_owned()), decoding_key);

        let state = ContextForgeDataPlaneAppState {
            authorization_service: Arc::new(
                JwtAuthorizationService::from_keys(vec![verfication_key]).await.expect("this should work"),
            ),
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
