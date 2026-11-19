// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! The ordered steps of a sandbox create, as it runs.
//!
//! `POST /v1/sandboxes` answers only when the sandbox is ready, and the first
//! create on a host can spend minutes pulling its image. This records what
//! the create is doing, step by step, so a client can show it while it waits:
//! the client names the create with a `creation_id` and reads it from
//! `GET /v1/sandboxes/creations/{creation_id}`, or, once the sandbox row
//! exists, from `GET /v1/sandboxes/{id}/creation`.
//!
//! Why in memory, like [`super::job_tracker`]: a create is process-local (it
//! dies with the server, and the row it leaves is reconciled on startup), so
//! its steps are too. The deployment pipeline's steps are rows of a durable
//! workflow; a sandbox create is not resumable, and a row per step would
//! cost writes on every create for state nothing reads after a minute.
//!
//! Memory: one entry per create, a few hundred bytes. Finished creates are
//! kept for [`FINISHED_RETENTION`] so a client polling at its own pace still
//! reads the outcome, and the map is capped at [`MAX_TRACKED_CREATES`]
//! (oldest finished first). Running creates are never evicted; they are
//! bounded by the creates the server is running. All locking is a
//! `std::sync::Mutex` held for a field update, never across an `.await`.

use std::collections::HashMap;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

use temps_sandbox_providers::{CreatePhase, CreatePhaseEvent, ImagePrepareStatus};

/// How long a finished create stays readable.
pub const FINISHED_RETENTION: chrono::Duration = chrono::Duration::minutes(15);
/// Most creates tracked at once (finished ones beyond it are dropped first).
pub const MAX_TRACKED_CREATES: usize = 1024;
/// Longest client-chosen `creation_id`.
pub const MAX_CREATION_ID_LEN: usize = 64;

/// One step of a sandbox create, in the order the create runs them.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum CreationStepKind {
    /// Validating the request and resolving its source.
    CheckRequest,
    /// Choosing the node that hosts the sandbox.
    SelectNode,
    /// Applying operator limits and recording the sandbox.
    RecordSandbox,
    /// Making sure the image is on the node (pulling it when it is not).
    PullImage,
    /// Setting up the sandbox network and preview gateway, then creating
    /// the container.
    CreateContainer,
    /// Starting the container.
    StartContainer,
    /// Fixing workspace ownership and waiting for the sandbox runtime.
    WaitReady,
    /// Cloning or extracting the requested source into the workspace.
    SeedSource,
    /// Recording the result; the sandbox is ready.
    Ready,
}

impl CreationStepKind {
    /// Every step, in order.
    pub const ALL: [CreationStepKind; 9] = [
        Self::CheckRequest,
        Self::SelectNode,
        Self::RecordSandbox,
        Self::PullImage,
        Self::CreateContainer,
        Self::StartContainer,
        Self::WaitReady,
        Self::SeedSource,
        Self::Ready,
    ];

    pub fn title(self) -> &'static str {
        match self {
            Self::CheckRequest => "Check the request",
            Self::SelectNode => "Choose a node",
            Self::RecordSandbox => "Apply limits and record the sandbox",
            Self::PullImage => "Pull the sandbox image",
            Self::CreateContainer => "Create the network and container",
            Self::StartContainer => "Start the container",
            Self::WaitReady => "Wait for the sandbox to be ready",
            Self::SeedSource => "Copy the source into the workspace",
            Self::Ready => "Ready",
        }
    }

    fn from_phase(phase: CreatePhase) -> Self {
        match phase {
            CreatePhase::Image => Self::PullImage,
            CreatePhase::CreateContainer => Self::CreateContainer,
            CreatePhase::StartContainer => Self::StartContainer,
            CreatePhase::WaitReady => Self::WaitReady,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum CreationStepStatus {
    Pending,
    Running,
    Done,
    Failed,
    Skipped,
}

/// One step and where it stands.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct CreationStep {
    pub kind: CreationStepKind,
    pub title: String,
    pub status: CreationStepStatus,
    /// What the step did or is doing (`already present`, `on node
    /// worker-1`, `downloading`), or why it was skipped.
    pub detail: Option<String>,
    /// Why the step failed, with the node, image or sandbox involved.
    pub error: Option<String>,
    #[schema(value_type = Option<String>, format = DateTime)]
    pub started_at: Option<DateTime<Utc>>,
    #[schema(value_type = Option<String>, format = DateTime)]
    pub finished_at: Option<DateTime<Utc>>,
    /// Image download progress (`pull_image` only), when the registry
    /// reports sizes.
    pub bytes_done: Option<u64>,
    pub bytes_total: Option<u64>,
}

impl CreationStep {
    fn new(kind: CreationStepKind) -> Self {
        Self {
            kind,
            title: kind.title().to_string(),
            status: CreationStepStatus::Pending,
            detail: None,
            error: None,
            started_at: None,
            finished_at: None,
            bytes_done: None,
            bytes_total: None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum CreationStatus {
    Running,
    Succeeded,
    Failed,
}

/// A sandbox create and its steps.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct SandboxCreation {
    /// The id the client gave the create (or one the server generated).
    pub creation_id: String,
    /// The sandbox's public id, once its row exists.
    pub sandbox_id: Option<String>,
    pub status: CreationStatus,
    /// Every step, in order.
    pub steps: Vec<CreationStep>,
    /// Why the create failed.
    pub error: Option<String>,
    #[schema(value_type = String, format = DateTime)]
    pub started_at: DateTime<Utc>,
    #[schema(value_type = Option<String>, format = DateTime)]
    pub finished_at: Option<DateTime<Utc>>,
}

/// Why a create could not be tracked under the id its client chose.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum CreationIdError {
    #[error(
        "creation_id '{creation_id}' is invalid: use 8 to {MAX_CREATION_ID_LEN} letters, digits, '-' or '_'"
    )]
    Invalid { creation_id: String },
    #[error("creation_id '{creation_id}' is already used by another create of user {user_id}")]
    InUse { creation_id: String, user_id: i32 },
}

/// Whether `id` is acceptable as a client-chosen creation id.
pub fn valid_creation_id(id: &str) -> bool {
    (8..=MAX_CREATION_ID_LEN).contains(&id.len())
        && id
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
}

type Key = (i32, String);

#[derive(Default)]
struct Inner {
    creations: HashMap<Key, SandboxCreation>,
    /// Sandbox public id → its create, once the row exists.
    by_sandbox: HashMap<String, Key>,
}

/// Every create this process is running or finished recently.
#[derive(Default)]
pub struct CreationTracker {
    inner: Mutex<Inner>,
}

impl CreationTracker {
    fn lock(&self) -> MutexGuard<'_, Inner> {
        // Field updates only: a poisoned lock still holds consistent data.
        self.inner.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Start tracking a create for `user_id`. `creation_id` is the client's
    /// (validated) or, when `None`, a generated one.
    pub fn begin(
        self: &Arc<Self>,
        user_id: i32,
        creation_id: Option<&str>,
    ) -> Result<CreationProgress, CreationIdError> {
        let creation_id = match creation_id {
            Some(id) if !valid_creation_id(id) => {
                return Err(CreationIdError::Invalid {
                    creation_id: id.chars().take(MAX_CREATION_ID_LEN).collect(),
                })
            }
            Some(id) => id.to_string(),
            None => generated_creation_id(),
        };
        let key = (user_id, creation_id.clone());
        let now = Utc::now();
        let mut inner = self.lock();
        if inner.creations.contains_key(&key) {
            return Err(CreationIdError::InUse {
                creation_id,
                user_id,
            });
        }
        prune(&mut inner, now);
        inner.creations.insert(
            key.clone(),
            SandboxCreation {
                creation_id,
                sandbox_id: None,
                status: CreationStatus::Running,
                steps: CreationStepKind::ALL
                    .into_iter()
                    .map(CreationStep::new)
                    .collect(),
                error: None,
                started_at: now,
                finished_at: None,
            },
        );
        Ok(CreationProgress {
            tracker: Arc::clone(self),
            key,
        })
    }

    /// A user's create by the id it was started with.
    pub fn get(&self, user_id: i32, creation_id: &str) -> Option<SandboxCreation> {
        self.lock()
            .creations
            .get(&(user_id, creation_id.to_string()))
            .cloned()
    }

    /// The create of a sandbox, by its public id. The caller has already
    /// authorized access to the sandbox.
    pub fn for_sandbox(&self, public_id: &str) -> Option<SandboxCreation> {
        let inner = self.lock();
        let key = inner.by_sandbox.get(public_id)?;
        inner.creations.get(key).cloned()
    }

    fn update(&self, key: &Key, f: impl FnOnce(&mut SandboxCreation)) {
        if let Some(creation) = self.lock().creations.get_mut(key) {
            f(creation);
        }
    }
}

/// Drop finished creates older than the retention, then the oldest finished
/// ones while the map is over its cap.
fn prune(inner: &mut Inner, now: DateTime<Utc>) {
    let expired: Vec<Key> = inner
        .creations
        .iter()
        .filter(|(_, c)| {
            c.finished_at
                .is_some_and(|at| now - at > FINISHED_RETENTION)
        })
        .map(|(k, _)| k.clone())
        .collect();
    for key in expired {
        remove(inner, &key);
    }
    if inner.creations.len() < MAX_TRACKED_CREATES {
        return;
    }
    let mut finished: Vec<(Key, DateTime<Utc>)> = inner
        .creations
        .iter()
        .filter_map(|(k, c)| c.finished_at.map(|at| (k.clone(), at)))
        .collect();
    finished.sort_by_key(|(_, at)| *at);
    let excess = inner.creations.len() + 1 - MAX_TRACKED_CREATES;
    for (key, _) in finished.into_iter().take(excess) {
        remove(inner, &key);
    }
    if inner.creations.len() >= MAX_TRACKED_CREATES {
        tracing::warn!(
            running = inner.creations.len(),
            cap = MAX_TRACKED_CREATES,
            "more sandbox creates are running than the progress tracker's cap; \
             tracking the new one anyway"
        );
    }
}

fn remove(inner: &mut Inner, key: &Key) {
    if let Some(creation) = inner.creations.remove(key) {
        if let Some(sandbox_id) = creation.sandbox_id {
            inner.by_sandbox.remove(&sandbox_id);
        }
    }
}

fn generated_creation_id() -> String {
    use rand::Rng;
    let mut bytes = [0u8; 8];
    rand::rng().fill_bytes(&mut bytes);
    format!("crt_{}", hex::encode(bytes))
}

/// Records one create's steps. Cheap to clone; every method is a short,
/// non-blocking update.
#[derive(Clone)]
pub struct CreationProgress {
    tracker: Arc<CreationTracker>,
    key: Key,
}

impl CreationProgress {
    pub fn creation_id(&self) -> &str {
        &self.key.1
    }

    fn step(&self, kind: CreationStepKind, f: impl FnOnce(&mut CreationStep)) {
        self.tracker.update(&self.key, |creation| {
            if let Some(step) = creation.steps.iter_mut().find(|s| s.kind == kind) {
                f(step);
            }
        });
    }

    /// Mark `kind` running (from now) unless it already finished.
    pub fn start(&self, kind: CreationStepKind) {
        self.step(kind, |step| {
            if matches!(step.status, CreationStepStatus::Pending) {
                step.status = CreationStepStatus::Running;
                step.started_at = Some(Utc::now());
            }
        });
    }

    /// Mark `kind` done, with what it did. A step that never started is
    /// recorded as starting now.
    pub fn done(&self, kind: CreationStepKind, detail: Option<String>) {
        self.step(kind, |step| {
            if matches!(
                step.status,
                CreationStepStatus::Done | CreationStepStatus::Skipped | CreationStepStatus::Failed
            ) {
                return;
            }
            let now = Utc::now();
            step.status = CreationStepStatus::Done;
            step.started_at.get_or_insert(now);
            step.finished_at = Some(now);
            if detail.is_some() {
                step.detail = detail;
            }
            if let Some(total) = step.bytes_total {
                step.bytes_done = Some(total);
            }
        });
    }

    /// End a step the provider ran inside its create: a step it reported is
    /// closed as is; one it never reported is done now, with `detail`
    /// saying so.
    pub fn done_unreported(&self, kind: CreationStepKind, detail: &str) {
        self.step(kind, |step| {
            let now = Utc::now();
            match step.status {
                CreationStepStatus::Pending => {
                    step.detail.get_or_insert_with(|| detail.to_string());
                }
                CreationStepStatus::Running => {}
                CreationStepStatus::Done
                | CreationStepStatus::Failed
                | CreationStepStatus::Skipped => return,
            }
            step.status = CreationStepStatus::Done;
            step.started_at.get_or_insert(now);
            step.finished_at = Some(now);
            if let Some(total) = step.bytes_total {
                step.bytes_done = Some(total);
            }
        });
    }

    /// Mark a step that does not apply to this create.
    pub fn skip(&self, kind: CreationStepKind, reason: impl Into<String>) {
        let reason = reason.into();
        self.step(kind, |step| {
            if matches!(step.status, CreationStepStatus::Pending) {
                step.status = CreationStepStatus::Skipped;
                step.detail = Some(reason);
            }
        });
    }

    /// Say what a running step is doing.
    pub fn detail(&self, kind: CreationStepKind, detail: impl Into<String>) {
        let detail = detail.into();
        self.step(kind, |step| step.detail = Some(detail));
    }

    /// Record the image download's progress on the pull step.
    pub fn image_progress(&self, status: &ImagePrepareStatus) {
        self.step(CreationStepKind::PullImage, |step| {
            if matches!(step.status, CreationStepStatus::Pending) {
                step.status = CreationStepStatus::Running;
                step.started_at = Some(Utc::now());
            }
            if status.bytes_total.is_some() {
                step.bytes_done = status.bytes_done;
                step.bytes_total = status.bytes_total;
            }
            if let Some(detail) = &status.detail {
                step.detail = Some(detail.clone());
            }
        });
    }

    /// The sandbox row exists: its public id now finds this create too.
    pub fn sandbox_recorded(&self, public_id: &str) {
        let mut inner = self.tracker.lock();
        if let Some(creation) = inner.creations.get_mut(&self.key) {
            creation.sandbox_id = Some(public_id.to_string());
            inner
                .by_sandbox
                .insert(public_id.to_string(), self.key.clone());
        }
    }

    /// Apply one phase a provider reported from inside its create.
    pub fn apply_phase(&self, event: CreatePhaseEvent) {
        match event {
            CreatePhaseEvent::Started(phase) => self.start(CreationStepKind::from_phase(phase)),
            CreatePhaseEvent::Finished(phase) => {
                self.done(CreationStepKind::from_phase(phase), None)
            }
            CreatePhaseEvent::ImageAlreadyPresent => {
                self.detail(CreationStepKind::PullImage, "already present on the node")
            }
            CreatePhaseEvent::ImageProgress(status) => self.image_progress(&status),
        }
    }

    /// A sink for [`temps_sandbox_providers::with_create_progress`].
    pub fn phase_sink(&self) -> temps_sandbox_providers::CreateProgressSink {
        let progress = self.clone();
        Arc::new(move |event| progress.apply_phase(event))
    }

    /// The create succeeded: finish the remaining steps. Steps a provider
    /// ran without reporting them separately are done now, saying so.
    pub fn succeed(&self, unreported_detail: &str) {
        let now = Utc::now();
        self.tracker.update(&self.key, |creation| {
            for step in creation.steps.iter_mut() {
                match step.status {
                    CreationStepStatus::Pending | CreationStepStatus::Running => {
                        if step.kind != CreationStepKind::Ready
                            && step.status == CreationStepStatus::Pending
                        {
                            step.detail
                                .get_or_insert_with(|| unreported_detail.to_string());
                        }
                        step.status = CreationStepStatus::Done;
                        step.started_at.get_or_insert(now);
                        step.finished_at = Some(now);
                    }
                    CreationStepStatus::Done
                    | CreationStepStatus::Failed
                    | CreationStepStatus::Skipped => {}
                }
            }
            creation.status = CreationStatus::Succeeded;
            creation.finished_at = Some(now);
        });
    }

    /// The create failed: the running step (else the first pending one)
    /// failed with `error`; the steps after it were not reached and stay
    /// pending.
    pub fn fail(&self, error: &str) {
        let now = Utc::now();
        self.tracker.update(&self.key, |creation| {
            let failed = creation
                .steps
                .iter()
                .rposition(|s| s.status == CreationStepStatus::Running)
                .or_else(|| {
                    creation
                        .steps
                        .iter()
                        .position(|s| s.status == CreationStepStatus::Pending)
                });
            if let Some(index) = failed {
                let step = &mut creation.steps[index];
                step.status = CreationStepStatus::Failed;
                step.started_at.get_or_insert(now);
                step.finished_at = Some(now);
                step.error = Some(error.to_string());
            }
            // Any other step a provider left running did not finish either.
            for step in creation.steps.iter_mut() {
                if step.status == CreationStepStatus::Running {
                    step.status = CreationStepStatus::Failed;
                    step.finished_at = Some(now);
                }
            }
            creation.status = CreationStatus::Failed;
            creation.error = Some(error.to_string());
            creation.finished_at = Some(now);
        });
    }

    /// The create's current record (tests and logging).
    pub fn snapshot(&self) -> Option<SandboxCreation> {
        self.tracker.lock().creations.get(&self.key).cloned()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use temps_sandbox_providers::ImagePrepareState;

    fn tracker() -> Arc<CreationTracker> {
        Arc::new(CreationTracker::default())
    }

    fn status_of(creation: &SandboxCreation, kind: CreationStepKind) -> CreationStepStatus {
        creation
            .steps
            .iter()
            .find(|s| s.kind == kind)
            .map(|s| s.status)
            .unwrap()
    }

    #[test]
    fn steps_start_pending_in_order() {
        let tracker = tracker();
        let progress = tracker.begin(1, Some("create-0001")).unwrap();
        let creation = progress.snapshot().unwrap();
        assert_eq!(creation.status, CreationStatus::Running);
        let kinds: Vec<_> = creation.steps.iter().map(|s| s.kind).collect();
        assert_eq!(kinds, CreationStepKind::ALL.to_vec());
        assert!(creation
            .steps
            .iter()
            .all(|s| s.status == CreationStepStatus::Pending));
    }

    #[test]
    fn a_successful_create_goes_through_its_steps() {
        let tracker = tracker();
        let progress = tracker.begin(1, Some("create-0002")).unwrap();
        progress.start(CreationStepKind::CheckRequest);
        progress.done(CreationStepKind::CheckRequest, None);
        progress.done(CreationStepKind::SelectNode, Some("control plane".into()));
        progress.done(CreationStepKind::RecordSandbox, None);
        progress.sandbox_recorded("sbx_abc");
        progress.skip(CreationStepKind::SeedSource, "no source requested");
        progress.apply_phase(CreatePhaseEvent::Started(CreatePhase::Image));
        progress.apply_phase(CreatePhaseEvent::ImageAlreadyPresent);
        progress.apply_phase(CreatePhaseEvent::Finished(CreatePhase::Image));
        progress.apply_phase(CreatePhaseEvent::Started(CreatePhase::CreateContainer));
        progress.apply_phase(CreatePhaseEvent::Finished(CreatePhase::CreateContainer));
        progress.succeed("reported with the container create");

        let creation = tracker.for_sandbox("sbx_abc").expect("found by sandbox id");
        assert_eq!(creation.status, CreationStatus::Succeeded);
        assert!(creation.finished_at.is_some());
        for step in &creation.steps {
            let expected = if step.kind == CreationStepKind::SeedSource {
                CreationStepStatus::Skipped
            } else {
                CreationStepStatus::Done
            };
            assert_eq!(step.status, expected, "{:?}", step.kind);
            if expected == CreationStepStatus::Done {
                assert!(step.started_at.is_some() && step.finished_at.is_some());
                assert!(step.started_at <= step.finished_at);
            }
        }
        let pull = creation
            .steps
            .iter()
            .find(|s| s.kind == CreationStepKind::PullImage)
            .unwrap();
        assert_eq!(pull.detail.as_deref(), Some("already present on the node"));
        // Steps the provider did not report say so.
        let start = creation
            .steps
            .iter()
            .find(|s| s.kind == CreationStepKind::StartContainer)
            .unwrap();
        assert_eq!(
            start.detail.as_deref(),
            Some("reported with the container create")
        );
    }

    #[test]
    fn a_failed_create_marks_the_running_step_with_its_reason() {
        let tracker = tracker();
        let progress = tracker.begin(7, None).unwrap();
        assert!(progress.creation_id().starts_with("crt_"));
        progress.done(CreationStepKind::CheckRequest, None);
        progress.done(CreationStepKind::SelectNode, None);
        progress.done(CreationStepKind::RecordSandbox, None);
        progress.apply_phase(CreatePhaseEvent::Started(CreatePhase::Image));
        progress.apply_phase(CreatePhaseEvent::ImageProgress(ImagePrepareStatus {
            bytes_done: Some(10),
            bytes_total: Some(100),
            detail: Some("downloading".into()),
            ..ImagePrepareStatus::new("img:1", ImagePrepareState::Pulling)
        }));
        progress.fail("Failed to pull image img:1 on node 'worker-1': manifest unknown");

        let creation = tracker.get(7, progress.creation_id()).unwrap();
        assert_eq!(creation.status, CreationStatus::Failed);
        let pull = creation
            .steps
            .iter()
            .find(|s| s.kind == CreationStepKind::PullImage)
            .unwrap();
        assert_eq!(pull.status, CreationStepStatus::Failed);
        assert_eq!((pull.bytes_done, pull.bytes_total), (Some(10), Some(100)));
        assert!(pull.error.as_deref().unwrap().contains("manifest unknown"));
        assert_eq!(
            status_of(&creation, CreationStepKind::CreateContainer),
            CreationStepStatus::Pending,
            "steps after the failure were not reached"
        );
    }

    #[test]
    fn a_failure_before_any_step_ran_fails_the_first_one() {
        let tracker = tracker();
        let progress = tracker.begin(1, None).unwrap();
        progress.fail("egress_policy: unknown mode");
        let creation = progress.snapshot().unwrap();
        assert_eq!(
            status_of(&creation, CreationStepKind::CheckRequest),
            CreationStepStatus::Failed
        );
    }

    #[test]
    fn creation_ids_are_validated_and_scoped_to_their_user() {
        let tracker = tracker();
        assert!(matches!(
            tracker.begin(1, Some("short")),
            Err(CreationIdError::Invalid { .. })
        ));
        assert!(matches!(
            tracker.begin(1, Some("has spaces in it")),
            Err(CreationIdError::Invalid { .. })
        ));
        let _first = tracker.begin(1, Some("same-id-123")).unwrap();
        assert!(matches!(
            tracker.begin(1, Some("same-id-123")),
            Err(CreationIdError::InUse { user_id: 1, .. })
        ));
        // Another user's id space is separate, and reads are per user.
        let _other = tracker.begin(2, Some("same-id-123")).unwrap();
        assert!(tracker.get(3, "same-id-123").is_none());
    }

    #[test]
    fn finished_creates_are_bounded() {
        let tracker = tracker();
        let running = tracker.begin(1, Some("still-running")).unwrap();
        for i in 0..(MAX_TRACKED_CREATES + 10) {
            let p = tracker.begin(2, Some(&format!("create-{i:06}"))).unwrap();
            p.succeed("");
        }
        let inner = tracker.lock();
        assert!(inner.creations.len() <= MAX_TRACKED_CREATES);
        assert!(inner
            .creations
            .contains_key(&(1, "still-running".to_string())));
        drop(inner);
        drop(running);
    }
}
