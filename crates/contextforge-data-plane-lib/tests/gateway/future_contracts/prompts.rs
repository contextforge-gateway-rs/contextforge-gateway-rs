use contextforge_data_plane_lib::Result;

use crate::harness::{TEST_USER_ID, connect_modern_client, create_client, modern_client_info, start_counter_gateway};

const EXPECTED_PROMPTS: &[&str] =
    &["00000000-0000-0000-0000-000000000001-counter_analysis", "00000000-0000-0000-0000-000000000001-example_prompt"];

#[tokio::test]
#[ignore = "blocked on federated list-prompts support"]
async fn plaintext_lists_prefixed_backend_prompts() -> Result<()> {
    let fixture = start_counter_gateway(TEST_USER_ID).await?;
    let service = connect_modern_client(&fixture.gateway_url, create_client(TEST_USER_ID), modern_client_info()).await;
    let response = service.list_prompts(None).await?;
    let mut names: Vec<_> = response.prompts.iter().map(|prompt| prompt.name.as_str()).collect();
    names.sort_unstable();
    assert_eq!(EXPECTED_PROMPTS, names);
    Ok(())
}
