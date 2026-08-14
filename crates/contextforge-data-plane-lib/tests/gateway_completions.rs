mod support;

use contextforge_data_plane_lib::Result;
use tracing::info;

use support::{
    ListToolsGatewaySettings, TEST_USER_ID, connect_client, create_client, create_gateway_with_four_counters,
    create_ports, plaintext_config,
};

#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
#[test_log::test]
#[ignore = "2026-07-28 protocol transition"]
async fn plaintext_completes_prompt_argument_through_prefixed_backend() -> Result<()> {
    let gateway_port = create_ports(1)[0];
    let user = TEST_USER_ID;
    let Ok(ListToolsGatewaySettings { handle, gateway_url, .. }) =
        create_gateway_with_four_counters(user, plaintext_config(gateway_port)).await
    else {
        panic!("Invalid configuration ");
    };

    let client = create_client(user);
    let maybe_passed = assert_prompt_completion(gateway_url, client).await;

    handle.abort();
    maybe_passed
}

#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
#[test_log::test]
#[ignore = "2026-07-28 protocol transition"]
async fn plaintext_completes_resource_argument_through_prefixed_backend() -> Result<()> {
    let gateway_port = create_ports(1)[0];
    let user = TEST_USER_ID;
    let Ok(ListToolsGatewaySettings { handle, gateway_url, .. }) =
        create_gateway_with_four_counters(user, plaintext_config(gateway_port)).await
    else {
        panic!("Invalid configuration ");
    };

    let client = create_client(user);
    let maybe_passed = assert_resource_completion(gateway_url, client).await;

    handle.abort();
    maybe_passed
}

#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
#[test_log::test]
async fn plaintext_complete_for_unrouted_reference_errors() -> Result<()> {
    let gateway_port = create_ports(1)[0];
    let user = TEST_USER_ID;
    let Ok(ListToolsGatewaySettings { handle, gateway_url, .. }) =
        create_gateway_with_four_counters(user, plaintext_config(gateway_port)).await
    else {
        panic!("Invalid configuration ");
    };

    let client = create_client(user);
    let maybe_passed = assert_unrouted_completion_errors(gateway_url, client).await;

    handle.abort();
    maybe_passed
}

async fn assert_prompt_completion(gateway_url: String, client: reqwest::Client) -> Result<()> {
    info!("Sending request to {gateway_url}");
    let running_service = connect_client(gateway_url, client).await?;

    // Spec-compliant clients only issue completion/complete when the server advertises the
    // capability, so the gateway must declare it for the proxying below to be reachable.
    if running_service.peer_info().and_then(|info| info.capabilities.completions.clone()).is_none() {
        return Err("gateway must advertise the completions capability".into());
    }

    let prompts = running_service.list_prompts(None).await?;
    let prompt_name = prompts
        .prompts
        .iter()
        .find(|p| p.name.ends_with("-example_prompt"))
        .map(|p| p.name.clone())
        .ok_or("expected a federated example_prompt")?;

    // The backend only knows the un-prefixed prompt name, so a non-empty result proves the gateway
    // routed to a single backend and stripped the namespace prefix before forwarding.
    let values = running_service.complete_prompt_simple(prompt_name, "message", "h").await?;
    if !values.contains(&"hello".to_owned()) {
        return Err(format!("expected backend prompt completions, got: {values:?}").into());
    }

    Ok(())
}

async fn assert_resource_completion(gateway_url: String, client: reqwest::Client) -> Result<()> {
    let running_service = connect_client(gateway_url, client).await?;

    let resources = running_service.list_resources(None).await?;
    let uri = resources.resources.first().ok_or("expected at least one federated resource")?.uri.clone();

    // The mock echoes back the URI it received; it must be the stripped backend-local URI.
    let values = running_service.complete_resource_simple(uri, "path", "").await?;
    match values.first() {
        Some(value) if !value.starts_with("backend-") => Ok(()),
        other => Err(format!("expected stripped backend URI in completion, got: {other:?}").into()),
    }
}

async fn assert_unrouted_completion_errors(gateway_url: String, client: reqwest::Client) -> Result<()> {
    let running_service = connect_client(gateway_url, client).await?;

    // No backend namespace prefix => no route, so the gateway must reject it.
    let result = running_service.complete_prompt_simple("unrouted_prompt", "message", "h").await;
    if result.is_ok() {
        return Err("expected a routing error for an unrouted completion reference".into());
    }

    Ok(())
}
