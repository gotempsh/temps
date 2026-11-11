// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Persistence for managed-service TLS (ADR 093): the managed-services CA and
//! the per-service mode and server certificate.
//!
//! The CA is created lazily, the first time a service needs a certificate,
//! and its key is encrypted at rest with `EncryptionService`. A service's
//! certificate is reused until it is close to expiry, the names it must carry
//! change, or the CA changes; then a new one is issued under a row lock so
//! two concurrent lifecycle operations cannot each deliver a different one.

use chrono::{DateTime, Utc};
use sea_orm::sea_query::OnConflict;
use sea_orm::{
    ActiveModelTrait, ColumnTrait, ConnectionTrait, DatabaseConnection, EntityTrait, QueryFilter,
    QuerySelect, Set, TransactionTrait,
};
use std::sync::Arc;
use temps_core::managed_service_pki::{self, ManagedPkiError};
use temps_core::EncryptionService;
use temps_entities::external_service_tls;
use temps_entities::managed_service_certificate_authorities::{
    self as authorities, MANAGED_SERVICES_PURPOSE,
};
use temps_providers_core::externalsvc::server_tls::{ServerTlsMaterial, ServerTlsMode};
use temps_providers_core::service_errors::ExternalServiceError;
use tracing::info;

/// Errors from managed-service TLS persistence.
#[derive(Debug, thiserror::Error)]
pub enum ManagedTlsError {
    #[error("Database error while managing TLS for {context}: {source}")]
    Database {
        context: String,
        #[source]
        source: sea_orm::DbErr,
    },
    #[error("Certificate operation failed for {context}: {source}")]
    Pki {
        context: String,
        #[source]
        source: ManagedPkiError,
    },
    #[error("Failed to {operation} for {context}: {reason}")]
    Encryption {
        operation: &'static str,
        context: String,
        reason: String,
    },
    #[error(
        "The managed-services CA (purpose '{purpose}') could not be read back after it was created"
    )]
    AuthorityMissing { purpose: String },
    #[error("External service {service_id} has an unknown stored TLS mode '{value}'")]
    InvalidStoredMode { service_id: i32, value: String },
    #[error("The TLS record of external service {service_id} is inconsistent: {reason}")]
    CorruptRecord { service_id: i32, reason: String },
}

type Result<T> = std::result::Result<T, ManagedTlsError>;

/// Errors from reading or changing a service's server TLS setting.
#[derive(Debug, thiserror::Error)]
pub enum ServerTlsError {
    #[error("External service {service_id} not found")]
    NotFound { service_id: i32 },
    #[error("Server TLS cannot be enabled on external service {service_id} ('{name}'): {reason}")]
    Unsupported {
        service_id: i32,
        name: String,
        reason: String,
    },
    #[error(
        "Server TLS mode '{mode}' is not supported yet for external service {service_id}: \
         rejecting plaintext needs every linked app and every internal Temps consumer \
         (backups, metrics, the data explorer) to connect with TLS first (ADR 093 Phase 2). \
         Use 'available' to offer TLS while still accepting plaintext"
    )]
    ModeNotYetSupported {
        service_id: i32,
        mode: ServerTlsMode,
    },
    #[error(
        "No managed-services CA exists yet (requested for external service {service_id}); \
         it is created the first time TLS is enabled on a service"
    )]
    NoAuthority { service_id: i32 },
    #[error(
        "External service {service_id} ('{name}') is {status}: its TLS mode can be changed \
         once it is running or stopped, so the change cannot race its start"
    )]
    Busy {
        service_id: i32,
        name: String,
        status: String,
    },
    #[error(
        "Failed to apply server TLS mode '{mode}' to external service {service_id}; \
         restoring the previous mode '{previous}': {rollback}. The failure: {source}"
    )]
    Apply {
        service_id: i32,
        mode: ServerTlsMode,
        previous: ServerTlsMode,
        /// What became of the database: whether it serves `previous` again.
        rollback: String,
        /// Whether `previous` is recorded again. When it is not, `mode`
        /// stays recorded, and the change is audited.
        previous_recorded: bool,
        #[source]
        source: Box<ExternalServiceError>,
    },
    #[error(
        "Server TLS mode '{mode}' was not applied to external service {service_id} \
         ('{name}'): it restarted on its existing container, which the recreate guard \
         kept in place to protect its data (the service's logs say why), so it still \
         serves '{previous}'; {rollback}"
    )]
    NotApplied {
        service_id: i32,
        name: String,
        mode: ServerTlsMode,
        previous: ServerTlsMode,
        /// Whether `previous` is recorded again, in words.
        rollback: String,
        /// Whether `previous` is recorded again.
        previous_recorded: bool,
    },
    #[error(
        "Failed to apply server TLS mode '{mode}' to cluster service {service_id}; \
         {rollback}: {failures}"
    )]
    ClusterApply {
        service_id: i32,
        mode: ServerTlsMode,
        previous: ServerTlsMode,
        /// One `member: reason` entry per data node that did not converge.
        failures: String,
        /// What became of the setting: whether `previous` is recorded again.
        rollback: String,
        /// Whether `previous` is recorded again.
        previous_recorded: bool,
    },
    #[error(
        "The server TLS mode of external service {service_id} was changed from '{previous}' \
         to '{mode}', but reading its resulting TLS status failed: {source}"
    )]
    StatusUnavailable {
        service_id: i32,
        mode: ServerTlsMode,
        previous: ServerTlsMode,
        #[source]
        source: Box<ServerTlsError>,
    },
    #[error(
        "Changing the server TLS mode of external service {service_id} to '{mode}' did not \
         finish: {reason}; the TLS status shows what it serves"
    )]
    ChangeAborted {
        service_id: i32,
        mode: ServerTlsMode,
        reason: String,
    },
    #[error(transparent)]
    Service(#[from] ExternalServiceError),
    #[error(transparent)]
    Storage(#[from] ManagedTlsError),
}

impl ServerTlsError {
    /// `(previous, recorded)` when this failure still left a different mode
    /// recorded than before the change: the change was written, so it must
    /// be audited like a successful one.
    pub fn recorded_change(&self) -> Option<(ServerTlsMode, ServerTlsMode)> {
        match self {
            Self::StatusUnavailable { previous, mode, .. }
            | Self::Apply {
                previous,
                mode,
                previous_recorded: false,
                ..
            }
            | Self::NotApplied {
                previous,
                mode,
                previous_recorded: false,
                ..
            }
            | Self::ClusterApply {
                previous,
                mode,
                previous_recorded: false,
                ..
            } => (previous != mode).then_some((*previous, *mode)),
            _ => None,
        }
    }
}

/// What the running container does about TLS.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ContainerTlsState {
    /// Not inspectable from here: not created yet, on a worker node, or this
    /// process has no Docker daemon.
    Unknown,
    /// The container does not offer TLS.
    Off,
    /// The container offers TLS with the certificate of this fingerprint
    /// (`None`: created before the fingerprint was recorded).
    On { fingerprint_sha256: Option<String> },
    /// The container offers TLS with a certificate Temps did not issue for
    /// it: a cluster data node's pg_auto_failover self-signed certificate,
    /// which clients cannot verify.
    Unmanaged { fingerprint_sha256: String },
}

impl ContainerTlsState {
    /// Whether this state lags `mode` (with `issued` the fingerprint of the
    /// service's current certificate). Unknown never lags: nothing to act on.
    pub fn lags(&self, mode: ServerTlsMode, issued: Option<&str>) -> bool {
        match (self, mode.offers_tls()) {
            (ContainerTlsState::Unknown, _) => false,
            (ContainerTlsState::Off | ContainerTlsState::Unmanaged { .. }, wants) => wants,
            (ContainerTlsState::On { .. }, false) => true,
            (ContainerTlsState::On { fingerprint_sha256 }, true) => {
                fingerprint_sha256.as_deref() != issued
            }
        }
    }

    /// Whether the managed certificate is being served: `None` when unknown.
    pub fn serves_managed_certificate(&self) -> Option<bool> {
        match self {
            ContainerTlsState::Unknown => None,
            ContainerTlsState::Off | ContainerTlsState::Unmanaged { .. } => Some(false),
            ContainerTlsState::On { .. } => Some(true),
        }
    }

    /// The state of a cluster data node from the fingerprint it serves.
    pub fn from_served_fingerprint(served: &str, issued: Option<&str>) -> Self {
        if served.is_empty() {
            ContainerTlsState::Off
        } else if Some(served) == issued {
            ContainerTlsState::On {
                fingerprint_sha256: Some(served.to_string()),
            }
        } else {
            ContainerTlsState::Unmanaged {
                fingerprint_sha256: served.to_string(),
            }
        }
    }
}

/// TLS state of one data node of a cluster.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MemberTlsState {
    pub container_name: String,
    /// The member's lifecycle status (`running`, `stopped`, ...).
    pub status: String,
    pub tls: ContainerTlsState,
    /// Why the state is unknown, when it is.
    pub detail: Option<String>,
}

/// A service's effective server TLS status, with no secrets.
#[derive(Debug, Clone)]
pub struct ServiceTlsStatus {
    pub service_id: i32,
    pub mode: ServerTlsMode,
    /// `None` when the service supports managed TLS.
    pub unsupported_reason: Option<String>,
    pub certificate: Option<IssuedCertificateInfo>,
    pub authority: Option<AuthorityInfo>,
    /// The standalone container, or for a cluster the state every data
    /// node agrees on (`Unknown` when they differ or none could be read).
    pub container: ContainerTlsState,
    /// Whether the service is an HA cluster: the setting is applied to its
    /// running data nodes in place, with no restart.
    pub cluster: bool,
    /// A cluster's data nodes; empty for a standalone service.
    pub members: Vec<MemberTlsState>,
}

impl ServiceTlsStatus {
    fn issued_fingerprint(&self) -> Option<&str> {
        self.certificate
            .as_ref()
            .map(|c| c.fingerprint_sha256.as_str())
    }

    /// Whether what runs lags the setting: TLS asked for but not offered (or
    /// offered with an older certificate), or offered when off. A
    /// standalone container catches up when it is recreated; a cluster's
    /// data nodes when the setting is applied again (at the next start).
    pub fn pending_restart(&self) -> bool {
        let issued = self.issued_fingerprint();
        if self.cluster {
            return self
                .members
                .iter()
                .any(|member| member.tls.lags(self.mode, issued));
        }
        self.container.lags(self.mode, issued)
    }

    /// The data nodes that lag the setting.
    pub fn lagging_members(&self) -> Vec<&MemberTlsState> {
        let issued = self.issued_fingerprint();
        self.members
            .iter()
            .filter(|member| member.tls.lags(self.mode, issued))
            .collect()
    }
}

/// The state every member agrees on, or `Unknown`.
pub fn agreed_member_state(members: &[MemberTlsState]) -> ContainerTlsState {
    let Some(first) = members.first() else {
        return ContainerTlsState::Unknown;
    };
    if members.iter().all(|member| member.tls == first.tls) {
        first.tls.clone()
    } else {
        ContainerTlsState::Unknown
    }
}

/// Public facts about the managed-services CA.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AuthorityInfo {
    pub fingerprint_sha256: String,
    pub not_before: DateTime<Utc>,
    pub not_after: DateTime<Utc>,
}

impl From<&authorities::Model> for AuthorityInfo {
    fn from(row: &authorities::Model) -> Self {
        Self {
            fingerprint_sha256: row.fingerprint_sha256.clone(),
            not_before: row.not_before,
            not_after: row.not_after,
        }
    }
}

/// The CA certificate, for download.
#[derive(Debug, Clone)]
pub struct AuthorityCertificate {
    pub certificate_pem: String,
    pub info: AuthorityInfo,
}

fn db_error(context: impl Into<String>) -> impl FnOnce(sea_orm::DbErr) -> ManagedTlsError {
    let context = context.into();
    move |source| ManagedTlsError::Database { context, source }
}

/// The CA with its decrypted key. Never leaves the control plane.
pub struct AuthorityMaterial {
    pub record: authorities::Model,
    pub key_pem: String,
}

/// Public facts about the certificate issued for a service.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IssuedCertificateInfo {
    pub fingerprint_sha256: String,
    pub subject_alt_names: Vec<String>,
    pub not_before: DateTime<Utc>,
    pub not_after: DateTime<Utc>,
    pub authority_id: i32,
}

/// What the TLS record of a service says, with no secrets.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ServiceTlsRecord {
    pub mode: ServerTlsMode,
    pub certificate: Option<IssuedCertificateInfo>,
}

/// Managed-service TLS persistence. A stateless wrapper over the shared
/// database connection and encryption service.
#[derive(Clone)]
pub struct ManagedTlsService {
    db: Arc<DatabaseConnection>,
    encryption: Arc<EncryptionService>,
}

impl ManagedTlsService {
    pub fn new(db: Arc<DatabaseConnection>, encryption: Arc<EncryptionService>) -> Self {
        Self { db, encryption }
    }

    /// The managed-services CA, if one has been created. Public material only.
    pub async fn authority(&self) -> Result<Option<authorities::Model>> {
        authorities::Entity::find()
            .filter(authorities::Column::Purpose.eq(MANAGED_SERVICES_PURPOSE))
            .one(self.db.as_ref())
            .await
            .map_err(db_error("the managed-services CA"))
    }

    /// Return the managed-services CA, creating it on first use. Concurrent
    /// callers converge on the single row the unique `purpose` index admits.
    pub async fn ensure_authority(&self) -> Result<AuthorityMaterial> {
        if let Some(record) = self.authority().await? {
            return self.decrypt_authority(record);
        }
        let ca =
            managed_service_pki::generate_managed_services_ca(Utc::now()).map_err(|source| {
                ManagedTlsError::Pki {
                    context: "the managed-services CA".into(),
                    source,
                }
            })?;
        let key_encrypted = self.encryption.encrypt_string(&ca.key_pem).map_err(|e| {
            ManagedTlsError::Encryption {
                operation: "encrypt the CA key",
                context: "the managed-services CA".into(),
                reason: e.to_string(),
            }
        })?;
        let candidate = authorities::ActiveModel {
            purpose: Set(MANAGED_SERVICES_PURPOSE.to_string()),
            certificate_pem: Set(ca.cert_pem),
            private_key_encrypted: Set(key_encrypted),
            fingerprint_sha256: Set(ca.fingerprint_sha256.clone()),
            not_before: Set(ca.not_before),
            not_after: Set(ca.not_after),
            created_at: Set(Utc::now()),
            ..Default::default()
        };
        authorities::Entity::insert(candidate)
            .on_conflict(
                OnConflict::column(authorities::Column::Purpose)
                    .do_nothing()
                    .to_owned(),
            )
            .do_nothing()
            .exec(self.db.as_ref())
            .await
            .map_err(db_error("creating the managed-services CA"))?;
        let record = self
            .authority()
            .await?
            .ok_or_else(|| ManagedTlsError::AuthorityMissing {
                purpose: MANAGED_SERVICES_PURPOSE.into(),
            })?;
        if record.fingerprint_sha256 == ca.fingerprint_sha256 {
            info!(
                fingerprint = %record.fingerprint_sha256,
                not_after = %record.not_after,
                "Created the managed-services certificate authority"
            );
        }
        self.decrypt_authority(record)
    }

    fn decrypt_authority(&self, record: authorities::Model) -> Result<AuthorityMaterial> {
        let key_pem = self
            .encryption
            .decrypt_string(&record.private_key_encrypted)
            .map_err(|e| ManagedTlsError::Encryption {
                operation: "decrypt the CA key",
                context: format!("the managed-services CA {}", record.id),
                reason: e.to_string(),
            })?;
        Ok(AuthorityMaterial { record, key_pem })
    }

    /// The TLS record of a service. A service without a row is `off`.
    pub async fn record(&self, service_id: i32) -> Result<ServiceTlsRecord> {
        let row = external_service_tls::Entity::find_by_id(service_id)
            .one(self.db.as_ref())
            .await
            .map_err(db_error(format!("external service {service_id}")))?;
        match row {
            None => Ok(ServiceTlsRecord {
                mode: ServerTlsMode::Off,
                certificate: None,
            }),
            Some(row) => Ok(ServiceTlsRecord {
                mode: parse_mode(service_id, &row.mode)?,
                certificate: certificate_info(&row)?,
            }),
        }
    }

    /// Store `mode` for a service, returning the mode it had before.
    pub async fn set_mode(&self, service_id: i32, mode: ServerTlsMode) -> Result<ServerTlsMode> {
        set_mode_on(self.db.as_ref(), service_id, mode).await
    }

    /// The certificate and key `service_id` should serve, issuing (or
    /// re-issuing) one when there is none, it expires within the renewal
    /// window, its names differ from `subject_alt_names`, or it was signed by
    /// a different CA.
    pub async fn ensure_server_certificate(
        &self,
        service_id: i32,
        common_name: &str,
        subject_alt_names: &[String],
    ) -> Result<ServerTlsMaterial> {
        let authority = self.ensure_authority().await?;
        let wanted = managed_service_pki::normalize_subject_alt_names(subject_alt_names);
        let context = format!("external service {service_id}");
        let txn = self.db.begin().await.map_err(db_error(context.clone()))?;
        let row = external_service_tls::Entity::find_by_id(service_id)
            .lock_exclusive()
            .one(&txn)
            .await
            .map_err(db_error(context.clone()))?;
        let row = match row {
            Some(row) => row,
            None => {
                // The mode is written first by every caller; a missing row
                // means TLS was never turned on, so record it as `off` and
                // still issue: the caller asked for material explicitly.
                external_service_tls::ActiveModel {
                    service_id: Set(service_id),
                    mode: Set(ServerTlsMode::Off.as_str().to_string()),
                    ..Default::default()
                }
                .insert(&txn)
                .await
                .map_err(db_error(context.clone()))?
            }
        };

        if let Some(existing) = reusable_material(
            &row,
            authority.record.id,
            &wanted,
            Utc::now(),
            &self.encryption,
        )? {
            txn.commit().await.map_err(db_error(context))?;
            return Ok(existing);
        }

        let issued = managed_service_pki::issue_server_certificate(
            &authority.record.certificate_pem,
            &authority.key_pem,
            common_name,
            &wanted,
            Utc::now(),
        )
        .map_err(|source| ManagedTlsError::Pki {
            context: context.clone(),
            source,
        })?;
        let key_encrypted = self
            .encryption
            .encrypt_string(&issued.key_pem)
            .map_err(|e| ManagedTlsError::Encryption {
                operation: "encrypt the server key",
                context: context.clone(),
                reason: e.to_string(),
            })?;
        let sans_json = serde_json::to_string(&issued.subject_alt_names).map_err(|e| {
            ManagedTlsError::CorruptRecord {
                service_id,
                reason: format!("cannot encode subject alternative names: {e}"),
            }
        })?;
        let mut update: external_service_tls::ActiveModel = row.into();
        update.authority_id = Set(Some(authority.record.id));
        update.certificate_pem = Set(Some(issued.cert_pem.clone()));
        update.private_key_encrypted = Set(Some(key_encrypted));
        update.fingerprint_sha256 = Set(Some(issued.fingerprint_sha256.clone()));
        update.subject_alt_names = Set(Some(sans_json));
        update.not_before = Set(Some(issued.not_before));
        update.not_after = Set(Some(issued.not_after));
        update
            .update(&txn)
            .await
            .map_err(db_error(context.clone()))?;
        txn.commit().await.map_err(db_error(context))?;
        info!(
            service_id,
            fingerprint = %issued.fingerprint_sha256,
            not_after = %issued.not_after,
            "Issued a server TLS certificate for managed service"
        );
        Ok(ServerTlsMaterial {
            cert_pem: issued.cert_pem,
            key_pem: issued.key_pem,
            fingerprint_sha256: issued.fingerprint_sha256,
        })
    }
}

/// Upsert the mode of `service_id` on `db` (a connection or a transaction),
/// returning the previous mode (`off` when there was no row).
pub async fn set_mode_on<C: ConnectionTrait>(
    db: &C,
    service_id: i32,
    mode: ServerTlsMode,
) -> Result<ServerTlsMode> {
    let context = format!("external service {service_id}");
    let previous = external_service_tls::Entity::find_by_id(service_id)
        .one(db)
        .await
        .map_err(db_error(context.clone()))?;
    match previous {
        Some(row) => {
            let previous_mode = parse_mode(service_id, &row.mode)?;
            let mut update: external_service_tls::ActiveModel = row.into();
            update.mode = Set(mode.as_str().to_string());
            update.update(db).await.map_err(db_error(context))?;
            Ok(previous_mode)
        }
        None => {
            external_service_tls::ActiveModel {
                service_id: Set(service_id),
                mode: Set(mode.as_str().to_string()),
                ..Default::default()
            }
            .insert(db)
            .await
            .map_err(db_error(context))?;
            Ok(ServerTlsMode::Off)
        }
    }
}

fn parse_mode(service_id: i32, value: &str) -> Result<ServerTlsMode> {
    value
        .parse()
        .map_err(|_| ManagedTlsError::InvalidStoredMode {
            service_id,
            value: value.to_string(),
        })
}

fn stored_sans(row: &external_service_tls::Model) -> Result<Option<Vec<String>>> {
    row.subject_alt_names
        .as_deref()
        .map(|json| {
            serde_json::from_str::<Vec<String>>(json).map_err(|e| ManagedTlsError::CorruptRecord {
                service_id: row.service_id,
                reason: format!("subject_alt_names is not a JSON array of strings: {e}"),
            })
        })
        .transpose()
}

fn certificate_info(row: &external_service_tls::Model) -> Result<Option<IssuedCertificateInfo>> {
    let (Some(fingerprint), Some(not_before), Some(not_after), Some(authority_id)) = (
        row.fingerprint_sha256.clone(),
        row.not_before,
        row.not_after,
        row.authority_id,
    ) else {
        return Ok(None);
    };
    Ok(Some(IssuedCertificateInfo {
        fingerprint_sha256: fingerprint,
        subject_alt_names: stored_sans(row)?.unwrap_or_default(),
        not_before,
        not_after,
        authority_id,
    }))
}

/// The stored certificate and decrypted key when they can be served as they
/// are: same CA, same names (order-insensitive), outside the renewal window.
fn reusable_material(
    row: &external_service_tls::Model,
    authority_id: i32,
    wanted_sans: &[String],
    now: DateTime<Utc>,
    encryption: &EncryptionService,
) -> Result<Option<ServerTlsMaterial>> {
    let (Some(cert_pem), Some(key_encrypted), Some(fingerprint), Some(not_after)) = (
        row.certificate_pem.as_ref(),
        row.private_key_encrypted.as_ref(),
        row.fingerprint_sha256.as_ref(),
        row.not_after,
    ) else {
        return Ok(None);
    };
    if row.authority_id != Some(authority_id)
        || managed_service_pki::needs_renewal(not_after, now)
        || !same_names(&stored_sans(row)?.unwrap_or_default(), wanted_sans)
    {
        return Ok(None);
    }
    let key_pem =
        encryption
            .decrypt_string(key_encrypted)
            .map_err(|e| ManagedTlsError::Encryption {
                operation: "decrypt the server key",
                context: format!("external service {}", row.service_id),
                reason: e.to_string(),
            })?;
    Ok(Some(ServerTlsMaterial {
        cert_pem: cert_pem.clone(),
        key_pem,
        fingerprint_sha256: fingerprint.clone(),
    }))
}

fn same_names(a: &[String], b: &[String]) -> bool {
    let normalize = |names: &[String]| {
        let mut out: Vec<String> = names.iter().map(|n| n.to_ascii_lowercase()).collect();
        out.sort();
        out.dedup();
        out
    };
    normalize(a) == normalize(b)
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Duration;
    use sea_orm::{DatabaseBackend, MockDatabase};

    /// Only a failure that left a different mode recorded counts as a
    /// change to audit.
    #[test]
    fn a_failure_is_a_recorded_change_only_when_the_new_mode_stays() {
        use ServerTlsMode::{Available, Off};
        let source = || Box::new(ExternalServiceError::ServiceNotFound { id: 7 });
        let apply = |previous_recorded| ServerTlsError::Apply {
            service_id: 7,
            mode: Available,
            previous: Off,
            rollback: String::new(),
            previous_recorded,
            source: source(),
        };
        assert_eq!(apply(true).recorded_change(), None);
        assert_eq!(apply(false).recorded_change(), Some((Off, Available)));

        let not_applied = |previous_recorded| ServerTlsError::NotApplied {
            service_id: 7,
            name: "orders".into(),
            mode: Available,
            previous: Off,
            rollback: String::new(),
            previous_recorded,
        };
        assert_eq!(not_applied(true).recorded_change(), None);
        assert_eq!(not_applied(false).recorded_change(), Some((Off, Available)));

        let cluster = |previous_recorded| ServerTlsError::ClusterApply {
            service_id: 7,
            mode: Available,
            previous: Off,
            failures: "postgres-orders-1: exit 5".into(),
            rollback: String::new(),
            previous_recorded,
        };
        assert_eq!(cluster(true).recorded_change(), None);
        assert_eq!(cluster(false).recorded_change(), Some((Off, Available)));

        let unavailable = |previous| ServerTlsError::StatusUnavailable {
            service_id: 7,
            mode: Available,
            previous,
            source: Box::new(ServerTlsError::NotFound { service_id: 7 }),
        };
        assert_eq!(unavailable(Off).recorded_change(), Some((Off, Available)));
        // Re-selecting the same mode is no change.
        assert_eq!(unavailable(Available).recorded_change(), None);

        // Failures before anything was written are not changes.
        assert_eq!(
            ServerTlsError::NotFound { service_id: 7 }.recorded_change(),
            None
        );
        assert_eq!(
            ServerTlsError::ChangeAborted {
                service_id: 7,
                mode: Available,
                reason: "task 3 panicked".into(),
            }
            .recorded_change(),
            None
        );
    }

    fn encryption() -> Arc<EncryptionService> {
        Arc::new(EncryptionService::new_from_password("managed-tls-test"))
    }

    fn ca_row(encryption: &EncryptionService) -> (authorities::Model, String) {
        let ca = managed_service_pki::generate_managed_services_ca(Utc::now()).unwrap();
        (
            authorities::Model {
                id: 1,
                purpose: MANAGED_SERVICES_PURPOSE.into(),
                certificate_pem: ca.cert_pem.clone(),
                private_key_encrypted: encryption.encrypt_string(&ca.key_pem).unwrap(),
                fingerprint_sha256: ca.fingerprint_sha256,
                not_before: ca.not_before,
                not_after: ca.not_after,
                created_at: Utc::now(),
            },
            ca.key_pem,
        )
    }

    fn tls_row(service_id: i32, mode: &str) -> external_service_tls::Model {
        external_service_tls::Model {
            service_id,
            mode: mode.into(),
            authority_id: None,
            certificate_pem: None,
            private_key_encrypted: None,
            fingerprint_sha256: None,
            subject_alt_names: None,
            not_before: None,
            not_after: None,
            created_at: Utc::now(),
            updated_at: Utc::now(),
        }
    }

    fn issued_row(
        encryption: &EncryptionService,
        ca: &authorities::Model,
        ca_key: &str,
        sans: &[String],
        now: DateTime<Utc>,
    ) -> (external_service_tls::Model, String) {
        let leaf = managed_service_pki::issue_server_certificate(
            &ca.certificate_pem,
            ca_key,
            "postgres-orders",
            sans,
            now,
        )
        .unwrap();
        let mut row = tls_row(7, "available");
        row.authority_id = Some(ca.id);
        row.certificate_pem = Some(leaf.cert_pem);
        row.private_key_encrypted = Some(encryption.encrypt_string(&leaf.key_pem).unwrap());
        row.fingerprint_sha256 = Some(leaf.fingerprint_sha256);
        row.subject_alt_names = Some(serde_json::to_string(&leaf.subject_alt_names).unwrap());
        row.not_before = Some(leaf.not_before);
        row.not_after = Some(leaf.not_after);
        (row, leaf.key_pem)
    }

    fn sans() -> Vec<String> {
        vec![
            "postgres-orders".into(),
            "localhost".into(),
            "127.0.0.1".into(),
        ]
    }

    #[tokio::test]
    async fn the_ca_key_round_trips_through_encryption_at_rest() {
        let encryption = encryption();
        let (row, key_pem) = ca_row(&encryption);
        assert!(!row.private_key_encrypted.contains("PRIVATE KEY"));
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results([vec![row.clone()]])
            .into_connection();
        let service = ManagedTlsService::new(Arc::new(db), encryption);
        let material = service.ensure_authority().await.unwrap();
        assert_eq!(material.key_pem, key_pem);
        assert_eq!(material.record.fingerprint_sha256, row.fingerprint_sha256);
    }

    #[tokio::test]
    async fn the_ca_is_created_lazily_and_read_back_from_the_winning_row() {
        let encryption = encryption();
        let (winner, winner_key) = ca_row(&encryption);
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            // No CA yet.
            .append_query_results([Vec::<authorities::Model>::new()])
            // The insert (`RETURNING` on Postgres): no row, another request won.
            .append_query_results([Vec::<authorities::Model>::new()])
            // Read back: another request's CA won the race.
            .append_query_results([vec![winner.clone()]])
            .into_connection();
        let service = ManagedTlsService::new(Arc::new(db), encryption);
        let material = service.ensure_authority().await.unwrap();
        assert_eq!(material.record.id, winner.id);
        assert_eq!(
            material.key_pem, winner_key,
            "every caller gets the winner's key"
        );
    }

    #[tokio::test]
    async fn a_ca_key_encrypted_under_another_key_is_reported_not_returned() {
        let (row, _) = ca_row(&EncryptionService::new_from_password("another-instance"));
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results([vec![row]])
            .into_connection();
        let service = ManagedTlsService::new(Arc::new(db), encryption());
        let err = service.ensure_authority().await.err().unwrap();
        assert!(
            matches!(
                err,
                ManagedTlsError::Encryption {
                    operation: "decrypt the CA key",
                    ..
                }
            ),
            "{err}"
        );
    }

    #[tokio::test]
    async fn a_service_without_a_tls_row_is_off() {
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results([Vec::<external_service_tls::Model>::new()])
            .into_connection();
        let service = ManagedTlsService::new(Arc::new(db), encryption());
        let record = service.record(7).await.unwrap();
        assert_eq!(record.mode, ServerTlsMode::Off);
        assert!(record.certificate.is_none());
    }

    #[tokio::test]
    async fn an_unknown_stored_mode_is_an_error_naming_the_service() {
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results([vec![tls_row(7, "sometimes")]])
            .into_connection();
        let service = ManagedTlsService::new(Arc::new(db), encryption());
        let err = service.record(7).await.unwrap_err();
        assert!(matches!(
            err,
            ManagedTlsError::InvalidStoredMode { service_id: 7, .. }
        ));
        assert!(err.to_string().contains("sometimes"));
    }

    #[tokio::test]
    async fn setting_a_mode_reports_the_previous_one() {
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results([Vec::<external_service_tls::Model>::new()])
            .append_query_results([vec![tls_row(7, "available")]])
            .into_connection();
        let service = ManagedTlsService::new(Arc::new(db), encryption());
        assert_eq!(
            service.set_mode(7, ServerTlsMode::Available).await.unwrap(),
            ServerTlsMode::Off
        );
    }

    #[tokio::test]
    async fn a_database_error_carries_the_service_id() {
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_errors([sea_orm::DbErr::Custom("connection reset".into())])
            .into_connection();
        let service = ManagedTlsService::new(Arc::new(db), encryption());
        let err = service.record(42).await.unwrap_err();
        assert!(matches!(err, ManagedTlsError::Database { .. }));
        assert!(err.to_string().contains("external service 42"), "{err}");
    }

    #[test]
    fn a_valid_certificate_is_reused_with_its_decrypted_key() {
        let encryption = encryption();
        let (ca, ca_key) = ca_row(&encryption);
        let now = Utc::now();
        let (row, key) = issued_row(&encryption, &ca, &ca_key, &sans(), now);
        let reversed: Vec<String> = sans().into_iter().rev().collect();
        let material = reusable_material(&row, ca.id, &reversed, now, &encryption)
            .unwrap()
            .expect("same names in another order are the same certificate");
        assert_eq!(material.key_pem, key);
        assert_eq!(Some(material.fingerprint_sha256), row.fingerprint_sha256);
    }

    #[test]
    fn a_certificate_is_reissued_near_expiry_on_new_names_or_a_new_ca() {
        let encryption = encryption();
        let (ca, ca_key) = ca_row(&encryption);
        let now = Utc::now();
        let (row, _) = issued_row(&encryption, &ca, &ca_key, &sans(), now);

        let near_expiry =
            now + Duration::days(managed_service_pki::SERVER_CERTIFICATE_VALIDITY_DAYS - 20);
        assert!(
            reusable_material(&row, ca.id, &sans(), near_expiry, &encryption)
                .unwrap()
                .is_none()
        );

        let mut renamed = sans();
        renamed.push("orders.temps.local".into());
        assert!(reusable_material(&row, ca.id, &renamed, now, &encryption)
            .unwrap()
            .is_none());

        assert!(
            reusable_material(&row, ca.id + 1, &sans(), now, &encryption)
                .unwrap()
                .is_none()
        );

        assert!(
            reusable_material(&tls_row(7, "available"), ca.id, &sans(), now, &encryption)
                .unwrap()
                .is_none()
        );
    }

    #[test]
    fn corrupt_subject_alt_names_are_reported() {
        let mut row = tls_row(9, "available");
        row.subject_alt_names = Some("not json".into());
        let err = stored_sans(&row).unwrap_err();
        assert!(matches!(
            err,
            ManagedTlsError::CorruptRecord { service_id: 9, .. }
        ));
    }
}
