use std::{collections::HashMap, sync::Arc};

use contextforge_data_plane_apis::user_store::BackendMCPGateway;
use http::request::Parts;
use rmcp::{
    ErrorData, RoleClient, RoleServer, ServiceExt,
    model::{ErrorCode, Implementation, InitializeRequestParams, InitializeResult, ServerCapabilities},
    service::{RequestContext, RunningService},
    transport::{StreamableHttpClientTransport, streamable_http_client::StreamableHttpClientTransportConfig},
};
use tracing::{info, warn};

use super::McpService;
use crate::gateway::{
    backend_client::GatewayBackendClient,
    backend_transports::{BackendTransportKey, BackendTransportService},
    mcp_call_validator::InitializeCallValidator,
    session_store::{UserSession, UserSessionStore},
};

pub(super) async fn initialize<T>(
    mcp_service: &McpService<T>,
    request: InitializeRequestParams,
    cx: RequestContext<RoleServer>,
) -> Result<InitializeResult, ErrorData>
where
    T: UserSessionStore + Send + Sync + 'static,
{
    let call_validator = InitializeCallValidator::new(&cx);
    let (virtual_host, downstream_session_id, claims) = call_validator.validate()?;
    let session_mapping = if let Ok(maybe_session_mapping) = mcp_service
        .user_session_store
        .get_session(&UserSession::new(claims.sub.clone(), Arc::from(downstream_session_id.value().as_str())))
        .await
    {
        maybe_session_mapping.unwrap_or_default()
    } else {
        return Err(ErrorData {
            code: ErrorCode::INTERNAL_ERROR,
            message: "Internal problem... session store can't be accessed".into(),
            data: None,
        });
    };

    let namespace_identifiers = virtual_host.backends.len() > 1;
    // SESSION-SCOPED: downstream headers are snapshotted here and baked into
    // the StreamableHttpClientTransportConfig for the lifetime of the backend
    // transport. Post-initialize calls (tools, resources, prompts) reuse these
    // headers. True per-request propagation requires either per-request transport
    // reconstruction or an SDK-level per-call header injection API on RunningService.
    // When the stateless path (MCP 2026-07-28, SEP-2575/SEP-2567) is implemented,
    // transports will be per-request and headers will naturally be request-scoped.
    let downstream_headers = cx.extensions.get::<Parts>().map(|parts| parts.headers.clone());
    let tasks: Vec<_> = virtual_host
        .backends
        .iter()
        .map(|(name, backend)| {
            let client = mcp_service.http_client.clone();
            let backend_client = GatewayBackendClient::new(
                name.clone(),
                namespace_identifiers,
                request.clone(),
                mcp_service.plugin_runtime.clone(),
            );
            let backend_url = backend.url.clone();
            let backend_cfg = backend.clone();
            let downstream_headers = downstream_headers.clone();
            let downstream_session_id = downstream_session_id.clone();

            Box::pin(async move {
                let mut headers = HashMap::new();
                if let Some(host) = backend_url.host_str()
                    && backend_url.scheme() == "https"
                {
                    let host = if let Some(port) = backend_url.port() {
                        format!("{host}:{port}")
                    } else {
                        host.to_owned()
                    };

                    if let Ok(value) = http::HeaderValue::from_str(&host) {
                        headers.insert(http::header::HOST, value);
                    } else {
                        warn!("Really can't set the host header for {:?}", backend_url.host_str());
                    }
                }

                apply_header_config(&mut headers, &backend_cfg, downstream_headers.as_ref());

                // Propagate the active W3C trace context to the backend so the
                // gateway span links to the downstream MCP server's spans.
                crate::telemetry::inject_current_context(&mut headers);

                let config =
                    StreamableHttpClientTransportConfig::with_uri(backend_url.to_string()).custom_headers(headers);
                let transport = StreamableHttpClientTransport::with_client(client, config);
                let maybe_running_service = backend_client.serve(transport).await;
                if let Ok(running_service) = maybe_running_service {
                    info!("initialize: intialized for {downstream_session_id:?} {name:?}");
                    (name, Some(running_service))
                } else {
                    warn!(
                        "initialize: Unable to initialize for {downstream_session_id:?} {name:?} {maybe_running_service:?}",
                    );
                    (name, None)
                }
            })
        })
        .collect();

    let initialization_results: Vec<(&String, Option<RunningService<RoleClient, GatewayBackendClient>>)> =
        futures::future::join_all(tasks).await;

    let (capabilities, backend_services): (Vec<_>, Vec<_>) = initialization_results
        .into_iter()
        .map(|(name, running_service): (_, _)| {
            info!(
                "initialize: Adding transport: session_id {downstream_session_id:#?} backend {name} {running_service:?}"
            );

            let server_capabilities = running_service
                .as_ref()
                .and_then(|rs| rs.peer().peer_info().as_ref().map(|pi| pi.capabilities.clone()));
            (
                (name.clone(), server_capabilities.clone()),
                (name.clone(), BackendTransportService::from((server_capabilities, running_service.map(Arc::new)))),
            )
        })
        .unzip();

    if mcp_service
        .user_session_store
        .set_session(
            &UserSession::new(claims.sub.clone(), Arc::from(downstream_session_id.value().as_str())),
            &session_mapping,
        )
        .await
        .is_err()
    {
        return Err(ErrorData {
            code: ErrorCode::INTERNAL_ERROR,
            message: "Internal problem... session store can't be written".into(),
            data: None,
        });
    }

    let mut transports = mcp_service.transports.inner().lock().await;
    for (name, service) in backend_services {
        transports
            .entry(BackendTransportKey::from((
                name.as_str(),
                downstream_session_id.value().as_str(),
                claims.sub.as_str(),
            )))
            .insert_entry(service);
    }
    drop(transports);

    Ok(InitializeResult::new(merge_and_build_capabilities(capabilities))
        .with_server_info(Implementation::new("rust-conformance-server", "0.1.0"))
        .with_instructions("Rust MCP conformance test server"))
}

fn merge_and_build_capabilities(server_capabilities: Vec<(String, Option<ServerCapabilities>)>) -> ServerCapabilities {
    let mut merged = ServerCapabilities::default();

    for (_, capabilities) in server_capabilities {
        let Some(capabilities) = capabilities else {
            continue;
        };

        if capabilities.completions.is_some() {
            merged.completions.get_or_insert_default();
        }

        if capabilities.prompts.is_some() {
            merged.prompts.get_or_insert_default();
        }

        if let Some(resources) = capabilities.resources {
            let merged_resources = merged.resources.get_or_insert_default();
            if resources.subscribe == Some(true) {
                merged_resources.subscribe = Some(true);
            }
        }

        if capabilities.tools.is_some() {
            merged.tools.get_or_insert_default();
        }
    }

    merged
}

/// Apply a backend's header config to the upstream header map.
///
/// Order: passthrough (copy named headers from the downstream request) -> add
/// (inject/override static headers) -> remove (strip named headers).
///
/// Protected headers are silently skipped in every phase:
/// - Gateway-managed: `Host` (set from backend URL before this runs)
/// - Body-framing: `Content-Length`, `Content-Type` (gateway owns framing)
/// - Hop-by-hop (RFC 7230 §6.1): `Connection`, `Keep-Alive`, `Proxy-Authenticate`,
///   `Proxy-Authorization`, `TE`, `Trailer`, `Trailers`, `Transfer-Encoding`, `Upgrade`
/// - Non-standard hop-by-hop: `Proxy-Connection`
/// - RMCP transport-reserved: `Mcp-Session-Id`, `Accept`, `Last-Event-Id`
///
/// ponytail: single-value per name; a repeated downstream header keeps its first value.
fn apply_header_config(
    headers: &mut HashMap<http::HeaderName, http::HeaderValue>,
    backend: &BackendMCPGateway,
    downstream: Option<&http::HeaderMap>,
) {
    if let Some(downstream) = downstream {
        for name in &backend.passthrough_headers {
            let Ok(name) = http::HeaderName::from_bytes(name.as_bytes()) else { continue };
            if is_protected_header(&name) {
                continue;
            }
            if let Some(value) = downstream.get(&name) {
                headers.insert(name, value.clone());
            }
        }
    }
    for (name, value) in &backend.add_headers {
        let (Ok(name), Ok(value)) = (http::HeaderName::from_bytes(name.as_bytes()), http::HeaderValue::from_str(value))
        else {
            continue;
        };
        if is_protected_header(&name) {
            continue;
        }
        headers.insert(name, value);
    }
    for name in &backend.remove_headers {
        let Ok(name) = http::HeaderName::from_bytes(name.as_bytes()) else { continue };
        if is_protected_header(&name) {
            continue;
        }
        headers.remove(&name);
    }
}

/// Returns `true` for headers that config must never touch:
/// - Gateway-managed: `Host`
/// - Body-framing: `Content-Length`, `Content-Type` (gateway owns framing; forwarding corrupts body or enables encoding-dispatch bypass)
/// - Hop-by-hop (RFC 7230 §6.1): `Connection`, `Keep-Alive`, `Proxy-Authenticate`, `Proxy-Authorization`, `TE`, `Trailer`, `Trailers`, `Transfer-Encoding`, `Upgrade`
/// - Non-standard hop-by-hop: `Proxy-Connection` (must not cross gateway boundary)
/// - RMCP transport-reserved: `Mcp-Session-Id`, `Accept`, `Last-Event-Id`
fn is_protected_header(name: &http::HeaderName) -> bool {
    const PROTECTED: &[&str] = &[
        "host",
        // body-framing: gateway owns these; forwarding corrupts framing or enables encoding-dispatch bypass
        "content-length",
        "content-type",
        // hop-by-hop (RFC 7230 §6.1)
        "connection",
        "keep-alive",
        "proxy-authenticate",
        "proxy-authorization",
        "te",
        "trailer",
        "trailers",
        "transfer-encoding",
        "upgrade",
        // non-standard hop-by-hop; must not cross gateway boundary
        "proxy-connection",
        // RMCP transport-reserved
        "mcp-session-id",
        "accept",
        "last-event-id",
    ];
    PROTECTED.iter().any(|&p| name.as_str().eq_ignore_ascii_case(p))
}

#[cfg(test)]
mod tests {
    use contextforge_data_plane_apis::user_store::Transport;

    use super::*;

    #[test]
    fn merge_and_build_capabilities_only_advertises_upstream_capabilities() {
        let capabilities = merge_and_build_capabilities(vec![
            ("first".to_owned(), Some(ServerCapabilities::builder().enable_tools().build())),
            ("second".to_owned(), Some(ServerCapabilities::builder().enable_resources().enable_completions().build())),
            ("third".to_owned(), Some(ServerCapabilities::builder().enable_tools().build())),
        ]);

        assert!(capabilities.tools.is_some());
        assert!(capabilities.completions.is_some());
        assert!(capabilities.resources.is_some());
        assert!(capabilities.prompts.is_none());
    }

    fn backend(passthrough: &[&str], add: &[(&str, &str)], remove: &[&str]) -> BackendMCPGateway {
        BackendMCPGateway {
            name: "b".into(),
            url: "https://upstream.example/mcp".parse().unwrap(),
            transport: Transport::default(),
            passthrough_headers: passthrough.iter().map(|s| (*s).to_owned()).collect(),
            add_headers: add.iter().map(|(k, v)| ((*k).to_owned(), (*v).to_owned())).collect(),
            remove_headers: remove.iter().map(|s| (*s).to_owned()).collect(),
            allowed_tool_names: vec![],
            tool_name_aliases: HashMap::new(),
            allowed_resource_names: vec![],
            allowed_prompt_names: vec![],
        }
    }

    fn downstream(pairs: &[(&str, &str)]) -> http::HeaderMap {
        let mut map = http::HeaderMap::new();
        for (k, v) in pairs {
            map.insert(http::HeaderName::from_bytes(k.as_bytes()).unwrap(), http::HeaderValue::from_str(v).unwrap());
        }
        map
    }

    #[test]
    fn pass_add_remove_and_host_protected() {
        let mut headers = HashMap::new();
        // Gateway sets HOST before config runs; config must never touch it.
        headers.insert(http::header::HOST, http::HeaderValue::from_static("upstream.example"));

        let ds = downstream(&[("Authorization", "Bearer downstream"), ("X-Drop", "1"), ("Host", "downstream.example")]);
        let cfg = backend(
            &["authorization", "host", "x-drop"],
            &[("X-Add", "added"), ("Authorization", "Bearer override")],
            &["x-drop"],
        );

        apply_header_config(&mut headers, &cfg, Some(&ds));

        // add overrides passthrough
        assert_eq!(headers[&http::header::AUTHORIZATION], "Bearer override");
        // static add present
        assert_eq!(headers[&http::HeaderName::from_static("x-add")], "added");
        // remove wins last
        assert!(!headers.contains_key(&http::HeaderName::from_static("x-drop")));
        // gateway HOST untouched by passthrough of downstream Host
        assert_eq!(headers[&http::header::HOST], "upstream.example");
    }

    #[test]
    fn no_downstream_headers_still_applies_add_remove() {
        let mut headers = HashMap::new();
        let cfg = backend(&["authorization"], &[("X-Add", "added")], &[]);
        apply_header_config(&mut headers, &cfg, None);
        assert_eq!(headers[&http::HeaderName::from_static("x-add")], "added");
        assert!(!headers.contains_key(&http::header::AUTHORIZATION));
    }

    #[test]
    fn hop_by_hop_headers_cannot_be_passed_through() {
        let mut headers = HashMap::new();
        let ds = downstream(&[
            ("Connection", "keep-alive"),
            ("Transfer-Encoding", "chunked"),
            ("Upgrade", "websocket"),
            ("Keep-Alive", "timeout=5"),
            ("TE", "trailers"),
            ("Trailers", "X-Foo"),
            ("Proxy-Authorization", "Basic abc"),
            ("Proxy-Authenticate", "Basic realm=x"),
            ("X-Custom", "value"),
        ]);
        let cfg = backend(
            &[
                "connection",
                "transfer-encoding",
                "upgrade",
                "keep-alive",
                "te",
                "trailers",
                "proxy-authorization",
                "proxy-authenticate",
                "x-custom",
            ],
            &[],
            &[],
        );
        apply_header_config(&mut headers, &cfg, Some(&ds));
        // Only the non-hop-by-hop header gets through
        assert_eq!(headers[&http::HeaderName::from_static("x-custom")], "value");
        for blocked in &[
            "connection",
            "transfer-encoding",
            "upgrade",
            "keep-alive",
            "te",
            "trailers",
            "proxy-authorization",
            "proxy-authenticate",
        ] {
            assert!(!headers.contains_key(&http::HeaderName::from_static(blocked)), "{blocked} must be blocked");
        }
    }

    #[test]
    fn rmcp_reserved_headers_cannot_be_passed_through_or_added() {
        let mut headers = HashMap::new();
        let ds = downstream(&[("Mcp-Session-Id", "sess123"), ("Accept", "text/html"), ("Last-Event-Id", "42")]);
        let cfg = backend(
            &["mcp-session-id", "accept", "last-event-id"],
            &[("Mcp-Session-Id", "injected"), ("Accept", "text/html")],
            &[],
        );
        apply_header_config(&mut headers, &cfg, Some(&ds));
        assert!(headers.is_empty(), "no RMCP-reserved header must reach the upstream config");
    }

    #[test]
    fn body_framing_and_connection_management_headers_cannot_be_forwarded() {
        let mut headers = HashMap::new();
        let ds = downstream(&[
            ("Content-Type", "text/plain"),
            ("Content-Length", "42"),
            ("Proxy-Connection", "keep-alive"),
            ("Trailer", "X-Checksum"),
            ("X-Custom", "ok"),
        ]);
        let cfg = backend(
            &["content-type", "content-length", "proxy-connection", "trailer", "x-custom"],
            &[("Content-Length", "99")],
            &[],
        );
        apply_header_config(&mut headers, &cfg, Some(&ds));
        // legitimate header passes through
        assert_eq!(headers[&http::HeaderName::from_static("x-custom")], "ok");
        // blocked in both passthrough and add phases
        for blocked in &["content-type", "content-length", "proxy-connection", "trailer"] {
            assert!(!headers.contains_key(&http::HeaderName::from_static(blocked)), "{blocked} must be blocked");
        }
    }
}
