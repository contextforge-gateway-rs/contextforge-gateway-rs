use std::{fs, sync::Arc};

use axum::middleware;
use axum_otel_metrics::HttpMetricsLayerBuilder;
use contextforge_data_plane_cpex::GatewayPluginRuntimeHandle;
use futures::FutureExt;
use http::uri::Authority;
use jsonwebtoken::DecodingKey;
use rmcp::transport::{
    StreamableHttpServerConfig,
    streamable_http_server::{session::local::LocalSessionManager, tower::StreamableHttpService},
};
mod common;
mod const_values;
mod gateway;
mod layers;
mod transports;

#[cfg(feature = "with_tools")]
mod tools;

mod user_config_store;
pub use common::{RedisClient, RedisConfig, UpstreamConnectionMode};
use gateway::{BackendTransports, McpService};
use layers::session_id::SessionId;
use tower_http::cors::{Any, CorsLayer};
use tower_http::trace::TraceLayer;
use transports::{DownstreamTls, Tcp};
use typed_builder::TypedBuilder;
pub use user_config_store::RedisUserConfigStore;
pub use user_config_store::{ConfigStoreError, UserConfigStore};

pub use crate::common::Config;
pub use contextforge_data_plane_observability::{LogRotation, OtlpProtocol};

pub type Error = Box<dyn std::error::Error + Send + Sync + 'static>;
pub type Result<T> = std::result::Result<T, Error>;

use crate::{
    common::{ContextForgeDataPlaneAppState, JwtTokenDecoders},
    gateway::LocalUserSessionStore,
    layers::{
        claims_id::claims_layer,
        mcp_origin::mcp_origin_layer,
        session_id::{SessionIdState, session_id_layer},
        user_config_store::user_config_store_layer,
        virtual_host_config::virtual_host_config_layer,
        virtual_host_id::virtual_host_id_layer,
    },
};

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
}

impl Gateway {
    pub async fn run_gateway(self) -> Result<()> {
        let config = &self.config;
        let session_manager = self.session_manager;
        let user_config_store = match self.user_config_store_type {
            UserConfigStoreType::Redis => Arc::new(get_config_store(config).await?),
            UserConfigStoreType::Test(store) => store,
        };
        let user_config_store = user_config_store as Arc<dyn UserConfigStore + Send + Sync>;

        let user_session_store = LocalUserSessionStore::new();
        let backend_transports = BackendTransports::default();
        let session_id_state = SessionIdState {
            user_session_store: Arc::new(user_session_store.clone()),
            backend_transports: backend_transports.clone(),
        };
        let mcp_plugin_runtime = self.plugin_runtime;

        // mcp_origin_layer is the sole enforcement point; disable RMCP's built-in checks.
        // Pass the host list to RMCP as well when configured (defense-in-depth).
        let streamable_config = if let Some(ref hosts) = config.mcp_allowed_hosts {
            StreamableHttpServerConfig::default()
                .with_allowed_hosts(hosts.iter().map(Authority::as_str))
                .disable_allowed_origins()
        } else {
            StreamableHttpServerConfig::default().disable_allowed_hosts().disable_allowed_origins()
        };

        let reqwest_backend_client = reqwest::Client::try_from(config)?;

        // Create streamable HTTP service
        let mcp_service: StreamableHttpService<McpService<LocalUserSessionStore>, LocalSessionManager> =
            StreamableHttpService::new(
                move || {
                    Ok(McpService::builder()
                        .with_user_session_store(user_session_store.clone())
                        .with_http_client(reqwest_backend_client.clone())
                        .with_transports(backend_transports.clone())
                        .with_plugin_runtime(mcp_plugin_runtime.clone())
                        .build())
                },
                session_manager,
                streamable_config,
            );

        let cors_layer = CorsLayer::new().allow_origin(Any).allow_methods(Any).allow_headers(Any).expose_headers(Any);

        let rs_decoding_key = config.token_verification_public_key.as_ref().map(|path| {
            let Ok(key) =
                fs::read(path).map_err(|e| format!("Error when creating local decoder {e:?} {}", path.display()))
            else {
                return Err(format!("Error when creating local decoder. Can't read path {}", path.display()));
            };

            let Ok(key) = DecodingKey::from_rsa_pem(&key) else {
                return Err(format!("Error when creating local decoder. Can't read the key {}", path.display()));
            };
            Ok(key)
        });

        let mcp_add_state: ContextForgeDataPlaneAppState = ContextForgeDataPlaneAppState {
            jwt_token_decoding_keys: JwtTokenDecoders {
                rs: rs_decoding_key.transpose()?,
                hmac_sha: config
                    .token_verification_secret
                    .as_ref()
                    .map(|token| DecodingKey::from_secret(token.value().as_bytes())),
            },

            config_store: Arc::clone(&user_config_store),
            config: config.clone(),
        };

        let app = axum::Router::new()
            .nest_service("/servers/{virtual_host_name}/mcp", mcp_service)
            .layer(middleware::from_fn(virtual_host_config_layer))
            .layer(middleware::from_fn_with_state(mcp_add_state.clone(), user_config_store_layer))
            .layer(middleware::from_fn_with_state(session_id_state, session_id_layer))
            .layer(middleware::from_fn_with_state(mcp_add_state.clone(), claims_layer))
            .layer(middleware::from_fn(virtual_host_id_layer))
            .layer(cors_layer)
            // mcp_origin_layer is the outermost wrapper: fires before JWT auth,
            // session creation, and backend fan-out.
            .layer(middleware::from_fn_with_state(config.clone(), mcp_origin_layer));

        #[cfg(feature = "with_tools")]
        let app = tools::add_tools(app);

        let app = app.with_state(mcp_add_state);
        let app = axum::Router::new()
            .nest("/contextforge-rs", app)
            .layer(
                TraceLayer::new_for_http()
                    .make_span_with(contextforge_data_plane_observability::ExtractingMakeSpan)
                    .on_response(contextforge_data_plane_observability::LogOnResponse),
            )
            .layer(middleware::from_fn(contextforge_data_plane_observability::correlation_layer))
            .layer(HttpMetricsLayerBuilder::new().build());

        let mut handlers = vec![];

        if let Some(tcp) = Option::<Tcp>::try_from(config)? {
            handlers.push(tcp.handle_tcp(app.clone()).boxed());
        }

        if let Some(tls) = Option::<DownstreamTls>::try_from(config)? {
            handlers.push(tls.handle_tls(app.clone()).boxed());
        }

        let _ = futures::future::join_all(handlers).await;

        Ok(())
    }
}

pub async fn get_config_store(config: &Config) -> Result<RedisUserConfigStore> {
    let redis_config = RedisConfig::try_from(config)?;
    let cache_expiry = std::time::Duration::from_secs(config.user_config_cache_expiry_seconds);
    RedisUserConfigStore::new(&RedisClient::try_from(redis_config)?, cache_expiry).await
}
