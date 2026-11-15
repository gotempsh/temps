// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Anonymous product telemetry for backup runs.
//!
//! The backup executor already publishes `BackupStarted`, `BackupCompleted`
//! and `BackupFailed` on the shared job queue. [`BackupTelemetry`] turns those
//! into `backup_succeeded` / `backup_failed` events, so every run is covered
//! (scheduled, manual, every engine) without touching the executor.
//!
//! Only the engine key, a coarse duration band, a coarse size band and a fixed
//! failure code are sent. Backup IDs, S3 locations and error messages stay on
//! the instance.

use std::collections::HashMap;
use std::time::Instant;

use temps_core::telemetry::{TelemetryEvent, TelemetryEventKind};
use temps_core::Job;

/// Always subscribe, including when telemetry is disabled at startup. The reporter
/// applies the current operator preference per event, so enabling it at runtime
/// works without restarting the backup plugin.
pub(crate) fn spawn_backup_telemetry(
    queue: &dyn temps_core::JobQueue,
    reporter: std::sync::Arc<dyn temps_core::telemetry::TelemetryReporter>,
) -> tokio::task::JoinHandle<()> {
    let mut receiver = queue.subscribe();
    tokio::spawn(async move {
        let mut telemetry = BackupTelemetry::new();
        loop {
            match receiver.recv().await {
                Ok(job) => {
                    if let Some(event) = telemetry.on_job(&job) {
                        reporter.report(event);
                    }
                }
                Err(temps_core::QueueError::ChannelClosed) => break,
                // Queue lag affects only best-effort telemetry.
                Err(_) => continue,
            }
        }
    })
}

/// Upper bound on in-flight start times kept for duration measurement. Far
/// above the executor's concurrency; when exceeded (e.g. completions were
/// lost to queue lag) the map is cleared and those runs are reported without
/// a duration.
const MAX_TRACKED_RUNS: usize = 1024;

/// Converts backup lifecycle jobs into telemetry events.
#[derive(Default)]
pub struct BackupTelemetry {
    started: HashMap<i32, Instant>,
}

impl BackupTelemetry {
    pub fn new() -> Self {
        Self::default()
    }

    /// Feed one job from the queue. Returns the event to report, if any.
    pub fn on_job(&mut self, job: &Job) -> Option<TelemetryEvent> {
        self.on_job_at(job, Instant::now())
    }

    fn on_job_at(&mut self, job: &Job, now: Instant) -> Option<TelemetryEvent> {
        match job {
            Job::BackupStarted(started) => {
                if self.started.len() >= MAX_TRACKED_RUNS {
                    self.started.clear();
                }
                self.started.insert(started.backup_id, now);
                None
            }
            Job::BackupCompleted(completed) => {
                let event = TelemetryEvent::new(TelemetryEventKind::BackupSucceeded)
                    .with("engine", engine_label(&completed.engine))
                    .with_opt("size_bucket", completed.size_bytes.map(size_bucket));
                Some(self.with_duration(event, completed.backup_id, now))
            }
            Job::BackupFailed(failed) => {
                let event = TelemetryEvent::new(TelemetryEventKind::BackupFailed)
                    .with("engine", engine_label(&failed.engine))
                    .with_failure_from_message(&failed.error_message);
                Some(self.with_duration(event, failed.backup_id, now))
            }
            _ => None,
        }
    }

    fn with_duration(
        &mut self,
        event: TelemetryEvent,
        backup_id: i32,
        now: Instant,
    ) -> TelemetryEvent {
        match self.started.remove(&backup_id) {
            Some(started) => event.with_duration(now.saturating_duration_since(started)),
            None => event,
        }
    }
}

/// Engine keys are code-defined (`postgres_pgdump`, `redis`, ...). Anything
/// that doesn't look like one is sent as `other` rather than verbatim.
fn engine_label(engine: &str) -> String {
    match engine {
        "control_plane" | "postgres" | "mariadb" | "mongodb" | "redis" | "s3"
        | "postgres_pgdump" | "postgres_walg" | "postgres_cluster" | "worker_agent"
        | "s3_mirror" | "mariadb_dump" | "mariadb_physical" => engine.to_string(),
        _ => "other".to_string(),
    }
}

/// Outcome for legacy operations that do not publish executor lifecycle jobs.
pub(crate) fn operation_outcome(
    restore: bool,
    engine: &str,
    elapsed: std::time::Duration,
    outcome: Result<(), String>,
) -> TelemetryEvent {
    let kind = match (restore, outcome.is_ok()) {
        (false, true) => TelemetryEventKind::BackupSucceeded,
        (false, false) => TelemetryEventKind::BackupFailed,
        (true, true) => TelemetryEventKind::RestoreSucceeded,
        (true, false) => TelemetryEventKind::RestoreFailed,
    };
    let event = TelemetryEvent::new(kind)
        .with("engine", engine_label(engine))
        .with_duration(elapsed);
    match outcome {
        Ok(()) => event,
        Err(message) => event.with_failure_from_message(&message),
    }
}

/// Coarse size band. Exact sizes would fingerprint an instance's data volume.
fn size_bucket(bytes: i64) -> &'static str {
    const MB: i64 = 1024 * 1024;
    match bytes {
        b if b < 10 * MB => "<10MB",
        b if b < 100 * MB => "10-100MB",
        b if b < 1024 * MB => "100MB-1GB",
        b if b < 10 * 1024 * MB => "1-10GB",
        _ => ">10GB",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;
    use temps_core::jobs::{BackupCompletedJob, BackupFailedJob, BackupStartedJob};

    fn started(id: i32) -> Job {
        Job::BackupStarted(BackupStartedJob {
            backup_id: id,
            engine: "postgres_pgdump".to_string(),
        })
    }

    #[tokio::test]
    async fn subscription_survives_runtime_opt_in() {
        use std::sync::{
            atomic::{AtomicBool, AtomicUsize, Ordering},
            Arc, Mutex,
        };
        use temps_core::telemetry::TelemetryReporter;
        use temps_core::{JobQueue, JobReceiver, QueueError};
        struct Receiver(tokio::sync::mpsc::Receiver<Job>);
        #[async_trait::async_trait]
        impl JobReceiver for Receiver {
            async fn recv(&mut self) -> Result<Job, QueueError> {
                self.0.recv().await.ok_or(QueueError::ChannelClosed)
            }
        }
        struct Queue {
            tx: tokio::sync::mpsc::Sender<Job>,
            rx: Mutex<Option<tokio::sync::mpsc::Receiver<Job>>>,
        }
        #[async_trait::async_trait]
        impl JobQueue for Queue {
            async fn send(&self, job: Job) -> Result<(), QueueError> {
                self.tx
                    .send(job)
                    .await
                    .map_err(|_| QueueError::ChannelClosed)
            }
            fn subscribe(&self) -> Box<dyn JobReceiver> {
                Box::new(Receiver(self.rx.lock().unwrap().take().unwrap()))
            }
        }
        #[derive(Default)]
        struct Reporter {
            enabled: AtomicBool,
            attempts: AtomicUsize,
            events: Mutex<Vec<TelemetryEvent>>,
        }
        impl TelemetryReporter for Reporter {
            fn is_enabled(&self) -> bool {
                self.enabled.load(Ordering::SeqCst)
            }
            fn report(&self, event: TelemetryEvent) {
                if self.is_enabled() {
                    self.events.lock().unwrap().push(event);
                }
                self.attempts.fetch_add(1, Ordering::SeqCst);
            }
        }
        let (tx, rx) = tokio::sync::mpsc::channel(4);
        let queue = Queue {
            tx,
            rx: Mutex::new(Some(rx)),
        };
        let reporter = Arc::new(Reporter::default());
        let task = spawn_backup_telemetry(&queue, reporter.clone());
        let completed = |id| {
            Job::BackupCompleted(BackupCompletedJob {
                backup_id: id,
                engine: "redis".into(),
                s3_location: "s3://sample/backup".into(),
                size_bytes: None,
            })
        };
        queue.send(completed(1)).await.unwrap();
        tokio::time::timeout(Duration::from_secs(1), async {
            while reporter.attempts.load(Ordering::SeqCst) == 0 {
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
        assert!(reporter.events.lock().unwrap().is_empty());
        reporter.enabled.store(true, Ordering::SeqCst);
        queue.send(completed(2)).await.unwrap();
        drop(queue);
        task.await.unwrap();
        let events = reporter.events.lock().unwrap();
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].event_type, "backup_succeeded");
    }

    #[test]
    fn legacy_operation_outcomes_are_anonymous_and_cover_each_result() {
        for (restore, success, expected) in [
            (false, true, "backup_succeeded"),
            (false, false, "backup_failed"),
            (true, true, "restore_succeeded"),
            (true, false, "restore_failed"),
        ] {
            let event = operation_outcome(
                restore,
                "private_engine",
                Duration::from_secs(90),
                if success {
                    Ok(())
                } else {
                    Err("upload to private.example failed: AccessDenied".into())
                },
            );
            assert_eq!(event.event_type, expected);
            assert_eq!(event.properties["engine"], "other");
            assert_eq!(event.properties["duration_bucket"], "1-5m");
            assert!(!serde_json::to_string(&event).unwrap().contains("private"));
        }
    }

    #[test]
    fn completed_run_reports_engine_duration_and_size() {
        let mut telemetry = BackupTelemetry::new();
        let t0 = Instant::now();
        assert!(telemetry.on_job_at(&started(7), t0).is_none());

        let event = telemetry
            .on_job_at(
                &Job::BackupCompleted(BackupCompletedJob {
                    backup_id: 7,
                    engine: "postgres_pgdump".to_string(),
                    s3_location: "s3://private-bucket/backups/7.dump".to_string(),
                    size_bytes: Some(250 * 1024 * 1024),
                }),
                t0 + Duration::from_secs(90),
            )
            .expect("completion is reported");

        assert_eq!(event.event_type, "backup_succeeded");
        assert_eq!(event.properties["engine"], "postgres_pgdump");
        assert_eq!(event.properties["duration_bucket"], "1-5m");
        assert_eq!(event.properties["size_bucket"], "100MB-1GB");
        let serialized = serde_json::to_string(&event).unwrap();
        assert!(!serialized.contains("private-bucket"));
    }

    #[test]
    fn failed_run_reports_code_not_message() {
        let mut telemetry = BackupTelemetry::new();
        let event = telemetry
            .on_job(&Job::BackupFailed(BackupFailedJob {
                backup_id: 9,
                engine: "redis".to_string(),
                error_message: "upload to s3://acme-backups failed: AccessDenied".to_string(),
            }))
            .expect("failure is reported");

        assert_eq!(event.event_type, "backup_failed");
        assert_eq!(event.properties["engine"], "redis");
        assert_eq!(event.properties["failure_code"], "permission_denied");
        // No BackupStarted was seen, so no duration is guessed.
        assert!(!event.properties.contains_key("duration_bucket"));
        let serialized = serde_json::to_string(&event).unwrap();
        assert!(!serialized.contains("acme-backups"));
    }

    #[test]
    fn unexpected_engine_values_are_not_sent_verbatim() {
        assert_eq!(engine_label("postgres_walg"), "postgres_walg");
        assert_eq!(engine_label("Customer DB (prod)"), "other");
        assert_eq!(engine_label(""), "other");
    }

    #[test]
    fn tracked_starts_are_bounded() {
        let mut telemetry = BackupTelemetry::new();
        let now = Instant::now();
        for id in 0..(MAX_TRACKED_RUNS as i32 + 10) {
            telemetry.on_job_at(&started(id), now);
        }
        assert!(telemetry.started.len() <= MAX_TRACKED_RUNS);
    }

    #[test]
    fn unrelated_jobs_are_ignored() {
        let mut telemetry = BackupTelemetry::new();
        assert!(telemetry
            .on_job(&Job::CustomDomainAdded("example.com".to_string()))
            .is_none());
    }
}
