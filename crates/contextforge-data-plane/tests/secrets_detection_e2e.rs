// Copyright 2026
// SPDX-License-Identifier: Apache-2.0

#![cfg(feature = "plugins")]

use std::{
    collections::{HashMap, HashSet},
    fs,
    net::TcpStream as StdTcpStream,
    path::PathBuf,
    process::{Child, Command, Stdio},
    sync::{Arc, Mutex},
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

use contextforge_data_plane_apis::{
    User,
    runtime_plugin_config::{
        RUNTIME_PLUGIN_CONFIG_KEY, RUNTIME_PLUGIN_CONFIG_VERSION, RuntimePluginName, RuntimeRevision,
    },
    user_store::{BackendMCPGateway, PluginBindings, UserConfig, VirtualHost},
};
use http::{HeaderMap, HeaderValue};
use jsonwebtoken::{Algorithm, EncodingKey, Header, encode};
use redis::aio::ConnectionManagerConfig;
use rmcp::{
    ErrorData, RoleClient, RoleServer, ServerHandler, ServiceExt,
    model::{
        CallToolRequestParams, CallToolResponse, CallToolResult, ClientCapabilities, ContentBlock, ErrorCode,
        Implementation, InitializeRequestParams, InitializeResult, ServerCapabilities,
    },
    service::{RequestContext, ServiceError},
    transport::{
        StreamableHttpClientTransport, StreamableHttpServerConfig, StreamableHttpService,
        streamable_http_client::StreamableHttpClientTransportConfig,
        streamable_http_server::session::local::LocalSessionManager,
    },
};

use serde_json::{Map, Value, json};
use tokio::net::TcpListener;

const TEST_USER_ID: &str = "11111111-1111-1111-1111-111111111111";
const TEST_USER_EMAIL: &str = "admin@example.com";
const TEST_VIRTUAL_HOST_ID: &str = "vh-secrets-e2e";
const TEST_TOKEN_TTL_SECS: u64 = 60 * 60;
const REDACTED: &str = "[redacted]";

#[derive(Clone, Debug)]
struct BackendObservation {
    args: Option<Map<String, Value>>,
}

#[derive(Clone, Default)]
struct BackendState {
    calls: Arc<Mutex<Vec<BackendObservation>>>,
}

#[derive(Clone)]
struct TestBackend {
    state: BackendState,
}

#[allow(clippy::unused_async_trait_impl)]
impl ServerHandler for TestBackend {
    async fn initialize(
        &self,
        _request: InitializeRequestParams,
        _cx: RequestContext<RoleServer>,
    ) -> Result<InitializeResult, ErrorData> {
        Ok(InitializeResult::new(ServerCapabilities::builder().enable_tools().build())
            .with_server_info(Implementation::new("secrets-e2e-backend", "0.1.0")))
    }

    async fn call_tool(
        &self,
        request: CallToolRequestParams,
        _cx: RequestContext<RoleServer>,
    ) -> Result<CallToolResponse, ErrorData> {
        self.state
            .calls
            .lock()
            .expect("backend calls lock poisoned")
            .push(BackendObservation { args: request.arguments.clone() });

        let result = match request.name.as_ref() {
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
            "reflect_text" => {
                let text = request
                    .arguments
                    .as_ref()
                    .and_then(|args| args.get("text"))
                    .and_then(Value::as_str)
                    .ok_or_else(|| ErrorData::invalid_params("reflect_text requires text", None))?;
                Ok(CallToolResult::success(vec![ContentBlock::text(text.to_owned())]))
            },
            _ => Err(ErrorData {
                code: ErrorCode::METHOD_NOT_FOUND,
                message: format!("unknown tool {}", request.name).into(),
                data: None,
            }),
        }?;

        Ok(result.into())
    }
}

struct ChildProcess {
    name: &'static str,
    child: Child,
    temp_dir: Option<PathBuf>,
    port: Option<u16>,
}

impl ChildProcess {
    fn new(name: &'static str, child: Child) -> Self {
        Self { name, child, temp_dir: None, port: None }
    }

    fn with_temp_dir(mut self, temp_dir: PathBuf) -> Self {
        self.temp_dir = Some(temp_dir);
        self
    }

    fn with_port(mut self, port: u16) -> Self {
        self.port = Some(port);
        self
    }

    fn port(&self) -> u16 {
        self.port.expect("child process records a port")
    }
}

impl Drop for ChildProcess {
    fn drop(&mut self) {
        if self.child.try_wait().ok().flatten().is_none() {
            let _ = self.child.kill();
        }
        let _ = self.child.wait();
        if let Some(temp_dir) = &self.temp_dir {
            let _ = fs::remove_dir_all(temp_dir);
        }
    }
}

struct RunningBackend {
    url: String,
    state: BackendState,
    handle: tokio::task::JoinHandle<()>,
}

impl Drop for RunningBackend {
    fn drop(&mut self) {
        self.handle.abort();
    }
}

struct E2eEnvironment {
    gateway_url: String,
    backend: RunningBackend,
    _redis: ChildProcess,
    _gateway: ChildProcess,
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "spawns redis-server and the contextforge-data-plane binary"]
async fn binary_e2e_redacts_tool_arguments_and_results() {
    let backend = start_backend().await;
    let env = start_environment(
        backend,
        json!({
            "redact": true,
            "redaction_text": REDACTED,
            "block_on_detection": false,
        }),
    )
    .await;
    let service = connect_client(&env.gateway_url).await;

    let result = service
        .call_tool(sum_request_with_secret("credential", fake_aws_access_key("1111111111111111")))
        .await
        .expect("secret argument is redacted and call succeeds");

    assert_eq!("3", tool_text(&result));
    {
        let backend_calls = env.backend.state.calls.lock().expect("backend calls lock poisoned");
        assert_eq!(1, backend_calls.len());
        assert_eq!(
            Some(&Value::from(REDACTED)),
            backend_calls[0].args.as_ref().and_then(|args| args.get("credential"))
        );
    }

    let result = service
        .call_tool(reflect_text_request(fake_aws_access_key("2222222222222222")))
        .await
        .expect("secret result is redacted and call succeeds");

    assert_eq!(REDACTED, tool_text(&result));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "spawns redis-server and the contextforge-data-plane binary"]
async fn binary_e2e_blocks_tool_arguments_before_backend_call() {
    let backend = start_backend().await;
    let env = start_environment(
        backend,
        json!({
            "redact": false,
            "block_on_detection": true,
        }),
    )
    .await;
    let service = connect_client(&env.gateway_url).await;

    let error = service
        .call_tool(sum_request_with_secret("credential", fake_aws_access_key("3333333333333333")))
        .await
        .expect_err("secret argument blocks the call");

    assert_eq!(ErrorCode::INVALID_REQUEST, mcp_error_code(error));
    assert!(env.backend.state.calls.lock().expect("backend calls lock poisoned").is_empty());
}

async fn start_environment(backend: RunningBackend, plugin_config: Value) -> E2eEnvironment {
    let redis = start_redis().await;
    write_redis_config(redis.port(), &backend).await;
    write_runtime_plugin_config(redis.port(), plugin_config).await;

    let gateway_port = openport::pick_random_unused_port().expect("gateway port");
    let mut gateway = start_gateway_process(gateway_port, redis.port());
    wait_for_port(gateway_port, &mut gateway).await;

    E2eEnvironment {
        gateway_url: format!("http://127.0.0.1:{gateway_port}/contextforge-rs/servers/{TEST_VIRTUAL_HOST_ID}/mcp"),
        backend,
        _redis: redis,
        _gateway: gateway,
    }
}

async fn start_redis() -> ChildProcess {
    let port = openport::pick_random_unused_port().expect("redis port");
    let temp_dir = std::env::temp_dir().join(format!("contextforge-data-plane-redis-{}-{port}", std::process::id()));
    fs::create_dir_all(&temp_dir).expect("redis temp dir is created");

    let child = Command::new("redis-server")
        .args([
            "--port",
            &port.to_string(),
            "--save",
            "",
            "--appendonly",
            "no",
            "--dir",
            temp_dir.to_str().expect("redis temp dir is UTF-8"),
        ])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .expect("redis-server starts; install redis-server to run this ignored E2E test");
    let mut redis = ChildProcess::new("redis-server", child).with_temp_dir(temp_dir).with_port(port);
    wait_for_redis(port, &mut redis).await;
    redis
}

async fn start_backend() -> RunningBackend {
    let listener = TcpListener::bind("127.0.0.1:0").await.expect("backend binds");
    let port = listener.local_addr().expect("backend address").port();
    let state = BackendState::default();
    let backend_state = state.clone();
    let service = StreamableHttpService::new(
        move || Ok(TestBackend { state: backend_state.clone() }),
        LocalSessionManager::default().into(),
        StreamableHttpServerConfig::default(),
    );
    let router = axum::Router::new().route_service("/mcp", service);
    let handle = tokio::spawn(async move {
        axum::serve(listener, router).await.expect("backend serves");
    });
    RunningBackend { url: format!("http://127.0.0.1:{port}/mcp"), state, handle }
}

fn start_gateway_process(gateway_port: u16, redis_port: u16) -> ChildProcess {
    let binary = env!("CARGO_BIN_EXE_contextforge-data-plane");
    let child = Command::new(binary)
        .current_dir(env!("CARGO_MANIFEST_DIR"))
        .args([
            "--address",
            &format!("127.0.0.1:{gateway_port}"),
            "--redis-port",
            &redis_port.to_string(),
            "--redis-address",
            "127.0.0.1",
            "--token-verification-public-key",
            "../../assets/jwt.key.pub",
            "--number-of-cpus",
            "1",
            "--redis-mode",
            "plain-text",
            "--upstream-connection-mode",
            "plain-text-or-tls",
            "--runtime-plugins-enabled",
            "true",
        ])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .expect("contextforge-data-plane binary starts");
    ChildProcess::new("contextforge-data-plane", child)
}

async fn wait_for_redis(port: u16, child: &mut ChildProcess) {
    let client = redis::Client::open(format!("redis://127.0.0.1:{port}/")).expect("redis client opens");
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        assert_child_running(child);
        if let Ok(mut connection) = client.get_connection_manager_with_config(ConnectionManagerConfig::default()).await
            && redis::cmd("PING").query_async::<String>(&mut connection).await.is_ok()
        {
            return;
        }
        assert!(Instant::now() < deadline, "redis-server did not start on port {port}");
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

async fn wait_for_port(port: u16, child: &mut ChildProcess) {
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        assert_child_running(child);
        if StdTcpStream::connect(("127.0.0.1", port)).is_ok() {
            return;
        }
        assert!(Instant::now() < deadline, "{} did not start on port {port}", child.name);
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

fn assert_child_running(child: &mut ChildProcess) {
    match child.child.try_wait() {
        Ok(None) => {},
        Ok(Some(status)) => panic!("{} exited before the E2E test completed: {status}", child.name),
        Err(error) => panic!("failed to inspect {} process: {error}", child.name),
    }
}

async fn write_redis_config(redis_port: u16, backend: &RunningBackend) {
    let client = redis::Client::open(format!("redis://127.0.0.1:{redis_port}/")).expect("redis client opens");
    let mut connection = client
        .get_connection_manager_with_config(ConnectionManagerConfig::default())
        .await
        .expect("redis connection opens");
    redis::cmd("FLUSHDB").query_async::<String>(&mut connection).await.expect("redis flush succeeds");

    let key = rmp_serde::encode::to_vec(&User::new(TEST_USER_ID)).expect("user key encodes");
    let config = UserConfig {
        virtual_hosts: HashMap::from([(
            TEST_VIRTUAL_HOST_ID.to_owned(),
            VirtualHost {
                backends: HashMap::from([(
                    "backend".to_owned(),
                    BackendMCPGateway {
                        name: "backend".to_owned(),
                        url: backend.url.parse().expect("backend URL parses"),
                        mcp_protocol_version: rmcp::model::ProtocolVersion::V_2026_07_28,
                        passthrough_headers: Vec::new(),
                        add_headers: HashMap::new(),
                        remove_headers: Vec::new(),
                        tool_schemas: HashMap::from([
                            ("sum".to_owned(), Map::new()),
                            ("reflect_text".to_owned(), Map::new()),
                        ]),
                        tool_name_aliases: HashSet::new(),
                        resource_uri_aliases: HashSet::new(),
                        prompt_name_aliases: HashSet::new(),
                        completion: HashMap::new(),
                    },
                )]),
                plugin_bindings: PluginBindings {
                    revision: Some(
                        RuntimeRevision::try_from("secrets-detection-v1".to_owned()).expect("valid runtime revision"),
                    ),
                    tools: HashMap::from([(
                        "backend".to_owned(),
                        HashMap::from([
                            (
                                "sum".to_owned(),
                                vec![
                                    RuntimePluginName::try_from("secrets-detection".to_owned())
                                        .expect("valid plugin name"),
                                ],
                            ),
                            (
                                "reflect_text".to_owned(),
                                vec![
                                    RuntimePluginName::try_from("secrets-detection".to_owned())
                                        .expect("valid plugin name"),
                                ],
                            ),
                        ]),
                    )]),
                    resources: HashMap::new(),
                    prompts: HashMap::new(),
                },
            },
        )]),
    };
    let encoded = rmp_serde::encode::to_vec(&config).expect("user config encodes");
    redis::cmd("SET")
        .arg(key)
        .arg(encoded)
        .query_async::<String>(&mut connection)
        .await
        .expect("user config is written");
}

async fn write_runtime_plugin_config(redis_port: u16, plugin_config: Value) {
    let client = redis::Client::open(format!("redis://127.0.0.1:{redis_port}/")).expect("redis client opens");
    let mut connection = client
        .get_connection_manager_with_config(ConnectionManagerConfig::default())
        .await
        .expect("redis connection opens");
    let document = json!({
        "version": RUNTIME_PLUGIN_CONFIG_VERSION,
        "snapshots": {
            "secrets-detection-v1": {
                "plugins": [{
                    "name": "secrets-detection",
                    "kind": "validator/secrets-detection",
                    "hooks": ["cmf.tool_pre_invoke", "cmf.tool_post_invoke"],
                    "config": plugin_config,
                }]
            }
        }
    });
    redis::cmd("SET")
        .arg(RUNTIME_PLUGIN_CONFIG_KEY)
        .arg(serde_json::to_vec(&document).expect("runtime plugin config serializes"))
        .query_async::<String>(&mut connection)
        .await
        .expect("runtime plugin config is written");
}

async fn connect_client(gateway_url: &str) -> rmcp::service::RunningService<RoleClient, InitializeRequestParams> {
    let mut headers = HeaderMap::new();
    headers.insert(
        http::header::AUTHORIZATION,
        HeaderValue::from_str(&format!("Bearer {}", token(TEST_USER_ID))).expect("valid auth header"),
    );
    let client = reqwest::Client::builder().default_headers(headers).build().expect("client builds");
    let transport = StreamableHttpClientTransport::with_client(
        client,
        StreamableHttpClientTransportConfig::with_uri(gateway_url.to_owned()),
    );
    InitializeRequestParams::new(ClientCapabilities::default(), Implementation::new("secrets-e2e-client", "0.1.0"))
        .serve(transport)
        .await
        .expect("client connects to gateway")
}

fn sum_request_with_secret(secret_field: &str, secret: String) -> CallToolRequestParams {
    let mut arguments = Map::from_iter([("a".to_owned(), Value::from(1)), ("b".to_owned(), Value::from(2))]);
    arguments.insert(secret_field.to_owned(), Value::String(secret));
    CallToolRequestParams::new("sum").with_arguments(arguments)
}

fn reflect_text_request(text: String) -> CallToolRequestParams {
    CallToolRequestParams::new("reflect_text")
        .with_arguments(Map::from_iter([("text".to_owned(), Value::String(text))]))
}

fn fake_aws_access_key(suffix: &str) -> String {
    ["AKIA", suffix].concat()
}

fn tool_text(result: &CallToolResult) -> String {
    result.content.iter().filter_map(|content| content.as_text()).map(|text| text.text.as_str()).collect()
}

fn mcp_error_code(error: ServiceError) -> ErrorCode {
    let ServiceError::McpError(error) = error else {
        panic!("expected MCP error, got {error:?}");
    };
    error.code
}

fn token(user_id: &str) -> String {
    let key = EncodingKey::from_rsa_pem(&fs::read("../../assets/jwt.key").expect("jwt key")).expect("encoding key");
    let mut header = Header::new(Algorithm::RS256);
    header.kid = Some("test".to_owned());
    let now = SystemTime::now().duration_since(UNIX_EPOCH).expect("system clock").as_secs();
    let claims = json!({
        "iss": "mcpgateway",
        "sub": user_id,
        "aud": "mcpgateway-api",
        "exp": now + TEST_TOKEN_TTL_SECS,
        "iat": now,
        "jti": "test-token",
        "token_use": "api",
        "teams": ["team_awesome"],
        "user": {
            "email": TEST_USER_EMAIL,
            "full_name": "API Token User",
            "is_admin": true,
            "auth_provider": "api_token"
        },
        "scopes": {
            "server_id": "my_id",
            "permissions": ["tools.read", "servers.use"],
            "ip_restrictions": ["192.169.1.0/24"],
            "time_restrictions": null
        },
    });
    encode(&header, &claims, &key).expect("jwt token")
}
