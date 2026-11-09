// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Internal DNS observability on the console (ADR-095).
//!
//! - [`ClusterDnsService`] answers the console: each resolver's health,
//!   computed from its last report ([`resolver_health`]), and test
//!   resolutions, asked of a worker's agent or of the control plane's
//!   resolver over the network.
//! - [`ClusterDnsCollector`] pulls every active worker's
//!   `GET /agent/dns/report` on the monitoring scrape interval, stores it
//!   through [`DnsObservabilityService::ingest`], then evaluates the DNS
//!   alarms ([`evaluate_alerts`]) and prunes the query log.

use std::collections::{HashMap, HashSet};
use std::net::{IpAddr, SocketAddr};
use std::sync::Arc;
use std::time::Duration;

use chrono::{DateTime, Utc};
use futures::StreamExt;
use reqwest::StatusCode;
use sea_orm::{
    ColumnTrait, DatabaseBackend, DatabaseConnection, EntityTrait, FromQueryResult, QueryFilter,
    Statement, Value,
};
use temps_config::ConfigService;
use temps_core::EncryptionService;
use temps_dns::services::observability::{
    ReportOutcome, ReportState, CONTROL_PLANE_NODE_ID, MAX_DNS_REPORT_BYTES,
};
use temps_dns::DnsObservabilityService;
use temps_entities::{dns_resolver_reports, nodes};
use temps_monitoring::{AlarmService, AlarmSeverity, AlarmType, FireAlarmRequest};
use temps_node_dns::observe::DnsReport;
use temps_node_dns::resolve::{
    parse_test_query_name, parse_test_query_type, resolve_over_udp, ResolveAnswer,
};
use tracing::{debug, info, warn};

/// The agent's DNS report and test-resolution paths (ADR-095 §2).
pub const AGENT_DNS_REPORT_PATH: &str = "/agent/dns/report";
pub const AGENT_DNS_RESOLVE_PATH: &str = "/agent/dns/resolve";

/// Floor and fallback for the pull interval, as for worker metrics.
const MIN_INTERVAL_SECS: u64 = 15;
const DEFAULT_INTERVAL_SECS: u64 = 30;
const PULL_TIMEOUT: Duration = Duration::from_secs(5);
const PULL_CONCURRENCY: usize = 8;
/// A test resolution may wait for a slow forwarded answer (2 s budget).
const RESOLVE_TIMEOUT: Duration = Duration::from_secs(6);
/// How often the query log is pruned.
const PRUNE_EVERY: Duration = Duration::from_secs(300);

/// A worker that has not synced for longer than this serves a stale zone.
pub const SYNC_STALE_AFTER_SECS: i64 = 120;
/// Failed syncs in a row that mark a worker's zone stale.
pub const SYNC_STALE_FAILURES: i32 = 5;
/// Pull intervals without a report after which a resolver counts as not
/// reporting.
const MISSED_REPORTS: i64 = 3;
/// The window the query-failure alarm looks at.
pub const FAILURE_WINDOW_MINUTES: i64 = 5;
/// SERVFAIL answers for internal names, in the window, that alarm…
pub const SERVFAIL_MIN: i64 = 10;
/// …when they are at least this share of the node's internal queries.
pub const SERVFAIL_MIN_RATIO: f64 = 0.05;
/// NXDOMAIN answers for names that should exist, in the window, that alarm.
pub const EXPECTED_NXDOMAIN_MIN: i64 = 5;

/// What a resolver looks like right now.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, utoipa::ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum ResolverHealth {
    /// Running, answering, its zone current.
    Healthy,
    /// Running, but its zone has not synced recently.
    Stale,
    /// Not running, or its tasks died.
    Down,
    /// Cluster DNS is off: no resolver is expected.
    Off,
    /// The node is not active (offline, pending, drained).
    NodeUnavailable,
    /// The agent predates DNS reports; only its heartbeat health is known.
    AgentTooOld,
    /// No report recently: the agent cannot be reached, or the process that
    /// owns the control plane's resolver is not running.
    NotReporting,
    /// The control plane has never run a resolver of its own: nothing
    /// started one because no containers run on it (a hosted plane, or a
    /// host without a container runtime). Expected, so it never alarms;
    /// each worker's resolver serves the containers on that worker.
    NoLocalResolver,
}

impl ResolverHealth {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Healthy => "healthy",
            Self::Stale => "stale",
            Self::Down => "down",
            Self::Off => "off",
            Self::NodeUnavailable => "node_unavailable",
            Self::AgentTooOld => "agent_too_old",
            Self::NotReporting => "not_reporting",
            Self::NoLocalResolver => "no_local_resolver",
        }
    }
}

/// The inputs [`resolver_health`] decides on.
#[derive(Debug, Clone)]
pub struct HealthInput<'a> {
    pub node_id: i32,
    pub cluster_dns_enabled: bool,
    /// `active`, `draining`, `offline`... The control plane is `active`.
    pub node_status: &'a str,
    pub report: Option<&'a dns_resolver_reports::Model>,
    /// The heartbeat's view, for an agent too old to report.
    pub heartbeat_running: Option<bool>,
    pub interval_secs: i64,
    pub now: DateTime<Utc>,
}

/// The resolver's health and, when not healthy, why.
pub fn resolver_health(input: &HealthInput<'_>) -> (ResolverHealth, Option<String>) {
    if !input.cluster_dns_enabled {
        return (ResolverHealth::Off, None);
    }
    let control_plane = input.node_id == CONTROL_PLANE_NODE_ID;
    if !control_plane && !matches!(input.node_status, "active" | "draining") {
        return (
            ResolverHealth::NodeUnavailable,
            Some(format!("The node is {}", input.node_status)),
        );
    }
    let Some(report) = input.report else {
        // The control plane's reporter starts with its resolver, so a
        // control plane that never reported never ran one. Once it has
        // reported, a missing report is an outage again (below).
        return if control_plane {
            (
                ResolverHealth::NoLocalResolver,
                Some(
                    "The control plane runs no resolver of its own: no containers run on it, \
                     so none was started. Each worker's resolver serves the containers on \
                     that worker"
                        .to_string(),
                ),
            )
        } else {
            (
                ResolverHealth::NotReporting,
                Some("No DNS report from this node's agent yet".to_string()),
            )
        };
    };
    let stale_after = chrono::Duration::seconds(input.interval_secs.max(1) * MISSED_REPORTS);
    match ReportState::parse(&report.report_state) {
        Some(ReportState::AgentTooOld) => {
            return match input.heartbeat_running {
                Some(false) => (
                    ResolverHealth::Down,
                    Some("The agent's heartbeat says its resolver is not running".into()),
                ),
                _ => (
                    ResolverHealth::AgentTooOld,
                    Some("Upgrade the agent on this node to see its queries and traffic".into()),
                ),
            };
        }
        Some(ReportState::Ok) => {}
        _ => {
            let fresh = report
                .last_ok_at
                .is_some_and(|at| input.now - at < stale_after);
            if !fresh {
                return (
                    ResolverHealth::NotReporting,
                    Some(
                        report
                            .last_error
                            .clone()
                            .unwrap_or_else(|| "The node's DNS report could not be read".into()),
                    ),
                );
            }
        }
    }
    let last_ok = report.last_ok_at.unwrap_or(report.reported_at);
    if input.now - last_ok >= stale_after {
        return (
            ResolverHealth::NotReporting,
            Some(format!(
                "No DNS report for {} seconds",
                (input.now - last_ok).num_seconds()
            )),
        );
    }
    if !report.running {
        return (
            ResolverHealth::Down,
            Some(
                report
                    .last_error
                    .clone()
                    .unwrap_or_else(|| "The resolver is not running".into()),
            ),
        );
    }
    if !report.tasks_alive {
        return (
            ResolverHealth::Down,
            Some("The resolver's server or sync task stopped".into()),
        );
    }
    if !control_plane {
        if report.consecutive_sync_failures >= SYNC_STALE_FAILURES {
            return (
                ResolverHealth::Stale,
                Some(format!(
                    "{} zone syncs in a row failed: {}",
                    report.consecutive_sync_failures,
                    report.last_error.as_deref().unwrap_or("no error recorded")
                )),
            );
        }
        match report.last_sync_age_secs {
            None => {
                return (
                    ResolverHealth::Stale,
                    Some("The resolver has not synced the zone yet".into()),
                )
            }
            Some(age) if age > SYNC_STALE_AFTER_SECS => {
                return (
                    ResolverHealth::Stale,
                    Some(format!("The zone last synced {age} seconds ago")),
                )
            }
            Some(_) => {}
        }
    }
    (ResolverHealth::Healthy, None)
}

/// How a worker serves port 80 of its bridge address (ADR-098 D5).
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize, utoipa::ToSchema,
)]
#[serde(rename_all = "snake_case")]
pub enum InternalRoutingMode {
    /// The internal proxy on its own port, and the nftables redirect of
    /// containers' port-80 calls to it.
    Redirect,
    /// The fallback: the proxy bound on port 80 of the bridge address
    /// itself, because the redirect is missing.
    Direct,
    /// Nothing serves the calls.
    Down,
}

/// Whether a worker's port-80 redirect is in place.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize, utoipa::ToSchema,
)]
#[serde(rename_all = "snake_case")]
pub enum InternalRedirectState {
    Installed,
    Missing,
    /// The agent's operating system has no redirect (not Linux); it binds
    /// port 80 directly.
    Unsupported,
}

/// A worker's internal routing, from its heartbeat's
/// `capacity.internal_proxy`. Every `<env>.<project>.temps.local` name
/// points at the node's bridge, port 80: the resolver can be healthy while
/// nothing serves those calls, and then the names resolve and every call to
/// them fails.
#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Serialize, utoipa::ToSchema)]
pub struct InternalProxyState {
    /// Whether containers' calls to port 80 of the bridge address are
    /// served. `None` when the agent does not report it (an older agent, or
    /// no overlay bridge yet): unknown, never "not listening".
    pub listening: Option<bool>,
    /// What containers call, e.g. `10.205.0.1:80`.
    pub address: Option<String>,
    /// Why the calls are not served; `None` while they are.
    pub error: Option<String>,
    /// How they are served. `None` from an agent older than ADR-098, which
    /// only ever binds port 80 directly.
    pub mode: Option<InternalRoutingMode>,
    /// The proxy's own listener, e.g. `10.205.0.1:591`, while the agent
    /// holds it.
    pub proxy_address: Option<String>,
    /// Whether the redirect is in place; `None` from an older agent.
    pub redirect: Option<InternalRedirectState>,
    /// Why the redirect is missing, also while the fallback serves.
    pub redirect_error: Option<String>,
}

impl InternalProxyState {
    /// Served, but through the fallback on port 80 because the redirect is
    /// missing: the agent holds port 80 of the bridge address, which the
    /// operator may need. Worth a warning, not an alarm.
    pub fn redirect_missing(&self) -> bool {
        self.listening == Some(true)
            && self.mode == Some(InternalRoutingMode::Direct)
            && self.redirect == Some(InternalRedirectState::Missing)
    }
}

/// Read [`InternalProxyState`] from a heartbeat's capacity document.
pub fn internal_proxy_state(capacity: &serde_json::Value) -> InternalProxyState {
    let proxy = &capacity["internal_proxy"];
    let text = |key: &str| {
        proxy[key]
            .as_str()
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(str::to_string)
    };
    let mode = match text("mode").as_deref() {
        Some("redirect") => Some(InternalRoutingMode::Redirect),
        Some("direct") => Some(InternalRoutingMode::Direct),
        Some("down") => Some(InternalRoutingMode::Down),
        _ => None,
    };
    let redirect = match text("redirect").as_deref() {
        Some("installed") => Some(InternalRedirectState::Installed),
        Some("missing") => Some(InternalRedirectState::Missing),
        Some("unsupported") => Some(InternalRedirectState::Unsupported),
        _ => None,
    };
    InternalProxyState {
        listening: proxy["listening"].as_bool(),
        address: text("address"),
        error: text("error"),
        mode,
        proxy_address: text("proxy_address"),
        redirect,
        redirect_error: text("redirect_error"),
    }
}

/// Why the internal-proxy alarm should be open on a worker, if it should:
/// cluster DNS on (else no name points there), the node active, and the
/// proxy reported not listening on two evaluations in a row, so a bind
/// retry in progress pages nobody.
pub fn internal_proxy_alert(
    cluster_dns_enabled: bool,
    node_status: &str,
    proxy: &InternalProxyState,
    down_streak: u32,
) -> Option<String> {
    if !cluster_dns_enabled || !matches!(node_status, "active" | "draining") {
        return None;
    }
    if proxy.listening != Some(false) || down_streak < 2 {
        return None;
    }
    let address = proxy.address.as_deref().unwrap_or("its bridge address");
    Some(format!(
        "Internal routing is down: `*.temps.local` names resolve on this node but nothing serves \
         the HTTP calls to them on {address}: {}",
        proxy.error.as_deref().unwrap_or("no error reported")
    ))
}

/// One node whose resolver the console shows and alarms on: the control
/// plane (node 0, which has no `nodes` row) first, then the workers given.
#[derive(Debug, Clone)]
pub struct DnsNode {
    pub node_id: i32,
    pub node_name: String,
    pub node_status: String,
    /// The heartbeat's view of the resolver, for an agent too old to report.
    pub heartbeat_running: Option<bool>,
    /// `None` for the control plane, whose main proxy routes internal HTTP.
    pub internal_proxy: Option<InternalProxyState>,
}

/// The control plane, then each worker in `workers` (node 0 rows skipped).
pub fn dns_nodes(control_plane_name: &str, workers: Vec<nodes::Model>) -> Vec<DnsNode> {
    let mut rows = vec![DnsNode {
        node_id: CONTROL_PLANE_NODE_ID,
        node_name: control_plane_name.to_string(),
        node_status: "active".to_string(),
        heartbeat_running: None,
        internal_proxy: None,
    }];
    rows.extend(workers.into_iter().filter(|n| n.id > 0).map(|n| DnsNode {
        internal_proxy: Some(internal_proxy_state(&n.capacity)),
        node_id: n.id,
        node_name: n.name,
        node_status: n.status,
        heartbeat_running: n.dns_resolver_running,
    }));
    rows
}

/// The alarms one evaluation wants open, per node.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct AlertDecision {
    pub resolver_down: Option<String>,
    pub sync_stale: Option<String>,
    pub query_failures: Option<String>,
}

/// One node's recent internal failures, for [`evaluate_alerts`].
#[derive(Debug, Clone, Default)]
pub struct RecentFailures {
    pub internal_total: i64,
    pub servfail: i64,
    /// NXDOMAIN answers per name, for names that should exist.
    pub expected_nxdomain: Vec<(String, i64)>,
    /// SERVFAIL answers per name.
    pub servfail_names: Vec<(String, i64)>,
}

fn top_names(names: &[(String, i64)]) -> String {
    let mut sorted = names.to_vec();
    sorted.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
    sorted
        .iter()
        .take(3)
        .map(|(name, count)| format!("{name} ({count})"))
        .collect::<Vec<_>>()
        .join(", ")
}

/// Which DNS alarms a node should have open. `down_streak` counts the
/// evaluations in a row the resolver was down; one is not enough, so a
/// restart in progress does not page anyone.
pub fn evaluate_alerts(
    health: ResolverHealth,
    reason: Option<&str>,
    down_streak: u32,
    is_control_plane: bool,
    failures: &RecentFailures,
) -> AlertDecision {
    let reason = reason.unwrap_or("no detail").to_string();
    let resolver_down = match health {
        ResolverHealth::Down if down_streak >= 2 => Some(reason.clone()),
        // Only the control plane's own reporter is expected to be there
        // whatever the network; an unreachable worker is the node-offline
        // alarm's business.
        ResolverHealth::NotReporting if is_control_plane && down_streak >= 2 => {
            Some(reason.clone())
        }
        _ => None,
    };
    let sync_stale = (health == ResolverHealth::Stale).then_some(reason);
    let servfail_spike = failures.servfail >= SERVFAIL_MIN
        && failures.internal_total > 0
        && (failures.servfail as f64 / failures.internal_total as f64) >= SERVFAIL_MIN_RATIO;
    let expected_missing: i64 = failures.expected_nxdomain.iter().map(|(_, c)| c).sum();
    let query_failures = match (servfail_spike, expected_missing >= EXPECTED_NXDOMAIN_MIN) {
        (false, false) => None,
        (servfail, nxdomain) => {
            let mut parts = Vec::new();
            if servfail {
                parts.push(format!(
                    "{} of {} internal queries answered SERVFAIL in the last {FAILURE_WINDOW_MINUTES} \
                     minutes: {}",
                    failures.servfail,
                    failures.internal_total,
                    top_names(&failures.servfail_names)
                ));
            }
            if nxdomain {
                parts.push(format!(
                    "{expected_missing} NXDOMAIN answers in the last {FAILURE_WINDOW_MINUTES} minutes \
                     for names that should exist: {}",
                    top_names(&failures.expected_nxdomain)
                ));
            }
            Some(parts.join(". "))
        }
    };
    AlertDecision {
        resolver_down,
        sync_stale,
        query_failures,
    }
}

/// Why a test resolution could not be answered.
#[derive(Debug, thiserror::Error)]
pub enum ClusterDnsError {
    #[error("Node {node_id} not found")]
    NodeNotFound { node_id: i32 },
    #[error("Node {node_id} ({node_name}) is {status}; its resolver cannot be asked")]
    NodeUnavailable {
        node_id: i32,
        node_name: String,
        status: String,
    },
    #[error("Project {project_id} has no container with a published address on node {node_id}")]
    NoProjectContainer { project_id: i32, node_id: i32 },
    #[error("Invalid test resolution: {reason}")]
    Invalid { reason: String },
    #[error("The resolver on node {node_id} is not running: {reason}")]
    ResolverNotRunning { node_id: i32, reason: String },
    #[error("The agent on node {node_id} predates test resolution (HTTP {status}); upgrade it")]
    AgentTooOld { node_id: i32, status: u16 },
    #[error("Could not reach node {node_id}'s resolver: {reason}")]
    Unreachable { node_id: i32, reason: String },
    #[error("Database error while {operation}: {source}")]
    Database {
        operation: &'static str,
        #[source]
        source: sea_orm::DbErr,
    },
    #[error(transparent)]
    Observability(#[from] temps_dns::DnsObservabilityError),
}

/// A test resolution's answer, and from where it was asked.
#[derive(Debug, Clone)]
pub struct TestResolution {
    pub node_id: i32,
    pub name: String,
    pub record_type: String,
    /// The address the query was answered as, when it was a project's
    /// container.
    pub asked_as: Option<IpAddr>,
    pub answer: ResolveAnswer,
    /// A caveat about how the answer was obtained.
    pub note: Option<String>,
}

#[derive(Debug, FromQueryResult)]
struct SourceRow {
    target_ip: Option<String>,
}

fn decrypt_token(
    encryption_service: &EncryptionService,
    node: &nodes::Model,
) -> Result<String, String> {
    let encrypted = node
        .token_encrypted
        .as_ref()
        .ok_or_else(|| "the node has no stored agent token".to_string())?;
    let bytes = encryption_service
        .decrypt(encrypted)
        .map_err(|e| format!("cannot decrypt the node's agent token: {e}"))?;
    String::from_utf8(bytes).map_err(|_| "the node's agent token is not valid UTF-8".to_string())
}

/// Reads a bounded body: at most `max` bytes, or an error.
async fn read_capped(mut response: reqwest::Response, max: usize) -> Result<Vec<u8>, String> {
    if response
        .content_length()
        .is_some_and(|len| usize::try_from(len).unwrap_or(usize::MAX) > max)
    {
        return Err(format!("the report is larger than {max} bytes"));
    }
    let mut body = Vec::new();
    while let Some(chunk) = response.chunk().await.map_err(|e| e.to_string())? {
        if body.len() + chunk.len() > max {
            return Err(format!("the report is larger than {max} bytes"));
        }
        body.extend_from_slice(&chunk);
    }
    Ok(body)
}

/// The console's view of internal DNS.
pub struct ClusterDnsService {
    db: Arc<DatabaseConnection>,
    observability: Arc<DnsObservabilityService>,
    config_service: Arc<ConfigService>,
    encryption_service: Arc<EncryptionService>,
}

impl ClusterDnsService {
    pub fn new(
        db: Arc<DatabaseConnection>,
        observability: Arc<DnsObservabilityService>,
        config_service: Arc<ConfigService>,
        encryption_service: Arc<EncryptionService>,
    ) -> Self {
        Self {
            db,
            observability,
            config_service,
            encryption_service,
        }
    }

    /// The service the handlers list from.
    pub fn observability(&self) -> &DnsObservabilityService {
        &self.observability
    }

    /// The pull interval, which also sets when a report counts as missing.
    pub async fn interval_secs(&self) -> u64 {
        match self.config_service.get_settings().await {
            Ok(settings) => settings
                .monitoring
                .scrape_interval_secs
                .max(MIN_INTERVAL_SECS),
            Err(_) => DEFAULT_INTERVAL_SECS,
        }
    }

    async fn agent_client(
        &self,
        node: &nodes::Model,
        timeout: Duration,
    ) -> Result<(reqwest::Client, String, String), String> {
        let token = decrypt_token(&self.encryption_service, node)?;
        let endpoint = crate::cluster_ca::node_agent_endpoint(node);
        let client = crate::cluster_ca::build_node_http_client(
            &endpoint,
            self.config_service.as_ref(),
            self.encryption_service.as_ref(),
            Some(timeout),
        )
        .await
        .map_err(|e| e.to_string())?;
        Ok((
            client,
            endpoint.dial_url.trim_end_matches('/').to_string(),
            token,
        ))
    }

    /// Pull one worker's report, continuing from what is stored.
    pub async fn pull(&self, node: &nodes::Model) -> ReportOutcome {
        let failed = |state, reason: String| ReportOutcome::Failed { state, reason };
        let stored = dns_resolver_reports::Entity::find_by_id(node.id)
            .one(self.db.as_ref())
            .await
            .ok()
            .flatten();
        let (client, base, token) = match self.agent_client(node, PULL_TIMEOUT).await {
            Ok(parts) => parts,
            Err(reason) => return failed(ReportState::Rejected, reason),
        };
        let mut url = format!("{base}{AGENT_DNS_REPORT_PATH}");
        if let Some((cursor, instance)) = stored
            .as_ref()
            .and_then(|r| r.query_cursor.zip(r.instance_id.clone()))
        {
            url.push_str(&format!(
                "?after={cursor}&instance={}",
                urlencoding_component(&instance)
            ));
        }
        let fetch = async {
            let response = client
                .get(&url)
                .bearer_auth(&token)
                .send()
                .await
                .map_err(|e| failed(ReportState::Unreachable, e.to_string()))?;
            let status = response.status();
            if !status.is_success() {
                return Err(match status {
                    StatusCode::NOT_FOUND | StatusCode::METHOD_NOT_ALLOWED => failed(
                        ReportState::AgentTooOld,
                        format!("The agent has no DNS report endpoint (HTTP {status})"),
                    ),
                    StatusCode::UNAUTHORIZED | StatusCode::FORBIDDEN => failed(
                        ReportState::Rejected,
                        format!("The agent refused the control plane's node token (HTTP {status})"),
                    ),
                    s if s.is_server_error() => failed(
                        ReportState::Unreachable,
                        format!("The agent answered HTTP {s}"),
                    ),
                    s => failed(ReportState::Invalid, format!("The agent answered HTTP {s}")),
                });
            }
            let body = read_capped(response, MAX_DNS_REPORT_BYTES)
                .await
                .map_err(|reason| failed(ReportState::Invalid, reason))?;
            serde_json::from_slice::<DnsReport>(&body).map_err(|e| {
                failed(
                    ReportState::Invalid,
                    format!("The DNS report could not be read: {e}"),
                )
            })
        };
        match tokio::time::timeout(PULL_TIMEOUT, fetch).await {
            Ok(Ok(report)) => ReportOutcome::Report(Box::new(report)),
            Ok(Err(outcome)) => outcome,
            Err(_) => failed(
                ReportState::Unreachable,
                format!("no answer within {} s", PULL_TIMEOUT.as_secs()),
            ),
        }
    }

    /// The address of one of `project_id`'s containers on `node_id`, from
    /// the endpoints published for its environments.
    async fn project_source(
        &self,
        project_id: i32,
        node_id: i32,
    ) -> Result<IpAddr, ClusterDnsError> {
        let node_clause = if node_id == CONTROL_PLANE_NODE_ID {
            "(se.node_id IS NULL OR se.node_id = 0)"
        } else {
            "se.node_id = $2"
        };
        let mut values: Vec<Value> = vec![project_id.into()];
        if node_id != CONTROL_PLANE_NODE_ID {
            values.push(node_id.into());
        }
        let rows = SourceRow::find_by_statement(Statement::from_sql_and_values(
            DatabaseBackend::Postgres,
            format!(
                "SELECT se.target_ip FROM service_endpoints se \
                 JOIN environments e ON se.owner_kind = 'environment' AND e.id = se.owner_id \
                 WHERE e.project_id = $1 AND {node_clause} AND se.target_ip IS NOT NULL \
                 ORDER BY se.updated_at DESC LIMIT 10"
            ),
            values,
        ))
        .all(self.db.as_ref())
        .await
        .map_err(|source| ClusterDnsError::Database {
            operation: "finding a project's container address",
            source,
        })?;
        rows.into_iter()
            .filter_map(|r| r.target_ip)
            .find_map(|ip| ip.parse::<IpAddr>().ok())
            .ok_or(ClusterDnsError::NoProjectContainer {
                project_id,
                node_id,
            })
    }

    /// Resolve `name` from `node_id`'s resolver, as `project_id`'s container
    /// there would be answered when one is given.
    pub async fn resolve(
        &self,
        node_id: i32,
        name: &str,
        record_type: &str,
        project_id: Option<i32>,
    ) -> Result<TestResolution, ClusterDnsError> {
        let invalid = |e: temps_node_dns::ResolverError| ClusterDnsError::Invalid {
            reason: e.to_string(),
        };
        let parsed_name = parse_test_query_name(name).map_err(invalid)?;
        let qtype = parse_test_query_type(record_type).map_err(invalid)?;
        let shown_name = parsed_name.to_utf8().trim_end_matches('.').to_string();
        let shown_type = qtype.to_string();

        if node_id == CONTROL_PLANE_NODE_ID {
            return self
                .resolve_on_control_plane(parsed_name, qtype, project_id, shown_name, shown_type)
                .await;
        }

        let node = nodes::Entity::find_by_id(node_id)
            .one(self.db.as_ref())
            .await
            .map_err(|source| ClusterDnsError::Database {
                operation: "reading a node",
                source,
            })?
            .ok_or(ClusterDnsError::NodeNotFound { node_id })?;
        if !matches!(node.status.as_str(), "active" | "draining") {
            return Err(ClusterDnsError::NodeUnavailable {
                node_id,
                node_name: node.name.clone(),
                status: node.status.clone(),
            });
        }
        let source = match project_id {
            Some(project_id) => Some(self.project_source(project_id, node_id).await?),
            None => None,
        };
        let (client, base, token) = self
            .agent_client(&node, RESOLVE_TIMEOUT)
            .await
            .map_err(|reason| ClusterDnsError::Unreachable { node_id, reason })?;
        let body = serde_json::json!({
            "name": shown_name,
            "record_type": shown_type,
            "source_ip": source,
        });
        let response = client
            .post(format!("{base}{AGENT_DNS_RESOLVE_PATH}"))
            .bearer_auth(&token)
            .json(&body)
            .send()
            .await
            .map_err(|e| ClusterDnsError::Unreachable {
                node_id,
                reason: e.to_string(),
            })?;
        let status = response.status();
        if status == StatusCode::NOT_FOUND || status == StatusCode::METHOD_NOT_ALLOWED {
            return Err(ClusterDnsError::AgentTooOld {
                node_id,
                status: status.as_u16(),
            });
        }
        let bytes = read_capped(response, 64 * 1024)
            .await
            .map_err(|reason| ClusterDnsError::Unreachable { node_id, reason })?;
        if status == StatusCode::CONFLICT {
            return Err(ClusterDnsError::ResolverNotRunning {
                node_id,
                reason: problem_detail(&bytes),
            });
        }
        if status == StatusCode::BAD_REQUEST {
            return Err(ClusterDnsError::Invalid {
                reason: problem_detail(&bytes),
            });
        }
        if !status.is_success() {
            return Err(ClusterDnsError::Unreachable {
                node_id,
                reason: format!("HTTP {status}: {}", problem_detail(&bytes)),
            });
        }
        let answer: ResolveAnswer =
            serde_json::from_slice(&bytes).map_err(|e| ClusterDnsError::Unreachable {
                node_id,
                reason: format!("the answer could not be read: {e}"),
            })?;
        Ok(TestResolution {
            node_id,
            name: shown_name,
            record_type: shown_type,
            asked_as: source,
            answer,
            note: None,
        })
    }

    async fn resolve_on_control_plane(
        &self,
        name: hickory_proto::rr::Name,
        qtype: hickory_proto::rr::RecordType,
        project_id: Option<i32>,
        shown_name: String,
        shown_type: String,
    ) -> Result<TestResolution, ClusterDnsError> {
        let node_id = CONTROL_PLANE_NODE_ID;
        let report = dns_resolver_reports::Entity::find_by_id(node_id)
            .one(self.db.as_ref())
            .await
            .map_err(|source| ClusterDnsError::Database {
                operation: "reading the control plane's DNS report",
                source,
            })?;
        let address: Option<SocketAddr> = report
            .as_ref()
            .filter(|r| r.running)
            .and_then(|r| r.listen_address.as_deref())
            .and_then(|list| list.split(',').next())
            .and_then(|a| a.trim().parse().ok());
        let Some(address) = address else {
            return Err(ClusterDnsError::ResolverNotRunning {
                node_id,
                reason: report
                    .and_then(|r| r.last_error)
                    .unwrap_or_else(|| "it has not reported a listen address".into()),
            });
        };
        // The resolver lives in the proxy process; ask it over the network.
        let note = match project_id {
            Some(project_id) => {
                // Still check the project has a container here, so the
                // answer is about a real workload.
                self.project_source(project_id, node_id).await?;
                Some(
                    "Asked from the console, not from the project's container: the control \
                     plane's resolver runs in the proxy process, so the project's network policy \
                     is not applied to this answer."
                        .to_string(),
                )
            }
            None => None,
        };
        let answer = resolve_over_udp(address, name, qtype).await.map_err(|e| {
            ClusterDnsError::Unreachable {
                node_id,
                reason: e.to_string(),
            }
        })?;
        Ok(TestResolution {
            node_id,
            name: shown_name,
            record_type: shown_type,
            asked_as: None,
            answer,
            note,
        })
    }
}

/// `detail` of a Problem body, or the body itself.
fn problem_detail(bytes: &[u8]) -> String {
    #[derive(serde::Deserialize)]
    struct Detail {
        detail: Option<String>,
        title: Option<String>,
    }
    match serde_json::from_slice::<Detail>(bytes) {
        Ok(Detail {
            detail: Some(detail),
            ..
        }) => detail,
        Ok(Detail {
            title: Some(title), ..
        }) => title,
        _ => String::from_utf8_lossy(&bytes[..bytes.len().min(512)]).into_owned(),
    }
}

/// Percent-encode a query component (instance ids are hex, but the value
/// comes from the database and is not trusted to stay that way).
fn urlencoding_component(value: &str) -> String {
    value
        .bytes()
        .map(|b| match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                (b as char).to_string()
            }
            other => format!("%{other:02X}"),
        })
        .collect()
}

/// Pulls worker DNS reports, raises and resolves DNS alarms, prunes the
/// query log.
pub struct ClusterDnsCollector {
    db: Arc<DatabaseConnection>,
    service: Arc<ClusterDnsService>,
    alarms: Arc<AlarmService>,
}

/// Per node: evaluations in a row the resolver, and the internal proxy,
/// were down.
#[derive(Default)]
struct AlertState {
    down_streak: HashMap<i32, u32>,
    proxy_down_streak: HashMap<i32, u32>,
}

impl ClusterDnsCollector {
    pub fn new(
        db: Arc<DatabaseConnection>,
        service: Arc<ClusterDnsService>,
        alarms: Arc<AlarmService>,
    ) -> Self {
        Self {
            db,
            service,
            alarms,
        }
    }

    /// Pull forever. Spawn on a background task.
    pub async fn run(self) {
        let mut state = AlertState::default();
        let mut last_prune = std::time::Instant::now();
        loop {
            let interval = self.service.interval_secs().await;
            tokio::time::sleep(Duration::from_secs(interval)).await;
            let enabled = match self.service.config_service.get_settings().await {
                Ok(settings) => settings.cluster_dns.enabled,
                Err(e) => {
                    debug!("Cluster DNS collector: cannot read settings ({e}); skipping");
                    continue;
                }
            };
            if enabled {
                self.pull_all().await;
            }
            if let Err(e) = self.evaluate(&mut state, enabled, interval).await {
                warn!("Cluster DNS alarms not evaluated this interval: {e}");
            }
            if last_prune.elapsed() >= PRUNE_EVERY {
                last_prune = std::time::Instant::now();
                match self.service.observability.prune_query_log().await {
                    Ok(0) => {}
                    Ok(n) => debug!(deleted = n, "Pruned the DNS query log"),
                    Err(e) => warn!("Could not prune the DNS query log: {e}"),
                }
            }
        }
    }

    async fn workers(&self) -> Result<Vec<nodes::Model>, sea_orm::DbErr> {
        nodes::Entity::find()
            .filter(nodes::Column::Id.gt(0))
            .filter(nodes::Column::Status.is_in(["active", "draining"]))
            .all(self.db.as_ref())
            .await
    }

    async fn pull_all(&self) {
        let workers = match self.workers().await {
            Ok(nodes) => nodes,
            Err(e) => {
                debug!("Cluster DNS collector: cannot list nodes ({e}); skipping this interval");
                return;
            }
        };
        let outcomes: Vec<(i32, ReportOutcome, DateTime<Utc>)> = futures::stream::iter(workers)
            .map(|node| async move {
                let outcome = self.service.pull(&node).await;
                (node.id, outcome, Utc::now())
            })
            .buffer_unordered(PULL_CONCURRENCY)
            .collect()
            .await;
        for (node_id, outcome, received_at) in outcomes {
            match self
                .service
                .observability
                .ingest(node_id, outcome, received_at)
                .await
            {
                Ok(summary) => {
                    if let Some(reason) = summary.rejected {
                        warn!(node_id, "Refused a DNS report: {reason}");
                    }
                }
                Err(e) => warn!(node_id, "Could not store a DNS report: {e}"),
            }
        }
    }

    async fn evaluate(
        &self,
        state: &mut AlertState,
        enabled: bool,
        interval_secs: u64,
    ) -> Result<(), ClusterDnsError> {
        let now = Utc::now();
        let observability = &self.service.observability;
        let reports: HashMap<i32, dns_resolver_reports::Model> = observability
            .resolver_reports()
            .await?
            .into_iter()
            .map(|r| (r.node_id, r))
            .collect();
        let workers = self
            .workers()
            .await
            .map_err(|source| ClusterDnsError::Database {
                operation: "listing nodes for DNS alarms",
                source,
            })?;
        let since = now - chrono::Duration::minutes(FAILURE_WINDOW_MINUTES);
        let (totals, failures, expected) = if enabled {
            (
                observability.internal_totals_since(since).await?,
                observability.internal_failures_since(since).await?,
                observability.expected_names().await?,
            )
        } else {
            (Vec::new(), Vec::new(), HashSet::new())
        };
        let mut recent: HashMap<i32, RecentFailures> = HashMap::new();
        for (node_id, total) in totals {
            recent.entry(node_id).or_default().internal_total = total;
        }
        for failure in failures {
            let entry = recent.entry(failure.node_id).or_default();
            let name = failure.qname.unwrap_or_default();
            match failure.outcome.as_str() {
                "servfail" => {
                    entry.servfail += failure.count;
                    entry.servfail_names.push((name, failure.count));
                }
                "nxdomain" if expected.contains(&name) => {
                    entry.expected_nxdomain.push((name, failure.count));
                }
                _ => {}
            }
        }

        let nodes = dns_nodes("control plane", workers);
        let interval = i64::try_from(interval_secs).unwrap_or(i64::MAX);
        let seen: HashSet<i32> = nodes.iter().map(|n| n.node_id).collect();
        state.down_streak.retain(|id, _| seen.contains(id));
        state.proxy_down_streak.retain(|id, _| seen.contains(id));

        for DnsNode {
            node_id,
            node_name,
            node_status: status,
            heartbeat_running,
            internal_proxy: proxy,
        } in nodes
        {
            let (health, reason) = resolver_health(&HealthInput {
                node_id,
                cluster_dns_enabled: enabled,
                node_status: &status,
                report: reports.get(&node_id),
                heartbeat_running,
                interval_secs: interval,
                now,
            });
            let streak = state.down_streak.entry(node_id).or_default();
            *streak = match health {
                ResolverHealth::Down | ResolverHealth::NotReporting => streak.saturating_add(1),
                _ => 0,
            };
            let decision = evaluate_alerts(
                health,
                reason.as_deref(),
                *streak,
                node_id == CONTROL_PLANE_NODE_ID,
                recent.get(&node_id).unwrap_or(&RecentFailures::default()),
            );
            self.apply(
                node_id,
                &node_name,
                AlarmType::DnsResolverDown,
                decision.resolver_down,
            )
            .await;
            self.apply(
                node_id,
                &node_name,
                AlarmType::DnsSyncStale,
                decision.sync_stale,
            )
            .await;
            self.apply(
                node_id,
                &node_name,
                AlarmType::DnsQueryFailures,
                decision.query_failures,
            )
            .await;
            if let Some(proxy) = proxy {
                let streak = state.proxy_down_streak.entry(node_id).or_default();
                *streak = if proxy.listening == Some(false) {
                    streak.saturating_add(1)
                } else {
                    0
                };
                let reason = internal_proxy_alert(enabled, &status, &proxy, *streak);
                self.apply(node_id, &node_name, AlarmType::DnsInternalProxyDown, reason)
                    .await;
            }
        }
        Ok(())
    }

    /// Open `alarm_type` for the node with `reason`, or resolve it.
    async fn apply(
        &self,
        node_id: i32,
        node_name: &str,
        alarm_type: AlarmType,
        reason: Option<String>,
    ) {
        let result = match reason {
            Some(message) => match self.alarms.has_open_node_alarm(node_id, alarm_type).await {
                Ok(true) => Ok(()),
                Ok(false) => {
                    let (severity, title) = match alarm_type {
                        AlarmType::DnsResolverDown => (
                            AlarmSeverity::Critical,
                            format!("Internal DNS resolver down on {node_name}"),
                        ),
                        AlarmType::DnsSyncStale => (
                            AlarmSeverity::Warning,
                            format!("Internal DNS zone stale on {node_name}"),
                        ),
                        AlarmType::DnsInternalProxyDown => (
                            AlarmSeverity::Critical,
                            format!("Internal HTTP routing down on {node_name}"),
                        ),
                        _ => (
                            AlarmSeverity::Warning,
                            format!("Internal DNS lookups failing on {node_name}"),
                        ),
                    };
                    info!(node_id, node = %node_name, alarm = alarm_type.as_str(), "{message}");
                    self.alarms
                        .fire_alarm(FireAlarmRequest {
                            project_id: None,
                            environment_id: None,
                            deployment_id: None,
                            container_id: None,
                            service_id: None,
                            alarm_type,
                            severity,
                            title,
                            message,
                            metadata: Some(serde_json::json!({
                                "node_id": node_id,
                                "node_name": node_name,
                                "settings_path": "/settings/dns",
                            })),
                        })
                        .await
                        .map(|_| ())
                }
                Err(e) => Err(e),
            },
            None => self
                .alarms
                .resolve_node_alarms(node_id, alarm_type)
                .await
                .map(|resolved| {
                    if !resolved.is_empty() {
                        info!(
                            node_id,
                            node = %node_name,
                            alarm = alarm_type.as_str(),
                            "Internal DNS condition cleared; alarm resolved"
                        );
                    }
                }),
        };
        if let Err(e) = result {
            warn!(
                node_id,
                alarm = alarm_type.as_str(),
                "Could not update a DNS alarm: {e}"
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// What an agent from before ADR-098 reports when port 80 is held.
    fn held_proxy() -> InternalProxyState {
        internal_proxy_state(&serde_json::json!({
            "cpu_cores": 4,
            "internal_proxy": {
                "address": "10.205.0.1:80",
                "listening": false,
                "error": "another program already listens on 10.205.0.1:80",
                "attempts": 7,
            },
        }))
    }

    #[test]
    fn internal_proxy_state_is_read_from_the_heartbeat() {
        assert_eq!(
            held_proxy(),
            InternalProxyState {
                listening: Some(false),
                address: Some("10.205.0.1:80".into()),
                error: Some("another program already listens on 10.205.0.1:80".into()),
                mode: None,
                proxy_address: None,
                redirect: None,
                redirect_error: None,
            }
        );
        let serving = internal_proxy_state(&serde_json::json!({
            "internal_proxy": {"address": "10.205.0.1:80", "listening": true, "error": ""},
        }));
        assert_eq!(serving.listening, Some(true));
        assert_eq!(serving.error, None, "an empty error is no error");
        for unknown in [
            serde_json::json!({"cpu_cores": 2}),
            serde_json::json!({"internal_proxy": "yes"}),
            serde_json::Value::Null,
        ] {
            assert_eq!(
                internal_proxy_state(&unknown),
                InternalProxyState::default()
            );
        }
    }

    /// The heartbeat of an ADR-098 agent, in each of its states.
    #[test]
    fn redirect_states_are_read_from_the_heartbeat() {
        let redirected = internal_proxy_state(&serde_json::json!({
            "internal_proxy": {
                "address": "10.205.0.1:80",
                "listening": true,
                "error": null,
                "attempts": 4,
                "mode": "redirect",
                "proxy_address": "10.205.0.1:591",
                "redirect": "installed",
                "redirect_error": null,
            },
        }));
        assert_eq!(
            redirected,
            InternalProxyState {
                listening: Some(true),
                address: Some("10.205.0.1:80".into()),
                error: None,
                mode: Some(InternalRoutingMode::Redirect),
                proxy_address: Some("10.205.0.1:591".into()),
                redirect: Some(InternalRedirectState::Installed),
                redirect_error: None,
            }
        );
        assert!(!redirected.redirect_missing());

        let direct = internal_proxy_state(&serde_json::json!({
            "internal_proxy": {
                "address": "10.205.0.1:80",
                "listening": true,
                "mode": "direct",
                "proxy_address": "10.205.0.1:591",
                "redirect": "missing",
                "redirect_error": "nft (nftables) is not installed on this worker",
            },
        }));
        assert!(direct.redirect_missing());
        assert_eq!(
            direct.redirect_error.as_deref(),
            Some("nft (nftables) is not installed on this worker")
        );
        assert_eq!(
            internal_proxy_alert(true, "active", &direct, 9),
            None,
            "served: no alarm"
        );

        let not_linux = internal_proxy_state(&serde_json::json!({
            "internal_proxy": {"listening": true, "mode": "direct", "redirect": "unsupported"},
        }));
        assert!(!not_linux.redirect_missing(), "nothing to fix there");

        // A value this control plane does not know yet: unknown, not wrong.
        let future = internal_proxy_state(&serde_json::json!({
            "internal_proxy": {"listening": true, "mode": "tunnel", "redirect": "bpf"},
        }));
        assert_eq!((future.mode, future.redirect), (None, None));
    }

    #[test]
    fn a_held_proxy_alarms_on_the_second_evaluation_with_its_reason() {
        let proxy = held_proxy();
        assert_eq!(internal_proxy_alert(true, "active", &proxy, 1), None);
        let reason = internal_proxy_alert(true, "active", &proxy, 2).expect("alarm");
        assert!(reason.contains("10.205.0.1:80"), "{reason}");
        assert!(reason.contains("already listens"), "{reason}");
        assert!(reason.starts_with("Internal routing is down"), "{reason}");
        assert!(!reason.contains("cannot listen"), "{reason}");
        assert!(internal_proxy_alert(true, "draining", &proxy, 3).is_some());

        let down = internal_proxy_state(&serde_json::json!({
            "internal_proxy": {
                "address": "10.205.0.1:80",
                "listening": false,
                "mode": "down",
                "redirect": "missing",
                "error": "containers' calls to port 80 of 10.205.0.1 are not redirected",
            },
        }));
        let reason = internal_proxy_alert(true, "active", &down, 2).expect("alarm");
        assert!(reason.contains("are not redirected"), "{reason}");
        assert!(!reason.to_lowercase().contains("free port"), "{reason}");
    }

    #[test]
    fn the_proxy_alarm_stays_quiet_when_nothing_points_at_it_or_it_is_unknown() {
        let proxy = held_proxy();
        assert_eq!(
            internal_proxy_alert(false, "active", &proxy, 5),
            None,
            "cluster DNS off"
        );
        assert_eq!(
            internal_proxy_alert(true, "offline", &proxy, 5),
            None,
            "node offline"
        );
        let unknown = InternalProxyState::default();
        assert_eq!(
            internal_proxy_alert(true, "active", &unknown, 5),
            None,
            "older agent"
        );
        let listening = InternalProxyState {
            listening: Some(true),
            ..held_proxy()
        };
        assert_eq!(internal_proxy_alert(true, "active", &listening, 5), None);
    }

    fn report(state: &str, at: DateTime<Utc>) -> dns_resolver_reports::Model {
        dns_resolver_reports::Model {
            node_id: 3,
            report_state: state.into(),
            reported_at: at,
            last_ok_at: (state == "ok").then_some(at),
            instance_id: Some("i".into()),
            build_version: Some("1.0.0".into()),
            running: true,
            tasks_alive: true,
            listen_address: Some("10.237.0.1:53".into()),
            zone_generation: Some(4),
            record_count: Some(3),
            last_sync_age_secs: Some(5),
            consecutive_sync_failures: 0,
            last_error: None,
            query_cursor: Some(9),
            dropped_log_entries: 0,
            created_at: at,
            updated_at: at,
        }
    }

    fn health(
        node_id: i32,
        status: &str,
        report: Option<&dns_resolver_reports::Model>,
        now: DateTime<Utc>,
    ) -> (ResolverHealth, Option<String>) {
        resolver_health(&HealthInput {
            node_id,
            cluster_dns_enabled: true,
            node_status: status,
            report,
            heartbeat_running: None,
            interval_secs: 30,
            now,
        })
    }

    #[test]
    fn a_fresh_report_of_a_synced_resolver_is_healthy() {
        let now = Utc::now();
        let r = report("ok", now - chrono::Duration::seconds(10));
        assert_eq!(
            health(3, "active", Some(&r), now).0,
            ResolverHealth::Healthy
        );
    }

    #[test]
    fn cluster_dns_off_and_unavailable_nodes_are_not_failures() {
        let now = Utc::now();
        let r = report("ok", now);
        let off = resolver_health(&HealthInput {
            node_id: 3,
            cluster_dns_enabled: false,
            node_status: "active",
            report: Some(&r),
            heartbeat_running: None,
            interval_secs: 30,
            now,
        });
        assert_eq!(off.0, ResolverHealth::Off);
        let (h, reason) = health(3, "offline", Some(&r), now);
        assert_eq!(h, ResolverHealth::NodeUnavailable);
        assert!(reason.unwrap().contains("offline"));
    }

    #[test]
    fn a_resolver_that_stopped_or_lost_tasks_is_down() {
        let now = Utc::now();
        let mut r = report("ok", now);
        r.running = false;
        r.last_error = Some("port 53 already in use".into());
        let (h, reason) = health(3, "active", Some(&r), now);
        assert_eq!(h, ResolverHealth::Down);
        assert_eq!(reason.as_deref(), Some("port 53 already in use"));
        let mut r = report("ok", now);
        r.tasks_alive = false;
        assert_eq!(health(3, "active", Some(&r), now).0, ResolverHealth::Down);
    }

    #[test]
    fn a_worker_that_stopped_syncing_is_stale_but_the_control_plane_is_not() {
        let now = Utc::now();
        let mut r = report("ok", now);
        r.last_sync_age_secs = Some(SYNC_STALE_AFTER_SECS + 1);
        let (h, reason) = health(3, "active", Some(&r), now);
        assert_eq!(h, ResolverHealth::Stale);
        assert!(reason.unwrap().contains("121 seconds"));
        r.last_sync_age_secs = Some(1);
        r.consecutive_sync_failures = SYNC_STALE_FAILURES;
        r.last_error = Some("control plane unreachable".into());
        let (h, reason) = health(3, "active", Some(&r), now);
        assert_eq!(h, ResolverHealth::Stale);
        assert!(reason.unwrap().contains("control plane unreachable"));
        r.last_sync_age_secs = None;
        r.consecutive_sync_failures = 0;
        assert_eq!(health(3, "active", Some(&r), now).0, ResolverHealth::Stale);
        // The control plane feeds its zone locally: no sync to go stale.
        assert_eq!(
            health(0, "active", Some(&r), now).0,
            ResolverHealth::Healthy
        );
    }

    #[test]
    fn missing_or_old_reports_are_not_reporting() {
        let now = Utc::now();
        assert_eq!(
            health(3, "active", None, now).0,
            ResolverHealth::NotReporting
        );
        let old = report("ok", now - chrono::Duration::seconds(91));
        let (h, reason) = health(3, "active", Some(&old), now);
        assert_eq!(h, ResolverHealth::NotReporting);
        assert!(reason.unwrap().contains("91 seconds"));
        let mut unreachable = report("unreachable", now);
        unreachable.last_ok_at = None;
        unreachable.last_error = Some("connection refused".into());
        let (h, reason) = health(3, "active", Some(&unreachable), now);
        assert_eq!(h, ResolverHealth::NotReporting);
        assert_eq!(reason.as_deref(), Some("connection refused"));
        // One failed pull after a recent good one keeps the last state.
        unreachable.last_ok_at = Some(now - chrono::Duration::seconds(30));
        assert_eq!(
            health(3, "active", Some(&unreachable), now).0,
            ResolverHealth::Healthy
        );
    }

    #[test]
    fn an_old_agent_falls_back_to_its_heartbeat() {
        let now = Utc::now();
        let r = report("agent_too_old", now);
        assert_eq!(
            health(3, "active", Some(&r), now).0,
            ResolverHealth::AgentTooOld
        );
        let down = resolver_health(&HealthInput {
            node_id: 3,
            cluster_dns_enabled: true,
            node_status: "active",
            report: Some(&r),
            heartbeat_running: Some(false),
            interval_secs: 30,
            now,
        });
        assert_eq!(down.0, ResolverHealth::Down);
    }

    #[test]
    fn resolver_down_needs_two_evaluations() {
        let none = RecentFailures::default();
        let once = evaluate_alerts(ResolverHealth::Down, Some("x"), 1, false, &none);
        assert_eq!(once.resolver_down, None);
        let twice = evaluate_alerts(ResolverHealth::Down, Some("x"), 2, false, &none);
        assert_eq!(twice.resolver_down.as_deref(), Some("x"));
        // A worker that does not report is the node-offline alarm's job; the
        // control plane's own reporter going quiet is this one's.
        let worker = evaluate_alerts(ResolverHealth::NotReporting, Some("x"), 3, false, &none);
        assert_eq!(worker.resolver_down, None);
        let cp = evaluate_alerts(ResolverHealth::NotReporting, Some("x"), 2, true, &none);
        assert!(cp.resolver_down.is_some());
        let stale = evaluate_alerts(ResolverHealth::Stale, Some("y"), 0, false, &none);
        assert_eq!(stale.sync_stale.as_deref(), Some("y"));
        assert_eq!(stale.resolver_down, None);
        let healthy = evaluate_alerts(ResolverHealth::Healthy, None, 0, false, &none);
        assert_eq!(healthy, AlertDecision::default());
    }

    #[test]
    fn a_control_plane_that_never_ran_a_resolver_is_expected_and_never_alarms() {
        // A hosted plane: no containers run on the control plane, so its
        // resolver (and the reporter that starts with it) never started.
        let now = Utc::now();
        let (h, reason) = health(0, "active", None, now);
        assert_eq!(h, ResolverHealth::NoLocalResolver);
        assert_eq!(h.as_str(), "no_local_resolver");
        assert!(reason.unwrap().contains("no containers run on it"));
        let none = RecentFailures::default();
        let decision = evaluate_alerts(h, Some("x"), 10, true, &none);
        assert_eq!(decision, AlertDecision::default());
        // Once it has reported, going quiet is an outage again.
        let old = report("ok", now - chrono::Duration::seconds(120));
        assert_eq!(
            health(0, "active", Some(&old), now).0,
            ResolverHealth::NotReporting
        );
    }

    #[test]
    fn servfail_spikes_need_a_count_and_a_share() {
        let few = RecentFailures {
            internal_total: 20,
            servfail: SERVFAIL_MIN - 1,
            ..Default::default()
        };
        assert_eq!(
            evaluate_alerts(ResolverHealth::Healthy, None, 0, false, &few).query_failures,
            None
        );
        let diluted = RecentFailures {
            internal_total: 10_000,
            servfail: 20,
            ..Default::default()
        };
        assert_eq!(
            evaluate_alerts(ResolverHealth::Healthy, None, 0, false, &diluted).query_failures,
            None,
            "20 of 10,000 is under 5%"
        );
        let spike = RecentFailures {
            internal_total: 100,
            servfail: 12,
            servfail_names: vec![("primary.orders.temps.local".into(), 12)],
            ..Default::default()
        };
        let message = evaluate_alerts(ResolverHealth::Healthy, None, 0, false, &spike)
            .query_failures
            .unwrap();
        assert!(message.contains("12 of 100"), "{message}");
        assert!(
            message.contains("primary.orders.temps.local (12)"),
            "{message}"
        );
    }

    #[test]
    fn nxdomain_alarms_only_for_names_that_should_exist() {
        let typos = RecentFailures {
            internal_total: 100,
            expected_nxdomain: vec![],
            ..Default::default()
        };
        assert_eq!(
            evaluate_alerts(ResolverHealth::Healthy, None, 0, false, &typos).query_failures,
            None
        );
        let missing = RecentFailures {
            internal_total: 100,
            expected_nxdomain: vec![
                ("production.shop.temps.local".into(), 4),
                ("orders.temps.local".into(), 2),
            ],
            ..Default::default()
        };
        let message = evaluate_alerts(ResolverHealth::Healthy, None, 0, false, &missing)
            .query_failures
            .unwrap();
        assert!(message.contains("6 NXDOMAIN"), "{message}");
        assert!(
            message.starts_with("6 NXDOMAIN")
                && message.contains("production.shop.temps.local (4), orders.temps.local (2)"),
            "{message}"
        );
    }

    #[test]
    fn query_components_are_encoded() {
        assert_eq!(urlencoding_component("abc-123"), "abc-123");
        assert_eq!(urlencoding_component("a b&c"), "a%20b%26c");
    }

    #[test]
    fn problem_details_are_read() {
        assert_eq!(
            problem_detail(br#"{"title":"T","detail":"the reason"}"#),
            "the reason"
        );
        assert_eq!(problem_detail(br#"{"title":"T"}"#), "T");
        assert_eq!(problem_detail(b"plain"), "plain");
    }
}
