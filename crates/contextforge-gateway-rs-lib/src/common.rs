use std::{
    fs::{self, File},
    io::{Cursor, Read},
    net::SocketAddr,
    path::PathBuf,
    sync::Arc,
};

use clap::{Parser, ValueEnum};
use http::uri::Authority;
use jsonwebtoken::DecodingKey;
use redis::{ConnectionAddr, IntoConnectionInfo, RedisError};
use rustls_pki_types::{CertificateDer, PrivatePkcs8KeyDer, pem::PemObject};
use secret_string::SecretString;
use serde::{Deserialize, Serialize};
use thiserror::Error;
use typed_builder::TypedBuilder;

use crate::user_config_store::UserConfigStore;

#[derive(Clone)]
pub struct JwtTokenDecoders {
    pub rs: Option<DecodingKey>,
    pub hmac_sha: Option<DecodingKey>,
}

#[allow(unused)]
#[derive(Clone)]
pub struct ContextForgeGatewayAppState {
    pub(crate) jwt_token_decoding_keys: JwtTokenDecoders,
    pub(crate) config_store: Arc<dyn UserConfigStore + Send + Sync>,
    pub(crate) config: Config,
}

#[derive(Clone, Debug, Serialize, Deserialize, TypedBuilder)]
pub struct User {
    email: String,
    full_name: String,
    is_admin: bool,
    auth_provider: String,
}

#[derive(Clone, Debug, Serialize, Deserialize, TypedBuilder)]
pub struct Scopes {
    server_id: Option<String>,
    permissions: Vec<String>,
    ip_restrictions: Vec<String>,
    time_restrictions: Option<serde_json::Value>,
}

#[derive(Clone, Debug, Serialize, Deserialize, TypedBuilder)]
pub struct ContextForgeClaims {
    pub sub: String,
    pub jti: String,
    pub token_use: String,
    pub iat: Option<u64>,
    pub iss: String,
    pub aud: String,
    pub exp: u64,
    pub teams: Option<Vec<String>>,
    pub user: User,
    pub scopes: Scopes,
}

pub type RedisClient = redis::Client;

#[derive(Debug, Clone)]
pub enum RedisConfig {
    PlainText { host: String, port: u16 },

    Tls { host: String, port: u16, trust_bundle: Vec<u8> },
    MTls { host: String, port: u16, trust_bundle: Vec<u8>, client_cert: Vec<u8>, client_key: Vec<u8> },
}

impl TryFrom<RedisConfig> for RedisClient {
    type Error = RedisError;

    fn try_from(redis_config: RedisConfig) -> Result<Self, Self::Error> {
        match redis_config {
            RedisConfig::PlainText { host, port } => {
                Ok(RedisClient::open(ConnectionAddr::Tcp(host, port).into_connection_info()?)?)
            },
            RedisConfig::Tls { host, port, trust_bundle } => RedisClient::build_with_tls(
                format!("rediss://{host}:{port}"),
                redis::TlsCertificates { client_tls: None, root_cert: Some(trust_bundle) },
            ),
            RedisConfig::MTls { host, port, trust_bundle, client_cert, client_key } => RedisClient::build_with_tls(
                format!("rediss://{host}:{port}"),
                redis::TlsCertificates {
                    client_tls: Some(redis::ClientTlsConfig { client_cert, client_key }),
                    root_cert: Some(trust_bundle),
                },
            ),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, ValueEnum)]
pub enum UpstreamConnectionMode {
    PlainTextOrTls,
    PlainTextOrMTls,
    TlsOnly,
    MtlsOnly,
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, ValueEnum)]
#[derive(Default)]
pub enum RedisConnectionMode {
    PlainText,
    #[default]
    Tls,
    Mtls,
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, ValueEnum)]
#[derive(Default)]
pub enum LogRotation {
    Minutely,
    #[default]
    Hourly,
    Daily,
    Never,
}

/// Wire protocol used to export OpenTelemetry data to the collector / backend.
///
/// `Grpc` targets the standard OTLP/gRPC port (e.g. `4317`) used by
/// collectors such as the OpenTelemetry Collector and Tempo.
/// `HttpProtobuf` targets OTLP over HTTP/1.1 with a protobuf payload
/// (e.g. `4318/v1/traces`) and is the only protocol supported by Langfuse's
/// OTel ingestion endpoint (`/api/public/otel/v1/traces`).
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, ValueEnum, Default)]
pub enum OtlpProtocol {
    #[default]
    Grpc,
    HttpProtobuf,
}

#[derive(Debug, Clone, Parser, Default)]
#[command(name = "contextforge-gateway-rs")]
#[command(about = "Minimal, fast and experimental Gateway/Dataplane for ContextForge")]
pub struct Config {
    #[arg(long, env = "CONTEXTFORGE_GATEWAY_RS_ADDRESS")]
    pub address: Option<SocketAddr>,

    #[arg(long, env = "CONTEXTFORGE_GATEWAY_RS_TOKEN_VERIFICATION_PUBLIC_KEY")]
    pub token_verification_public_key: Option<PathBuf>,

    #[cfg(feature = "with_tools")]
    #[arg(long, env = "CONTEXTFORGE_GATEWAY_RS_TOKEN_VERIFICATION_PRIVATE_KEY")]
    pub token_verification_private_key: PathBuf,

    #[arg(long, env = "CONTEXTFORGE_GATEWAY_RS_TOKEN_SECRET")]
    pub token_verification_secret: Option<SecretString<String>>,

    #[arg(long, env = "CONTEXTFORGE_GATEWAY_RS_ENABLE_OPEN_TELEMETRY")]
    pub enable_open_telemetry: Option<bool>,

    /// OTLP exporter endpoint. For `grpc` this is the collector address
    /// (e.g. `http://127.0.0.1:4317`). For `http-protobuf` this must be the
    /// full traces URL (e.g. `http://langfuse-web:3000/api/public/otel/v1/traces`
    /// for Langfuse, or `http://collector:4318/v1/traces` for the OTel Collector).
    #[arg(long, env = "OTEL_EXPORTER_OTLP_ENDPOINT")]
    pub otlp_endpoint: Option<http::Uri>,

    /// OTLP wire protocol. Use `http-protobuf` when exporting directly to
    /// Langfuse (it does not accept gRPC).
    #[arg(long, env = "OTEL_EXPORTER_OTLP_PROTOCOL")]
    pub otlp_protocol: Option<OtlpProtocol>,

    /// Additional headers to attach to every OTLP request, formatted as
    /// `key1=value1,key2=value2`. Used to pass authentication (for example
    /// Langfuse's `Authorization=Basic <base64(public:secret)>`).
    #[arg(long, env = "OTEL_EXPORTER_OTLP_HEADERS")]
    pub otlp_headers: Option<String>,

    /// Overrides the `service.name` OpenTelemetry resource attribute.
    /// Defaults to `CONTEXTFORGE-GATEWAY-RS`.
    #[arg(long, env = "CONTEXTFORGE_GATEWAY_RS_SERVICE_NAME")]
    pub otlp_service_name: Option<String>,

    /// Enables OTLP export of HTTP server metrics (request counts, latency
    /// histograms, in-flight gauge, body sizes) emitted by `axum-otel-metrics`.
    /// Independent from `enable_open_telemetry` so traces and metrics can be
    /// turned on individually. Langfuse does not ingest metrics, so this
    /// typically targets an OpenTelemetry Collector.
    #[arg(long, env = "CONTEXTFORGE_GATEWAY_RS_ENABLE_OTEL_METRICS")]
    pub enable_otel_metrics: Option<bool>,

    /// OTLP metrics endpoint. For `grpc` defaults to `http://127.0.0.1:4317`;
    /// for `http-protobuf` defaults to `http://127.0.0.1:4318/v1/metrics`.
    /// Kept separate from `otlp_endpoint` so traces and metrics can be routed
    /// to different backends (typical: traces to Langfuse, metrics to an
    /// OTel Collector).
    #[arg(long, env = "OTEL_EXPORTER_OTLP_METRICS_ENDPOINT")]
    pub otlp_metrics_endpoint: Option<http::Uri>,

    #[arg(long, env = "CONTEXTFORGE_GATEWAY_RS_GATEWAY_CPUS")]
    pub number_of_cpus: Option<usize>,

    #[arg(long, env = "CONTEXTFORGE_GATEWAY_RS_SINGLE_RUNTIME")]
    pub single_runtime: Option<bool>,

    #[arg(long, env = "CONTEXTFORGE_GATEWAY_RS_RUNTIME_PLUGINS_ENABLED")]
    pub runtime_plugins_enabled: Option<bool>,

    #[arg(long, env = "CONTEXTFORGE_GATEWAY_RS_TLS_ADDRESS")]
    pub tls_address: Option<SocketAddr>,

    #[arg(long, env = "CONTEXTFORGE_GATEWAY_RS_TLS_SERVER_PRIVATE_KEY")]
    pub server_private_key: Option<PathBuf>,

    #[arg(long, env = "CONTEXTFORGE_GATEWAY_RS_TLS_SERVER_CERTIFICATE")]
    pub server_certificate: Option<PathBuf>,

    #[arg(long, env = "CONTEXTFORGE_GATEWAY_RS_UPSTREAM_CONNECTION_MODE")]
    pub upstream_connection_mode: Option<UpstreamConnectionMode>,

    #[arg(long, env = "CONTEXTFORGE_GATEWAY_RS_TLS_UPSTREAM_PRIVATE_KEY")]
    pub upstream_private_key: Option<PathBuf>,

    #[arg(long, env = "CONTEXTFORGE_GATEWAY_RS_TLS_UPSTREAM_CERTIFICATE")]
    pub upstream_certificate: Option<PathBuf>,

    #[arg(long, env = "CONTEXTFORGE_GATEWAY_RS_TLS_UPSTREAM_TRUST_BUNDLE")]
    pub upstream_trust_bundle: Option<PathBuf>,

    #[arg(long, env = "CONTEXTFORGE_GATEWAY_RS_REDIS_HOSTNAME")]
    pub redis_address: String,
    #[arg(long, env = "CONTEXTFORGE_GATEWAY_RS_REDIS_PORT")]
    pub redis_port: u16,

    #[arg(long, env = "CONTEXTFORGE_GATEWAY_RS_REDIS_CONNECTION_MODE")]
    pub redis_mode: RedisConnectionMode,

    #[arg(long, env = "CONTEXTFORGE_GATEWAY_RS_REDIS_TLS_REDIS_TRUST_BUNDLE")]
    pub redis_tls_trust_bundle: Option<PathBuf>,

    #[arg(long, env = "CONTEXTFORGE_GATEWAY_RS_REDIS_TLS_REDIS_CLIENT_PRIVATE_KEY")]
    pub redis_tls_client_private_key: Option<PathBuf>,

    #[arg(long, env = "CONTEXTFORGE_GATEWAY_RS_REDIS_TLS_REDIS_CLIENT_CERTIFICATE")]
    pub redis_tls_client_certificate: Option<PathBuf>,

    #[arg(long, env = "CONTEXTFORGE_GATEWAY_LOG_NAME")]
    pub log_name: Option<String>,

    #[arg(long, env = "CONTEXTFORGE_GATEWAY_LOG_ROTATION")]
    pub log_rotation: Option<LogRotation>,
}

#[derive(Error, Debug)]
pub enum ConfigValidationError {
    #[error("Redis Configuration Error")]
    RedisConfigurationError(String),
}

impl TryFrom<&Config> for RedisConfig {
    fn try_from(value: &Config) -> Result<Self, Self::Error> {
        let _: Authority = format!("{}:{}", value.redis_address, value.redis_port)
            .parse::<Authority>()
            .map_err(|e| ConfigValidationError::RedisConfigurationError(e.to_string()))?;

        match value.redis_mode {
            RedisConnectionMode::PlainText => {
                Ok(Self::PlainText { host: value.redis_address.clone(), port: value.redis_port })
            },
            RedisConnectionMode::Tls => {
                let Some(trust_bundle) = &value.redis_tls_trust_bundle else {
                    return Err(ConfigValidationError::RedisConfigurationError(format!(
                        "Trust bundle is required for Redis {:?}",
                        value.redis_mode
                    )));
                };

                let trust_bundle = validate_certs(trust_bundle)?;

                Ok(Self::Tls { host: value.redis_address.clone(), port: value.redis_port, trust_bundle })
            },
            RedisConnectionMode::Mtls => {
                let Some(trust_bundle) = &value.redis_tls_trust_bundle else {
                    return Err(ConfigValidationError::RedisConfigurationError(format!(
                        "Trust bundle is required for Redis {:?}",
                        value.redis_mode
                    )));
                };

                let trust_bundle = validate_certs(trust_bundle)?;

                let Some(certificate) = &value.redis_tls_client_certificate else {
                    return Err(ConfigValidationError::RedisConfigurationError(format!(
                        "Client certificate is required for Redis {:?}",
                        value.redis_mode
                    )));
                };

                let client_cert = validate_certs(certificate)?;

                let Some(key) = &value.redis_tls_client_private_key else {
                    return Err(ConfigValidationError::RedisConfigurationError(format!(
                        "Client key is required for Redis {:?}",
                        value.redis_mode
                    )));
                };

                let client_key = validate_key(key)?;

                Ok(Self::MTls {
                    host: value.redis_address.clone(),
                    port: value.redis_port,
                    trust_bundle,
                    client_cert,
                    client_key,
                })
            },
        }
    }

    type Error = ConfigValidationError;
}

fn validate_certs(path: &PathBuf) -> Result<Vec<u8>, ConfigValidationError> {
    let mut buf = Vec::new();
    File::open(path)
        .map_err(|e| ConfigValidationError::RedisConfigurationError(e.to_string()))?
        .read_to_end(&mut buf)
        .map_err(|e| ConfigValidationError::RedisConfigurationError(e.to_string()))?;
    let mut cursor = Cursor::new(buf);

    let certs = CertificateDer::pem_reader_iter(&mut cursor)
        .collect::<Result<Vec<_>, _>>()
        .map_err(|e| ConfigValidationError::RedisConfigurationError(e.to_string()))?;

    if certs.is_empty() {
        Err(ConfigValidationError::RedisConfigurationError("No certificates provided".to_owned()))
    } else {
        Ok(cursor.into_inner())
    }
}

fn validate_key(path: &PathBuf) -> Result<Vec<u8>, ConfigValidationError> {
    let mut buf = Vec::new();
    File::open(path)
        .map_err(|e| ConfigValidationError::RedisConfigurationError(e.to_string()))?
        .read_to_end(&mut buf)
        .map_err(|e| ConfigValidationError::RedisConfigurationError(e.to_string()))?;

    let _ = PrivatePkcs8KeyDer::from_pem_slice(&buf)
        .map_err(|_| ConfigValidationError::RedisConfigurationError("Private key is invalid".to_owned()))?;
    Ok(buf)
}

impl TryFrom<&Config> for reqwest::Client {
    type Error = crate::Error;

    fn try_from(config: &Config) -> Result<Self, Self::Error> {
        let builder = reqwest::Client::builder();
        let builder = match config.upstream_connection_mode.as_ref() {
            None | Some(UpstreamConnectionMode::TlsOnly) => builder.https_only(true),
            Some(UpstreamConnectionMode::PlainTextOrTls) => builder.https_only(false),
            Some(UpstreamConnectionMode::PlainTextOrMTls) => {
                builder.https_only(false).identity(extract_identity(config)?)
            },
            Some(UpstreamConnectionMode::MtlsOnly) => builder.https_only(true).identity(extract_identity(config)?),
        };

        let builder = if let Some(trust_bundle) = config.upstream_trust_bundle.as_ref() {
            let mut buf = Vec::new();
            File::open(trust_bundle)?.read_to_end(&mut buf)?;
            let certificates = reqwest::Certificate::from_pem_bundle(&buf)?;
            builder.tls_certs_merge(certificates)
        } else {
            builder
        };

        Ok(builder.build()?)
    }
}

fn extract_identity(config: &Config) -> crate::Result<reqwest::Identity> {
    match (config.upstream_private_key.as_ref(), config.upstream_certificate.as_ref()) {
        (Some(private_key), Some(certificate)) => {
            let mut cert = fs::read(certificate)?;
            let key = fs::read(private_key)?;
            cert.extend(key);
            Ok(reqwest::Identity::from_pem(&cert)?)
        },

        _ => Err("Invalid/missing configuration".into()),
    }
}
