use std::fmt;

use chrono::{SecondsFormat, Utc};
use contextforge_data_plane_lib::Config;
use serde_json::{Map, Value};
use tracing::{Event, Subscriber};
use tracing_subscriber::{
    fmt::{
        FmtContext,
        format::{FormatEvent, FormatFields, Json, Writer},
    },
    registry::LookupSpan,
};

pub(super) const DEFAULT_SERVICE_NAME: &str = "contextforge-data-plane";
const UNCLASSIFIED_ERROR_CODE: &str = "CFDP-UNCLASSIFIED";
const SPAN_FIELDS: &[&str] =
    &["transaction_id", "correlation_id", "trace_id", "span_id", "user_id", "component", "operation"];

fn configured_value(value: Option<&str>, fallback: &str) -> String {
    value.map(str::trim).filter(|value| !value.is_empty()).unwrap_or(fallback).to_owned()
}

fn has_meaningful_value(value: Option<&Value>) -> bool {
    value.is_some_and(|value| match value {
        Value::Null => false,
        Value::String(value) => !value.trim().is_empty(),
        _ => true,
    })
}

#[derive(Clone, Debug)]
pub(super) struct LoggingMetadata {
    service_name: String,
    version: String,
    environment: String,
    cluster_id: String,
}

impl LoggingMetadata {
    pub(super) fn from_config(configuration: &Config) -> Self {
        Self {
            service_name: configured_value(configuration.otlp_service_name.as_deref(), DEFAULT_SERVICE_NAME),
            version: env!("CARGO_PKG_VERSION").to_owned(),
            environment: configured_value(configuration.environment.as_deref(), "unknown"),
            cluster_id: configured_value(configuration.cluster_id.as_deref(), "unknown"),
        }
    }

    pub(super) fn service_name(&self) -> &str {
        &self.service_name
    }

    pub(super) fn version(&self) -> &str {
        &self.version
    }

    pub(super) fn environment(&self) -> &str {
        &self.environment
    }

    pub(super) fn cluster_id(&self) -> &str {
        &self.cluster_id
    }
}

#[derive(Clone, Debug)]
pub(super) struct StructuredJsonFormatter {
    inner: tracing_subscriber::fmt::format::Format<Json>,
    metadata: LoggingMetadata,
}

impl StructuredJsonFormatter {
    pub(super) fn new(metadata: LoggingMetadata) -> Self {
        Self {
            inner: tracing_subscriber::fmt::format()
                .json()
                .flatten_event(true)
                .with_current_span(true)
                .with_span_list(true),
            metadata,
        }
    }

    fn merge_span_fields(object: &mut Map<String, Value>, span: &Map<String, Value>) {
        for key in SPAN_FIELDS {
            if let Some(value) = span.get(*key) {
                object.entry((*key).to_owned()).or_insert_with(|| value.clone());
            }
        }
    }

    fn inherit_span_fields(object: &mut Map<String, Value>) {
        let span_list = object.get("spans").and_then(Value::as_array).cloned().unwrap_or_default();
        for span in span_list.iter().filter_map(Value::as_object) {
            Self::merge_span_fields(object, span);
        }
        if let Some(span) = object.get("span").and_then(Value::as_object).cloned() {
            Self::merge_span_fields(object, &span);
        }
        object.remove("span");
        object.remove("spans");
    }

    fn apply_error_contract(object: &mut Map<String, Value>) {
        if !has_meaningful_value(object.get("error_code")) {
            object.insert("error_code".to_owned(), Value::String(UNCLASSIFIED_ERROR_CODE.to_owned()));
        }
        if !has_meaningful_value(object.get("root_cause")) {
            let root_cause = object
                .get("error")
                .filter(|value| has_meaningful_value(Some(value)))
                .cloned()
                .or_else(|| object.get("message").filter(|value| has_meaningful_value(Some(value))).cloned())
                .unwrap_or_else(|| Value::String("root cause unavailable".to_owned()));
            object.insert("root_cause".to_owned(), root_cause);
        }
        object.entry("impact_scope".to_owned()).or_insert_with(|| Value::String("unknown".to_owned()));
        object.entry("retryable".to_owned()).or_insert(Value::Bool(false));
        object.entry("http_status".to_owned()).or_insert(Value::Null);
        object.entry("stack_trace".to_owned()).or_insert(Value::Null);
    }

    fn apply_contract(&self, object: &mut Map<String, Value>) {
        Self::inherit_span_fields(object);

        let mut log_level = object.remove("level").unwrap_or_else(|| Value::String("INFO".to_owned()));
        if object.remove("fatal").and_then(|value| value.as_bool()) == Some(true) {
            log_level = Value::String("FATAL".to_owned());
        }
        object.insert("log_level".to_owned(), log_level);
        object.insert("timestamp".to_owned(), Value::String(Utc::now().to_rfc3339_opts(SecondsFormat::Micros, true)));
        object.insert("service_name".to_owned(), Value::String(self.metadata.service_name.clone()));
        object.insert("version".to_owned(), Value::String(self.metadata.version.clone()));
        object.insert("environment".to_owned(), Value::String(self.metadata.environment.clone()));
        object.insert("cluster_id".to_owned(), Value::String(self.metadata.cluster_id.clone()));

        for key in ["transaction_id", "correlation_id", "trace_id", "span_id", "user_id"] {
            object.entry(key.to_owned()).or_insert(Value::Null);
        }
        object.entry("message".to_owned()).or_insert_with(|| Value::String(String::new()));
        let target = object.get("target").and_then(Value::as_str).unwrap_or(DEFAULT_SERVICE_NAME).to_owned();
        object.entry("component".to_owned()).or_insert_with(|| Value::String(target));

        let is_error = matches!(object.get("log_level").and_then(Value::as_str), Some("ERROR" | "FATAL"));
        if is_error {
            Self::apply_error_contract(object);
        } else {
            object.entry("error_code".to_owned()).or_insert(Value::Null);
        }
    }
}

impl<S, N> FormatEvent<S, N> for StructuredJsonFormatter
where
    S: Subscriber + for<'lookup> LookupSpan<'lookup>,
    N: for<'writer> FormatFields<'writer> + 'static,
{
    fn format_event(&self, context: &FmtContext<'_, S, N>, mut writer: Writer<'_>, event: &Event<'_>) -> fmt::Result {
        let mut encoded = String::new();
        self.inner.format_event(context, Writer::new(&mut encoded), event)?;
        let mut value: Value = serde_json::from_str(encoded.trim()).map_err(|_| fmt::Error)?;
        let object = value.as_object_mut().ok_or(fmt::Error)?;
        self.apply_contract(object);
        let encoded = serde_json::to_string(object).map_err(|_| fmt::Error)?;
        writer.write_str(&encoded)?;
        writer.write_char('\n')
    }
}

#[cfg(test)]
mod tests {
    use std::{
        io::Write,
        sync::{Arc, Mutex},
    };

    use serde_json::Value;
    use tracing_subscriber::{Registry, fmt::MakeWriter, layer::SubscriberExt};

    use super::{DEFAULT_SERVICE_NAME, LoggingMetadata, StructuredJsonFormatter, UNCLASSIFIED_ERROR_CODE};

    #[derive(Clone, Default)]
    struct SharedBuffer(Arc<Mutex<Vec<u8>>>);

    struct BufferWriter(SharedBuffer);

    impl Write for BufferWriter {
        fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
            self.0.0.lock().unwrap_or_else(std::sync::PoisonError::into_inner).extend_from_slice(bytes);
            Ok(bytes.len())
        }

        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    impl<'writer> MakeWriter<'writer> for SharedBuffer {
        type Writer = BufferWriter;

        fn make_writer(&'writer self) -> Self::Writer {
            BufferWriter(self.clone())
        }
    }

    fn metadata() -> LoggingMetadata {
        LoggingMetadata {
            service_name: "contextforge-data-plane".to_owned(),
            version: "1.2.3".to_owned(),
            environment: "onprem".to_owned(),
            cluster_id: "cluster-test-01".to_owned(),
        }
    }

    fn event_from(run: impl FnOnce()) -> Value {
        let output = SharedBuffer::default();
        let subscriber = Registry::default().with(
            tracing_subscriber::fmt::layer()
                .event_format(StructuredJsonFormatter::new(metadata()))
                .fmt_fields(tracing_subscriber::fmt::format::JsonFields::new())
                .with_ansi(false)
                .with_writer(output.clone()),
        );
        tracing::subscriber::with_default(subscriber, run);
        let bytes = output.0.lock().unwrap_or_else(std::sync::PoisonError::into_inner).clone();
        serde_json::from_slice(&bytes).expect("log line should be valid JSON")
    }

    #[test]
    fn metadata_uses_safe_defaults() {
        let metadata = LoggingMetadata::from_config(&contextforge_data_plane_lib::Config::default());

        assert_eq!(metadata.service_name(), DEFAULT_SERVICE_NAME);
        assert_eq!(metadata.environment(), "unknown");
        assert_eq!(metadata.cluster_id(), "unknown");
    }

    #[test]
    fn every_event_contains_the_structured_contract() {
        let event = event_from(|| {
            let span = tracing::info_span!(
                "request",
                transaction_id = "txn-1",
                correlation_id = "corr-1",
                trace_id = "00000000000000000000000000000001",
                span_id = "0000000000000001"
            );
            let _entered = span.enter();
            tracing::info!(component = "Routing", operation = "test_logging", "structured event");
        });

        assert!(event["timestamp"].as_str().is_some_and(|value| value.ends_with('Z')));
        assert_eq!(event["service_name"], "contextforge-data-plane");
        assert_eq!(event["version"], "1.2.3");
        assert_eq!(event["environment"], "onprem");
        assert_eq!(event["cluster_id"], "cluster-test-01");
        assert_eq!(event["transaction_id"], "txn-1");
        assert_eq!(event["correlation_id"], "corr-1");
        assert_eq!(event["trace_id"], "00000000000000000000000000000001");
        assert_eq!(event["span_id"], "0000000000000001");
        assert_eq!(event["user_id"], Value::Null);
        assert_eq!(event["log_level"], "INFO");
        assert_eq!(event["error_code"], Value::Null);
        assert_eq!(event["message"], "structured event");
        assert_eq!(event["component"], "Routing");
        assert!(event.get("span").is_none());
        assert!(event.get("spans").is_none());
    }

    #[test]
    fn error_events_receive_safe_defaults() {
        let event = event_from(|| tracing::error!(error = "serialization failed"));

        assert_eq!(event["error_code"], UNCLASSIFIED_ERROR_CODE);
        assert_eq!(event["root_cause"], "serialization failed");
        assert_eq!(event["impact_scope"], "unknown");
        assert_eq!(event["retryable"], false);
        assert!(event.get("http_status").is_some());
        assert!(event.get("stack_trace").is_some());
    }

    #[test]
    fn fatal_marker_sets_fatal_level() {
        let event = event_from(|| {
            tracing::error!(
                fatal = true,
                error_code = "CFDP-BOOTSTRAP",
                root_cause = "startup failed",
                impact_scope = "service-wide",
                "service terminated"
            );
        });

        assert_eq!(event["log_level"], "FATAL");
        assert!(event.get("fatal").is_none());
    }
}
