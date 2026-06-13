use std::sync::{
    Arc,
    atomic::{AtomicU64, Ordering},
};

use cpex_core::{
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
    model::{CallToolRequestParams, CallToolResult, ProgressNotificationParam},
};
use tokio::sync::Mutex;

use crate::{
    cmf::{tool_call_payload, tool_json_result_payload, tool_result_payload},
    error::GatewayPluginRuntimeError,
    hooks::{RuntimeHookState, ToolArgumentsUpdate, ToolPreCallResult},
    pipeline::{
        effective_post_progress, effective_post_result, effective_pre_args, log_pipeline_errors, plugin_denied_error,
    },
};

#[derive(Default)]
pub(crate) struct GatewayPluginRuntime {
    manager: PluginManager,
    has_pre_hook: bool,
    has_post_hook: bool,
}

struct ToolCallState {
    context_table: PluginContextTable,
    tool_call_id: String,
}

type SharedToolCallState = Mutex<ToolCallState>;

static TOOL_CALL_ID: AtomicU64 = AtomicU64::new(1);

fn next_tool_call_id() -> String {
    format!("gateway-tool-call-{}", TOOL_CALL_ID.fetch_add(1, Ordering::Relaxed))
}

fn new_tool_call_state() -> RuntimeHookState {
    Arc::new(Mutex::new(ToolCallState {
        context_table: PluginContextTable::default(),
        tool_call_id: next_tool_call_id(),
    }))
}

impl GatewayPluginRuntime {
    pub(crate) fn has_post_hook(&self) -> bool {
        self.has_post_hook
    }

    pub(crate) async fn from_config(
        config: CpexConfig,
        factories: &PluginFactoryRegistry,
    ) -> Result<Self, GatewayPluginRuntimeError> {
        validate_gateway_supported_config(&config)?;

        let has_pre_hook =
            config.plugins.iter().any(|plugin| plugin.hooks.iter().any(|hook| hook == cmf_hook_names::TOOL_PRE_INVOKE));
        let has_post_hook = config
            .plugins
            .iter()
            .any(|plugin| plugin.hooks.iter().any(|hook| hook == cmf_hook_names::TOOL_POST_INVOKE));
        let manager = PluginManager::from_config(config, factories)
            .map_err(|source| GatewayPluginRuntimeError::Configuration { hook: "config", source })?;
        manager.initialize().await.map_err(|source| GatewayPluginRuntimeError::Initialization { source })?;
        Ok(Self { manager, has_pre_hook, has_post_hook })
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

        if plugin
            .hooks
            .iter()
            .any(|hook| hook != cmf_hook_names::TOOL_PRE_INVOKE && hook != cmf_hook_names::TOOL_POST_INVOKE)
        {
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

    /// Runs the tool post pipeline for one event of an in-flight call: the
    /// payload is built from the call's `tool_call_id`, the call's context table
    /// seeds the pipeline, and the resulting context table is carried back so
    /// later events in the same call observe it.
    async fn run_tool_post(
        &self,
        state: &SharedToolCallState,
        build_payload: impl FnOnce(&str) -> MessagePayload,
    ) -> PipelineResult {
        let mut state = state.lock().await;
        let payload = build_payload(&state.tool_call_id);
        let post_result = self.invoke_tool_post(payload, Some(state.context_table.clone())).await;
        if !post_result.is_denied() {
            state.context_table = post_result.context_table.clone();
        }
        post_result
    }

    pub(crate) async fn before_tool_call(
        &self,
        request: &CallToolRequestParams,
        tool_name: &str,
        backend_name: &str,
    ) -> Result<ToolPreCallResult, ErrorData> {
        if !self.has_pre_hook {
            let state = self.has_post_hook.then(new_tool_call_state);
            return Ok(ToolPreCallResult { arguments: ToolArgumentsUpdate::Unchanged, state });
        }

        let tool_call_id = next_tool_call_id();
        let original_payload = tool_call_payload(request, tool_name, backend_name, &tool_call_id);
        let pre_result = self.invoke_tool_pre(original_payload).await;
        if pre_result.is_denied() {
            return Err(plugin_denied_error(pre_result));
        }

        let arguments = effective_pre_args(request.arguments.as_ref(), &pre_result)?;
        let state = Mutex::new(ToolCallState { context_table: pre_result.context_table, tool_call_id });
        Ok(ToolPreCallResult { arguments, state: Some(Arc::new(state)) })
    }

    pub(crate) async fn after_tool_call(
        &self,
        tool_name: &str,
        response: CallToolResult,
        state: Option<RuntimeHookState>,
    ) -> Result<CallToolResult, ErrorData> {
        if !self.has_post_hook {
            return Ok(response);
        }

        let Some(state) = state.and_then(|state| state.downcast::<SharedToolCallState>().ok()) else {
            return Ok(response);
        };

        let post_result = self.run_tool_post(&state, |id| tool_result_payload(tool_name, &response, id)).await;
        if post_result.is_denied() {
            return Err(plugin_denied_error(post_result));
        }
        Ok(effective_post_result(response, &post_result))
    }

    pub(crate) async fn after_progress_notification(
        &self,
        tool_name: &str,
        progress: ProgressNotificationParam,
        state: Option<RuntimeHookState>,
    ) -> Result<Option<ProgressNotificationParam>, ErrorData> {
        if !self.has_post_hook {
            return Ok(Some(progress));
        }

        let Some(state) = state.and_then(|state| state.downcast::<SharedToolCallState>().ok()) else {
            return Ok(Some(progress));
        };

        let content = serde_json::to_value(&progress).unwrap_or(serde_json::Value::Null);
        let post_result =
            self.run_tool_post(&state, |id| tool_json_result_payload(tool_name, content, false, id)).await;
        if post_result.is_denied() {
            return Ok(None);
        }
        Ok(Some(effective_post_progress(progress, &post_result)?))
    }
}
