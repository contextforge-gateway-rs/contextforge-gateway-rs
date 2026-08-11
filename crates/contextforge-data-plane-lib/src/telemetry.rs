//! Request correlation and W3C trace-context propagation glue.
//!
//! The OTLP exporter is configured in the binary crate; this module wires the
//! request seams so the gateway emits correlated structured events, continues
//! the caller's distributed trace, and passes both contexts to backend MCP
//! servers. W3C propagation is a no-op unless a global text-map propagator is
//! installed (see the binary's `init_tracing_logging`); request correlation
//! remains active when OpenTelemetry is disabled.

use std::{collections::HashMap, time::Duration};

use axum::{extract::Request, middleware::Next, response::Response};
use opentelemetry::global;
use opentelemetry::propagation::{Extractor, Injector};
use opentelemetry::trace::TraceContextExt;
use tower_http::trace::{MakeSpan, OnResponse};
use tracing::{Span, field};
use tracing_opentelemetry::OpenTelemetrySpanExt;
use uuid::Uuid;

pub const TRANSACTION_ID_HEADER: http::HeaderName = http::HeaderName::from_static("x-contextforge-transaction-id");
pub const CORRELATION_ID_HEADER: http::HeaderName = http::HeaderName::from_static("x-contextforge-correlation-id");

tokio::task_local! {
    static CURRENT_CORRELATION: CorrelationContext;
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct CorrelationContext {
    transaction_id: String,
    correlation_id: String,
}

impl CorrelationContext {
    fn from_headers(headers: &http::HeaderMap) -> Self {
        let incoming_correlation = non_empty_header(headers, &CORRELATION_ID_HEADER);
        let transaction_id = non_empty_header(headers, &TRANSACTION_ID_HEADER)
            .or_else(|| incoming_correlation.clone())
            .unwrap_or_else(|| Uuid::new_v4().to_string());
        let correlation_id = incoming_correlation.unwrap_or_else(|| transaction_id.clone());
        Self { transaction_id, correlation_id }
    }

    fn insert_response_headers(&self, headers: &mut http::HeaderMap) {
        if let Ok(value) = http::HeaderValue::from_str(&self.transaction_id) {
            headers.insert(TRANSACTION_ID_HEADER, value);
        }
        if let Ok(value) = http::HeaderValue::from_str(&self.correlation_id) {
            headers.insert(CORRELATION_ID_HEADER, value);
        }
    }

    fn insert_outbound_headers(&self, headers: &mut HashMap<http::HeaderName, http::HeaderValue>) {
        if let Ok(value) = http::HeaderValue::from_str(&self.transaction_id) {
            headers.insert(TRANSACTION_ID_HEADER, value);
        }
        if let Ok(value) = http::HeaderValue::from_str(&self.correlation_id) {
            headers.insert(CORRELATION_ID_HEADER, value);
        }
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

pub(crate) async fn correlation_layer(mut request: Request, next: Next) -> Response {
    let context = CorrelationContext::from_headers(request.headers());
    request.extensions_mut().insert(context.clone());
    let mut response = CURRENT_CORRELATION.scope(context.clone(), next.run(request)).await;
    context.insert_response_headers(response.headers_mut());
    response
}

/// Reads inbound `http::HeaderMap` for the text-map propagator (extract side).
struct HeaderExtractor<'a>(&'a http::HeaderMap);

impl Extractor for HeaderExtractor<'_> {
    fn get(&self, key: &str) -> Option<&str> {
        self.0.get(key).and_then(|value| value.to_str().ok())
    }

    fn keys(&self) -> Vec<&str> {
        self.0.keys().map(http::HeaderName::as_str).collect()
    }
}

/// Writes the outbound backend header map for the text-map propagator (inject
/// side). Malformed keys/values are dropped rather than propagated.
struct HeaderInjector<'a>(&'a mut HashMap<http::HeaderName, http::HeaderValue>);

impl Injector for HeaderInjector<'_> {
    fn set(&mut self, key: &str, value: String) {
        if let (Ok(name), Ok(value)) =
            (http::HeaderName::from_bytes(key.as_bytes()), http::HeaderValue::from_str(&value))
        {
            self.0.insert(name, value);
        }
    }
}

/// [`MakeSpan`] that opens the per-request span and re-parents it onto any W3C
/// trace context found in the inbound headers, so the gateway span continues
/// the caller's trace instead of starting a fresh one.
///
/// Body-generic on purpose: the outermost `TraceLayer` sees whatever body type
/// the server hands it, and a struct impl avoids pinning that down.
#[derive(Clone, Copy, Debug, Default)]
pub struct ExtractingMakeSpan;

impl<B> MakeSpan<B> for ExtractingMakeSpan {
    fn make_span(&mut self, request: &http::Request<B>) -> Span {
        let correlation = request
            .extensions()
            .get::<CorrelationContext>()
            .cloned()
            .unwrap_or_else(|| CorrelationContext::from_headers(request.headers()));
        let span = tracing::info_span!(
            "http-request",
            transaction_id = %correlation.transaction_id,
            correlation_id = %correlation.correlation_id,
            trace_id = field::Empty,
            span_id = field::Empty,
            user_id = field::Empty,
            component = "HttpServer",
            operation = "http_request",
            http_method = %request.method(),
            http_path = request.uri().path(),
            http_version = ?request.version(),
        );
        let parent =
            global::get_text_map_propagator(|propagator| propagator.extract(&HeaderExtractor(request.headers())));
        // Errors only when no OTel layer is registered (OTel disabled); ignore.
        let _ = span.set_parent(parent);
        let context = span.context();
        let otel_span = context.span();
        let span_context = otel_span.span_context();
        if span_context.is_valid() {
            let trace_id = span_context.trace_id().to_string();
            let span_id = span_context.span_id().to_string();
            span.record("trace_id", &trace_id);
            span.record("span_id", &span_id);
        }
        span
    }
}

#[derive(Clone, Copy, Debug, Default)]
pub(crate) struct LogOnResponse;

impl<B> OnResponse<B> for LogOnResponse {
    fn on_response(self, response: &http::Response<B>, latency: Duration, _span: &Span) {
        let http_status = response.status().as_u16();
        let latency_ms = u64::try_from(latency.as_millis()).unwrap_or(u64::MAX);
        if response.status().is_server_error() {
            tracing::error!(
                component = "HttpServer",
                operation = "http_request",
                event_type = "PERFORMANCE",
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
                outcome = "success",
                http_status,
                latency_ms,
                "HTTP request completed"
            );
        }
    }
}

/// Injects the current span's trace context into `headers` so the trace
/// propagates to the backend MCP server. No-op when there is no valid active
/// context (e.g. OTel disabled): the propagator writes nothing.
pub fn inject_current_context(headers: &mut HashMap<http::HeaderName, http::HeaderValue>) {
    if let Ok(correlation) = CURRENT_CORRELATION.try_with(Clone::clone) {
        correlation.insert_outbound_headers(headers);
    }
    let context = Span::current().context();
    global::get_text_map_propagator(|propagator| propagator.inject_context(&context, &mut HeaderInjector(headers)));
}

#[cfg(test)]
mod tests {
    use axum::{Router, body::Body, middleware, routing::get};
    use http::{Request, StatusCode};
    use opentelemetry::propagation::TextMapPropagator;
    use opentelemetry_sdk::propagation::TraceContextPropagator;
    use tower::ServiceExt;

    use super::*;

    #[test]
    fn round_trips_traceparent_through_extract_and_inject() {
        // A well-formed W3C traceparent carrying a known trace id.
        let trace_id = "0af7651916cd43dd8448eb211c80319c";
        let traceparent = format!("00-{trace_id}-b7ad6b7169203331-01");

        let mut inbound = http::HeaderMap::new();
        inbound.insert("traceparent", http::HeaderValue::from_str(&traceparent).unwrap());

        let propagator = TraceContextPropagator::new();
        let parent = propagator.extract(&HeaderExtractor(&inbound));

        let mut outbound = HashMap::new();
        propagator.inject_context(&parent, &mut HeaderInjector(&mut outbound));

        let injected = outbound.get(&http::HeaderName::from_static("traceparent")).expect("traceparent injected");
        // Same trace id must survive extract -> inject (span id differs).
        assert!(injected.to_str().unwrap().contains(trace_id));
    }

    #[tokio::test]
    async fn correlation_layer_preserves_incoming_ids() {
        let app = Router::new()
            .route("/", get(|| async { StatusCode::NO_CONTENT }))
            .layer(middleware::from_fn(correlation_layer));
        let request = Request::builder()
            .header(TRANSACTION_ID_HEADER, "transaction-1")
            .header(CORRELATION_ID_HEADER, "correlation-1")
            .body(Body::empty())
            .expect("request should build");

        let response = app.oneshot(request).await.expect("request should complete");

        assert_eq!(response.headers()[TRANSACTION_ID_HEADER], "transaction-1");
        assert_eq!(response.headers()[CORRELATION_ID_HEADER], "correlation-1");
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
        assert_eq!(correlation_id, transaction_id);
    }

    #[tokio::test]
    async fn correlation_id_becomes_transaction_id_when_transaction_id_is_missing() {
        let app = Router::new()
            .route("/", get(|| async { StatusCode::NO_CONTENT }))
            .layer(middleware::from_fn(correlation_layer));
        let request = Request::builder()
            .header(CORRELATION_ID_HEADER, "correlation-1")
            .body(Body::empty())
            .expect("request should build");

        let response = app.oneshot(request).await.expect("request should complete");

        assert_eq!(response.headers()[TRANSACTION_ID_HEADER], "correlation-1");
        assert_eq!(response.headers()[CORRELATION_ID_HEADER], "correlation-1");
    }

    #[tokio::test]
    async fn outbound_requests_receive_correlation_headers() {
        let correlation = CorrelationContext {
            transaction_id: "transaction-1".to_owned(),
            correlation_id: "correlation-1".to_owned(),
        };

        let headers = CURRENT_CORRELATION
            .scope(correlation, async {
                let mut headers = HashMap::new();
                inject_current_context(&mut headers);
                headers
            })
            .await;

        assert_eq!(headers[&TRANSACTION_ID_HEADER], "transaction-1");
        assert_eq!(headers[&CORRELATION_ID_HEADER], "correlation-1");
    }
}
