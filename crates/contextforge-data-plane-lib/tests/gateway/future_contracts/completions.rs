use contextforge_data_plane_lib::Result;

use crate::harness::{TEST_USER_ID, connect_modern_client, create_client, modern_client_info, start_counter_gateway};

#[tokio::test]
#[ignore = "blocked on federated prompt-completion capability and routing"]
async fn plaintext_completes_prompt_argument_through_prefixed_backend() -> Result<()> {
    let fixture = start_counter_gateway(TEST_USER_ID).await?;
    let service = connect_modern_client(&fixture.gateway_url, create_client(TEST_USER_ID), modern_client_info()).await;

    assert!(
        service.peer_info().and_then(|info| info.capabilities.completions.clone()).is_some(),
        "gateway must advertise completions before clients can call completion/complete"
    );
    let prompts = service.list_prompts(None).await?;
    let prompt_name = prompts
        .prompts
        .iter()
        .find(|prompt| prompt.name.ends_with("-example_prompt"))
        .map(|prompt| prompt.name.clone())
        .ok_or("expected a federated example_prompt")?;
    let values = service.complete_prompt_simple(prompt_name, "message", "h").await?;
    assert!(values.contains(&"hello".to_owned()), "expected backend prompt completions, got {values:?}");
    Ok(())
}

#[tokio::test]
#[ignore = "blocked on federated resource-completion capability and routing"]
async fn plaintext_completes_resource_argument_through_prefixed_backend() -> Result<()> {
    let fixture = start_counter_gateway(TEST_USER_ID).await?;
    let service = connect_modern_client(&fixture.gateway_url, create_client(TEST_USER_ID), modern_client_info()).await;

    let resources = service.list_resources(None).await?;
    let uri = resources.resources.first().ok_or("expected at least one federated resource")?.uri.clone();
    let values = service.complete_resource_simple(uri, "path", "").await?;
    assert!(
        values.first().is_some_and(|value| !value.starts_with("backend-")),
        "expected a stripped backend URI, got {values:?}"
    );
    Ok(())
}
