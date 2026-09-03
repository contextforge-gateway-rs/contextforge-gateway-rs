use axum::{middleware::Next, response::Response};

use tracing::debug;

use crate::errors::bad_request;

#[derive(Clone, Debug, PartialEq, PartialOrd)]
pub struct VirtualHostId {
    value: String,
}

impl VirtualHostId {
    pub(crate) fn new(value: String) -> Self {
        Self { value }
    }

    pub fn value(&self) -> &String {
        &self.value
    }
}

pub async fn virtual_host_id_layer(mut request: http::Request<axum::body::Body>, next: Next) -> Response {
    let path = request.uri().path().to_owned();

    if let Some(virtual_host_id) = extract_virtual_host_id(&path) {
        debug!("virtual_host_id_layer - extracted virtual host from path path = {path}");
        request.extensions_mut().insert(virtual_host_id);
        next.run(request).await
    } else {
        debug!("virtual_host_id_layer - failed to extract virtual host id from request path path = {path}");
        bad_request("Problem occured retrieving the configuration")
    }
}

fn extract_virtual_host_id(path: &str) -> Option<VirtualHostId> {
    if path.starts_with("/servers/") {
        path.ends_with("/mcp").then(|| {
            let l1 = "/servers/".len();
            let l2 = path.len() - "/mcp".len();
            let vh = &path[l1..l2];
            VirtualHostId::new(vh.to_owned())
        })
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use crate::layers::virtual_host_id::{VirtualHostId, extract_virtual_host_id};

    #[test]
    fn test_virtual_host_extractor() {
        assert_eq!(None, extract_virtual_host_id("/mcp/servers"));
        assert_eq!(None, extract_virtual_host_id("/servers"));
        assert_eq!(None, extract_virtual_host_id("/servers/12345_abcd-efgh/mcp/dkfjk"));
        assert_eq!(
            Some(VirtualHostId { value: "12345_abcd-efgh".to_owned() }),
            extract_virtual_host_id("/servers/12345_abcd-efgh/mcp")
        );
        assert_eq!(None, extract_virtual_host_id("/12345_abcd-efgh/12345_abcd-efgh/mcp"));
        assert_eq!(None, extract_virtual_host_id("//12345_abcd-efgh/12345_abcd-efgh/mcp"));
    }
}
