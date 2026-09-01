use std::sync::Arc;

use contextforge_data_plane_cpex::CpexRuntimeRegistry;
use contextforge_data_plane_lib::Result;
use rmcp::model::{CallToolRequestParams, ErrorCode, ProtocolVersion, ReadResourceRequestParams, ResourceContents};

use crate::harness::{
    TEST_USER_ID, connect_client_with_protocol, connect_modern_client, create_client, modern_client_info,
    start_counter_gateway, start_gateway, start_legacy_counter_gateway,
};

const DECREMENT_TOOL: &str = "00000000-0000-0000-0000-000000000001-decrement";
const MEMO_RESOURCE: &str = "00000000-0000-0000-0000-000000000001-memo://insights";
const EXPECTED_MEMO: &str = "Business Intelligence Memo\n\nAnalysis has revealed 5 key insights ...";

#[tokio::test]
async fn plaintext_call_prefixed_backend_tools_modern_legacy() -> Result<()> {
    let fixture = start_legacy_counter_gateway(TEST_USER_ID).await?;
    assert_decrement(fixture.gateway_url, ProtocolVersion::V_2026_07_28).await
}

#[tokio::test]
async fn plaintext_call_prefixed_backend_tools_legacy_modern() -> Result<()> {
    let fixture = start_counter_gateway(TEST_USER_ID).await?;
    assert_decrement(fixture.gateway_url, ProtocolVersion::V_2025_11_25).await
}

#[tokio::test]
async fn plaintext_call_prefixed_read_resources_modern_legacy() -> Result<()> {
    let fixture = start_legacy_counter_gateway(TEST_USER_ID).await?;
    assert_memo_read(fixture.gateway_url, ProtocolVersion::V_2026_07_28).await
}

#[tokio::test]
async fn plaintext_call_prefixed_read_resources_legacy_modern() -> Result<()> {
    let fixture = start_counter_gateway(TEST_USER_ID).await?;
    assert_memo_read(fixture.gateway_url, ProtocolVersion::V_2025_11_25).await
}

#[tokio::test]
async fn legacy_tool_call_without_published_schema_reaches_backend() {
    let gateway = start_gateway(TEST_USER_ID, false, Arc::new(CpexRuntimeRegistry::default())).await;
    let service = gateway.connect_legacy(TEST_USER_ID).await;
    let error = service.call_tool(CallToolRequestParams::new("missing_schema_tool")).await.unwrap_err();
    let rmcp::service::ServiceError::McpError(error) = error else {
        panic!("expected backend MCP error, got {error:?}");
    };
    assert_eq!(ErrorCode::METHOD_NOT_FOUND, error.code);
    let backend_calls = gateway.backend_state.calls.lock().expect("backend calls lock poisoned");
    assert_eq!("missing_schema_tool", backend_calls[0].tool_name);
}

async fn assert_decrement(gateway_url: String, client_protocol: ProtocolVersion) -> Result<()> {
    let service = connect_for_protocol(gateway_url, client_protocol).await?;
    let result = service.call_tool(CallToolRequestParams::new(DECREMENT_TOOL)).await?;
    let text = result.content.first().and_then(|content| content.as_text()).expect("text tool result");
    assert_eq!("-1", text.text);
    Ok(())
}

async fn assert_memo_read(gateway_url: String, client_protocol: ProtocolVersion) -> Result<()> {
    let service = connect_for_protocol(gateway_url, client_protocol).await?;
    let response = service.read_resource(ReadResourceRequestParams::new(MEMO_RESOURCE)).await?;
    let Some(ResourceContents::TextResourceContents { text, .. }) = response.contents.first() else {
        panic!("expected one text resource, got {:?}", response.contents);
    };
    assert_eq!(EXPECTED_MEMO, text);
    Ok(())
}

async fn connect_for_protocol(
    gateway_url: String,
    client_protocol: ProtocolVersion,
) -> Result<rmcp::service::RunningService<rmcp::RoleClient, rmcp::model::InitializeRequestParams>> {
    let client = create_client(TEST_USER_ID);
    if client_protocol == ProtocolVersion::V_2026_07_28 {
        Ok(connect_modern_client(&gateway_url, client, modern_client_info()).await)
    } else {
        connect_client_with_protocol(gateway_url, client, client_protocol).await
    }
}
