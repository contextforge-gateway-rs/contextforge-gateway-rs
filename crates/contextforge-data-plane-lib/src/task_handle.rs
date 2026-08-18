//! Stateless routing handles for the MCP Tasks extension.
//!
//! Handles are encrypted and authenticated because their payload contains an
//! upstream task identifier that may be a bearer token. The trusted
//! authorization context, virtual host, configuration revision, and backend
//! generation prevent replay through a different caller, policy snapshot, or
//! upstream route.

use std::{fmt, str::FromStr};

use aes_gcm_siv::{
    Aes256GcmSiv, Nonce,
    aead::{Aead, KeyInit, Payload as AeadPayload},
};
use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use rmcp::{ErrorData, model::ErrorCode};
use secret_string::SecretString;
use serde::{Deserialize, Serialize};
use thiserror::Error;

const HANDLE_PREFIX: &str = "cfth1";
const HANDLE_FAMILY_PREFIX: &str = "cfth";
const KEY_LEN: usize = 32;
const NONCE_LEN: usize = 12;
const TAG_LEN: usize = 16;

/// A validated AES-256-GCM-SIV key for task-handle protection.
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
///
/// The authorization-context ID and configuration revision must come from the
/// verified JWT and the validated effective-configuration snapshot. Client
/// metadata and MCP params are not trusted sources for either value.
#[derive(Clone, Copy, PartialEq, Eq)]
pub struct TaskHandleScope<'a> {
    authorization_context_id: &'a str,
    virtual_host_id: &'a str,
    configuration_revision: &'a str,
}

impl<'a> TaskHandleScope<'a> {
    /// Creates a scope from trusted authorization and routing context.
    pub fn new(authorization_context_id: &'a str, virtual_host_id: &'a str, configuration_revision: &'a str) -> Self {
        Self { authorization_context_id, virtual_host_id, configuration_revision }
    }
}

/// Stable backend identity stored in a task handle.
///
/// `generation` must change when the routing key is reassigned to a different
/// upstream or when routing material changes in a way that invalidates
/// outstanding upstream task IDs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TaskHandleBackend<'a> {
    id: &'a str,
    generation: &'a str,
}

impl<'a> TaskHandleBackend<'a> {
    /// Creates backend identity from trusted effective configuration.
    pub fn new(id: &'a str, generation: &'a str) -> Self {
        Self { id, generation }
    }

    /// Stable backend identifier used for routing.
    pub fn id(self) -> &'a str {
        self.id
    }

    /// Generation of the backend routing material.
    pub fn generation(self) -> &'a str {
        self.generation
    }
}

/// The route recovered from a valid task handle.
#[derive(Clone, PartialEq, Eq)]
pub struct TaskHandleRoute {
    backend_id: String,
    upstream_task_id: String,
}

impl TaskHandleRoute {
    /// Stable backend ID in the caller's current effective configuration.
    pub fn backend_id(&self) -> &str {
        &self.backend_id
    }

    /// Original task identifier expected by the upstream backend.
    pub fn upstream_task_id(&self) -> &str {
        &self.upstream_task_id
    }
}

impl fmt::Debug for TaskHandleRoute {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("TaskHandleRoute")
            .field("backend_id", &self.backend_id)
            .field("upstream_task_id", &"[REDACTED]")
            .finish()
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

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct TaskHandlePayload {
    authorization_context_id: String,
    virtual_host_id: String,
    configuration_revision: String,
    backend_id: String,
    backend_generation: String,
    upstream_task_id: String,
}

/// Encodes and decodes versioned task handles shared across dataplane replicas.
#[derive(Clone)]
pub struct TaskHandleCodec {
    cipher: Aes256GcmSiv,
}

impl TaskHandleCodec {
    /// Creates a codec from a validated shared key.
    pub fn new(key: &TaskHandleKey) -> Result<Self, TaskHandleKeyError> {
        let cipher = Aes256GcmSiv::new_from_slice(&key.bytes()?).map_err(|_| TaskHandleKeyError)?;
        Ok(Self { cipher })
    }

    /// Creates an opaque handle for one upstream task.
    pub fn encode(
        &self,
        scope: TaskHandleScope<'_>,
        backend: TaskHandleBackend<'_>,
        upstream_task_id: &str,
    ) -> Result<String, TaskHandleError> {
        let payload = TaskHandlePayload {
            authorization_context_id: scope.authorization_context_id.to_owned(),
            virtual_host_id: scope.virtual_host_id.to_owned(),
            configuration_revision: scope.configuration_revision.to_owned(),
            backend_id: backend.id.to_owned(),
            backend_generation: backend.generation.to_owned(),
            upstream_task_id: upstream_task_id.to_owned(),
        };
        let plaintext = serde_json::to_vec(&payload).map_err(|_| TaskHandleError::Encode)?;

        let mut nonce_bytes = [0_u8; NONCE_LEN];
        getrandom::fill(&mut nonce_bytes).map_err(|_| TaskHandleError::Encode)?;
        let nonce = Nonce::from(nonce_bytes);
        let ciphertext = self
            .cipher
            .encrypt(&nonce, AeadPayload { msg: &plaintext, aad: HANDLE_PREFIX.as_bytes() })
            .map_err(|_| TaskHandleError::Encode)?;

        let mut protected = Vec::with_capacity(NONCE_LEN + ciphertext.len());
        protected.extend_from_slice(&nonce_bytes);
        protected.extend_from_slice(&ciphertext);
        Ok(format!("{HANDLE_PREFIX}.{}", URL_SAFE_NO_PAD.encode(protected)))
    }

    /// Decodes a handle and verifies that it belongs to the authenticated
    /// authorization context and references the same backend generation in
    /// current effective configuration.
    pub fn decode<F>(
        &self,
        handle: &str,
        expected_scope: TaskHandleScope<'_>,
        backend_is_current: F,
    ) -> Result<TaskHandleRoute, TaskHandleError>
    where
        F: FnOnce(TaskHandleBackend<'_>) -> bool,
    {
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
        let nonce = Nonce::from(nonce_bytes);
        let plaintext = self
            .cipher
            .decrypt(&nonce, AeadPayload { msg: ciphertext, aad: HANDLE_PREFIX.as_bytes() })
            .map_err(|_| TaskHandleError::Invalid)?;
        let payload: TaskHandlePayload = serde_json::from_slice(&plaintext).map_err(|_| TaskHandleError::Invalid)?;

        if payload.authorization_context_id != expected_scope.authorization_context_id
            || payload.virtual_host_id != expected_scope.virtual_host_id
            || payload.configuration_revision != expected_scope.configuration_revision
        {
            return Err(TaskHandleError::WrongScope);
        }
        if !backend_is_current(TaskHandleBackend::new(&payload.backend_id, &payload.backend_generation)) {
            return Err(TaskHandleError::UnavailableBackend);
        }

        Ok(TaskHandleRoute { backend_id: payload.backend_id, upstream_task_id: payload.upstream_task_id })
    }
}

fn is_other_version(prefix: &str) -> bool {
    prefix
        .strip_prefix(HANDLE_FAMILY_PREFIX)
        .is_some_and(|version| !version.is_empty() && version.bytes().all(|byte| byte.is_ascii_digit()))
}

#[cfg(test)]
mod tests {
    use super::*;

    const KEY: &str = "AAECAwQFBgcICQoLDA0ODxAREhMUFRYXGBkaGxwdHh8"; // pragma: allowlist secret
    const AUTHORIZATION_CONTEXT: &str = "tenant-a:principal-a:team-a:scope-set-a";
    const VIRTUAL_HOST_ID: &str = "host-a";
    const CONFIGURATION_REVISION: &str = "revision-7";
    const BACKEND_ID: &str = "backend-a";
    const BACKEND_GENERATION: &str = "generation-3";

    fn codec() -> TaskHandleCodec {
        TaskHandleCodec::new(&KEY.parse().expect("test key is valid")).expect("test key initializes AES-256-GCM-SIV")
    }

    fn scope<'a>(
        authorization_context_id: &'a str,
        virtual_host_id: &'a str,
        configuration_revision: &'a str,
    ) -> TaskHandleScope<'a> {
        TaskHandleScope::new(authorization_context_id, virtual_host_id, configuration_revision)
    }

    fn current_scope() -> TaskHandleScope<'static> {
        scope(AUTHORIZATION_CONTEXT, VIRTUAL_HOST_ID, CONFIGURATION_REVISION)
    }

    fn backend<'a>(id: &'a str, generation: &'a str) -> TaskHandleBackend<'a> {
        TaskHandleBackend::new(id, generation)
    }

    fn current_backend() -> TaskHandleBackend<'static> {
        backend(BACKEND_ID, BACKEND_GENERATION)
    }

    fn decode_current(
        codec: &TaskHandleCodec,
        handle: &str,
        expected_scope: TaskHandleScope<'_>,
        expected_backend: TaskHandleBackend<'_>,
    ) -> Result<TaskHandleRoute, TaskHandleError> {
        codec.decode(handle, expected_scope, |decoded_backend| {
            decoded_backend.id() == expected_backend.id()
                && decoded_backend.generation() == expected_backend.generation()
        })
    }

    #[test]
    fn arbitrary_upstream_task_ids_round_trip_without_loss() {
        let codec = codec();
        let task_ids = ["", "simple", "with/slashes?and=query", "nul\0byte", "emoji-🦀", "line\nbreak"];

        for task_id in task_ids {
            let handle = codec.encode(current_scope(), current_backend(), task_id).expect("handle encodes");
            let route = decode_current(&codec, &handle, current_scope(), current_backend()).expect("handle decodes");

            assert_eq!(route.backend_id(), BACKEND_ID);
            assert_eq!(route.upstream_task_id(), task_id);
        }
    }

    #[test]
    fn identical_task_ids_from_different_backends_remain_isolated() {
        let codec = codec();
        let backend_a = backend("backend-a", "generation-a");
        let backend_b = backend("backend-b", "generation-b");
        let handle_a = codec.encode(current_scope(), backend_a, "same-id").expect("handle A encodes");
        let handle_b = codec.encode(current_scope(), backend_b, "same-id").expect("handle B encodes");

        let route_a = decode_current(&codec, &handle_a, current_scope(), backend_a).expect("handle A decodes");
        let route_b = decode_current(&codec, &handle_b, current_scope(), backend_b).expect("handle B decodes");

        assert_ne!(handle_a, handle_b);
        assert_eq!(route_a.backend_id(), "backend-a");
        assert_eq!(route_b.backend_id(), "backend-b");
    }

    #[test]
    fn handle_decodes_on_another_codec_with_the_same_key() {
        let first_replica = codec();
        let second_replica = codec();
        let handle = first_replica.encode(current_scope(), current_backend(), "task-42").expect("handle encodes");

        let route =
            decode_current(&second_replica, &handle, current_scope(), current_backend()).expect("handle decodes");

        assert_eq!(route.upstream_task_id(), "task-42");
    }

    #[test]
    fn malformed_and_tampered_handles_fail_closed() {
        let codec = codec();
        let handle = codec.encode(current_scope(), current_backend(), "task-42").expect("handle encodes");
        let mut tampered = handle.into_bytes();
        let last = tampered.last_mut().expect("handle is non-empty");
        *last = if *last == b'A' { b'B' } else { b'A' };
        let tampered = String::from_utf8(tampered).expect("tampered handle remains UTF-8");

        assert_eq!(
            decode_current(&codec, "not-a-handle", current_scope(), current_backend()),
            Err(TaskHandleError::Invalid)
        );
        assert_eq!(
            decode_current(&codec, "cfth1.AA", current_scope(), current_backend()),
            Err(TaskHandleError::Invalid)
        );
        assert_eq!(
            decode_current(&codec, &tampered, current_scope(), current_backend()),
            Err(TaskHandleError::Invalid)
        );
    }

    #[test]
    fn unsupported_handle_versions_are_rejected() {
        let codec = codec();

        assert_eq!(
            decode_current(&codec, "cfth2.AA", current_scope(), current_backend()),
            Err(TaskHandleError::UnsupportedVersion)
        );
        assert_eq!(
            decode_current(&codec, "cfthx.AA", current_scope(), current_backend()),
            Err(TaskHandleError::Invalid)
        );
    }

    #[test]
    fn authorization_virtual_host_and_revision_scope_are_enforced() {
        let codec = codec();
        let handle = codec.encode(current_scope(), current_backend(), "task-42").expect("handle encodes");

        for wrong_scope in [
            scope("tenant-b:principal-a:team-a:scope-set-a", VIRTUAL_HOST_ID, CONFIGURATION_REVISION),
            scope(AUTHORIZATION_CONTEXT, "host-b", CONFIGURATION_REVISION),
            scope(AUTHORIZATION_CONTEXT, VIRTUAL_HOST_ID, "revision-8"),
        ] {
            assert_eq!(
                decode_current(&codec, &handle, wrong_scope, current_backend()),
                Err(TaskHandleError::WrongScope)
            );
        }
    }

    #[test]
    fn removed_or_reassigned_backends_are_rejected_without_exposing_identity() {
        let codec = codec();
        let handle = codec.encode(current_scope(), current_backend(), "task-42").expect("handle encodes");

        assert!(decode_current(&codec, &handle, current_scope(), current_backend()).is_ok());
        let removed = codec.decode(&handle, current_scope(), |_| false).expect_err("removed backend is rejected");
        let reassigned = decode_current(&codec, &handle, current_scope(), backend(BACKEND_ID, "generation-4"))
            .expect_err("reassigned backend is rejected");

        for error in [removed, reassigned] {
            assert_eq!(error, TaskHandleError::UnavailableBackend);
            assert_eq!(error.to_string(), "invalid task handle");
            assert!(!error.to_string().contains(BACKEND_ID));
            assert!(!error.to_string().contains(BACKEND_GENERATION));
        }
    }

    #[test]
    fn invalid_keys_and_sensitive_debug_output_are_redacted() {
        assert_eq!("short".parse::<TaskHandleKey>(), Err(TaskHandleKeyError));
        let key: TaskHandleKey = KEY.parse().expect("test key is valid");
        let codec = TaskHandleCodec::new(&key).expect("test key initializes codec");
        let handle = codec.encode(current_scope(), current_backend(), "bearer-task-id").expect("handle encodes");
        let route = decode_current(&codec, &handle, current_scope(), current_backend()).expect("handle decodes");

        assert_eq!(format!("{key:?}"), "TaskHandleKey([REDACTED])");
        assert!(!format!("{key:?}").contains(KEY));
        assert_eq!(
            format!("{route:?}"),
            "TaskHandleRoute { backend_id: \"backend-a\", upstream_task_id: \"[REDACTED]\" }"
        );
        assert!(!format!("{route:?}").contains("bearer-task-id"));
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
