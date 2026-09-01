use contextforge_data_plane_lib::Result;
use rmcp::model::{ErrorCode, SubscribeRequestParams};

use crate::harness::{
    TEST_USER_ID, connect_modern_client, create_client, error_parts, modern_client_info, start_counter_gateway,
};

#[tokio::test]
#[expect(deprecated, reason = "legacy RMCP API used to assert the delegated operation response")]
async fn plaintext_subscribe_to_unrouted_resource_errors() -> Result<()> {
    let fixture = start_counter_gateway(TEST_USER_ID).await?;
    let service = connect_modern_client(&fixture.gateway_url, create_client(TEST_USER_ID), modern_client_info()).await;

    let error = service
        .subscribe(SubscribeRequestParams::new("unrouted://resource"))
        .await
        .expect_err("subscriptions are delegated to the control plane");
    let (code, message) = error_parts(error);
    assert_eq!(ErrorCode::METHOD_NOT_FOUND, code);
    assert_eq!("resources/subscribe", message);
    Ok(())
}
