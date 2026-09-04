use std::collections::HashMap;

use contextforge_data_plane_apis::{
    User,
    user_store::{BackendMCPGateway, UserConfig, VirtualHost},
};
use contextforge_data_plane_lib::{Config, Result, UpstreamConnectionMode, UserConfigStore};
use rmcp::{
    model::PaginatedRequestParams,
    transport::{
        StreamableHttpServerConfig, StreamableHttpService, streamable_http_server::session::local::LocalSessionManager,
    },
};

use crate::harness::{
    GatewayFixture, GatewayTestConfig, MemoryUserConfigStore, TEST_USER_ID, TestServer, connect_modern_client,
    create_client, create_default_config, modern_client_info, paginating_mock,
};

const VIRTUAL_HOST_ID: &str = "33333333-3333-3333-3333-333333333333";

async fn start_paginating_gateway(backend_count: usize) -> Result<GatewayFixture> {
    let mut backend_servers = Vec::with_capacity(backend_count);
    let mut backends = HashMap::with_capacity(backend_count);

    for backend_number in 1..=backend_count {
        let service = StreamableHttpService::new(
            || Ok(paginating_mock::PaginatingServer),
            LocalSessionManager::default().into(),
            StreamableHttpServerConfig::default(),
        );
        let server = TestServer::start_http(axum::Router::new().route_service("/mcp", service)).await?;
        let backend_id = format!("00000000-0000-0000-0000-{backend_number:012}");
        backends.insert(
            backend_id,
            BackendMCPGateway {
                name: format!("paginating-backend-{backend_number}"),
                url: server.url("/mcp").parse().expect("backend URL"),
                mcp_protocol_version: rmcp::model::ProtocolVersion::V_2026_07_28,
                passthrough_headers: Vec::new(),
                add_headers: HashMap::new(),
                remove_headers: Vec::new(),
                tool_schemas: HashMap::new(),
                completion: HashMap::new(),
            },
        );
        backend_servers.push(server);
    }

    let store = MemoryUserConfigStore::default();
    store
        .set_config(
            &User::new(TEST_USER_ID),
            &UserConfig {
                virtual_hosts: HashMap::from([(
                    VIRTUAL_HOST_ID.to_owned(),
                    VirtualHost {
                        backends,
                        tools: HashMap::new(),
                        resources: HashMap::new(),
                        resource_templates: HashMap::new(),
                        prompts: HashMap::new(),
                    },
                )]),
            },
        )
        .await?;

    GatewayFixture::start(GatewayTestConfig {
        config: Config {
            upstream_connection_mode: Some(UpstreamConnectionMode::PlainTextOrTls),
            ..create_default_config()
        },
        user_store: store,
        user_id: TEST_USER_ID.to_owned(),
        virtual_host_id: VIRTUAL_HOST_ID.to_owned(),
        backends: backend_servers,
        plugin_runtime: None,
    })
    .await
}

#[tokio::test]
#[ignore = "blocked on federated list-tools pagination support"]
async fn single_backend_pagination_all_tools_reachable() -> Result<()> {
    let fixture = start_paginating_gateway(1).await?;
    let service =
        connect_modern_client(&fixture.gateway_url(), create_client(TEST_USER_ID), modern_client_info()).await;

    let page1 = service.list_tools(None).await.expect("page 1");
    let page1_names: Vec<&str> = page1.tools.iter().map(|tool| tool.name.as_ref()).collect();
    assert!(page1.next_cursor.is_some(), "page 1 must carry a next_cursor");
    assert_eq!(page1_names, ["tool_alpha", "tool_beta"]);

    let cursor = page1.next_cursor.map(|cursor| PaginatedRequestParams::default().with_cursor(Some(cursor)));
    let page2 = service.list_tools(cursor).await.expect("page 2");
    let page2_names: Vec<&str> = page2.tools.iter().map(|tool| tool.name.as_ref()).collect();
    assert!(page2.next_cursor.is_none(), "page 2 must be the final page");
    assert_eq!(page2_names, ["tool_gamma"]);

    let mut all_names = page1_names;
    all_names.extend_from_slice(&page2_names);
    all_names.sort_unstable();
    assert_eq!(all_names, paginating_mock::PaginatingServer::all_tool_names());
    Ok(())
}

#[tokio::test]
#[ignore = "blocked on federated list-tools pagination support"]
async fn multi_backend_exhausted_backend_not_requeried() -> Result<()> {
    let fixture = start_paginating_gateway(2).await?;
    let service =
        connect_modern_client(&fixture.gateway_url(), create_client(TEST_USER_ID), modern_client_info()).await;

    let page1 = service.list_tools(None).await.expect("page 1");
    assert!(page1.next_cursor.is_some(), "page 1 must carry a next_cursor");
    assert_eq!(page1.tools.len(), 4, "page 1 should have two tools from each backend");

    let cursor = page1.next_cursor.map(|cursor| PaginatedRequestParams::default().with_cursor(Some(cursor)));
    let page2 = service.list_tools(cursor).await.expect("page 2");
    assert!(page2.next_cursor.is_none(), "page 2 must be the final page");
    assert_eq!(page2.tools.len(), 2, "page 2 should have one tool from each backend");

    let mut all_names: Vec<_> = page1.tools.iter().chain(page2.tools.iter()).map(|tool| tool.name.clone()).collect();
    all_names.sort_unstable();
    all_names.dedup();
    assert_eq!(all_names.len(), page1.tools.len() + page2.tools.len(), "no duplicate tools across pages");
    Ok(())
}
