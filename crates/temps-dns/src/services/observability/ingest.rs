// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Storing one resolver report (ADR-095 §2–§4).
//!
//! A worker's report is untrusted input: [`validate_report`] bounds every
//! field before anything is stored, counters become deltas only against the
//! same resolver instance, and each log entry is stamped with the control
//! plane's receive time minus its age, clamped to the window since the last
//! pull, so a worker cannot date entries outside it.

use std::collections::{HashMap, HashSet};
use std::net::IpAddr;

use chrono::{DateTime, Duration, Utc};
use sea_orm::{
    ActiveModelTrait, ActiveValue::Set, ConnectionTrait, DatabaseBackend, EntityTrait,
    FromQueryResult, Statement, TransactionTrait, Value,
};
use temps_entities::{dns_query_log, dns_resolver_reports};
use temps_metrics::{MetricKind, MetricPoint, SourceKind};
use temps_node_dns::observe::{
    DnsCounters, DnsQueryEntry, DnsReport, QueryOutcome, ZoneClass, ZoneCounters,
    DNS_REPORT_VERSION, MAX_QUERY_NAME_LEN, QUERY_RING_CAPACITY,
};
use tracing::warn;

use super::{Baseline, DnsObservabilityError, DnsObservabilityService};

/// The most a report may weigh. A full ring of 512 entries is ~100 KiB.
pub const MAX_DNS_REPORT_BYTES: usize = 256 * 1024;

/// How far back the first pull of a new instance may date its entries.
const FIRST_PULL_WINDOW_MINUTES: i64 = 10;

/// Longest error text kept from a report.
const MAX_ERROR_LEN: usize = 1024;

/// Query-log rows written per statement.
const INSERT_CHUNK: usize = 500;

/// Every metric name ingest writes: `dns.<zone>.<outcome>` and
/// `dns.<zone>.latency_us`, all per-interval deltas.
pub const DNS_METRIC_NAMES: [&str; 14] = [
    "dns.internal.noerror",
    "dns.internal.nodata",
    "dns.internal.nxdomain",
    "dns.internal.servfail",
    "dns.internal.refused",
    "dns.internal.error",
    "dns.internal.latency_us",
    "dns.external.noerror",
    "dns.external.nodata",
    "dns.external.nxdomain",
    "dns.external.servfail",
    "dns.external.refused",
    "dns.external.error",
    "dns.external.latency_us",
];

/// What came of one report.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReportState {
    Ok,
    /// The agent predates `/agent/dns/report`.
    AgentTooOld,
    Unreachable,
    /// The agent refused the node token, or the token is unavailable.
    Rejected,
    /// The report could not be read or broke a bound.
    Invalid,
}

impl ReportState {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Ok => "ok",
            Self::AgentTooOld => "agent_too_old",
            Self::Unreachable => "unreachable",
            Self::Rejected => "rejected",
            Self::Invalid => "invalid",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        [
            Self::Ok,
            Self::AgentTooOld,
            Self::Unreachable,
            Self::Rejected,
            Self::Invalid,
        ]
        .into_iter()
        .find(|s| s.as_str() == value)
    }
}

/// One pull's result, as the collector hands it over.
#[derive(Debug, Clone)]
pub enum ReportOutcome {
    Report(Box<DnsReport>),
    Failed { state: ReportState, reason: String },
}

/// What [`DnsObservabilityService::ingest`] stored.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IngestSummary {
    pub state: ReportState,
    pub queries_logged: usize,
    pub points_written: usize,
    /// Why the report was refused, for an `Invalid` state.
    pub rejected: Option<String>,
}

fn truncate(value: &str, max: usize) -> String {
    let mut end = value.len().min(max);
    while !value.is_char_boundary(end) {
        end -= 1;
    }
    value[..end].to_string()
}

fn valid_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= MAX_QUERY_NAME_LEN
        && name
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'-' | b'_'))
}

/// Refuse a report that breaks a bound; the reason names the field.
pub fn validate_report(report: &DnsReport) -> Result<(), String> {
    if report.version != DNS_REPORT_VERSION {
        return Err(format!(
            "report version {} is not {DNS_REPORT_VERSION}",
            report.version
        ));
    }
    if report.instance_id.is_empty() || report.instance_id.len() > 64 {
        return Err("instance_id must be 1-64 characters".into());
    }
    if report.build_version.len() > 64 {
        return Err("build_version is longer than 64 characters".into());
    }
    if report.resolver.listen_addrs.len() > 8
        || report.resolver.listen_addrs.iter().any(|a| a.len() > 64)
    {
        return Err("listen_addrs has more than 8 entries or an entry over 64 characters".into());
    }
    if report.queries.len() > QUERY_RING_CAPACITY {
        return Err(format!(
            "{} query entries, more than the {QUERY_RING_CAPACITY} a resolver keeps",
            report.queries.len()
        ));
    }
    for entry in &report.queries {
        if let Some(name) = &entry.name {
            if !valid_name(name) {
                return Err(format!(
                    "query entry {} has an invalid name ({} bytes)",
                    entry.seq,
                    name.len()
                ));
            }
            if entry.zone == ZoneClass::External {
                return Err(format!(
                    "query entry {} carries a forwarded name, which is never reported",
                    entry.seq
                ));
            }
        }
        if entry.qtype.is_empty()
            || entry.qtype.len() > 16
            || !entry.qtype.bytes().all(|b| b.is_ascii_alphanumeric())
        {
            return Err(format!("query entry {} has an invalid type", entry.seq));
        }
    }
    Ok(())
}

fn zone_delta(current: &ZoneCounters, previous: &ZoneCounters) -> Option<ZoneCounters> {
    let sub = |a: u64, b: u64| a.checked_sub(b);
    let mut latency_buckets = [0u64; temps_node_dns::observe::LATENCY_BUCKETS];
    for (i, slot) in latency_buckets.iter_mut().enumerate() {
        *slot = sub(current.latency_buckets[i], previous.latency_buckets[i])?;
    }
    Some(ZoneCounters {
        noerror: sub(current.noerror, previous.noerror)?,
        nodata: sub(current.nodata, previous.nodata)?,
        nxdomain: sub(current.nxdomain, previous.nxdomain)?,
        servfail: sub(current.servfail, previous.servfail)?,
        refused: sub(current.refused, previous.refused)?,
        error: sub(current.error, previous.error)?,
        latency_us_sum: sub(current.latency_us_sum, previous.latency_us_sum)?,
        latency_buckets,
    })
}

/// `current - previous`, or `None` when any counter went backwards (the
/// resolver restarted without a new id, or the report is inconsistent).
pub(super) fn counters_delta(current: &DnsCounters, previous: &DnsCounters) -> Option<DnsCounters> {
    Some(DnsCounters {
        internal: zone_delta(&current.internal, &previous.internal)?,
        external: zone_delta(&current.external, &previous.external)?,
        dropped_log_entries: current
            .dropped_log_entries
            .checked_sub(previous.dropped_log_entries)?,
    })
}

/// The metric points for one interval's deltas, all stamped `at`.
pub fn metric_points(node_id: i32, delta: &DnsCounters, at: DateTime<Utc>) -> Vec<MetricPoint> {
    let mut points = Vec::with_capacity(DNS_METRIC_NAMES.len());
    for (zone, counters) in [
        (ZoneClass::Internal, &delta.internal),
        (ZoneClass::External, &delta.external),
    ] {
        let mut push = |suffix: &str, value: u64| {
            points.push(MetricPoint {
                time: at,
                source_kind: SourceKind::Node,
                source_id: node_id,
                name: format!("dns.{}.{suffix}", zone.as_str()),
                value: value as f64,
                kind: MetricKind::Counter,
                engine: None,
                environment: None,
                node_id: Some(node_id),
                labels: HashMap::new(),
            });
        };
        for outcome in QueryOutcome::ALL {
            push(outcome.as_str(), counters.get(outcome));
        }
        push("latency_us", counters.latency_us_sum);
    }
    points
}

/// When each entry was answered: receive time minus its age, kept inside
/// `[window_start, received_at]`.
pub(super) fn stamp(
    entry: &DnsQueryEntry,
    received_at: DateTime<Utc>,
    window_start: DateTime<Utc>,
) -> DateTime<Utc> {
    let age = Duration::milliseconds(i64::try_from(entry.age_ms).unwrap_or(i64::MAX));
    let at = received_at.checked_sub_signed(age).unwrap_or(window_start);
    at.clamp(window_start.min(received_at), received_at)
}

/// Who owns an address on a node, from the published endpoints.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(super) struct Attribution {
    pub project_id: Option<i32>,
    pub environment_id: Option<i32>,
    pub service_id: Option<i32>,
}

#[derive(Debug, FromQueryResult)]
struct AttributionRow {
    target_ip: String,
    environment_id: Option<i32>,
    project_id: Option<i32>,
    service_id: Option<i32>,
}

impl DnsObservabilityService {
    /// Store one resolver's report, or why there is none.
    pub async fn ingest(
        &self,
        node_id: i32,
        outcome: ReportOutcome,
        received_at: DateTime<Utc>,
    ) -> Result<IngestSummary, DnsObservabilityError> {
        let existing = dns_resolver_reports::Entity::find_by_id(node_id)
            .one(self.db.as_ref())
            .await
            .map_err(DnsObservabilityError::db("reading a DNS resolver report"))?;

        let report = match outcome {
            ReportOutcome::Report(report) => report,
            ReportOutcome::Failed { state, reason } => {
                self.record_failure(node_id, existing, state, &reason, received_at)
                    .await?;
                return Ok(IngestSummary {
                    state,
                    queries_logged: 0,
                    points_written: 0,
                    rejected: None,
                });
            }
        };
        if let Err(reason) = validate_report(&report) {
            self.record_failure(
                node_id,
                existing,
                ReportState::Invalid,
                &reason,
                received_at,
            )
            .await?;
            return Ok(IngestSummary {
                state: ReportState::Invalid,
                queries_logged: 0,
                points_written: 0,
                rejected: Some(reason),
            });
        }

        // Counters: a delta only against the same instance.
        let delta = {
            let mut baselines = self.baselines.lock().unwrap_or_else(|p| p.into_inner());
            let delta = baselines
                .get(&node_id)
                .filter(|b| b.instance_id == report.instance_id)
                .and_then(|b| counters_delta(&report.counters, &b.counters));
            baselines.insert(
                node_id,
                Baseline {
                    instance_id: report.instance_id.clone(),
                    counters: report.counters.clone(),
                },
            );
            delta
        };

        // Query log: entries past the stored cursor of the same instance.
        let same_instance = existing
            .as_ref()
            .is_some_and(|row| row.instance_id.as_deref() == Some(report.instance_id.as_str()));
        let cursor = existing
            .as_ref()
            .filter(|_| same_instance)
            .and_then(|row| row.query_cursor)
            .and_then(|c| u64::try_from(c).ok());
        let window_start = existing
            .as_ref()
            .filter(|_| same_instance)
            .and_then(|row| row.last_ok_at)
            .filter(|at| *at <= received_at)
            .unwrap_or_else(|| received_at - Duration::minutes(FIRST_PULL_WINDOW_MINUTES));
        let mut seen = HashSet::new();
        let entries: Vec<&DnsQueryEntry> = report
            .queries
            .iter()
            .filter(|e| cursor.is_none_or(|c| e.seq > c))
            .filter(|e| seen.insert(e.seq))
            .collect();
        let new_cursor = entries
            .iter()
            .map(|e| e.seq)
            .max()
            .or(cursor)
            .and_then(|c| i64::try_from(c).ok());

        let attributions = self.attribute(node_id, &entries).await?;

        let txn = self.db.begin().await.map_err(DnsObservabilityError::db(
            "starting a DNS report transaction",
        ))?;
        let rows: Vec<dns_query_log::ActiveModel> = entries
            .iter()
            .map(|entry| {
                let source = entry.source.to_string();
                let owner = attributions.get(&source).copied().unwrap_or_default();
                dns_query_log::ActiveModel {
                    node_id: Set(node_id),
                    observed_at: Set(stamp(entry, received_at, window_start)),
                    qname: Set(entry.name.clone()),
                    qtype: Set(entry.qtype.clone()),
                    zone: Set(entry.zone.as_str().to_string()),
                    outcome: Set(entry.outcome.as_str().to_string()),
                    latency_us: Set(i64::try_from(entry.latency_us).unwrap_or(i64::MAX)),
                    answer_count: Set(i32::from(entry.answers)),
                    source_ip: Set(source),
                    project_id: Set(owner.project_id),
                    environment_id: Set(owner.environment_id),
                    service_id: Set(owner.service_id),
                    created_at: Set(received_at),
                    ..Default::default()
                }
            })
            .collect();
        let queries_logged = rows.len();
        let mut rows = rows.into_iter().peekable();
        while rows.peek().is_some() {
            let chunk: Vec<_> = rows.by_ref().take(INSERT_CHUNK).collect();
            dns_query_log::Entity::insert_many(chunk)
                .exec(&txn)
                .await
                .map_err(DnsObservabilityError::db("storing DNS query log entries"))?;
        }

        let resolver = &report.resolver;
        let mut row = base_row(node_id, existing);
        row.report_state = Set(ReportState::Ok.as_str().to_string());
        row.reported_at = Set(received_at);
        row.last_ok_at = Set(Some(received_at));
        row.instance_id = Set(Some(report.instance_id.clone()));
        row.build_version = Set(Some(report.build_version.clone()));
        row.running = Set(resolver.running);
        row.tasks_alive = Set(resolver.tasks_alive);
        row.listen_address =
            Set((!resolver.listen_addrs.is_empty()).then(|| resolver.listen_addrs.join(", ")));
        row.zone_generation = Set(resolver.running.then_some(resolver.zone_generation));
        row.record_count = Set(resolver
            .running
            .then(|| i64::try_from(resolver.record_count).unwrap_or(i64::MAX)));
        row.last_sync_age_secs = Set(resolver
            .last_sync_age_secs
            .map(|s| i64::try_from(s).unwrap_or(i64::MAX)));
        row.consecutive_sync_failures =
            Set(i32::try_from(resolver.consecutive_sync_failures).unwrap_or(i32::MAX));
        row.last_error = Set(resolver
            .last_error
            .as_deref()
            .map(|e| truncate(e, MAX_ERROR_LEN)));
        row.query_cursor = Set(new_cursor);
        row.dropped_log_entries =
            Set(i64::try_from(report.counters.dropped_log_entries).unwrap_or(i64::MAX));
        save(row, &txn).await?;
        txn.commit()
            .await
            .map_err(DnsObservabilityError::db("committing a DNS report"))?;

        // Counters last: a lost write only loses this interval's points.
        let points = delta
            .map(|d| metric_points(node_id, &d, received_at))
            .unwrap_or_default();
        let points_written = points.len();
        if !points.is_empty() {
            if let Err(e) = self.metrics.write_batch(points).await {
                warn!(node_id, "DNS counters not stored for this interval: {e}");
                return Ok(IngestSummary {
                    state: ReportState::Ok,
                    queries_logged,
                    points_written: 0,
                    rejected: None,
                });
            }
        }
        Ok(IngestSummary {
            state: ReportState::Ok,
            queries_logged,
            points_written,
            rejected: None,
        })
    }

    async fn record_failure(
        &self,
        node_id: i32,
        existing: Option<dns_resolver_reports::Model>,
        state: ReportState,
        reason: &str,
        received_at: DateTime<Utc>,
    ) -> Result<(), DnsObservabilityError> {
        let mut row = base_row(node_id, existing);
        row.report_state = Set(state.as_str().to_string());
        row.reported_at = Set(received_at);
        row.last_error = Set(Some(truncate(reason, MAX_ERROR_LEN)));
        save(row, self.db.as_ref()).await
    }

    /// Owners of the source addresses among `entries`, by address: the
    /// endpoint on this node (or unpinned) published for it.
    async fn attribute(
        &self,
        node_id: i32,
        entries: &[&DnsQueryEntry],
    ) -> Result<HashMap<String, Attribution>, DnsObservabilityError> {
        let sources: Vec<String> = entries
            .iter()
            .map(|e| e.source)
            .collect::<HashSet<IpAddr>>()
            .into_iter()
            .map(|ip| ip.to_string())
            .collect();
        if sources.is_empty() {
            return Ok(HashMap::new());
        }
        let placeholders: Vec<String> = (0..sources.len()).map(|i| format!("${}", i + 2)).collect();
        let sql = format!(
            "SELECT se.target_ip, \
                    e.id AS environment_id, \
                    e.project_id AS project_id, \
                    CASE se.owner_kind \
                        WHEN 'service_member' THEN sm.service_id \
                        WHEN 'service_role' THEN se.owner_id::int4 \
                    END AS service_id \
             FROM service_endpoints se \
             LEFT JOIN environments e \
                    ON se.owner_kind = 'environment' AND e.id = se.owner_id \
             LEFT JOIN service_members sm \
                    ON se.owner_kind = 'service_member' AND sm.id = se.owner_id \
             WHERE se.target_ip IN ({}) \
               AND (se.node_id = $1 OR se.node_id IS NULL) \
               AND se.owner_kind IN ('environment', 'service_member', 'service_role') \
             ORDER BY se.updated_at DESC",
            placeholders.join(", ")
        );
        let mut values: Vec<Value> = vec![node_id.into()];
        values.extend(sources.into_iter().map(Value::from));
        let rows = AttributionRow::find_by_statement(Statement::from_sql_and_values(
            DatabaseBackend::Postgres,
            sql,
            values,
        ))
        .all(self.db.as_ref())
        .await
        .map_err(DnsObservabilityError::db("attributing DNS query sources"))?;
        let mut by_ip = HashMap::new();
        for row in rows {
            // Newest endpoint first: the first row per address wins.
            by_ip.entry(row.target_ip).or_insert(Attribution {
                project_id: row.project_id,
                environment_id: row.environment_id,
                service_id: row.service_id,
            });
        }
        Ok(by_ip)
    }

    /// Delete query-log rows past the retention, then the oldest past the
    /// row cap. Returns how many went.
    pub async fn prune_query_log(&self) -> Result<u64, DnsObservabilityError> {
        let aged = self
            .db
            .execute(Statement::from_sql_and_values(
                DatabaseBackend::Postgres,
                "DELETE FROM dns_query_log WHERE observed_at < now() - make_interval(hours => $1)",
                [Value::from(super::DNS_QUERY_LOG_RETENTION_HOURS as i32)],
            ))
            .await
            .map_err(DnsObservabilityError::db("pruning aged DNS query log rows"))?
            .rows_affected();
        let capped = self
            .db
            .execute(Statement::from_sql_and_values(
                DatabaseBackend::Postgres,
                "DELETE FROM dns_query_log WHERE id < \
                 (SELECT id FROM dns_query_log ORDER BY id DESC OFFSET $1 LIMIT 1)",
                [Value::from(
                    i64::try_from(super::DNS_QUERY_LOG_MAX_ROWS).unwrap_or(i64::MAX) - 1,
                )],
            ))
            .await
            .map_err(DnsObservabilityError::db("capping the DNS query log"))?
            .rows_affected();
        Ok(aged + capped)
    }

    /// Store how the control plane serves `*.temps.local` HTTP calls to its
    /// own containers (ADR-104 D4) on node 0's report row, which
    /// [`Self::ingest`] wrote just before. `None` clears it: the control
    /// plane does not run the proxy (cluster DNS is off).
    pub async fn record_control_plane_internal_proxy(
        &self,
        internal_proxy: Option<serde_json::Value>,
    ) -> Result<(), DnsObservabilityError> {
        self.db
            .execute(Statement::from_sql_and_values(
                DatabaseBackend::Postgres,
                "UPDATE dns_resolver_reports SET internal_proxy = $1 WHERE node_id = $2",
                [
                    Value::Json(internal_proxy.map(Box::new)),
                    Value::from(super::CONTROL_PLANE_NODE_ID),
                ],
            ))
            .await
            .map_err(DnsObservabilityError::db(
                "storing the control plane's internal routing state",
            ))?;
        Ok(())
    }

    /// Forget a removed node's report row.
    pub async fn forget_node(&self, node_id: i32) -> Result<(), DnsObservabilityError> {
        dns_resolver_reports::Entity::delete_by_id(node_id)
            .exec(self.db.as_ref())
            .await
            .map_err(DnsObservabilityError::db("deleting a DNS resolver report"))?;
        self.baselines
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .remove(&node_id);
        Ok(())
    }
}

/// The row to write: the existing one, or a new one with defaults.
fn base_row(
    node_id: i32,
    existing: Option<dns_resolver_reports::Model>,
) -> dns_resolver_reports::ActiveModel {
    match existing {
        Some(model) => model.into(),
        None => dns_resolver_reports::ActiveModel {
            node_id: Set(node_id),
            running: Set(false),
            tasks_alive: Set(false),
            consecutive_sync_failures: Set(0),
            dropped_log_entries: Set(0),
            last_ok_at: Set(None),
            instance_id: Set(None),
            build_version: Set(None),
            listen_address: Set(None),
            zone_generation: Set(None),
            record_count: Set(None),
            last_sync_age_secs: Set(None),
            last_error: Set(None),
            query_cursor: Set(None),
            internal_proxy: Set(None),
            ..Default::default()
        },
    }
}

async fn save<C: ConnectionTrait>(
    row: dns_resolver_reports::ActiveModel,
    db: &C,
) -> Result<(), DnsObservabilityError> {
    // A new row has its key Set but was never stored: insert it.
    let is_new = matches!(row.created_at, sea_orm::ActiveValue::NotSet);
    if is_new {
        row.insert(db)
            .await
            .map_err(DnsObservabilityError::db("storing a DNS resolver report"))?;
    } else {
        row.update(db)
            .await
            .map_err(DnsObservabilityError::db("updating a DNS resolver report"))?;
    }
    Ok(())
}
