use axum::{body::Body, middleware::Next, response::Response};
use http::{StatusCode, header};
use tracing::debug;

/// Virtual-server identifier extracted from the downstream MCP route.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct VirtualHostId {
    value: String,
}

impl VirtualHostId {
    /// Creates a virtual-server identifier from its canonical route value.
    pub fn new(value: impl Into<String>) -> Self {
        Self { value: value.into() }
    }

    /// Returns the canonical route value.
    pub fn as_str(&self) -> &str {
        &self.value
    }
}

pub async fn virtual_host_id_layer(mut request: http::Request<axum::body::Body>, next: Next) -> Response {
    let path = request.uri().path().to_owned();

    debug!("virtual_host_id_layer - extracting virtual host from path path = {path}");
    if let Some(virtual_host_id) = extract_virtual_host_id(&path) {
        request.extensions_mut().insert(virtual_host_id);
        next.run(request).await
    } else {
        debug!("virtual_host_id_layer - failed to extract virtual host id from request path path = {path}");
        Response::builder()
            .status(StatusCode::BAD_REQUEST)
            .header(header::CONTENT_TYPE, "text/plain")
            .body(Body::from("Problem occured retrieving the configuration"))
            .expect("Expecting this to work")
    }
}

fn extract_virtual_host_id(path: &str) -> Option<VirtualHostId> {
    if path.starts_with("/servers/") {
        path.ends_with("/mcp").then(|| {
            let l1 = "/servers/".len();
            let l2 = path.len() - "/mcp".len();
            let vh = &path[l1..l2];
            VirtualHostId::new(vh)
        })
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use crate::layers::virtual_host_id::{VirtualHostId, extract_virtual_host_id};

    #[test]
    fn extracts_only_virtual_host_from_server_mcp_routes() {
        assert_eq!(None, extract_virtual_host_id("/mcp/servers"));
        assert_eq!(None, extract_virtual_host_id("/servers"));
        assert_eq!(None, extract_virtual_host_id("/servers/12345_abcd-efgh/mcp/dkfjk"));
        assert_eq!(
            Some(VirtualHostId::new("12345_abcd-efgh")),
            extract_virtual_host_id("/servers/12345_abcd-efgh/mcp")
        );
        assert_eq!(None, extract_virtual_host_id("/12345_abcd-efgh/12345_abcd-efgh/mcp"));
        assert_eq!(None, extract_virtual_host_id("//12345_abcd-efgh/12345_abcd-efgh/mcp"));
    }
}
