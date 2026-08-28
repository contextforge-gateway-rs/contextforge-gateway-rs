mod support;

use contextforge_data_plane_lib::{Config, Result, UpstreamConnectionMode};
use rmcp::model::GetPromptRequestParams;
use serde_json::json;
use tracing::{info, warn};

use support::{
    ListToolsGatewaySettings, TEST_USER_ID, connect_client, create_client, create_gateway_with_four_counters,
    create_ports,
};

use crate::support::connect_modern_client;

#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
#[test_log::test]
#[ignore = "Fan out list tools is not supported at the moment. This should be enabled in 2.x"]
async fn plaintext_lists_prefixed_backend_prompts() -> Result<()> {
    let gateway_port = create_ports(1)[0];

    let config = Config {
        address: Some(format!("127.0.0.1:{gateway_port}").parse().expect("This should work")),
        token_verification_public_key: Some("../../assets/jwt.key.pub".into()),
        upstream_connection_mode: Some(UpstreamConnectionMode::PlainTextOrTls),
        ..Default::default()
    };

    let user = TEST_USER_ID;
    let Ok(ListToolsGatewaySettings { handle, gateway_url, expected_prompt_names, .. }) =
        create_gateway_with_four_counters(user, config).await
    else {
        panic!("Invalid configuration ");
    };

    let client = create_client(user);
    let maybe_passed = assert_list_prompts(gateway_url, client, expected_prompt_names).await;

    handle.abort();
    maybe_passed
}

#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
#[test_log::test]
async fn plaintext_gets_prompt_from_prefixed_backend_name() -> Result<()> {
    let gateway_port = create_ports(1)[0];

    let config = Config {
        address: Some(format!("127.0.0.1:{gateway_port}").parse().expect("This should work")),
        token_verification_public_key: Some("../../assets/jwt.key.pub".into()),
        upstream_connection_mode: Some(UpstreamConnectionMode::PlainTextOrTls),
        ..Default::default()
    };

    let user = TEST_USER_ID;
    let Ok(ListToolsGatewaySettings { handle, gateway_url, expected_prompt_names, .. }) =
        create_gateway_with_four_counters(user, config).await
    else {
        panic!("Invalid configuration ");
    };

    let prompt_name = expected_prompt_names
        .iter()
        .find(|name| name.ends_with("-example_prompt"))
        .expect("example prompt is registered")
        .clone();

    let client = create_client(user);
    let maybe_passed = assert_get_prompt(gateway_url, client, prompt_name).await;

    handle.abort();
    maybe_passed
}

async fn assert_list_prompts(
    gateway_url: String,
    client: reqwest::Client,
    expected_prompt_names: Vec<String>,
) -> Result<()> {
    info!("Sending request to {gateway_url}");

    let running_service = connect_client(gateway_url, client).await?;

    let list_prompts = running_service.list_prompts(None).await;
    let Ok(list_prompts) = list_prompts else {
        let msg = format!("List prompts returned error {list_prompts:?}");
        warn!(msg);
        return Err(msg.into());
    };

    let mut names: Vec<String> = list_prompts.prompts.iter().map(|p| p.name.clone()).collect();
    names.sort();

    if expected_prompt_names != names {
        warn!("Actual {names:#?} Expected {expected_prompt_names:#?}");
        return Err("Expected prompt names don't match actual".into());
    }

    Ok(())
}

async fn assert_get_prompt(gateway_url: String, client: reqwest::Client, prompt_name: String) -> Result<()> {
    let running_service = connect_modern_client(&gateway_url, client, support::modern_client_info()).await;
    let mut arguments = serde_json::Map::new();
    arguments.insert("message".to_owned(), json!("hello from gateway"));

    let result = running_service.get_prompt(GetPromptRequestParams::new(prompt_name).with_arguments(arguments)).await?;
    let Some(message) = result.messages.first() else {
        return Err("Expected prompt message".into());
    };
    let Some(text) = message.content.as_text().map(|content| &content.text) else {
        return Err("Expected text prompt message".into());
    };

    if !text.contains("hello from gateway") {
        return Err(format!("Expected routed prompt to include argument, got: {text}").into());
    }

    Ok(())
}
