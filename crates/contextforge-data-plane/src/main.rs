mod logging;
mod runtime;
#[cfg(feature = "test-plugins")]
mod test_plugins;

use std::sync::Arc;

use clap::Parser;
use contextforge_data_plane_cpex::CpexRuntimeRegistry;
use contextforge_data_plane_lib::{Config, Gateway, RedisClient, RedisConfig, UserConfigStoreType};
use rustls::crypto;
use tikv_jemallocator::Jemalloc;
use tracing::info;

#[global_allocator]
static GLOBAL: Jemalloc = Jemalloc;
fn main() -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let provider = crypto::ring::default_provider();
    _ = provider.install_default();

    let config = Config::parse();
    let _guard = logging::init_tracing_logging(&config)?;
    info!("starting contextforge-data-plane {config:?}");

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
