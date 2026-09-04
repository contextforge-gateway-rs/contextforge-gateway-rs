use std::time::Duration;

use futures::StreamExt as _;
use jsonwebtoken::{
    Algorithm, AlgorithmFamily, DecodingKey, Header, Validation, decode,
    jwk::{Jwk, JwkSet, KeyOperations, PublicKeyUse},
};
use lru_time_cache::LruCache;

use reqwest::Url;
use serde_json::Value;
use tokio::sync::RwLock;
use tracing::debug;
use typed_builder::TypedBuilder;

use crate::authorization::{
    AuthorizationClaims, AuthorizationError,
    jwks::principal::{DefaultPrincipalExtractor, PrincipalExtractor},
};

pub const JWKS_CACHE_TTL: Duration = Duration::from_mins(5);
pub const JWKS_CACHE_KEY: &str = "jwks";

const JWKS_MAX_RESPONSE_BYTES: usize = 1024 * 1024;

#[derive(TypedBuilder)]
pub(super) struct Jwks<T>
where
    T: PrincipalExtractor,
{
    client: reqwest::Client,
    url: Url,
    #[builder(default = RwLock::new(LruCache::with_expiry_duration(JWKS_CACHE_TTL)))]
    cache: RwLock<LruCache<String, Vec<VerificationKey>>>,
    #[builder(default = false)]
    validate_audience: bool,
    #[builder(default = true)]
    validate_expiry: bool,
    #[builder(default = true)]
    validate_not_before: bool,
    principal_extractor: T,
}

impl Jwks<DefaultPrincipalExtractor> {
    fn validation(&self, alg: Algorithm) -> Validation {
        let mut validation = Validation::new(alg);
        validation.required_spec_claims.clear();
        validation.validate_aud = self.validate_audience;
        validation.validate_exp = self.validate_expiry;
        validation.validate_nbf = self.validate_not_before;
        validation
    }

    pub async fn validate(&self, token: &str, header: &Header) -> Option<AuthorizationClaims> {
        {
            let cache = self.cache.read().await;
            if let Some(keys) = cache.peek(JWKS_CACHE_KEY)
                && keys.iter().any(|key| key.matches(header))
            {
                return self.validate_with_keys(keys, token, header, &self.validation(header.alg));
            }
        }

        match fetch_jwks(&self.client, &self.url).await {
            Ok(keys) => {
                let key_count = keys.len();
                let claims = self.validate_with_keys(&keys, token, header, &self.validation(header.alg));
                self.cache.write().await.insert(JWKS_CACHE_KEY.to_owned(), keys);
                tracing::info!("validate: SaaS JWKS cache refreshed {key_count}");

                claims
            },
            Err(error) => {
                tracing::info!("validate: unable to refresh SaaS JWKS {error:?}");
                None
            },
        }
    }

    fn validate_with_keys(
        &self,
        keys: &[VerificationKey],
        token: &str,
        header: &Header,
        validation: &Validation,
    ) -> Option<AuthorizationClaims> {
        keys.iter()
            .filter(|key| key.matches(header))
            .find_map(|key| self.validate_and_decode_claims(token, &key.decoding_key, validation))
    }

    fn validate_and_decode_claims(
        &self,
        token: &str,
        key: &DecodingKey,
        validation: &Validation,
    ) -> Option<AuthorizationClaims> {
        let claims = decode::<Value>(token, key, validation)
            .inspect_err(|e| {
                debug!("validate_and_decode_claims: problem {e:?}");
            })
            .ok()?
            .claims;
        let claims = claims.as_object()?;
        let user_id = self.principal_extractor.user_id(claims)?;
        let tenant_id = self.principal_extractor.tenant_id(claims)?;

        Some(AuthorizationClaims::new(user_id, tenant_id))
    }
}

pub struct VerificationKey {
    pub(crate) key_id: Option<String>,
    pub(crate) decoding_key: DecodingKey,
}

impl VerificationKey {
    fn from_jwk(jwk: Jwk) -> Result<Option<Self>, AuthorizationError> {
        if jwk.common.public_key_use.as_ref().is_some_and(|key_use| key_use != &PublicKeyUse::Signature)
            || jwk.common.key_operations.as_ref().is_some_and(|operations| !operations.contains(&KeyOperations::Verify))
        {
            return Ok(None);
        }

        let decoding_key = DecodingKey::from_jwk(&jwk).map_err(AuthorizationError::InvalidKey)?;
        if !matches!(decoding_key.family(), AlgorithmFamily::Rsa | AlgorithmFamily::Ec) {
            return Ok(None);
        }

        Ok(Some(Self { key_id: jwk.common.key_id, decoding_key }))
    }

    pub(super) fn matches(&self, header: &Header) -> bool {
        self.decoding_key.family() == header.alg.family()
            && header
                .kid
                .as_ref()
                .is_none_or(|header_key_id| self.key_id.as_ref().is_none_or(|key_id| key_id == header_key_id))
    }
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

pub(super) fn validated_json_web_keys(
    jwks: impl IntoIterator<Item = Jwk>,
) -> Result<Vec<VerificationKey>, AuthorizationError> {
    let mut keys = Vec::new();
    for jwk in jwks {
        if let Some(key) = VerificationKey::from_jwk(jwk)? {
            keys.push(key);
        }
    }

    if keys.is_empty() {
        return Err(AuthorizationError::NoSupportedKeys);
    }
    Ok(keys)
}
