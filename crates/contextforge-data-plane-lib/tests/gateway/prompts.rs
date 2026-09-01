use contextforge_data_plane_lib::Result;
use rmcp::model::GetPromptRequestParams;
use serde_json::json;

use crate::harness::{TEST_USER_ID, connect_modern_client, create_client, modern_client_info, start_counter_gateway};

const EXAMPLE_PROMPT: &str = "00000000-0000-0000-0000-000000000001-example_prompt";

#[tokio::test]
async fn plaintext_gets_prompt_from_prefixed_backend_name() -> Result<()> {
    let fixture = start_counter_gateway(TEST_USER_ID).await?;
    let service = connect_modern_client(&fixture.gateway_url, create_client(TEST_USER_ID), modern_client_info()).await;
    let mut arguments = serde_json::Map::new();
    arguments.insert("message".to_owned(), json!("hello from gateway"));

    let result = service.get_prompt(GetPromptRequestParams::new(EXAMPLE_PROMPT).with_arguments(arguments)).await?;
    let text = result
        .messages
        .first()
        .and_then(|message| message.content.as_text())
        .map(|content| &content.text)
        .ok_or("expected a text prompt message")?;
    assert!(text.contains("hello from gateway"), "unexpected prompt text: {text}");
    Ok(())
}
