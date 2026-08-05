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
    model::{
        CallToolRequestParams, CallToolResult, CompleteResult, GetPromptRequestParams, GetPromptResult, Prompt,
        ReadResourceResult, Resource as McpResource, ResourceTemplate,
    },
    serde::{Serialize, de::DeserializeOwned},
};
use tokio::sync::Mutex;

use crate::{
    cmf::{
        prompt_completion_payload, prompt_completion_result_payload, prompt_list_request_payload,
        prompt_list_result_payload, prompt_request_payload, prompt_result_payload, read_resource_result_payload,
        resource_completion_payload, resource_completion_result_payload, resource_list_request_payload,
        resource_list_result_payload, resource_request_payload, resource_template_list_result_payload,
        tool_call_payload, tool_json_result_payload, tool_result_payload,
    },
    error::GatewayPluginRuntimeError,
    hooks::{
        CompletionRequest, CompletionTarget, PromptArgumentsUpdate, PromptPreFetchResult, ResourcePreFetchResult,
        ResourceUriUpdate, RuntimeHookState, ToolArgumentsUpdate, ToolPreCallResult,
    },
    pipeline::{
        effective_post_completion_result, effective_post_json, effective_post_prompt_result,
        effective_post_read_resource, effective_post_result, effective_pre_args, effective_pre_prompt_args,
        effective_pre_resource_uri, log_pipeline_errors, plugin_denied_error,
    },
};

/// Whether the loaded plugin config declares the pre and post hook of one MCP path.
#[derive(Default)]
struct HookPair {
    pre: bool,
    post: bool,
}

impl HookPair {
    fn from_config(config: &CpexConfig, pre: &str, post: &str) -> Self {
        let declares = |name: &str| config.plugins.iter().any(|plugin| plugin.hooks.iter().any(|hook| hook == name));
        Self { pre: declares(pre), post: declares(post) }
    }
}

/// Which hooks the loaded plugin config actually declares. Each MCP path checks only its own
/// pair, so a prompt-only plugin config leaves the tool hot path untouched.
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

/// Per-call CPEX state carried from a pre hook to its matching post hook. `correlation_id` is the
/// CMF `tool_call_id` / `prompt_request_id` that links the two payloads.
struct CallState {
    context_table: PluginContextTable,
    correlation_id: String,
}

type SharedCallState = Mutex<CallState>;

static CORRELATION_ID: AtomicU64 = AtomicU64::new(1);

fn next_correlation_id(kind: &str) -> String {
    format!("gateway-{kind}-{}", CORRELATION_ID.fetch_add(1, Ordering::Relaxed))
}

fn new_call_state(kind: &str) -> RuntimeHookState {
    Arc::new(Mutex::new(CallState {
        context_table: PluginContextTable::default(),
        correlation_id: next_correlation_id(kind),
    }))
}

const TOOL_CALL_KIND: &str = "tool-call";
const PROMPT_REQUEST_KIND: &str = "prompt-request";
const PROMPT_LIST_KIND: &str = "prompt-list";
const RESOURCE_TEMPLATE_LIST_KIND: &str = "resource-template-list";
const RESOURCE_SUBSCRIPTION_KIND: &str = "resource-subscription";
const COMPLETION_KIND: &str = "completion";
const RESOURCE_READ_KIND: &str = "resource-read";
const RESOURCE_LIST_KIND: &str = "resource-list";

impl GatewayPluginRuntime {
    pub(crate) fn has_tool_post_hook(&self) -> bool {
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
            tool: HookPair::from_config(&config, cmf_hook_names::TOOL_PRE_INVOKE, cmf_hook_names::TOOL_POST_INVOKE),
            prompt: HookPair::from_config(&config, cmf_hook_names::PROMPT_PRE_FETCH, cmf_hook_names::PROMPT_POST_FETCH),
            resource: HookPair::from_config(
                &config,
                cmf_hook_names::RESOURCE_PRE_FETCH,
                cmf_hook_names::RESOURCE_POST_FETCH,
            ),
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

/// CPEX hooks the gateway is able to run. Tool hooks wrap `call_tool`; prompt and resource hooks
/// wrap the non-tool MCP pass-through paths. Any other hook is rejected before the runtime loads,
/// because the gateway has no defined behavior for it on the hot path.
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

    async fn invoke_resource_pre(&self, payload: MessagePayload) -> PipelineResult {
        let (result, background_tasks) = self
            .manager
            .invoke_named::<CmfHook>(cmf_hook_names::RESOURCE_PRE_FETCH, payload, Extensions::default(), None)
            .await;
        log_pipeline_errors(cmf_hook_names::RESOURCE_PRE_FETCH, &result);
        drop(background_tasks);
        result
    }

    async fn invoke_resource_post(
        &self,
        payload: MessagePayload,
        context_table: Option<PluginContextTable>,
    ) -> PipelineResult {
        let (result, background_tasks) = self
            .manager
            .invoke_named::<CmfHook>(cmf_hook_names::RESOURCE_POST_FETCH, payload, Extensions::default(), context_table)
            .await;
        log_pipeline_errors(cmf_hook_names::RESOURCE_POST_FETCH, &result);
        drop(background_tasks);
        result
    }

    fn completion_hooks(&self, target: CompletionTarget) -> &HookPair {
        match target {
            CompletionTarget::Prompt => &self.hooks.prompt,
            CompletionTarget::Resource => &self.hooks.resource,
        }
    }

    pub(crate) fn has_completion_post_hook(&self, target: CompletionTarget) -> bool {
        self.completion_hooks(target).post
    }

    /// Runs the pre hook for `completion/complete`, after routing. CPEX has no completion hook, so
    /// this dispatches to the prompt or resource family depending on what is being completed.
    ///
    /// Deny-only: the argument value is a partial user keystroke, so rewriting it would return
    /// completions for text the user never typed.
    pub(crate) async fn before_complete(
        &self,
        request: &CompletionRequest<'_>,
    ) -> Result<Option<RuntimeHookState>, ErrorData> {
        let CompletionRequest { target, identifier, backend_name, argument, context } = *request;
        let hooks = self.completion_hooks(target);
        if !hooks.pre {
            return Ok(hooks.post.then(|| new_call_state(COMPLETION_KIND)));
        }

        let correlation_id = next_correlation_id(COMPLETION_KIND);
        let pre_result = match target {
            CompletionTarget::Prompt => {
                let payload = prompt_completion_payload(identifier, backend_name, argument, context, &correlation_id);
                self.invoke_prompt_pre(payload).await
            },
            CompletionTarget::Resource => {
                let payload = resource_completion_payload(identifier, backend_name, argument, context, &correlation_id);
                self.invoke_resource_pre(payload).await
            },
        };
        if pre_result.is_denied() {
            return Err(plugin_denied_error(pre_result, "completion"));
        }

        Ok(Some(Arc::new(Mutex::new(CallState { context_table: pre_result.context_table, correlation_id }))))
    }

    /// Runs the post hook for `completion/complete`. A plugin may rewrite the completion values;
    /// the count must match, so the backend's `total` and `has_more` stay accurate.
    pub(crate) async fn after_complete(
        &self,
        target: CompletionTarget,
        identifier: &str,
        response: CompleteResult,
        state: Option<RuntimeHookState>,
    ) -> Result<CompleteResult, ErrorData> {
        if !self.completion_hooks(target).post {
            return Ok(response);
        }

        let state = state.and_then(|state| state.downcast::<SharedCallState>().ok());
        let Some(state) = state else { return Ok(response) };

        let mut state = state.lock().await;
        let values = &response.completion.values;
        let post_result = match target {
            CompletionTarget::Prompt => {
                let payload = prompt_completion_result_payload(identifier, values, &state.correlation_id);
                self.invoke_prompt_post(payload, Some(state.context_table.clone())).await
            },
            CompletionTarget::Resource => {
                let payload = resource_completion_result_payload(identifier, values, &state.correlation_id);
                self.invoke_resource_post(payload, Some(state.context_table.clone())).await
            },
        };
        if post_result.is_denied() {
            return Err(plugin_denied_error(post_result, "completion"));
        }

        state.context_table = post_result.context_table.clone();
        effective_post_completion_result(response, &post_result)
    }

    /// Runs the resource pre hook for `resources/read`, after routing. A plugin can deny the read
    /// or rewrite the target URI, and state is carried forward to the post hook.
    pub(crate) async fn before_read_resource(
        &self,
        uri: &str,
        backend_name: &str,
    ) -> Result<ResourcePreFetchResult, ErrorData> {
        if !self.hooks.resource.pre {
            let state = self.hooks.resource.post.then(|| new_call_state(RESOURCE_READ_KIND));
            return Ok(ResourcePreFetchResult { uri: ResourceUriUpdate::Unchanged, state });
        }

        let resource_request_id = next_correlation_id(RESOURCE_READ_KIND);
        let pre_result =
            self.invoke_resource_pre(resource_request_payload(uri, backend_name, &resource_request_id)).await;
        if pre_result.is_denied() {
            return Err(plugin_denied_error(pre_result, "resource read"));
        }

        let uri = effective_pre_resource_uri(uri, &pre_result)?;
        let state =
            Mutex::new(CallState { context_table: pre_result.context_table, correlation_id: resource_request_id });
        Ok(ResourcePreFetchResult { uri, state: Some(Arc::new(state)) })
    }

    /// Runs the resource post hook over the contents `resources/read` returned. A plugin may
    /// rewrite text contents; blob bytes are never exposed and never modified.
    pub(crate) async fn after_read_resource(
        &self,
        response: ReadResourceResult,
        state: Option<RuntimeHookState>,
    ) -> Result<ReadResourceResult, ErrorData> {
        if !self.hooks.resource.post {
            return Ok(response);
        }

        let state = state.and_then(|state| state.downcast::<SharedCallState>().ok());
        let Some(state) = state else { return Ok(response) };

        let mut state = state.lock().await;
        let post_result = self
            .invoke_resource_post(
                read_resource_result_payload(&response.contents, &state.correlation_id),
                Some(state.context_table.clone()),
            )
            .await;
        if post_result.is_denied() {
            return Err(plugin_denied_error(post_result, "resource read"));
        }

        state.context_table = post_result.context_table.clone();
        effective_post_read_resource(response, &post_result)
    }

    /// Runs the resource pre hook for `resources/list`, before any backend is contacted.
    /// Deny-only, matching the template listing.
    pub(crate) async fn before_list_resources(&self) -> Result<Option<RuntimeHookState>, ErrorData> {
        self.before_resource_listing(RESOURCE_LIST_KIND, "resource list").await
    }

    /// Runs the resource post hook over the merged, namespaced resource listing. Read-only: MCP
    /// `Resource` listing entries are metadata that cannot be rebuilt from CMF content parts.
    pub(crate) async fn after_list_resources(
        &self,
        resources: &[McpResource],
        state: Option<RuntimeHookState>,
    ) -> Result<(), ErrorData> {
        self.after_resource_listing(state, "resource list", |correlation_id| {
            resource_list_result_payload(resources, correlation_id)
        })
        .await
    }

    /// Runs the resource pre hook for `resources/subscribe` and `resources/unsubscribe`, after
    /// routing. A plugin can deny the request or rewrite the target URI. There is no post hook:
    /// both paths return an empty acknowledgement. `denied` names the operation in the error a
    /// denial produces.
    pub(crate) async fn before_resource_subscription(
        &self,
        uri: &str,
        backend_name: &str,
        denied: &str,
    ) -> Result<ResourceUriUpdate, ErrorData> {
        if !self.hooks.resource.pre {
            return Ok(ResourceUriUpdate::Unchanged);
        }

        let resource_request_id = next_correlation_id(RESOURCE_SUBSCRIPTION_KIND);
        let pre_result =
            self.invoke_resource_pre(resource_request_payload(uri, backend_name, &resource_request_id)).await;
        if pre_result.is_denied() {
            return Err(plugin_denied_error(pre_result, denied));
        }

        effective_pre_resource_uri(uri, &pre_result)
    }

    /// Runs the resource pre hook for `resources/templates/list`, before any backend is contacted.
    /// Deny-only: the payload carries no routable resource, so a modified payload has nothing to
    /// apply to.
    pub(crate) async fn before_list_resource_templates(&self) -> Result<Option<RuntimeHookState>, ErrorData> {
        self.before_resource_listing(RESOURCE_TEMPLATE_LIST_KIND, "resource template list").await
    }

    /// Runs the resource post hook over the merged, namespaced template listing. Read-only: a
    /// modified payload is ignored because MCP `ResourceTemplate` metadata cannot be rebuilt from
    /// CMF content parts. Denial is still honored and surfaces as an MCP error.
    pub(crate) async fn after_list_resource_templates(
        &self,
        templates: &[ResourceTemplate],
        state: Option<RuntimeHookState>,
    ) -> Result<(), ErrorData> {
        self.after_resource_listing(state, "resource template list", |correlation_id| {
            resource_template_list_result_payload(templates, correlation_id)
        })
        .await
    }

    /// Shared pre-hook body for the resource listings, which differ only in correlation kind and
    /// the operation named in a denial.
    async fn before_resource_listing(&self, kind: &str, denied: &str) -> Result<Option<RuntimeHookState>, ErrorData> {
        if !self.hooks.resource.pre {
            return Ok(self.hooks.resource.post.then(|| new_call_state(kind)));
        }

        let resource_request_id = next_correlation_id(kind);
        let pre_result = self.invoke_resource_pre(resource_list_request_payload(&resource_request_id)).await;
        if pre_result.is_denied() {
            return Err(plugin_denied_error(pre_result, denied));
        }

        Ok(Some(Arc::new(Mutex::new(CallState {
            context_table: pre_result.context_table,
            correlation_id: resource_request_id,
        }))))
    }

    /// Shared post-hook body for the resource listings. Read-only: the modified payload is
    /// discarded, but a denial still surfaces as an MCP error.
    async fn after_resource_listing(
        &self,
        state: Option<RuntimeHookState>,
        denied: &str,
        payload: impl FnOnce(&str) -> MessagePayload,
    ) -> Result<(), ErrorData> {
        if !self.hooks.resource.post {
            return Ok(());
        }

        let state = state.and_then(|state| state.downcast::<SharedCallState>().ok());
        let Some(state) = state else { return Ok(()) };

        let mut state = state.lock().await;
        let post_result =
            self.invoke_resource_post(payload(&state.correlation_id), Some(state.context_table.clone())).await;
        if post_result.is_denied() {
            return Err(plugin_denied_error(post_result, denied));
        }

        state.context_table = post_result.context_table.clone();
        Ok(())
    }

    pub(crate) async fn before_tool_call(
        &self,
        request: &CallToolRequestParams,
        tool_name: &str,
        backend_name: &str,
    ) -> Result<ToolPreCallResult, ErrorData> {
        if !self.hooks.tool.pre {
            let state = self.hooks.tool.post.then(|| new_call_state(TOOL_CALL_KIND));
            return Ok(ToolPreCallResult { arguments: ToolArgumentsUpdate::Unchanged, state });
        }

        let tool_call_id = next_correlation_id(TOOL_CALL_KIND);
        let original_payload = tool_call_payload(request, tool_name, backend_name, &tool_call_id);
        let pre_result = self.invoke_tool_pre(original_payload).await;
        if pre_result.is_denied() {
            return Err(plugin_denied_error(pre_result, "tool call"));
        }

        let arguments = effective_pre_args(request.arguments.as_ref(), &pre_result)?;
        let state = Mutex::new(CallState { context_table: pre_result.context_table, correlation_id: tool_call_id });
        Ok(ToolPreCallResult { arguments, state: Some(Arc::new(state)) })
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

        let state = state.and_then(|state| state.downcast::<SharedCallState>().ok());
        let Some(state) = state else { return Ok(response) };

        let mut state = state.lock().await;
        let post_result = self
            .invoke_tool_post(
                tool_result_payload(tool_name, &response, &state.correlation_id),
                Some(state.context_table.clone()),
            )
            .await;
        if post_result.is_denied() {
            return Err(plugin_denied_error(post_result, "tool call"));
        }

        state.context_table = post_result.context_table.clone();
        Ok(effective_post_result(response, &post_result))
    }

    /// Runs the prompt pre hook for `prompts/list`, before any backend is contacted. Deny-only:
    /// the payload carries no routable prompt, so a modified payload has nothing to apply to.
    pub(crate) async fn before_list_prompts(
        &self,
        cursor: Option<&str>,
    ) -> Result<Option<RuntimeHookState>, ErrorData> {
        if !self.hooks.prompt.pre {
            return Ok(self.hooks.prompt.post.then(|| new_call_state(PROMPT_LIST_KIND)));
        }

        let prompt_request_id = next_correlation_id(PROMPT_LIST_KIND);
        let pre_result = self.invoke_prompt_pre(prompt_list_request_payload(cursor, &prompt_request_id)).await;
        if pre_result.is_denied() {
            return Err(plugin_denied_error(pre_result, "prompt list"));
        }

        Ok(Some(Arc::new(Mutex::new(CallState {
            context_table: pre_result.context_table,
            correlation_id: prompt_request_id,
        }))))
    }

    /// Runs the prompt post hook over the merged, namespaced listing. Read-only: a modified payload
    /// is ignored because MCP `Prompt` metadata cannot be rebuilt from CMF content parts. Denial is
    /// still honored and surfaces as an MCP error.
    pub(crate) async fn after_list_prompts(
        &self,
        prompts: &[Prompt],
        state: Option<RuntimeHookState>,
    ) -> Result<(), ErrorData> {
        if !self.hooks.prompt.post {
            return Ok(());
        }

        let state = state.and_then(|state| state.downcast::<SharedCallState>().ok());
        let Some(state) = state else { return Ok(()) };

        let mut state = state.lock().await;
        let post_result = self
            .invoke_prompt_post(
                prompt_list_result_payload(prompts, &state.correlation_id),
                Some(state.context_table.clone()),
            )
            .await;
        if post_result.is_denied() {
            return Err(plugin_denied_error(post_result, "prompt list"));
        }

        state.context_table = post_result.context_table.clone();
        Ok(())
    }

    pub(crate) async fn before_get_prompt(
        &self,
        request: &GetPromptRequestParams,
        prompt_name: &str,
        backend_name: &str,
    ) -> Result<PromptPreFetchResult, ErrorData> {
        if !self.hooks.prompt.pre {
            let state = self.hooks.prompt.post.then(|| new_call_state(PROMPT_REQUEST_KIND));
            return Ok(PromptPreFetchResult { arguments: PromptArgumentsUpdate::Unchanged, state });
        }

        let prompt_request_id = next_correlation_id(PROMPT_REQUEST_KIND);
        let original_payload = prompt_request_payload(request, prompt_name, backend_name, &prompt_request_id);
        let pre_result = self.invoke_prompt_pre(original_payload).await;
        if pre_result.is_denied() {
            return Err(plugin_denied_error(pre_result, "prompt fetch"));
        }

        let arguments = effective_pre_prompt_args(request.arguments.as_ref(), &pre_result)?;
        let state =
            Mutex::new(CallState { context_table: pre_result.context_table, correlation_id: prompt_request_id });
        Ok(PromptPreFetchResult { arguments, state: Some(Arc::new(state)) })
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

        let state = state.and_then(|state| state.downcast::<SharedCallState>().ok());
        let Some(state) = state else { return Ok(response) };

        let mut state = state.lock().await;
        let post_result = self
            .invoke_prompt_post(
                prompt_result_payload(prompt_name, &response, &state.correlation_id),
                Some(state.context_table.clone()),
            )
            .await;
        if post_result.is_denied() {
            return Err(plugin_denied_error(post_result, "prompt fetch"));
        }

        state.context_table = post_result.context_table.clone();
        effective_post_prompt_result(response, &post_result)
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

        let state = state.and_then(|state| state.downcast::<SharedCallState>().ok());
        let Some(state) = state else { return Ok(Some(event)) };

        let content = serde_json::to_value(&event).unwrap_or(serde_json::Value::Null);
        let mut state = state.lock().await;
        let post_result = self
            .invoke_tool_post(
                tool_json_result_payload(tool_name, content, false, &state.correlation_id),
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
