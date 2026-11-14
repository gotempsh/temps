// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Payload encryption and storage (ADR-106 §5, Layer 1).
//!
//! Every payload (run input, output and error, step results and errors, event
//! payloads) is AES-256-GCM encrypted by `EncryptionService` under associated
//! data naming its project, environment, run and field. There is no plaintext
//! option. A ciphertext up to the environment's inline cap is stored in the
//! row; a larger one goes to blob storage when it is available, and is
//! rejected with a typed error otherwise. Plaintext exists only in memory, on
//! its way to the executor or to an audited reveal, and never in a log line or
//! an error message.

use std::sync::Arc;

use bytes::Bytes;
use serde_json::Value;
use temps_core::{EncryptionService, PayloadBlobStore};
use uuid::Uuid;

use crate::error::{WorkflowError, WorkflowResult};
use crate::types::WorkflowSettings;

/// Reserved blob prefix; nothing else in a project's bucket uses it.
pub const BLOB_PREFIX: &str = ".temps/workflows";

/// Which payload of a run (or an event) a value is.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PayloadField {
    RunInput,
    RunOutput,
    RunError,
    StepResult(i32),
    StepError(i32),
    /// An event's payload, bound to its name and key.
    Event {
        name: String,
        key: String,
    },
}

impl PayloadField {
    /// The label used in associated data, blob keys and error messages.
    pub fn label(&self) -> String {
        match self {
            PayloadField::RunInput => "input".into(),
            PayloadField::RunOutput => "output".into(),
            PayloadField::RunError => "error".into(),
            PayloadField::StepResult(sequence) => format!("step-{sequence}-result"),
            PayloadField::StepError(sequence) => format!("step-{sequence}-error"),
            PayloadField::Event { .. } => "event".into(),
        }
    }
}

/// Where a payload belongs. Events have no run; their scope uses the nil UUID
/// and the field carries the event's identity.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PayloadScope {
    pub project_id: i32,
    pub environment_id: i32,
    pub run_id: Uuid,
}

impl PayloadScope {
    fn associated_data(&self, field: &PayloadField) -> Vec<u8> {
        let mut aad = format!(
            "temps-workflows/v1|project:{}|environment:{}|run:{}|{}",
            self.project_id,
            self.environment_id,
            self.run_id,
            field.label()
        );
        if let PayloadField::Event { name, key } = field {
            // Length-prefixed, so no name/key pair can collide with another.
            aad.push_str(&format!("|{}:{name}|{}:{key}", name.len(), key.len()));
        }
        aad.into_bytes()
    }

    /// A key no other write uses. A write that loses a race (a retried
    /// callback, a second delivery of an event) discards its own object and
    /// never the one the winning write committed under the same lease.
    fn blob_key(&self, field: &PayloadField, lease: i64) -> String {
        format!(
            "{BLOB_PREFIX}/{}/{}/{}-{lease}-{}",
            self.environment_id,
            self.run_id,
            field.label(),
            Uuid::new_v4().simple()
        )
    }

    /// Every blob of the run lives under this prefix.
    pub fn run_blob_prefix(&self) -> String {
        format!("{BLOB_PREFIX}/{}/{}/", self.environment_id, self.run_id)
    }
}

/// A sealed payload as the row stores it. Holds ciphertext only.
#[derive(Clone, PartialEq, Eq, Default, serde::Serialize, serde::Deserialize)]
pub struct StoredPayload {
    pub encrypted: Option<String>,
    pub blob_key: Option<String>,
    /// Plaintext JSON size in bytes.
    pub bytes: i32,
}

impl std::fmt::Debug for StoredPayload {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("StoredPayload")
            .field("inline", &self.encrypted.is_some())
            .field("blob_key", &self.blob_key)
            .field("bytes", &self.bytes)
            .finish()
    }
}

impl StoredPayload {
    pub fn is_empty(&self) -> bool {
        self.encrypted.is_none() && self.blob_key.is_none()
    }
}

/// Encrypts, stores and reads payloads. `blobs` is `None` in a binary without
/// blob storage; payloads over the inline cap are then rejected.
pub struct PayloadCodec {
    encryption: Arc<EncryptionService>,
    project_keys: Option<Arc<crate::project_keys::ProjectKeys>>,
    blobs: Option<Arc<dyn PayloadBlobStore>>,
}

impl PayloadCodec {
    pub fn new(
        encryption: Arc<EncryptionService>,
        blobs: Option<Arc<dyn PayloadBlobStore>>,
    ) -> Self {
        Self {
            encryption,
            blobs,
            project_keys: None,
        }
    }

    /// Production mode: new project payloads use independently shreddable keys.
    pub(crate) fn with_project_keys(mut self, keys: Arc<crate::project_keys::ProjectKeys>) -> Self {
        self.project_keys = Some(keys);
        self
    }

    pub(crate) async fn project_key_history(
        &self,
        project_id: i32,
        page: u64,
        page_size: u64,
    ) -> WorkflowResult<(Vec<temps_entities::workflow_project_keys::Model>, u64)> {
        match &self.project_keys {
            Some(keys) => keys.list(project_id, page, page_size).await,
            None => Ok((Vec::new(), 0)),
        }
    }

    pub(crate) async fn shred_project_key(
        &self,
        project_id: i32,
        key_id: Uuid,
    ) -> WorkflowResult<()> {
        let keys = self
            .project_keys
            .as_ref()
            .ok_or(WorkflowError::ProjectKeyUnavailable { project_id, key_id })?;
        keys.shred(project_id, key_id).await
    }

    pub fn has_blob_storage(&self) -> bool {
        self.blobs.is_some()
    }

    /// Encrypt `value` for `field` and store it inline or in blob storage.
    /// `lease` makes the blob key unique per attempt, so a stale writer cannot
    /// overwrite the object a newer lease recorded.
    pub async fn seal(
        &self,
        scope: &PayloadScope,
        field: &PayloadField,
        value: &Value,
        lease: i64,
        settings: &WorkflowSettings,
    ) -> WorkflowResult<StoredPayload> {
        let plaintext = serde_json::to_vec(value).map_err(|error| WorkflowError::Encoding {
            run_id: scope.run_id,
            field: field.label(),
            reason: error.to_string(),
        })?;
        let max = usize::try_from(settings.max_payload_bytes).unwrap_or(0);
        let inline = usize::try_from(settings.inline_payload_limit_bytes).unwrap_or(0);
        if plaintext.len() > max {
            return Err(WorkflowError::PayloadTooLarge {
                field: static_label(field),
                size: plaintext.len(),
                limit: max,
                blob_unavailable: false,
            });
        }
        let ciphertext =
            if let Some(keys) = self.project_keys.as_ref().filter(|_| scope.project_id > 0) {
                let key = keys.active(scope.project_id).await?;
                let wrapped =
                    key.wrapped_key
                        .as_deref()
                        .ok_or(WorkflowError::ProjectKeyUnavailable {
                            project_id: scope.project_id,
                            key_id: key.id,
                        })?;
                let encrypted = self
                    .encryption
                    .encrypt_with_project_key(
                        scope.project_id,
                        &key.id.to_string(),
                        wrapped,
                        &plaintext,
                        &scope.associated_data(field),
                    )
                    .map_err(|source| WorkflowError::Encryption {
                        run_id: scope.run_id,
                        field: field.label(),
                        operation: "encrypt project payload",
                        source,
                    })?;
                format!("pk1:{}:{encrypted}", key.id)
            } else {
                self.encryption
                    .encrypt_with_aad(&plaintext, &scope.associated_data(field))
                    .map_err(|source| WorkflowError::Encryption {
                        run_id: scope.run_id,
                        field: field.label(),
                        operation: "encrypt",
                        source,
                    })?
            };
        let bytes = i32::try_from(plaintext.len()).unwrap_or(i32::MAX);
        if plaintext.len() <= inline {
            return Ok(StoredPayload {
                encrypted: Some(ciphertext),
                blob_key: None,
                bytes,
            });
        }
        let Some(blobs) = &self.blobs else {
            return Err(WorkflowError::PayloadTooLarge {
                field: static_label(field),
                size: plaintext.len(),
                limit: inline,
                blob_unavailable: true,
            });
        };
        let key = scope.blob_key(field, lease);
        blobs
            .put(scope.project_id, &key, Bytes::from(ciphertext.into_bytes()))
            .await?;
        Ok(StoredPayload {
            encrypted: None,
            blob_key: Some(key),
            bytes,
        })
    }

    /// Decrypt a stored payload; `None` when nothing was stored.
    pub async fn open(
        &self,
        scope: &PayloadScope,
        field: &PayloadField,
        stored: &StoredPayload,
        settings: &WorkflowSettings,
    ) -> WorkflowResult<Option<Value>> {
        let ciphertext = match (&stored.encrypted, &stored.blob_key) {
            (Some(inline), _) => inline.clone(),
            (None, Some(key)) => {
                let Some(blobs) = &self.blobs else {
                    return Err(WorkflowError::Blob(temps_core::PayloadBlobError::Unavailable {
                        project_id: scope.project_id,
                        reason: format!(
                            "the {} payload of run {} is in blob storage, which this server cannot reach",
                            field.label(),
                            scope.run_id
                        ),
                    }));
                };
                // Base64 of the ciphertext is 4/3 of the plaintext plus the
                // nonce and tag; twice the cap bounds it with room to spare.
                let max = usize::try_from(settings.max_payload_bytes)
                    .unwrap_or(0)
                    .saturating_mul(2);
                let body = blobs.get(scope.project_id, key, max).await?;
                String::from_utf8(body.to_vec()).map_err(|_| WorkflowError::Encoding {
                    run_id: scope.run_id,
                    field: field.label(),
                    reason: "blob object is not base64 text".into(),
                })?
            }
            (None, None) => return Ok(None),
        };
        let plaintext = if let Some(envelope) = ciphertext.strip_prefix("pk1:") {
            let (id, encrypted) =
                envelope
                    .split_once(':')
                    .ok_or_else(|| WorkflowError::Encoding {
                        run_id: scope.run_id,
                        field: field.label(),
                        reason: "invalid project-key envelope".into(),
                    })?;
            let id = Uuid::parse_str(id).map_err(|_| WorkflowError::Encoding {
                run_id: scope.run_id,
                field: field.label(),
                reason: "invalid project-key identifier".into(),
            })?;
            let keys = self
                .project_keys
                .as_ref()
                .ok_or(WorkflowError::ProjectKeyUnavailable {
                    project_id: scope.project_id,
                    key_id: id,
                })?;
            let key = keys.get(scope.project_id, id).await?;
            let wrapped =
                key.wrapped_key
                    .as_deref()
                    .ok_or(WorkflowError::ProjectKeyUnavailable {
                        project_id: scope.project_id,
                        key_id: id,
                    })?;
            self.encryption
                .decrypt_with_project_key(
                    scope.project_id,
                    &id.to_string(),
                    wrapped,
                    encrypted,
                    &scope.associated_data(field),
                )
                .map_err(|source| WorkflowError::Encryption {
                    run_id: scope.run_id,
                    field: field.label(),
                    operation: "decrypt project payload",
                    source,
                })?
        } else {
            self.encryption
                .decrypt_with_aad(&ciphertext, &scope.associated_data(field))
                .map_err(|source| WorkflowError::Encryption {
                    run_id: scope.run_id,
                    field: field.label(),
                    operation: "decrypt",
                    source,
                })?
        };
        serde_json::from_slice(&plaintext)
            .map(Some)
            .map_err(|_| WorkflowError::Encoding {
                run_id: scope.run_id,
                field: field.label(),
                reason: "decrypted payload is not JSON".into(),
            })
    }

    /// Remove a blob this request uploaded but did not end up recording.
    pub async fn discard(&self, project_id: i32, stored: &StoredPayload) {
        if let (Some(blobs), Some(key)) = (&self.blobs, &stored.blob_key) {
            if let Err(error) = blobs.delete_prefix(project_id, key).await {
                tracing::warn!(project_id, blob_key = %key, %error, "could not remove an unrecorded workflow payload blob");
            }
        }
    }

    /// Delete every blob of a run (run deletion, retention).
    pub async fn delete_run_blobs(&self, scope: &PayloadScope) -> bool {
        if let Some(blobs) = &self.blobs {
            if let Err(error) = blobs
                .delete_prefix(scope.project_id, &scope.run_blob_prefix())
                .await
            {
                tracing::warn!(project_id = scope.project_id, run_id = %scope.run_id, %error, "could not delete a workflow run's payload blobs; durable cleanup will retry");
                return false;
            }
            return true;
        }
        false
    }
}

fn static_label(field: &PayloadField) -> &'static str {
    match field {
        PayloadField::RunInput => "input",
        PayloadField::RunOutput => "output",
        PayloadField::RunError => "error",
        PayloadField::StepResult(_) => "step result",
        PayloadField::StepError(_) => "step error",
        PayloadField::Event { .. } => "event",
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use std::collections::HashMap;
    use std::sync::Mutex;
    use temps_core::PayloadBlobError;

    /// In-memory blob store for tests.
    #[derive(Default)]
    pub(crate) struct MemoryBlobs {
        pub objects: Mutex<HashMap<(i32, String), Bytes>>,
        pub fail_delete: std::sync::atomic::AtomicBool,
    }

    #[async_trait::async_trait]
    impl PayloadBlobStore for MemoryBlobs {
        async fn put(
            &self,
            project_id: i32,
            key: &str,
            body: Bytes,
        ) -> Result<(), PayloadBlobError> {
            self.objects
                .lock()
                .unwrap()
                .insert((project_id, key.to_string()), body);
            Ok(())
        }
        async fn get(
            &self,
            project_id: i32,
            key: &str,
            _max: usize,
        ) -> Result<Bytes, PayloadBlobError> {
            self.objects
                .lock()
                .unwrap()
                .get(&(project_id, key.to_string()))
                .cloned()
                .ok_or(PayloadBlobError::NotFound {
                    project_id,
                    key: key.to_string(),
                })
        }
        async fn delete_prefix(
            &self,
            project_id: i32,
            prefix: &str,
        ) -> Result<u64, PayloadBlobError> {
            if self.fail_delete.load(std::sync::atomic::Ordering::SeqCst) {
                return Err(PayloadBlobError::Unavailable {
                    project_id,
                    reason: "simulated storage outage".into(),
                });
            }
            let mut objects = self.objects.lock().unwrap();
            let before = objects.len();
            objects.retain(|(p, k), _| !(*p == project_id && k.starts_with(prefix)));
            Ok((before - objects.len()) as u64)
        }
    }

    pub(crate) fn encryption() -> Arc<EncryptionService> {
        Arc::new(EncryptionService::new("12345678901234567890123456789012").unwrap())
    }

    fn scope() -> PayloadScope {
        PayloadScope {
            project_id: 1,
            environment_id: 2,
            run_id: Uuid::new_v4(),
        }
    }

    const SECRET: &str = "card-4242-4242-SECRET-MARKER";

    #[tokio::test]
    async fn sealed_payloads_hold_no_plaintext_and_open_only_in_place() {
        let codec = PayloadCodec::new(encryption(), None);
        let scope = scope();
        let settings = WorkflowSettings::default();
        let value = serde_json::json!({ "card": SECRET });
        let stored = codec
            .seal(&scope, &PayloadField::StepResult(0), &value, 1, &settings)
            .await
            .unwrap();
        let inline = stored.encrypted.clone().unwrap();
        assert!(!inline.contains(SECRET));
        assert!(
            !format!("{stored:?}").contains(&inline),
            "Debug never prints ciphertext"
        );
        assert_eq!(
            codec
                .open(&scope, &PayloadField::StepResult(0), &stored, &settings)
                .await
                .unwrap(),
            Some(value)
        );
        // Bound to step 0: the same ciphertext does not open as step 1, nor
        // in another run.
        assert!(codec
            .open(&scope, &PayloadField::StepResult(1), &stored, &settings)
            .await
            .is_err());
        let other_run = PayloadScope {
            run_id: Uuid::new_v4(),
            ..scope
        };
        assert!(codec
            .open(&other_run, &PayloadField::StepResult(0), &stored, &settings)
            .await
            .is_err());
    }

    #[tokio::test]
    async fn payloads_over_the_inline_cap_go_to_blob_storage_encrypted() {
        let blobs = Arc::new(MemoryBlobs::default());
        let codec = PayloadCodec::new(encryption(), Some(blobs.clone()));
        let scope = scope();
        let settings = WorkflowSettings {
            inline_payload_limit_bytes: 1024,
            ..WorkflowSettings::default()
        };
        let value = serde_json::json!({ "blob": format!("{SECRET}{}", "x".repeat(4096)) });
        let stored = codec
            .seal(&scope, &PayloadField::RunInput, &value, 3, &settings)
            .await
            .unwrap();
        let key = stored.blob_key.clone().unwrap();
        assert!(key.starts_with(&scope.run_blob_prefix()));
        let object = blobs
            .objects
            .lock()
            .unwrap()
            .get(&(1, key))
            .cloned()
            .unwrap();
        assert!(!String::from_utf8_lossy(&object).contains(SECRET));
        assert_eq!(
            codec
                .open(&scope, &PayloadField::RunInput, &stored, &settings)
                .await
                .unwrap(),
            Some(value)
        );
        codec.delete_run_blobs(&scope).await;
        assert!(blobs.objects.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn oversize_payloads_are_typed_errors_without_the_payload() {
        let codec = PayloadCodec::new(encryption(), None);
        let settings = WorkflowSettings {
            inline_payload_limit_bytes: 1024,
            ..WorkflowSettings::default()
        };
        let value = serde_json::json!({ "blob": format!("{SECRET}{}", "x".repeat(4096)) });
        let error = codec
            .seal(&scope(), &PayloadField::RunInput, &value, 1, &settings)
            .await
            .unwrap_err();
        assert!(matches!(
            error,
            WorkflowError::PayloadTooLarge {
                blob_unavailable: true,
                limit: 1024,
                ..
            }
        ));
        assert!(!error.to_string().contains(SECRET));

        let over_max = WorkflowSettings {
            max_payload_bytes: 2048,
            inline_payload_limit_bytes: 1024,
            ..WorkflowSettings::default()
        };
        let error = codec
            .seal(&scope(), &PayloadField::RunInput, &value, 1, &over_max)
            .await
            .unwrap_err();
        assert!(matches!(
            error,
            WorkflowError::PayloadTooLarge {
                blob_unavailable: false,
                limit: 2048,
                ..
            }
        ));
    }

    #[tokio::test]
    async fn payload_never_reaches_an_error_message() {
        let codec = PayloadCodec::new(encryption(), None);
        let scope = scope();
        let settings = WorkflowSettings::default();
        let stored = codec
            .seal(
                &scope,
                &PayloadField::RunOutput,
                &serde_json::json!(SECRET),
                1,
                &settings,
            )
            .await
            .unwrap();
        let error = codec
            .open(&scope, &PayloadField::RunError, &stored, &settings)
            .await
            .unwrap_err();
        let message = error.to_string();
        assert!(!message.contains(SECRET));
        assert!(!message.contains(stored.encrypted.as_deref().unwrap()));
    }

    #[test]
    fn event_associated_data_cannot_collide_across_name_and_key() {
        let scope = scope();
        let a = scope.associated_data(&PayloadField::Event {
            name: "a|b".into(),
            key: "c".into(),
        });
        let b = scope.associated_data(&PayloadField::Event {
            name: "a".into(),
            key: "b|c".into(),
        });
        assert_ne!(a, b);
    }
}
