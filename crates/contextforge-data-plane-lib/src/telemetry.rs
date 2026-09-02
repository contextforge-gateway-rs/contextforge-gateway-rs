//! W3C trace-context propagation glue.
//!
//! The OTLP exporter is configured in the binary crate; this module wires the
//! two request seams so the gateway continues the caller's distributed trace
//! and passes it on to backend MCP servers. All functions here are no-ops
//! unless a global text-map propagator is installed (see the binary's
//! `init_tracing_logging`), so they are safe to call when OTel is disabled.

use std::collections::HashMap;

use opentelemetry::global;
use opentelemetry::propagation::{Extractor, Injector};
use tower_http::trace::MakeSpan;
use tracing::Span;
use tracing_opentelemetry::OpenTelemetrySpanExt;

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
        let span = tracing::info_span!(
            "http-request",
            method = %request.method(),
            uri = %request.uri(),
            version = ?request.version(),
        );
        let parent =
            global::get_text_map_propagator(|propagator| propagator.extract(&HeaderExtractor(request.headers())));
        // Errors only when no OTel layer is registered (OTel disabled); ignore.
        let _ = span.set_parent(parent);
        span
    }
}

/// Injects the current span's trace context into `headers` so the trace
/// propagates to the backend MCP server. No-op when there is no valid active
/// context (e.g. OTel disabled): the propagator writes nothing.
pub fn inject_current_context(headers: &mut HashMap<http::HeaderName, http::HeaderValue>) {
    let context = Span::current().context();
    global::get_text_map_propagator(|propagator| propagator.inject_context(&context, &mut HeaderInjector(headers)));
}

/// Returns the current validated OpenTelemetry trace and span identifiers.
pub(crate) fn current_trace_ids() -> (Option<String>, Option<String>) {
    let context = Span::current().context();
    let span = opentelemetry::trace::TraceContextExt::span(&context);
    let span_context = span.span_context();
    if span_context.is_valid() {
        (Some(span_context.trace_id().to_string()), Some(span_context.span_id().to_string()))
    } else {
        (None, None)
    }
}

#[cfg(test)]
mod tests {
    use opentelemetry::propagation::TextMapPropagator;
    use opentelemetry_sdk::propagation::TraceContextPropagator;

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
}
