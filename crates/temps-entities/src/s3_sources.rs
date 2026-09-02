// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

use async_trait::async_trait;
use sea_orm::entity::prelude::*;
use sea_orm::{ActiveValue::Set, ConnectionTrait, DbErr};
use serde::{Deserialize, Serialize};
use temps_core::DBDateTime;

#[derive(Clone, Debug, PartialEq, DeriveEntityModel, Eq, Serialize, Deserialize)]
#[sea_orm(table_name = "s3_sources")]
pub struct Model {
    #[sea_orm(primary_key)]
    pub id: i32,
    pub name: String,
    pub bucket_name: String,
    pub region: String,
    pub endpoint: Option<String>,
    pub bucket_path: String,
    pub access_key_id: String,
    pub secret_key: String,
    /// STS-style session token for a *temporary* credential, encrypted at rest
    /// with the same [`temps_core::EncryptionService`] as `secret_key`.
    ///
    /// `None` for an ordinary long-lived credential — which is every source an
    /// operator ever typed in. Only a credential vended by Temps Cloud (which
    /// is prefix-scoped and short-lived, so it can only be minted through an
    /// STS-style API) carries one. SigV4 rejects a temporary key pair unless
    /// this token accompanies it as `X-Amz-Security-Token`, so it has to reach
    /// both the in-process `aws-sdk-s3` clients and the `AWS_SESSION_TOKEN`
    /// environment of every shelled-out engine (`wal-g`, `mc`, `mariabackup`).
    pub session_token: Option<String>,
    /// When the credential in this row stops working, for a temporary
    /// credential. `None` means it does not expire.
    ///
    /// Not a secret, and currently **write-only**: the Cloud enrolment path
    /// records it, but nothing reads it yet — it is absent from
    /// `S3SourceResponse` and the rotation loop does not consult it. It is
    /// stored now so a future console surface can warn about a lapse before an
    /// upload fails, and so rotation can eventually be scheduled off the actual
    /// expiry instead of a fixed interval.
    pub credentials_expire_at: Option<DBDateTime>,
    pub force_path_style: Option<bool>,
    pub is_default: bool,
    /// True when this row was auto-provisioned by a Temps Cloud link rather
    /// than entered by an operator. Managed rows are excluded from the
    /// user-initiated edit/delete paths in `temps-backup`; only the Cloud
    /// disconnect cleanup path may remove one.
    #[sea_orm(default_value = false)]
    pub managed_by_cloud: bool,
    pub created_at: DBDateTime,
    pub updated_at: DBDateTime,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {
    #[sea_orm(has_many = "super::backup_schedules::Entity")]
    BackupSchedules,
    #[sea_orm(has_many = "super::backups::Entity")]
    Backups,
}

impl Related<super::backup_schedules::Entity> for Entity {
    fn to() -> RelationDef {
        Relation::BackupSchedules.def()
    }
}

impl Related<super::backups::Entity> for Entity {
    fn to() -> RelationDef {
        Relation::Backups.def()
    }
}

#[async_trait]
impl ActiveModelBehavior for ActiveModel {
    async fn before_save<C>(mut self, _db: &C, insert: bool) -> Result<Self, DbErr>
    where
        C: ConnectionTrait,
    {
        let now = chrono::Utc::now();

        if insert {
            if self.created_at.is_not_set() {
                self.created_at = Set(now);
            }
            if self.updated_at.is_not_set() {
                self.updated_at = Set(now);
            }
        } else {
            self.updated_at = Set(now);
        }

        Ok(self)
    }
}

/// Plaintext S3-compatible location and credentials for a source about to be
/// persisted. Never logged or serialized — [`insert_encrypted`] and
/// [`update_encrypted`] are the only ways this leaves the caller's stack, and
/// both encrypt `access_key_id`/`secret_key` before anything touches the
/// database.
#[derive(Clone)]
pub struct S3SourceCredentials {
    pub name: String,
    pub bucket_name: String,
    pub bucket_path: String,
    pub access_key_id: String,
    pub secret_key: String,
    /// STS-style session token, when the credential is a temporary one.
    /// `None` for every operator-configured long-lived credential.
    pub session_token: Option<String>,
    /// When a temporary credential lapses. `None` for a long-lived one.
    pub credentials_expire_at: Option<DBDateTime>,
    pub region: String,
    pub endpoint: Option<String>,
    pub force_path_style: Option<bool>,
}

/// Hand-written so a stray `{:?}` — in a `tracing` field, an error string, a
/// test failure message — can never print the secret key or the session token.
/// The derived impl printed `secret_key` verbatim.
impl std::fmt::Debug for S3SourceCredentials {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("S3SourceCredentials")
            .field("name", &self.name)
            .field("bucket_name", &self.bucket_name)
            .field("bucket_path", &self.bucket_path)
            .field("access_key_id", &self.access_key_id)
            .field("secret_key", &"[REDACTED]")
            .field(
                "session_token",
                &self.session_token.as_ref().map(|_| "[REDACTED]"),
            )
            .field("credentials_expire_at", &self.credentials_expire_at)
            .field("region", &self.region)
            .field("endpoint", &self.endpoint)
            .field("force_path_style", &self.force_path_style)
            .finish()
    }
}

/// Failure encrypting, decrypting or persisting an [`S3SourceCredentials`]
/// value. Callers map this into their own domain error type (`temps-backup`'s
/// `BackupError`, `temps-cloud`'s `CloudServiceError`) — this crate is a data
/// access layer and does not own an HTTP-facing error type.
#[derive(Debug, thiserror::Error)]
pub enum S3SourceCredentialError {
    #[error("Failed to encrypt {field} for S3 source '{name}': {reason}")]
    Encryption {
        name: String,
        field: &'static str,
        reason: String,
    },
    #[error("Failed to decrypt {field} for S3 source '{name}': {reason}")]
    Decryption {
        name: String,
        field: &'static str,
        reason: String,
    },
    #[error("Database error while persisting S3 source '{name}': {source}")]
    Database { name: String, source: DbErr },
}

/// The three credential columns as they are stored: access key id, secret key,
/// and the optional session token — each encrypted with the same service, so
/// there is exactly one place where a credential field could be persisted in
/// plaintext by mistake.
struct EncryptedCredentialColumns {
    access_key_id: String,
    secret_key: String,
    session_token: Option<String>,
}

fn encrypt_credentials(
    encryption: &temps_core::EncryptionService,
    credentials: &S3SourceCredentials,
) -> Result<EncryptedCredentialColumns, S3SourceCredentialError> {
    let access_key_id = encryption
        .encrypt_string(&credentials.access_key_id)
        .map_err(|error| S3SourceCredentialError::Encryption {
            name: credentials.name.clone(),
            field: "access_key_id",
            reason: error.to_string(),
        })?;
    let secret_key = encryption
        .encrypt_string(&credentials.secret_key)
        .map_err(|error| S3SourceCredentialError::Encryption {
            name: credentials.name.clone(),
            field: "secret_key",
            reason: error.to_string(),
        })?;
    // `None` stays `None`: an operator-configured long-lived credential must
    // persist a NULL column, not an encrypted empty string, so that reading it
    // back yields "no session token" rather than "a session token that is the
    // empty string" (which would be signed and rejected by the provider).
    //
    // `Some("")` is normalised to the same NULL. Callers already filter it at
    // the wire boundary, but this is the DB-write boundary and the only place
    // that can guarantee the invariant regardless of what any caller does: an
    // *encrypted* empty string is a non-empty ciphertext column, so
    // `decrypt_session_token`'s own empty-string guard would not catch it and
    // every signer downstream would receive `Some("")`.
    let session_token = credentials
        .session_token
        .as_deref()
        .filter(|token| !token.is_empty())
        .map(|token| {
            encryption
                .encrypt_string(token)
                .map_err(|error| S3SourceCredentialError::Encryption {
                    name: credentials.name.clone(),
                    field: "session_token",
                    reason: error.to_string(),
                })
        })
        .transpose()?;
    Ok(EncryptedCredentialColumns {
        access_key_id,
        secret_key,
        session_token,
    })
}

/// Decrypt the optional session token stored on a row.
///
/// Returns `Ok(None)` for the overwhelmingly common case of a long-lived
/// operator-configured credential, so every caller can pass the result straight
/// into `aws_sdk_s3::config::Credentials::new`'s third argument (or omit
/// `AWS_SESSION_TOKEN`) without branching on which kind of source it holds.
pub fn decrypt_session_token(
    encryption: &temps_core::EncryptionService,
    model: &Model,
) -> Result<Option<String>, S3SourceCredentialError> {
    model
        .session_token
        .as_deref()
        .filter(|token| !token.is_empty())
        .map(|token| {
            encryption
                .decrypt_string(token)
                .map_err(|error| S3SourceCredentialError::Decryption {
                    name: model.name.clone(),
                    field: "session_token",
                    reason: error.to_string(),
                })
        })
        .transpose()
}

/// Encrypt `credentials` and insert a new row.
///
/// Shared by every call site that creates an S3 source — operator-provided
/// (`temps-backup::create_s3_source`) and Cloud-managed
/// (`temps-cloud::CloudService::enroll`) — so the credential-encryption call
/// site and the persisted row shape can never drift between the two.
pub async fn insert_encrypted<C: ConnectionTrait>(
    db: &C,
    encryption: &temps_core::EncryptionService,
    credentials: S3SourceCredentials,
    is_default: bool,
    managed_by_cloud: bool,
) -> Result<Model, S3SourceCredentialError> {
    let columns = encrypt_credentials(encryption, &credentials)?;
    let now = chrono::Utc::now();
    ActiveModel {
        id: sea_orm::ActiveValue::NotSet,
        name: Set(credentials.name.clone()),
        bucket_name: Set(credentials.bucket_name),
        bucket_path: Set(credentials.bucket_path),
        access_key_id: Set(columns.access_key_id),
        secret_key: Set(columns.secret_key),
        session_token: Set(columns.session_token),
        credentials_expire_at: Set(credentials.credentials_expire_at),
        region: Set(credentials.region),
        endpoint: Set(credentials.endpoint),
        force_path_style: Set(credentials.force_path_style),
        is_default: Set(is_default),
        managed_by_cloud: Set(managed_by_cloud),
        created_at: Set(now),
        updated_at: Set(now),
    }
    .insert(db)
    .await
    .map_err(|source| S3SourceCredentialError::Database {
        name: credentials.name,
        source,
    })
}

/// Encrypt `credentials` and overwrite an existing row's location and
/// credential fields in place, leaving `is_default` and `managed_by_cloud`
/// untouched. Used to rotate a Cloud-managed credential without losing the
/// row's id (and therefore any backup schedule already pointing at it).
///
/// `session_token` and `credentials_expire_at` are written unconditionally,
/// including when they are `None`. Rotation must be able to move a source from
/// a temporary credential back to a long-lived one without leaving a stale
/// token behind that would then be signed into every request.
pub async fn update_encrypted<C: ConnectionTrait>(
    db: &C,
    encryption: &temps_core::EncryptionService,
    id: i32,
    credentials: S3SourceCredentials,
) -> Result<Model, S3SourceCredentialError> {
    let columns = encrypt_credentials(encryption, &credentials)?;
    let active = ActiveModel {
        id: Set(id),
        name: Set(credentials.name.clone()),
        bucket_name: Set(credentials.bucket_name),
        bucket_path: Set(credentials.bucket_path),
        access_key_id: Set(columns.access_key_id),
        secret_key: Set(columns.secret_key),
        session_token: Set(columns.session_token),
        credentials_expire_at: Set(credentials.credentials_expire_at),
        region: Set(credentials.region),
        endpoint: Set(credentials.endpoint),
        force_path_style: Set(credentials.force_path_style),
        updated_at: Set(chrono::Utc::now()),
        ..Default::default()
    };
    active
        .update(db)
        .await
        .map_err(|source| S3SourceCredentialError::Database {
            name: credentials.name,
            source,
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn encryption() -> temps_core::EncryptionService {
        temps_core::EncryptionService::new_from_password("s3-source-session-token-tests")
    }

    fn long_lived_credentials() -> S3SourceCredentials {
        S3SourceCredentials {
            name: "operator-source".to_string(),
            bucket_name: "backups".to_string(),
            bucket_path: "prod".to_string(),
            access_key_id: "AKIAOPERATOR".to_string(),
            secret_key: "operator-secret".to_string(),
            session_token: None,
            credentials_expire_at: None,
            region: "us-east-1".to_string(),
            endpoint: None,
            force_path_style: Some(true),
        }
    }

    fn model_with_session_token(session_token: Option<String>) -> Model {
        let now = chrono::Utc::now();
        Model {
            id: 1,
            name: "source".to_string(),
            bucket_name: "backups".to_string(),
            region: "us-east-1".to_string(),
            endpoint: None,
            bucket_path: "prod".to_string(),
            access_key_id: "ciphertext".to_string(),
            secret_key: "ciphertext".to_string(),
            session_token,
            credentials_expire_at: None,
            force_path_style: Some(true),
            is_default: false,
            managed_by_cloud: false,
            created_at: now,
            updated_at: now,
        }
    }

    /// The load-bearing guarantee for the ~286 installs already running their
    /// own S3/R2/MinIO credentials: nothing about an operator-configured source
    /// changes, and no session token is invented for it.
    #[test]
    fn a_long_lived_credential_persists_a_null_session_token() {
        let columns =
            encrypt_credentials(&encryption(), &long_lived_credentials()).expect("encrypt");

        assert!(
            columns.session_token.is_none(),
            "an operator-configured source must store NULL, not an encrypted empty string"
        );
    }

    /// Distinct from the ciphertext-empty-string case below: here the
    /// *plaintext* is empty, which only a Cloud-vended wire payload could
    /// produce. Encrypting it would store a perfectly valid non-empty
    /// ciphertext, so `decrypt_session_token`'s empty-string guard would not
    /// catch it and every signer downstream would receive `Some("")` —
    /// `X-Amz-Security-Token: ` gets signed and the provider rejects it.
    #[test]
    fn an_empty_session_token_persists_as_null_rather_than_encrypted_emptiness() {
        let credentials = S3SourceCredentials {
            session_token: Some(String::new()),
            ..long_lived_credentials()
        };

        let columns = encrypt_credentials(&encryption(), &credentials).expect("encrypt");

        assert!(
            columns.session_token.is_none(),
            "an empty session token must store NULL, not the ciphertext of an empty string"
        );
    }

    #[test]
    fn a_temporary_credential_round_trips_its_session_token_through_encryption() {
        let encryption = encryption();
        let credentials = S3SourceCredentials {
            session_token: Some("sts-session-token".to_string()),
            ..long_lived_credentials()
        };

        let columns = encrypt_credentials(&encryption, &credentials).expect("encrypt");
        let stored = columns.session_token.expect("session token is stored");
        assert_ne!(
            stored, "sts-session-token",
            "the session token must be encrypted at rest, never stored in plaintext"
        );

        let decrypted = decrypt_session_token(&encryption, &model_with_session_token(Some(stored)))
            .expect("decrypt");
        assert_eq!(decrypted.as_deref(), Some("sts-session-token"));
    }

    #[test]
    fn decrypting_a_row_without_a_session_token_yields_none() {
        assert!(
            decrypt_session_token(&encryption(), &model_with_session_token(None))
                .expect("decrypt")
                .is_none()
        );
        // A legacy row could carry an empty string rather than NULL; that is
        // still "no session token", never an empty one to sign with.
        assert!(decrypt_session_token(
            &encryption(),
            &model_with_session_token(Some(String::new()))
        )
        .expect("decrypt")
        .is_none());
    }

    #[test]
    fn credentials_debug_output_redacts_both_secrets() {
        let credentials = S3SourceCredentials {
            secret_key: "operator-secret-value".to_string(),
            session_token: Some("sts-session-token".to_string()),
            ..long_lived_credentials()
        };

        let rendered = format!("{credentials:?}");
        assert!(!rendered.contains("operator-secret-value"));
        assert!(!rendered.contains("sts-session-token"));
        assert!(rendered.contains("AKIAOPERATOR"));
    }
}
