use std::{
    collections::HashMap,
    sync::{Arc, Mutex},
};

use async_trait::async_trait;
use cpex::cpex_core::{
    cmf::{CmfHook, ContentPart, Message, MessagePayload, PromptResult as CmfPromptResult, Role},
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
    fn handle(
        &self,
        payload: &MessagePayload,
        _extensions: &Extensions,
        ctx: &mut PluginContext,
    ) -> impl std::future::Future<Output = PluginResult<MessagePayload>> {
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

        std::future::ready(if is_post {
            match self.post_behavior {
                PostBehavior::Allow => PluginResult::allow(),
                PostBehavior::Rewrite => {
                    let mut modified = payload.clone();
                    let result_text = cmf_result_text(payload);
                    if let Some(ContentPart::ToolResult { content }) =
                        modified.message.content.iter_mut().find(|part| matches!(part, ContentPart::ToolResult { .. }))
                    {
                        if !is_tool_result_content(&content.content) {
                            return std::future::ready(PluginResult::allow());
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
                            return std::future::ready(PluginResult::allow());
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
                        return std::future::ready(PluginResult::modify_payload(modified));
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
        })
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

pub(crate) const REWRITTEN_PROMPT_TOPIC: &str = "rewritten-topic";
pub(crate) const REWRITTEN_PROMPT_TEXT: &str = "review of [REDACTED]";
pub(crate) const REWRITTEN_PROMPT_RESOURCE: &str = "config with [REDACTED]";
pub(crate) const PROMPT_POST_DENY_ERROR_CODE: i32 = -32004;
pub(crate) const PROMPT_ERROR_MESSAGE: &str = "prompt blocked by policy";

fn prompt_result_mut(payload: &mut MessagePayload) -> Option<&mut CmfPromptResult> {
    payload.message.content.iter_mut().find_map(|part| match part {
        ContentPart::PromptResult { content } => Some(content),
        _ => None,
    })
}

#[derive(Clone, Copy, Default)]
pub(crate) enum PromptBehavior {
    #[default]
    Rewrite,
    DropText,
    ContextRoundtrip,
    Deny,
    MarkError,
}

pub(crate) struct PromptTestPlugin {
    pub(crate) config: PluginConfig,
    pub(crate) observations: Arc<Mutex<PromptObservations>>,
    pub(crate) behavior: PromptBehavior,
    pub(crate) events: Option<Arc<Mutex<Vec<&'static str>>>>,
}

#[derive(Default)]
pub(crate) struct PromptObservations {
    pub(crate) pre_calls: usize,
    pub(crate) pre_name: Option<String>,
    pub(crate) pre_server_id: Option<String>,
    pub(crate) post_calls: usize,
    pub(crate) post_prompt_name: Option<String>,
}

impl PromptTestPlugin {
    pub(crate) fn new(name: &str, hooks: Vec<&'static str>) -> Self {
        Self {
            config: PluginConfig {
                name: name.to_owned(),
                kind: "prompt-test".to_owned(),
                hooks: hooks.into_iter().map(str::to_owned).collect(),
                ..Default::default()
            },
            observations: Arc::new(Mutex::new(PromptObservations::default())),
            behavior: PromptBehavior::default(),
            events: None,
        }
    }

    pub(crate) fn with_events(mut self, events: Arc<Mutex<Vec<&'static str>>>) -> Self {
        self.events = Some(events);
        self
    }

    pub(crate) fn rebuild(&self, config: PluginConfig) -> Self {
        Self {
            config,
            observations: Arc::clone(&self.observations),
            behavior: self.behavior,
            events: self.events.clone(),
        }
    }

    fn record(&self, event: &'static str) {
        if let Some(events) = &self.events {
            events.lock().expect("events lock poisoned").push(event);
        }
    }

    pub(crate) fn with_behavior(mut self, behavior: PromptBehavior) -> Self {
        self.behavior = behavior;
        self
    }

    pub(crate) fn observations(&self) -> Arc<Mutex<PromptObservations>> {
        Arc::clone(&self.observations)
    }

    fn handle_pre(&self, payload: &MessagePayload, ctx: &mut PluginContext) -> PluginResult<MessagePayload> {
        self.record("pre");
        let mut observations = self.observations.lock().expect("observations lock poisoned");
        observations.pre_calls += 1;
        if let Some(request) = payload.message.get_prompt_requests().first() {
            observations.pre_name = Some(request.name.clone());
            observations.pre_server_id.clone_from(&request.server_id);
        }
        drop(observations);

        match self.behavior {
            PromptBehavior::ContextRoundtrip => {
                ctx.set_global("prompt_pre_seen", json!(true));
                PluginResult::allow()
            },
            PromptBehavior::Rewrite | PromptBehavior::DropText | PromptBehavior::Deny | PromptBehavior::MarkError => {
                let mut modified = payload.clone();
                if let Some(ContentPart::PromptRequest { content }) =
                    modified.message.content.iter_mut().find(|part| matches!(part, ContentPart::PromptRequest { .. }))
                {
                    content.arguments = HashMap::from([("topic".to_owned(), json!(REWRITTEN_PROMPT_TOPIC))]);
                }
                PluginResult::modify_payload(modified)
            },
        }
    }

    fn handle_post(&self, payload: &MessagePayload, ctx: &mut PluginContext) -> PluginResult<MessagePayload> {
        self.record("post");
        let mut observations = self.observations.lock().expect("observations lock poisoned");
        observations.post_calls += 1;
        if let Some(result) = payload.message.get_prompt_results().first() {
            observations.post_prompt_name = Some(result.prompt_name.clone());
        }
        drop(observations);

        match self.behavior {
            PromptBehavior::Rewrite => {
                let mut modified = payload.clone();
                if let Some(result) = prompt_result_mut(&mut modified) {
                    for message in &mut result.messages {
                        for part in &mut message.content {
                            match part {
                                ContentPart::Text { text } => REWRITTEN_PROMPT_TEXT.clone_into(text),
                                ContentPart::Resource { content } => {
                                    content.content = Some(REWRITTEN_PROMPT_RESOURCE.to_owned());
                                },
                                _ => {},
                            }
                        }
                    }
                }
                PluginResult::modify_payload(modified)
            },
            PromptBehavior::DropText => {
                let mut modified = payload.clone();
                if let Some(result) = prompt_result_mut(&mut modified) {
                    result.messages.clear();
                }
                PluginResult::modify_payload(modified)
            },
            PromptBehavior::MarkError => {
                let mut modified = payload.clone();
                if let Some(result) = prompt_result_mut(&mut modified) {
                    result.is_error = true;
                    result.error_message = Some(PROMPT_ERROR_MESSAGE.to_owned());
                }
                PluginResult::modify_payload(modified)
            },
            PromptBehavior::Deny => PluginResult::deny(
                PluginViolation::new("prompt_post_denied", "prompt post denied")
                    .with_proto_error_code(i64::from(PROMPT_POST_DENY_ERROR_CODE)),
            ),
            PromptBehavior::ContextRoundtrip => {
                if ctx.get_global("prompt_pre_seen") == Some(&json!(true)) {
                    PluginResult::allow()
                } else {
                    PluginResult::deny(
                        PluginViolation::new("missing_prompt_context", "prompt pre context missing")
                            .with_proto_error_code(i64::from(MISSING_CONTEXT_ERROR_CODE)),
                    )
                }
            },
        }
    }
}

#[async_trait]
impl Plugin for PromptTestPlugin {
    fn config(&self) -> &PluginConfig {
        &self.config
    }
}

impl HookHandler<CmfHook> for PromptTestPlugin {
    fn handle(
        &self,
        payload: &MessagePayload,
        _extensions: &Extensions,
        ctx: &mut PluginContext,
    ) -> impl std::future::Future<Output = PluginResult<MessagePayload>> {
        std::future::ready(if payload.message.get_prompt_results().is_empty() {
            self.handle_pre(payload, ctx)
        } else {
            self.handle_post(payload, ctx)
        })
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
