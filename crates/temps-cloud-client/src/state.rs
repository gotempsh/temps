// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Persisted link state: which account this instance is connected to, and the
//! credential it uses.
//!
//! Written atomically (temp file + rename) so a crash mid-write cannot leave a
//! half-parsed file that makes a working instance look unenrolled.

use std::io::Write;
use std::path::Path;

use serde::{Deserialize, Serialize};
use temps_cloud_protocol::SpanRecord;
use thiserror::Error;
use uuid::Uuid;

const ENCRYPTED_STATE_VERSION: u8 = 1;

/// A telemetry shipment that has left the in-memory spool but has not yet
/// received a durable acknowledgement from Cloud.
///
/// Persisting both the id and payload is what makes an OSS process restart
/// safe after Cloud has reserved the submission but before it has stored the
/// spans. Retrying with a new id would strand the original money reservation;
/// dropping the payload would silently lose the managed projection.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub(crate) struct PendingSubmission {
    pub submission_id: Uuid,
    pub spans: Vec<SpanRecord>,
}

#[derive(Serialize, Deserialize)]
struct EncryptedEnrollmentState {
    version: u8,
    ciphertext: String,
}

#[derive(Debug, Error)]
pub enum StateError {
    #[error(
        "Cloud link state at {path} is unreadable. Restore the encryption key used to create it, or back up and remove the state file before reconnecting"
    )]
    UnreadableStateBlocksMutation { path: String },

    #[error("Disconnect from {current} before changing the managed backend to {requested}")]
    BackendChangeRequiresDisconnect { current: String, requested: String },

    #[error("Failed to read link state at {path}: {reason}")]
    Read { path: String, reason: String },

    #[error("Failed to write link state at {path}: {reason}")]
    Write { path: String, reason: String },

    #[error("Link state at {path} is corrupt: {reason}")]
    Corrupt { path: String, reason: String },

    #[error("Failed to {operation} encrypted link state at {path}: {reason}")]
    Encryption {
        path: String,
        operation: &'static str,
        reason: String,
    },
}

#[derive(Clone, PartialEq, Serialize, Deserialize)]
pub struct EnrollmentState {
    /// Minted once, on first run, and kept forever. Stable across
    /// re-enrollment so the backend recognises a returning instance rather
    /// than accumulating duplicates.
    pub instance_id: Uuid,

    /// Base URL of the managed backend.
    pub base_url: String,

    /// Records the explicit policy decision that admitted a loopback
    /// development origin. This survives restart without an ambient bypass.
    #[serde(default)]
    pub allow_loopback_development: bool,

    /// Bearer token. `None` means "known backend, not linked" — a different
    /// state from having no file at all, and worth distinguishing in the UI.
    pub token: Option<String>,

    pub tenant_id: Option<Uuid>,

    /// Cloud account shown in the local UI. Older state files legitimately do
    /// not contain it and can refresh it by reconnecting.
    #[serde(default)]
    pub account_email: Option<String>,

    /// Active telemetry retry, encrypted together with the link credential.
    /// Older state files predate durable retries and therefore load as `None`.
    #[serde(default)]
    pub(crate) pending_submission: Option<PendingSubmission>,
}

impl std::fmt::Debug for EnrollmentState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("EnrollmentState")
            .field("instance_id", &self.instance_id)
            .field("base_url", &self.base_url)
            .field("token", &self.token.as_ref().map(|_| "[REDACTED]"))
            .field("tenant_id", &self.tenant_id)
            .field("account_email", &self.account_email)
            .field(
                "pending_submission",
                &self
                    .pending_submission
                    .as_ref()
                    .map(|pending| (pending.submission_id, pending.spans.len())),
            )
            .finish()
    }
}

impl EnrollmentState {
    /// A brand-new, unlinked instance.
    pub fn new(base_url: impl Into<String>) -> Self {
        Self {
            instance_id: Uuid::new_v4(),
            base_url: base_url.into(),
            allow_loopback_development: false,
            token: None,
            tenant_id: None,
            account_email: None,
            pending_submission: None,
        }
    }

    pub fn is_linked(&self) -> bool {
        self.token.is_some()
    }

    fn migrate_legacy_loopback_policy(&mut self) -> bool {
        if self.allow_loopback_development {
            return false;
        }
        let Ok(url) = url::Url::parse(&self.base_url) else {
            return false;
        };
        let loopback = url
            .host_str()
            .is_some_and(|host| host.eq_ignore_ascii_case("localhost"))
            || matches!(url.host(), Some(url::Host::Ipv4(ip)) if ip.is_loopback())
            || matches!(url.host(), Some(url::Host::Ipv6(ip)) if ip.is_loopback());
        if url.scheme() == "http" && loopback {
            // Production validation has never admitted this shape, so a
            // persisted loopback HTTP origin proves prior explicit dev opt-in.
            self.allow_loopback_development = true;
            true
        } else {
            false
        }
    }

    /// Load, or `Ok(None)` when this instance has never been linked.
    ///
    /// A missing file is a normal state, not an error — most instances never
    /// connect anything, and treating that as a failure would fill their logs.
    pub fn load(path: &Path) -> Result<Option<Self>, StateError> {
        #[cfg(unix)]
        if path.exists() {
            use std::os::unix::fs::PermissionsExt;

            let read_err = |reason: String| StateError::Read {
                path: path.display().to_string(),
                reason,
            };
            let dir = path
                .parent()
                .filter(|parent| !parent.as_os_str().is_empty())
                .unwrap_or_else(|| Path::new("."));
            std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700))
                .map_err(|e| read_err(format!("protect credential directory: {e}")))?;
            std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))
                .map_err(|e| read_err(format!("protect credential file: {e}")))?;
        }

        let raw = match std::fs::read_to_string(path) {
            Ok(r) => r,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(e) => {
                return Err(StateError::Read {
                    path: path.display().to_string(),
                    reason: e.to_string(),
                })
            }
        };

        let mut state: Self = serde_json::from_str(&raw).map_err(|e| StateError::Corrupt {
            path: path.display().to_string(),
            reason: e.to_string(),
        })?;
        if state.migrate_legacy_loopback_policy() {
            state.save(path)?;
        }
        Ok(Some(state))
    }

    /// Load the versioned encrypted format, atomically migrating a legacy
    /// plaintext state before returning it to the caller.
    pub fn load_encrypted(
        path: &Path,
        encryption: &temps_core::EncryptionService,
    ) -> Result<Option<Self>, StateError> {
        let Some(raw) = read_state_file(path)? else {
            return Ok(None);
        };
        if let Ok(envelope) = serde_json::from_str::<EncryptedEnrollmentState>(&raw) {
            if envelope.version != ENCRYPTED_STATE_VERSION {
                return Err(StateError::Corrupt {
                    path: path.display().to_string(),
                    reason: format!("unsupported encrypted state version {}", envelope.version),
                });
            }
            let plaintext = encryption
                .decrypt_string(&envelope.ciphertext)
                .map_err(|error| StateError::Encryption {
                    path: path.display().to_string(),
                    operation: "decrypt",
                    reason: error.to_string(),
                })?;
            let mut state: Self =
                serde_json::from_str(&plaintext).map_err(|error| StateError::Corrupt {
                    path: path.display().to_string(),
                    reason: format!("decrypted state is invalid: {error}"),
                })?;
            if state.migrate_legacy_loopback_policy() {
                state.save_encrypted(path, encryption)?;
            }
            return Ok(Some(state));
        }

        let mut legacy: Self = serde_json::from_str(&raw).map_err(|error| StateError::Corrupt {
            path: path.display().to_string(),
            reason: error.to_string(),
        })?;
        legacy.migrate_legacy_loopback_policy();
        legacy.save_encrypted(path, encryption)?;
        Ok(Some(legacy))
    }

    /// Persist atomically.
    pub fn save(&self, path: &Path) -> Result<(), StateError> {
        let write_err = |reason: String| StateError::Write {
            path: path.display().to_string(),
            reason,
        };

        let dir = path
            .parent()
            .filter(|parent| !parent.as_os_str().is_empty())
            .unwrap_or_else(|| Path::new("."));
        std::fs::create_dir_all(dir).map_err(|e| write_err(e.to_string()))?;

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700))
                .map_err(|e| write_err(e.to_string()))?;
        }

        let json = serde_json::to_string_pretty(self).map_err(|e| write_err(e.to_string()))?;

        // A unique O_EXCL temporary file prevents a predictable `.tmp` symlink
        // from redirecting the credential write. Same directory keeps the final
        // persist atomic on Unix.
        let mut tmp = tempfile::NamedTempFile::new_in(dir)
            .map_err(|e| write_err(format!("create secure temporary file: {e}")))?;
        tmp.write_all(json.as_bytes())
            .map_err(|e| write_err(format!("write secure temporary file: {e}")))?;
        tmp.as_file()
            .sync_all()
            .map_err(|e| write_err(format!("sync secure temporary file: {e}")))?;

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            tmp.as_file()
                .set_permissions(std::fs::Permissions::from_mode(0o600))
                .map_err(|e| write_err(format!("protect secure temporary file: {e}")))?;
        }

        tmp.persist(path)
            .map_err(|e| write_err(format!("atomically replace link state: {}", e.error)))?;

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))
                .map_err(|e| write_err(format!("protect link state: {e}")))?;
        }

        Ok(())
    }

    pub fn save_encrypted(
        &self,
        path: &Path,
        encryption: &temps_core::EncryptionService,
    ) -> Result<(), StateError> {
        let plaintext = serde_json::to_string(self).map_err(|error| StateError::Corrupt {
            path: path.display().to_string(),
            reason: error.to_string(),
        })?;
        let ciphertext =
            encryption
                .encrypt_string(&plaintext)
                .map_err(|error| StateError::Encryption {
                    path: path.display().to_string(),
                    operation: "encrypt",
                    reason: error.to_string(),
                })?;
        write_state_file(
            path,
            &serde_json::to_string_pretty(&EncryptedEnrollmentState {
                version: ENCRYPTED_STATE_VERSION,
                ciphertext,
            })
            .map_err(|error| StateError::Corrupt {
                path: path.display().to_string(),
                reason: error.to_string(),
            })?,
        )
    }

    /// Forget the credential but keep the identity.
    ///
    /// Disconnecting must not mint a new `instance_id`: re-linking later should
    /// reattach to the same instance record rather than orphaning its history.
    pub fn unlink(&mut self) {
        self.token = None;
        self.tenant_id = None;
        self.account_email = None;
        self.pending_submission = None;
    }
}

fn read_state_file(path: &Path) -> Result<Option<String>, StateError> {
    match std::fs::read_to_string(path) {
        Ok(raw) => Ok(Some(raw)),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(StateError::Read {
            path: path.display().to_string(),
            reason: error.to_string(),
        }),
    }
}

fn write_state_file(path: &Path, json: &str) -> Result<(), StateError> {
    let write_err = |reason: String| StateError::Write {
        path: path.display().to_string(),
        reason,
    };
    let dir = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    std::fs::create_dir_all(dir).map_err(|error| write_err(error.to_string()))?;
    let mut tmp = tempfile::NamedTempFile::new_in(dir)
        .map_err(|error| write_err(format!("create secure temporary file: {error}")))?;
    tmp.write_all(json.as_bytes())
        .map_err(|error| write_err(format!("write secure temporary file: {error}")))?;
    tmp.as_file()
        .sync_all()
        .map_err(|error| write_err(format!("sync secure temporary file: {error}")))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700))
            .map_err(|error| write_err(format!("protect credential directory: {error}")))?;
        tmp.as_file()
            .set_permissions(std::fs::Permissions::from_mode(0o600))
            .map_err(|error| write_err(format!("protect secure temporary file: {error}")))?;
    }
    tmp.persist(path)
        .map_err(|error| write_err(format!("atomically replace link state: {}", error.error)))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn temp() -> (tempfile::TempDir, PathBuf) {
        let d = tempfile::tempdir().unwrap();
        let p = d.path().join("nested").join("link.json");
        (d, p)
    }

    #[test]
    fn a_never_linked_instance_loads_as_none_not_an_error() {
        let (_d, p) = temp();
        assert!(matches!(EnrollmentState::load(&p), Ok(None)));
    }

    #[test]
    fn state_round_trips_through_disk() {
        let (_d, p) = temp();
        let mut s = EnrollmentState::new("https://cloud.test");
        s.token = Some("inst_abc".into());
        s.tenant_id = Some(Uuid::new_v4());
        s.account_email = Some("owner@example.com".into());

        s.save(&p).unwrap();
        assert_eq!(EnrollmentState::load(&p).unwrap(), Some(s));
    }

    #[test]
    fn encrypted_state_round_trips_without_plaintext_token() {
        let (_directory, path) = temp();
        let encryption = temps_core::EncryptionService::new_from_password("state-test-key");
        let mut state = EnrollmentState::new("https://cloud.test");
        state.token = Some("inst_secret_token".into());
        state.save_encrypted(&path, &encryption).unwrap();

        let raw = std::fs::read_to_string(&path).unwrap();
        assert!(!raw.contains("inst_secret_token"));
        assert!(raw.contains("\"version\": 1"));
        assert_eq!(
            EnrollmentState::load_encrypted(&path, &encryption).unwrap(),
            Some(state)
        );
    }

    #[test]
    fn encrypted_legacy_loopback_state_is_safely_migrated() {
        let (_directory, path) = temp();
        let encryption = temps_core::EncryptionService::new_from_password("legacy-loopback-key");
        let plaintext = serde_json::json!({
            "instance_id": Uuid::new_v4(),
            "base_url": "http://127.0.0.1:19202/",
            "token": null,
            "tenant_id": null,
            "account_email": null
        })
        .to_string();
        let envelope = EncryptedEnrollmentState {
            version: ENCRYPTED_STATE_VERSION,
            ciphertext: encryption.encrypt_string(&plaintext).unwrap(),
        };
        write_state_file(&path, &serde_json::to_string(&envelope).unwrap()).unwrap();

        let migrated = EnrollmentState::load_encrypted(&path, &encryption)
            .unwrap()
            .expect("legacy state");
        assert!(migrated.allow_loopback_development);

        let raw = std::fs::read_to_string(&path).unwrap();
        let persisted: EncryptedEnrollmentState = serde_json::from_str(&raw).unwrap();
        let migrated_plaintext = encryption.decrypt_string(&persisted.ciphertext).unwrap();
        let migrated_json: serde_json::Value = serde_json::from_str(&migrated_plaintext).unwrap();
        assert_eq!(
            migrated_json
                .get("allow_loopback_development")
                .and_then(serde_json::Value::as_bool),
            Some(true)
        );
    }

    #[test]
    fn legacy_plaintext_state_is_atomically_migrated_on_load() {
        let (_directory, path) = temp();
        let encryption = temps_core::EncryptionService::new_from_password("migration-test-key");
        let mut state = EnrollmentState::new("https://cloud.test");
        state.token = Some("inst_legacy_secret".into());
        state.save(&path).unwrap();

        assert_eq!(
            EnrollmentState::load_encrypted(&path, &encryption).unwrap(),
            Some(state)
        );
        let migrated = std::fs::read_to_string(&path).unwrap();
        assert!(!migrated.contains("inst_legacy_secret"));
        assert!(serde_json::from_str::<EncryptedEnrollmentState>(&migrated).is_ok());
    }

    #[test]
    fn saving_creates_missing_parent_directories() {
        let (_d, p) = temp();
        EnrollmentState::new("https://cloud.test").save(&p).unwrap();
        assert!(p.exists());
    }

    #[test]
    fn a_corrupt_file_is_reported_with_its_path_not_silently_ignored() {
        let (_d, p) = temp();
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(&p, "{not json").unwrap();

        match EnrollmentState::load(&p) {
            Err(StateError::Corrupt { path, .. }) => {
                assert!(path.contains("link.json"), "error must name the file");
            }
            other => panic!("corruption must not be swallowed, got {other:?}"),
        }
    }

    #[test]
    fn unlinking_keeps_the_instance_identity() {
        let mut s = EnrollmentState::new("https://cloud.test");
        let id = s.instance_id;
        s.token = Some("inst_abc".into());
        s.tenant_id = Some(Uuid::new_v4());
        s.account_email = Some("owner@example.com".into());

        s.unlink();

        assert!(!s.is_linked());
        assert!(s.tenant_id.is_none());
        assert!(s.account_email.is_none());
        assert_eq!(s.instance_id, id, "re-linking must reattach, not orphan");
    }

    #[test]
    fn legacy_state_without_an_account_email_still_loads() {
        let (_d, p) = temp();
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        let instance_id = Uuid::new_v4();
        std::fs::write(
            &p,
            serde_json::json!({
                "instance_id": instance_id,
                "base_url": "https://cloud.test",
                "token": "inst_legacy",
                "tenant_id": Uuid::new_v4()
            })
            .to_string(),
        )
        .unwrap();

        let state = EnrollmentState::load(&p).unwrap().unwrap();
        assert_eq!(state.instance_id, instance_id);
        assert!(state.account_email.is_none());
    }

    #[test]
    fn saving_twice_leaves_no_temp_file_behind() {
        let (_d, p) = temp();
        let s = EnrollmentState::new("https://cloud.test");
        s.save(&p).unwrap();
        s.save(&p).unwrap();
        assert!(
            !p.with_extension("tmp").exists(),
            "temp file was left behind"
        );
    }

    #[cfg(unix)]
    #[test]
    fn persisted_credentials_are_private_to_the_owner() {
        use std::os::unix::fs::PermissionsExt;

        let (_d, p) = temp();
        let mut s = EnrollmentState::new("https://cloud.test");
        s.token = Some("inst_secret".into());
        s.save(&p).unwrap();

        assert_eq!(
            std::fs::metadata(&p).unwrap().permissions().mode() & 0o777,
            0o600
        );
        assert_eq!(
            std::fs::metadata(p.parent().unwrap())
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o700
        );
    }

    #[cfg(unix)]
    #[test]
    fn loading_repairs_permissions_from_a_legacy_installation() {
        use std::os::unix::fs::PermissionsExt;

        let (_d, p) = temp();
        let mut s = EnrollmentState::new("https://cloud.test");
        s.token = Some("inst_secret".into());
        s.save(&p).unwrap();
        std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o644)).unwrap();
        std::fs::set_permissions(p.parent().unwrap(), std::fs::Permissions::from_mode(0o755))
            .unwrap();

        assert!(EnrollmentState::load(&p).unwrap().is_some());
        assert_eq!(
            std::fs::metadata(&p).unwrap().permissions().mode() & 0o777,
            0o600
        );
        assert_eq!(
            std::fs::metadata(p.parent().unwrap())
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o700
        );
    }

    #[test]
    fn debug_output_redacts_the_bearer_token() {
        let mut s = EnrollmentState::new("https://cloud.test");
        s.token = Some("inst_secret".into());
        assert!(!format!("{s:?}").contains("inst_secret"));
    }

    #[test]
    fn a_known_backend_without_a_token_is_not_linked() {
        // Distinct from "no file": the operator configured a backend and has
        // not finished connecting, which the UI should say plainly.
        let s = EnrollmentState::new("https://cloud.test");
        assert!(!s.is_linked());
        assert!(s.token.is_none());
    }
}
