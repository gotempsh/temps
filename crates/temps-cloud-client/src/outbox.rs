// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! The durable, byte-bounded queue behind Cloud-primary span writes
//! (ADR-041 §3).
//!
//! # Why this exists next to [`crate::spool::Spool`] rather than replacing it
//!
//! Two mechanisms, two roles, neither pretending to be the other.
//!
//! [`Spool`](crate::spool::Spool) is a best-effort *mirror* buffer for projects
//! in the default `Local` write mode. Local storage is authoritative for them,
//! so 10,000 spans in memory, oldest-first eviction and no persistence are all
//! correct: during an incident the newest telemetry is the useful telemetry,
//! and nothing is lost because the real copy is already on disk. It stays
//! exactly as it is.
//!
//! This type serves projects whose spans are **not** written locally at all.
//! Every premise above inverts:
//!
//! | Property | `Spool` (mirror) | `SpanOutbox` (primary) |
//! |---|---|---|
//! | Survives restart | no | yes — rows in Postgres |
//! | Overflow policy | drop **oldest** | reject **newest** at the boundary |
//! | Overflow visibility | a lifetime counter | a gap window with a start and an end |
//! | Producer handoff | `try_send` on an 8-batch channel, whole batches dropped | a durable write on the ingest path, nothing dropped silently |
//! | Bound | 8 MiB of RAM | an operator-set byte cap on disk |
//!
//! Dropping the oldest is the specific failure a primary path must not have: it
//! ships some spans of a trace and discards others, rendering a broken tree
//! that reads as an instrumentation bug rather than an outage. Rejecting at the
//! boundary produces one clean, dated hole instead.
//!
//! # Why Postgres and not a file queue
//!
//! Postgres is unconditionally present — `TEMPS_DATABASE_URL` is bootstrap
//! configuration on every deployment shape, including the ClickHouse-less
//! default. The claim/deliver/dead-letter pattern, its cursor semantics and its
//! operational shape already exist in this codebase and are proven in
//! production for the analytics ClickHouse fan-out (`events_ch_outbox`).
//! Transactional ack removes the "shipped but not marked" window a file queue
//! has to solve with fsync discipline. And a new durability primitive on the
//! one code path whose entire purpose is not losing data would be a new
//! corruption surface for no gain.
//!
//! This is a local write, and the ADR is explicit that it must not be described
//! as "no local writes". It is not the local *span store*: one narrow
//! append-only table, no facet slot columns, no hypertable chunks, no
//! per-attribute indexes, no retention scan, and rows are deleted on ack rather
//! than retained for the retention window.

use std::sync::atomic::{AtomicI64, AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Duration;

use sea_orm::{
    ConnectionTrait, DatabaseBackend, DatabaseConnection, DbErr, FromQueryResult, Statement,
    TransactionTrait,
};
use temps_cloud_protocol::SpanRecord;
use temps_entities::project_telemetry_write_intervals::TelemetryWriteIntervalReason;

/// Rows claimed per shipping attempt.
///
/// Matches `link.rs`'s `BATCH_SIZE` deliberately: the wire batch size is a
/// property of `POST /v1/telemetry`, not of the queue in front of it, and two
/// numbers that must agree should not be written down twice with different
/// names. The throughput lever ADR-041 §3b actually pulls is draining until
/// idle rather than once per tick, not a bigger batch.
pub const OUTBOX_BATCH_SIZE: u32 = 500;

/// Delivery attempts before a row is dead-lettered.
///
/// Ten, the same ceiling `events_ch_outbox` uses. Combined with the flusher's
/// capped backoff this is hours of retrying, which is long enough that anything
/// still failing is a problem only the operator can fix — and dead-lettering is
/// how they find out, rather than the row spinning forever behind newer work.
pub const OUTBOX_MAX_ATTEMPTS: i32 = 10;

/// How long a settled (delivered or spilled) row is kept before the sweep
/// removes it.
///
/// Short, because the outbox is a queue and not a second copy of the telemetry:
/// once Cloud has acknowledged a span, keeping it here is duplicated storage
/// with no reader. An hour is enough to answer "did this actually ship" while
/// debugging, and nothing more. Dead-lettered rows are deliberately **not**
/// deleted — see [`SpanOutbox::sweep_settled`] and
/// [`SpanOutbox::redact_expired_dead_letters`].
pub const OUTBOX_SETTLED_RETENTION: Duration = Duration::from_secs(60 * 60);

/// How long a dead-lettered row keeps the span itself.
///
/// A dead letter is the record of telemetry this instance accepted and never
/// delivered, so the *evidence* — project, attempts, `last_error`, timestamps —
/// is kept until an operator deals with it. The span **content** is a different
/// question: at `Queryable` fidelity it is real customer telemetry, and a row
/// nothing will ever ship has no reason to hold it forever.
///
/// Seven days is long enough that an operator who notices a dead-letter count on
/// the status card during a working week can still look at what failed, and
/// short enough that an unattended instance is not accumulating an
/// unbounded-lifetime plaintext copy of span content. Past it the payload is
/// nulled and everything else stays exactly where it was.
pub const DEAD_LETTER_PAYLOAD_RETENTION: Duration = Duration::from_secs(7 * 24 * 60 * 60);

/// How close two refusals must be for the later one to extend the earlier
/// gap window rather than open a new one.
///
/// Without coalescing, a multi-hour outage at a few hundred spans/second would
/// write one gap row per rejected batch — an unbounded row count during exactly
/// the incident the operator is trying to read about. Five minutes keeps a long
/// outage as one honest interval while still separating two genuinely distinct
/// outages an hour apart.
pub const GAP_COALESCE_WINDOW: Duration = Duration::from_secs(5 * 60);

/// Longest `last_error` persisted per row. Upstream error bodies can be
/// arbitrarily large and this is a control column, not a log.
const LAST_ERROR_MAX_CHARS: usize = 500;

/// Everything that can go wrong talking to the outbox table.
#[derive(Debug, thiserror::Error)]
pub enum SpanOutboxError {
    #[error("Failed to enqueue {span_count} span(s) for project {project_id} into the Temps Cloud telemetry outbox: {source}")]
    Enqueue {
        project_id: i32,
        span_count: usize,
        #[source]
        source: DbErr,
    },

    #[error("Failed to claim a batch from the Temps Cloud telemetry outbox (batch_size {batch_size}): {source}")]
    Claim {
        batch_size: u32,
        #[source]
        source: DbErr,
    },

    #[error("Failed to mark {row_count} Temps Cloud telemetry outbox row(s) as {state}: {source}")]
    Settle {
        row_count: usize,
        state: &'static str,
        #[source]
        source: DbErr,
    },

    #[error("Failed to read Temps Cloud telemetry outbox statistics: {source}")]
    Stats {
        #[source]
        source: DbErr,
    },

    #[error("Failed to purge the Temps Cloud telemetry outbox for project {project_id}: {source}")]
    Purge {
        project_id: i32,
        #[source]
        source: DbErr,
    },

    #[error("Failed to record a telemetry gap window for project {project_id} ({dropped_spans} span(s) dropped): {source}")]
    GapWindow {
        project_id: i32,
        dropped_spans: u64,
        #[source]
        source: DbErr,
    },

    #[error("Failed to serialize a span for project {project_id} into the Temps Cloud telemetry outbox: {source}")]
    Serialize {
        project_id: i32,
        #[source]
        source: serde_json::Error,
    },
}

/// One claimed row, ready to ship.
#[derive(Debug, Clone)]
pub struct ClaimedSpan {
    pub id: i64,
    pub project_id: i32,
    pub span: SpanRecord,
    pub bytes: i64,
}

/// What an enqueue actually did.
///
/// Both numbers are always reported, even when nothing was refused, so a caller
/// never has to infer partial acceptance from a count mismatch.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct EnqueueOutcome {
    pub accepted: usize,
    /// Spans refused because the queue is at its byte cap. Every one of these
    /// is also inside a recorded gap window.
    pub refused: usize,
    pub refused_bytes: u64,
}

impl EnqueueOutcome {
    pub fn refused_any(&self) -> bool {
        self.refused > 0
    }
}

/// Aggregate queue state, for the operator-facing status surfaces.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct OutboxStats {
    pub pending_rows: i64,
    pub pending_bytes: i64,
    pub dead_letter_rows: i64,
    /// Age of the oldest span still waiting, in seconds. `None` when the queue
    /// is empty — which is a different thing from "zero seconds old" and must
    /// not render as one.
    pub oldest_pending_age_secs: Option<i64>,
}

/// What one project never managed to deliver, for the operator-facing surface.
///
/// Metadata only. `last_error` is a bounded, instance-generated failure reason
/// (see [`truncate_error`]); the span payload is never part of this type, and a
/// status endpoint must never grow a field that would carry it.
#[derive(Debug, Clone, PartialEq, Eq, Default, FromQueryResult)]
pub struct DeadLetterSummary {
    pub rows: i64,
    pub last_error: Option<String>,
    pub last_settled_at: Option<chrono::DateTime<chrono::Utc>>,
}

/// The durable queue.
///
/// Cheap to clone through an `Arc`; all state is either in Postgres or in
/// atomics.
pub struct SpanOutbox {
    db: Arc<DatabaseConnection>,
    /// Operator-set ceiling in bytes, read from the singleton `settings` row
    /// (never an environment variable — ADR-041 §3d) and refreshed by the
    /// worker so a change takes effect without a restart.
    max_bytes: AtomicU64,
    /// Running total of `payload_bytes` over `state = 'pending'`.
    ///
    /// Maintained in-process rather than re-summed per enqueue: the enqueue
    /// happens on the ingest path while a permit is held, and a `SUM` over the
    /// backlog there would put an aggregate scan between an exporter and its
    /// acknowledgement. [`SpanOutbox::resync`] re-reads the true value on a
    /// cadence so drift (a restart, an operator's manual `DELETE`) self-heals
    /// rather than accumulating.
    pending_bytes: AtomicI64,
    /// Lifetime counters, never reset — the operator watches these.
    dropped_spans: AtomicU64,
    dropped_bytes: AtomicU64,
    /// Woken on every enqueue so a fresh span does not wait out a poll
    /// interval, exactly as `ch_fanout` does.
    wake: Arc<tokio::sync::Notify>,
}

#[derive(Debug, FromQueryResult)]
struct ClaimedRow {
    id: i64,
    project_id: i32,
    payload: String,
    payload_bytes: i32,
    /// Returned so the claimed batch can be put back into FIFO order.
    ///
    /// `UPDATE ... WHERE id IN (SELECT ... ORDER BY ...) RETURNING ...` selects
    /// the right *set* of rows but returns them in whatever order the executor
    /// produced — Postgres makes no ordering guarantee for `RETURNING`, and in
    /// practice it comes back shuffled. Without this column there is nothing to
    /// re-sort by, and a primary write path would reorder a customer's
    /// telemetry inside every batch it ships.
    enqueued_at: chrono::DateTime<chrono::Utc>,
}

#[derive(Debug, FromQueryResult)]
struct PendingTotals {
    rows: i64,
    bytes: i64,
    oldest_age_secs: Option<f64>,
}

#[derive(Debug, FromQueryResult)]
struct RowCount {
    n: i64,
}

/// Only its presence is read: `RETURNING id` on the gap-window `UPDATE`
/// answers "was there an open window to extend", and the value itself is never
/// needed.
#[derive(Debug, FromQueryResult)]
struct InsertedId {
    #[allow(dead_code)]
    id: i64,
}

impl SpanOutbox {
    pub fn new(db: Arc<DatabaseConnection>, max_bytes: u64) -> Self {
        Self {
            db,
            max_bytes: AtomicU64::new(max_bytes),
            pending_bytes: AtomicI64::new(0),
            dropped_spans: AtomicU64::new(0),
            dropped_bytes: AtomicU64::new(0),
            wake: Arc::new(tokio::sync::Notify::new()),
        }
    }

    /// Apply a new operator-set ceiling.
    ///
    /// Lowering it below the current backlog does **not** discard anything: the
    /// existing rows still ship, and only new spans are refused until the queue
    /// drains under the new cap. Deleting a customer's already-accepted
    /// telemetry because a number was edited would be the worst possible
    /// reading of "set a limit".
    pub fn set_max_bytes(&self, max_bytes: u64) {
        self.max_bytes.store(max_bytes, Ordering::Release);
    }

    pub fn max_bytes(&self) -> u64 {
        self.max_bytes.load(Ordering::Acquire)
    }

    /// Signalled on every accepted enqueue.
    pub fn wake_handle(&self) -> Arc<tokio::sync::Notify> {
        self.wake.clone()
    }

    /// Cached pending byte total. Exact after a [`Self::resync`].
    pub fn pending_bytes(&self) -> i64 {
        self.pending_bytes.load(Ordering::Acquire).max(0)
    }

    /// Spans refused at the cap over this process's lifetime.
    pub fn dropped_spans(&self) -> u64 {
        self.dropped_spans.load(Ordering::Relaxed)
    }

    pub fn dropped_bytes(&self) -> u64 {
        self.dropped_bytes.load(Ordering::Relaxed)
    }

    /// Whether the queue is currently refusing new spans.
    pub fn is_at_capacity(&self) -> bool {
        self.pending_bytes() >= self.max_bytes() as i64
    }

    /// Re-read the true pending byte total from Postgres.
    ///
    /// Called at startup and once per worker cycle. Without it, a restart would
    /// start from zero and happily double the queue past its cap, and an
    /// operator's manual cleanup would leave the counter permanently high.
    pub async fn resync(&self) -> Result<OutboxStats, SpanOutboxError> {
        let stats = self.stats().await?;
        self.pending_bytes
            .store(stats.pending_bytes, Ordering::Release);
        Ok(stats)
    }

    /// Queue depth, size, dead letters and oldest-unshipped age.
    pub async fn stats(&self) -> Result<OutboxStats, SpanOutboxError> {
        let totals = PendingTotals::find_by_statement(Statement::from_sql_and_values(
            DatabaseBackend::Postgres,
            "SELECT COUNT(*)::bigint AS rows, \
                    COALESCE(SUM(payload_bytes), 0)::bigint AS bytes, \
                    EXTRACT(EPOCH FROM (NOW() - MIN(enqueued_at)))::double precision \
                        AS oldest_age_secs \
             FROM cloud_span_outbox WHERE state = 'pending'",
            vec![],
        ))
        .one(self.db.as_ref())
        .await
        .map_err(|source| SpanOutboxError::Stats { source })?;

        let dead = RowCount::find_by_statement(Statement::from_sql_and_values(
            DatabaseBackend::Postgres,
            "SELECT COUNT(*)::bigint AS n FROM cloud_span_outbox WHERE state = 'dead_letter'",
            vec![],
        ))
        .one(self.db.as_ref())
        .await
        .map_err(|source| SpanOutboxError::Stats { source })?;

        let totals = totals.unwrap_or(PendingTotals {
            rows: 0,
            bytes: 0,
            oldest_age_secs: None,
        });

        Ok(OutboxStats {
            pending_rows: totals.rows,
            pending_bytes: totals.bytes,
            dead_letter_rows: dead.map_or(0, |row| row.n),
            // `MIN(enqueued_at)` over an empty set is NULL, which is genuinely
            // "there is no oldest span" and must not become 0.
            oldest_pending_age_secs: totals.oldest_age_secs.map(|secs| secs.max(0.0) as i64),
        })
    }

    /// Per-project pending row count, for the aggregate status surface.
    pub async fn pending_rows_for_project(&self, project_id: i32) -> Result<i64, SpanOutboxError> {
        let row = RowCount::find_by_statement(Statement::from_sql_and_values(
            DatabaseBackend::Postgres,
            "SELECT COUNT(*)::bigint AS n FROM cloud_span_outbox \
             WHERE project_id = $1 AND state = 'pending'",
            vec![project_id.into()],
        ))
        .one(self.db.as_ref())
        .await
        .map_err(|source| SpanOutboxError::Stats { source })?;
        Ok(row.map_or(0, |row| row.n))
    }

    /// Every project that currently has something queued.
    ///
    /// The disconnect spill scopes itself by this rather than by which projects
    /// *declare* `write_mode = cloud`, because those two sets diverge: a project
    /// switched back to `local` while the link was still up correctly leaves its
    /// rows queued for the worker to ship, and if the instance then disconnects
    /// entirely, a declared-mode-scoped spill would miss exactly those rows and
    /// strand them — neither shipped nor written locally, which is the one
    /// outcome ADR-041 §7c says this path never produces.
    pub async fn pending_project_ids(&self) -> Result<Vec<i32>, SpanOutboxError> {
        #[derive(FromQueryResult)]
        struct ProjectId {
            project_id: i32,
        }
        let rows = ProjectId::find_by_statement(Statement::from_sql_and_values(
            DatabaseBackend::Postgres,
            "SELECT DISTINCT project_id FROM cloud_span_outbox \
             WHERE state = 'pending' ORDER BY project_id",
            vec![],
        ))
        .all(self.db.as_ref())
        .await
        .map_err(|source| SpanOutboxError::Stats { source })?;
        Ok(rows.into_iter().map(|row| row.project_id).collect())
    }

    /// What this instance never managed to deliver for one project.
    ///
    /// Metadata only — the count, when the last one gave up and the reason it
    /// gave. The payload is deliberately not part of this: a status surface has
    /// no business rendering span content, and an operator investigating a
    /// dead letter needs to know *why* delivery failed, not what was in it.
    pub async fn dead_letter_summary_for_project(
        &self,
        project_id: i32,
    ) -> Result<DeadLetterSummary, SpanOutboxError> {
        let row = DeadLetterSummary::find_by_statement(Statement::from_sql_and_values(
            DatabaseBackend::Postgres,
            "SELECT COUNT(*)::bigint AS rows, \
                    MAX(settled_at) AS last_settled_at, \
                    ( SELECT last_error FROM cloud_span_outbox \
                      WHERE project_id = $1 AND state = 'dead_letter' \
                      ORDER BY settled_at DESC NULLS LAST, id DESC LIMIT 1 ) AS last_error \
             FROM cloud_span_outbox WHERE project_id = $1 AND state = 'dead_letter'",
            vec![project_id.into()],
        ))
        .one(self.db.as_ref())
        .await
        .map_err(|source| SpanOutboxError::Stats { source })?;
        Ok(row.unwrap_or_default())
    }

    /// Remove every row this project owns, in every state.
    ///
    /// Called when the project is deleted. The table has no foreign key to
    /// `projects` — on purpose, so a project deleted mid-outage cannot wedge the
    /// queue — which also means `ON DELETE CASCADE` does not reach it and the
    /// rows would otherwise outlive the project they belong to *and still be
    /// exported to Cloud*. Deleting an entire project has to mean its queued
    /// telemetry stops existing too.
    ///
    /// The cached pending-byte counter is re-read afterwards; a failure to do so
    /// is not an error, because the worker resyncs on every idle cycle anyway.
    pub async fn purge_project(&self, project_id: i32) -> Result<u64, SpanOutboxError> {
        let removed = Self::purge_project_rows(self.db.as_ref(), project_id).await?;
        if removed > 0 {
            if let Err(error) = self.resync().await {
                tracing::debug!(
                    project_id,
                    %error,
                    "Purged a deleted project's telemetry outbox rows but could not re-read the \
                     queue size; the worker's next idle cycle will correct it"
                );
            }
        }
        Ok(removed)
    }

    /// [`Self::purge_project`] without an outbox instance.
    ///
    /// The outbox is only constructed on an instance that has a Cloud link, but
    /// its rows outlive that link — an instance that linked, queued spans and
    /// then disconnected still has them on disk. Project deletion has to clean
    /// them up either way, so the purge is available with nothing but a
    /// connection.
    pub async fn purge_project_rows<C: ConnectionTrait>(
        db: &C,
        project_id: i32,
    ) -> Result<u64, SpanOutboxError> {
        let result = db
            .execute(Statement::from_sql_and_values(
                DatabaseBackend::Postgres,
                "DELETE FROM cloud_span_outbox WHERE project_id = $1",
                vec![project_id.into()],
            ))
            .await
            .map_err(|source| SpanOutboxError::Purge { project_id, source })?;
        Ok(result.rows_affected())
    }

    /// Remove rows whose project no longer exists, or is fenced for deletion.
    ///
    /// Defense in depth for [`Self::claim`]'s `EXISTS` guard: the guard stops
    /// an orphaned row *shipping*, and this stops it sitting in the queue
    /// forever consuming the operator's byte cap. Run on the worker's idle
    /// sweep, where it is one `DELETE` against an index and normally matches
    /// nothing.
    pub async fn purge_orphaned(&self) -> Result<u64, SpanOutboxError> {
        let result = self
            .db
            .execute(Statement::from_string(
                DatabaseBackend::Postgres,
                "DELETE FROM cloud_span_outbox o \
                 WHERE NOT EXISTS ( \
                     SELECT 1 FROM projects p \
                     WHERE p.id = o.project_id AND p.deleted_at IS NULL \
                 )"
                .to_string(),
            ))
            .await
            .map_err(|source| SpanOutboxError::Purge {
                project_id: 0,
                source,
            })?;
        Ok(result.rows_affected())
    }

    /// Accept spans for `project_id`, up to the byte cap.
    ///
    /// Refuses the **newest** spans at the boundary rather than evicting the
    /// oldest, and records the refusal as a gap window. Returns what was
    /// accepted and what was not; never partially reports.
    ///
    /// This runs on the OTLP ingest path with a permit held, so it is one
    /// multi-row `INSERT` and nothing else — no aggregate, no per-span
    /// round trip, no read-modify-write.
    pub async fn enqueue(
        &self,
        project_id: i32,
        spans: &[SpanRecord],
    ) -> Result<EnqueueOutcome, SpanOutboxError> {
        if spans.is_empty() {
            return Ok(EnqueueOutcome::default());
        }

        let mut payloads: Vec<(String, i32)> = Vec::with_capacity(spans.len());
        for span in spans {
            let payload = serde_json::to_string(span)
                .map_err(|source| SpanOutboxError::Serialize { project_id, source })?;
            let bytes = payload.len().min(i32::MAX as usize) as i32;
            payloads.push((payload, bytes));
        }

        let budget =
            (self.max_bytes() as i64).saturating_sub(self.pending_bytes.load(Ordering::Acquire));
        let (accepted, refused, refused_bytes) = split_at_cap(payloads, budget);

        let accepted_count = accepted.len();
        if accepted_count > 0 {
            let accepted_bytes: i64 = accepted.iter().map(|(_, bytes)| i64::from(*bytes)).sum();
            self.insert_rows(project_id, &accepted).await?;
            self.pending_bytes
                .fetch_add(accepted_bytes, Ordering::AcqRel);
            self.wake.notify_waiters();
        }

        if refused > 0 {
            self.dropped_spans
                .fetch_add(refused as u64, Ordering::Relaxed);
            self.dropped_bytes
                .fetch_add(refused_bytes, Ordering::Relaxed);
            self.record_gap(
                project_id,
                refused as u64,
                refused_bytes,
                TelemetryWriteIntervalReason::QueueOverflowSpill,
            )
            .await?;
        }

        Ok(EnqueueOutcome {
            accepted: accepted_count,
            refused,
            refused_bytes,
        })
    }

    /// One multi-row `INSERT`. Built with positional parameters rather than
    /// string interpolation so a span name can never become SQL.
    async fn insert_rows(
        &self,
        project_id: i32,
        rows: &[(String, i32)],
    ) -> Result<(), SpanOutboxError> {
        // `UNNEST` keeps the statement text constant regardless of batch size,
        // so Postgres can reuse one plan instead of preparing a new statement
        // per distinct batch length.
        let payloads: Vec<sea_orm::Value> = rows
            .iter()
            .map(|(payload, _)| sea_orm::Value::from(payload.clone()))
            .collect();
        let sizes: Vec<sea_orm::Value> = rows
            .iter()
            .map(|(_, bytes)| sea_orm::Value::from(*bytes))
            .collect();

        let sql = "INSERT INTO cloud_span_outbox \
                       (project_id, payload, payload_bytes, enqueued_at, attempts, state) \
                   SELECT $1, payload, payload_bytes, NOW(), 0, 'pending' \
                   FROM UNNEST($2::text[], $3::int[]) AS t(payload, payload_bytes)";

        self.db
            .execute(Statement::from_sql_and_values(
                DatabaseBackend::Postgres,
                sql,
                vec![
                    project_id.into(),
                    sea_orm::Value::Array(
                        sea_orm::sea_query::ArrayType::String,
                        Some(Box::new(payloads)),
                    ),
                    sea_orm::Value::Array(
                        sea_orm::sea_query::ArrayType::Int,
                        Some(Box::new(sizes)),
                    ),
                ],
            ))
            .await
            .map_err(|source| SpanOutboxError::Enqueue {
                project_id,
                span_count: rows.len(),
                source,
            })?;
        Ok(())
    }

    /// Claim up to `batch_size` pending rows, incrementing their attempt count.
    ///
    /// `FOR UPDATE SKIP LOCKED` so two workers (or a worker and a drain
    /// triggered by disconnect) never fight over the same rows, and
    /// `ORDER BY enqueued_at, id` so delivery stays FIFO — a primary path must
    /// not reorder a customer's telemetry.
    ///
    /// A row whose payload no longer deserializes (an older or newer schema,
    /// a manual edit) is dead-lettered rather than retried forever: it can
    /// never succeed, and leaving it in the claim window would starve
    /// everything behind it.
    ///
    /// # Rows whose project is gone are never shipped
    ///
    /// The `EXISTS` guard is the second half of the project-deletion story.
    /// [`Self::purge_project`] removes a deleted project's rows when the
    /// `ProjectDeleted` job runs, but this table deliberately has no foreign
    /// key (see the entity docs), so `ON DELETE CASCADE` does not cover it and
    /// a deletion path that never emits that job — or the window between the
    /// delete committing and the job being handled — would otherwise still
    /// export the project's telemetry. `deleted_at IS NOT NULL` is included
    /// because `begin_project_deletion` sets it as a deletion *fence*
    /// immediately before the hard delete: a project on its way out must not
    /// have another span leave the instance on its behalf.
    ///
    /// Skipped rows are not stranded — [`Self::purge_orphaned`] removes them on
    /// the worker's idle sweep, so they cannot silently consume the byte cap.
    pub async fn claim(&self, batch_size: u32) -> Result<Vec<ClaimedSpan>, SpanOutboxError> {
        let sql = "UPDATE cloud_span_outbox \
                   SET attempts = attempts + 1 \
                   WHERE id IN ( \
                       SELECT o.id FROM cloud_span_outbox o \
                       WHERE o.state = 'pending' AND o.attempts < $1 \
                         AND EXISTS ( \
                             SELECT 1 FROM projects p \
                             WHERE p.id = o.project_id AND p.deleted_at IS NULL \
                         ) \
                       ORDER BY o.enqueued_at, o.id \
                       LIMIT $2 \
                       FOR UPDATE SKIP LOCKED \
                   ) \
                   RETURNING id, project_id, payload, payload_bytes, enqueued_at";

        let mut rows = ClaimedRow::find_by_statement(Statement::from_sql_and_values(
            DatabaseBackend::Postgres,
            sql,
            vec![OUTBOX_MAX_ATTEMPTS.into(), (batch_size as i64).into()],
        ))
        .all(self.db.as_ref())
        .await
        .map_err(|source| SpanOutboxError::Claim { batch_size, source })?;

        // Restore FIFO order. The `ORDER BY` inside the claim subquery picks
        // the right rows; it does not order what `RETURNING` hands back.
        rows.sort_by(|a, b| a.enqueued_at.cmp(&b.enqueued_at).then(a.id.cmp(&b.id)));

        let mut claimed = Vec::with_capacity(rows.len());
        let mut undecodable: Vec<i64> = Vec::new();
        for row in rows {
            match serde_json::from_str::<SpanRecord>(&row.payload) {
                Ok(span) => claimed.push(ClaimedSpan {
                    id: row.id,
                    project_id: row.project_id,
                    span,
                    bytes: i64::from(row.payload_bytes),
                }),
                Err(error) => {
                    tracing::error!(
                        outbox_id = row.id,
                        project_id = row.project_id,
                        %error,
                        "Temps Cloud telemetry outbox row could not be decoded; \
                         dead-lettering it rather than blocking the queue behind it"
                    );
                    undecodable.push(row.id);
                }
            }
        }

        if !undecodable.is_empty() {
            self.dead_letter(&undecodable, "outbox payload could not be decoded")
                .await?;
        }

        Ok(claimed)
    }

    /// Mark rows delivered. Called only after Cloud acknowledges them.
    pub async fn mark_delivered(&self, ids: &[i64]) -> Result<(), SpanOutboxError> {
        self.settle(ids, "delivered", None).await
    }

    /// Mark rows dead-lettered. Never retried again, never swept.
    pub async fn dead_letter(&self, ids: &[i64], reason: &str) -> Result<(), SpanOutboxError> {
        self.settle(ids, "dead_letter", Some(reason)).await
    }

    /// Mark rows as written to the local span store instead of Cloud.
    ///
    /// The caller must have durably stored them locally *before* calling this:
    /// the row is the only copy until then, and settling first would turn a
    /// storage failure into silent loss.
    pub async fn mark_spilled(&self, ids: &[i64]) -> Result<(), SpanOutboxError> {
        self.settle(ids, "spilled_to_local", None).await
    }

    /// Undo a claim's attempt increment.
    ///
    /// Used when the batch was never actually offered to Cloud — the instance
    /// was unlinked, or telemetry export was switched off, between the claim
    /// and the request. Those are not delivery failures, and counting them
    /// would burn the retry budget of every queued span during a disconnect and
    /// dead-letter a queue for a reason that has nothing to do with the spans
    /// in it.
    pub async fn release_claim(&self, ids: &[i64]) -> Result<(), SpanOutboxError> {
        if ids.is_empty() {
            return Ok(());
        }
        self.db
            .execute(Statement::from_sql_and_values(
                DatabaseBackend::Postgres,
                "UPDATE cloud_span_outbox \
                 SET attempts = GREATEST(attempts - 1, 0) \
                 WHERE id = ANY($1) AND state = 'pending'",
                vec![ids.to_vec().into()],
            ))
            .await
            .map_err(|source| SpanOutboxError::Settle {
                row_count: ids.len(),
                state: "pending",
                source,
            })?;
        Ok(())
    }

    /// Record why an attempt failed without settling the rows — they stay
    /// `pending` and will be retried.
    pub async fn record_attempt_failure(
        &self,
        ids: &[i64],
        reason: &str,
    ) -> Result<(), SpanOutboxError> {
        if ids.is_empty() {
            return Ok(());
        }
        self.db
            .execute(Statement::from_sql_and_values(
                DatabaseBackend::Postgres,
                "UPDATE cloud_span_outbox SET last_error = $2 WHERE id = ANY($1)",
                vec![ids.to_vec().into(), truncate_error(reason).into()],
            ))
            .await
            .map_err(|source| SpanOutboxError::Settle {
                row_count: ids.len(),
                state: "pending",
                source,
            })?;

        // A row that has now exhausted its attempts must leave the claim window
        // in the same cycle. Otherwise the worker re-reads it forever and the
        // dead-letter count an operator watches never moves.
        self.db
            .execute(Statement::from_sql_and_values(
                DatabaseBackend::Postgres,
                "UPDATE cloud_span_outbox \
                 SET state = 'dead_letter', settled_at = NOW() \
                 WHERE id = ANY($1) AND state = 'pending' AND attempts >= $2",
                vec![ids.to_vec().into(), OUTBOX_MAX_ATTEMPTS.into()],
            ))
            .await
            .map_err(|source| SpanOutboxError::Settle {
                row_count: ids.len(),
                state: "dead_letter",
                source,
            })?;
        Ok(())
    }

    async fn settle(
        &self,
        ids: &[i64],
        state: &'static str,
        reason: Option<&str>,
    ) -> Result<(), SpanOutboxError> {
        if ids.is_empty() {
            return Ok(());
        }
        let sql = "UPDATE cloud_span_outbox \
                   SET state = $2, settled_at = NOW(), \
                       last_error = COALESCE($3, last_error) \
                   WHERE id = ANY($1) AND state = 'pending' \
                   RETURNING payload_bytes";

        #[derive(FromQueryResult)]
        struct SettledBytes {
            payload_bytes: i32,
        }

        let settled = SettledBytes::find_by_statement(Statement::from_sql_and_values(
            DatabaseBackend::Postgres,
            sql,
            vec![
                ids.to_vec().into(),
                state.to_string().into(),
                reason.map(truncate_error).into(),
            ],
        ))
        .all(self.db.as_ref())
        .await
        .map_err(|source| SpanOutboxError::Settle {
            row_count: ids.len(),
            state,
            source,
        })?;

        let freed: i64 = settled.iter().map(|row| i64::from(row.payload_bytes)).sum();
        if freed > 0 {
            self.pending_bytes.fetch_sub(freed, Ordering::AcqRel);
        }
        Ok(())
    }

    /// Pending rows for the given projects, oldest first, for the disconnect
    /// and quota-fallback spill paths.
    ///
    /// Does **not** settle them — the caller stores them locally first, then
    /// calls [`Self::mark_spilled`].
    pub async fn pending_for_projects(
        &self,
        project_ids: &[i32],
        limit: u32,
    ) -> Result<Vec<ClaimedSpan>, SpanOutboxError> {
        if project_ids.is_empty() {
            return Ok(Vec::new());
        }
        // A plain `SELECT ... ORDER BY` does keep its order, unlike the
        // `RETURNING` in `claim`.
        let rows = ClaimedRow::find_by_statement(Statement::from_sql_and_values(
            DatabaseBackend::Postgres,
            "SELECT id, project_id, payload, payload_bytes, enqueued_at FROM cloud_span_outbox \
             WHERE state = 'pending' AND project_id = ANY($1) \
             ORDER BY enqueued_at, id LIMIT $2",
            vec![project_ids.to_vec().into(), (limit as i64).into()],
        ))
        .all(self.db.as_ref())
        .await
        .map_err(|source| SpanOutboxError::Claim {
            batch_size: limit,
            source,
        })?;

        Ok(rows
            .into_iter()
            .filter_map(|row| {
                serde_json::from_str::<SpanRecord>(&row.payload)
                    .ok()
                    .map(|span| ClaimedSpan {
                        id: row.id,
                        project_id: row.project_id,
                        span,
                        bytes: i64::from(row.payload_bytes),
                    })
            })
            .collect())
    }

    /// Delete settled rows older than [`OUTBOX_SETTLED_RETENTION`].
    ///
    /// Dead letters are excluded from the *delete* on purpose: they are the
    /// record of telemetry this instance accepted and never delivered, and
    /// deleting them on a timer would erase the only evidence that it happened.
    /// An operator removes them deliberately, after looking.
    ///
    /// Their span content is a separate question and is bounded by
    /// [`Self::redact_expired_dead_letters`] — keeping the evidence must not
    /// mean keeping a plaintext copy of the telemetry forever.
    pub async fn sweep_settled(&self) -> Result<u64, SpanOutboxError> {
        let secs = OUTBOX_SETTLED_RETENTION.as_secs() as i64;
        let result = self
            .db
            .execute(Statement::from_sql_and_values(
                DatabaseBackend::Postgres,
                "DELETE FROM cloud_span_outbox \
                 WHERE state IN ('delivered', 'spilled_to_local') \
                   AND settled_at IS NOT NULL \
                   AND settled_at < NOW() - ($1 * INTERVAL '1 second')",
                vec![secs.into()],
            ))
            .await
            .map_err(|source| SpanOutboxError::Stats { source })?;
        Ok(result.rows_affected())
    }

    /// Drop the span content of dead letters older than
    /// [`DEAD_LETTER_PAYLOAD_RETENTION`], keeping everything else.
    ///
    /// The row survives — project, attempts, `last_error`, `enqueued_at`,
    /// `settled_at` — so an operator can still read "this project had N
    /// dead-lettered deliveries, and here is why they failed". Only `payload`,
    /// which at `Queryable` fidelity is real span content that nothing will ever
    /// ship, is nulled. `payload_bytes` is zeroed with it so the queue-size
    /// accounting keeps matching what is actually stored.
    ///
    /// Returns how many rows were redacted.
    pub async fn redact_expired_dead_letters(&self) -> Result<u64, SpanOutboxError> {
        let secs = DEAD_LETTER_PAYLOAD_RETENTION.as_secs() as i64;
        let result = self
            .db
            .execute(Statement::from_sql_and_values(
                DatabaseBackend::Postgres,
                "UPDATE cloud_span_outbox \
                 SET payload = NULL, payload_bytes = 0 \
                 WHERE state = 'dead_letter' \
                   AND payload IS NOT NULL \
                   AND settled_at IS NOT NULL \
                   AND settled_at < NOW() - ($1 * INTERVAL '1 second')",
                vec![secs.into()],
            ))
            .await
            .map_err(|source| SpanOutboxError::Stats { source })?;
        Ok(result.rows_affected())
    }

    /// Open or extend this project's gap window.
    ///
    /// Extending in place is what keeps a long outage to one row. The whole
    /// operation is one transaction so a concurrent enqueue cannot split one
    /// outage into two overlapping windows.
    pub async fn record_gap(
        &self,
        project_id: i32,
        dropped_spans: u64,
        dropped_bytes: u64,
        reason: TelemetryWriteIntervalReason,
    ) -> Result<(), SpanOutboxError> {
        let coalesce_secs = GAP_COALESCE_WINDOW.as_secs() as i64;
        let txn = self
            .db
            .begin()
            .await
            .map_err(|source| SpanOutboxError::GapWindow {
                project_id,
                dropped_spans,
                source,
            })?;

        let extended = InsertedId::find_by_statement(Statement::from_sql_and_values(
            DatabaseBackend::Postgres,
            "UPDATE telemetry_gap_windows \
             SET ended_at = NOW(), \
                 dropped_spans = dropped_spans + $2, \
                 dropped_bytes = dropped_bytes + $3 \
             WHERE id = ( \
                 SELECT id FROM telemetry_gap_windows \
                 WHERE project_id = $1 AND reason = $4 \
                   AND ended_at >= NOW() - ($5 * INTERVAL '1 second') \
                 ORDER BY ended_at DESC LIMIT 1 \
                 FOR UPDATE \
             ) \
             RETURNING id",
            vec![
                project_id.into(),
                (dropped_spans as i64).into(),
                (dropped_bytes as i64).into(),
                reason.to_string().into(),
                coalesce_secs.into(),
            ],
        ))
        .one(&txn)
        .await
        .map_err(|source| SpanOutboxError::GapWindow {
            project_id,
            dropped_spans,
            source,
        })?;

        if extended.is_none() {
            txn.execute(Statement::from_sql_and_values(
                DatabaseBackend::Postgres,
                "INSERT INTO telemetry_gap_windows \
                     (project_id, started_at, ended_at, dropped_spans, dropped_bytes, reason) \
                 VALUES ($1, NOW(), NOW(), $2, $3, $4)",
                vec![
                    project_id.into(),
                    (dropped_spans as i64).into(),
                    (dropped_bytes as i64).into(),
                    reason.to_string().into(),
                ],
            ))
            .await
            .map_err(|source| SpanOutboxError::GapWindow {
                project_id,
                dropped_spans,
                source,
            })?;
        }

        txn.commit()
            .await
            .map_err(|source| SpanOutboxError::GapWindow {
                project_id,
                dropped_spans,
                source,
            })?;
        Ok(())
    }
}

/// Split a batch at the byte budget: what fits, and what is refused.
///
/// The load-bearing policy of ADR-041 §3d, extracted so it is testable without
/// a database.
///
/// **Once the cap is hit, everything after it is refused** — including a small
/// span that would individually still fit behind a refused large one. Letting
/// it through would reorder the queue and produce exactly the artefact the
/// reject-newest policy exists to avoid: some spans of a trace present, others
/// missing, rendering as a broken tree that reads like an instrumentation bug
/// rather than an outage. A contiguous refusal is one honest hole.
fn split_at_cap(payloads: Vec<(String, i32)>, mut budget: i64) -> (Vec<(String, i32)>, usize, u64) {
    let mut accepted: Vec<(String, i32)> = Vec::with_capacity(payloads.len());
    let mut refused = 0usize;
    let mut refused_bytes = 0u64;

    for (payload, bytes) in payloads {
        let cost = i64::from(bytes);
        if refused > 0 || cost > budget {
            refused += 1;
            refused_bytes = refused_bytes.saturating_add(bytes.max(0) as u64);
            continue;
        }
        budget -= cost;
        accepted.push((payload, bytes));
    }

    (accepted, refused, refused_bytes)
}

/// Bound a persisted failure reason. Counts *characters*, never bytes, so a
/// multi-byte message cannot be split into invalid UTF-8.
fn truncate_error(reason: &str) -> String {
    let mut out: String = reason.chars().take(LAST_ERROR_MAX_CHARS).collect();
    if reason.chars().count() > LAST_ERROR_MAX_CHARS {
        out.push('…');
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn span(i: i64) -> SpanRecord {
        SpanRecord {
            trace_id: format!("trace-{i}"),
            span_id: format!("span-{i}"),
            name: "GET /".into(),
            ts_millis: i,
            duration_ms: 1.0,
            attributes: Default::default(),
            ..Default::default()
        }
    }

    fn outbox(max_bytes: u64) -> SpanOutbox {
        // MockDatabase is enough for the pure-accounting assertions below; the
        // SQL paths are exercised against real Postgres in the integration and
        // load tests.
        let db = sea_orm::MockDatabase::new(DatabaseBackend::Postgres).into_connection();
        SpanOutbox::new(Arc::new(db), max_bytes)
    }

    #[test]
    fn a_short_reason_is_left_alone() {
        assert_eq!(truncate_error("connection reset"), "connection reset");
    }

    #[test]
    fn a_long_reason_is_bounded_without_splitting_a_character() {
        let reason = "é".repeat(LAST_ERROR_MAX_CHARS + 50);
        let out = truncate_error(&reason);
        assert_eq!(out.chars().count(), LAST_ERROR_MAX_CHARS + 1);
        assert!(out.ends_with('…'));
        assert!(out.starts_with('é'));
    }

    #[test]
    fn a_queued_span_survives_the_round_trip_through_the_payload_column() {
        // The queue stores each span as JSON and hands it back to the shipper
        // later, possibly after a restart. If that round trip were lossy the
        // loss would be invisible: the row count would still be right.
        let original = span(42);
        let payload = serde_json::to_string(&original).expect("a span must serialize");
        let restored: SpanRecord =
            serde_json::from_str(&payload).expect("a stored payload must deserialize");

        assert_eq!(restored.trace_id, original.trace_id);
        assert_eq!(restored.span_id, original.span_id);
        assert_eq!(restored.name, original.name);
        assert_eq!(restored.ts_millis, original.ts_millis);
        assert_eq!(restored.duration_ms, original.duration_ms);
        assert_eq!(
            serde_json::to_string(&restored).expect("must re-serialize"),
            payload,
            "a stored span must be byte-identical after a round trip"
        );
    }

    #[test]
    fn the_recorded_byte_size_is_the_payload_that_is_actually_stored() {
        // `payload_bytes` is what the cap is enforced against. Measuring
        // anything other than the stored string would let the queue grow past
        // the ceiling the operator set.
        let payload = serde_json::to_string(&span(7)).expect("a span must serialize");
        let recorded = payload.len().min(i32::MAX as usize) as i32;
        assert_eq!(recorded as usize, payload.len());
        assert!(recorded > 0);
    }

    #[test]
    fn the_cap_can_be_raised_at_runtime_without_a_restart() {
        let outbox = outbox(1024);
        assert_eq!(outbox.max_bytes(), 1024);
        outbox.set_max_bytes(4096);
        assert_eq!(outbox.max_bytes(), 4096);
    }

    #[test]
    fn lowering_the_cap_below_the_backlog_does_not_discard_anything() {
        // Editing a number must never delete telemetry the instance already
        // accepted. Only *new* spans are refused until the queue drains.
        let outbox = outbox(1_000_000);
        outbox.pending_bytes.store(900_000, Ordering::Release);
        outbox.set_max_bytes(1_000);
        assert_eq!(outbox.pending_bytes(), 900_000);
        assert!(outbox.is_at_capacity());
        assert_eq!(outbox.dropped_spans(), 0);
    }

    #[test]
    fn an_empty_queue_is_not_at_capacity() {
        let outbox = outbox(1024);
        assert!(!outbox.is_at_capacity());
        assert_eq!(outbox.pending_bytes(), 0);
    }

    #[test]
    fn a_negative_byte_total_reads_as_zero_rather_than_underflowing() {
        // Settling rows an out-of-process cleanup already removed could push
        // the cached counter negative; a negative queue size must never be
        // shown to an operator or compared against the cap.
        let outbox = outbox(1024);
        outbox.pending_bytes.store(-50, Ordering::Release);
        assert_eq!(outbox.pending_bytes(), 0);
    }

    #[tokio::test]
    async fn enqueueing_nothing_is_not_an_error_and_touches_no_state() {
        let outbox = outbox(1024);
        let outcome = outbox.enqueue(7, &[]).await.expect("empty enqueue");
        assert_eq!(outcome, EnqueueOutcome::default());
        assert!(!outcome.refused_any());
        assert_eq!(outbox.pending_bytes(), 0);
    }

    // ── The byte cap: reject the newest, contiguously (ADR-041 §3d) ──────

    fn sized(bytes: i32) -> (String, i32) {
        ("x".repeat(bytes.max(0) as usize), bytes)
    }

    #[test]
    fn everything_is_accepted_when_the_batch_fits() {
        let (accepted, refused, refused_bytes) =
            split_at_cap(vec![sized(10), sized(10), sized(10)], 100);
        assert_eq!(accepted.len(), 3);
        assert_eq!(refused, 0);
        assert_eq!(refused_bytes, 0);
    }

    #[test]
    fn at_the_cap_the_newest_spans_are_refused_and_counted_exactly() {
        // Two fit, the rest do not. Nothing already queued is evicted, and the
        // refusal count and byte total are exact rather than approximate — the
        // operator sizes their cap against these numbers.
        let (accepted, refused, refused_bytes) =
            split_at_cap(vec![sized(10), sized(10), sized(10), sized(10)], 25);
        assert_eq!(accepted.len(), 2);
        assert_eq!(refused, 2);
        assert_eq!(refused_bytes, 20);
    }

    #[test]
    fn a_refusal_is_contiguous_even_when_a_later_span_would_fit() {
        // A 1-byte span behind a refused 100-byte one must NOT slip through.
        // Letting it would reorder the queue and produce a half-present trace,
        // which reads as an instrumentation bug rather than an outage.
        let (accepted, refused, refused_bytes) =
            split_at_cap(vec![sized(5), sized(100), sized(1)], 50);
        assert_eq!(accepted.len(), 1, "only the first span fits");
        assert_eq!(refused, 2, "the span after the boundary is refused too");
        assert_eq!(refused_bytes, 101);
    }

    #[test]
    fn a_zero_budget_refuses_everything_without_evicting_anything() {
        let (accepted, refused, refused_bytes) = split_at_cap(vec![sized(1), sized(1)], 0);
        assert!(accepted.is_empty());
        assert_eq!(refused, 2);
        assert_eq!(refused_bytes, 2);
    }

    #[test]
    fn a_negative_budget_is_treated_as_full_rather_than_wrapping() {
        // A resync against a queue that grew out from under the cached counter
        // can leave the budget negative. That must mean "full", never "infinite
        // room" via an unsigned wrap.
        let (accepted, refused, _) = split_at_cap(vec![sized(1)], -500);
        assert!(accepted.is_empty());
        assert_eq!(refused, 1);
    }

    #[test]
    fn an_empty_batch_produces_no_refusal() {
        let (accepted, refused, refused_bytes) = split_at_cap(Vec::new(), 100);
        assert!(accepted.is_empty());
        assert_eq!(refused, 0);
        assert_eq!(refused_bytes, 0);
    }

    #[test]
    fn a_single_span_larger_than_the_whole_cap_is_refused_not_split() {
        let (accepted, refused, refused_bytes) = split_at_cap(vec![sized(1000)], 100);
        assert!(accepted.is_empty());
        assert_eq!(refused, 1);
        assert_eq!(refused_bytes, 1000);
    }

    #[test]
    fn the_batch_size_matches_the_wire_batch_size() {
        // Two numbers that must agree should not be written down twice.
        assert_eq!(OUTBOX_BATCH_SIZE, 500);
    }

    #[test]
    fn the_default_settled_retention_is_short_because_the_outbox_is_a_queue() {
        assert_eq!(OUTBOX_SETTLED_RETENTION, Duration::from_secs(3600));
    }

    // ── Retention of what nothing will ever ship ─────────────────────────

    #[test]
    fn a_dead_letter_keeps_its_span_content_for_long_enough_to_be_investigated() {
        // Short enough that an unattended instance is not holding a plaintext
        // copy of customer telemetry forever; long enough that an operator who
        // sees the count on the status card during a working week can still
        // look at what failed.
        assert_eq!(
            DEAD_LETTER_PAYLOAD_RETENTION,
            Duration::from_secs(7 * 24 * 3600)
        );
        assert!(
            DEAD_LETTER_PAYLOAD_RETENTION > OUTBOX_SETTLED_RETENTION,
            "a delivery that failed must be inspectable for longer than one that \
             succeeded, or the dead-letter count would point at nothing"
        );
    }

    #[test]
    fn the_dead_letter_summary_carries_no_span_content() {
        // A status surface has no business rendering telemetry. This asserts the
        // *shape*: the only free-text field is the instance-generated failure
        // reason, which `truncate_error` already bounds.
        let summary = DeadLetterSummary {
            rows: 3,
            last_error: Some("connection reset".into()),
            last_settled_at: Some(chrono::Utc::now()),
        };
        assert_eq!(summary.rows, 3);
        assert_eq!(summary.last_error.as_deref(), Some("connection reset"));
        // An empty queue reports zero rows and no reason, never a fabricated one.
        assert_eq!(DeadLetterSummary::default().rows, 0);
        assert!(DeadLetterSummary::default().last_error.is_none());
    }
}
