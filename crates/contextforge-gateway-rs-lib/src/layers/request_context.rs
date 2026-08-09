use std::future::Future;

use contextforge_gateway_rs_apis::user_store::VirtualHost;

use crate::{common::ContextForgeClaims, layers::virtual_host_id::VirtualHostId};

tokio::task_local! {
    static GATEWAY_REQUEST_CONTEXT: GatewayRequestContext;
}

#[derive(Clone, Debug)]
pub(crate) struct GatewayRequestContext {
    principal: String,
    virtual_host_id: String,
    virtual_host: VirtualHost,
}

impl GatewayRequestContext {
    pub(crate) fn new(
        claims: &ContextForgeClaims,
        virtual_host_id: &VirtualHostId,
        virtual_host: &VirtualHost,
    ) -> Self {
        Self {
            principal: claims.sub.clone(),
            virtual_host_id: virtual_host_id.value().clone(),
            virtual_host: virtual_host.clone(),
        }
    }

    pub(crate) fn principal(&self) -> &str {
        &self.principal
    }

    pub(crate) fn virtual_host_id(&self) -> &str {
        &self.virtual_host_id
    }

    pub(crate) fn virtual_host(&self) -> &VirtualHost {
        &self.virtual_host
    }
}

pub(crate) async fn scope_gateway_request_context<F, R>(context: GatewayRequestContext, future: F) -> R
where
    F: Future<Output = R>,
{
    GATEWAY_REQUEST_CONTEXT.scope(context, future).await
}

pub(crate) fn current_gateway_request_context() -> Option<GatewayRequestContext> {
    GATEWAY_REQUEST_CONTEXT.try_with(Clone::clone).ok()
}
