use std::sync::Arc;

use axum::middleware;
use axum_otel_metrics::HttpMetricsLayerBuilder;
use contextforge_data_plane_cpex::GatewayPluginRuntimeHandle;
use futures::FutureExt;
use http::uri::Authority;

use rmcp::transport::{
    StreamableHttpServerConfig,
    streamable_http_server::{session::local::LocalSessionManager, tower::StreamableHttpService},
};
mod authorization;
mod common;
mod const_values;
mod errors;
mod gateway;
mod layers;
mod mcp_standard_headers;
mod telemetry;
mod transports;

#[cfg(feature = "with_tools")]
mod tools;

mod user_config_store;
pub use common::{RedisClient, RedisConfig, UpstreamConnectionMode};
use gateway::McpService;

use tower_http::cors::{Any, CorsLayer};
use tower_http::trace::TraceLayer;
use transports::{DownstreamTls, Tcp};
use typed_builder::TypedBuilder;
pub use user_config_store::RedisUserConfigStore;
pub use user_config_store::{ConfigStoreError, UserConfigStore};

pub use crate::common::*;

pub type Error = Box<dyn std::error::Error + Send + Sync + 'static>;
pub type Result<T> = std::result::Result<T, Error>;

use crate::layers::{
    claims_id::claims_layer,
    mcp_header_limits::{StandardHeaderLimits, mcp_header_limits_layer},
    mcp_origin::mcp_origin_layer,
    principal_extractor::principal_extractor_layer,
    user_config_store::user_config_store_layer,
    virtual_host_config::virtual_host_config_layer,
    virtual_host_id::virtual_host_id_layer,
};
pub use authorization::{AuthorizationClaims, AuthorizationService, get_authorization_service};

#[derive(Clone)]
pub enum UserConfigStoreType {
    Redis,
    Test(Arc<dyn UserConfigStore + std::marker::Send + Sync>),
}

#[derive(Clone, TypedBuilder)]
#[builder(field_defaults(setter(prefix = "with_")))]
pub struct Gateway {
    config: Config,
    session_manager: Arc<LocalSessionManager>,
    user_config_store_type: UserConfigStoreType,
    #[builder(default)]
    plugin_runtime: Option<GatewayPluginRuntimeHandle>,
    authorization_service: Arc<dyn AuthorizationService + Send + Sync>,
}

impl Gateway {
    pub async fn run_gateway(self) -> Result<()> {
        let config = self.config.clone();
        let app = self.build_app().await?;

        let mut handlers = vec![];

        match (Option::<Tcp>::try_from(&config), Option::<DownstreamTls>::try_from(&config)) {
            (Ok(Some(tcp)), Ok(Some(tls))) => {
                handlers.push(tcp.handle_tcp(app.clone()).boxed());
                handlers.push(tls.handle_tls(app.clone()).boxed());
            },
            (Ok(Some(tcp)), Ok(None)) => {
                handlers.push(tcp.handle_tcp(app.clone()).boxed());
            },

            (Ok(None), Ok(Some(tls))) => {
                handlers.push(tls.handle_tls(app.clone()).boxed());
            },

            (Ok(None), Ok(None)) => return Err("At least one listening address has to be provided ".into()),

            (Ok(_), Err(e)) => return Err(format!("TLS is miconfigured {e:?}").into()),
            _ => return Err("Listening address(es) misconfigured".into()),
        }

        let _ = futures::future::join_all(handlers).await;

        Ok(())
    }

    async fn build_app(self) -> Result<axum::Router> {
        let Gateway { config, session_manager, user_config_store_type, plugin_runtime, authorization_service } = self;
        let user_config_store = match user_config_store_type {
            UserConfigStoreType::Redis => Arc::new(get_config_store(&config).await?),
            UserConfigStoreType::Test(store) => store,
        };
        let user_config_store = user_config_store as Arc<dyn UserConfigStore + Send + Sync>;

        // RMCP owns Host validation. Keep its Origin validator disabled because
        // mcp_origin_layer enforces exact origin tuples and returns 403 for every
        // invalid present Origin, including when no allowlist is configured.
        let streamable_config = if let Some(ref hosts) = config.mcp_allowed_hosts {
            StreamableHttpServerConfig::default()
                .with_allowed_hosts(hosts.iter().map(Authority::as_str))
                .disable_allowed_origins()
        } else {
            StreamableHttpServerConfig::default().disable_allowed_hosts().disable_allowed_origins()
        };
        let reqwest_backend_client = reqwest::Client::try_from(&config)?;

        // Create streamable HTTP service
        let mcp_service: StreamableHttpService<McpService, LocalSessionManager> = StreamableHttpService::new(
            move || {
                Ok(McpService::builder()
                    .with_http_client(reqwest_backend_client.clone())
                    .with_plugin_runtime(plugin_runtime.clone())
                    .build())
            },
            session_manager,
            streamable_config,
        );

        let cors_layer = CorsLayer::new().allow_origin(Any).allow_methods(Any).allow_headers(Any).expose_headers(Any);

        let mcp_gateway_state: ContextForgeDataPlaneAppState = ContextForgeDataPlaneAppState {
            authorization_service,
            config_store: Arc::clone(&user_config_store),
            config: config.clone(),
        };
        let mcp_standard_header_limits = StandardHeaderLimits::from(&config);

        let app = axum::Router::new()
            .nest_service("/servers/{virtual_host_name}/mcp", mcp_service)
            .layer(middleware::from_fn(virtual_host_config_layer))
            .layer(middleware::from_fn_with_state(mcp_gateway_state.clone(), user_config_store_layer))
            .layer(middleware::from_fn(principal_extractor_layer))
            .layer(middleware::from_fn_with_state(mcp_gateway_state.clone(), claims_layer))
            .layer(middleware::from_fn(virtual_host_id_layer))
            // Keep this outside auth/config/RMCP work so oversized MCP headers
            // are rejected before JWT validation or body parsing.
            .layer(middleware::from_fn_with_state(mcp_standard_header_limits, mcp_header_limits_layer))
            .layer(cors_layer)
            // mcp_origin_layer is the outermost wrapper: fires before JWT auth,
            // session creation, and backend fan-out.
            .layer(middleware::from_fn_with_state(config.clone(), mcp_origin_layer));

        #[cfg(feature = "with_tools")]
        let app = tools::add_tools(app);

        let app = app.with_state(mcp_gateway_state);
        let app = axum::Router::new()
            .nest("/contextforge-rs", app)
            .layer(TraceLayer::new_for_http().make_span_with(telemetry::ExtractingMakeSpan))
            .layer(HttpMetricsLayerBuilder::new().build());

        Ok(app)
    }
}

pub async fn get_config_store(config: &Config) -> Result<RedisUserConfigStore> {
    let redis_config = RedisConfig::try_from(config)?;
    let cache_expiry = std::time::Duration::from_secs(config.user_config_cache_expiry_seconds);
    RedisUserConfigStore::new(&RedisClient::try_from(redis_config)?, cache_expiry).await
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use async_trait::async_trait;
    use axum::body::Body;
    use contextforge_data_plane_apis::{User, user_store::UserConfig};
    use http::{Request, StatusCode};
    use rmcp::transport::streamable_http_server::session::local::LocalSessionManager;
    use tower::ServiceExt;

    use crate::{
        Config, Gateway, UserConfigStoreType, get_authorization_service,
        user_config_store::{ConfigStoreError, UserConfigStore},
    };

    #[derive(Clone)]
    struct UnusedConfigStore;

    #[async_trait]
    impl UserConfigStore for UnusedConfigStore {
        async fn get_config<'a>(&self, _key: &'a User) -> Result<UserConfig, ConfigStoreError> {
            unreachable!("mcp header limit rejection must run before config lookup")
        }

        async fn set_config<'a>(&self, _key: &'a User, _user_config: &'a UserConfig) -> Result<(), ConfigStoreError> {
            unreachable!("mcp header limit rejection must run before config write")
        }
    }

    #[tokio::test]
    async fn production_router_rejects_excessive_mcp_headers_before_auth() {
        let config = Config { mcp_standard_header_max_count: 1, ..Config::default() };
        let app = Gateway::builder()
            .with_authorization_service(get_authorization_service(&config).expect("this should not fail"))
            .with_config(config)
            .with_session_manager(Arc::new(LocalSessionManager::default()))
            .with_user_config_store_type(UserConfigStoreType::Test(Arc::new(UnusedConfigStore)))
            .build()
            .build_app()
            .await
            .expect("Expecting this to work");
        let request = Request::builder()
            .method("POST")
            .uri("/contextforge-rs/servers/test-vhost/mcp")
            .header("Mcp-Method", "tools/call")
            .header("Mcp-Name", "example")
            .body(Body::empty())
            .expect("Expecting this to work");

        let response = app.oneshot(request).await.expect("Expecting this to work");

        assert_eq!(response.status(), StatusCode::REQUEST_HEADER_FIELDS_TOO_LARGE);
    }
}
