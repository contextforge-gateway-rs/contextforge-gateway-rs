use std::sync::LazyLock;

use http::HeaderName;
use rmcp::transport::common::http_header::{
    HEADER_MCP_METHOD, HEADER_MCP_NAME, HEADER_MCP_PARAM_PREFIX, HEADER_MCP_PROTOCOL_VERSION, HEADER_SESSION_ID,
};

static MCP_METHOD: LazyLock<HeaderName> = LazyLock::new(|| header_name(HEADER_MCP_METHOD));
static MCP_NAME: LazyLock<HeaderName> = LazyLock::new(|| header_name(HEADER_MCP_NAME));
static MCP_PROTOCOL_VERSION: LazyLock<HeaderName> = LazyLock::new(|| header_name(HEADER_MCP_PROTOCOL_VERSION));
static MCP_SESSION_ID: LazyLock<HeaderName> = LazyLock::new(|| header_name(HEADER_SESSION_ID));

pub(crate) fn is_limited(name: &HeaderName) -> bool {
    is_exact(name, &MCP_METHOD)
        || is_exact(name, &MCP_NAME)
        || is_exact(name, &MCP_PROTOCOL_VERSION)
        || is_exact(name, &MCP_SESSION_ID)
        || is_param(name)
}

pub(crate) fn is_computed(name: &HeaderName) -> bool {
    is_exact(name, &MCP_METHOD) || is_exact(name, &MCP_NAME) || is_exact(name, &MCP_PROTOCOL_VERSION) || is_param(name)
}

fn header_name(name: &str) -> HeaderName {
    HeaderName::from_bytes(name.as_bytes()).expect("RMCP header constant must be a valid HTTP header name")
}

fn is_exact(name: &HeaderName, expected: &LazyLock<HeaderName>) -> bool {
    name == **expected
}

fn is_param(name: &HeaderName) -> bool {
    name.as_str()
        .get(..HEADER_MCP_PARAM_PREFIX.len())
        .is_some_and(|prefix| prefix.eq_ignore_ascii_case(HEADER_MCP_PARAM_PREFIX))
}
