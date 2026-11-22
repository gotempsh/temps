// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Managed databases on the workload plane (ADR-055 Phase 4).
//!
//! `external_services.workload_mode` decides, per database, how its
//! container is created:
//!
//! - `legacy` (the default): the engine on the control plane and the agent
//!   on a worker build their own requests, exactly as before.
//! - `shadow`: the same, plus a bounded background pass
//!   (`ExternalServiceManager::run_workload_shadow_pass`) that computes the
//!   database's spec ([`spec`]), inspects its running container and logs
//!   every difference. It never changes anything.
//! - `enforce`: both creation paths build the container from the admitted
//!   spec through the one Docker driver: the control-plane engines through
//!   [`registry::enforced_create`], a worker through the agent's
//!   `POST /agent/services/workload`. A running container changes only when
//!   it is next recreated, and the API that confirms a recreate lists the
//!   changes first ([`PendingChanges`]).

pub mod error;
pub mod registry;
pub mod runtime;
pub mod shadow;
pub mod spec;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use temps_entities::external_service_workload_mode::ServiceWorkloadMode;
pub use temps_workload_docker::{ShapeDifference, ShapeField};
use utoipa::ToSchema;

pub use error::ServiceWorkloadError;
pub use spec::{
    database_create_body, database_instance, DatabaseEngine, DatabaseIdentity, DatabaseProcess,
    DatabaseWorkloadPlan, NodeAttachment, SecretValues,
};

/// Whether a database can leave `legacy`, and why not.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct WorkloadSupport {
    pub supported: bool,
    /// Why the database stays on its engine's own path, when unsupported.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
}

impl WorkloadSupport {
    pub fn yes() -> Self {
        Self {
            supported: true,
            reason: None,
        }
    }

    pub fn no(reason: impl Into<String>) -> Self {
        Self {
            supported: false,
            reason: Some(reason.into()),
        }
    }
}

/// One thing a recreate from the spec would change.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct WorkloadChange {
    /// `labels`, `restart_policy`, `pids_limit`, `no_new_privileges`,
    /// `healthcheck`, `log_config`, ...
    pub field: String,
    /// What the running container has. Secrets are shown as `***`.
    pub current: String,
    /// What the container will have once recreated from its spec.
    pub after_recreate: String,
}

impl From<&ShapeDifference> for WorkloadChange {
    fn from(difference: &ShapeDifference) -> Self {
        Self {
            field: difference.field.as_str().to_string(),
            current: difference.actual.clone(),
            after_recreate: difference.expected.clone(),
        }
    }
}

/// What recreating a database's container from its workload spec would
/// change, listed before anything is changed. A recreate is only accepted
/// with this response's `fingerprint`, so what the administrator confirmed
/// is what is applied.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct PendingChanges {
    pub service_id: i32,
    pub service_name: String,
    pub mode: ServiceWorkloadMode,
    /// `None`: the control plane.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub node_id: Option<i32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub container_name: Option<String>,
    pub support: WorkloadSupport,
    /// Whether the database's container exists to compare against.
    pub container_found: bool,
    pub changes: Vec<WorkloadChange>,
    /// Identifies `changes`: pass it back to confirm a recreate.
    pub fingerprint: String,
    #[schema(value_type = String, format = DateTime)]
    pub computed_at: DateTime<Utc>,
}

/// The fingerprint of a list of changes.
pub fn changes_fingerprint(changes: &[WorkloadChange]) -> String {
    let mut hasher = Sha256::new();
    for change in changes {
        for part in [&change.field, &change.current, &change.after_recreate] {
            hasher.update(part.as_bytes());
            hasher.update([0u8]);
        }
        hasher.update([1u8]);
    }
    hex::encode(hasher.finalize())
}

/// The outcome for one database of a shadow pass.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ShadowResult {
    pub service_id: i32,
    pub service_name: String,
    pub node_id: Option<i32>,
    pub container_name: String,
    pub differences: Vec<ShapeDifference>,
}

impl ShadowResult {
    pub fn fields(&self) -> Vec<ShapeField> {
        self.differences.iter().map(|d| d.field).collect()
    }
}

/// Why a database in shadow mode was not compared.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ShadowSkip {
    pub service_id: i32,
    pub service_name: String,
    pub reason: String,
}

/// One shadow pass: every database in `shadow` mode, up to the batch cap.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct ShadowPassReport {
    pub compared: Vec<ShadowResult>,
    pub skipped: Vec<ShadowSkip>,
    /// More shadow-mode databases exist than one pass compares.
    pub truncated: bool,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_fingerprint_changes_with_any_change() {
        let change = WorkloadChange {
            field: "restart_policy".into(),
            current: "unless-stopped".into(),
            after_recreate: "always".into(),
        };
        let one = changes_fingerprint(std::slice::from_ref(&change));
        assert_eq!(one, changes_fingerprint(std::slice::from_ref(&change)));
        assert_ne!(one, changes_fingerprint(&[]));
        let mut other = change.clone();
        other.current = "no".into();
        assert_ne!(one, changes_fingerprint(&[other]));
    }
}
