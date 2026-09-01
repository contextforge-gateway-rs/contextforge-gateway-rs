use std::{
    collections::HashSet,
    sync::{Arc, Mutex},
    time::Instant,
};

use contextforge_data_plane_lib::Result;
use futures::future::try_join_all;
use rmcp::{
    ClientHandler,
    model::{
        ClientCapabilities, Implementation, InitializeRequestParams, ResourceUpdatedNotificationParam,
        SubscribeRequestParams, UnsubscribeRequestParams,
    },
    service::{NotificationContext, RoleClient},
};

use crate::harness::{
    CLIENT_CONNECT_TIMEOUT, TEST_POLL_INTERVAL, TEST_USER_ID, connect_modern_client, create_client,
    mock_counter::{KNOWN_RESOURCE_URIS, RESOURCE_UPDATE_NOTIFY_INTERVAL},
    start_counter_gateway_with_backends,
};

const MIN_UPDATES_PER_BACKEND: usize = 4;

type Recorded<T> = Arc<Mutex<Vec<T>>>;

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

#[tokio::test]
#[ignore = "blocked on federated resource-subscription support"]
#[expect(deprecated, reason = "subscription APIs remain deferred to the control plane")]
async fn plaintext_subscribes_and_unsubscribes_through_two_prefixed_backends() -> Result<()> {
    let fixture = start_counter_gateway_with_backends(TEST_USER_ID, 2).await?;
    let recording_client = RecordingClient::default();
    let resource_updates = Arc::clone(&recording_client.resource_updates);
    let service = connect_modern_client(&fixture.gateway_url, create_client(TEST_USER_ID), recording_client).await;

    let resources = service.list_resources(None).await?;
    let mut selected_backends = HashSet::new();
    let mut selected_uris = Vec::new();
    for resource in resources.resources {
        let backend_name = mock_backend_name(&resource.uri)
            .ok_or_else(|| format!("expected mock backend-prefixed URI, got {}", resource.uri))?;
        if selected_backends.insert(backend_name) {
            selected_uris.push(resource.uri);
        }
        if selected_uris.len() == 2 {
            break;
        }
    }
    assert_eq!(2, selected_uris.len(), "expected resources from two backends");

    try_join_all(selected_uris.iter().map(|uri| service.subscribe(SubscribeRequestParams::new(uri.clone())))).await?;
    wait_for_resource_updates(&resource_updates, &selected_uris, MIN_UPDATES_PER_BACKEND).await?;
    for uri in selected_uris {
        service.unsubscribe(UnsubscribeRequestParams::new(uri)).await?;
    }
    assert_no_more_resource_updates(&resource_updates).await
}

async fn assert_no_more_resource_updates(
    resource_updates: &Mutex<Vec<ResourceUpdatedNotificationParam>>,
) -> Result<()> {
    tokio::time::sleep(RESOURCE_UPDATE_NOTIFY_INTERVAL * 5).await;
    let count_after_drain = resource_updates.lock().expect("resource update lock poisoned").len();
    tokio::time::sleep(RESOURCE_UPDATE_NOTIFY_INTERVAL * 10).await;
    let count_after_quiet = resource_updates.lock().expect("resource update lock poisoned").len();
    assert_eq!(count_after_drain, count_after_quiet, "updates continued after unsubscribe");
    Ok(())
}

async fn wait_for_resource_updates(
    resource_updates: &Mutex<Vec<ResourceUpdatedNotificationParam>>,
    expected_uris: &[String],
    expected_count_per_uri: usize,
) -> Result<()> {
    let deadline = Instant::now() + CLIENT_CONNECT_TIMEOUT;
    loop {
        let counts = {
            let updates = resource_updates.lock().expect("resource update lock poisoned");
            expected_uris
                .iter()
                .map(|uri| updates.iter().filter(|update| update.uri == *uri).count())
                .collect::<Vec<_>>()
        };
        if counts.iter().all(|count| *count >= expected_count_per_uri) {
            return Ok(());
        }
        if Instant::now() >= deadline {
            return Err(format!("expected {expected_count_per_uri} updates per URI, got {counts:?}").into());
        }
        tokio::time::sleep(TEST_POLL_INTERVAL).await;
    }
}

fn mock_backend_name(uri: &str) -> Option<String> {
    KNOWN_RESOURCE_URIS
        .iter()
        .find_map(|known| uri.strip_suffix(known))
        .and_then(|prefix| prefix.strip_suffix('-'))
        .map(str::to_owned)
}
