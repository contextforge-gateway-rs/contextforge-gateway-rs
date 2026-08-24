use std::{
    collections::HashMap,
    sync::{Arc, Mutex as StdMutex, OnceLock},
    time::{Duration, Instant},
};

use contextforge_data_plane_apis::{
    User,
    user_store::{BackendMCPGateway, UserConfig, VirtualHost},
};
use contextforge_data_plane_cpex::CpexRuntimeRegistry;
use contextforge_data_plane_lib::{Config, Gateway, UpstreamConnectionMode, UserConfigStore, UserConfigStoreType};
use futures::FutureExt;
use http::{HeaderMap, HeaderValue, request::Parts};
use rmcp::{
    ErrorData, RoleClient, RoleServer, ServerHandler, ServiceExt,
    model::{
        CallToolRequestParams, CallToolResponse, CallToolResult, ContentBlock, ErrorCode, GetPromptRequestParams,
        GetPromptResponse, GetPromptResult, Implementation, InitializeRequestParams, InitializeResult, NumberOrString,
        ProgressNotificationParam, ProgressToken, PromptMessage, ResourceContents, Role, ServerCapabilities,
    },
    service::{RequestContext, Service},
    transport::{
        StreamableHttpClientTransport, StreamableHttpServerConfig, StreamableHttpService,
        streamable_http_client::StreamableHttpClientTransportConfig,
        streamable_http_server::session::local::LocalSessionManager,
    },
};
use serde_json::{Map, Value, json};
use tokio::sync::Mutex as TokioMutex;

use super::{MemoryUserConfigStore, token};

pub(crate) const BACKEND_PROMPT_RESOURCE: &str = "token=secret";
pub(crate) const BACKEND_PROMPT_IMAGE: &str = "aW1hZ2UtYnl0ZXM=";

static GATEWAY_PORT_LOCK: OnceLock<Arc<TokioMutex<()>>> = OnceLock::new();
const CLIENT_CONNECT_TIMEOUT: Duration = Duration::from_secs(2);
const GATEWAY_PORT_READY_TIMEOUT: Duration = Duration::from_secs(10);
const TEST_POLL_INTERVAL: Duration = Duration::from_millis(20);

#[derive(Clone)]
pub(crate) struct BackendObservation {
    pub(crate) tool_name: String,
    pub(crate) args: Option<Map<String, Value>>,
}

#[derive(Clone, Default)]
pub(crate) struct BackendState {
    pub(crate) calls: Arc<StdMutex<Vec<BackendObservation>>>,
    pub(crate) request_headers: Arc<StdMutex<Vec<HeaderMap>>>,
    pub(crate) prompts: Arc<StdMutex<Vec<BackendObservation>>>,
    pub(crate) cancellations: Arc<StdMutex<Vec<String>>>,
    pub(crate) events: Arc<StdMutex<Vec<&'static str>>>,
    parameter_headers: bool,
}

#[derive(Clone)]
struct TestBackend {
    state: BackendState,
}

fn published_tool_schemas(parameter_headers: bool) -> HashMap<String, Map<String, Value>> {
    let mut schemas =
        ["sum", "reflect_text", "progress_sum", "progress_counter_tokens", "wait_for_cancellation", "missing_tool"]
            .into_iter()
            .map(|name| (name.to_owned(), Map::new()))
            .collect::<HashMap<_, _>>();
    if parameter_headers {
        schemas.insert(
            "sum".to_owned(),
            json!({
                "type": "object",
                "properties": {
                    "a": { "type": "integer", "x-mcp-header": "A" },
                    "b": { "type": "integer", "x-mcp-header": "B" }
                },
                "required": ["a", "b"]
            })
            .as_object()
            .expect("sum schema is an object")
            .clone(),
        );
    }
    schemas
}

#[allow(clippy::unused_async_trait_impl)]
impl ServerHandler for TestBackend {
    async fn initialize(
        &self,
        _request: InitializeRequestParams,
        _cx: RequestContext<RoleServer>,
    ) -> Result<InitializeResult, ErrorData> {
        Ok(InitializeResult::new(ServerCapabilities::builder().enable_tools().enable_prompts().build())
            .with_server_info(Implementation::new("test-backend", "0.1.0")))
    }

    async fn get_prompt(
        &self,
        request: GetPromptRequestParams,
        _cx: RequestContext<RoleServer>,
    ) -> Result<GetPromptResponse, ErrorData> {
        self.state
            .prompts
            .lock()
            .expect("backend prompts lock poisoned")
            .push(BackendObservation { tool_name: request.name.clone(), args: request.arguments.clone() });
        self.state.events.lock().expect("backend events lock poisoned").push("backend");

        let topic = request
            .arguments
            .as_ref()
            .and_then(|arguments| arguments.get("topic"))
            .and_then(Value::as_str)
            .unwrap_or("nothing");
        if request.name == "review_bundle" {
            return Ok(GetPromptResult::new(vec![
                PromptMessage::new_text(Role::User, format!("review of {topic}")),
                PromptMessage::new(
                    Role::User,
                    ContentBlock::resource(ResourceContents::text(BACKEND_PROMPT_RESOURCE, "file:///app.env")),
                ),
                PromptMessage::new(Role::Assistant, ContentBlock::image(BACKEND_PROMPT_IMAGE, "image/png")),
            ])
            .into());
        }

        Ok(GetPromptResult::new(vec![PromptMessage::new_text(Role::User, format!("review of {topic}"))]).into())
    }

    async fn call_tool(
        &self,
        request: CallToolRequestParams,
        cx: RequestContext<RoleServer>,
    ) -> Result<CallToolResponse, ErrorData> {
        if let Some(parts) = cx.extensions.get::<Parts>() {
            self.state
                .request_headers
                .lock()
                .expect("backend request headers lock poisoned")
                .push(parts.headers.clone());
        }
        self.state
            .calls
            .lock()
            .expect("backend calls lock poisoned")
            .push(BackendObservation { tool_name: request.name.to_string(), args: request.arguments.clone() });

        let result: Result<CallToolResult, ErrorData> = match request.name.as_ref() {
            "sum" => {
                let args = request
                    .arguments
                    .as_ref()
                    .ok_or_else(|| ErrorData::invalid_params("sum requires arguments", None))?;
                let a = args
                    .get("a")
                    .and_then(Value::as_i64)
                    .ok_or_else(|| ErrorData::invalid_params("sum requires numeric a", None))?;
                let b = args
                    .get("b")
                    .and_then(Value::as_i64)
                    .ok_or_else(|| ErrorData::invalid_params("sum requires numeric b", None))?;
                Ok(CallToolResult::success(vec![ContentBlock::text((a + b).to_string())]))
            },
            "progress_sum" => {
                if let Some(progress_token) = cx.meta.get_progress_token() {
                    for package in 1..=4 {
                        cx.peer
                            .notify_progress(
                                ProgressNotificationParam::new(progress_token.clone(), f64::from(package))
                                    .with_total(4.0)
                                    .with_message(format!("package {package}/4")),
                            )
                            .await
                            .map_err(|error| {
                                ErrorData::internal_error(format!("progress notification failed: {error}"), None)
                            })?;
                        tokio::time::sleep(Duration::from_millis(10)).await;
                    }
                }
                Ok(CallToolResult::success(vec![ContentBlock::text("completed 4 packages")]))
            },
            "progress_counter_tokens" => {
                for package in 1..=4i32 {
                    cx.peer
                        .notify_progress(
                            ProgressNotificationParam::new(
                                ProgressToken(NumberOrString::String(format!("unexpected-backend-{package}").into())),
                                f64::from(package),
                            )
                            .with_total(4.0)
                            .with_message(format!("package {package}/4")),
                        )
                        .await
                        .map_err(|error| {
                            ErrorData::internal_error(format!("progress notification failed: {error}"), None)
                        })?;
                    tokio::time::sleep(Duration::from_millis(10)).await;
                }
                Ok(CallToolResult::success(vec![ContentBlock::text("completed 4 packages")]))
            },
            "reflect_text" => {
                let text = request
                    .arguments
                    .as_ref()
                    .and_then(|args| args.get("text"))
                    .and_then(Value::as_str)
                    .ok_or_else(|| ErrorData::invalid_params("reflect_text requires text", None))?;
                Ok(CallToolResult::success(vec![ContentBlock::text(text.to_owned())]))
            },
            "wait_for_cancellation" => {
                cx.ct.cancelled().await;
                self.state
                    .cancellations
                    .lock()
                    .expect("backend cancellations lock poisoned")
                    .push(request.name.to_string());
                Ok(CallToolResult::success(vec![ContentBlock::text("cancelled")]))
            },
            _ => Err(ErrorData {
                code: ErrorCode::METHOD_NOT_FOUND,
                message: format!("unknown tool {}", request.name).into(),
                data: None,
            }),
        };
        result.map(Into::into)
    }
}

pub(crate) struct RunningGateway {
    pub(crate) backend_state: BackendState,
    pub(crate) backend_name: String,
    gateway_url: String,
    handle: Option<tokio::task::JoinHandle<Vec<contextforge_data_plane_lib::Result<()>>>>,
}

impl RunningGateway {
    pub(crate) fn gateway_url(&self) -> &str {
        &self.gateway_url
    }

    pub(crate) async fn connect(
        &self,
        user: &str,
    ) -> rmcp::service::RunningService<rmcp::RoleClient, InitializeRequestParams> {
        self.connect_with_handler(user, InitializeRequestParams::default()).await
    }

    pub(crate) async fn connect_with_handler<S>(
        &self,
        user: &str,
        handler: S,
    ) -> rmcp::service::RunningService<RoleClient, S>
    where
        S: Service<RoleClient> + Send + Sync + Clone + 'static,
    {
        let deadline = Instant::now() + CLIENT_CONNECT_TIMEOUT;
        loop {
            let mut headers = HeaderMap::new();
            headers.insert(
                http::header::AUTHORIZATION,
                HeaderValue::from_str(&format!("Bearer {}", token(user))).expect("valid auth header"),
            );
            let client = reqwest::Client::builder().default_headers(headers).build().expect("client builds");
            let transport = StreamableHttpClientTransport::with_client(
                client,
                StreamableHttpClientTransportConfig::with_uri(self.gateway_url.clone()),
            );
            match handler.clone().serve(transport).await {
                Ok(service) => return service,
                Err(error) if Instant::now() < deadline => {
                    let _ = error;
                    tokio::time::sleep(TEST_POLL_INTERVAL).await;
                },
                Err(error) => panic!("gateway service starts: {error:?}"),
            }
        }
    }
}

impl Drop for RunningGateway {
    fn drop(&mut self) {
        if let Some(handle) = self.handle.take() {
            handle.abort();
        }
    }
}

pub(crate) async fn start_gateway(
    user: &str,
    runtime_plugins_enabled: bool,
    plugin_runtime: Arc<CpexRuntimeRegistry>,
) -> RunningGateway {
    start_gateway_with_runtime(user, runtime_plugins_enabled, plugin_runtime, false).await
}

pub(crate) async fn start_gateway_with_parameter_headers(
    user: &str,
    runtime_plugins_enabled: bool,
    plugin_runtime: Arc<CpexRuntimeRegistry>,
) -> RunningGateway {
    start_gateway_with_state(
        user,
        runtime_plugins_enabled,
        plugin_runtime,
        false,
        BackendState { parameter_headers: true, ..Default::default() },
    )
    .await
}

pub(crate) async fn start_gateway_with_events(
    user: &str,
    plugin_runtime: Arc<CpexRuntimeRegistry>,
    events: Arc<StdMutex<Vec<&'static str>>>,
) -> RunningGateway {
    start_gateway_with_state(user, true, plugin_runtime, false, BackendState { events, ..Default::default() }).await
}

pub(crate) async fn start_gateway_with_json_backend_responses(
    user: &str,
    runtime_plugins_enabled: bool,
    plugin_runtime: Arc<CpexRuntimeRegistry>,
) -> RunningGateway {
    start_gateway_with_runtime(user, runtime_plugins_enabled, plugin_runtime, true).await
}

async fn start_gateway_with_runtime(
    user: &str,
    runtime_plugins_enabled: bool,
    plugin_runtime: Arc<CpexRuntimeRegistry>,
    json_backend_responses: bool,
) -> RunningGateway {
    start_gateway_with_state(
        user,
        runtime_plugins_enabled,
        plugin_runtime,
        json_backend_responses,
        BackendState::default(),
    )
    .await
}

async fn start_gateway_with_state(
    user: &str,
    runtime_plugins_enabled: bool,
    plugin_runtime: Arc<CpexRuntimeRegistry>,
    json_backend_responses: bool,
    backend_state: BackendState,
) -> RunningGateway {
    let port_lock = Arc::clone(GATEWAY_PORT_LOCK.get_or_init(|| Arc::new(TokioMutex::new(()))));
    let port_guard = port_lock.lock().await;
    let gateway_port = openport::pick_random_unused_port().expect("gateway port");
    let backend_listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.expect("backend binds");
    let backend_port = backend_listener.local_addr().expect("backend address").port();
    let backend_name = format!("backend-{backend_port}");
    let virtual_host_id = "vh-cpex-test";
    let parameter_headers = backend_state.parameter_headers;

    let backend_service = StreamableHttpService::new(
        {
            let backend_state = backend_state.clone();
            move || Ok(TestBackend { state: backend_state.clone() })
        },
        LocalSessionManager::default().into(),
        StreamableHttpServerConfig::default().with_json_response(json_backend_responses),
    );
    let backend_router = axum::Router::new().route_service("/mcp", backend_service);

    let user_store = MemoryUserConfigStore::default();
    user_store
        .set_config(
            &User::new(user),
            &UserConfig {
                virtual_hosts: HashMap::from([(
                    virtual_host_id.to_owned(),
                    VirtualHost {
                        backends: HashMap::from([(
                            backend_name.clone(),
                            BackendMCPGateway {
                                url: format!("http://127.0.0.1:{backend_port}/mcp").parse().expect("backend URL"),
                                name: String::new(),
                                passthrough_headers: Vec::new(),
                                add_headers: HashMap::default(),
                                remove_headers: Vec::new(),
                                allowed_tool_names: Vec::new(),
                                tool_schemas: published_tool_schemas(parameter_headers),
                                tool_name_aliases: HashMap::new(),
                                allowed_resource_names: Vec::new(),
                                allowed_prompt_names: Vec::new(),
                                resource_name_aliases: HashMap::new(),
                                prompt_name_aliases: HashMap::new(),
                                completion: HashMap::new(),
                            },
                        )]),
                    },
                )]),
            },
        )
        .await
        .expect("user config is stored");

    let gateway = Gateway::builder()
        .with_config(Config {
            address: Some(format!("127.0.0.1:{gateway_port}").parse().expect("This should work")),
            token_verification_public_key: Some("../../assets/jwt.key.pub".into()),
            upstream_connection_mode: Some(UpstreamConnectionMode::PlainTextOrTls),
            runtime_plugins_enabled: Some(runtime_plugins_enabled),
            ..Default::default()
        })
        .with_session_manager(Arc::new(LocalSessionManager::default()))
        .with_user_config_store_type(UserConfigStoreType::Test(Arc::new(user_store)))
        .with_plugin_runtime(runtime_plugins_enabled.then(|| plugin_runtime.handle()))
        .build();

    let gateway = async move { gateway.run_gateway().await }.boxed();
    let backend = async move {
        axum::serve(backend_listener, backend_router).await.expect("backend serves");
        Ok(())
    }
    .boxed();

    let handle = tokio::spawn(futures::future::join_all(vec![gateway, backend]));
    wait_for_gateway_port(gateway_port).await;
    drop(port_guard);

    RunningGateway {
        backend_state,
        backend_name,
        gateway_url: format!("http://127.0.0.1:{gateway_port}/contextforge-rs/servers/{virtual_host_id}/mcp"),
        handle: Some(handle),
    }
}

async fn wait_for_gateway_port(port: u16) {
    let deadline = Instant::now() + GATEWAY_PORT_READY_TIMEOUT;
    loop {
        match tokio::net::TcpStream::connect(("127.0.0.1", port)).await {
            Ok(_) => return,
            Err(error) if Instant::now() < deadline => {
                let _ = error;
                tokio::time::sleep(TEST_POLL_INTERVAL).await;
            },
            Err(error) => panic!("gateway TCP listener starts: {error:?}"),
        }
    }
}
