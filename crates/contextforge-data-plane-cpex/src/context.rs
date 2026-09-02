use std::{
    collections::{HashMap, HashSet},
    sync::Arc,
};

use cpex::cpex_core::extensions::{
    Extensions, HttpExtension, MCPExtension, MetaExtension, PromptMetadata, RequestExtension, ResourceMetadata,
    SecurityExtension, SubjectExtension, SubjectType, ToolMetadata,
};

#[derive(Debug)]
pub struct HookRequestMetadata {
    pub request_id: String,
    pub trace_id: Option<String>,
    pub span_id: Option<String>,
}

#[derive(Debug)]
pub struct HookSubject {
    pub id: String,
    pub teams: HashSet<String>,
    pub permissions: HashSet<String>,
}

#[derive(Debug)]
pub struct HookHttpRequest {
    pub method: String,
    pub path: String,
    pub authority: Option<String>,
    pub scheme: Option<String>,
    pub headers: HashMap<String, String>,
}

#[derive(Debug)]
pub enum HookTarget {
    Tool { name: String, backend: String },
    Resource { uri: String, backend: String },
    Prompt { name: String, backend: String },
}

impl HookTarget {
    fn into_parts(self) -> (&'static str, String, String, MCPExtension) {
        match self {
            Self::Tool { name, backend } => {
                let mcp = MCPExtension {
                    tool: Some(ToolMetadata {
                        name: name.clone(),
                        server_id: Some(backend.clone()),
                        namespace: Some(backend.clone()),
                        ..Default::default()
                    }),
                    ..Default::default()
                };
                ("tool", name, backend, mcp)
            },
            Self::Resource { uri, backend } => {
                let mcp = MCPExtension {
                    resource: Some(ResourceMetadata {
                        uri: uri.clone(),
                        server_id: Some(backend.clone()),
                        ..Default::default()
                    }),
                    ..Default::default()
                };
                ("resource", uri, backend, mcp)
            },
            Self::Prompt { name, backend } => {
                let mcp = MCPExtension {
                    prompt: Some(PromptMetadata {
                        name: name.clone(),
                        server_id: Some(backend.clone()),
                        ..Default::default()
                    }),
                    ..Default::default()
                };
                ("prompt", name, backend, mcp)
            },
        }
    }
}

pub struct HookOperation {
    pub mcp_method: String,
    pub virtual_host: String,
    pub downstream_target: String,
    pub target: HookTarget,
}

/// Trusted, host-built context for one canonically routed MCP operation.
pub struct McpHookContext {
    extensions: Extensions,
}

impl McpHookContext {
    pub fn new(
        request: HookRequestMetadata,
        subject: HookSubject,
        http: HookHttpRequest,
        operation: HookOperation,
    ) -> Self {
        let request = RequestExtension {
            request_id: Some(request.request_id),
            trace_id: request.trace_id,
            span_id: request.span_id,
            ..Default::default()
        };
        let security = SecurityExtension {
            subject: Some(SubjectExtension {
                id: Some(subject.id),
                subject_type: Some(SubjectType::User),
                teams: subject.teams,
                permissions: subject.permissions,
                ..Default::default()
            }),
            auth_method: Some("jwt".to_owned()),
            ..Default::default()
        };
        let http = HttpExtension {
            request_headers: http.headers,
            method: Some(http.method),
            path: Some(http.path),
            host: http.authority,
            scheme: http.scheme,
            ..Default::default()
        };
        let (entity_type, entity_name, backend, mcp) = operation.target.into_parts();
        let meta = MetaExtension {
            entity_type: Some(entity_type.to_owned()),
            entity_name: Some(entity_name),
            scope: Some(operation.virtual_host),
            properties: HashMap::from([
                ("mcp_method".to_owned(), operation.mcp_method),
                ("downstream_target".to_owned(), operation.downstream_target),
                ("backend".to_owned(), backend),
            ]),
            ..Default::default()
        };

        Self {
            extensions: Extensions {
                request: Some(Arc::new(request)),
                http: Some(Arc::new(http)),
                security: Some(Arc::new(security)),
                mcp: Some(Arc::new(mcp)),
                meta: Some(Arc::new(meta)),
                ..Default::default()
            },
        }
    }

    pub(crate) fn into_extensions(self) -> Extensions {
        self.extensions
    }
}

pub struct ScopedMcpHook<'a> {
    binding_revision: &'a str,
    plugin_names: &'a [String],
    context: McpHookContext,
}

impl<'a> ScopedMcpHook<'a> {
    pub fn new(binding_revision: &'a str, plugin_names: &'a [String], context: McpHookContext) -> Self {
        Self { binding_revision, plugin_names, context }
    }

    pub(crate) fn into_parts(self) -> (&'a str, &'a [String], McpHookContext) {
        (self.binding_revision, self.plugin_names, self.context)
    }
}
