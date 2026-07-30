use std::{
    collections::HashMap,
    sync::{Arc, Mutex},
};

use async_trait::async_trait;
use cpex::cpex_core::{
    cmf::{CmfHook, ContentPart, Message, MessagePayload, Role},
    context::PluginContext,
    error::{PluginError, PluginViolation},
    factory::{PluginFactory, PluginInstance},
    hooks::{Extensions, HookHandler, PluginResult, TypedHandlerAdapter, types::cmf_hook_names},
    plugin::{Plugin, PluginConfig},
};
use rmcp::model::{CallToolResult, ContentBlock, ProgressNotificationParam};
use serde_json::{Value, json};

use super::tool::text;

pub(crate) const PRE_DENY_ERROR_CODE: i32 = -32001;
pub(crate) const POST_DENY_ERROR_CODE: i32 = -32002;
const MISSING_CONTEXT_ERROR_CODE: i32 = -32003;
pub(crate) const REWRITTEN_SUM_A: i64 = 10;
pub(crate) const REWRITTEN_SUM_B: i64 = 20;

#[derive(Default)]
pub(crate) struct Observations {
    pub(crate) pre_calls: usize,
    pub(crate) post_calls: usize,
    pub(crate) shutdown_calls: usize,
    pub(crate) pre_payload_name: Option<String>,
    pub(crate) pre_payload_namespace: Option<String>,
    pub(crate) pre_payload_role: Option<Role>,
    pub(crate) pre_tool_call_id: Option<String>,
    pub(crate) post_payload_name: Option<String>,
    pub(crate) post_tool_call_ids: Vec<String>,
    pub(crate) post_result_text: Option<String>,
}

#[derive(Clone, Copy, Default)]
pub(crate) enum PreBehavior {
    #[default]
    Allow,
    Rewrite,
    Deny,
    InvalidArgs,
    SetContext,
}

#[derive(Clone, Copy, Default)]
pub(crate) enum PostBehavior {
    #[default]
    Allow,
    Rewrite,
    RewriteRaw,
    RewriteStreamEvents,
    DenyStreamEvents,
    Deny,
    RequireContext,
}

pub(crate) struct TestPlugin {
    pub(crate) config: PluginConfig,
    pub(crate) observations: Arc<Mutex<Observations>>,
    pre_behavior: PreBehavior,
    post_behavior: PostBehavior,
}

impl TestPlugin {
    pub(crate) fn new(name: &str, hooks: Vec<&'static str>) -> Self {
        Self {
            config: PluginConfig {
                name: name.to_owned(),
                kind: "test".to_owned(),
                hooks: hooks.into_iter().map(str::to_owned).collect(),
                ..Default::default()
            },
            observations: Arc::new(Mutex::new(Observations::default())),
            pre_behavior: PreBehavior::Allow,
            post_behavior: PostBehavior::Allow,
        }
    }

    pub(crate) fn rewrite_from_config(config: PluginConfig) -> Self {
        Self {
            config,
            observations: Arc::new(Mutex::new(Observations::default())),
            pre_behavior: PreBehavior::Rewrite,
            post_behavior: PostBehavior::Allow,
        }
    }

    pub(crate) fn with_pre_rewrite(mut self) -> Self {
        self.pre_behavior = PreBehavior::Rewrite;
        self
    }

    pub(crate) fn with_post_rewrite(mut self) -> Self {
        self.post_behavior = PostBehavior::Rewrite;
        self
    }

    pub(crate) fn with_raw_post_rewrite(mut self) -> Self {
        self.post_behavior = PostBehavior::RewriteRaw;
        self
    }

    pub(crate) fn with_stream_event_rewrite(mut self) -> Self {
        self.post_behavior = PostBehavior::RewriteStreamEvents;
        self
    }

    pub(crate) fn with_stream_event_deny(mut self) -> Self {
        self.post_behavior = PostBehavior::DenyStreamEvents;
        self
    }

    pub(crate) fn with_pre_deny(mut self) -> Self {
        self.pre_behavior = PreBehavior::Deny;
        self
    }

    pub(crate) fn with_post_deny(mut self) -> Self {
        self.post_behavior = PostBehavior::Deny;
        self
    }

    pub(crate) fn with_invalid_pre_args(mut self) -> Self {
        self.pre_behavior = PreBehavior::InvalidArgs;
        self
    }

    pub(crate) fn with_context_roundtrip(mut self) -> Self {
        self.pre_behavior = PreBehavior::SetContext;
        self.post_behavior = PostBehavior::RequireContext;
        self
    }

    pub(crate) fn observations(&self) -> Arc<Mutex<Observations>> {
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

impl HookHandler<CmfHook> for TestPlugin {
    async fn handle(
        &self,
        payload: &MessagePayload,
        _extensions: &Extensions,
        ctx: &mut PluginContext,
    ) -> PluginResult<MessagePayload> {
        let is_post = payload.message.role == Role::Tool;
        let mut observations = self.observations.lock().expect("observations lock poisoned");
        if is_post {
            observations.post_calls += 1;
            if let Some(result) = payload.message.get_tool_results().first() {
                observations.post_payload_name = Some(result.tool_name.clone());
                observations.post_tool_call_ids.push(result.tool_call_id.clone());
            }
            observations.post_result_text = Some(cmf_result_text(payload));
        } else {
            observations.pre_calls += 1;
            if let Some(call) = payload.message.get_tool_calls().first() {
                observations.pre_payload_name = Some(call.name.clone());
                observations.pre_payload_namespace.clone_from(&call.namespace);
                observations.pre_payload_role = Some(payload.message.role);
                observations.pre_tool_call_id = Some(call.tool_call_id.clone());
            }
        }
        drop(observations);

        if is_post {
            match self.post_behavior {
                PostBehavior::Allow => PluginResult::allow(),
                PostBehavior::Rewrite => {
                    let mut modified = payload.clone();
                    let result_text = cmf_result_text(payload);
                    if let Some(ContentPart::ToolResult { content }) =
                        modified.message.content.iter_mut().find(|part| matches!(part, ContentPart::ToolResult { .. }))
                    {
                        if !is_tool_result_content(&content.content) {
                            return PluginResult::allow();
                        }
                        content.content = serde_json::to_value(CallToolResult::success(vec![ContentBlock::text(
                            format!("post:{result_text}"),
                        )]))
                        .expect("tool result serializes");
                    }
                    PluginResult::modify_payload(modified)
                },
                PostBehavior::RewriteRaw => {
                    let mut modified = payload.clone();
                    if let Some(ContentPart::ToolResult { content }) =
                        modified.message.content.iter_mut().find(|part| matches!(part, ContentPart::ToolResult { .. }))
                    {
                        if !is_tool_result_content(&content.content) {
                            return PluginResult::allow();
                        }
                        content.content = json!("raw-post");
                    }
                    PluginResult::modify_payload(modified)
                },
                PostBehavior::RewriteStreamEvents => {
                    let mut modified = payload.clone();
                    if let Some(ContentPart::ToolResult { content }) =
                        modified.message.content.iter_mut().find(|part| matches!(part, ContentPart::ToolResult { .. }))
                        && let Ok(mut progress) =
                            serde_json::from_value::<ProgressNotificationParam>(content.content.clone())
                    {
                        progress.message = progress.message.map(|message| format!("plugin:{message}"));
                        content.content = serde_json::to_value(progress).expect("progress serializes");
                        return PluginResult::modify_payload(modified);
                    }
                    PluginResult::allow()
                },
                PostBehavior::DenyStreamEvents => {
                    let is_stream_event = payload
                        .message
                        .get_tool_results()
                        .first()
                        .is_some_and(|result| !is_tool_result_content(&result.content));
                    if is_stream_event {
                        PluginResult::deny(PluginViolation::new("stream_denied", "stream denied"))
                    } else {
                        PluginResult::allow()
                    }
                },
                PostBehavior::Deny => PluginResult::deny(
                    PluginViolation::new("post_denied", "post denied")
                        .with_proto_error_code(i64::from(POST_DENY_ERROR_CODE)),
                ),
                PostBehavior::RequireContext => {
                    if ctx.get_global("pre_seen") == Some(&json!(true)) {
                        PluginResult::allow()
                    } else {
                        PluginResult::deny(
                            PluginViolation::new("missing_context", "pre context missing")
                                .with_proto_error_code(i64::from(MISSING_CONTEXT_ERROR_CODE)),
                        )
                    }
                },
            }
        } else {
            match self.pre_behavior {
                PreBehavior::Allow => PluginResult::allow(),
                PreBehavior::Rewrite => {
                    let mut modified = payload.clone();
                    if let Some(ContentPart::ToolCall { content }) =
                        modified.message.content.iter_mut().find(|part| matches!(part, ContentPart::ToolCall { .. }))
                    {
                        "echo".clone_into(&mut content.name);
                        content.arguments = HashMap::from([
                            ("a".to_owned(), json!(REWRITTEN_SUM_A)),
                            ("b".to_owned(), json!(REWRITTEN_SUM_B)),
                        ]);
                    }
                    PluginResult::modify_payload(modified)
                },
                PreBehavior::Deny => PluginResult::deny(
                    PluginViolation::new("pre_denied", "pre denied")
                        .with_proto_error_code(i64::from(PRE_DENY_ERROR_CODE)),
                ),
                PreBehavior::InvalidArgs => {
                    PluginResult::modify_payload(MessagePayload { message: Message::text(Role::User, "invalid") })
                },
                PreBehavior::SetContext => {
                    ctx.set_global("pre_seen", json!(true));
                    PluginResult::allow()
                },
            }
        }
    }
}

/// Progress notifications run through the same post hook as tool results;
/// result-rewriting behaviors must leave them untouched.
fn is_tool_result_content(content: &Value) -> bool {
    serde_json::from_value::<CallToolResult>(content.clone()).is_ok()
}

fn cmf_result_text(payload: &MessagePayload) -> String {
    payload
        .message
        .get_tool_results()
        .first()
        .and_then(|result| serde_json::from_value::<CallToolResult>(result.content.clone()).ok())
        .map_or_else(|| payload.message.get_text_content(), |result| text(&result))
}

pub(crate) const PROMPT_PRE_DENY_ERROR_CODE: i32 = -32011;
pub(crate) const PROMPT_POST_DENY_ERROR_CODE: i32 = -32012;
pub(crate) const REWRITTEN_PROMPT_TOPIC: &str = "rewritten-topic";

#[derive(Default)]
pub(crate) struct PromptObservations {
    pub(crate) pre_calls: usize,
    pub(crate) post_calls: usize,
    pub(crate) pre_name: Option<String>,
    pub(crate) pre_server_id: Option<String>,
    pub(crate) pre_request_id: Option<String>,
    pub(crate) post_request_id: Option<String>,
    pub(crate) post_texts: Vec<String>,
    pub(crate) post_prompt_names: Vec<String>,
}

#[derive(Clone, Copy, Default)]
pub(crate) enum PromptBehavior {
    #[default]
    Allow,
    RewriteArguments,
    DenyPre,
    RewriteText,
    DenyPost,
    /// Renames every listed prompt. `prompts/list` exposure is read-only, so the client must still
    /// receive the original names.
    RewriteListNames,
}

/// Prompt hooks carry `PromptRequest` / `PromptResult` content parts rather than the tool parts
/// [`TestPlugin`] handles. Pre and post are told apart by the CMF role — `User` on the request
/// side, `Assistant` on the response side — which holds for both `prompts/get` and `prompts/list`.
/// Content part alone does not work: a `prompts/list` post payload is also made of
/// `PromptRequest` parts.
pub(crate) struct PromptTestPlugin {
    pub(crate) config: PluginConfig,
    pub(crate) observations: Arc<Mutex<PromptObservations>>,
    behavior: PromptBehavior,
}

impl PromptTestPlugin {
    pub(crate) fn new(name: &str, hooks: Vec<&'static str>, behavior: PromptBehavior) -> Self {
        Self {
            config: PluginConfig {
                name: name.to_owned(),
                kind: "prompt-test".to_owned(),
                hooks: hooks.into_iter().map(str::to_owned).collect(),
                ..Default::default()
            },
            observations: Arc::new(Mutex::new(PromptObservations::default())),
            behavior,
        }
    }

    pub(crate) fn observations(&self) -> Arc<Mutex<PromptObservations>> {
        Arc::clone(&self.observations)
    }
}

#[async_trait]
impl Plugin for PromptTestPlugin {
    fn config(&self) -> &PluginConfig {
        &self.config
    }
}

impl HookHandler<CmfHook> for PromptTestPlugin {
    async fn handle(
        &self,
        payload: &MessagePayload,
        _extensions: &Extensions,
        _ctx: &mut PluginContext,
    ) -> PluginResult<MessagePayload> {
        let is_post = payload.message.role == Role::Assistant;
        let mut observations = self.observations.lock().expect("observations lock poisoned");
        if is_post {
            observations.post_calls += 1;
            if let Some(result) = payload.message.get_prompt_results().first() {
                observations.post_request_id = Some(result.prompt_request_id.clone());
                // `prompts/get` carries text inside the rendered messages; a prompt-reference
                // completion carries its values as top-level text parts.
                observations.post_texts = result
                    .messages
                    .iter()
                    .flat_map(|message| message.content.iter())
                    .chain(payload.message.content.iter())
                    .filter_map(|part| match part {
                        ContentPart::Text { text } => Some(text.clone()),
                        _ => None,
                    })
                    .collect();
            } else {
                // `prompts/list` post: the listing arrives as one `PromptRequest` per prompt, with
                // each description following as text.
                let listed = payload.message.get_prompt_requests();
                observations.post_request_id = listed.first().map(|request| request.prompt_request_id.clone());
                observations.post_prompt_names = listed.iter().map(|request| request.name.clone()).collect();
                observations.post_texts = payload
                    .message
                    .content
                    .iter()
                    .filter_map(|part| match part {
                        ContentPart::Text { text } => Some(text.clone()),
                        _ => None,
                    })
                    .collect();
            }
        } else {
            observations.pre_calls += 1;
            if let Some(request) = payload.message.get_prompt_requests().first() {
                observations.pre_name = Some(request.name.clone());
                observations.pre_server_id.clone_from(&request.server_id);
                observations.pre_request_id = Some(request.prompt_request_id.clone());
            }
        }
        drop(observations);

        match (is_post, self.behavior) {
            (false, PromptBehavior::RewriteArguments) => {
                let mut modified = payload.clone();
                if let Some(ContentPart::PromptRequest { content }) =
                    modified.message.content.iter_mut().find(|part| matches!(part, ContentPart::PromptRequest { .. }))
                {
                    content.arguments = HashMap::from([("topic".to_owned(), json!(REWRITTEN_PROMPT_TOPIC))]);
                }
                PluginResult::modify_payload(modified)
            },
            (false, PromptBehavior::DenyPre) => PluginResult::deny(
                PluginViolation::new("prompt_pre_denied", "prompt pre denied")
                    .with_proto_error_code(i64::from(PROMPT_PRE_DENY_ERROR_CODE)),
            ),
            (true, PromptBehavior::RewriteText) => {
                let mut modified = payload.clone();
                // `prompts/get` nests text inside the rendered messages; a prompt-reference
                // completion carries its values as top-level text parts.
                for part in &mut modified.message.content {
                    match part {
                        ContentPart::PromptResult { content } => {
                            for message in &mut content.messages {
                                for part in &mut message.content {
                                    if let ContentPart::Text { text } = part {
                                        *text = format!("redacted:{text}");
                                    }
                                }
                            }
                        },
                        ContentPart::Text { text } => *text = format!("redacted:{text}"),
                        _ => {},
                    }
                }
                PluginResult::modify_payload(modified)
            },
            (true, PromptBehavior::RewriteListNames) => {
                let mut modified = payload.clone();
                for part in &mut modified.message.content {
                    if let ContentPart::PromptRequest { content } = part {
                        content.name = format!("mutated-{}", content.name);
                    }
                }
                PluginResult::modify_payload(modified)
            },
            (true, PromptBehavior::DenyPost) => PluginResult::deny(
                PluginViolation::new("prompt_post_denied", "prompt post denied")
                    .with_proto_error_code(i64::from(PROMPT_POST_DENY_ERROR_CODE)),
            ),
            _ => PluginResult::allow(),
        }
    }
}

pub(crate) struct PromptTestPluginFactory {
    observations: Arc<Mutex<PromptObservations>>,
    behavior: PromptBehavior,
}

impl PromptTestPluginFactory {
    pub(crate) fn from_plugin(plugin: &PromptTestPlugin) -> Self {
        Self { observations: Arc::clone(&plugin.observations), behavior: plugin.behavior }
    }
}

impl PluginFactory for PromptTestPluginFactory {
    fn create(&self, config: &PluginConfig) -> Result<PluginInstance, Box<PluginError>> {
        let plugin = Arc::new(PromptTestPlugin {
            config: config.clone(),
            observations: Arc::clone(&self.observations),
            behavior: self.behavior,
        });
        let handlers = config
            .hooks
            .iter()
            .filter_map(|hook| {
                let hook = match hook.as_str() {
                    cmf_hook_names::PROMPT_PRE_FETCH => cmf_hook_names::PROMPT_PRE_FETCH,
                    cmf_hook_names::PROMPT_POST_FETCH => cmf_hook_names::PROMPT_POST_FETCH,
                    _ => return None,
                };
                Some((
                    hook,
                    Arc::new(TypedHandlerAdapter::<CmfHook, _>::new(Arc::clone(&plugin)))
                        as Arc<dyn cpex::cpex_core::registry::AnyHookHandler>,
                ))
            })
            .collect();
        let plugin: Arc<dyn Plugin> = plugin;
        Ok(PluginInstance { plugin, handlers })
    }
}

pub(crate) const RESOURCE_PRE_DENY_ERROR_CODE: i32 = -32021;
pub(crate) const RESOURCE_POST_DENY_ERROR_CODE: i32 = -32022;
pub(crate) const REWRITTEN_RESOURCE_URI: &str = "rewritten://resource";

#[derive(Default)]
pub(crate) struct ResourceObservations {
    pub(crate) pre_calls: usize,
    pub(crate) post_calls: usize,
    pub(crate) pre_uri: Option<String>,
    pub(crate) pre_backend: Option<String>,
    pub(crate) pre_request_id: Option<String>,
    pub(crate) post_request_id: Option<String>,
    pub(crate) post_uris: Vec<String>,
    pub(crate) post_names: Vec<String>,
    pub(crate) post_texts: Vec<String>,
    pub(crate) post_contents: Vec<String>,
}

#[derive(Clone, Copy, Default)]
pub(crate) enum ResourceBehavior {
    #[default]
    Allow,
    DenyPre,
    DenyPost,
    /// Rewrites the target URI of a subscribe/unsubscribe request.
    RewriteUri,
    /// Rewrites completion values, which arrive as top-level text parts.
    RewriteText,
    /// Rewrites `resources/read` text contents.
    RewriteContent,
    /// Rewrites every listed template URI. `resources/templates/list` exposure is read-only, so the
    /// client must still receive the original URI templates.
    RewriteListUris,
}

/// Resource hooks carry `ResourceRef` content parts. Pre and post are told apart by the CMF role,
/// the same convention the prompt hooks use.
pub(crate) struct ResourceTestPlugin {
    pub(crate) config: PluginConfig,
    pub(crate) observations: Arc<Mutex<ResourceObservations>>,
    behavior: ResourceBehavior,
}

impl ResourceTestPlugin {
    pub(crate) fn new(name: &str, hooks: Vec<&'static str>, behavior: ResourceBehavior) -> Self {
        Self {
            config: PluginConfig {
                name: name.to_owned(),
                kind: "resource-test".to_owned(),
                hooks: hooks.into_iter().map(str::to_owned).collect(),
                ..Default::default()
            },
            observations: Arc::new(Mutex::new(ResourceObservations::default())),
            behavior,
        }
    }

    pub(crate) fn observations(&self) -> Arc<Mutex<ResourceObservations>> {
        Arc::clone(&self.observations)
    }
}

#[async_trait]
impl Plugin for ResourceTestPlugin {
    fn config(&self) -> &PluginConfig {
        &self.config
    }
}

impl HookHandler<CmfHook> for ResourceTestPlugin {
    async fn handle(
        &self,
        payload: &MessagePayload,
        _extensions: &Extensions,
        _ctx: &mut PluginContext,
    ) -> PluginResult<MessagePayload> {
        let is_post = payload.message.role == Role::Assistant;
        let references = payload.message.get_resource_refs();
        let mut observations = self.observations.lock().expect("observations lock poisoned");
        if is_post {
            observations.post_calls += 1;
            let resources = payload.message.get_resources();
            observations.post_request_id = references
                .first()
                .map(|item| item.resource_request_id.clone())
                .or_else(|| resources.first().map(|item| item.resource_request_id.clone()));
            observations.post_uris = references
                .iter()
                .map(|item| item.uri.clone())
                .chain(resources.iter().map(|item| item.uri.clone()))
                .collect();
            observations.post_names = references.iter().filter_map(|item| item.name.clone()).collect();
            observations.post_texts = payload
                .message
                .content
                .iter()
                .filter_map(|part| match part {
                    ContentPart::Text { text } => Some(text.clone()),
                    _ => None,
                })
                .collect();
            observations.post_contents = resources.iter().filter_map(|item| item.content.clone()).collect();
        } else {
            observations.pre_calls += 1;
            // Subscribe/unsubscribe payloads carry a `Resource`; listings carry `ResourceRef`.
            let resources = payload.message.get_resources();
            if let Some(resource) = resources.first() {
                observations.pre_uri = Some(resource.uri.clone());
                observations.pre_request_id = Some(resource.resource_request_id.clone());
                observations.pre_backend =
                    resource.annotations.get("backend").and_then(Value::as_str).map(str::to_owned);
            } else if let Some(reference) = references.first() {
                observations.pre_uri = Some(reference.uri.clone());
                observations.pre_request_id = Some(reference.resource_request_id.clone());
            }
        }
        drop(observations);

        match (is_post, self.behavior) {
            (false, ResourceBehavior::DenyPre) => PluginResult::deny(
                PluginViolation::new("resource_pre_denied", "resource pre denied")
                    .with_proto_error_code(i64::from(RESOURCE_PRE_DENY_ERROR_CODE)),
            ),
            (true, ResourceBehavior::DenyPost) => PluginResult::deny(
                PluginViolation::new("resource_post_denied", "resource post denied")
                    .with_proto_error_code(i64::from(RESOURCE_POST_DENY_ERROR_CODE)),
            ),
            (false, ResourceBehavior::RewriteUri) => {
                let mut modified = payload.clone();
                for part in &mut modified.message.content {
                    if let ContentPart::Resource { content } = part {
                        REWRITTEN_RESOURCE_URI.clone_into(&mut content.uri);
                    }
                }
                PluginResult::modify_payload(modified)
            },
            (true, ResourceBehavior::RewriteContent) => {
                let mut modified = payload.clone();
                for part in &mut modified.message.content {
                    if let ContentPart::Resource { content } = part
                        && let Some(text) = &content.content
                    {
                        content.content = Some(format!("redacted:{text}"));
                    }
                }
                PluginResult::modify_payload(modified)
            },
            (true, ResourceBehavior::RewriteText) => {
                let mut modified = payload.clone();
                for part in &mut modified.message.content {
                    if let ContentPart::Text { text } = part {
                        *text = format!("redacted:{text}");
                    }
                }
                PluginResult::modify_payload(modified)
            },
            (true, ResourceBehavior::RewriteListUris) => {
                let mut modified = payload.clone();
                for part in &mut modified.message.content {
                    if let ContentPart::ResourceRef { content } = part {
                        content.uri = format!("mutated://{}", content.uri);
                    }
                }
                PluginResult::modify_payload(modified)
            },
            _ => PluginResult::allow(),
        }
    }
}

pub(crate) struct ResourceTestPluginFactory {
    observations: Arc<Mutex<ResourceObservations>>,
    behavior: ResourceBehavior,
}

impl ResourceTestPluginFactory {
    pub(crate) fn from_plugin(plugin: &ResourceTestPlugin) -> Self {
        Self { observations: Arc::clone(&plugin.observations), behavior: plugin.behavior }
    }
}

impl PluginFactory for ResourceTestPluginFactory {
    fn create(&self, config: &PluginConfig) -> Result<PluginInstance, Box<PluginError>> {
        let plugin = Arc::new(ResourceTestPlugin {
            config: config.clone(),
            observations: Arc::clone(&self.observations),
            behavior: self.behavior,
        });
        let handlers = config
            .hooks
            .iter()
            .filter_map(|hook| {
                let hook = match hook.as_str() {
                    cmf_hook_names::RESOURCE_PRE_FETCH => cmf_hook_names::RESOURCE_PRE_FETCH,
                    cmf_hook_names::RESOURCE_POST_FETCH => cmf_hook_names::RESOURCE_POST_FETCH,
                    _ => return None,
                };
                Some((
                    hook,
                    Arc::new(TypedHandlerAdapter::<CmfHook, _>::new(Arc::clone(&plugin)))
                        as Arc<dyn cpex::cpex_core::registry::AnyHookHandler>,
                ))
            })
            .collect();
        let plugin: Arc<dyn Plugin> = plugin;
        Ok(PluginInstance { plugin, handlers })
    }
}

pub(crate) struct TestPluginFactory {
    pub(crate) observations: Arc<Mutex<Observations>>,
    pub(crate) pre_behavior: PreBehavior,
    pub(crate) post_behavior: PostBehavior,
}

impl TestPluginFactory {
    pub(crate) fn from_plugin(plugin: &TestPlugin) -> Self {
        Self {
            observations: Arc::clone(&plugin.observations),
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
            pre_behavior: self.pre_behavior,
            post_behavior: self.post_behavior,
        });
        let mut handlers = Vec::new();
        if config.hooks.iter().any(|hook| hook == cmf_hook_names::TOOL_PRE_INVOKE) {
            handlers.push((
                cmf_hook_names::TOOL_PRE_INVOKE,
                Arc::new(TypedHandlerAdapter::<CmfHook, _>::new(Arc::clone(&plugin)))
                    as Arc<dyn cpex::cpex_core::registry::AnyHookHandler>,
            ));
        }
        if config.hooks.iter().any(|hook| hook == cmf_hook_names::TOOL_POST_INVOKE) {
            handlers.push((
                cmf_hook_names::TOOL_POST_INVOKE,
                Arc::new(TypedHandlerAdapter::<CmfHook, _>::new(Arc::clone(&plugin)))
                    as Arc<dyn cpex::cpex_core::registry::AnyHookHandler>,
            ));
        }
        Ok(PluginInstance { plugin: Arc::<TestPlugin>::clone(&plugin), handlers })
    }
}
