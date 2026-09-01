use std::sync::{Arc, Mutex};

use contextforge_data_plane_cpex::CpexRuntimeRegistry;
use rmcp::{
    model::{CallToolRequestParams, ClientRequest, Request},
    service::PeerRequestOptions,
};

use crate::harness::{TEST_USER_ID, connect_modern_client, create_client, modern_client_info, start_gateway};

#[tokio::test]
#[ignore = "blocked on downstream cancellation relay for the 2026-07-28 lifecycle"]
async fn downstream_cancellation_is_relayed_to_backend() {
    tokio::time::timeout(std::time::Duration::from_secs(3), assert_downstream_cancellation())
        .await
        .expect("downstream cancellation relay completes within three seconds");
}

async fn assert_downstream_cancellation() {
    let gateway = start_gateway(TEST_USER_ID, true, Arc::new(CpexRuntimeRegistry::default())).await;
    let service = connect_modern_client(gateway.gateway_url(), create_client(TEST_USER_ID), modern_client_info()).await;

    let handle = service
        .send_cancellable_request(
            ClientRequest::CallToolRequest(Request::new(CallToolRequestParams::new("wait_for_cancellation"))),
            PeerRequestOptions::no_options(),
        )
        .await
        .expect("wait_for_cancellation request is sent");
    wait_for_event_count(&gateway.backend_state.calls, 1).await;

    handle.cancel(Some("client gave up".to_owned())).await.expect("cancellation is sent");
    wait_for_event_count(&gateway.backend_state.cancellations, 1).await;
}

async fn wait_for_event_count<T>(events: &Mutex<Vec<T>>, expected: usize) {
    for _ in 0..50 {
        if events.lock().expect("events lock poisoned").len() >= expected {
            return;
        }
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    }
    panic!("expected {expected} recorded events");
}
