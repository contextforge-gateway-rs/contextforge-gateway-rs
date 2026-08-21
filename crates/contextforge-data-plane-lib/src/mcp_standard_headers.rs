use http::HeaderName;
use rmcp::transport::common::http_header::{
    HEADER_MCP_METHOD, HEADER_MCP_NAME, HEADER_MCP_PARAM_PREFIX, HEADER_MCP_PROTOCOL_VERSION, HEADER_SESSION_ID,
};

pub(crate) fn is_limited(name: &HeaderName) -> bool {
    is_exact(name, HEADER_MCP_METHOD)
        || is_exact(name, HEADER_MCP_NAME)
        || is_exact(name, HEADER_MCP_PROTOCOL_VERSION)
        || is_exact(name, HEADER_SESSION_ID)
        || is_param(name)
}

pub(crate) fn is_computed(name: &HeaderName) -> bool {
    is_exact(name, HEADER_MCP_METHOD)
        || is_exact(name, HEADER_MCP_NAME)
        || is_exact(name, HEADER_MCP_PROTOCOL_VERSION)
        || is_param(name)
}

fn is_exact(name: &HeaderName, expected: &str) -> bool {
    name.as_str().eq_ignore_ascii_case(expected)
}

pub(crate) fn is_param(name: &HeaderName) -> bool {
    name.as_str()
        .get(..HEADER_MCP_PARAM_PREFIX.len())
        .is_some_and(|prefix| prefix.eq_ignore_ascii_case(HEADER_MCP_PARAM_PREFIX))
}
