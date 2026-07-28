use std::{collections::HashMap, sync::Arc};

use rmcp::{
    ErrorData, RoleClient, RoleServer, ServiceExt,
    model::{ErrorCode, Implementation, InitializeRequestParams, InitializeResult, ServerCapabilities},
    service::{RequestContext, RunningService},
    transport::{StreamableHttpClientTransport, streamable_http_client::StreamableHttpClientTransportConfig},
};
use tracing::{info, warn};

use super::McpService;
use crate::gateway::{
    backend_client::GatewayBackendClient,
    backend_transports::{BackendTransportKey, BackendTransportService},
    mcp_call_validator::InitializeCallValidator,
    session_store::{UserSession, UserSessionStore},
};

pub(super) async fn initialize<T>(
    mcp_service: &McpService<T>,
    request: InitializeRequestParams,
    cx: RequestContext<RoleServer>,
) -> Result<InitializeResult, ErrorData>
where
    T: UserSessionStore + Send + Sync + 'static,
{
    let call_validator = InitializeCallValidator::new(&cx);
    let (virtual_host, downstream_session_id, claims) = call_validator.validate()?;
    let session_mapping = if let Ok(maybe_session_mapping) = mcp_service
        .user_session_store
        .get_session(&UserSession::new(claims.sub.clone(), Arc::from(downstream_session_id.value().as_str())))
        .await
    {
        maybe_session_mapping.unwrap_or_default()
    } else {
        return Err(ErrorData {
            code: ErrorCode::INTERNAL_ERROR,
            message: "Internal problem... session store can't be accessed".into(),
            data: None,
        });
    };

    let namespace_identifiers = virtual_host.backends.len() > 1;
    let tasks: Vec<_> = virtual_host
        .backends
        .iter()
        .map(|(name, backend)| {
            let client = mcp_service.http_client.clone();
            let backend_client = GatewayBackendClient::new(
                name.clone(),
                namespace_identifiers,
                request.clone(),
                mcp_service.plugin_runtime.clone(),
            );
            let backend_url = backend.url.clone();
            let downstream_session_id = downstream_session_id.clone();

            Box::pin(async move {
                let mut headers = HashMap::new();
                if let Some(host) = backend_url.host_str()
                    && backend_url.scheme() == "https"
                {
                    let host = if let Some(port) = backend_url.port() {
                        format!("{host}:{port}")
                    } else {
                        host.to_owned()
                    };

                    if let Ok(value) = http::HeaderValue::from_str(&host) {
                        headers.insert(http::header::HOST, value);
                    } else {
                        warn!("Really can't set the host header for {:?}", backend_url.host_str());
                    }
                }

                let config =
                    StreamableHttpClientTransportConfig::with_uri(backend_url.to_string()).custom_headers(headers);
                let transport = StreamableHttpClientTransport::with_client(client, config);
                let maybe_running_service = backend_client.serve(transport).await;
                if let Ok(running_service) = maybe_running_service {
                    info!("initialize: intialized for {downstream_session_id:?} {name:?}");
                    (name, Some(running_service))
                } else {
                    warn!(
                        "initialize: Unable to initialize for {downstream_session_id:?} {name:?} {maybe_running_service:?}",
                    );
                    (name, None)
                }
            })
        })
        .collect();

    let initialization_results: Vec<(&String, Option<RunningService<RoleClient, GatewayBackendClient>>)> =
        futures::future::join_all(tasks).await;

    let (capabilities, backend_services): (Vec<_>, Vec<_>) = initialization_results
        .into_iter()
        .map(|(name, running_service): (_, _)| {
            info!(
                "initialize: Adding transport: session_id {downstream_session_id:#?} backend {name} {running_service:?}"
            );

            let server_capabilities = running_service
                .as_ref()
                .and_then(|rs| rs.peer().peer_info().as_ref().map(|pi| pi.capabilities.clone()));
            (
                (name.clone(), server_capabilities.clone()),
                (name.clone(), BackendTransportService::from((server_capabilities, running_service.map(Arc::new)))),
            )
        })
        .unzip();

    if mcp_service
        .user_session_store
        .set_session(
            &UserSession::new(claims.sub.clone(), Arc::from(downstream_session_id.value().as_str())),
            &session_mapping,
        )
        .await
        .is_err()
    {
        return Err(ErrorData {
            code: ErrorCode::INTERNAL_ERROR,
            message: "Internal problem... session store can't be written".into(),
            data: None,
        });
    }

    let mut transports = mcp_service.transports.inner().lock().await;
    for (name, service) in backend_services {
        transports
            .entry(BackendTransportKey::from((
                name.as_str(),
                downstream_session_id.value().as_str(),
                claims.sub.as_str(),
            )))
            .insert_entry(service);
    }
    drop(transports);

    Ok(InitializeResult::new(merge_and_build_capabilities(capabilities))
        .with_server_info(Implementation::new("rust-conformance-server", "0.1.0"))
        .with_instructions("Rust MCP conformance test server"))
}

fn merge_and_build_capabilities(server_capabilities: Vec<(String, Option<ServerCapabilities>)>) -> ServerCapabilities {
    let mut merged = ServerCapabilities::default();

    for (_, capabilities) in server_capabilities {
        let Some(capabilities) = capabilities else {
            continue;
        };

        if capabilities.completions.is_some() {
            merged.completions.get_or_insert_default();
        }

        if capabilities.prompts.is_some() {
            merged.prompts.get_or_insert_default();
        }

        if let Some(resources) = capabilities.resources {
            let merged_resources = merged.resources.get_or_insert_default();
            if resources.subscribe == Some(true) {
                merged_resources.subscribe = Some(true);
            }
        }

        if capabilities.tools.is_some() {
            merged.tools.get_or_insert_default();
        }
    }

    merged
}

#[cfg(test)]
mod tests {
    use rmcp::model::ServerCapabilities;

    use super::merge_and_build_capabilities;

    #[test]
    fn merge_and_build_capabilities_only_advertises_upstream_capabilities() {
        let capabilities = merge_and_build_capabilities(vec![
            ("first".to_owned(), Some(ServerCapabilities::builder().enable_tools().build())),
            ("second".to_owned(), Some(ServerCapabilities::builder().enable_resources().enable_completions().build())),
            ("third".to_owned(), Some(ServerCapabilities::builder().enable_tools().build())),
        ]);

        assert!(capabilities.tools.is_some());
        assert!(capabilities.completions.is_some());
        assert!(capabilities.resources.is_some());
        assert!(capabilities.prompts.is_none());
    }
}
