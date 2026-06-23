mod support;

use contextforge_gateway_rs_lib::{Config, Result, UpstreamConnectionMode};
use tracing::{info, warn};

use support::{
    ListToolsGatewaySettings, connect_client, create_client, create_gateway_with_four_counters, create_ports,
};

#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
#[test_log::test]
async fn plaintext_lists_prefixed_backend_resource_templates() -> Result<()> {
    let gateway_port = create_ports(1)[0];

    let config = Config {
        address: Some(format!("127.0.0.1:{gateway_port}").parse().expect("This should work")),
        token_verification_public_key: Some("../../assets/jwt.key.pub".into()),
        upstream_connection_mode: Some(UpstreamConnectionMode::PlainTextOrTls),
        ..Default::default()
    };

    let user = "admin@example.com";
    let Ok(ListToolsGatewaySettings { handle, gateway_url, expected_resource_template_names, .. }) =
        create_gateway_with_four_counters(user, config).await
    else {
        panic!("Invalid configuration ");
    };

    let client = create_client(user);
    let maybe_passed = assert_list_resource_templates(gateway_url, client, expected_resource_template_names).await;

    handle.abort();
    maybe_passed
}

async fn assert_list_resource_templates(
    gateway_url: String,
    client: reqwest::Client,
    expected_template_names: Vec<String>,
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

    if let Some(template) = list_templates.resource_templates.iter().find(|t| !t.uri_template.starts_with("backend-")) {
        return Err(format!("Expected namespaced uri_template, got: {}", template.uri_template).into());
    }

    Ok(())
}
