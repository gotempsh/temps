// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Bounded authorization reads for one worker. Saved intentions never become
//! live policy: only fresh consent tied to the current incarnation is eligible.
mod credentials;
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
use std::{
    collections::{BTreeMap, BTreeSet},
    sync::{Arc, OnceLock},
    time::Duration,
};
use temps_core::{secret_string::SecretString, EncryptionService, ProjectAccessChecker};
use temps_entities::sandbox_access_runtimes;
use temps_service_access_runtime::{WorkerIdentity, MAX_WORKER_BINDINGS};
use uuid::Uuid;
use zeroize::Zeroizing;

// Read one overflow binding so old excess rows can be retired without blocking renewals.
const MAX_GRANTS: usize = (MAX_WORKER_BINDINGS + 1) * crate::tokenless::MAX_BINDING_GRANTS;
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
    #[error("Cannot resolve GitHub credential for service {service_id} on worker boot {boot_id}")]
    Github {
        boot_id: Uuid,
        service_id: String,
        #[source]
        source: crate::github::GithubCredentialError,
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

/// Filled after every plugin has registered; unset must never mean OSS access.
#[derive(Default)]
pub(crate) struct ProjectAccessBinding(pub OnceLock<Option<Arc<dyn ProjectAccessChecker>>>);

pub struct PolicyPlanner {
    db: Arc<DatabaseConnection>,
    encryption: Arc<EncryptionService>,
    github: Arc<dyn crate::github::GithubCredentialResolver>,
    // Optional by the ProjectAccessChecker contract; OSS may have no checker.
    checker: Arc<ProjectAccessBinding>,
}
impl PolicyPlanner {
    pub fn new(
        db: Arc<DatabaseConnection>,
        encryption: Arc<EncryptionService>,
        checker: Option<Arc<dyn ProjectAccessChecker>>,
        github: Arc<dyn crate::github::GithubCredentialResolver>,
    ) -> Self {
        Self {
            db,
            encryption,
            checker: Arc::new(ProjectAccessBinding(OnceLock::from(checker))),
            github,
        }
    }

    pub(crate) fn bind_project_access(&mut self, binding: Arc<ProjectAccessBinding>) {
        self.checker = binding;
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
        self.prepare_rows(boot_id, authority, runtimes, candidates)
            .await
    }

    async fn prepare_rows(
        &self,
        boot_id: Uuid,
        authority: temps_entities::sandbox_access_authorities::Model,
        runtimes: Vec<sandbox_access_runtimes::Model>,
        candidates: Vec<Candidate>,
    ) -> Result<WorkerPlan, PlanError> {
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
        let mut grouped: BTreeMap<Uuid, Vec<(Candidate, SandboxAccessPolicy)>> = BTreeMap::new();
        let mut github_connections = BTreeSet::new();
        for mut candidate in candidates {
            let permissions = allowed.get(&(candidate.approved_by, candidate.project_id));
            let needs_git = candidate
                .policy
                .get("github_repository")
                .is_some_and(|value| !value.is_null());
            if (candidate.admin || permissions.is_some_and(|git| !needs_git || *git))
                && candidate.expires_at > now + chrono::Duration::seconds(1)
            {
                let policy: SandboxAccessPolicy = serde_json::from_value(std::mem::take(
                    &mut candidate.policy,
                ))
                .map_err(|source| PlanError::Policy {
                    boot_id,
                    service_id: candidate.service_id.clone(),
                    source,
                })?;
                if let Some(source) = &policy.github_repository {
                    github_connections.insert(source.connection_id);
                }
                grouped
                    .entry(candidate.runtime_id)
                    .or_default()
                    .push((candidate, policy));
            }
        }
        let resolved_github = self.resolve_github(boot_id, github_connections).await;
        let mut bindings = Vec::new();
        let mut retire = Vec::new();
        for runtime in runtimes {
            let Some(grants) = grouped.remove(&runtime.id) else {
                retire.push(runtime);
                continue;
            };
            if bindings.len() >= MAX_WORKER_BINDINGS {
                // Admission prevents new overflow. Drain legacy excess in bounded
                // batches while retaining the existing bindings selected first.
                retire.push(runtime);
                continue;
            }
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
            for (grant, policy) in grants {
                let credential = if let Some(source) = &policy.github_repository {
                    let Some(credential) = resolved_github
                        .get(&source.connection_id)
                        .and_then(Option::as_ref)
                    else {
                        continue;
                    };
                    credential.clone()
                } else {
                    let encrypted = grant.credential_encrypted.as_deref().ok_or_else(|| {
                        invalid(
                            boot_id,
                            "decode service credential",
                            "stored bearer credential is missing",
                        )
                    })?;
                    let bytes =
                        Zeroizing::new(self.encryption.decrypt(encrypted).map_err(|source| {
                            PlanError::Encryption {
                                boot_id,
                                service_id: grant.service_id.clone(),
                                source,
                            }
                        })?);
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
                    SecretString::new(value)
                };
                credentials.insert(grant.grant_id.clone(), credential);
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
            // A transient credential outage does not revoke consent. Omit the
            // unavailable binding now and retry on its next renewal command.
            if requests.is_empty() {
                continue;
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
    credential_encrypted: Option<String>,
}

#[cfg(test)]
mod tests;
