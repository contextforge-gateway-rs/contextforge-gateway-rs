use serde_json::Value;

pub struct DefaultPrincipalExtractor {}

pub trait PrincipalExtractor {
    fn user_id<'a>(&self, claims: &'a serde_json::Map<String, Value>) -> Option<&'a str> {
        ["sub", "user_id", "UserId"].into_iter().find_map(|claim| claims.get(claim)).and_then(non_empty_string)
    }

    fn tenant_id<'a>(&self, claims: &'a serde_json::Map<String, Value>) -> Option<&'a str> {
        ["tenantId", "tenant_id"].into_iter().find_map(|claim| claims.get(claim).and_then(non_empty_string))
    }
}

impl PrincipalExtractor for DefaultPrincipalExtractor {}

fn non_empty_string(value: &Value) -> Option<&str> {
    value.as_str().filter(|value| !value.trim().is_empty())
}
