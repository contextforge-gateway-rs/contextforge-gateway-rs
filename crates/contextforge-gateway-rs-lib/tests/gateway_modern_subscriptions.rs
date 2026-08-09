mod support;

use contextforge_gateway_rs_lib::Result;
use rmcp::{
    ClientLifecycleMode, ClientServiceExt,
    model::{ClientInfo, ErrorCode, ProtocolVersion, SubscriptionFilter},
    transport::{StreamableHttpClientTransport, streamable_http_client::StreamableHttpClientTransportConfig},
};
use serde_json::{Value, json};

use support::{
    ListToolsGatewaySettings, TEST_USER_ID, create_client, create_gateway_with_four_counters, create_ports,
    plaintext_config,
};

#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
#[test_log::test]
async fn modern_discover_reports_context_capabilities_and_listen_acknowledges_filter() -> Result<()> {
    let gateway_port = create_ports(1)[0];
    let user = TEST_USER_ID;
    let Ok(ListToolsGatewaySettings { handle, gateway_url, .. }) =
        create_gateway_with_four_counters(user, plaintext_config(gateway_port)).await
    else {
        panic!("invalid test gateway configuration");
    };

    let maybe_passed = assert_modern_discover_and_listen(gateway_url, user).await;

    handle.abort();
    maybe_passed
}

#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
#[test_log::test]
async fn rmcp_gates_legacy_and_modern_subscription_methods() -> Result<()> {
    let gateway_port = create_ports(1)[0];
    let user = TEST_USER_ID;
    let Ok(ListToolsGatewaySettings { handle, gateway_url, .. }) =
        create_gateway_with_four_counters(user, plaintext_config(gateway_port)).await
    else {
        panic!("invalid test gateway configuration");
    };

    let maybe_passed = assert_rmcp_subscription_method_gating(&gateway_url, user).await;

    handle.abort();
    maybe_passed
}

async fn assert_modern_discover_and_listen(gateway_url: String, user: &str) -> Result<()> {
    let client = create_client(user);
    let transport =
        StreamableHttpClientTransport::with_client(client, StreamableHttpClientTransportConfig::with_uri(gateway_url));
    let running_service = ClientInfo::default()
        .serve_with_lifecycle(
            transport,
            ClientLifecycleMode::Discover { preferred_versions: vec![ProtocolVersion::V_2026_07_28] },
        )
        .await?;
    let peer_info = running_service.peer_info().expect("discover lifecycle sets peer info");
    assert_eq!(ProtocolVersion::V_2026_07_28, peer_info.protocol_version);
    assert_eq!(Some(true), peer_info.capabilities.tools.as_ref().and_then(|tools| tools.list_changed));
    assert_eq!(Some(true), peer_info.capabilities.prompts.as_ref().and_then(|prompts| prompts.list_changed));
    let resources = peer_info.capabilities.resources.as_ref().expect("resources are advertised");
    assert_eq!(Some(true), resources.subscribe);
    assert_eq!(Some(true), resources.list_changed);

    let mut subscription = running_service
        .listen(
            SubscriptionFilter::builder()
                .tools_list_changed()
                .resources_list_changed()
                .resource_subscription("unroutable://resource")
                .build(),
        )
        .await?;
    let acknowledged = subscription.acknowledged();
    assert_eq!(Some(true), acknowledged.tools_list_changed);
    assert_eq!(Some(true), acknowledged.resources_list_changed);
    assert_eq!(None, acknowledged.resource_subscriptions);

    subscription.cancel().await?;
    running_service.cancel().await?;
    Ok(())
}

async fn assert_rmcp_subscription_method_gating(gateway_url: &str, user: &str) -> Result<()> {
    let client = create_client(user);

    let modern_subscribe = post_raw_mcp(
        &client,
        gateway_url,
        "2026-07-28",
        json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "resources/subscribe",
            "params": {
                "uri": "memo://known",
                "_meta": request_meta("2026-07-28")
            }
        }),
    )
    .await?;
    assert_jsonrpc_error_code(&modern_subscribe, i64::from(ErrorCode::METHOD_NOT_FOUND.0));

    let legacy_listen = post_raw_mcp(
        &client,
        gateway_url,
        "2025-11-25",
        json!({
            "jsonrpc": "2.0",
            "id": 2,
            "method": "subscriptions/listen",
            "params": {
                "notifications": {
                    "toolsListChanged": true
                },
                "_meta": request_meta("2025-11-25")
            }
        }),
    )
    .await?;
    assert_jsonrpc_error_code(&legacy_listen, i64::from(ErrorCode::METHOD_NOT_FOUND.0));

    Ok(())
}

async fn post_raw_mcp(
    client: &reqwest::Client,
    gateway_url: &str,
    protocol_version: &str,
    body: Value,
) -> Result<Value> {
    let method = body.get("method").and_then(Value::as_str).expect("test request has method");
    let mut request = client
        .post(gateway_url)
        .header(http::header::CONTENT_TYPE, "application/json")
        .header(http::header::ACCEPT, "application/json, text/event-stream")
        .header("MCP-Protocol-Version", protocol_version)
        .header("Mcp-Method", method)
        .json(&body);
    if let Some(name) =
        body.get("params").and_then(|params| params.get("name").or_else(|| params.get("uri"))).and_then(Value::as_str)
    {
        request = request.header("Mcp-Name", name);
    }
    let response = request.send().await?;
    let status = response.status();
    let body = response.text().await?;
    let values = response_values(&body)?;
    assert!(!values.is_empty(), "expected JSON-RPC response body for status {status}, got empty body");
    Ok(values.into_iter().next().expect("non-empty response values"))
}

fn request_meta(protocol_version: &str) -> Value {
    json!({
        "io.modelcontextprotocol/protocolVersion": protocol_version,
        "io.modelcontextprotocol/clientInfo": {
            "name": "modern-subscription-test-client",
            "version": "0.1.0"
        },
        "io.modelcontextprotocol/clientCapabilities": {}
    })
}

fn response_values(body: &str) -> Result<Vec<Value>> {
    let values = body
        .lines()
        .filter_map(|line| line.strip_prefix("data:"))
        .map(str::trim)
        .filter(|data| !data.is_empty())
        .map(serde_json::from_str)
        .collect::<std::result::Result<Vec<_>, _>>()?;
    if !values.is_empty() {
        return Ok(values);
    }

    Ok(vec![serde_json::from_str(body.trim())?])
}

fn assert_jsonrpc_error_code(response: &Value, expected_code: i64) {
    assert_eq!(
        Some(expected_code),
        response.pointer("/error/code").and_then(Value::as_i64),
        "unexpected JSON-RPC response: {response}"
    );
}
