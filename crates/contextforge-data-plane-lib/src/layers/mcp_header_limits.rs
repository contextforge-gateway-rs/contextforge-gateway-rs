use axum::{body::Body, extract::State, middleware::Next, response::Response};
use http::{HeaderName, StatusCode, header};

use crate::common::{
    Config, DEFAULT_MCP_STANDARD_HEADER_MAX_COUNT, DEFAULT_MCP_STANDARD_HEADER_MAX_TOTAL_BYTES,
    DEFAULT_MCP_STANDARD_HEADER_MAX_VALUE_BYTES,
};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct McpStandardHeaderLimits {
    pub(crate) max_count: usize,
    pub(crate) max_value_bytes: usize,
    pub(crate) max_total_bytes: usize,
}

impl McpStandardHeaderLimits {
    pub(crate) fn from_config(config: &Config) -> Self {
        Self {
            max_count: configured_or_default(
                config.mcp_standard_header_max_count,
                DEFAULT_MCP_STANDARD_HEADER_MAX_COUNT,
            ),
            max_value_bytes: configured_or_default(
                config.mcp_standard_header_max_value_bytes,
                DEFAULT_MCP_STANDARD_HEADER_MAX_VALUE_BYTES,
            ),
            max_total_bytes: configured_or_default(
                config.mcp_standard_header_max_total_bytes,
                DEFAULT_MCP_STANDARD_HEADER_MAX_TOTAL_BYTES,
            ),
        }
    }
}

fn configured_or_default(configured: usize, default: usize) -> usize {
    if configured == 0 { default } else { configured }
}

pub(crate) async fn mcp_header_limits_layer(
    State(limits): State<McpStandardHeaderLimits>,
    request: http::Request<axum::body::Body>,
    next: Next,
) -> Response {
    if exceeds_limits(request.headers(), limits) {
        return Response::builder()
            .status(StatusCode::REQUEST_HEADER_FIELDS_TOO_LARGE)
            .header(header::CONTENT_TYPE, "text/plain")
            .body(Body::from("MCP standard header limits exceeded"))
            .expect("Expecting this to work");
    }

    next.run(request).await
}

fn exceeds_limits(headers: &http::HeaderMap, limits: McpStandardHeaderLimits) -> bool {
    let mut count = 0usize;
    let mut total_bytes = 0usize;

    for (name, value) in headers.iter().filter(|(name, _)| is_mcp_limited_header_name(name)) {
        count = count.saturating_add(1);
        if count > limits.max_count {
            return true;
        }

        let value_bytes = value.as_bytes().len();
        if value_bytes > limits.max_value_bytes {
            return true;
        }

        // Application budget only: this is not exact HTTP/1 wire size and does
        // not model HTTP/2 HPACK compression.
        total_bytes = total_bytes.saturating_add(name.as_str().len()).saturating_add(value_bytes);
        if total_bytes > limits.max_total_bytes {
            return true;
        }
    }

    false
}

pub(crate) fn is_mcp_limited_header_name(name: &HeaderName) -> bool {
    is_exact_mcp_header_name(name, "mcp-method")
        || is_exact_mcp_header_name(name, "mcp-name")
        || is_exact_mcp_header_name(name, "mcp-protocol-version")
        || is_exact_mcp_header_name(name, "mcp-session-id")
        || is_mcp_param_header_name(name)
}

pub(crate) fn is_mcp_computed_header_name(name: &HeaderName) -> bool {
    is_exact_mcp_header_name(name, "mcp-method")
        || is_exact_mcp_header_name(name, "mcp-name")
        || is_exact_mcp_header_name(name, "mcp-protocol-version")
        || is_mcp_param_header_name(name)
}

fn is_exact_mcp_header_name(name: &HeaderName, expected: &str) -> bool {
    name.as_str().eq_ignore_ascii_case(expected)
}

fn is_mcp_param_header_name(name: &HeaderName) -> bool {
    const PREFIX: &str = "mcp-param-";
    name.as_str().get(..PREFIX.len()).is_some_and(|prefix| prefix.eq_ignore_ascii_case(PREFIX))
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use async_trait::async_trait;
    use axum::{Router, body::Body, middleware, response::Response, routing::get};
    use contextforge_data_plane_apis::{User, user_store::UserConfig};
    use http::{Request, StatusCode};
    use tower::ServiceExt;

    use crate::{
        Config,
        common::{ContextForgeDataPlaneAppState, JwtTokenDecoders},
        layers::{
            claims_id::claims_layer,
            mcp_header_limits::{McpStandardHeaderLimits, mcp_header_limits_layer},
        },
        user_config_store::{ConfigStoreError, UserConfigStore},
    };

    async fn ok() -> Response {
        Response::builder().status(StatusCode::OK).body(Body::empty()).expect("Expecting this to work")
    }

    fn app(limits: McpStandardHeaderLimits) -> Router {
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
        let limits = McpStandardHeaderLimits { max_count: 2, max_value_bytes: 1024, max_total_bytes: 4096 };
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
        let limits = McpStandardHeaderLimits { max_count: 32, max_value_bytes: 4, max_total_bytes: 4096 };
        let response = app(limits)
            .oneshot(request_with_headers(&[("Mcp-Param-User", "alice")]))
            .await
            .expect("Expecting this to work");

        assert_eq!(response.status(), StatusCode::REQUEST_HEADER_FIELDS_TOO_LARGE);
    }

    #[tokio::test]
    async fn rejects_excessive_total_mcp_header_bytes() {
        let limits = McpStandardHeaderLimits { max_count: 32, max_value_bytes: 16, max_total_bytes: 24 };
        let response = app(limits)
            .oneshot(request_with_headers(&[("Mcp-Method", "tools/call"), ("Mcp-Name", "example")]))
            .await
            .expect("Expecting this to work");

        assert_eq!(response.status(), StatusCode::REQUEST_HEADER_FIELDS_TOO_LARGE);
    }

    #[tokio::test]
    async fn counts_mcp_headers_case_insensitively() {
        let limits = McpStandardHeaderLimits { max_count: 1, max_value_bytes: 1024, max_total_bytes: 4096 };
        let response = app(limits)
            .oneshot(request_with_headers(&[("McP-MeThOd", "tools/call"), ("mCp-PaRaM-User", "alice")]))
            .await
            .expect("Expecting this to work");

        assert_eq!(response.status(), StatusCode::REQUEST_HEADER_FIELDS_TOO_LARGE);
    }

    #[tokio::test]
    async fn ignores_non_mcp_headers_for_mcp_specific_budget() {
        let limits = McpStandardHeaderLimits { max_count: 1, max_value_bytes: 1024, max_total_bytes: 4096 };
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

    #[tokio::test]
    async fn rejects_excessive_mcp_headers_before_auth() {
        let limits = McpStandardHeaderLimits { max_count: 1, max_value_bytes: 1024, max_total_bytes: 4096 };
        let state = ContextForgeDataPlaneAppState {
            jwt_token_decoding_keys: JwtTokenDecoders { rs: None, hmac_sha: None },
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
