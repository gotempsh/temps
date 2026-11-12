// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Per-service server TLS setting and the certificate issued for it
//! (ADR 093).
//!
//! One optional row per service. **No row means `off`**, which is what every
//! service created before this table existed gets, so no backfill is needed.
//! The certificate columns are all set or all NULL (a CHECK constraint
//! enforces it): NULL until TLS is first enabled. `Debug` omits the key.

use async_trait::async_trait;
use sea_orm::entity::prelude::*;
use sea_orm::{ActiveValue::Set, ConnectionTrait, DbErr};
use temps_core::DBDateTime;

#[derive(Clone, PartialEq, DeriveEntityModel, Eq)]
#[sea_orm(table_name = "external_service_tls")]
pub struct Model {
    #[sea_orm(primary_key, auto_increment = false)]
    pub service_id: i32,
    /// `off` | `available` | `required`. `available` offers TLS and still
    /// accepts plaintext; `required` rejects plaintext. Defaults to `off`.
    pub mode: String,
    /// The CA that signed `certificate_pem`.
    pub authority_id: Option<i32>,
    /// Server certificate (PEM) currently issued for the service.
    pub certificate_pem: Option<String>,
    /// Server private key (PEM), encrypted with `EncryptionService`.
    pub private_key_encrypted: Option<String>,
    /// SHA-256 (lowercase hex) of the certificate's DER encoding.
    pub fingerprint_sha256: Option<String>,
    /// JSON array of the certificate's subject alternative names.
    pub subject_alt_names: Option<String>,
    pub not_before: Option<DBDateTime>,
    pub not_after: Option<DBDateTime>,
    pub created_at: DBDateTime,
    pub updated_at: DBDateTime,
}

impl std::fmt::Debug for Model {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ExternalServiceTls")
            .field("service_id", &self.service_id)
            .field("mode", &self.mode)
            .field("authority_id", &self.authority_id)
            .field("fingerprint_sha256", &self.fingerprint_sha256)
            .field("not_after", &self.not_after)
            .finish_non_exhaustive()
    }
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {
    #[sea_orm(
        belongs_to = "super::external_services::Entity",
        from = "Column::ServiceId",
        to = "super::external_services::Column::Id",
        on_update = "NoAction",
        on_delete = "Cascade"
    )]
    ExternalService,
    #[sea_orm(
        belongs_to = "super::managed_service_certificate_authorities::Entity",
        from = "Column::AuthorityId",
        to = "super::managed_service_certificate_authorities::Column::Id",
        on_update = "NoAction",
        on_delete = "Restrict"
    )]
    Authority,
}

impl Related<super::external_services::Entity> for Entity {
    fn to() -> RelationDef {
        Relation::ExternalService.def()
    }
}

impl Related<super::managed_service_certificate_authorities::Entity> for Entity {
    fn to() -> RelationDef {
        Relation::Authority.def()
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

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Utc;

    #[test]
    fn debug_never_prints_the_encrypted_key_or_certificate() {
        let row = Model {
            service_id: 7,
            mode: "available".into(),
            authority_id: Some(1),
            certificate_pem: Some("cert-sentinel".into()),
            private_key_encrypted: Some("key-sentinel".into()),
            fingerprint_sha256: Some("ab".into()),
            subject_alt_names: Some("[]".into()),
            not_before: Some(Utc::now()),
            not_after: Some(Utc::now()),
            created_at: Utc::now(),
            updated_at: Utc::now(),
        };
        let debug = format!("{row:?}");
        assert!(!debug.contains("key-sentinel"), "{debug}");
        assert!(!debug.contains("cert-sentinel"), "{debug}");
        assert!(debug.contains("available"));
    }
}
