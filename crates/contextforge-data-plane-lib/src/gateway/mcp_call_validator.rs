use contextforge_data_plane_apis::user_store::{UserConfig, VirtualHost};
use http::request::Parts;
use rmcp::{ErrorData, RoleServer, model::ErrorCode, service::RequestContext};
use tracing::debug;

use crate::{authorization::AuthorizationClaims, layers::virtual_host_id::VirtualHostId};

pub struct AuthorizedCallValidator<'a> {
    call_name: &'a str,
    ctx: &'a RequestContext<RoleServer>,
}

impl<'a> AuthorizedCallValidator<'a> {
    pub fn new(call_name: &'a str, ctx: &'a RequestContext<RoleServer>) -> Self {
        Self { call_name, ctx }
    }

    pub fn validate_stateless(self) -> Result<(&'a VirtualHost, &'a AuthorizationClaims), ErrorData> {
        let maybe_parts = self.ctx.extensions.get::<Parts>();
        let maybe_user_config = maybe_parts.and_then(|parts| parts.extensions.get::<UserConfig>());
        let maybe_claims = maybe_parts.and_then(|parts| parts.extensions.get::<AuthorizationClaims>());
        let maybe_virtual_host_id = maybe_parts.and_then(|parts| parts.extensions.get::<VirtualHostId>());
        let call_name = self.call_name;
        let has_user_config = maybe_user_config.is_some();
        let virtual_hosts = maybe_user_config.map_or(0, |user_config| user_config.virtual_hosts.len());
        let has_claims = maybe_claims.is_some();
        let virtual_host_id = maybe_virtual_host_id.map_or("<missing>", |id| id.value().as_str());
        debug!(
            "AuthorizedCallValidator::validate - mcp call validation call_name = {call_name} has_user_config = {has_user_config} virtual_hosts = {virtual_hosts} has_claims = {has_claims} virtual_host_id = {virtual_host_id}"
        );

        let Some(user_config) = maybe_user_config else {
            return Err(ErrorData {
                code: ErrorCode::INTERNAL_ERROR,
                message: "Routing problem... user config not found".into(),
                data: None,
            });
        };

        let Some(virtual_host_id) = maybe_virtual_host_id else {
            return Err(ErrorData {
                code: ErrorCode::INTERNAL_ERROR,
                message: "Routing problem... virtual host not known".into(),
                data: None,
            });
        };

        let Some(virtual_host) = user_config.virtual_hosts.get(virtual_host_id.value()) else {
            let call_name = self.call_name;
            let virtual_host_id = virtual_host_id.value();
            let virtual_hosts = user_config.virtual_hosts.len();
            debug!(
                "AuthorizedCallValidator::validate - mcp virtual host config missing call_name = {call_name} virtual_host_id = {virtual_host_id} virtual_hosts = {virtual_hosts}"
            );
            return Err(ErrorData {
                code: ErrorCode::RESOURCE_NOT_FOUND,
                message: "No configuration".into(),
                data: None,
            });
        };

        let Some(claims) = maybe_claims else {
            return Err(ErrorData {
                code: ErrorCode::INTERNAL_ERROR,
                message: "Routing problem... claims not found".into(),
                data: None,
            });
        };

        Ok((virtual_host, claims))
    }
}
