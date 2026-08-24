use std::collections::HashMap;

use contextforge_data_plane_apis::user_store::BackendMCPGateway;
use http::request::Parts;
use rmcp::{
    ClientLifecycleMode, ErrorData, RoleClient, RoleServer,
    model::{
        ClientCapabilities, ErrorCode, Implementation, InitializeRequestParams, InitializeResult, ProtocolVersion,
        ServerCapabilities,
    },
    service::serve_client_with_lifecycle_and_ct,
    service::{RequestContext, RunningService},
    transport::{StreamableHttpClientTransport, streamable_http_client::StreamableHttpClientTransportConfig},
};
use tracing::warn;

use super::McpService;
use crate::gateway::backend_client::GatewayBackendClient;
use crate::mcp_standard_headers;

#[allow(clippy::unused_async)]
pub(super) async fn initialize(
    _svc: &McpService,
    params: InitializeRequestParams,
    _ctx: RequestContext<RoleServer>,
) -> Result<InitializeResult, ErrorData> {
    if params.protocol_version == ProtocolVersion::V_2026_07_28 {
        Err(ErrorData::invalid_request("Initialize not supported by this dataplane", None))
    } else {
        Ok(InitializeResult::new(ServerCapabilities::default())
            .with_server_info(Implementation::new("contextforge-dataplane-gateway", "0.1.0"))
            .with_instructions("contextforge-dataplane-gateway"))
    }
}

pub(super) async fn connect_backend_for_request(
    mcp_service: &McpService,
    backend_name: &str,
    backend: &BackendMCPGateway,
    namespace_identifiers: bool,
    cx: &RequestContext<RoleServer>,
) -> Result<RunningService<RoleClient, GatewayBackendClient>, ErrorData> {
    let mut headers = HashMap::new();
    let downstream_headers = cx.extensions.get::<Parts>().map(|parts| &parts.headers);

    if let Some(host) = backend.url.host_str()
        && backend.url.scheme() == "https"
    {
        let authority = if let Some(port) = backend.url.port() { format!("{host}:{port}") } else { host.to_owned() };
        if let Ok(value) = http::HeaderValue::from_str(&authority) {
            headers.insert(http::header::HOST, value);
        } else {
            warn!("connect_backend_for_request - invalid backend host backend_name = {backend_name}");
        }
    }

    apply_header_config(&mut headers, backend, downstream_headers);
    crate::telemetry::inject_current_context(&mut headers);

    let config = StreamableHttpClientTransportConfig::with_uri(backend.url.to_string()).custom_headers(headers);
    let transport = StreamableHttpClientTransport::with_client(mcp_service.http_client.clone(), config);
    let client_info = InitializeRequestParams::new(
        ClientCapabilities::default(),
        Implementation::new("contextforge-data-plane", env!("CARGO_PKG_VERSION")),
    )
    .with_protocol_version(ProtocolVersion::V_2026_07_28);

    let backend_client = GatewayBackendClient::new(
        backend_name.to_owned(),
        namespace_identifiers,
        client_info,
        mcp_service.plugin_runtime.clone(),
    );

    serve_client_with_lifecycle_and_ct(
        backend_client,
        transport,
        ClientLifecycleMode::Discover { preferred_versions: vec![ProtocolVersion::V_2026_07_28] },
        cx.ct.clone(),
    )
    .await
    .map_err(|error| {
        warn!(
            "connect_backend_for_request - backend connection failed backend_name = {backend_name} error = {error:?}"
        );
        ErrorData {
            code: ErrorCode::INTERNAL_ERROR,
            message: "Routing problem... backend unavailable".into(),
            data: None,
        }
    })
}

/// Apply a backend's header config to the upstream header map.
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
/// - MCP standard computed headers: `Mcp-Method`, `Mcp-Name`, `Mcp-Protocol-Version`, `Mcp-Param-*`
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
    PROTECTED.iter().any(|&p| name.as_str().eq_ignore_ascii_case(p)) || mcp_standard_headers::is_computed(name)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn backend(passthrough: &[&str], add: &[(&str, &str)], remove: &[&str]) -> BackendMCPGateway {
        BackendMCPGateway {
            name: "b".into(),
            url: "https://upstream.example/mcp".parse().unwrap(),
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
    fn computed_mcp_headers_cannot_be_passed_through_added_or_removed() {
        let mut headers = HashMap::new();
        headers.insert(http::HeaderName::from_static("mcp-method"), http::HeaderValue::from_static("tools/call"));
        headers.insert(http::HeaderName::from_static("mcp-param-user"), http::HeaderValue::from_static("computed"));
        let ds = downstream(&[
            ("Mcp-Method", "wrong/method"),
            ("Mcp-Name", "wrong-tool"),
            ("Mcp-Protocol-Version", "2020-01-01"),
            ("Mcp-Param-User", "wrong-user"),
        ]);
        let cfg = backend(
            &["mcp-method", "mcp-name", "mcp-protocol-version", "mcp-param-user"],
            &[
                ("Mcp-Method", "added/method"),
                ("Mcp-Name", "added-tool"),
                ("Mcp-Protocol-Version", "2020-01-01"),
                ("Mcp-Param-User", "added-user"),
            ],
            &["mcp-method", "mcp-param-user"],
        );

        apply_header_config(&mut headers, &cfg, Some(&ds));

        assert_eq!(headers[&http::HeaderName::from_static("mcp-method")], "tools/call");
        assert_eq!(headers[&http::HeaderName::from_static("mcp-param-user")], "computed");
        assert!(!headers.contains_key(&http::HeaderName::from_static("mcp-name")));
        assert!(!headers.contains_key(&http::HeaderName::from_static("mcp-protocol-version")));
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
