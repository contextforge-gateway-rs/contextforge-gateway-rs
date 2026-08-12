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
    model::{CallToolRequestParams, CallToolResult, GetPromptRequestParams, GetPromptResult},
    serde::{Serialize, de::DeserializeOwned},
};
use tokio::sync::Mutex;

use crate::{
    cmf::{
        prompt_request_payload, prompt_result_payload, tool_call_payload, tool_json_result_payload, tool_result_payload,
    },
    error::GatewayPluginRuntimeError,
    hooks::{PromptPreFetchResult, RuntimeHookState, ToolArgumentsUpdate, ToolPreCallResult},
    pipeline::{
        effective_post_json, effective_post_prompt_result, effective_post_result, effective_pre_args,
        effective_pre_prompt_args, log_pipeline_errors, plugin_denied_error,
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

impl GatewayPluginRuntime {
    pub(crate) fn has_post_hook(&self) -> bool {
        self.hooks.tool.post
    }

    pub(crate) fn has_prompt_post_hook(&self) -> bool {
        self.hooks.prompt.post
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

const SUPPORTED_HOOKS: [&str; 4] = [
    cmf_hook_names::TOOL_PRE_INVOKE,
    cmf_hook_names::TOOL_POST_INVOKE,
    cmf_hook_names::PROMPT_PRE_FETCH,
    cmf_hook_names::PROMPT_POST_FETCH,
];

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

        if plugin.hooks.iter().any(|hook| !SUPPORTED_HOOKS.contains(&hook.as_str())) {
            return Err(GatewayPluginRuntimeError::ConfigUnsupported);
        }
    }

    Ok(())
}

impl GatewayPluginRuntime {
    async fn invoke_tool_pre(&self, payload: MessagePayload) -> PipelineResult {
        let (result, background_tasks) = self
            .manager
            .invoke_named::<CmfHook>(cmf_hook_names::TOOL_PRE_INVOKE, payload, Extensions::default(), None)
            .await;
        log_pipeline_errors(cmf_hook_names::TOOL_PRE_INVOKE, &result);
        drop(background_tasks);
        result
    }

    async fn invoke_tool_post(
        &self,
        payload: MessagePayload,
        context_table: Option<PluginContextTable>,
    ) -> PipelineResult {
        let (result, background_tasks) = self
            .manager
            .invoke_named::<CmfHook>(cmf_hook_names::TOOL_POST_INVOKE, payload, Extensions::default(), context_table)
            .await;
        log_pipeline_errors(cmf_hook_names::TOOL_POST_INVOKE, &result);
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
        let pre_result = self.invoke_tool_pre(original_payload).await;
        if pre_result.is_denied() {
            return Err(plugin_denied_error("tool call", pre_result));
        }

        let arguments = effective_pre_args(request.arguments.as_ref(), &pre_result)?;
        let state = Mutex::new(ToolCallState { context_table: pre_result.context_table, tool_call_id });
        Ok(ToolPreCallResult { arguments, state: Some(Arc::new(state)) })
    }

    async fn invoke_prompt_pre(&self, payload: MessagePayload) -> PipelineResult {
        let (result, background_tasks) = self
            .manager
            .invoke_named::<CmfHook>(cmf_hook_names::PROMPT_PRE_FETCH, payload, Extensions::default(), None)
            .await;
        log_pipeline_errors(cmf_hook_names::PROMPT_PRE_FETCH, &result);
        drop(background_tasks);
        result
    }

    async fn invoke_prompt_post(
        &self,
        payload: MessagePayload,
        context_table: Option<PluginContextTable>,
    ) -> PipelineResult {
        let (result, background_tasks) = self
            .manager
            .invoke_named::<CmfHook>(cmf_hook_names::PROMPT_POST_FETCH, payload, Extensions::default(), context_table)
            .await;
        log_pipeline_errors(cmf_hook_names::PROMPT_POST_FETCH, &result);
        drop(background_tasks);
        result
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
        let pre_result = self.invoke_prompt_pre(payload).await;
        if pre_result.is_denied() {
            return Err(plugin_denied_error("prompt", pre_result));
        }

        let arguments = effective_pre_prompt_args(request.arguments.as_ref(), &pre_result)?;
        let state =
            self.hooks.prompt.post.then(|| new_prompt_call_state(pre_result.context_table.clone(), prompt_request_id));
        Ok(PromptPreFetchResult { arguments, state })
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
        let post_result = self.invoke_prompt_post(payload, Some(state.context_table.clone())).await;
        if post_result.is_denied() {
            return Err(plugin_denied_error("prompt", post_result));
        }

        effective_post_prompt_result(response, &post_result, prompt_name, &state.prompt_request_id)
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
            .invoke_tool_post(
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
            .invoke_tool_post(
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
