mod completion;
mod initialization;
mod prompts;
mod resources;
mod tools;

use contextforge_data_plane_cpex::GatewayPluginRuntimeHandle;
use rmcp::{
    ErrorData, RoleServer, ServerHandler,
    model::{
        CallToolRequestParams, CallToolResponse, CompleteRequestParams, CompleteResult, ErrorCode,
        GetPromptRequestParams, GetPromptResponse, InitializeRequestParams, InitializeResult, ListPromptsResult,
        ListResourceTemplatesResult, ListResourcesResult, ListToolsResult, PaginatedRequestParams,
        ReadResourceRequestParams, ReadResourceResponse, SubscribeRequestParams, UnsubscribeRequestParams,
    },
    service::RequestContext,
};
use typed_builder::TypedBuilder;

#[derive(Clone, TypedBuilder)]
#[builder(field_defaults(setter(prefix = "with_")))]
pub struct McpService {
    http_client: reqwest::Client,
    #[builder(default)]
    plugin_runtime: Option<GatewayPluginRuntimeHandle>,
}

#[allow(clippy::unused_async_trait_impl)]
impl ServerHandler for McpService {
    async fn initialize(
        &self,
        request: InitializeRequestParams,
        cx: RequestContext<RoleServer>,
    ) -> Result<InitializeResult, ErrorData> {
        initialization::initialize(self, request, cx).await
    }

    async fn list_tools(
        &self,
        _: Option<PaginatedRequestParams>,
        _: RequestContext<RoleServer>,
    ) -> Result<ListToolsResult, ErrorData> {
        Err(ErrorData {
            code: ErrorCode::INVALID_REQUEST,
            message: "Fan out not supported at the moment. Go to control plane".into(),
            data: None,
        })
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
        _: Option<PaginatedRequestParams>,
        _: RequestContext<RoleServer>,
    ) -> Result<ListResourcesResult, ErrorData> {
        async {}.await;
        Err(ErrorData {
            code: ErrorCode::INVALID_REQUEST,
            message: "Fan out not supported at the moment. Go to control plane".into(),
            data: None,
        })
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
        _: Option<PaginatedRequestParams>,
        _: RequestContext<RoleServer>,
    ) -> Result<ListResourceTemplatesResult, ErrorData> {
        Err(ErrorData {
            code: ErrorCode::INVALID_REQUEST,
            message: "Fan out not supported at the moment. Go to control plane".into(),
            data: None,
        })
    }

    async fn subscribe(&self, _: SubscribeRequestParams, _: RequestContext<RoleServer>) -> Result<(), ErrorData> {
        Err(ErrorData {
            code: ErrorCode::INVALID_REQUEST,
            message: "Fan out not supported at the moment. Go to control plane".into(),
            data: None,
        })
    }

    async fn unsubscribe(&self, _: UnsubscribeRequestParams, _: RequestContext<RoleServer>) -> Result<(), ErrorData> {
        Err(ErrorData {
            code: ErrorCode::INVALID_REQUEST,
            message: "Fan out not supported at the moment. Go to control plane".into(),
            data: None,
        })
    }

    async fn list_prompts(
        &self,
        _: Option<PaginatedRequestParams>,
        _: RequestContext<RoleServer>,
    ) -> Result<ListPromptsResult, ErrorData> {
        Err(ErrorData {
            code: ErrorCode::INVALID_REQUEST,
            message: "Fan out not supported at the moment. Go to control plane".into(),
            data: None,
        })
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
        _: CompleteRequestParams,
        _: RequestContext<RoleServer>,
    ) -> Result<CompleteResult, ErrorData> {
        Err(ErrorData {
            code: ErrorCode::INVALID_REQUEST,
            message: "Fan out not supported at the moment. Go to control plane".into(),
            data: None,
        })
    }
}
