use std::{
    collections::HashMap,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};

use arc_swap::ArcSwap;
use cpex::cpex_core::{
    config::CpexConfig,
    factory::{PluginFactory, PluginFactoryRegistry},
};
use rmcp::{
    ErrorData,
    model::{
        CallToolRequestParams, CallToolResult, ErrorCode, GetPromptRequestParams, GetPromptResult, ReadResourceResult,
    },
    serde::{Serialize, de::DeserializeOwned},
};
use tokio::task::JoinHandle;

use contextforge_data_plane_apis::runtime_plugin_config::RuntimeRevision;

use crate::{
    config::{LoadedRuntimePluginConfig, RedisRuntimePluginConfigStore, RuntimePluginConfigStore, cpex_configs},
    context::ScopedMcpHook,
    error::GatewayPluginRuntimeError,
    hooks::{PromptPreFetchResult, ResourcePreFetchResult, RuntimeHookError, RuntimeHookState, ToolPreCallResult},
    runtime::GatewayPluginRuntime,
};

#[cfg(test)]
use crate::context::McpHookContext;

const DEFAULT_CONFIG_WATCHER_INTERVAL: Duration = Duration::from_mins(10);

pub struct CpexRuntimeRegistry {
    runtime: Arc<ArcSwap<RuntimeState>>,
    config_store: Option<Arc<dyn RuntimePluginConfigStore>>,
    factories: Arc<PluginFactoryRegistry>,
    watcher_started: AtomicBool,
    watcher_interval: Duration,
}

#[derive(Clone)]
pub struct GatewayPluginRuntimeHandle {
    runtime: Arc<ArcSwap<RuntimeState>>,
}

struct RegistryCallState {
    runtime: Arc<GatewayPluginRuntime>,
    state: Option<RuntimeHookState>,
}

enum RuntimeState {
    Active(Arc<RuntimeCatalog>),
    Failed(String),
}

#[derive(Default)]
struct RuntimeCatalog {
    runtimes: HashMap<RuntimeRevision, Arc<GatewayPluginRuntime>>,
}

impl RuntimeCatalog {
    async fn from_configs(
        configs: Vec<(RuntimeRevision, CpexConfig)>,
        factories: &PluginFactoryRegistry,
    ) -> Result<Self, GatewayPluginRuntimeError> {
        let mut runtimes = HashMap::with_capacity(configs.len());
        for (revision, config) in configs {
            if runtimes.contains_key(&revision) {
                return Err(GatewayPluginRuntimeError::ConfigWrongFormat);
            }
            let runtime = Arc::new(GatewayPluginRuntime::from_config(config, factories).await?);
            runtimes.insert(revision, runtime);
        }
        Ok(Self { runtimes })
    }

    fn get(&self, revision: &RuntimeRevision) -> Option<Arc<GatewayPluginRuntime>> {
        self.runtimes.get(revision).cloned()
    }

    fn only(&self) -> Option<(&RuntimeRevision, &Arc<GatewayPluginRuntime>)> {
        (self.runtimes.len() == 1).then(|| self.runtimes.iter().next()).flatten()
    }
}

impl Default for CpexRuntimeRegistry {
    fn default() -> Self {
        Self {
            runtime: Arc::new(ArcSwap::from_pointee(RuntimeState::Active(Arc::new(RuntimeCatalog::default())))),
            config_store: None,
            factories: Arc::new(PluginFactoryRegistry::new()),
            watcher_started: AtomicBool::new(false),
            watcher_interval: DEFAULT_CONFIG_WATCHER_INTERVAL,
        }
    }
}

impl CpexRuntimeRegistry {
    pub fn with_redis_config(redis_client: redis::Client) -> Self {
        Self { config_store: Some(Arc::new(RedisRuntimePluginConfigStore::new(redis_client))), ..Self::default() }
    }

    pub fn register_factory(
        &mut self,
        kind: impl Into<String>,
        factory: Box<dyn PluginFactory>,
    ) -> Result<(), GatewayPluginRuntimeError> {
        let factories = Arc::get_mut(&mut self.factories).ok_or(GatewayPluginRuntimeError::FactoryRegistryShared)?;
        factories.register(kind, factory);
        Ok(())
    }

    pub async fn reload(&self) -> Result<(), GatewayPluginRuntimeError> {
        reload_runtime(&self.runtime, self.config_store.as_ref(), &self.factories).await.map(|_| ())
    }

    pub async fn apply_config(
        &self,
        revision: impl Into<String>,
        config: Option<CpexConfig>,
    ) -> Result<(), GatewayPluginRuntimeError> {
        let configs = match config {
            Some(config) => vec![(
                RuntimeRevision::try_from(revision.into()).map_err(|_| GatewayPluginRuntimeError::ConfigWrongFormat)?,
                config,
            )],
            None => Vec::new(),
        };
        apply_runtime_configs(&self.runtime, &self.factories, configs).await
    }

    pub async fn apply_configs(&self, configs: Vec<(String, CpexConfig)>) -> Result<(), GatewayPluginRuntimeError> {
        let configs = configs
            .into_iter()
            .map(|(revision, config)| {
                RuntimeRevision::try_from(revision)
                    .map(|revision| (revision, config))
                    .map_err(|_| GatewayPluginRuntimeError::ConfigWrongFormat)
            })
            .collect::<Result<Vec<_>, _>>()?;
        apply_runtime_configs(&self.runtime, &self.factories, configs).await
    }

    pub fn handle(&self) -> GatewayPluginRuntimeHandle {
        GatewayPluginRuntimeHandle { runtime: Arc::clone(&self.runtime) }
    }

    fn start_config_watcher(&self, initial_config: Option<Vec<u8>>) -> Option<JoinHandle<()>> {
        let config_store = self.config_store.clone()?;
        if self.watcher_started.swap(true, Ordering::AcqRel) {
            return None;
        }

        let runtime = Arc::downgrade(&self.runtime);
        let factories = Arc::clone(&self.factories);
        let watcher_interval = self.watcher_interval;
        Some(tokio::spawn(async move {
            let mut last_applied_config = initial_config;
            loop {
                tokio::time::sleep(watcher_interval).await;
                let Some(runtime) = runtime.upgrade() else {
                    break;
                };
                match config_store.get_config().await {
                    Ok(Some(config)) => {
                        if last_applied_config.as_ref() == Some(&config.fingerprint) {
                            continue;
                        }
                        let fingerprint = config.fingerprint.clone();
                        let result = match config_to_cpex(&config) {
                            Ok(configs) => apply_runtime_configs(&runtime, &factories, configs).await,
                            Err(error) => Err(error),
                        };
                        match result {
                            Ok(()) => last_applied_config = Some(fingerprint),
                            Err(error) => {
                                tracing::warn!(%error, "failed to reload CPEX runtime plugin config");
                                set_runtime_failed(&runtime, &error);
                                last_applied_config = None;
                            },
                        }
                    },
                    Ok(None) => {
                        let error = GatewayPluginRuntimeError::ConfigMissing;
                        tracing::warn!(%error, "failed to reload CPEX runtime plugin config");
                        set_runtime_failed(&runtime, &error);
                        last_applied_config = None;
                    },
                    Err(error) => {
                        tracing::warn!(%error, "failed to load CPEX runtime plugin config");
                        set_runtime_failed(&runtime, &error);
                        last_applied_config = None;
                    },
                }
            }
        }))
    }
}

async fn reload_runtime(
    runtime: &ArcSwap<RuntimeState>,
    config_store: Option<&Arc<dyn RuntimePluginConfigStore>>,
    factories: &PluginFactoryRegistry,
) -> Result<Option<Vec<u8>>, GatewayPluginRuntimeError> {
    let Some(config_store) = config_store else {
        return Ok(None);
    };
    match load_runtime_config(config_store, factories).await {
        Ok((config, fingerprint)) => {
            drop(runtime.swap(Arc::new(RuntimeState::Active(Arc::new(config)))));
            Ok(fingerprint)
        },
        Err(error) => {
            set_runtime_failed(runtime, &error);
            Err(error)
        },
    }
}

fn config_to_cpex(
    config: &LoadedRuntimePluginConfig,
) -> Result<Vec<(RuntimeRevision, CpexConfig)>, GatewayPluginRuntimeError> {
    cpex_configs(&config.document)
}

#[cfg(test)]
impl CpexRuntimeRegistry {
    fn with_config_store(config_store: Arc<dyn RuntimePluginConfigStore>) -> Self {
        Self { config_store: Some(config_store), ..Self::default() }
    }

    fn with_config_store_interval(config_store: Arc<dyn RuntimePluginConfigStore>, watcher_interval: Duration) -> Self {
        Self { config_store: Some(config_store), watcher_interval, ..Self::default() }
    }

    async fn before_tool_call(
        &self,
        request: &CallToolRequestParams,
        tool_name: &str,
        backend_name: &str,
    ) -> Result<ToolPreCallResult, ErrorData> {
        self.handle().before_tool_call_for_test(request, tool_name, backend_name).await
    }

    async fn before_get_prompt(
        &self,
        request: &GetPromptRequestParams,
        prompt_name: &str,
        backend_name: &str,
    ) -> Result<PromptPreFetchResult, ErrorData> {
        self.handle().before_get_prompt_for_test(request, prompt_name, backend_name).await
    }

    async fn after_tool_call(
        &self,
        tool_name: &str,
        response: CallToolResult,
        state: Option<RuntimeHookState>,
    ) -> Result<CallToolResult, ErrorData> {
        self.handle().after_tool_call(tool_name, response, state).await
    }

    fn dispatch_plan_count(&self) -> usize {
        let state = self.runtime.load_full();
        let RuntimeState::Active(catalog) = state.as_ref() else {
            return 0;
        };
        catalog.only().map_or(0, |(_, runtime)| runtime.dispatch_plan_count())
    }
}

async fn apply_runtime_configs(
    runtime: &ArcSwap<RuntimeState>,
    factories: &PluginFactoryRegistry,
    configs: Vec<(RuntimeRevision, CpexConfig)>,
) -> Result<(), GatewayPluginRuntimeError> {
    let catalog = RuntimeCatalog::from_configs(configs, factories).await?;
    drop(runtime.swap(Arc::new(RuntimeState::Active(Arc::new(catalog)))));
    Ok(())
}

async fn load_runtime_config(
    config_store: &Arc<dyn RuntimePluginConfigStore>,
    factories: &PluginFactoryRegistry,
) -> Result<(RuntimeCatalog, Option<Vec<u8>>), GatewayPluginRuntimeError> {
    let config = config_store.get_config().await?.ok_or(GatewayPluginRuntimeError::ConfigMissing)?;
    let fingerprint = Some(config.fingerprint.clone());
    let runtime = RuntimeCatalog::from_configs(config_to_cpex(&config)?, factories).await?;
    Ok((runtime, fingerprint))
}

fn set_runtime_failed(runtime: &ArcSwap<RuntimeState>, error: &GatewayPluginRuntimeError) {
    drop(runtime.swap(Arc::new(RuntimeState::Failed(error.to_string()))));
}

impl CpexRuntimeRegistry {
    pub async fn initialize(&self) -> Result<Option<JoinHandle<()>>, RuntimeHookError> {
        let initial_config = reload_runtime(&self.runtime, self.config_store.as_ref(), &self.factories).await?;
        Ok(self.start_config_watcher(initial_config))
    }
}

impl GatewayPluginRuntimeHandle {
    fn current(&self) -> Arc<RuntimeState> {
        self.runtime.load_full()
    }

    pub fn configured_binding_snapshot(&self) -> Result<(String, Vec<String>), ErrorData> {
        let state = self.current();
        let RuntimeState::Active(catalog) = state.as_ref() else {
            return Err(runtime_failed_error(state.as_ref()));
        };
        let (revision, runtime) = catalog.only().ok_or_else(|| {
            runtime_binding_error("test binding snapshot requires exactly one configured runtime revision")
        })?;
        Ok((revision.as_str().to_owned(), runtime.plugin_names().iter().map(|name| name.as_str().to_owned()).collect()))
    }

    pub async fn before_tool_call(
        &self,
        request: &CallToolRequestParams,
        tool_name: &str,
        backend_name: &str,
        scope: ScopedMcpHook<'_>,
    ) -> Result<ToolPreCallResult, ErrorData> {
        let state = self.current();
        let RuntimeState::Active(catalog) = state.as_ref() else {
            return Err(runtime_failed_error(state.as_ref()));
        };
        let runtime = catalog
            .get(scope.binding_revision())
            .ok_or_else(|| runtime_binding_error("binding revision has no loaded runtime snapshot"))?;
        let mut result = runtime.before_tool_call(request, tool_name, backend_name, scope).await?;
        if result.state.is_some() {
            let state = result.state.take();
            result.state = Some(Arc::new(RegistryCallState { runtime, state }));
        }
        Ok(result)
    }

    pub async fn before_get_prompt(
        &self,
        request: &GetPromptRequestParams,
        prompt_name: &str,
        backend_name: &str,
        scope: ScopedMcpHook<'_>,
    ) -> Result<PromptPreFetchResult, ErrorData> {
        let state = self.current();
        let RuntimeState::Active(catalog) = state.as_ref() else {
            return Err(runtime_failed_error(state.as_ref()));
        };
        let runtime = catalog
            .get(scope.binding_revision())
            .ok_or_else(|| runtime_binding_error("binding revision has no loaded runtime snapshot"))?;
        let mut result = runtime.before_get_prompt(request, prompt_name, backend_name, scope).await?;
        if result.state.is_some() {
            let state = result.state.take();
            result.state = Some(Arc::new(RegistryCallState { runtime, state }));
        }
        Ok(result)
    }

    pub async fn before_read_resource(
        &self,
        resource_uri: &str,
        scope: ScopedMcpHook<'_>,
    ) -> Result<ResourcePreFetchResult, ErrorData> {
        let state = self.current();
        let RuntimeState::Active(catalog) = state.as_ref() else {
            return Err(runtime_failed_error(state.as_ref()));
        };
        let runtime = catalog
            .get(scope.binding_revision())
            .ok_or_else(|| runtime_binding_error("binding revision has no loaded runtime snapshot"))?;
        let mut result = runtime.before_read_resource(resource_uri, scope).await?;
        if result.state.is_some() {
            let state = result.state.take();
            result.state = Some(Arc::new(RegistryCallState { runtime, state }));
        }
        Ok(result)
    }

    pub async fn after_get_prompt(
        &self,
        prompt_name: &str,
        response: GetPromptResult,
        state: Option<RuntimeHookState>,
    ) -> Result<GetPromptResult, ErrorData> {
        match state.and_then(|state| state.downcast::<RegistryCallState>().ok()) {
            Some(state) => state.runtime.after_get_prompt(prompt_name, response, state.state.clone()).await,
            None => Ok(response),
        }
    }

    pub async fn after_read_resource(
        &self,
        response: ReadResourceResult,
        state: Option<RuntimeHookState>,
    ) -> Result<ReadResourceResult, ErrorData> {
        match state.and_then(|state| state.downcast::<RegistryCallState>().ok()) {
            Some(state) => state.runtime.after_read_resource(response, state.state.clone()).await,
            None => Ok(response),
        }
    }

    pub async fn after_tool_call(
        &self,
        tool_name: &str,
        response: CallToolResult,
        state: Option<RuntimeHookState>,
    ) -> Result<CallToolResult, ErrorData> {
        match state.and_then(|state| state.downcast::<RegistryCallState>().ok()) {
            Some(state) => state.runtime.after_tool_call(tool_name, response, state.state.clone()).await,
            None => Ok(response),
        }
    }

    /// Runs the tool post hooks over a streamed tool event (progress or logging
    /// notification). Returns `None` when a plugin denies the event.
    pub async fn after_stream_event<T>(
        &self,
        tool_name: &str,
        event: T,
        state: Option<RuntimeHookState>,
    ) -> Result<Option<T>, ErrorData>
    where
        T: Serialize + DeserializeOwned,
    {
        match state.and_then(|state| state.downcast::<RegistryCallState>().ok()) {
            Some(state) => state.runtime.after_tool_event(tool_name, event, state.state.clone()).await,
            None => Ok(Some(event)),
        }
    }

    #[cfg(test)]
    async fn before_tool_call_for_test(
        &self,
        request: &CallToolRequestParams,
        tool_name: &str,
        backend_name: &str,
    ) -> Result<ToolPreCallResult, ErrorData> {
        let state = self.current();
        let RuntimeState::Active(catalog) = state.as_ref() else {
            return Err(runtime_failed_error(state.as_ref()));
        };
        let (revision, runtime) = catalog
            .only()
            .ok_or_else(|| runtime_binding_error("test hook requires exactly one configured runtime revision"))?;
        self.before_tool_call(
            request,
            tool_name,
            backend_name,
            ScopedMcpHook::new(
                revision,
                runtime.plugin_names(),
                test_hook_context(crate::context::HookTarget::Tool {
                    name: tool_name.to_owned(),
                    backend: backend_name.to_owned(),
                }),
            ),
        )
        .await
    }

    #[cfg(test)]
    async fn before_get_prompt_for_test(
        &self,
        request: &GetPromptRequestParams,
        prompt_name: &str,
        backend_name: &str,
    ) -> Result<PromptPreFetchResult, ErrorData> {
        let state = self.current();
        let RuntimeState::Active(catalog) = state.as_ref() else {
            return Err(runtime_failed_error(state.as_ref()));
        };
        let (revision, runtime) = catalog
            .only()
            .ok_or_else(|| runtime_binding_error("test hook requires exactly one configured runtime revision"))?;
        self.before_get_prompt(
            request,
            prompt_name,
            backend_name,
            ScopedMcpHook::new(
                revision,
                runtime.plugin_names(),
                test_hook_context(crate::context::HookTarget::Prompt {
                    name: prompt_name.to_owned(),
                    backend: backend_name.to_owned(),
                }),
            ),
        )
        .await
    }
}

#[cfg(test)]
fn test_hook_context(target: crate::context::HookTarget) -> McpHookContext {
    use std::collections::{HashMap, HashSet};

    use crate::context::{HookHttpRequest, HookOperation, HookRequestMetadata, HookSubject};

    McpHookContext::new(
        HookRequestMetadata { request_id: "test-request".to_owned(), trace_id: None, span_id: None },
        HookSubject { id: "test-subject".to_owned(), teams: HashSet::new(), permissions: HashSet::new() },
        HookHttpRequest {
            method: "POST".to_owned(),
            path: "/mcp".to_owned(),
            authority: Some("gateway.test".to_owned()),
            scheme: Some("https".to_owned()),
            headers: HashMap::new(),
        },
        HookOperation {
            mcp_method: match &target {
                crate::context::HookTarget::Tool { .. } => "tools/call",
                crate::context::HookTarget::Resource { .. } => "resources/read",
                crate::context::HookTarget::Prompt { .. } => "prompts/get",
            }
            .to_owned(),
            virtual_host: "test-vhost".to_owned(),
            downstream_target: "test-target".to_owned(),
            target,
        },
    )
}

fn runtime_failed_error(state: &RuntimeState) -> ErrorData {
    if let RuntimeState::Failed(error) = state {
        tracing::warn!(%error, "rejecting tool call because CPEX runtime is failed");
    }
    ErrorData { code: ErrorCode::INTERNAL_ERROR, message: "Runtime plugin reload failed".into(), data: None }
}

fn runtime_binding_error(reason: &str) -> ErrorData {
    tracing::warn!(reason, "rejecting call with invalid runtime plugin binding");
    ErrorData::internal_error("Runtime plugin binding is invalid", None)
}

#[cfg(test)]
mod tests {
    use std::{
        collections::{HashMap, HashSet},
        sync::{
            Arc, Mutex,
            atomic::{AtomicUsize, Ordering},
        },
        time::Duration,
    };

    use async_trait::async_trait;
    use cpex::cpex_core::{
        cmf::{CmfHook, ContentPart, MessagePayload, Role},
        context::PluginContext,
        error::{PluginError, PluginViolation},
        factory::{PluginFactory, PluginInstance},
        hooks::{Extensions, HookHandler, PluginResult, TypedHandlerAdapter, types::cmf_hook_names},
        plugin::{Plugin, PluginConfig},
        registry::AnyHookHandler,
    };
    use rmcp::model::{
        CallToolRequestParams, CallToolResult, ContentBlock, NumberOrString, ProgressNotificationParam, ProgressToken,
    };
    use serde_json::{Value, json};
    use tokio::sync::Mutex as TokioMutex;

    use contextforge_data_plane_apis::runtime_plugin_config::{
        RUNTIME_PLUGIN_CONFIG_VERSION, RuntimePluginConfigDocument, RuntimePluginName, RuntimePluginSnapshot,
        RuntimeRevision,
    };

    use crate::config::LoadedRuntimePluginConfig;
    use crate::{CmfPluginFactory, PromptArgumentsUpdate, ToolArgumentsUpdate};

    use super::*;

    const TEST_MISSING_CONTEXT_ERROR_CODE: i64 = -32003;
    const TEST_REWRITTEN_SUM_A: i64 = 10;
    const TEST_REWRITTEN_SUM_B: i64 = 20;
    const TEST_REWRITTEN_PROMPT_TOPIC: &str = "rewritten-topic";
    const TEST_SHUTDOWN_RETRY_COUNT: usize = 20;
    const TEST_SHUTDOWN_RETRY_INTERVAL: Duration = Duration::from_millis(10);
    const TEST_WATCHER_INTERVAL: Duration = Duration::from_millis(10);
    const TEST_WATCHER_RETRY_COUNT: usize = 20;
    const TEST_WATCHER_RETRY_INTERVAL: Duration = Duration::from_millis(20);

    #[derive(Clone, Default)]
    struct MemoryConfigStore {
        config: Arc<TokioMutex<Option<RuntimePluginConfigDocument>>>,
        calls: Arc<AtomicUsize>,
    }

    impl MemoryConfigStore {
        fn with_config(config: RuntimePluginConfigDocument) -> Self {
            Self { config: Arc::new(TokioMutex::new(Some(config))), calls: Arc::new(AtomicUsize::new(0)) }
        }

        async fn set_config(&self, config: RuntimePluginConfigDocument) {
            *self.config.lock().await = Some(config);
        }

        async fn clear_config(&self) {
            *self.config.lock().await = None;
        }

        fn calls(&self) -> usize {
            self.calls.load(Ordering::SeqCst)
        }
    }

    #[async_trait]
    impl RuntimePluginConfigStore for MemoryConfigStore {
        async fn get_config(&self) -> Result<Option<LoadedRuntimePluginConfig>, GatewayPluginRuntimeError> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            Ok(self.config.lock().await.as_ref().map(loaded_config))
        }
    }

    #[derive(Debug, Default, PartialEq, Eq)]
    struct ExtensionObservation {
        request_id: Option<String>,
        trace_id: Option<String>,
        span_id: Option<String>,
        subject_id: Option<String>,
        teams: HashSet<String>,
        permissions: HashSet<String>,
        http_method: Option<String>,
        headers: HashMap<String, String>,
        tool_name: Option<String>,
        backend: Option<String>,
        virtual_host: Option<String>,
        downstream_target: Option<String>,
    }

    impl ExtensionObservation {
        fn from_extensions(extensions: &Extensions) -> Self {
            let subject = extensions.security.as_ref().and_then(|security| security.subject.as_ref());
            let tool = extensions.mcp.as_ref().and_then(|mcp| mcp.tool.as_ref());
            Self {
                request_id: extensions.request.as_ref().and_then(|request| request.request_id.clone()),
                trace_id: extensions.request.as_ref().and_then(|request| request.trace_id.clone()),
                span_id: extensions.request.as_ref().and_then(|request| request.span_id.clone()),
                subject_id: subject.and_then(|subject| subject.id.clone()),
                teams: subject.map(|subject| subject.teams.clone()).unwrap_or_default(),
                permissions: subject.map(|subject| subject.permissions.clone()).unwrap_or_default(),
                http_method: extensions.http.as_ref().and_then(|http| http.method.clone()),
                headers: extensions.http.as_ref().map(|http| http.request_headers.clone()).unwrap_or_default(),
                tool_name: tool.map(|tool| tool.name.clone()),
                backend: tool.and_then(|tool| tool.server_id.clone()),
                virtual_host: extensions.meta.as_ref().and_then(|meta| meta.scope.clone()),
                downstream_target: extensions
                    .meta
                    .as_ref()
                    .and_then(|meta| meta.properties.get("downstream_target").cloned()),
            }
        }
    }

    #[derive(Default)]
    struct Observations {
        pre_calls: usize,
        post_calls: usize,
        shutdown_calls: usize,
        pre_tool_call_id: Option<String>,
        post_tool_call_id: Option<String>,
        pre_extensions: Option<ExtensionObservation>,
        post_extensions: Vec<ExtensionObservation>,
    }

    #[derive(Clone, Copy, Default)]
    enum PreBehavior {
        #[default]
        Allow,
        Rewrite,
        SetContext,
        MutateHttpIdentity,
    }

    #[derive(Clone, Copy, Default)]
    enum PostBehavior {
        #[default]
        Allow,
        Rewrite,
        RewriteStreamEvent,
        RewriteInvalid,
        Deny,
        RequireContext,
    }

    struct TestPlugin {
        config: PluginConfig,
        observations: Arc<Mutex<Observations>>,
        execution_order: Option<Arc<Mutex<Vec<String>>>>,
        pre_behavior: PreBehavior,
        post_behavior: PostBehavior,
    }

    impl TestPlugin {
        fn new(name: &str, hooks: Vec<&'static str>) -> Self {
            Self {
                config: PluginConfig {
                    name: name.to_owned(),
                    kind: "test".to_owned(),
                    hooks: hooks.into_iter().map(str::to_owned).collect(),
                    ..Default::default()
                },
                observations: Arc::new(Mutex::new(Observations::default())),
                execution_order: None,
                pre_behavior: PreBehavior::Allow,
                post_behavior: PostBehavior::Allow,
            }
        }

        fn rewrite_from_config(config: PluginConfig) -> Self {
            Self { config, ..Self::new("generic-pre", vec![cmf_hook_names::TOOL_PRE_INVOKE]).with_pre_rewrite() }
        }

        fn with_pre_rewrite(mut self) -> Self {
            self.pre_behavior = PreBehavior::Rewrite;
            self
        }

        fn with_post_rewrite(mut self) -> Self {
            self.post_behavior = PostBehavior::Rewrite;
            self
        }

        fn with_stream_event_rewrite(mut self) -> Self {
            self.post_behavior = PostBehavior::RewriteStreamEvent;
            self
        }

        fn with_invalid_stream_rewrite(mut self) -> Self {
            self.post_behavior = PostBehavior::RewriteInvalid;
            self
        }

        fn with_post_deny(mut self) -> Self {
            self.post_behavior = PostBehavior::Deny;
            self
        }

        fn with_context_roundtrip(mut self) -> Self {
            self.pre_behavior = PreBehavior::SetContext;
            self.post_behavior = PostBehavior::RequireContext;
            self
        }

        fn with_http_identity_mutation(mut self) -> Self {
            self.pre_behavior = PreBehavior::MutateHttpIdentity;
            self
        }

        fn with_capabilities(mut self, capabilities: &[&str]) -> Self {
            self.config.capabilities = capabilities.iter().map(|capability| (*capability).to_owned()).collect();
            self
        }

        fn with_execution_order(mut self, execution_order: Arc<Mutex<Vec<String>>>) -> Self {
            self.execution_order = Some(execution_order);
            self
        }

        fn observations(&self) -> Arc<Mutex<Observations>> {
            Arc::clone(&self.observations)
        }
    }

    #[async_trait]
    impl Plugin for TestPlugin {
        fn config(&self) -> &PluginConfig {
            &self.config
        }

        async fn shutdown(&self) -> Result<(), Box<PluginError>> {
            self.observations.lock().expect("observations lock poisoned").shutdown_calls += 1;
            Ok(())
        }
    }

    #[allow(clippy::unused_async_trait_impl)]
    impl HookHandler<CmfHook> for TestPlugin {
        async fn handle(
            &self,
            payload: &MessagePayload,
            extensions: &Extensions,
            ctx: &mut PluginContext,
        ) -> PluginResult<MessagePayload> {
            let is_post = payload.message.role == Role::Tool;
            if !is_post && let Some(execution_order) = &self.execution_order {
                execution_order.lock().expect("execution order lock poisoned").push(self.config.name.clone());
            }
            let mut observations = self.observations.lock().expect("observations lock poisoned");
            if is_post {
                observations.post_calls += 1;
                observations.post_tool_call_id =
                    payload.message.get_tool_results().first().map(|result| result.tool_call_id.clone());
                observations.post_extensions.push(ExtensionObservation::from_extensions(extensions));
            } else {
                observations.pre_calls += 1;
                observations.pre_tool_call_id =
                    payload.message.get_tool_calls().first().map(|call| call.tool_call_id.clone());
                observations.pre_extensions = Some(ExtensionObservation::from_extensions(extensions));
            }
            drop(observations);

            if is_post {
                match self.post_behavior {
                    PostBehavior::Allow => PluginResult::allow(),
                    PostBehavior::Rewrite => PluginResult::modify_payload(payload.clone()),
                    PostBehavior::RewriteStreamEvent => {
                        let mut modified = payload.clone();
                        if let Some(ContentPart::ToolResult { content }) = modified
                            .message
                            .content
                            .iter_mut()
                            .find(|part| matches!(part, ContentPart::ToolResult { .. }))
                            && let Ok(mut progress) =
                                serde_json::from_value::<ProgressNotificationParam>(content.content.clone())
                        {
                            progress.message = progress.message.map(|message| format!("plugin:{message}"));
                            content.content = serde_json::to_value(progress).expect("progress serializes");
                        }
                        PluginResult::modify_payload(modified)
                    },
                    PostBehavior::RewriteInvalid => {
                        let mut modified = payload.clone();
                        if let Some(ContentPart::ToolResult { content }) = modified
                            .message
                            .content
                            .iter_mut()
                            .find(|part| matches!(part, ContentPart::ToolResult { .. }))
                        {
                            content.content = json!("not-a-stream-event");
                        }
                        PluginResult::modify_payload(modified)
                    },
                    PostBehavior::Deny => PluginResult::deny(PluginViolation::new("post_denied", "post denied")),
                    PostBehavior::RequireContext => {
                        if ctx.get_global("pre_seen") == Some(&json!(true)) {
                            PluginResult::allow()
                        } else {
                            PluginResult::deny(
                                PluginViolation::new("missing_context", "pre context missing")
                                    .with_proto_error_code(TEST_MISSING_CONTEXT_ERROR_CODE),
                            )
                        }
                    },
                }
            } else {
                match self.pre_behavior {
                    PreBehavior::Allow => PluginResult::allow(),
                    PreBehavior::Rewrite => {
                        let mut modified = payload.clone();
                        if let Some(ContentPart::ToolCall { content }) = modified
                            .message
                            .content
                            .iter_mut()
                            .find(|part| matches!(part, ContentPart::ToolCall { .. }))
                        {
                            content.arguments = HashMap::from([
                                ("a".to_owned(), json!(TEST_REWRITTEN_SUM_A)),
                                ("b".to_owned(), json!(TEST_REWRITTEN_SUM_B)),
                            ]);
                        }
                        if let Some(ContentPart::PromptRequest { content }) = modified
                            .message
                            .content
                            .iter_mut()
                            .find(|part| matches!(part, ContentPart::PromptRequest { .. }))
                        {
                            content.arguments =
                                HashMap::from([("topic".to_owned(), json!(TEST_REWRITTEN_PROMPT_TOPIC))]);
                        }
                        PluginResult::modify_payload(modified)
                    },
                    PreBehavior::SetContext => {
                        ctx.set_global("pre_seen", json!(true));
                        PluginResult::allow()
                    },
                    PreBehavior::MutateHttpIdentity => {
                        let mut modified = extensions.cow_copy();
                        if let (Some(http), Some(token)) = (&mut modified.http, &modified.http_write_token) {
                            http.write(token).method = Some("DELETE".to_owned());
                        }
                        PluginResult::modify_extensions(modified)
                    },
                }
            }
        }
    }

    struct TestPluginFactory {
        observations: Arc<Mutex<Observations>>,
        execution_order: Option<Arc<Mutex<Vec<String>>>>,
        pre_behavior: PreBehavior,
        post_behavior: PostBehavior,
    }

    impl TestPluginFactory {
        fn from_plugin(plugin: &TestPlugin) -> Self {
            Self {
                observations: Arc::clone(&plugin.observations),
                execution_order: plugin.execution_order.clone(),
                pre_behavior: plugin.pre_behavior,
                post_behavior: plugin.post_behavior,
            }
        }
    }

    impl PluginFactory for TestPluginFactory {
        fn create(&self, config: &PluginConfig) -> Result<PluginInstance, Box<PluginError>> {
            let plugin = Arc::new(TestPlugin {
                config: config.clone(),
                observations: Arc::clone(&self.observations),
                execution_order: self.execution_order.clone(),
                pre_behavior: self.pre_behavior,
                post_behavior: self.post_behavior,
            });
            let handlers = config
                .hooks
                .iter()
                .filter_map(|hook| {
                    let hook = match hook.as_str() {
                        cmf_hook_names::TOOL_PRE_INVOKE => cmf_hook_names::TOOL_PRE_INVOKE,
                        cmf_hook_names::TOOL_POST_INVOKE => cmf_hook_names::TOOL_POST_INVOKE,
                        _ => return None,
                    };
                    Some((
                        hook,
                        Arc::new(TypedHandlerAdapter::<CmfHook, _>::new(Arc::clone(&plugin)))
                            as Arc<dyn AnyHookHandler>,
                    ))
                })
                .collect();
            let plugin: Arc<dyn Plugin> = plugin;
            Ok(PluginInstance { plugin, handlers })
        }
    }

    fn sum_request(a: i64, b: i64) -> CallToolRequestParams {
        CallToolRequestParams::new("sum")
            .with_arguments(serde_json::Map::from_iter([("a".to_owned(), json!(a)), ("b".to_owned(), json!(b))]))
    }

    fn review_request(topic: &str) -> GetPromptRequestParams {
        GetPromptRequestParams::new("review")
            .with_arguments(serde_json::Map::from_iter([("topic".to_owned(), json!(topic))]))
    }

    fn progress_event() -> ProgressNotificationParam {
        ProgressNotificationParam::new(ProgressToken(NumberOrString::String("stream-token".into())), 1.0)
            .with_message("step 1/2")
    }

    fn trusted_tool_context() -> McpHookContext {
        use crate::context::{HookHttpRequest, HookOperation, HookRequestMetadata, HookSubject, HookTarget};

        McpHookContext::new(
            HookRequestMetadata {
                request_id: "trusted-request-id".to_owned(),
                trace_id: Some("4bf92f3577b34da6a3ce929d0e0e4736".to_owned()), // pragma: allowlist secret
                span_id: Some("00f067aa0ba902b7".to_owned()),                  // pragma: allowlist secret
            },
            HookSubject {
                id: "subject-123".to_owned(),
                teams: HashSet::from(["security".to_owned()]),
                permissions: HashSet::from(["tools:call".to_owned()]),
            },
            HookHttpRequest {
                method: "POST".to_owned(),
                path: "/mcp".to_owned(),
                authority: Some("gateway.test".to_owned()),
                scheme: Some("https".to_owned()),
                headers: HashMap::from([("content-type".to_owned(), "application/json".to_owned())]),
            },
            HookOperation {
                mcp_method: "tools/call".to_owned(),
                virtual_host: "virtual-host-1".to_owned(),
                downstream_target: "backend__sum".to_owned(),
                target: HookTarget::Tool { name: "sum".to_owned(), backend: "backend".to_owned() },
            },
        )
    }

    fn config_document(cpex: Value) -> RuntimePluginConfigDocument {
        config_document_at("test-bindings-v1", cpex)
    }

    fn config_document_at(revision: &str, cpex: Value) -> RuntimePluginConfigDocument {
        let cpex: CpexConfig = serde_json::from_value(cpex).expect("test CPEX config parses");
        RuntimePluginConfigDocument {
            version: RUNTIME_PLUGIN_CONFIG_VERSION,
            snapshots: HashMap::from([(
                runtime_revision(revision),
                RuntimePluginSnapshot::try_from(cpex).expect("test CPEX config is dataplane-supported"),
            )]),
        }
    }

    fn test_revision() -> RuntimeRevision {
        RuntimeRevision::try_from("test-bindings-v1".to_owned()).expect("valid test revision")
    }

    fn runtime_revision(value: &str) -> RuntimeRevision {
        RuntimeRevision::try_from(value.to_owned()).expect("valid test revision")
    }

    fn runtime_plugin_names(names: &[&str]) -> Vec<RuntimePluginName> {
        names
            .iter()
            .map(|name| RuntimePluginName::try_from((*name).to_owned()).expect("valid test plugin name"))
            .collect()
    }

    fn loaded_config(document: &RuntimePluginConfigDocument) -> LoadedRuntimePluginConfig {
        LoadedRuntimePluginConfig::decode(serde_json::to_vec(document).expect("test CPEX config serializes"))
            .expect("test CPEX config decodes")
    }

    fn plugin_config(plugins: &[Arc<TestPlugin>]) -> RuntimePluginConfigDocument {
        config_document(json!({
            "plugins": plugins.iter().map(|plugin| {
                json!({
                    "name": plugin.config.name.clone(),
                    "kind": plugin.config.kind.clone(),
                    "hooks": plugin.config.hooks.clone(),
                    "capabilities": plugin.config.capabilities.clone(),
                })
            }).collect::<Vec<_>>()
        }))
    }

    fn expect_runtime_failed(result: Result<ToolPreCallResult, ErrorData>) -> ErrorData {
        match result {
            Ok(_) => panic!("runtime should be failed"),
            Err(error) => error,
        }
    }

    async fn runtime_with_plugin(plugin: &Arc<TestPlugin>, config: RuntimePluginConfigDocument) -> CpexRuntimeRegistry {
        let mut runtime = CpexRuntimeRegistry::with_config_store(Arc::new(MemoryConfigStore::with_config(config)));
        runtime
            .register_factory("test", Box::new(TestPluginFactory::from_plugin(plugin)))
            .expect("test factory registers");
        runtime.initialize().await.expect("runtime initializes");
        runtime
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 1)]
    async fn runtime_config_store_is_loaded_on_initialize() {
        let config_store = MemoryConfigStore::with_config(config_document(json!({ "plugins": [] })));
        let runtime = CpexRuntimeRegistry::with_config_store(Arc::new(config_store.clone()));

        let handle = runtime.initialize().await.expect("runtime initializes");

        assert!(handle.is_some());
        assert!(config_store.calls() >= 1);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 1)]
    async fn missing_runtime_plugin_config_is_rejected_on_initialize() {
        let runtime = CpexRuntimeRegistry::with_config_store(Arc::new(MemoryConfigStore::default()));

        let error = runtime.initialize().await.expect_err("missing config is rejected");

        assert_eq!("runtime plugin config is missing", error.to_string());
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 1)]
    async fn invalid_runtime_plugin_config_documents_are_rejected() {
        let mut invalid = config_document(json!({ "plugins": [] }));
        invalid.version = RUNTIME_PLUGIN_CONFIG_VERSION + 1;
        for config in [invalid] {
            let runtime = CpexRuntimeRegistry::with_config_store(Arc::new(MemoryConfigStore::with_config(config)));
            let error = runtime.initialize().await.expect_err("invalid config is rejected");

            assert_eq!("runtime plugin config is in wrong format", error.to_string());
        }
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 1)]
    async fn unsupported_runtime_plugin_config_is_rejected() {
        for cpex in [
            json!({ "plugins": [{ "name": "llm", "kind": "test", "hooks": [cmf_hook_names::LLM_INPUT] }] }),
            json!({
                "plugins": [{
                    "name": "overprivileged",
                    "kind": "test",
                    "hooks": [cmf_hook_names::TOOL_PRE_INVOKE],
                    "capabilities": ["read_everything"]
                }]
            }),
        ] {
            let runtime =
                CpexRuntimeRegistry::with_config_store(Arc::new(MemoryConfigStore::with_config(config_document(cpex))));
            let error = runtime.initialize().await.expect_err("unsupported config is rejected");

            assert_eq!("runtime plugin config is unsupported", error.to_string());
        }
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 1)]
    async fn prompt_hooks_are_accepted_config() {
        let plugin = Arc::new(TestPlugin::new("prompt", vec![cmf_hook_names::PROMPT_PRE_FETCH]));
        // runtime_with_plugin initializes and expects success
        runtime_with_plugin(&plugin, plugin_config(&[Arc::clone(&plugin)])).await;
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 1)]
    async fn runtime_config_loads_registered_factory_plugin() {
        let plugin =
            Arc::new(TestPlugin::new("configured-pre", vec![cmf_hook_names::TOOL_PRE_INVOKE]).with_pre_rewrite());
        let observations = plugin.observations();
        let runtime = runtime_with_plugin(&plugin, plugin_config(&[Arc::clone(&plugin)])).await;

        let result = runtime.before_tool_call(&sum_request(1, 2), "sum", "backend").await.expect("pre hook runs");

        assert!(matches!(result.arguments, ToolArgumentsUpdate::Replace(Some(_))));
        assert_eq!(1, observations.lock().expect("observations lock poisoned").pre_calls);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 1)]
    async fn runtime_config_loads_generic_cmf_factory_plugin() {
        let config = config_document(json!({
            "plugins": [{
                "name": "generic-pre",
                "kind": "generic",
                "hooks": [cmf_hook_names::TOOL_PRE_INVOKE]
            }]
        }));
        let mut runtime = CpexRuntimeRegistry::with_config_store(Arc::new(MemoryConfigStore::with_config(config)));
        runtime
            .register_factory("generic", Box::new(CmfPluginFactory::new(TestPlugin::rewrite_from_config)))
            .expect("test factory registers");
        runtime.initialize().await.expect("runtime initializes");

        let result = runtime.before_tool_call(&sum_request(1, 2), "sum", "backend").await.expect("pre hook runs");

        assert!(matches!(result.arguments, ToolArgumentsUpdate::Replace(Some(_))));
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 1)]
    async fn generic_cmf_factory_registers_prompt_only_plugin() {
        let config = config_document(json!({
            "plugins": [{
                "name": "generic-prompt",
                "kind": "generic",
                "hooks": [cmf_hook_names::PROMPT_PRE_FETCH]
            }]
        }));
        let mut runtime = CpexRuntimeRegistry::with_config_store(Arc::new(MemoryConfigStore::with_config(config)));
        runtime
            .register_factory("generic", Box::new(CmfPluginFactory::new(TestPlugin::rewrite_from_config)))
            .expect("test factory registers");
        runtime.initialize().await.expect("runtime initializes");

        let result = runtime
            .before_get_prompt(&review_request("weather"), "review", "backend")
            .await
            .expect("prompt pre hook runs");

        assert!(
            matches!(result.arguments, PromptArgumentsUpdate::Replace(Some(_))),
            "the prompt hook must actually run, not merely be accepted by config validation"
        );
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 1)]
    async fn generic_cmf_factory_registers_mixed_tool_and_prompt_plugin() {
        let config = config_document(json!({
            "plugins": [{
                "name": "generic-mixed",
                "kind": "generic",
                "hooks": [cmf_hook_names::TOOL_PRE_INVOKE, cmf_hook_names::PROMPT_PRE_FETCH]
            }]
        }));
        let mut runtime = CpexRuntimeRegistry::with_config_store(Arc::new(MemoryConfigStore::with_config(config)));
        runtime
            .register_factory("generic", Box::new(CmfPluginFactory::new(TestPlugin::rewrite_from_config)))
            .expect("test factory registers");
        runtime.initialize().await.expect("runtime initializes");

        let tool = runtime.before_tool_call(&sum_request(1, 2), "sum", "backend").await.expect("tool pre hook runs");
        let prompt = runtime
            .before_get_prompt(&review_request("weather"), "review", "backend")
            .await
            .expect("prompt pre hook runs");

        assert!(matches!(tool.arguments, ToolArgumentsUpdate::Replace(Some(_))));
        assert!(matches!(prompt.arguments, PromptArgumentsUpdate::Replace(Some(_))));
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 1)]
    async fn runtime_reload_replaces_and_clears_current_runtime() {
        let plugin =
            Arc::new(TestPlugin::new("configured-pre", vec![cmf_hook_names::TOOL_PRE_INVOKE]).with_pre_rewrite());
        let observations = plugin.observations();
        let config_store = MemoryConfigStore::with_config(config_document(json!({ "plugins": [] })));
        let mut runtime = CpexRuntimeRegistry::with_config_store(Arc::new(config_store.clone()));
        runtime
            .register_factory("test", Box::new(TestPluginFactory::from_plugin(&plugin)))
            .expect("test factory registers");
        runtime.initialize().await.expect("runtime initializes");

        let result = runtime.before_tool_call(&sum_request(1, 2), "sum", "backend").await.expect("pre hook skips");
        assert!(matches!(result.arguments, ToolArgumentsUpdate::Unchanged));

        config_store.set_config(plugin_config(&[Arc::clone(&plugin)])).await;
        runtime.reload().await.expect("runtime reloads");
        let result = runtime.before_tool_call(&sum_request(1, 2), "sum", "backend").await.expect("pre hook runs");
        assert!(matches!(result.arguments, ToolArgumentsUpdate::Replace(Some(_))));

        config_store.set_config(config_document_at("test-bindings-v2", json!({ "plugins": [] }))).await;
        runtime.reload().await.expect("runtime reloads");
        let result = runtime.before_tool_call(&sum_request(1, 2), "sum", "backend").await.expect("pre hook skips");
        assert!(matches!(result.arguments, ToolArgumentsUpdate::Unchanged));
        assert_eq!(1, observations.lock().expect("observations lock poisoned").pre_calls);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 1)]
    async fn failed_runtime_reload_rejects_new_calls_until_valid_reload() {
        let plugin =
            Arc::new(TestPlugin::new("configured-pre", vec![cmf_hook_names::TOOL_PRE_INVOKE]).with_pre_rewrite());
        let observations = plugin.observations();
        let config_store = MemoryConfigStore::with_config(plugin_config(&[Arc::clone(&plugin)]));
        let mut runtime = CpexRuntimeRegistry::with_config_store(Arc::new(config_store.clone()));
        runtime
            .register_factory("test", Box::new(TestPluginFactory::from_plugin(&plugin)))
            .expect("test factory registers");
        runtime.initialize().await.expect("runtime initializes");

        let mut invalid = config_document(json!({ "plugins": [] }));
        invalid.version = RUNTIME_PLUGIN_CONFIG_VERSION + 1;
        config_store.set_config(invalid).await;
        runtime.reload().await.expect_err("invalid reload fails");
        let error = expect_runtime_failed(runtime.before_tool_call(&sum_request(1, 2), "sum", "backend").await);
        assert_eq!(ErrorCode::INTERNAL_ERROR, error.code);
        assert_eq!("Runtime plugin reload failed", error.message);

        config_store.clear_config().await;
        runtime.reload().await.expect_err("missing reload fails");
        let error = expect_runtime_failed(runtime.before_tool_call(&sum_request(1, 2), "sum", "backend").await);
        assert_eq!(ErrorCode::INTERNAL_ERROR, error.code);
        assert_eq!("Runtime plugin reload failed", error.message);

        config_store.set_config(plugin_config(&[Arc::clone(&plugin)])).await;
        runtime.reload().await.expect("runtime recovers");
        let result = runtime.before_tool_call(&sum_request(1, 2), "sum", "backend").await.expect("pre hook runs");
        assert!(matches!(result.arguments, ToolArgumentsUpdate::Replace(Some(_))));
        assert_eq!(1, observations.lock().expect("observations lock poisoned").pre_calls);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 1)]
    async fn combined_plugin_preserves_context_from_pre_to_post_across_replacement() {
        let plugin = Arc::new(
            TestPlugin::new("context", vec![cmf_hook_names::TOOL_PRE_INVOKE, cmf_hook_names::TOOL_POST_INVOKE])
                .with_context_roundtrip(),
        );
        let observations = plugin.observations();
        let config_store = MemoryConfigStore::with_config(plugin_config(&[Arc::clone(&plugin)]));
        let mut runtime = CpexRuntimeRegistry::with_config_store(Arc::new(config_store.clone()));
        runtime
            .register_factory("test", Box::new(TestPluginFactory::from_plugin(&plugin)))
            .expect("test factory registers");
        runtime.initialize().await.expect("runtime initializes");

        let pre = runtime.before_tool_call(&sum_request(1, 2), "sum", "backend").await.expect("pre hook runs");
        config_store.set_config(config_document_at("test-bindings-v2", json!({ "plugins": [] }))).await;
        runtime.reload().await.expect("runtime reloads");
        let response = CallToolResult::success(vec![ContentBlock::text("3")]);
        runtime.after_tool_call("sum", response, pre.state).await.expect("post hook runs");

        let observations = observations.lock().expect("observations lock poisoned");
        assert_eq!(1, observations.post_calls);
        assert_eq!(observations.pre_tool_call_id, observations.post_tool_call_id);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 1)]
    async fn typed_extensions_are_capability_gated_and_stable_across_tool_lifecycle() {
        let plugin = Arc::new(
            TestPlugin::new("trusted-context", vec![cmf_hook_names::TOOL_PRE_INVOKE, cmf_hook_names::TOOL_POST_INVOKE])
                .with_context_roundtrip()
                .with_capabilities(&["read_subject", "read_teams", "read_permissions", "read_headers"]),
        );
        let observations = plugin.observations();
        let runtime = runtime_with_plugin(&plugin, plugin_config(&[Arc::clone(&plugin)])).await;
        let revision = test_revision();
        let plugin_names = runtime_plugin_names(&["trusted-context"]);

        let pre = runtime
            .handle()
            .before_tool_call(
                &sum_request(1, 2),
                "sum",
                "backend",
                ScopedMcpHook::new(&revision, &plugin_names, trusted_tool_context()),
            )
            .await
            .expect("pre hook runs");
        runtime
            .handle()
            .after_stream_event("sum", progress_event(), pre.state.clone())
            .await
            .expect("stream post hook runs");
        runtime
            .after_tool_call("sum", CallToolResult::success(vec![ContentBlock::text("3")]), pre.state)
            .await
            .expect("post hook runs");

        let observations = observations.lock().expect("observations lock poisoned");
        let pre = observations.pre_extensions.as_ref().expect("pre extensions observed");
        assert_eq!(2, observations.post_extensions.len());
        assert!(observations.post_extensions.iter().all(|post| post == pre));
        let post = &observations.post_extensions[1];
        assert_eq!(Some("trusted-request-id"), pre.request_id.as_deref());
        assert_eq!(pre.request_id, post.request_id);
        assert_eq!(pre.trace_id, post.trace_id);
        assert_eq!(pre.span_id, post.span_id);
        assert_eq!(Some("subject-123"), pre.subject_id.as_deref());
        assert_eq!(pre.subject_id, post.subject_id);
        assert_eq!(HashSet::from(["security".to_owned()]), pre.teams);
        assert_eq!(HashSet::from(["tools:call".to_owned()]), pre.permissions);
        assert_eq!(Some("POST"), pre.http_method.as_deref());
        assert_eq!(Some("application/json"), pre.headers.get("content-type").map(String::as_str));
        assert_eq!(Some("sum"), pre.tool_name.as_deref());
        assert_eq!(Some("backend"), pre.backend.as_deref());
        assert_eq!(Some("virtual-host-1"), pre.virtual_host.as_deref());
        assert_eq!(Some("backend__sum"), pre.downstream_target.as_deref());
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 1)]
    async fn plugins_without_capabilities_cannot_see_subject_or_http() {
        let plugin = Arc::new(TestPlugin::new("least-privilege", vec![cmf_hook_names::TOOL_PRE_INVOKE]));
        let observations = plugin.observations();
        let runtime = runtime_with_plugin(&plugin, plugin_config(&[Arc::clone(&plugin)])).await;
        let revision = test_revision();
        let plugin_names = runtime_plugin_names(&["least-privilege"]);

        runtime
            .handle()
            .before_tool_call(
                &sum_request(1, 2),
                "sum",
                "backend",
                ScopedMcpHook::new(&revision, &plugin_names, trusted_tool_context()),
            )
            .await
            .expect("pre hook runs");

        let observations = observations.lock().expect("observations lock poisoned");
        let extensions = observations.pre_extensions.as_ref().expect("extensions observed");
        assert_eq!(Some("trusted-request-id"), extensions.request_id.as_deref());
        assert_eq!(Some("sum"), extensions.tool_name.as_deref());
        assert_eq!(Some("virtual-host-1"), extensions.virtual_host.as_deref());
        assert!(extensions.subject_id.is_none());
        assert!(extensions.teams.is_empty());
        assert!(extensions.permissions.is_empty());
        assert!(extensions.http_method.is_none());
        assert!(extensions.headers.is_empty());
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 1)]
    async fn scoped_bindings_select_distinct_plugin_lineups() {
        let mut first = TestPlugin::new("first", vec![cmf_hook_names::TOOL_PRE_INVOKE]);
        first.config.kind = "first-kind".to_owned();
        let first = Arc::new(first);
        let first_observations = first.observations();
        let mut second = TestPlugin::new("second", vec![cmf_hook_names::TOOL_PRE_INVOKE]);
        second.config.kind = "second-kind".to_owned();
        let second = Arc::new(second);
        let second_observations = second.observations();
        let config = plugin_config(&[Arc::clone(&first), Arc::clone(&second)]);
        let mut runtime = CpexRuntimeRegistry::with_config_store(Arc::new(MemoryConfigStore::with_config(config)));
        runtime
            .register_factory("first-kind", Box::new(TestPluginFactory::from_plugin(&first)))
            .expect("first factory registers");
        runtime
            .register_factory("second-kind", Box::new(TestPluginFactory::from_plugin(&second)))
            .expect("second factory registers");
        runtime.initialize().await.expect("runtime initializes");
        let revision = test_revision();
        let first_binding = runtime_plugin_names(&["first"]);
        let second_binding = runtime_plugin_names(&["second"]);

        runtime
            .handle()
            .before_tool_call(
                &sum_request(1, 2),
                "sum",
                "backend",
                ScopedMcpHook::new(&revision, &first_binding, trusted_tool_context()),
            )
            .await
            .expect("first scoped call runs");
        runtime
            .handle()
            .before_tool_call(
                &sum_request(1, 2),
                "sum",
                "backend",
                ScopedMcpHook::new(&revision, &second_binding, trusted_tool_context()),
            )
            .await
            .expect("second scoped call runs");

        assert_eq!(1, first_observations.lock().expect("first observations lock poisoned").pre_calls);
        assert_eq!(1, second_observations.lock().expect("second observations lock poisoned").pre_calls);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 1)]
    async fn revision_keyed_catalog_selects_exact_runtime_snapshot() {
        let mut first = TestPlugin::new("first", vec![cmf_hook_names::TOOL_PRE_INVOKE]);
        first.config.kind = "first-kind".to_owned();
        let first = Arc::new(first);
        let first_observations = first.observations();
        let mut second = TestPlugin::new("second", vec![cmf_hook_names::TOOL_PRE_INVOKE]);
        second.config.kind = "second-kind".to_owned();
        let second = Arc::new(second);
        let second_observations = second.observations();
        let mut runtime = CpexRuntimeRegistry::default();
        runtime
            .register_factory("first-kind", Box::new(TestPluginFactory::from_plugin(&first)))
            .expect("first factory registers");
        runtime
            .register_factory("second-kind", Box::new(TestPluginFactory::from_plugin(&second)))
            .expect("second factory registers");
        runtime
            .apply_configs(vec![
                ("revision-1".to_owned(), CpexConfig { plugins: vec![first.config.clone()], ..Default::default() }),
                ("revision-2".to_owned(), CpexConfig { plugins: vec![second.config.clone()], ..Default::default() }),
            ])
            .await
            .expect("revision catalog applies");

        let first_revision = runtime_revision("revision-1");
        let first_binding = runtime_plugin_names(&["first"]);
        runtime
            .handle()
            .before_tool_call(
                &sum_request(1, 2),
                "sum",
                "backend",
                ScopedMcpHook::new(&first_revision, &first_binding, trusted_tool_context()),
            )
            .await
            .expect("first revision runs");
        let second_revision = runtime_revision("revision-2");
        let second_binding = runtime_plugin_names(&["second"]);
        runtime
            .handle()
            .before_tool_call(
                &sum_request(1, 2),
                "sum",
                "backend",
                ScopedMcpHook::new(&second_revision, &second_binding, trusted_tool_context()),
            )
            .await
            .expect("second revision runs");

        assert_eq!(1, first_observations.lock().expect("first observations lock poisoned").pre_calls);
        assert_eq!(1, second_observations.lock().expect("second observations lock poisoned").pre_calls);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 1)]
    async fn ordered_binding_dispatch_plan_is_cached_and_stable() {
        let execution_order = Arc::new(Mutex::new(Vec::new()));
        let mut first = TestPlugin::new("first", vec![cmf_hook_names::TOOL_PRE_INVOKE])
            .with_execution_order(Arc::clone(&execution_order));
        first.config.kind = "first-kind".to_owned();
        let first = Arc::new(first);
        let mut second = TestPlugin::new("second", vec![cmf_hook_names::TOOL_PRE_INVOKE])
            .with_execution_order(Arc::clone(&execution_order));
        second.config.kind = "second-kind".to_owned();
        let second = Arc::new(second);
        let config = plugin_config(&[Arc::clone(&first), Arc::clone(&second)]);
        let mut runtime = CpexRuntimeRegistry::with_config_store(Arc::new(MemoryConfigStore::with_config(config)));
        runtime
            .register_factory("first-kind", Box::new(TestPluginFactory::from_plugin(&first)))
            .expect("first factory registers");
        runtime
            .register_factory("second-kind", Box::new(TestPluginFactory::from_plugin(&second)))
            .expect("second factory registers");
        runtime.initialize().await.expect("runtime initializes");
        let revision = test_revision();
        let binding = runtime_plugin_names(&["second", "first"]);

        for _ in 0..2 {
            runtime
                .handle()
                .before_tool_call(
                    &sum_request(1, 2),
                    "sum",
                    "backend",
                    ScopedMcpHook::new(&revision, &binding, trusted_tool_context()),
                )
                .await
                .expect("ordered scoped call runs");
        }

        assert_eq!(vec!["second", "first", "second", "first"], *execution_order.lock().unwrap());
        assert_eq!(1, runtime.dispatch_plan_count());
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 1)]
    async fn unknown_scoped_plugin_binding_fails_closed() {
        let plugin = Arc::new(TestPlugin::new("known", vec![cmf_hook_names::TOOL_PRE_INVOKE]));
        let observations = plugin.observations();
        let runtime = runtime_with_plugin(&plugin, plugin_config(&[Arc::clone(&plugin)])).await;
        let revision = test_revision();
        let binding = runtime_plugin_names(&["unknown"]);

        let result = runtime
            .handle()
            .before_tool_call(
                &sum_request(1, 2),
                "sum",
                "backend",
                ScopedMcpHook::new(&revision, &binding, trusted_tool_context()),
            )
            .await;
        let Err(error) = result else { panic!("unknown binding is accepted") };

        assert_eq!(ErrorCode::INTERNAL_ERROR, error.code);
        assert_eq!(0, observations.lock().expect("observations lock poisoned").pre_calls);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 1)]
    async fn stale_scoped_plugin_binding_fails_closed() {
        let plugin = Arc::new(TestPlugin::new("known", vec![cmf_hook_names::TOOL_PRE_INVOKE]));
        let observations = plugin.observations();
        let runtime = runtime_with_plugin(&plugin, plugin_config(&[Arc::clone(&plugin)])).await;
        let revision = runtime_revision("stale-revision");
        let binding = runtime_plugin_names(&["known"]);

        let result = runtime
            .handle()
            .before_tool_call(
                &sum_request(1, 2),
                "sum",
                "backend",
                ScopedMcpHook::new(&revision, &binding, trusted_tool_context()),
            )
            .await;
        let Err(error) = result else { panic!("stale binding is accepted") };

        assert_eq!(ErrorCode::INTERNAL_ERROR, error.code);
        assert_eq!(0, observations.lock().expect("observations lock poisoned").pre_calls);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 1)]
    async fn plugin_cannot_mutate_trusted_http_route_identity() {
        let plugin = Arc::new(
            TestPlugin::new("route-mutator", vec![cmf_hook_names::TOOL_PRE_INVOKE])
                .with_http_identity_mutation()
                .with_capabilities(&["read_headers", "write_headers"]),
        );
        let runtime = runtime_with_plugin(&plugin, plugin_config(&[Arc::clone(&plugin)])).await;
        let revision = test_revision();
        let binding = runtime_plugin_names(&["route-mutator"]);

        let result = runtime
            .handle()
            .before_tool_call(
                &sum_request(1, 2),
                "sum",
                "backend",
                ScopedMcpHook::new(&revision, &binding, trusted_tool_context()),
            )
            .await;
        let Err(error) = result else { panic!("trusted HTTP identity mutation is accepted") };

        assert_eq!(ErrorCode::INTERNAL_ERROR, error.code);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 1)]
    async fn post_only_binding_receives_initial_typed_extensions() {
        let plugin = Arc::new(
            TestPlugin::new("post-only", vec![cmf_hook_names::TOOL_POST_INVOKE])
                .with_capabilities(&["read_subject", "read_headers"]),
        );
        let observations = plugin.observations();
        let runtime = runtime_with_plugin(&plugin, plugin_config(&[Arc::clone(&plugin)])).await;
        let revision = test_revision();
        let binding = runtime_plugin_names(&["post-only"]);

        let pre = runtime
            .handle()
            .before_tool_call(
                &sum_request(1, 2),
                "sum",
                "backend",
                ScopedMcpHook::new(&revision, &binding, trusted_tool_context()),
            )
            .await
            .expect("post-only binding prepares state");
        let registry_state = pre
            .state
            .clone()
            .expect("post-only binding stores lifecycle state")
            .downcast::<RegistryCallState>()
            .expect("registry state is retained");
        let runtime_state = registry_state.state.as_ref().expect("runtime state is retained");
        assert_eq!(Some(false), crate::runtime::tool_state_has_context(runtime_state));
        runtime
            .after_tool_call("sum", CallToolResult::success(vec![ContentBlock::text("3")]), pre.state)
            .await
            .expect("post hook runs");

        let observations = observations.lock().expect("observations lock poisoned");
        let extensions = observations.post_extensions.first().expect("post extensions observed");
        assert_eq!(Some("trusted-request-id"), extensions.request_id.as_deref());
        assert_eq!(Some("subject-123"), extensions.subject_id.as_deref());
        assert_eq!(Some("POST"), extensions.http_method.as_deref());
        assert_eq!(Some("sum"), extensions.tool_name.as_deref());
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 1)]
    async fn post_only_runtime_does_not_apply_new_post_hook_to_in_flight_call() {
        let plugin = Arc::new(TestPlugin::new("post", vec![cmf_hook_names::TOOL_POST_INVOKE]).with_post_rewrite());
        let observations = plugin.observations();
        let config_store = MemoryConfigStore::with_config(config_document(json!({ "plugins": [] })));
        let mut runtime = CpexRuntimeRegistry::with_config_store(Arc::new(config_store.clone()));
        runtime
            .register_factory("test", Box::new(TestPluginFactory::from_plugin(&plugin)))
            .expect("test factory registers");
        runtime.initialize().await.expect("runtime initializes");

        let pre = runtime.before_tool_call(&sum_request(1, 2), "sum", "backend").await.expect("pre hook skips");
        config_store.set_config(plugin_config(&[Arc::clone(&plugin)])).await;
        runtime.reload().await.expect("runtime reloads");
        let response = CallToolResult::success(vec![ContentBlock::text("3")]);
        runtime.after_tool_call("sum", response, pre.state).await.expect("post hook skips");

        assert_eq!(0, observations.lock().expect("observations lock poisoned").post_calls);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 1)]
    async fn stream_event_without_state_skips_post_hook() {
        let plugin =
            Arc::new(TestPlugin::new("post", vec![cmf_hook_names::TOOL_POST_INVOKE]).with_stream_event_rewrite());
        let observations = plugin.observations();
        let runtime = runtime_with_plugin(&plugin, plugin_config(&[Arc::clone(&plugin)])).await;

        let event = runtime.handle().after_stream_event("sum", progress_event(), None).await.expect("event passes");

        assert_eq!(Some("step 1/2"), event.expect("event is kept").message.as_deref());
        assert_eq!(0, observations.lock().expect("observations lock poisoned").post_calls);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 1)]
    async fn stream_event_is_rewritten_by_post_hook() {
        let plugin =
            Arc::new(TestPlugin::new("post", vec![cmf_hook_names::TOOL_POST_INVOKE]).with_stream_event_rewrite());
        let observations = plugin.observations();
        let runtime = runtime_with_plugin(&plugin, plugin_config(&[Arc::clone(&plugin)])).await;

        let pre = runtime.before_tool_call(&sum_request(1, 2), "sum", "backend").await.expect("pre state is created");
        let event =
            runtime.handle().after_stream_event("sum", progress_event(), pre.state).await.expect("event passes");

        assert_eq!(Some("plugin:step 1/2"), event.expect("event is kept").message.as_deref());
        assert_eq!(1, observations.lock().expect("observations lock poisoned").post_calls);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 1)]
    async fn denied_stream_event_is_dropped() {
        let plugin = Arc::new(TestPlugin::new("post", vec![cmf_hook_names::TOOL_POST_INVOKE]).with_post_deny());
        let runtime = runtime_with_plugin(&plugin, plugin_config(&[Arc::clone(&plugin)])).await;

        let pre = runtime.before_tool_call(&sum_request(1, 2), "sum", "backend").await.expect("pre state is created");
        let event = runtime
            .handle()
            .after_stream_event("sum", progress_event(), pre.state)
            .await
            .expect("deny drops the event");

        assert!(event.is_none());
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 1)]
    async fn invalid_stream_event_rewrite_is_rejected() {
        let plugin =
            Arc::new(TestPlugin::new("post", vec![cmf_hook_names::TOOL_POST_INVOKE]).with_invalid_stream_rewrite());
        let runtime = runtime_with_plugin(&plugin, plugin_config(&[Arc::clone(&plugin)])).await;

        let pre = runtime.before_tool_call(&sum_request(1, 2), "sum", "backend").await.expect("pre state is created");
        let error = runtime
            .handle()
            .after_stream_event("sum", progress_event(), pre.state)
            .await
            .expect_err("invalid rewrite is rejected");

        assert_eq!(ErrorCode::INVALID_PARAMS, error.code);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 1)]
    async fn replaced_runtime_shutdowns_on_drop() {
        let plugin = Arc::new(TestPlugin::new("pre", vec![cmf_hook_names::TOOL_PRE_INVOKE]).with_pre_rewrite());
        let observations = plugin.observations();
        let config_store = MemoryConfigStore::with_config(plugin_config(&[Arc::clone(&plugin)]));
        let mut runtime = CpexRuntimeRegistry::with_config_store(Arc::new(config_store.clone()));
        runtime
            .register_factory("test", Box::new(TestPluginFactory::from_plugin(&plugin)))
            .expect("test factory registers");
        runtime.initialize().await.expect("runtime initializes");

        config_store.set_config(config_document(json!({ "plugins": [] }))).await;
        runtime.reload().await.expect("runtime reloads");

        for _ in 0..TEST_SHUTDOWN_RETRY_COUNT {
            if observations.lock().expect("observations lock poisoned").shutdown_calls > 0 {
                return;
            }
            tokio::time::sleep(TEST_SHUTDOWN_RETRY_INTERVAL).await;
        }
        panic!("replaced runtime did not shut down");
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 1)]
    async fn watcher_applies_config_changes() {
        let plugin =
            Arc::new(TestPlugin::new("configured-pre", vec![cmf_hook_names::TOOL_PRE_INVOKE]).with_pre_rewrite());
        let observations = plugin.observations();
        let config_store = MemoryConfigStore::with_config(config_document(json!({ "plugins": [] })));
        let mut runtime =
            CpexRuntimeRegistry::with_config_store_interval(Arc::new(config_store.clone()), TEST_WATCHER_INTERVAL);
        runtime
            .register_factory("test", Box::new(TestPluginFactory::from_plugin(&plugin)))
            .expect("test factory registers");
        let handle = runtime.initialize().await.expect("runtime initializes");
        assert!(handle.is_some());

        config_store.set_config(plugin_config(&[Arc::clone(&plugin)])).await;
        for _ in 0..TEST_WATCHER_RETRY_COUNT {
            let result = runtime.before_tool_call(&sum_request(1, 2), "sum", "backend").await.expect("pre hook runs");
            if matches!(result.arguments, ToolArgumentsUpdate::Replace(Some(_))) {
                config_store.clear_config().await;
                tokio::time::sleep(TEST_WATCHER_INTERVAL + TEST_WATCHER_RETRY_INTERVAL).await;
                let error = expect_runtime_failed(runtime.before_tool_call(&sum_request(1, 2), "sum", "backend").await);
                assert_eq!(ErrorCode::INTERNAL_ERROR, error.code);

                config_store.set_config(plugin_config(&[Arc::clone(&plugin)])).await;
                for _ in 0..TEST_WATCHER_RETRY_COUNT {
                    if let Ok(result) = runtime.before_tool_call(&sum_request(1, 2), "sum", "backend").await
                        && matches!(result.arguments, ToolArgumentsUpdate::Replace(Some(_)))
                    {
                        assert_eq!(2, observations.lock().expect("observations lock poisoned").pre_calls);
                        return;
                    }
                    tokio::time::sleep(TEST_WATCHER_RETRY_INTERVAL).await;
                }
                panic!("config watcher did not recover from missing plugin config");
            }
            tokio::time::sleep(TEST_WATCHER_RETRY_INTERVAL).await;
        }
        panic!("config watcher did not apply plugin config");
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 1)]
    async fn initialize_without_config_store_returns_no_watcher() {
        let runtime = CpexRuntimeRegistry::default();

        let handle = runtime.initialize().await.expect("runtime initializes");

        assert!(handle.is_none());
    }
}
