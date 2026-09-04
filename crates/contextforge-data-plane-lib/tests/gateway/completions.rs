use contextforge_data_plane_lib::Result;

use crate::harness::{TEST_USER_ID, connect_modern_client, create_client, modern_client_info, start_counter_gateway};

#[tokio::test]
async fn plaintext_complete_for_unrouted_reference_errors() -> Result<()> {
    let fixture = start_counter_gateway(TEST_USER_ID).await?;
    let service = connect_modern_client(&fixture.gateway_url, create_client(TEST_USER_ID), modern_client_info()).await;

    service
        .complete_prompt_simple("unrouted_prompt", "message", "h")
        .await
        .expect_err("an unrouted completion reference must fail");
    Ok(())
}
