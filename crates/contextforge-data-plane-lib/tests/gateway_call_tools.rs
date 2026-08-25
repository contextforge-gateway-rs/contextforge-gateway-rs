mod support;

use contextforge_data_plane_lib::{Config, Result, UpstreamConnectionMode};
use rmcp::model::CallToolRequestParams;
use tracing::{info, warn};

use support::{
    ListToolsGatewaySettings, TEST_USER_ID, connect_client, create_client, create_gateway_with_four_counters,
    create_ports,
};

#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
#[test_log::test]
async fn plaintext_call_prefixed_backend_tools() -> Result<()> {
    let gateway_port = create_ports(1)[0];

    let config = Config {
        address: Some(format!("127.0.0.1:{gateway_port}").parse().expect("This should work")),
        token_verification_public_key: Some("../../assets/jwt.key.pub".into()),
        upstream_connection_mode: Some(UpstreamConnectionMode::PlainTextOrTls),
        ..Default::default()
    };

    let user = TEST_USER_ID;

    let Ok(ListToolsGatewaySettings { handle, gateway_url, expected_tool_names, .. }) =
        create_gateway_with_four_counters(user, config).await
    else {
        panic!("Invalid configuration ");
    };

    let client = create_client(user);

    let mut call_params = CallToolRequestParams::default();
    call_params.name = expected_tool_names[0].clone().into();
    let maybe_passed = assert_tools_call(gateway_url, client, call_params, "-1".to_owned()).await;

    handle.abort();
    if maybe_passed.is_ok() {
        info!("Test passed");
    } else {
        info!("Test NOT passed {maybe_passed:?}");
        panic!()
    }

    Ok(())
}

async fn assert_tools_call(
    gateway_url: String,
    client: reqwest::Client,
    call_tool_params: CallToolRequestParams,
    expected_result: String,
) -> Result<()> {
    info!("Seding request to {gateway_url}");

    let running_service = connect_client(gateway_url, client).await?;

    let call_tool = running_service.call_tool(call_tool_params).await;
    let Ok(call_tool) = call_tool else {
        let msg = format!("Call tool returned error  {call_tool:?}");
        warn!(msg);
        return Err(msg.into());
    };

    if call_tool.content.is_empty() {
        let msg = format!("Call tool returned empty response {call_tool:?}");
        warn!(msg);
        return Err(msg.into());
    }

    if let Some(text) = call_tool.content[0].as_text() {
        if text.text != expected_result {
            let msg = format!("Call tool returned unexpected response {} {} {call_tool:?}", text.text, expected_result);
            warn!(msg);
            return Err(msg.into());
        }
    } else {
        let msg = format!("Call tool returned non text response {call_tool:?}");
        warn!(msg);
        return Err(msg.into());
    }

    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
#[test_log::test]
async fn plaintext_call_invalid_backend_tools() -> Result<()> {
    let gateway_port = create_ports(1)[0];

    let config = Config {
        address: Some(format!("127.0.0.1:{gateway_port}").parse().expect("This should work")),
        token_verification_public_key: Some("../../assets/jwt.key.pub".into()),
        upstream_connection_mode: Some(UpstreamConnectionMode::PlainTextOrTls),
        ..Default::default()
    };

    let user = TEST_USER_ID;

    let Ok(ListToolsGatewaySettings { handle, gateway_url, .. }) =
        create_gateway_with_four_counters(user, config).await
    else {
        panic!("Invalid configuration ");
    };

    let client = create_client(user);

    let mut call_params = CallToolRequestParams::default();
    call_params.name = "dummy_tool".into();
    let maybe_passed = assert_tools_call(gateway_url, client, call_params, "-1".to_owned()).await;

    handle.abort();
    if maybe_passed.is_ok() {
        info!("Test NOT passed {maybe_passed:?}");
        panic!()
    }
    Ok(())
}
