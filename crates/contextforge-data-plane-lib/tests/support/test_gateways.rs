use std::{collections::HashMap, sync::Arc};

use contextforge_data_plane_apis::{
    User,
    user_store::{BackendMCPGateway, ServiceRoute, UserConfig, VirtualHost},
};
use contextforge_data_plane_lib::{
    Config, Gateway, Result, UpstreamConnectionMode, UserConfigStore, UserConfigStoreType,
};
use futures::{FutureExt, future::BoxFuture};
use rmcp::transport::{
    StreamableHttpServerConfig, StreamableHttpService, streamable_http_server::session::local::LocalSessionManager,
};
use rustls::ProtocolVersion;
use tracing::warn;

use crate::support::create_default_config;

use super::{MemoryUserConfigStore, mock_counter};

const MOCK_COUNTER_TOOL_NAMES: &[&str] =
    &["decrement", "echo", "get_session_id", "get_value", "increment", "long_task", "say_hello", "sum"];
const MOCK_COUNTER_PROMPT_NAMES: &[&str] = &["counter_analysis", "example_prompt"];
const MOCK_COUNTER_RESOURCE_TEMPLATE_NAMES: &[&str] = &["filesystem", "memo"];
const MOCK_COUNTER_RESOURCE_TEMPLATE_URIS: &[&str] = &["memo://{id}", "str:////{path}"];
const MOCK_COUNTER_RESOURCE_URIS: &[&str] = &["memo://insights", "str:////Users/to/some/path/"];

pub(crate) struct ListToolsGatewaySettings {
    pub(crate) handle: tokio::task::JoinHandle<Vec<Result<()>>>,
    pub(crate) gateway_url: String,
    pub(crate) expected_tool_names: Vec<String>,
    pub(crate) expected_prompt_names: Vec<String>,
    pub(crate) expected_resource_template_names: Vec<String>,
    pub(crate) expected_resource_template_uris: Vec<String>,
    pub(crate) expected_resource_uris: Vec<String>,
}

/// Gateway config for plaintext-upstream tests, shared by the integration test binaries.
pub(crate) fn plaintext_config(gateway_port: u16) -> Config {
    Config {
        address: Some(format!("127.0.0.1:{gateway_port}").parse().expect("This should work")),
        upstream_connection_mode: Some(UpstreamConnectionMode::PlainTextOrTls),
        ..create_default_config()
    }
}

pub(crate) fn create_ports(ports: usize) -> Vec<u16> {
    let mut selected = Vec::with_capacity(ports);
    while selected.len() < ports {
        let port = openport::pick_random_unused_port().expect("Expecting to find port");
        if !selected.contains(&port) {
            selected.push(port);
        }
    }
    selected
}

pub fn construct_services(backend_name: &str, service_names: &[&str]) -> HashMap<String, ServiceRoute> {
    service_names
        .iter()
        .map(|&name| {
            (name.to_owned(), ServiceRoute { backend_name: backend_name.to_owned(), upstream_name: name.to_owned() })
        })
        .collect()
}

fn create_services_from_ports(ports: &[u16], service_names: &[&str]) -> HashMap<String, ServiceRoute> {
    let mut services = HashMap::new();

    for &port in ports {
        let backend_id = backend_id(port);

        for &service_name in service_names {
            let key = format!("{backend_id}-{service_name}");
            let value = ServiceRoute { backend_name: backend_id.clone(), upstream_name: service_name.to_owned() };

            services.insert(key, value);
        }
    }
    services
}

async fn create_gateway_with_four_counters_and_custom_config(
    user: &str,
    config: Config,
    create_backends: impl Fn(&[u16]) -> HashMap<String, BackendMCPGateway>,
) -> Result<ListToolsGatewaySettings> {
    let mocked_user_config_store = MemoryUserConfigStore::default();

    let config_address = config.address.expect("This must be set");
    let gateway_port = config_address.port();

    let service = StreamableHttpService::new(
        || Ok(mock_counter::Counter::new()),
        LocalSessionManager::default().into(),
        StreamableHttpServerConfig::default(),
    );

    let router = axum::Router::new().route_service("/mcp", service);

    let (gateway_one_ports, servers_one) = create_axum_servers(2, gateway_port, &router).await?;
    let (gateway_two_ports, servers_two) = create_axum_servers(2, gateway_port, &router).await?;

    assert_ne!(gateway_one_ports, gateway_two_ports);

    let gateway_one_backends = create_backends(&gateway_one_ports);
    let gateway_two_backends = create_backends(&gateway_two_ports);

    let mut virtual_host_one_tool_names = create_tool_names(&gateway_one_ports);
    virtual_host_one_tool_names.sort();
    let mut virtual_host_one_prompt_names = create_prompt_names(&gateway_one_ports);
    virtual_host_one_prompt_names.sort();
    let mut virtual_host_one_resource_template_names = create_resource_template_names(&gateway_one_ports);
    virtual_host_one_resource_template_names.sort();
    let mut virtual_host_one_resource_template_uris = create_resource_template_uris(&gateway_one_ports);
    virtual_host_one_resource_template_uris.sort();
    let mut virtual_host_one_resource_uris = create_resource_uris(&gateway_one_ports);
    virtual_host_one_resource_uris.sort();

    let gateway_one_tools = create_services_from_ports(&gateway_one_ports, MOCK_COUNTER_TOOL_NAMES);
    let gateway_one_resources = create_services_from_ports(&gateway_one_ports, MOCK_COUNTER_RESOURCE_URIS);
    let gateway_one_prompts = create_services_from_ports(&gateway_one_ports, MOCK_COUNTER_PROMPT_NAMES);

    let user_key = User::new(user);

    let virtual_host_one_id = uuid::Uuid::new_v4().to_string();
    let virtual_host_two_id = uuid::Uuid::new_v4().to_string();

    let virtual_hosts = HashMap::from([
        (
            virtual_host_one_id.clone(),
            VirtualHost {
                backends: gateway_one_backends,
                tools: gateway_one_tools,
                resources: gateway_one_resources,
                resource_templates: HashMap::new(),
                prompts: gateway_one_prompts,
            },
        ),
        (
            virtual_host_two_id,
            VirtualHost {
                backends: gateway_two_backends,
                tools: HashMap::new(),
                resources: HashMap::new(),
                resource_templates: HashMap::new(),
                prompts: HashMap::new(),
            },
        ),
    ]);

    let user_config = UserConfig { virtual_hosts };

    mocked_user_config_store.set_config(&user_key, &user_config).await.expect("This should work");

    let gateway = Gateway::builder()
        .with_config(config.clone())
        .with_session_manager(Arc::new(LocalSessionManager::default()))
        .with_user_config_store_type(UserConfigStoreType::Test(Arc::new(mocked_user_config_store)))
        .build();

    let gateway = async move {
        let res = gateway.run_gateway().await;
        warn!("Gateway exited with result {res:?}");
        Ok(())
    }
    .boxed();

    let address = config_address;
    let gateway_url = format!("http://{address}/contextforge-rs/servers/{virtual_host_one_id}/mcp");

    let handle =
        tokio::spawn(futures::future::join_all(vec![gateway].into_iter().chain(servers_one).chain(servers_two)));

    Ok(ListToolsGatewaySettings {
        handle,
        gateway_url,
        expected_tool_names: virtual_host_one_tool_names,
        expected_prompt_names: virtual_host_one_prompt_names,
        expected_resource_template_names: virtual_host_one_resource_template_names,
        expected_resource_template_uris: virtual_host_one_resource_template_uris,
        expected_resource_uris: virtual_host_one_resource_uris,
    })
}

pub(crate) async fn create_gateway_with_four_counters(user: &str, config: Config) -> Result<ListToolsGatewaySettings> {
    create_gateway_with_four_counters_and_custom_config(user, config, create_plain_backends).await
}

pub(crate) async fn create_gateway_with_four_legacy_counters(
    user: &str,
    config: Config,
) -> Result<ListToolsGatewaySettings> {
    create_gateway_with_four_counters_and_custom_config(user, config, create_plain_legacy_backends).await
}

pub(crate) async fn create_tls_gateway_with_four_tls_counters(
    user: &str,
    config: Config,
) -> Result<ListToolsGatewaySettings> {
    create_gateway_with_four_counters_and_custom_config(user, config, create_tls_backends).await
}

fn create_backends(
    ports: &[u16],
    with_tls: bool,
    protocol_version: &rmcp::model::ProtocolVersion,
) -> HashMap<String, BackendMCPGateway> {
    ports
        .iter()
        .map(|port| {
            let url = if with_tls {
                format!("https://127.0.0.1:{port}/mcp").parse().expect("This should work")
            } else {
                format!("http://127.0.0.1:{port}/mcp").parse().expect("This should work")
            };

            let backend_id = backend_id(*port);
            (
                backend_id.clone(),
                BackendMCPGateway {
                    name: format!("backend-{port}"),
                    url,
                    mcp_protocol_version: protocol_version.clone(),
                    passthrough_headers: Vec::new(),
                    add_headers: HashMap::default(),
                    remove_headers: Vec::new(),
                    tool_schemas: MOCK_COUNTER_TOOL_NAMES
                        .iter()
                        .map(|name| ((*name).to_owned(), serde_json::Map::new()))
                        .collect(),

                    completion: HashMap::new(),
                },
            )
        })
        .collect()
}

fn create_plain_backends(ports: &[u16]) -> HashMap<String, BackendMCPGateway> {
    create_backends(ports, false, &rmcp::model::ProtocolVersion::V_2026_07_28)
}

fn create_plain_legacy_backends(ports: &[u16]) -> HashMap<String, BackendMCPGateway> {
    create_backends(ports, false, &rmcp::model::ProtocolVersion::V_2025_11_25)
}

fn create_tls_backends(ports: &[u16]) -> HashMap<String, BackendMCPGateway> {
    create_backends(ports, true, &rmcp::model::ProtocolVersion::V_2026_07_28)
}

fn backend_id(port: u16) -> String {
    format!("00000000-0000-0000-0000-{port:012}")
}

fn create_tool_names(ports: &[u16]) -> Vec<String> {
    ports
        .iter()
        .flat_map(|port| {
            let backend_id = backend_id(*port);
            MOCK_COUNTER_TOOL_NAMES.iter().map(move |name| format!("{backend_id}-{name}"))
        })
        .collect()
}

fn create_prompt_names(ports: &[u16]) -> Vec<String> {
    ports
        .iter()
        .flat_map(|port| {
            let backend_id = backend_id(*port);
            MOCK_COUNTER_PROMPT_NAMES.iter().map(move |name| format!("{backend_id}-{name}"))
        })
        .collect()
}

fn create_resource_template_names(ports: &[u16]) -> Vec<String> {
    ports
        .iter()
        .flat_map(|port| {
            let backend_id = backend_id(*port);
            MOCK_COUNTER_RESOURCE_TEMPLATE_NAMES.iter().map(move |name| format!("{backend_id}-{name}"))
        })
        .collect()
}

fn create_resource_template_uris(ports: &[u16]) -> Vec<String> {
    ports
        .iter()
        .flat_map(|port| {
            let backend_id = backend_id(*port);
            MOCK_COUNTER_RESOURCE_TEMPLATE_URIS.iter().map(move |uri| format!("{backend_id}-{uri}"))
        })
        .collect()
}

fn create_resource_uris(ports: &[u16]) -> Vec<String> {
    ports
        .iter()
        .flat_map(|port| {
            let backend_id = backend_id(*port);
            MOCK_COUNTER_RESOURCE_URIS.iter().map(move |uri| format!("{backend_id}-{uri}"))
        })
        .collect()
}

async fn create_axum_servers(
    server_count: usize,
    gateway_port: u16,
    router: &axum::Router,
) -> Result<(Vec<u16>, Vec<BoxFuture<'static, Result<()>>>)> {
    let mut ports = Vec::with_capacity(server_count);
    let mut servers = Vec::with_capacity(server_count);

    while ports.len() < server_count {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
        let port = listener.local_addr()?.port();
        if port == gateway_port {
            continue;
        }

        let router = router.clone();
        ports.push(port);
        servers.push(
            async move {
                axum::serve(listener, router).await?;
                Ok(())
            }
            .boxed(),
        );
    }

    Ok((ports, servers))
}

async fn create_axum_tls_servers(
    server_count: usize,
    gateway_port: u16,
    router: axum::Router,
) -> Result<(Vec<u16>, Vec<BoxFuture<'static, Result<()>>>)> {
    let config = axum_server::tls_rustls::RustlsConfig::from_pem_file(
        "../../assets/contextforgeCA/contextforge-server.cert.pem",
        "../../assets/contextforgeCA/contextforge-server.key.pem",
    )
    .await
    .expect("Expect this to work");
    let mut ports = Vec::with_capacity(server_count);
    let mut servers = Vec::with_capacity(server_count);

    while ports.len() < server_count {
        let listener = std::net::TcpListener::bind("127.0.0.1:0")?;
        let port = listener.local_addr()?.port();
        if port == gateway_port {
            continue;
        }

        listener.set_nonblocking(true)?;
        let server = axum_server::from_tcp_rustls(listener, config.clone())?;
        let router = router.clone();
        ports.push(port);
        servers.push(
            async move {
                server.serve(router.into_make_service()).await?;
                Ok(())
            }
            .boxed(),
        );
    }

    Ok((ports, servers))
}
