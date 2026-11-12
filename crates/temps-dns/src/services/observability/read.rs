// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! What the console reads (ADR-095 §5): resolver reports, the zone's
//! records with their owners, the query log and traffic over time, and the
//! failure counts the alert monitor evaluates.

use std::collections::{BTreeMap, HashSet};

use chrono::{DateTime, Duration, Utc};
use futures::future::try_join_all;
use sea_orm::{
    ColumnTrait, DatabaseBackend, EntityTrait, FromQueryResult, QueryFilter, QueryOrder,
    QuerySelect, Statement, Value,
};
use temps_entities::{alarms, dns_resolver_reports};
use temps_metrics::{RangeQuery, SourceKind};
use temps_node_dns::observe::{QueryOutcome, ZoneClass};

use super::{DnsObservabilityError, DnsObservabilityService};

/// Longest search text a list accepts.
const MAX_SEARCH_LEN: usize = 253;

/// Filters for the query log. Every field narrows the list.
#[derive(Debug, Clone, Default)]
pub struct QueryLogFilter {
    pub node_id: Option<i32>,
    pub outcome: Option<QueryOutcome>,
    pub zone: Option<ZoneClass>,
    /// Substring of the name, case-insensitive.
    pub name: Option<String>,
    pub project_id: Option<i32>,
    pub since: Option<DateTime<Utc>>,
}

/// One query-log row with the names of who asked.
#[derive(Debug, Clone, PartialEq, Eq, FromQueryResult)]
pub struct QueryLogRow {
    pub id: i64,
    pub node_id: i32,
    pub node_name: Option<String>,
    pub observed_at: DateTime<Utc>,
    pub qname: Option<String>,
    pub qtype: String,
    pub zone: String,
    pub outcome: String,
    pub latency_us: i64,
    pub answer_count: i32,
    pub source_ip: String,
    pub project_id: Option<i32>,
    pub project_name: Option<String>,
    pub environment_id: Option<i32>,
    pub environment_name: Option<String>,
    pub service_id: Option<i32>,
    pub service_name: Option<String>,
}

/// Filters for the record list.
#[derive(Debug, Clone, Default)]
pub struct RecordFilter {
    /// Substring of the name or target, case-insensitive.
    pub search: Option<String>,
    pub node_id: Option<i32>,
    pub owner_kind: Option<String>,
}

/// One record of the internal zone with its owner's names.
#[derive(Debug, Clone, PartialEq, Eq, FromQueryResult)]
pub struct RecordRow {
    pub id: i64,
    pub fqdn: String,
    pub record_type: String,
    pub target_ip: Option<String>,
    pub target_port: Option<i32>,
    pub ttl: i32,
    pub owner_kind: String,
    pub owner_id: i64,
    pub node_id: Option<i32>,
    pub node_name: Option<String>,
    pub project_id: Option<i32>,
    pub project_name: Option<String>,
    pub environment_id: Option<i32>,
    pub environment_name: Option<String>,
    pub service_id: Option<i32>,
    pub service_name: Option<String>,
    pub generation: i64,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

/// One bucket of DNS traffic on a node.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct TrafficBucket {
    pub time: DateTime<Utc>,
    pub internal: OutcomeCounts,
    pub external: OutcomeCounts,
    /// Mean latency of internal queries in the bucket, in microseconds;
    /// `None` without internal queries.
    pub internal_latency_us: Option<f64>,
}

/// Queries per outcome in one bucket.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct OutcomeCounts {
    pub noerror: f64,
    pub nodata: f64,
    pub nxdomain: f64,
    pub servfail: f64,
    pub refused: f64,
    pub error: f64,
}

impl OutcomeCounts {
    fn set(&mut self, outcome: QueryOutcome, value: f64) {
        match outcome {
            QueryOutcome::Noerror => self.noerror = value,
            QueryOutcome::Nodata => self.nodata = value,
            QueryOutcome::Nxdomain => self.nxdomain = value,
            QueryOutcome::Servfail => self.servfail = value,
            QueryOutcome::Refused => self.refused = value,
            QueryOutcome::Error => self.error = value,
        }
    }

    pub fn total(&self) -> f64 {
        self.noerror + self.nodata + self.nxdomain + self.servfail + self.refused + self.error
    }
}

/// Traffic of one node over a window.
#[derive(Debug, Clone, PartialEq)]
pub struct TrafficSeries {
    pub node_id: i32,
    pub from: DateTime<Utc>,
    pub to: DateTime<Utc>,
    pub step_secs: i64,
    pub buckets: Vec<TrafficBucket>,
}

/// Internal failures for one name on one node, for the alert monitor.
#[derive(Debug, Clone, PartialEq, Eq, FromQueryResult)]
pub struct FailureCount {
    pub node_id: i32,
    pub qname: Option<String>,
    pub outcome: String,
    pub count: i64,
}

#[derive(Debug, FromQueryResult)]
struct Total {
    total: i64,
}

#[derive(Debug, FromQueryResult)]
struct NodeTotal {
    node_id: i32,
    total: i64,
}

#[derive(Debug, FromQueryResult)]
struct NameRow {
    name: Option<String>,
}

fn search_pattern(value: &str, field: &'static str) -> Result<String, DnsObservabilityError> {
    let trimmed = value.trim();
    if trimmed.len() > MAX_SEARCH_LEN {
        return Err(DnsObservabilityError::InvalidFilter {
            field,
            reason: format!("longer than {MAX_SEARCH_LEN} characters"),
        });
    }
    // Escape LIKE's wildcards so the text matches literally.
    let escaped = trimmed
        .to_ascii_lowercase()
        .replace('\\', "\\\\")
        .replace('%', "\\%")
        .replace('_', "\\_");
    Ok(format!("%{escaped}%"))
}

/// A `WHERE` clause under construction with its bound values.
struct Where {
    clauses: Vec<String>,
    values: Vec<Value>,
}

impl Where {
    fn new() -> Self {
        Self {
            clauses: Vec::new(),
            values: Vec::new(),
        }
    }

    /// Add `clause`, whose `?` is replaced by the next placeholder.
    fn push(&mut self, clause: &str, value: impl Into<Value>) {
        self.values.push(value.into());
        let placeholder = format!("${}", self.values.len());
        self.clauses.push(clause.replace('?', &placeholder));
    }

    fn sql(&self) -> String {
        if self.clauses.is_empty() {
            String::new()
        } else {
            format!("WHERE {}", self.clauses.join(" AND "))
        }
    }

    fn next(&self) -> usize {
        self.values.len() + 1
    }
}

/// Alarm types the DNS monitor raises (ADR-095 §6).
pub const DNS_ALARM_TYPES: [&str; 4] = [
    "dns_resolver_down",
    "dns_sync_stale",
    "dns_query_failures",
    "dns_internal_proxy_down",
];

impl DnsObservabilityService {
    /// Open (firing or acknowledged) DNS alarms, newest first.
    pub async fn open_dns_alarms(&self) -> Result<Vec<alarms::Model>, DnsObservabilityError> {
        alarms::Entity::find()
            .filter(alarms::Column::AlarmType.is_in(DNS_ALARM_TYPES))
            .filter(alarms::Column::Status.is_in(["firing", "acknowledged"]))
            .order_by_desc(alarms::Column::FiredAt)
            .limit(50)
            .all(self.db.as_ref())
            .await
            .map_err(DnsObservabilityError::db("listing open DNS alarms"))
    }

    /// Every node's last report, control plane first.
    pub async fn resolver_reports(
        &self,
    ) -> Result<Vec<dns_resolver_reports::Model>, DnsObservabilityError> {
        dns_resolver_reports::Entity::find()
            .order_by_asc(dns_resolver_reports::Column::NodeId)
            .all(self.db.as_ref())
            .await
            .map_err(DnsObservabilityError::db("listing DNS resolver reports"))
    }

    /// The query log, newest first.
    pub async fn query_log(
        &self,
        filter: &QueryLogFilter,
        page: u64,
        per_page: u64,
    ) -> Result<(Vec<QueryLogRow>, u64), DnsObservabilityError> {
        let mut conditions = Where::new();
        if let Some(node_id) = filter.node_id {
            conditions.push("q.node_id = ?", node_id);
        }
        if let Some(outcome) = filter.outcome {
            conditions.push("q.outcome = ?", outcome.as_str());
        }
        if let Some(zone) = filter.zone {
            conditions.push("q.zone = ?", zone.as_str());
        }
        if let Some(name) = filter.name.as_deref().filter(|n| !n.trim().is_empty()) {
            conditions.push("q.qname ILIKE ?", search_pattern(name, "name")?);
        }
        if let Some(project_id) = filter.project_id {
            conditions.push("q.project_id = ?", project_id);
        }
        if let Some(since) = filter.since {
            conditions.push("q.observed_at >= ?", since);
        }
        let where_sql = conditions.sql();

        let total = Total::find_by_statement(Statement::from_sql_and_values(
            DatabaseBackend::Postgres,
            format!("SELECT COUNT(*)::int8 AS total FROM dns_query_log q {where_sql}"),
            conditions.values.clone(),
        ))
        .one(self.db.as_ref())
        .await
        .map_err(DnsObservabilityError::db("counting DNS query log rows"))?
        .map_or(0, |t| u64::try_from(t.total).unwrap_or(0));

        let limit = conditions.next();
        let offset = limit + 1;
        let sql = format!(
            "SELECT q.id, q.node_id, n.name AS node_name, q.observed_at, q.qname, q.qtype, \
                    q.zone, q.outcome, q.latency_us, q.answer_count, q.source_ip, \
                    q.project_id, p.name AS project_name, \
                    q.environment_id, e.name AS environment_name, \
                    q.service_id, s.name AS service_name \
             FROM dns_query_log q \
             LEFT JOIN nodes n ON n.id = q.node_id \
             LEFT JOIN projects p ON p.id = q.project_id \
             LEFT JOIN environments e ON e.id = q.environment_id \
             LEFT JOIN external_services s ON s.id = q.service_id \
             {where_sql} \
             ORDER BY q.observed_at DESC, q.id DESC \
             LIMIT ${limit} OFFSET ${offset}"
        );
        let mut values = conditions.values;
        values.push(Value::from(i64::try_from(per_page).unwrap_or(i64::MAX)));
        values.push(Value::from(
            i64::try_from(page.saturating_sub(1).saturating_mul(per_page)).unwrap_or(i64::MAX),
        ));
        let rows = QueryLogRow::find_by_statement(Statement::from_sql_and_values(
            DatabaseBackend::Postgres,
            sql,
            values,
        ))
        .all(self.db.as_ref())
        .await
        .map_err(DnsObservabilityError::db("listing DNS query log rows"))?;
        Ok((rows, total))
    }

    /// The internal zone's records with their owners, newest first.
    pub async fn records(
        &self,
        filter: &RecordFilter,
        page: u64,
        per_page: u64,
    ) -> Result<(Vec<RecordRow>, u64), DnsObservabilityError> {
        let mut conditions = Where::new();
        if let Some(search) = filter.search.as_deref().filter(|s| !s.trim().is_empty()) {
            let pattern = search_pattern(search, "search")?;
            // One placeholder, used twice.
            conditions.push(
                "(lower(se.fqdn) LIKE ? OR coalesce(se.target_ip, '') LIKE ?)",
                pattern,
            );
        }
        if let Some(node_id) = filter.node_id {
            conditions.push("se.node_id = ?", node_id);
        }
        if let Some(kind) = filter.owner_kind.as_deref() {
            if !matches!(
                kind,
                "environment"
                    | "service_member"
                    | "service_role"
                    | "node"
                    | "static"
                    | "deployment"
            ) {
                return Err(DnsObservabilityError::InvalidFilter {
                    field: "owner_kind",
                    reason: format!("unknown owner kind {kind:?}"),
                });
            }
            conditions.push("se.owner_kind = ?", kind);
        }
        let where_sql = conditions.sql();
        let total = Total::find_by_statement(Statement::from_sql_and_values(
            DatabaseBackend::Postgres,
            format!("SELECT COUNT(*)::int8 AS total FROM service_endpoints se {where_sql}"),
            conditions.values.clone(),
        ))
        .one(self.db.as_ref())
        .await
        .map_err(DnsObservabilityError::db("counting internal DNS records"))?
        .map_or(0, |t| u64::try_from(t.total).unwrap_or(0));

        let limit = conditions.next();
        let offset = limit + 1;
        let sql = format!(
            "SELECT se.id, se.fqdn, se.record_type, se.target_ip, se.target_port, se.ttl, \
                    se.owner_kind, se.owner_id, se.node_id, n.name AS node_name, \
                    e.project_id AS project_id, p.name AS project_name, \
                    e.id AS environment_id, e.name AS environment_name, \
                    s.id AS service_id, s.name AS service_name, \
                    se.generation, se.created_at, se.updated_at \
             FROM service_endpoints se \
             LEFT JOIN nodes n ON n.id = se.node_id \
             LEFT JOIN environments e \
                    ON se.owner_kind = 'environment' AND e.id = se.owner_id \
             LEFT JOIN projects p ON p.id = e.project_id \
             LEFT JOIN service_members sm \
                    ON se.owner_kind = 'service_member' AND sm.id = se.owner_id \
             LEFT JOIN external_services s \
                    ON s.id = CASE se.owner_kind \
                                 WHEN 'service_member' THEN sm.service_id \
                                 WHEN 'service_role' THEN se.owner_id::int4 \
                              END \
             {where_sql} \
             ORDER BY se.created_at DESC, se.id DESC \
             LIMIT ${limit} OFFSET ${offset}"
        );
        let mut values = conditions.values;
        values.push(Value::from(i64::try_from(per_page).unwrap_or(i64::MAX)));
        values.push(Value::from(
            i64::try_from(page.saturating_sub(1).saturating_mul(per_page)).unwrap_or(i64::MAX),
        ));
        let rows = RecordRow::find_by_statement(Statement::from_sql_and_values(
            DatabaseBackend::Postgres,
            sql,
            values,
        ))
        .all(self.db.as_ref())
        .await
        .map_err(DnsObservabilityError::db("listing internal DNS records"))?;
        Ok((rows, total))
    }

    /// Queries per outcome on `node_id` between `from` and `to`, in
    /// buckets of `step`.
    pub async fn traffic(
        &self,
        node_id: i32,
        from: DateTime<Utc>,
        to: DateTime<Utc>,
        step: Duration,
    ) -> Result<TrafficSeries, DnsObservabilityError> {
        let names: Vec<(ZoneClass, Option<QueryOutcome>, String)> =
            [ZoneClass::Internal, ZoneClass::External]
                .into_iter()
                .flat_map(|zone| {
                    QueryOutcome::ALL
                        .into_iter()
                        .map(move |o| {
                            (
                                zone,
                                Some(o),
                                format!("dns.{}.{}", zone.as_str(), o.as_str()),
                            )
                        })
                        .chain(std::iter::once((
                            zone,
                            None,
                            format!("dns.{}.latency_us", zone.as_str()),
                        )))
                })
                .collect();
        let series = try_join_all(names.iter().map(|(_, _, name)| {
            self.metrics.query_range_filled(RangeQuery {
                source_kind: SourceKind::Node,
                source_id: node_id,
                name: name.clone(),
                from,
                to,
                step,
                monotonic: false,
            })
        }))
        .await
        .map_err(|e| DnsObservabilityError::Metrics {
            operation: "reading DNS traffic",
            reason: e.to_string(),
        })?;

        let mut buckets: BTreeMap<DateTime<Utc>, (TrafficBucket, f64)> = BTreeMap::new();
        for ((zone, outcome, _), points) in names.iter().zip(series) {
            for (time, value) in points {
                let (bucket, latency_sum) = buckets.entry(time).or_insert_with(|| {
                    (
                        TrafficBucket {
                            time,
                            ..Default::default()
                        },
                        0.0,
                    )
                });
                let value = value.unwrap_or(0.0);
                match (zone, outcome) {
                    (ZoneClass::Internal, Some(o)) => bucket.internal.set(*o, value),
                    (ZoneClass::External, Some(o)) => bucket.external.set(*o, value),
                    (ZoneClass::Internal, None) => *latency_sum = value,
                    (ZoneClass::External, None) => {}
                }
            }
        }
        let buckets = buckets
            .into_values()
            .map(|(mut bucket, latency_sum)| {
                let total = bucket.internal.total();
                bucket.internal_latency_us = (total > 0.0).then(|| latency_sum / total);
                bucket
            })
            .collect();
        Ok(TrafficSeries {
            node_id,
            from,
            to,
            step_secs: step.num_seconds(),
            buckets,
        })
    }

    /// Internal-zone queries per node since `since`.
    pub async fn internal_totals_since(
        &self,
        since: DateTime<Utc>,
    ) -> Result<Vec<(i32, i64)>, DnsObservabilityError> {
        let rows = NodeTotal::find_by_statement(Statement::from_sql_and_values(
            DatabaseBackend::Postgres,
            "SELECT node_id, COUNT(*)::int8 AS total FROM dns_query_log \
             WHERE zone = 'internal' AND observed_at >= $1 GROUP BY node_id",
            [Value::from(since)],
        ))
        .all(self.db.as_ref())
        .await
        .map_err(DnsObservabilityError::db(
            "counting recent internal DNS queries",
        ))?;
        Ok(rows.into_iter().map(|r| (r.node_id, r.total)).collect())
    }

    /// SERVFAIL and NXDOMAIN answers for internal names since `since`, per
    /// node and name.
    pub async fn internal_failures_since(
        &self,
        since: DateTime<Utc>,
    ) -> Result<Vec<FailureCount>, DnsObservabilityError> {
        FailureCount::find_by_statement(Statement::from_sql_and_values(
            DatabaseBackend::Postgres,
            "SELECT node_id, qname, outcome, COUNT(*)::int8 AS count FROM dns_query_log \
             WHERE zone = 'internal' AND outcome IN ('servfail', 'nxdomain') \
               AND observed_at >= $1 \
             GROUP BY node_id, qname, outcome \
             ORDER BY count DESC \
             LIMIT 1000",
            [Value::from(since)],
        ))
        .all(self.db.as_ref())
        .await
        .map_err(DnsObservabilityError::db(
            "counting recent internal DNS failures",
        ))
    }

    /// Names that should resolve: those in the zone, and those the control
    /// plane publishes for every environment and database, whether or not
    /// they are published right now. Lowercase, no trailing dot.
    pub async fn expected_names(&self) -> Result<HashSet<String>, DnsObservabilityError> {
        let rows = NameRow::find_by_statement(Statement::from_string(
            DatabaseBackend::Postgres,
            "WITH labels AS ( \
                 SELECT lower(s.slug) AS label FROM external_services s \
                  WHERE s.slug ~ '^[A-Za-z0-9-]+$' \
                 UNION SELECT lower(s.name) FROM external_services s \
                  WHERE s.name ~ '^[A-Za-z0-9-]+$' \
             ) \
             SELECT lower(fqdn) AS name FROM service_endpoints \
             UNION SELECT lower(e.slug || '.' || p.slug || '.temps.local') FROM environments e \
                    JOIN projects p ON p.id = e.project_id \
                    WHERE e.deleted_at IS NULL AND p.deleted_at IS NULL \
             UNION SELECT label || '.temps.local' FROM labels \
             UNION SELECT 'primary.' || label || '.temps.local' FROM labels \
             UNION SELECT 'replica.' || label || '.temps.local' FROM labels",
        ))
        .all(self.db.as_ref())
        .await
        .map_err(DnsObservabilityError::db(
            "listing expected internal DNS names",
        ))?;
        Ok(rows
            .into_iter()
            .filter_map(|r| r.name)
            .map(|n| n.trim_end_matches('.').to_string())
            .collect())
    }
}
