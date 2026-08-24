mod support;

use contextforge_data_plane_lib::{Config, Result, UpstreamConnectionMode};
use rmcp::model::{ReadResourceRequestParams, ResourceContents};
use tracing::{info, warn};

use support::{
    ListToolsGatewaySettings, TEST_USER_ID, connect_client, create_client, create_gateway_with_four_counters,
    create_ports,
};

#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
#[test_log::test]
async fn plaintext_lists_prefixed_backend_resource_templates() -> Result<()> {
    let gateway_port = create_ports(1)[0];

    let config = Config {
        address: format!("127.0.0.1:{gateway_port}").parse().expect("This should work"),
        token_verification_public_key: Some("../../assets/jwt.key.pub".into()),
        upstream_connection_mode: Some(UpstreamConnectionMode::PlainTextOrTls),
        ..Default::default()
    };

    let user = TEST_USER_ID;
    let Ok(ListToolsGatewaySettings {
        handle,
        gateway_url,
        expected_resource_template_names,
        expected_resource_template_uris,
        ..
    }) = create_gateway_with_four_counters(user, config).await
    else {
        panic!("Invalid configuration ");
    };

    let client = create_client(user);
    let maybe_passed = assert_list_resource_templates(
        gateway_url,
        client,
        expected_resource_template_names,
        expected_resource_template_uris,
    )
    .await;

    handle.abort();
    maybe_passed
}

#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
#[test_log::test]
async fn plaintext_reads_resource_from_prefixed_template() -> Result<()> {
    let gateway_port = create_ports(1)[0];

    let config = Config {
        address: format!("127.0.0.1:{gateway_port}").parse().expect("This should work"),
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
    let maybe_passed = assert_read_resource_from_template(gateway_url, client).await;

    handle.abort();
    maybe_passed
}

async fn assert_list_resource_templates(
    gateway_url: String,
    client: reqwest::Client,
    expected_template_names: Vec<String>,
    expected_template_uris: Vec<String>,
) -> Result<()> {
    info!("Sending request to {gateway_url}");

    let running_service = connect_client(gateway_url, client).await?;

    let list_templates = running_service.list_resource_templates(None).await;
    let Ok(list_templates) = list_templates else {
        let msg = format!("List resource templates returned error {list_templates:?}");
        warn!(msg);
        return Err(msg.into());
    };

    let mut names: Vec<String> = list_templates.resource_templates.iter().map(|t| t.name.clone()).collect();
    names.sort();

    if expected_template_names != names {
        warn!("Actual {names:#?} Expected {expected_template_names:#?}");
        return Err("Expected resource template names don't match actual".into());
    }

    let mut uris: Vec<String> = list_templates.resource_templates.iter().map(|t| t.uri_template.clone()).collect();
    uris.sort();

    if expected_template_uris != uris {
        warn!("Actual {uris:#?} Expected {expected_template_uris:#?}");
        return Err("Expected resource template uris don't match actual".into());
    }

    Ok(())
}

async fn assert_read_resource_from_template(gateway_url: String, client: reqwest::Client) -> Result<()> {
    let running_service = connect_client(gateway_url, client).await?;

    let list_templates = running_service.list_resource_templates(None).await?;
    let Some(memo_template) = list_templates.resource_templates.iter().find(|t| t.uri_template.contains("memo://"))
    else {
        return Err("Expected a memo resource template".into());
    };

    // Expand the namespaced template the way a client would, then read it back through the gateway.
    let uri = memo_template.uri_template.replace("{id}", "insights");
    let result = running_service.read_resource(ReadResourceRequestParams::new(uri)).await?;

    let Some(ResourceContents::TextResourceContents { text, .. }) = result.contents.first() else {
        return Err("Expected text resource contents".into());
    };

    if !text.contains("Business Intelligence Memo") {
        return Err(format!("Expected routed resource to include memo content, got: {text}").into());
    }

    Ok(())
}
