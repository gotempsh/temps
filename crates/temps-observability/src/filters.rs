//! Query filters and merge utilities used by the unified Observe service.
//!
//! Kept in its own module so the merge logic (k-way merge, kind-set
//! parsing) can be unit-tested without spinning up a database.

use chrono::{DateTime, Utc};
use std::collections::HashSet;

use crate::error::ObservabilityError;
use crate::types::{EventKind, ObservabilityEvent};

/// Default page size for the unified events list. Mirrors the project's
/// pagination convention (20 default, 100 max).
pub const DEFAULT_PAGE_SIZE: u64 = 50;
/// Hard cap on page size — enforced server-side regardless of client input.
pub const MAX_PAGE_SIZE: u64 = 200;

#[derive(Clone, Debug)]
pub struct EventFilters {
    pub project_id: i32,
    pub kinds: HashSet<EventKind>,
    pub from: Option<DateTime<Utc>>,
    pub to: Option<DateTime<Utc>>,
    pub deployment_id: Option<i32>,
    pub environment_id: Option<i32>,
    pub search: Option<String>,
    pub limit: u64,
    /// When `Some(true)`, exclude rows flagged as bots (`is_bot = true`).
    /// When `Some(false)`, only include bot rows. `None` includes everything.
    /// Currently only applied to the `Request` kind — other kinds don't
    /// carry a bot flag.
    pub hide_bots: Option<bool>,
}

impl EventFilters {
    pub fn validate(&self) -> Result<(), ObservabilityError> {
        if let (Some(from), Some(to)) = (self.from, self.to) {
            if from > to {
                return Err(ObservabilityError::InvalidTimeRange {
                    from: from.to_rfc3339(),
                    to: to.to_rfc3339(),
                });
            }
        }
        Ok(())
    }
}

/// Parse a comma-separated `kinds` query parameter into a typed set.
/// Empty string or `None` yields the full set so the default-on UX matches
/// the design plan (every kind visible until the user toggles).
pub fn parse_kinds(raw: Option<&str>) -> Result<HashSet<EventKind>, ObservabilityError> {
    let Some(s) = raw else {
        return Ok(EventKind::ALL.iter().copied().collect());
    };
    let s = s.trim();
    if s.is_empty() {
        return Ok(EventKind::ALL.iter().copied().collect());
    }
    let mut out = HashSet::new();
    for tok in s.split(',') {
        let tok = tok.trim();
        if tok.is_empty() {
            continue;
        }
        let kind = EventKind::parse(tok).ok_or_else(|| ObservabilityError::InvalidKindsFilter {
            value: tok.to_string(),
        })?;
        out.insert(kind);
    }
    if out.is_empty() {
        return Ok(EventKind::ALL.iter().copied().collect());
    }
    Ok(out)
}

/// Clamp a client-supplied limit into the [1, MAX_PAGE_SIZE] range.
pub fn clamp_limit(raw: Option<u64>) -> u64 {
    raw.unwrap_or(DEFAULT_PAGE_SIZE).clamp(1, MAX_PAGE_SIZE)
}

/// k-way merge of pre-sorted per-kind streams (each already DESC by ts)
/// into a single combined stream. Stable across kinds — when two rows share
/// a timestamp the order falls back to (kind, id) defined by the cursor
/// scheme.
///
/// Implemented as a heap-free linear scan over the per-kind cursors since
/// `EventKind::ALL.len()` is 5; the constant factor beats the heap setup
/// for any realistic page size.
pub fn merge_desc_by_ts(
    streams: Vec<Vec<ObservabilityEvent>>,
    limit: usize,
) -> Vec<ObservabilityEvent> {
    let mut cursors: Vec<std::vec::IntoIter<ObservabilityEvent>> =
        streams.into_iter().map(|s| s.into_iter()).collect();
    let mut heads: Vec<Option<ObservabilityEvent>> = cursors.iter_mut().map(|c| c.next()).collect();

    let mut out = Vec::with_capacity(limit);
    while out.len() < limit {
        // Find the head with the latest ts (ties: prefer lower kind index
        // so the order is deterministic across runs).
        let mut best: Option<usize> = None;
        for (i, h) in heads.iter().enumerate() {
            let Some(candidate) = h else { continue };
            match best {
                None => best = Some(i),
                Some(j) => {
                    let cur_best = heads[j].as_ref().expect("best is occupied");
                    let candidate_key = (candidate.ts(), candidate.kind() as u8);
                    let best_key = (cur_best.ts(), cur_best.kind() as u8);
                    // We want descending ts; for equal ts prefer lower kind index.
                    if candidate_key.0 > best_key.0
                        || (candidate_key.0 == best_key.0 && candidate_key.1 < best_key.1)
                    {
                        best = Some(i);
                    }
                }
            }
        }
        let Some(idx) = best else { break };
        out.push(heads[idx].take().expect("head present"));
        heads[idx] = cursors[idx].next();
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::{ErrorRow, RequestRow};

    fn ts(s: &str) -> DateTime<Utc> {
        DateTime::parse_from_rfc3339(s).unwrap().with_timezone(&Utc)
    }

    fn err_at(t: &str, id: i64) -> ObservabilityEvent {
        ObservabilityEvent::Error(ErrorRow {
            id,
            ts: ts(t),
            deployment_id: None,
            environment_id: None,
            trace_id: None,
            error_group_id: 1,
            fingerprint: format!("fp-{id}"),
            error_class: "Boom".into(),
            message: None,
            stacktrace_preview: serde_json::json!([]),
            stacktrace_truncated: false,
        })
    }
    fn req_at(t: &str, id: i64) -> ObservabilityEvent {
        ObservabilityEvent::Request(RequestRow {
            id: format!("req-{id}"),
            ts: ts(t),
            deployment_id: None,
            environment_id: None,
            trace_id: None,
            error_group_id: None,
            method: "GET".into(),
            host: "x".into(),
            path: "/".into(),
            query_string: None,
            status: 200,
            latency_ms: None,
            client_ip: None,
            country: None,
            user_agent: None,
            referrer: None,
            request_headers: serde_json::json!({}),
            response_headers: serde_json::json!({}),
            headers_truncated: false,
        })
    }

    // ── parse_kinds ──────────────────────────────────────────────────────

    #[test]
    fn parse_kinds_default_returns_all() {
        let kinds = parse_kinds(None).unwrap();
        assert_eq!(kinds.len(), 4);
    }

    #[test]
    fn parse_kinds_empty_string_returns_all() {
        let kinds = parse_kinds(Some("")).unwrap();
        assert_eq!(kinds.len(), 4);
        let kinds2 = parse_kinds(Some("   ")).unwrap();
        assert_eq!(kinds2.len(), 4);
    }

    #[test]
    fn parse_kinds_parses_csv() {
        let kinds = parse_kinds(Some("request,error")).unwrap();
        assert_eq!(kinds.len(), 2);
        assert!(kinds.contains(&EventKind::Request));
        assert!(kinds.contains(&EventKind::Error));
    }

    #[test]
    fn parse_kinds_trims_whitespace() {
        let kinds = parse_kinds(Some(" request , error ")).unwrap();
        assert_eq!(kinds.len(), 2);
    }

    #[test]
    fn parse_kinds_rejects_unknown_kind() {
        let err = parse_kinds(Some("request,nope")).unwrap_err();
        assert!(matches!(err, ObservabilityError::InvalidKindsFilter { .. }));
    }

    // ── clamp_limit ──────────────────────────────────────────────────────

    #[test]
    fn clamp_limit_uses_default_when_none() {
        assert_eq!(clamp_limit(None), DEFAULT_PAGE_SIZE);
    }

    #[test]
    fn clamp_limit_caps_at_max() {
        assert_eq!(clamp_limit(Some(99_999)), MAX_PAGE_SIZE);
    }

    #[test]
    fn clamp_limit_lower_bound_is_one() {
        assert_eq!(clamp_limit(Some(0)), 1);
    }

    // ── EventFilters::validate ───────────────────────────────────────────

    #[test]
    fn validate_rejects_inverted_time_range() {
        let f = EventFilters {
            project_id: 1,
            kinds: EventKind::ALL.iter().copied().collect(),
            from: Some(ts("2026-05-02T00:00:00Z")),
            to: Some(ts("2026-05-01T00:00:00Z")),
            deployment_id: None,
            environment_id: None,
            search: None,
            limit: 50,
            hide_bots: None,
        };
        assert!(matches!(
            f.validate(),
            Err(ObservabilityError::InvalidTimeRange { .. })
        ));
    }

    #[test]
    fn validate_accepts_equal_endpoints() {
        let f = EventFilters {
            project_id: 1,
            kinds: EventKind::ALL.iter().copied().collect(),
            from: Some(ts("2026-05-01T00:00:00Z")),
            to: Some(ts("2026-05-01T00:00:00Z")),
            deployment_id: None,
            environment_id: None,
            search: None,
            limit: 50,
            hide_bots: None,
        };
        assert!(f.validate().is_ok());
    }

    // ── merge_desc_by_ts ─────────────────────────────────────────────────

    #[test]
    fn merge_interleaves_by_ts_desc() {
        // errors:  [2026-05-01T12:03Z, 2026-05-01T12:01Z]
        // requests:[2026-05-01T12:02Z, 2026-05-01T12:00Z]
        let errs = vec![
            err_at("2026-05-01T12:03:00Z", 3),
            err_at("2026-05-01T12:01:00Z", 1),
        ];
        let reqs = vec![
            req_at("2026-05-01T12:02:00Z", 2),
            req_at("2026-05-01T12:00:00Z", 0),
        ];
        let merged = merge_desc_by_ts(vec![errs, reqs], 10);
        let kinds_in_order: Vec<_> = merged.iter().map(|e| e.kind()).collect();
        assert_eq!(
            kinds_in_order,
            vec![
                EventKind::Error,
                EventKind::Request,
                EventKind::Error,
                EventKind::Request,
            ]
        );
    }

    #[test]
    fn merge_respects_limit() {
        let stream = vec![
            err_at("2026-05-01T12:03:00Z", 1),
            err_at("2026-05-01T12:02:00Z", 2),
            err_at("2026-05-01T12:01:00Z", 3),
        ];
        let merged = merge_desc_by_ts(vec![stream], 2);
        assert_eq!(merged.len(), 2);
    }

    #[test]
    fn merge_handles_single_empty_stream() {
        let merged: Vec<ObservabilityEvent> = merge_desc_by_ts(vec![vec![]], 10);
        assert!(merged.is_empty());
    }

    #[test]
    fn merge_breaks_ties_by_kind_for_determinism() {
        // Same ts, different kinds — Request comes before Error because
        // Request's EventKind discriminant is lower.
        let reqs = vec![req_at("2026-05-01T12:00:00Z", 1)];
        let errs = vec![err_at("2026-05-01T12:00:00Z", 1)];
        let merged = merge_desc_by_ts(vec![reqs, errs], 10);
        assert_eq!(merged[0].kind(), EventKind::Request);
        assert_eq!(merged[1].kind(), EventKind::Error);
    }

    #[test]
    fn merge_drains_all_streams() {
        let errs = vec![err_at("2026-05-01T12:03:00Z", 1)];
        let reqs = vec![req_at("2026-05-01T12:02:00Z", 1)];
        let merged = merge_desc_by_ts(vec![errs, reqs], 100);
        assert_eq!(merged.len(), 2);
    }
}
