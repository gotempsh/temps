// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Performance-issue detectors over one stored trace (ADR 111, D2).
//!
//! Pure functions of a trace's spans: no I/O, no clock. Each finding names
//! the pattern it found and a grouping key, so the same pattern in the same
//! operation lands in the same issue across traces.

use std::collections::BTreeMap;

use temps_otel::types::{SpanKind, SpanRecord};

/// The same query this many times under one parent is an N+1.
pub const N_PLUS_ONE_MIN_REPEATS: usize = 5;
/// A single database span at least this slow is a slow query.
pub const SLOW_DB_SPAN_MS: f64 = 1_000.0;
/// This many HTTP calls one after another under one parent, none
/// overlapping, could have been made in parallel.
pub const SEQUENTIAL_HTTP_MIN_CALLS: usize = 3;
/// Each call in a sequential chain takes at least this long; faster calls
/// gain little from running in parallel.
pub const SEQUENTIAL_HTTP_MIN_CALL_MS: f64 = 50.0;
/// A request or response body at least this large is oversized.
pub const OVERSIZED_PAYLOAD_BYTES: u64 = 1_000_000;
/// Longest pattern text kept in a title or grouping key.
const MAX_PATTERN_CHARS: usize = 200;

/// What a detector found.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum DetectorKind {
    NPlusOneQuery,
    SlowDbQuery,
    SequentialHttp,
    OversizedPayload,
}

impl DetectorKind {
    /// Stored as the group's `error_type`, so it reads as the issue's kind.
    pub fn title(self) -> &'static str {
        match self {
            Self::NPlusOneQuery => "N+1 Query",
            Self::SlowDbQuery => "Slow DB Query",
            Self::SequentialHttp => "Consecutive HTTP Calls",
            Self::OversizedPayload => "Large HTTP Payload",
        }
    }

    pub fn key(self) -> &'static str {
        match self {
            Self::NPlusOneQuery => "n_plus_one_query",
            Self::SlowDbQuery => "slow_db_query",
            Self::SequentialHttp => "sequential_http",
            Self::OversizedPayload => "oversized_payload",
        }
    }
}

/// One finding in one trace.
#[derive(Debug, Clone, PartialEq)]
pub struct Finding {
    pub kind: DetectorKind,
    /// The service of the spans involved.
    pub service: String,
    /// The trace's root operation (`GET /checkout`), or the parent span's
    /// name when the root is missing.
    pub operation: String,
    /// The normalized query, call target or payload location.
    pub pattern: String,
    /// One line for the issue's message.
    pub summary: String,
    pub trace_id: String,
    pub span_ids: Vec<String>,
    /// How many spans are involved.
    pub occurrences: usize,
    /// Their combined duration.
    pub duration_ms: f64,
}

impl Finding {
    /// Occurrences with the same key are one issue.
    pub fn grouping_key(&self) -> String {
        format!(
            "{}\u{1f}{}\u{1f}{}\u{1f}{}",
            self.kind.key(),
            self.service,
            self.operation,
            self.pattern
        )
    }

    /// This occurrence: the same finding in the same trace is stored once.
    pub fn dedup_key(&self) -> String {
        format!("{}\u{1f}{}", self.trace_id, self.grouping_key())
    }
}

fn attribute<'a>(span: &'a SpanRecord, keys: &[&str]) -> Option<&'a str> {
    keys.iter()
        .find_map(|key| span.attributes.get(*key).map(String::as_str))
        .filter(|value| !value.is_empty())
}

fn is_db(span: &SpanRecord) -> bool {
    attribute(span, &["db.system", "db.system.name"]).is_some()
}

fn db_statement(span: &SpanRecord) -> String {
    attribute(span, &["db.query.text", "db.statement"])
        .map(normalize_statement)
        .unwrap_or_else(|| truncate(&span.name))
}

fn is_http_client(span: &SpanRecord) -> bool {
    span.kind == SpanKind::Client
        && attribute(span, &["http.request.method", "http.method"]).is_some()
}

fn http_target(span: &SpanRecord) -> String {
    let method = attribute(span, &["http.request.method", "http.method"]).unwrap_or("HTTP");
    let host = attribute(span, &["server.address", "net.peer.name", "http.host"])
        .map(str::to_string)
        .or_else(|| {
            attribute(span, &["url.full", "http.url"])
                .and_then(|url| url.split("://").nth(1))
                .and_then(|rest| rest.split('/').next())
                .map(str::to_string)
        })
        .unwrap_or_else(|| span.name.clone());
    truncate(&format!("{method} {host}"))
}

fn truncate(value: &str) -> String {
    value.chars().take(MAX_PATTERN_CHARS).collect()
}

/// A query with its literals replaced, so `WHERE id = 7` and `WHERE id = 9`
/// read the same: quoted strings and numbers become `?`, an `IN (...)` list
/// becomes `IN (?)`, and whitespace collapses.
pub fn normalize_statement(statement: &str) -> String {
    let mut out = String::with_capacity(statement.len().min(512));
    let mut chars = statement.chars().peekable();
    let mut previous_word_char = false;
    while let Some(c) = chars.next() {
        match c {
            '\'' | '"' => {
                // A quoted literal; `''` inside is an escaped quote.
                let quote = c;
                while let Some(next) = chars.next() {
                    if next == quote {
                        if chars.peek() == Some(&quote) {
                            chars.next();
                            continue;
                        }
                        break;
                    }
                }
                out.push('?');
                previous_word_char = false;
            }
            c if c.is_ascii_digit() && !previous_word_char => {
                while chars
                    .peek()
                    .is_some_and(|next| next.is_ascii_digit() || *next == '.')
                {
                    chars.next();
                }
                out.push('?');
                previous_word_char = false;
            }
            c if c.is_whitespace() => {
                if !out.ends_with(' ') && !out.is_empty() {
                    out.push(' ');
                }
                previous_word_char = false;
            }
            c => {
                out.push(c);
                previous_word_char = c.is_alphanumeric() || c == '_' || c == '$';
            }
        }
        if out.len() > 4 * MAX_PATTERN_CHARS {
            break;
        }
    }
    let collapsed = collapse_in_lists(out.trim_end());
    truncate(&collapsed)
}

/// `IN (?, ?, ?)` → `IN (?)`, so a batch size does not split a pattern.
fn collapse_in_lists(statement: &str) -> String {
    let mut out = String::with_capacity(statement.len());
    let mut rest = statement;
    while let Some(start) = rest.find("(?") {
        let (head, tail) = rest.split_at(start);
        out.push_str(head);
        match tail.find(')') {
            Some(end)
                if tail[1..end]
                    .chars()
                    .all(|c| c == '?' || c == ',' || c == ' ') =>
            {
                out.push_str("(?)");
                rest = &tail[end + 1..];
            }
            _ => {
                out.push_str("(?");
                rest = &tail[2..];
            }
        }
    }
    out.push_str(rest);
    out
}

/// Run every detector over one trace. At most one finding per detector and
/// pattern, so a loop of 500 queries is one finding with 500 occurrences.
pub fn detect(spans: &[SpanRecord]) -> Vec<Finding> {
    if spans.is_empty() {
        return Vec::new();
    }
    let by_id: BTreeMap<&str, &SpanRecord> = spans
        .iter()
        .map(|span| (span.span_id.as_str(), span))
        .collect();
    let root = spans
        .iter()
        .find(|span| {
            span.parent_span_id
                .as_deref()
                .is_none_or(|parent| !by_id.contains_key(parent))
        })
        .unwrap_or(&spans[0]);
    let operation = truncate(&root.name);
    let trace_id = root.trace_id.clone();

    let mut findings: BTreeMap<(DetectorKind, String), Finding> = BTreeMap::new();
    let mut add = |kind: DetectorKind,
                   service: &str,
                   pattern: String,
                   summary: String,
                   members: &[&SpanRecord]| {
        let entry = findings
            .entry((kind, pattern.clone()))
            .or_insert_with(|| Finding {
                kind,
                service: service.to_string(),
                operation: operation.clone(),
                pattern,
                summary,
                trace_id: trace_id.clone(),
                span_ids: Vec::new(),
                occurrences: 0,
                duration_ms: 0.0,
            });
        for span in members {
            if entry.span_ids.len() < 20 {
                entry.span_ids.push(span.span_id.clone());
            }
            entry.occurrences += 1;
            entry.duration_ms += span.duration_ms.max(0.0);
        }
    };

    // Children of each parent, in start order.
    let mut children: BTreeMap<&str, Vec<&SpanRecord>> = BTreeMap::new();
    for span in spans {
        if let Some(parent) = span.parent_span_id.as_deref() {
            children.entry(parent).or_default().push(span);
        }
    }
    for siblings in children.values_mut() {
        siblings.sort_by_key(|span| span.start_time);
    }

    for siblings in children.values() {
        // N+1: the same statement repeated under one parent.
        let mut statements: BTreeMap<String, Vec<&SpanRecord>> = BTreeMap::new();
        for span in siblings.iter().filter(|span| is_db(span)) {
            statements.entry(db_statement(span)).or_default().push(span);
        }
        for (statement, repeated) in statements {
            if repeated.len() >= N_PLUS_ONE_MIN_REPEATS {
                let summary = format!("{statement} ran {} times in {operation}", repeated.len());
                add(
                    DetectorKind::NPlusOneQuery,
                    &repeated[0].resource.service_name,
                    statement,
                    summary,
                    &repeated,
                );
            }
        }

        // Sequential HTTP: the longest run of non-overlapping slow calls.
        let calls: Vec<&SpanRecord> = siblings
            .iter()
            .copied()
            .filter(|span| is_http_client(span) && span.duration_ms >= SEQUENTIAL_HTTP_MIN_CALL_MS)
            .collect();
        let mut chain: Vec<&SpanRecord> = Vec::new();
        let mut best: Vec<&SpanRecord> = Vec::new();
        for call in calls {
            match chain.last() {
                Some(last) if call.start_time >= last.end_time => chain.push(call),
                _ => chain = vec![call],
            }
            if chain.len() > best.len() {
                best = chain.clone();
            }
        }
        if best.len() >= SEQUENTIAL_HTTP_MIN_CALLS {
            let targets: Vec<String> = best.iter().map(|span| http_target(span)).collect();
            let mut distinct = targets.clone();
            distinct.dedup();
            let pattern = truncate(&distinct.join(" → "));
            let summary = format!(
                "{} HTTP calls ran one after another in {operation}: {pattern}",
                best.len()
            );
            add(
                DetectorKind::SequentialHttp,
                &best[0].resource.service_name,
                pattern,
                summary,
                &best,
            );
        }
    }

    for span in spans {
        if is_db(span) && span.duration_ms >= SLOW_DB_SPAN_MS {
            let statement = db_statement(span);
            let summary = format!("{statement} took {:.0} ms in {operation}", span.duration_ms);
            add(
                DetectorKind::SlowDbQuery,
                &span.resource.service_name,
                statement,
                summary,
                &[span],
            );
        }
        for (key, direction) in [
            ("http.response.body.size", "response"),
            ("http.response_content_length", "response"),
            ("http.request.body.size", "request"),
            ("http.request_content_length", "request"),
        ] {
            let Some(bytes) = span
                .attributes
                .get(key)
                .and_then(|value| value.parse::<u64>().ok())
            else {
                continue;
            };
            if bytes >= OVERSIZED_PAYLOAD_BYTES {
                let location = if span.kind == SpanKind::Client {
                    http_target(span)
                } else {
                    truncate(&span.name)
                };
                let pattern = truncate(&format!("{direction} of {location}"));
                let summary = format!(
                    "a {:.1} MB {direction} body in {operation} ({location})",
                    bytes as f64 / 1_000_000.0
                );
                add(
                    DetectorKind::OversizedPayload,
                    &span.resource.service_name,
                    pattern,
                    summary,
                    &[span],
                );
                break;
            }
        }
    }

    findings.into_values().collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::{Duration, TimeZone, Utc};
    use temps_otel::types::{ResourceInfo, SpanStatusCode};

    fn span(
        id: &str,
        parent: Option<&str>,
        name: &str,
        start_ms: i64,
        duration_ms: i64,
    ) -> SpanRecord {
        let start = Utc
            .with_ymd_and_hms(2026, 11, 20, 10, 0, 0)
            .single()
            .expect("time")
            + Duration::milliseconds(start_ms);
        SpanRecord {
            ingest_identity: None,
            project_id: 1,
            deployment_id: None,
            resource: ResourceInfo {
                service_name: "shop-api".into(),
                service_version: None,
                deployment_environment: None,
                attributes: BTreeMap::new(),
            },
            trace_id: "0af7651916cd43dd8448eb211c80319c".into(),
            span_id: id.into(),
            parent_span_id: parent.map(str::to_string),
            name: name.into(),
            kind: SpanKind::Internal,
            start_time: start,
            end_time: start + Duration::milliseconds(duration_ms),
            duration_ms: duration_ms as f64,
            status_code: SpanStatusCode::Ok,
            status_message: String::new(),
            attributes: BTreeMap::new(),
            events: Vec::new(),
        }
    }

    fn db(id: &str, statement: &str, start_ms: i64, duration_ms: i64) -> SpanRecord {
        let mut span = span(id, Some("root"), "SELECT", start_ms, duration_ms);
        span.kind = SpanKind::Client;
        span.attributes
            .insert("db.system".into(), "postgresql".into());
        span.attributes
            .insert("db.statement".into(), statement.into());
        span
    }

    fn http(id: &str, host: &str, start_ms: i64, duration_ms: i64) -> SpanRecord {
        let mut span = span(id, Some("root"), "GET", start_ms, duration_ms);
        span.kind = SpanKind::Client;
        span.attributes
            .insert("http.request.method".into(), "GET".into());
        span.attributes.insert("server.address".into(), host.into());
        span
    }

    #[test]
    fn statements_normalize_literals_lists_and_whitespace() {
        assert_eq!(
            normalize_statement("SELECT *  FROM items\n WHERE cart_id = 42 AND sku = 'A''1'"),
            "SELECT * FROM items WHERE cart_id = ? AND sku = ?"
        );
        assert_eq!(
            normalize_statement("SELECT * FROM t WHERE id IN (1, 2, 3) AND v2 = $1"),
            "SELECT * FROM t WHERE id IN (?) AND v2 = $1"
        );
    }

    #[test]
    fn a_repeated_query_under_one_parent_is_an_n_plus_one() {
        let mut spans = vec![span("root", None, "GET /checkout", 0, 300)];
        for n in 0..6 {
            spans.push(db(
                &format!("q{n}"),
                &format!("SELECT * FROM items WHERE cart_id = {n}"),
                n * 10,
                5,
            ));
        }
        // Four of another query is under the threshold.
        for n in 0..4 {
            spans.push(db(&format!("p{n}"), "SELECT 1", 100 + n, 1));
        }
        let findings = detect(&spans);
        assert_eq!(findings.len(), 1, "{findings:?}");
        let finding = &findings[0];
        assert_eq!(finding.kind, DetectorKind::NPlusOneQuery);
        assert_eq!(finding.operation, "GET /checkout");
        assert_eq!(finding.pattern, "SELECT * FROM items WHERE cart_id = ?");
        assert_eq!(finding.occurrences, 6);
        assert_eq!(finding.duration_ms, 30.0);
        assert!(finding.summary.contains("ran 6 times"));
    }

    #[test]
    fn a_slow_query_is_found_once_per_pattern() {
        let spans = vec![
            span("root", None, "POST /orders", 0, 4000),
            db("a", "UPDATE stock SET n = n - 1 WHERE sku = 'x'", 0, 1500),
            db(
                "b",
                "UPDATE stock SET n = n - 1 WHERE sku = 'y'",
                1600,
                1200,
            ),
            db("c", "SELECT 1", 2900, 999),
        ];
        let findings = detect(&spans);
        assert_eq!(findings.len(), 1, "{findings:?}");
        assert_eq!(findings[0].kind, DetectorKind::SlowDbQuery);
        assert_eq!(findings[0].occurrences, 2);
    }

    #[test]
    fn http_calls_one_after_another_are_found_but_parallel_ones_are_not() {
        let sequential = vec![
            span("root", None, "GET /profile", 0, 500),
            http("a", "users.internal", 0, 100),
            http("b", "billing.internal", 110, 100),
            http("c", "prefs.internal", 220, 100),
        ];
        let findings = detect(&sequential);
        assert_eq!(findings.len(), 1, "{findings:?}");
        assert_eq!(findings[0].kind, DetectorKind::SequentialHttp);
        assert_eq!(
            findings[0].pattern,
            "GET users.internal → GET billing.internal → GET prefs.internal"
        );

        let parallel = vec![
            span("root", None, "GET /profile", 0, 200),
            http("a", "users.internal", 0, 100),
            http("b", "billing.internal", 10, 100),
            http("c", "prefs.internal", 20, 100),
        ];
        assert!(detect(&parallel).is_empty());

        let fast = vec![
            span("root", None, "GET /profile", 0, 200),
            http("a", "users.internal", 0, 10),
            http("b", "billing.internal", 20, 10),
            http("c", "prefs.internal", 40, 10),
        ];
        assert!(
            detect(&fast).is_empty(),
            "fast calls gain little in parallel"
        );
    }

    #[test]
    fn a_large_body_is_an_oversized_payload() {
        let mut root = span("root", None, "GET /export", 0, 900);
        root.kind = SpanKind::Server;
        root.attributes
            .insert("http.response.body.size".into(), "2500000".into());
        let mut small = span("child", Some("root"), "render", 0, 10);
        small
            .attributes
            .insert("http.response.body.size".into(), "999999".into());
        let findings = detect(&[root, small]);
        assert_eq!(findings.len(), 1, "{findings:?}");
        assert_eq!(findings[0].kind, DetectorKind::OversizedPayload);
        assert_eq!(findings[0].pattern, "response of GET /export");
        assert!(findings[0].summary.contains("2.5 MB"));
    }

    #[test]
    fn keys_are_stable_across_traces_and_distinct_per_trace() {
        let build = |trace: &str| {
            let mut spans = vec![span("root", None, "GET /checkout", 0, 300)];
            for n in 0..5 {
                spans.push(db(
                    &format!("q{n}"),
                    &format!("SELECT * FROM a WHERE id = {n}"),
                    n,
                    1,
                ));
            }
            for span in &mut spans {
                span.trace_id = trace.into();
            }
            detect(&spans).remove(0)
        };
        let (first, second) = (build("trace-a"), build("trace-b"));
        assert_eq!(first.grouping_key(), second.grouping_key());
        assert_ne!(first.dedup_key(), second.dedup_key());
    }

    #[test]
    fn an_empty_trace_finds_nothing() {
        assert!(detect(&[]).is_empty());
    }
}
