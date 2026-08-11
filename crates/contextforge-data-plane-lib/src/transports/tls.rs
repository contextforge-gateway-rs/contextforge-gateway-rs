use std::sync::Arc;

use axum::Router;
use http::Request;
use hyper::body::Incoming;
use hyper_util::rt::{TokioExecutor, TokioIo};
use rustls::ServerConfig;
use rustls_pki_types::{self, CertificateDer, PrivateKeyDer, pem::PemObject};
use tokio::net::TcpListener;
use tokio_rustls::TlsAcceptor;
use tower::Service;
use tracing::{info, warn};

use crate::{Config, Error, transports::tcp::Tcp};

pub struct DownstreamTls {
    tcp: Tcp,
    server_config: ServerConfig,
}

impl TryFrom<&Config> for Option<DownstreamTls> {
    type Error = Error;

    fn try_from(config: &Config) -> Result<Self, Self::Error> {
        match (config.tls_address, config.server_certificate.clone(), config.server_private_key.clone()) {
            (Some(address), Some(certificate), Some(private_key)) => {
                let certificates = CertificateDer::pem_file_iter(&certificate)?.flatten().collect::<Vec<_>>();
                let private_key = PrivateKeyDer::from_pem_file(&private_key)?;
                let server_config = ServerConfig::builder_with_protocol_versions(rustls::ALL_VERSIONS)
                    .with_no_client_auth()
                    .with_single_cert(certificates, private_key)?;

                if let Some(tcp_address) = config.address
                    && tcp_address == address
                {
                    return Err("Invalid configuration TCP and TLS ports are the same ".into());
                }

                let tcp = Tcp::new(address);
                Ok(Some(DownstreamTls { tcp, server_config }))
            },
            (None, ..) => Ok(None),
            (Some(_), ..) => Err("Invalid tls config... configuration missing ".into()),
        }
    }
}

impl DownstreamTls {
    pub async fn handle_tls(self, service: Router) -> crate::Result<()> {
        let DownstreamTls { tcp, server_config } = self;
        info!(
            component = "Transport",
            operation = "listen",
            transport = "tls",
            address = %tcp.address,
            "listener starting"
        );
        let tcp_listener: TcpListener = tcp.try_into()?;

        let tls_acceptor = TlsAcceptor::from(Arc::new(server_config));

        loop {
            tokio::select! {
                    maybe_stream = tcp_listener.accept() => {
                        let tower_service = service.clone();
                        let tls_acceptor = tls_acceptor.clone();

                        if let Ok((tcp_stream, _addr)) = maybe_stream {
                            tokio::spawn(async move {
                                let stream = match tls_acceptor.accept(tcp_stream).await {
                                    Ok(stream) => stream,
                                    Err(error) => {
                                        tracing::error!(
                                            component = "Transport",
                                            operation = "tls_handshake",
                                            transport = "tls",
                                            error_code = "CFDP-TLS-HANDSHAKE",
                                            root_cause = %error,
                                            impact_scope = "connection",
                                            retryable = true,
                                            error = %error,
                                            "TLS handshake failed"
                                        );
                                        return;
                                    },
                                };

                                let stream = TokioIo::new(stream);

                                let hyper_service = hyper::service::service_fn(move |request: Request<Incoming>| {
                                    tower_service.clone().call(request)
                                });

                                let ret = hyper_util::server::conn::auto::Builder::new(TokioExecutor::new())
                                    .serve_connection_with_upgrades(stream, hyper_service)
                                    .await;

                                if let Err(err) = ret {
                                    warn!(
                                        component = "Transport",
                                        operation = "serve_connection",
                                        transport = "tls",
                                        error = %err,
                                        "TLS connection terminated with an error"
                                    );
                                }
                            })
                        } else {
                            warn!(
                                component = "Transport",
                                operation = "accept_connection",
                                transport = "tcp",
                                error = ?maybe_stream,
                                "TCP connection accept failed"
                            );
                            return Err(maybe_stream.expect_err("Expect this to work").into());
                        };
                    }
                    _= tokio::signal::ctrl_c()=>{
                        return Ok(())
                    }

            }
        }
    }
}
