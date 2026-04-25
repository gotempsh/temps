//! Tracks background jobs spawned via `exec_detached`. Each job owns a
//! `tokio::task::JoinHandle` and an `Arc<Mutex<JobState>>` that the task
//! mutates as output arrives and when the command exits.
//!
//! Why in-memory (not DB): jobs are process-local and ephemeral — they
//! die with the server. A user reconnecting after a restart should see
//! the sandbox but not the previous run's background jobs. This matches
//! the `execDetached` contract from `@vercel/sandbox`, which also has no
//! durability guarantees.

use std::collections::HashMap;
use std::sync::Arc;

use chrono::{DateTime, Utc};
use tokio::sync::{broadcast, Mutex};
use tokio::task::JoinHandle;

use temps_agents::sandbox::ExecStream;

use crate::error::SandboxError;

/// Capacity for each job's per-stream broadcast channel. Lines older than
/// this drop silently for lagging subscribers — the full record lives in
/// `JobState`, so SSE reconnects can re-fetch the history via `job_status`
/// and only miss lines produced during their reconnect gap.
const LOG_CHANNEL_CAPACITY: usize = 1024;

/// A single log event emitted by a detached job.
#[derive(Debug, Clone)]
pub struct JobLogEvent {
    pub stream: ExecStream,
    pub line: String,
}

/// Lifecycle of a background job.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum JobStatus {
    Running,
    Exited { exit_code: i32 },
    Failed { reason: String },
}

/// Mutable state for a background job. Appended to as stdout arrives;
/// the terminal status is set when the task exits.
#[derive(Debug)]
pub struct JobState {
    pub status: JobStatus,
    pub stdout: String,
    pub stderr: String,
}

impl Default for JobState {
    fn default() -> Self {
        Self {
            status: JobStatus::Running,
            stdout: String::new(),
            stderr: String::new(),
        }
    }
}

pub struct Job {
    /// Opaque ID surfaced to callers. Format `job_` + 16 hex chars.
    pub id: String,
    /// Mutex protecting live state. Cloneable `Arc` so the background
    /// task and the HTTP handlers share the same state.
    pub state: Arc<Mutex<JobState>>,
    /// Handle used to `abort()` the task on sandbox teardown. Once the
    /// task exits naturally the handle is still valid — awaiting it is
    /// a no-op but also not useful, so we simply abort on cleanup.
    pub task: JoinHandle<()>,
    /// Per-job broadcast channel carrying live stdout/stderr events.
    /// SSE subscribers (`/jobs/{id}/logs`) grab a `Receiver` from here
    /// and consume events as they're produced. Lines also land in
    /// `state.stdout`/`state.stderr` for late-joining snapshots.
    pub log_tx: broadcast::Sender<JobLogEvent>,
    /// `pkill -f` pattern that matches the process(es) started for this
    /// job. Set by `exec_detached` so `kill_job` can terminate the
    /// underlying process inside the container (abort()ing the tokio
    /// task alone leaves the child running). Empty string disables the
    /// pkill pass — the task is still aborted.
    pub kill_pattern: String,
    /// Human-readable command the user submitted (argv joined by space).
    /// Used only for the jobs list UI; the real argv was already consumed
    /// by exec. Kept truncated-free here so the UI can ellipsize itself.
    pub cmd_display: String,
    /// Wall-clock start time for the jobs list. Ephemeral — jobs don't
    /// survive server restarts, so this needn't persist.
    pub started_at: DateTime<Utc>,
}

/// Lightweight row for `list_jobs`. Excludes stdout/stderr so listing a
/// sandbox with noisy long-lived jobs doesn't drag a megabyte into the
/// response. Consumers fetch full state via `status` / `logs`.
#[derive(Debug, Clone)]
pub struct JobSummary {
    pub id: String,
    pub status: JobStatus,
    pub cmd: String,
    pub started_at: DateTime<Utc>,
}

/// Registry of background jobs keyed by sandbox internal ID. All
/// operations are cheap (RwLock over a HashMap of Arcs).
pub struct JobTracker {
    jobs: Mutex<HashMap<i32, HashMap<String, Job>>>,
}

impl JobTracker {
    pub fn new() -> Self {
        Self {
            jobs: Mutex::new(HashMap::new()),
        }
    }

    /// Register a newly-spawned job. Returns its ID.
    pub async fn insert(&self, sandbox_id: i32, job: Job) -> String {
        let id = job.id.clone();
        let mut map = self.jobs.lock().await;
        map.entry(sandbox_id)
            .or_insert_with(HashMap::new)
            .insert(id.clone(), job);
        id
    }

    /// Snapshot a job's current state. Returns `JobNotFound` when the
    /// job is unknown — separate from `SandboxNotFound` so callers can
    /// distinguish.
    pub async fn status(
        &self,
        sandbox_public_id: &str,
        sandbox_id: i32,
        job_id: &str,
    ) -> Result<JobState, SandboxError> {
        let map = self.jobs.lock().await;
        let job = map
            .get(&sandbox_id)
            .and_then(|m| m.get(job_id))
            .ok_or_else(|| SandboxError::JobNotFound {
                sandbox_id: sandbox_public_id.to_string(),
                job_id: job_id.to_string(),
            })?;
        let state = job.state.lock().await;
        Ok(JobState {
            status: state.status.clone(),
            stdout: state.stdout.clone(),
            stderr: state.stderr.clone(),
        })
    }

    /// Subscribe to the live log stream of an existing job. Returns
    /// `JobNotFound` when the job is unknown. The returned receiver
    /// sees events produced AFTER subscription — historical lines must
    /// be fetched separately via `status`.
    pub async fn subscribe(
        &self,
        sandbox_public_id: &str,
        sandbox_id: i32,
        job_id: &str,
    ) -> Result<broadcast::Receiver<JobLogEvent>, SandboxError> {
        let map = self.jobs.lock().await;
        let job = map
            .get(&sandbox_id)
            .and_then(|m| m.get(job_id))
            .ok_or_else(|| SandboxError::JobNotFound {
                sandbox_id: sandbox_public_id.to_string(),
                job_id: job_id.to_string(),
            })?;
        Ok(job.log_tx.subscribe())
    }

    /// Construct a fresh broadcast channel for a new job. The sender is
    /// stored on the [`Job`]; callers clone it into the exec callback
    /// so every line reaches both the in-memory accumulator and any
    /// live SSE subscribers.
    pub fn new_log_channel() -> broadcast::Sender<JobLogEvent> {
        let (tx, _rx) = broadcast::channel(LOG_CHANNEL_CAPACITY);
        tx
    }

    /// Abort a single job's task and remove it from the registry.
    /// Returns the job's `kill_pattern` so the caller can additionally
    /// `pkill -f` the underlying process inside the sandbox container.
    /// Returns `JobNotFound` when the job id is unknown.
    pub async fn kill_and_remove(
        &self,
        sandbox_public_id: &str,
        sandbox_id: i32,
        job_id: &str,
    ) -> Result<String, SandboxError> {
        let mut map = self.jobs.lock().await;
        let jobs = map
            .get_mut(&sandbox_id)
            .ok_or_else(|| SandboxError::JobNotFound {
                sandbox_id: sandbox_public_id.to_string(),
                job_id: job_id.to_string(),
            })?;
        let job = jobs
            .remove(job_id)
            .ok_or_else(|| SandboxError::JobNotFound {
                sandbox_id: sandbox_public_id.to_string(),
                job_id: job_id.to_string(),
            })?;
        job.task.abort();
        // Flip status so any late `job_status` read sees the terminal state.
        {
            let mut s = job.state.lock().await;
            s.status = JobStatus::Failed {
                reason: "killed".into(),
            };
        }
        Ok(job.kill_pattern.clone())
    }

    /// Snapshot every job registered for a sandbox. Returns an empty vec
    /// when nothing has been spawned — *not* an error — so the UI can
    /// render "no background jobs" without distinguishing from a missing
    /// sandbox (separate 404 handling upstream). Newest first.
    pub async fn list(&self, sandbox_id: i32) -> Vec<JobSummary> {
        let map = self.jobs.lock().await;
        let Some(jobs) = map.get(&sandbox_id) else {
            return Vec::new();
        };
        let mut out = Vec::with_capacity(jobs.len());
        for job in jobs.values() {
            let state = job.state.lock().await;
            out.push(JobSummary {
                id: job.id.clone(),
                status: state.status.clone(),
                cmd: job.cmd_display.clone(),
                started_at: job.started_at,
            });
        }
        out.sort_by_key(|job| std::cmp::Reverse(job.started_at));
        out
    }

    /// Abort every job for a sandbox and drop all state. Called during
    /// sandbox teardown so background dev servers don't outlive the
    /// container.
    pub async fn abort_all(&self, sandbox_id: i32) {
        let mut map = self.jobs.lock().await;
        if let Some(jobs) = map.remove(&sandbox_id) {
            for (_, job) in jobs {
                job.task.abort();
            }
        }
    }

    /// Generate a new job ID. Same format as sandbox public IDs but
    /// with `job_` prefix.
    pub fn new_job_id() -> String {
        use rand::RngCore;
        let mut bytes = [0u8; 8];
        rand::thread_rng().fill_bytes(&mut bytes);
        format!("job_{}", hex::encode(bytes))
    }
}

impl Default for JobTracker {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_state_is_running() {
        let s = JobState::default();
        assert_eq!(s.status, JobStatus::Running);
        assert!(s.stdout.is_empty());
        assert!(s.stderr.is_empty());
    }

    #[test]
    fn new_job_id_has_prefix() {
        let id = JobTracker::new_job_id();
        assert!(id.starts_with("job_"));
        let rest = id.strip_prefix("job_").unwrap();
        assert_eq!(rest.len(), 16);
    }

    #[tokio::test]
    async fn status_returns_job_not_found_for_missing() {
        let tracker = JobTracker::new();
        let err = tracker.status("sbx_public", 42, "job_missing").await;
        assert!(matches!(err, Err(SandboxError::JobNotFound { .. })));
    }

    #[tokio::test]
    async fn abort_all_is_idempotent_for_unknown_sandbox() {
        let tracker = JobTracker::new();
        tracker.abort_all(999).await; // must not panic
    }

    #[tokio::test]
    async fn insert_then_status_returns_running() {
        let tracker = JobTracker::new();
        let state = Arc::new(Mutex::new(JobState::default()));
        // Spawn a trivial task that finishes immediately but doesn't
        // mutate state — we only care that status reports Running.
        let task = tokio::spawn(async {});
        let id = JobTracker::new_job_id();
        tracker
            .insert(
                1,
                Job {
                    id: id.clone(),
                    state: state.clone(),
                    task,
                    log_tx: JobTracker::new_log_channel(),
                    kill_pattern: String::new(),
                    cmd_display: String::new(),
                    started_at: Utc::now(),
                },
            )
            .await;
        let got = tracker.status("sbx_x", 1, &id).await.unwrap();
        assert_eq!(got.status, JobStatus::Running);
    }
}
