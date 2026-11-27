// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! What a workload kind tells the recording service about its workloads.
//!
//! Everything else about recording is the same for every kind and lives in
//! this crate. A kind answers only the questions that differ: where its
//! opt-in is stored, which project owns a workload, where it runs and what
//! it is called.

use async_trait::async_trait;
use temps_workload::{WorkloadKind, WorkloadRef};

use crate::error::EgressRecordingError;

/// One workload as its own product's tables describe it now.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RecordedWorkload {
    /// The identity the proxy and storage know it by.
    pub workload: WorkloadRef,
    /// The id its own API names it by, which every record carries
    /// (`sbx_...` for a sandbox, the environment id for an environment).
    pub public_id: String,
    /// The project that owns it, when one does. Its records are then also
    /// that project's logs.
    pub project_id: Option<i32>,
    /// The nodes it runs on now; `0` is the control plane. Empty when
    /// nothing of it is running.
    pub nodes: Vec<i32>,
    /// Its owner turned recording on.
    pub enabled: bool,
    /// `false` once it is destroyed or deleted: nothing is recorded for it.
    pub live: bool,
    /// Why no recorder can see this workload's traffic, whatever its node
    /// can do: it runs on a backend or is deployed in a way nothing steers
    /// to the egress proxy. `None` when it can be recorded. Turning
    /// recording on is refused with this reason.
    pub unrecordable: Option<String>,
}

/// One workload kind's side of recording.
#[async_trait]
pub trait WorkloadEgressAdapter: Send + Sync {
    /// The kind of [`WorkloadRef`] this adapter answers for.
    fn kind(&self) -> WorkloadKind;

    /// What users call one of these, in messages: `sandbox`, `environment`.
    fn noun(&self) -> &'static str;

    /// The attribute that carries [`RecordedWorkload::public_id`] on every
    /// record (`temps.sandbox.id`), which a project's logs are filtered by.
    fn id_attribute(&self) -> &'static str;

    /// The workloads with these ids, as the database has them now. An id
    /// that does not exist is left out.
    async fn load(&self, ids: &[i64]) -> Result<Vec<RecordedWorkload>, EgressRecordingError>;

    /// Stores the opt-in on the row it configures, only where it differs,
    /// and says whether this write changed it. Of two requests racing to
    /// the same value only one changes it, and only that one is a change.
    async fn set_enabled(
        &self,
        workload: WorkloadRef,
        enabled: bool,
    ) -> Result<bool, EgressRecordingError>;

    /// Called after every write of the opt-in, changed or not, with the
    /// workload as it is now: the place to steer its traffic to or away
    /// from the recorder on this host. It must be safe to repeat: setting
    /// the same value again repairs a change that was stored but not
    /// followed (the control plane stopped in between). It must also
    /// survive being cancelled part-way (the request that called it
    /// dropped): whatever it has not done yet is left for a retry it
    /// arranges before it awaits anything. Returns why the workload's
    /// traffic does not follow the change yet, when it does not. Nothing by
    /// default.
    async fn changed(&self, _workload: &RecordedWorkload) -> Option<String> {
        None
    }

    /// Why `workload`'s traffic does not follow its opt-in yet, when an
    /// earlier [`Self::changed`] could not make it: reported by its status
    /// until it does. `None` by default.
    fn not_applied(&self, _workload: &RecordedWorkload) -> Option<String> {
        None
    }
}
