// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Internal DNS observability for the console's Settings → DNS page
//! (ADR-095 §5): resolver health per node, the zone's records, the query
//! log, traffic over time and test resolution.
//!
//! All are `SettingsRead`, like `GET /cluster/dns/status`: infrastructure
//! visibility for instance administrators. The test resolution is a read
//! too; it changes nothing and is not audit-logged, but it is logged.

use std::collections::HashMap;
use std::sync::Arc;

use axum::extract::{Query, State};
use axum::response::IntoResponse;
use axum::Json;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use temps_auth::{permission_guard, RequireAuth};
use temps_core::problemdetails::{self, Problem};
use temps_dns::services::observability::{
    QueryLogFilter, QueryLogRow, RecordFilter, RecordRow, TrafficBucket, CONTROL_PLANE_NODE_ID,
};
use temps_node_dns::observe::{QueryOutcome, ZoneClass};
use temps_node_dns::resolve::ResolvedRecord;
use tracing::info;
use utoipa::{IntoParams, OpenApi, ToSchema};

use crate::handlers::types::NodeAdminState;
use crate::services::cluster_dns::{
    dns_nodes, resolver_health, ClusterDnsError, DnsNode, HealthInput, InternalProxyState,
    ResolverHealth, TestResolution,
};

const CONTROL_PLANE_NAME: &str = "control plane";

fn page_params(page: Option<u64>, page_size: Option<u64>) -> (u64, u64) {
    (
        page.unwrap_or(1).clamp(1, temps_core::config::MAX_PAGE),
        page_size.unwrap_or(20).clamp(1, 100),
    )
}

impl From<ClusterDnsError> for Problem {
    fn from(error: ClusterDnsError) -> Self {
        use axum::http::StatusCode;
        let (status, title) = match &error {
            ClusterDnsError::NodeNotFound { .. } => (StatusCode::NOT_FOUND, "Node not found"),
            ClusterDnsError::NoProjectContainer { .. } => (
                StatusCode::CONFLICT,
                "No container of that project on this node",
            ),
            ClusterDnsError::NodeUnavailable { .. } => (StatusCode::CONFLICT, "Node unavailable"),
            ClusterDnsError::Invalid { .. } => (StatusCode::BAD_REQUEST, "Invalid test resolution"),
            ClusterDnsError::ResolverNotRunning { .. } => {
                (StatusCode::CONFLICT, "DNS resolver not running")
            }
            ClusterDnsError::AgentTooOld { .. } => (StatusCode::CONFLICT, "Agent too old"),
            ClusterDnsError::Unreachable { .. } => {
                (StatusCode::BAD_GATEWAY, "DNS resolver unreachable")
            }
            ClusterDnsError::Database { .. } => (
                StatusCode::INTERNAL_SERVER_ERROR,
                "DNS observability database error",
            ),
            ClusterDnsError::Observability(_) => {
                (StatusCode::INTERNAL_SERVER_ERROR, "DNS observability error")
            }
        };
        problemdetails::new(status)
            .with_title(title)
            .with_detail(error.to_string())
    }
}

// ── Resolvers ────────────────────────────────────────────────────────────────

/// One node's resolver, as last reported.
#[derive(Debug, Serialize, ToSchema)]
pub struct DnsResolverEntry {
    /// 0 is the control plane.
    pub node_id: i32,
    pub node_name: String,
    /// `active`, `draining`, `offline`...; `active` for the control plane.
    pub node_status: String,
    pub is_control_plane: bool,
    pub health: ResolverHealth,
    /// Why the resolver is not healthy.
    pub health_reason: Option<String>,
    /// `ok`, `agent_too_old`, `unreachable`, `rejected` or `invalid`;
    /// `None` before the first report.
    pub report_state: Option<String>,
    pub reported_at: Option<DateTime<Utc>>,
    pub last_ok_at: Option<DateTime<Utc>>,
    pub build_version: Option<String>,
    pub running: bool,
    pub tasks_alive: bool,
    pub listen_address: Option<String>,
    pub zone_generation: Option<i64>,
    /// Records the resolver serves.
    pub record_count: Option<i64>,
    /// Seconds since the zone last synced; `None` for the control plane,
    /// whose zone is fed locally.
    pub last_sync_age_secs: Option<i64>,
    pub consecutive_sync_failures: i32,
    pub last_error: Option<String>,
    /// Query-log entries the resolver dropped under load.
    pub dropped_log_entries: i64,
    /// The worker's internal edge proxy, which serves the HTTP behind every
    /// `<env>.<project>.temps.local` name on that node. `None` for the
    /// control plane, whose main proxy does this.
    pub internal_proxy: Option<InternalProxyState>,
}

/// An open DNS alarm.
#[derive(Debug, Serialize, ToSchema)]
pub struct DnsAlertEntry {
    pub id: i32,
    /// `dns_resolver_down`, `dns_sync_stale`, `dns_query_failures` or
    /// `dns_internal_proxy_down`.
    pub alarm_type: String,
    pub severity: String,
    pub status: String,
    pub title: String,
    pub message: Option<String>,
    pub node_id: Option<i32>,
    pub fired_at: DateTime<Utc>,
}

/// `GET /cluster/dns/resolvers`.
#[derive(Debug, Serialize, ToSchema)]
pub struct ClusterDnsResolversResponse {
    pub cluster_dns_enabled: bool,
    /// Records in the zone (`service_endpoints`).
    pub zone_record_count: i64,
    /// How often resolvers report, in seconds.
    pub report_interval_secs: u64,
    /// Control plane first, then workers by id.
    pub resolvers: Vec<DnsResolverEntry>,
    pub open_alerts: Vec<DnsAlertEntry>,
}

/// Each node's resolver: health, version, records served, last sync, and
/// the open DNS alarms.
#[utoipa::path(
    tag = "Nodes",
    get,
    path = "/cluster/dns/resolvers",
    responses(
        (status = 200, description = "Resolver health per node", body = ClusterDnsResolversResponse),
        (status = 401, description = "Unauthorized"),
        (status = 403, description = "Insufficient permissions"),
        (status = 500, description = "Internal server error")
    ),
    security(("bearer_auth" = []))
)]
pub async fn list_dns_resolvers(
    RequireAuth(auth): RequireAuth,
    State(state): State<Arc<NodeAdminState>>,
) -> Result<impl IntoResponse, Problem> {
    permission_guard!(auth, SettingsRead);
    let cluster_dns_enabled = state
        .config_service
        .get_settings()
        .await
        .map(|s| s.cluster_dns.enabled)
        .unwrap_or(false);
    let interval = state.cluster_dns.interval_secs().await;
    let observability = state.cluster_dns.observability();
    let reports: HashMap<i32, temps_entities::dns_resolver_reports::Model> = observability
        .resolver_reports()
        .await?
        .into_iter()
        .map(|r| (r.node_id, r))
        .collect();
    let nodes = state.node_service.list_all().await.map_err(Problem::from)?;
    let zone_record_count = state
        .node_service
        .total_dns_record_count()
        .await
        .map_err(Problem::from)?;
    let now = Utc::now();
    let interval_i64 = i64::try_from(interval).unwrap_or(i64::MAX);

    let rows = dns_nodes(CONTROL_PLANE_NAME, nodes);
    let resolvers = rows
        .into_iter()
        .map(
            |DnsNode {
                 node_id,
                 node_name,
                 node_status,
                 heartbeat_running,
                 internal_proxy,
             }| {
                let report = reports.get(&node_id);
                let (health, health_reason) = resolver_health(&HealthInput {
                    node_id,
                    cluster_dns_enabled,
                    node_status: &node_status,
                    report,
                    heartbeat_running,
                    interval_secs: interval_i64,
                    now,
                });
                DnsResolverEntry {
                    node_id,
                    is_control_plane: node_id == CONTROL_PLANE_NODE_ID,
                    node_name,
                    node_status,
                    health,
                    health_reason,
                    report_state: report.map(|r| r.report_state.clone()),
                    reported_at: report.map(|r| r.reported_at),
                    last_ok_at: report.and_then(|r| r.last_ok_at),
                    build_version: report.and_then(|r| r.build_version.clone()),
                    running: report.is_some_and(|r| r.running),
                    tasks_alive: report.is_some_and(|r| r.tasks_alive),
                    listen_address: report.and_then(|r| r.listen_address.clone()),
                    zone_generation: report.and_then(|r| r.zone_generation),
                    record_count: report.and_then(|r| r.record_count),
                    last_sync_age_secs: report.and_then(|r| r.last_sync_age_secs),
                    consecutive_sync_failures: report.map_or(0, |r| r.consecutive_sync_failures),
                    last_error: report.and_then(|r| r.last_error.clone()),
                    dropped_log_entries: report.map_or(0, |r| r.dropped_log_entries),
                    internal_proxy,
                }
            },
        )
        .collect();
    let open_alerts = observability
        .open_dns_alarms()
        .await?
        .into_iter()
        .map(|a| DnsAlertEntry {
            node_id: a
                .metadata
                .as_ref()
                .and_then(|m| m.get("node_id"))
                .and_then(|v| v.as_i64())
                .and_then(|v| i32::try_from(v).ok()),
            id: a.id,
            alarm_type: a.alarm_type,
            severity: a.severity,
            status: a.status,
            title: a.title,
            message: a.message,
            fired_at: a.fired_at,
        })
        .collect();
    Ok(Json(ClusterDnsResolversResponse {
        cluster_dns_enabled,
        zone_record_count,
        report_interval_secs: interval,
        resolvers,
        open_alerts,
    }))
}

// ── Records ──────────────────────────────────────────────────────────────────

#[derive(Debug, Deserialize, IntoParams)]
#[into_params(parameter_in = Query)]
pub struct DnsRecordsQuery {
    /// Page number (1-indexed).
    pub page: Option<u64>,
    /// Records per page, at most 100. Defaults to 20.
    pub page_size: Option<u64>,
    /// Substring of the name or target address.
    pub search: Option<String>,
    /// Records pinned to this node.
    pub node_id: Option<i32>,
    /// `environment`, `service_member`, `service_role`, `node` or `static`.
    pub owner_kind: Option<String>,
}

/// One record of the internal zone.
#[derive(Debug, Serialize, ToSchema)]
pub struct DnsRecordEntry {
    pub id: i64,
    pub fqdn: String,
    pub record_type: String,
    pub target_ip: Option<String>,
    pub target_port: Option<i32>,
    pub ttl: i32,
    /// What published it: `environment`, `service_member`, `service_role`,
    /// `node`, `static`.
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

impl From<RecordRow> for DnsRecordEntry {
    fn from(r: RecordRow) -> Self {
        Self {
            id: r.id,
            fqdn: r.fqdn,
            record_type: r.record_type,
            target_ip: r.target_ip,
            target_port: r.target_port,
            ttl: r.ttl,
            owner_kind: r.owner_kind,
            owner_id: r.owner_id,
            node_name: r.node_name,
            node_id: r.node_id,
            project_id: r.project_id,
            project_name: r.project_name,
            environment_id: r.environment_id,
            environment_name: r.environment_name,
            service_id: r.service_id,
            service_name: r.service_name,
            generation: r.generation,
            created_at: r.created_at,
            updated_at: r.updated_at,
        }
    }
}

#[derive(Debug, Serialize, ToSchema)]
pub struct DnsRecordListResponse {
    pub items: Vec<DnsRecordEntry>,
    pub total: u64,
    pub page: u64,
    pub page_size: u64,
}

/// The internal zone's records with their owners, newest first.
#[utoipa::path(
    tag = "Nodes",
    get,
    path = "/cluster/dns/records",
    params(DnsRecordsQuery),
    responses(
        (status = 200, description = "Records of the internal zone", body = DnsRecordListResponse),
        (status = 400, description = "Invalid filter"),
        (status = 401, description = "Unauthorized"),
        (status = 403, description = "Insufficient permissions"),
        (status = 500, description = "Internal server error")
    ),
    security(("bearer_auth" = []))
)]
pub async fn list_dns_records(
    RequireAuth(auth): RequireAuth,
    State(state): State<Arc<NodeAdminState>>,
    Query(query): Query<DnsRecordsQuery>,
) -> Result<impl IntoResponse, Problem> {
    permission_guard!(auth, SettingsRead);
    let (page, page_size) = page_params(query.page, query.page_size);
    let (rows, total) = state
        .cluster_dns
        .observability()
        .records(
            &RecordFilter {
                search: query.search,
                node_id: query.node_id,
                owner_kind: query.owner_kind,
            },
            page,
            page_size,
        )
        .await?;
    Ok(Json(DnsRecordListResponse {
        items: rows.into_iter().map(DnsRecordEntry::from).collect(),
        total,
        page,
        page_size,
    }))
}

// ── Query log ────────────────────────────────────────────────────────────────

#[derive(Debug, Deserialize, IntoParams)]
#[into_params(parameter_in = Query)]
pub struct DnsQueriesQuery {
    /// Page number (1-indexed).
    pub page: Option<u64>,
    /// Entries per page, at most 100. Defaults to 20.
    pub page_size: Option<u64>,
    /// Queries answered by this node's resolver (0 is the control plane).
    pub node_id: Option<i32>,
    /// `noerror`, `nodata`, `nxdomain`, `servfail`, `refused` or `error`.
    pub outcome: Option<String>,
    /// `internal` or `external`.
    pub zone: Option<String>,
    /// Substring of the name asked.
    pub name: Option<String>,
    /// Queries from this project's containers.
    pub project_id: Option<i32>,
}

/// One logged query.
#[derive(Debug, Serialize, ToSchema)]
pub struct DnsQueryLogEntry {
    pub id: i64,
    pub node_id: i32,
    pub node_name: String,
    pub observed_at: DateTime<Utc>,
    /// The name asked; `None` for a forwarded (outside-zone) name, which is
    /// never stored.
    pub qname: Option<String>,
    pub qtype: String,
    /// `internal` or `external`.
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

impl From<QueryLogRow> for DnsQueryLogEntry {
    fn from(r: QueryLogRow) -> Self {
        Self {
            node_name: if r.node_id == CONTROL_PLANE_NODE_ID {
                CONTROL_PLANE_NAME.to_string()
            } else {
                r.node_name.unwrap_or_else(|| format!("node {}", r.node_id))
            },
            id: r.id,
            node_id: r.node_id,
            observed_at: r.observed_at,
            qname: r.qname,
            qtype: r.qtype,
            zone: r.zone,
            outcome: r.outcome,
            latency_us: r.latency_us,
            answer_count: r.answer_count,
            source_ip: r.source_ip,
            project_id: r.project_id,
            project_name: r.project_name,
            environment_id: r.environment_id,
            environment_name: r.environment_name,
            service_id: r.service_id,
            service_name: r.service_name,
        }
    }
}

#[derive(Debug, Serialize, ToSchema)]
pub struct DnsQueryLogResponse {
    pub items: Vec<DnsQueryLogEntry>,
    pub total: u64,
    pub page: u64,
    pub page_size: u64,
    /// How long entries are kept, in hours.
    pub retention_hours: i64,
}

fn bad_filter(field: &str, value: &str, allowed: &str) -> Problem {
    problemdetails::new(axum::http::StatusCode::BAD_REQUEST)
        .with_title("Invalid filter")
        .with_detail(format!(
            "Unknown {field} {value:?}; expected one of {allowed}"
        ))
}

/// Recent queries answered by the internal resolvers, newest first. Kept
/// for 24 hours; forwarded names are never stored.
#[utoipa::path(
    tag = "Nodes",
    get,
    path = "/cluster/dns/queries",
    params(DnsQueriesQuery),
    responses(
        (status = 200, description = "Recent DNS queries", body = DnsQueryLogResponse),
        (status = 400, description = "Invalid filter"),
        (status = 401, description = "Unauthorized"),
        (status = 403, description = "Insufficient permissions"),
        (status = 500, description = "Internal server error")
    ),
    security(("bearer_auth" = []))
)]
pub async fn list_dns_queries(
    RequireAuth(auth): RequireAuth,
    State(state): State<Arc<NodeAdminState>>,
    Query(query): Query<DnsQueriesQuery>,
) -> Result<impl IntoResponse, Problem> {
    permission_guard!(auth, SettingsRead);
    let (page, page_size) = page_params(query.page, query.page_size);
    let outcome = match query.outcome.as_deref().filter(|o| !o.is_empty()) {
        None => None,
        Some(value) => Some(QueryOutcome::parse(value).ok_or_else(|| {
            bad_filter(
                "outcome",
                value,
                "noerror, nodata, nxdomain, servfail, refused, error",
            )
        })?),
    };
    let zone = match query.zone.as_deref().filter(|z| !z.is_empty()) {
        None => None,
        Some(value) => Some(
            ZoneClass::parse(value)
                .ok_or_else(|| bad_filter("zone", value, "internal, external"))?,
        ),
    };
    let (rows, total) = state
        .cluster_dns
        .observability()
        .query_log(
            &QueryLogFilter {
                node_id: query.node_id,
                outcome,
                zone,
                name: query.name,
                project_id: query.project_id,
                since: None,
            },
            page,
            page_size,
        )
        .await?;
    Ok(Json(DnsQueryLogResponse {
        items: rows.into_iter().map(DnsQueryLogEntry::from).collect(),
        total,
        page,
        page_size,
        retention_hours: temps_dns::services::observability::DNS_QUERY_LOG_RETENTION_HOURS,
    }))
}

// ── Traffic ──────────────────────────────────────────────────────────────────

#[derive(Debug, Deserialize, IntoParams)]
#[into_params(parameter_in = Query)]
pub struct DnsTrafficQuery {
    /// The node whose resolver to chart; 0 (the default) is the control
    /// plane.
    pub node_id: Option<i32>,
    /// `1h` (default), `6h`, `24h` or `7d`.
    pub range: Option<String>,
}

/// Queries per outcome in one bucket.
#[derive(Debug, Serialize, ToSchema)]
pub struct DnsOutcomeCounts {
    pub noerror: f64,
    pub nodata: f64,
    pub nxdomain: f64,
    pub servfail: f64,
    pub refused: f64,
    pub error: f64,
}

impl From<&temps_dns::services::observability::OutcomeCounts> for DnsOutcomeCounts {
    fn from(c: &temps_dns::services::observability::OutcomeCounts) -> Self {
        Self {
            noerror: c.noerror,
            nodata: c.nodata,
            nxdomain: c.nxdomain,
            servfail: c.servfail,
            refused: c.refused,
            error: c.error,
        }
    }
}

#[derive(Debug, Serialize, ToSchema)]
pub struct DnsTrafficBucket {
    pub time: DateTime<Utc>,
    /// `*.temps.local` queries.
    pub internal: DnsOutcomeCounts,
    /// Forwarded queries.
    pub external: DnsOutcomeCounts,
    /// Mean latency of internal queries, in milliseconds.
    pub internal_latency_ms: Option<f64>,
}

impl From<&TrafficBucket> for DnsTrafficBucket {
    fn from(b: &TrafficBucket) -> Self {
        Self {
            time: b.time,
            internal: (&b.internal).into(),
            external: (&b.external).into(),
            internal_latency_ms: b.internal_latency_us.map(|us| us / 1000.0),
        }
    }
}

#[derive(Debug, Serialize, ToSchema)]
pub struct DnsTrafficResponse {
    pub node_id: i32,
    pub range: String,
    pub from: DateTime<Utc>,
    pub to: DateTime<Utc>,
    pub step_secs: i64,
    pub buckets: Vec<DnsTrafficBucket>,
}

/// Queries per outcome over time for one node's resolver.
#[utoipa::path(
    tag = "Nodes",
    get,
    path = "/cluster/dns/traffic",
    params(DnsTrafficQuery),
    responses(
        (status = 200, description = "DNS traffic over time", body = DnsTrafficResponse),
        (status = 400, description = "Invalid range"),
        (status = 401, description = "Unauthorized"),
        (status = 403, description = "Insufficient permissions"),
        (status = 500, description = "Internal server error")
    ),
    security(("bearer_auth" = []))
)]
pub async fn dns_traffic(
    RequireAuth(auth): RequireAuth,
    State(state): State<Arc<NodeAdminState>>,
    Query(query): Query<DnsTrafficQuery>,
) -> Result<impl IntoResponse, Problem> {
    permission_guard!(auth, SettingsRead);
    let range = query.range.unwrap_or_else(|| "1h".to_string());
    if !matches!(range.as_str(), "1h" | "6h" | "24h" | "7d") {
        return Err(bad_filter("range", &range, "1h, 6h, 24h, 7d"));
    }
    let node_id = query.node_id.unwrap_or(CONTROL_PLANE_NODE_ID);
    let (window, step) = temps_metrics::range_to_step(&range);
    let to = Utc::now();
    let from = to - window;
    let series = state
        .cluster_dns
        .observability()
        .traffic(node_id, from, to, step)
        .await?;
    Ok(Json(DnsTrafficResponse {
        node_id,
        range,
        from,
        to,
        step_secs: series.step_secs,
        buckets: series.buckets.iter().map(DnsTrafficBucket::from).collect(),
    }))
}

// ── Test resolution ──────────────────────────────────────────────────────────

/// `POST /cluster/dns/resolve`.
#[derive(Debug, Deserialize, ToSchema)]
pub struct DnsResolveRequest {
    /// The node whose resolver answers; 0 is the control plane.
    pub node_id: i32,
    /// The name, e.g. `production.shop.temps.local`.
    pub name: String,
    /// `A` (default), `AAAA`, `CNAME`, `SRV` or `TXT`.
    pub record_type: Option<String>,
    /// Answer as one of this project's containers on that node would be
    /// answered, its network policy included.
    pub project_id: Option<i32>,
}

/// One answer record.
#[derive(Debug, Serialize, ToSchema)]
pub struct DnsResolvedRecord {
    pub name: String,
    pub record_type: String,
    pub ttl: u32,
    pub data: String,
}

impl From<ResolvedRecord> for DnsResolvedRecord {
    fn from(r: ResolvedRecord) -> Self {
        Self {
            name: r.name,
            record_type: r.record_type,
            ttl: r.ttl,
            data: r.data,
        }
    }
}

#[derive(Debug, Serialize, ToSchema)]
pub struct DnsResolveResponse {
    pub node_id: i32,
    pub name: String,
    pub record_type: String,
    /// `NOERROR`, `NXDOMAIN`, `SERVFAIL`...
    pub rcode: String,
    /// `noerror`, `nodata`, `nxdomain`, `servfail`, `error`.
    pub outcome: String,
    /// Answered from the internal zone.
    pub authoritative: bool,
    pub answers: Vec<DnsResolvedRecord>,
    pub latency_ms: f64,
    /// The container address the query was answered as.
    pub asked_as: Option<String>,
    /// A caveat about how the answer was obtained.
    pub note: Option<String>,
}

impl From<TestResolution> for DnsResolveResponse {
    fn from(t: TestResolution) -> Self {
        Self {
            node_id: t.node_id,
            name: t.name,
            record_type: t.record_type,
            rcode: t.answer.rcode,
            outcome: t.answer.outcome.as_str().to_string(),
            authoritative: t.answer.authoritative,
            answers: t.answer.answers.into_iter().map(Into::into).collect(),
            latency_ms: t.answer.latency_us as f64 / 1000.0,
            asked_as: t.asked_as.map(|ip| ip.to_string()),
            note: t.note,
        }
    }
}

/// Resolve a name from one node's resolver and show the answer.
#[utoipa::path(
    tag = "Nodes",
    post,
    path = "/cluster/dns/resolve",
    request_body = DnsResolveRequest,
    responses(
        (status = 200, description = "The answer", body = DnsResolveResponse),
        (status = 400, description = "Invalid name or record type"),
        (status = 401, description = "Unauthorized"),
        (status = 403, description = "Insufficient permissions"),
        (status = 404, description = "Node not found"),
        (status = 409, description = "The resolver is not running, the agent is too old, or the project has no container on that node"),
        (status = 502, description = "The resolver could not be reached"),
        (status = 500, description = "Internal server error")
    ),
    security(("bearer_auth" = []))
)]
pub async fn resolve_dns_name(
    RequireAuth(auth): RequireAuth,
    State(state): State<Arc<NodeAdminState>>,
    Json(request): Json<DnsResolveRequest>,
) -> Result<impl IntoResponse, Problem> {
    permission_guard!(auth, SettingsRead);
    let record_type = request.record_type.unwrap_or_else(|| "A".to_string());
    info!(
        node_id = request.node_id,
        project_id = ?request.project_id,
        record_type = %record_type,
        "Test DNS resolution requested from the console"
    );
    let resolution = state
        .cluster_dns
        .resolve(
            request.node_id,
            &request.name,
            &record_type,
            request.project_id,
        )
        .await?;
    Ok(Json(DnsResolveResponse::from(resolution)))
}

#[derive(OpenApi)]
#[openapi(
    paths(
        list_dns_resolvers,
        list_dns_records,
        list_dns_queries,
        dns_traffic,
        resolve_dns_name,
    ),
    components(schemas(
        ClusterDnsResolversResponse,
        DnsResolverEntry,
        DnsAlertEntry,
        ResolverHealth,
        InternalProxyState,
        DnsRecordListResponse,
        DnsRecordEntry,
        DnsQueryLogResponse,
        DnsQueryLogEntry,
        DnsTrafficResponse,
        DnsTrafficBucket,
        DnsOutcomeCounts,
        DnsResolveRequest,
        DnsResolveResponse,
        DnsResolvedRecord,
    ))
)]
pub struct ClusterDnsApiDoc;
