mod runtime;
#[cfg(feature = "test-plugins")]
mod test_plugins;

use std::{process::ExitCode, sync::Arc};

use clap::Parser;
use contextforge_data_plane_cpex::CpexRuntimeRegistry;
use contextforge_data_plane_lib::{Config, Gateway, RedisClient, RedisConfig, UserConfigStoreType};
use contextforge_data_plane_observability::{LoggingConfig, emit_bootstrap_failure, init_observability};
use rmcp::transport::streamable_http_server::session::local::LocalSessionManager;
use rustls::crypto;
use tikv_jemallocator::Jemalloc;
use tracing::info;

#[global_allocator]
static GLOBAL: Jemalloc = Jemalloc;
fn main() -> ExitCode {
    let provider = crypto::ring::default_provider();
    _ = provider.install_default();

    let config = Config::parse();
    let logging_config = logging_config(&config);
    let _guard = match init_observability(&logging_config) {
        Ok(guard) => guard,
        Err(error) => {
            emit_bootstrap_failure(&logging_config, error.as_ref());
            return ExitCode::FAILURE;
        },
    };

    match run(config) {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            tracing::error!(
                fatal = true,
                component = "Bootstrap",
                operation = "startup",
                error = %error,
                error_code = "CFDP-BOOTSTRAP",
                root_cause = %error,
                impact_scope = "service-wide",
                retryable = false,
                "service startup failed"
            );
            ExitCode::FAILURE
        },
    }
}

fn logging_config(config: &Config) -> LoggingConfig {
    LoggingConfig {
        service_name: config.otlp_service_name.clone(),
        version: env!("CARGO_PKG_VERSION").to_owned(),
        environment: config.environment.clone(),
        cluster_id: config.cluster_id.clone(),
        log_name: config.log_name.clone(),
        log_rotation: config.log_rotation.clone().unwrap_or_default(),
        enable_open_telemetry: config.enable_open_telemetry.unwrap_or(false),
        enable_otel_metrics: config.enable_otel_metrics.unwrap_or(false),
        otlp_endpoint: config.otlp_endpoint.clone(),
        otlp_metrics_endpoint: config.otlp_metrics_endpoint.clone(),
        otlp_protocol: config.otlp_protocol.clone().unwrap_or_default(),
        otlp_headers: config.otlp_headers.clone(),
    }
}

fn run(config: Config) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let feature_flags =
        [cfg!(feature = "plugins").then_some("plugins"), cfg!(feature = "test-plugins").then_some("test-plugins")]
            .into_iter()
            .flatten()
            .collect::<Vec<_>>()
            .join(",");
    let feature_flags = if feature_flags.is_empty() { "none" } else { &feature_flags };
    info!(
        component = "Bootstrap",
        operation = "startup",
        address = ?config.address,
        tls_address = ?config.tls_address,
        redis_mode = ?config.redis_mode,
        upstream_connection_mode = ?config.upstream_connection_mode,
        runtime_plugins_enabled = config.runtime_plugins_enabled.unwrap_or(false),
        open_telemetry_enabled = config.enable_open_telemetry.unwrap_or(false),
        otel_metrics_enabled = config.enable_otel_metrics.unwrap_or(false),
        single_runtime = config.single_runtime.unwrap_or(true),
        configured_cpus = ?config.number_of_cpus,
        git_commit_sha = option_env!("GIT_COMMIT_SHA").unwrap_or("unknown"),
        build_timestamp = option_env!("BUILD_TIMESTAMP").unwrap_or("unknown"),
        config_profile = config.environment.as_deref().unwrap_or("unknown"),
        feature_flags,
        db_version = "not_applicable",
        external_dependencies_reachable = "not_checked",
        "starting contextforge-data-plane"
    );

    let runtime = runtime::Runtime::from(&config);

    let plugin_registry = if config.runtime_plugins_enabled.unwrap_or(false) {
        Some(Arc::new(plugin_runtime_from_config(&config)?))
    } else {
        None
    };
    let plugin_runtime = plugin_registry.as_ref().map(|runtime| runtime.handle());

    let gateway = Gateway::builder()
        .with_config(config)
        .with_user_config_store_type(UserConfigStoreType::Redis)
        .with_session_manager(Arc::new(LocalSessionManager::default()))
        .with_plugin_runtime(plugin_runtime.clone())
        .build();

    runtime.execute(gateway, plugin_registry)
}

fn plugin_runtime_from_config(
    config: &Config,
) -> Result<CpexRuntimeRegistry, Box<dyn std::error::Error + Send + Sync>> {
    let redis_client = RedisClient::try_from(RedisConfig::try_from(config)?)?;
    let plugin_runtime = CpexRuntimeRegistry::with_redis_config(redis_client);
    #[cfg(any(feature = "test-plugins", feature = "plugins"))]
    let plugin_runtime = register_builtin_factories(plugin_runtime)?;
    Ok(plugin_runtime)
}

#[cfg(any(feature = "test-plugins", feature = "plugins"))]
fn register_builtin_factories(
    mut plugin_runtime: CpexRuntimeRegistry,
) -> Result<CpexRuntimeRegistry, Box<dyn std::error::Error + Send + Sync>> {
    #[cfg(feature = "test-plugins")]
    {
        test_plugins::register(&mut plugin_runtime)?;
    }
    #[cfg(feature = "plugins")]
    {
        plugin_runtime.register_factory(
            cpex_secrets_detection::KIND,
            Box::new(cpex_secrets_detection::SecretsDetectionFactory),
        )?;
    }
    Ok(plugin_runtime)
}
