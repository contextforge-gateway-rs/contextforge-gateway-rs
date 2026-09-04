use contextforge_data_plane_lib::Result;
use rmcp::model::{ReadResourceRequestParams, ResourceContents};
use tracing::{info, warn};

use crate::harness::{TEST_USER_ID, connect_modern_client, create_client, modern_client_info, start_counter_gateway};

const EXPECTED_TEMPLATE_NAMES: &[&str] =
    &["00000000-0000-0000-0000-000000000001-filesystem", "00000000-0000-0000-0000-000000000001-memo"];
const EXPECTED_TEMPLATE_URIS: &[&str] =
    &["00000000-0000-0000-0000-000000000001-memo://{id}", "00000000-0000-0000-0000-000000000001-str:////{path}"];

#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
#[test_log::test]
#[ignore = "blocked on federated list-resource-templates support"]
async fn plaintext_lists_prefixed_backend_resource_templates() -> Result<()> {
    let fixture = start_counter_gateway(TEST_USER_ID).await?;
    assert_list_resource_templates(
        fixture.gateway_url.clone(),
        create_client(TEST_USER_ID),
        EXPECTED_TEMPLATE_NAMES,
        EXPECTED_TEMPLATE_URIS,
    )
    .await
}

#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
#[test_log::test]
#[ignore = "blocked on federated resource-template discovery and routing"]
async fn plaintext_reads_resource_from_prefixed_template() -> Result<()> {
    let fixture = start_counter_gateway(TEST_USER_ID).await?;
    assert_read_resource_from_template(fixture.gateway_url.clone(), create_client(TEST_USER_ID)).await
}

async fn assert_list_resource_templates(
    gateway_url: String,
    client: reqwest::Client,
    expected_template_names: &[&str],
    expected_template_uris: &[&str],
) -> Result<()> {
    info!("Sending request to {gateway_url}");

    let running_service = connect_modern_client(&gateway_url, client, modern_client_info()).await;

    let list_templates = running_service.list_resource_templates(None).await;
    let Ok(list_templates) = list_templates else {
        let msg = format!("List resource templates returned error {list_templates:?}");
        warn!(msg);
        return Err(msg.into());
    };

    let mut names: Vec<&str> =
        list_templates.resource_templates.iter().map(|template| template.name.as_str()).collect();
    names.sort_unstable();

    if expected_template_names != names {
        warn!("Actual {names:#?} Expected {expected_template_names:#?}");
        return Err("Expected resource template names don't match actual".into());
    }

    let mut uris: Vec<&str> =
        list_templates.resource_templates.iter().map(|template| template.uri_template.as_str()).collect();
    uris.sort_unstable();

    if expected_template_uris != uris {
        warn!("Actual {uris:#?} Expected {expected_template_uris:#?}");
        return Err("Expected resource template uris don't match actual".into());
    }

    Ok(())
}

async fn assert_read_resource_from_template(gateway_url: String, client: reqwest::Client) -> Result<()> {
    let running_service = connect_modern_client(&gateway_url, client, modern_client_info()).await;

    let list_templates = running_service.list_resource_templates(None).await?;
    let Some(memo_template) = list_templates.resource_templates.iter().find(|t| t.uri_template.contains("memo://"))
    else {
        return Err("Expected a memo resource template".into());
    };

    // Expand the namespaced template the way a client would, then read it back through the gateway.
    let uri = memo_template.uri_template.replace("{id}", "insights");
    let result = running_service.read_resource(ReadResourceRequestParams::new(uri)).await?;

    let Some(ResourceContents::TextResourceContents { text, .. }) = result.contents.first() else {
        return Err("Expected text resource contents".into());
    };

    if !text.contains("Business Intelligence Memo") {
        return Err(format!("Expected routed resource to include memo content, got: {text}").into());
    }

    Ok(())
}
