use axum::{body::Body, middleware::Next, response::Response};
use contextforge_data_plane_apis::user_store::UserConfig;
use http::{StatusCode, header};
use tracing::debug;

use crate::layers::virtual_host_id::VirtualHostId;

const SERVER_NOT_FOUND_BODY: &str = r#"{"detail":"Server not found"}"#;

pub async fn virtual_host_config_layer(request: http::Request<axum::body::Body>, next: Next) -> Response {
    let virtual_host_id = request.extensions().get::<VirtualHostId>();
    let user_config = request.extensions().get::<UserConfig>();

    if let (Some(virtual_host_id), Some(user_config)) = (virtual_host_id, user_config)
        && !has_virtual_host(user_config, virtual_host_id)
    {
        let virtual_host_id = virtual_host_id.value();
        let virtual_hosts = user_config.virtual_hosts.len();
        debug!(
            "virtual_host_config_layer - virtual host config missing virtual_host_id = {virtual_host_id} virtual_hosts = {virtual_hosts}"
        );
        return server_not_found_response();
    }

    next.run(request).await
}

fn has_virtual_host(user_config: &UserConfig, virtual_host_id: &VirtualHostId) -> bool {
    user_config.virtual_hosts.contains_key(virtual_host_id.value())
}

fn server_not_found_response() -> Response {
    Response::builder()
        .status(StatusCode::NOT_FOUND)
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from(SERVER_NOT_FOUND_BODY))
        .expect("response should build")
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use axum::{Router, body::to_bytes, middleware, routing::get};
    use contextforge_data_plane_apis::user_store::VirtualHost;
    use http::Request;
    use tower::ServiceExt;

    use super::*;

    async fn handler() -> StatusCode {
        StatusCode::NO_CONTENT
    }

    fn user_config_with_virtual_host(virtual_host_id: &str) -> UserConfig {
        UserConfig {
            virtual_hosts: HashMap::from([(
                virtual_host_id.to_owned(),
                VirtualHost {
                    backends: HashMap::new(),
                    tools: HashMap::new(),
                    resources: HashMap::new(),
                    resource_templates: HashMap::new(),
                    prompts: HashMap::new(),
                },
            )]),
        }
    }

    fn request_with_extensions(user_config: UserConfig, virtual_host_id: &str) -> Request<Body> {
        let mut request = Request::builder().uri("/").body(Body::empty()).expect("request should build");
        request.extensions_mut().insert(user_config);
        request.extensions_mut().insert(VirtualHostId::new(virtual_host_id.to_owned()));
        request
    }

    #[tokio::test]
    async fn missing_virtual_host_returns_control_plane_style_404() {
        let app = Router::new().route("/", get(handler)).layer(middleware::from_fn(virtual_host_config_layer));
        let request = request_with_extensions(user_config_with_virtual_host("known"), "missing");

        let response = app.oneshot(request).await.expect("response should be returned");

        assert_eq!(response.status(), StatusCode::NOT_FOUND);

        let body = to_bytes(response.into_body(), SERVER_NOT_FOUND_BODY.len()).await.expect("body should be readable");
        assert_eq!(&body[..], SERVER_NOT_FOUND_BODY.as_bytes());
    }

    #[tokio::test]
    async fn existing_virtual_host_reaches_downstream_service() {
        let app = Router::new().route("/", get(handler)).layer(middleware::from_fn(virtual_host_config_layer));
        let request = request_with_extensions(user_config_with_virtual_host("known"), "known");

        let response = app.oneshot(request).await.expect("response should be returned");

        assert_eq!(response.status(), StatusCode::NO_CONTENT);
    }
}
