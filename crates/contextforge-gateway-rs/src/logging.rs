use std::collections::HashMap;

use contextforge_gateway_rs_lib::{Config, LogRotation, OtlpProtocol};
use opentelemetry::global;
use opentelemetry::trace::TracerProvider;
use opentelemetry_otlp::{MetricExporter, Protocol, SpanExporter, WithExportConfig, WithHttpConfig, WithTonicConfig};
use opentelemetry_sdk::metrics::{PeriodicReader, SdkMeterProvider};
use opentelemetry_sdk::trace::{RandomIdGenerator, Sampler};
use tonic::metadata::{MetadataKey, MetadataMap, MetadataValue};
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
const DEFAULT_GRPC_TRACES_ENDPOINT: &str = "http://127.0.0.1:4317";
const DEFAULT_GRPC_METRICS_ENDPOINT: &str = "http://127.0.0.1:4317";
const DEFAULT_HTTP_TRACES_ENDPOINT: &str = "http://127.0.0.1:4318/v1/traces";
const DEFAULT_HTTP_METRICS_ENDPOINT: &str = "http://127.0.0.1:4318/v1/metrics";
const METRICS_EXPORT_INTERVAL: std::time::Duration = std::time::Duration::from_secs(30);

pub fn init_tracing_logging(configuration: &Config) -> Result<Guard, Box<dyn std::error::Error + Send + Sync>> {
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
        let headers = parse_otlp_headers(configuration.otlp_headers.as_deref())?;

        let exporter = match protocol {
            OtlpProtocol::Grpc => {
                let endpoint =
                    configuration.otlp_endpoint.clone().unwrap_or_else(|| DEFAULT_GRPC_TRACES_ENDPOINT.to_owned());
                SpanExporter::builder()
                    .with_tonic()
                    .with_endpoint(endpoint)
                    .with_metadata(headers_to_metadata(&headers))
                    .with_timeout(std::time::Duration::from_secs(3))
                    .build()?
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
                builder.build()?
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

        let meter_provider = init_meter_provider(configuration, &service_name)?;

        registry.with(console_layer).with(file_layer).with(telemetry.with_filter(tracing_filter)).init();

        Ok(Guard { appender: guard, meter_provider })
    } else {
        let meter_provider = init_meter_provider(configuration, CONTROLLER_NAME)?;
        registry.with(console_layer).with(file_layer).init();
        Ok(Guard { appender: guard, meter_provider })
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
fn init_meter_provider(
    configuration: &Config,
    service_name: &str,
) -> Result<Option<SdkMeterProvider>, Box<dyn std::error::Error + Send + Sync>> {
    if configuration.enable_otel_metrics != Some(true) {
        return Ok(None);
    }

    let protocol = configuration.otlp_protocol.clone().unwrap_or_default();
    let headers = parse_otlp_headers(configuration.otlp_headers.as_deref())?;

    let exporter = match protocol {
        OtlpProtocol::Grpc => {
            let endpoint = configuration
                .otlp_metrics_endpoint
                .as_ref()
                .map_or_else(|| DEFAULT_GRPC_METRICS_ENDPOINT.to_owned(), ToString::to_string);
            MetricExporter::builder()
                .with_tonic()
                .with_endpoint(endpoint)
                .with_metadata(headers_to_metadata(&headers))
                .with_timeout(std::time::Duration::from_secs(3))
                .build()?
        },
        OtlpProtocol::HttpProtobuf => {
            let endpoint = configuration
                .otlp_metrics_endpoint
                .as_ref()
                .map_or_else(|| DEFAULT_HTTP_METRICS_ENDPOINT.to_owned(), ToString::to_string);
            let mut builder = MetricExporter::builder()
                .with_http()
                .with_endpoint(endpoint)
                .with_protocol(Protocol::HttpBinary)
                .with_timeout(std::time::Duration::from_secs(10));
            if !headers.is_empty() {
                builder = builder.with_headers(headers);
            }
            builder.build()?
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
    Ok(Some(provider))
}

/// Converts a header [`HashMap`] to a tonic [`MetadataMap`] for gRPC metadata
/// attachment. Entries whose key or value cannot be encoded as valid ASCII gRPC
/// metadata are logged and skipped; they do not abort startup.
fn headers_to_metadata(headers: &HashMap<String, String>) -> MetadataMap {
    let mut map = MetadataMap::new();
    for (k, v) in headers {
        match (MetadataKey::from_bytes(k.as_bytes()), MetadataValue::try_from(v.as_str())) {
            (Ok(key), Ok(val)) => {
                map.insert(key, val);
            },
            _ => {
                tracing::warn!("skipping gRPC metadata entry with invalid key or value: {k:?}={v:?}");
            },
        }
    }
    map
}

/// Parses a comma-separated `key=value` header string into a [`HashMap`].
///
/// Whitespace around keys and values is trimmed. Empty segments (from
/// trailing commas or whitespace-only entries) are silently skipped.
/// Any non-empty segment that is missing the `=` separator, or has an
/// empty key, is treated as a configuration error so the application
/// fails fast rather than silently dropping user-supplied headers.
fn parse_otlp_headers(raw: Option<&str>) -> Result<HashMap<String, String>, Box<dyn std::error::Error + Send + Sync>> {
    let mut out = HashMap::new();
    let Some(raw) = raw else { return Ok(out) };
    for entry in raw.split(',') {
        let entry = entry.trim();
        if entry.is_empty() {
            continue;
        }
        match entry.split_once('=') {
            None => {
                return Err(format!("malformed OTLP header entry (missing '=' separator): {entry:?}").into());
            },
            Some((key, _)) if key.trim().is_empty() => {
                return Err(format!("malformed OTLP header entry (empty key): {entry:?}").into());
            },
            Some((key, value)) => {
                out.insert(key.trim().to_owned(), value.trim().to_owned());
            },
        }
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::parse_otlp_headers;

    #[test]
    fn parse_otlp_headers_handles_empty_and_missing_input() {
        assert!(parse_otlp_headers(None).unwrap().is_empty());
        assert!(parse_otlp_headers(Some("")).unwrap().is_empty());
        assert!(parse_otlp_headers(Some(" , , ")).unwrap().is_empty());
    }

    #[test]
    fn parse_otlp_headers_parses_multiple_entries_and_trims_whitespace() {
        let parsed = parse_otlp_headers(Some(" Authorization = Basic abc , X-Project=demo ")).unwrap();
        assert_eq!(parsed.get("Authorization"), Some(&"Basic abc".to_owned()));
        assert_eq!(parsed.get("X-Project"), Some(&"demo".to_owned()));
        assert_eq!(parsed.len(), 2);
    }

    #[test]
    fn parse_otlp_headers_rejects_entry_without_separator() {
        assert!(parse_otlp_headers(Some("no-equals")).is_err());
        assert!(parse_otlp_headers(Some("good=value,no-equals")).is_err());
    }

    #[test]
    fn parse_otlp_headers_rejects_entry_with_empty_key() {
        assert!(parse_otlp_headers(Some("=missing-key")).is_err());
    }
}
