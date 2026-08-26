use contextforge_data_plane_apis::user_store::VirtualHost;
use rmcp::{ErrorData, model::ErrorCode, service::ServiceError};
use tracing::warn;

/// Preserves identifiers for a single backend. For multiple backends, splits a
/// `{backend}-{identifier}` namespace so duplicate identifiers remain routable.
fn route_identifier<'a, N: AsRef<str>>(identifier: &'a str, backend_names: &'a [N]) -> Option<(&'a str, &'a str)> {
    if let [backend] = backend_names {
        return Some((backend.as_ref(), identifier));
    }

    backend_names.iter().find_map(|backend| {
        let backend = backend.as_ref();
        identifier.strip_prefix(backend)?.strip_prefix('-').map(|rest| (backend, rest))
    })
}

/// Joins a backend name and a backend-local name into the namespaced `{backend}-{rest}` form.
pub(crate) fn prefixed_name(backend_name: &str, rest: &str) -> String {
    format!("{backend_name}-{rest}")
}

/// Resolves an exact control-plane alias to its backend and upstream name. Without an alias,
/// single-backend hosts preserve the upstream name and multi-backend hosts use the legacy prefix.
pub(super) fn resolve_tool_route<'a, N: AsRef<str>>(
    virtual_host: &'a VirtualHost,
    name: &'a str,
    backend_names: &'a [N],
) -> Option<(&'a str, &'a str)> {
    let mut aliases = backend_names.iter().filter_map(|backend_name| {
        let backend_name = backend_name.as_ref();
        let original_name = virtual_host.backends.get(backend_name)?.tool_name_aliases.get(name)?;
        Some((backend_name, original_name.as_str()))
    });
    let alias = aliases.next();
    if aliases.next().is_some() {
        return None;
    }
    alias.or_else(|| route_identifier(name, backend_names))
}

pub(super) fn resolve_resources_route<'a, N: AsRef<str>>(
    virtual_host: &'a VirtualHost,
    name: &'a str,
    backend_names: &'a [N],
) -> Option<(&'a str, &'a str)> {
    let mut aliases = backend_names.iter().filter_map(|backend_name| {
        let backend_name = backend_name.as_ref();
        let original_name = virtual_host.backends.get(backend_name)?.resource_name_aliases.get(name)?;
        Some((backend_name, original_name.as_str()))
    });
    let alias = aliases.next();
    if aliases.next().is_some() {
        return None;
    }
    alias.or_else(|| route_identifier(name, backend_names))
}

pub(super) fn resolve_prompt_route<'a, N: AsRef<str>>(
    virtual_host: &'a VirtualHost,
    name: &'a str,
    backend_names: &'a [N],
) -> Option<(&'a str, &'a str)> {
    let mut aliases = backend_names.iter().filter_map(|backend_name| {
        let backend_name = backend_name.as_ref();
        let original_name = virtual_host.backends.get(backend_name)?.prompt_name_aliases.get(name)?;
        Some((backend_name, original_name.as_str()))
    });
    let alias = aliases.next();
    if aliases.next().is_some() {
        return None;
    }
    alias.or_else(|| route_identifier(name, backend_names))
}

/// Returns the control-plane alias for an upstream tool when configured. Without an alias,
/// single-backend hosts preserve the upstream name and multi-backend hosts use the legacy prefix.
#[allow(dead_code)]
pub(super) fn exposed_tool_name(virtual_host: &VirtualHost, backend_name: &str, original_name: &str) -> String {
    virtual_host
        .backends
        .get(backend_name)
        .and_then(|backend| {
            backend
                .tool_name_aliases
                .iter()
                .find_map(|(alias, original)| (original == original_name).then(|| alias.clone()))
        })
        .unwrap_or_else(|| {
            if virtual_host.backends.len() == 1 {
                original_name.to_owned()
            } else {
                prefixed_name(backend_name, original_name)
            }
        })
}

pub(super) fn backend_forward_error(op: &str, backend_name: &str, error: &ServiceError) -> ErrorData {
    warn!("{op}: backend {backend_name} error = {error:?}");

    match error {
        ServiceError::McpError(mcp_error) => mcp_error.to_owned(),
        _ => ErrorData {
            code: ErrorCode::INTERNAL_ERROR,
            message: "Routing problem... got no responses from backends".into(),
            data: None,
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn multi_backend_route_requires_exact_backend_prefix() {
        let backend_names = vec!["counter-on", "counter-oneee", "counter-one"];
        assert_eq!(Some(("counter-one", "increment")), route_identifier("counter-one-increment", &backend_names));
        assert_eq!(None, route_identifier("counter-oneincrement", &backend_names));
        assert_eq!(None, route_identifier("counteroneincrement", &backend_names));
        assert_eq!(Some(("counter-one", "get-value")), route_identifier("counter-one-get-value", &backend_names));

        // Tool, resource, and prompt routing all share this splitter.
        assert_eq!(
            Some(("counter-one", "example-prompt")),
            route_identifier("counter-one-example-prompt", &backend_names)
        );
        assert_eq!(None, route_identifier("counter-oneexample-prompt", &backend_names));

        let backend_names = vec!["counter_on", "counter_oneee", "counter_one"];
        assert_eq!(Some(("counter_one", "get-value")), route_identifier("counter_one-get-value", &backend_names));
    }

    #[test]
    fn single_backend_routes_unprefixed_identifier_unchanged() {
        let backend_names = vec!["backend-id"];

        assert_eq!(Some(("backend-id", "test_simple_text")), route_identifier("test_simple_text", &backend_names));
        assert_eq!(Some(("backend-id", "backend-id-tool")), route_identifier("backend-id-tool", &backend_names));
        assert_eq!(
            Some(("backend-id", "test://template/123/data")),
            route_identifier("test://template/123/data", &backend_names)
        );
    }

    #[test]
    fn control_plane_alias_is_advertised_and_routes_to_original_name() {
        let config_json = serde_json::json!({
            "backends": {
                "79fabb70-2188-4de8-95ed-dc1e976e14d4": {
                    "name": "compliance_reference",
                    "url": "http://upstream:9000/mcp",
                    "passthrough_headers": [],
                    "allowed_tool_names": ["get_stats", "echo"],
                    "tool_name_aliases": {
                        "Public.Tool": "get_stats",
                        "Echo_Tool": "echo"
                    },
                    "allowed_resource_names": [],
                    "allowed_prompt_names": [],
                    "disable_tool_names_filtering": true,
                    "disable_prompt_names_filtering": true,
                    "disable_resource_names_filtering": true,
                }
            }
        });
        let virtual_host: VirtualHost = serde_json::from_value(config_json).expect("valid virtual host");
        let backend_ids = vec!["79fabb70-2188-4de8-95ed-dc1e976e14d4"];

        assert_eq!(
            "Public.Tool",
            exposed_tool_name(&virtual_host, "79fabb70-2188-4de8-95ed-dc1e976e14d4", "get_stats")
        );
        assert_eq!(
            Some(("79fabb70-2188-4de8-95ed-dc1e976e14d4", "get_stats")),
            resolve_tool_route(&virtual_host, "Public.Tool", &backend_ids)
        );
    }

    #[test]
    fn multi_backend_tool_routing_falls_back_to_legacy_prefixed_names() {
        let config_json = serde_json::json!({
            "backends": {
                "compliance-reference": {
                    "name": "compliance_reference",
                    "url": "http://upstream:9000/mcp",
                    "passthrough_headers": [],
                    "allowed_tool_names": ["get_stats"],
                    "allowed_resource_names": [],
                    "allowed_prompt_names": [],
                    "disable_tool_names_filtering": true,
                    "disable_prompt_names_filtering": true,
                    "disable_resource_names_filtering": true,
                },
                "other": {
                    "name": "other",
                    "url": "http://other:9000/mcp",
                    "passthrough_headers": [],
                    "allowed_tool_names": [],
                    "allowed_resource_names": [],
                    "allowed_prompt_names": [],
                    "disable_tool_names_filtering": true,
                    "disable_prompt_names_filtering": true,
                    "disable_resource_names_filtering": true,
                }
            }
        });
        let virtual_host: VirtualHost = serde_json::from_value(config_json).expect("valid virtual host");
        let backend_names = vec!["compliance-reference", "other"];

        assert_eq!(
            "compliance-reference-get_stats",
            exposed_tool_name(&virtual_host, "compliance-reference", "get_stats")
        );
        assert_eq!(
            Some(("compliance-reference", "get_stats")),
            resolve_tool_route(&virtual_host, "compliance-reference-get_stats", &backend_names)
        );
    }
}
