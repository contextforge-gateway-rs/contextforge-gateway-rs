mod support;

use contextforge_data_plane_lib::{Config, Result, UpstreamConnectionMode};
use rmcp::model::ReadResourceRequestParams;
use tracing::{info, warn};

use support::{
    ListToolsGatewaySettings, TEST_USER_ID, connect_client, create_client, create_gateway_with_four_counters,
    create_ports,
};

#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
#[test_log::test]
async fn plaintext_call_prefixed_read_resources() -> Result<()> {
    let gateway_port = create_ports(1)[0];

    let config = Config {
        address: Some(format!("127.0.0.1:{gateway_port}").parse().expect("This should work")),
        token_verification_public_key: Some("../../assets/jwt.key.pub".into()),
        upstream_connection_mode: Some(UpstreamConnectionMode::PlainTextOrTls),
        ..Default::default()
    };

    let user = TEST_USER_ID;

    let Ok(ListToolsGatewaySettings { handle, gateway_url, expected_resource_uris, .. }) =
        create_gateway_with_four_counters(user, config).await
    else {
        panic!("Invalid configuration ");
    };

    let client = create_client(user);

    let call_params = ReadResourceRequestParams::new(expected_resource_uris.first().expect("should work"));

    let maybe_passed = assert_resource_read(
        gateway_url,
        client,
        call_params,
        "Business Intelligence Memo\n\nAnalysis has revealed 5 key insights ...".to_owned(),
    )
    .await;

    handle.abort();
    if maybe_passed.is_ok() {
        info!("Test passed");
    } else {
        info!("Test NOT passed {maybe_passed:?}");
        panic!()
    }

    Ok(())
}

async fn assert_resource_read(
    gateway_url: String,
    client: reqwest::Client,
    params: ReadResourceRequestParams,
    expected_result: String,
) -> Result<()> {
    info!("Seding request to {gateway_url}");

    let running_service = connect_client(gateway_url, client).await?;

    let response = running_service.read_resource(params).await;
    let Ok(response) = response else {
        let msg = format!("Request returned error  {response:?}");
        warn!(msg);
        return Err(msg.into());
    };

    if response.contents.is_empty() {
        let msg = format!("Request returned empty response {response:?}");
        warn!(msg);
        return Err(msg.into());
    }

    if let Some(response) = response.contents.first() {
        if let rmcp::model::ResourceContents::TextResourceContents { text, .. } = response {
            if text != &expected_result {
                let msg = format!("Request returned invalid response {text} {expected_result} {response:?}");
                warn!(msg);
                return Err(msg.into());
            }
        } else {
            let msg = format!("Request returned non text response {response:?}");
            warn!(msg);
            return Err(msg.into());
        }
    } else {
        let msg = format!("Request returned empty response {response:?}");
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

    let call_params = ReadResourceRequestParams::new("http://dummy.dummy");
    let maybe_passed = assert_resource_read(gateway_url, client, call_params, "-1".to_owned()).await;

    handle.abort();
    if maybe_passed.is_ok() {
        info!("Test NOT passed {maybe_passed:?}");
        panic!()
    }
    Ok(())
}
