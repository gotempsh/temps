// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Bounded, administrator-visible milestones for synchronous repository installs.
//! Messages are static so subprocess output and source credentials never reach polling clients.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

use serde::Serialize;
use thiserror::Error;
use tokio::sync::Mutex;
use utoipa::ToSchema;

const MAX_ENTRIES: usize = 64;
const RETAIN_FINISHED: Duration = Duration::from_secs(600);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, ToSchema)]
#[serde(rename_all = "lowercase")]
pub enum ProgressStatus {
    Running,
    Completed,
    Failed,
}

#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct ProgressStage {
    pub stage: &'static str,
    pub message: &'static str,
    pub status: ProgressStatus,
    pub elapsed_ms: u64,
}

#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct ProgressSnapshot {
    pub id: String,
    pub status: ProgressStatus,
    pub elapsed_ms: u64,
    pub stages: Vec<ProgressStage>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Stage {
    FetchingSource,
    WaitingForLifecycle,
    PreparingBuilder,
    InstallingDependencies,
    PreparingRuntime,
    Compiling,
    ExtractingBinary,
    StartingPlugin,
    PromotingPlugin,
}

impl Stage {
    fn label(self) -> (&'static str, &'static str) {
        match self {
            Self::FetchingSource => ("fetching_source", "Fetching repository source"),
            Self::WaitingForLifecycle => ("waiting_for_lifecycle", "Waiting for plugin operations"),
            Self::PreparingBuilder => ("preparing_builder", "Preparing isolated builder"),
            Self::InstallingDependencies => ("installing_dependencies", "Installing dependencies"),
            Self::PreparingRuntime => ("preparing_runtime", "Preparing target runtime"),
            Self::Compiling => ("compiling", "Compiling plugin"),
            Self::ExtractingBinary => ("extracting_binary", "Extracting compiled plugin"),
            Self::StartingPlugin => ("starting_plugin", "Starting plugin"),
            Self::PromotingPlugin => ("promoting_plugin", "Promoting verified plugin"),
        }
    }
}

#[derive(Debug, Error)]
pub enum ProgressError {
    #[error("Install progress ID '{id}' already exists")]
    Duplicate { id: String },
    #[error("Install progress capacity is full ({MAX_ENTRIES} active installs)")]
    Full,
}

struct StageEntry {
    stage: Stage,
    status: ProgressStatus,
    started: Instant,
    ended: Option<Instant>,
}

struct Entry {
    registration: Arc<()>,
    started: Instant,
    ended: Option<Instant>,
    status: ProgressStatus,
    stages: Vec<StageEntry>,
}

#[derive(Default)]
pub struct InstallProgressStore {
    entries: Mutex<HashMap<String, Entry>>,
}

#[derive(Clone)]
pub struct ProgressHandle {
    store: Arc<InstallProgressStore>,
    id: String,
    registration: Arc<()>,
}

impl InstallProgressStore {
    pub async fn register(self: &Arc<Self>, id: String) -> Result<ProgressHandle, ProgressError> {
        let now = Instant::now();
        let mut entries = self.entries.lock().await;
        entries.retain(|_, entry| {
            entry
                .ended
                .is_none_or(|ended| now.duration_since(ended) < RETAIN_FINISHED)
        });
        if entries.contains_key(&id) {
            return Err(ProgressError::Duplicate { id });
        }
        if entries.len() >= MAX_ENTRIES {
            let oldest_finished = entries
                .iter()
                .filter_map(|(id, entry)| entry.ended.map(|ended| (id.clone(), ended)))
                .min_by_key(|(_, ended)| *ended);
            if let Some((oldest_id, _)) = oldest_finished {
                entries.remove(&oldest_id);
            } else {
                return Err(ProgressError::Full);
            }
        }
        let registration = Arc::new(());
        entries.insert(
            id.clone(),
            Entry {
                registration: registration.clone(),
                started: now,
                ended: None,
                status: ProgressStatus::Running,
                stages: Vec::with_capacity(9),
            },
        );
        Ok(ProgressHandle {
            store: self.clone(),
            id,
            registration,
        })
    }

    pub async fn snapshot(&self, id: &str) -> Option<ProgressSnapshot> {
        let now = Instant::now();
        let mut entries = self.entries.lock().await;
        entries.retain(|_, entry| {
            entry
                .ended
                .is_none_or(|ended| now.duration_since(ended) < RETAIN_FINISHED)
        });
        let entry = entries.get(id)?;
        Some(ProgressSnapshot {
            id: id.to_string(),
            status: entry.status,
            elapsed_ms: millis(entry.started, entry.ended.unwrap_or(now)),
            stages: entry
                .stages
                .iter()
                .map(|stage| {
                    let (name, message) = stage.stage.label();
                    ProgressStage {
                        stage: name,
                        message,
                        status: stage.status,
                        elapsed_ms: millis(stage.started, stage.ended.unwrap_or(now)),
                    }
                })
                .collect(),
        })
    }
}

pub struct ProgressGuard {
    handle: ProgressHandle,
}

impl ProgressGuard {
    pub fn new(handle: ProgressHandle) -> Self {
        Self { handle }
    }
    pub fn handle(&self) -> &ProgressHandle {
        &self.handle
    }
    pub async fn finish_success(&self) {
        self.handle.finish(true).await;
    }
}

impl Drop for ProgressGuard {
    fn drop(&mut self) {
        let handle = self.handle.clone();
        tokio::spawn(async move {
            handle.finish(false).await;
        });
    }
}

impl ProgressHandle {
    pub async fn advance(&self, stage: Stage) {
        let now = Instant::now();
        let mut entries = self.store.entries.lock().await;
        let Some(entry) = entries.get_mut(&self.id) else {
            return;
        };
        if !Arc::ptr_eq(&entry.registration, &self.registration)
            || entry.status != ProgressStatus::Running
            || entry.stages.last().is_some_and(|last| last.stage == stage)
        {
            return;
        }
        if let Some(previous) = entry
            .stages
            .last_mut()
            .filter(|previous| previous.status == ProgressStatus::Running)
        {
            previous.status = ProgressStatus::Completed;
            previous.ended = Some(now);
        }
        entry.stages.push(StageEntry {
            stage,
            status: ProgressStatus::Running,
            started: now,
            ended: None,
        });
    }

    pub async fn complete_current(&self) {
        let now = Instant::now();
        let mut entries = self.store.entries.lock().await;
        let Some(entry) = entries.get_mut(&self.id) else {
            return;
        };
        if !Arc::ptr_eq(&entry.registration, &self.registration)
            || entry.status != ProgressStatus::Running
        {
            return;
        }
        if let Some(last) = entry
            .stages
            .last_mut()
            .filter(|last| last.status == ProgressStatus::Running)
        {
            last.status = ProgressStatus::Completed;
            last.ended = Some(now);
        }
    }

    pub async fn finish(&self, success: bool) {
        let now = Instant::now();
        let mut entries = self.store.entries.lock().await;
        let Some(entry) = entries.get_mut(&self.id) else {
            return;
        };
        if !Arc::ptr_eq(&entry.registration, &self.registration)
            || entry.status != ProgressStatus::Running
        {
            return;
        }
        entry.status = if success {
            ProgressStatus::Completed
        } else {
            ProgressStatus::Failed
        };
        entry.ended = Some(now);
        if let Some(last) = entry
            .stages
            .last_mut()
            .filter(|last| last.status == ProgressStatus::Running)
        {
            last.status = entry.status;
            last.ended = Some(now);
        }
    }
}

fn millis(start: Instant, end: Instant) -> u64 {
    end.duration_since(start).as_millis().min(u64::MAX as u128) as u64
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn stages_advance_in_order_and_fail_at_actual_stage() {
        let store = Arc::new(InstallProgressStore::default());
        let handle = store.register("first".into()).await.expect("register");
        handle.advance(Stage::FetchingSource).await;
        handle.advance(Stage::PreparingBuilder).await;
        handle.advance(Stage::InstallingDependencies).await;
        handle.finish(false).await;
        let snapshot = store.snapshot("first").await.expect("snapshot");
        assert_eq!(snapshot.status, ProgressStatus::Failed);
        assert_eq!(
            snapshot.stages.iter().map(|s| s.stage).collect::<Vec<_>>(),
            [
                "fetching_source",
                "preparing_builder",
                "installing_dependencies"
            ]
        );
        assert_eq!(snapshot.stages[0].status, ProgressStatus::Completed);
        assert_eq!(snapshot.stages[2].status, ProgressStatus::Failed);
        handle.advance(Stage::Compiling).await;
        assert_eq!(
            store
                .snapshot("first")
                .await
                .expect("terminal")
                .stages
                .len(),
            3
        );
    }

    #[tokio::test]
    async fn abandoned_request_fails_without_marking_completed_source_as_failed() {
        let store = Arc::new(InstallProgressStore::default());
        let handle = store.register("abandoned".into()).await.expect("register");
        handle.advance(Stage::FetchingSource).await;
        handle.complete_current().await;
        let guard = ProgressGuard::new(handle);
        drop(guard);
        tokio::task::yield_now().await;
        let snapshot = store.snapshot("abandoned").await.expect("snapshot");
        assert_eq!(snapshot.status, ProgressStatus::Failed);
        assert_eq!(snapshot.stages[0].status, ProgressStatus::Completed);
    }

    #[tokio::test]
    async fn ids_are_isolated_bounded_and_expired_entries_are_removed() {
        let store = Arc::new(InstallProgressStore::default());
        let first = store.register("first".into()).await.expect("first");
        let second = store.register("second".into()).await.expect("second");
        assert!(matches!(
            store.register("first".into()).await,
            Err(ProgressError::Duplicate { .. })
        ));
        first.advance(Stage::FetchingSource).await;
        second.advance(Stage::PreparingBuilder).await;
        first.finish(true).await;
        assert_eq!(
            store
                .snapshot("first")
                .await
                .expect("first snapshot")
                .status,
            ProgressStatus::Completed
        );
        assert_eq!(
            store
                .snapshot("second")
                .await
                .expect("second snapshot")
                .stages[0]
                .stage,
            "preparing_builder"
        );
        assert!(store.snapshot("unknown").await.is_none());
        for index in 2..MAX_ENTRIES {
            store.register(index.to_string()).await.expect("capacity");
        }
        store
            .register("overflow".into())
            .await
            .expect("evict finished");
        assert!(store.snapshot("first").await.is_none());
        assert!(store.snapshot("second").await.is_some());
        assert!(matches!(
            store.register("new".into()).await,
            Err(ProgressError::Full)
        ));
        store
            .entries
            .lock()
            .await
            .get_mut("second")
            .expect("second entry")
            .ended = Some(Instant::now() - RETAIN_FINISHED);
        assert!(store.snapshot("second").await.is_none());
        assert!(store.register("new".into()).await.is_ok());
    }

    #[tokio::test]
    async fn more_than_capacity_finished_installs_evict_oldest_without_touching_active() {
        let store = Arc::new(InstallProgressStore::default());
        let active = store.register("active".into()).await.expect("active");
        active.advance(Stage::FetchingSource).await;
        for index in 0..(MAX_ENTRIES * 2) {
            let id = format!("finished-{index}");
            let handle = store.register(id).await.expect("finished slot");
            handle.advance(Stage::FetchingSource).await;
            handle.finish(index % 2 == 0).await;
        }
        assert_eq!(store.entries.lock().await.len(), MAX_ENTRIES);
        assert!(store.snapshot("active").await.is_some());
        assert!(store.snapshot("finished-0").await.is_none());
        assert!(store.snapshot("finished-64").await.is_none());
        assert_eq!(
            store
                .snapshot("finished-65")
                .await
                .expect("newer finished")
                .status,
            ProgressStatus::Failed
        );
        assert_eq!(
            store
                .snapshot("finished-126")
                .await
                .expect("newest completed")
                .status,
            ProgressStatus::Completed
        );
        assert!(matches!(
            store.register("finished-127".into()).await,
            Err(ProgressError::Duplicate { .. })
        ));
        store
            .register("next".into())
            .await
            .expect("evict another finished");
        assert!(store.snapshot("active").await.is_some());
    }

    #[tokio::test]
    async fn full_active_capacity_rejects_another_install() {
        let store = Arc::new(InstallProgressStore::default());
        for index in 0..MAX_ENTRIES {
            store
                .register(index.to_string())
                .await
                .expect("active slot");
        }
        assert!(matches!(
            store.register("extra".into()).await,
            Err(ProgressError::Full)
        ));
        assert_eq!(store.entries.lock().await.len(), MAX_ENTRIES);
    }

    #[tokio::test]
    async fn evicted_registration_cannot_mutate_reused_id() {
        let store = Arc::new(InstallProgressStore::default());
        let old = store
            .register("reused".into())
            .await
            .expect("old registration");
        old.advance(Stage::FetchingSource).await;
        old.finish(true).await;
        let old_guard = ProgressGuard::new(old.clone());
        for index in 0..(MAX_ENTRIES - 1) {
            store
                .register(index.to_string())
                .await
                .expect("fill capacity");
        }
        let evict = store
            .register("evict".into())
            .await
            .expect("evict old entry");
        assert!(store.snapshot("reused").await.is_none());
        evict.finish(true).await;
        let current = store.register("reused".into()).await.expect("reuse ID");
        current.advance(Stage::WaitingForLifecycle).await;
        old.advance(Stage::Compiling).await;
        old.complete_current().await;
        old.finish(true).await;
        old.finish(false).await;
        drop(old_guard);
        tokio::task::yield_now().await;
        let snapshot = store.snapshot("reused").await.expect("current entry");
        assert_eq!(snapshot.status, ProgressStatus::Running);
        assert_eq!(snapshot.stages.len(), 1);
        assert_eq!(snapshot.stages[0].stage, "waiting_for_lifecycle");
        assert_eq!(snapshot.stages[0].status, ProgressStatus::Running);
        current.advance(Stage::PreparingBuilder).await;
        current.finish(false).await;
        let snapshot = store
            .snapshot("reused")
            .await
            .expect("current terminal entry");
        assert_eq!(snapshot.status, ProgressStatus::Failed);
        assert_eq!(snapshot.stages[1].stage, "preparing_builder");
    }
}
