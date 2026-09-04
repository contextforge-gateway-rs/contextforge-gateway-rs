#[allow(clippy::module_inception)]
mod jwks;
mod jwks_authorization;
mod principal;

pub use jwks_authorization::JwtAuthorizationService;
