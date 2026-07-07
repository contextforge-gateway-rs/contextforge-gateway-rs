mod support;

use std::{
    collections::HashSet,
    sync::{Arc, Mutex as StdMutex},
    time::{Duration, Instant},
};

use contextforge_gateway_rs_lib::{Config, Result, UpstreamConnectionMode};
use futures::future::try_join_all;
use rmcp::{
    ClientHandler, ServiceExt,
    model::{
        ClientCapabilities, Implementation, InitializeRequestParams, ResourceUpdatedNotificationParam,
        SubscribeRequestParams, UnsubscribeRequestParams,
    },
    service::{NotificationContext, RoleClient, RunningService},
    transport::{StreamableHttpClientTransport, streamable_http_client::StreamableHttpClientTransportConfig},
};
use tracing::warn;

use support::{
    ListToolsGatewaySettings, connect_client, create_client, create_gateway_with_four_counters, create_ports,
};

const CLIENT_CONNECT_TIMEOUT: Duration = Duration::from_secs(2);
const TEST_POLL_INTERVAL: Duration = Duration::from_millis(20);
const EXPECTED_UPDATES_PER_BACKEND: usize = 4;

type Recorded<T> = Arc<StdMutex<Vec<T>>>;

#[derive(Clone, Default)]
struct RecordingClient {
    resource_updates: Recorded<ResourceUpdatedNotificationParam>,
}

impl ClientHandler for RecordingClient {
    fn get_info(&self) -> InitializeRequestParams {
        InitializeRequestParams::new(
            ClientCapabilities::default(),
            Implementation::new("resource-update-recording-test-client", "0.1.0"),
        )
    }

    async fn on_resource_updated(
        &self,
        params: ResourceUpdatedNotificationParam,
        _context: NotificationContext<RoleClient>,
    ) {
        self.resource_updates.lock().expect("resource update lock poisoned").push(params);
    }
}

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
async fn plaintext_subscribes_and_unsubscribes_through_two_prefixed_backends() -> Result<()> {
    let gateway_port = create_ports(1)[0];
    let user = "admin@example.com";
    let Ok(ListToolsGatewaySettings { handle, gateway_url, .. }) =
        create_gateway_with_four_counters(user, plaintext_config(gateway_port)).await
    else {
        panic!("Invalid configuration ");
    };

    let client = create_client(user);
    let maybe_passed = assert_two_backend_subscribe_roundtrips(gateway_url, client).await;

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

async fn assert_two_backend_subscribe_roundtrips(gateway_url: String, client: reqwest::Client) -> Result<()> {
    let recording_client = RecordingClient::default();
    let resource_updates = Arc::clone(&recording_client.resource_updates);
    let running_service = connect_recording_client(gateway_url, client, recording_client).await?;

    let resources = running_service.list_resources(None).await?;
    let mut selected_backends = HashSet::new();
    let mut selected_uris = Vec::new();

    for resource in resources.resources {
        let backend_name = mock_backend_name(&resource.uri)
            .ok_or_else(|| format!("expected mock backend-prefixed URI, got {}", resource.uri))?;
        if selected_backends.insert(backend_name) {
            selected_uris.push(resource.uri.clone());
        }
        if selected_uris.len() == 2 {
            break;
        }
    }

    if selected_uris.len() != 2 {
        return Err(format!("expected resources from at least two backends, got {}", selected_uris.len()).into());
    }

    try_join_all(selected_uris.iter().map(|uri| running_service.subscribe(SubscribeRequestParams::new(uri.clone()))))
        .await?;
    wait_for_resource_updates(&resource_updates, &selected_uris, EXPECTED_UPDATES_PER_BACKEND).await?;
    for uri in selected_uris {
        running_service.unsubscribe(UnsubscribeRequestParams::new(uri)).await?;
    }

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

async fn connect_recording_client(
    gateway_url: String,
    client: reqwest::Client,
    recording_client: RecordingClient,
) -> Result<RunningService<RoleClient, RecordingClient>> {
    let deadline = Instant::now() + CLIENT_CONNECT_TIMEOUT;
    loop {
        let config = StreamableHttpClientTransportConfig::with_uri(gateway_url.clone());
        let transport = StreamableHttpClientTransport::with_client(client.clone(), config);

        match recording_client.clone().serve(transport).await {
            Ok(running_service) => return Ok(running_service),
            Err(error) if Instant::now() < deadline => {
                warn!("No Service {error:?}");
                tokio::time::sleep(TEST_POLL_INTERVAL).await;
            },
            Err(error) => {
                warn!("No Service {error:?}");
                return Err("Couldn't get a service".into());
            },
        }
    }
}

async fn wait_for_resource_updates(
    resource_updates: &StdMutex<Vec<ResourceUpdatedNotificationParam>>,
    expected_uris: &[String],
    expected_count_per_uri: usize,
) -> Result<()> {
    let deadline = Instant::now() + CLIENT_CONNECT_TIMEOUT;

    loop {
        let counts = {
            let updates = resource_updates.lock().expect("resource update lock poisoned");
            expected_uris
                .iter()
                .map(|uri| {
                    let count = updates.iter().filter(|update| update.uri == *uri).count();
                    (uri.clone(), count)
                })
                .collect::<Vec<_>>()
        };

        if counts.iter().all(|(_, count)| *count >= expected_count_per_uri) {
            return Ok(());
        }
        if Instant::now() >= deadline {
            return Err(
                format!("expected {expected_count_per_uri} resource updates per URI, got counts {counts:?}").into()
            );
        }

        tokio::time::sleep(TEST_POLL_INTERVAL).await;
    }
}

fn mock_backend_name(uri: &str) -> Option<String> {
    uri.strip_suffix("-str:////Users/to/some/path/").or_else(|| uri.strip_suffix("-memo://insights")).map(str::to_owned)
}
