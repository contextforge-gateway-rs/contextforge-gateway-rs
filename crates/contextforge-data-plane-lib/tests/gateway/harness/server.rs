use std::{net::SocketAddr, path::Path, time::Duration};

use axum::Router;
use contextforge_data_plane_lib::Result;
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;

const SHUTDOWN_TIMEOUT: Duration = Duration::from_secs(2);

/// An in-process HTTP server whose listener is bound before its task starts.
#[must_use = "dropping the server shuts it down"]
pub(crate) struct TestServer {
    address: SocketAddr,
    scheme: &'static str,
    shutdown: CancellationToken,
    handle: Option<JoinHandle<Result<()>>>,
}

impl TestServer {
    pub(crate) async fn start_http(router: Router) -> Result<Self> {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
        let address = listener.local_addr()?;
        let shutdown = CancellationToken::new();
        let server_shutdown = shutdown.clone();
        let handle = tokio::spawn(async move {
            axum::serve(listener, router).with_graceful_shutdown(server_shutdown.cancelled_owned()).await?;
            Ok(())
        });

        Ok(Self { address, scheme: "http", shutdown, handle: Some(handle) })
    }

    pub(crate) async fn start_tls(
        router: Router,
        certificate: impl AsRef<Path>,
        private_key: impl AsRef<Path>, // pragma: allowlist secret
    ) -> Result<Self> {
        let tls_config = axum_server::tls_rustls::RustlsConfig::from_pem_file(certificate, private_key).await?;
        let listener = std::net::TcpListener::bind("127.0.0.1:0")?;
        let address = listener.local_addr()?;
        listener.set_nonblocking(true)?;

        let shutdown = CancellationToken::new();
        let server_shutdown = shutdown.clone();
        let server_handle = axum_server::Handle::new();
        let graceful_handle = server_handle.clone();
        let handle = tokio::spawn(async move {
            let server = axum_server::from_tcp_rustls(listener, tls_config)?
                .handle(server_handle)
                .serve(router.into_make_service());
            tokio::pin!(server);

            tokio::select! {
                result = &mut server => result?,
                () = server_shutdown.cancelled() => {
                    graceful_handle.graceful_shutdown(None);
                    server.await?;
                }
            }
            Ok(())
        });

        Ok(Self { address, scheme: "https", shutdown, handle: Some(handle) })
    }

    pub(crate) fn url(&self, path: &str) -> String {
        format!("{}://{}{}", self.scheme, self.address, path)
    }

    pub(crate) async fn shutdown(mut self) -> Result<()> {
        self.stop().await
    }

    async fn stop(&mut self) -> Result<()> {
        self.shutdown.cancel();
        let Some(mut handle) = self.handle.take() else {
            return Ok(());
        };

        let Ok(result) = tokio::time::timeout(SHUTDOWN_TIMEOUT, &mut handle).await else {
            handle.abort();
            let _ = handle.await;
            return Ok(());
        };
        result??;
        Ok(())
    }
}

impl Drop for TestServer {
    fn drop(&mut self) {
        self.shutdown.cancel();
        if let Some(handle) = self.handle.take() {
            handle.abort();
        }
    }
}
