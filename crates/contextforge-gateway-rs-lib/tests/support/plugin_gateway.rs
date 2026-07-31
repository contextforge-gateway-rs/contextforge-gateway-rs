use std::{
    collections::HashMap,
    sync::{Arc, Mutex as StdMutex, OnceLock},
    time::{Duration, Instant},
};

use contextforge_gateway_rs_apis::{
    User,
    user_store::{BackendMCPGateway, Transport, UserConfig, VirtualHost},
};
use contextforge_gateway_rs_cpex::CpexRuntimeRegistry;
use contextforge_gateway_rs_lib::{Config, Gateway, UpstreamConnectionMode, UserConfigStore, UserConfigStoreType};
use futures::FutureExt;
use http::{HeaderMap, HeaderValue};
use rmcp::{
    ErrorData, RoleClient, RoleServer, ServerHandler, ServiceExt,
    model::{
        CallToolRequestParams, CallToolResponse, CallToolResult, CompleteRequestParams, CompleteResult, CompletionInfo,
        ContentBlock, ErrorCode, GetPromptRequestParams, GetPromptResponse, GetPromptResult, Implementation,
        InitializeRequestParams, InitializeResult, ListPromptsResult, ListResourceTemplatesResult, ListResourcesResult,
        NumberOrString, PaginatedRequestParams, ProgressNotificationParam, ProgressToken, Prompt, PromptMessage,
        ReadResourceRequestParams, ReadResourceResponse, ReadResourceResult, Reference, Resource, ResourceContents,
        ResourceTemplate, Role, ServerCapabilities, SubscribeRequestParams, UnsubscribeRequestParams,
    },
    service::{RequestContext, Service},
    transport::{
        StreamableHttpClientTransport, StreamableHttpServerConfig, StreamableHttpService,
        streamable_http_client::StreamableHttpClientTransportConfig,
        streamable_http_server::session::local::LocalSessionManager,
    },
};
use serde_json::{Map, Value};
use tokio::sync::Mutex as TokioMutex;

use super::{MemoryUserConfigStore, token};

static GATEWAY_PORT_LOCK: OnceLock<Arc<TokioMutex<()>>> = OnceLock::new();
const CLIENT_CONNECT_TIMEOUT: Duration = Duration::from_secs(2);
pub(crate) const PROMPT_DESCRIPTION: &str = "Reviews a topic";
pub(crate) const TEMPLATE_URI: &str = "report://{id}/summary";
pub(crate) const TEMPLATE_DESCRIPTION: &str = "A generated report";
pub(crate) const COMPLETION_VALUES: [&str; 2] = ["alpha", "beta"];
pub(crate) const RESOURCE_URI: &str = "report://42/summary";
pub(crate) const RESOURCE_DESCRIPTION: &str = "A stored report";
pub(crate) const RESOURCE_TEXT: &str = "quarterly numbers";
const GATEWAY_PORT_READY_TIMEOUT: Duration = Duration::from_secs(10);
const TEST_POLL_INTERVAL: Duration = Duration::from_millis(20);

#[derive(Clone)]
pub(crate) struct BackendObservation {
    pub(crate) name: String,
    pub(crate) args: Option<Map<String, Value>>,
}

#[derive(Clone, Default)]
pub(crate) struct BackendState {
    pub(crate) calls: Arc<StdMutex<Vec<BackendObservation>>>,
    pub(crate) prompts: Arc<StdMutex<Vec<BackendObservation>>>,
    pub(crate) prompt_lists: Arc<StdMutex<usize>>,
    pub(crate) template_lists: Arc<StdMutex<usize>>,
    pub(crate) completions: Arc<StdMutex<Vec<String>>>,
    pub(crate) reads: Arc<StdMutex<Vec<String>>>,
    pub(crate) resource_lists: Arc<StdMutex<usize>>,
    pub(crate) subscriptions: Arc<StdMutex<Vec<String>>>,
    pub(crate) unsubscriptions: Arc<StdMutex<Vec<String>>>,
    pub(crate) cancellations: Arc<StdMutex<Vec<String>>>,
}

#[derive(Clone)]
struct TestBackend {
    state: BackendState,
}

impl ServerHandler for TestBackend {
    async fn initialize(
        &self,
        _request: InitializeRequestParams,
        _cx: RequestContext<RoleServer>,
    ) -> Result<InitializeResult, ErrorData> {
        Ok(InitializeResult::new(
            ServerCapabilities::builder()
                .enable_tools()
                .enable_prompts()
                .enable_resources()
                .enable_resources_subscribe()
                .build(),
        )
        .with_server_info(Implementation::new("test-backend", "0.1.0")))
    }

    async fn list_prompts(
        &self,
        _request: Option<PaginatedRequestParams>,
        _cx: RequestContext<RoleServer>,
    ) -> Result<ListPromptsResult, ErrorData> {
        *self.state.prompt_lists.lock().expect("backend prompt lists lock poisoned") += 1;
        Ok(ListPromptsResult::with_all_items(vec![Prompt::new("review", Some(PROMPT_DESCRIPTION), None)]))
    }

    async fn subscribe(
        &self,
        request: SubscribeRequestParams,
        _cx: RequestContext<RoleServer>,
    ) -> Result<(), ErrorData> {
        self.state.subscriptions.lock().expect("backend subscriptions lock poisoned").push(request.uri.clone());
        Ok(())
    }

    async fn unsubscribe(
        &self,
        request: UnsubscribeRequestParams,
        _cx: RequestContext<RoleServer>,
    ) -> Result<(), ErrorData> {
        self.state.unsubscriptions.lock().expect("backend unsubscriptions lock poisoned").push(request.uri.clone());
        Ok(())
    }

    async fn list_resources(
        &self,
        _request: Option<PaginatedRequestParams>,
        _cx: RequestContext<RoleServer>,
    ) -> Result<ListResourcesResult, ErrorData> {
        *self.state.resource_lists.lock().expect("backend resource lists lock poisoned") += 1;
        Ok(ListResourcesResult::with_all_items(vec![
            Resource::new(RESOURCE_URI, "report").with_description(RESOURCE_DESCRIPTION),
        ]))
    }

    async fn read_resource(
        &self,
        request: ReadResourceRequestParams,
        _cx: RequestContext<RoleServer>,
    ) -> Result<ReadResourceResponse, ErrorData> {
        self.state.reads.lock().expect("backend reads lock poisoned").push(request.uri.clone());
        Ok(ReadResourceResult::new(vec![ResourceContents::text(RESOURCE_TEXT, request.uri)]).into())
    }

    async fn complete(
        &self,
        request: CompleteRequestParams,
        _cx: RequestContext<RoleServer>,
    ) -> Result<CompleteResult, ErrorData> {
        let identifier = match &request.r#ref {
            Reference::Prompt(prompt) => prompt.name.clone(),
            Reference::Resource(resource) => resource.uri.clone(),
            _ => String::new(),
        };
        self.state.completions.lock().expect("backend completions lock poisoned").push(identifier);
        let completion = CompletionInfo::new(COMPLETION_VALUES.iter().map(|value| (*value).to_owned()).collect())
            .expect("completion values are within the MCP limit");
        Ok(CompleteResult::new(completion))
    }

    async fn list_resource_templates(
        &self,
        _request: Option<PaginatedRequestParams>,
        _cx: RequestContext<RoleServer>,
    ) -> Result<ListResourceTemplatesResult, ErrorData> {
        *self.state.template_lists.lock().expect("backend template lists lock poisoned") += 1;
        Ok(ListResourceTemplatesResult::with_all_items(vec![
            ResourceTemplate::new(TEMPLATE_URI, "report").with_description(TEMPLATE_DESCRIPTION),
        ]))
    }

    /// Renders `review` from its `topic` argument so tests can prove a pre-hook argument rewrite
    /// reached the backend, and prove a post-hook rewrite changed what the client received.
    async fn get_prompt(
        &self,
        request: GetPromptRequestParams,
        _cx: RequestContext<RoleServer>,
    ) -> Result<GetPromptResponse, ErrorData> {
        self.state
            .prompts
            .lock()
            .expect("backend prompts lock poisoned")
            .push(BackendObservation { name: request.name.clone(), args: request.arguments.clone() });

        if request.name != "review" {
            return Err(ErrorData {
                code: ErrorCode::METHOD_NOT_FOUND,
                message: format!("unknown prompt {}", request.name).into(),
                data: None,
            });
        }
        let topic = request
            .arguments
            .as_ref()
            .and_then(|arguments| arguments.get("topic"))
            .and_then(Value::as_str)
            .unwrap_or("nothing");
        Ok(GetPromptResult::new(vec![PromptMessage::new_text(Role::User, format!("review of {topic}"))]).into())
    }

    async fn call_tool(
        &self,
        request: CallToolRequestParams,
        cx: RequestContext<RoleServer>,
    ) -> Result<CallToolResponse, ErrorData> {
        self.state
            .calls
            .lock()
            .expect("backend calls lock poisoned")
            .push(BackendObservation { name: request.name.to_string(), args: request.arguments.clone() });

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
    handle: Option<tokio::task::JoinHandle<Vec<contextforge_gateway_rs_lib::Result<()>>>>,
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
    let port_lock = Arc::clone(GATEWAY_PORT_LOCK.get_or_init(|| Arc::new(TokioMutex::new(()))));
    let port_guard = port_lock.lock().await;
    let gateway_port = openport::pick_random_unused_port().expect("gateway port");
    let backend_listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.expect("backend binds");
    let backend_port = backend_listener.local_addr().expect("backend address").port();
    let backend_name = format!("backend-{backend_port}");
    let virtual_host_id = "vh-cpex-test";
    let backend_state = BackendState::default();

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
                                transport: Transport::default(),
                                passthrough_headers: Vec::new(),
                                add_headers: HashMap::default(),
                                remove_headers: Vec::new(),
                                allowed_tool_names: Vec::new(),
                                tool_name_aliases: HashMap::new(),
                                allowed_resource_names: Vec::new(),
                                allowed_prompt_names: Vec::new(),
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
            address: Some(format!("127.0.0.1:{gateway_port}").parse().expect("gateway address")),
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
