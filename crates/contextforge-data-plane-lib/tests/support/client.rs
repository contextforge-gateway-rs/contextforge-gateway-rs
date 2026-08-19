use std::time::{Duration, Instant};

use contextforge_data_plane_lib::Result;
use http::{HeaderMap, HeaderValue};
use rmcp::{
    ServiceExt,
    model::InitializeRequestParams,
    transport::{StreamableHttpClientTransport, streamable_http_client::StreamableHttpClientTransportConfig},
};
use tracing::warn;

use super::auth::token;

pub(crate) const CLIENT_CONNECT_TIMEOUT: Duration = Duration::from_secs(2);
pub(crate) const TEST_POLL_INTERVAL: Duration = Duration::from_millis(20);

pub(crate) fn create_client(user: &str) -> reqwest::Client {
    reqwest::Client::builder().default_headers(auth_headers(user)).build().expect("This should work")
}

pub(crate) fn create_tls_client(user: &str, certificates: Vec<reqwest::Certificate>) -> reqwest::Client {
    reqwest::Client::builder()
        .https_only(true)
        .tls_certs_only(certificates)
        .default_headers(auth_headers(user))
        .build()
        .expect("This should work")
}

fn auth_headers(user: &str) -> HeaderMap {
    let mut default_headers = HeaderMap::new();
    let token = token(user);
    default_headers.insert(
        http::header::AUTHORIZATION,
        HeaderValue::from_str(format!("Bearer {token}").as_str()).expect("This should work"),
    );
    default_headers
}

pub(crate) async fn connect_client(
    gateway_url: String,
    client: reqwest::Client,
) -> Result<rmcp::service::RunningService<rmcp::RoleClient, InitializeRequestParams>> {
    connect_client_with_handler(gateway_url, client, InitializeRequestParams::default()).await
}

/// Connects any `ClientHandler` to the gateway, retrying until `CLIENT_CONNECT_TIMEOUT`.
pub(crate) async fn connect_client_with_handler<H>(
    gateway_url: String,
    client: reqwest::Client,
    handler: H,
) -> Result<rmcp::service::RunningService<rmcp::RoleClient, H>>
where
    H: rmcp::ClientHandler + Clone,
{
    let deadline = Instant::now() + CLIENT_CONNECT_TIMEOUT;
    loop {
        let config = StreamableHttpClientTransportConfig::with_uri(gateway_url.clone());
        let transport = StreamableHttpClientTransport::with_client(client.clone(), config);

        match handler.clone().serve(transport).await {
            Ok(running_service) => return Ok(running_service),
            Err(error) if Instant::now() < deadline => {
                warn!("No Service {error:?}");
                tokio::time::sleep(TEST_POLL_INTERVAL).await;
            },
            Err(error) => {
                warn!("No Service {error:?}");
                return Err("Couldn't get a service".into());
            },
        }
    }
}

pub(crate) fn modern_client_info() -> InitializeRequestParams {
    use rmcp::model::{ClientCapabilities, Implementation, ProtocolVersion};
    InitializeRequestParams::new(ClientCapabilities::default(), Implementation::new("stateless-test-client", "0.1.0"))
        .with_protocol_version(ProtocolVersion::V_2026_07_28)
}

pub(crate) async fn connect_modern_client<H>(
    gateway_url: &str,
    client: reqwest::Client,
    handler: H,
) -> rmcp::service::RunningService<rmcp::RoleClient, H>
where
    H: rmcp::ClientHandler + Clone,
{
    let deadline = Instant::now() + CLIENT_CONNECT_TIMEOUT;
    loop {
        let transport = StreamableHttpClientTransport::with_client(
            client.clone(),
            StreamableHttpClientTransportConfig::with_uri(gateway_url.to_owned()),
        );
        match rmcp::service::serve_client_with_lifecycle(
            handler.clone(),
            transport,
            rmcp::ClientLifecycleMode::Discover {
                preferred_versions: vec![rmcp::model::ProtocolVersion::V_2026_07_28],
            },
        )
        .await
        {
            Ok(service) => return service,
            Err(error) if Instant::now() < deadline => {
                warn!("No Service {error:?}");
                tokio::time::sleep(TEST_POLL_INTERVAL).await;
            },
            Err(error) => panic!("modern stateless client connects: {error:?}"),
        }
    }
}
