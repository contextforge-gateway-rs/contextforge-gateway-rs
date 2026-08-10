use axum::{body::Body, extract::State, middleware::Next, response::Response};
use http::{StatusCode, header, uri::Authority};
use tracing::{debug, warn};
use url::{Origin, Url};

use crate::common::Config;

/// Parses a serialized RFC 6454 origin (`scheme://host[:port]`) into a typed [`url::Origin`].
/// Returns `None` for `"null"` and any value that is not a bare scheme+authority.
fn parse_origin(raw: &str) -> Option<Origin> {
    if raw.bytes().any(|b| b < 0x20 || b == 0x7F) {
        return None;
    }
    if raw != raw.trim() {
        return None;
    }
    if raw.eq_ignore_ascii_case("null") {
        return None;
    }
    if raw.contains('\\') || raw.contains('@') || raw.contains('?') || raw.contains('#') {
        return None;
    }
    let (_, authority_part) = raw.split_once("://")?;
    if authority_part.is_empty() || authority_part.contains('/') {
        return None;
    }
    // Trailing ":" with no port (e.g. "https://host:") is accepted by url but not a valid origin.
    let host_for_port_check = authority_part.trim_start_matches('[');
    if let Some((_, port_part)) = host_for_port_check.rsplit_once(':')
        && port_part.is_empty()
    {
        return None;
    }

    let url = Url::parse(&format!("{raw}/")).ok()?;

    if url.path() != "/" || !url.username().is_empty() || url.password().is_some() {
        return None;
    }
    if url.query().is_some() || url.fragment().is_some() {
        return None;
    }
    url.host()?;

    match url.origin() {
        Origin::Tuple(_, _, _) => Some(url.origin()),
        Origin::Opaque(_) => None,
    }
}

pub(crate) fn parse_origin_str(raw: &str) -> Option<Origin> {
    parse_origin(raw)
}

fn request_authority(request: &http::Request<Body>) -> Option<Authority> {
    request
        .headers()
        .get(header::HOST)
        .and_then(|v| v.to_str().ok())
        .and_then(|s| s.parse::<Authority>().ok())
        .or_else(|| request.uri().authority().cloned())
}

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

fn forbidden_response() -> Response {
    Response::builder()
        .status(StatusCode::FORBIDDEN)
        .header(header::CONTENT_TYPE, "text/plain")
        .body(Body::from("Forbidden: Origin header is not allowed"))
        .expect("response should build")
}

/// MCP 2026-07-28 DNS-rebinding protection middleware.
pub async fn mcp_origin_layer(State(config): State<Config>, request: http::Request<Body>, next: Next) -> Response {
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

    let Some(origin_header) = request.headers().get(header::ORIGIN) else {
        debug!("mcp_origin_layer - no Origin header, allowing request");
        return next.run(request).await;
    };

    let Ok(origin_str) = origin_header.to_str() else {
        warn!("mcp_origin_layer - rejected non-UTF-8 Origin header");
        return forbidden_response();
    };

    if origin_str.trim().eq_ignore_ascii_case("null") {
        warn!("mcp_origin_layer - rejected opaque null Origin");
        return forbidden_response();
    }

    let Some(request_origin) = parse_origin(origin_str) else {
        warn!("mcp_origin_layer - rejected malformed Origin header origin = {origin_str}");
        return forbidden_response();
    };

    if config.mcp_allowed_origins.is_empty() {
        warn!("mcp_origin_layer - rejected Origin: no allowed origins configured origin = {origin_str}");
        return forbidden_response();
    }

    if config.mcp_allowed_origins.contains(&request_origin) {
        debug!("mcp_origin_layer - Origin accepted via allowlist origin = {origin_str}");
        next.run(request).await
    } else {
        warn!("mcp_origin_layer - rejected Origin not in allowlist origin = {origin_str}");
        forbidden_response()
    }
}

#[cfg(test)]
mod tests {
    use axum::{Router, body::to_bytes, middleware, routing::get};
    use http::{Request, StatusCode};
    use tower::ServiceExt;
    use url::Origin;

    use super::*;

    // ── helpers ───────────────────────────────────────────────────────────────

    fn config_origins(origins: &[&str]) -> Config {
        Config {
            mcp_allowed_origins: origins.iter().map(|s| parse_origin_str(s).unwrap()).collect(),
            ..Config::default()
        }
    }

    fn config_hosts(hosts: &[&str]) -> Config {
        Config { mcp_allowed_hosts: hosts.iter().map(|s| (*s).to_owned()).collect(), ..Config::default() }
    }

    fn config_origins_and_hosts(origins: &[&str], hosts: &[&str]) -> Config {
        Config {
            mcp_allowed_origins: origins.iter().map(|s| parse_origin_str(s).unwrap()).collect(),
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

    // ── parse_origin: new strict syntax regressions ───────────────────────────

    #[test]
    fn extra_slashes_after_scheme_returns_none() {
        // "https:///…" and "https:////…" — url crate collapses these to a valid
        // host but they are not valid serialized origins.
        assert!(parse_origin("https:///app.example.com").is_none());
        assert!(parse_origin("https:////app.example.com").is_none());
    }

    #[test]
    fn trailing_colon_without_port_returns_none() {
        // "https://app.example.com:" — url crate accepts this as no-port.
        assert!(parse_origin("https://app.example.com:").is_none());
    }

    #[test]
    fn leading_whitespace_returns_none() {
        // url crate silently trims leading/trailing whitespace.
        assert!(parse_origin("  https://app.example.com").is_none());
        assert!(parse_origin("https://app.example.com  ").is_none());
    }

    #[test]
    fn dot_segment_path_returns_none() {
        // "https://app.example.com/." — url crate collapses "/." to "/" so the
        // post-parse path check cannot catch this; the pre-parse "/" check must.
        assert!(parse_origin("https://app.example.com/.").is_none());
    }

    #[test]
    fn embedded_tab_returns_none() {
        // url crate silently strips embedded horizontal tab; pre-parse control-
        // character check must reject it before the parser runs.
        assert!(parse_origin("https://app.\texample.com").is_none());
    }

    // ── parse_origin_str unit tests ───────────────────────────────────────────

    #[test]
    fn parse_origin_str_accepts_valid_origin() {
        assert!(parse_origin_str("https://app.example.com").is_some());
    }

    #[test]
    fn parse_origin_str_rejects_invalid_origin() {
        assert!(parse_origin_str(r"https:\bad").is_none());
    }

    #[test]
    fn parse_origin_str_rejects_path_component() {
        assert!(parse_origin_str("https:////bad2.example.com").is_none());
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

    #[tokio::test]
    async fn normalized_malformed_origins_return_403() {
        // Both values are normalized by the url crate into the same typed Origin
        // as https://app.example.com, so they must be rejected by pre-parse
        // checks before Url::parse is called.
        let app = make_app(config_origins(&["https://app.example.com"]));

        for origin in ["https://app.example.com/.", "https://app.\texample.com"] {
            let req = Request::builder()
                .uri("/mcp")
                .method("POST")
                .header(header::ORIGIN, origin)
                .body(Body::empty())
                .unwrap();

            let res = app.clone().oneshot(req).await.unwrap();
            assert_eq!(res.status(), StatusCode::FORBIDDEN, "{origin}");
        }
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
