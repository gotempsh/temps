// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Durable authority for authenticated worker boots. Registration is a host
//! operation, never a guest enrollment endpoint. Loading keys does not grant a
//! sandbox access; issuance must receive freshly authorized binding plans.
mod issue;
pub use issue::{AuthorizedBinding, IssuedPolicy};

use base64::{engine::general_purpose::STANDARD, Engine};
use ed25519_dalek::SigningKey;
use sea_orm::{ConnectionTrait, DatabaseBackend, DatabaseConnection, EntityTrait, Statement};
use std::sync::Arc;
use temps_core::{AuditLogger, AuditOperation, EncryptionService};
use temps_entities::sandbox_access_authorities as authorities;
use temps_service_access_runtime::{WorkerAuthority, WorkerIdentity};
use uuid::Uuid;
use zeroize::Zeroizing;

#[derive(Debug, thiserror::Error)]
pub enum AuthorityError {
    #[error("Cannot {operation} service-access authority for worker boot {boot_id}: {reason}")]
    Invalid {
        boot_id: Uuid,
        operation: &'static str,
        reason: &'static str,
    },
    #[error(
        "Cannot {operation} service-access authority for worker boot {boot_id}: database failed"
    )]
    Database {
        boot_id: Uuid,
        operation: &'static str,
        #[source]
        source: sea_orm::DbErr,
    },
    #[error("Cannot {operation} encrypted service-access key for worker boot {boot_id}")]
    Encryption {
        boot_id: Uuid,
        operation: &'static str,
        #[source]
        source: temps_core::EncryptionError,
    },
    #[error("Cannot generate service-access certificate for worker boot {boot_id}")]
    Certificate {
        boot_id: Uuid,
        #[source]
        source: rcgen::Error,
    },
    #[error("Cannot {operation} service-access policy for worker boot {boot_id}")]
    Sdk {
        boot_id: Uuid,
        operation: &'static str,
        #[source]
        source: tokenless_core::CoreError,
    },
    #[error("Cannot serialize sealed service-access headers for worker boot {boot_id}")]
    Encoding {
        boot_id: Uuid,
        #[source]
        source: serde_json::Error,
    },
}
fn invalid(boot_id: Uuid, operation: &'static str, reason: &'static str) -> AuthorityError {
    AuthorityError::Invalid {
        boot_id,
        operation,
        reason,
    }
}
fn database(boot_id: Uuid, operation: &'static str, source: sea_orm::DbErr) -> AuthorityError {
    AuthorityError::Database {
        boot_id,
        operation,
        source,
    }
}

pub struct AuthorityStore {
    db: Arc<DatabaseConnection>,
    encryption: Arc<EncryptionService>,
    audit: Arc<dyn AuditLogger>,
}

impl AuthorityStore {
    pub fn new(
        db: Arc<DatabaseConnection>,
        encryption: Arc<EncryptionService>,
        audit: Arc<dyn AuditLogger>,
    ) -> Self {
        Self {
            db,
            encryption,
            audit,
        }
    }

    /// `node_id` and `identity` must come from the authenticated owner transport.
    /// Once registered, a boot cannot move to another node or change its key.
    pub async fn register(
        &self,
        node_id: Option<i32>,
        identity: &WorkerIdentity,
    ) -> Result<WorkerAuthority, AuthorityError> {
        validate_identity(node_id, identity)?;
        if let Some(row) = authorities::Entity::find_by_id(identity.boot_id)
            .one(self.db.as_ref())
            .await
            .map_err(|source| database(identity.boot_id, "read registration", source))?
        {
            return self.public_authority(&row, node_id, identity);
        }
        let candidate = self.generate(node_id, identity)?;
        // A competing dispatcher may have won registration. Never replace its
        // keys: every dispatcher loads the single committed authority below.
        let inserted = self.db.execute(Statement::from_sql_and_values(DatabaseBackend::Postgres,
            "INSERT INTO sandbox_access_authorities (id,node_id,sdk_node_id,seal_public_key,snapshot_key_encrypted,session_key_encrypted,ca_certificate,ca_key_encrypted) VALUES ($1,$2,$3,$4,$5,$6,$7,$8) ON CONFLICT (id) DO NOTHING",
            [candidate.id.into(), candidate.node_id.into(), candidate.sdk_node_id.into(), candidate.seal_public_key.into(), candidate.snapshot_key_encrypted.into(), candidate.session_key_encrypted.into(), candidate.ca_certificate.into(), candidate.ca_key_encrypted.into()])).await
            .map_err(|source| database(identity.boot_id, "register worker", source))?;
        if inserted.rows_affected() == 1 {
            self.audit_change(identity.boot_id, node_id, "CREATE_SANDBOX_ACCESS_AUTHORITY")
                .await;
        }
        let row = authorities::Entity::find_by_id(identity.boot_id)
            .one(self.db.as_ref())
            .await
            .map_err(|source| database(identity.boot_id, "read winning registration", source))?
            .ok_or_else(|| {
                invalid(
                    identity.boot_id,
                    "read registration",
                    "registration disappeared",
                )
            })?;
        self.public_authority(&row, node_id, identity)
    }

    /// Stop signing future snapshots for a retired boot. Already issued leases
    /// remain bounded by the runtime lease; this is not a revocation receipt.
    pub async fn close(
        &self,
        node_id: Option<i32>,
        identity: &WorkerIdentity,
    ) -> Result<(), AuthorityError> {
        validate_identity(node_id, identity)?;
        let result = self.db.execute(Statement::from_sql_and_values(DatabaseBackend::Postgres,
            "UPDATE sandbox_access_authorities SET closed_at=COALESCE(closed_at,now()), updated_at=now() WHERE id=$1 AND node_id IS NOT DISTINCT FROM $2 AND seal_public_key=$3",
            [identity.boot_id.into(), node_id.into(), STANDARD.encode(identity.seal_public_key).into()])).await
            .map_err(|source| database(identity.boot_id, "close worker authority", source))?;
        if result.rows_affected() != 1 {
            return Err(invalid(
                identity.boot_id,
                "close worker authority",
                "registration is missing or identity changed",
            ));
        }
        self.audit_change(identity.boot_id, node_id, "CLOSE_SANDBOX_ACCESS_AUTHORITY")
            .await;
        Ok(())
    }

    // Reserve monotonically before signing. Failed issuance burns a version;
    // reuse after failure/restart would allow old snapshots to replace new ones.
    async fn reserve(
        &self,
        node_id: Option<i32>,
        identity: &WorkerIdentity,
        expected_revision: i64,
    ) -> Result<authorities::Model, AuthorityError> {
        validate_identity(node_id, identity)?;
        authorities::Entity::find().from_raw_sql(Statement::from_sql_and_values(DatabaseBackend::Postgres,
            "UPDATE sandbox_access_authorities SET snapshot_version=snapshot_version+1, updated_at=now() WHERE id=$1 AND node_id IS NOT DISTINCT FROM $2 AND seal_public_key=$3 AND closed_at IS NULL AND snapshot_version < 9223372036854775807 AND desired_revision=$4 RETURNING *",
            [identity.boot_id.into(), node_id.into(), STANDARD.encode(identity.seal_public_key).into(), expected_revision.into()])).one(self.db.as_ref()).await
            .map_err(|source| database(identity.boot_id, "reserve snapshot version", source))?
            .ok_or_else(|| invalid(identity.boot_id, "reserve snapshot version", "registration is missing, closed, exhausted or its identity or authorization revision changed"))
    }

    fn public_authority(
        &self,
        row: &authorities::Model,
        node_id: Option<i32>,
        identity: &WorkerIdentity,
    ) -> Result<WorkerAuthority, AuthorityError> {
        if row.id != identity.boot_id
            || row.node_id != node_id
            || row.closed_at.is_some()
            || row.seal_public_key != STANDARD.encode(identity.seal_public_key)
        {
            return Err(invalid(
                identity.boot_id,
                "load worker authority",
                "worker registration is closed or identity changed",
            ));
        }
        let key = self.signing_key(row.id, &row.snapshot_key_encrypted)?;
        Ok(WorkerAuthority {
            boot_id: row.id,
            node_id: row.sdk_node_id,
            snapshot_verify_key: key.verifying_key().to_bytes(),
        })
    }

    fn decrypt(
        &self,
        boot_id: Uuid,
        encrypted: &str,
    ) -> Result<Zeroizing<Vec<u8>>, AuthorityError> {
        self.encryption
            .decrypt(encrypted)
            .map(Zeroizing::new)
            .map_err(|source| AuthorityError::Encryption {
                boot_id,
                operation: "decrypt key",
                source,
            })
    }
    fn signing_key(&self, boot_id: Uuid, encrypted: &str) -> Result<SigningKey, AuthorityError> {
        let bytes = self.decrypt(boot_id, encrypted)?;
        let raw: &[u8; 32] = bytes.as_slice().try_into().map_err(|_| {
            invalid(
                boot_id,
                "decode signing key",
                "stored key must contain 32 bytes",
            )
        })?;
        Ok(SigningKey::from_bytes(raw))
    }
    fn generate(
        &self,
        node_id: Option<i32>,
        identity: &WorkerIdentity,
    ) -> Result<authorities::Model, AuthorityError> {
        let boot_id = identity.boot_id;
        let encrypt = |value: &[u8]| {
            self.encryption
                .encrypt(value)
                .map_err(|source| AuthorityError::Encryption {
                    boot_id,
                    operation: "persist key",
                    source,
                })
        };
        let snapshot_key = SigningKey::generate(&mut rand08::rngs::OsRng);
        let session_key = SigningKey::generate(&mut rand08::rngs::OsRng);
        let certificate = |source| AuthorityError::Certificate { boot_id, source };
        let ca_key = rcgen::KeyPair::generate().map_err(certificate)?;
        let mut params =
            rcgen::CertificateParams::new(Vec::<String>::new()).map_err(certificate)?;
        params.is_ca = rcgen::IsCa::Ca(rcgen::BasicConstraints::Unconstrained);
        params
            .distinguished_name
            .push(rcgen::DnType::CommonName, "Temps sandbox service access");
        params.key_usages = vec![
            rcgen::KeyUsagePurpose::KeyCertSign,
            rcgen::KeyUsagePurpose::CrlSign,
            rcgen::KeyUsagePurpose::DigitalSignature,
        ];
        let ca = params.self_signed(&ca_key).map_err(certificate)?;
        let now = chrono::Utc::now();
        Ok(authorities::Model {
            id: boot_id,
            node_id,
            sdk_node_id: Uuid::new_v4(),
            seal_public_key: STANDARD.encode(identity.seal_public_key),
            snapshot_key_encrypted: encrypt(snapshot_key.as_bytes())?,
            session_key_encrypted: encrypt(session_key.as_bytes())?,
            ca_certificate: STANDARD.encode(ca.der()),
            ca_key_encrypted: encrypt(&Zeroizing::new(ca_key.serialize_der()))?,
            snapshot_version: 0,
            desired_revision: 0,
            next_reconcile_at: None,
            closed_at: None,
            created_at: now,
            updated_at: now,
        })
    }

    async fn audit_change(&self, boot_id: Uuid, node_id: Option<i32>, operation: &'static str) {
        let audit = AuthorityAudit {
            boot_id,
            node_id,
            operation,
        };
        if let Err(error) = self.audit.create_audit_log(&audit).await {
            tracing::warn!(%boot_id, ?node_id, operation, %error, "Cannot audit sandbox service-access authority change");
        }
    }
}

fn validate_identity(
    node_id: Option<i32>,
    identity: &WorkerIdentity,
) -> Result<(), AuthorityError> {
    if identity.boot_id.is_nil() || node_id.is_some_and(|id| id <= 0) {
        return Err(invalid(
            identity.boot_id,
            "validate worker identity",
            "boot must be non-nil and node ID positive",
        ));
    }
    // Reject low-order X25519 public keys through the SDK's contributory check.
    tokenless_core::seal::seal(
        &tokenless_core::seal::SealPublicKey::from(identity.seal_public_key),
        b"",
        b"registration",
    )
    .map_err(|_| {
        invalid(
            identity.boot_id,
            "validate worker identity",
            "invalid sealing public key",
        )
    })?;
    Ok(())
}

#[derive(serde::Serialize)]
struct AuthorityAudit {
    boot_id: Uuid,
    node_id: Option<i32>,
    operation: &'static str,
}
impl AuditOperation for AuthorityAudit {
    fn operation_type(&self) -> String {
        self.operation.into()
    }
    fn user_id(&self) -> Option<i32> {
        None
    }
    fn ip_address(&self) -> Option<String> {
        None
    }
    fn user_agent(&self) -> &str {
        "temps-service-access"
    }
    fn serialize(&self) -> temps_core::AuditResult<String> {
        temps_core::serialize_audit_operation(self)
    }
}

#[cfg(test)]
mod tests;
