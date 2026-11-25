// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Recording a sandbox's outbound connections: the sandbox side.
//!
//! Recording itself is not a sandbox feature. The opt-in flow, the ingest,
//! the records, the activity query and the node-0 recorder are written
//! once, for every workload kind, in `temps-workload-egress`. This module
//! is what is particular to sandboxes:
//!
//! - **the adapter** ([`SandboxEgressAdapter`]): the opt-in is a column on
//!   the sandbox's own row (`sandboxes.egress_recording`), a sandbox runs
//!   on one node, and it may belong to a project or to none;
//! - **who may see and change it** ([`SandboxEgress`]): the sandbox's
//!   owner, and nobody else learns that it exists;
//! - **how its traffic reaches the recorder on this host** ([`local`]): its
//!   sidecar hands it over, held to the sandbox's own egress policy
//!   ([`policy`]).

pub mod local;
pub mod policy;

#[cfg(test)]
pub(crate) mod tests;

use std::sync::{Arc, OnceLock};

use async_trait::async_trait;
use sea_orm::{ActiveModelTrait, ColumnTrait, DatabaseConnection, EntityTrait, QueryFilter, Set};
use temps_entities::sandboxes;
use temps_workload::{WorkloadKind, WorkloadRef};
use temps_workload_egress::local::LocalEgressRecorder;
use temps_workload_egress::{
    EgressActivityPage, EgressOutcomeFilter, EgressRecordingError, EgressRecordingService,
    EgressRecordingStatus, RecordedWorkload, WorkloadEgressAdapter, CONTROL_PLANE_NODE_ID,
};

use crate::services::public_id;

/// The attribute every record of a sandbox carries with its public id,
/// which the console's logs link filters on.
pub const SANDBOX_ID_ATTRIBUTE: &str = "temps.sandbox.id";

/// What users call one, in messages.
const NOUN: &str = "sandbox";

/// A sandbox row as the recording service sees a workload.
pub fn recorded(row: &sandboxes::Model) -> RecordedWorkload {
    RecordedWorkload {
        workload: WorkloadRef::new(WorkloadKind::Sandbox, i64::from(row.id)),
        public_id: row.public_id.clone(),
        project_id: row.project_id,
        nodes: vec![row.node_id.unwrap_or(CONTROL_PLANE_NODE_ID)],
        enabled: row.egress_recording,
        live: row.status != "destroyed",
    }
}

/// What steering a sandbox's traffic on this host needs once its opt-in
/// changed: the recorder, and the sandboxes' own traffic source.
pub struct LocalSteering {
    pub recorder: Arc<LocalEgressRecorder>,
    pub traffic: Arc<local::SandboxTraffic>,
}

/// The sandbox kind's answers to the recording service.
pub struct SandboxEgressAdapter {
    db: Arc<DatabaseConnection>,
    /// Set once this host records its own sandboxes; absent, a change is
    /// only stored.
    local: OnceLock<LocalSteering>,
}

impl SandboxEgressAdapter {
    pub fn new(db: Arc<DatabaseConnection>) -> Self {
        Self {
            db,
            local: OnceLock::new(),
        }
    }

    /// Wires what a change does on this host. Set once.
    pub fn set_local(&self, steering: LocalSteering) {
        let _ = self.local.set(steering);
    }
}

#[async_trait]
impl WorkloadEgressAdapter for SandboxEgressAdapter {
    fn kind(&self) -> WorkloadKind {
        WorkloadKind::Sandbox
    }

    fn noun(&self) -> &'static str {
        NOUN
    }

    fn id_attribute(&self) -> &'static str {
        SANDBOX_ID_ATTRIBUTE
    }

    async fn load(&self, ids: &[i64]) -> Result<Vec<RecordedWorkload>, EgressRecordingError> {
        // An id outside the column's range names no sandbox.
        let ids: Vec<i32> = ids
            .iter()
            .filter_map(|id| i32::try_from(*id).ok())
            .collect();
        if ids.is_empty() {
            return Ok(Vec::new());
        }
        let rows = sandboxes::Entity::find()
            .filter(sandboxes::Column::Id.is_in(ids))
            .all(self.db.as_ref())
            .await
            .map_err(EgressRecordingError::db(
                "loading the sandboxes named by a batch of egress records",
            ))?;
        Ok(rows.iter().map(recorded).collect())
    }

    async fn set_enabled(
        &self,
        workload: WorkloadRef,
        enabled: bool,
    ) -> Result<(), EgressRecordingError> {
        let id = i32::try_from(workload.id).map_err(|_| EgressRecordingError::NotFound {
            noun: NOUN,
            id: workload.id.to_string(),
        })?;
        sandboxes::ActiveModel {
            id: Set(id),
            egress_recording: Set(enabled),
            ..Default::default()
        }
        .update(self.db.as_ref())
        .await
        .map_err(EgressRecordingError::db(
            "updating a sandbox's egress recording",
        ))?;
        Ok(())
    }

    /// The table and the published set follow at once, and a sandbox of
    /// this host gets its sidecar recreated with or without the hand-off.
    async fn changed(&self, workload: &RecordedWorkload) {
        let Some(local) = self.local.get() else {
            return;
        };
        local.recorder.refresh().await;
        let Ok(id) = i32::try_from(workload.workload.id) else {
            return;
        };
        local.traffic.steer(id).await;
        local.recorder.wake();
    }
}

/// A sandbox's recording as its owner sees and changes it: resolves the
/// caller's sandbox, then asks the recording service.
pub struct SandboxEgress {
    db: Arc<DatabaseConnection>,
    recording: Arc<EgressRecordingService>,
}

impl SandboxEgress {
    pub fn new(db: Arc<DatabaseConnection>, recording: Arc<EgressRecordingService>) -> Self {
        Self { db, recording }
    }

    /// The caller's live sandbox `public_id_value`, or not found.
    async fn owned(
        &self,
        public_id_value: &str,
        user_id: i32,
    ) -> Result<RecordedWorkload, EgressRecordingError> {
        let not_found = || EgressRecordingError::NotFound {
            noun: NOUN,
            id: public_id_value.to_string(),
        };
        if !public_id::is_valid(public_id_value) {
            return Err(not_found());
        }
        let row = sandboxes::Entity::find()
            .filter(sandboxes::Column::PublicId.eq(public_id_value))
            .one(self.db.as_ref())
            .await
            .map_err(EgressRecordingError::db("looking up a sandbox"))?
            .ok_or_else(not_found)?;
        // Don't leak existence to non-owners; a destroyed sandbox is gone.
        if row.user_id != Some(user_id) || row.status == "destroyed" {
            return Err(not_found());
        }
        Ok(recorded(&row))
    }

    /// Whether the caller's sandbox records its connections, and whether
    /// it can.
    pub async fn status(
        &self,
        public_id_value: &str,
        user_id: i32,
    ) -> Result<EgressRecordingStatus, EgressRecordingError> {
        let sandbox = self.owned(public_id_value, user_id).await?;
        self.recording.status(&sandbox).await
    }

    /// Turns recording on or off for the caller's sandbox. Returns the
    /// value before and the status after, for the audit log.
    pub async fn set_enabled(
        &self,
        public_id_value: &str,
        user_id: i32,
        enabled: bool,
    ) -> Result<(bool, EgressRecordingStatus), EgressRecordingError> {
        let sandbox = self.owned(public_id_value, user_id).await?;
        self.recording.set_enabled(&sandbox, enabled).await
    }

    /// A page of the caller's sandbox's recorded outbound connections,
    /// newest first.
    pub async fn activity(
        &self,
        public_id_value: &str,
        user_id: i32,
        page: Option<u64>,
        per_page: Option<u64>,
        outcome: Option<EgressOutcomeFilter>,
    ) -> Result<EgressActivityPage, EgressRecordingError> {
        let sandbox = self.owned(public_id_value, user_id).await?;
        self.recording
            .activity(&sandbox, page, per_page, outcome)
            .await
    }
}
