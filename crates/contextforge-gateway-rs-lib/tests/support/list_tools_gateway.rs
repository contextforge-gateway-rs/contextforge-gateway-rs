use std::{collections::HashMap, net::SocketAddr, sync::Arc};

use contextforge_gateway_rs_apis::{
    User,
    user_store::{BackendMCPGateway, Transport, UserConfig, VirtualHost},
};
use contextforge_gateway_rs_lib::{Config, Gateway, Result, UserConfigStore, UserConfigStoreType};
use futures::{FutureExt, future::BoxFuture};
use rmcp::transport::{
    StreamableHttpServerConfig, StreamableHttpService, streamable_http_server::session::local::LocalSessionManager,
};
use tracing::warn;

use super::{MemoryUserConfigStore, mock_counter};

const MOCK_COUNTER_TOOL_NAMES: &[&str] =
    &["decrement", "echo", "get_session_id", "get_value", "increment", "long_task", "say_hello", "sum"];
const MOCK_COUNTER_PROMPT_NAMES: &[&str] = &["counter_analysis", "example_prompt"];
const MOCK_COUNTER_RESOURCE_TEMPLATE_NAMES: &[&str] = &["filesystem", "memo"];
const MOCK_COUNTER_RESOURCE_TEMPLATE_URIS: &[&str] = &["memo://{id}", "str:////{path}"];

pub(crate) struct ListToolsGatewaySettings {
    pub(crate) handle: tokio::task::JoinHandle<Vec<Result<()>>>,
    pub(crate) gateway_url: String,
    pub(crate) expected_tool_names: Vec<String>,
    pub(crate) expected_prompt_names: Vec<String>,
    pub(crate) expected_resource_template_names: Vec<String>,
    pub(crate) expected_resource_template_uris: Vec<String>,
}

pub(crate) fn create_ports(ports: usize) -> Vec<u16> {
    (0..ports).map(|_| openport::pick_random_unused_port().expect("Expecting to find port")).collect()
}

pub(crate) async fn create_gateway_with_four_counters(user: &str, config: Config) -> Result<ListToolsGatewaySettings> {
    let mocked_user_config_store = MemoryUserConfigStore::default();

    let gateway_one_ports = create_ports(2);
    let gateway_two_ports = create_ports(2);

    let service = StreamableHttpService::new(
        || Ok(mock_counter::Counter::new()),
        LocalSessionManager::default().into(),
        StreamableHttpServerConfig::default(),
    );

    let router = axum::Router::new().route_service("/mcp", service);

    assert_ne!(gateway_one_ports, gateway_two_ports);

    let gateway_one_backends = create_backends(&gateway_one_ports, false);
    let gateway_two_backends = create_backends(&gateway_two_ports, false);

    let mut virtual_host_one_tool_names = create_tool_names(&gateway_one_ports);
    virtual_host_one_tool_names.sort();
    let mut virtual_host_one_prompt_names = create_prompt_names(&gateway_one_ports);
    virtual_host_one_prompt_names.sort();
    let mut virtual_host_one_resource_template_names = create_resource_template_names(&gateway_one_ports);
    virtual_host_one_resource_template_names.sort();
    let mut virtual_host_one_resource_template_uris = create_resource_template_uris(&gateway_one_ports);
    virtual_host_one_resource_template_uris.sort();

    let user_key = User::new(user);

    let virtual_host_one_id = uuid::Uuid::new_v4().to_string();
    let virtual_host_two_id = uuid::Uuid::new_v4().to_string();

    let virtual_hosts = HashMap::from([
        (virtual_host_one_id.clone(), VirtualHost { backends: gateway_one_backends }),
        (virtual_host_two_id, VirtualHost { backends: gateway_two_backends }),
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

    if let Some(address) = config.address.as_ref() {
        let gateway_url = format!("http://{address}/contextforge-rs/servers/{virtual_host_one_id}/mcp");

        let servers_one = create_axum_servers(&gateway_one_ports, &router);
        let servers_two = create_axum_servers(&gateway_two_ports, &router);
        let handle =
            tokio::spawn(futures::future::join_all(vec![gateway].into_iter().chain(servers_one).chain(servers_two)));

        Ok(ListToolsGatewaySettings {
            handle,
            gateway_url,
            expected_tool_names: virtual_host_one_tool_names,
            expected_prompt_names: virtual_host_one_prompt_names,
            expected_resource_template_names: virtual_host_one_resource_template_names,
            expected_resource_template_uris: virtual_host_one_resource_template_uris,
        })
    } else {
        Err("Invalid configuration".into())
    }
}

pub(crate) async fn create_tls_gateway_with_four_tls_counters(
    user: &str,
    config: Config,
) -> Result<ListToolsGatewaySettings> {
    let mocked_user_config_store = MemoryUserConfigStore::default();

    let gateway_one_ports = create_ports(2);
    let gateway_two_ports = create_ports(2);

    let service = StreamableHttpService::new(
        || Ok(mock_counter::Counter::new()),
        LocalSessionManager::default().into(),
        StreamableHttpServerConfig::default().disable_allowed_hosts().disable_allowed_origins(),
    );

    let router = axum::Router::new().route_service("/mcp", service);

    assert_ne!(gateway_one_ports, gateway_two_ports);

    let gateway_one_backends = create_backends(&gateway_one_ports, true);
    let gateway_two_backends = create_backends(&gateway_two_ports, true);

    let mut virtual_host_one_tool_names = create_tool_names(&gateway_one_ports);
    virtual_host_one_tool_names.sort();
    let mut virtual_host_one_prompt_names = create_prompt_names(&gateway_one_ports);
    virtual_host_one_prompt_names.sort();
    let mut virtual_host_one_resource_template_names = create_resource_template_names(&gateway_one_ports);
    virtual_host_one_resource_template_names.sort();
    let mut virtual_host_one_resource_template_uris = create_resource_template_uris(&gateway_one_ports);
    virtual_host_one_resource_template_uris.sort();

    let user_key = User::new(user);

    let virtual_host_one_id = uuid::Uuid::new_v4().to_string();
    let virtual_host_two_id = uuid::Uuid::new_v4().to_string();

    let virtual_hosts = HashMap::from([
        (virtual_host_one_id.clone(), VirtualHost { backends: gateway_one_backends }),
        (virtual_host_two_id, VirtualHost { backends: gateway_two_backends }),
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

    if let Some(address) = config.tls_address.as_ref() {
        let gateway_url = format!("https://{address}/contextforge-rs/servers/{virtual_host_one_id}/mcp");

        let servers_one = create_axum_tls_servers(&gateway_one_ports, router.clone()).await;
        let servers_two = create_axum_tls_servers(&gateway_two_ports, router.clone()).await;
        let handle =
            tokio::spawn(futures::future::join_all(vec![gateway].into_iter().chain(servers_one).chain(servers_two)));

        Ok(ListToolsGatewaySettings {
            handle,
            gateway_url,
            expected_tool_names: virtual_host_one_tool_names,
            expected_prompt_names: virtual_host_one_prompt_names,
            expected_resource_template_names: virtual_host_one_resource_template_names,
            expected_resource_template_uris: virtual_host_one_resource_template_uris,
        })
    } else {
        Err("Invalid configuration".into())
    }
}

fn create_backends(ports: &[u16], with_tls: bool) -> HashMap<String, BackendMCPGateway> {
    ports
        .iter()
        .map(|port| {
            let url = if with_tls {
                format!("https://127.0.0.1:{port}/mcp").parse().expect("This should work")
            } else {
                format!("http://127.0.0.1:{port}/mcp").parse().expect("This should work")
            };

            let name = format!("backend-{port}");
            let allowed_tool_names =
                MOCK_COUNTER_TOOL_NAMES.iter().map(|tool_name| format!("{name}-{tool_name}")).collect();
            (
                name.clone(),
                BackendMCPGateway {
                    name,
                    url,
                    transport: Transport::default(),
                    passthrough_headers: Vec::new(),
                    allowed_tool_names,
                    allowed_resource_names: Vec::new(),
                    allowed_prompt_names: Vec::new(),
                },
            )
        })
        .collect()
}

fn create_tool_names(ports: &[u16]) -> Vec<String> {
    ports
        .iter()
        .flat_map(|port| MOCK_COUNTER_TOOL_NAMES.iter().map(move |name| format!("backend-{port}-{name}")))
        .collect()
}

fn create_prompt_names(ports: &[u16]) -> Vec<String> {
    ports
        .iter()
        .flat_map(|port| MOCK_COUNTER_PROMPT_NAMES.iter().map(move |name| format!("backend-{port}-{name}")))
        .collect()
}

fn create_resource_template_names(ports: &[u16]) -> Vec<String> {
    ports
        .iter()
        .flat_map(|port| MOCK_COUNTER_RESOURCE_TEMPLATE_NAMES.iter().map(move |name| format!("backend-{port}-{name}")))
        .collect()
}

fn create_resource_template_uris(ports: &[u16]) -> Vec<String> {
    ports
        .iter()
        .flat_map(|port| MOCK_COUNTER_RESOURCE_TEMPLATE_URIS.iter().map(move |uri| format!("backend-{port}-{uri}")))
        .collect()
}

fn create_axum_servers(ports: &[u16], router: &axum::Router) -> Vec<BoxFuture<'static, Result<()>>> {
    ports
        .iter()
        .map(|port| {
            let addr = format!("127.0.0.1:{port}");
            let router = router.clone();
            async {
                let listener = tokio::net::TcpListener::bind(addr).await.expect("Expect this to work");
                axum::serve(listener, router).await.expect("server runs");
                Ok(())
            }
            .boxed()
        })
        .collect()
}

async fn create_axum_tls_servers(ports: &[u16], router: axum::Router) -> Vec<BoxFuture<'static, Result<()>>> {
    let config = axum_server::tls_rustls::RustlsConfig::from_pem_file(
        "../../assets/contextforgeCA/contextforge-server.cert.pem",
        "../../assets/contextforgeCA/contextforge-server.key.pem",
    )
    .await
    .expect("Expect this to work");

    ports
        .iter()
        .map(|port| {
            let router = router.clone();
            let addr: SocketAddr = format!("127.0.0.1:{port}").parse().expect("Expect this to work");
            let config = config.clone();
            async move {
                _ = axum_server::bind_rustls(addr, config).serve(router.into_make_service()).await;
                Ok(())
            }
            .boxed()
        })
        .collect()
}
