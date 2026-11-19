// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Log-derived series for alert rules (ADR 108 D1).
//!
//! A log alert is a metric alert rule whose series is the number of log
//! lines matching a saved search per evaluation window. The rule evaluator
//! lives in `temps-otel` and the log store in `temps-log-aggregator`, which
//! do not depend on each other, so the count is reached through this trait:
//! the log aggregator registers an implementation in the plugin service
//! registry and the evaluator looks it up once its own plugin initialises.
//!
//! The evaluator only ever asks for one bounded window per rule per tick.
//! Nothing here runs on the ingest path.

use async_trait::async_trait;
use chrono::{DateTime, Utc};

/// Widest window a log alert may count, in seconds. One hour keeps a text
/// search over the chunk store bounded on a small host; the line index
/// could go further, but one rule for both paths is easier to reason about.
pub const LOG_ALERT_MAX_WINDOW_SECS: i32 = 3600;

/// Upper bound on a counted window. A text search walks lines through the
/// chunk store, so counting stops here and the sample is reported as
/// saturated; a static threshold must therefore stay below this value.
pub const LOG_ALERT_COUNT_CAP: u64 = 1_000;

/// The number of matching lines in one window.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LogSeriesSample {
    /// Matching lines found. When `saturated`, at least this many.
    pub count: u64,
    /// `true` when counting stopped at [`LOG_ALERT_COUNT_CAP`] or the store's
    /// own time budget ran out: the count is a lower bound, never an
    /// overstatement, so a rule never fires on a number that is too high.
    pub saturated: bool,
}

/// Why a log series could not be produced.
#[derive(Debug, thiserror::Error)]
pub enum LogSeriesError {
    #[error("Saved log search {saved_search_id} not found in project {project_id}")]
    SavedSearchNotFound {
        project_id: i32,
        saved_search_id: i32,
    },
    #[error("Log series are unavailable: {reason}")]
    Unavailable { reason: String },
    #[error("Counting log lines for saved search {saved_search_id} in project {project_id} failed: {reason}")]
    Query {
        project_id: i32,
        saved_search_id: i32,
        reason: String,
    },
}

/// Counts log lines matching a saved search over a bounded window.
#[async_trait]
pub trait LogSeriesSource: Send + Sync {
    /// Number of lines matching saved search `saved_search_id` of
    /// `project_id` with timestamps in `[start, end)`. The saved search must
    /// belong to the project; a mismatch is `SavedSearchNotFound`.
    async fn count_matching(
        &self,
        project_id: i32,
        saved_search_id: i32,
        start: DateTime<Utc>,
        end: DateTime<Utc>,
    ) -> Result<LogSeriesSample, LogSeriesError>;
}
