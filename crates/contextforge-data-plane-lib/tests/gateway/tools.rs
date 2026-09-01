use contextforge_data_plane_lib::Result;
use rmcp::model::{CallToolRequestParams, ErrorCode};

use crate::harness::{
    TEST_USER_ID, connect_modern_client, create_client, error_parts, modern_client_info, start_counter_gateway,
};

const DECREMENT_TOOL: &str = "00000000-0000-0000-0000-000000000001-decrement";

#[tokio::test]
async fn plaintext_call_prefixed_backend_tools_modern_modern() -> Result<()> {
    let fixture = start_counter_gateway(TEST_USER_ID).await?;
    let service = connect_modern_client(&fixture.gateway_url, create_client(TEST_USER_ID), modern_client_info()).await;
    let result = service.call_tool(CallToolRequestParams::new(DECREMENT_TOOL)).await?;
    let text = result.content.first().and_then(|content| content.as_text()).expect("text tool result");
    assert_eq!("-1", text.text);
    drop(service);
    fixture.shutdown().await
}

#[tokio::test]
async fn plaintext_call_invalid_backend_tools() -> Result<()> {
    let fixture = start_counter_gateway(TEST_USER_ID).await?;
    let service = connect_modern_client(&fixture.gateway_url, create_client(TEST_USER_ID), modern_client_info()).await;
    let error = service
        .call_tool(CallToolRequestParams::new("dummy_tool"))
        .await
        .expect_err("an unknown tool must fail routing");
    let (code, message) = error_parts(error);
    assert_eq!(ErrorCode::INVALID_PARAMS, code);
    assert_eq!("Routing problem... tool not found", message);
    Ok(())
}
