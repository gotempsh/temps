// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Bounded authorization reads for one worker. Saved intentions never become
//! live policy: only fresh consent tied to the current incarnation is eligible.
mod permissions;
mod read;

use crate::{
    authority::AuthorizedBinding,
    tokenless::{BindingTarget, PreparationError, PrepareSession, PreparedBinding},
    types::SandboxAccessPolicy,
};
use base64::{engine::general_purpose::STANDARD, Engine};
use chrono::{DateTime, Utc};
use sea_orm::{DatabaseConnection, FromQueryResult};
use std::{collections::BTreeMap, sync::Arc, time::Duration};
use temps_core::{secret_string::SecretString, EncryptionService, ProjectAccessChecker};
use temps_entities::sandbox_access_runtimes;
use temps_service_access_runtime::{WorkerIdentity, MAX_WORKER_BINDINGS};
use uuid::Uuid;
use zeroize::Zeroizing;

const MAX_GRANTS: usize = MAX_WORKER_BINDINGS * crate::tokenless::MAX_BINDING_GRANTS;
const PLAN_TIMEOUT: Duration = Duration::from_secs(10);

#[derive(Debug, thiserror::Error)]
pub enum PlanError {
    #[error(
        "Cannot prepare service policy for worker boot {boot_id} during {operation}: {reason}"
    )]
    Invalid {
        boot_id: Uuid,
        operation: &'static str,
        reason: &'static str,
    },
    #[error(
        "Cannot read service policy for worker boot {boot_id} during {operation}: database failed"
    )]
    Database {
        boot_id: Uuid,
        operation: &'static str,
        #[source]
        source: sea_orm::DbErr,
    },
    #[error("Cannot recheck service consent for user {user_id} on worker boot {boot_id}")]
    Permission {
        boot_id: Uuid,
        user_id: i32,
        #[source]
        source: Box<dyn std::error::Error + Send + Sync>,
    },
    #[error("Cannot decrypt credential for service {service_id} on worker boot {boot_id}")]
    Encryption {
        boot_id: Uuid,
        service_id: String,
        #[source]
        source: temps_core::EncryptionError,
    },
    #[error("Cannot decode stored policy for service {service_id} on worker boot {boot_id}")]
    Policy {
        boot_id: Uuid,
        service_id: String,
        #[source]
        source: serde_json::Error,
    },
    #[error("Cannot prepare authorized binding {runtime_id} on worker boot {boot_id}")]
    Binding {
        boot_id: Uuid,
        runtime_id: Uuid,
        #[source]
        source: Box<PreparationError>,
    },
}
fn invalid(boot_id: Uuid, operation: &'static str, reason: &'static str) -> PlanError {
    PlanError::Invalid {
        boot_id,
        operation,
        reason,
    }
}
fn database(boot_id: Uuid, operation: &'static str, source: sea_orm::DbErr) -> PlanError {
    PlanError::Database {
        boot_id,
        operation,
        source,
    }
}

/// Server-only plan, not an access receipt. Issue with this revision to fence
/// changes since the consistent read; deliver and verify the worker ACK next.
#[derive(Debug)]
pub struct WorkerPlan {
    pub worker: WorkerIdentity,
    pub node_id: Option<i32>,
    pub expected_revision: i64,
    pub bindings: Vec<AuthorizedBinding>,
    /// Current incarnations without any eligible grants must be cut, including
    /// when an approver lost access. Never reconnect them from saved intentions.
    pub retire: Vec<sandbox_access_runtimes::Model>,
}

pub struct PolicyPlanner {
    db: Arc<DatabaseConnection>,
    encryption: Arc<EncryptionService>,
    // Optional by the ProjectAccessChecker contract; OSS may have no checker.
    checker: Option<Arc<dyn ProjectAccessChecker>>,
}
impl PolicyPlanner {
    pub fn new(
        db: Arc<DatabaseConnection>,
        encryption: Arc<EncryptionService>,
        checker: Option<Arc<dyn ProjectAccessChecker>>,
    ) -> Self {
        Self {
            db,
            encryption,
            checker,
        }
    }

    /// Control-plane work only. At most 128 bindings / 2048 grants, three DB
    /// reads, deduplicated permission batches, and one ten-second total budget.
    /// No credential is decrypted until its current permission check succeeds.
    pub async fn prepare(&self, boot_id: Uuid) -> Result<WorkerPlan, PlanError> {
        if boot_id.is_nil() {
            return Err(invalid(
                boot_id,
                "validate authority",
                "worker boot must be non-nil",
            ));
        }
        tokio::time::timeout(PLAN_TIMEOUT, self.prepare_inner(boot_id))
            .await
            .map_err(|_| {
                invalid(
                    boot_id,
                    "read authorization",
                    "authorization deadline exceeded",
                )
            })?
    }

    async fn prepare_inner(&self, boot_id: Uuid) -> Result<WorkerPlan, PlanError> {
        let (authority, runtimes, candidates) = self.read(boot_id).await?;
        let seal_public_key: [u8; 32] = STANDARD
            .decode(&authority.seal_public_key)
            .ok()
            .and_then(|bytes| bytes.try_into().ok())
            .ok_or_else(|| {
                invalid(
                    boot_id,
                    "decode worker key",
                    "stored sealing key must contain 32 bytes",
                )
            })?;
        let allowed = self.recheck(boot_id, &candidates).await?;
        let now = Utc::now();
        let mut grouped: BTreeMap<Uuid, Vec<Candidate>> = BTreeMap::new();
        for candidate in candidates {
            if (candidate.admin || allowed.contains(&(candidate.approved_by, candidate.project_id)))
                && candidate.expires_at > now + chrono::Duration::seconds(1)
            {
                grouped
                    .entry(candidate.runtime_id)
                    .or_default()
                    .push(candidate);
            }
        }
        let mut bindings = Vec::new();
        let mut retire = Vec::new();
        for runtime in runtimes {
            let Some(grants) = grouped.remove(&runtime.id) else {
                retire.push(runtime);
                continue;
            };
            let version = u64::try_from(runtime.desired_version)
                .ok()
                .filter(|v| *v > 0)
                .ok_or_else(|| {
                    invalid(
                        boot_id,
                        "prepare binding",
                        "runtime version must be positive",
                    )
                })?;
            let mut requests = Vec::with_capacity(grants.len());
            let mut credentials = BTreeMap::new();
            for grant in grants {
                let policy: SandboxAccessPolicy =
                    serde_json::from_value(grant.policy).map_err(|source| PlanError::Policy {
                        boot_id,
                        service_id: grant.service_id.clone(),
                        source,
                    })?;
                let bytes = Zeroizing::new(
                    self.encryption
                        .decrypt(&grant.credential_encrypted)
                        .map_err(|source| PlanError::Encryption {
                            boot_id,
                            service_id: grant.service_id.clone(),
                            source,
                        })?,
                );
                let value = std::str::from_utf8(&bytes).map_err(|_| {
                    invalid(
                        boot_id,
                        "decode service credential",
                        "stored credential is not UTF-8",
                    )
                })?;
                if value.is_empty()
                    || value.len() > 16384
                    || !value.bytes().all(|b| (33..=126).contains(&b))
                {
                    return Err(invalid(
                        boot_id,
                        "validate service credential",
                        "stored bearer credential is invalid",
                    ));
                }
                credentials.insert(grant.grant_id.clone(), SecretString::new(value));
                requests.push(PrepareSession {
                    target: BindingTarget {
                        project_id: runtime.project_id,
                        sandbox_id: runtime.sandbox_id,
                        incarnation_id: runtime.id,
                        node_id: authority.sdk_node_id,
                    },
                    grant_id: grant.grant_id,
                    service_id: grant.service_id,
                    service_version: grant.service_version,
                    policy,
                    expires_at: grant.expires_at,
                    max_ttl_seconds: grant.max_ttl_seconds,
                });
            }
            let plan = PreparedBinding::prepare(requests, version, now).map_err(|source| {
                PlanError::Binding {
                    boot_id,
                    runtime_id: runtime.id,
                    source: Box::new(source),
                }
            })?;
            bindings.push(AuthorizedBinding { plan, credentials });
        }
        if !grouped.is_empty() {
            return Err(invalid(
                boot_id,
                "assemble worker policy",
                "grant has no current runtime",
            ));
        }
        Ok(WorkerPlan {
            worker: WorkerIdentity {
                boot_id,
                seal_public_key,
            },
            node_id: authority.node_id,
            expected_revision: authority.desired_revision,
            bindings,
            retire,
        })
    }
}

// Intentionally no Debug/Serialize: this projection contains encrypted secrets.
#[derive(FromQueryResult)]
struct Candidate {
    runtime_id: Uuid,
    project_id: i32,
    approved_by: i32,
    admin: bool,
    grant_id: String,
    service_id: String,
    service_version: i32,
    policy: serde_json::Value,
    max_ttl_seconds: i32,
    expires_at: DateTime<Utc>,
    credential_encrypted: String,
}

#[cfg(test)]
mod tests;
