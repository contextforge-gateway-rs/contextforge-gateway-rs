mod support;

use std::{collections::HashMap, sync::Arc};

use contextforge_data_plane_apis::{
    User,
    user_store::{BackendMCPGateway, UserConfig, VirtualHost},
};
use contextforge_data_plane_lib::{Config, Gateway, Result, UserConfigStore, UserConfigStoreType};
use rmcp::{
    model::PaginatedRequestParams,
    transport::{
        StreamableHttpServerConfig, StreamableHttpService, streamable_http_server::session::local::LocalSessionManager,
    },
};
use tracing::warn;

use support::{
    MemoryUserConfigStore, TEST_USER_ID, connect_client, create_client, create_ports, paginating_mock, plaintext_config,
};

/// Build a single-backend `BackendMCPGateway` pointed at `port`.
fn paginating_backend(port: u16) -> BackendMCPGateway {
    BackendMCPGateway {
        name: format!("backend-{port}"),
        url: format!("http://127.0.0.1:{port}/mcp").parse().expect("valid url"),
        passthrough_headers: Vec::new(),
        add_headers: HashMap::new(),
        remove_headers: Vec::new(),
        allowed_tool_names: Vec::new(),
        tool_name_aliases: HashMap::new(),
        allowed_resource_names: Vec::new(),
        allowed_prompt_names: Vec::new(),
        resource_name_aliases: HashMap::new(),
        prompt_name_aliases: HashMap::new(),
        completion: HashMap::new(),
    }
}

fn backend_id(port: u16) -> String {
    format!("00000000-0000-0000-0000-{port:012}")
}

/// Bind the TCP port for a backend; returns the ready listener.
/// Call this *before* `tokio::spawn` so the port is reserved before the test proceeds.
async fn bind_backend_port(port: u16) -> tokio::net::TcpListener {
    tokio::net::TcpListener::bind(format!("127.0.0.1:{port}")).await.expect("bind backend")
}

/// Start an axum MCP server on an already-bound listener serving a `PaginatingServer`.
async fn serve_paginating_backend(listener: tokio::net::TcpListener) {
    let service = StreamableHttpService::new(
        || Ok(paginating_mock::PaginatingServer),
        LocalSessionManager::default().into(),
        StreamableHttpServerConfig::default(),
    );
    let router = axum::Router::new().route_service("/mcp", service);
    axum::serve(listener, router).await.expect("backend server");
}

/// Boot the gateway with the given config and user config; return the gateway URL.
async fn start_gateway(config: Config, virtual_host_id: &str, user_config: UserConfig) -> String {
    let store = MemoryUserConfigStore::default();
    store.set_config(&User::new(TEST_USER_ID), &user_config).await.expect("set config");

    let address = config.address.expect("This should be set");
    let gateway_url = format!("http://{address}/contextforge-rs/servers/{virtual_host_id}/mcp");

    let gateway = Gateway::builder()
        .with_config(config)
        .with_session_manager(Arc::new(LocalSessionManager::default()))
        .with_user_config_store_type(UserConfigStoreType::Test(Arc::new(store)))
        .build();

    tokio::spawn(async move {
        let res = gateway.run_gateway().await;
        warn!("Gateway exited {res:?}");
    });

    gateway_url
}

/// A paginating backend returns tools across two pages; the gateway must expose
/// all of them to the client without any items being silently dropped.
#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
#[test_log::test]
#[ignore = "Fan out list tools is not supported at the moment. This should be enabled in 2.x"]
async fn single_backend_pagination_all_tools_reachable() -> Result<()> {
    let ports = create_ports(2);
    let (backend_port, gateway_port) = (ports[0], ports[1]);
    let config = plaintext_config(gateway_port);

    let virtual_host_id = "22222222-2222-2222-2222-222222222222";
    let backends = HashMap::from([(backend_id(backend_port), paginating_backend(backend_port))]);
    let user_config =
        UserConfig { virtual_hosts: HashMap::from([(virtual_host_id.to_owned(), VirtualHost { backends })]) };

    let backend_listener = bind_backend_port(backend_port).await;
    tokio::spawn(serve_paginating_backend(backend_listener));
    let gateway_url = start_gateway(config, virtual_host_id, user_config).await;

    let svc = connect_client(gateway_url, create_client(TEST_USER_ID)).await?;

    // Page 1
    let page1 = svc.list_tools(None).await.expect("page 1");
    let page1_names: Vec<&str> = page1.tools.iter().map(|t| t.name.as_ref()).collect();
    assert!(page1.next_cursor.is_some(), "page 1 must carry a next_cursor");
    assert_eq!(page1_names, ["tool_alpha", "tool_beta"]);

    // Page 2
    let cursor = page1.next_cursor.map(|c| PaginatedRequestParams::default().with_cursor(Some(c)));
    let page2 = svc.list_tools(cursor).await.expect("page 2");
    let page2_names: Vec<&str> = page2.tools.iter().map(|t| t.name.as_ref()).collect();
    assert!(page2.next_cursor.is_none(), "page 2 must be the final page");
    assert_eq!(page2_names, ["tool_gamma"]);

    // All tools reachable with no duplication
    let mut all_names = page1_names.clone();
    all_names.extend_from_slice(&page2_names);
    all_names.sort_unstable();
    assert_eq!(all_names, paginating_mock::PaginatingServer::all_tool_names());

    Ok(())
}

/// When one backend exhausts its pages, it must be excluded from the resume
/// request. Without the filter, the exhausted backend would be re-queried and
/// its tools would appear in every subsequent page as duplicates.
#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
#[test_log::test]
#[ignore = "Fan out list tools is not supported at the moment. This should be enabled in 2.x"]
async fn multi_backend_exhausted_backend_not_requeried() -> Result<()> {
    // Backend A: PaginatingServer (2 pages: 2 tools + 1 tool)
    // Backend B: another PaginatingServer (same 2 pages, different backend ID)
    //
    // With 2 backends, tool names get the backend-ID prefix.
    // Page 1: 2 tools from A page 1 + 2 tools from B page 1 = 4 total
    // Page 2: 1 tool from A page 2 + 1 tool from B page 2 = 2 total
    // If either backend were re-queried, its page-1 tools would reappear.
    let ports = create_ports(3);
    let (port_a, port_b, gateway_port) = (ports[0], ports[1], ports[2]);
    let config = plaintext_config(gateway_port);

    let virtual_host_id = "33333333-3333-3333-3333-333333333333";
    let backends = HashMap::from([
        (backend_id(port_a), paginating_backend(port_a)),
        (backend_id(port_b), paginating_backend(port_b)),
    ]);
    let user_config =
        UserConfig { virtual_hosts: HashMap::from([(virtual_host_id.to_owned(), VirtualHost { backends })]) };

    let listener_a = bind_backend_port(port_a).await;
    let listener_b = bind_backend_port(port_b).await;
    tokio::spawn(serve_paginating_backend(listener_a));
    tokio::spawn(serve_paginating_backend(listener_b));
    let gateway_url = start_gateway(config, virtual_host_id, user_config).await;

    let svc = connect_client(gateway_url, create_client(TEST_USER_ID)).await?;

    // Page 1: both backends contribute their first page (2 tools each)
    let page1 = svc.list_tools(None).await.expect("page 1");
    assert!(page1.next_cursor.is_some(), "page 1 must carry a next_cursor");
    assert_eq!(page1.tools.len(), 4, "page 1 should have 2 tools from each backend");

    // Page 2: both backends contribute their second page (1 tool each)
    let cursor = page1.next_cursor.map(|c| PaginatedRequestParams::default().with_cursor(Some(c)));
    let page2 = svc.list_tools(cursor).await.expect("page 2");
    assert!(page2.next_cursor.is_none(), "page 2 must be the final page");
    assert_eq!(page2.tools.len(), 2, "page 2 should have 1 tool from each backend");

    // Union has 6 unique tools, no duplicates
    let mut all_names: Vec<_> = page1.tools.iter().chain(page2.tools.iter()).map(|t| t.name.clone()).collect();
    all_names.sort_unstable();
    all_names.dedup();
    assert_eq!(all_names.len(), page1.tools.len() + page2.tools.len(), "no duplicate tools across pages");

    Ok(())
}
