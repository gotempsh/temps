// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Push backup lifecycle events (started/completed/failed) to Cloud as they
//! happen, instead of waiting for [`crate::backup_mirror`]'s next poll to
//! notice. That sweep remains the source of truth for what actually
//! happened -- a dropped or failed push here costs Cloud a stale
//! "processing" indicator until the next sweep tick, never an incorrect or
//! lost backup record.

use std::sync::Arc;

use temps_cloud_protocol::{BackupLifecycleEventRequest, BackupLifecycleStage};
use temps_core::{Job, JobQueue};
use tracing::{debug, error, info, warn};

use crate::service::CloudService;

/// Subscribe to the job queue and forward backup lifecycle events to Cloud.
/// Runs for the lifetime of the process; there is no shutdown signal because
/// the underlying `JobQueue` receiver simply stops yielding jobs on shutdown.
pub async fn run(service: Arc<CloudService>, queue: Arc<dyn JobQueue>) {
    info!("Cloud backup lifecycle notifier started");
    let mut receiver = queue.subscribe();
    loop {
        match receiver.recv().await {
            Ok(job) => {
                let Some(stage_job) = to_lifecycle_job(&job) else {
                    continue;
                };
                if !service.link().is_linked() {
                    debug!("Cloud is not linked; skipping backup lifecycle push");
                    continue;
                }
                let Some(instance_id) = service.link().instance_id() else {
                    debug!("Cloud link has no instance_id yet; skipping backup lifecycle push");
                    continue;
                };

                let event = BackupLifecycleEventRequest {
                    instance_id,
                    backup_id: stage_job.backup_id,
                    engine: stage_job.engine,
                    stage: stage_job.stage,
                    occurred_at: chrono::Utc::now(),
                    s3_location: stage_job.s3_location,
                    size_bytes: stage_job.size_bytes,
                    error_message: stage_job.error_message,
                };

                match service.link().notify_backup_lifecycle(&event).await {
                    Ok(_) => debug!(
                        backup_id = event.backup_id,
                        stage = ?event.stage,
                        "reported backup lifecycle event to Cloud",
                    ),
                    Err(e) => warn!(
                        backup_id = event.backup_id,
                        stage = ?event.stage,
                        error = %e,
                        "failed to report backup lifecycle event to Cloud; the mirror sweep will still catch the outcome",
                    ),
                }
            }
            Err(e) => {
                error!(
                    "backup lifecycle notifier failed to receive job from queue: {}",
                    e
                );
                tokio::time::sleep(std::time::Duration::from_millis(100)).await;
            }
        }
    }
}

/// Intermediate, borrow-free extraction of the fields needed to build a
/// [`BackupLifecycleEventRequest`], decoupled from the enum match so the
/// match arms below stay a single line each.
struct LifecycleJob {
    backup_id: i32,
    engine: String,
    stage: BackupLifecycleStage,
    s3_location: Option<String>,
    size_bytes: Option<i64>,
    error_message: Option<String>,
}

fn to_lifecycle_job(job: &Job) -> Option<LifecycleJob> {
    match job {
        Job::BackupStarted(j) => Some(LifecycleJob {
            backup_id: j.backup_id,
            engine: j.engine.clone(),
            stage: BackupLifecycleStage::Started,
            s3_location: None,
            size_bytes: None,
            error_message: None,
        }),
        Job::BackupCompleted(j) => Some(LifecycleJob {
            backup_id: j.backup_id,
            engine: j.engine.clone(),
            stage: BackupLifecycleStage::Completed,
            s3_location: Some(j.s3_location.clone()),
            size_bytes: j.size_bytes,
            error_message: None,
        }),
        Job::BackupFailed(j) => Some(LifecycleJob {
            backup_id: j.backup_id,
            engine: j.engine.clone(),
            stage: BackupLifecycleStage::Failed,
            s3_location: None,
            size_bytes: None,
            error_message: Some(bound_error_message(&j.error_message)),
        }),
        _ => None,
    }
}

/// Failure reasons on this path come from raw engine stderr, which is not
/// scrubbed of credentials at every call site (`s3_mirror`'s own reason
/// string is the one place that already had to special-case this). This is
/// the first path that ships that text off-box, so bound it defensively --
/// this caps exposure, it does not replace fixing redaction at the source.
const MAX_ERROR_MESSAGE_LEN: usize = 500;

fn bound_error_message(message: &str) -> String {
    if message.len() <= MAX_ERROR_MESSAGE_LEN {
        return message.to_string();
    }
    let mut truncated: String = message.chars().take(MAX_ERROR_MESSAGE_LEN).collect();
    truncated.push_str(" [truncated]");
    truncated
}
