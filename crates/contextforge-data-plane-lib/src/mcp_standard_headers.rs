use http::HeaderName;

pub(crate) fn is_limited(name: &HeaderName) -> bool {
    is_exact(name, "mcp-method")
        || is_exact(name, "mcp-name")
        || is_exact(name, "mcp-protocol-version")
        || is_exact(name, "mcp-session-id")
        || is_param(name)
}

pub(crate) fn is_computed(name: &HeaderName) -> bool {
    is_exact(name, "mcp-method")
        || is_exact(name, "mcp-name")
        || is_exact(name, "mcp-protocol-version")
        || is_param(name)
}

fn is_exact(name: &HeaderName, expected: &str) -> bool {
    name.as_str().eq_ignore_ascii_case(expected)
}

fn is_param(name: &HeaderName) -> bool {
    const PREFIX: &str = "mcp-param-";
    name.as_str().get(..PREFIX.len()).is_some_and(|prefix| prefix.eq_ignore_ascii_case(PREFIX))
}
