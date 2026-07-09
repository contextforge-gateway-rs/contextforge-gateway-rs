mod support;

use std::{
    collections::HashSet,
    sync::{Arc, Mutex as StdMutex},
    time::Instant,
};

use contextforge_gateway_rs_lib::Result;
use futures::future::try_join_all;
use rmcp::{
    ClientHandler,
    model::{
        ClientCapabilities, Implementation, InitializeRequestParams, ResourceUpdatedNotificationParam,
        SubscribeRequestParams, UnsubscribeRequestParams,
    },
    service::{NotificationContext, RoleClient},
};

use support::{
    CLIENT_CONNECT_TIMEOUT, ListToolsGatewaySettings, TEST_POLL_INTERVAL, connect_client, connect_client_with_handler,
    create_client, create_gateway_with_four_counters, create_ports,
    mock_counter::{KNOWN_RESOURCE_URIS, RESOURCE_UPDATE_NOTIFY_INTERVAL},
    plaintext_config,
};

/// The mocks notify continuously, so this is just the threshold proving delivery works.
const MIN_UPDATES_PER_BACKEND: usize = 4;

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
    let running_service = connect_client_with_handler(gateway_url, client, recording_client).await?;

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
    wait_for_resource_updates(&resource_updates, &selected_uris, MIN_UPDATES_PER_BACKEND).await?;
    for uri in selected_uris {
        running_service.unsubscribe(UnsubscribeRequestParams::new(uri)).await?;
    }

    assert_no_more_resource_updates(&resource_updates).await
}

/// The mock backends keep notifying after unsubscribe, so any update recorded after the quiet
/// window starts would mean the gateway kept forwarding for an unsubscribed URI.
async fn assert_no_more_resource_updates(
    resource_updates: &StdMutex<Vec<ResourceUpdatedNotificationParam>>,
) -> Result<()> {
    // Let updates the gateway forwarded before the unsubscribe finish arriving.
    tokio::time::sleep(RESOURCE_UPDATE_NOTIFY_INTERVAL * 5).await;
    let count_after_drain = resource_updates.lock().expect("resource update lock poisoned").len();

    tokio::time::sleep(RESOURCE_UPDATE_NOTIFY_INTERVAL * 10).await;
    let count_after_quiet = resource_updates.lock().expect("resource update lock poisoned").len();

    if count_after_quiet != count_after_drain {
        return Err(format!(
            "expected no resource updates after unsubscribe, got {} new",
            count_after_quiet - count_after_drain
        )
        .into());
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

/// Extracts the backend prefix from a namespaced mock resource URI; the suffixes are the
/// backend-local URIs the mock owns, so this stays in lockstep with the mock's list.
fn mock_backend_name(uri: &str) -> Option<String> {
    KNOWN_RESOURCE_URIS
        .iter()
        .find_map(|known| uri.strip_suffix(known))
        .and_then(|prefix| prefix.strip_suffix('-'))
        .map(str::to_owned)
}
