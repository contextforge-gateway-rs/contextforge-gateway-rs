use std::{collections::HashMap, sync::Arc};

use contextforge_gateway_rs_apis::user_store::VirtualHost;
use contextforge_gateway_rs_cpex::{GatewayPluginRuntimeHandle, ToolPreCallResult};
use itertools::Itertools;
use rmcp::{
    ErrorData, RoleClient, RoleServer, ServerHandler, ServiceExt,
    model::{
        CallToolRequestParams, CallToolResult, CompleteRequestParams, CompleteResult, ErrorCode,
        GetPromptRequestParams, GetPromptResult, Implementation, InitializeRequestParams, InitializeResult,
        ListPromptsResult, ListResourceTemplatesResult, ListResourcesResult, ListToolsResult, PaginatedRequestParams,
        Prompt, ReadResourceRequestParams, ReadResourceResult, Reference, Resource, ResourceTemplate,
        ServerCapabilities, SubscribeRequestParams, Tool, UnsubscribeRequestParams,
    },
    service::{RequestContext, RunningService},
    transport::{StreamableHttpClientTransport, streamable_http_client::StreamableHttpClientTransportConfig},
};
use tokio::sync::Mutex;
use tracing::{debug, info, warn};
use typed_builder::TypedBuilder;

use super::{
    backend_client::{GatewayBackendClient, call_backend_tool},
    mcp_call_validator::AuthorizedCallValidator,
};
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
    #[builder(default = BackendTransports::default())]
    transports: BackendTransports,
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
                let backend_client =
                    GatewayBackendClient::new(name.clone(), request.clone(), self.plugin_runtime.clone());
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
                        let maybe_running_service = backend_client.serve(transport).await;
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

        let responses = fan_out_list(
            backend_transports,
            "list_tools",
            |response: &ListToolsResult| response.tools.len(),
            |service| {
                let request = request.clone();
                async move { service.list_tools(request).await }
            },
        )
        .await;

        Ok(ListToolsResult { meta: None, tools: merge_tools(responses, virtual_host), next_cursor: None })
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

        let Some((backend_name, tool_name)) = resolve_tool_route(virtual_host, &request.name, &backend_names) else {
            return Err(ErrorData {
                code: ErrorCode::INTERNAL_ERROR,
                message: "Routing problem... wrong tool name".into(),
                data: None,
            });
        };
        let backend_name = backend_name.to_owned();
        let tool_name = tool_name.to_owned();

        let (service_name, service) = resolve_backend(&session_manager, "call_tool", &backend_name).await?;

        let pre_result = if let Some(plugin_runtime) = &self.plugin_runtime {
            plugin_runtime.before_tool_call(&request, &tool_name, &service_name).await?
        } else {
            ToolPreCallResult::unchanged()
        };
        let post_state = pre_result.state;
        let mut routed_request = request;
        pre_result.arguments.apply_to_request(&mut routed_request, &tool_name);

        let progress_token = cx.meta.get_progress_token();
        service
            .service()
            .track_tool_call(tool_name.clone(), cx.peer.clone(), progress_token.clone(), post_state.clone())
            .await;
        let response = call_backend_tool(service.peer(), routed_request, progress_token.clone(), cx.ct.clone()).await;
        service.service().stop_tracking_tool_call(progress_token.clone()).await;

        let response = response.map_err(|error| backend_forward_error("call_tool", &service_name, &error))?;
        let response = match (&self.plugin_runtime, post_state) {
            (Some(plugin_runtime), Some(post_state)) => {
                plugin_runtime.after_tool_call(&tool_name, response, Some(post_state)).await?
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

        let responses = fan_out_list(
            backend_transports,
            "list_resources",
            |response: &ListResourcesResult| response.resources.len(),
            |service| {
                let request = request.clone();
                async move { service.list_resources(request).await }
            },
        )
        .await;

        Ok(ListResourcesResult { meta: None, resources: merge_resources(responses), next_cursor: None })
    }

    async fn read_resource(
        &self,
        request: ReadResourceRequestParams,
        cx: RequestContext<RoleServer>,
    ) -> Result<ReadResourceResult, ErrorData> {
        let mcp_call_validator = AuthorizedCallValidator::new("read_resource", &cx);
        let (virtual_host, session_id, claims) = mcp_call_validator.validate()?;
        let session_manager = SessionManager::new(virtual_host, session_id, claims.sub.as_str(), &self.transports);

        let (service_name, service, resource_uri) = route_prefixed_name(
            &session_manager,
            "read_resource",
            &request.uri,
            "Routing problem... wrong resource name",
        )
        .await?;

        let mut routed_request = request;
        routed_request.uri = resource_uri;
        let response = service
            .read_resource(routed_request)
            .await
            .map_err(|error| backend_forward_error("read_resource", &service_name, &error))?;
        info!("read_resource: backend {service_name} returned {} contents", response.contents.len());
        Ok(response)
    }

    async fn list_resource_templates(
        &self,
        request: Option<PaginatedRequestParams>,
        cx: RequestContext<RoleServer>,
    ) -> Result<ListResourceTemplatesResult, ErrorData> {
        let mcp_call_validator = AuthorizedCallValidator::new("list_resource_templates", &cx);
        let (virtual_host, session_id, claims) = mcp_call_validator.validate()?;

        let session_manager = SessionManager::new(virtual_host, session_id, claims.sub.as_str(), &self.transports);
        let backend_transports: Vec<_> = session_manager.borrow_transports().await;

        let responses = fan_out_list(
            backend_transports,
            "list_resource_templates",
            |response: &ListResourceTemplatesResult| response.resource_templates.len(),
            |service| {
                let request = request.clone();
                async move { service.list_resource_templates(request).await }
            },
        )
        .await;

        Ok(ListResourceTemplatesResult {
            meta: None,
            resource_templates: merge_resource_templates(responses),
            next_cursor: None,
        })
    }

    async fn subscribe(
        &self,
        request: SubscribeRequestParams,
        cx: RequestContext<RoleServer>,
    ) -> Result<(), ErrorData> {
        let mcp_call_validator = AuthorizedCallValidator::new("subscribe", &cx);
        let (virtual_host, session_id, claims) = mcp_call_validator.validate()?;
        let session_manager = SessionManager::new(virtual_host, session_id, claims.sub.as_str(), &self.transports);

        let (service_name, service, resource_uri) =
            route_prefixed_name(&session_manager, "subscribe", &request.uri, "Routing problem... wrong resource name")
                .await?;

        let mut routed_request = request;
        routed_request.uri = resource_uri.clone();
        service.service().track_resource_subscription(&resource_uri, cx.peer.clone()).await;

        if let Err(error) = service.subscribe(routed_request).await {
            service.service().stop_tracking_resource_subscription(&resource_uri).await;
            return Err(backend_forward_error("subscribe", &service_name, &error));
        }
        info!("subscribe: backend {service_name} completed");
        Ok(())
    }

    async fn unsubscribe(
        &self,
        request: UnsubscribeRequestParams,
        cx: RequestContext<RoleServer>,
    ) -> Result<(), ErrorData> {
        let mcp_call_validator = AuthorizedCallValidator::new("unsubscribe", &cx);
        let (virtual_host, session_id, claims) = mcp_call_validator.validate()?;
        let session_manager = SessionManager::new(virtual_host, session_id, claims.sub.as_str(), &self.transports);

        let (service_name, service, resource_uri) = route_prefixed_name(
            &session_manager,
            "unsubscribe",
            &request.uri,
            "Routing problem... wrong resource name",
        )
        .await?;

        let mut routed_request = request;
        routed_request.uri = resource_uri.clone();
        service
            .unsubscribe(routed_request)
            .await
            .map_err(|error| backend_forward_error("unsubscribe", &service_name, &error))?;
        service.service().stop_tracking_resource_subscription(&resource_uri).await;
        info!("unsubscribe: backend {service_name} completed");
        Ok(())
    }

    async fn list_prompts(
        &self,
        request: Option<PaginatedRequestParams>,
        cx: RequestContext<RoleServer>,
    ) -> Result<ListPromptsResult, ErrorData> {
        let mcp_call_validator = AuthorizedCallValidator::new("list_prompts", &cx);
        let (virtual_host, session_id, claims) = mcp_call_validator.validate()?;

        let session_manager = SessionManager::new(virtual_host, session_id, claims.sub.as_str(), &self.transports);
        let backend_transports: Vec<_> = session_manager.borrow_transports().await;

        let responses = fan_out_list(
            backend_transports,
            "list_prompts",
            |response: &ListPromptsResult| response.prompts.len(),
            |service| {
                let request = request.clone();
                async move { service.list_prompts(request).await }
            },
        )
        .await;

        Ok(ListPromptsResult { meta: None, prompts: merge_prompts(responses), next_cursor: None })
    }

    async fn get_prompt(
        &self,
        request: GetPromptRequestParams,
        cx: RequestContext<RoleServer>,
    ) -> Result<GetPromptResult, ErrorData> {
        let mcp_call_validator = AuthorizedCallValidator::new("get_prompt", &cx);
        let (virtual_host, session_id, claims) = mcp_call_validator.validate()?;
        let session_manager = SessionManager::new(virtual_host, session_id, claims.sub.as_str(), &self.transports);

        let (service_name, service, prompt_name) =
            route_prefixed_name(&session_manager, "get_prompt", &request.name, "Routing problem... wrong prompt name")
                .await?;

        let mut routed_request = request;
        routed_request.name = prompt_name;
        let response = service
            .get_prompt(routed_request)
            .await
            .map_err(|error| backend_forward_error("get_prompt", &service_name, &error))?;
        info!("get_prompt: backend {service_name} returned {} messages", response.messages.len());
        Ok(response)
    }

    async fn complete(
        &self,
        request: CompleteRequestParams,
        cx: RequestContext<RoleServer>,
    ) -> Result<CompleteResult, ErrorData> {
        let mcp_call_validator = AuthorizedCallValidator::new("complete", &cx);
        let (virtual_host, session_id, claims) = mcp_call_validator.validate()?;
        let session_manager = SessionManager::new(virtual_host, session_id, claims.sub.as_str(), &self.transports);

        // The reference carries a namespaced prompt name or resource URI; route on that.
        let namespaced = match &request.r#ref {
            Reference::Prompt(prompt) => prompt.name.as_str(),
            Reference::Resource(resource) => resource.uri.as_str(),
        };

        let (service_name, service, stripped) = route_prefixed_name(
            &session_manager,
            "complete",
            namespaced,
            "Routing problem... wrong completion reference",
        )
        .await?;

        let mut routed_request = request;
        match &mut routed_request.r#ref {
            Reference::Prompt(prompt) => prompt.name = stripped,
            Reference::Resource(resource) => resource.uri = stripped,
        }
        let response = service
            .complete(routed_request)
            .await
            .map_err(|error| backend_forward_error("complete", &service_name, &error))?;
        info!("complete: backend {service_name} returned {} values", response.completion.values.len());
        Ok(response)
    }
}

/// Splits a `{backend}-{rest}` routing name, returning `(backend_name, rest)` for the first
/// backend whose name is a `-`-delimited prefix. Shared by tool, resource, and prompt routing.
fn split_prefixed_name<'a, N: AsRef<str>>(name: &'a str, backend_names: &'a [N]) -> Option<(&'a str, &'a str)> {
    backend_names.iter().find_map(|backend| {
        let backend = backend.as_ref();
        name.strip_prefix(backend)?.strip_prefix('-').map(|rest| (backend, rest))
    })
}

/// Joins a backend name and a backend-local name into the namespaced `{backend}-{rest}` form.
/// Inverse of [`split_prefixed_name`]; together they own the naming convention.
pub(crate) fn prefixed_name(backend_name: &str, rest: &str) -> String {
    format!("{backend_name}-{rest}")
}

/// Resolves an exact control-plane alias to its backend and upstream name.
/// Older configs without aliases retain the legacy `{backend}-{tool}` route.
fn resolve_tool_route<'a, N: AsRef<str>>(
    virtual_host: &'a VirtualHost,
    name: &'a str,
    backend_names: &'a [N],
) -> Option<(&'a str, &'a str)> {
    let mut aliases = backend_names.iter().filter_map(|backend_name| {
        let backend_name = backend_name.as_ref();
        let original_name = virtual_host.backends.get(backend_name)?.tool_name_aliases.get(name)?;
        Some((backend_name, original_name.as_str()))
    });
    let alias = aliases.next();
    if aliases.next().is_some() {
        return None;
    }
    alias.or_else(|| split_prefixed_name(name, backend_names))
}

/// Returns the control-plane name for an upstream tool, with the legacy
/// namespaced form as a fallback for configs published before aliases existed.
fn exposed_tool_name(virtual_host: &VirtualHost, backend_name: &str, original_name: &str) -> String {
    virtual_host
        .backends
        .get(backend_name)
        .and_then(|backend| {
            backend
                .tool_name_aliases
                .iter()
                .find_map(|(alias, original)| (original == original_name).then(|| alias.clone()))
        })
        .unwrap_or_else(|| prefixed_name(backend_name, original_name))
}

/// Logs a backend forwarding failure and maps it to the routing error every handler returns.
fn backend_forward_error(op: &str, backend_name: &str, error: &impl std::fmt::Debug) -> ErrorData {
    warn!("{op}: backend {backend_name} {error:?}");
    ErrorData {
        code: ErrorCode::INTERNAL_ERROR,
        message: "Routing problem... got no responses from backends".into(),
        data: None,
    }
}

/// Fans a paginated list request out to every connected backend concurrently, logs each response,
/// and returns the `(backend_name, result)` pairs that succeeded.
async fn fan_out_list<R, E, F, Fut, C>(
    backends: Vec<ServiceHolder>,
    op: &str,
    item_count: C,
    call: F,
) -> Vec<(String, R)>
where
    F: Fn(McpClientService) -> Fut,
    Fut: std::future::Future<Output = Result<R, E>>,
    C: Fn(&R) -> usize,
    E: std::fmt::Debug,
{
    let tasks = backends.into_iter().map(|service_holder| {
        let call = &call;
        async move {
            let response = match service_holder.running_service {
                Some(service) => Some(call(service).await),
                None => None,
            };
            (service_holder.name, response)
        }
    });

    futures::future::join_all(tasks)
        .await
        .into_iter()
        .filter_map(|(name, response)| {
            log_list_backend_response(op, &name, response.as_ref(), &item_count);
            match response {
                Some(Ok(response)) => Some((name, response)),
                _ => None,
            }
        })
        .collect()
}

/// Routes a namespaced `{backend}-{rest}` name: splits it against the session's backend names
/// and resolves the owning backend, returning `(backend_name, service, rest)`. Shared by tool,
/// resource, prompt, and completion routing.
async fn route_prefixed_name(
    session_manager: &SessionManager<'_>,
    op: &str,
    namespaced: &str,
    no_route_message: &'static str,
) -> Result<(String, McpClientService, String), ErrorData> {
    let backend_names = session_manager.get_backend_names();
    let Some((backend_name, rest)) = split_prefixed_name(namespaced, &backend_names) else {
        return Err(ErrorData { code: ErrorCode::INTERNAL_ERROR, message: no_route_message.into(), data: None });
    };
    let rest = rest.to_owned();
    let (backend_name, service) = resolve_backend(session_manager, op, backend_name).await?;
    Ok((backend_name, service, rest))
}

/// Resolves the single connected backend named `backend_name` and takes its running service.
/// Shared by tool, resource, and prompt routing so they reject duplicate or missing backends
/// the same way; a duplicate match means the session is invalid, so it is cleaned up.
async fn resolve_backend(
    session_manager: &SessionManager<'_>,
    op: &str,
    backend_name: &str,
) -> Result<(String, McpClientService), ErrorData> {
    let backend_transports = session_manager.borrow_transports().await;
    debug!("{op}: resolving backend {backend_name} from {backend_transports:?}");

    let mut target = None;
    for service_holder in backend_transports {
        if service_holder.name == backend_name {
            if target.is_some() {
                warn!("{op}: more than one backend matching {backend_name}");
                session_manager.cleanup_backends("invalid session.. duplicate backends detected").await;
                return Err(ErrorData {
                    code: ErrorCode::INVALID_REQUEST,
                    message: "Routing problem... multiple matching backends".into(),
                    data: None,
                });
            }
            target = Some(service_holder);
        }
    }

    let Some(ServiceHolder { name, running_service }) = target else {
        return Err(ErrorData {
            code: ErrorCode::INTERNAL_ERROR,
            message: "Routing problem... got no responses from backends".into(),
            data: None,
        });
    };
    let Some(service) = running_service else {
        warn!("{op}: no running backend for {backend_name}");
        return Err(ErrorData {
            code: ErrorCode::INTERNAL_ERROR,
            message: "Routing problem... got no responses from backends".into(),
            data: None,
        });
    };
    Ok((name, service))
}

fn merge_capabilities(_server_capabilities: Vec<(String, Option<ServerCapabilities>)>) -> ServerCapabilities {
    ServerCapabilities::builder()
        .enable_completions()
        .enable_prompts()
        .enable_resources()
        .enable_resources_subscribe()
        .enable_tools()
        .build()
}

fn log_list_backend_response<T, E: std::fmt::Debug>(
    kind: &str,
    name: &str,
    response: Option<&Result<T, E>>,
    item_count: impl Fn(&T) -> usize,
) {
    match response {
        Some(Ok(response)) => info!("{kind}: backend {name} completed ({} items)", item_count(response)),
        Some(Err(error)) => warn!("{kind}: backend {name} {error:?}"),
        None => info!("{kind}: backend {name} unavailable"),
    }
}

fn merge_tools(tools: Vec<(String, ListToolsResult)>, virtual_host: &VirtualHost) -> Vec<Tool> {
    tools
        .into_iter()
        .flat_map(|(backend_name, result)| {
            result
                .tools
                .into_iter()
                .map(|mut t| {
                    t.name = exposed_tool_name(virtual_host, &backend_name, &t.name).into();
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
                    t.name = prefixed_name(&backend_name, &t.name);
                    t.uri = prefixed_name(&backend_name, &t.uri);
                    t
                })
                .collect::<Vec<_>>()
        })
        .sorted_by(|t, o| t.name.cmp(&o.name))
        .collect::<Vec<_>>()
}

fn merge_resource_templates(templates: Vec<(String, ListResourceTemplatesResult)>) -> Vec<ResourceTemplate> {
    templates
        .into_iter()
        .flat_map(|(backend_name, result)| {
            result.resource_templates.into_iter().map(move |mut template| {
                template.name = prefixed_name(&backend_name, &template.name);
                template.uri_template = prefixed_name(&backend_name, &template.uri_template);
                template
            })
        })
        .sorted_by(|template, other| template.name.cmp(&other.name))
        .collect::<Vec<_>>()
}

fn merge_prompts(prompts: Vec<(String, ListPromptsResult)>) -> Vec<Prompt> {
    prompts
        .into_iter()
        .flat_map(|(backend_name, result)| {
            result.prompts.into_iter().map(move |mut prompt| {
                prompt.name = prefixed_name(&backend_name, &prompt.name);
                prompt
            })
        })
        .sorted_by(|prompt, other| prompt.name.cmp(&other.name))
        .collect::<Vec<_>>()
}

#[cfg(test)]
mod tests {
    // Note this useful idiom: importing names from outer (for mod tests) scope.
    use super::*;

    #[test]
    fn test_splitting() {
        let backend_names = vec!["counter-on", "counter-oneee", "counter-one"];
        assert_eq!(Some(("counter-one", "increment")), split_prefixed_name("counter-one-increment", &backend_names));
        assert_eq!(None, split_prefixed_name("counter-oneincrement", &backend_names));
        assert_eq!(None, split_prefixed_name("counteroneincrement", &backend_names));
        assert_eq!(Some(("counter-one", "get-value")), split_prefixed_name("counter-one-get-value", &backend_names));

        // Tool, resource, and prompt routing all share this splitter.
        assert_eq!(
            Some(("counter-one", "example-prompt")),
            split_prefixed_name("counter-one-example-prompt", &backend_names)
        );
        assert_eq!(None, split_prefixed_name("counter-oneexample-prompt", &backend_names));

        let backend_names = vec!["counter_on", "counter_oneee", "counter_one"];
        assert_eq!(Some(("counter_one", "get-value")), split_prefixed_name("counter_one-get-value", &backend_names));
    }

    #[test]
    fn test_control_plane_alias_is_advertised_and_routes_to_original_name() {
        let config_json = serde_json::json!({
            "backends": {
                "79fabb70-2188-4de8-95ed-dc1e976e14d4": {
                    "name": "compliance_reference",
                    "url": "http://upstream:9000/mcp",
                    "transport": "STREAMABLEHTTP",
                    "passthrough_headers": [],
                    "allowed_tool_names": ["get_stats", "echo"],
                    "tool_name_aliases": {
                        "Public.Tool": "get_stats",
                        "Echo_Tool": "echo"
                    },
                    "allowed_resource_names": [],
                    "allowed_prompt_names": []
                }
            }
        });
        let virtual_host: VirtualHost = serde_json::from_value(config_json).expect("valid virtual host");
        let backend_ids = vec!["79fabb70-2188-4de8-95ed-dc1e976e14d4"];

        assert_eq!(
            "Public.Tool",
            exposed_tool_name(&virtual_host, "79fabb70-2188-4de8-95ed-dc1e976e14d4", "get_stats")
        );
        assert_eq!(
            Some(("79fabb70-2188-4de8-95ed-dc1e976e14d4", "get_stats")),
            resolve_tool_route(&virtual_host, "Public.Tool", &backend_ids)
        );
    }

    #[test]
    fn test_tool_routing_falls_back_to_legacy_prefixed_names() {
        let config_json = serde_json::json!({
            "backends": {
                "compliance-reference": {
                    "name": "compliance_reference",
                    "url": "http://upstream:9000/mcp",
                    "transport": "STREAMABLEHTTP",
                    "passthrough_headers": [],
                    "allowed_tool_names": ["get_stats"],
                    "allowed_resource_names": [],
                    "allowed_prompt_names": []
                }
            }
        });
        let virtual_host: VirtualHost = serde_json::from_value(config_json).expect("valid virtual host");
        let backend_names = vec!["compliance-reference"];

        assert_eq!(
            "compliance-reference-get_stats",
            exposed_tool_name(&virtual_host, "compliance-reference", "get_stats")
        );
        assert_eq!(
            Some(("compliance-reference", "get_stats")),
            resolve_tool_route(&virtual_host, "compliance-reference-get_stats", &backend_names)
        );
    }
}
