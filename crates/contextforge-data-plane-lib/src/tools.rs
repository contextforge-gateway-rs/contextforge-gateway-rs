use axum::{
    Json,
    body::Body,
    extract::{Path, Query, State},
    response::{IntoResponse, Response},
    routing::{Router, get, post},
};
use contextforge_data_plane_apis::{User as CFUser, user_store::UserConfig};
use http::{
    StatusCode,
    header::{self, CACHE_CONTROL},
};
use jsonwebtoken::jwk::{Jwk, JwkSet};
use serde::Deserialize;
use std::fs;

use crate::{authorization::AuthorizationClaims, common::ContextForgeDataPlaneAppState};

const DEFAULT_TOKEN_EMAIL: &str = "admin@example.com";
const JWKS_CACHE_CONTROL: &str = "public, max-age=300, must-revalidate";
const TOKEN_PATH: &str = "/admin/tokens/{tenant_id}/{user_id}";
const JWKS_PATH: &str = "/admin/.well-known/jwks.json";
const CONFIGURE_USER_PATH: &str = "/admin/userconfigs/{user_id}";

#[derive(Debug, Deserialize)]
pub struct TokenQuery {
    email: Option<String>,
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
    Path((tenant_id, user_id)): Path<(String, String)>,
    Query(query): Query<TokenQuery>,
) -> Response {
    let key = jsonwebtoken::EncodingKey::from_rsa_pem(
        &fs::read(&state.config.token_verification_private_key).expect("Expecting this to work"),
    )
    .expect("Expecting this to work");

    let user_email = query.email.as_deref().unwrap_or(DEFAULT_TOKEN_EMAIL);
    let mut claims = AuthorizationClaims::new(&user_id, user_email);
    claims.tenant_id = tenant_id;
    let mut header = jsonwebtoken::Header::new(jsonwebtoken::Algorithm::RS256);
    header.kid = Some("test".to_owned());
    let token = jsonwebtoken::encode::<AuthorizationClaims>(&header, &claims, &key).expect("Expecting this to work");

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

    use crate::authorization::AuthorizationClaims;

    #[test]
    fn new_claims_keeps_subject_and_email_metadata_separate() {
        let claims = AuthorizationClaims::new("11111111-1111-1111-1111-111111111111", "admin@example.com");

        let payload = serde_json::to_value(claims).expect("claims should serialize");

        assert_eq!(payload["sub"], Value::String("11111111-1111-1111-1111-111111111111".to_owned()));
        assert_eq!(payload["user"]["email"], Value::String("admin@example.com".to_owned()));
    }
}
