use axum::{extract::State, middleware::Next, response::Response};
use http::StatusCode;
use tracing::debug;

use crate::common::{
    Config, DEFAULT_MCP_STANDARD_HEADER_MAX_COUNT, DEFAULT_MCP_STANDARD_HEADER_MAX_TOTAL_BYTES,
    DEFAULT_MCP_STANDARD_HEADER_MAX_VALUE_BYTES,
};
use crate::errors::custom_error;
use crate::mcp_standard_headers;

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct StandardHeaderLimits {
    pub(crate) count: usize,
    pub(crate) value_bytes: usize,
    pub(crate) total_bytes: usize,
}

impl From<&Config> for StandardHeaderLimits {
    fn from(config: &Config) -> Self {
        Self {
            count: configured_or_default(config.mcp_standard_header_max_count, DEFAULT_MCP_STANDARD_HEADER_MAX_COUNT),
            value_bytes: configured_or_default(
                config.mcp_standard_header_max_value_bytes,
                DEFAULT_MCP_STANDARD_HEADER_MAX_VALUE_BYTES,
            ),
            total_bytes: configured_or_default(
                config.mcp_standard_header_max_total_bytes,
                DEFAULT_MCP_STANDARD_HEADER_MAX_TOTAL_BYTES,
            ),
        }
    }
}

fn configured_or_default(configured: usize, default: usize) -> usize {
    if configured == 0 { default } else { configured }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct StandardHeaderUsage {
    count: usize,
    value_bytes: usize,
    total_bytes: usize,
}

pub(crate) async fn mcp_header_limits_layer(
    State(limits): State<StandardHeaderLimits>,
    request: http::Request<axum::body::Body>,
    next: Next,
) -> Response {
    if let Some(usage) = exceeded_limits(request.headers(), &limits) {
        let count = usage.count;
        let value_bytes = usage.value_bytes;
        let total_bytes = usage.total_bytes;
        debug!(
            "mcp_header_limits_layer - rejecting request count = {count} value_bytes = {value_bytes} total_bytes = {total_bytes}"
        );
        return custom_error(StatusCode::REQUEST_HEADER_FIELDS_TOO_LARGE, "MCP standard header limits exceeded");
    }

    next.run(request).await
}

fn exceeded_limits(headers: &http::HeaderMap, limits: &StandardHeaderLimits) -> Option<StandardHeaderUsage> {
    let usage = mcp_standard_header_usage(headers);

    usage.exceeds(limits).then_some(usage)
}

fn mcp_standard_header_usage(headers: &http::HeaderMap) -> StandardHeaderUsage {
    let mut usage = StandardHeaderUsage { count: 0, value_bytes: 0, total_bytes: 0 };
    for (name, value) in headers.iter().filter(|(name, _)| mcp_standard_headers::is_limited(name)) {
        let value_bytes = value.as_bytes().len();

        usage.count = usage.count.saturating_add(1);
        usage.value_bytes = usage.value_bytes.max(value_bytes);
        // Application budget only: this is not exact HTTP/1 wire size and does
        // not model HTTP/2 HPACK compression.
        usage.total_bytes = usage.total_bytes.saturating_add(name.as_str().len()).saturating_add(value_bytes);
    }

    usage
}

impl StandardHeaderUsage {
    fn exceeds(self, limits: &StandardHeaderLimits) -> bool {
        self.count > limits.count || self.value_bytes > limits.value_bytes || self.total_bytes > limits.total_bytes
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use async_trait::async_trait;
    use axum::{Router, body::Body, middleware, response::Response, routing::get};
    use contextforge_data_plane_apis::{User, user_store::UserConfig};
    use http::{HeaderValue, Request, StatusCode};
    use tower::ServiceExt;

    use crate::{
        Config,
        authorization::{AuthorizationClaims, AuthorizationService},
        common::ContextForgeDataPlaneAppState,
        layers::{
            claims_id::claims_layer,
            mcp_header_limits::{StandardHeaderLimits, mcp_header_limits_layer},
        },
        user_config_store::{ConfigStoreError, UserConfigStore},
    };

    async fn ok() -> Response {
        Response::builder().status(StatusCode::OK).body(Body::empty()).expect("Expecting this to work")
    }

    fn app(limits: StandardHeaderLimits) -> Router {
        Router::new().route("/", get(ok)).layer(middleware::from_fn_with_state(limits, mcp_header_limits_layer))
    }

    fn request_with_headers(headers: &[(&str, &str)]) -> Request<Body> {
        let mut builder = Request::builder().uri("/");
        for (name, value) in headers {
            builder = builder.header(*name, *value);
        }
        builder.body(Body::empty()).expect("Expecting this to work")
    }

    #[tokio::test]
    async fn rejects_too_many_mcp_headers() {
        let limits = StandardHeaderLimits { count: 2, value_bytes: 1024, total_bytes: 4096 };
        let response = app(limits)
            .oneshot(request_with_headers(&[
                ("Mcp-Method", "tools/call"),
                ("Mcp-Name", "example"),
                ("Mcp-Param-User", "alice"),
            ]))
            .await
            .expect("Expecting this to work");

        assert_eq!(response.status(), StatusCode::REQUEST_HEADER_FIELDS_TOO_LARGE);
    }

    #[tokio::test]
    async fn rejects_oversized_mcp_header_value() {
        let limits = StandardHeaderLimits { count: 32, value_bytes: 4, total_bytes: 4096 };
        let response = app(limits)
            .oneshot(request_with_headers(&[("Mcp-Param-User", "alice")]))
            .await
            .expect("Expecting this to work");

        assert_eq!(response.status(), StatusCode::REQUEST_HEADER_FIELDS_TOO_LARGE);
    }

    #[tokio::test]
    async fn rejects_excessive_total_mcp_header_bytes() {
        let limits = StandardHeaderLimits { count: 32, value_bytes: 16, total_bytes: 24 };
        let response = app(limits)
            .oneshot(request_with_headers(&[("Mcp-Method", "tools/call"), ("Mcp-Name", "example")]))
            .await
            .expect("Expecting this to work");

        assert_eq!(response.status(), StatusCode::REQUEST_HEADER_FIELDS_TOO_LARGE);
    }

    #[tokio::test]
    async fn counts_mcp_headers_case_insensitively() {
        let limits = StandardHeaderLimits { count: 1, value_bytes: 1024, total_bytes: 4096 };
        let response = app(limits)
            .oneshot(request_with_headers(&[("McP-MeThOd", "tools/call"), ("mCp-PaRaM-User", "alice")]))
            .await
            .expect("Expecting this to work");

        assert_eq!(response.status(), StatusCode::REQUEST_HEADER_FIELDS_TOO_LARGE);
    }

    #[tokio::test]
    async fn ignores_non_mcp_headers_for_mcp_specific_budget() {
        let limits = StandardHeaderLimits { count: 1, value_bytes: 1024, total_bytes: 4096 };
        let response = app(limits)
            .oneshot(request_with_headers(&[
                ("X-One", "1"),
                ("X-Two", "2"),
                ("X-Three", "3"),
                ("Mcp-Method", "tools/call"),
            ]))
            .await
            .expect("Expecting this to work");

        assert_eq!(response.status(), StatusCode::OK);
    }

    #[derive(Clone)]
    struct UnusedConfigStore;

    #[async_trait]
    impl UserConfigStore for UnusedConfigStore {
        async fn get_config<'a>(&self, _key: &'a User) -> Result<UserConfig, ConfigStoreError> {
            unreachable!("mcp header limit rejection must run before config lookup")
        }

        async fn set_config<'a>(&self, _key: &'a User, _user_config: &'a UserConfig) -> Result<(), ConfigStoreError> {
            unreachable!("mcp header limit rejection must run before config lookup")
        }
    }

    #[derive(Debug)]
    pub struct Noop;

    #[async_trait]
    impl AuthorizationService for Noop {
        async fn authorize(&self, _: &HeaderValue) -> Option<AuthorizationClaims> {
            None
        }
    }

    #[tokio::test]
    async fn rejects_excessive_mcp_headers_before_auth() {
        let limits = StandardHeaderLimits { count: 1, value_bytes: 1024, total_bytes: 4096 };
        let state = ContextForgeDataPlaneAppState {
            authorization_service: Arc::new(Noop {}),
            config_store: Arc::new(UnusedConfigStore),
            config: Config::default(),
        };
        let app = Router::new()
            .route("/", get(ok))
            .layer(middleware::from_fn_with_state(state, claims_layer))
            .layer(middleware::from_fn_with_state(limits, mcp_header_limits_layer));

        let response = app
            .oneshot(request_with_headers(&[("Mcp-Method", "tools/call"), ("Mcp-Name", "example")]))
            .await
            .expect("Expecting this to work");

        assert_eq!(response.status(), StatusCode::REQUEST_HEADER_FIELDS_TOO_LARGE);
    }
}
