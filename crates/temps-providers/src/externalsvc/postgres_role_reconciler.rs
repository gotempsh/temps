//! Postgres cluster role reconciler (ADR-011, Phase 4).
//!
//! Runs as a per-cluster Tokio task on the control plane. Every
//! [`TICK_INTERVAL`], it:
//!
//! 1. Looks up the cluster's monitor in `service_members` by
//!    `(service_id, role='monitor')`.
//! 2. Connects to the monitor as `autoctl_node` (trust auth, no password).
//! 3. Queries `pgautofailover.node` to discover the current primary,
//!    secondaries, and their reported states.
//! 4. Writes a single batch of [`EndpointDraft`]s for `owner_kind = service_role`
//!    via `DnsRegistry::replace_endpoints_for_owner`:
//!    - `primary.<svc>.temps.local` → A, TTL 5 (so failover propagates fast)
//!    - `replica.<svc>.temps.local` → multi-A, TTL 30
//!    - `<svc>.temps.local` → multi-A across all healthy data members,
//!      TTL 30
//!
//! ## Why one batch
//!
//! `replace_endpoints_for_owner` is atomic — apps never see "primary
//! removed but new primary not yet inserted". Doing one call per record
//! type would let resolvers observe transient bad states.
//!
//! ## Why this lives in `temps-providers` not `temps-dns`
//!
//! The reconciler reads from the Postgres monitor (a `pg_auto_failover`
//! detail) and writes to `DnsRegistry`. Putting it in `temps-providers`
//! keeps the engine-specific knowledge co-located with the rest of the
//! cluster code, while `temps-dns` stays engine-agnostic.

use std::collections::HashMap;
use std::str::FromStr;
use std::sync::Arc;
use std::time::Duration;

use sea_orm::{ColumnTrait, DatabaseConnection, EntityTrait, QueryFilter};
use temps_dns::{DnsRegistry, EndpointDraft, InternalOwnerKind, InternalRecordType};
use temps_entities::service_members;
use thiserror::Error;
use tokio::sync::Notify;
use tracing::{debug, info, warn};

/// How often the reconciler queries the monitor. 5 s strikes a balance:
/// short enough that primary promotion → DNS flip is bounded under
/// 6 s end-to-end (add the ~1 s sync long-poll on each agent), long
/// enough not to hammer the monitor or churn `service_endpoints`
/// when nothing's changed.
pub const TICK_INTERVAL: Duration = Duration::from_secs(5);

/// TTLs (seconds). Primary is short so apps recover fast; replicas and
/// VIPs can be longer because their members change less often.
const PRIMARY_TTL: i32 = 5;
const REPLICA_TTL: i32 = 30;
const VIP_TTL: i32 = 30;

/// One row out of `pgautofailover.node`. We only carry the fields we need.
#[derive(Debug, Clone, PartialEq)]
pub struct MonitorNode {
    pub nodeid: i64,
    pub nodename: String,
    pub nodehost: String,
    pub nodeport: i32,
    /// pg_auto_failover reported state — one of:
    /// `primary`, `single`, `secondary`, `wait_primary`, `catchingup`,
    /// `apply_settings`, `draining`, `demoted`, …
    pub reported_state: String,
}

impl MonitorNode {
    /// Parsed view of `reported_state`. Always succeeds — unknown values
    /// land in [`PgAutoFailoverState::Other`] and are correctly treated as
    /// non-primary, non-secondary.
    pub fn state(&self) -> super::PgAutoFailoverState {
        self.reported_state.parse().expect("Infallible")
    }

    fn is_primary(&self) -> bool {
        self.state().is_primary()
    }

    fn is_secondary(&self) -> bool {
        self.state().is_secondary()
    }

    fn is_data_member(&self) -> bool {
        self.state().is_data_member()
    }
}

#[derive(Error, Debug)]
pub enum ReconcilerError {
    #[error("Monitor not found for cluster service {service_id}")]
    MonitorMissing { service_id: i32 },

    #[error("Monitor for cluster service {service_id} has no compute_ip yet")]
    MonitorNotReady { service_id: i32 },

    #[error("Failed to connect to monitor at {host}:{port} for service {service_id}: {reason}")]
    MonitorConnect {
        service_id: i32,
        host: String,
        port: i32,
        reason: String,
    },

    #[error("Monitor query failed for service {service_id}: {reason}")]
    MonitorQuery { service_id: i32, reason: String },

    #[error("Database error: {0}")]
    Database(#[from] sea_orm::DbErr),

    #[error("DNS registry error: {0}")]
    Registry(#[from] temps_dns::DnsRegistryError),
}

/// Build the desired set of role/VIP endpoint drafts from a cluster
/// snapshot. Pure function — no IO — so unit tests can drive it directly.
///
/// Returns drafts that should be passed to
/// `DnsRegistry::replace_endpoints_for_owner(ServiceRole, service_id, …)`.
pub fn drafts_for_snapshot(
    service_id: i32,
    service_name: &str,
    monitor_nodes: &[MonitorNode],
    member_ip_by_hostname: &HashMap<String, String>,
) -> Vec<EndpointDraft> {
    let mut drafts: Vec<EndpointDraft> = Vec::new();

    let primary_fqdn = format!("primary.{}.temps.local", service_name);
    let replica_fqdn = format!("replica.{}.temps.local", service_name);
    let vip_fqdn = format!("{}.temps.local", service_name);

    let owner_id = service_id as i64;

    // Primary — singleton record. We pick the first node reporting a
    // primary state; in a healthy cluster there's exactly one. If two
    // appear (mid-failover edge case), we take the first by nodeid for
    // determinism — apps will just retry against whichever still accepts
    // `target_session_attrs=read-write`.
    let primary = monitor_nodes
        .iter()
        .filter(|n| n.is_primary())
        .min_by_key(|n| n.nodeid);
    if let Some(p) = primary {
        if let Some(ip) = lookup_ip(p, member_ip_by_hostname) {
            drafts.push(EndpointDraft {
                fqdn: primary_fqdn.clone(),
                record_type: InternalRecordType::A,
                target_ip: Some(ip.clone()),
                target_port: Some(p.nodeport),
                ttl: PRIMARY_TTL,
                owner_kind: InternalOwnerKind::ServiceRole,
                owner_id,
                node_id: None,
            });
        }
    }

    // Replicas — multi-A, one record per healthy secondary.
    for sec in monitor_nodes.iter().filter(|n| n.is_secondary()) {
        if let Some(ip) = lookup_ip(sec, member_ip_by_hostname) {
            drafts.push(EndpointDraft {
                fqdn: replica_fqdn.clone(),
                record_type: InternalRecordType::A,
                target_ip: Some(ip),
                target_port: Some(sec.nodeport),
                ttl: REPLICA_TTL,
                owner_kind: InternalOwnerKind::ServiceRole,
                owner_id,
                node_id: None,
            });
        }
    }

    // VIP — multi-A across every data member. libpq's
    // `target_session_attrs=read-write` lands writes on the primary;
    // simple read connections fan out across the set.
    for n in monitor_nodes.iter().filter(|n| n.is_data_member()) {
        if let Some(ip) = lookup_ip(n, member_ip_by_hostname) {
            drafts.push(EndpointDraft {
                fqdn: vip_fqdn.clone(),
                record_type: InternalRecordType::A,
                target_ip: Some(ip),
                target_port: Some(n.nodeport),
                ttl: VIP_TTL,
                owner_kind: InternalOwnerKind::ServiceRole,
                owner_id,
                node_id: None,
            });
        }
    }

    drafts
}

/// Look up the overlay IP for a monitor-node by either its `nodehost`
/// (if it was registered under an FQDN) or by container name. Returns
/// `None` if no service_member matches — that member is then skipped
/// for this tick, which is acceptable: the next tick re-runs and will
/// pick it up once the lifecycle hook has populated `compute_ip`.
fn lookup_ip(
    node: &MonitorNode,
    member_ip_by_hostname: &HashMap<String, String>,
) -> Option<String> {
    // Try the hostname pg_auto_failover knows for the node first.
    if let Some(ip) = member_ip_by_hostname.get(&node.nodehost) {
        return Some(ip.clone());
    }
    // Fall back to nodename — pg_auto_failover assigns names like
    // "node-1", "node-2"; in practice we don't index by these, so this
    // returns None and the caller drops the record.
    member_ip_by_hostname.get(&node.nodename).cloned()
}

/// Single reconciliation pass. Public for tests; production callers use
/// [`run`].
pub async fn reconcile_once(
    db: &DatabaseConnection,
    registry: &DnsRegistry,
    service_id: i32,
    service_name: &str,
) -> Result<(), ReconcilerError> {
    // ---- 1. Discover monitor + members ----
    let members = service_members::Entity::find()
        .filter(service_members::Column::ServiceId.eq(service_id))
        .all(db)
        .await?;

    let monitor = members
        .iter()
        .find(|m| super::ClusterRole::from_str(&m.role).ok() == Some(super::ClusterRole::Monitor))
        .ok_or(ReconcilerError::MonitorMissing { service_id })?;

    // The monitor must be reachable from the control plane. Resolution
    // priority:
    //   1. `compute_ip` (overlay IP) — only useful if the control plane
    //      itself is on the overlay, which it isn't in the dev cluster
    //      and typically isn't in production either.
    //   2. `nodes.private_address` — the worker's underlay IP, always
    //      reachable from the control plane.
    //   3. "localhost" — single-host clusters where the monitor runs on
    //      the control-plane host and binds the host port.
    //
    // We deliberately do NOT fall back to `member.hostname` (the FQDN)
    // because it only resolves on hosts running the per-node Hickory
    // resolver — and the control plane doesn't run one.
    let monitor_host: String = if let Some(ip) = monitor.compute_ip.as_deref() {
        ip.to_string()
    } else if let Some(node_id) = monitor.node_id {
        match temps_entities::nodes::Entity::find_by_id(node_id)
            .one(db)
            .await
        {
            Ok(Some(n)) => n.private_address,
            _ => {
                return Err(ReconcilerError::MonitorNotReady { service_id });
            }
        }
    } else {
        "localhost".to_string()
    };
    let monitor_port = monitor.port.unwrap_or(5432);

    // Build {nodehost → compute_ip} for the role-record DNS targets.
    // pg_auto_failover's `nodehost` is the worker's underlay IP
    // (e.g. `10.42.0.21`), NOT the FQDN — confirmed by inspecting
    // `pg_stat_replication.client_addr`. So we index by `node.private_address`,
    // not `member.hostname` (which IS the FQDN). Without this the
    // monitor-row → compute_ip join misses every member and the role
    // records never get IPs.
    //
    // We also index by the FQDN as a backup, in case a future setup
    // does report nodehost as FQDN.
    let mut ip_by_hostname: HashMap<String, String> = HashMap::new();
    for m in &members {
        if super::ClusterRole::from_str(&m.role).ok() == Some(super::ClusterRole::Monitor) {
            continue;
        }
        let Some(ip) = &m.compute_ip else { continue };
        // Primary lookup: node.private_address (matches pg_auto_failover's nodehost)
        if let Some(node_id) = m.node_id {
            if let Ok(Some(n)) = temps_entities::nodes::Entity::find_by_id(node_id)
                .one(db)
                .await
            {
                ip_by_hostname.insert(n.private_address, ip.clone());
            }
        }
        // Backup lookup: FQDN (covers single-host clusters and any future
        // setup where pg_auto_failover is configured with FQDN nodehost).
        if let Some(host) = &m.hostname {
            ip_by_hostname.insert(host.clone(), ip.clone());
        }
    }

    // ---- 2. Query monitor ----
    let monitor_nodes = match query_monitor(service_id, &monitor_host, monitor_port).await {
        Ok(rows) => rows,
        Err(e) => return Err(e),
    };

    debug!(
        service_id,
        service_name,
        monitor_host,
        monitor_port,
        nodes = monitor_nodes.len(),
        "reconciler observed monitor state"
    );

    // ---- 2a. Sync service_members.role to match the monitor ----
    //
    // `service_members.role` is what the UI reads, but until now we
    // only wrote it at create time. After a failover, pg_auto_failover
    // updates its own state but our table stays stale. Sync each row
    // here so the Cluster Members table reflects reality on the next
    // tick (≤5s after a failover).
    //
    // We map monitor nodes → service_members rows by `nodehost` (which
    // is the worker's underlay IP) → `node.private_address`. Same join
    // we already do for the IP map below.
    sync_member_roles(db, service_id, &members, &monitor_nodes).await;

    // ---- 3. Compute desired records + apply ----
    let drafts = drafts_for_snapshot(service_id, service_name, &monitor_nodes, &ip_by_hostname);

    if drafts.is_empty() {
        // Nothing to advertise yet (no IPs known). delete_by_owner so any
        // stale role records from a previous tick get cleared.
        let _ = registry
            .delete_by_owner(InternalOwnerKind::ServiceRole, service_id as i64)
            .await;
        return Ok(());
    }

    registry
        .replace_endpoints_for_owner(InternalOwnerKind::ServiceRole, service_id as i64, &drafts)
        .await?;

    info!(
        service_id,
        service_name,
        records = drafts.len(),
        "reconciled cluster role/VIP DNS records"
    );

    Ok(())
}

/// Bring `service_members.role` in line with what the monitor reports.
/// pg_auto_failover is the source of truth for who's primary; this
/// function keeps our row labels consistent so the UI's Cluster Members
/// table doesn't go stale after a failover.
///
/// Legacy cleanup pass: demote any row that still has
/// `role = 'primary'` to `replica`.
///
/// `service_members.role` is now config state — `monitor` for the
/// orchestrator, `replica` for every data node. Runtime "is this the
/// primary?" comes from the monitor via `ServiceMemberInfo.live_state`.
/// Rows from clusters created before that change carry stale `primary`
/// values; this function writes them back to `replica` once and is
/// otherwise a no-op.
///
/// `monitor_nodes` is unused now and kept only so the reconciler call
/// site doesn't need to be reshuffled.
async fn sync_member_roles(
    db: &DatabaseConnection,
    service_id: i32,
    members: &[temps_entities::service_members::Model],
    _monitor_nodes: &[MonitorNode],
) {
    use sea_orm::ActiveModelTrait;
    use sea_orm::ActiveValue::Set;

    for m in members {
        // Only touch rows that still carry the legacy `primary` value.
        // Anything else is left alone — `replica`, `monitor`, and any
        // unknown future role are valid stored states.
        if m.role != "primary" {
            continue;
        }

        info!(
            service_id,
            member_id = m.id,
            container = %m.container_name,
            "Demoting legacy 'primary' role to 'replica' — runtime role now lives in live_state"
        );

        let mut active: temps_entities::service_members::ActiveModel = m.clone().into();
        active.role = Set("replica".to_string());
        active.updated_at = Set(chrono::Utc::now());
        if let Err(e) = active.update(db).await {
            warn!(
                service_id,
                member_id = m.id,
                error = %e,
                "Failed to demote legacy primary; will retry next tick"
            );
        }
    }
}

/// Walk an error's `source()` chain and concatenate. `tokio_postgres::Error`
/// hides its real cause behind a brief `db error` tag — without this,
/// reconciler error logs say only `Failed to connect ...: db error`.
fn format_pg_error<E: std::error::Error>(err: &E) -> String {
    let mut out = err.to_string();
    let mut cause: Option<&dyn std::error::Error> = err.source();
    while let Some(c) = cause {
        let s = c.to_string();
        if !s.is_empty() {
            out.push_str(": ");
            out.push_str(&s);
        }
        cause = c.source();
    }
    out
}

/// Open a fresh tokio-postgres connection per tick and read the monitor.
/// Per-tick connections (no pool) are intentional: `tokio_postgres` keeps
/// the spawned `Connection` alive only as long as the handle is held, and
/// monitor traffic is minuscule (one SELECT every 5 s per cluster). The
/// alternative — a long-lived connection — would need separate liveness
/// management for failures we already handle by simply retrying next tick.
async fn query_monitor(
    service_id: i32,
    host: &str,
    port: i32,
) -> Result<Vec<MonitorNode>, ReconcilerError> {
    // pg_auto_failover only opens hba for `autoctl_node` over SSL
    // (`hostssl pg_auto_failover autoctl_node 0.0.0.0/0 trust`). Plain
    // TCP gets rejected with "no pg_hba.conf entry". Use the same
    // self-signed-accepting TLS connector the cluster_health probe uses.
    let conn_str = format!(
        "host={host} port={port} user=autoctl_node dbname=pg_auto_failover \
         sslmode=require connect_timeout=3"
    );
    let client = temps_query_postgres::connect_with_self_signed_tls(&conn_str)
        .await
        .map_err(|e| ReconcilerError::MonitorConnect {
            service_id,
            host: host.to_string(),
            port,
            reason: format_pg_error(&e),
        })?;

    let rows = client
        .query(
            // Cast `reportedstate` to text — pg_auto_failover stores it as
            // a typed enum (`pgautofailover.replication_state`) that
            // tokio-postgres can't auto-deserialize to String, leading to
            // "error retrieving column reportedstate: error deserializing
            // column 4". The cast forces wire-format text.
            "SELECT nodeid::bigint, nodename, nodehost, \
                    nodeport::int, reportedstate::text \
             FROM pgautofailover.node",
            &[],
        )
        .await;

    // Drop the client to close the connection cleanly. The driver task
    // is owned by `connect_with_self_signed_tls` and exits on drop.
    drop(client);

    let rows = rows.map_err(|e| ReconcilerError::MonitorQuery {
        service_id,
        reason: format_pg_error(&e),
    })?;

    let mut out = Vec::with_capacity(rows.len());
    for row in rows {
        // pg_auto_failover stores reportedstate as a typed enum
        // `pgautofailover.replication_state`. tokio-postgres receives it
        // as text by default — so we do the read as TEXT.
        let nodeid: i64 = row.get("nodeid");
        let nodename: String = row.get("nodename");
        let nodehost: String = row.get("nodehost");
        let nodeport: i32 = row.get("nodeport");
        let reported_state: String = row.get("reportedstate");
        out.push(MonitorNode {
            nodeid,
            nodename,
            nodehost,
            nodeport,
            reported_state,
        });
    }
    Ok(out)
}

/// Long-running reconciler loop. Exits when `shutdown` is notified.
pub async fn run(
    db: Arc<DatabaseConnection>,
    registry: Arc<DnsRegistry>,
    service_id: i32,
    service_name: String,
    shutdown: Arc<Notify>,
) {
    info!(service_id, service_name, "starting role reconciler");
    loop {
        if let Err(e) = reconcile_once(&db, &registry, service_id, &service_name).await {
            // All errors are transient retries. Log at WARN, sleep, try
            // again. The only thing that stops the loop is shutdown.
            warn!(
                service_id,
                service_name,
                error = %e,
                "reconciler tick failed; will retry"
            );
        }
        tokio::select! {
            _ = tokio::time::sleep(TICK_INTERVAL) => {}
            _ = shutdown.notified() => {
                info!(service_id, service_name, "role reconciler shutting down");
                return;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn node(id: i64, host: &str, port: i32, state: &str) -> MonitorNode {
        MonitorNode {
            nodeid: id,
            nodename: format!("node-{}", id),
            nodehost: host.to_string(),
            nodeport: port,
            reported_state: state.to_string(),
        }
    }

    fn ip_map(pairs: &[(&str, &str)]) -> HashMap<String, String> {
        pairs
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect()
    }

    #[test]
    fn drafts_emit_primary_replica_and_vip() {
        let monitor = vec![
            node(1, "orders-1.orders.temps.local", 6001, "primary"),
            node(2, "orders-2.orders.temps.local", 6001, "secondary"),
            node(3, "orders-3.orders.temps.local", 6001, "secondary"),
        ];
        let ips = ip_map(&[
            ("orders-1.orders.temps.local", "172.20.5.10"),
            ("orders-2.orders.temps.local", "172.20.5.11"),
            ("orders-3.orders.temps.local", "172.20.5.12"),
        ]);

        let drafts = drafts_for_snapshot(42, "orders", &monitor, &ips);

        // 1 primary + 2 replicas + 3 VIP A records
        assert_eq!(drafts.len(), 6, "expected 6 records, got: {:#?}", drafts);

        let primary = drafts
            .iter()
            .find(|d| d.fqdn == "primary.orders.temps.local")
            .expect("primary record");
        assert_eq!(primary.target_ip.as_deref(), Some("172.20.5.10"));
        assert_eq!(primary.ttl, PRIMARY_TTL);

        let replicas: Vec<_> = drafts
            .iter()
            .filter(|d| d.fqdn == "replica.orders.temps.local")
            .collect();
        assert_eq!(replicas.len(), 2);
        for r in &replicas {
            assert_eq!(r.ttl, REPLICA_TTL);
        }

        let vip: Vec<_> = drafts
            .iter()
            .filter(|d| d.fqdn == "orders.temps.local")
            .collect();
        assert_eq!(vip.len(), 3);
    }

    #[test]
    fn drafts_skip_nodes_without_known_ip() {
        let monitor = vec![
            node(1, "orders-1.orders.temps.local", 6001, "primary"),
            node(2, "orders-2.orders.temps.local", 6001, "secondary"),
        ];
        // Replica's IP isn't known yet (lifecycle hook hasn't populated it).
        let ips = ip_map(&[("orders-1.orders.temps.local", "172.20.5.10")]);

        let drafts = drafts_for_snapshot(1, "orders", &monitor, &ips);
        // Only the primary + VIP-of-the-primary survive.
        assert_eq!(drafts.len(), 2);
        assert!(drafts
            .iter()
            .any(|d| d.fqdn == "primary.orders.temps.local"));
        assert!(drafts.iter().any(|d| d.fqdn == "orders.temps.local"));
        assert!(!drafts
            .iter()
            .any(|d| d.fqdn == "replica.orders.temps.local"));
    }

    #[test]
    fn drafts_treat_single_state_as_primary() {
        // 1-node clusters report 'single' from pg_auto_failover.
        let monitor = vec![node(1, "solo.solo.temps.local", 6001, "single")];
        let ips = ip_map(&[("solo.solo.temps.local", "172.20.5.99")]);

        let drafts = drafts_for_snapshot(7, "solo", &monitor, &ips);
        // primary + VIP, no replicas.
        assert_eq!(drafts.len(), 2);
        assert!(drafts.iter().any(|d| d.fqdn == "primary.solo.temps.local"));
        assert!(!drafts.iter().any(|d| d.fqdn == "replica.solo.temps.local"));
    }

    #[test]
    fn drafts_pick_lowest_nodeid_when_two_primaries_visible() {
        // Mid-failover edge case: monitor briefly reports two primaries.
        // We pick the lower nodeid for determinism.
        let monitor = vec![
            node(2, "b.svc.temps.local", 6001, "primary"),
            node(1, "a.svc.temps.local", 6001, "primary"),
        ];
        let ips = ip_map(&[
            ("a.svc.temps.local", "172.20.5.1"),
            ("b.svc.temps.local", "172.20.5.2"),
        ]);
        let drafts = drafts_for_snapshot(1, "svc", &monitor, &ips);
        let primary = drafts
            .iter()
            .find(|d| d.fqdn == "primary.svc.temps.local")
            .expect("primary record");
        assert_eq!(primary.target_ip.as_deref(), Some("172.20.5.1"));
    }

    #[test]
    fn drafts_empty_when_no_data_members() {
        // All members are draining/demoted/etc. — VIP set is empty.
        let monitor = vec![
            node(1, "x.svc.temps.local", 6001, "draining"),
            node(2, "y.svc.temps.local", 6001, "demoted"),
        ];
        let ips = ip_map(&[
            ("x.svc.temps.local", "1.1.1.1"),
            ("y.svc.temps.local", "2.2.2.2"),
        ]);
        let drafts = drafts_for_snapshot(1, "svc", &monitor, &ips);
        assert!(drafts.is_empty());
    }

    #[test]
    fn failover_flips_primary_record_to_promoted_replica() {
        // Initial state: node-1 is primary, node-2 is replica.
        let before = vec![
            node(1, "orders-1.orders.temps.local", 6001, "primary"),
            node(2, "orders-2.orders.temps.local", 6001, "secondary"),
        ];
        // After failover: node-1 demoted (draining), node-2 promoted.
        let after = vec![
            node(1, "orders-1.orders.temps.local", 6001, "draining"),
            node(2, "orders-2.orders.temps.local", 6001, "primary"),
        ];
        let ips = ip_map(&[
            ("orders-1.orders.temps.local", "172.20.5.10"),
            ("orders-2.orders.temps.local", "172.20.5.11"),
        ]);

        let drafts_before = drafts_for_snapshot(1, "orders", &before, &ips);
        let drafts_after = drafts_for_snapshot(1, "orders", &after, &ips);

        let primary_before = drafts_before
            .iter()
            .find(|d| d.fqdn == "primary.orders.temps.local")
            .unwrap();
        let primary_after = drafts_after
            .iter()
            .find(|d| d.fqdn == "primary.orders.temps.local")
            .unwrap();

        assert_eq!(primary_before.target_ip.as_deref(), Some("172.20.5.10"));
        assert_eq!(primary_after.target_ip.as_deref(), Some("172.20.5.11"));
        assert_ne!(
            primary_before.target_ip, primary_after.target_ip,
            "primary record must flip to promoted replica's IP"
        );

        // VIP should also drop the draining node from the multi-A set.
        let vip_after_ips: Vec<_> = drafts_after
            .iter()
            .filter(|d| d.fqdn == "orders.temps.local")
            .filter_map(|d| d.target_ip.as_deref())
            .collect();
        assert_eq!(vip_after_ips, vec!["172.20.5.11"]);
    }

    #[test]
    fn monitor_node_is_primary_handles_known_states() {
        assert!(node(1, "h", 1, "primary").is_primary());
        assert!(node(1, "h", 1, "single").is_primary());
        assert!(!node(1, "h", 1, "secondary").is_primary());
        assert!(!node(1, "h", 1, "wait_primary").is_primary());
    }

    #[test]
    fn monitor_node_is_secondary_handles_known_states() {
        assert!(node(1, "h", 1, "secondary").is_secondary());
        assert!(node(1, "h", 1, "catchingup").is_secondary());
        assert!(node(1, "h", 1, "apply_settings").is_secondary());
        assert!(!node(1, "h", 1, "primary").is_secondary());
    }
}
