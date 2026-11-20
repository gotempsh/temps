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
/// to the quota.
#[derive(Debug, Clone, FromQueryResult)]
struct RemovalRow {
    id: i64,
    project_id: i32,
    stored: bool,
}

/// Attachments: ingest hand-off, storage, reads and the sweeper.
pub struct AttachmentService {
    db: Arc<DatabaseConnection>,
    queue: AttachmentQueue,
    receiver: Mutex<Option<AttachmentReceiver>>,
    stats: Arc<AttachmentStats>,
    store: OnceLock<Arc<dyn PayloadBlobStore>>,
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
    /// or when no blob store was set.
    pub fn start(self: &Arc<Self>) {
        let Some(store) = self.store().cloned() else {
            return;
        };
        let receiver = self.receiver.lock().ok().and_then(|mut slot| slot.take());
        let Some(mut receiver) = receiver else {
            return;
        };
        let uploader = self.clone();
        let uploader_store = store.clone();
        tokio::spawn(async move {
            while let Some(queued) = receiver.next().await {
                let project_id = queued.batch.project_id;
                if let Err(error) = uploader
                    .store_batch(uploader_store.as_ref(), &queued.batch)
                    .await
                {
                    uploader
                        .stats
                        .failed
                        .fetch_add(queued.batch.attachments.len() as u64, Ordering::Relaxed);
                    tracing::warn!(
                        project_id,
                        "could not store the attachments of event {} in project {project_id}: {error}",
                        queued.batch.sdk_event_id
                    );
                }
            }
        });
        let sweeper = self.clone();
        tokio::spawn(async move {
            let mut interval = tokio::time::interval(SWEEP_INTERVAL);
            interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
            loop {
                interval.tick().await;
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
    async fn resolve_owner(
        &self,
        project_id: i32,
        sdk_event_id: &str,
    ) -> Result<Option<(AttachmentOwner, Option<i32>, Option<DateTime<Utc>>)>, ErrorTrackingError>
    {
        let key = self
            .db
            .query_one(Statement::from_sql_and_values(
                DatabaseBackend::Postgres,
                "SELECT error_event_id, error_group_id, received_at FROM error_event_source_keys \
                 WHERE project_id = $1 AND sdk_event_id = $2",
                [project_id.into(), sdk_event_id.into()],
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
                 WHERE project_id = $1 AND source = 'sentry' AND source_id = $2",
                [project_id.into(), sdk_event_id.into()],
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
    /// of the same envelope stores nothing twice.
    pub async fn store_batch(
        &self,
        store: &dyn PayloadBlobStore,
        batch: &AttachmentBatch,
    ) -> Result<StoreOutcome, ErrorTrackingError> {
        let mut outcome = StoreOutcome::default();
        let project_id = batch.project_id;
        let Some((owner, group_id, event_time)) =
            self.resolve_owner(project_id, &batch.sdk_event_id).await?
        else {
            outcome.without_owner = batch.attachments.len() as u64;
            self.record(outcome);
            tracing::debug!(
                project_id,
                "no stored event or feedback {} in project {project_id}; its attachments are dropped",
                batch.sdk_event_id
            );
            return Ok(outcome);
        };
        for (ordinal, attachment) in batch.attachments.iter().enumerate() {
            let Some(id) = self
                .reserve(
                    project_id,
                    owner,
                    ordinal as i16,
                    group_id,
                    event_time,
                    attachment,
                    &mut outcome,
                )
                .await?
            else {
                continue;
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
                        // The sweeper removed the row while the object was
                        // being written (its event expired meanwhile).
                        let _ = store.delete_prefix(project_id, &object_prefix(id)).await;
                        outcome.failed += 1;
                        continue;
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
                    self.remove_rows(&[RemovalRow {
                        id,
                        project_id,
                        stored: false,
                    }])
                    .await?;
                    outcome.failed += 1;
                }
            }
        }
        self.record(outcome);
        Ok(outcome)
    }

    /// Reserve `attachment` in the quota and insert its row, or say why not.
    #[allow(
        clippy::too_many_arguments,
        reason = "one reservation is one row: its owner, position, event and file"
    )]
    async fn reserve(
        &self,
        project_id: i32,
        owner: AttachmentOwner,
        ordinal: i16,
        group_id: Option<i32>,
        event_time: Option<DateTime<Utc>>,
        attachment: &IncomingAttachment,
        outcome: &mut StoreOutcome,
    ) -> Result<Option<i64>, ErrorTrackingError> {
        let size = attachment.body.len() as i64;
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
        if used.saturating_add(size) > quota {
            outcome.over_quota += 1;
            tracing::debug!(
                project_id,
                "project {project_id} uses {used} of {quota} attachment bytes; an attachment of {size} bytes is dropped"
            );
            return Ok(None);
        }
        let inserted = tx
            .query_one(Statement::from_sql_and_values(
                DatabaseBackend::Postgres,
                "INSERT INTO error_attachments (project_id, owner_kind, owner_id, ordinal, \
                     error_group_id, event_time, filename, content_type, attachment_type, size_bytes) \
                 VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10) \
                 ON CONFLICT (project_id, owner_kind, owner_id, ordinal) DO NOTHING RETURNING id",
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
        let tx = self.db.begin().await?;
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
                Ok(Err(error)) if !row.stored => {
                    // A reservation never written may have no object at all.
                    tracing::debug!(
                        project_id = row.project_id,
                        "attachment {} was never stored; removing its row ({error})",
                        row.id
                    );
                    gone.push(row);
                }
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
        // event order and stop at the first batch whose events all exist.
        for _ in 0..MAX_SWEEP_BATCHES {
            let rows = RemovalRow::find_by_statement(Statement::from_sql_and_values(
                DatabaseBackend::Postgres,
                "SELECT a.id, a.project_id, a.stored_at IS NOT NULL AS stored \
                 FROM (SELECT id, project_id, stored_at, owner_id, event_time \
                       FROM error_attachments WHERE owner_kind = 'error' \
                       ORDER BY event_time, id LIMIT $1) a \
                 WHERE NOT EXISTS (SELECT 1 FROM error_events e \
                                   WHERE e.id = a.owner_id AND e.timestamp = a.event_time)",
                [(SWEEP_BATCH as i64).into()],
            ))
            .all(self.db.as_ref())
            .await?;
            if rows.is_empty() {
                break;
            }
            outcome.expired += self.remove(store, rows).await?;
        }

        let stale_before = now
            - chrono::Duration::from_std(STALE_RESERVATION).unwrap_or(chrono::Duration::hours(1));
        for _ in 0..MAX_SWEEP_BATCHES {
            let rows = RemovalRow::find_by_statement(Statement::from_sql_and_values(
                DatabaseBackend::Postgres,
                "SELECT id, project_id, FALSE AS stored FROM error_attachments \
                 WHERE stored_at IS NULL AND created_at < $1 ORDER BY created_at LIMIT $2",
                [stale_before.into(), (SWEEP_BATCH as i64).into()],
            ))
            .all(self.db.as_ref())
            .await?;
            if rows.is_empty() {
                break;
            }
            let found = rows.len();
            outcome.stale += self.remove(store, rows).await?;
            if found < SWEEP_BATCH as usize {
                break;
            }
        }

        // Deleting a feedback deletes its attachments; one whose object
        // could not be deleted then is left here.
        for _ in 0..MAX_SWEEP_BATCHES {
            let rows = RemovalRow::find_by_statement(Statement::from_sql_and_values(
                DatabaseBackend::Postgres,
                "SELECT a.id, a.project_id, a.stored_at IS NOT NULL AS stored \
                 FROM error_attachments a WHERE a.owner_kind = 'feedback' \
                   AND NOT EXISTS (SELECT 1 FROM project_feedback f \
                                   WHERE f.id = a.owner_id AND f.project_id = a.project_id) \
                 ORDER BY a.id LIMIT $1",
                [(SWEEP_BATCH as i64).into()],
            ))
            .all(self.db.as_ref())
            .await?;
            if rows.is_empty() {
                break;
            }
            let found = rows.len();
            outcome.orphaned_feedback += self.remove(store, rows).await?;
            if found < SWEEP_BATCH as usize {
                break;
            }
        }

        let deleted_projects = self
            .db
            .query_all(Statement::from_sql_and_values(
                DatabaseBackend::Postgres,
                "SELECT u.project_id FROM error_attachment_usage u \
                 WHERE NOT EXISTS (SELECT 1 FROM projects p \
                                   WHERE p.id = u.project_id AND NOT p.is_deleted) \
                 ORDER BY u.project_id LIMIT $1",
                [(MAX_SWEEP_BATCHES as i64).into()],
            ))
            .await?;
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
                    stored: true,
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
            "SELECT id, project_id, stored_at IS NOT NULL AS stored \
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
    ) -> Result<(i64, AttachmentUsage), ErrorTrackingError> {
        if !(0..=MAX_QUOTA_BYTES).contains(&quota_bytes) {
            return Err(ErrorTrackingError::Validation(format!(
                "quota_bytes must be between 0 and {MAX_QUOTA_BYTES}, got {quota_bytes}"
            )));
        }
        let previous = self
            .db
            .query_one(Statement::from_sql_and_values(
                DatabaseBackend::Postgres,
                "WITH old AS (SELECT quota_bytes FROM error_attachment_usage WHERE project_id = $1) \
                 INSERT INTO error_attachment_usage (project_id, quota_bytes) VALUES ($1, $2) \
                 ON CONFLICT (project_id) DO UPDATE SET quota_bytes = EXCLUDED.quota_bytes, \
                     updated_at = NOW() \
                 RETURNING (SELECT quota_bytes FROM old) AS previous",
                [project_id.into(), quota_bytes.into()],
            ))
            .await?
            .map(|row| row.try_get::<Option<i64>>("", "previous"))
            .transpose()?
            .flatten()
            .unwrap_or(DEFAULT_QUOTA_BYTES);
        Ok((previous, self.usage(project_id).await?))
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
