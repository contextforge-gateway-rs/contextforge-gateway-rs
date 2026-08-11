use contextforge_data_plane_apis::user_store::VirtualHost;
use tracing::{debug, info};

use super::backend_transports::{BackendTransportKey, BackendTransports, ServiceHolder};
use crate::layers::session_id::SessionId;

pub struct SessionManager<'a> {
    virtual_host: &'a VirtualHost,
    session_id: &'a SessionId,
    principal: &'a str,
    transports: &'a BackendTransports,
}

impl<'a> SessionManager<'a> {
    pub fn new(
        virtual_host: &'a VirtualHost,
        session_id: &'a SessionId,
        principal: &'a str,
        transports: &'a BackendTransports,
    ) -> Self {
        Self { virtual_host, session_id, principal, transports }
    }

    pub fn get_backend_names(&self) -> Vec<&str> {
        self.virtual_host.backends.keys().map(std::string::String::as_str).collect()
    }

    pub async fn borrow_transports(&self) -> Vec<ServiceHolder> {
        let names: Vec<_> = self.virtual_host.backends.keys().cloned().collect();
        let mut transports = self.transports.inner().lock().await;
        names
            .into_iter()
            .filter_map(|name| {
                transports
                    .get_mut(&BackendTransportKey::from((&name, self.session_id, self.principal)))
                    .map(|b| ServiceHolder::new(name, b.service.clone()))
            })
            .collect()
    }

    // pub async fn return_transports(&self, backend_transports: impl Iterator<Item = ServiceHolder>) {
    //     let backend_transports = backend_transports.collect::<Vec<_>>();
    //     let mut transports = self.transports.inner().lock().await;
    //     for svc_holder in backend_transports {
    //         transports
    //             .entry(BackendTransportKey::from((&svc_holder.name, self.session_id, self.principal)))
    //             .and_modify(|e| e.service = svc_holder.running_service);
    //     }
    // }

    pub async fn cleanup_backends(&self, reason: &'static str) {
        let names: Vec<_> = self.virtual_host.backends.keys().cloned().collect();
        info!(
            component = "Session",
            operation = "cleanup_backends",
            backend_count = names.len(),
            reason,
            "cleaning up backend transports"
        );
        let mut transports = self.transports.inner().lock().await;
        for name in names {
            let key = BackendTransportKey::from((&name, self.session_id, self.principal));
            debug!(
                component = "Session",
                operation = "cleanup_backend",
                backend_name = name,
                reason,
                "removing backend transport"
            );
            transports.remove(&key);
        }
    }
}
