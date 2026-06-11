use std::{
    collections::{HashMap, HashSet},
    sync::Arc,
};

use contextforge_gateway_rs_apis::user_store::UserConfig;
use contextforge_gateway_rs_cpex::{GatewayPluginRuntimeHandle, RuntimeHookState, ToolPreCallResult};
use http::request::Parts;
use itertools::Itertools;
use rmcp::{
    ClientHandler, ErrorData, RoleClient, RoleServer, ServerHandler, ServiceExt,
    handler::client::progress::ProgressDispatcher,
    model::{
        AnnotateAble, CallToolRequestParams, CallToolResult, ClientRequest, CompleteRequestParams, CompleteResult,
        CompletionInfo, ErrorCode, GetPromptRequestParams, GetPromptResult, Implementation, InitializeRequestParams,
        InitializeResult, ListPromptsResult, ListResourceTemplatesResult, ListResourcesResult, ListToolsResult,
        LoggingLevel, LoggingMessageNotificationParam, PaginatedRequestParams, ProgressNotificationParam, Prompt,
        PromptArgument, PromptMessage, PromptMessageContent, PromptMessageRole, RawImageContent, RawResourceTemplate,
        ReadResourceRequestParams, ReadResourceResult, Reference, Request, Resource, ServerCapabilities, ServerResult,
        SetLevelRequestParams, SubscribeRequestParams, Tool, UnsubscribeRequestParams,
    },
    service::{NotificationContext, PeerRequestOptions, RequestContext, RunningService},
    transport::{StreamableHttpClientTransport, streamable_http_client::StreamableHttpClientTransportConfig},
};
use tokio::sync::{Mutex, broadcast, watch};
use tracing::{debug, info, warn};
use typed_builder::TypedBuilder;

use super::mcp_call_validator::AuthorizedCallValidator;
pub use crate::gateway::session_store::LocalUserSessionStore;
use crate::{
    SessionId,
    gateway::{
        mcp_call_validator::InitializeCallValidator,
        session_manager::SessionManager,
        session_store::{UserSession, UserSessionStore},
    },
};

#[derive(Clone, Default)]
pub struct BackendTransports(Arc<Mutex<HashMap<BackendTransportKey, BackendTransportService>>>);

impl BackendTransports {
    pub async fn remove_session(&self, principal: &str, session_id: &str) {
        let mut transports = self.0.lock().await;
        transports.retain(|key, _| key.principal != principal || key.session_id != session_id);
    }

    pub fn inner(&self) -> &Arc<Mutex<HashMap<BackendTransportKey, BackendTransportService>>> {
        &self.0
    }
}

#[derive(Clone, TypedBuilder)]
#[builder(field_defaults(setter(prefix = "with_")))]
pub struct McpService<T>
where
    T: UserSessionStore,
{
    #[builder(default = Arc::new(Mutex::new(HashSet::new())))]
    subscriptions: Arc<Mutex<HashSet<String>>>,
    #[builder(default = BackendTransports::default())]
    transports: BackendTransports,
    #[builder(default = Arc::new(Mutex::new(LoggingLevel::Debug)))]
    log_level: Arc<Mutex<LoggingLevel>>,
    http_client: reqwest::Client,
    user_session_store: T,
    #[builder(default)]
    plugin_runtime: Option<GatewayPluginRuntimeHandle>,
}

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct BackendTransportKey {
    principal: String,
    backend_name: String,
    session_id: String,
}

type McpClientService = Arc<RunningService<RoleClient, GatewayBackendClient>>;

#[derive(Clone, Debug)]
pub(crate) struct GatewayBackendClient {
    initialize_request: InitializeRequestParams,
    progress_dispatcher: ProgressDispatcher,
    logging_messages: broadcast::Sender<LoggingMessageNotificationParam>,
}

impl GatewayBackendClient {
    fn new(initialize_request: InitializeRequestParams) -> Self {
        let (logging_messages, _) = broadcast::channel(16);
        Self { initialize_request, progress_dispatcher: ProgressDispatcher::new(), logging_messages }
    }
}

impl ClientHandler for GatewayBackendClient {
    fn get_info(&self) -> InitializeRequestParams {
        self.initialize_request.clone()
    }

    async fn on_progress(&self, params: ProgressNotificationParam, _context: NotificationContext<RoleClient>) {
        self.progress_dispatcher.handle_notification(params).await;
    }

    async fn on_logging_message(
        &self,
        params: LoggingMessageNotificationParam,
        _context: NotificationContext<RoleClient>,
    ) {
        let _ = self.logging_messages.send(params);
    }
}

async fn apply_logging_post_hook(
    plugin_runtime: &Option<GatewayPluginRuntimeHandle>,
    tool_name: &str,
    message: LoggingMessageNotificationParam,
    post_state: Option<&RuntimeHookState>,
) -> Option<LoggingMessageNotificationParam> {
    let Some(plugin_runtime) = plugin_runtime else {
        return Some(message);
    };
    match plugin_runtime.after_logging_message(tool_name, message, post_state).await {
        Ok(message) => message,
        Err(error) => {
            warn!("call_tool: plugin rejected backend logging notification: {error:?}");
            None
        },
    }
}

#[derive(Debug)]
pub struct ServiceHolder {
    pub name: String,
    pub running_service: Option<McpClientService>,
}

impl ServiceHolder {
    pub fn new(name: String, running_service: Option<McpClientService>) -> ServiceHolder {
        Self { name, running_service }
    }
}

#[derive(Debug)]
pub struct BackendTransportService {
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

impl<T> ServerHandler for McpService<T>
where
    T: UserSessionStore + Send + Sync + 'static,
{
    async fn initialize(
        &self,
        request: InitializeRequestParams,
        cx: RequestContext<RoleServer>,
    ) -> Result<InitializeResult, ErrorData> {
        let call_validator = InitializeCallValidator::new(&cx);
        let (virtual_host, downstream_session_id, claims) = call_validator.validate()?;
        let session_mapping = if let Ok(maybe_session_mapping) = self
            .user_session_store
            .get_session(&UserSession::new(claims.sub.clone(), Arc::clone(&downstream_session_id.session_id)))
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

        let tasks: Vec<_> = virtual_host
            .backends
            .iter()
            .map(|(name, backend)| {
                let client = self.http_client.clone();
                let request = request.clone();
                let backend_url = backend.url.clone();
                let downstream_session_id = downstream_session_id.clone();

                    Box::pin(async move {
                        let mut headers = HashMap::new();
                        if let Some(host) = backend_url.host_str() && backend_url.scheme() == "https"{
                            let host = if let Some(port) = backend_url.port(){
                                format!("{host}:{port}")
                            }else{
                                host.to_owned()
                            };

                            if let Ok(value) = http::HeaderValue::from_str(&host){
                                headers.insert(http::header::HOST, value);
                            }else{
                                warn!("Really can't set the host header for {:?}",backend_url.host_str());
                            }
                        }

                        let config = StreamableHttpClientTransportConfig::with_uri(backend_url.to_string())
                            .custom_headers(headers);
                        let transport = StreamableHttpClientTransport::with_client(client, config);
                        let maybe_running_service = GatewayBackendClient::new(request).serve(transport).await;
                        if let Ok(running_service) = maybe_running_service {
                            info!("initialize: intialized for {downstream_session_id:?} {name:?}");
                            (name, Some(running_service))
                        } else {
                            warn!("initialize: Unable to initialize for {downstream_session_id:?} {name:?} {maybe_running_service:?}",);
                            (name, None)
                        }
                    })
            }).collect();

        let initialization_results: Vec<(&String, Option<RunningService<RoleClient, GatewayBackendClient>>)> =
            futures::future::join_all(tasks).await;

        let (capabilities, backend_services): (Vec<_>, Vec<_>) = initialization_results
            .into_iter()
            .map(|(name, running_service):(_,_)| {
                info!("initialize: Adding transport: session_id {downstream_session_id:#?} backend {name} {running_service:?}");

                let server_capabilities =
                    running_service.as_ref()
                        .and_then(|rs|
                            rs.peer()
                                .peer_info()
                                .as_ref()
                                .map(|pi| pi.capabilities.clone()));
                (
                    (name.clone(), server_capabilities.clone()),
                    (name.clone(), BackendTransportService::from((server_capabilities, running_service.map(Arc::new)))),
                )
            })
            .unzip();

        if self
            .user_session_store
            .set_session(
                &UserSession::new(claims.sub.clone(), Arc::clone(&downstream_session_id.session_id)),
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

        let mut transports = self.transports.inner().lock().await;
        for (name, svc) in backend_services {
            transports
                .entry(BackendTransportKey::from((name.as_str(), downstream_session_id.value(), claims.sub.as_str())))
                .insert_entry(svc);
        }
        drop(transports);

        Ok(InitializeResult::new(merge_capabilities(capabilities))
            .with_server_info(Implementation::new("rust-conformance-server", "0.1.0"))
            .with_instructions("Rust MCP conformance test server"))
    }

    async fn ping(&self, _cx: RequestContext<RoleServer>) -> Result<(), ErrorData> {
        Ok(())
    }

    async fn list_tools(
        &self,
        request: Option<PaginatedRequestParams>,
        cx: RequestContext<RoleServer>,
    ) -> Result<ListToolsResult, ErrorData> {
        let mcp_call_validator = AuthorizedCallValidator::new("list_tools", &cx);
        let (virtual_host, session_id, claims) = mcp_call_validator.validate()?;

        let session_manager = SessionManager::new(virtual_host, session_id, claims.sub.as_str(), &self.transports);
        let backend_transports: Vec<_> = session_manager.borrow_transports().await;

        let list_tools_tasks = backend_transports
            .into_iter()
            .map(|service_holder| {
                let request = request.clone();
                async move {
                    if let Some(service) = service_holder.running_service {
                        //let service = service.read().await;
                        let response = service.list_tools(request).await;
                        (service_holder.name, Some(response))
                    } else {
                        (service_holder.name, None)
                    }
                }
            })
            .collect::<Vec<_>>();

        let list_tools_tasks_results: Vec<(String, Option<_>)> = futures::future::join_all(list_tools_tasks).await;

        let responses: Vec<_> = list_tools_tasks_results
            .into_iter()
            .map(|(name, response)| {
                info!("list_tools: backend {name} {response:?}");
                (name, response)
            })
            .collect();

        let responses = responses
            .into_iter()
            .filter_map(
                |(name, response)| if let Some(Ok(response)) = response { Some((name, response)) } else { None },
            )
            .collect::<Vec<_>>();

        let merged_list_tools = merge_tools(responses);

        Ok(ListToolsResult { meta: None, tools: merged_list_tools, next_cursor: None })
    }

    async fn call_tool(
        &self,
        request: CallToolRequestParams,
        cx: RequestContext<RoleServer>,
    ) -> Result<CallToolResult, ErrorData> {
        let mcp_call_validator = AuthorizedCallValidator::new("call_tool", &cx);
        let (virtual_host, session_id, claims) = mcp_call_validator.validate()?;
        let session_manager = SessionManager::new(virtual_host, session_id, claims.sub.as_str(), &self.transports);

        let backend_names = session_manager.get_backend_names();

        let Some(BackendToolPair { backend_name, tool_name }) = split_tool_name(&request.name, &backend_names) else {
            return Err(ErrorData {
                code: ErrorCode::INTERNAL_ERROR,
                message: "Routing problem... wrong tool name".into(),
                data: None,
            });
        };
        let backend_name = backend_name.to_owned();
        let tool_name = tool_name.to_owned();
        let request_name = request.name.clone();

        let backend_transports = session_manager.borrow_transports().await;
        info!("Borrowed transports {session_id:?} {backend_transports:?}");
        let mut target_service = None;
        for service_holder in backend_transports {
            debug!(
                "call_tool: Finding backend for {} {service_holder:?} {backend_name} tool_name = {tool_name}",
                &request_name,
            );
            if service_holder.name == backend_name {
                if target_service.is_some() {
                    warn!("call_tool: More than one tool matching for tool name {}", request_name);
                    session_manager.cleanup_backends("call_tool: invalid session.. duplicate tools detected").await;
                    return Err(ErrorData {
                        code: ErrorCode::INVALID_REQUEST,
                        message: "Routing problem... multiple matching tools".into(),
                        data: None,
                    });
                }
                target_service = Some(service_holder);
            }
        }

        let Some(mut target_service) = target_service else {
            return Err(ErrorData {
                code: ErrorCode::INTERNAL_ERROR,
                message: "Routing problem... got no responses from backends".into(),
                data: None,
            });
        };
        let Some(service) = target_service.running_service.take() else {
            warn!(
                "call_tool: trying to call a tool for which we have no backend {target_service:?} {backend_name} tool_name = {tool_name}"
            );
            return Err(ErrorData {
                code: ErrorCode::INTERNAL_ERROR,
                message: "Routing problem... got no responses from backends".into(),
                data: None,
            });
        };

        let pre_result = if let Some(plugin_runtime) = &self.plugin_runtime {
            plugin_runtime.before_tool_call(&request, &tool_name, &backend_name).await?
        } else {
            ToolPreCallResult::unchanged()
        };
        let post_state = pre_result.state;
        let mut routed_request = request;
        pre_result.arguments.apply_to_request(&mut routed_request, &tool_name);

        let service_name = target_service.name.clone();
        let mut notification_forwarders = Vec::new();
        let progress_token = cx
            .meta
            .get_progress_token()
            .or_else(|| routed_request.meta.as_ref().and_then(|meta| meta.get_progress_token()));
        let subscribed_progress_token = progress_token.clone();
        if let Some(progress_token) = progress_token.clone() {
            routed_request.meta.get_or_insert_with(Default::default).set_progress_token(progress_token.clone());
            let mut progress_subscriber = service.service().progress_dispatcher.subscribe(progress_token).await;
            let downstream_peer = cx.peer.clone();
            let plugin_runtime = self.plugin_runtime.clone();
            let progress_tool_name = tool_name.clone();
            let progress_post_state = post_state.clone();
            notification_forwarders.push(tokio::spawn(async move {
                while let Some(progress) = futures::StreamExt::next(&mut progress_subscriber).await {
                    let progress = if let Some(plugin_runtime) = &plugin_runtime {
                        match plugin_runtime
                            .after_progress_notification(&progress_tool_name, progress, progress_post_state.as_ref())
                            .await
                        {
                            Ok(Some(progress)) => progress,
                            Ok(None) => continue,
                            Err(error) => {
                                warn!("call_tool: plugin rejected backend progress notification: {error:?}");
                                continue;
                            },
                        }
                    } else {
                        progress
                    };
                    if let Err(error) = downstream_peer.notify_progress(progress).await {
                        warn!("call_tool: unable to forward backend progress notification downstream: {error:?}");
                        break;
                    }
                }
            }));
        }
        let mut logging_messages = service.service().logging_messages.subscribe();
        let (stop_logging_tx, mut stop_logging_rx) = watch::channel(false);
        let downstream_peer = cx.peer.clone();
        let plugin_runtime = self.plugin_runtime.clone();
        let logging_tool_name = tool_name.clone();
        let logging_post_state = post_state.clone();
        notification_forwarders.push(tokio::spawn(async move {
            loop {
                tokio::select! {
                    result = logging_messages.recv() => match result {
                        Ok(message) => {
                            let Some(message) = apply_logging_post_hook(
                                &plugin_runtime,
                                &logging_tool_name,
                                message,
                                logging_post_state.as_ref(),
                            ).await else { continue };
                            if let Err(error) = downstream_peer.notify_logging_message(message).await {
                                warn!("call_tool: unable to forward backend logging notification downstream: {error:?}");
                                break;
                            }
                        },
                        Err(broadcast::error::RecvError::Lagged(skipped)) => {
                            warn!("call_tool: skipped {skipped} backend logging notifications while forwarding downstream");
                        },
                        Err(broadcast::error::RecvError::Closed) => break,
                    },
                    changed = stop_logging_rx.changed() => {
                        if changed.is_err() || *stop_logging_rx.borrow() {
                            loop {
                                match logging_messages.try_recv() {
                                    Ok(message) => {
                                        let Some(message) = apply_logging_post_hook(
                                            &plugin_runtime,
                                            &logging_tool_name,
                                            message,
                                            logging_post_state.as_ref(),
                                        ).await else { continue };
                                        if let Err(error) = downstream_peer.notify_logging_message(message).await {
                                            warn!("call_tool: unable to forward backend logging notification downstream: {error:?}");
                                            break;
                                        }
                                    },
                                    Err(broadcast::error::TryRecvError::Lagged(skipped)) => {
                                        warn!("call_tool: skipped {skipped} backend logging notifications while forwarding downstream");
                                    },
                                    Err(broadcast::error::TryRecvError::Empty | broadcast::error::TryRecvError::Closed) => break,
                                }
                            }
                            break;
                        }
                    },
                }
            }
        }));
        let response = if progress_token.is_some() {
            let mut options = PeerRequestOptions::no_options();
            options.meta = routed_request.meta.clone();
            let handle = service
                .send_cancellable_request(ClientRequest::CallToolRequest(Request::new(routed_request)), options)
                .await;
            match handle {
                Ok(handle) => match handle.await_response().await {
                    Ok(ServerResult::CallToolResult(result)) => Ok(result),
                    Ok(_) => {
                        warn!("call_tool: backend {service_name} returned unexpected response");
                        Err(rmcp::service::ServiceError::UnexpectedResponse)
                    },
                    Err(error) => Err(error),
                },
                Err(error) => Err(error),
            }
        } else {
            service.call_tool(routed_request).await
        };
        if let Some(progress_token) = subscribed_progress_token {
            service.service().progress_dispatcher.unsubscribe(&progress_token).await;
        }
        let _ = stop_logging_tx.send(true);
        for mut forwarder in notification_forwarders {
            if tokio::time::timeout(std::time::Duration::from_millis(200), &mut forwarder).await.is_err() {
                forwarder.abort();
            }
        }
        let response = response.map_err(|error| {
            warn!("call_tool: backend {service_name} {error:?}");
            ErrorData {
                code: ErrorCode::INTERNAL_ERROR,
                message: "Routing problem... got no responses from backends".into(),
                data: None,
            }
        })?;
        let response = match (&self.plugin_runtime, post_state) {
            (Some(plugin_runtime), Some(post_state)) => {
                plugin_runtime.after_tool_call(&tool_name, response, Some(&post_state)).await?
            },
            _ => response,
        };
        info!("call_tool: backend {service_name} completed");
        Ok(response)
    }

    async fn list_resources(
        &self,
        request: Option<PaginatedRequestParams>,
        cx: RequestContext<RoleServer>,
    ) -> Result<ListResourcesResult, ErrorData> {
        let mcp_call_validator = AuthorizedCallValidator::new("list_resources", &cx);
        let (virtual_host, session_id, claims) = mcp_call_validator.validate()?;

        let session_manager = SessionManager::new(virtual_host, session_id, claims.sub.as_str(), &self.transports);
        let backend_transports: Vec<_> = session_manager.borrow_transports().await;

        let list_resources_tasks = backend_transports
            .into_iter()
            .map(|service_holder| {
                let request = request.clone();
                async move {
                    if let Some(service) = service_holder.running_service {
                        //let service = service.read().await;
                        let response = service.list_resources(request).await;
                        (service_holder.name, Some(response))
                    } else {
                        (service_holder.name, None)
                    }
                }
            })
            .collect::<Vec<_>>();

        let list_tools_tasks_results: Vec<(String, Option<_>)> = futures::future::join_all(list_resources_tasks).await;

        let responses: Vec<_> = list_tools_tasks_results
            .into_iter()
            .map(|(name, response)| {
                info!("list_resources: backend {name} {response:?}");
                (name, response)
            })
            .collect();

        let responses = responses
            .into_iter()
            .filter_map(
                |(name, response)| if let Some(Ok(response)) = response { Some((name, response)) } else { None },
            )
            .collect::<Vec<_>>();

        let merged_list_resources = merge_resources(responses);

        Ok(ListResourcesResult { meta: None, resources: merged_list_resources, next_cursor: None })
    }

    async fn read_resource(
        &self,
        request: ReadResourceRequestParams,
        cx: RequestContext<RoleServer>,
    ) -> Result<ReadResourceResult, ErrorData> {
        let mcp_call_validator = AuthorizedCallValidator::new("read_resource", &cx);
        let (virtual_host, session_id, claims) = mcp_call_validator.validate()?;
        let session_manager = SessionManager::new(virtual_host, session_id, claims.sub.as_str(), &self.transports);

        let backend_names = session_manager.get_backend_names();

        let Some(BackendResourcePair { backend_name, resource_uri }) =
            split_resource_name(&request.uri, &backend_names)
        else {
            return Err(ErrorData {
                code: ErrorCode::INTERNAL_ERROR,
                message: "Routing problem... wrong resource name".into(),
                data: None,
            });
        };

        let backend_transports = session_manager.borrow_transports().await;
        info!("Borrowed transports {session_id:?} {backend_transports:?}");

        let call_tool_tasks: Vec<_> = backend_transports
            .into_iter()
            .map(|service_holder| {
                debug!(
                    "read_resource: Finding backend for {} {service_holder:?} {backend_name} read_resource = {resource_uri}",
                    &request.uri,

                );
                (service_holder.name == backend_name).then(|| {
                    let mut request = request.clone();
                    request.uri = String::from(resource_uri);
                        async move {
                            if let Some(service) = service_holder.running_service {
                                //let service = service.read().await;
                                let response = service.read_resource(request).await;

                                (service_holder.name, Some(response))
                            } else {
                                warn!("call_tool: trying to call a tool for which we have no backend {service_holder:?} {backend_name} resource_name = {resource_uri}");
                                (service_holder.name, None)
                            }
                        }
                })
            }).collect();

        let call_tool_tasks = call_tool_tasks.into_iter().flatten().collect::<Vec<_>>();
        if call_tool_tasks.len() > 1 {
            warn!("read_resource: More than one tool matching for tool name {}", request.uri);

            session_manager.cleanup_backends("read_resource: invalid session.. duplicate resources detected").await;

            return Err(ErrorData {
                code: ErrorCode::INVALID_REQUEST,
                message: "Routing problem... multiple matching resources".into(),
                data: None,
            });
        }

        let call_tool_tasks_results: Vec<_> = futures::future::join_all(call_tool_tasks).await;

        let responses: Vec<_> = call_tool_tasks_results
            .into_iter()
            .map(|(name, response)| {
                info!("read_resource: backend {name} {response:?}");
                (name, response)
            })
            .collect();

        let responses = responses
            .into_iter()
            .filter_map(
                |(name, response)| if let Some(Ok(response)) = response { Some((name, response)) } else { None },
            )
            .collect::<Vec<_>>();

        responses.first().cloned().map(|(_, r)| r).ok_or(ErrorData {
            code: ErrorCode::INTERNAL_ERROR,
            message: "Routing problem... got no responses from backends".into(),
            data: None,
        })
    }

    async fn list_resource_templates(
        &self,
        _request: Option<PaginatedRequestParams>,
        cx: RequestContext<RoleServer>,
    ) -> Result<ListResourceTemplatesResult, ErrorData> {
        let maybe_parts = cx.extensions.get::<Parts>();
        let maybe_session = maybe_parts.and_then(|parts| parts.extensions.get::<SessionId>());
        let maybe_user_config = maybe_parts.and_then(|parts| parts.extensions.get::<UserConfig>());
        info!("list_resource_templates user_config = {maybe_user_config:#?} session_id = {maybe_session:#?}");
        Ok(ListResourceTemplatesResult {
            meta: None,
            resource_templates: vec![
                RawResourceTemplate {
                    uri_template: "test://template/{id}/data".into(),
                    name: "Dynamic Resource".into(),
                    title: None,
                    description: Some("A dynamic resource with parameter substitution".into()),
                    mime_type: Some("application/json".into()),
                    icons: None,
                }
                .no_annotation(),
            ],
            next_cursor: None,
        })
    }

    async fn subscribe(
        &self,
        request: SubscribeRequestParams,
        cx: RequestContext<RoleServer>,
    ) -> Result<(), ErrorData> {
        let maybe_parts = cx.extensions.get::<Parts>();
        let maybe_session = maybe_parts.and_then(|parts| parts.extensions.get::<SessionId>());
        let maybe_user_config = maybe_parts.and_then(|parts| parts.extensions.get::<UserConfig>());
        info!("subscribe user_config = {maybe_user_config:#?} session_id = {maybe_session:#?}");

        let mut subs = self.subscriptions.lock().await;
        subs.insert(request.uri.clone());
        Ok(())
    }

    async fn unsubscribe(
        &self,
        request: UnsubscribeRequestParams,
        cx: RequestContext<RoleServer>,
    ) -> Result<(), ErrorData> {
        let maybe_parts = cx.extensions.get::<Parts>();
        let maybe_session = maybe_parts.and_then(|parts| parts.extensions.get::<SessionId>());
        let maybe_user_config = maybe_parts.and_then(|parts| parts.extensions.get::<UserConfig>());
        info!("unsubscribe user_config = {maybe_user_config:#?} session_id = {maybe_session:#?}");

        let mut subs = self.subscriptions.lock().await;
        subs.remove(request.uri.as_str());
        Ok(())
    }

    async fn list_prompts(
        &self,
        _request: Option<PaginatedRequestParams>,
        cx: RequestContext<RoleServer>,
    ) -> Result<ListPromptsResult, ErrorData> {
        let maybe_parts = cx.extensions.get::<Parts>();
        let maybe_session = maybe_parts.and_then(|parts| parts.extensions.get::<SessionId>());
        let maybe_user_config = maybe_parts.and_then(|parts| parts.extensions.get::<UserConfig>());
        info!("list_prompts user_config = {maybe_user_config:#?} session_id = {maybe_session:#?}");

        Ok(ListPromptsResult {
            meta: None,
            prompts: vec![
                Prompt::new("test_simple_prompt", Some("A simple test prompt with no arguments"), None),
                Prompt::new(
                    "test_prompt_with_arguments",
                    Some("A test prompt that accepts arguments"),
                    Some(vec![
                        PromptArgument::new("name").with_description("The name to greet").with_required(true),
                        PromptArgument::new("style").with_description("The greeting style").with_required(false),
                    ]),
                ),
                Prompt::new(
                    "test_prompt_with_embedded_resource",
                    Some("A test prompt that includes an embedded resource"),
                    None,
                ),
                Prompt::new("test_prompt_with_image", Some("A test prompt that includes an image"), None),
            ],
            next_cursor: None,
        })
    }

    async fn get_prompt(
        &self,
        request: GetPromptRequestParams,
        cx: RequestContext<RoleServer>,
    ) -> Result<GetPromptResult, ErrorData> {
        let maybe_parts = cx.extensions.get::<Parts>();
        let maybe_session = maybe_parts.and_then(|parts| parts.extensions.get::<SessionId>());
        let maybe_user_config = maybe_parts.and_then(|parts| parts.extensions.get::<UserConfig>());
        info!("get_prompt user_config = {maybe_user_config:#?} session_id = {maybe_session:#?}");
        match request.name.as_str() {
            "test_simple_prompt" => Ok(GetPromptResult::new(vec![PromptMessage::new_text(
                PromptMessageRole::User,
                "This is a simple test prompt.",
            )])
            .with_description("A simple test prompt")),
            "test_prompt_with_arguments" => {
                let args = request.arguments.unwrap_or_default();
                let name = args.get("name").and_then(|v| v.as_str()).unwrap_or("World");
                let style = args.get("style").and_then(|v| v.as_str()).unwrap_or("friendly");
                Ok(GetPromptResult::new(vec![PromptMessage::new_text(
                    PromptMessageRole::User,
                    format!("Please greet {name} in a {style} style."),
                )])
                .with_description("A prompt with arguments"))
            },
            "test_prompt_with_embedded_resource" => Ok(GetPromptResult::new(vec![
                PromptMessage::new_text(PromptMessageRole::User, "Here is a resource:"),
                PromptMessage::new_resource(
                    PromptMessageRole::User,
                    "test://static-text".into(),
                    Some("text/plain".into()),
                    Some("Resource content for prompt".into()),
                    None,
                    None,
                    None,
                ),
            ])
            .with_description("A prompt with an embedded resource")),
            "test_prompt_with_image" => {
                let image_content =
                    RawImageContent { data: TEST_IMAGE_DATA.into(), mime_type: "image/png".into(), meta: None };
                Ok(GetPromptResult::new(vec![
                    PromptMessage::new_text(PromptMessageRole::User, "Here is an image:"),
                    PromptMessage::new(
                        PromptMessageRole::User,
                        PromptMessageContent::Image { image: image_content.no_annotation() },
                    ),
                ])
                .with_description("A prompt with an image"))
            },
            _ => Err(ErrorData::invalid_params(format!("Unknown prompt: {}", request.name), None)),
        }
    }

    async fn complete(
        &self,
        request: CompleteRequestParams,
        cx: RequestContext<RoleServer>,
    ) -> Result<CompleteResult, ErrorData> {
        let maybe_parts = cx.extensions.get::<Parts>();
        let maybe_session = maybe_parts.and_then(|parts| parts.extensions.get::<SessionId>());
        let maybe_user_config = maybe_parts.and_then(|parts| parts.extensions.get::<UserConfig>());
        info!("complete user_config = {maybe_user_config:#?} session_id = {maybe_session:#?}");
        let values = match &request.r#ref {
            Reference::Resource(_) => {
                if request.argument.name == "id" {
                    vec!["1".into(), "2".into(), "3".into()]
                } else {
                    vec![]
                }
            },
            Reference::Prompt(prompt_ref) => {
                if request.argument.name == "name" {
                    vec!["Alice".into(), "Bob".into(), "Charlie".into()]
                } else if request.argument.name == "style" {
                    vec!["friendly".into(), "formal".into(), "casual".into()]
                } else {
                    vec![prompt_ref.name.clone()]
                }
            },
        };
        Ok(CompleteResult::new(CompletionInfo::new(values).map_err(|e| ErrorData::internal_error(e, None))?))
    }

    async fn set_level(&self, request: SetLevelRequestParams, cx: RequestContext<RoleServer>) -> Result<(), ErrorData> {
        let maybe_parts = cx.extensions.get::<Parts>();
        let maybe_session = maybe_parts.and_then(|parts| parts.extensions.get::<SessionId>());
        let maybe_user_config = maybe_parts.and_then(|parts| parts.extensions.get::<UserConfig>());
        info!("set_level user_config = {maybe_user_config:#?} session_id = {maybe_session:#?}");
        let mut level = self.log_level.lock().await;
        *level = request.level;
        Ok(())
    }
}

#[derive(Debug, PartialEq, PartialOrd, Ord, Eq)]
struct BackendToolPair<'a> {
    backend_name: &'a str,
    tool_name: &'a str,
}

#[derive(Debug, PartialEq, PartialOrd, Ord, Eq)]
struct BackendResourcePair<'a> {
    backend_name: &'a str,
    resource_uri: &'a str,
}

fn split_tool_name<'a, T: AsRef<str>, N: AsRef<str>>(
    tool_name: &'a T,
    backend_names: &'a [N],
) -> Option<BackendToolPair<'a>> {
    for name in backend_names {
        let tool_name = tool_name.as_ref();
        let name = name.as_ref();
        let extended_name = name.to_owned() + "-";
        if tool_name.starts_with(&extended_name) {
            return Some(BackendToolPair { backend_name: name, tool_name: &tool_name[extended_name.len()..] });
        }
    }
    None
}

const TEST_IMAGE_DATA: &str =
    "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAADUlEQVR42mP8/5+hHgAHggJ/PchI7wAAAABJRU5ErkJggg==";
// Small base64-encoded WAV (silence)

fn merge_capabilities(_server_capabilities: Vec<(String, Option<ServerCapabilities>)>) -> ServerCapabilities {
    ServerCapabilities::builder().enable_prompts().enable_resources().enable_tools().enable_logging().build()
}

fn merge_tools(tools: Vec<(String, ListToolsResult)>) -> Vec<Tool> {
    tools
        .into_iter()
        .flat_map(|(backend_name, result)| {
            result
                .tools
                .into_iter()
                .map(|mut t| {
                    t.name = format!("{backend_name}-{}", t.name).into();
                    t
                })
                .collect::<Vec<_>>()
        })
        .sorted_by(|t, o| t.name.cmp(&o.name))
        .collect::<Vec<_>>()
}

fn merge_resources(resources: Vec<(String, ListResourcesResult)>) -> Vec<Resource> {
    resources
        .into_iter()
        .flat_map(|(backend_name, result)| {
            result
                .resources
                .into_iter()
                .map(|mut t| {
                    t.name = format!("{backend_name}-{}", t.name);
                    t.uri = format!("{backend_name}-{}", t.uri);
                    t
                })
                .collect::<Vec<_>>()
        })
        .sorted_by(|t, o| t.name.cmp(&o.name))
        .collect::<Vec<_>>()
}

fn split_resource_name<'a, T: AsRef<str>, N: AsRef<str>>(
    resource_uri: &'a T,
    backend_names: &'a [N],
) -> Option<BackendResourcePair<'a>> {
    for name in backend_names {
        let resource_uri = resource_uri.as_ref();
        let name = name.as_ref();
        let extended_name = name.to_owned() + "-";
        if resource_uri.starts_with(&extended_name) {
            return Some(BackendResourcePair {
                backend_name: name,
                resource_uri: &resource_uri[extended_name.len()..],
            });
        }
    }
    None
}

#[cfg(test)]
mod tests {
    // Note this useful idiom: importing names from outer (for mod tests) scope.
    use super::*;

    #[test]
    fn test_splitting() {
        let tool_name = "counter-one-increment";
        let backend_names = vec!["counter-on", "counter-oneee", "counter-one"];
        let pair = BackendToolPair { backend_name: "counter-one", tool_name: "increment" };
        assert_eq!(Some(pair), split_tool_name(&tool_name, &backend_names));
        let tool_name = "counter-oneincrement";
        assert_eq!(None, split_tool_name(&tool_name, &backend_names));
        let tool_name = "counteroneincrement";
        assert_eq!(None, split_tool_name(&tool_name, &backend_names));
        let tool_name = "counter-one-get-value";
        let pair = BackendToolPair { backend_name: "counter-one", tool_name: "get-value" };
        assert_eq!(Some(pair), split_tool_name(&tool_name, &backend_names));

        let backend_names = vec!["counter_on", "counter_oneee", "counter_one"];
        let tool_name = "counter_one-get-value";
        let pair = BackendToolPair { backend_name: "counter_one", tool_name: "get-value" };
        assert_eq!(Some(pair), split_tool_name(&tool_name, &backend_names));
    }
}
