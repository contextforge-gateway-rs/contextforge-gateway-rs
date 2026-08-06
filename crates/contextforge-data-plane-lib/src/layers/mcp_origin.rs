use axum::{body::Body, extract::State, middleware::Next, response::Response};
use http::{StatusCode, header};
use tracing::{debug, warn};
use url::Url;

use crate::common::Config;

/// Parses an Origin header value into a canonical [`Url`].
///
/// Returns `None` for the opaque `"null"` origin and for any string that
/// cannot be parsed as a valid `scheme://host[:port]` origin (no path allowed).
///
/// The `url` crate normalises scheme and host to lowercase and silently
/// strips default ports (`https` → 443, `http` → 80), so two `Url` values
/// compare equal if and only if they represent the same RFC 6454 origin:
///
/// - `https://blah.com` == `https://blah.com:443`   (`:443` is the https default)
/// - `https://blah.com` != `https://blah.com:8443`  (non-default port)
/// - `HTTPS://BLAH.COM` == `https://blah.com`        (case-folded by the crate)
fn origin_to_url(origin: &str) -> Option<Url> {
    if origin.trim().eq_ignore_ascii_case("null") {
        return None;
    }
    // Origin values are `scheme "://" host [":" port]` with no path.
    // Appending "/" makes the string a valid absolute URL that the parser accepts.
    let url = Url::parse(&format!("{origin}/")).ok()?;
    // Reject any path beyond the root "/" we appended.
    if url.path() != "/" {
        return None;
    }
    // Reject origins that have no host (data:, blob:, …).
    url.host()?;
    Some(url)
}

/// Parses the request `Host` / HTTP/2 `:authority` header into a canonical
/// [`Url`], using the scheme from the request URI (defaulting to `"http"`).
fn host_to_url(request: &http::Request<Body>) -> Option<Url> {
    let authority = request
        .headers()
        .get(header::HOST)
        .and_then(|v| v.to_str().ok())
        .map(str::to_owned)
        .or_else(|| request.uri().authority().map(ToString::to_string))?;
    let scheme = request.uri().scheme_str().unwrap_or("http");
    Url::parse(&format!("{scheme}://{authority}/")).ok()
}

/// Returns `true` when `request_origin` matches at least one entry in
/// `allowed_origins`.
///
/// Both sides are parsed through [`origin_to_url`] and compared with [`Url`]
/// equality, which handles default-port normalization and case-folding
/// automatically:
///
/// - Allowlist entry `https://app.example.com` matches both
///   `Origin: https://app.example.com` and `Origin: https://app.example.com:443`.
/// - Allowlist entry `https://app.example.com:8443` matches only
///   `Origin: https://app.example.com:8443`.
fn origin_in_allowlist(request_origin: &Url, allowed_origins: &[String]) -> bool {
    allowed_origins
        .iter()
        .filter_map(|raw| origin_to_url(raw))
        .any(|allowed| allowed == *request_origin)
}

/// Returns `true` when the request `Host` authority matches at least one entry
/// in `allowed_hosts`.
///
/// Entries are plain hostnames (`gateway.example.com`) or `host:port`
/// authorities (`gateway.example.com:8080`) — no scheme prefix.
///
/// - Entry **without** a port → matches that host on **any** port.
/// - Entry **with** a port → matches only that exact `(host, port)` pair.
///
/// Comparison is case-insensitive on the host component.
fn host_in_allowlist(host_url: &Url, allowed_hosts: &[String]) -> bool {
    let request_host = host_url.host_str().unwrap_or("").to_ascii_lowercase();
    let request_port = host_url.port_or_known_default();

    allowed_hosts.iter().any(|entry| {
        let (entry_host, entry_port) = match entry.rsplit_once(':') {
            Some((h, p)) => match p.parse::<u16>() {
                Ok(port) => (h.to_ascii_lowercase(), Some(port)),
                Err(_) => (entry.to_ascii_lowercase(), None),
            },
            None => (entry.to_ascii_lowercase(), None),
        };
        entry_host == request_host
            && entry_port.is_none_or(|p| Some(p) == request_port)
    })
}

fn forbidden_response() -> Response {
    Response::builder()
        .status(StatusCode::FORBIDDEN)
        .header(header::CONTENT_TYPE, "text/plain")
        .body(Body::from("Forbidden: Origin header is not allowed"))
        .expect("response should build")
}

/// Axum middleware that enforces the MCP 2026-07-28 Streamable HTTP
/// DNS-rebinding protection requirement.
///
/// Per <https://modelcontextprotocol.io/specification/2026-07-28/basic/transports/streamable-http>:
///
/// > Servers MUST validate the Origin header on all incoming connections to
/// > prevent DNS rebinding attacks. If the Origin header is present and
/// > invalid, servers MUST respond with HTTP 403 Forbidden.
///
/// ## Host check (`mcp_allowed_hosts`)
///
/// When `Config::mcp_allowed_hosts` is non-empty, every request whose `Host`
/// header does not match an entry is rejected with **HTTP 403** before Origin
/// validation.  When the list is empty, Host validation is disabled.
///
/// ## Origin check (`mcp_allowed_origins`)
///
/// | `mcp_allowed_origins` | `Origin` absent | `Origin` in list | `Origin` not in list | `null` / malformed |
/// |---|---|---|---|---|
/// | **non-empty** | ✅ accept | ✅ accept | ❌ 403 | ❌ 403 |
/// | **empty** (default) | ✅ accept | ✅ if same-origin (`Origin == Host`) | ❌ 403 | ❌ 403 |
///
/// Port comparison uses `url::Url` equality, which normalizes default ports:
/// `https://app.example.com:443` and `https://app.example.com` are the same
/// origin; `https://app.example.com:8443` is a different origin.
///
/// This layer fires before JWT claims validation, session creation, and any
/// backend fan-out.
pub async fn mcp_origin_layer(
    State(config): State<Config>,
    request: http::Request<Body>,
    next: Next,
) -> Response {
    // ── 1. Host allowlist check ────────────────────────────────────────────
    if !config.mcp_allowed_hosts.is_empty() {
        match host_to_url(&request) {
            None => {
                warn!("mcp_origin_layer - rejected request: Host header missing or unparseable");
                return forbidden_response();
            },
            Some(ref host_url) if !host_in_allowlist(host_url, &config.mcp_allowed_hosts) => {
                warn!(
                    "mcp_origin_layer - rejected request: Host not in allowlist host = {host_url}"
                );
                return forbidden_response();
            },
            Some(_) => debug!("mcp_origin_layer - Host is in allowlist"),
        }
    }

    // ── 2. Origin header check ─────────────────────────────────────────────
    let Some(origin_header) = request.headers().get(header::ORIGIN) else {
        // No Origin header → native / non-browser client; always allow.
        debug!("mcp_origin_layer - no Origin header, allowing request");
        return next.run(request).await;
    };

    let Ok(origin_str) = origin_header.to_str() else {
        warn!("mcp_origin_layer - rejected non-UTF-8 Origin header");
        return forbidden_response();
    };

    // Opaque / sandbox origin — never valid regardless of config.
    if origin_str.trim().eq_ignore_ascii_case("null") {
        warn!("mcp_origin_layer - rejected opaque null Origin");
        return forbidden_response();
    }

    let Some(request_origin) = origin_to_url(origin_str) else {
        warn!("mcp_origin_layer - rejected malformed Origin header origin = {origin_str}");
        return forbidden_response();
    };

    // ── 3. Accept / reject based on allowlist or same-origin fallback ──────
    if config.mcp_allowed_origins.is_empty() {
        // No allowlist configured: fall back to same-origin check (Origin == Host).
        let Some(host_url) = host_to_url(&request) else {
            warn!("mcp_origin_layer - rejected request: could not determine Host for same-origin check origin = {origin_str}");
            return forbidden_response();
        };
        if request_origin == host_url {
            debug!("mcp_origin_layer - same-origin request accepted origin = {origin_str}");
            next.run(request).await
        } else {
            warn!("mcp_origin_layer - rejected cross-origin request origin = {origin_str} host = {host_url}");
            forbidden_response()
        }
    } else {
        // Explicit allowlist configured: Origin must appear in it.
        if origin_in_allowlist(&request_origin, &config.mcp_allowed_origins) {
            debug!("mcp_origin_layer - Origin accepted via allowlist origin = {origin_str}");
            next.run(request).await
        } else {
            warn!("mcp_origin_layer - rejected Origin not in allowlist origin = {origin_str}");
            forbidden_response()
        }
    }
}

#[cfg(test)]
mod tests {
    use axum::{Router, body::to_bytes, middleware, routing::get};
    use http::{Request, StatusCode};
    use tower::ServiceExt;

    use super::*;

    // ── helpers ──────────────────────────────────────────────────────────────

    fn config_origins(origins: &[&str]) -> Config {
        Config {
            mcp_allowed_origins: origins.iter().map(|s| (*s).to_owned()).collect(),
            ..Config::default()
        }
    }

    fn config_hosts(hosts: &[&str]) -> Config {
        Config {
            mcp_allowed_hosts: hosts.iter().map(|s| (*s).to_owned()).collect(),
            ..Config::default()
        }
    }

    fn config_origins_and_hosts(origins: &[&str], hosts: &[&str]) -> Config {
        Config {
            mcp_allowed_origins: origins.iter().map(|s| (*s).to_owned()).collect(),
            mcp_allowed_hosts: hosts.iter().map(|s| (*s).to_owned()).collect(),
            ..Config::default()
        }
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

    // ── origin_to_url unit tests ─────────────────────────────────────────────

    #[test]
    fn null_origin_returns_none() {
        assert!(origin_to_url("null").is_none());
        assert!(origin_to_url("NULL").is_none());
        assert!(origin_to_url("Null").is_none());
    }

    #[test]
    fn empty_origin_returns_none() {
        assert!(origin_to_url("").is_none());
    }

    #[test]
    fn origin_without_scheme_returns_none() {
        assert!(origin_to_url("app.example.com").is_none());
    }

    #[test]
    fn origin_with_path_returns_none() {
        assert!(origin_to_url("https://app.example.com/some/path").is_none());
    }

    #[test]
    fn origin_with_trailing_slash_returns_none() {
        assert!(origin_to_url("https://app.example.com/").is_none());
    }

    #[test]
    fn https_default_port_443_equals_portless() {
        // The url crate silently drops the default port — both parse to the same Url.
        let portless = origin_to_url("https://app.example.com").unwrap();
        let explicit = origin_to_url("https://app.example.com:443").unwrap();
        assert_eq!(portless, explicit, "https://blah.com:443 must equal https://blah.com");
    }

    #[test]
    fn http_default_port_80_equals_portless() {
        let portless = origin_to_url("http://app.example.com").unwrap();
        let explicit = origin_to_url("http://app.example.com:80").unwrap();
        assert_eq!(portless, explicit);
    }

    #[test]
    fn non_default_port_8443_is_distinct_from_portless() {
        let portless = origin_to_url("https://app.example.com").unwrap();
        let non_default = origin_to_url("https://app.example.com:8443").unwrap();
        assert_ne!(portless, non_default, "https://blah.com:8443 must NOT equal https://blah.com");
    }

    #[test]
    fn url_equality_is_case_insensitive_on_scheme_and_host() {
        // The url crate normalises scheme and host to lowercase.
        let lower = origin_to_url("https://app.example.com").unwrap();
        let upper = origin_to_url("HTTPS://APP.EXAMPLE.COM").unwrap();
        assert_eq!(lower, upper);
    }

    #[test]
    fn ipv6_origin_parsed_correctly() {
        let o = origin_to_url("http://[::1]:8080").unwrap();
        assert_eq!(o.host_str(), Some("[::1]"));
    }

    // ── origin_in_allowlist unit tests ───────────────────────────────────────

    #[test]
    fn allowlist_exact_match() {
        let req = origin_to_url("https://app.example.com").unwrap();
        assert!(origin_in_allowlist(&req, &["https://app.example.com".to_owned()]));
    }

    #[test]
    fn allowlist_portless_entry_matches_explicit_default_port() {
        // Entry has no port (→ :443); request sends :443 explicitly — same origin.
        let req = origin_to_url("https://app.example.com:443").unwrap();
        assert!(origin_in_allowlist(&req, &["https://app.example.com".to_owned()]));
    }

    #[test]
    fn allowlist_entry_with_443_matches_portless_request() {
        // Entry is :443; browser sends no explicit port — same origin.
        let req = origin_to_url("https://app.example.com").unwrap();
        assert!(origin_in_allowlist(&req, &["https://app.example.com:443".to_owned()]));
    }

    #[test]
    fn allowlist_portless_entry_does_not_match_non_default_port() {
        // Entry normalizes to :443; :8443 is a different origin.
        let req = origin_to_url("https://app.example.com:8443").unwrap();
        assert!(!origin_in_allowlist(&req, &["https://app.example.com".to_owned()]));
    }

    #[test]
    fn allowlist_8443_entry_does_not_match_default_port() {
        // Entry is :8443; portless request normalizes to :443 — different origin.
        let req = origin_to_url("https://app.example.com").unwrap();
        assert!(!origin_in_allowlist(&req, &["https://app.example.com:8443".to_owned()]));
    }

    #[test]
    fn allowlist_scheme_mismatch_rejected() {
        let req = origin_to_url("http://app.example.com").unwrap();
        assert!(!origin_in_allowlist(&req, &["https://app.example.com".to_owned()]));
    }

    #[test]
    fn allowlist_multiple_entries() {
        let allowed = vec![
            "https://app.example.com".to_owned(),
            "http://localhost:3000".to_owned(),
        ];
        assert!(origin_in_allowlist(&origin_to_url("https://app.example.com").unwrap(), &allowed));
        assert!(origin_in_allowlist(&origin_to_url("http://localhost:3000").unwrap(), &allowed));
        assert!(!origin_in_allowlist(&origin_to_url("https://other.example.com").unwrap(), &allowed));
    }

    // ── middleware integration: no Origin ────────────────────────────────────

    #[tokio::test]
    async fn no_origin_is_always_accepted_with_empty_config() {
        let app = make_app(Config::default());
        let req = Request::builder().uri("/mcp").method("GET").body(Body::empty()).unwrap();
        let res = app.oneshot(req).await.unwrap();
        assert_eq!(res.status(), StatusCode::NO_CONTENT);
    }

    #[tokio::test]
    async fn no_origin_is_always_accepted_with_allowlist_configured() {
        let app = make_app(config_origins(&["https://app.example.com"]));
        let req = Request::builder().uri("/mcp").method("GET").body(Body::empty()).unwrap();
        let res = app.oneshot(req).await.unwrap();
        assert_eq!(res.status(), StatusCode::NO_CONTENT);
    }

    // ── middleware integration: Origin allowlist (non-empty) ─────────────────

    #[tokio::test]
    async fn allowlisted_cross_origin_is_accepted() {
        // Origin differs from Host but is in the allowlist.
        let app = make_app(config_origins(&["https://app.example.com"]));
        let req = Request::builder()
            .uri("https://gateway.example.com/mcp")
            .method("POST")
            .header(header::HOST, "gateway.example.com")
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
            .uri("https://gateway.example.com/mcp")
            .method("POST")
            .header(header::HOST, "gateway.example.com")
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
            .uri("https://gateway.example.com/mcp")
            .method("POST")
            .header(header::HOST, "gateway.example.com")
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

    // ── middleware integration: same-origin fallback (empty allowlist) ────────

    #[tokio::test]
    async fn same_origin_accepted_when_no_allowlist() {
        let app = make_app(Config::default());
        let req = Request::builder()
            .uri("http://localhost/mcp")
            .method("POST")
            .header(header::HOST, "localhost")
            .header(header::ORIGIN, "http://localhost")
            .body(Body::empty())
            .unwrap();
        let res = app.oneshot(req).await.unwrap();
        assert_eq!(res.status(), StatusCode::NO_CONTENT);
    }

    #[tokio::test]
    async fn cross_origin_rejected_when_no_allowlist() {
        let app = make_app(Config::default());
        let req = Request::builder()
            .uri("http://localhost/mcp")
            .method("POST")
            .header(header::HOST, "localhost")
            .header(header::ORIGIN, "https://attacker.invalid")
            .body(Body::empty())
            .unwrap();
        let res = app.oneshot(req).await.unwrap();
        assert_eq!(res.status(), StatusCode::FORBIDDEN);
    }

    #[tokio::test]
    async fn default_port_normalization_same_origin_fallback() {
        // Origin: https://app.example.com:443 ↔ Host: app.example.com — same origin.
        let app = make_app(Config::default());
        let req = Request::builder()
            .uri("https://app.example.com/mcp")
            .method("POST")
            .header(header::HOST, "app.example.com")
            .header(header::ORIGIN, "https://app.example.com:443")
            .body(Body::empty())
            .unwrap();
        let res = app.oneshot(req).await.unwrap();
        assert_eq!(res.status(), StatusCode::NO_CONTENT);
    }

    #[tokio::test]
    async fn non_default_port_mismatch_rejected_in_same_origin_fallback() {
        // Host: app.example.com (→ :443), Origin: :8443 — different origin.
        let app = make_app(Config::default());
        let req = Request::builder()
            .uri("https://app.example.com/mcp")
            .method("POST")
            .header(header::HOST, "app.example.com")
            .header(header::ORIGIN, "https://app.example.com:8443")
            .body(Body::empty())
            .unwrap();
        let res = app.oneshot(req).await.unwrap();
        assert_eq!(res.status(), StatusCode::FORBIDDEN);
    }

    // ── middleware integration: null / malformed (always 403) ────────────────

    #[tokio::test]
    async fn null_origin_returns_403_with_allowlist() {
        let app = make_app(config_origins(&["https://app.example.com"]));
        let req = Request::builder()
            .uri("/mcp").method("POST").header(header::ORIGIN, "null").body(Body::empty()).unwrap();
        let res = app.oneshot(req).await.unwrap();
        assert_eq!(res.status(), StatusCode::FORBIDDEN);
    }

    #[tokio::test]
    async fn null_origin_returns_403_without_allowlist() {
        let app = make_app(Config::default());
        let req = Request::builder()
            .uri("/mcp").method("POST").header(header::ORIGIN, "null").body(Body::empty()).unwrap();
        let res = app.oneshot(req).await.unwrap();
        assert_eq!(res.status(), StatusCode::FORBIDDEN);
    }

    #[tokio::test]
    async fn malformed_origin_returns_403() {
        let app = make_app(Config::default());
        let req = Request::builder()
            .uri("/mcp").method("POST").header(header::ORIGIN, "not-an-origin").body(Body::empty()).unwrap();
        let res = app.oneshot(req).await.unwrap();
        assert_eq!(res.status(), StatusCode::FORBIDDEN);
    }

    // ── middleware integration: DELETE method ─────────────────────────────────

    #[tokio::test]
    async fn delete_allowlisted_origin_accepted() {
        let app = make_app(config_origins(&["https://app.example.com"]));
        let req = Request::builder()
            .uri("https://gateway.example.com/mcp")
            .method("DELETE")
            .header(header::HOST, "gateway.example.com")
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
            .uri("/mcp").method("DELETE").header(header::ORIGIN, "https://attacker.invalid").body(Body::empty()).unwrap();
        let res = app.oneshot(req).await.unwrap();
        assert_eq!(res.status(), StatusCode::FORBIDDEN);
    }

    // ── middleware integration: Host allowlist ────────────────────────────────

    #[tokio::test]
    async fn request_with_allowed_host_passes_host_check() {
        let app = make_app(config_hosts(&["gateway.example.com"]));
        let req = Request::builder()
            .uri("https://gateway.example.com/mcp")
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
            .uri("https://evil.example.com/mcp")
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
        let app = make_app(config_origins_and_hosts(
            &["https://app.example.com"],
            &["gateway.example.com"],
        ));
        let req = Request::builder()
            .uri("https://gateway.example.com/mcp")
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
        let app = make_app(config_origins_and_hosts(
            &["https://app.example.com"],
            &["gateway.example.com"],
        ));
        let req = Request::builder()
            .uri("https://gateway.example.com/mcp")
            .method("POST")
            .header(header::HOST, "gateway.example.com")
            .header(header::ORIGIN, "https://attacker.invalid")
            .body(Body::empty())
            .unwrap();
        let res = app.oneshot(req).await.unwrap();
        assert_eq!(res.status(), StatusCode::FORBIDDEN);
    }

    #[tokio::test]
    async fn forbidden_response_body_is_non_empty() {
        let app = make_app(config_origins(&["https://app.example.com"]));
        let req = Request::builder()
            .uri("/mcp").method("POST").header(header::ORIGIN, "https://attacker.invalid").body(Body::empty()).unwrap();
        let res = app.oneshot(req).await.unwrap();
        assert_eq!(res.status(), StatusCode::FORBIDDEN);
        let body = to_bytes(res.into_body(), 256).await.unwrap();
        assert!(!body.is_empty());
    }
}
