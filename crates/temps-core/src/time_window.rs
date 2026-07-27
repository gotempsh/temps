//! Bounded time windows for high-volume time-series read endpoints.
//!
//! Proxy logs, OTel spans and the unified Observe feed are all append-only
//! tables holding one row per HTTP request / per span. On a busy deployment
//! that is 100M+ rows inside the retention window, and the cost of answering
//! "the most recent N rows" is **linear in how much time the window spans** —
//! the sort keys are project-scoped (`(project_id, timestamp, …)`), so an
//! unscoped `ORDER BY timestamp DESC` cannot be answered from the index and the
//! engine reads every granule in the range before sorting.
//!
//! Measured on 150M proxy-log rows (ClickHouse 24.8), whole endpoint
//! (count + page), no project filter:
//!
//! | window | latency | rows read |
//! |--------|---------|-----------|
//! | 1 hour |  7.7 ms |      1.6M |
//! | 6 hours|   27 ms |      5.3M |
//! | 24 hours|  123 ms |     23.8M |
//! | 7 days | 1336 ms |     151.5M |
//!
//! Two rules follow, and this module is the single place both are defined so
//! every endpoint enforces the same contract:
//!
//! 1. **Never unbounded.** A caller that supplies no lower bound gets one.
//!    Omitting a date must not mean "scan the whole retention window".
//! 2. **Never unboundedly wide.** A window wider than [`MAX_WINDOW_DAYS`] is
//!    rejected with an actionable error rather than served slowly, because at
//!    these volumes "slow" means tens of seconds and a request that ties up a
//!    connection that long is a availability problem, not just a UX one.
//!
//! Rule 2 caps the window's WIDTH, not its AGE: retention is 30 days and any
//! point in it stays reachable — a caller just moves a 7-day window back rather
//! than asking for all 30 days at once.

use chrono::{DateTime, Duration, Utc};
use thiserror::Error;

/// Default lower bound when a caller supplies no start.
///
/// One hour keeps the default page load an order of magnitude inside a 100ms
/// budget (7.7ms above) while still covering "what is happening right now",
/// which is what these pages are opened for. Wider ranges are a deliberate,
/// visible choice in the UI's range picker.
pub const DEFAULT_LOOKBACK_HOURS: i64 = 1;

/// Widest window any of these endpoints will serve.
///
/// Seven days is the widest preset the UI offers and measures ~1.3s on the
/// 150M-row reference set — slow, but acceptable for an explicit action. Beyond
/// that the query grows into the tens of seconds.
pub const MAX_WINDOW_DAYS: i64 = 7;

#[derive(Debug, Error, PartialEq, Eq)]
pub enum TimeWindowError {
    /// The requested span exceeds [`MAX_WINDOW_DAYS`].
    ///
    /// The message names the cap AND how to work around it, because the caller
    /// has no other way to discover either.
    #[error(
        "Requested time range spans {requested_days} days, which exceeds the \
         {max_days}-day maximum for this endpoint. Older data is still \
         available — request it {max_days} days at a time by moving \
         start_date/end_date back, or narrow the range with filters."
    )]
    TooWide { requested_days: i64, max_days: i64 },

    /// `start` is after `end`.
    #[error("start ({start}) is after end ({end})")]
    Inverted { start: String, end: String },
}

/// A resolved, bounded window. `end` is `None` when the caller did not pin an
/// upper bound — callers that need a concrete instant should treat that as
/// "now", but leaving it open lets the storage layer omit the predicate
/// entirely, which is one less comparison per row.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TimeWindow {
    pub start: DateTime<Utc>,
    pub end: Option<DateTime<Utc>>,
}

/// Resolve a caller-supplied range into a bounded window.
///
/// * absent `start` → `end` (or now) minus `lookback`
/// * `start` after `end` → [`TimeWindowError::Inverted`]
/// * span wider than [`MAX_WINDOW_DAYS`] → [`TimeWindowError::TooWide`]
///
/// An absent `end` is measured against *now* for the width check, so
/// `start_date=<60 days ago>` with no end is rejected rather than silently
/// scanning the whole retention window.
pub fn resolve(
    start: Option<DateTime<Utc>>,
    end: Option<DateTime<Utc>>,
    lookback: Duration,
) -> Result<TimeWindow, TimeWindowError> {
    resolve_with_max(start, end, lookback, Duration::days(MAX_WINDOW_DAYS))
}

/// [`resolve`] with an explicit cap, for endpoints whose aggregation is cheap
/// enough to justify a wider ceiling.
pub fn resolve_with_max(
    start: Option<DateTime<Utc>>,
    end: Option<DateTime<Utc>>,
    lookback: Duration,
    max_span: Duration,
) -> Result<TimeWindow, TimeWindowError> {
    let start = match start {
        Some(s) => s,
        None => {
            let anchor = end.unwrap_or_else(Utc::now);
            // Checked: `end` arrives straight from a query parameter and a bare
            // subtraction panics at the representable boundary.
            anchor
                .checked_sub_signed(lookback)
                .unwrap_or(DateTime::<Utc>::MIN_UTC)
        }
    };

    // Width is measured against a concrete upper bound even when the caller
    // left `end` open, otherwise "start=90 days ago" would slip through.
    let effective_end = end.unwrap_or_else(Utc::now);

    if start > effective_end {
        return Err(TimeWindowError::Inverted {
            start: start.to_rfc3339(),
            end: effective_end.to_rfc3339(),
        });
    }

    let span = effective_end.signed_duration_since(start);
    if span > max_span {
        return Err(TimeWindowError::TooWide {
            // Round up: a 7-day-and-one-second request should not report "7".
            requested_days: (span.num_seconds() + 86_399) / 86_400,
            max_days: max_span.num_days(),
        });
    }

    Ok(TimeWindow { start, end })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(s: &str) -> DateTime<Utc> {
        DateTime::parse_from_rfc3339(s)
            .expect("valid fixture timestamp")
            .with_timezone(&Utc)
    }

    #[test]
    fn absent_start_defaults_to_lookback_before_now() {
        let before = Utc::now();
        let w = resolve(None, None, Duration::hours(1)).expect("bounded");
        let after = Utc::now();

        assert!(w.start >= before - Duration::hours(1));
        assert!(w.start <= after - Duration::hours(1));
        assert_eq!(w.end, None, "an open upper bound stays open");
    }

    #[test]
    fn absent_start_anchors_to_end_when_end_given() {
        let end = at("2026-07-20T12:00:00Z");
        let w = resolve(None, Some(end), Duration::hours(1)).expect("bounded");
        // The window ends where the caller asked, not at "now".
        assert_eq!(w.start, end - Duration::hours(1));
        assert_eq!(w.end, Some(end));
    }

    #[test]
    fn explicit_start_is_preserved_when_within_the_cap() {
        let start = at("2026-07-18T00:00:00Z");
        let end = at("2026-07-20T00:00:00Z");
        let w = resolve(Some(start), Some(end), Duration::hours(1)).expect("bounded");
        assert_eq!(w.start, start);
        assert_eq!(w.end, Some(end));
    }

    #[test]
    fn a_span_wider_than_the_cap_is_rejected() {
        let start = at("2026-06-01T00:00:00Z");
        let end = at("2026-07-01T00:00:00Z");
        let err = resolve(Some(start), Some(end), Duration::hours(1))
            .expect_err("30 days must exceed the 7-day cap");
        assert_eq!(
            err,
            TimeWindowError::TooWide {
                requested_days: 30,
                max_days: 7
            }
        );
        // The message has to tell the caller how to still get the data.
        let msg = err.to_string();
        assert!(msg.contains("7-day maximum"), "{msg}");
        assert!(msg.contains("still"), "must point at the workaround: {msg}");
    }

    #[test]
    fn exactly_the_cap_is_allowed() {
        let start = at("2026-07-13T00:00:00Z");
        let end = at("2026-07-20T00:00:00Z");
        assert!(resolve(Some(start), Some(end), Duration::hours(1)).is_ok());
    }

    /// An open `end` must not be an escape hatch around the width cap.
    #[test]
    fn an_ancient_start_with_no_end_is_still_rejected() {
        let start = Utc::now() - Duration::days(30);
        let err = resolve(Some(start), None, Duration::hours(1))
            .expect_err("width is measured against now when end is open");
        assert!(matches!(err, TimeWindowError::TooWide { .. }));
    }

    #[test]
    fn inverted_range_is_rejected_before_the_width_check() {
        let start = at("2026-07-20T00:00:00Z");
        let end = at("2026-07-19T00:00:00Z");
        let err = resolve(Some(start), Some(end), Duration::hours(1)).expect_err("inverted");
        assert!(matches!(err, TimeWindowError::Inverted { .. }));
    }

    /// Rounding up matters: a range one second over the cap must not report the
    /// cap back to the user as if it were within it.
    #[test]
    fn requested_days_rounds_up() {
        let start = at("2026-07-13T00:00:00Z");
        let end = at("2026-07-20T00:00:01Z");
        let err = resolve(Some(start), Some(end), Duration::hours(1)).expect_err("just over");
        assert_eq!(
            err,
            TimeWindowError::TooWide {
                requested_days: 8,
                max_days: 7
            }
        );
    }

    #[test]
    fn underflow_saturates_instead_of_panicking() {
        // `end` reaches this straight from a query parameter.
        let w = resolve(None, Some(DateTime::<Utc>::MIN_UTC), Duration::hours(1))
            .expect("saturating start is still a valid window");
        assert_eq!(w.start, DateTime::<Utc>::MIN_UTC);
    }
}
