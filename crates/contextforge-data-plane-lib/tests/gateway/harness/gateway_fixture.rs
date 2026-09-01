use std::{path::Path, sync::Arc};

use contextforge_data_plane_cpex::GatewayPluginRuntimeHandle;
use contextforge_data_plane_lib::{Config, Gateway, Result, UserConfigStoreType};
use rmcp::transport::streamable_http_server::session::local::LocalSessionManager;

use super::{MemoryUserConfigStore, TestServer, auth::AlwaysAllowAuthorizatioService}; // pragma: allowlist secret

/// Owns a gateway and every backend server used by one integration test.
#[must_use = "dropping the fixture shuts down all in-process servers"]
pub(crate) struct GatewayFixture {
    gateway: TestServer,
    backends: Vec<TestServer>,
    virtual_host_id: String,
}

pub(crate) struct GatewayTestConfig {
    pub(crate) config: Config,
    pub(crate) user_store: MemoryUserConfigStore,
    pub(crate) user_id: String,
    pub(crate) virtual_host_id: String,
    pub(crate) backends: Vec<TestServer>,
    pub(crate) plugin_runtime: Option<GatewayPluginRuntimeHandle>,
}

impl GatewayFixture {
    pub(crate) async fn start(test_config: GatewayTestConfig) -> Result<Self> {
        let GatewayTestConfig { config, user_store, user_id, virtual_host_id, backends, plugin_runtime } = test_config;
        let router = Self::router(config, user_store, user_id, plugin_runtime).await?;
        let gateway = TestServer::start_http(router).await?;

        Ok(Self { gateway, backends, virtual_host_id })
    }

    pub(crate) async fn start_tls(
        test_config: GatewayTestConfig,
        certificate: impl AsRef<Path>,
        private_key: impl AsRef<Path>, // pragma: allowlist secret
    ) -> Result<Self> {
        let GatewayTestConfig { config, user_store, user_id, virtual_host_id, backends, plugin_runtime } = test_config;
        let router = Self::router(config, user_store, user_id, plugin_runtime).await?;
        let gateway = TestServer::start_tls(router, certificate, private_key).await?;

        Ok(Self { gateway, backends, virtual_host_id })
    }

    async fn router(
        config: Config,
        user_store: MemoryUserConfigStore,
        user_id: String,
        plugin_runtime: Option<GatewayPluginRuntimeHandle>,
    ) -> Result<axum::Router> {
        Gateway::builder()
            .with_config(config)
            .with_session_manager(Arc::new(LocalSessionManager::default()))
            .with_user_config_store_type(UserConfigStoreType::Test(Arc::new(user_store)))
            .with_plugin_runtime(plugin_runtime)
            .with_authorization_service(Arc::new(AlwaysAllowAuthorizatioService::new(user_id)))
            .build()
            .into_router()
            .await
    }

    pub(crate) fn gateway_url(&self) -> String {
        self.gateway.url(&format!("/contextforge-rs/servers/{}/mcp", self.virtual_host_id))
    }

    pub(crate) async fn shutdown(self) -> Result<()> {
        let Self { gateway, backends, .. } = self;
        gateway.shutdown().await?;
        for backend in backends {
            backend.shutdown().await?;
        }
        Ok(())
    }
}
