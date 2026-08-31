use std::{
    net::IpAddr,
    path::{Path, PathBuf},
    time::Duration,
};

use futures::StreamExt as _;
use jsonwebtoken::{Header, Validation, jwk::JwkSet};
use lru_time_cache::LruCache;

use reqwest::Url;
use tokio::sync::RwLock;

use crate::authorization::{AuthorizationClaims, AuthorizationError};

use super::verification::{VerificationKey, decode_with_keys, validated_json_web_keys};

const JWKS_CACHE_TTL: Duration = Duration::from_mins(5);
const JWKS_CACHE_KEY: &str = "jwks";
const JWKS_REQUEST_TIMEOUT: Duration = Duration::from_secs(10);
const JWKS_CONNECT_TIMEOUT: Duration = Duration::from_secs(5);
const JWKS_READ_TIMEOUT: Duration = Duration::from_secs(5);
const JWKS_MAX_RESPONSE_BYTES: usize = 1024 * 1024;

pub(super) struct RemoteJwks {
    client: reqwest::Client,
    url: Url,
    cache: RwLock<LruCache<String, Vec<VerificationKey>>>,
}

impl RemoteJwks {
    pub(super) fn new(value: Url, ca_cert_path: Option<&PathBuf>) -> Result<Self, AuthorizationError> {
        let url = parse_jwks_url(value)?;
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
        Ok(Self { client, url, cache: RwLock::new(LruCache::with_expiry_duration(JWKS_CACHE_TTL)) })
    }

    pub(super) async fn decode(
        &self,
        token: &str,
        header: &Header,
        validation: &Validation,
    ) -> Option<AuthorizationClaims> {
        {
            let cache = self.cache.read().await;
            if let Some(keys) = cache.peek(JWKS_CACHE_KEY)
                && keys.iter().any(|key| key.matches(header))
            {
                return decode_with_keys(keys, token, header, validation);
            }
        }

        match fetch_jwks(&self.client, &self.url).await {
            Ok(keys) => {
                let key_count = keys.len();
                let claims = decode_with_keys(&keys, token, header, validation);
                self.cache.write().await.insert(JWKS_CACHE_KEY.to_owned(), keys);
                tracing::info!(
                    component = "Authorization",
                    operation = "refresh_jwks",
                    key_count,
                    "SaaS JWKS cache refreshed"
                );
                claims
            },
            Err(error) => {
                tracing::warn!(
                    component = "Authorization",
                    operation = "refresh_jwks",
                    root_cause = %error,
                    "unable to refresh SaaS JWKS"
                );
                None
            },
        }
    }
}

pub(super) fn load_ca_certificates(path: &Path) -> Result<Vec<reqwest::Certificate>, AuthorizationError> {
    let pem = std::fs::read(path)
        .map_err(|source| AuthorizationError::ReadJwksCaCertificate { path: path.to_owned(), source })?;
    let certificates = reqwest::Certificate::from_pem_bundle(&pem)
        .map_err(|source| AuthorizationError::InvalidJwksCaCertificate { path: path.to_owned(), source })?;
    if certificates.is_empty() {
        return Err(AuthorizationError::EmptyJwksCaCertificate { path: path.to_owned() });
    }
    Ok(certificates)
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

async fn fetch_jwks(client: &reqwest::Client, url: &Url) -> Result<Vec<VerificationKey>, AuthorizationError> {
    let response = client
        .get(url.clone())
        .send()
        .await
        .and_then(reqwest::Response::error_for_status)
        .map_err(AuthorizationError::JwksRequest)?;
    if response
        .content_length()
        .is_some_and(|length| length > u64::try_from(JWKS_MAX_RESPONSE_BYTES).unwrap_or(u64::MAX))
    {
        return Err(AuthorizationError::JwksResponseTooLarge);
    }
    let mut body = Vec::new();
    let mut chunks = response.bytes_stream();
    while let Some(chunk) = chunks.next().await {
        let chunk = chunk.map_err(AuthorizationError::JwksRequest)?;
        if chunk.len() > JWKS_MAX_RESPONSE_BYTES.saturating_sub(body.len()) {
            return Err(AuthorizationError::JwksResponseTooLarge);
        }
        body.extend_from_slice(&chunk);
    }
    let jwks = serde_json::from_slice::<JwkSet>(&body).map_err(AuthorizationError::InvalidJson)?;
    if jwks.keys.is_empty() { Ok(Vec::new()) } else { validated_json_web_keys(jwks.keys) }
}
