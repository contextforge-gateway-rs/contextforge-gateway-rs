use std::fmt;
use std::path::PathBuf;

use crate::authorization::jwks::remote_jwks::RemoteJwks;
use crate::authorization::{AuthorizationClaims, AuthorizationError, AuthorizationService};
use async_trait::async_trait;

use jsonwebtoken::{Validation, decode_header};
use url::Url;

pub struct JwtAuthorizationService {
    jwks: RemoteJwks,
}

impl JwtAuthorizationService {
    pub fn from_jwks_url(jwks_url: Url, ca_cert_path: Option<&PathBuf>) -> Result<Self, AuthorizationError> {
        Ok(Self { jwks: RemoteJwks::new(jwks_url, ca_cert_path)? })
    }

    async fn authorize_token(&self, token: &str) -> Option<AuthorizationClaims> {
        let header = decode_header(token).ok()?;
        header.kid.as_deref()?;

        let mut validation = Validation::new(header.alg);
        validation.required_spec_claims.clear();
        validation.validate_aud = false;
        validation.validate_exp = true;
        validation.validate_nbf = true;

        self.jwks.decode(token, &header, &validation).await
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
            tracing::debug!(component = "Authorization", operation = "validate_saas_jwt", "SaaS JWT was rejected");
        }

        claims
    }
}
