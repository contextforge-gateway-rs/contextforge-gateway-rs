use axum::{extract::Request, middleware::Next, response::Response};

use contextforge_data_plane_apis::User;
use tracing::debug;
use typed_builder::TypedBuilder;

use crate::{AuthorizationClaims, errors::unauthorized_response};

#[derive(Debug, Clone, TypedBuilder)]
#[allow(dead_code)]
pub struct AuthorizedPrincipal {
    user_id: String,
    tenant_id: String,
    scopes: Vec<String>,
}

impl<'a> From<&'a AuthorizedPrincipal> for User<'a> {
    fn from(value: &'a AuthorizedPrincipal) -> Self {
        Self::new(&value.user_id)
    }
}

impl TryFrom<&AuthorizationClaims> for AuthorizedPrincipal {
    type Error = Box<dyn std::error::Error + Send + Sync>;

    fn try_from(value: &AuthorizationClaims) -> Result<Self, Self::Error> {
        Ok(AuthorizedPrincipal::builder()
            .user_id(value.sub.clone())
            .tenant_id(value.tenant_id.clone())
            .scopes(vec![])
            .build())
    }
}

pub async fn principal_extractor_layer(request: http::Request<axum::body::Body>, next: Next) -> Response {
    let maybe_claims = request.extensions().get::<AuthorizationClaims>();
    let Some(Ok(authorized_principal)) = maybe_claims.map(|claims| {
        AuthorizedPrincipal::try_from(claims).inspect_err(|e| debug!("Can't extract the principal {e:?}"))
    }) else {
        return unauthorized_response("Invalid token. Unable to extract the principal from claims");
    };
    let (mut parts, body) = request.into_parts();
    parts.extensions.insert(authorized_principal);
    let request = Request::from_parts(parts, body);
    next.run(request).await
}
