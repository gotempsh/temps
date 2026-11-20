// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! A page's web vitals next to the backend requests that served it
//! (ADR 111, D2).
//!
//! A page load carries no trace id (the browser sends no `traceparent` on a
//! navigation), so the join is by project, path and time: the server spans
//! of the project whose `url.path` (or the older `http.target`) is the page's
//! path, in the same window as the page's web vitals. It answers "how much
//! of this page's time to first byte is the backend" and links the slowest
//! of those requests to their traces.

use chrono::{DateTime, Duration, Utc};
use sea_orm::{ConnectionTrait, DatabaseBackend, Statement};
use std::collections::BTreeMap;

use super::{OperationTrendService, MAX_BREAKDOWN_DAYS};
use crate::error::OtelError;
use crate::types::{SpanKind, SpanRecord, TraceQuery};

/// Server spans read per attribute looked up.
pub const PAGE_BACKEND_CANDIDATES: u64 = 500;
/// Slowest backend requests returned.
pub const PAGE_BACKEND_SLOWEST: usize = 10;
/// The longest path accepted.
pub const MAX_PATH_CHARS: usize = 2_048;

/// The 75th percentiles of a page's web vitals, as the analytics SDK
/// recorded them.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct PageVitals {
    pub page_loads: i64,
    pub ttfb_p75_ms: Option<f64>,
    pub fcp_p75_ms: Option<f64>,
    pub lcp_p75_ms: Option<f64>,
    pub inp_p75_ms: Option<f64>,
}

/// One backend request that served the page.
#[derive(Debug, Clone, PartialEq)]
pub struct BackendRequest {
    pub trace_id: String,
    pub span_name: String,
    pub service_name: String,
    pub start_time: DateTime<Utc>,
    pub duration_ms: f64,
    pub error: bool,
}

#[derive(Debug, Clone, PartialEq, Default)]
pub struct PageBackend {
    pub vitals: PageVitals,
    /// Backend requests found for the path.
    pub requests: usize,
    pub p50_ms: Option<f64>,
    pub p75_ms: Option<f64>,
    pub p95_ms: Option<f64>,
    pub errors: usize,
    /// Services that served it, by requests.
    pub services: Vec<(String, usize)>,
    /// Slowest first.
    pub slowest: Vec<BackendRequest>,
}

/// Whether `span` is a server request for `path` (query string ignored).
pub fn serves_path(span: &SpanRecord, path: &str) -> bool {
    if span.kind != SpanKind::Server {
        return false;
    }
    let strip = |value: &str| value.split('?').next().unwrap_or("").to_string();
    span.attributes
        .get("url.path")
        .or_else(|| span.attributes.get("http.target"))
        .is_some_and(|value| strip(value) == path)
}

fn percentile(sorted: &[f64], p: f64) -> Option<f64> {
    if sorted.is_empty() {
        return None;
    }
    let rank = (p * (sorted.len() - 1) as f64).round() as usize;
    sorted.get(rank.min(sorted.len() - 1)).copied()
}

/// Summarise the matched server spans.
pub fn summarize(vitals: PageVitals, spans: Vec<SpanRecord>) -> PageBackend {
    let mut durations: Vec<f64> = spans.iter().map(|s| s.duration_ms).collect();
    durations.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    let mut services: BTreeMap<String, usize> = BTreeMap::new();
    for span in &spans {
        *services
            .entry(span.resource.service_name.clone())
            .or_default() += 1;
    }
    let mut services: Vec<(String, usize)> = services.into_iter().collect();
    services.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
    let errors = spans
        .iter()
        .filter(|s| s.status_code == crate::types::SpanStatusCode::Error)
        .count();
    let mut slowest: Vec<BackendRequest> = spans
        .into_iter()
        .map(|span| BackendRequest {
            error: span.status_code == crate::types::SpanStatusCode::Error,
            trace_id: span.trace_id,
            span_name: span.name,
            service_name: span.resource.service_name,
            start_time: span.start_time,
            duration_ms: span.duration_ms,
        })
        .collect();
    slowest.sort_by(|a, b| {
        b.duration_ms
            .partial_cmp(&a.duration_ms)
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    slowest.truncate(PAGE_BACKEND_SLOWEST);
    PageBackend {
        vitals,
        requests: durations.len(),
        p50_ms: percentile(&durations, 0.50),
        p75_ms: percentile(&durations, 0.75),
        p95_ms: percentile(&durations, 0.95),
        errors,
        services,
        slowest,
    }
}

impl OperationTrendService {
    /// A page's web vitals and the backend requests for its path in
    /// `[start, end]`.
    pub async fn page_backend(
        &self,
        project_id: i32,
        path: &str,
        start: DateTime<Utc>,
        end: DateTime<Utc>,
    ) -> Result<PageBackend, OtelError> {
        if !path.starts_with('/') || path.chars().count() > MAX_PATH_CHARS {
            return Err(OtelError::Validation {
                message: format!(
                    "The page path must start with / and be at most {MAX_PATH_CHARS} characters, got {path:?}"
                ),
            });
        }
        if start >= end || end - start > Duration::days(MAX_BREAKDOWN_DAYS) {
            return Err(OtelError::Validation {
                message: format!(
                    "The window must start before it ends and span at most {MAX_BREAKDOWN_DAYS} days ({start} to {end})"
                ),
            });
        }
        let vitals = self.page_vitals(project_id, path, start, end).await?;
        let mut matched: Vec<SpanRecord> = Vec::new();
        let mut seen = std::collections::HashSet::new();
        // `url.path` holds the bare path; `http.target` (older semantic
        // conventions) carries the query string too, so it matches by prefix.
        for key in ["url.path", "http.target"] {
            let (attributes, attribute_path) = if key == "url.path" {
                (
                    Some(BTreeMap::from([(key.to_string(), path.to_string())])),
                    None,
                )
            } else {
                (
                    None,
                    Some(crate::types::AttributePath {
                        key: key.to_string(),
                        path: path.to_string(),
                    }),
                )
            };
            let spans = self
                .storage
                .query_spans(TraceQuery {
                    project_id,
                    start_time: Some(start),
                    end_time: Some(end),
                    attributes,
                    attribute_path,
                    limit: Some(PAGE_BACKEND_CANDIDATES),
                    ..Default::default()
                })
                .await?;
            for span in spans {
                if serves_path(&span, path) && seen.insert(span.span_id.clone()) {
                    matched.push(span);
                }
            }
        }
        Ok(summarize(vitals, matched))
    }

    async fn page_vitals(
        &self,
        project_id: i32,
        path: &str,
        start: DateTime<Utc>,
        end: DateTime<Utc>,
    ) -> Result<PageVitals, OtelError> {
        let row = self
            .db
            .query_one(Statement::from_sql_and_values(
                DatabaseBackend::Postgres,
                "SELECT COUNT(*)::bigint AS page_loads, \
                     percentile_cont(0.75) WITHIN GROUP (ORDER BY ttfb)::double precision AS ttfb, \
                     percentile_cont(0.75) WITHIN GROUP (ORDER BY fcp)::double precision AS fcp, \
                     percentile_cont(0.75) WITHIN GROUP (ORDER BY lcp)::double precision AS lcp, \
                     percentile_cont(0.75) WITHIN GROUP (ORDER BY inp)::double precision AS inp \
                 FROM performance_metrics \
                 WHERE project_id = $1 AND pathname = $2 AND NOT is_crawler \
                   AND recorded_at >= $3 AND recorded_at <= $4",
                [project_id.into(), path.into(), start.into(), end.into()],
            ))
            .await?;
        let Some(row) = row else {
            return Ok(PageVitals::default());
        };
        Ok(PageVitals {
            page_loads: row.try_get("", "page_loads")?,
            ttfb_p75_ms: row.try_get("", "ttfb")?,
            fcp_p75_ms: row.try_get("", "fcp")?,
            lcp_p75_ms: row.try_get("", "lcp")?,
            inp_p75_ms: row.try_get("", "inp")?,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::storage::OtelStorage;
    use crate::test_support::MockOtelStorage;
    use crate::types::{ResourceInfo, SpanStatusCode};
    use std::sync::Arc;

    fn span(id: &str, kind: SpanKind, attributes: &[(&str, &str)], duration_ms: f64) -> SpanRecord {
        let start = Utc::now() - Duration::minutes(10);
        SpanRecord {
            ingest_identity: None,
            project_id: 1,
            deployment_id: None,
            resource: ResourceInfo {
                service_name: "storefront".to_string(),
                service_version: None,
                deployment_environment: None,
                attributes: Default::default(),
            },
            trace_id: format!("{id:0>32}"),
            span_id: id.to_string(),
            parent_span_id: None,
            name: "GET /products/:id".to_string(),
            kind,
            start_time: start,
            end_time: start + Duration::milliseconds(duration_ms as i64),
            duration_ms,
            status_code: if duration_ms > 1000.0 {
                SpanStatusCode::Error
            } else {
                SpanStatusCode::Ok
            },
            status_message: String::new(),
            attributes: attributes
                .iter()
                .map(|(k, v)| (k.to_string(), v.to_string()))
                .collect(),
            events: Vec::new(),
        }
    }

    #[test]
    fn only_server_spans_for_the_exact_path_match() {
        let path = "/products/42";
        assert!(serves_path(
            &span("a", SpanKind::Server, &[("url.path", path)], 1.0),
            path
        ));
        assert!(serves_path(
            &span(
                "b",
                SpanKind::Server,
                &[("http.target", "/products/42?ref=home")],
                1.0
            ),
            path
        ));
        assert!(!serves_path(
            &span("c", SpanKind::Client, &[("url.path", path)], 1.0),
            path
        ));
        assert!(!serves_path(
            &span("d", SpanKind::Server, &[("url.path", "/products/420")], 1.0),
            path
        ));
        assert!(!serves_path(&span("e", SpanKind::Server, &[], 1.0), path));
    }

    #[test]
    fn the_summary_ranks_the_slowest_requests_and_counts_errors() {
        let spans: Vec<SpanRecord> = (0..20)
            .map(|i| {
                span(
                    &format!("s{i}"),
                    SpanKind::Server,
                    &[],
                    100.0 + i as f64 * 100.0,
                )
            })
            .collect();
        let summary = summarize(PageVitals::default(), spans);
        assert_eq!(summary.requests, 20);
        assert_eq!(summary.slowest.len(), PAGE_BACKEND_SLOWEST);
        assert_eq!(summary.slowest[0].duration_ms, 2000.0);
        assert_eq!(summary.errors, 10);
        assert_eq!(summary.p50_ms, Some(1100.0));
        assert_eq!(summary.services, vec![("storefront".to_string(), 20)]);
        let empty = summarize(PageVitals::default(), Vec::new());
        assert_eq!(empty.p75_ms, None);
    }

    #[tokio::test]
    async fn a_page_is_joined_to_the_server_spans_that_served_its_path() {
        let storage = Arc::new(MockOtelStorage::new());
        storage
            .store_spans(vec![
                span(
                    "a",
                    SpanKind::Server,
                    &[("url.path", "/products/42")],
                    120.0,
                ),
                span(
                    "b",
                    SpanKind::Server,
                    &[("http.target", "/products/42?x=1")],
                    300.0,
                ),
                span("c", SpanKind::Server, &[("url.path", "/cart")], 50.0),
                span("d", SpanKind::Client, &[("url.path", "/products/42")], 80.0),
            ])
            .await
            .expect("spans");
        let db = Arc::new(
            sea_orm::MockDatabase::new(DatabaseBackend::Postgres)
                .append_query_results([vec![BTreeMap::from([
                    ("page_loads", sea_orm::Value::from(12_i64)),
                    ("ttfb", sea_orm::Value::from(Some(400.0_f64))),
                    ("fcp", sea_orm::Value::from(Some(900.0_f64))),
                    ("lcp", sea_orm::Value::from(Some(1800.0_f64))),
                    ("inp", sea_orm::Value::from(None::<f64>)),
                ])]])
                .into_connection(),
        );
        let service = OperationTrendService::new(db, storage);
        let now = Utc::now();
        let result = service
            .page_backend(1, "/products/42", now - Duration::days(1), now)
            .await
            .expect("page backend");
        assert_eq!(result.vitals.page_loads, 12);
        assert_eq!(result.vitals.ttfb_p75_ms, Some(400.0));
        assert_eq!(result.vitals.inp_p75_ms, None);
        assert_eq!(
            result.requests, 2,
            "the client span and other path are left out"
        );
        assert_eq!(result.slowest[0].duration_ms, 300.0);
    }

    #[tokio::test]
    async fn a_bad_path_or_window_is_rejected() {
        let service = OperationTrendService::new(
            Arc::new(sea_orm::MockDatabase::new(DatabaseBackend::Postgres).into_connection()),
            Arc::new(MockOtelStorage::new()),
        );
        let now = Utc::now();
        for (path, start) in [
            ("products", now - Duration::days(1)),
            ("/products", now - Duration::days(MAX_BREAKDOWN_DAYS + 1)),
            ("/products", now),
        ] {
            let error = service
                .page_backend(1, path, start, now)
                .await
                .expect_err("rejected");
            assert!(matches!(error, OtelError::Validation { .. }), "{error}");
        }
    }
}
