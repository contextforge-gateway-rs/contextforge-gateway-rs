//! Shared structured logging, request correlation, and telemetry support for
//! ContextForge data-plane processes.

mod context;
mod formatter;
mod performance;

use std::collections::HashMap;

use chrono::{SecondsFormat, Utc};
use clap::ValueEnum;
use formatter::{LoggingMetadata, StructuredJsonFormatter};
use opentelemetry::global;
use opentelemetry::trace::TracerProvider;
use opentelemetry_otlp::{MetricExporter, Protocol, SpanExporter, WithExportConfig, WithHttpConfig, WithTonicConfig};
use opentelemetry_sdk::metrics::{PeriodicReader, SdkMeterProvider};
use opentelemetry_sdk::trace::{RandomIdGenerator, Sampler};
use tonic::metadata::{MetadataKey, MetadataMap, MetadataValue};
use tracing_appender::non_blocking::WorkerGuard;
use tracing_subscriber::{Layer, Registry, fmt, layer::SubscriberExt, util::SubscriberInitExt};

pub use context::{
    CORRELATION_ID_HEADER, ExtractingMakeSpan, LogOnResponse, TRANSACTION_ID_HEADER, correlation_layer,
    inject_current_context, record_authenticated_user,
};
pub use performance::PerformanceTimer;

type Error = Box<dyn std::error::Error + Send + Sync>;

const DEFAULT_GRPC_TRACES_ENDPOINT: &str = "http://127.0.0.1:4317";
const DEFAULT_GRPC_METRICS_ENDPOINT: &str = "http://127.0.0.1:4317";
const DEFAULT_HTTP_TRACES_ENDPOINT: &str = "http://127.0.0.1:4318/v1/traces";
const DEFAULT_HTTP_METRICS_ENDPOINT: &str = "http://127.0.0.1:4318/v1/metrics";
const METRICS_EXPORT_INTERVAL: std::time::Duration = std::time::Duration::from_secs(30);

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, ValueEnum, Default)]
pub enum LogRotation {
    Minutely,
    #[default]
    Hourly,
    Daily,
    Never,
}

/// Wire protocol used to export OpenTelemetry data.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, ValueEnum, Default)]
pub enum OtlpProtocol {
    #[default]
    Grpc,
    HttpProtobuf,
}

/// Application-owned settings consumed by [`init_observability`].
#[derive(Clone, Debug)]
pub struct LoggingConfig {
    pub service_name: Option<String>,
    pub version: String,
    pub environment: Option<String>,
    pub cluster_id: Option<String>,
    pub log_name: Option<String>,
    pub log_rotation: LogRotation,
    pub enable_open_telemetry: bool,
    pub enable_otel_metrics: bool,
    pub otlp_endpoint: Option<http::Uri>,
    pub otlp_metrics_endpoint: Option<http::Uri>,
    pub otlp_protocol: OtlpProtocol,
    pub otlp_headers: Option<String>,
}

impl Default for LoggingConfig {
    fn default() -> Self {
        Self {
            service_name: None,
            version: "unknown".to_owned(),
            environment: None,
            cluster_id: None,
            log_name: None,
            log_rotation: LogRotation::default(),
            enable_open_telemetry: false,
            enable_otel_metrics: false,
            otlp_endpoint: None,
            otlp_metrics_endpoint: None,
            otlp_protocol: OtlpProtocol::default(),
            otlp_headers: None,
        }
    }
}

/// Holds background exporter and file-writer handles for the process lifetime.
#[allow(dead_code)]
pub struct Guard {
    appender: WorkerGuard,
    meter_provider: Option<SdkMeterProvider>,
    tracer_provider: Option<opentelemetry_sdk::trace::SdkTracerProvider>,
}

impl Drop for Guard {
    fn drop(&mut self) {
        if let Some(provider) = self.meter_provider.take() {
            let _ = provider.shutdown();
        }
        if let Some(provider) = self.tracer_provider.take() {
            let _ = provider.shutdown();
        }
    }
}

/// Installs the process-wide subscriber. Applications should call this once at startup.
pub fn init_observability(configuration: &LoggingConfig) -> Result<Guard, Error> {
    let registry = Registry::default();
    let metadata = LoggingMetadata::from_config(configuration);
    let log_name = configuration.log_name.clone().unwrap_or("contextforge-data-plane.log".to_owned());

    let file_appender = match configuration.log_rotation {
        LogRotation::Minutely => tracing_appender::rolling::minutely(".", log_name),
        LogRotation::Hourly => tracing_appender::rolling::hourly(".", log_name),
        LogRotation::Daily => tracing_appender::rolling::daily(".", log_name),
        LogRotation::Never => tracing_appender::rolling::never(".", log_name),
    };

    let (non_blocking_appender, guard) = tracing_appender::non_blocking(file_appender);
    let file_filter =
        tracing_subscriber::EnvFilter::new(std::env::var("RUST_FILE_LOG").unwrap_or_else(|_| "info".to_owned()));
    let console_filter =
        tracing_subscriber::EnvFilter::new(std::env::var("RUST_LOG").unwrap_or_else(|_| "info".to_owned()));
    let tracing_filter =
        tracing_subscriber::EnvFilter::new(std::env::var("RUST_TRACE_LOG").unwrap_or_else(|_| "info".to_owned()));

    let console_layer = fmt::layer()
        .event_format(StructuredJsonFormatter::new(metadata.clone()))
        .fmt_fields(fmt::format::JsonFields::new())
        .with_ansi(false)
        .with_filter(console_filter);

    let file_layer = fmt::layer()
        .with_writer(non_blocking_appender)
        .event_format(StructuredJsonFormatter::new(metadata.clone()))
        .fmt_fields(fmt::format::JsonFields::new())
        .with_ansi(false)
        .with_filter(file_filter);

    global::set_text_map_propagator(opentelemetry_sdk::propagation::TraceContextPropagator::new());

    if configuration.enable_open_telemetry {
        let service_name = metadata.service_name().to_owned();
        let headers = parse_otlp_headers(configuration.otlp_headers.as_deref())?;
        let exporter = build_span_exporter(configuration, &headers)?;
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
        let tracer = tracer_provider.tracer(service_name.clone());
        let telemetry = tracing_opentelemetry::layer().with_tracer(tracer);
        let meter_provider = init_meter_provider(configuration, &service_name)?;

        registry.with(console_layer).with(file_layer).with(telemetry.with_filter(tracing_filter)).init();
        Ok(Guard { appender: guard, meter_provider, tracer_provider: Some(tracer_provider) })
    } else {
        let meter_provider = init_meter_provider(configuration, metadata.service_name())?;
        registry.with(console_layer).with(file_layer).init();
        Ok(Guard { appender: guard, meter_provider, tracer_provider: None })
    }
}

fn build_span_exporter(
    configuration: &LoggingConfig,
    headers: &HashMap<String, String>,
) -> Result<SpanExporter, Error> {
    match configuration.otlp_protocol {
        OtlpProtocol::Grpc => {
            let endpoint = configuration
                .otlp_endpoint
                .as_ref()
                .map_or_else(|| DEFAULT_GRPC_TRACES_ENDPOINT.to_owned(), ToString::to_string);
            Ok(SpanExporter::builder()
                .with_tonic()
                .with_endpoint(endpoint)
                .with_metadata(headers_to_metadata(headers)?)
                .with_timeout(std::time::Duration::from_secs(3))
                .build()?)
        },
        OtlpProtocol::HttpProtobuf => {
            let endpoint = configuration
                .otlp_endpoint
                .as_ref()
                .map_or_else(|| DEFAULT_HTTP_TRACES_ENDPOINT.to_owned(), ToString::to_string);
            let mut builder = SpanExporter::builder()
                .with_http()
                .with_endpoint(endpoint)
                .with_protocol(Protocol::HttpBinary)
                .with_timeout(std::time::Duration::from_secs(10));
            if !headers.is_empty() {
                builder = builder.with_headers(headers.clone());
            }
            Ok(builder.build()?)
        },
    }
}

#[allow(clippy::print_stderr)]
pub fn emit_bootstrap_failure(configuration: &LoggingConfig, error: &dyn std::fmt::Display) {
    let metadata = LoggingMetadata::from_config(configuration);
    let event = serde_json::json!({
        "timestamp": Utc::now().to_rfc3339_opts(SecondsFormat::Micros, true),
        "service_name": metadata.service_name(),
        "version": metadata.version(),
        "environment": metadata.environment(),
        "cluster_id": metadata.cluster_id(),
        "transaction_id": serde_json::Value::Null,
        "correlation_id": serde_json::Value::Null,
        "trace_id": serde_json::Value::Null,
        "span_id": serde_json::Value::Null,
        "user_id": serde_json::Value::Null,
        "log_level": "FATAL",
        "error_code": "CFDP-BOOTSTRAP",
        "message": "logging initialization failed",
        "component": "Bootstrap",
        "root_cause": error.to_string(),
        "impact_scope": "service-wide",
        "retryable": false,
        "http_status": serde_json::Value::Null,
        "stack_trace": serde_json::Value::Null,
    });
    eprintln!("{event}");
}

fn init_meter_provider(configuration: &LoggingConfig, service_name: &str) -> Result<Option<SdkMeterProvider>, Error> {
    if !configuration.enable_otel_metrics {
        return Ok(None);
    }

    let headers = parse_otlp_headers(configuration.otlp_headers.as_deref())?;
    let exporter = match configuration.otlp_protocol {
        OtlpProtocol::Grpc => {
            let endpoint = configuration
                .otlp_metrics_endpoint
                .as_ref()
                .map_or_else(|| DEFAULT_GRPC_METRICS_ENDPOINT.to_owned(), ToString::to_string);
            MetricExporter::builder()
                .with_tonic()
                .with_endpoint(endpoint)
                .with_metadata(headers_to_metadata(&headers)?)
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

fn headers_to_metadata(headers: &HashMap<String, String>) -> Result<MetadataMap, Error> {
    let mut map = MetadataMap::new();
    for (key, value) in headers {
        let key = MetadataKey::from_bytes(key.as_bytes())
            .map_err(|error| format!("invalid gRPC metadata key {key:?}: {error}"))?;
        let value =
            MetadataValue::try_from(value.as_str()).map_err(|error| format!("invalid gRPC metadata value: {error}"))?;
        map.insert(key, value);
    }
    Ok(map)
}

fn parse_otlp_headers(raw: Option<&str>) -> Result<HashMap<String, String>, Error> {
    let mut headers = HashMap::new();
    let Some(raw) = raw else { return Ok(headers) };
    for entry in raw.split(',').map(str::trim).filter(|entry| !entry.is_empty()) {
        match entry.split_once('=') {
            None => return Err(format!("malformed OTLP header entry (missing '=' separator): {entry:?}").into()),
            Some((key, _)) if key.trim().is_empty() => {
                return Err(format!("malformed OTLP header entry (empty key): {entry:?}").into());
            },
            Some((key, value)) => {
                headers.insert(key.trim().to_owned(), value.trim().to_owned());
            },
        }
    }
    Ok(headers)
}

#[cfg(test)]
mod tests {
    use super::parse_otlp_headers;

    #[test]
    fn parses_export_headers() {
        let parsed = parse_otlp_headers(Some(" Authorization = Basic abc , X-Project=demo ")).unwrap();
        assert_eq!(parsed.get("Authorization"), Some(&"Basic abc".to_owned()));
        assert_eq!(parsed.get("X-Project"), Some(&"demo".to_owned()));
    }

    #[test]
    fn rejects_malformed_export_headers() {
        assert!(parse_otlp_headers(Some("no-equals")).is_err());
        assert!(parse_otlp_headers(Some("=missing-key")).is_err());
    }
}
