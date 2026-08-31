use std::fs;

use axum::{
    Json,
    body::Body,
    extract::{Path, Query, State},
    response::{IntoResponse, Response},
    routing::{Router, get, post},
};
use chrono::Duration;
use contextforge_data_plane_apis::{User as CFUser, user_store::UserConfig};
use http::{
    StatusCode,
    header::{self, CACHE_CONTROL},
};
use jsonwebtoken::jwk::{Jwk, JwkSet};
use serde::Deserialize;
use uuid::Uuid;

//use tracing::debug;
use crate::{
    common::{ContextForgeClaims, ContextForgeDataPlaneAppState, Scopes, User},
    const_values::{CONTEXT_FORGE_GATEWAY_AUDIENCE, CONTEXT_FORGE_GATEWAY_ISSUER},
};

const DEFAULT_TOKEN_EMAIL: &str = "admin@example.com";
const JWKS_CACHE_CONTROL: &str = "public, max-age=300, must-revalidate";
const TOKEN_PATH: &str = "/admin/tokens/{tenant_id}/{user_id}";
const JWKS_PATH: &str = "/admin/.well-known/jwks.json";
const CONFIGURE_USER_PATH: &str = "admin/userconfigs/{user_id}";

#[derive(Debug, Deserialize)]
pub struct TokenQuery {
    email: Option<String>,
}

impl ContextForgeClaims {
    pub fn new(user_id: &str, user_email: &str) -> Self {
        let audience = CONTEXT_FORGE_GATEWAY_AUDIENCE.to_owned();
        let start = std::time::SystemTime::now();
        let now = start.duration_since(std::time::UNIX_EPOCH).expect("Time went backwards").as_secs();
        Self {
            iss: CONTEXT_FORGE_GATEWAY_ISSUER.to_owned(),
            sub: user_id.to_owned(),
            aud: audience,
            exp: now + Duration::hours(1).num_seconds().cast_unsigned(),
            iat: Some(now),
            jti: Uuid::new_v4().to_string(),
            token_use: Some("api".to_owned()),
            teams: Some(vec!["team_awesome".to_owned()]),
            user: User::builder()
                .email(user_email.to_owned())
                .auth_provider("api_token".to_owned())
                .full_name(Some("API Token User".to_owned()))
                .is_admin(true)
                .build(),
            scopes: Some(
                Scopes::builder()
                    .server_id(Some("my_id".to_owned()))
                    .ip_restrictions(vec!["192.169.1.0/24".to_owned()])
                    .permissions(vec!["tools.read".to_owned(), "servers.use".to_owned()])
                    .time_restrictions(None)
                    .build(),
            ),
        }
    }
}

async fn get_jwks(State(state): State<ContextForgeDataPlaneAppState>) -> Response {
    let Ok(key) = jsonwebtoken::EncodingKey::from_rsa_pem(
        &fs::read(&state.config.token_verification_private_key).expect("Expecting this to work"),
    ) else {
        return (StatusCode::INTERNAL_SERVER_ERROR, "Can't find the encoding key or the format is wrong")
            .into_response();
    };

    let Ok(key) = Jwk::from_encoding_key(&key, jsonwebtoken::Algorithm::RS256) else {
        return (StatusCode::INTERNAL_SERVER_ERROR, "Can't find the encoding key or the format is wrong")
            .into_response();
    };

    let keys = vec![key];
    (StatusCode::OK, [(CACHE_CONTROL, JWKS_CACHE_CONTROL)], Json(JwkSet { keys })).into_response()
}

pub fn add_tools(router: Router<ContextForgeDataPlaneAppState>) -> Router<ContextForgeDataPlaneAppState> {
    router
        .route(TOKEN_PATH, get(get_token))
        .route(JWKS_PATH, get(get_jwks))
        .route(CONFIGURE_USER_PATH, post(configure_user))
        .route("/health", get(health))
}

pub async fn health() -> Response {
    Response::builder()
        .status(StatusCode::OK)
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from("{\"status\": \"healthy\"}"))
        .expect("Expecting this to work")
}

pub async fn get_token(
    State(state): State<ContextForgeDataPlaneAppState>,
    Path(user_id): Path<String>,
    Query(query): Query<TokenQuery>,
) -> Response {
    let key = jsonwebtoken::EncodingKey::from_rsa_pem(
        &fs::read(&state.config.token_verification_private_key).expect("Expecting this to work"),
    )
    .expect("Expecting this to work");

    let user_email = query.email.as_deref().unwrap_or(DEFAULT_TOKEN_EMAIL);
    let claims = ContextForgeClaims::new(&user_id, user_email);
    let mut header = jsonwebtoken::Header::new(jsonwebtoken::Algorithm::RS256);
    header.kid = Some("test".to_owned());
    let token = jsonwebtoken::encode::<ContextForgeClaims>(&header, &claims, &key).expect("Expecting this to work");

    token.into_response()
}

//#[debug_handler]
pub async fn configure_user(
    Path(user_id): Path<String>,
    State(state): State<ContextForgeDataPlaneAppState>,
    Json(user_config): Json<UserConfig>,
) -> Response {
    if state.config_store.set_config(&CFUser::new(&user_id), &user_config).await.is_ok() {
        Response::builder()
            .status(StatusCode::ACCEPTED)
            .header(header::CONTENT_TYPE, "text/plain")
            .body(Body::from("Added"))
            .expect("Expecting this to work")
    } else {
        Response::builder()
            .status(StatusCode::INTERNAL_SERVER_ERROR)
            .header(header::CONTENT_TYPE, "text/plain")
            .body(Body::from("Problem with encoding "))
            .expect("Expecting this to work")
    }
}

#[cfg(test)]
mod tests {
    use serde_json::Value;

    use super::ContextForgeClaims;

    #[test]
    fn new_claims_keeps_subject_and_email_metadata_separate() {
        let claims = ContextForgeClaims::new("11111111-1111-1111-1111-111111111111", "admin@example.com");

        let payload = serde_json::to_value(claims).expect("claims should serialize");

        assert_eq!(payload["sub"], Value::String("11111111-1111-1111-1111-111111111111".to_owned()));
        assert_eq!(payload["user"]["email"], Value::String("admin@example.com".to_owned()));
    }
}
