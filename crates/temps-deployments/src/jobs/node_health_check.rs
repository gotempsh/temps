//! Periodic job that checks node health, marks stale nodes as offline,
//! and triggers failover redeployment for affected environments.
//!
//! Runs on the control plane every 60 seconds. Nodes that haven't sent
//! a heartbeat in >90 seconds are marked offline. When a node transitions
//! to offline, its affected environments are automatically redeployed
//! to healthy nodes.

use sea_orm::{ColumnTrait, DatabaseConnection, EntityTrait, QueryFilter};

use temps_entities::nodes;

use crate::services::node_service::NodeService;
use crate::DeploymentService;

/// Threshold in seconds — nodes with older heartbeats are marked offline.
const HEARTBEAT_STALE_THRESHOLD_SECS: i64 = 90;

/// Runs a single health check pass across all active nodes.
///
/// This is designed to be called by a scheduler (e.g., every 60 seconds).
/// It does NOT run in a loop itself.
///
/// Returns the list of node IDs that were marked offline (for failover).
pub async fn check_node_health(node_service: &NodeService, db: &DatabaseConnection) -> Vec<i32> {
    let cutoff = chrono::Utc::now() - chrono::Duration::seconds(HEARTBEAT_STALE_THRESHOLD_SECS);

    // Find nodes that are still marked "active" but have a stale heartbeat
    let stale_nodes = match nodes::Entity::find()
        .filter(nodes::Column::Status.eq("active"))
        .filter(
            nodes::Column::LastHeartbeat
                .lt(cutoff)
                .or(nodes::Column::LastHeartbeat.is_null()),
        )
        .all(db)
        .await
    {
        Ok(nodes) => nodes,
        Err(e) => {
            tracing::error!("Failed to query nodes for health check: {}", e);
            return vec![];
        }
    };

    let mut marked_offline = Vec::new();

    for node in &stale_nodes {
        tracing::warn!(
            node_id = node.id,
            node_name = %node.name,
            last_heartbeat = ?node.last_heartbeat,
            "Node heartbeat stale, marking as offline"
        );

        if let Err(e) = node_service.mark_offline(node.id).await {
            tracing::error!(
                node_id = node.id,
                node_name = %node.name,
                "Failed to mark node as offline: {}",
                e
            );
        } else {
            marked_offline.push(node.id);
        }
    }

    if !marked_offline.is_empty() {
        tracing::info!(
            count = marked_offline.len(),
            "Node health check completed: marked {} node(s) offline",
            marked_offline.len()
        );
    }

    marked_offline
}

/// Notify operators that worker nodes went offline (ADR-020 / monitoring).
///
/// Called by the health-check loop right after `check_node_health` marks
/// nodes offline. Sends one Alert-priority notification per node through the
/// shared notification pipeline (email / Slack / webhook, per the operator's
/// configured channels). A node is only marked offline on the active->offline
/// transition, so this fires exactly once per outage — no repeat spam while a
/// node stays down. Best-effort: delivery failures are logged, never fatal.
pub async fn notify_nodes_offline(
    offline_node_ids: &[i32],
    node_service: &NodeService,
    notification_service: &std::sync::Arc<dyn temps_core::notifications::NotificationService>,
) {
    use temps_core::notifications::{NotificationData, NotificationPriority, NotificationType};

    for &node_id in offline_node_ids {
        let name = node_service
            .get_by_id(node_id)
            .await
            .map(|n| n.name)
            .unwrap_or_else(|_| format!("node-{}", node_id));

        let mut metadata = std::collections::HashMap::new();
        metadata.insert("event".to_string(), "node_offline".to_string());
        metadata.insert("node_id".to_string(), node_id.to_string());
        metadata.insert("node_name".to_string(), name.clone());

        let notification = NotificationData {
            title: format!("Worker node '{}' is offline", name),
            message: format!(
                "Node '{}' (id {}) stopped sending heartbeats for over {}s and was marked offline. \
                 Affected workloads are being failed over to healthy nodes.",
                name, node_id, HEARTBEAT_STALE_THRESHOLD_SECS
            ),
            notification_type: NotificationType::Alert,
            priority: NotificationPriority::Critical,
            ..Default::default()
        };

        match notification_service.send_notification(notification).await {
            Ok(()) => tracing::info!(node_id, node_name = %name, "Sent node-offline alert"),
            Err(e) => tracing::error!(
                node_id,
                node_name = %name,
                "Failed to send node-offline alert: {}",
                e
            ),
        }
    }
}

/// Notify operators that a worker node came back online (ADR-020 / monitoring).
///
/// Called from the heartbeat handler when a node transitions offline->active
/// (the `was_offline` signal). Sends one Info-priority notification through the
/// shared notification pipeline — the recovery counterpart to
/// [`notify_nodes_offline`]. Best-effort: failures are logged, never fatal.
pub async fn notify_node_recovered(
    node_id: i32,
    node_name: &str,
    notification_service: &std::sync::Arc<dyn temps_core::notifications::NotificationService>,
) {
    use temps_core::notifications::{NotificationData, NotificationPriority, NotificationType};

    let mut metadata = std::collections::HashMap::new();
    metadata.insert("event".to_string(), "node_recovered".to_string());
    metadata.insert("node_id".to_string(), node_id.to_string());
    metadata.insert("node_name".to_string(), node_name.to_string());

    let notification = NotificationData {
        title: format!("Worker node '{}' is back online", node_name),
        message: format!(
            "Node '{}' (id {}) resumed sending heartbeats and was marked active again.",
            node_name, node_id
        ),
        notification_type: NotificationType::Info,
        priority: NotificationPriority::Normal,
        metadata,
        ..Default::default()
    };

    match notification_service.send_notification(notification).await {
        Ok(()) => tracing::info!(node_id, node_name = %node_name, "Sent node-recovery alert"),
        Err(e) => tracing::error!(
            node_id,
            node_name = %node_name,
            "Failed to send node-recovery alert: {}",
            e
        ),
    }
}

/// Send one resource-pressure alert. The title is STABLE (value lives in the
/// body) so the notification pipeline's batch-key throttle collapses repeated
/// alerts for the same node+metric instead of firing every 60s cycle.
async fn send_node_resource_alert(
    notification_service: &std::sync::Arc<dyn temps_core::notifications::NotificationService>,
    node_name: &str,
    node_id: i32,
    metric: &str,
    value: f64,
    threshold: f64,
) {
    use temps_core::notifications::{NotificationData, NotificationPriority, NotificationType};
    let mut metadata = std::collections::HashMap::new();
    metadata.insert("event".to_string(), "node_resource_high".to_string());
    metadata.insert("node_id".to_string(), node_id.to_string());
    metadata.insert("node_name".to_string(), node_name.to_string());
    metadata.insert("metric".to_string(), metric.to_string());
    metadata.insert("value_percent".to_string(), format!("{:.1}", value));
    metadata.insert("threshold_percent".to_string(), format!("{:.0}", threshold));
    let notification = NotificationData {
        title: format!("Worker node '{}' {} usage is high", node_name, metric),
        message: format!(
            "{} on node '{}' (id {}) is at {:.1}% (alert threshold {:.0}%).",
            metric, node_name, node_id, value, threshold
        ),
        notification_type: NotificationType::Warning,
        priority: NotificationPriority::High,
        metadata,
        ..Default::default()
    };
    if let Err(e) = notification_service.send_notification(notification).await {
        tracing::error!(node_id, metric, "Failed to send node resource alert: {}", e);
    } else {
        tracing::info!(node_id, node_name = %node_name, metric, value, "Sent node resource alert");
    }
}

/// Check active nodes' resource usage (CPU / memory / disk from the heartbeat
/// `capacity`) against operator-configurable thresholds, alerting on breaches
/// (ADR-020 / monitoring). Runs in the 60s health loop after the offline check.
/// Best-effort: query/delivery failures are logged, never fatal.
pub async fn check_node_resources(
    db: &DatabaseConnection,
    config_service: &temps_config::ConfigService,
    notification_service: &std::sync::Arc<dyn temps_core::notifications::NotificationService>,
) {
    // Operator-configurable thresholds (settings.multi_node.node_*_alert_percent);
    // `None` disables alerting for that metric. Defaults to 90%.
    let settings = match config_service.get_settings().await {
        Ok(s) => s,
        Err(e) => {
            tracing::error!("Node resource check: failed to read settings: {}", e);
            return;
        }
    };
    let cpu_threshold = settings.multi_node.node_cpu_alert_percent;
    let mem_threshold = settings.multi_node.node_memory_alert_percent;
    let disk_threshold = settings.multi_node.node_disk_alert_percent;
    if cpu_threshold.is_none() && mem_threshold.is_none() && disk_threshold.is_none() {
        return; // all node resource alerts disabled
    }

    let active = match nodes::Entity::find()
        .filter(nodes::Column::Status.eq("active"))
        .all(db)
        .await
    {
        Ok(n) => n,
        Err(e) => {
            tracing::error!("Node resource check: failed to query active nodes: {}", e);
            return;
        }
    };

    for node in &active {
        for (metric, value, threshold) in
            resource_breaches(&node.capacity, cpu_threshold, mem_threshold, disk_threshold)
        {
            send_node_resource_alert(
                notification_service,
                &node.name,
                node.id,
                metric,
                value,
                threshold,
            )
            .await;
        }
    }
}

/// Pure threshold evaluation for a node's heartbeat `capacity` JSON. Returns
/// `(metric, value_percent, threshold)` for each breach (value strictly above
/// threshold). `None` thresholds and missing/zero capacity fields are skipped.
/// Extracted from `check_node_resources` so the breach logic is unit-testable
/// without a DB / config / notification service.
fn resource_breaches(
    capacity: &serde_json::Value,
    cpu_threshold: Option<f64>,
    mem_threshold: Option<f64>,
    disk_threshold: Option<f64>,
) -> Vec<(&'static str, f64, f64)> {
    let mut breaches = Vec::new();

    if let Some(threshold) = cpu_threshold {
        if let Some(cpu) = capacity.get("cpu_percent").and_then(|v| v.as_f64()) {
            if cpu > threshold {
                breaches.push(("CPU", cpu, threshold));
            }
        }
    }

    let pct = |used_key: &str, total_key: &str| -> Option<f64> {
        let used = capacity.get(used_key).and_then(|v| v.as_f64())?;
        let total = capacity.get(total_key).and_then(|v| v.as_f64())?;
        (total > 0.0).then(|| used / total * 100.0)
    };

    if let Some(threshold) = mem_threshold {
        if let Some(pct) = pct("memory_used_bytes", "memory_total_bytes") {
            if pct > threshold {
                breaches.push(("memory", pct, threshold));
            }
        }
    }

    if let Some(threshold) = disk_threshold {
        if let Some(pct) = pct("disk_used_bytes", "disk_total_bytes") {
            if pct > threshold {
                breaches.push(("disk", pct, threshold));
            }
        }
    }

    breaches
}

// ── Control-plane self-metrics ──────────────────────────────────────────────
// The control plane runs `temps serve`, not `temps agent`, so it never
// heartbeats its own resource usage — the synthetic control-plane node (id 0)
// would otherwise always show empty metrics and never trigger resource alerts.
// The 60s health loop samples the CP host into this in-process cache (the CP's
// host metrics are a true process singleton), which the node-list handler reads
// and `check_control_plane_resources` evaluates for alerts.

/// Synthetic node id for the control plane (mirrors handlers::nodes).
const CONTROL_PLANE_NODE_ID: i32 = 0;

/// A control-plane host-metrics sample: the capacity JSON + when it was taken.
type CpSample = (serde_json::Value, chrono::DateTime<chrono::Utc>);

static CONTROL_PLANE_METRICS: std::sync::OnceLock<std::sync::RwLock<Option<CpSample>>> =
    std::sync::OnceLock::new();

fn control_plane_metrics_cell() -> &'static std::sync::RwLock<Option<CpSample>> {
    CONTROL_PLANE_METRICS.get_or_init(|| std::sync::RwLock::new(None))
}

/// Latest control-plane host metrics (capacity JSON + sample time), or `None`
/// until the first refresh. Read by the synthetic control-plane node response.
pub fn latest_control_plane_metrics() -> Option<CpSample> {
    control_plane_metrics_cell()
        .read()
        .ok()
        .and_then(|g| g.clone())
}

/// Sample the control plane's own host CPU/memory/disk (sysinfo) and cache it in
/// the same shape worker heartbeats use. Cheap; called from the 60s health loop
/// so the control-plane node shows live metrics like any worker.
pub fn refresh_control_plane_metrics() {
    use sysinfo::{Disks, System};

    let mut sys = System::new();
    sys.refresh_cpu_all();
    sys.refresh_memory();
    let disks = Disks::new_with_refreshed_list();

    let cpu_percent = sys.global_cpu_usage() as f64;
    let memory_used_bytes = sys.used_memory();
    let memory_total_bytes = sys.total_memory();
    // Root mount only, to avoid double-counting overlapping mounts (matches the
    // agent's collect_system_metrics).
    let (disk_used, disk_total) = disks
        .list()
        .iter()
        .find(|d| d.mount_point() == std::path::Path::new("/"))
        .map(|d| (d.total_space() - d.available_space(), d.total_space()))
        .unwrap_or((0, 0));

    let capacity = serde_json::json!({
        "cpu_percent": cpu_percent,
        "memory_used_bytes": memory_used_bytes,
        "memory_total_bytes": memory_total_bytes,
        "disk_used_bytes": disk_used,
        "disk_total_bytes": disk_total,
    });

    if let Ok(mut g) = control_plane_metrics_cell().write() {
        *g = Some((capacity, chrono::Utc::now()));
    }
}

/// Evaluate the control plane's own cached metrics against the resource-alert
/// thresholds and notify on breach — same thresholds/logic as worker nodes. The
/// CP isn't a `nodes` row, so `check_node_resources` skips it; this closes that
/// gap. No-op until the first `refresh_control_plane_metrics()`.
pub async fn check_control_plane_resources(
    config_service: &temps_config::ConfigService,
    notification_service: &std::sync::Arc<dyn temps_core::notifications::NotificationService>,
) {
    let Some((capacity, _)) = latest_control_plane_metrics() else {
        return;
    };
    let settings = match config_service.get_settings().await {
        Ok(s) => s,
        Err(e) => {
            tracing::error!(
                "Control-plane resource check: failed to read settings: {}",
                e
            );
            return;
        }
    };
    for (metric, value, threshold) in resource_breaches(
        &capacity,
        settings.multi_node.node_cpu_alert_percent,
        settings.multi_node.node_memory_alert_percent,
        settings.multi_node.node_disk_alert_percent,
    ) {
        send_node_resource_alert(
            notification_service,
            "control-plane",
            CONTROL_PLANE_NODE_ID,
            metric,
            value,
            threshold,
        )
        .await;
    }
}

/// Check all draining nodes for drain completion and transition them
/// to "drained" status when all containers have been migrated.
///
/// This is designed to be called after `check_node_health` in the same
/// periodic job (every 60 seconds). Returns the node IDs that completed.
pub async fn check_drain_completion(node_service: &NodeService) -> Vec<i32> {
    match node_service.check_all_drains().await {
        Ok(completed) => completed,
        Err(e) => {
            tracing::error!("Failed to check drain completion: {}", e);
            vec![]
        }
    }
}

/// Handle failover for nodes that just went offline.
///
/// For each affected deployment:
/// - If other nodes still have healthy replicas, just retire the containers
///   on the offline node (proxy stops routing to them on next refresh).
/// - If ALL replicas were on the offline node, trigger a full redeploy so
///   the workload is rescheduled to a healthy node.
pub async fn failover_offline_nodes(
    offline_node_ids: &[i32],
    node_service: &NodeService,
    deployment_service: &DeploymentService,
) {
    if offline_node_ids.is_empty() {
        return;
    }

    for &node_id in offline_node_ids {
        let affected = match node_service.affected_deployments(node_id).await {
            Ok(deps) => deps,
            Err(e) => {
                tracing::error!(
                    node_id,
                    "Failed to query affected deployments for failover: {}",
                    e
                );
                continue;
            }
        };

        if affected.is_empty() {
            tracing::debug!(node_id, "No affected deployments for offline node");
            continue;
        }

        tracing::warn!(
            node_id,
            affected_count = affected.len(),
            "Failover: processing {} deployment(s) on offline node",
            affected.len()
        );

        for dep in &affected {
            if dep.needs_redeploy() {
                // All replicas were on this node — must redeploy
                match deployment_service
                    .redeploy_environment(dep.project_id, dep.environment_id)
                    .await
                {
                    Ok(_) => {
                        tracing::info!(
                            node_id,
                            project_id = dep.project_id,
                            environment_id = dep.environment_id,
                            "Failover: triggered full redeploy (no healthy replicas elsewhere)"
                        );
                    }
                    Err(e) => {
                        tracing::error!(
                            node_id,
                            project_id = dep.project_id,
                            environment_id = dep.environment_id,
                            "Failover: failed to trigger redeploy: {}",
                            e
                        );
                    }
                }
            } else {
                // Other nodes have healthy replicas — just retire stale containers
                match node_service
                    .retire_containers_on_node(node_id, dep.deployment_id)
                    .await
                {
                    Ok(count) => {
                        tracing::info!(
                            node_id,
                            deployment_id = dep.deployment_id,
                            retired = count,
                            remaining = dep.total_active_containers - dep.containers_on_node,
                            "Failover: retired containers, healthy replicas remain"
                        );
                    }
                    Err(e) => {
                        tracing::error!(
                            node_id,
                            deployment_id = dep.deployment_id,
                            "Failover: failed to retire containers: {}",
                            e
                        );
                    }
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use sea_orm::{DatabaseBackend, MockDatabase};

    fn make_node(id: i32, name: &str, status: &str, heartbeat_age_secs: i64) -> nodes::Model {
        nodes::Model {
            id,
            name: name.to_string(),
            token_hash: "hash".to_string(),
            token_encrypted: None,
            address: format!("https://10.100.0.{}:3100", id),
            private_address: format!("10.100.0.{}", id),
            public_endpoint: None,
            wg_public_key: None,
            role: "worker".to_string(),
            status: status.to_string(),
            labels: serde_json::json!({}),
            capacity: serde_json::json!({}),
            last_heartbeat: Some(
                chrono::Utc::now() - chrono::Duration::seconds(heartbeat_age_secs),
            ),
            edge_public_key: None,
            compute_cidr: None,
            underlay_address: None,
            created_at: chrono::Utc::now(),
            updated_at: chrono::Utc::now(),
        }
    }

    #[test]
    fn test_heartbeat_threshold_is_reasonable() {
        // Heartbeat threshold should be greater than the heartbeat interval (30s)
        // but not so long that stale nodes linger
        const { assert!(HEARTBEAT_STALE_THRESHOLD_SECS > 30) };
        const { assert!(HEARTBEAT_STALE_THRESHOLD_SECS <= 300) };
    }

    #[tokio::test]
    async fn test_check_node_health_no_stale_nodes() {
        // No stale nodes — query returns empty
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results(vec![Vec::<nodes::Model>::new()])
            .into_connection();
        let node_service = NodeService::new(std::sync::Arc::new(
            MockDatabase::new(DatabaseBackend::Postgres).into_connection(),
        ));

        let marked = check_node_health(&node_service, &db).await;
        assert!(marked.is_empty());
    }

    #[tokio::test]
    async fn test_check_node_health_marks_stale_nodes() {
        // Two stale nodes returned by query
        let stale_node_1 = make_node(1, "worker-1", "active", 120);
        let stale_node_2 = make_node(2, "worker-2", "active", 200);

        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results(vec![vec![stale_node_1, stale_node_2]])
            .into_connection();

        // NodeService needs its own mock db for mark_offline calls:
        // - get_by_id for node 1, update for node 1
        // - get_by_id for node 2, update for node 2
        let node_1_for_service = make_node(1, "worker-1", "active", 120);
        let node_1_updated = make_node(1, "worker-1", "offline", 120);
        let node_2_for_service = make_node(2, "worker-2", "active", 200);
        let node_2_updated = make_node(2, "worker-2", "offline", 200);

        let service_db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results(vec![vec![node_1_for_service]])
            .append_query_results(vec![vec![node_1_updated]])
            .append_query_results(vec![vec![node_2_for_service]])
            .append_query_results(vec![vec![node_2_updated]])
            .into_connection();
        let node_service = NodeService::new(std::sync::Arc::new(service_db));

        let marked = check_node_health(&node_service, &db).await;
        assert_eq!(marked.len(), 2);
        assert!(marked.contains(&1));
        assert!(marked.contains(&2));
    }

    #[tokio::test]
    async fn test_check_node_health_returns_offline_ids() {
        let stale_node = make_node(5, "worker-5", "active", 200);

        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results(vec![vec![stale_node]])
            .into_connection();

        let node_for_service = make_node(5, "worker-5", "active", 200);
        let node_updated = make_node(5, "worker-5", "offline", 200);

        let service_db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results(vec![vec![node_for_service]])
            .append_query_results(vec![vec![node_updated]])
            .into_connection();
        let node_service = NodeService::new(std::sync::Arc::new(service_db));

        let marked = check_node_health(&node_service, &db).await;
        assert_eq!(marked, vec![5]);
    }

    // ── Resource-alert threshold evaluation (resource_breaches) ──────────

    #[test]
    fn test_resource_breaches_cpu_above_and_below() {
        let cap = serde_json::json!({ "cpu_percent": 95.0 });
        let breaches = resource_breaches(&cap, Some(90.0), None, None);
        assert_eq!(breaches.len(), 1);
        assert_eq!(breaches[0].0, "CPU");
        assert!((breaches[0].1 - 95.0).abs() < f64::EPSILON);

        // Below threshold → no breach. Exactly-at threshold is not a breach (>).
        assert!(resource_breaches(
            &serde_json::json!({"cpu_percent": 85.0}),
            Some(90.0),
            None,
            None
        )
        .is_empty());
        assert!(resource_breaches(
            &serde_json::json!({"cpu_percent": 90.0}),
            Some(90.0),
            None,
            None
        )
        .is_empty());
    }

    #[test]
    fn test_resource_breaches_memory_and_disk_percentage() {
        // 9 GiB used of 10 GiB = 90% — just over an 85% threshold.
        let cap = serde_json::json!({
            "memory_used_bytes": 9_000_000_000.0,
            "memory_total_bytes": 10_000_000_000.0,
            "disk_used_bytes": 1_000_000_000.0,
            "disk_total_bytes": 10_000_000_000.0,
        });
        let breaches = resource_breaches(&cap, None, Some(85.0), Some(85.0));
        // memory 90% > 85% breaches; disk 10% does not.
        assert_eq!(breaches.len(), 1);
        assert_eq!(breaches[0].0, "memory");
        assert!((breaches[0].1 - 90.0).abs() < 1e-6);
    }

    #[test]
    fn test_resource_breaches_disabled_and_missing_fields() {
        let cap = serde_json::json!({ "cpu_percent": 99.0 });
        // All thresholds None → nothing evaluated even though CPU is pegged.
        assert!(resource_breaches(&cap, None, None, None).is_empty());
        // Threshold set but the capacity field is absent → no breach, no panic.
        assert!(
            resource_breaches(&serde_json::json!({}), Some(90.0), Some(90.0), Some(90.0))
                .is_empty()
        );
        // Zero total guards against divide-by-zero.
        let zero = serde_json::json!({"memory_used_bytes": 5.0, "memory_total_bytes": 0.0});
        assert!(resource_breaches(&zero, None, Some(90.0), None).is_empty());
    }

    #[test]
    fn test_resource_breaches_all_three_metrics() {
        let cap = serde_json::json!({
            "cpu_percent": 99.0,
            "memory_used_bytes": 95.0, "memory_total_bytes": 100.0,
            "disk_used_bytes": 91.0, "disk_total_bytes": 100.0,
        });
        let breaches = resource_breaches(&cap, Some(90.0), Some(90.0), Some(90.0));
        assert_eq!(breaches.len(), 3);
        let metrics: Vec<&str> = breaches.iter().map(|b| b.0).collect();
        assert!(
            metrics.contains(&"CPU") && metrics.contains(&"memory") && metrics.contains(&"disk")
        );
    }

    #[test]
    fn test_refresh_control_plane_metrics_populates_cache() {
        refresh_control_plane_metrics();
        let (cap, _sampled_at) =
            latest_control_plane_metrics().expect("a sample should be cached after refresh");
        for key in [
            "cpu_percent",
            "memory_used_bytes",
            "memory_total_bytes",
            "disk_used_bytes",
            "disk_total_bytes",
        ] {
            assert!(cap.get(key).is_some(), "CP capacity missing key '{key}'");
        }
        // Any real host has non-zero total memory — confirms sysinfo sampled.
        assert!(
            cap.get("memory_total_bytes")
                .and_then(|v| v.as_u64())
                .unwrap_or(0)
                > 0
        );
    }
}
