use std::{collections::HashMap, sync::Arc};

use contextforge_gateway_rs_cpex::{GatewayPluginRuntimeHandle, ToolPreCallResult};
use rmcp::{
    ErrorData, RoleClient, RoleServer, ServerHandler, ServiceExt,
    model::{
        CallToolRequestParams, CallToolResponse, CompleteRequestParams, CompleteResult, ErrorCode,
        GetPromptRequestParams, GetPromptResponse, Implementation, InitializeRequestParams, InitializeResult,
        ListPromptsResult, ListResourceTemplatesResult, ListResourcesResult, ListToolsResult, PaginatedRequestParams,
        ReadResourceRequestParams, ReadResourceResponse, Reference, ServerCapabilities, SubscribeRequestParams,
        UnsubscribeRequestParams,
    },
    service::{RequestContext, RunningService},
    transport::{StreamableHttpClientTransport, streamable_http_client::StreamableHttpClientTransportConfig},
};
use tracing::{info, warn};
use typed_builder::TypedBuilder;

use super::{
    backend_client::{GatewayBackendClient, call_backend_tool},
    backend_transports::{BackendTransportKey, BackendTransportService, BackendTransports},
    identifier_routing::{backend_forward_error, resolve_backend, resolve_tool_route, route_identifier_to_backend},
    list_aggregation::{fan_out_list, merge_prompts, merge_resource_templates, merge_resources, merge_tools},
    mcp_call_validator::AuthorizedCallValidator,
};
use crate::gateway::{
    mcp_call_validator::InitializeCallValidator,
    session_manager::SessionManager,
    session_store::{UserSession, UserSessionStore},
};

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

        let namespace_identifiers = virtual_host.backends.len() > 1;
        let tasks: Vec<_> = virtual_host
            .backends
            .iter()
            .map(|(name, backend)| {
                let client = self.http_client.clone();
                let backend_client = GatewayBackendClient::new(
                    name.clone(),
                    namespace_identifiers,
                    request.clone(),
                    self.plugin_runtime.clone(),
                );
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

        Ok(ListToolsResult::with_all_items(merge_tools(responses, virtual_host)))
    }

    async fn call_tool(
        &self,
        request: CallToolRequestParams,
        cx: RequestContext<RoleServer>,
    ) -> Result<CallToolResponse, ErrorData> {
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
        let handle = service
            .service()
            .start_tool_call(
                service.peer(),
                routed_request,
                progress_token,
                tool_name.clone(),
                cx.peer.clone(),
                post_state.clone(),
            )
            .await
            .map_err(|error| backend_forward_error("call_tool", &service_name, &error))?;
        let backend_progress_token = handle.progress_token.clone();
        let response = call_backend_tool(handle, cx.ct.clone()).await;
        service.service().stop_tracking_tool_call(&backend_progress_token).await;

        let response = response.map_err(|error| backend_forward_error("call_tool", &service_name, &error))?;
        let response = match (&self.plugin_runtime, post_state) {
            (Some(plugin_runtime), Some(post_state)) => {
                plugin_runtime.after_tool_call(&tool_name, response, Some(post_state)).await?
            },
            _ => response,
        };
        info!("call_tool: backend {service_name} completed");
        Ok(response.into())
    }

    async fn list_resources(
        &self,
        request: Option<PaginatedRequestParams>,
        cx: RequestContext<RoleServer>,
    ) -> Result<ListResourcesResult, ErrorData> {
        let mcp_call_validator = AuthorizedCallValidator::new("list_resources", &cx);
        let (virtual_host, session_id, claims) = mcp_call_validator.validate()?;
        let namespace_identifiers = virtual_host.backends.len() > 1;

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

        Ok(ListResourcesResult::with_all_items(merge_resources(responses, namespace_identifiers)))
    }

    async fn read_resource(
        &self,
        request: ReadResourceRequestParams,
        cx: RequestContext<RoleServer>,
    ) -> Result<ReadResourceResponse, ErrorData> {
        let mcp_call_validator = AuthorizedCallValidator::new("read_resource", &cx);
        let (virtual_host, session_id, claims) = mcp_call_validator.validate()?;
        let session_manager = SessionManager::new(virtual_host, session_id, claims.sub.as_str(), &self.transports);

        let (service_name, service, resource_uri) = route_identifier_to_backend(
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
        Ok(response.into())
    }

    async fn list_resource_templates(
        &self,
        request: Option<PaginatedRequestParams>,
        cx: RequestContext<RoleServer>,
    ) -> Result<ListResourceTemplatesResult, ErrorData> {
        let mcp_call_validator = AuthorizedCallValidator::new("list_resource_templates", &cx);
        let (virtual_host, session_id, claims) = mcp_call_validator.validate()?;
        let namespace_identifiers = virtual_host.backends.len() > 1;

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

        Ok(ListResourceTemplatesResult::with_all_items(merge_resource_templates(responses, namespace_identifiers)))
    }

    async fn subscribe(
        &self,
        request: SubscribeRequestParams,
        cx: RequestContext<RoleServer>,
    ) -> Result<(), ErrorData> {
        let mcp_call_validator = AuthorizedCallValidator::new("subscribe", &cx);
        let (virtual_host, session_id, claims) = mcp_call_validator.validate()?;
        let session_manager = SessionManager::new(virtual_host, session_id, claims.sub.as_str(), &self.transports);

        let (service_name, service, resource_uri) = route_identifier_to_backend(
            &session_manager,
            "subscribe",
            &request.uri,
            "Routing problem... wrong resource name",
        )
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

        let (service_name, service, resource_uri) = route_identifier_to_backend(
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
        let namespace_identifiers = virtual_host.backends.len() > 1;

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

        Ok(ListPromptsResult::with_all_items(merge_prompts(responses, namespace_identifiers)))
    }

    async fn get_prompt(
        &self,
        request: GetPromptRequestParams,
        cx: RequestContext<RoleServer>,
    ) -> Result<GetPromptResponse, ErrorData> {
        let mcp_call_validator = AuthorizedCallValidator::new("get_prompt", &cx);
        let (virtual_host, session_id, claims) = mcp_call_validator.validate()?;
        let session_manager = SessionManager::new(virtual_host, session_id, claims.sub.as_str(), &self.transports);

        let (service_name, service, prompt_name) = route_identifier_to_backend(
            &session_manager,
            "get_prompt",
            &request.name,
            "Routing problem... wrong prompt name",
        )
        .await?;

        let mut routed_request = request;
        routed_request.name = prompt_name;
        let response = service
            .get_prompt(routed_request)
            .await
            .map_err(|error| backend_forward_error("get_prompt", &service_name, &error))?;
        info!("get_prompt: backend {service_name} returned {} messages", response.messages.len());
        Ok(response.into())
    }

    async fn complete(
        &self,
        request: CompleteRequestParams,
        cx: RequestContext<RoleServer>,
    ) -> Result<CompleteResult, ErrorData> {
        let mcp_call_validator = AuthorizedCallValidator::new("complete", &cx);
        let (virtual_host, session_id, claims) = mcp_call_validator.validate()?;
        let session_manager = SessionManager::new(virtual_host, session_id, claims.sub.as_str(), &self.transports);

        let identifier = match &request.r#ref {
            Reference::Prompt(prompt) => prompt.name.as_str(),
            Reference::Resource(resource) => resource.uri.as_str(),
            _ => return Err(ErrorData::invalid_params("Unsupported completion reference", None)),
        };

        let (service_name, service, routed_identifier) = route_identifier_to_backend(
            &session_manager,
            "complete",
            identifier,
            "Routing problem... wrong completion reference",
        )
        .await?;

        let mut routed_request = request;
        match &mut routed_request.r#ref {
            Reference::Prompt(prompt) => prompt.name = routed_identifier,
            Reference::Resource(resource) => resource.uri = routed_identifier,
            _ => return Err(ErrorData::invalid_params("Unsupported completion reference", None)),
        }
        let response = service
            .complete(routed_request)
            .await
            .map_err(|error| backend_forward_error("complete", &service_name, &error))?;
        info!("complete: backend {service_name} returned {} values", response.completion.values.len());
        Ok(response)
    }
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
