// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! One-shot Temps Cloud telemetry backfill (ADR-040 §1).
//!
//! Raising a project's `cloud_telemetry_fidelity` from `metered` to
//! `queryable` only affects spans ingested *after* the change. Without a
//! backfill that leaves a permanent hole between "link established" and
//! "fidelity raised" — precisely the window an operator cutting over to Cloud
//! retention wants to read. This module closes it.
//!
//! Modelled directly on `temps-analytics-events`'
//! `services::ch_backfill`, which already solves this shape:
//!
//! - **Out of process.** Driven by `temps backfill cloud-telemetry`, never by
//!   `temps serve`, so it cannot contend with live ingest for row locks or for
//!   the ingest semaphore.
//! - **Cursor-based.** Keyset pagination over `(start_time, id)` on Postgres
//!   and `(start_time, span_id)` on ClickHouse, so ordering stays stable even
//!   when many spans share a millisecond.
//! - **Resumable and safe to re-run.** The caller persists
//!   [`CloudBackfillCursor`] between invocations. Re-sending is harmless:
//!   Cloud dedupes on `submission_id` plus `(trace_id, span_id, ts)`.
//! - **Batched.** One [`CloudLink`] submission per batch, reusing the live
//!   path's metering, retry and idempotency semantics rather than a second
//!   transport that would have to reimplement them.
//!
//! # Why it refuses at `metered` fidelity
//!
//! Re-sending the `metered` projection would bill the operator for rows that
//! are, by construction, unreadable — the exact outcome the `--dry-run`
//! requirement exists to prevent. A `metered` project therefore gets
//! [`CloudBackfillError::FidelityNotQueryable`], which names the setting to
//! change. "Not set up yet" must be distinguishable from "broken".

use std::sync::Arc;
use std::time::Duration;

use sea_orm::{
    ConnectionTrait, DatabaseBackend, DatabaseConnection, FromQueryResult, Statement, Value,
};
use temps_cloud_client::{CloudLink, FlushOutcome, SubmissionScope};
use temps_core::DBDateTime;
use temps_entities::cloud_telemetry_fidelity::CloudTelemetryFidelity;
use tracing::debug;

use crate::services::cloud_fidelity::CloudTelemetryPolicy;
use crate::services::otel_service::cloud_span;
use crate::types::{ResourceInfo, SpanKind, SpanRecord, SpanStatusCode};

const WINDOW_LOG_TARGET: &str = "temps_otel::services::cloud_backfill::window";

/// Rows read per keyset page, and spans offered per Cloud submission.
///
/// Matches `temps-cloud-client`'s own per-submission `BATCH_SIZE`, so one page
/// maps to exactly one submission and the bounded producer channel between
/// [`CloudLink::record`] and its spool is never asked to hold more than it can.
pub const DEFAULT_BATCH_SIZE: u64 = 500;

/// Hard ceiling on the batch size a caller may ask for, for the same reason.
pub const MAX_BATCH_SIZE: u64 = 500;

/// How many spans `--dry-run` projects to derive an average wire size.
///
/// The estimate is `average_bytes * row_count`, not a full projection of the
/// window: projecting millions of spans to answer "how much will this cost"
/// would itself cost more than the answer is worth.
pub const ESTIMATE_SAMPLE_SIZE: u64 = 1_000;

/// Upper bound on flush attempts per batch before the run gives up and reports
/// what it managed. Prevents a permanently-refusing backend from spinning.
const MAX_FLUSHES_PER_BATCH: usize = 8;

#[derive(Debug, thiserror::Error)]
pub enum CloudBackfillError {
    #[error(
        "Failed to read local spans for project {project_id} in [{from}, {to}] \
         from the Postgres `otel_spans` table: {source}"
    )]
    Database {
        project_id: i32,
        from: String,
        to: String,
        #[source]
        source: sea_orm::DbErr,
    },

    #[error(
        "Failed to read local spans for project {project_id} in [{from}, {to}] \
         from the ClickHouse `spans` table: {reason}"
    )]
    ClickHouse {
        project_id: i32,
        from: String,
        to: String,
        reason: String,
    },

    #[error(
        "This instance is not linked to Temps Cloud, so there is nowhere to \
         backfill project {project_id} to. Link it from Settings → Cloud first."
    )]
    NotLinked { project_id: i32 },

    #[error(
        "Telemetry export to Temps Cloud is switched off for this instance, so \
         project {project_id} cannot be backfilled. Enable it from \
         Settings → Cloud first."
    )]
    TelemetryExportDisabled { project_id: i32 },

    #[error(
        "Project {project_id} is at `{fidelity}` telemetry fidelity. Backfilling \
         it would re-send metering-only rows that cost money and can never be \
         read back. Raise `cloud_telemetry_fidelity` to `queryable` for this \
         project first, then re-run."
    )]
    FidelityNotQueryable {
        project_id: i32,
        fidelity: CloudTelemetryFidelity,
    },

    #[error(
        "Could not project span {span_id} of project {project_id} for Temps \
         Cloud. The link credential is unavailable; re-enroll this instance."
    )]
    ProjectionFailed { project_id: i32, span_id: String },

    #[error(
        "Another Temps Cloud submission is already in flight on this instance, \
         so project {project_id} cannot be backfilled right now: {reason}"
    )]
    SubmissionScopeBusy { project_id: i32, reason: String },

    #[error(
        "Temps Cloud did not accept a batch of {spans} span(s) for project \
         {project_id} (resume from start_time {resume_from}): {reason}"
    )]
    ShipmentRefused {
        project_id: i32,
        spans: usize,
        resume_from: String,
        reason: String,
    },
}

/// Where local spans are read from.
///
/// An instance stores spans in exactly one of these (ADR-016 selects
/// ClickHouse when `TEMPS_CLICKHOUSE_*` is configured, TimescaleDB otherwise).
/// The backfill reads whichever one actually holds the data, so it does not
/// silently copy zero rows on a ClickHouse deployment.
pub enum CloudBackfillSource {
    /// The default backend: the Postgres/TimescaleDB `otel_spans` hypertable.
    Timescale(Arc<DatabaseConnection>),
    /// The ADR-016 backend: the ClickHouse `spans` table.
    ClickHouse(Arc<::clickhouse::Client>),
}

/// Names the table only. Neither inner client is printed: a
/// `DatabaseConnection` and a `clickhouse::Client` both carry connection
/// strings with credentials in them, and this type ends up in `expect`/error
/// output.
impl std::fmt::Debug for CloudBackfillSource {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CloudBackfillSource")
            .field("table", &self.describe())
            .finish()
    }
}

impl CloudBackfillSource {
    /// Human-readable name of the table this source reads, for operator output.
    pub fn describe(&self) -> &'static str {
        match self {
            CloudBackfillSource::Timescale(_) => "PostgreSQL `otel_spans`",
            CloudBackfillSource::ClickHouse(_) => "ClickHouse `spans`",
        }
    }
}

/// Keyset cursor. Serializable by the caller so a run can resume after a crash
/// or a deliberate stop.
///
/// Both tiebreakers are carried because the two sources order by different
/// ones; whichever is irrelevant for the active source is simply ignored.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct CloudBackfillCursor {
    pub last_start_time: Option<DBDateTime>,
    /// Postgres `otel_spans.id` of the last projected span.
    pub last_row_id: Option<i64>,
    /// ClickHouse tiebreaker, where there is no row id.
    pub last_span_id: Option<String>,
}

/// Outcome of one backfill window.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct CloudBackfillReport {
    /// Spans read out of local storage.
    pub spans_read: u64,
    /// Spans that survived projection and were offered to the link.
    pub spans_offered: u64,
    /// Spans Cloud acknowledged.
    pub spans_shipped: u64,
    pub batches: u64,
    /// Sum of the serialized size of every offered record.
    ///
    /// An estimate by construction: Cloud meters on its own receive-side
    /// accounting and echoes the authoritative figure back in `IngestAck`.
    pub estimated_metered_bytes: u64,
    pub final_cursor: CloudBackfillCursor,
}

/// What `--dry-run` reports. Nothing has been sent when this is produced.
#[derive(Debug, Clone, PartialEq)]
pub struct CloudBackfillEstimate {
    pub project_id: i32,
    /// Exact count of local spans in the window.
    pub spans: u64,
    /// `average_projected_bytes * spans`, rounded up.
    pub estimated_metered_bytes: u64,
    /// How many spans were actually projected to derive the average.
    pub sampled_spans: u64,
    /// Mean serialized size of a projected record, over the sample.
    pub average_span_bytes: f64,
    /// The fidelity the run would use.
    pub fidelity: CloudTelemetryFidelity,
    /// How many attribute keys the project allows through. Zero means no
    /// attribute values leave at all, even at `queryable`.
    pub allowlisted_attribute_keys: usize,
}

/// Count local spans in `[from, to]` for one project.
pub async fn count_spans_window(
    source: &CloudBackfillSource,
    project_id: i32,
    from: DBDateTime,
    to: DBDateTime,
) -> Result<u64, CloudBackfillError> {
    match source {
        CloudBackfillSource::Timescale(db) => {
            #[derive(FromQueryResult)]
            struct CountRow {
                total: i64,
            }

            let row = CountRow::find_by_statement(Statement::from_sql_and_values(
                DatabaseBackend::Postgres,
                "SELECT COUNT(*)::bigint AS total FROM otel_spans \
                 WHERE project_id = $1 AND start_time >= $2 AND start_time <= $3",
                vec![project_id.into(), from.into(), to.into()],
            ))
            .one(db.as_ref())
            .await
            .map_err(|source| CloudBackfillError::Database {
                project_id,
                from: from.to_rfc3339(),
                to: to.to_rfc3339(),
                source,
            })?;
            Ok(row.map(|r| r.total.max(0) as u64).unwrap_or(0))
        }
        CloudBackfillSource::ClickHouse(ch) => {
            let total = ch
                .query(
                    "SELECT count() FROM spans \
                     WHERE project_id = ? AND start_time >= fromUnixTimestamp64Milli(?) \
                       AND start_time <= fromUnixTimestamp64Milli(?)",
                )
                .bind(project_id)
                .bind(from.timestamp_millis())
                .bind(to.timestamp_millis())
                .fetch_one::<u64>()
                .await
                .map_err(|error| CloudBackfillError::ClickHouse {
                    project_id,
                    from: from.to_rfc3339(),
                    to: to.to_rfc3339(),
                    reason: error.to_string(),
                })?;
            Ok(total)
        }
    }
}

/// Answer "how many rows, and roughly how many metered bytes" **without
/// sending anything**.
///
/// This is a hard requirement of ADR-040: an operator must be able to answer
/// "what exactly am I about to send, and what will it cost" before the send,
/// not after the invoice. Nothing in this function touches the network.
pub async fn estimate_backfill(
    source: &CloudBackfillSource,
    link: &CloudLink,
    policy: &CloudTelemetryPolicy,
    project_id: i32,
    from: DBDateTime,
    to: DBDateTime,
) -> Result<CloudBackfillEstimate, CloudBackfillError> {
    guard_ready(link, policy, project_id)?;

    let spans = count_spans_window(source, project_id, from, to).await?;
    let sample = if spans == 0 {
        Vec::new()
    } else {
        fetch_batch(
            source,
            project_id,
            from,
            to,
            &CloudBackfillCursor::default(),
            ESTIMATE_SAMPLE_SIZE.min(spans),
        )
        .await?
    };

    let mut sampled_bytes = 0u64;
    let mut sampled_spans = 0u64;
    for sourced in &sample {
        let Some(record) = cloud_span(link, &sourced.span, policy) else {
            return Err(CloudBackfillError::ProjectionFailed {
                project_id,
                span_id: sourced.span.span_id.clone(),
            });
        };
        sampled_bytes = sampled_bytes.saturating_add(record_bytes(&record));
        sampled_spans += 1;
    }

    let average_span_bytes = if sampled_spans == 0 {
        0.0
    } else {
        sampled_bytes as f64 / sampled_spans as f64
    };

    Ok(CloudBackfillEstimate {
        project_id,
        spans,
        estimated_metered_bytes: (average_span_bytes * spans as f64).ceil() as u64,
        sampled_spans,
        average_span_bytes,
        fidelity: policy.fidelity,
        allowlisted_attribute_keys: policy.attribute_allowlist.len(),
    })
}

/// Project the first span of the window exactly as it would be sent, **without
/// sending it**.
///
/// Backs the `--dry-run` output. A byte count answers "what will this cost";
/// it does not answer "what exactly am I sending", and ADR-040 requires both to
/// be answerable before the send. Returns `None` when the window is empty.
pub async fn project_example_span(
    source: &CloudBackfillSource,
    link: &CloudLink,
    policy: &CloudTelemetryPolicy,
    project_id: i32,
    from: DBDateTime,
    to: DBDateTime,
) -> Result<Option<temps_cloud_protocol::SpanRecord>, CloudBackfillError> {
    guard_ready(link, policy, project_id)?;

    let batch = fetch_batch(
        source,
        project_id,
        from,
        to,
        &CloudBackfillCursor::default(),
        1,
    )
    .await?;
    let Some(first) = batch.first() else {
        return Ok(None);
    };
    match cloud_span(link, &first.span, policy) {
        Some(record) => Ok(Some(record)),
        None => Err(CloudBackfillError::ProjectionFailed {
            project_id,
            span_id: first.span.span_id.clone(),
        }),
    }
}

/// Ship every local span in `[from, to]` for one project to Temps Cloud at the
/// project's configured fidelity.
///
/// `start_cursor` resumes a previous run; pass [`CloudBackfillCursor::default`]
/// for a fresh start. `on_progress` is invoked after every acknowledged batch
/// with the running totals so a CLI can drive a progress bar without this
/// module depending on one.
///
/// `rate_limit` sleeps between batches so a backfill on a live instance does
/// not monopolise local read IO or the Cloud ingest allowance.
#[allow(clippy::too_many_arguments)]
pub async fn backfill_cloud_telemetry_window(
    source: &CloudBackfillSource,
    link: &CloudLink,
    policy: &CloudTelemetryPolicy,
    project_id: i32,
    from: DBDateTime,
    to: DBDateTime,
    batch_size: u64,
    start_cursor: CloudBackfillCursor,
    rate_limit: Option<Duration>,
    mut on_progress: impl FnMut(&CloudBackfillReport),
) -> Result<CloudBackfillReport, CloudBackfillError> {
    guard_ready(link, policy, project_id)?;

    // ADR-042 §2 (Finding 2): submit through a scope of our own rather than the
    // link's shared spool. On a running instance the live mirror records into
    // that spool continuously, so `link.spooled()` after our flushes would also
    // count the mirror's spans — this run's shipped counter would include
    // traffic it never read, and its drain-complete test could never settle on
    // a busy instance. The scope is held for the whole window and released when
    // this function returns, however it returns.
    let scope =
        link.submission_scope()
            .map_err(|error| CloudBackfillError::SubmissionScopeBusy {
                project_id,
                reason: error.to_string(),
            })?;

    let batch_size = batch_size.clamp(1, MAX_BATCH_SIZE);

    // Kept below INFO: this runs once per window while the CLI's progress bar
    // owns the terminal, and an INFO line per window would push the bar onto a
    // new row every time (the same reason `ch_backfill` uses `debug!`).
    debug!(
        target: WINDOW_LOG_TARGET,
        project_id,
        from = %from,
        to = %to,
        batch_size,
        source = source.describe(),
        fidelity = %policy.fidelity,
        "cloud telemetry backfill starting window"
    );

    let mut report = CloudBackfillReport {
        final_cursor: start_cursor,
        ..Default::default()
    };

    loop {
        let batch = fetch_batch(
            source,
            project_id,
            from,
            to,
            &report.final_cursor,
            batch_size,
        )
        .await?;
        if batch.is_empty() {
            break;
        }

        let read = batch.len() as u64;
        let next_cursor = cursor_after(&batch);

        let mut projected = Vec::with_capacity(batch.len());
        let mut batch_bytes = 0u64;
        for sourced in &batch {
            let Some(record) = cloud_span(link, &sourced.span, policy) else {
                return Err(CloudBackfillError::ProjectionFailed {
                    project_id,
                    span_id: sourced.span.span_id.clone(),
                });
            };
            batch_bytes = batch_bytes.saturating_add(record_bytes(&record));
            projected.push(record);
        }

        let offered = projected.len() as u64;
        let shipped = ship_batch(&scope, project_id, projected, &next_cursor).await?;

        report.spans_read += read;
        report.spans_offered += offered;
        report.spans_shipped += shipped;
        report.estimated_metered_bytes = report.estimated_metered_bytes.saturating_add(batch_bytes);
        report.batches += 1;
        report.final_cursor = next_cursor;
        on_progress(&report);

        debug!(
            project_id,
            read,
            offered,
            shipped,
            total_shipped = report.spans_shipped,
            batches = report.batches,
            "cloud telemetry backfill shipped batch"
        );

        if let Some(delay) = rate_limit {
            tokio::time::sleep(delay).await;
        }

        if read < batch_size {
            break;
        }
    }

    debug!(
        target: WINDOW_LOG_TARGET,
        project_id,
        spans_shipped = report.spans_shipped,
        batches = report.batches,
        "cloud telemetry backfill window complete"
    );

    Ok(report)
}

// ---------------------------------------------------------------------------
// Internals
// ---------------------------------------------------------------------------

/// Refuse early, and specifically, rather than partway through a paid send.
fn guard_ready(
    link: &CloudLink,
    policy: &CloudTelemetryPolicy,
    project_id: i32,
) -> Result<(), CloudBackfillError> {
    if !link.is_linked() {
        return Err(CloudBackfillError::NotLinked { project_id });
    }
    if !link.telemetry_enabled() {
        return Err(CloudBackfillError::TelemetryExportDisabled { project_id });
    }
    if !policy.fidelity.is_queryable() {
        return Err(CloudBackfillError::FidelityNotQueryable {
            project_id,
            fidelity: policy.fidelity,
        });
    }
    Ok(())
}

/// Serialized size of one record — the estimate's unit.
///
/// Serializing is deliberate here (unlike the ingest path's cheap size
/// approximation): this number is what an operator decides a spend against, so
/// it should reflect the actual bytes rather than a heuristic.
fn record_bytes(record: &temps_cloud_protocol::SpanRecord) -> u64 {
    serde_json::to_vec(record)
        .map(|b| b.len() as u64)
        .unwrap_or(0)
}

fn cursor_after(batch: &[SourcedSpan]) -> CloudBackfillCursor {
    match batch.last() {
        Some(last) => CloudBackfillCursor {
            last_start_time: Some(last.span.start_time),
            last_row_id: last.row_id,
            last_span_id: Some(last.span.span_id.clone()),
        },
        None => CloudBackfillCursor::default(),
    }
}

/// Offer one batch to the link and drain it, so a batch is fully acknowledged
/// before its cursor advances.
///
/// Goes through the link's own submission path rather than a second HTTP client
/// on purpose: that reuses the live mirror's `submission_id` idempotency and its
/// metering — the properties ADR-040 relies on to make a re-run safe.
///
/// It does so through a [`SubmissionScope`] (ADR-042 §2), not
/// [`CloudLink::record`]/[`CloudLink::flush`], so that everything counted here
/// is this batch and only this batch. Those two share one spool with the live
/// mirror, which is invisible today only because the CLI runs with the server
/// stopped; the moment this code runs inside `temps serve` the mirror is
/// recording into that spool continuously, and it would both inflate `shipped`
/// and keep the drain-complete test below from ever settling on a busy
/// instance. With a scope, no amount of concurrent mirror traffic can do
/// either.
async fn ship_batch(
    scope: &SubmissionScope<'_>,
    project_id: i32,
    projected: Vec<temps_cloud_protocol::SpanRecord>,
    cursor: &CloudBackfillCursor,
) -> Result<u64, CloudBackfillError> {
    let offered = projected.len();
    if offered == 0 {
        return Ok(0);
    }

    let resume_from = cursor
        .last_start_time
        .map(|ts| ts.to_rfc3339())
        .unwrap_or_else(|| "the start of the window".to_string());

    let dropped_before = scope.dropped();
    scope.record(projected);

    let mut shipped = 0u64;
    for _ in 0..MAX_FLUSHES_PER_BATCH {
        match scope.flush().await {
            FlushOutcome::Shipped { spans } => {
                shipped += spans as u64;
                if scope.spooled() == 0 {
                    break;
                }
            }
            FlushOutcome::Idle => break,
            FlushOutcome::NotLinked => {
                return Err(CloudBackfillError::ShipmentRefused {
                    project_id,
                    spans: offered,
                    resume_from,
                    reason: "the link was revoked or telemetry export was switched off \
                             while the backfill was running"
                        .to_string(),
                })
            }
            FlushOutcome::Retained { reason, .. } | FlushOutcome::Blocked { reason, .. } => {
                return Err(CloudBackfillError::ShipmentRefused {
                    project_id,
                    spans: offered,
                    resume_from,
                    reason,
                })
            }
        }
    }

    let still_queued = scope.spooled();
    if still_queued > 0 {
        return Err(CloudBackfillError::ShipmentRefused {
            project_id,
            spans: offered,
            resume_from,
            reason: format!(
                "{still_queued} span(s) were still queued after {MAX_FLUSHES_PER_BATCH} \
                 delivery attempts"
            ),
        });
    }

    // An empty queue is only proof of delivery if nothing was discarded on the
    // way. The operator is paying for this history, so a shed span must stop
    // the run at a cursor it can resume from rather than leave a hole nobody
    // ever hears about.
    let dropped = scope.dropped().saturating_sub(dropped_before);
    if dropped > 0 {
        return Err(CloudBackfillError::ShipmentRefused {
            project_id,
            spans: offered,
            resume_from,
            reason: format!(
                "{dropped} span(s) were discarded before delivery because the submission \
                 buffer was full"
            ),
        });
    }

    Ok(shipped)
}

/// One local span plus the tiebreaker the source ordered it by.
///
/// The two backends key on different things — Postgres has a row `id`,
/// ClickHouse only has `span_id` — and neither belongs on [`SpanRecord`],
/// which is the shape that gets projected and mirrored.
struct SourcedSpan {
    span: SpanRecord,
    /// `otel_spans.id`; `None` on ClickHouse.
    row_id: Option<i64>,
}

async fn fetch_batch(
    source: &CloudBackfillSource,
    project_id: i32,
    from: DBDateTime,
    to: DBDateTime,
    cursor: &CloudBackfillCursor,
    batch_size: u64,
) -> Result<Vec<SourcedSpan>, CloudBackfillError> {
    match source {
        CloudBackfillSource::Timescale(db) => {
            fetch_timescale_batch(db.as_ref(), project_id, from, to, cursor, batch_size).await
        }
        CloudBackfillSource::ClickHouse(ch) => {
            fetch_clickhouse_batch(ch.as_ref(), project_id, from, to, cursor, batch_size).await
        }
    }
}

/// Keyset page over `(start_time, id)` — expressed as
/// `(start_time > last) OR (start_time = last AND id > last_id)` so Postgres
/// can use the `(start_time, id)` ordering rather than sorting the window.
const TIMESCALE_PAGE: &str = "SELECT \
     id, project_id, service_name, deployment_environment, trace_id, span_id, \
     parent_span_id, name, kind, start_time, duration_ms, status_code, \
     attributes::text AS attributes \
     FROM otel_spans \
     WHERE project_id = $1 AND start_time >= $2 AND start_time <= $3 \
       AND (start_time, id) > ($4, $5) \
     ORDER BY start_time ASC, id ASC \
     LIMIT $6";

async fn fetch_timescale_batch(
    db: &DatabaseConnection,
    project_id: i32,
    from: DBDateTime,
    to: DBDateTime,
    cursor: &CloudBackfillCursor,
    batch_size: u64,
) -> Result<Vec<SourcedSpan>, CloudBackfillError> {
    // `(start_time, id) > (…)` with a sentinel one millisecond before the
    // window start is how a fresh run expresses "no cursor yet" without a
    // second query shape.
    let cursor_time = cursor
        .last_start_time
        .unwrap_or_else(|| from - chrono::Duration::milliseconds(1));
    let cursor_id = cursor.last_row_id.unwrap_or(i64::MIN);

    let rows = db
        .query_all(Statement::from_sql_and_values(
            DatabaseBackend::Postgres,
            TIMESCALE_PAGE,
            vec![
                project_id.into(),
                from.into(),
                to.into(),
                cursor_time.into(),
                cursor_id.into(),
                Value::BigInt(Some(batch_size as i64)),
            ],
        ))
        .await
        .map_err(|source| CloudBackfillError::Database {
            project_id,
            from: from.to_rfc3339(),
            to: to.to_rfc3339(),
            source,
        })?;

    let mut spans = Vec::with_capacity(rows.len());
    for row in &rows {
        // Selected as `attributes::text`, matching the existing
        // `otel_spans` → ClickHouse backfill, so the JSONB column arrives as a
        // string rather than needing a JSONB codec on this path.
        let attributes = row
            .try_get::<String>("", "attributes")
            .ok()
            .and_then(|raw| serde_json::from_str(&raw).ok())
            .unwrap_or_default();

        let start_time: DBDateTime = row.try_get("", "start_time").unwrap_or_default();
        spans.push(SourcedSpan {
            row_id: row.try_get::<i64>("", "id").ok(),
            span: SpanRecord {
                project_id: row.try_get("", "project_id").unwrap_or(project_id),
                deployment_id: None,
                resource: ResourceInfo {
                    service_name: row.try_get("", "service_name").unwrap_or_default(),
                    service_version: None,
                    deployment_environment: row.try_get("", "deployment_environment").ok(),
                    attributes: Default::default(),
                },
                trace_id: row.try_get("", "trace_id").unwrap_or_default(),
                span_id: row.try_get("", "span_id").unwrap_or_default(),
                parent_span_id: row.try_get("", "parent_span_id").ok(),
                name: row.try_get("", "name").unwrap_or_default(),
                kind: parse_kind(&row.try_get::<String>("", "kind").unwrap_or_default()),
                start_time,
                // Only the fields `cloud_span` reads are selected; `end_time`
                // is never mirrored, so re-using `start_time` here keeps the
                // page narrow rather than pulling a column for nothing.
                end_time: start_time,
                duration_ms: row.try_get("", "duration_ms").unwrap_or(0.0),
                status_code: parse_status(
                    &row.try_get::<String>("", "status_code").unwrap_or_default(),
                ),
                status_message: String::new(),
                attributes,
                events: Vec::new(),
            },
        });
    }

    Ok(spans)
}

/// ClickHouse has no row id, so `(start_time, span_id)` is the keyset. Both
/// are part of the table's sort key, so the page is a range scan.
const CLICKHOUSE_PAGE: &str = "SELECT \
     project_id, service_name, deployment_environment, trace_id, span_id, \
     parent_span_id, name, kind, toUnixTimestamp64Milli(start_time) AS start_ms, \
     duration_ms, status_code, attributes \
     FROM spans \
     WHERE project_id = ? \
       AND start_time >= fromUnixTimestamp64Milli(?) \
       AND start_time <= fromUnixTimestamp64Milli(?) \
       AND (toUnixTimestamp64Milli(start_time), span_id) > (?, ?) \
     ORDER BY start_time ASC, span_id ASC \
     LIMIT ?";

#[derive(Debug, ::clickhouse::Row, serde::Deserialize)]
struct ChBackfillRow {
    project_id: i32,
    service_name: String,
    deployment_environment: String,
    trace_id: String,
    span_id: String,
    parent_span_id: String,
    name: String,
    kind: String,
    start_ms: i64,
    duration_ms: f64,
    status_code: String,
    attributes: String,
}

async fn fetch_clickhouse_batch(
    ch: &::clickhouse::Client,
    project_id: i32,
    from: DBDateTime,
    to: DBDateTime,
    cursor: &CloudBackfillCursor,
    batch_size: u64,
) -> Result<Vec<SourcedSpan>, CloudBackfillError> {
    let cursor_ms = cursor
        .last_start_time
        .map(|ts| ts.timestamp_millis())
        .unwrap_or_else(|| from.timestamp_millis() - 1);
    let cursor_span_id = cursor.last_span_id.clone().unwrap_or_default();

    let rows = ch
        .query(CLICKHOUSE_PAGE)
        .bind(project_id)
        .bind(from.timestamp_millis())
        .bind(to.timestamp_millis())
        .bind(cursor_ms)
        .bind(cursor_span_id)
        .bind(batch_size)
        .fetch_all::<ChBackfillRow>()
        .await
        .map_err(|error| CloudBackfillError::ClickHouse {
            project_id,
            from: from.to_rfc3339(),
            to: to.to_rfc3339(),
            reason: error.to_string(),
        })?;

    Ok(rows.into_iter().map(chrow_to_span).collect())
}

fn chrow_to_span(row: ChBackfillRow) -> SourcedSpan {
    let start_time = chrono::DateTime::from_timestamp_millis(row.start_ms).unwrap_or_default();
    SourcedSpan {
        // ClickHouse has no row id; `span_id` is the keyset tiebreaker.
        row_id: None,
        span: SpanRecord {
            project_id: row.project_id,
            deployment_id: None,
            resource: ResourceInfo {
                service_name: row.service_name,
                service_version: None,
                deployment_environment: empty_to_none(row.deployment_environment),
                attributes: Default::default(),
            },
            trace_id: row.trace_id,
            span_id: row.span_id,
            parent_span_id: empty_to_none(row.parent_span_id),
            name: row.name,
            kind: parse_kind(&row.kind),
            start_time,
            end_time: start_time,
            duration_ms: row.duration_ms,
            status_code: parse_status(&row.status_code),
            status_message: String::new(),
            attributes: serde_json::from_str(&row.attributes).unwrap_or_default(),
            events: Vec::new(),
        },
    }
}

/// ClickHouse stores "absent" as `''` for these `LowCardinality(String)`
/// columns, and an empty environment is not the same as an environment named
/// "".
fn empty_to_none(value: String) -> Option<String> {
    if value.is_empty() {
        None
    } else {
        Some(value)
    }
}

fn parse_kind(kind: &str) -> SpanKind {
    match kind {
        "INTERNAL" => SpanKind::Internal,
        "SERVER" => SpanKind::Server,
        "CLIENT" => SpanKind::Client,
        "PRODUCER" => SpanKind::Producer,
        "CONSUMER" => SpanKind::Consumer,
        _ => SpanKind::Unspecified,
    }
}

fn parse_status(status: &str) -> SpanStatusCode {
    match status {
        "OK" => SpanStatusCode::Ok,
        "ERROR" => SpanStatusCode::Error,
        _ => SpanStatusCode::Unset,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A link with a persisted, enrolled credential and telemetry export on —
    /// the only state in which a backfill is meaningful.
    fn linked_cloud() -> (tempfile::TempDir, Arc<CloudLink>) {
        let directory = tempfile::tempdir().expect("temp dir");
        let state_path = directory.path().join("cloud-link/state.json");
        let mut state = temps_cloud_client::EnrollmentState::new("https://cloud.test/");
        state.token = Some("instance-token".to_string());
        state.tenant_id = Some(uuid::Uuid::new_v4());
        state.account_email = Some("owner@example.com".to_string());
        state.save(&state_path).expect("persist enrollment state");
        let link = Arc::new(CloudLink::load(directory.path().to_path_buf(), "test"));
        link.set_feature_switches(temps_cloud_client::CloudFeatureSwitches {
            telemetry: true,
            backups: false,
            notifications: false,
        })
        .expect("enable telemetry export");
        (directory, link)
    }

    fn unlinked_cloud() -> (tempfile::TempDir, Arc<CloudLink>) {
        let directory = tempfile::tempdir().expect("temp dir");
        let link = Arc::new(CloudLink::load(directory.path().to_path_buf(), "test"));
        (directory, link)
    }

    fn sourced(span_id: &str, row_id: Option<i64>, ts_millis: i64) -> SourcedSpan {
        SourcedSpan {
            row_id,
            span: SpanRecord {
                project_id: 7,
                deployment_id: None,
                resource: ResourceInfo::default(),
                trace_id: "4bf92f3577b34da6a3ce929d0e0e4736".into(),
                span_id: span_id.into(),
                parent_span_id: None,
                name: "GET /orders".into(),
                kind: SpanKind::Server,
                start_time: chrono::DateTime::from_timestamp_millis(ts_millis)
                    .expect("valid timestamp"),
                end_time: chrono::DateTime::from_timestamp_millis(ts_millis)
                    .expect("valid timestamp"),
                duration_ms: 1.0,
                status_code: SpanStatusCode::Ok,
                status_message: String::new(),
                attributes: Default::default(),
                events: Vec::new(),
            },
        }
    }

    #[test]
    fn a_metered_project_is_refused_with_the_setting_to_change() {
        // Backfilling at `metered` would bill the operator for rows that can
        // never be read back — the exact outcome `--dry-run` exists to prevent.
        let (_directory, link) = linked_cloud();

        let error = guard_ready(&link, &CloudTelemetryPolicy::metered(), 7)
            .expect_err("a metered project must be refused");

        assert!(matches!(
            error,
            CloudBackfillError::FidelityNotQueryable {
                project_id: 7,
                fidelity: CloudTelemetryFidelity::Metered
            }
        ));
        // The message must name the knob, not just the failure — a self-hosted
        // operator has nobody to ask what "fidelity" means here.
        let message = error.to_string();
        assert!(message.contains("cloud_telemetry_fidelity"), "{message}");
        assert!(message.contains("queryable"), "{message}");
    }

    #[test]
    fn an_unlinked_instance_is_refused_distinctly_from_a_metered_project() {
        // "Not set up yet" and "set up but not opted in" need different fixes,
        // so they must not collapse into one error.
        let (_directory, link) = unlinked_cloud();

        let error = guard_ready(
            &link,
            &CloudTelemetryPolicy::queryable(std::iter::empty()),
            7,
        )
        .expect_err("an unlinked instance must be refused");

        assert!(matches!(
            error,
            CloudBackfillError::NotLinked { project_id: 7 }
        ));
    }

    #[test]
    fn telemetry_export_being_off_is_its_own_error() {
        let (_directory, link) = linked_cloud();
        link.set_feature_switches(temps_cloud_client::CloudFeatureSwitches::default())
            .expect("disable telemetry export");

        let error = guard_ready(
            &link,
            &CloudTelemetryPolicy::queryable(std::iter::empty()),
            7,
        )
        .expect_err("telemetry export off must be refused");

        assert!(matches!(
            error,
            CloudBackfillError::TelemetryExportDisabled { project_id: 7 }
        ));
    }

    #[test]
    fn a_queryable_project_on_a_linked_instance_passes_the_guard() {
        let (_directory, link) = linked_cloud();
        assert!(guard_ready(
            &link,
            &CloudTelemetryPolicy::queryable(["http.route".to_string()]),
            7
        )
        .is_ok());
    }

    #[test]
    fn the_postgres_cursor_advances_on_the_row_id_and_the_clickhouse_one_on_the_span_id() {
        let pg = cursor_after(&[sourced("a", Some(1), 1_000), sourced("b", Some(2), 2_000)]);
        assert_eq!(pg.last_row_id, Some(2));
        assert_eq!(pg.last_span_id.as_deref(), Some("b"));
        assert_eq!(
            pg.last_start_time.map(|t| t.timestamp_millis()),
            Some(2_000)
        );

        let ch = cursor_after(&[sourced("a", None, 1_000), sourced("b", None, 2_000)]);
        assert_eq!(ch.last_row_id, None);
        assert_eq!(
            ch.last_span_id.as_deref(),
            Some("b"),
            "ClickHouse has no row id, so span_id is the tiebreaker"
        );
    }

    #[test]
    fn an_empty_page_leaves_the_cursor_at_its_default() {
        assert_eq!(cursor_after(&[]), CloudBackfillCursor::default());
    }

    #[test]
    fn record_bytes_measures_the_serialized_record() {
        let record = temps_cloud_protocol::SpanRecord {
            trace_id: "t".into(),
            span_id: "s".into(),
            name: "span".into(),
            ts_millis: 1,
            duration_ms: 1.0,
            ..Default::default()
        };
        let expected = serde_json::to_vec(&record)
            .expect("record must serialize")
            .len() as u64;

        assert_eq!(record_bytes(&record), expected);
        assert!(expected > 0);
    }

    #[test]
    fn a_queryable_record_is_measured_as_larger_than_the_metered_one() {
        // The whole point of `--dry-run`: opting in costs more, and the
        // operator sees how much more before sending.
        let (_directory, link) = linked_cloud();
        let span = sourced("00f067aa0ba902b7", Some(1), 1_700_000_000_000).span;

        let metered = cloud_span(&link, &span, &CloudTelemetryPolicy::metered())
            .expect("metered projection must build");
        let queryable = cloud_span(
            &link,
            &span,
            &CloudTelemetryPolicy::queryable(std::iter::empty()),
        )
        .expect("queryable projection must build");

        assert!(
            record_bytes(&queryable) > record_bytes(&metered),
            "queryable {} should exceed metered {}",
            record_bytes(&queryable),
            record_bytes(&metered)
        );
    }

    #[test]
    fn clickhouse_empty_strings_become_absent_rather_than_an_empty_environment() {
        assert_eq!(empty_to_none(String::new()), None);
        assert_eq!(
            empty_to_none("production".to_string()),
            Some("production".to_string())
        );
    }

    #[test]
    fn kind_and_status_parse_back_to_the_values_display_emits() {
        // These strings are what the storage layer wrote via `Display`; a
        // mismatch would silently downgrade every span to UNSPECIFIED/UNSET.
        for kind in [
            SpanKind::Unspecified,
            SpanKind::Internal,
            SpanKind::Server,
            SpanKind::Client,
            SpanKind::Producer,
            SpanKind::Consumer,
        ] {
            assert_eq!(parse_kind(&kind.to_string()), kind);
        }
        for status in [
            SpanStatusCode::Unset,
            SpanStatusCode::Ok,
            SpanStatusCode::Error,
        ] {
            assert_eq!(parse_status(&status.to_string()), status);
        }
        assert_eq!(parse_kind("SOMETHING_NEWER"), SpanKind::Unspecified);
        assert_eq!(parse_status("SOMETHING_NEWER"), SpanStatusCode::Unset);
    }

    #[test]
    fn the_source_names_the_table_it_reads_so_a_zero_row_run_is_explainable() {
        let db = Arc::new(sea_orm::DatabaseConnection::Disconnected);
        assert_eq!(
            CloudBackfillSource::Timescale(db).describe(),
            "PostgreSQL `otel_spans`"
        );
        assert_eq!(
            CloudBackfillSource::ClickHouse(Arc::new(::clickhouse::Client::default())).describe(),
            "ClickHouse `spans`"
        );
    }

    #[tokio::test]
    async fn a_dry_run_estimate_on_a_metered_project_refuses_before_reading_anything() {
        // `--dry-run` must fail the same way a real run would, so an operator
        // is never told "0 bytes" when the truth is "this would be refused".
        let (_directory, link) = linked_cloud();
        let source =
            CloudBackfillSource::Timescale(Arc::new(sea_orm::DatabaseConnection::Disconnected));
        let now = chrono::Utc::now();

        let error = estimate_backfill(
            &source,
            &link,
            &CloudTelemetryPolicy::metered(),
            7,
            now - chrono::Duration::days(1),
            now,
        )
        .await
        .expect_err("a metered project must be refused before any query runs");

        assert!(matches!(
            error,
            CloudBackfillError::FidelityNotQueryable { project_id: 7, .. }
        ));
    }

    #[tokio::test]
    async fn a_backfill_run_on_a_metered_project_refuses_before_sending_anything() {
        let (_directory, link) = linked_cloud();
        let source =
            CloudBackfillSource::Timescale(Arc::new(sea_orm::DatabaseConnection::Disconnected));
        let now = chrono::Utc::now();

        let error = backfill_cloud_telemetry_window(
            &source,
            &link,
            &CloudTelemetryPolicy::metered(),
            7,
            now - chrono::Duration::days(1),
            now,
            DEFAULT_BATCH_SIZE,
            CloudBackfillCursor::default(),
            None,
            |_| {},
        )
        .await
        .expect_err("a metered project must be refused");

        assert!(matches!(
            error,
            CloudBackfillError::FidelityNotQueryable { project_id: 7, .. }
        ));
        assert_eq!(
            link.spooled(),
            0,
            "a refused backfill must not have queued a single span"
        );
    }

    // -----------------------------------------------------------------------
    // ADR-042 P0: shipping while the live mirror is recording
    // -----------------------------------------------------------------------

    /// Accepts telemetry and counts spans per producer, so a test can assert
    /// whose spans Cloud actually received.
    #[derive(Clone, Default)]
    struct TelemetryStub {
        mirror_spans: Arc<std::sync::atomic::AtomicUsize>,
        backfill_spans: Arc<std::sync::atomic::AtomicUsize>,
    }

    async fn serve_stub(stub: TelemetryStub) -> Option<String> {
        use axum::{extract::State, routing::post, Json, Router};
        use std::sync::atomic::Ordering;

        let app = Router::new()
            .route(
                "/v1/enroll",
                post(|| async {
                    Json(serde_json::json!({
                        "tenant_id": uuid::Uuid::new_v4(),
                        "instance_token": "inst_backfill_scope_test"
                    }))
                }),
            )
            .route(
                "/v1/telemetry",
                post(
                    |State(stub): State<TelemetryStub>,
                     Json(batch): Json<temps_cloud_protocol::TelemetryBatch>| async move {
                        let spans = batch.spans.len();
                        for span in &batch.spans {
                            if span.name.starts_with("mirror") {
                                stub.mirror_spans.fetch_add(1, Ordering::SeqCst);
                            } else {
                                stub.backfill_spans.fetch_add(1, Ordering::SeqCst);
                            }
                        }
                        Json(serde_json::json!({
                            "submission_id": batch.submission_id,
                            "processed_spans": spans,
                            "stored_spans": spans,
                            "metered_bytes": 1
                        }))
                    },
                ),
            )
            .with_state(stub);

        let listener = match tokio::net::TcpListener::bind::<std::net::SocketAddr>(
            "127.0.0.1:0".parse().expect("loopback address must parse"),
        )
        .await
        {
            Ok(listener) => listener,
            Err(error) if error.kind() == std::io::ErrorKind::PermissionDenied => {
                eprintln!("skipping cloud backfill network test: sandbox denied TCP bind");
                return None;
            }
            Err(error) => panic!("test server must bind: {error}"),
        };
        let address = listener.local_addr().expect("test server has an address");
        tokio::spawn(async move {
            let _ = axum::serve(listener, app).await;
        });
        Some(format!("http://{address}"))
    }

    /// A link enrolled against the stub, with telemetry export on.
    async fn linked_to_stub(stub: TelemetryStub) -> Option<(tempfile::TempDir, Arc<CloudLink>)> {
        let directory = tempfile::tempdir().expect("temp dir");
        let backend = serve_stub(stub).await?;
        let link = Arc::new(CloudLink::load_for_loopback_development(
            directory.path().to_path_buf(),
            "backfill-scope-test",
        ));
        link.configure(
            temps_cloud_client::BackendUrl::loopback_development(&backend)
                .expect("stub backend URL must be accepted"),
        )
        .expect("test link must be configured");
        link.enroll("backfill-code")
            .await
            .expect("test link must enroll");
        link.set_feature_switches(temps_cloud_client::CloudFeatureSwitches {
            telemetry: true,
            backups: false,
            notifications: false,
        })
        .expect("enable telemetry export");
        Some((directory, link))
    }

    fn wire_spans(prefix: &str, count: usize) -> Vec<temps_cloud_protocol::SpanRecord> {
        (0..count)
            .map(|i| temps_cloud_protocol::SpanRecord {
                trace_id: "4bf92f3577b34da6a3ce929d0e0e4736".into(),
                span_id: format!("{prefix}-{i}"),
                name: format!("{prefix}-{i}"),
                ts_millis: 1_700_000_000_000 + i as i64,
                duration_ms: 1.0,
                ..Default::default()
            })
            .collect()
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn a_batch_ships_completely_while_the_mirror_flusher_is_recording() {
        // ADR-042 P0's acceptance criterion. Before the submission scope, this
        // batch's own drain-complete test read the shared spool, so live mirror
        // traffic recorded a millisecond earlier both inflated `shipped` and
        // could keep `spooled()` above zero for all eight attempts — a spurious
        // `ShipmentRefused` on a busy instance.
        use std::sync::atomic::{AtomicBool, Ordering};

        let stub = TelemetryStub::default();
        let Some((_directory, link)) = linked_to_stub(stub.clone()).await else {
            return;
        };

        // The live mirror: recording and flushing continuously, exactly as it
        // does inside `temps serve`.
        let mirror_running = Arc::new(AtomicBool::new(true));
        let mirror = tokio::spawn({
            let link = Arc::clone(&link);
            let mirror_running = Arc::clone(&mirror_running);
            async move {
                while mirror_running.load(Ordering::SeqCst) {
                    link.record(wire_spans("mirror", 5));
                    let _ = link.flush().await;
                    tokio::task::yield_now().await;
                }
            }
        });

        // Do not start until the mirror is demonstrably shipping, or a green
        // test would prove nothing about concurrency.
        let mut mirrored = 0;
        for _ in 0..2_000 {
            mirrored = stub.mirror_spans.load(Ordering::SeqCst);
            if mirrored > 0 {
                break;
            }
            tokio::time::sleep(Duration::from_millis(1)).await;
        }
        assert!(
            mirrored > 0,
            "the mirror must be shipping before the backfill batch starts"
        );

        let scope = link
            .submission_scope()
            .expect("no other submission scope is open");
        // Larger than one submission, so the drain loop runs more than once and
        // the mirror interleaves between iterations.
        let offered = 637;
        let cursor = CloudBackfillCursor {
            last_start_time: chrono::DateTime::from_timestamp_millis(1_700_000_000_000),
            last_row_id: Some(42),
            last_span_id: Some("backfill-0".into()),
        };

        let shipped = ship_batch(&scope, 7, wire_spans("backfill", offered), &cursor)
            .await
            .expect("the batch must drain despite concurrent mirror traffic");

        assert_eq!(
            shipped, offered as u64,
            "the batch must report exactly its own spans"
        );
        assert_eq!(
            scope.spooled(),
            0,
            "the batch's own queue must be empty when it reports success"
        );
        assert_eq!(
            stub.backfill_spans.load(Ordering::SeqCst),
            offered,
            "Cloud must have received exactly the backfilled spans"
        );
        assert!(
            stub.mirror_spans.load(Ordering::SeqCst) > mirrored,
            "the mirror must have kept shipping during the backfill batch"
        );

        mirror_running.store(false, Ordering::SeqCst);
        let _ = tokio::time::timeout(Duration::from_secs(5), mirror).await;
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_batch_that_ships_nothing_reports_the_link_rather_than_success() {
        // The failure direction of the same isolation: with export switched off
        // mid-run the scope is emptied, and the caller must get a refusal naming
        // the link — not a silent "0 spans, all done" that advances the cursor
        // over history nobody sent.
        let stub = TelemetryStub::default();
        let Some((_directory, link)) = linked_to_stub(stub.clone()).await else {
            return;
        };
        let scope = link
            .submission_scope()
            .expect("no other submission scope is open");
        link.set_feature_switches(temps_cloud_client::CloudFeatureSwitches::default())
            .expect("disable telemetry export");

        let error = ship_batch(
            &scope,
            7,
            wire_spans("backfill", 10),
            &CloudBackfillCursor::default(),
        )
        .await
        .expect_err("shipping with export switched off must fail");

        assert!(matches!(
            error,
            CloudBackfillError::ShipmentRefused { project_id: 7, .. }
        ));
        assert_eq!(
            stub.backfill_spans
                .load(std::sync::atomic::Ordering::SeqCst),
            0
        );
    }

    #[tokio::test]
    async fn a_second_backfill_is_refused_while_one_holds_the_submission_scope() {
        // Submission concurrency is 1 globally (ADR-041 §3b), so the second
        // caller is told what is in the way rather than silently sharing a
        // queue and mis-reporting both runs' progress.
        let (_directory, link) = linked_cloud();
        let held = link.submission_scope().expect("the first scope must open");

        let source =
            CloudBackfillSource::Timescale(Arc::new(sea_orm::DatabaseConnection::Disconnected));
        let now = chrono::Utc::now();
        let error = backfill_cloud_telemetry_window(
            &source,
            &link,
            &CloudTelemetryPolicy::queryable(std::iter::empty()),
            7,
            now - chrono::Duration::days(1),
            now,
            DEFAULT_BATCH_SIZE,
            CloudBackfillCursor::default(),
            None,
            |_| {},
        )
        .await
        .expect_err("a second concurrent backfill must be refused");

        assert!(matches!(
            error,
            CloudBackfillError::SubmissionScopeBusy { project_id: 7, .. }
        ));
        drop(held);
    }
}
