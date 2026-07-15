use std::{collections::HashMap, sync::Arc};

use rmcp::{RoleClient, model::ServerCapabilities, service::RunningService};
use tokio::sync::Mutex;

use super::backend_client::GatewayBackendClient;
use crate::SessionId;

pub(crate) type McpClientService = Arc<RunningService<RoleClient, GatewayBackendClient>>;

#[derive(Clone, Default)]
pub struct BackendTransports(Arc<Mutex<HashMap<BackendTransportKey, BackendTransportService>>>);

impl BackendTransports {
    pub async fn remove_session(&self, principal: &str, session_id: &str) {
        let mut transports = self.0.lock().await;
        transports.retain(|key, _| key.principal != principal || key.session_id != session_id);
    }

    pub(crate) fn inner(&self) -> &Arc<Mutex<HashMap<BackendTransportKey, BackendTransportService>>> {
        &self.0
    }
}

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub(crate) struct BackendTransportKey {
    principal: String,
    backend_name: String,
    session_id: String,
}

#[derive(Debug)]
pub(crate) struct ServiceHolder {
    pub(crate) name: String,
    pub(crate) running_service: Option<McpClientService>,
}

impl ServiceHolder {
    pub(crate) fn new(name: String, running_service: Option<McpClientService>) -> Self {
        Self { name, running_service }
    }
}

#[derive(Debug)]
pub(crate) struct BackendTransportService {
    #[expect(dead_code, reason = "stored backend capabilities are kept with transport state for future routing")]
    capabilities: Option<ServerCapabilities>,
    pub(crate) service: Option<McpClientService>,
}

impl From<(&str, &str, &str)> for BackendTransportKey {
    fn from((backend_name, session_name, principal): (&str, &str, &str)) -> Self {
        Self {
            principal: principal.to_owned(),
            backend_name: backend_name.to_owned(),
            session_id: session_name.to_owned(),
        }
    }
}

impl From<(&String, &SessionId, &str)> for BackendTransportKey {
    fn from((backend_name, session_name, principal): (&String, &SessionId, &str)) -> Self {
        Self {
            principal: principal.to_owned(),
            backend_name: backend_name.to_owned(),
            session_id: session_name.value().to_owned(),
        }
    }
}

impl From<(Option<ServerCapabilities>, Option<McpClientService>)> for BackendTransportService {
    fn from((capabilities, service): (Option<ServerCapabilities>, Option<McpClientService>)) -> Self {
        Self { capabilities, service }
    }
}
