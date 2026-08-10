use std::sync::Arc;

use contextforge_data_plane_cpex::CpexRuntimeRegistry;
use cpex::cpex_core::config::CpexConfig;
use serde_json::json;

use super::{PromptTestPlugin, PromptTestPluginFactory, TestPlugin, TestPluginFactory};

pub(crate) async fn runtime_with_prompt_plugin(plugin: Arc<PromptTestPlugin>) -> Arc<CpexRuntimeRegistry> {
    let mut runtime = CpexRuntimeRegistry::default();
    runtime
        .register_factory("prompt-test", Box::new(PromptTestPluginFactory::from_plugin(&plugin)))
        .expect("prompt test factory registers");
    let config = serde_json::from_value(json!({
        "plugins": [{
            "name": plugin.config.name.clone(),
            "kind": plugin.config.kind.clone(),
            "hooks": plugin.config.hooks.clone(),
        }]
    }))
    .expect("prompt CPEX config parses");
    runtime.apply_config(Some(config)).await.expect("prompt runtime config applies");
    Arc::new(runtime)
}

pub(crate) async fn runtime_with_pre(plugin: Arc<TestPlugin>) -> Arc<CpexRuntimeRegistry> {
    runtime_with_plugins(&[plugin]).await
}

pub(crate) async fn runtime_with_post(plugin: Arc<TestPlugin>) -> Arc<CpexRuntimeRegistry> {
    runtime_with_plugins(&[plugin]).await
}

pub(crate) async fn runtime_with_pre_and_post(
    pre_plugin: Arc<TestPlugin>,
    post_plugin: Arc<TestPlugin>,
) -> Arc<CpexRuntimeRegistry> {
    runtime_with_plugins(&[pre_plugin, post_plugin]).await
}

pub(crate) async fn runtime_with_plugins(plugins: &[Arc<TestPlugin>]) -> Arc<CpexRuntimeRegistry> {
    let mut runtime = CpexRuntimeRegistry::default();
    for (index, plugin) in plugins.iter().enumerate() {
        runtime
            .register_factory(format!("test-{index}"), Box::new(TestPluginFactory::from_plugin(plugin)))
            .expect("test factory registers");
    }
    runtime.apply_config(Some(cpex_config(plugins))).await.expect("test runtime config applies");
    Arc::new(runtime)
}

fn cpex_config(plugins: &[Arc<TestPlugin>]) -> CpexConfig {
    serde_json::from_value(json!({
        "plugins": plugins.iter().enumerate().map(|(index, plugin)| {
            json!({
                "name": plugin.config.name.clone(),
                "kind": format!("test-{index}"),
                "hooks": plugin.config.hooks.clone(),
            })
        }).collect::<Vec<_>>()
    }))
    .expect("test CPEX config parses")
}
