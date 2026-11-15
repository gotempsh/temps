// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Certificate authorities that sign managed-service server certificates
//! (ADR 093). One row per `purpose`; today only
//! [`MANAGED_SERVICES_PURPOSE`]. Internal: the encrypted key is never part of
//! a response or an audit payload, so `Debug` omits it.

use sea_orm::entity::prelude::*;
use temps_core::DBDateTime;

/// The `purpose` of the CA that signs every managed database's certificate.
pub const MANAGED_SERVICES_PURPOSE: &str = "managed-services";

#[derive(Clone, PartialEq, DeriveEntityModel, Eq)]
#[sea_orm(table_name = "managed_service_certificate_authorities")]
pub struct Model {
    #[sea_orm(primary_key)]
    pub id: i32,
    /// What this CA signs; unique.
    pub purpose: String,
    /// Public CA certificate (PEM): what clients trust.
    pub certificate_pem: String,
    /// CA private key (PEM), encrypted with `EncryptionService`.
    pub private_key_encrypted: String,
    /// SHA-256 (lowercase hex) of the certificate's DER encoding.
    pub fingerprint_sha256: String,
    pub not_before: DBDateTime,
    pub not_after: DBDateTime,
    pub created_at: DBDateTime,
}

impl std::fmt::Debug for Model {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ManagedServiceCertificateAuthority")
            .field("id", &self.id)
            .field("purpose", &self.purpose)
            .field("fingerprint_sha256", &self.fingerprint_sha256)
            .field("not_after", &self.not_after)
            .finish_non_exhaustive()
    }
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {}

impl ActiveModelBehavior for ActiveModel {}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Utc;

    #[test]
    fn debug_never_prints_the_encrypted_key() {
        let row = Model {
            id: 1,
            purpose: MANAGED_SERVICES_PURPOSE.into(),
            certificate_pem: "cert".into(),
            private_key_encrypted: "key-sentinel".into(),
            fingerprint_sha256: "ab".into(),
            not_before: Utc::now(),
            not_after: Utc::now(),
            created_at: Utc::now(),
        };
        let debug = format!("{row:?}");
        assert!(!debug.contains("key-sentinel"), "{debug}");
        assert!(debug.contains("managed-services"));
    }
}
