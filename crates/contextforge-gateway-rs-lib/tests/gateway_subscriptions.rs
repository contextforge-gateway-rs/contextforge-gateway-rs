mod support;

use contextforge_gateway_rs_lib::{Config, Result, UpstreamConnectionMode};
use rmcp::model::{SubscribeRequestParams, UnsubscribeRequestParams};
use tracing::info;

use support::{
    ListToolsGatewaySettings, connect_client, create_client, create_gateway_with_four_counters, create_ports,
};

fn plaintext_config(gateway_port: u16) -> Config {
    Config {
        address: Some(format!("127.0.0.1:{gateway_port}").parse().expect("This should work")),
        token_verification_public_key: Some("../../assets/jwt.key.pub".into()),
        upstream_connection_mode: Some(UpstreamConnectionMode::PlainTextOrTls),
        ..Default::default()
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
#[test_log::test]
async fn plaintext_subscribes_and_unsubscribes_through_prefixed_backend() -> Result<()> {
    let gateway_port = create_ports(1)[0];
    let user = "admin@example.com";
    let Ok(ListToolsGatewaySettings { handle, gateway_url, .. }) =
        create_gateway_with_four_counters(user, plaintext_config(gateway_port)).await
    else {
        panic!("Invalid configuration ");
    };

    let client = create_client(user);
    let maybe_passed = assert_subscribe_roundtrip(gateway_url, client).await;

    handle.abort();
    maybe_passed
}

#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
#[test_log::test]
async fn plaintext_subscribe_to_unrouted_resource_errors() -> Result<()> {
    let gateway_port = create_ports(1)[0];
    let user = "admin@example.com";
    let Ok(ListToolsGatewaySettings { handle, gateway_url, .. }) =
        create_gateway_with_four_counters(user, plaintext_config(gateway_port)).await
    else {
        panic!("Invalid configuration ");
    };

    let client = create_client(user);
    let maybe_passed = assert_unrouted_subscribe_errors(gateway_url, client).await;

    handle.abort();
    maybe_passed
}

async fn assert_subscribe_roundtrip(gateway_url: String, client: reqwest::Client) -> Result<()> {
    info!("Sending request to {gateway_url}");
    let running_service = connect_client(gateway_url, client).await?;

    // Pick a namespaced resource URI the gateway federated from a backend.
    let resources = running_service.list_resources(None).await?;
    let resource = resources.resources.first().ok_or("expected at least one federated resource")?;
    let uri = resource.uri.clone();

    // The mock backend only accepts its own backend-local URIs, so success proves the gateway
    // routed to a single backend and stripped the namespace prefix before forwarding.
    running_service.subscribe(SubscribeRequestParams::new(uri.clone())).await?;
    running_service.unsubscribe(UnsubscribeRequestParams::new(uri)).await?;

    Ok(())
}

async fn assert_unrouted_subscribe_errors(gateway_url: String, client: reqwest::Client) -> Result<()> {
    let running_service = connect_client(gateway_url, client).await?;

    // No backend namespace prefix => no route, so the gateway must reject it.
    let result = running_service.subscribe(SubscribeRequestParams::new("unrouted://resource")).await;
    if result.is_ok() {
        return Err("expected a routing error for an unrouted resource URI".into());
    }

    Ok(())
}
