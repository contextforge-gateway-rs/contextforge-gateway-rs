use http::HeaderName;
use rmcp::transport::common::http_header::HEADER_MCP_PARAM_PREFIX;

// Lowercase literals mirror RMCP header constants because HeaderName::from_static
// requires normalized lowercase input.
const MCP_METHOD: HeaderName = HeaderName::from_static("mcp-method");
const MCP_NAME: HeaderName = HeaderName::from_static("mcp-name");
const MCP_PROTOCOL_VERSION: HeaderName = HeaderName::from_static("mcp-protocol-version");
const MCP_SESSION_ID: HeaderName = HeaderName::from_static("mcp-session-id");

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

fn is_exact(name: &HeaderName, expected: &HeaderName) -> bool {
    name == expected
}

fn is_param(name: &HeaderName) -> bool {
    name.as_str()
        .get(..HEADER_MCP_PARAM_PREFIX.len())
        .is_some_and(|prefix| prefix.eq_ignore_ascii_case(HEADER_MCP_PARAM_PREFIX))
}
