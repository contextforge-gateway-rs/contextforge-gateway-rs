use std::{
    collections::HashMap,
    sync::{Arc, Mutex},
};

use rmcp::{
    model::{RequestId, SubscriptionFilter},
    service::SubscriptionSink,
};

use crate::layers::request_context::GatewayRequestContext;

#[derive(Clone, Default)]
pub(crate) struct DownstreamSubscriptionRegistry {
    inner: Arc<Mutex<HashMap<DownstreamSubscriptionKey, SubscriptionSink>>>,
}

impl DownstreamSubscriptionRegistry {
    pub(crate) fn register(
        &self,
        context: &GatewayRequestContext,
        filter: &SubscriptionFilter,
        sink: &SubscriptionSink,
    ) -> DownstreamSubscriptionGuard {
        let keys = subscription_keys(context, filter, sink.id());
        let mut subscriptions = self.inner.lock().expect("downstream subscription registry lock poisoned");
        for key in &keys {
            subscriptions.insert(key.clone(), sink.clone());
        }
        DownstreamSubscriptionGuard { registry: self.clone(), keys }
    }

    fn remove_all(&self, keys: &[DownstreamSubscriptionKey]) {
        let mut subscriptions = self.inner.lock().expect("downstream subscription registry lock poisoned");
        for key in keys {
            subscriptions.remove(key);
        }
    }
}

pub(crate) struct DownstreamSubscriptionGuard {
    registry: DownstreamSubscriptionRegistry,
    keys: Vec<DownstreamSubscriptionKey>,
}

impl Drop for DownstreamSubscriptionGuard {
    fn drop(&mut self) {
        self.registry.remove_all(&self.keys);
    }
}

#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub(crate) struct DownstreamSubscriptionKey {
    principal: String,
    virtual_host_id: String,
    subscription_id: RequestId,
    notification: DownstreamSubscriptionNotification,
}

#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub(crate) enum DownstreamSubscriptionNotification {
    ToolsListChanged,
    PromptsListChanged,
    ResourcesListChanged,
    ResourceUpdated { uri: String },
}

pub(super) fn subscription_keys(
    context: &GatewayRequestContext,
    filter: &SubscriptionFilter,
    subscription_id: &RequestId,
) -> Vec<DownstreamSubscriptionKey> {
    let mut keys = Vec::new();
    if filter.tools_list_changed == Some(true) {
        keys.push(subscription_key(context, subscription_id, DownstreamSubscriptionNotification::ToolsListChanged));
    }
    if filter.prompts_list_changed == Some(true) {
        keys.push(subscription_key(context, subscription_id, DownstreamSubscriptionNotification::PromptsListChanged));
    }
    if filter.resources_list_changed == Some(true) {
        keys.push(subscription_key(context, subscription_id, DownstreamSubscriptionNotification::ResourcesListChanged));
    }
    if let Some(uris) = &filter.resource_subscriptions {
        keys.extend(uris.iter().map(|uri| {
            subscription_key(
                context,
                subscription_id,
                DownstreamSubscriptionNotification::ResourceUpdated { uri: uri.clone() },
            )
        }));
    }
    keys
}

fn subscription_key(
    context: &GatewayRequestContext,
    subscription_id: &RequestId,
    notification: DownstreamSubscriptionNotification,
) -> DownstreamSubscriptionKey {
    DownstreamSubscriptionKey {
        principal: context.principal().to_owned(),
        virtual_host_id: context.virtual_host_id().to_owned(),
        subscription_id: subscription_id.clone(),
        notification,
    }
}
