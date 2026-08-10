use std::{
    collections::HashMap,
    sync::{
        Arc, Mutex,
        atomic::{AtomicU64, Ordering},
    },
};

use rmcp::{
    model::{RequestId, SubscriptionFilter},
    service::SubscriptionSink,
};

use crate::layers::request_context::GatewayRequestContext;

#[derive(Clone, Default)]
pub(crate) struct DownstreamSubscriptionRegistry {
    inner: Arc<Mutex<HashMap<DownstreamSubscriptionKey, SubscriptionSink>>>,
    next_registration_id: Arc<AtomicU64>,
}

impl DownstreamSubscriptionRegistry {
    pub(crate) fn register(
        &self,
        context: &GatewayRequestContext,
        filter: &SubscriptionFilter,
        sink: &SubscriptionSink,
    ) -> DownstreamSubscriptionGuard {
        let registration_id = self.next_registration_id.fetch_add(1, Ordering::Relaxed);
        let keys = subscription_keys(context, filter, sink.id(), registration_id);
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
    registration_id: u64,
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
    registration_id: u64,
) -> Vec<DownstreamSubscriptionKey> {
    let mut keys = Vec::new();
    if filter.tools_list_changed == Some(true) {
        keys.push(subscription_key(
            context,
            subscription_id,
            registration_id,
            DownstreamSubscriptionNotification::ToolsListChanged,
        ));
    }
    if filter.prompts_list_changed == Some(true) {
        keys.push(subscription_key(
            context,
            subscription_id,
            registration_id,
            DownstreamSubscriptionNotification::PromptsListChanged,
        ));
    }
    if filter.resources_list_changed == Some(true) {
        keys.push(subscription_key(
            context,
            subscription_id,
            registration_id,
            DownstreamSubscriptionNotification::ResourcesListChanged,
        ));
    }
    if let Some(uris) = &filter.resource_subscriptions {
        keys.extend(uris.iter().map(|uri| {
            subscription_key(
                context,
                subscription_id,
                registration_id,
                DownstreamSubscriptionNotification::ResourceUpdated { uri: uri.clone() },
            )
        }));
    }
    keys
}

fn subscription_key(
    context: &GatewayRequestContext,
    subscription_id: &RequestId,
    registration_id: u64,
    notification: DownstreamSubscriptionNotification,
) -> DownstreamSubscriptionKey {
    DownstreamSubscriptionKey {
        principal: context.principal().to_owned(),
        virtual_host_id: context.virtual_host_id().to_owned(),
        subscription_id: subscription_id.clone(),
        registration_id,
        notification,
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use contextforge_data_plane_apis::user_store::{BackendMCPGateway, Transport, VirtualHost};
    use rmcp::model::RequestId;

    use super::*;

    #[test]
    fn keys_include_subscription_id_and_notification_kind() {
        let gateway_context = GatewayRequestContext::new(&test_claims(), &test_virtual_host_id(), &test_virtual_host());
        let filter = SubscriptionFilter::builder().tools_list_changed().resource_subscription("memo://known").build();

        let keys = subscription_keys(&gateway_context, &filter, &RequestId::Number(7), 9);

        assert_eq!(2, keys.len());
        assert!(keys.iter().all(|key| key.subscription_id == RequestId::Number(7)));
        assert!(keys.iter().all(|key| key.registration_id == 9));
    }

    #[test]
    fn registration_id_is_part_of_key_identity() {
        let gateway_context = GatewayRequestContext::new(&test_claims(), &test_virtual_host_id(), &test_virtual_host());
        let filter = SubscriptionFilter::builder().tools_list_changed().build();

        let first = subscription_keys(&gateway_context, &filter, &RequestId::Number(7), 0);
        let second = subscription_keys(&gateway_context, &filter, &RequestId::Number(7), 1);

        assert_ne!(first, second);
    }

    fn test_virtual_host() -> VirtualHost {
        VirtualHost {
            backends: HashMap::from([(
                "backend-one".to_owned(),
                BackendMCPGateway {
                    name: "backend-one".to_owned(),
                    url: "http://127.0.0.1:9999/mcp".parse().expect("valid URL"),
                    transport: Transport::default(),
                    passthrough_headers: Vec::new(),
                    add_headers: HashMap::new(),
                    remove_headers: Vec::new(),
                    allowed_tool_names: Vec::new(),
                    tool_name_aliases: HashMap::new(),
                    allowed_resource_names: Vec::new(),
                    allowed_prompt_names: Vec::new(),
                },
            )]),
        }
    }

    fn test_claims() -> crate::common::ContextForgeClaims {
        crate::common::ContextForgeClaims {
            sub: "test-principal".to_owned(),
            jti: "test-jti".to_owned(),
            token_use: None,
            iat: None,
            iss: "test-issuer".to_owned(),
            aud: "test-audience".to_owned(),
            exp: 1,
            teams: None,
            user: crate::common::User::builder()
                .email("test@example.com".to_owned())
                .full_name(None)
                .is_admin(false)
                .auth_provider("test".to_owned())
                .build(),
            scopes: None,
        }
    }

    fn test_virtual_host_id() -> crate::layers::virtual_host_id::VirtualHostId {
        crate::layers::virtual_host_id::VirtualHostId::new("test-vhost".to_owned())
    }
}
