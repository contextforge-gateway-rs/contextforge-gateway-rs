use axum::{body::Body, extract::State, middleware::Next, response::Response};
use http::{StatusCode, header, uri::Authority};
use tracing::{debug, warn};
use url::{Origin, Url};

use crate::common::Config;

// ── Origin parsing ────────────────────────────────────────────────────────────

/// Strictly parses a serialized RFC 6454 origin string into a typed
/// [`url::Origin`].
///
/// A valid serialized origin is exactly `scheme "://" host [":" port]` with
/// **no** userinfo, path, query, or fragment component.  The `url` crate
/// silently repairs many malformed inputs (backslashes, userinfo stripping,
/// etc.), so this function validates the raw string before handing it to the
/// parser:
///
/// - Contains `\` → rejected (backslash normalization attack).
/// - Contains `@` before the first `/` → rejected (userinfo present).
/// - Contains `?` or `#` → rejected (query / fragment present).
///
/// After parsing, additional structural checks are applied:
///
/// - Parsed URL has non-empty username or a password → rejected.
/// - Parsed URL has a path other than `"/"` (from the slash we appended) →
///   rejected (path component present).
/// - Parsed URL has a query or fragment → rejected.
/// - Parsed URL has no host → rejected (e.g. `data:`, `blob:`).
/// - `url::Origin` is opaque → rejected.
///
/// Returns `None` for the literal `"null"` opaque origin (RFC 6454 §6.2) and
/// for any value that fails the checks above.
///
/// Port normalization is handled by the `url` crate: `https://blah.com:443`
/// and `https://blah.com` produce the same `Origin::Tuple`; `https://blah.com:8443`
/// is distinct.
fn parse_origin(raw: &str) -> Option<Origin> {
    if raw.trim().eq_ignore_ascii_case("null") {
        return None;
    }

    // ── Pre-parse structural checks on the raw string ─────────────────────
    // Backslash — the url crate treats it as a slash (WHATWG URL §5.1).
    if raw.contains('\\') {
        return None;
    }
    // Userinfo — "@" before the first "/" after the scheme separator.
    // A valid origin has no path, so any "@" means userinfo.
    if raw.contains('@') {
        return None;
    }
    // Query / fragment.
    if raw.contains('?') || raw.contains('#') {
        return None;
    }

    // Append "/" so the url crate accepts a bare `scheme://host[:port]` string.
    let url = Url::parse(&format!("{raw}/")).ok()?;

    // ── Post-parse structural checks ──────────────────────────────────────
    // Path must be exactly the "/" we appended.
    if url.path() != "/" {
        return None;
    }
    // Re-check userinfo fields (defense-in-depth, url crate may strip "@").
    if !url.username().is_empty() || url.password().is_some() {
        return None;
    }
    // No query or fragment.
    if url.query().is_some() || url.fragment().is_some() {
        return None;
    }
    // Must have a host.
    url.host()?;

    // Reject opaque origins (data:, blob:, …).
    match url.origin() {
        Origin::Tuple(_, _, _) => Some(url.origin()),
        Origin::Opaque(_) => None,
    }
}

// ── Host allowlist ────────────────────────────────────────────────────────────

/// Parses the `Host` header (or HTTP/2 `:authority` pseudo-header) into an
/// [`Authority`].
fn request_authority(request: &http::Request<Body>) -> Option<Authority> {
    request
        .headers()
        .get(header::HOST)
        .and_then(|v| v.to_str().ok())
        .and_then(|s| s.parse::<Authority>().ok())
        .or_else(|| request.uri().authority().cloned())
}

/// Returns `true` when `authority` matches at least one entry in
/// `allowed_hosts`.
///
/// Entries are plain hostnames (`gateway.example.com`) or `host:port`
/// authorities (`gateway.example.com:8080`) — no scheme prefix.
///
/// - Entry **without** a port → matches that host on **any** port.
/// - Entry **with** a port → matches only that exact `(host, port)` pair.
///
/// Comparison is case-insensitive on the host component.
fn authority_in_allowlist(authority: &Authority, allowed_hosts: &[String]) -> bool {
    let request_host = authority.host().to_ascii_lowercase();
    let request_port = authority.port_u16();

    allowed_hosts.iter().any(|entry| {
        let (entry_host, entry_port) = match entry.rsplit_once(':') {
            Some((h, p)) => match p.parse::<u16>() {
                Ok(port) => (h.to_ascii_lowercase(), Some(port)),
                Err(_) => (entry.to_ascii_lowercase(), None),
            },
            None => (entry.to_ascii_lowercase(), None),
        };
        entry_host == request_host && entry_port.is_none_or(|p| Some(p) == request_port)
    })
}

// ── Allowed-origins cache ─────────────────────────────────────────────────────

/// Parses the operator-configured origin strings once and returns the valid
/// [`Origin`] values.  Invalid entries are logged and skipped so a single
/// misconfigured entry does not silently disable all protection.
pub fn parse_allowed_origins(raw: &[String]) -> Vec<Origin> {
    raw.iter()
        .filter_map(|s| {
            let origin = parse_origin(s);
            if origin.is_none() {
                warn!("mcp_origin_layer - configured origin is invalid and will be ignored origin = {s}");
            }
            origin
        })
        .collect()
}

// ── Response helpers ──────────────────────────────────────────────────────────

fn forbidden_response() -> Response {
    Response::builder()
        .status(StatusCode::FORBIDDEN)
        .header(header::CONTENT_TYPE, "text/plain")
        .body(Body::from("Forbidden: Origin header is not allowed"))
        .expect("response should build")
}

// ── Middleware ────────────────────────────────────────────────────────────────

/// Axum middleware that enforces the MCP 2026-07-28 Streamable HTTP
/// DNS-rebinding protection requirement.
///
/// Per <https://modelcontextprotocol.io/specification/2026-07-28/basic/transports/streamable-http>:
///
/// > Servers MUST validate the Origin header on all incoming connections to
/// > prevent DNS rebinding attacks. If the Origin header is present and
/// > invalid, servers MUST respond with HTTP 403 Forbidden.
///
/// ## Decision table
///
/// | Condition | Result |
/// |---|---|
/// | `mcp_allowed_hosts` set, `Host` not in list | ❌ 403 |
/// | `Origin` absent | ✅ accept (native / non-browser clients) |
/// | `Origin: null` | ❌ 403 |
/// | `Origin` malformed, has backslash / userinfo / path / query / fragment | ❌ 403 |
/// | `mcp_allowed_origins` non-empty, parsed `Origin` in list | ✅ accept |
/// | `mcp_allowed_origins` non-empty, parsed `Origin` not in list | ❌ 403 |
/// | `mcp_allowed_origins` **empty** (default) | ❌ 403 — no fallback |
///
/// **There is no same-origin fallback.** A present `Origin` always requires
/// an explicit trusted allowlist (`CONTEXTFORGE_GATEWAY_RS_MCP_ALLOWED_ORIGINS`).
/// An empty allowlist is not a bypass; it rejects every `Origin` that is present.
///
/// Port comparison uses [`url::Origin`] typed equality, which normalizes
/// default ports: `https://app.example.com:443` and `https://app.example.com`
/// are the same origin; `https://app.example.com:8443` is different.
///
/// This layer fires before JWT claims validation, session creation, and any
/// backend fan-out.
pub async fn mcp_origin_layer(State(config): State<Config>, request: http::Request<Body>, next: Next) -> Response {
    // ── 1. Host allowlist check ───────────────────────────────────────────────
    if !config.mcp_allowed_hosts.is_empty() {
        match request_authority(&request) {
            None => {
                warn!("mcp_origin_layer - rejected request: Host header missing or unparseable");
                return forbidden_response();
            },
            Some(ref authority) if !authority_in_allowlist(authority, &config.mcp_allowed_hosts) => {
                warn!("mcp_origin_layer - rejected request: Host not in allowlist host = {authority}");
                return forbidden_response();
            },
            Some(_) => debug!("mcp_origin_layer - Host is in allowlist"),
        }
    }

    // ── 2. Origin header check ────────────────────────────────────────────────
    let Some(origin_header) = request.headers().get(header::ORIGIN) else {
        // No Origin header → native / non-browser client; always allow.
        debug!("mcp_origin_layer - no Origin header, allowing request");
        return next.run(request).await;
    };

    let Ok(origin_str) = origin_header.to_str() else {
        warn!("mcp_origin_layer - rejected non-UTF-8 Origin header");
        return forbidden_response();
    };

    // Opaque / sandbox origin — always rejected regardless of config.
    if origin_str.trim().eq_ignore_ascii_case("null") {
        warn!("mcp_origin_layer - rejected opaque null Origin");
        return forbidden_response();
    }

    let Some(request_origin) = parse_origin(origin_str) else {
        warn!("mcp_origin_layer - rejected malformed Origin header origin = {origin_str}");
        return forbidden_response();
    };

    // ── 3. Allowlist check ────────────────────────────────────────────────────
    // An empty allowlist is not a bypass: any present Origin is rejected until
    // the operator explicitly configures trusted origins.
    if config.mcp_parsed_origins.is_empty() {
        warn!("mcp_origin_layer - rejected Origin: no allowed origins configured origin = {origin_str}");
        return forbidden_response();
    }

    if config.mcp_parsed_origins.contains(&request_origin) {
        debug!("mcp_origin_layer - Origin accepted via allowlist origin = {origin_str}");
        next.run(request).await
    } else {
        warn!("mcp_origin_layer - rejected Origin not in allowlist origin = {origin_str}");
        forbidden_response()
    }
}

// ─────────────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use axum::{Router, body::to_bytes, middleware, routing::get};
    use http::{Request, StatusCode};
    use tower::ServiceExt;
    use url::Origin;

    use super::*;

    // ── helpers ───────────────────────────────────────────────────────────────

    fn config_origins(origins: &[&str]) -> Config {
        let mut c =
            Config { mcp_allowed_origins: origins.iter().map(|s| (*s).to_owned()).collect(), ..Config::default() };
        c.finalize();
        c
    }

    fn config_hosts(hosts: &[&str]) -> Config {
        Config { mcp_allowed_hosts: hosts.iter().map(|s| (*s).to_owned()).collect(), ..Config::default() }
    }

    fn config_origins_and_hosts(origins: &[&str], hosts: &[&str]) -> Config {
        let mut c = Config {
            mcp_allowed_origins: origins.iter().map(|s| (*s).to_owned()).collect(),
            mcp_allowed_hosts: hosts.iter().map(|s| (*s).to_owned()).collect(),
            ..Config::default()
        };
        c.finalize();
        c
    }

    fn make_app(config: Config) -> axum::Router {
        Router::new()
            .route("/mcp", get(handler).post(handler).delete(handler))
            .layer(middleware::from_fn_with_state(config.clone(), mcp_origin_layer))
            .with_state(config)
    }

    async fn handler() -> StatusCode {
        StatusCode::NO_CONTENT
    }

    // ── parse_origin unit tests ───────────────────────────────────────────────

    #[test]
    fn null_origin_returns_none() {
        assert!(parse_origin("null").is_none());
        assert!(parse_origin("NULL").is_none());
        assert!(parse_origin("Null").is_none());
    }

    #[test]
    fn empty_origin_returns_none() {
        assert!(parse_origin("").is_none());
    }

    #[test]
    fn origin_without_scheme_returns_none() {
        assert!(parse_origin("app.example.com").is_none());
    }

    #[test]
    fn origin_with_path_returns_none() {
        assert!(parse_origin("https://app.example.com/some/path").is_none());
    }

    #[test]
    fn origin_with_trailing_slash_returns_none() {
        assert!(parse_origin("https://app.example.com/").is_none());
    }

    #[test]
    fn origin_with_query_returns_none() {
        assert!(parse_origin("https://app.example.com?q=1").is_none());
    }

    #[test]
    fn origin_with_fragment_returns_none() {
        assert!(parse_origin("https://app.example.com#frag").is_none());
    }

    #[test]
    fn origin_with_userinfo_returns_none() {
        // "@" in the raw string is caught before parsing.
        assert!(parse_origin("https://user@app.example.com").is_none());
    }

    #[test]
    fn origin_with_backslash_returns_none() {
        // url crate silently normalizes backslash to "/"; pre-parse check blocks it.
        assert!(parse_origin(r"https:\app.example.com").is_none());
        assert!(parse_origin(r"https:\\app.example.com").is_none());
    }

    #[test]
    fn origin_with_data_scheme_returns_none() {
        // data: produces an opaque origin.
        assert!(parse_origin("data:text/plain,foo").is_none());
    }

    #[test]
    fn https_default_port_443_equals_portless() {
        let portless = parse_origin("https://app.example.com").unwrap();
        let explicit = parse_origin("https://app.example.com:443").unwrap();
        assert_eq!(portless, explicit, "https://blah.com:443 must equal https://blah.com");
    }

    #[test]
    fn http_default_port_80_equals_portless() {
        let portless = parse_origin("http://app.example.com").unwrap();
        let explicit = parse_origin("http://app.example.com:80").unwrap();
        assert_eq!(portless, explicit);
    }

    #[test]
    fn non_default_port_8443_is_distinct_from_portless() {
        let portless = parse_origin("https://app.example.com").unwrap();
        let non_default = parse_origin("https://app.example.com:8443").unwrap();
        assert_ne!(portless, non_default, "https://blah.com:8443 must NOT equal https://blah.com");
    }

    #[test]
    fn parse_origin_is_case_insensitive_on_scheme_and_host() {
        let lower = parse_origin("https://app.example.com").unwrap();
        let upper = parse_origin("HTTPS://APP.EXAMPLE.COM").unwrap();
        assert_eq!(lower, upper);
    }

    #[test]
    fn ipv6_origin_parsed_correctly() {
        // IPv6 address produces a valid Tuple origin.
        let o = parse_origin("http://[::1]:8080").unwrap();
        assert!(matches!(o, Origin::Tuple(_, _, 8080)));
    }

    // ── parse_allowed_origins unit tests ─────────────────────────────────────

    #[test]
    fn invalid_configured_origin_is_skipped() {
        let parsed = parse_allowed_origins(&["https://valid.example.com".to_owned(), r"https:\bad".to_owned()]);
        assert_eq!(parsed.len(), 1);
        assert_eq!(parsed[0], parse_origin("https://valid.example.com").unwrap());
    }

    #[test]
    fn empty_configured_origins_produces_empty_list() {
        assert!(parse_allowed_origins(&[]).is_empty());
    }

    // ── authority_in_allowlist unit tests ─────────────────────────────────────

    #[test]
    fn authority_exact_host_match() {
        let auth = "gateway.example.com".parse::<Authority>().unwrap();
        assert!(authority_in_allowlist(&auth, &["gateway.example.com".to_owned()]));
    }

    #[test]
    fn authority_entry_without_port_matches_any_port() {
        let auth = "gateway.example.com:8080".parse::<Authority>().unwrap();
        assert!(authority_in_allowlist(&auth, &["gateway.example.com".to_owned()]));
    }

    #[test]
    fn authority_entry_with_port_matches_only_that_port() {
        let auth8080 = "gateway.example.com:8080".parse::<Authority>().unwrap();
        let auth443 = "gateway.example.com:443".parse::<Authority>().unwrap();
        assert!(authority_in_allowlist(&auth8080, &["gateway.example.com:8080".to_owned()]));
        assert!(!authority_in_allowlist(&auth443, &["gateway.example.com:8080".to_owned()]));
    }

    #[test]
    fn authority_mismatch_returns_false() {
        let auth = "evil.example.com".parse::<Authority>().unwrap();
        assert!(!authority_in_allowlist(&auth, &["gateway.example.com".to_owned()]));
    }

    // ── middleware: no Origin ─────────────────────────────────────────────────

    #[tokio::test]
    async fn no_origin_accepted_with_empty_config() {
        let app = make_app(Config::default());
        let req = Request::builder().uri("/mcp").method("GET").body(Body::empty()).unwrap();
        let res = app.oneshot(req).await.unwrap();
        assert_eq!(res.status(), StatusCode::NO_CONTENT);
    }

    #[tokio::test]
    async fn no_origin_accepted_with_allowlist_configured() {
        let app = make_app(config_origins(&["https://app.example.com"]));
        let req = Request::builder().uri("/mcp").method("GET").body(Body::empty()).unwrap();
        let res = app.oneshot(req).await.unwrap();
        assert_eq!(res.status(), StatusCode::NO_CONTENT);
    }

    // ── middleware: empty allowlist rejects any present Origin ────────────────

    #[tokio::test]
    async fn present_origin_with_empty_allowlist_returns_403() {
        // Empty allowlist is not a bypass; any present Origin must be rejected.
        let app = make_app(Config::default());
        let req = Request::builder()
            .uri("/mcp")
            .method("POST")
            .header(header::ORIGIN, "https://app.example.com")
            .body(Body::empty())
            .unwrap();
        let res = app.oneshot(req).await.unwrap();
        assert_eq!(res.status(), StatusCode::FORBIDDEN);
    }

    #[tokio::test]
    async fn attacker_controlled_host_and_origin_match_but_still_rejected_without_allowlist() {
        // DNS-rebinding: attacker controls both Host and Origin to the same value.
        // Without an explicit allowlist this must be rejected, not accepted.
        let app = make_app(Config::default());
        let req = Request::builder()
            .uri("http://attacker.invalid/mcp")
            .method("POST")
            .header(header::HOST, "attacker.invalid")
            .header(header::ORIGIN, "http://attacker.invalid")
            .body(Body::empty())
            .unwrap();
        let res = app.oneshot(req).await.unwrap();
        assert_eq!(res.status(), StatusCode::FORBIDDEN);
    }

    // ── middleware: allowlist (non-empty) ─────────────────────────────────────

    #[tokio::test]
    async fn allowlisted_origin_accepted() {
        let app = make_app(config_origins(&["https://app.example.com"]));
        let req = Request::builder()
            .uri("/mcp")
            .method("POST")
            .header(header::ORIGIN, "https://app.example.com")
            .body(Body::empty())
            .unwrap();
        let res = app.oneshot(req).await.unwrap();
        assert_eq!(res.status(), StatusCode::NO_CONTENT);
    }

    #[tokio::test]
    async fn non_allowlisted_origin_returns_403() {
        let app = make_app(config_origins(&["https://app.example.com"]));
        let req = Request::builder()
            .uri("/mcp")
            .method("POST")
            .header(header::ORIGIN, "https://attacker.invalid")
            .body(Body::empty())
            .unwrap();
        let res = app.oneshot(req).await.unwrap();
        assert_eq!(res.status(), StatusCode::FORBIDDEN);
    }

    #[tokio::test]
    async fn allowlisted_origin_with_explicit_default_port_accepted() {
        // Browser sends :443 explicitly; allowlist has no port — same origin.
        let app = make_app(config_origins(&["https://app.example.com"]));
        let req = Request::builder()
            .uri("/mcp")
            .method("POST")
            .header(header::ORIGIN, "https://app.example.com:443")
            .body(Body::empty())
            .unwrap();
        let res = app.oneshot(req).await.unwrap();
        assert_eq!(res.status(), StatusCode::NO_CONTENT);
    }

    #[tokio::test]
    async fn allowlist_entry_with_443_accepts_portless_origin() {
        // Allowlist has :443; browser sends no port — same origin.
        let app = make_app(config_origins(&["https://app.example.com:443"]));
        let req = Request::builder()
            .uri("/mcp")
            .method("POST")
            .header(header::ORIGIN, "https://app.example.com")
            .body(Body::empty())
            .unwrap();
        let res = app.oneshot(req).await.unwrap();
        assert_eq!(res.status(), StatusCode::NO_CONTENT);
    }

    #[tokio::test]
    async fn non_default_port_not_in_allowlist_returns_403() {
        // Allowlist entry normalizes to :443; :8443 is a different origin.
        let app = make_app(config_origins(&["https://app.example.com"]));
        let req = Request::builder()
            .uri("/mcp")
            .method("POST")
            .header(header::ORIGIN, "https://app.example.com:8443")
            .body(Body::empty())
            .unwrap();
        let res = app.oneshot(req).await.unwrap();
        assert_eq!(res.status(), StatusCode::FORBIDDEN);
    }

    #[tokio::test]
    async fn allowlist_with_8443_does_not_match_default_port() {
        // Allowlist entry is :8443; portless request is :443 — different origin.
        let app = make_app(config_origins(&["https://app.example.com:8443"]));
        let req = Request::builder()
            .uri("/mcp")
            .method("POST")
            .header(header::ORIGIN, "https://app.example.com")
            .body(Body::empty())
            .unwrap();
        let res = app.oneshot(req).await.unwrap();
        assert_eq!(res.status(), StatusCode::FORBIDDEN);
    }

    #[tokio::test]
    async fn multiple_origins_in_allowlist_all_accepted() {
        let app = make_app(config_origins(&["https://app.example.com", "http://localhost:3000"]));
        for origin in &["https://app.example.com", "http://localhost:3000"] {
            let req = Request::builder()
                .uri("/mcp")
                .method("POST")
                .header(header::ORIGIN, *origin)
                .body(Body::empty())
                .unwrap();
            let res = app.clone().oneshot(req).await.unwrap();
            assert_eq!(res.status(), StatusCode::NO_CONTENT, "expected accept for {origin}");
        }
        let req = Request::builder()
            .uri("/mcp")
            .method("POST")
            .header(header::ORIGIN, "https://other.invalid")
            .body(Body::empty())
            .unwrap();
        let res = app.oneshot(req).await.unwrap();
        assert_eq!(res.status(), StatusCode::FORBIDDEN);
    }

    // ── middleware: HTTPS origin-form requests ────────────────────────────────

    #[tokio::test]
    async fn https_origin_accepted_when_allowlisted_origin_form_request() {
        // A normal HTTP/1.1 request has URI `/mcp` (origin-form, no scheme).
        // The scheme cannot be inferred from the request URI; only the Origin
        // header value matters for the allowlist comparison.
        let app = make_app(config_origins(&["https://app.example.com"]));
        let req = Request::builder()
            // origin-form URI — no scheme
            .uri("/mcp")
            .method("POST")
            .header(header::HOST, "app.example.com")
            .header(header::ORIGIN, "https://app.example.com")
            .body(Body::empty())
            .unwrap();
        let res = app.oneshot(req).await.unwrap();
        assert_eq!(res.status(), StatusCode::NO_CONTENT);
    }

    #[tokio::test]
    async fn http_origin_rejected_when_only_https_allowlisted_origin_form_request() {
        // Origin: http://... must not match an allowlist entry for https://...
        // even when the request URI has no scheme and Host matches.
        let app = make_app(config_origins(&["https://app.example.com"]));
        let req = Request::builder()
            .uri("/mcp")
            .method("POST")
            .header(header::HOST, "app.example.com")
            .header(header::ORIGIN, "http://app.example.com")
            .body(Body::empty())
            .unwrap();
        let res = app.oneshot(req).await.unwrap();
        assert_eq!(res.status(), StatusCode::FORBIDDEN);
    }

    // ── middleware: malformed-but-normalizable Origins ────────────────────────

    #[tokio::test]
    async fn backslash_origin_returns_403() {
        // url crate would normalize https:\app.example.com to https://app.example.com
        // but pre-parse check must reject it first.
        let app = make_app(config_origins(&["https://app.example.com"]));
        let req = Request::builder()
            .uri("/mcp")
            .method("POST")
            .header(header::ORIGIN, r"https:\app.example.com")
            .body(Body::empty())
            .unwrap();
        let res = app.oneshot(req).await.unwrap();
        assert_eq!(res.status(), StatusCode::FORBIDDEN);
    }

    #[tokio::test]
    async fn userinfo_origin_returns_403() {
        // url crate strips userinfo from the origin; we must reject before that.
        let app = make_app(config_origins(&["https://app.example.com"]));
        let req = Request::builder()
            .uri("/mcp")
            .method("POST")
            .header(header::ORIGIN, "https://user@app.example.com")
            .body(Body::empty())
            .unwrap();
        let res = app.oneshot(req).await.unwrap();
        assert_eq!(res.status(), StatusCode::FORBIDDEN);
    }

    #[tokio::test]
    async fn origin_with_query_returns_403() {
        let app = make_app(config_origins(&["https://app.example.com"]));
        let req = Request::builder()
            .uri("/mcp")
            .method("POST")
            .header(header::ORIGIN, "https://app.example.com?q=1")
            .body(Body::empty())
            .unwrap();
        let res = app.oneshot(req).await.unwrap();
        assert_eq!(res.status(), StatusCode::FORBIDDEN);
    }

    #[tokio::test]
    async fn origin_with_fragment_returns_403() {
        let app = make_app(config_origins(&["https://app.example.com"]));
        let req = Request::builder()
            .uri("/mcp")
            .method("POST")
            .header(header::ORIGIN, "https://app.example.com#frag")
            .body(Body::empty())
            .unwrap();
        let res = app.oneshot(req).await.unwrap();
        assert_eq!(res.status(), StatusCode::FORBIDDEN);
    }

    // ── middleware: null / malformed (always 403) ─────────────────────────────

    #[tokio::test]
    async fn null_origin_returns_403_with_allowlist() {
        let app = make_app(config_origins(&["https://app.example.com"]));
        let req =
            Request::builder().uri("/mcp").method("POST").header(header::ORIGIN, "null").body(Body::empty()).unwrap();
        let res = app.oneshot(req).await.unwrap();
        assert_eq!(res.status(), StatusCode::FORBIDDEN);
    }

    #[tokio::test]
    async fn null_origin_returns_403_without_allowlist() {
        let app = make_app(Config::default());
        let req =
            Request::builder().uri("/mcp").method("POST").header(header::ORIGIN, "null").body(Body::empty()).unwrap();
        let res = app.oneshot(req).await.unwrap();
        assert_eq!(res.status(), StatusCode::FORBIDDEN);
    }

    #[tokio::test]
    async fn malformed_origin_returns_403() {
        let app = make_app(Config::default());
        let req = Request::builder()
            .uri("/mcp")
            .method("POST")
            .header(header::ORIGIN, "not-an-origin")
            .body(Body::empty())
            .unwrap();
        let res = app.oneshot(req).await.unwrap();
        assert_eq!(res.status(), StatusCode::FORBIDDEN);
    }

    // ── middleware: DELETE method ─────────────────────────────────────────────

    #[tokio::test]
    async fn delete_allowlisted_origin_accepted() {
        let app = make_app(config_origins(&["https://app.example.com"]));
        let req = Request::builder()
            .uri("/mcp")
            .method("DELETE")
            .header(header::ORIGIN, "https://app.example.com")
            .body(Body::empty())
            .unwrap();
        let res = app.oneshot(req).await.unwrap();
        assert_eq!(res.status(), StatusCode::NO_CONTENT);
    }

    #[tokio::test]
    async fn delete_non_allowlisted_origin_returns_403() {
        let app = make_app(config_origins(&["https://app.example.com"]));
        let req = Request::builder()
            .uri("/mcp")
            .method("DELETE")
            .header(header::ORIGIN, "https://attacker.invalid")
            .body(Body::empty())
            .unwrap();
        let res = app.oneshot(req).await.unwrap();
        assert_eq!(res.status(), StatusCode::FORBIDDEN);
    }

    // ── middleware: Host allowlist ────────────────────────────────────────────

    #[tokio::test]
    async fn request_with_allowed_host_passes_host_check() {
        let app = make_app(config_hosts(&["gateway.example.com"]));
        let req = Request::builder()
            .uri("/mcp")
            .method("GET")
            .header(header::HOST, "gateway.example.com")
            .body(Body::empty())
            .unwrap();
        let res = app.oneshot(req).await.unwrap();
        assert_eq!(res.status(), StatusCode::NO_CONTENT);
    }

    #[tokio::test]
    async fn request_with_disallowed_host_returns_403() {
        let app = make_app(config_hosts(&["gateway.example.com"]));
        let req = Request::builder()
            .uri("/mcp")
            .method("POST")
            .header(header::HOST, "evil.example.com")
            .header(header::ORIGIN, "https://app.example.com")
            .body(Body::empty())
            .unwrap();
        let res = app.oneshot(req).await.unwrap();
        assert_eq!(res.status(), StatusCode::FORBIDDEN);
    }

    #[tokio::test]
    async fn host_and_origin_both_valid_accepted() {
        let app = make_app(config_origins_and_hosts(&["https://app.example.com"], &["gateway.example.com"]));
        let req = Request::builder()
            .uri("/mcp")
            .method("POST")
            .header(header::HOST, "gateway.example.com")
            .header(header::ORIGIN, "https://app.example.com")
            .body(Body::empty())
            .unwrap();
        let res = app.oneshot(req).await.unwrap();
        assert_eq!(res.status(), StatusCode::NO_CONTENT);
    }

    #[tokio::test]
    async fn valid_host_but_invalid_origin_returns_403() {
        let app = make_app(config_origins_and_hosts(&["https://app.example.com"], &["gateway.example.com"]));
        let req = Request::builder()
            .uri("/mcp")
            .method("POST")
            .header(header::HOST, "gateway.example.com")
            .header(header::ORIGIN, "https://attacker.invalid")
            .body(Body::empty())
            .unwrap();
        let res = app.oneshot(req).await.unwrap();
        assert_eq!(res.status(), StatusCode::FORBIDDEN);
    }

    // ── misc ──────────────────────────────────────────────────────────────────

    #[tokio::test]
    async fn forbidden_response_body_is_non_empty() {
        let app = make_app(config_origins(&["https://app.example.com"]));
        let req = Request::builder()
            .uri("/mcp")
            .method("POST")
            .header(header::ORIGIN, "https://attacker.invalid")
            .body(Body::empty())
            .unwrap();
        let res = app.oneshot(req).await.unwrap();
        assert_eq!(res.status(), StatusCode::FORBIDDEN);
        let body = to_bytes(res.into_body(), 256).await.unwrap();
        assert!(!body.is_empty());
    }
}
