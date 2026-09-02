use std::collections::{HashMap, HashSet};

use contextforge_data_plane_apis::runtime_plugin_config::{RuntimePluginName, RuntimeRevision};
use contextforge_data_plane_cpex::{
    HookHttpRequest, HookOperation, HookRequestMetadata, HookSubject, HookTarget, McpHookContext,
};
use http::header::{ACCEPT, CONTENT_TYPE, USER_AGENT};
use rmcp::ErrorData;
use uuid::Uuid;

use super::mcp_call_validator::AuthorizedCallContext;
use crate::telemetry;

const SAFE_PLUGIN_HEADERS: [http::HeaderName; 3] = [ACCEPT, CONTENT_TYPE, USER_AGENT];

pub(crate) fn require_plugin_binding<'a>(
    revision: Option<&'a RuntimeRevision>,
    plugins: Option<&'a [RuntimePluginName]>,
) -> Result<(&'a RuntimeRevision, &'a [RuntimePluginName]), ErrorData> {
    let plugins = plugins.ok_or_else(|| {
        tracing::warn!("rejecting call with missing runtime plugin target binding");
        ErrorData::internal_error("Runtime plugin binding is missing", None)
    })?;
    let revision = revision.ok_or_else(|| {
        tracing::warn!("rejecting call with missing runtime plugin binding revision");
        ErrorData::internal_error("Runtime plugin binding is invalid", None)
    })?;
    Ok((revision, plugins))
}

pub(crate) fn build_plugin_context(
    authorized: &AuthorizedCallContext<'_>,
    mcp_method: &str,
    downstream_target: &str,
    target: HookTarget,
) -> McpHookContext {
    let (trace_id, span_id) = telemetry::current_trace_ids();
    let teams = authorized.claims.teams.iter().flatten().cloned().collect::<HashSet<_>>();
    let permissions =
        authorized.claims.scopes.iter().flat_map(|scopes| scopes.permissions().iter().cloned()).collect::<HashSet<_>>();

    McpHookContext::new(
        HookRequestMetadata { request_id: Uuid::new_v4().to_string(), trace_id, span_id },
        HookSubject { id: authorized.claims.sub.clone(), teams, permissions },
        safe_http_request(authorized.parts),
        HookOperation {
            mcp_method: mcp_method.to_owned(),
            virtual_host: authorized.virtual_host_id.value().clone(),
            downstream_target: downstream_target.to_owned(),
            target,
        },
    )
}

fn safe_http_request(parts: &http::request::Parts) -> HookHttpRequest {
    HookHttpRequest {
        method: parts.method.as_str().to_owned(),
        path: parts.uri.path().to_owned(),
        // URI authority and scheme may be client-supplied absolute-form data. Omit them until
        // the HTTP layer publishes a normalized, validated route identity.
        authority: None,
        scheme: None,
        headers: safe_plugin_headers(&parts.headers),
    }
}

fn safe_plugin_headers(headers: &http::HeaderMap) -> HashMap<String, String> {
    SAFE_PLUGIN_HEADERS
        .iter()
        .filter_map(|name| {
            headers
                .get(name)
                .and_then(|value| value.to_str().ok())
                .map(|value| (name.as_str().to_owned(), value.to_owned()))
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use contextforge_data_plane_apis::user_store::PluginBindings;

    #[test]
    fn plugin_bindings_resolve_only_exact_canonical_backend_targets() {
        let revision = RuntimeRevision::try_from("revision-1".to_owned()).expect("valid revision");
        let plugin_name = RuntimePluginName::try_from("validator-a".to_owned()).expect("valid plugin name");
        let bindings = PluginBindings {
            revision: Some(revision),
            tools: HashMap::from([(
                "backend-a".to_owned(),
                HashMap::from([("canonical-tool".to_owned(), vec![plugin_name.clone()])]),
            )]),
            resources: HashMap::new(),
            prompts: HashMap::new(),
        };

        assert_eq!(Some([plugin_name].as_slice()), bindings.tool_plugins("backend-a", "canonical-tool"));
        assert!(bindings.tool_plugins("backend-a", "downstream-alias").is_none());
        assert!(bindings.tool_plugins("backend-b", "canonical-tool").is_none());
    }

    #[test]
    fn missing_or_unversioned_plugin_bindings_fail_closed() {
        let revision = RuntimeRevision::try_from("revision-1".to_owned()).expect("valid revision");
        assert!(require_plugin_binding(None, Some(&[])).is_err());
        assert!(require_plugin_binding(Some(&revision), None).is_err());
        assert!(require_plugin_binding(Some(&revision), Some(&[])).is_ok());
    }

    #[test]
    fn malformed_plugin_bindings_are_rejected_during_deserialization() {
        for bindings in [
            serde_json::json!({
                "revision": "revision-1",
                "tools": {},
                "resources": {},
                "prompts": {},
                "typo": true
            }),
            serde_json::json!({
                "tools": {"backend": {"sum": ["validator"]}}
            }),
            serde_json::json!({
                "revision": "revision-1",
                "tools": {"backend": {"sum": []}}
            }),
            serde_json::json!({
                "revision": "revision-1",
                "tools": {"backend": {"sum": ["validator", "validator"]}}
            }),
        ] {
            assert!(serde_json::from_value::<PluginBindings>(bindings).is_err());
        }
    }

    #[test]
    fn plugin_http_headers_use_an_explicit_nonsecret_allowlist() {
        let headers = http::HeaderMap::from_iter([
            (ACCEPT, http::HeaderValue::from_static("application/json")),
            (CONTENT_TYPE, http::HeaderValue::from_static("application/json")),
            (USER_AGENT, http::HeaderValue::from_static("test-client")),
            (http::header::AUTHORIZATION, http::HeaderValue::from_static("Bearer secret")),
            (http::header::COOKIE, http::HeaderValue::from_static("session=secret")),
            (http::header::CONNECTION, http::HeaderValue::from_static("keep-alive")),
            (http::HeaderName::from_static("x-api-key"), http::HeaderValue::from_static("secret")),
            (http::HeaderName::from_static("traceparent"), http::HeaderValue::from_static("spoofed")),
        ]);

        let filtered = safe_plugin_headers(&headers);

        assert_eq!(3, filtered.len());
        assert_eq!(Some("application/json"), filtered.get("accept").map(String::as_str));
        assert_eq!(Some("application/json"), filtered.get("content-type").map(String::as_str));
        assert_eq!(Some("test-client"), filtered.get("user-agent").map(String::as_str));
        assert!(!filtered.contains_key("authorization"));
        assert!(!filtered.contains_key("cookie"));
        assert!(!filtered.contains_key("x-api-key"));
        assert!(!filtered.contains_key("traceparent"));
    }

    #[test]
    fn plugin_http_route_omits_client_supplied_absolute_authority() {
        let (parts, ()) = http::Request::builder()
            .method("POST")
            .uri("https://attacker.invalid/servers/vh/mcp")
            .body(())
            .expect("request builds")
            .into_parts();

        let http = safe_http_request(&parts);

        assert_eq!("POST", http.method);
        assert_eq!("/servers/vh/mcp", http.path);
        assert!(http.authority.is_none());
        assert!(http.scheme.is_none());
    }
}
