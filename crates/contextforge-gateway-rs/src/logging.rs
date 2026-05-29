use std::collections::HashMap;

use contextforge_gateway_rs_lib::{Config, LogRotation, OtlpProtocol};
use opentelemetry::global;
use opentelemetry::trace::TracerProvider;
use opentelemetry_otlp::{MetricExporter, Protocol, SpanExporter, WithExportConfig, WithHttpConfig};
use opentelemetry_sdk::metrics::{PeriodicReader, SdkMeterProvider};
use opentelemetry_sdk::trace::{RandomIdGenerator, Sampler};
use tracing_appender::non_blocking::WorkerGuard;
use tracing_subscriber::{
    Layer, Registry, filter,
    fmt::{self, format::FmtSpan},
    layer::SubscriberExt,
    util::SubscriberInitExt,
};

/// Holds RAII handles whose lifetime must match the process so background
/// telemetry tasks keep running. The file appender's worker thread needs
/// the [`WorkerGuard`] to stay alive to flush logs on shutdown, and the
/// metrics [`SdkMeterProvider`] needs to stay alive so its
/// [`PeriodicReader`] task keeps exporting at the configured interval.
#[allow(dead_code)]
pub struct Guard {
    appender: WorkerGuard,
    meter_provider: Option<SdkMeterProvider>,
}

const CONTROLLER_NAME: &str = "CONTEXTFORGE-GATEWAY-RS";
const DEFAULT_GRPC_ENDPOINT: &str = "http://127.0.0.1:4317";
const DEFAULT_HTTP_TRACES_ENDPOINT: &str = "http://127.0.0.1:4318/v1/traces";
const DEFAULT_HTTP_METRICS_ENDPOINT: &str = "http://127.0.0.1:4318/v1/metrics";
const METRICS_EXPORT_INTERVAL: std::time::Duration = std::time::Duration::from_secs(30);

pub fn init_tracing_logging(configuration: &Config) -> Guard {
    let registry = Registry::default();

    let log_name = configuration.log_name.clone().unwrap_or("contextforge-gateway-rs.log".to_owned());

    let file_appender = match configuration.log_rotation.clone().unwrap_or_default() {
        LogRotation::Minutely => tracing_appender::rolling::minutely(".", log_name),
        LogRotation::Hourly => tracing_appender::rolling::hourly(".", log_name),
        LogRotation::Daily => tracing_appender::rolling::daily(".", log_name),
        LogRotation::Never => tracing_appender::rolling::never(".", log_name),
    };

    let (non_blocking_appender, guard) = tracing_appender::non_blocking(file_appender);
    let file_filter =
        tracing_subscriber::EnvFilter::new(std::env::var("RUST_FILE_LOG").unwrap_or_else(|_| "debug".to_owned()));
    let console_filter =
        tracing_subscriber::EnvFilter::new(std::env::var("RUST_LOG").unwrap_or_else(|_| "debug".to_owned()));
    let tracing_filter =
        tracing_subscriber::EnvFilter::new(std::env::var("RUST_TRACE_LOG").unwrap_or_else(|_| "info".to_owned()));

    let console_layer = fmt::layer()
        .event_format(fmt::format().compact())
        .with_target(true)
        .with_span_events(FmtSpan::NONE)
        .with_ansi(false)
        .with_filter(filter::filter_fn(|meta| !meta.is_span()))
        .with_filter(console_filter);

    let file_layer = fmt::layer()
        .with_writer(non_blocking_appender)
        .with_target(true)
        .with_span_events(FmtSpan::NONE)
        .with_ansi(false)
        .with_filter(filter::filter_fn(|meta| !meta.is_span()))
        .with_filter(file_filter);

    if let Some(true) = configuration.enable_open_telemetry {
        let protocol = configuration.otlp_protocol.clone().unwrap_or_default();
        let service_name = configuration.otlp_service_name.clone().unwrap_or_else(|| CONTROLLER_NAME.to_owned());
        let headers = parse_otlp_headers(configuration.otlp_headers.as_deref());

        let exporter = match protocol {
            OtlpProtocol::Grpc => {
                let endpoint = configuration.otlp_endpoint.clone().unwrap_or_else(|| DEFAULT_GRPC_ENDPOINT.to_owned());
                SpanExporter::builder()
                    .with_tonic()
                    .with_endpoint(endpoint)
                    .with_timeout(std::time::Duration::from_secs(3))
                    .build()
                    .expect("failed to build OTLP/gRPC span exporter")
            },
            OtlpProtocol::HttpProtobuf => {
                let endpoint =
                    configuration.otlp_endpoint.clone().unwrap_or_else(|| DEFAULT_HTTP_TRACES_ENDPOINT.to_owned());
                let mut builder = SpanExporter::builder()
                    .with_http()
                    .with_endpoint(endpoint)
                    .with_protocol(Protocol::HttpBinary)
                    .with_timeout(std::time::Duration::from_secs(10));
                if !headers.is_empty() {
                    builder = builder.with_headers(headers);
                }
                builder.build().expect("failed to build OTLP/HTTP span exporter")
            },
        };

        let tracer_provider = opentelemetry_sdk::trace::SdkTracerProvider::builder()
            .with_batch_exporter(exporter)
            .with_id_generator(RandomIdGenerator::default())
            .with_sampler(Sampler::AlwaysOn)
            .with_resource(
                opentelemetry_sdk::Resource::builder()
                    .with_attributes(vec![opentelemetry::KeyValue::new("service.name", service_name.clone())])
                    .build(),
            )
            .build();

        let tracer = tracer_provider.tracer(CONTROLLER_NAME);
        let telemetry = tracing_opentelemetry::layer().with_tracer(tracer);

        let meter_provider = init_meter_provider(configuration, &service_name);

        registry.with(console_layer).with(file_layer).with(telemetry.with_filter(tracing_filter)).init();

        Guard { appender: guard, meter_provider }
    } else {
        let meter_provider = init_meter_provider(configuration, CONTROLLER_NAME);
        registry.with(console_layer).with(file_layer).init();
        Guard { appender: guard, meter_provider }
    }
}

/// Builds an OTLP metrics pipeline and installs it as the process-wide
/// [`global::meter_provider`] when `enable_otel_metrics = true`.
///
/// Mirrors the trace exporter's gRPC / HTTP protocol branching and reuses
/// the same `service.name` resource attribute so traces and metrics show up
/// under one identity.
///
/// Returns `None` when metrics are disabled; the returned provider must be
/// held alive (via [`Guard`]) for the [`PeriodicReader`]'s background task
/// to keep exporting.
fn init_meter_provider(configuration: &Config, service_name: &str) -> Option<SdkMeterProvider> {
    if configuration.enable_otel_metrics != Some(true) {
        return None;
    }

    let protocol = configuration.otlp_protocol.clone().unwrap_or_default();
    let headers = parse_otlp_headers(configuration.otlp_headers.as_deref());

    let exporter = match protocol {
        OtlpProtocol::Grpc => {
            let endpoint =
                configuration.otlp_metrics_endpoint.clone().unwrap_or_else(|| DEFAULT_GRPC_ENDPOINT.to_owned());
            MetricExporter::builder()
                .with_tonic()
                .with_endpoint(endpoint)
                .with_timeout(std::time::Duration::from_secs(3))
                .build()
                .expect("failed to build OTLP/gRPC metric exporter")
        },
        OtlpProtocol::HttpProtobuf => {
            let endpoint =
                configuration.otlp_metrics_endpoint.clone().unwrap_or_else(|| DEFAULT_HTTP_METRICS_ENDPOINT.to_owned());
            let mut builder = MetricExporter::builder()
                .with_http()
                .with_endpoint(endpoint)
                .with_protocol(Protocol::HttpBinary)
                .with_timeout(std::time::Duration::from_secs(10));
            if !headers.is_empty() {
                builder = builder.with_headers(headers);
            }
            builder.build().expect("failed to build OTLP/HTTP metric exporter")
        },
    };

    let reader = PeriodicReader::builder(exporter).with_interval(METRICS_EXPORT_INTERVAL).build();

    let provider = SdkMeterProvider::builder()
        .with_reader(reader)
        .with_resource(
            opentelemetry_sdk::Resource::builder()
                .with_attributes(vec![opentelemetry::KeyValue::new("service.name", service_name.to_owned())])
                .build(),
        )
        .build();

    global::set_meter_provider(provider.clone());
    Some(provider)
}

/// Parse a comma separated list of `key=value` pairs into a header map.
///
/// Whitespace around keys and values is trimmed. Empty entries and entries
/// without an `=` are silently ignored so partial or malformed input never
/// breaks telemetry startup.
fn parse_otlp_headers(raw: Option<&str>) -> HashMap<String, String> {
    let mut out = HashMap::new();
    let Some(raw) = raw else { return out };
    for entry in raw.split(',') {
        let entry = entry.trim();
        if entry.is_empty() {
            continue;
        }
        if let Some((key, value)) = entry.split_once('=') {
            let key = key.trim();
            let value = value.trim();
            if !key.is_empty() {
                out.insert(key.to_owned(), value.to_owned());
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::parse_otlp_headers;

    #[test]
    fn parse_otlp_headers_handles_empty_and_missing_input() {
        assert!(parse_otlp_headers(None).is_empty());
        assert!(parse_otlp_headers(Some("")).is_empty());
        assert!(parse_otlp_headers(Some(" , , ")).is_empty());
    }

    #[test]
    fn parse_otlp_headers_parses_multiple_entries_and_trims_whitespace() {
        let parsed = parse_otlp_headers(Some(" Authorization = Basic abc , X-Project=demo "));
        assert_eq!(parsed.get("Authorization"), Some(&"Basic abc".to_owned()));
        assert_eq!(parsed.get("X-Project"), Some(&"demo".to_owned()));
        assert_eq!(parsed.len(), 2);
    }

    #[test]
    fn parse_otlp_headers_ignores_malformed_entries() {
        let parsed = parse_otlp_headers(Some("no-equals,=missing-key,good=value"));
        assert_eq!(parsed.len(), 1);
        assert_eq!(parsed.get("good"), Some(&"value".to_owned()));
    }
}
