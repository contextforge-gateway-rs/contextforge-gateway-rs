//! HTTP request correlation and W3C trace-context propagation.

use std::{collections::HashMap, fmt::Write as _, time::Duration};

use axum::{extract::Request, middleware::Next, response::Response};
use opentelemetry::global;
use opentelemetry::propagation::{Extractor, Injector};
use opentelemetry::trace::TraceContextExt;
use sha2::{Digest, Sha256};
use tower_http::trace::{MakeSpan, OnResponse};
use tracing::Span;
use tracing_opentelemetry::OpenTelemetrySpanExt;
use uuid::Uuid;

pub const TRANSACTION_ID_HEADER: http::HeaderName = http::HeaderName::from_static("x-contextforge-transaction-id");
pub const CORRELATION_ID_HEADER: http::HeaderName = http::HeaderName::from_static("x-contextforge-correlation-id");

tokio::task_local! {
    static CURRENT_CONTEXT: RequestContext;
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct TraceFields {
    trace_id: String,
    span_id: String,
    trace_flags: String,
}

impl TraceFields {
    fn from_headers(headers: &http::HeaderMap) -> Self {
        headers
            .get(http::header::HeaderName::from_static("traceparent"))
            .and_then(|value| value.to_str().ok())
            .and_then(parse_traceparent)
            .unwrap_or_else(Self::generated)
    }

    fn generated() -> Self {
        let trace_id = Uuid::new_v4().simple().to_string();
        let span_id = Uuid::new_v4().simple().to_string()[..16].to_owned();
        Self { trace_id, span_id, trace_flags: "01".to_owned() }
    }

    fn traceparent(&self) -> String {
        format!("00-{}-{}-{}", self.trace_id, self.span_id, self.trace_flags)
    }
}

fn parse_traceparent(value: &str) -> Option<TraceFields> {
    let mut parts = value.trim().split('-');
    let version = parts.next()?;
    let trace_id = parts.next()?;
    let span_id = parts.next()?;
    let trace_flags = parts.next()?;
    if parts.next().is_some()
        || version.len() != 2
        || version.eq_ignore_ascii_case("ff")
        || trace_id.len() != 32
        || span_id.len() != 16
        || trace_flags.len() != 2
        || ![version, trace_id, span_id, trace_flags]
            .iter()
            .all(|part| part.bytes().all(|byte| byte.is_ascii_hexdigit()))
        || trace_id.bytes().all(|byte| byte == b'0')
        || span_id.bytes().all(|byte| byte == b'0')
    {
        return None;
    }
    Some(TraceFields {
        trace_id: trace_id.to_ascii_lowercase(),
        span_id: span_id.to_ascii_lowercase(),
        trace_flags: trace_flags.to_ascii_lowercase(),
    })
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct RequestContext {
    transaction_id: String,
    correlation_id: String,
    trace: TraceFields,
}

impl RequestContext {
    fn from_headers(headers: &http::HeaderMap) -> Self {
        let incoming_correlation = uuid_header(headers, &CORRELATION_ID_HEADER);
        let transaction_id = non_empty_header(headers, &TRANSACTION_ID_HEADER)
            .or_else(|| incoming_correlation.clone())
            .unwrap_or_else(|| Uuid::new_v4().to_string());
        let correlation_id = incoming_correlation.unwrap_or_else(|| Uuid::new_v4().to_string());
        Self { transaction_id, correlation_id, trace: TraceFields::from_headers(headers) }
    }

    fn insert_response_headers(&self, headers: &mut http::HeaderMap) {
        if let Ok(value) = http::HeaderValue::from_str(&self.transaction_id) {
            headers.insert(TRANSACTION_ID_HEADER, value);
        }
        if let Ok(value) = http::HeaderValue::from_str(&self.correlation_id) {
            headers.insert(CORRELATION_ID_HEADER, value);
        }
    }

    fn insert_outbound_headers<S>(&self, headers: &mut HashMap<http::HeaderName, http::HeaderValue, S>)
    where
        S: std::hash::BuildHasher,
    {
        insert_header(headers, TRANSACTION_ID_HEADER, &self.transaction_id);
        insert_header(headers, CORRELATION_ID_HEADER, &self.correlation_id);
        insert_header(headers, http::header::HeaderName::from_static("traceparent"), &self.trace.traceparent());
    }
}

fn insert_header<S>(headers: &mut HashMap<http::HeaderName, http::HeaderValue, S>, name: http::HeaderName, value: &str)
where
    S: std::hash::BuildHasher,
{
    if let Ok(value) = http::HeaderValue::from_str(value) {
        headers.insert(name, value);
    }
}

fn non_empty_header(headers: &http::HeaderMap, name: &http::HeaderName) -> Option<String> {
    headers
        .get(name)
        .and_then(|value| value.to_str().ok())
        .map(str::trim)
        .filter(|value| !value.is_empty() && value.len() <= 128)
        .map(ToOwned::to_owned)
}

fn uuid_header(headers: &http::HeaderMap, name: &http::HeaderName) -> Option<String> {
    non_empty_header(headers, name).and_then(|value| Uuid::parse_str(&value).ok().map(|id| id.to_string()))
}

pub async fn correlation_layer(mut request: Request, next: Next) -> Response {
    let context = RequestContext::from_headers(request.headers());
    request.extensions_mut().insert(context.clone());
    let mut response = CURRENT_CONTEXT.scope(context.clone(), next.run(request)).await;
    context.insert_response_headers(response.headers_mut());
    response
}

struct HeaderExtractor<'a>(&'a http::HeaderMap);

impl Extractor for HeaderExtractor<'_> {
    fn get(&self, key: &str) -> Option<&str> {
        self.0.get(key).and_then(|value| value.to_str().ok())
    }

    fn keys(&self) -> Vec<&str> {
        self.0.keys().map(http::HeaderName::as_str).collect()
    }
}

struct HeaderInjector<'a, S>(&'a mut HashMap<http::HeaderName, http::HeaderValue, S>);

impl<S> Injector for HeaderInjector<'_, S>
where
    S: std::hash::BuildHasher,
{
    fn set(&mut self, key: &str, value: String) {
        if let (Ok(name), Ok(value)) =
            (http::HeaderName::from_bytes(key.as_bytes()), http::HeaderValue::from_str(&value))
        {
            self.0.insert(name, value);
        }
    }
}

#[derive(Clone, Copy, Debug, Default)]
pub struct ExtractingMakeSpan;

impl<B> MakeSpan<B> for ExtractingMakeSpan {
    fn make_span(&mut self, request: &http::Request<B>) -> Span {
        let request_context = request
            .extensions()
            .get::<RequestContext>()
            .cloned()
            .unwrap_or_else(|| RequestContext::from_headers(request.headers()));
        let span = tracing::info_span!(
            "http-request",
            transaction_id = %request_context.transaction_id,
            correlation_id = %request_context.correlation_id,
            trace_id = %request_context.trace.trace_id,
            span_id = %request_context.trace.span_id,
            user_id = tracing::field::Empty,
            component = "HttpServer",
            operation = "http_request",
            http_method = %request.method(),
            http_path = request.uri().path(),
            http_version = ?request.version(),
        );
        let parent =
            global::get_text_map_propagator(|propagator| propagator.extract(&HeaderExtractor(request.headers())));
        let _ = span.set_parent(parent);
        let context = span.context();
        let otel_span = context.span();
        let span_context = otel_span.span_context();
        if span_context.is_valid() {
            span.record("trace_id", span_context.trace_id().to_string());
            span.record("span_id", span_context.span_id().to_string());
        }
        span
    }
}

#[derive(Clone, Copy, Debug, Default)]
pub struct LogOnResponse;

impl<B> OnResponse<B> for LogOnResponse {
    fn on_response(self, response: &http::Response<B>, latency: Duration, _span: &Span) {
        let http_status = response.status().as_u16();
        let latency_ms = u64::try_from(latency.as_millis()).unwrap_or(u64::MAX);
        if response.status().is_server_error() {
            tracing::error!(
                component = "HttpServer",
                operation = "http_request",
                event_type = "PERFORMANCE",
                metric = "request_latency",
                outcome = "error",
                http_status,
                latency_ms,
                error_code = "CFDP-HTTP-SERVER",
                root_cause = "request returned a server error",
                impact_scope = "single-request",
                retryable = false,
                "HTTP request failed"
            );
        } else if response.status().is_client_error() {
            tracing::warn!(
                component = "HttpServer",
                operation = "http_request",
                event_type = "PERFORMANCE",
                metric = "request_latency",
                outcome = "client_error",
                http_status,
                latency_ms,
                "HTTP request rejected"
            );
        } else {
            tracing::info!(
                component = "HttpServer",
                operation = "http_request",
                event_type = "PERFORMANCE",
                metric = "request_latency",
                outcome = "success",
                http_status,
                latency_ms,
                "HTTP request completed"
            );
        }
    }
}

/// Adds request correlation and trace context to an outbound backend request.
pub fn inject_current_context<S>(headers: &mut HashMap<http::HeaderName, http::HeaderValue, S>)
where
    S: std::hash::BuildHasher,
{
    if let Ok(context) = CURRENT_CONTEXT.try_with(Clone::clone) {
        context.insert_outbound_headers(headers);
    }
    let context = Span::current().context();
    global::get_text_map_propagator(|propagator| propagator.inject_context(&context, &mut HeaderInjector(headers)));
}

/// Records a stable pseudonym for an authenticated subject on the current request span.
pub fn record_authenticated_user(subject: &str) {
    Span::current().record("user_id", pseudonymous_user_id(subject));
}

fn pseudonymous_user_id(subject: &str) -> String {
    let digest = Sha256::digest(subject.as_bytes());
    let mut pseudonym = String::with_capacity(19);
    pseudonym.push_str("sha256:");
    for byte in &digest[..6] {
        let _ = write!(pseudonym, "{byte:02x}");
    }
    pseudonym
}

#[cfg(test)]
mod tests {
    use axum::{Router, body::Body, middleware, routing::get};
    use http::{Request, StatusCode};
    use opentelemetry::propagation::TextMapPropagator;
    use opentelemetry_sdk::propagation::TraceContextPropagator;
    use tower::ServiceExt;

    use super::*;

    const CORRELATION_ID: &str = "a663c5c5-a4b2-4f01-97a0-4aee5749d41e";

    #[test]
    fn round_trips_traceparent_through_extract_and_inject() {
        let trace_id = "0af7651916cd43dd8448eb211c80319c"; // pragma: allowlist secret
        let traceparent = format!("00-{trace_id}-b7ad6b7169203331-01");
        let mut inbound = http::HeaderMap::new();
        inbound.insert("traceparent", http::HeaderValue::from_str(&traceparent).unwrap());

        let propagator = TraceContextPropagator::new();
        let parent = propagator.extract(&HeaderExtractor(&inbound));
        let mut outbound = HashMap::new();
        propagator.inject_context(&parent, &mut HeaderInjector(&mut outbound));

        let injected = outbound.get(&http::HeaderName::from_static("traceparent")).expect("traceparent injected");
        assert!(injected.to_str().unwrap().contains(trace_id));
    }

    #[tokio::test]
    async fn correlation_layer_preserves_valid_incoming_ids() {
        let app = Router::new()
            .route("/", get(|| async { StatusCode::NO_CONTENT }))
            .layer(middleware::from_fn(correlation_layer));
        let request = Request::builder()
            .header(TRANSACTION_ID_HEADER, "transaction-1")
            .header(CORRELATION_ID_HEADER, CORRELATION_ID)
            .body(Body::empty())
            .expect("request should build");

        let response = app.oneshot(request).await.expect("request should complete");

        assert_eq!(response.headers()[TRANSACTION_ID_HEADER], "transaction-1");
        assert_eq!(response.headers()[CORRELATION_ID_HEADER], CORRELATION_ID);
    }

    #[tokio::test]
    async fn invalid_correlation_id_is_replaced() {
        let app = Router::new()
            .route("/", get(|| async { StatusCode::NO_CONTENT }))
            .layer(middleware::from_fn(correlation_layer));
        let request = Request::builder()
            .header(TRANSACTION_ID_HEADER, "transaction-1")
            .header(CORRELATION_ID_HEADER, "not-a-uuid")
            .body(Body::empty())
            .expect("request should build");

        let response = app.oneshot(request).await.expect("request should complete");

        assert_eq!(response.headers()[TRANSACTION_ID_HEADER], "transaction-1");
        assert!(Uuid::parse_str(response.headers()[CORRELATION_ID_HEADER].to_str().unwrap()).is_ok());
    }

    #[tokio::test]
    async fn correlation_layer_generates_ids_when_missing() {
        let app = Router::new()
            .route("/", get(|| async { StatusCode::NO_CONTENT }))
            .layer(middleware::from_fn(correlation_layer));

        let response = app.oneshot(Request::new(Body::empty())).await.expect("request should complete");

        let transaction_id = response.headers()[TRANSACTION_ID_HEADER].to_str().expect("valid transaction ID");
        let correlation_id = response.headers()[CORRELATION_ID_HEADER].to_str().expect("valid correlation ID");
        assert!(Uuid::parse_str(transaction_id).is_ok());
        assert!(Uuid::parse_str(correlation_id).is_ok());
    }

    #[tokio::test]
    async fn valid_correlation_id_becomes_transaction_id_when_transaction_id_is_missing() {
        let app = Router::new()
            .route("/", get(|| async { StatusCode::NO_CONTENT }))
            .layer(middleware::from_fn(correlation_layer));
        let request = Request::builder()
            .header(CORRELATION_ID_HEADER, CORRELATION_ID)
            .body(Body::empty())
            .expect("request should build");

        let response = app.oneshot(request).await.expect("request should complete");

        assert_eq!(response.headers()[TRANSACTION_ID_HEADER], CORRELATION_ID);
        assert_eq!(response.headers()[CORRELATION_ID_HEADER], CORRELATION_ID);
    }

    #[tokio::test]
    async fn outbound_requests_receive_context_without_an_exporter() {
        let context = RequestContext {
            transaction_id: "transaction-1".to_owned(),
            correlation_id: CORRELATION_ID.to_owned(),
            trace: TraceFields {
                trace_id: "0af7651916cd43dd8448eb211c80319c".to_owned(), // pragma: allowlist secret
                span_id: "b7ad6b7169203331".to_owned(),                  // pragma: allowlist secret
                trace_flags: "01".to_owned(),
            },
        };

        let headers = CURRENT_CONTEXT
            .scope(context, async {
                let mut headers = HashMap::new();
                inject_current_context(&mut headers);
                headers
            })
            .await;

        assert_eq!(headers[&TRANSACTION_ID_HEADER], "transaction-1");
        assert_eq!(headers[&CORRELATION_ID_HEADER], CORRELATION_ID);
        assert_eq!(
            headers[&http::HeaderName::from_static("traceparent")],
            "00-0af7651916cd43dd8448eb211c80319c-b7ad6b7169203331-01"
        );
    }

    #[test]
    fn traceparent_parser_rejects_zero_and_malformed_ids() {
        assert!(parse_traceparent("00-00000000000000000000000000000000-b7ad6b7169203331-01").is_none());
        assert!(parse_traceparent("00-0af7651916cd43dd8448eb211c80319c-0000000000000000-01").is_none());
        assert!(parse_traceparent("malformed").is_none());
    }

    #[test]
    fn authenticated_subjects_receive_stable_non_reversible_ids() {
        let subject = "private-user-subject";
        let first = pseudonymous_user_id(subject);

        assert_eq!(first, pseudonymous_user_id(subject));
        assert!(first.starts_with("sha256:"));
        assert_eq!(first.len(), 19);
        assert!(!first.contains(subject));
    }
}
