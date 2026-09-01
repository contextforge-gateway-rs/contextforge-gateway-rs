use std::{path::PathBuf, sync::Arc};

use async_trait::async_trait;
use chrono::Duration;
use http::HeaderValue;
use serde::{Deserialize, Serialize};
use typed_builder::TypedBuilder;

use crate::Config;

mod jwks;

pub const AUDIENCE: &str = "audience";
pub const ISSUER: &str = "issuer";

pub fn get_authorization_service(
    config: &Config,
) -> Result<Arc<dyn AuthorizationService + Send + Sync>, AuthorizationError> {
    let service =
        jwks::JwtAuthorizationService::from_jwks_url(config.jwks_url.clone(), config.jwks_ca_cert_path.as_ref())?;
    Ok(Arc::new(service) as Arc<dyn AuthorizationService + Send + Sync>)
}

#[async_trait]
pub trait AuthorizationService: std::fmt::Debug {
    async fn authorize(&self, authorization_token: &HeaderValue) -> Option<AuthorizationClaims>;
}

#[derive(Debug, thiserror::Error)]
#[allow(dead_code)]
pub enum AuthorizationError {
    #[error("SaaS JWKS contains no supported signing keys")]
    NoSupportedKeys,
    #[error("SaaS JWKS is invalid")]
    InvalidJson(#[source] serde_json::Error),
    #[error("SaaS JWKS is invalid")]
    InvalidKey(#[source] jsonwebtoken::errors::Error),

    #[error("MCPOPS_JWKS_URL must use HTTPS (HTTP is allowed only for loopback testing)")]
    InsecureJwksUrl,
    #[error("unable to retrieve SaaS JWKS")]
    JwksRequest(#[source] reqwest::Error),
    #[error("unable to read JWKS CA certificate `{path}`")]
    ReadJwksCaCertificate {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("JWKS CA certificate `{path}` is invalid")]
    InvalidJwksCaCertificate {
        path: PathBuf,
        #[source]
        source: reqwest::Error,
    },
    #[error("JWKS CA certificate `{path}` contains no certificates")]
    EmptyJwksCaCertificate { path: PathBuf },
    #[error("SaaS JWKS response exceeds 1 MiB")]
    JwksResponseTooLarge,
}

#[derive(Clone, Debug, Serialize, Deserialize, TypedBuilder, PartialEq)]
pub struct User {
    pub user_id: String,
    pub tenant_id: String,
}

impl From<AuthorizationClaims> for User {
    fn from(claims: AuthorizationClaims) -> Self {
        Self { user_id: claims.idp_unique_id.clone(), tenant_id: claims.tenant_id.clone() }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, TypedBuilder, PartialEq)]
pub struct Scopes {
    server_id: Option<String>,
    permissions: Vec<String>,
    ip_restrictions: Vec<String>,
    time_restrictions: Option<serde_json::Value>,
}

#[derive(Clone, Debug, Serialize, Deserialize, TypedBuilder, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct Idp {
    real_name: String,
    iss: String,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Default, TypedBuilder)]
#[serde(rename_all = "camelCase")]
pub struct AuthorizationClaims {
    pub iss: String,
    pub jti: String,
    pub aud: String,
    pub exp: u64,
    pub iat: Option<u64>,
    pub nbf: Option<u64>,
    pub tenant_id: String,
    pub subscription_id: String,
    pub sub: String,
    pub entity_type: String,
    pub email: Option<String>,
    pub name: Option<String>,
    pub displayname: Option<String>,
    pub idp: Option<Idp>,
    pub groups: Option<Vec<String>>,
    pub roles: Option<Vec<String>>,
    pub idp_unique_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub teams: Option<Vec<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub user: Option<User>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub scopes: Option<Scopes>,
    pub token_use: Option<String>,
}

impl AuthorizationClaims {
    pub fn new(user_id: &str, tenant_id: &str) -> Self {
        let audience = AUDIENCE.to_owned();
        let start = std::time::SystemTime::now();
        let now = start.duration_since(std::time::UNIX_EPOCH).expect("Time went backwards").as_secs();
        Self {
            iss: ISSUER.to_owned(),
            sub: user_id.to_owned(),
            aud: audience,
            exp: now + Duration::hours(1).num_seconds().cast_unsigned(),
            iat: Some(now),
            nbf: Some(now - Duration::minutes(5).num_seconds().cast_unsigned()),
            idp_unique_id: user_id.to_owned(),
            tenant_id: tenant_id.to_owned(),
            groups: Some(vec!["team_awesome".to_owned()]),
            ..Default::default()
        }
    }
}
