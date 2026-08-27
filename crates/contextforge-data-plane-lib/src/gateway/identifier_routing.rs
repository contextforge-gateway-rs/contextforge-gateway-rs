use contextforge_data_plane_apis::user_store::{BackendMCPGateway, NameAlias, VirtualHost};
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

fn resolve_route<'a, N: AsRef<str>>(
    virtual_host: &'a VirtualHost,
    name: &'a str,
    backend_names: &'a [N],
    name_extractor: impl Fn(&'a str, &'a BackendMCPGateway) -> Option<&'a str>,
) -> Result<Option<(&'a str, &'a str)>, Box<dyn std::error::Error + Send + Sync>> {
    let mut aliases = backend_names.iter().filter_map(|backend_name| {
        let backend_name = backend_name.as_ref();
        let backend = virtual_host.backends.get(backend_name)?;
        let upstream_name = name_extractor(name, backend)?;
        Some((backend_name, upstream_name))
    });
    let alias = aliases.next();
    if aliases.next().is_some() {
        return Err(format!("Multiple backends found for {name}").into());
    }
    Ok(alias.or_else(|| route_identifier(name, backend_names)))
}

/// Resolves an exact control-plane alias to its backend and upstream name. Without an alias,
/// single-backend hosts preserve the upstream name and multi-backend hosts use the legacy prefix.
pub(super) fn resolve_tool_route<'a, N: AsRef<str>>(
    virtual_host: &'a VirtualHost,
    name: &'a str,
    backend_names: &'a [N],
) -> Result<Option<(&'a str, &'a str)>, Box<dyn std::error::Error + Send + Sync>> {
    resolve_route(virtual_host, name, backend_names, |name: &'a str, backend: &'a BackendMCPGateway| {
        backend
            .tool_name_aliases
            .get(&NameAlias::with_downstream_prefixed_name(name.to_owned()))
            .map(NameAlias::get_upstream_name)
    })
}

pub(super) fn resolve_resources_route<'a, N: AsRef<str>>(
    virtual_host: &'a VirtualHost,
    name: &'a str,
    backend_names: &'a [N],
) -> Result<Option<(&'a str, &'a str)>, Box<dyn std::error::Error + Send + Sync>> {
    resolve_route(virtual_host, name, backend_names, |name: &'a str, backend: &'a BackendMCPGateway| {
        backend
            .resource_uri_aliases
            .get(&NameAlias::with_downstream_prefixed_name(name.to_owned()))
            .map(NameAlias::get_upstream_name)
    })
}

pub(super) fn resolve_prompt_route<'a, N: AsRef<str>>(
    virtual_host: &'a VirtualHost,
    name: &'a str,
    backend_names: &'a [N],
) -> Result<Option<(&'a str, &'a str)>, Box<dyn std::error::Error + Send + Sync>> {
    resolve_route(virtual_host, name, backend_names, |name: &'a str, backend: &'a BackendMCPGateway| {
        backend
            .prompt_name_aliases
            .get(&NameAlias::with_downstream_prefixed_name(name.to_owned()))
            .map(NameAlias::get_upstream_name)
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

    /// Joins a backend name and a backend-local name into the namespaced `{backend}-{rest}` form.
    fn prefixed_name(backend_name: &str, rest: &str) -> String {
        format!("{backend_name}-{rest}")
    }

    /// Returns the control-plane alias for an upstream tool when configured. Without an alias,
    /// single-backend hosts preserve the upstream name and multi-backend hosts use the legacy prefix.
    fn exposed_tool_name(virtual_host: &VirtualHost, backend_name: &str, original_name: &str) -> String {
        virtual_host
            .backends
            .get(backend_name)
            .and_then(|backend| {
                backend
                    .tool_name_aliases
                    .iter()
                    .find_map(|alias| (alias.get_upstream_name() == original_name).then(|| alias.clone()))
            })
            .map_or_else(
                || {
                    if virtual_host.backends.len() == 1 {
                        original_name.to_owned()
                    } else {
                        prefixed_name(backend_name, original_name)
                    }
                },
                |a| a.get_downstream_prefixed_name().to_owned(),
            )
    }

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
                    "mcp_protocol_version": "2026_07_28",
                    "passthrough_headers": [],
                    "tool_name_aliases": [
                        {"downstream_prefixed_name":"Public.Tool", "upstream_name":"get_stats"},
                        {"downstream_prefixed_name":"Echo_Tool", "upstream_name":"echo"}
                    ]
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
            resolve_tool_route(&virtual_host, "Public.Tool", &backend_ids).expect("this should work")
        );
    }

    #[test]
    fn multi_backend_tool_routing_falls_back_to_legacy_prefixed_names() {
        let config_json = serde_json::json!({
            "backends": {
                "compliance-reference": {
                    "name": "compliance_reference",
                    "url": "http://upstream:9000/mcp",
                    "mcp_protocol_version": "2026_07_28",
                    "passthrough_headers": [],
                },
                "other": {
                    "name": "other",
                    "url": "http://other:9000/mcp",
                    "mcp_protocol_version": "2026_07_28",
                    "passthrough_headers": [],
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
                .expect("this should work")
        );
    }
}
