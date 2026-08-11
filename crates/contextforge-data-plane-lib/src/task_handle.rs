//! Stateless routing handles for the MCP Tasks extension.
//!
//! Handles are encrypted and authenticated because their payload contains an
//! upstream task identifier that may be a bearer token. The authenticated
//! subject and virtual-host scope prevents a handle from being replayed through
//! a different caller or route, while the backend lookup ensures that removed
//! backends fail closed.

use std::{fmt, str::FromStr};

use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use contextforge_data_plane_apis::user_store::VirtualHost;
use ring::{
    aead::{AES_256_GCM, Aad, LessSafeKey, Nonce, UnboundKey},
    rand::{SecureRandom, SystemRandom},
};
use rmcp::{ErrorData, model::ErrorCode};
use secret_string::SecretString;
use serde::{Deserialize, Serialize};
use thiserror::Error;

const HANDLE_PREFIX: &str = "cfth1";
const HANDLE_FAMILY_PREFIX: &str = "cfth";
const KEY_LEN: usize = 32;
const NONCE_LEN: usize = 12;
const TAG_LEN: usize = 16;

/// A validated AES-256 key for task-handle protection.
///
/// The textual form is URL-safe base64 without padding and must decode to
/// exactly 32 bytes. Its [`Debug`] output is always redacted.
#[derive(Clone, PartialEq, Eq)]
pub struct TaskHandleKey(SecretString<String>);

impl TaskHandleKey {
    fn bytes(&self) -> Result<[u8; KEY_LEN], TaskHandleKeyError> {
        let bytes = URL_SAFE_NO_PAD.decode(self.0.value()).map_err(|_| TaskHandleKeyError)?;
        bytes.try_into().map_err(|_| TaskHandleKeyError)
    }
}

impl FromStr for TaskHandleKey {
    type Err = TaskHandleKeyError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        let key = Self(SecretString::new(value.to_owned()));
        key.bytes()?;
        Ok(key)
    }
}

impl fmt::Debug for TaskHandleKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("TaskHandleKey([REDACTED])")
    }
}

/// Returned when task-handle key material is not a URL-safe base64-encoded
/// 256-bit key.
#[derive(Debug, Error, PartialEq, Eq)]
#[error("task handle key must be URL-safe base64 without padding and decode to exactly 32 bytes")]
pub struct TaskHandleKeyError;

/// Authenticated request scope to which a task handle is bound.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TaskHandleScope<'a> {
    subject: &'a str,
    virtual_host_id: &'a str,
}

impl<'a> TaskHandleScope<'a> {
    /// Creates a scope from the authenticated subject and path virtual-host ID.
    pub fn new(subject: &'a str, virtual_host_id: &'a str) -> Self {
        Self { subject, virtual_host_id }
    }
}

/// The route recovered from a valid task handle.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TaskHandleRoute {
    backend_name: String,
    upstream_task_id: String,
}

impl TaskHandleRoute {
    /// Backend map key in the caller's current virtual-host configuration.
    pub fn backend_name(&self) -> &str {
        &self.backend_name
    }

    /// Original task identifier expected by the upstream backend.
    pub fn upstream_task_id(&self) -> &str {
        &self.upstream_task_id
    }
}

/// Errors produced while encoding or decoding task handles.
///
/// Decode errors intentionally have the same display text so a caller cannot
/// distinguish a malformed handle from a valid handle outside its scope.
#[derive(Debug, Error, PartialEq, Eq)]
pub enum TaskHandleError {
    #[error("failed to create task handle")]
    Encode,
    #[error("invalid task handle")]
    Invalid,
    #[error("invalid task handle")]
    UnsupportedVersion,
    #[error("invalid task handle")]
    WrongScope,
    #[error("invalid task handle")]
    UnavailableBackend,
}

impl From<TaskHandleError> for ErrorData {
    fn from(error: TaskHandleError) -> Self {
        match error {
            TaskHandleError::Encode => ErrorData::new(ErrorCode::INTERNAL_ERROR, "failed to create task handle", None),
            TaskHandleError::Invalid
            | TaskHandleError::UnsupportedVersion
            | TaskHandleError::WrongScope
            | TaskHandleError::UnavailableBackend => ErrorData::new(ErrorCode::INVALID_PARAMS, "invalid task ID", None),
        }
    }
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct TaskHandlePayload {
    subject: String,
    virtual_host_id: String,
    backend_name: String,
    upstream_task_id: String,
}

/// Encodes and decodes versioned task handles shared across dataplane replicas.
#[derive(Clone)]
pub struct TaskHandleCodec {
    key: LessSafeKey,
    random: SystemRandom,
}

impl TaskHandleCodec {
    /// Creates a codec from a validated shared key.
    pub fn new(key: &TaskHandleKey) -> Result<Self, TaskHandleKeyError> {
        let key = UnboundKey::new(&AES_256_GCM, &key.bytes()?).map_err(|_| TaskHandleKeyError)?;
        Ok(Self { key: LessSafeKey::new(key), random: SystemRandom::new() })
    }

    /// Creates an opaque handle for one upstream task.
    pub fn encode(
        &self,
        scope: TaskHandleScope<'_>,
        backend_name: &str,
        upstream_task_id: &str,
    ) -> Result<String, TaskHandleError> {
        let payload = TaskHandlePayload {
            subject: scope.subject.to_owned(),
            virtual_host_id: scope.virtual_host_id.to_owned(),
            backend_name: backend_name.to_owned(),
            upstream_task_id: upstream_task_id.to_owned(),
        };
        let mut ciphertext = serde_json::to_vec(&payload).map_err(|_| TaskHandleError::Encode)?;

        let mut nonce_bytes = [0_u8; NONCE_LEN];
        self.random.fill(&mut nonce_bytes).map_err(|_| TaskHandleError::Encode)?;
        let nonce = Nonce::assume_unique_for_key(nonce_bytes);
        self.key
            .seal_in_place_append_tag(nonce, Aad::from(HANDLE_PREFIX), &mut ciphertext)
            .map_err(|_| TaskHandleError::Encode)?;

        let mut protected = Vec::with_capacity(NONCE_LEN + ciphertext.len());
        protected.extend_from_slice(&nonce_bytes);
        protected.extend_from_slice(&ciphertext);
        Ok(format!("{HANDLE_PREFIX}.{}", URL_SAFE_NO_PAD.encode(protected)))
    }

    /// Decodes a handle and verifies that it belongs to the authenticated
    /// caller's current virtual host and references a currently configured
    /// backend.
    pub fn decode(
        &self,
        handle: &str,
        expected_scope: TaskHandleScope<'_>,
        virtual_host: &VirtualHost,
    ) -> Result<TaskHandleRoute, TaskHandleError> {
        let (prefix, protected) = handle.split_once('.').ok_or(TaskHandleError::Invalid)?;
        if prefix != HANDLE_PREFIX {
            return if is_other_version(prefix) {
                Err(TaskHandleError::UnsupportedVersion)
            } else {
                Err(TaskHandleError::Invalid)
            };
        }

        let protected = URL_SAFE_NO_PAD.decode(protected).map_err(|_| TaskHandleError::Invalid)?;
        if protected.len() < NONCE_LEN + TAG_LEN {
            return Err(TaskHandleError::Invalid);
        }
        let (nonce_bytes, ciphertext) = protected.split_at(NONCE_LEN);
        let nonce_bytes: [u8; NONCE_LEN] = nonce_bytes.try_into().map_err(|_| TaskHandleError::Invalid)?;
        let nonce = Nonce::assume_unique_for_key(nonce_bytes);
        let mut ciphertext = ciphertext.to_vec();
        let plaintext = self
            .key
            .open_in_place(nonce, Aad::from(HANDLE_PREFIX), &mut ciphertext)
            .map_err(|_| TaskHandleError::Invalid)?;
        let payload: TaskHandlePayload = serde_json::from_slice(plaintext).map_err(|_| TaskHandleError::Invalid)?;

        if payload.subject != expected_scope.subject || payload.virtual_host_id != expected_scope.virtual_host_id {
            return Err(TaskHandleError::WrongScope);
        }
        if !virtual_host.backends.contains_key(&payload.backend_name) {
            return Err(TaskHandleError::UnavailableBackend);
        }

        Ok(TaskHandleRoute { backend_name: payload.backend_name, upstream_task_id: payload.upstream_task_id })
    }
}

fn is_other_version(prefix: &str) -> bool {
    prefix
        .strip_prefix(HANDLE_FAMILY_PREFIX)
        .is_some_and(|version| !version.is_empty() && version.bytes().all(|byte| byte.is_ascii_digit()))
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use contextforge_data_plane_apis::user_store::BackendMCPGateway;
    use url::Url;

    use super::*;

    const KEY: &str = "AAECAwQFBgcICQoLDA0ODxAREhMUFRYXGBkaGxwdHh8"; // pragma: allowlist secret

    fn codec() -> TaskHandleCodec {
        TaskHandleCodec::new(&KEY.parse().expect("test key is valid")).expect("test key initializes AES-256-GCM")
    }

    fn backend(name: &str) -> BackendMCPGateway {
        BackendMCPGateway {
            name: name.to_owned(),
            url: Url::parse(&format!("https://{name}.example.com/mcp")).expect("test URL is valid"),
            passthrough_headers: Vec::new(),
            add_headers: HashMap::new(),
            remove_headers: Vec::new(),
            allowed_tool_names: Vec::new(),
            tool_name_aliases: HashMap::new(),
            allowed_resource_names: Vec::new(),
            allowed_prompt_names: Vec::new(),
        }
    }

    fn virtual_host(names: &[&str]) -> VirtualHost {
        VirtualHost { backends: names.iter().map(|name| ((*name).to_owned(), backend(name))).collect() }
    }

    fn scope<'a>(subject: &'a str, virtual_host_id: &'a str) -> TaskHandleScope<'a> {
        TaskHandleScope::new(subject, virtual_host_id)
    }

    #[test]
    fn arbitrary_upstream_task_ids_round_trip_without_loss() {
        let codec = codec();
        let virtual_host = virtual_host(&["backend-a"]);
        let task_ids = ["", "simple", "with/slashes?and=query", "nul\0byte", "emoji-🦀", "line\nbreak"];

        for task_id in task_ids {
            let handle = codec.encode(scope("caller", "host-a"), "backend-a", task_id).expect("handle encodes");
            let route = codec.decode(&handle, scope("caller", "host-a"), &virtual_host).expect("handle decodes");

            assert_eq!(route.backend_name(), "backend-a");
            assert_eq!(route.upstream_task_id(), task_id);
        }
    }

    #[test]
    fn identical_task_ids_from_different_backends_remain_isolated() {
        let codec = codec();
        let virtual_host = virtual_host(&["backend-a", "backend-b"]);
        let handle_a = codec.encode(scope("caller", "host-a"), "backend-a", "same-id").expect("handle A encodes");
        let handle_b = codec.encode(scope("caller", "host-a"), "backend-b", "same-id").expect("handle B encodes");

        let route_a = codec.decode(&handle_a, scope("caller", "host-a"), &virtual_host).expect("handle A decodes");
        let route_b = codec.decode(&handle_b, scope("caller", "host-a"), &virtual_host).expect("handle B decodes");

        assert_ne!(handle_a, handle_b);
        assert_eq!(route_a.backend_name(), "backend-a");
        assert_eq!(route_b.backend_name(), "backend-b");
    }

    #[test]
    fn handle_decodes_on_another_codec_with_the_same_key() {
        let first_replica = codec();
        let second_replica = codec();
        let virtual_host = virtual_host(&["backend-a"]);
        let handle = first_replica.encode(scope("caller", "host-a"), "backend-a", "task-42").expect("handle encodes");

        let route = second_replica.decode(&handle, scope("caller", "host-a"), &virtual_host).expect("handle decodes");

        assert_eq!(route.upstream_task_id(), "task-42");
    }

    #[test]
    fn malformed_and_tampered_handles_fail_closed() {
        let codec = codec();
        let virtual_host = virtual_host(&["backend-a"]);
        let handle = codec.encode(scope("caller", "host-a"), "backend-a", "task-42").expect("handle encodes");
        let mut tampered = handle.into_bytes();
        let last = tampered.last_mut().expect("handle is non-empty");
        *last = if *last == b'A' { b'B' } else { b'A' };
        let tampered = String::from_utf8(tampered).expect("tampered handle remains UTF-8");

        assert_eq!(
            codec.decode("not-a-handle", scope("caller", "host-a"), &virtual_host),
            Err(TaskHandleError::Invalid)
        );
        assert_eq!(codec.decode("cfth1.AA", scope("caller", "host-a"), &virtual_host), Err(TaskHandleError::Invalid));
        assert_eq!(codec.decode(&tampered, scope("caller", "host-a"), &virtual_host), Err(TaskHandleError::Invalid));
    }

    #[test]
    fn unsupported_handle_versions_are_rejected() {
        let codec = codec();
        let virtual_host = virtual_host(&["backend-a"]);

        assert_eq!(
            codec.decode("cfth2.AA", scope("caller", "host-a"), &virtual_host),
            Err(TaskHandleError::UnsupportedVersion)
        );
        assert_eq!(codec.decode("cfthx.AA", scope("caller", "host-a"), &virtual_host), Err(TaskHandleError::Invalid));
    }

    #[test]
    fn caller_and_virtual_host_scope_are_enforced() {
        let codec = codec();
        let virtual_host = virtual_host(&["backend-a"]);
        let handle = codec.encode(scope("caller-a", "host-a"), "backend-a", "task-42").expect("handle encodes");

        assert_eq!(codec.decode(&handle, scope("caller-b", "host-a"), &virtual_host), Err(TaskHandleError::WrongScope));
        assert_eq!(codec.decode(&handle, scope("caller-a", "host-b"), &virtual_host), Err(TaskHandleError::WrongScope));
    }

    #[test]
    fn removed_backends_are_rejected_without_exposing_the_backend_name() {
        let codec = codec();
        let original_virtual_host = virtual_host(&["backend-a"]);
        let current_virtual_host = virtual_host(&["backend-b"]);
        let handle = codec.encode(scope("caller", "host-a"), "backend-a", "task-42").expect("handle encodes");

        assert!(codec.decode(&handle, scope("caller", "host-a"), &original_virtual_host).is_ok());
        let error =
            codec.decode(&handle, scope("caller", "host-a"), &current_virtual_host).expect_err("backend was removed");

        assert_eq!(error, TaskHandleError::UnavailableBackend);
        assert_eq!(error.to_string(), "invalid task handle");
        assert!(!error.to_string().contains("backend-a"));
    }

    #[test]
    fn invalid_keys_are_rejected_and_debug_output_is_redacted() {
        assert_eq!("short".parse::<TaskHandleKey>(), Err(TaskHandleKeyError));
        let key: TaskHandleKey = KEY.parse().expect("test key is valid");

        assert_eq!(format!("{key:?}"), "TaskHandleKey([REDACTED])");
        assert!(!format!("{key:?}").contains(KEY));
    }

    #[test]
    fn decode_errors_map_to_indistinguishable_invalid_params_errors() {
        for error in [
            TaskHandleError::Invalid,
            TaskHandleError::UnsupportedVersion,
            TaskHandleError::WrongScope,
            TaskHandleError::UnavailableBackend,
        ] {
            let protocol_error = ErrorData::from(error);

            assert_eq!(protocol_error.code, ErrorCode::INVALID_PARAMS);
            assert_eq!(protocol_error.message, "invalid task ID");
            assert_eq!(protocol_error.data, None);
        }

        let protocol_error = ErrorData::from(TaskHandleError::Encode);
        assert_eq!(protocol_error.code, ErrorCode::INTERNAL_ERROR);
        assert_eq!(protocol_error.message, "failed to create task handle");
    }
}
