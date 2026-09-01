use std::{fs::File, io::Read};

use contextforge_data_plane_lib::Result;
use rustls::crypto;

use crate::harness::{
    TEST_USER_ID, connect_modern_client, create_client, create_tls_client, modern_client_info, start_counter_gateway,
    start_tls_counter_gateway,
};

const EXPECTED_TOOLS: &[&str] = &[
    "00000000-0000-0000-0000-000000000001-decrement",
    "00000000-0000-0000-0000-000000000001-echo",
    "00000000-0000-0000-0000-000000000001-get_session_id",
    "00000000-0000-0000-0000-000000000001-get_value",
    "00000000-0000-0000-0000-000000000001-increment",
    "00000000-0000-0000-0000-000000000001-long_task",
    "00000000-0000-0000-0000-000000000001-say_hello",
    "00000000-0000-0000-0000-000000000001-sum",
];

#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
#[test_log::test]
#[ignore = "blocked on federated list-tools support"]
async fn plaintext_lists_prefixed_backend_tools() -> Result<()> {
    let fixture = start_counter_gateway(TEST_USER_ID).await?;
    assert_list_tools(fixture.gateway_url.clone(), create_client(TEST_USER_ID)).await
}

#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
#[test_log::test]
#[ignore = "blocked on control-plane TLS publication and federated list-tools support"]
async fn tls_lists_prefixed_backend_tools() -> Result<()> {
    let provider = crypto::ring::default_provider();
    _ = provider.install_default();
    let fixture = start_tls_counter_gateway(TEST_USER_ID).await?;

    let mut trust_bundle = Vec::new();
    File::open("../../assets/contextforgeCA/contextforge.intermediate.ca-chain.cert.pem")?
        .read_to_end(&mut trust_bundle)?;
    let certificates = reqwest::Certificate::from_pem_bundle(&trust_bundle)?;
    assert_list_tools(fixture.gateway_url.clone(), create_tls_client(TEST_USER_ID, certificates)).await
}

async fn assert_list_tools(gateway_url: String, client: reqwest::Client) -> Result<()> {
    let running_service = connect_modern_client(&gateway_url, client, modern_client_info()).await;
    let response = running_service.list_tools(None).await?;
    let mut names: Vec<&str> = response.tools.iter().map(|tool| tool.name.as_ref()).collect();
    names.sort_unstable();
    assert_eq!(EXPECTED_TOOLS, names);
    Ok(())
}
