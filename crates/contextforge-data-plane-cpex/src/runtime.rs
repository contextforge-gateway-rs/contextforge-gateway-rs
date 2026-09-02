use std::{
    collections::{HashMap, HashSet},
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicU64, Ordering},
    },
};

use cpex::cpex_core::{
    cmf::{CmfHook, MessagePayload},
    config::CpexConfig,
    context::PluginContextTable,
    executor::PipelineResult,
    extensions::{Capability, Extensions},
    factory::PluginFactoryRegistry,
    hooks::types::cmf_hook_names,
    manager::PluginManager,
    registry::HookEntry,
};
use rmcp::{
    ErrorData,
    model::{CallToolRequestParams, CallToolResult, GetPromptRequestParams, GetPromptResult, ReadResourceResult},
    serde::{Serialize, de::DeserializeOwned},
};
use tokio::sync::Semaphore;

use contextforge_data_plane_apis::runtime_plugin_config::RuntimePluginName;

use crate::{
    cmf::{
        prompt_request_payload, prompt_result_payload, resource_request_payload, resource_result_payload,
        tool_call_payload, tool_json_result_payload, tool_result_payload,
    },
    context::ScopedMcpHook,
    error::GatewayPluginRuntimeError,
    hooks::{PromptPreFetchResult, ResourcePreFetchResult, RuntimeHookState, ToolArgumentsUpdate, ToolPreCallResult},
    pipeline::{
        effective_post_json, effective_post_prompt_result, effective_post_resource_result, effective_post_result,
        effective_pre_args, effective_pre_prompt_args, log_pipeline_errors, plugin_denied_error,
        validate_pre_resource_result,
    },
};

#[derive(Default)]
pub(crate) struct GatewayPluginRuntime {
    manager: PluginManager,
    plugin_names: Vec<RuntimePluginName>,
    dispatch_plans: Mutex<HashMap<DispatchPlanKey, Arc<ResolvedHookPair>>>,
    dispatch_plan_cache_max_entries: usize,
    dispatch_plan_cache_full_warned: AtomicBool,
}

struct ToolCallLifecycle {
    context_table: Option<PluginContextTable>,
    extensions: Extensions,
}

struct SharedToolCallState {
    post_entries: Arc<[HookEntry]>,
    tool_call_id: String,
    gate: Semaphore,
    lifecycle: Mutex<ToolCallLifecycle>,
}

static CORRELATION_ID: AtomicU64 = AtomicU64::new(1);

fn next_tool_call_id() -> String {
    format!("gateway-tool-call-{}", CORRELATION_ID.fetch_add(1, Ordering::Relaxed))
}

fn new_tool_call_state(
    post_entries: Arc<[HookEntry]>,
    tool_call_id: String,
    context_table: Option<PluginContextTable>,
    extensions: Extensions,
) -> RuntimeHookState {
    Arc::new(SharedToolCallState {
        post_entries,
        tool_call_id,
        gate: Semaphore::new(1),
        lifecycle: Mutex::new(ToolCallLifecycle { context_table, extensions }),
    })
}

fn next_prompt_request_id() -> String {
    format!("gateway-prompt-request-{}", CORRELATION_ID.fetch_add(1, Ordering::Relaxed))
}

struct PromptCallState {
    post_entries: Arc<[HookEntry]>,
    context_table: Option<PluginContextTable>,
    extensions: Extensions,
    prompt_request_id: String,
}

fn new_prompt_call_state(
    post_entries: Arc<[HookEntry]>,
    context_table: Option<PluginContextTable>,
    extensions: Extensions,
    prompt_request_id: String,
) -> RuntimeHookState {
    Arc::new(PromptCallState { post_entries, context_table, extensions, prompt_request_id })
}

struct ResourceCallState {
    post_entries: Arc<[HookEntry]>,
    context_table: Option<PluginContextTable>,
    extensions: Extensions,
    resource_request_id: String,
}

fn next_resource_request_id() -> String {
    format!("gateway-resource-request-{}", CORRELATION_ID.fetch_add(1, Ordering::Relaxed))
}

fn new_resource_call_state(
    post_entries: Arc<[HookEntry]>,
    context_table: Option<PluginContextTable>,
    extensions: Extensions,
    resource_request_id: String,
) -> RuntimeHookState {
    Arc::new(ResourceCallState { post_entries, context_table, extensions, resource_request_id })
}

impl GatewayPluginRuntime {
    pub(crate) async fn from_config(
        config: CpexConfig,
        factories: &PluginFactoryRegistry,
    ) -> Result<Self, GatewayPluginRuntimeError> {
        validate_gateway_supported_config(&config)?;

        let plugin_names = config
            .plugins
            .iter()
            .map(|plugin| RuntimePluginName::try_from(plugin.name.clone()))
            .collect::<Result<Vec<_>, _>>()
            .map_err(|_| GatewayPluginRuntimeError::ConfigWrongFormat)?;
        let dispatch_plan_cache_max_entries = config.plugin_settings.route_cache_max_entries;
        let manager = PluginManager::from_config(config, factories)
            .map_err(|source| GatewayPluginRuntimeError::Configuration { hook: "config", source })?;
        manager.initialize().await.map_err(|source| GatewayPluginRuntimeError::Initialization { source })?;
        Ok(Self {
            manager,
            plugin_names,
            dispatch_plans: Mutex::new(HashMap::new()),
            dispatch_plan_cache_max_entries,
            dispatch_plan_cache_full_warned: AtomicBool::new(false),
        })
    }

    pub(crate) fn plugin_names(&self) -> &[RuntimePluginName] {
        &self.plugin_names
    }

    #[cfg(test)]
    pub(crate) fn dispatch_plan_count(&self) -> usize {
        self.dispatch_plans.lock().expect("dispatch plan cache lock poisoned").len()
    }
}

#[cfg(test)]
pub(crate) fn tool_state_has_context(state: &RuntimeHookState) -> Option<bool> {
    Arc::clone(state)
        .downcast::<SharedToolCallState>()
        .ok()
        .map(|state| state.lifecycle.lock().expect("tool lifecycle lock poisoned").context_table.is_some())
}

struct ResolvedHookPair {
    pre: Arc<[HookEntry]>,
    post: Arc<[HookEntry]>,
}

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
enum HookFamily {
    Tool,
    Prompt,
    Resource,
}

impl HookFamily {
    fn hook_names(self) -> (&'static str, &'static str) {
        match self {
            Self::Tool => (cmf_hook_names::TOOL_PRE_INVOKE, cmf_hook_names::TOOL_POST_INVOKE),
            Self::Prompt => (cmf_hook_names::PROMPT_PRE_FETCH, cmf_hook_names::PROMPT_POST_FETCH),
            Self::Resource => (cmf_hook_names::RESOURCE_PRE_FETCH, cmf_hook_names::RESOURCE_POST_FETCH),
        }
    }
}

#[derive(Clone, Debug, Eq, Hash, PartialEq)]
struct DispatchPlanKey {
    hook_family: HookFamily,
    plugin_names: Vec<RuntimePluginName>,
}

fn binding_error(reason: &str) -> ErrorData {
    tracing::warn!(reason, "rejecting call with invalid runtime plugin binding");
    ErrorData::internal_error("Runtime plugin binding is invalid", None)
}

fn same_arc<T>(left: Option<&Arc<T>>, right: Option<&Arc<T>>) -> bool {
    match (left, right) {
        (Some(left), Some(right)) => Arc::ptr_eq(left, right),
        (None, None) => true,
        _ => false,
    }
}

fn trusted_fields_unchanged(before: &Extensions, after: &Extensions) -> bool {
    let immutable_slots = same_arc(before.request.as_ref(), after.request.as_ref())
        && same_arc(before.mcp.as_ref(), after.mcp.as_ref())
        && same_arc(before.meta.as_ref(), after.meta.as_ref());
    let http_identity = match (&before.http, &after.http) {
        (Some(before), Some(after)) => {
            before.method == after.method
                && before.path == after.path
                && before.host == after.host
                && before.scheme == after.scheme
        },
        (None, None) => true,
        _ => false,
    };
    let security_identity = match (&before.security, &after.security) {
        (Some(before), Some(after)) => {
            let subject = match (&before.subject, &after.subject) {
                (Some(before), Some(after)) => {
                    before.id == after.id
                        && before.subject_type == after.subject_type
                        && before.roles == after.roles
                        && before.permissions == after.permissions
                        && before.teams == after.teams
                        && before.claims == after.claims
                },
                (None, None) => true,
                _ => false,
            };
            subject && before.auth_method == after.auth_method
        },
        (None, None) => true,
        _ => false,
    };

    immutable_slots && http_identity && security_identity
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

const SUPPORTED_HOOKS: [&str; 6] = [
    cmf_hook_names::TOOL_PRE_INVOKE,
    cmf_hook_names::TOOL_POST_INVOKE,
    cmf_hook_names::PROMPT_PRE_FETCH,
    cmf_hook_names::PROMPT_POST_FETCH,
    cmf_hook_names::RESOURCE_PRE_FETCH,
    cmf_hook_names::RESOURCE_POST_FETCH,
];

fn validate_gateway_supported_config(config: &CpexConfig) -> Result<(), GatewayPluginRuntimeError> {
    if config.routing_enabled()
        || config.plugin_settings.fail_on_plugin_error
        || config.plugin_settings.parallel_execution_within_band
        || !config.routes.is_empty()
        || !config.plugin_dirs.is_empty()
        || !config.global.policies.is_empty()
        || !config.global.defaults.is_empty()
        || config.global.identity.is_some()
    {
        return Err(GatewayPluginRuntimeError::ConfigUnsupported);
    }

    let mut plugin_names = HashSet::new();
    for plugin in &config.plugins {
        if plugin.name.trim().is_empty() || plugin.kind.trim().is_empty() || !plugin_names.insert(plugin.name.as_str())
        {
            return Err(GatewayPluginRuntimeError::ConfigWrongFormat);
        }
        if !plugin.conditions.is_empty() {
            return Err(GatewayPluginRuntimeError::ConfigUnsupported);
        }

        if plugin.hooks.iter().any(|hook| !SUPPORTED_HOOKS.contains(&hook.as_str())) {
            return Err(GatewayPluginRuntimeError::ConfigUnsupported);
        }

        if plugin.capabilities.iter().any(|capability| {
            serde_json::from_value::<Capability>(serde_json::Value::String(capability.clone())).is_err()
        }) {
            return Err(GatewayPluginRuntimeError::ConfigUnsupported);
        }
    }

    Ok(())
}

struct HookInvocation {
    extensions: Extensions,
    context_table: Option<PluginContextTable>,
}

impl GatewayPluginRuntime {
    fn resolve_hooks(
        &self,
        plugin_names: &[RuntimePluginName],
        hook_family: HookFamily,
    ) -> Result<Arc<ResolvedHookPair>, ErrorData> {
        let key = DispatchPlanKey { hook_family, plugin_names: plugin_names.to_vec() };
        if let Some(plan) = self
            .dispatch_plans
            .lock()
            .map_err(|_| binding_error("runtime dispatch plan cache lock poisoned"))?
            .get(&key)
            .cloned()
        {
            return Ok(plan);
        }

        let (pre_hook, post_hook) = hook_family.hook_names();
        let mut seen = HashSet::new();
        let mut pre = Vec::new();
        let mut post = Vec::new();
        for plugin_name in plugin_names {
            if !seen.insert(plugin_name) {
                return Err(binding_error("binding contains a duplicate plugin name"));
            }

            let entries = self.manager.find_plugin_entries(plugin_name.as_str());
            if entries.is_empty() {
                return Err(binding_error("binding references an unknown plugin"));
            }
            let mut supports_target = false;
            for (hook_name, entry) in entries {
                if hook_name == pre_hook {
                    supports_target = true;
                    pre.push(entry);
                } else if hook_name == post_hook {
                    supports_target = true;
                    post.push(entry);
                }
            }
            if !supports_target {
                return Err(binding_error("binding references a plugin without a compatible hook"));
            }
        }
        let plan = Arc::new(ResolvedHookPair { pre: pre.into(), post: post.into() });
        let mut dispatch_plans =
            self.dispatch_plans.lock().map_err(|_| binding_error("runtime dispatch plan cache lock poisoned"))?;
        if dispatch_plans.len() < self.dispatch_plan_cache_max_entries {
            return Ok(Arc::clone(dispatch_plans.entry(key).or_insert_with(|| Arc::clone(&plan))));
        }
        if !self.dispatch_plan_cache_full_warned.swap(true, Ordering::Relaxed) {
            tracing::warn!(
                max_entries = self.dispatch_plan_cache_max_entries,
                "runtime dispatch plan cache is full; new plans will not be cached"
            );
        }
        Ok(plan)
    }

    async fn invoke_hook(
        &self,
        hook_name: &'static str,
        entries: &[HookEntry],
        payload: MessagePayload,
        invocation: HookInvocation,
    ) -> Result<PipelineResult, ErrorData> {
        let trusted_extensions = invocation.extensions.clone();
        let (result, background_tasks) = self
            .manager
            .invoke_entries::<CmfHook>(entries, payload, invocation.extensions, invocation.context_table)
            .await;
        log_pipeline_errors(hook_name, &result);
        drop(background_tasks);
        let Some(next_extensions) = result.modified_extensions.as_ref() else {
            return Err(binding_error("CPEX pipeline omitted lifecycle extensions"));
        };
        if !trusted_fields_unchanged(&trusted_extensions, next_extensions) {
            return Err(binding_error("plugin attempted to mutate trusted request identity"));
        }
        Ok(result)
    }

    pub(crate) async fn before_tool_call(
        &self,
        request: &CallToolRequestParams,
        tool_name: &str,
        backend_name: &str,
        scope: ScopedMcpHook<'_>,
    ) -> Result<ToolPreCallResult, ErrorData> {
        let (plugin_names, context) = scope.into_parts();
        let hooks = self.resolve_hooks(plugin_names, HookFamily::Tool)?;
        let tool_call_id = next_tool_call_id();
        let extensions = context.into_extensions();
        if hooks.pre.is_empty() {
            let state = (!hooks.post.is_empty())
                .then(|| new_tool_call_state(Arc::clone(&hooks.post), tool_call_id, None, extensions));
            return Ok(ToolPreCallResult { arguments: ToolArgumentsUpdate::Unchanged, state });
        }

        let original_payload = tool_call_payload(request, tool_name, backend_name, &tool_call_id);
        let mut pre_result = self
            .invoke_hook(
                cmf_hook_names::TOOL_PRE_INVOKE,
                &hooks.pre,
                original_payload,
                HookInvocation { extensions, context_table: None },
            )
            .await?;
        if pre_result.is_denied() {
            return Err(plugin_denied_error("tool call", pre_result));
        }

        let arguments = effective_pre_args(request.arguments.as_ref(), &pre_result)?;
        let state = if hooks.post.is_empty() {
            None
        } else {
            let extensions = pre_result
                .modified_extensions
                .take()
                .ok_or_else(|| binding_error("CPEX pipeline omitted lifecycle extensions"))?;
            Some(new_tool_call_state(Arc::clone(&hooks.post), tool_call_id, Some(pre_result.context_table), extensions))
        };
        Ok(ToolPreCallResult { arguments, state })
    }

    pub(crate) async fn before_get_prompt(
        &self,
        request: &GetPromptRequestParams,
        prompt_name: &str,
        backend_name: &str,
        scope: ScopedMcpHook<'_>,
    ) -> Result<PromptPreFetchResult, ErrorData> {
        let (plugin_names, context) = scope.into_parts();
        let hooks = self.resolve_hooks(plugin_names, HookFamily::Prompt)?;
        let prompt_request_id = next_prompt_request_id();
        let extensions = context.into_extensions();
        if hooks.pre.is_empty() {
            let mut result = PromptPreFetchResult::unchanged();
            result.state = (!hooks.post.is_empty())
                .then(|| new_prompt_call_state(Arc::clone(&hooks.post), None, extensions, prompt_request_id));
            return Ok(result);
        }

        let payload = prompt_request_payload(request, prompt_name, backend_name, &prompt_request_id);
        let mut pre_result = self
            .invoke_hook(
                cmf_hook_names::PROMPT_PRE_FETCH,
                &hooks.pre,
                payload,
                HookInvocation { extensions, context_table: None },
            )
            .await?;
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
        let state = if hooks.post.is_empty() {
            None
        } else {
            let extensions = pre_result
                .modified_extensions
                .take()
                .ok_or_else(|| binding_error("CPEX pipeline omitted lifecycle extensions"))?;
            Some(new_prompt_call_state(
                Arc::clone(&hooks.post),
                Some(pre_result.context_table),
                extensions,
                prompt_request_id,
            ))
        };
        Ok(PromptPreFetchResult { arguments, state })
    }

    pub(crate) async fn before_read_resource(
        &self,
        resource_uri: &str,
        scope: ScopedMcpHook<'_>,
    ) -> Result<ResourcePreFetchResult, ErrorData> {
        let (plugin_names, context) = scope.into_parts();
        let hooks = self.resolve_hooks(plugin_names, HookFamily::Resource)?;
        let resource_request_id = next_resource_request_id();
        let extensions = context.into_extensions();
        if hooks.pre.is_empty() {
            let state = (!hooks.post.is_empty())
                .then(|| new_resource_call_state(Arc::clone(&hooks.post), None, extensions, resource_request_id));
            return Ok(ResourcePreFetchResult { state });
        }

        let payload = resource_request_payload(resource_uri, &resource_request_id);
        let mut pre_result = self
            .invoke_hook(
                cmf_hook_names::RESOURCE_PRE_FETCH,
                &hooks.pre,
                payload,
                HookInvocation { extensions, context_table: None },
            )
            .await?;
        if pre_result.is_denied() {
            return Err(plugin_denied_error("resource", pre_result));
        }
        validate_pre_resource_result(&pre_result, resource_uri, &resource_request_id)?;
        let state = if hooks.post.is_empty() {
            None
        } else {
            let extensions = pre_result
                .modified_extensions
                .take()
                .ok_or_else(|| binding_error("CPEX pipeline omitted lifecycle extensions"))?;
            Some(new_resource_call_state(
                Arc::clone(&hooks.post),
                Some(pre_result.context_table),
                extensions,
                resource_request_id,
            ))
        };
        Ok(ResourcePreFetchResult { state })
    }

    pub(crate) async fn after_get_prompt(
        &self,
        prompt_name: &str,
        response: GetPromptResult,
        state: Option<RuntimeHookState>,
    ) -> Result<GetPromptResult, ErrorData> {
        let state = state.and_then(|state| state.downcast::<PromptCallState>().ok());
        let Some(state) = state else { return Ok(response) };

        let payload = prompt_result_payload(&response, prompt_name, &state.prompt_request_id);
        let post_result = self
            .invoke_hook(
                cmf_hook_names::PROMPT_POST_FETCH,
                &state.post_entries,
                payload,
                HookInvocation { extensions: state.extensions.clone(), context_table: state.context_table.clone() },
            )
            .await?;
        if post_result.is_denied() {
            return Err(plugin_denied_error("prompt", post_result));
        }

        effective_post_prompt_result(response, &post_result, prompt_name, &state.prompt_request_id)
    }

    pub(crate) async fn after_read_resource(
        &self,
        response: ReadResourceResult,
        state: Option<RuntimeHookState>,
    ) -> Result<ReadResourceResult, ErrorData> {
        let state = state.and_then(|state| state.downcast::<ResourceCallState>().ok());
        let Some(state) = state else { return Ok(response) };

        let payload = resource_result_payload(&response, &state.resource_request_id)
            .ok_or_else(|| binding_error("resource response contains an unsupported content type"))?;
        let post_result = self
            .invoke_hook(
                cmf_hook_names::RESOURCE_POST_FETCH,
                &state.post_entries,
                payload,
                HookInvocation { extensions: state.extensions.clone(), context_table: state.context_table.clone() },
            )
            .await?;
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
        let state = state.and_then(|state| state.downcast::<SharedToolCallState>().ok());
        let Some(state) = state else { return Ok(response) };

        let _permit = state.gate.acquire().await.map_err(|_| binding_error("tool hook lifecycle closed"))?;
        let (context_table, extensions) = {
            let lifecycle = state.lifecycle.lock().map_err(|_| binding_error("tool hook lifecycle lock poisoned"))?;
            (lifecycle.context_table.clone(), lifecycle.extensions.clone())
        };
        let post_result = self
            .invoke_hook(
                cmf_hook_names::TOOL_POST_INVOKE,
                &state.post_entries,
                tool_result_payload(tool_name, &response, &state.tool_call_id),
                HookInvocation { extensions, context_table },
            )
            .await?;
        if post_result.is_denied() {
            return Err(plugin_denied_error("tool call", post_result));
        }

        let mut lifecycle = state.lifecycle.lock().map_err(|_| binding_error("tool hook lifecycle lock poisoned"))?;
        lifecycle.context_table = Some(post_result.context_table.clone());
        lifecycle.extensions = post_result
            .modified_extensions
            .clone()
            .ok_or_else(|| binding_error("CPEX pipeline omitted lifecycle extensions"))?;
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
        let state = state.and_then(|state| state.downcast::<SharedToolCallState>().ok());
        let Some(state) = state else { return Ok(Some(event)) };

        let content = serde_json::to_value(&event).unwrap_or(serde_json::Value::Null);
        let _permit = state.gate.acquire().await.map_err(|_| binding_error("tool hook lifecycle closed"))?;
        let (context_table, extensions) = {
            let lifecycle = state.lifecycle.lock().map_err(|_| binding_error("tool hook lifecycle lock poisoned"))?;
            (lifecycle.context_table.clone(), lifecycle.extensions.clone())
        };
        let post_result = self
            .invoke_hook(
                cmf_hook_names::TOOL_POST_INVOKE,
                &state.post_entries,
                tool_json_result_payload(tool_name, content, false, &state.tool_call_id),
                HookInvocation { extensions, context_table },
            )
            .await?;
        if post_result.is_denied() {
            return Ok(None);
        }

        let mut lifecycle = state.lifecycle.lock().map_err(|_| binding_error("tool hook lifecycle lock poisoned"))?;
        lifecycle.context_table = Some(post_result.context_table.clone());
        lifecycle.extensions = post_result
            .modified_extensions
            .clone()
            .ok_or_else(|| binding_error("CPEX pipeline omitted lifecycle extensions"))?;
        Ok(Some(effective_post_json(event, &post_result)?))
    }
}
