use std::time::Instant;

use contextforge_data_plane_lib::Result;
use rmcp::{
    ServiceExt,
    model::{InitializeRequestParams, ProtocolVersion},
    transport::{StreamableHttpClientTransport, streamable_http_client::StreamableHttpClientTransportConfig},
};
use tracing::warn;

use super::{CLIENT_CONNECT_TIMEOUT, TEST_POLL_INTERVAL};

/// Connects through the pre-discovery lifecycle retained for compatibility coverage.
pub(crate) async fn connect_client_with_protocol(
    gateway_url: String,
    client: reqwest::Client,
    protocol_version: ProtocolVersion,
) -> Result<rmcp::service::RunningService<rmcp::RoleClient, InitializeRequestParams>> {
    let handler = InitializeRequestParams::default().with_protocol_version(protocol_version);
    let deadline = Instant::now() + CLIENT_CONNECT_TIMEOUT;

    loop {
        let transport = StreamableHttpClientTransport::with_client(
            client.clone(),
            StreamableHttpClientTransportConfig::with_uri(gateway_url.clone()),
        );
        match handler.clone().serve(transport).await {
            Ok(service) => return Ok(service),
            Err(error) if Instant::now() < deadline => {
                warn!("compatibility client has not connected yet: {error:?}");
                tokio::time::sleep(TEST_POLL_INTERVAL).await;
            },
            Err(error) => return Err(format!("compatibility client could not connect: {error:?}").into()),
        }
    }
}
