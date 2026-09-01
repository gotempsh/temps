// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Shared progress for the out-of-process Cloud telemetry backfill
//! (ADR-040 §1).
//!
//! The CLI already has an `indicatif` bar and a local resume checkpoint. Both
//! are private to the terminal that launched the run. This service is the half
//! everyone else can see: the CLI writes to it once per chunk, and the Console
//! reads it through
//! [`crate::handlers::cloud_backfill_handler::get_cloud_backfill_status`].
//!
//! Two rules shape the design:
//!
//! 1. **Progress writes never fail a backfill.** They are bookkeeping about a
//!    data transfer, not part of it. The CLI logs a failed write and keeps
//!    going; losing a progress row costs visibility, and aborting a paid,
//!    half-finished egress to preserve a status field would be strictly worse.
//! 2. **No row means "never started".** The read path materialises that rather
//!    than 404-ing, because "never run" and "broken" need different UI and a
//!    404 cannot tell them apart.
//! 3. **The stored failure reason is bounded.** It originates as raw
//!    `sea_orm::DbErr`/ClickHouse server text and is then served to every
//!    project member with `OtelRead` — not only the operator who ran the
//!    command. It is kept verbatim (an operator debugging alone needs the real
//!    message) but truncated to [`MAX_LAST_ERROR_CHARS`], so an unbounded
//!    internal string cannot be republished in full through a read endpoint.

use std::sync::Arc;

use sea_orm::{
    ActiveModelTrait, ColumnTrait, DatabaseConnection, EntityTrait, QueryFilter, Set, Unchanged,
};
use temps_core::DBDateTime;
use temps_entities::cloud_telemetry_backfills::{
    self, CloudTelemetryBackfillStatus, Model as BackfillProgress,
};

/// Hard ceiling on the stored failure reason, in characters.
///
/// Every `CloudBackfillError` message this crate produces — the longest being
/// `ShipmentRefused`, which carries a project id, a span count, a resume
/// timestamp and the transport's own reason — fits well inside this, so a
/// truncation in practice means a database or ClickHouse driver appended a
/// dump: exactly the case where the tail is internal detail rather than
/// operator-actionable information.
pub const MAX_LAST_ERROR_CHARS: usize = 300;

/// Marker appended when [`truncate_failure_reason`] cut something off, so a
/// reader never mistakes a truncated message for the whole story.
const TRUNCATION_MARKER: &str = "… (truncated)";

/// Bound a failure reason to [`MAX_LAST_ERROR_CHARS`] characters.
///
/// Counts characters rather than bytes so a multi-byte message cannot be split
/// mid-codepoint. This is a length cap, not sanitisation: the message stays
/// verbatim up to the cut, because a self-hosted operator debugging alone needs
/// the real text, and no credential is reachable through this path. What it
/// closes is the unbounded republication of driver/server output to every
/// project member with `OtelRead`.
pub fn truncate_failure_reason(reason: &str) -> String {
    match reason.char_indices().nth(MAX_LAST_ERROR_CHARS) {
        Some((cut, _)) => format!("{}{TRUNCATION_MARKER}", &reason[..cut]),
        None => reason.to_string(),
    }
}

#[derive(Debug, thiserror::Error)]
pub enum CloudBackfillProgressError {
    #[error(
        "Failed to read the Cloud telemetry backfill progress for project {project_id}: {source}"
    )]
    Read {
        project_id: i32,
        #[source]
        source: sea_orm::DbErr,
    },

    #[error(
        "Failed to record Cloud telemetry backfill progress for project {project_id} \
         ({spans_processed}/{spans_total} spans, status {status}): {source}"
    )]
    Write {
        project_id: i32,
        spans_processed: i64,
        spans_total: i64,
        status: CloudTelemetryBackfillStatus,
        #[source]
        source: sea_orm::DbErr,
    },
}

/// Reads and writes the one-row-per-project backfill progress record.
pub struct CloudBackfillProgressService {
    db: Arc<DatabaseConnection>,
}

impl CloudBackfillProgressService {
    pub fn new(db: Arc<DatabaseConnection>) -> Self {
        Self { db }
    }

    /// Progress for `project_id`, or `None` when no backfill has ever run.
    pub async fn get(
        &self,
        project_id: i32,
    ) -> Result<Option<BackfillProgress>, CloudBackfillProgressError> {
        cloud_telemetry_backfills::Entity::find()
            .filter(cloud_telemetry_backfills::Column::ProjectId.eq(project_id))
            .one(self.db.as_ref())
            .await
            .map_err(|source| CloudBackfillProgressError::Read { project_id, source })
    }

    /// Mark a run as started, replacing any previous run's progress.
    ///
    /// `spans_total` comes from the same estimate `--dry-run` prints, so the
    /// Console's percentage means the same thing as the CLI's bar.
    pub async fn start(
        &self,
        project_id: i32,
        spans_total: u64,
        window_from: DBDateTime,
        window_to: DBDateTime,
    ) -> Result<BackfillProgress, CloudBackfillProgressError> {
        let now = chrono::Utc::now();
        let spans_total = clamp_to_i64(spans_total);
        self.upsert(
            project_id,
            spans_total,
            CloudTelemetryBackfillStatus::Running,
            0,
            Some(window_from),
            Some(window_to),
            Some(now),
            None,
            None,
        )
        .await
    }

    /// Record how far the run has got. Called once per chunk, alongside the
    /// CLI's own local checkpoint write.
    pub async fn record_progress(
        &self,
        project_id: i32,
        spans_processed: u64,
        spans_total: u64,
    ) -> Result<BackfillProgress, CloudBackfillProgressError> {
        self.upsert(
            project_id,
            clamp_to_i64(spans_total),
            CloudTelemetryBackfillStatus::Running,
            clamp_to_i64(spans_processed),
            None,
            None,
            None,
            None,
            None,
        )
        .await
    }

    /// Mark the run finished.
    pub async fn complete(
        &self,
        project_id: i32,
        spans_processed: u64,
        spans_total: u64,
    ) -> Result<BackfillProgress, CloudBackfillProgressError> {
        self.upsert(
            project_id,
            clamp_to_i64(spans_total),
            CloudTelemetryBackfillStatus::Completed,
            clamp_to_i64(spans_processed),
            None,
            None,
            None,
            Some(chrono::Utc::now()),
            None,
        )
        .await
    }

    /// Mark the run failed, keeping the reason verbatim up to
    /// [`MAX_LAST_ERROR_CHARS`].
    pub async fn fail(
        &self,
        project_id: i32,
        spans_processed: u64,
        spans_total: u64,
        reason: impl AsRef<str>,
    ) -> Result<BackfillProgress, CloudBackfillProgressError> {
        self.upsert(
            project_id,
            clamp_to_i64(spans_total),
            CloudTelemetryBackfillStatus::Failed,
            clamp_to_i64(spans_processed),
            None,
            None,
            None,
            None,
            Some(reason.as_ref().to_string()),
        )
        .await
    }

    /// Insert or update the single row for `project_id`.
    ///
    /// `None` for the optional window/timestamp arguments means "leave whatever
    /// the running row already has", so a per-chunk progress write does not
    /// have to restate the window it is filling.
    #[allow(clippy::too_many_arguments)]
    async fn upsert(
        &self,
        project_id: i32,
        spans_total: i64,
        status: CloudTelemetryBackfillStatus,
        spans_processed: i64,
        window_from: Option<DBDateTime>,
        window_to: Option<DBDateTime>,
        started_at: Option<DBDateTime>,
        completed_at: Option<DBDateTime>,
        last_error: Option<String>,
    ) -> Result<BackfillProgress, CloudBackfillProgressError> {
        let write_error = |source: sea_orm::DbErr| CloudBackfillProgressError::Write {
            project_id,
            spans_processed,
            spans_total,
            status,
            source,
        };

        // Bounded at the single choke point every write goes through, so no
        // present or future caller can route an unbounded driver/server message
        // into a column the read API serves to every project member with
        // `OtelRead`.
        let last_error = last_error.as_deref().map(truncate_failure_reason);

        let existing = self.get(project_id).await?;
        let now = chrono::Utc::now();

        match existing {
            Some(row) => {
                let mut active = cloud_telemetry_backfills::ActiveModel {
                    id: Unchanged(row.id),
                    project_id: Unchanged(row.project_id),
                    status: Set(status),
                    spans_processed: Set(spans_processed),
                    spans_total: Set(spans_total),
                    updated_at: Set(now),
                    // A new failure reason replaces the old one; a successful
                    // write clears it, so a completed run never displays the
                    // error from the attempt before it.
                    last_error: Set(last_error),
                    ..Default::default()
                };
                active.window_from = Set(window_from.or(row.window_from));
                active.window_to = Set(window_to.or(row.window_to));
                active.started_at = Set(started_at.or(row.started_at));
                active.completed_at = Set(completed_at.or(
                    // Reopening a completed row for a new run must clear the
                    // old completion timestamp, or the Console would show a
                    // running backfill that already "finished".
                    if status == CloudTelemetryBackfillStatus::Running {
                        None
                    } else {
                        row.completed_at
                    },
                ));
                active.update(self.db.as_ref()).await.map_err(write_error)
            }
            None => cloud_telemetry_backfills::ActiveModel {
                project_id: Set(project_id),
                status: Set(status),
                spans_processed: Set(spans_processed),
                spans_total: Set(spans_total),
                window_from: Set(window_from),
                window_to: Set(window_to),
                started_at: Set(started_at.or(Some(now))),
                updated_at: Set(now),
                completed_at: Set(completed_at),
                last_error: Set(last_error),
                ..Default::default()
            }
            .insert(self.db.as_ref())
            .await
            .map_err(write_error),
        }
    }
}

/// Span counts are `u64` in the backfill and `BIGINT` in Postgres. Saturating
/// rather than wrapping: a nonsensical count should read as "very large", never
/// as a negative that would render as a negative percentage.
fn clamp_to_i64(value: u64) -> i64 {
    i64::try_from(value).unwrap_or(i64::MAX)
}

/// Completion percentage, or `None` when the total is unknown or zero.
///
/// Free function so both the handler and the CLI derive the number the same
/// way — two independent "processed / total * 100" expressions is how a
/// Console and a terminal end up disagreeing about the same run.
pub fn percent_complete(spans_processed: i64, spans_total: i64) -> Option<f64> {
    if spans_total <= 0 {
        return None;
    }
    let percent = (spans_processed as f64 / spans_total as f64) * 100.0;
    Some(percent.clamp(0.0, 100.0))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn percent_is_none_when_the_total_is_unknown() {
        // An empty window is not "0% done" — it is a run with nothing to do,
        // and a 0% bar would read as stuck.
        assert_eq!(percent_complete(0, 0), None);
        assert_eq!(percent_complete(10, 0), None);
        assert_eq!(percent_complete(10, -1), None);
    }

    #[test]
    fn percent_tracks_progress_and_is_clamped_to_the_readable_range() {
        assert_eq!(percent_complete(0, 200), Some(0.0));
        assert_eq!(percent_complete(50, 200), Some(25.0));
        assert_eq!(percent_complete(200, 200), Some(100.0));
        // A run that ships more than the estimate (spans arrived after the
        // count) must show 100%, never 137%.
        assert_eq!(percent_complete(275, 200), Some(100.0));
    }

    #[test]
    fn span_counts_saturate_rather_than_wrapping_negative() {
        assert_eq!(clamp_to_i64(0), 0);
        assert_eq!(clamp_to_i64(1_000), 1_000);
        assert_eq!(clamp_to_i64(u64::MAX), i64::MAX);
        assert!(clamp_to_i64(u64::MAX) > 0, "must never render as negative");
    }

    #[test]
    fn a_short_failure_reason_is_stored_exactly_as_written() {
        // The common case. An operator debugging alone needs the real message,
        // so the cap must not paraphrase, redact or reformat anything.
        let reason = "Temps Cloud did not accept a batch of 500 span(s) for project 7";
        assert_eq!(truncate_failure_reason(reason), reason);
    }

    #[test]
    fn an_oversized_failure_reason_is_capped_and_says_so() {
        // A `DbErr` or a ClickHouse server response can carry an arbitrarily
        // long dump, and this column is served to every project member with
        // `OtelRead` — not just whoever ran the command.
        let reason = format!("Cloud refused the batch: {}", "x".repeat(10_000));

        let stored = truncate_failure_reason(&reason);

        assert!(stored.starts_with("Cloud refused the batch: xxx"));
        assert!(stored.ends_with(TRUNCATION_MARKER), "{stored}");
        assert_eq!(
            stored.chars().count(),
            MAX_LAST_ERROR_CHARS + TRUNCATION_MARKER.chars().count()
        );
    }

    #[test]
    fn truncation_never_splits_a_multi_byte_character() {
        // Slicing on a byte offset here would panic and take down a run that
        // had already shipped data — the worst possible moment to abort.
        let reason = "é".repeat(MAX_LAST_ERROR_CHARS * 2);

        let stored = truncate_failure_reason(&reason);

        assert!(stored.starts_with('é'));
        assert_eq!(
            stored.chars().filter(|c| *c == 'é').count(),
            MAX_LAST_ERROR_CHARS
        );
    }

    #[test]
    fn a_reason_exactly_at_the_cap_is_not_marked_as_truncated() {
        let reason = "x".repeat(MAX_LAST_ERROR_CHARS);
        assert_eq!(truncate_failure_reason(&reason), reason);
    }

    #[test]
    fn write_errors_name_the_project_and_the_counters_that_failed_to_persist() {
        let error = CloudBackfillProgressError::Write {
            project_id: 7,
            spans_processed: 120,
            spans_total: 500,
            status: CloudTelemetryBackfillStatus::Running,
            source: sea_orm::DbErr::Custom("connection reset".into()),
        };
        let message = error.to_string();

        assert!(message.contains("project 7"), "{message}");
        assert!(message.contains("120/500"), "{message}");
        assert!(message.contains("running"), "{message}");
    }
}
