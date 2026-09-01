use std::collections::HashMap;

use contextforge_data_plane_apis::{
    User,
    user_store::{BackendMCPGateway, ServiceRoute, UserConfig, VirtualHost},
};
use contextforge_data_plane_lib::{Config, Result, UpstreamConnectionMode, UserConfigStore};
use rmcp::transport::{
    StreamableHttpServerConfig, StreamableHttpService, streamable_http_server::session::local::LocalSessionManager,
};

use super::{
    GatewayFixture, GatewayTestConfig, MemoryUserConfigStore, TestServer, create_default_config, mock_counter,
};

const SERVER_CERTIFICATE: &str = "../../assets/contextforgeCA/contextforge-server.cert.pem";
const SERVER_PRIVATE_KEY: &str = "../../assets/contextforgeCA/contextforge-server.key.pem"; // pragma: allowlist secret
const UPSTREAM_TRUST_BUNDLE: &str = "../../assets/contextforgeCA/contextforge.intermediate.ca-chain.cert.pem";
const VIRTUAL_HOST_ID: &str = "11111111-1111-1111-1111-111111111111";

const MOCK_COUNTER_TOOL_NAMES: &[&str] =
    &["decrement", "echo", "get_session_id", "get_value", "increment", "long_task", "say_hello", "sum"];
const MOCK_COUNTER_PROMPT_NAMES: &[&str] = &["counter_analysis", "example_prompt"];
const MOCK_COUNTER_RESOURCE_URIS: &[&str] = &["memo://insights", "str:////Users/to/some/path/"];

#[must_use = "dropping the fixture shuts down the gateway and its backends"]
pub(crate) struct CounterGatewayFixture {
    fixture: GatewayFixture,
    pub(crate) gateway_url: String,
}

impl CounterGatewayFixture {
    pub(crate) async fn shutdown(self) -> Result<()> {
        self.fixture.shutdown().await
    }
}

#[derive(Clone, Copy)]
enum TestTransport {
    Plaintext,
    Tls,
}

pub(crate) async fn start_counter_gateway(user: &str) -> Result<CounterGatewayFixture> {
    start_counter_gateway_with_protocol(user, rmcp::model::ProtocolVersion::V_2026_07_28).await
}

pub(crate) async fn start_legacy_counter_gateway(user: &str) -> Result<CounterGatewayFixture> {
    start_counter_gateway_with_protocol(user, rmcp::model::ProtocolVersion::V_2025_11_25).await
}

pub(crate) async fn start_counter_gateway_with_backends(
    user: &str,
    backend_count: usize,
) -> Result<CounterGatewayFixture> {
    start_counter_gateway_inner(
        user,
        rmcp::model::ProtocolVersion::V_2026_07_28,
        backend_count,
        TestTransport::Plaintext,
    )
    .await
}

pub(crate) async fn start_tls_counter_gateway(user: &str) -> Result<CounterGatewayFixture> {
    start_counter_gateway_inner(user, rmcp::model::ProtocolVersion::V_2026_07_28, 1, TestTransport::Tls).await
}

async fn start_counter_gateway_with_protocol(
    user: &str,
    protocol_version: rmcp::model::ProtocolVersion,
) -> Result<CounterGatewayFixture> {
    start_counter_gateway_inner(user, protocol_version, 1, TestTransport::Plaintext).await
}

async fn start_counter_gateway_inner(
    user: &str,
    protocol_version: rmcp::model::ProtocolVersion,
    backend_count: usize,
    transport: TestTransport,
) -> Result<CounterGatewayFixture> {
    assert!(backend_count > 0, "a gateway fixture needs at least one backend");

    let mut backend_servers = Vec::with_capacity(backend_count);
    let mut backends = HashMap::with_capacity(backend_count);

    for backend_number in 1..=backend_count {
        let service = StreamableHttpService::new(
            || Ok(mock_counter::Counter::new()),
            LocalSessionManager::default().into(),
            StreamableHttpServerConfig::default(),
        );
        let router = axum::Router::new().route_service("/mcp", service);
        let server = match transport {
            TestTransport::Plaintext => TestServer::start_http(router).await?,
            TestTransport::Tls => TestServer::start_tls(router, SERVER_CERTIFICATE, SERVER_PRIVATE_KEY).await?,
        };

        let backend_id = backend_id(backend_number);
        let url = server.url("/mcp").parse().expect("backend URL is valid");
        backends.insert(backend_id.clone(), backend_config(&backend_id, url, protocol_version.clone()));
        backend_servers.push(server);
    }

    let user_store = MemoryUserConfigStore::default();
    let tools = routes(&backends, MOCK_COUNTER_TOOL_NAMES);
    let resources = routes(&backends, MOCK_COUNTER_RESOURCE_URIS);
    let prompts = routes(&backends, MOCK_COUNTER_PROMPT_NAMES);
    user_store
        .set_config(
            &User::new(user),
            &UserConfig {
                virtual_hosts: HashMap::from([(
                    VIRTUAL_HOST_ID.to_owned(),
                    VirtualHost { backends, tools, resources, resource_templates: HashMap::new(), prompts },
                )]),
            },
        )
        .await?;

    let config = Config {
        upstream_connection_mode: Some(UpstreamConnectionMode::PlainTextOrTls),
        upstream_trust_bundle: matches!(transport, TestTransport::Tls).then(|| UPSTREAM_TRUST_BUNDLE.into()),
        server_certificate: matches!(transport, TestTransport::Tls).then(|| SERVER_CERTIFICATE.into()),
        server_private_key: matches!(transport, TestTransport::Tls).then(|| SERVER_PRIVATE_KEY.into()), // pragma: allowlist secret
        ..create_default_config()
    };

    let fixture = match transport {
        TestTransport::Plaintext => {
            GatewayFixture::start(GatewayTestConfig {
                config,
                user_store,
                user_id: user.to_owned(),
                virtual_host_id: VIRTUAL_HOST_ID.to_owned(),
                backends: backend_servers,
                plugin_runtime: None,
            })
            .await?
        },
        TestTransport::Tls => {
            GatewayFixture::start_tls(
                GatewayTestConfig {
                    config,
                    user_store,
                    user_id: user.to_owned(),
                    virtual_host_id: VIRTUAL_HOST_ID.to_owned(),
                    backends: backend_servers,
                    plugin_runtime: None,
                },
                SERVER_CERTIFICATE,
                SERVER_PRIVATE_KEY,
            )
            .await?
        },
    };
    let gateway_url = fixture.gateway_url();

    Ok(CounterGatewayFixture { fixture, gateway_url })
}

fn backend_config(
    backend_id: &str,
    url: url::Url,
    protocol_version: rmcp::model::ProtocolVersion,
) -> BackendMCPGateway {
    BackendMCPGateway {
        name: backend_id.to_owned(),
        url,
        mcp_protocol_version: protocol_version,
        passthrough_headers: Vec::new(),
        add_headers: HashMap::new(),
        remove_headers: Vec::new(),
        tool_schemas: MOCK_COUNTER_TOOL_NAMES.iter().map(|name| ((*name).to_owned(), serde_json::Map::new())).collect(),
        completion: HashMap::new(),
    }
}

pub(crate) fn construct_services(backend_name: &str, service_names: &[&str]) -> HashMap<String, ServiceRoute> {
    service_names
        .iter()
        .map(|&name| {
            (name.to_owned(), ServiceRoute { backend_name: backend_name.to_owned(), upstream_name: name.to_owned() })
        })
        .collect()
}

fn routes(backends: &HashMap<String, BackendMCPGateway>, names: &[&str]) -> HashMap<String, ServiceRoute> {
    backends
        .keys()
        .flat_map(|backend_name| {
            names.iter().map(move |&name| {
                (
                    format!("{backend_name}-{name}"),
                    ServiceRoute { backend_name: backend_name.clone(), upstream_name: name.to_owned() },
                )
            })
        })
        .collect()
}

fn backend_id(backend_number: usize) -> String {
    format!("00000000-0000-0000-0000-{backend_number:012}")
}
