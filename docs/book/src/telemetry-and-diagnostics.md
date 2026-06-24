# Telemetry And Diagnostics

> Status: draft. To be implemented.

This chapter will explain how to see what the gateway is doing.

## To implement

- logging setup and useful log fields
- `tower_http::TraceLayer` request tracing
- `axum-otel-metrics` HTTP metrics
- OTLP trace export and Langfuse example
- OTLP metrics export and OpenTelemetry Collector example
- Prometheus queries for count, latency, active requests, and body sizes
- expected delay for periodic metric export
- debugging checklist for auth, config, routing, backend, and plugin problems
