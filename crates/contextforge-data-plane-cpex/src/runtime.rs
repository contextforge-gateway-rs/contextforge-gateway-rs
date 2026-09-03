use std::sync::{
    Arc,
    atomic::{AtomicU64, Ordering},
};

use cpex::cpex_core::{
    cmf::{CmfHook, MessagePayload},
    config::CpexConfig,
    context::PluginContextTable,
    executor::PipelineResult,
    factory::PluginFactoryRegistry,
    hooks::{payload::Extensions, types::cmf_hook_names},
    manager::PluginManager,
};
use rmcp::{
    ErrorData,
    model::{CallToolRequestParams, CallToolResult, GetPromptRequestParams, GetPromptResult, ReadResourceResult},
    serde::{Serialize, de::DeserializeOwned},
};
use tokio::sync::Mutex;

use crate::{
    cmf::{
        prompt_request_payload, prompt_result_payload, resource_request_payload, resource_result_payload,
        tool_call_payload, tool_json_result_payload, tool_result_payload,
    },
    error::GatewayPluginRuntimeError,
    factory::supported_cmf_hook_name,
    hooks::{
        PromptPreFetchResult, ResourcePreFetchResult, RuntimeHookState, ToolArgumentsUpdate, ToolPreCallResult,
        invalid_resource_hook_state_error,
    },
    pipeline::{
        effective_post_json, effective_post_prompt_result, effective_post_resource_result, effective_post_result,
        effective_pre_args, effective_pre_prompt_args, log_pipeline_errors, plugin_denied_error,
        validate_pre_resource_result,
    },
};

#[derive(Default)]
struct HookPair {
    pre: bool,
    post: bool,
}

#[derive(Default)]
struct HookPresence {
    tool: HookPair,
    prompt: HookPair,
    resource: HookPair,
}

#[derive(Default)]
pub(crate) struct GatewayPluginRuntime {
    manager: PluginManager,
    hooks: HookPresence,
}

struct ToolCallState {
    context_table: PluginContextTable,
    tool_call_id: String,
}

type SharedToolCallState = Mutex<ToolCallState>;

static CORRELATION_ID: AtomicU64 = AtomicU64::new(1);

fn next_tool_call_id() -> String {
    format!("gateway-tool-call-{}", CORRELATION_ID.fetch_add(1, Ordering::Relaxed))
}

fn new_tool_call_state() -> RuntimeHookState {
    Arc::new(Mutex::new(ToolCallState {
        context_table: PluginContextTable::default(),
        tool_call_id: next_tool_call_id(),
    }))
}

fn next_prompt_request_id() -> String {
    format!("gateway-prompt-request-{}", CORRELATION_ID.fetch_add(1, Ordering::Relaxed))
}

struct PromptCallState {
    context_table: PluginContextTable,
    prompt_request_id: String,
}

fn new_prompt_call_state(context_table: PluginContextTable, prompt_request_id: String) -> RuntimeHookState {
    Arc::new(PromptCallState { context_table, prompt_request_id })
}

struct ResourceCallState {
    context_table: PluginContextTable,
    resource_request_id: String,
}

fn next_resource_request_id() -> String {
    format!("gateway-resource-request-{}", CORRELATION_ID.fetch_add(1, Ordering::Relaxed))
}

fn new_resource_call_state(context_table: PluginContextTable, resource_request_id: String) -> RuntimeHookState {
    Arc::new(ResourceCallState { context_table, resource_request_id })
}

impl GatewayPluginRuntime {
    pub(crate) fn has_post_hook(&self) -> bool {
        self.hooks.tool.post
    }

    pub(crate) fn has_prompt_post_hook(&self) -> bool {
        self.hooks.prompt.post
    }

    pub(crate) fn has_resource_post_hook(&self) -> bool {
        self.hooks.resource.post
    }

    pub(crate) async fn from_config(
        config: CpexConfig,
        factories: &PluginFactoryRegistry,
    ) -> Result<Self, GatewayPluginRuntimeError> {
        validate_gateway_supported_config(&config)?;

        let hooks = HookPresence {
            tool: HookPair {
                pre: declares(&config, cmf_hook_names::TOOL_PRE_INVOKE),
                post: declares(&config, cmf_hook_names::TOOL_POST_INVOKE),
            },
            prompt: HookPair {
                pre: declares(&config, cmf_hook_names::PROMPT_PRE_FETCH),
                post: declares(&config, cmf_hook_names::PROMPT_POST_FETCH),
            },
            resource: HookPair {
                pre: declares(&config, cmf_hook_names::RESOURCE_PRE_FETCH),
                post: declares(&config, cmf_hook_names::RESOURCE_POST_FETCH),
            },
        };
        let manager = PluginManager::from_config(config, factories)
            .map_err(|source| GatewayPluginRuntimeError::Configuration { hook: "config", source })?;
        manager.initialize().await.map_err(|source| GatewayPluginRuntimeError::Initialization { source })?;
        Ok(Self { manager, hooks })
    }
}

impl Drop for GatewayPluginRuntime {
    fn drop(&mut self) {
        let manager = std::mem::take(&mut self.manager);
        match tokio::runtime::Handle::try_current() {
            Ok(handle) => {
                handle.spawn(async move {
                    manager.shutdown().await;
                });
            },
            Err(error) => tracing::warn!(%error, "skipping CPEX plugin shutdown outside a Tokio runtime"),
        }
    }
}

fn declares(config: &CpexConfig, hook_name: &str) -> bool {
    config.plugins.iter().any(|plugin| plugin.hooks.iter().any(|hook| hook == hook_name))
}

fn validate_gateway_supported_config(config: &CpexConfig) -> Result<(), GatewayPluginRuntimeError> {
    if config.routing_enabled()
        || config.plugin_settings.fail_on_plugin_error
        || !config.routes.is_empty()
        || !config.plugin_dirs.is_empty()
        || !config.global.policies.is_empty()
        || !config.global.defaults.is_empty()
    {
        return Err(GatewayPluginRuntimeError::ConfigUnsupported);
    }

    for plugin in &config.plugins {
        if !plugin.conditions.is_empty() {
            return Err(GatewayPluginRuntimeError::ConfigUnsupported);
        }

        if plugin.hooks.iter().any(|hook| supported_cmf_hook_name(hook).is_none()) {
            return Err(GatewayPluginRuntimeError::ConfigUnsupported);
        }
    }

    Ok(())
}

impl GatewayPluginRuntime {
    async fn invoke_cmf_hook(
        &self,
        hook_name: &'static str,
        payload: MessagePayload,
        context_table: Option<PluginContextTable>,
    ) -> PipelineResult {
        let (result, background_tasks) =
            self.manager.invoke_named::<CmfHook>(hook_name, payload, Extensions::default(), context_table).await;
        log_pipeline_errors(hook_name, &result);
        drop(background_tasks);
        result
    }

    pub(crate) async fn before_tool_call(
        &self,
        request: &CallToolRequestParams,
        tool_name: &str,
        backend_name: &str,
    ) -> Result<ToolPreCallResult, ErrorData> {
        if !self.hooks.tool.pre {
            let state = self.hooks.tool.post.then(new_tool_call_state);
            return Ok(ToolPreCallResult { arguments: ToolArgumentsUpdate::Unchanged, state });
        }

        let tool_call_id = next_tool_call_id();
        let original_payload = tool_call_payload(request, tool_name, backend_name, &tool_call_id);
        let pre_result = self.invoke_cmf_hook(cmf_hook_names::TOOL_PRE_INVOKE, original_payload, None).await;
        if pre_result.is_denied() {
            return Err(plugin_denied_error("tool call", pre_result));
        }

        let arguments = effective_pre_args(request.arguments.as_ref(), &pre_result)?;
        let state = Mutex::new(ToolCallState { context_table: pre_result.context_table, tool_call_id });
        Ok(ToolPreCallResult { arguments, state: Some(Arc::new(state)) })
    }

    pub(crate) async fn before_get_prompt(
        &self,
        request: &GetPromptRequestParams,
        prompt_name: &str,
        backend_name: &str,
    ) -> Result<PromptPreFetchResult, ErrorData> {
        if !self.hooks.prompt.pre {
            let mut result = PromptPreFetchResult::unchanged();
            result.state = self
                .hooks
                .prompt
                .post
                .then(|| new_prompt_call_state(PluginContextTable::default(), next_prompt_request_id()));
            return Ok(result);
        }

        let prompt_request_id = next_prompt_request_id();
        let payload = prompt_request_payload(request, prompt_name, backend_name, &prompt_request_id);
        let pre_result = self.invoke_cmf_hook(cmf_hook_names::PROMPT_PRE_FETCH, payload, None).await;
        if pre_result.is_denied() {
            return Err(plugin_denied_error("prompt", pre_result));
        }

        let arguments = effective_pre_prompt_args(
            request.arguments.as_ref(),
            &pre_result,
            prompt_name,
            backend_name,
            &prompt_request_id,
        )?;
        let state =
            self.hooks.prompt.post.then(|| new_prompt_call_state(pre_result.context_table.clone(), prompt_request_id));
        Ok(PromptPreFetchResult { arguments, state })
    }

    pub(crate) async fn before_read_resource(&self, resource_uri: &str) -> Result<ResourcePreFetchResult, ErrorData> {
        if !self.hooks.resource.pre && !self.hooks.resource.post {
            return Ok(ResourcePreFetchResult::unchanged());
        }

        let resource_request_id = next_resource_request_id();
        if !self.hooks.resource.pre {
            return Ok(ResourcePreFetchResult::with_post_state(new_resource_call_state(
                PluginContextTable::default(),
                resource_request_id,
            )));
        }

        let payload = resource_request_payload(resource_uri, &resource_request_id);
        let pre_result = self.invoke_cmf_hook(cmf_hook_names::RESOURCE_PRE_FETCH, payload, None).await;
        if pre_result.is_denied() {
            return Err(plugin_denied_error("resource", pre_result));
        }
        validate_pre_resource_result(&pre_result, resource_uri, &resource_request_id)?;
        if self.hooks.resource.post {
            Ok(ResourcePreFetchResult::with_post_state(new_resource_call_state(
                pre_result.context_table,
                resource_request_id,
            )))
        } else {
            Ok(ResourcePreFetchResult::unchanged())
        }
    }

    pub(crate) async fn after_get_prompt(
        &self,
        prompt_name: &str,
        response: GetPromptResult,
        state: Option<RuntimeHookState>,
    ) -> Result<GetPromptResult, ErrorData> {
        if !self.hooks.prompt.post {
            return Ok(response);
        }

        let state = state.and_then(|state| state.downcast::<PromptCallState>().ok());
        let Some(state) = state else { return Ok(response) };

        let payload = prompt_result_payload(&response, prompt_name, &state.prompt_request_id);
        let post_result =
            self.invoke_cmf_hook(cmf_hook_names::PROMPT_POST_FETCH, payload, Some(state.context_table.clone())).await;
        if post_result.is_denied() {
            return Err(plugin_denied_error("prompt", post_result));
        }

        effective_post_prompt_result(response, &post_result, prompt_name, &state.prompt_request_id)
    }

    pub(crate) async fn after_read_resource(
        &self,
        response: ReadResourceResult,
        state: RuntimeHookState,
    ) -> Result<ReadResourceResult, ErrorData> {
        let state = state.downcast::<ResourceCallState>().map_err(|_| invalid_resource_hook_state_error())?;
        let payload = resource_result_payload(&response, &state.resource_request_id)
            .ok_or_else(|| ErrorData::internal_error("Resource response contains an unsupported content type", None))?;
        let post_result =
            self.invoke_cmf_hook(cmf_hook_names::RESOURCE_POST_FETCH, payload, Some(state.context_table.clone())).await;
        if post_result.is_denied() {
            return Err(plugin_denied_error("resource", post_result));
        }
        effective_post_resource_result(response, &post_result, &state.resource_request_id)
    }

    pub(crate) async fn after_tool_call(
        &self,
        tool_name: &str,
        response: CallToolResult,
        state: Option<RuntimeHookState>,
    ) -> Result<CallToolResult, ErrorData> {
        if !self.hooks.tool.post {
            return Ok(response);
        }

        let state = state.and_then(|state| state.downcast::<SharedToolCallState>().ok());
        let Some(state) = state else { return Ok(response) };

        let mut state = state.lock().await;
        let post_result = self
            .invoke_cmf_hook(
                cmf_hook_names::TOOL_POST_INVOKE,
                tool_result_payload(tool_name, &response, &state.tool_call_id),
                Some(state.context_table.clone()),
            )
            .await;
        if post_result.is_denied() {
            return Err(plugin_denied_error("tool call", post_result));
        }

        state.context_table = post_result.context_table.clone();
        Ok(effective_post_result(response, &post_result))
    }

    pub(crate) async fn after_tool_event<T>(
        &self,
        tool_name: &str,
        event: T,
        state: Option<RuntimeHookState>,
    ) -> Result<Option<T>, ErrorData>
    where
        T: Serialize + DeserializeOwned,
    {
        if !self.hooks.tool.post {
            return Ok(Some(event));
        }

        let state = state.and_then(|state| state.downcast::<SharedToolCallState>().ok());
        let Some(state) = state else { return Ok(Some(event)) };

        let content = serde_json::to_value(&event).unwrap_or(serde_json::Value::Null);
        let mut state = state.lock().await;
        let post_result = self
            .invoke_cmf_hook(
                cmf_hook_names::TOOL_POST_INVOKE,
                tool_json_result_payload(tool_name, content, false, &state.tool_call_id),
                Some(state.context_table.clone()),
            )
            .await;
        if post_result.is_denied() {
            return Ok(None);
        }

        state.context_table = post_result.context_table.clone();
        Ok(Some(effective_post_json(event, &post_result)?))
    }
}
