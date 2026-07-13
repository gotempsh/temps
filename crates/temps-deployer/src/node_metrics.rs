//! Node resource-metric collection.
//!
//! Single source of truth for the `nodes.capacity` JSON shape, shared by:
//! - the worker agent's heartbeat loop (`temps-agent`), which reports a worker's
//!   resources to the control plane, and
//! - the control plane's own self-reporting loop (`temps serve`), which reports
//!   the control-plane node's resources in-process.
//!
//! Keeping a single collector guarantees both rows carry an identical shape so
//! the nodes UI renders control-plane and worker resources the same way.

/// Recommended minimum delay between the two CPU samples. CPU usage is a delta
/// between successive refreshes, so a single refresh always reads ~0%; sysinfo
/// recommends ~200ms between samples for an accurate reading.
const CPU_SAMPLE_INTERVAL: std::time::Duration = std::time::Duration::from_millis(200);

/// Collect this host's CPU/memory/disk usage as the `capacity` JSON stored on a
/// node's heartbeat.
///
/// Fields:
/// - `cpu_percent`: global CPU utilisation (0–100, summed across cores / core count)
/// - `memory_used_bytes` / `memory_total_bytes`
/// - `disk_used_bytes` / `disk_total_bytes` for the root (`/`) mount
///
/// Async because it samples the CPU twice with a short delay in between — a
/// single sample would always report 0% on the first refresh.
pub async fn collect_capacity_metrics() -> serde_json::Value {
    use sysinfo::{Disks, System};

    let mut sys = System::new_all();
    // Two CPU samples a short interval apart so cpu_percent reflects real load
    // instead of a cold 0 on the first refresh.
    sys.refresh_cpu_all();
    tokio::time::sleep(CPU_SAMPLE_INTERVAL).await;
    sys.refresh_cpu_usage();
    sys.refresh_memory();
    let disks = Disks::new_with_refreshed_list();

    // Use only the root mount point to avoid double-counting overlapping mounts.
    let (disk_used, disk_total) = disks
        .list()
        .iter()
        .find(|d| d.mount_point() == std::path::Path::new("/"))
        .map(|d| (d.total_space() - d.available_space(), d.total_space()))
        .unwrap_or((0, 0));

    serde_json::json!({
        "cpu_percent": sys.global_cpu_usage(),
        "memory_used_bytes": sys.used_memory(),
        "memory_total_bytes": sys.total_memory(),
        "disk_used_bytes": disk_used,
        "disk_total_bytes": disk_total,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn collect_capacity_metrics_has_expected_shape() {
        let capacity = collect_capacity_metrics().await;
        let obj = capacity
            .as_object()
            .expect("capacity metrics should be a JSON object");

        // The nodes UI keys off exactly these fields; all must be present and
        // numeric so the control-plane and worker rows render identically.
        for key in [
            "cpu_percent",
            "memory_used_bytes",
            "memory_total_bytes",
            "disk_used_bytes",
            "disk_total_bytes",
        ] {
            let value = obj
                .get(key)
                .unwrap_or_else(|| panic!("capacity metrics missing key '{key}'"));
            assert!(
                value.is_number(),
                "capacity metric '{key}' should be numeric, got {value}"
            );
        }
    }
}
