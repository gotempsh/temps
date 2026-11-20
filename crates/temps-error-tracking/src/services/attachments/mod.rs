// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Attachments sent with error events and feedback (ADR 111, D4).
//!
//! Sentry SDKs send files with an event (`Sentry.getCurrentScope()
//! .addAttachment(...)`), and the feedback widget sends its screenshot the
//! same way. Ingest validates them in the envelope reader and offers them to
//! a bounded [`queue::AttachmentQueue`]; one background task stores them in
//! the project's blob storage, after reserving their size against the
//! project's quota, and records them in `error_attachments`.
//!
//! An attachment lives as long as what it was sent with: the sweeper removes
//! those whose error event has expired, and those of deleted projects;
//! deleting a feedback deletes its attachments.

pub mod queue;

#[cfg(test)]
pub(crate) mod tests;

use std::sync::atomic::Ordering;
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;

use axum::body::Bytes;
use chrono::{DateTime, Utc};
use sea_orm::{
    ConnectionTrait, DatabaseBackend, DatabaseConnection, FromQueryResult, Statement,
    TransactionTrait,
};
use serde::{Deserialize, Serialize};
use temps_core::{PayloadBlobError, PayloadBlobStore};
use utoipa::ToSchema;

use super::types::ErrorTrackingError;
pub use queue::{
    AttachmentBatch, AttachmentCounts, AttachmentQueue, AttachmentReceiver, AttachmentStats,
    IncomingAttachment, OfferOutcome,
};

/// Largest single attachment stored.
pub const MAX_ATTACHMENT_BYTES: usize = 2 * 1024 * 1024;
/// Largest total of the attachments of one event or feedback.
pub const MAX_EVENT_ATTACHMENT_BYTES: usize = 4 * 1024 * 1024;
/// Most attachments stored for one event or feedback.
pub const MAX_ATTACHMENTS_PER_EVENT: usize = 8;
/// Attachment storage a project may use until the admin changes it.
pub const DEFAULT_QUOTA_BYTES: i64 = 256 * 1024 * 1024;
/// Largest quota the admin can set.
pub const MAX_QUOTA_BYTES: i64 = 100 * 1024 * 1024 * 1024;
/// Longest stored file name, in characters.
pub const MAX_FILENAME_CHARS: usize = 200;
/// Longest stored content type, in characters.
pub const MAX_CONTENT_TYPE_CHARS: usize = 100;
/// The only attachment type stored: a plain file. Minidumps, crash reports
/// and other native types belong to D7.
pub const STORED_ATTACHMENT_TYPE: &str = "event.attachment";

/// How often the sweeper looks for attachments to delete.
pub const SWEEP_INTERVAL: Duration = Duration::from_secs(600);
/// Attachments checked per sweep step.
pub const SWEEP_BATCH: u64 = 100;
/// Sweep steps per kind and pass, so one pass is bounded.
pub const MAX_SWEEP_BATCHES: usize = 20;
/// A reservation whose object never arrived is removed after this long.
pub const STALE_RESERVATION: Duration = Duration::from_secs(3600);
/// Deadline for one blob storage call.
const STORE_TIMEOUT: Duration = Duration::from_secs(30);
/// Deadline for the availability probe of the usage endpoint.
const PROBE_TIMEOUT: Duration = Duration::from_secs(3);

/// Objects live under this prefix of the project's blob storage.
pub const BLOB_PREFIX: &str = ".temps/error-attachments/";
/// Where blob storage is enabled in the console.
pub const SETUP_PATH: &str = "/storage";

/// The object key of attachment `id`. Each object sits in its own
/// directory so a prefix delete cannot reach another attachment.
pub fn object_key(id: i64) -> String {
    format!("{BLOB_PREFIX}{id}/data")
}

fn object_prefix(id: i64) -> String {
    format!("{BLOB_PREFIX}{id}/")
}

/// What an attachment was sent with.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AttachmentOwner {
    /// An error event, by its id.
    Error(i64),
    /// A feedback, by its id.
    Feedback(i64),
}

impl AttachmentOwner {
    fn kind(self) -> &'static str {
        match self {
            Self::Error(_) => "error",
            Self::Feedback(_) => "feedback",
        }
    }

    fn id(self) -> i64 {
        match self {
            Self::Error(id) | Self::Feedback(id) => id,
        }
    }
}

/// A stored attachment.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema, FromQueryResult)]
pub struct AttachmentView {
    pub id: i64,
    /// `error` or `feedback`.
    pub owner_kind: String,
    /// The error event id or feedback id.
    pub owner_id: i64,
    pub filename: String,
    /// As the SDK sent it. The download is served as an image only for
    /// PNG, JPEG, GIF and WebP, and as a file to save otherwise.
    pub content_type: String,
    pub attachment_type: String,
    pub size_bytes: i64,
    #[schema(value_type = String, format = DateTime)]
    pub created_at: DateTime<Utc>,
}

/// Whether attachments can be stored, and how much of the quota is used.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct AttachmentUsage {
    /// Whether blob storage is set up and answering.
    pub configured: bool,
    /// Why attachments are not stored, when they are not.
    pub reason: Option<String>,
    /// Console path where blob storage is enabled.
    pub setup_path: Option<String>,
    pub used_bytes: i64,
    pub files: i64,
    pub quota_bytes: i64,
    pub max_attachment_bytes: i64,
    pub max_event_attachment_bytes: i64,
    pub max_attachments_per_event: i64,
    /// Since the server started: attachments dropped because the project
    /// was over its quota, across every project.
    pub dropped_over_quota: u64,
    /// Since the server started: attachments dropped because the upload
    /// queue was full, across every project.
    pub dropped_queue_full: u64,
}

/// What one uploader pass over a batch did.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct StoreOutcome {
    pub stored: u64,
    pub duplicates: u64,
    pub over_quota: u64,
    /// Would have taken the event or feedback past
    /// [`MAX_EVENT_ATTACHMENT_BYTES`] with files an earlier envelope stored.
    pub over_event_limit: u64,
    pub without_owner: u64,
    pub failed: u64,
}

/// What one sweeper pass deleted.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct SweepOutcome {
    pub expired: u64,
    pub stale: u64,
    /// Attachments whose feedback was deleted without them.
    pub orphaned_feedback: u64,
    pub deleted_projects: u64,
}

/// A row to remove: its object first, then the row, whose size goes back
/// to the quota. The row stays until its object is known to be gone, even
/// for a write that failed, since a store can keep a file whose `put` it
/// never answered.
#[derive(Debug, Clone, FromQueryResult)]
struct RemovalRow {
    id: i64,
    project_id: i32,
}

/// The head of a page of error attachments in event order, each marked
/// whether its event is gone; the caller adds the filter, order and limit.
const EXPIRED_PAGE_SELECT: &str = "SELECT a.id, a.project_id, a.event_time, \
     NOT EXISTS (SELECT 1 FROM error_events e \
                 WHERE e.id = a.owner_id AND e.timestamp = a.event_time) AS expired \
     FROM (SELECT id, project_id, owner_id, event_time FROM error_attachments";

/// One row of a sweep page: where the page ends, and whether to remove it.
#[derive(Debug, Clone, FromQueryResult)]
struct SweepCandidate {
    id: i64,
    project_id: i32,
    event_time: Option<DateTime<Utc>>,
    expired: bool,
}

/// Attachments: ingest hand-off, storage, reads and the sweeper.
pub struct AttachmentService {
    db: Arc<DatabaseConnection>,
    queue: AttachmentQueue,
    receiver: Mutex<Option<AttachmentReceiver>>,
    stats: Arc<AttachmentStats>,
    store: OnceLock<Arc<dyn PayloadBlobStore>>,
    /// Dropped with the service, which tells the sweeper to stop. The
    /// uploader stops when the queue's sender, also held here, is dropped.
    stop: tokio::sync::watch::Sender<()>,
    /// The last deleted project a sweep looked at, so a project whose
    /// files cannot be deleted does not hold back the ones after it.
    deleted_project_cursor: std::sync::atomic::AtomicI32,
}

/// The uploader and sweeper tasks [`AttachmentService::start`] spawned.
/// Both hold the service weakly and end once it is dropped.
pub struct AttachmentTasks {
    pub uploader: tokio::task::JoinHandle<()>,
    pub sweeper: tokio::task::JoinHandle<()>,
}

impl AttachmentService {
    pub fn new(db: Arc<DatabaseConnection>) -> Self {
        let stats = Arc::new(AttachmentStats::default());
        let (queue, receiver) =
            AttachmentQueue::new(queue::QUEUE_BATCHES, queue::QUEUE_BYTES, stats.clone());
        Self {
            db,
            queue,
            receiver: Mutex::new(Some(receiver)),
            stats,
            store: OnceLock::new(),
            stop: tokio::sync::watch::channel(()).0,
            deleted_project_cursor: std::sync::atomic::AtomicI32::new(0),
        }
    }

    /// Store attachments in this blob store. Unset, ingest drops them and
    /// the usage endpoint says blob storage is missing.
    pub fn set_store(&self, store: Arc<dyn PayloadBlobStore>) {
        let _ = self.store.set(store);
    }

    fn store(&self) -> Option<&Arc<dyn PayloadBlobStore>> {
        self.store.get()
    }

    pub fn counts(&self) -> AttachmentCounts {
        self.stats.snapshot()
    }

    /// Start the uploader and the sweeper. Does nothing the second time,
    /// or when no blob store was set. The tasks hold the service weakly:
    /// dropping it closes the queue and the stop signal, and both end.
    pub fn start(self: &Arc<Self>) -> Option<AttachmentTasks> {
        let store = self.store().cloned()?;
        let mut receiver = self.receiver.lock().ok().and_then(|mut slot| slot.take())?;
        let service = Arc::downgrade(self);
        let uploader_store = store.clone();
        let uploader_task = tokio::spawn(async move {
            while let Some(queued) = receiver.next().await {
                let Some(uploader) = service.upgrade() else {
                    break;
                };
                let project_id = queued.batch.project_id;
                if let Err(error) = uploader
                    .store_batch(uploader_store.as_ref(), &queued.batch)
                    .await
                {
                    // `store_batch` already counted the files it could not handle.
                    tracing::warn!(
                        project_id,
                        "could not store the attachments of event {} in project {project_id}: {error}",
                        queued.batch.sdk_event_id
                    );
                }
            }
        });
        let service = Arc::downgrade(self);
        let mut stop = self.stop.subscribe();
        let sweeper_task = tokio::spawn(async move {
            let mut interval = tokio::time::interval(SWEEP_INTERVAL);
            interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
            loop {
                tokio::select! {
                    _ = interval.tick() => {}
                    // Only ever an error: the sender is dropped with the service.
                    _ = stop.changed() => break,
                }
                let Some(sweeper) = service.upgrade() else {
                    break;
                };
                match sweeper.sweep(store.as_ref(), Utc::now()).await {
                    Ok(outcome) if outcome != SweepOutcome::default() => tracing::info!(
                        expired = outcome.expired,
                        stale = outcome.stale,
                        orphaned_feedback = outcome.orphaned_feedback,
                        deleted_projects = outcome.deleted_projects,
                        "attachment sweep deleted {} expired, {} unfinished and {} orphaned feedback attachments, and those of {} deleted projects",
                        outcome.expired,
                        outcome.stale,
                        outcome.orphaned_feedback,
                        outcome.deleted_projects
                    ),
                    Ok(_) => {}
                    Err(error) => tracing::warn!("attachment sweep failed: {error}"),
                }
            }
        });
        Some(AttachmentTasks {
            uploader: uploader_task,
            sweeper: sweeper_task,
        })
    }

    /// Offer an envelope's attachments to the uploader (hot path: one
    /// `try_send`, no I/O). Without blob storage they are dropped.
    pub fn offer(&self, batch: AttachmentBatch) -> OfferOutcome {
        if self.store().is_none() {
            return OfferOutcome {
                queued: 0,
                dropped: batch.attachments.len() as u64,
            };
        }
        self.queue.offer(batch)
    }

    /// The event or feedback the SDK sent as `sdk_event_id`.
    /// Only an event or feedback stored under the envelope's own DSN scope
    /// (environment and deployment) can own its files: a DSN of one scope
    /// cannot attach files to what another scope of the project sent.
    async fn resolve_owner(
        &self,
        batch: &AttachmentBatch,
    ) -> Result<Option<(AttachmentOwner, Option<i32>, Option<DateTime<Utc>>)>, ErrorTrackingError>
    {
        let project_id = batch.project_id;
        let sdk_event_id = batch.sdk_event_id.as_str();
        let key = self
            .db
            .query_one(Statement::from_sql_and_values(
                DatabaseBackend::Postgres,
                "SELECT error_event_id, error_group_id, received_at FROM error_event_source_keys \
                 WHERE project_id = $1 AND sdk_event_id = $2 \
                   AND environment_id IS NOT DISTINCT FROM $3 \
                   AND deployment_id IS NOT DISTINCT FROM $4",
                [
                    project_id.into(),
                    sdk_event_id.into(),
                    batch.environment_id.into(),
                    batch.deployment_id.into(),
                ],
            ))
            .await?;
        if let Some(row) = key {
            let event_id: i64 = row.try_get("", "error_event_id")?;
            let group_id: i32 = row.try_get("", "error_group_id")?;
            let received_at: DateTime<Utc> = row.try_get("", "received_at")?;
            // The event's own timestamp, which the sweeper checks. It is
            // looked up near the time its key was written, so one chunk is
            // read; an SDK whose clock is off by more than that falls back to
            // a lookup by id across chunks.
            let near = self
                .db
                .query_one(Statement::from_sql_and_values(
                    DatabaseBackend::Postgres,
                    "SELECT timestamp FROM error_events WHERE project_id = $1 AND id = $2 \
                     AND timestamp >= $3 - INTERVAL '1 hour' AND timestamp <= $3 + INTERVAL '1 hour' \
                     LIMIT 1",
                    [project_id.into(), event_id.into(), received_at.into()],
                ))
                .await?;
            let event = match near {
                Some(event) => Some(event),
                None => {
                    self.db
                        .query_one(Statement::from_sql_and_values(
                            DatabaseBackend::Postgres,
                            "SELECT timestamp FROM error_events WHERE project_id = $1 AND id = $2 \
                             LIMIT 1",
                            [project_id.into(), event_id.into()],
                        ))
                        .await?
                }
            };
            return Ok(match event {
                Some(event) => {
                    let timestamp: DateTime<Utc> = event.try_get("", "timestamp")?;
                    Some((
                        AttachmentOwner::Error(event_id),
                        Some(group_id),
                        Some(timestamp),
                    ))
                }
                None => None,
            });
        }
        let feedback = self
            .db
            .query_one(Statement::from_sql_and_values(
                DatabaseBackend::Postgres,
                "SELECT id FROM project_feedback \
                 WHERE project_id = $1 AND source = 'sentry' AND source_id = $2 \
                   AND environment_id IS NOT DISTINCT FROM $3 \
                   AND deployment_id IS NOT DISTINCT FROM $4",
                [
                    project_id.into(),
                    sdk_event_id.into(),
                    batch.environment_id.into(),
                    batch.deployment_id.into(),
                ],
            ))
            .await?;
        Ok(match feedback {
            Some(row) => Some((
                AttachmentOwner::Feedback(row.try_get("", "id")?),
                None,
                None,
            )),
            None => None,
        })
    }

    /// Store one envelope's attachments: reserve each against the quota,
    /// write the object, then mark the row stored. Idempotent: an SDK retry
    /// of the same envelope stores nothing twice. On a database error the
    /// files already handled keep their outcome and only the rest count as
    /// failed.
    pub async fn store_batch(
        &self,
        store: &dyn PayloadBlobStore,
        batch: &AttachmentBatch,
    ) -> Result<StoreOutcome, ErrorTrackingError> {
        let mut outcome = StoreOutcome::default();
        let project_id = batch.project_id;
        let owner = match self.resolve_owner(batch).await {
            Ok(owner) => owner,
            Err(error) => {
                outcome.failed = batch.attachments.len() as u64;
                self.record(outcome);
                return Err(error);
            }
        };
        let Some((owner, group_id, event_time)) = owner else {
            outcome.without_owner = batch.attachments.len() as u64;
            self.record(outcome);
            tracing::debug!(
                project_id,
                "no event or feedback {} stored in project {project_id} under this DSN's scope; its attachments are dropped",
                batch.sdk_event_id
            );
            return Ok(outcome);
        };
        let mut handled = 0usize;
        let result = async {
            for attachment in &batch.attachments {
                self.store_one(
                    store,
                    project_id,
                    owner,
                    group_id,
                    event_time,
                    attachment,
                    &mut outcome,
                )
                .await?;
                handled += 1;
            }
            Ok::<(), ErrorTrackingError>(())
        }
        .await;
        if result.is_err() {
            outcome.failed += (batch.attachments.len() - handled) as u64;
        }
        self.record(outcome);
        result.map(|()| outcome)
    }

    /// Reserve, write and mark one file, adding what happened to `outcome`.
    #[allow(
        clippy::too_many_arguments,
        reason = "one file of one owner: its project, owner, event and outcome"
    )]
    async fn store_one(
        &self,
        store: &dyn PayloadBlobStore,
        project_id: i32,
        owner: AttachmentOwner,
        group_id: Option<i32>,
        event_time: Option<DateTime<Utc>>,
        attachment: &IncomingAttachment,
        outcome: &mut StoreOutcome,
    ) -> Result<(), ErrorTrackingError> {
        let Some(id) = self
            .reserve(project_id, owner, group_id, event_time, attachment, outcome)
            .await?
        else {
            return Ok(());
        };
        let put = tokio::time::timeout(
            STORE_TIMEOUT,
            store.put(project_id, &object_key(id), attachment.body.clone()),
        )
        .await;
        match put {
            Ok(Ok(())) => {
                let marked = self
                    .db
                    .execute(Statement::from_sql_and_values(
                        DatabaseBackend::Postgres,
                        "UPDATE error_attachments SET stored_at = NOW() WHERE id = $1",
                        [id.into()],
                    ))
                    .await?
                    .rows_affected();
                if marked == 0 {
                    // The sweeper removed the row while the object was being
                    // written (its event expired meanwhile). Bounded like every
                    // other storage call, so a store that stops answering
                    // cannot hold the only uploader.
                    let deleted = tokio::time::timeout(
                        STORE_TIMEOUT,
                        store.delete_prefix(project_id, &object_prefix(id)),
                    )
                    .await;
                    let problem = match deleted {
                        Ok(Ok(_)) | Ok(Err(PayloadBlobError::NotFound { .. })) => None,
                        Ok(Err(error)) => Some(error.to_string()),
                        Err(_) => Some(format!("no answer within {}s", STORE_TIMEOUT.as_secs())),
                    };
                    if let Some(problem) = problem {
                        tracing::warn!(
                            project_id,
                            attachment_id = id,
                            "attachment {id} of project {project_id} expired while it was written, and its object could not be deleted: {problem}"
                        );
                    }
                    outcome.failed += 1;
                    return Ok(());
                }
                outcome.stored += 1;
            }
            failed => {
                let reason = match failed {
                    Ok(Err(error)) => error.to_string(),
                    _ => format!("no answer within {}s", STORE_TIMEOUT.as_secs()),
                };
                tracing::warn!(
                    project_id,
                    attachment_id = id,
                    "could not write attachment {id} of project {project_id} to blob storage: {reason}"
                );
                // The object may exist anyway. Deleting it confirms it does
                // not; otherwise the row (and its quota) stays as an
                // unfinished write the sweeper retries.
                self.remove(store, vec![RemovalRow { id, project_id }])
                    .await?;
                outcome.failed += 1;
            }
        }
        Ok(())
    }

    /// Reserve `attachment` in the quota and insert its row, or say why not.
    ///
    /// Everything is decided under the project's usage row lock, in this
    /// order: a file already stored for the owner (same name and SHA-256)
    /// is a duplicate and costs nothing; then the owner's count and byte
    /// limits, which cover every envelope it came in; then the project
    /// quota. A new file takes the owner's next position.
    async fn reserve(
        &self,
        project_id: i32,
        owner: AttachmentOwner,
        group_id: Option<i32>,
        event_time: Option<DateTime<Utc>>,
        attachment: &IncomingAttachment,
        outcome: &mut StoreOutcome,
    ) -> Result<Option<i64>, ErrorTrackingError> {
        use sha2::Digest;
        let size = attachment.body.len() as i64;
        let digest = sha2::Sha256::digest(&attachment.body).to_vec();
        let tx = self.db.begin().await?;
        tx.execute(Statement::from_sql_and_values(
            DatabaseBackend::Postgres,
            "INSERT INTO error_attachment_usage (project_id, quota_bytes) VALUES ($1, $2) \
             ON CONFLICT (project_id) DO NOTHING",
            [project_id.into(), DEFAULT_QUOTA_BYTES.into()],
        ))
        .await?;
        let usage = tx
            .query_one(Statement::from_sql_and_values(
                DatabaseBackend::Postgres,
                "SELECT bytes, quota_bytes FROM error_attachment_usage \
                 WHERE project_id = $1 FOR UPDATE",
                [project_id.into()],
            ))
            .await?
            .ok_or(ErrorTrackingError::ProjectNotFound)?;
        let used: i64 = usage.try_get("", "bytes")?;
        let quota: i64 = usage.try_get("", "quota_bytes")?;
        // The same file for the same owner: unique by name and content.
        let replay = tx
            .query_one(Statement::from_sql_and_values(
                DatabaseBackend::Postgres,
                "SELECT id, stored_at IS NOT NULL AS stored FROM error_attachments \
                 WHERE project_id = $1 AND owner_kind = $2 AND owner_id = $3 \
                   AND filename = $4 AND content_sha256 = $5",
                [
                    project_id.into(),
                    owner.kind().into(),
                    owner.id().into(),
                    attachment.filename.clone().into(),
                    digest.clone().into(),
                ],
            ))
            .await?;
        if let Some(row) = replay {
            if row.try_get::<bool>("", "stored")? {
                outcome.duplicates += 1;
                return Ok(None);
            }
            // A write that never finished (its object may or may not be
            // there): the resend completes it under the reservation it
            // already holds, charged once. The single uploader is the only
            // writer, so nothing else is writing this row now.
            return Ok(Some(row.try_get("", "id")?));
        }
        let owned = tx
            .query_one(Statement::from_sql_and_values(
                DatabaseBackend::Postgres,
                "SELECT COUNT(*)::BIGINT AS files, \
                        COALESCE(SUM(size_bytes), 0)::BIGINT AS bytes, \
                        COALESCE(MAX(ordinal) + 1, 0)::INTEGER AS next_ordinal \
                 FROM error_attachments \
                 WHERE project_id = $1 AND owner_kind = $2 AND owner_id = $3",
                [project_id.into(), owner.kind().into(), owner.id().into()],
            ))
            .await?
            .ok_or(ErrorTrackingError::ProjectNotFound)?;
        let owner_files: i64 = owned.try_get("", "files")?;
        let owner_bytes: i64 = owned.try_get("", "bytes")?;
        let next_ordinal: i32 = owned.try_get("", "next_ordinal")?;
        if owner_files >= MAX_ATTACHMENTS_PER_EVENT as i64
            || owner_bytes.saturating_add(size) > MAX_EVENT_ATTACHMENT_BYTES as i64
            || next_ordinal > i32::from(i16::MAX)
        {
            outcome.over_event_limit += 1;
            tracing::debug!(
                project_id,
                "{} {} already has {owner_files} attachments of {owner_bytes} bytes; one more of {size} bytes would pass the limit of {MAX_ATTACHMENTS_PER_EVENT} files or {MAX_EVENT_ATTACHMENT_BYTES} bytes and is dropped",
                owner.kind(),
                owner.id()
            );
            return Ok(None);
        }
        if used.saturating_add(size) > quota {
            outcome.over_quota += 1;
            tracing::debug!(
                project_id,
                "project {project_id} uses {used} of {quota} attachment bytes; an attachment of {size} bytes is dropped"
            );
            return Ok(None);
        }
        // `next_ordinal` was checked against `i16::MAX` above.
        let ordinal = next_ordinal as i16;
        let inserted = tx
            .query_one(Statement::from_sql_and_values(
                DatabaseBackend::Postgres,
                "INSERT INTO error_attachments (project_id, owner_kind, owner_id, ordinal, \
                     error_group_id, event_time, filename, content_type, attachment_type, \
                     size_bytes, content_sha256) \
                 VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11) \
                 ON CONFLICT DO NOTHING RETURNING id",
                [
                    project_id.into(),
                    owner.kind().into(),
                    owner.id().into(),
                    ordinal.into(),
                    group_id.into(),
                    event_time.into(),
                    attachment.filename.clone().into(),
                    attachment.content_type.clone().into(),
                    attachment.attachment_type.clone().into(),
                    size.into(),
                    digest.into(),
                ],
            ))
            .await?;
        let Some(row) = inserted else {
            outcome.duplicates += 1;
            return Ok(None);
        };
        let id: i64 = row.try_get("", "id")?;
        tx.execute(Statement::from_sql_and_values(
            DatabaseBackend::Postgres,
            "UPDATE error_attachment_usage SET bytes = bytes + $2, files = files + 1, \
             updated_at = NOW() WHERE project_id = $1",
            [project_id.into(), size.into()],
        ))
        .await?;
        tx.commit().await?;
        Ok(Some(id))
    }

    fn record(&self, outcome: StoreOutcome) {
        let add = |counter: &std::sync::atomic::AtomicU64, value: u64| {
            counter.fetch_add(value, Ordering::Relaxed);
        };
        add(&self.stats.stored, outcome.stored);
        add(&self.stats.duplicates, outcome.duplicates);
        add(&self.stats.over_quota, outcome.over_quota);
        add(&self.stats.over_event_limit, outcome.over_event_limit);
        add(&self.stats.without_owner, outcome.without_owner);
        add(&self.stats.failed, outcome.failed);
    }

    /// Delete the rows and give their bytes back to the quota. The caller
    /// has deleted their objects.
    async fn remove_rows(&self, rows: &[RemovalRow]) -> Result<(), ErrorTrackingError> {
        if rows.is_empty() {
            return Ok(());
        }
        let ids: Vec<i64> = rows.iter().map(|row| row.id).collect();
        let mut projects: Vec<i32> = rows.iter().map(|row| row.project_id).collect();
        projects.sort_unstable();
        projects.dedup();
        let tx = self.db.begin().await?;
        // Take the usage row locks first, in project order, as a reservation
        // does, so a delete and a reservation never wait on each other.
        tx.execute(Statement::from_sql_and_values(
            DatabaseBackend::Postgres,
            "SELECT project_id FROM error_attachment_usage WHERE project_id = ANY($1) \
             ORDER BY project_id FOR UPDATE",
            [projects.into()],
        ))
        .await?;
        let deleted = tx
            .query_all(Statement::from_sql_and_values(
                DatabaseBackend::Postgres,
                "DELETE FROM error_attachments WHERE id = ANY($1) \
                 RETURNING project_id, size_bytes",
                [ids.into()],
            ))
            .await?;
        let mut per_project: std::collections::BTreeMap<i32, (i64, i64)> = Default::default();
        for row in deleted {
            let project_id: i32 = row.try_get("", "project_id")?;
            let size: i64 = row.try_get("", "size_bytes")?;
            let entry = per_project.entry(project_id).or_default();
            entry.0 += size;
            entry.1 += 1;
        }
        for (project_id, (bytes, files)) in per_project {
            tx.execute(Statement::from_sql_and_values(
                DatabaseBackend::Postgres,
                "UPDATE error_attachment_usage SET bytes = GREATEST(bytes - $2, 0), \
                 files = GREATEST(files - $3, 0), updated_at = NOW() WHERE project_id = $1",
                [project_id.into(), bytes.into(), files.into()],
            ))
            .await?;
        }
        tx.commit().await?;
        Ok(())
    }

    /// Delete the objects of `rows`, then the rows whose object is gone.
    /// A row whose object could not be deleted stays for the next sweep.
    async fn remove(
        &self,
        store: &dyn PayloadBlobStore,
        rows: Vec<RemovalRow>,
    ) -> Result<u64, ErrorTrackingError> {
        let mut gone = Vec::with_capacity(rows.len());
        for row in rows {
            let deleted = tokio::time::timeout(
                STORE_TIMEOUT,
                store.delete_prefix(row.project_id, &object_prefix(row.id)),
            )
            .await;
            match deleted {
                Ok(Ok(_)) => gone.push(row),
                Ok(Err(PayloadBlobError::NotFound { .. })) => gone.push(row),
                Ok(Err(error)) => tracing::warn!(
                    project_id = row.project_id,
                    attachment_id = row.id,
                    "could not delete attachment {} of project {} from blob storage, retrying on the next sweep: {error}",
                    row.id,
                    row.project_id
                ),
                Err(_) => tracing::warn!(
                    project_id = row.project_id,
                    attachment_id = row.id,
                    "deleting attachment {} of project {} timed out, retrying on the next sweep",
                    row.id,
                    row.project_id
                ),
            }
        }
        let count = gone.len() as u64;
        self.remove_rows(&gone).await?;
        Ok(count)
    }

    /// Delete attachments whose event expired, reservations never
    /// finished, and every attachment of a deleted project. Bounded: at
    /// most [`MAX_SWEEP_BATCHES`] steps of [`SWEEP_BATCH`] per kind.
    pub async fn sweep(
        &self,
        store: &dyn PayloadBlobStore,
        now: DateTime<Utc>,
    ) -> Result<SweepOutcome, ErrorTrackingError> {
        let mut outcome = SweepOutcome::default();

        // Error events expire by whole time chunks, oldest first, so the
        // attachments of expired events are the oldest ones: walk them in
        // event order and stop at the first page whose events all exist.
        // Each page starts after the previous one, so a row whose object
        // cannot be deleted is passed over until the next sweep.
        let mut after: Option<(DateTime<Utc>, i64)> = None;
        for _ in 0..MAX_SWEEP_BATCHES {
            // Two forms rather than `$1 IS NULL OR ...`, so both walk the
            // `(event_time, id)` index.
            let statement = match after {
                None => Statement::from_sql_and_values(
                    DatabaseBackend::Postgres,
                    format!(
                        "{EXPIRED_PAGE_SELECT} WHERE owner_kind = 'error' AND event_time IS NOT NULL \
                         ORDER BY event_time, id LIMIT $1) a ORDER BY a.event_time, a.id"
                    ),
                    [(SWEEP_BATCH as i64).into()],
                ),
                Some((after_time, after_id)) => Statement::from_sql_and_values(
                    DatabaseBackend::Postgres,
                    format!(
                        "{EXPIRED_PAGE_SELECT} WHERE owner_kind = 'error' AND event_time IS NOT NULL \
                         AND (event_time, id) > ($1, $2) \
                         ORDER BY event_time, id LIMIT $3) a ORDER BY a.event_time, a.id"
                    ),
                    [after_time.into(), after_id.into(), (SWEEP_BATCH as i64).into()],
                ),
            };
            let page = SweepCandidate::find_by_statement(statement)
                .all(self.db.as_ref())
                .await?;
            let Some(last) = page.last() else { break };
            after = last.event_time.map(|t| (t, last.id));
            let full = page.len() as u64 == SWEEP_BATCH;
            let rows: Vec<RemovalRow> = page
                .into_iter()
                .filter(|row| row.expired)
                .map(|row| RemovalRow {
                    id: row.id,
                    project_id: row.project_id,
                })
                .collect();
            if rows.is_empty() {
                break;
            }
            outcome.expired += self.remove(store, rows).await?;
            if !full {
                break;
            }
        }

        let stale_before = now
            - chrono::Duration::from_std(STALE_RESERVATION).unwrap_or(chrono::Duration::hours(1));
        let mut after_id = 0i64;
        for _ in 0..MAX_SWEEP_BATCHES {
            let rows = RemovalRow::find_by_statement(Statement::from_sql_and_values(
                DatabaseBackend::Postgres,
                "SELECT id, project_id FROM error_attachments \
                 WHERE stored_at IS NULL AND created_at < $1 AND id > $2 ORDER BY id LIMIT $3",
                [
                    stale_before.into(),
                    after_id.into(),
                    (SWEEP_BATCH as i64).into(),
                ],
            ))
            .all(self.db.as_ref())
            .await?;
            let Some(last) = rows.last() else { break };
            after_id = last.id;
            let full = rows.len() as u64 == SWEEP_BATCH;
            outcome.stale += self.remove(store, rows).await?;
            if !full {
                break;
            }
        }

        // Deleting a feedback deletes its attachments; one whose object
        // could not be deleted then is left here.
        let mut after_id = 0i64;
        for _ in 0..MAX_SWEEP_BATCHES {
            let rows = RemovalRow::find_by_statement(Statement::from_sql_and_values(
                DatabaseBackend::Postgres,
                "SELECT a.id, a.project_id \
                 FROM error_attachments a WHERE a.owner_kind = 'feedback' AND a.id > $1 \
                   AND NOT EXISTS (SELECT 1 FROM project_feedback f \
                                   WHERE f.id = a.owner_id AND f.project_id = a.project_id) \
                 ORDER BY a.id LIMIT $2",
                [after_id.into(), (SWEEP_BATCH as i64).into()],
            ))
            .all(self.db.as_ref())
            .await?;
            let Some(last) = rows.last() else { break };
            after_id = last.id;
            let full = rows.len() as u64 == SWEEP_BATCH;
            outcome.orphaned_feedback += self.remove(store, rows).await?;
            if !full {
                break;
            }
        }

        // Deleted projects, a page per pass, continuing where the last
        // pass stopped and starting over once the end is reached.
        let after_project = self.deleted_project_cursor.load(Ordering::Relaxed);
        let deleted_projects = self
            .db
            .query_all(Statement::from_sql_and_values(
                DatabaseBackend::Postgres,
                "SELECT u.project_id FROM error_attachment_usage u \
                 WHERE u.project_id > $1 \
                   AND NOT EXISTS (SELECT 1 FROM projects p \
                                   WHERE p.id = u.project_id AND NOT p.is_deleted) \
                 ORDER BY u.project_id LIMIT $2",
                [after_project.into(), (MAX_SWEEP_BATCHES as i64).into()],
            ))
            .await?;
        let next_cursor = if deleted_projects.len() < MAX_SWEEP_BATCHES {
            0
        } else {
            match deleted_projects.last() {
                Some(row) => row.try_get("", "project_id")?,
                None => 0,
            }
        };
        self.deleted_project_cursor
            .store(next_cursor, Ordering::Relaxed);
        for row in deleted_projects {
            let project_id: i32 = row.try_get("", "project_id")?;
            let deleted =
                tokio::time::timeout(STORE_TIMEOUT, store.delete_prefix(project_id, BLOB_PREFIX))
                    .await;
            match deleted {
                Ok(Ok(_)) | Ok(Err(PayloadBlobError::NotFound { .. })) => {
                    let tx = self.db.begin().await?;
                    tx.execute(Statement::from_sql_and_values(
                        DatabaseBackend::Postgres,
                        "DELETE FROM error_attachments WHERE project_id = $1",
                        [project_id.into()],
                    ))
                    .await?;
                    tx.execute(Statement::from_sql_and_values(
                        DatabaseBackend::Postgres,
                        "DELETE FROM error_attachment_usage WHERE project_id = $1",
                        [project_id.into()],
                    ))
                    .await?;
                    tx.commit().await?;
                    outcome.deleted_projects += 1;
                }
                Ok(Err(error)) => tracing::warn!(
                    project_id,
                    "could not delete the attachments of deleted project {project_id}, retrying on the next sweep: {error}"
                ),
                Err(_) => tracing::warn!(
                    project_id,
                    "deleting the attachments of deleted project {project_id} timed out, retrying on the next sweep"
                ),
            }
        }
        self.stats.swept.fetch_add(
            outcome.expired + outcome.stale + outcome.orphaned_feedback,
            Ordering::Relaxed,
        );
        Ok(outcome)
    }

    /// The stored attachments of `owner`, in the order the SDK sent them.
    pub async fn list(
        &self,
        project_id: i32,
        owner: AttachmentOwner,
    ) -> Result<Vec<AttachmentView>, ErrorTrackingError> {
        Ok(
            AttachmentView::find_by_statement(Statement::from_sql_and_values(
                DatabaseBackend::Postgres,
                "SELECT id, owner_kind, owner_id, filename, content_type, attachment_type, \
                     size_bytes, created_at \
                 FROM error_attachments \
                 WHERE project_id = $1 AND owner_kind = $2 AND owner_id = $3 \
                   AND stored_at IS NOT NULL \
                 ORDER BY ordinal LIMIT $4",
                [
                    project_id.into(),
                    owner.kind().into(),
                    owner.id().into(),
                    (MAX_ATTACHMENTS_PER_EVENT as i64).into(),
                ],
            ))
            .all(self.db.as_ref())
            .await?,
        )
    }

    async fn find(
        &self,
        project_id: i32,
        attachment_id: i64,
    ) -> Result<AttachmentView, ErrorTrackingError> {
        AttachmentView::find_by_statement(Statement::from_sql_and_values(
            DatabaseBackend::Postgres,
            "SELECT id, owner_kind, owner_id, filename, content_type, attachment_type, \
                 size_bytes, created_at \
             FROM error_attachments \
             WHERE project_id = $1 AND id = $2 AND stored_at IS NOT NULL",
            [project_id.into(), attachment_id.into()],
        ))
        .one(self.db.as_ref())
        .await?
        .ok_or(ErrorTrackingError::AttachmentNotFound {
            project_id,
            attachment_id,
        })
    }

    fn require_store(
        &self,
        project_id: i32,
    ) -> Result<&Arc<dyn PayloadBlobStore>, ErrorTrackingError> {
        self.store()
            .ok_or_else(|| ErrorTrackingError::AttachmentStorageUnavailable {
                project_id,
                reason: "no blob storage is set up".into(),
            })
    }

    /// An attachment and its bytes.
    pub async fn content(
        &self,
        project_id: i32,
        attachment_id: i64,
    ) -> Result<(AttachmentView, Bytes), ErrorTrackingError> {
        let view = self.find(project_id, attachment_id).await?;
        let store = self.require_store(project_id)?;
        let read = tokio::time::timeout(
            STORE_TIMEOUT,
            store.get(project_id, &object_key(attachment_id), MAX_ATTACHMENT_BYTES),
        )
        .await
        .map_err(|_| ErrorTrackingError::AttachmentStorageUnavailable {
            project_id,
            reason: format!("no answer within {}s", STORE_TIMEOUT.as_secs()),
        })?;
        match read {
            Ok(bytes) => Ok((view, bytes)),
            Err(PayloadBlobError::NotFound { .. }) => Err(ErrorTrackingError::AttachmentNotFound {
                project_id,
                attachment_id,
            }),
            Err(error) => Err(ErrorTrackingError::AttachmentStorageUnavailable {
                project_id,
                reason: error.to_string(),
            }),
        }
    }

    /// Delete one attachment, object first. Returns what was deleted.
    pub async fn delete(
        &self,
        project_id: i32,
        attachment_id: i64,
    ) -> Result<AttachmentView, ErrorTrackingError> {
        let view = self.find(project_id, attachment_id).await?;
        let store = self.require_store(project_id)?;
        let removed = self
            .remove(
                store.as_ref(),
                vec![RemovalRow {
                    id: view.id,
                    project_id,
                }],
            )
            .await?;
        if removed == 0 {
            return Err(ErrorTrackingError::AttachmentStorageUnavailable {
                project_id,
                reason: format!(
                    "attachment {attachment_id} could not be deleted from blob storage"
                ),
            });
        }
        Ok(view)
    }

    /// Delete every attachment of `owner` (a feedback being deleted) and
    /// return how many were deleted. An attachment whose object cannot be
    /// deleted now keeps its row, which the sweeper removes once its owner
    /// is gone.
    pub async fn delete_owner(
        &self,
        project_id: i32,
        owner: AttachmentOwner,
    ) -> Result<u64, ErrorTrackingError> {
        let rows = RemovalRow::find_by_statement(Statement::from_sql_and_values(
            DatabaseBackend::Postgres,
            "SELECT id, project_id \
             FROM error_attachments WHERE project_id = $1 AND owner_kind = $2 AND owner_id = $3",
            [project_id.into(), owner.kind().into(), owner.id().into()],
        ))
        .all(self.db.as_ref())
        .await?;
        if rows.is_empty() {
            return Ok(0);
        }
        // Without blob storage the objects cannot be deleted now; their rows
        // stay so the sweeper deletes them once storage is back.
        let Some(store) = self.store() else {
            tracing::warn!(
                project_id,
                "blob storage is not available, so the {} attachments of {} {} are deleted later",
                rows.len(),
                owner.kind(),
                owner.id()
            );
            return Ok(0);
        };
        self.remove(store.as_ref(), rows).await
    }

    /// Whether attachments are stored for `project_id`, and how much of its
    /// quota they use.
    pub async fn usage(&self, project_id: i32) -> Result<AttachmentUsage, ErrorTrackingError> {
        let row = self
            .db
            .query_one(Statement::from_sql_and_values(
                DatabaseBackend::Postgres,
                "SELECT bytes, files, quota_bytes FROM error_attachment_usage WHERE project_id = $1",
                [project_id.into()],
            ))
            .await?;
        let (used_bytes, files, quota_bytes) = match row {
            Some(row) => (
                row.try_get("", "bytes")?,
                row.try_get("", "files")?,
                row.try_get("", "quota_bytes")?,
            ),
            None => (0, 0, DEFAULT_QUOTA_BYTES),
        };
        let (configured, reason) = self.availability(project_id).await;
        let counts = self.counts();
        Ok(AttachmentUsage {
            configured,
            setup_path: (!configured).then(|| SETUP_PATH.to_string()),
            reason,
            used_bytes,
            files,
            quota_bytes,
            max_attachment_bytes: MAX_ATTACHMENT_BYTES as i64,
            max_event_attachment_bytes: MAX_EVENT_ATTACHMENT_BYTES as i64,
            max_attachments_per_event: MAX_ATTACHMENTS_PER_EVENT as i64,
            dropped_over_quota: counts.over_quota,
            dropped_queue_full: counts.dropped_queue_full,
        })
    }

    /// Probe blob storage: a read of a key that never exists answers
    /// "not found" when storage works.
    async fn availability(&self, project_id: i32) -> (bool, Option<String>) {
        let Some(store) = self.store() else {
            return (
                false,
                Some("No blob storage is set up, so attachments sent by SDKs are dropped. Enable Blob storage to keep them.".into()),
            );
        };
        let probe = format!("{BLOB_PREFIX}probe/none");
        match tokio::time::timeout(PROBE_TIMEOUT, store.get(project_id, &probe, 1)).await {
            Ok(Ok(_)) | Ok(Err(PayloadBlobError::NotFound { .. })) => (true, None),
            Ok(Err(error)) => (
                false,
                Some(format!(
                    "Blob storage is not answering, so attachments sent by SDKs are dropped: {error}"
                )),
            ),
            Err(_) => (
                false,
                Some(format!(
                    "Blob storage did not answer within {}s, so attachments sent by SDKs may be dropped.",
                    PROBE_TIMEOUT.as_secs()
                )),
            ),
        }
    }

    /// Set the attachment storage `project_id` may use. Lowering it below
    /// the current use deletes nothing; new attachments are dropped until
    /// use falls under it.
    pub async fn set_quota(
        &self,
        project_id: i32,
        quota_bytes: i64,
    ) -> Result<i64, ErrorTrackingError> {
        if !(0..=MAX_QUOTA_BYTES).contains(&quota_bytes) {
            return Err(ErrorTrackingError::Validation(format!(
                "quota_bytes must be between 0 and {MAX_QUOTA_BYTES}, got {quota_bytes}"
            )));
        }
        // Read the previous quota under the row lock, so two concurrent
        // saves each report the quota the other one replaced.
        let tx = self.db.begin().await?;
        tx.execute(Statement::from_sql_and_values(
            DatabaseBackend::Postgres,
            "INSERT INTO error_attachment_usage (project_id, quota_bytes) VALUES ($1, $2) \
             ON CONFLICT (project_id) DO NOTHING",
            [project_id.into(), DEFAULT_QUOTA_BYTES.into()],
        ))
        .await?;
        let previous: i64 = tx
            .query_one(Statement::from_sql_and_values(
                DatabaseBackend::Postgres,
                "SELECT quota_bytes FROM error_attachment_usage WHERE project_id = $1 FOR UPDATE",
                [project_id.into()],
            ))
            .await?
            .ok_or(ErrorTrackingError::ProjectNotFound)?
            .try_get("", "quota_bytes")?;
        tx.execute(Statement::from_sql_and_values(
            DatabaseBackend::Postgres,
            "UPDATE error_attachment_usage SET quota_bytes = $2, updated_at = NOW() \
             WHERE project_id = $1",
            [project_id.into(), quota_bytes.into()],
        ))
        .await?;
        tx.commit().await?;
        Ok(previous)
    }
}

/// Keep a file name readable and harmless: no directories, no control
/// characters, at most [`MAX_FILENAME_CHARS`].
pub fn clean_filename(raw: Option<&str>) -> String {
    let name = raw
        .unwrap_or("")
        .rsplit(['/', '\\'])
        .next()
        .unwrap_or("")
        .chars()
        .filter(|c| !c.is_control())
        .take(MAX_FILENAME_CHARS)
        .collect::<String>();
    let name = name.trim();
    if name.is_empty() || name == "." || name == ".." {
        "attachment".to_string()
    } else {
        name.to_string()
    }
}

/// A content type as `type/subtype`, lower case, or
/// `application/octet-stream` when it is missing or malformed.
pub fn clean_content_type(raw: Option<&str>) -> String {
    let essence = raw
        .unwrap_or("")
        .split(';')
        .next()
        .unwrap_or("")
        .trim()
        .to_ascii_lowercase();
    let valid = essence.len() <= MAX_CONTENT_TYPE_CHARS
        && essence.split_once('/').is_some_and(|(kind, sub)| {
            let token = |s: &str| {
                !s.is_empty()
                    && s.chars()
                        .all(|c| c.is_ascii_alphanumeric() || "!#$&-^_.+".contains(c))
            };
            token(kind) && token(sub)
        });
    if valid {
        essence
    } else {
        "application/octet-stream".to_string()
    }
}

/// Content types the console may show inline. Everything else is served as
/// a download, so a stored HTML or SVG file never runs in the console.
pub fn is_inline_image(content_type: &str) -> bool {
    matches!(
        content_type,
        "image/png" | "image/jpeg" | "image/gif" | "image/webp"
    )
}

/// The SDK event id as stored in `error_event_source_keys`: 32 lowercase
/// hex digits, or `None` when it is not an event id.
pub fn normalize_event_id(raw: &str) -> Option<String> {
    let id: String = raw
        .chars()
        .filter(|c| *c != '-')
        .collect::<String>()
        .to_ascii_lowercase();
    (id.len() == 32 && id.bytes().all(|b| b.is_ascii_hexdigit()) && id.bytes().any(|b| b != b'0'))
        .then_some(id)
}
