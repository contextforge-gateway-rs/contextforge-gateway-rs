mod completion;
mod initialization;
mod prompts;
mod resources;
mod tools;

use std::{borrow::Cow, collections::HashSet};

use contextforge_data_plane_apis::user_store::VirtualHost;
use contextforge_data_plane_cpex::GatewayPluginRuntimeHandle;
use rmcp::{
    ErrorData, RoleServer, ServerHandler,
    model::{
        CallToolRequestParams, CallToolResponse, CompleteRequestParams, CompleteResult, GetPromptRequestParams,
        GetPromptResponse, Implementation, InitializeRequestParams, InitializeResult, ListPromptsResult,
        ListResourceTemplatesResult, ListResourcesResult, ListToolsResult, PaginatedRequestParams, ProtocolVersion,
        ReadResourceRequestParams, ReadResourceResponse, ServerCapabilities, ServerInfo, SubscribeRequestParams,
        SubscriptionFilter, UnsubscribeRequestParams,
    },
    service::{RequestContext, SubscriptionContext},
};
use typed_builder::TypedBuilder;

use super::{DownstreamSubscriptionRegistry, backend_transports::BackendTransports, session_store::UserSessionStore};
use crate::layers::request_context::GatewayRequestContext;

const SUPPORTED_PROTOCOL_VERSIONS: &[ProtocolVersion] = &[ProtocolVersion::V_2026_07_28];

#[derive(Clone, TypedBuilder)]
#[builder(
    field_defaults(setter(prefix = "with_")),
    builder_method(vis = "pub(crate)"),
    builder_type(vis = "pub(crate)"),
    build_method(vis = "pub(crate)")
)]
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
    #[builder(default, setter(skip))]
    gateway_context: Option<GatewayRequestContext>,
    #[builder(default, setter(skip))]
    capabilities: ServerCapabilities,
    #[builder(default, setter(skip))]
    downstream_subscriptions: DownstreamSubscriptionRegistry,
}

impl<T> McpService<T>
where
    T: UserSessionStore,
{
    pub(crate) fn with_gateway_request_context(mut self, gateway_context: Option<GatewayRequestContext>) -> Self {
        self.capabilities = gateway_context
            .as_ref()
            .map_or_else(ServerCapabilities::default, |context| capabilities_for_virtual_host(context.virtual_host()));
        self.gateway_context = gateway_context;
        self
    }

    pub(crate) fn with_downstream_subscription_registry(mut self, registry: DownstreamSubscriptionRegistry) -> Self {
        self.downstream_subscriptions = registry;
        self
    }

    #[cfg(test)]
    fn with_test_capabilities(mut self, capabilities: ServerCapabilities) -> Self {
        self.capabilities = capabilities;
        self
    }
}

impl<T> ServerHandler for McpService<T>
where
    T: UserSessionStore + Send + Sync + 'static,
{
    fn supported_protocol_versions(&self) -> Cow<'static, [ProtocolVersion]> {
        Cow::Borrowed(SUPPORTED_PROTOCOL_VERSIONS)
    }

    fn get_info(&self) -> ServerInfo {
        ServerInfo::new(self.capabilities.clone())
            .with_server_info(gateway_server_implementation())
            .with_protocol_version(ProtocolVersion::V_2026_07_28)
    }

    fn accepted_subscription_filter(&self, requested: &SubscriptionFilter) -> Option<SubscriptionFilter> {
        let gateway_context = self.gateway_context.as_ref()?;
        let mut accepted = requested.supported_by(&self.capabilities);
        accepted.resource_subscriptions = accepted.resource_subscriptions.take().and_then(|uris| {
            let mut seen = HashSet::new();
            let uris = uris
                .into_iter()
                .filter(|uri| {
                    seen.insert(uri.clone()) && is_routable_resource_subscription(gateway_context.virtual_host(), uri)
                })
                .collect::<Vec<_>>();
            (!uris.is_empty()).then_some(uris)
        });
        Some(accepted)
    }

    async fn listen(&self, context: SubscriptionContext) -> Result<(), ErrorData> {
        let Some(gateway_context) = self.gateway_context.as_ref() else {
            return Err(ErrorData::internal_error("subscriptions/listen missing gateway request context", None));
        };
        let _subscription_guard =
            self.downstream_subscriptions.register(gateway_context, context.accepted(), context.sink());

        context.cancelled().await;
        Ok(())
    }

    async fn initialize(
        &self,
        request: InitializeRequestParams,
        cx: RequestContext<RoleServer>,
    ) -> Result<InitializeResult, ErrorData> {
        initialization::initialize(self, request, cx).await
    }

    async fn ping(&self, _cx: RequestContext<RoleServer>) -> Result<(), ErrorData> {
        Ok(())
    }

    async fn list_tools(
        &self,
        request: Option<PaginatedRequestParams>,
        cx: RequestContext<RoleServer>,
    ) -> Result<ListToolsResult, ErrorData> {
        tools::list_tools(self, request, cx).await
    }

    async fn call_tool(
        &self,
        request: CallToolRequestParams,
        cx: RequestContext<RoleServer>,
    ) -> Result<CallToolResponse, ErrorData> {
        tools::call_tool(self, request, cx).await
    }

    async fn list_resources(
        &self,
        request: Option<PaginatedRequestParams>,
        cx: RequestContext<RoleServer>,
    ) -> Result<ListResourcesResult, ErrorData> {
        resources::list_resources(self, request, cx).await
    }

    async fn read_resource(
        &self,
        request: ReadResourceRequestParams,
        cx: RequestContext<RoleServer>,
    ) -> Result<ReadResourceResponse, ErrorData> {
        resources::read_resource(self, request, cx).await
    }

    async fn list_resource_templates(
        &self,
        request: Option<PaginatedRequestParams>,
        cx: RequestContext<RoleServer>,
    ) -> Result<ListResourceTemplatesResult, ErrorData> {
        resources::list_resource_templates(self, request, cx).await
    }

    async fn subscribe(
        &self,
        request: SubscribeRequestParams,
        cx: RequestContext<RoleServer>,
    ) -> Result<(), ErrorData> {
        resources::subscribe(self, request, cx).await
    }

    async fn unsubscribe(
        &self,
        request: UnsubscribeRequestParams,
        cx: RequestContext<RoleServer>,
    ) -> Result<(), ErrorData> {
        resources::unsubscribe(self, request, cx).await
    }

    async fn list_prompts(
        &self,
        request: Option<PaginatedRequestParams>,
        cx: RequestContext<RoleServer>,
    ) -> Result<ListPromptsResult, ErrorData> {
        prompts::list_prompts(self, request, cx).await
    }

    async fn get_prompt(
        &self,
        request: GetPromptRequestParams,
        cx: RequestContext<RoleServer>,
    ) -> Result<GetPromptResponse, ErrorData> {
        prompts::get_prompt(self, request, cx).await
    }

    async fn complete(
        &self,
        request: CompleteRequestParams,
        cx: RequestContext<RoleServer>,
    ) -> Result<CompleteResult, ErrorData> {
        completion::complete(self, request, cx).await
    }
}

pub(crate) fn capabilities_for_virtual_host(virtual_host: &VirtualHost) -> ServerCapabilities {
    if virtual_host.backends.is_empty() {
        return ServerCapabilities::default();
    }

    ServerCapabilities::builder()
        .enable_completions()
        .enable_prompts()
        .enable_prompts_list_changed()
        .enable_resources()
        .enable_resources_subscribe()
        .enable_resources_list_changed()
        .enable_tools()
        .enable_tool_list_changed()
        .build()
}

fn gateway_server_implementation() -> Implementation {
    Implementation::new("rust-conformance-server", "0.1.0")
}

fn is_routable_resource_subscription(virtual_host: &VirtualHost, uri: &str) -> bool {
    if virtual_host.backends.len() <= 1 {
        return !virtual_host.backends.is_empty();
    }

    virtual_host
        .backends
        .keys()
        .any(|backend_name| uri.strip_prefix(backend_name).and_then(|rest| rest.strip_prefix('-')).is_some())
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use contextforge_data_plane_apis::user_store::{BackendMCPGateway, Transport};
    use rmcp::model::RequestId;

    use super::*;
    use crate::gateway::session_store::LocalUserSessionStore;

    #[test]
    fn capabilities_for_non_empty_virtual_host_include_subscription_capabilities() {
        let virtual_host = virtual_host_with_backends(&["backend-one"]);

        let capabilities = capabilities_for_virtual_host(&virtual_host);

        assert!(capabilities.completions.is_some());
        assert_eq!(Some(true), capabilities.tools.and_then(|tools| tools.list_changed));
        assert_eq!(Some(true), capabilities.prompts.and_then(|prompts| prompts.list_changed));
        let resources = capabilities.resources.expect("resources are enabled");
        assert_eq!(Some(true), resources.subscribe);
        assert_eq!(Some(true), resources.list_changed);
    }

    #[test]
    fn accepted_subscription_filter_keeps_only_routable_resource_uris() {
        let virtual_host = virtual_host_with_backends(&["backend-one", "backend-two"]);
        let gateway_context = GatewayRequestContext::new(&test_claims(), &test_virtual_host_id(), &virtual_host);
        let service = test_service(Some(gateway_context), capabilities_for_virtual_host(&virtual_host));
        let requested = SubscriptionFilter::builder()
            .tools_list_changed()
            .prompts_list_changed()
            .resources_list_changed()
            .resource_subscription("backend-one-memo://known")
            .resource_subscription("missing-memo://unknown")
            .resource_subscription("backend-one-memo://known")
            .build();

        let accepted = service.accepted_subscription_filter(&requested).expect("subscriptions/listen is implemented");

        assert_eq!(Some(true), accepted.tools_list_changed);
        assert_eq!(Some(true), accepted.prompts_list_changed);
        assert_eq!(Some(true), accepted.resources_list_changed);
        assert_eq!(Some(vec!["backend-one-memo://known".to_owned()]), accepted.resource_subscriptions);
    }

    #[test]
    fn accepted_subscription_filter_is_not_implemented_without_gateway_context() {
        let service =
            test_service(None, ServerCapabilities::builder().enable_tools().enable_tool_list_changed().build());
        let requested = SubscriptionFilter::builder().tools_list_changed().build();

        assert!(service.accepted_subscription_filter(&requested).is_none());
    }

    #[test]
    fn subscription_filter_for_empty_vhost_accepts_no_notifications() {
        let virtual_host = VirtualHost { backends: HashMap::new() };
        let gateway_context = GatewayRequestContext::new(&test_claims(), &test_virtual_host_id(), &virtual_host);
        let service = test_service(Some(gateway_context), capabilities_for_virtual_host(&virtual_host));
        let requested = SubscriptionFilter::builder()
            .tools_list_changed()
            .prompts_list_changed()
            .resources_list_changed()
            .resource_subscription("memo://known")
            .build();

        let accepted = service.accepted_subscription_filter(&requested).expect("context exists");

        assert_eq!(None, accepted.tools_list_changed);
        assert_eq!(None, accepted.prompts_list_changed);
        assert_eq!(None, accepted.resources_list_changed);
        assert_eq!(None, accepted.resource_subscriptions);
    }

    fn test_service(
        gateway_context: Option<GatewayRequestContext>,
        capabilities: ServerCapabilities,
    ) -> McpService<LocalUserSessionStore> {
        McpService::builder()
            .with_user_session_store(LocalUserSessionStore::new())
            .with_http_client(reqwest::Client::new())
            .build()
            .with_gateway_request_context(gateway_context)
            .with_test_capabilities(capabilities)
    }

    fn virtual_host_with_backends(names: &[&str]) -> VirtualHost {
        VirtualHost {
            backends: names
                .iter()
                .map(|name| {
                    (
                        (*name).to_owned(),
                        BackendMCPGateway {
                            name: (*name).to_owned(),
                            url: "http://127.0.0.1:9999/mcp".parse().expect("valid URL"),
                            transport: Transport::default(),
                            passthrough_headers: Vec::new(),
                            add_headers: HashMap::new(),
                            remove_headers: Vec::new(),
                            allowed_tool_names: Vec::new(),
                            tool_name_aliases: HashMap::new(),
                            allowed_resource_names: Vec::new(),
                            allowed_prompt_names: Vec::new(),
                        },
                    )
                })
                .collect(),
        }
    }

    fn test_claims() -> crate::common::ContextForgeClaims {
        crate::common::ContextForgeClaims {
            sub: "test-principal".to_owned(),
            jti: "test-jti".to_owned(),
            token_use: None,
            iat: None,
            iss: "test-issuer".to_owned(),
            aud: "test-audience".to_owned(),
            exp: 1,
            teams: None,
            user: crate::common::User::builder()
                .email("test@example.com".to_owned())
                .full_name(None)
                .is_admin(false)
                .auth_provider("test".to_owned())
                .build(),
            scopes: None,
        }
    }

    fn test_virtual_host_id() -> crate::layers::virtual_host_id::VirtualHostId {
        crate::layers::virtual_host_id::VirtualHostId::new("test-vhost".to_owned())
    }

    #[test]
    fn registry_keys_include_subscription_id_and_notification_kind() {
        let virtual_host = virtual_host_with_backends(&["backend-one"]);
        let gateway_context = GatewayRequestContext::new(&test_claims(), &test_virtual_host_id(), &virtual_host);
        let filter = SubscriptionFilter::builder().tools_list_changed().resource_subscription("memo://known").build();
        let keys = super::super::downstream_subscriptions::subscription_keys(
            &gateway_context,
            &filter,
            &RequestId::Number(7),
            0,
        );

        assert_eq!(2, keys.len());
    }
}
