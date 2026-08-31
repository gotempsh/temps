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
#[derive(Debug, Clone)]
pub struct S3SourceCredentials {
    pub name: String,
    pub bucket_name: String,
    pub bucket_path: String,
    pub access_key_id: String,
    pub secret_key: String,
    pub region: String,
    pub endpoint: Option<String>,
    pub force_path_style: Option<bool>,
}

/// Failure encrypting or persisting an [`S3SourceCredentials`] value. Callers
/// map this into their own domain error type (`temps-backup`'s `BackupError`,
/// `temps-cloud`'s `CloudServiceError`) — this crate is a data-access layer
/// and does not own an HTTP-facing error type.
#[derive(Debug, thiserror::Error)]
pub enum S3SourceCredentialError {
    #[error("Failed to encrypt {field} for S3 source '{name}': {reason}")]
    Encryption {
        name: String,
        field: &'static str,
        reason: String,
    },
    #[error("Database error while persisting S3 source '{name}': {source}")]
    Database { name: String, source: DbErr },
}

fn encrypt_credentials(
    encryption: &temps_core::EncryptionService,
    credentials: &S3SourceCredentials,
) -> Result<(String, String), S3SourceCredentialError> {
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
    Ok((access_key_id, secret_key))
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
    let (access_key_id, secret_key) = encrypt_credentials(encryption, &credentials)?;
    let now = chrono::Utc::now();
    ActiveModel {
        id: sea_orm::ActiveValue::NotSet,
        name: Set(credentials.name.clone()),
        bucket_name: Set(credentials.bucket_name),
        bucket_path: Set(credentials.bucket_path),
        access_key_id: Set(access_key_id),
        secret_key: Set(secret_key),
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
pub async fn update_encrypted<C: ConnectionTrait>(
    db: &C,
    encryption: &temps_core::EncryptionService,
    id: i32,
    credentials: S3SourceCredentials,
) -> Result<Model, S3SourceCredentialError> {
    let (access_key_id, secret_key) = encrypt_credentials(encryption, &credentials)?;
    let active = ActiveModel {
        id: Set(id),
        name: Set(credentials.name.clone()),
        bucket_name: Set(credentials.bucket_name),
        bucket_path: Set(credentials.bucket_path),
        access_key_id: Set(access_key_id),
        secret_key: Set(secret_key),
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
