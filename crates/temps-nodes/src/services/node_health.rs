// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Node health helpers the heartbeat and node-list handlers read: resolving the
//! offline alarm when a worker recovers, and the control plane's own cached
//! host metrics.

use temps_monitoring::alarm_service::{AlarmService, AlarmType};

/// Notify operators that a worker node came back online (ADR-020 / monitoring).
///
/// Called from the heartbeat handler when a node transitions offline->active
/// (the `was_offline` signal). Sends one Info-priority notification through the
/// shared notification pipeline — the recovery counterpart to
/// [`notify_nodes_offline`]. Best-effort: failures are logged, never fatal.
pub async fn notify_node_recovered(
    node_id: i32,
    node_name: &str,
    alarm_service: &std::sync::Arc<AlarmService>,
) {
    match alarm_service
        .resolve_node_alarms(node_id, AlarmType::NodeOffline)
        .await
    {
        Ok(_) => tracing::info!(node_id, node_name = %node_name, "Resolved node-offline alarm(s)"),
        Err(e) => tracing::error!(
            node_id,
            node_name = %node_name,
            "Failed to resolve node-offline alarm(s): {}",
            e
        ),
    }
}

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

    let mut sys = match System::new() {
        Ok(sys) => sys,
        Err(error) => {
            tracing::warn!(%error, "Could not read control-plane host metrics; keeping the last sample");
            return;
        }
    };
    sys.refresh_cpu_all();
    sys.refresh_memory();

    let cpu_percent = sys.global_cpu_usage() as f64;
    let memory_used_bytes = sys.used_memory();
    let memory_total_bytes = sys.total_memory();
    // Root mount only, to avoid double-counting overlapping mounts (matches the
    // agent's collect_system_metrics).
    let (disk_used, disk_total) = Disks::new_with_refreshed_list()
        .inspect_err(|error| {
            tracing::warn!(%error, "Could not list mounted disks; reporting 0 disk space");
        })
        .ok()
        .and_then(|disks| {
            disks
                .iter()
                .find(|d| d.mount_point() == std::path::Path::new("/"))
                .map(|d| (d.total_space() - d.available_space(), d.total_space()))
        })
        .unwrap_or((0, 0));

    let capacity = serde_json::json!({
        "cpu_percent": cpu_percent,
        "memory_used_bytes": memory_used_bytes,
        "memory_total_bytes": memory_total_bytes,
        "disk_used_bytes": disk_used,
        "disk_total_bytes": disk_total,
        // What the scheduler needs to enforce CPU reservations on node 0,
        // as the agent's heartbeat reports it for a worker.
        "cpu_cores": sys.cpus().len(),
    });

    if let Ok(mut g) = control_plane_metrics_cell().write() {
        *g = Some((capacity, chrono::Utc::now()));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
