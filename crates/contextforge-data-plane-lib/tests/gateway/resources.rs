use contextforge_data_plane_lib::Result;
use rmcp::model::{ErrorCode, ReadResourceRequestParams, ResourceContents};

use crate::harness::{
    TEST_USER_ID, connect_modern_client, create_client, error_parts, modern_client_info, start_counter_gateway,
};

const MEMO_RESOURCE: &str = "00000000-0000-0000-0000-000000000001-memo://insights";
const EXPECTED_MEMO: &str = "Business Intelligence Memo\n\nAnalysis has revealed 5 key insights ...";

#[tokio::test]
async fn plaintext_call_prefixed_read_resources_modern_modern() -> Result<()> {
    let fixture = start_counter_gateway(TEST_USER_ID).await?;
    let service = connect_modern_client(&fixture.gateway_url, create_client(TEST_USER_ID), modern_client_info()).await;
    let response = service.read_resource(ReadResourceRequestParams::new(MEMO_RESOURCE)).await?;
    let Some(ResourceContents::TextResourceContents { text, .. }) = response.contents.first() else {
        panic!("expected one text resource, got {:?}", response.contents);
    };
    assert_eq!(EXPECTED_MEMO, text);
    Ok(())
}

#[tokio::test]
async fn plaintext_read_invalid_backend_resource() -> Result<()> {
    let fixture = start_counter_gateway(TEST_USER_ID).await?;
    let service = connect_modern_client(&fixture.gateway_url, create_client(TEST_USER_ID), modern_client_info()).await;
    let error = service
        .read_resource(ReadResourceRequestParams::new("http://dummy.dummy"))
        .await
        .expect_err("an unknown resource must fail routing");
    let (code, message) = error_parts(error);
    assert_eq!(ErrorCode::INVALID_PARAMS, code);
    assert_eq!("Routing problem... resource not found", message);
    Ok(())
}
