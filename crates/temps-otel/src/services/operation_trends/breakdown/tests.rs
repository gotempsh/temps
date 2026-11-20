// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

use super::*;
use crate::types::{ResourceInfo, SpanStatusCode};
use chrono::{DateTime, Duration, Utc};
use std::collections::BTreeMap;

fn base() -> DateTime<Utc> {
    DateTime::parse_from_rfc3339("2026-11-20T10:00:00Z")
        .unwrap()
        .with_timezone(&Utc)
}

#[allow(clippy::too_many_arguments, reason = "a test fixture spelling out one span")]
fn span(
    trace: &str,
    id: &str,
    parent: Option<&str>,
    service: &str,
    name: &str,
    kind: SpanKind,
    start_ms: i64,
    duration_ms: i64,
    attributes: &[(&str, &str)],
) -> SpanRecord {
    let start = base() + Duration::milliseconds(start_ms);
    SpanRecord {
        ingest_identity: None,
        project_id: 1,
        deployment_id: None,
        resource: ResourceInfo {
            service_name: service.to_string(),
            service_version: None,
            deployment_environment: None,
            attributes: Default::default(),
        },
        trace_id: trace.to_string(),
        span_id: id.to_string(),
        parent_span_id: parent.map(str::to_string),
        name: name.to_string(),
        kind,
        start_time: start,
        end_time: start + Duration::milliseconds(duration_ms),
        duration_ms: duration_ms as f64,
        status_code: SpanStatusCode::Ok,
        status_message: String::new(),
        attributes: attributes
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect::<BTreeMap<_, _>>(),
        events: Vec::new(),
    }
}

/// `GET /cart` (100 ms) runs a 60 ms query, then calls a pricing service
/// whose 30 ms server span does a 20 ms query of its own.
fn cart_trace(trace: &str, region: &str) -> Vec<SpanRecord> {
    vec![
        span(trace, "a", None, "checkout-api", "GET /cart", SpanKind::Server, 0, 100, &[("region", region)]),
        span(trace, "b", Some("a"), "checkout-api", "SELECT cart_items", SpanKind::Client, 0, 60, &[("db.system", "postgresql")]),
        span(trace, "c", Some("a"), "checkout-api", "POST /prices", SpanKind::Client, 60, 35, &[("http.request.method", "POST")]),
        span(trace, "d", Some("c"), "pricing", "POST /prices", SpanKind::Server, 62, 30, &[]),
        span(trace, "e", Some("d"), "pricing", "SELECT prices", SpanKind::Client, 65, 20, &[("db.system", "postgresql")]),
    ]
}

#[test]
fn time_is_charged_to_the_span_that_spent_it() {
    let mut builder = BreakdownBuilder::new("checkout-api", "GET /cart", None);
    builder.add_trace(&cart_trace("t1", "eu"));
    let breakdown = builder.finish();
    assert_eq!(breakdown.instances, 1);
    assert_eq!(breakdown.total_ms, 100.0);
    // 100 ms minus the 95 ms its two children cover.
    assert!((breakdown.self_ms - 5.0).abs() < 1e-6);
    let by_name: HashMap<_, _> = breakdown
        .groups
        .iter()
        .map(|g| ((g.service_name.as_str(), g.span_name.as_str()), g))
        .collect();
    let query = by_name[&("checkout-api", "SELECT cart_items")];
    assert_eq!(query.category, "database");
    assert!((query.total_ms - 60.0).abs() < 1e-6);
    // The HTTP client span is charged only the 5 ms its server span does not cover.
    let client = by_name[&("checkout-api", "POST /prices")];
    assert_eq!(client.category, "http");
    assert!((client.total_ms - 5.0).abs() < 1e-6);
    let server = by_name[&("pricing", "POST /prices")];
    assert!((server.total_ms - 10.0).abs() < 1e-6);
    assert_eq!(breakdown.groups[0].span_name, "SELECT cart_items", "largest first");
}

#[test]
fn groups_sum_across_calls_and_count_the_calls_that_made_them() {
    let mut builder = BreakdownBuilder::new("checkout-api", "GET /cart", Some("region"));
    builder.add_trace(&cart_trace("t1", "eu"));
    builder.add_trace(&cart_trace("t2", "us"));
    let mut slow = cart_trace("t3", "us");
    slow[0].duration_ms = 300.0;
    slow[0].end_time = slow[0].start_time + Duration::milliseconds(300);
    builder.add_trace(&slow);
    let breakdown = builder.finish();
    assert_eq!(breakdown.traces, 3);
    assert_eq!(breakdown.instances, 3);
    let query = breakdown
        .groups
        .iter()
        .find(|g| g.span_name == "SELECT cart_items")
        .unwrap();
    assert_eq!(query.calls, 3);
    assert_eq!(query.instances, 3);
    assert_eq!(breakdown.by_attribute.len(), 2);
    assert_eq!(breakdown.by_attribute[0].value.as_deref(), Some("us"));
    assert_eq!(breakdown.by_attribute[0].calls, 2);
    assert_eq!(breakdown.by_attribute[0].max_ms, 300.0);
    assert!((breakdown.by_attribute[0].avg_ms - 200.0).abs() < 1e-6);
}

#[test]
fn a_loop_of_queries_is_one_group_with_many_calls() {
    let mut spans = vec![span("t1", "a", None, "api", "GET /orders", SpanKind::Server, 0, 100, &[])];
    for i in 0..10 {
        spans.push(span(
            "t1",
            &format!("q{i}"),
            Some("a"),
            "api",
            "SELECT order",
            SpanKind::Client,
            i * 10,
            8,
            &[("db.system", "postgresql")],
        ));
    }
    let mut builder = BreakdownBuilder::new("api", "GET /orders", None);
    builder.add_trace(&spans);
    let breakdown = builder.finish();
    assert_eq!(breakdown.groups.len(), 1);
    assert_eq!(breakdown.groups[0].calls, 10);
    assert_eq!(breakdown.groups[0].instances, 1);
    assert!((breakdown.groups[0].total_ms - 80.0).abs() < 1e-6);
    assert!((breakdown.self_ms - 20.0).abs() < 1e-6);
}

#[test]
fn a_nested_call_of_the_operation_is_counted_once() {
    let spans = vec![
        span("t1", "a", None, "api", "render", SpanKind::Internal, 0, 100, &[]),
        span("t1", "b", Some("a"), "api", "render", SpanKind::Internal, 10, 50, &[]),
    ];
    let mut builder = BreakdownBuilder::new("api", "render", None);
    builder.add_trace(&spans);
    let breakdown = builder.finish();
    assert_eq!(breakdown.instances, 1);
    assert_eq!(breakdown.groups.len(), 1, "the inner call is a downstream group");
}

#[test]
fn overlapping_children_are_not_double_subtracted_and_overruns_are_clipped() {
    let spans = vec![
        span("t1", "a", None, "api", "GET /", SpanKind::Server, 0, 100, &[]),
        span("t1", "b", Some("a"), "api", "fetch a", SpanKind::Client, 10, 50, &[]),
        span("t1", "c", Some("a"), "api", "fetch b", SpanKind::Client, 30, 50, &[]),
        // Outlives its parent by 20 ms; only 10 ms of it is inside.
        span("t1", "d", Some("a"), "api", "async job", SpanKind::Producer, 90, 30, &[]),
    ];
    let mut builder = BreakdownBuilder::new("api", "GET /", None);
    builder.add_trace(&spans);
    let breakdown = builder.finish();
    // Covered: [10, 80) and [90, 100) = 80 ms, so 20 ms of self time.
    assert!((breakdown.self_ms - 20.0).abs() < 1e-6, "{}", breakdown.self_ms);
    let job = breakdown.groups.iter().find(|g| g.span_name == "async job").unwrap();
    assert_eq!(job.category, "messaging");
}

#[test]
fn groups_past_the_cap_are_summed_into_one_row() {
    let mut spans = vec![span("t1", "root", None, "api", "GET /", SpanKind::Server, 0, 1000, &[])];
    for i in 0..(MAX_GROUPS as i64 + 5) {
        spans.push(span(
            "t1",
            &format!("s{i}"),
            Some("root"),
            "api",
            &format!("step {i:02}"),
            SpanKind::Internal,
            i,
            1 + i,
            &[],
        ));
    }
    let mut builder = BreakdownBuilder::new("api", "GET /", None);
    builder.add_trace(&spans);
    let breakdown = builder.finish();
    assert_eq!(breakdown.groups.len(), MAX_GROUPS);
    let other = breakdown.groups.last().unwrap();
    assert_eq!(other.category, "other");
    assert_eq!(other.span_name, "6 other calls");
    assert_eq!(other.calls, 6);
}

#[test]
fn a_trace_without_the_operation_adds_nothing() {
    let mut builder = BreakdownBuilder::new("api", "GET /missing", None);
    builder.add_trace(&cart_trace("t1", "eu"));
    let breakdown = builder.finish();
    assert_eq!(breakdown.traces, 1);
    assert_eq!(breakdown.instances, 0);
    assert!(breakdown.groups.is_empty());
}
