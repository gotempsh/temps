// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Periodic container stats polling for the health monitor.
//!
//! The health monitor samples every service container once per tick with a
//! Docker `one_shot` read. Docker zeroes `precpu_stats` on such a read, so a
//! lone response only yields the container's lifetime-average CPU. The rate
//! here always comes from the poller's own previous sample of the same
//! container: the tick interval is the rate window.
//!
//! Next to the CPU counters, the poller remembers the container's applied
//! CPU limit, so CPU can be reported the way the resources card shows it:
//! cores used against the limit (or the host's cores when uncapped).

use super::runtime_limits::{compute_stats_sample, sample_container_stats_once};
use crate::services::*;
use bollard::models::{ContainerCpuStats, ContainerStatsResponse, HostConfig};
use std::collections::HashMap;
use std::sync::atomic::Ordering;
use std::time::{Duration, Instant};
use temps_entities::external_services;
use tracing::debug;

/// How long a poller trusts a container's cached CPU limit before inspecting
/// the container again. Limits changed through Temps are re-read on the next
/// tick (`ExternalServiceManager::cpu_limits_epoch`), and a recreated
/// container (new ID) is inspected on its first tick; this window only bounds
/// how long a change made outside Temps (`docker update` by hand) goes
/// unnoticed. At the default 30 s tick this is one inspect per container
/// every ten ticks instead of one per tick.
pub const CPU_LIMIT_REFRESH: Duration = Duration::from_secs(300);

/// The CPU a container may use, read from its `HostConfig`.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum CpuLimit {
    /// No CPU cap: the container may use every online host core.
    Unlimited,
    /// Capped at this many cores (`NanoCpus`, or `CpuQuota / CpuPeriod`).
    Cores(f64),
}

impl CpuLimit {
    /// The limit a container's `HostConfig` applies. `NanoCpus` is what
    /// Temps sets; a `CpuQuota`/`CpuPeriod` pair is the equivalent older form.
    pub fn from_host_config(host_config: &HostConfig) -> Self {
        if let Some(nano) = host_config.nano_cpus.filter(|&n| n > 0) {
            return Self::Cores(nano as f64 / 1_000_000_000.0);
        }
        match (host_config.cpu_quota, host_config.cpu_period) {
            (Some(quota), Some(period)) if quota > 0 && period > 0 => {
                Self::Cores(quota as f64 / period as f64)
            }
            _ => Self::Unlimited,
        }
    }

    /// The limit in cores, `None` when uncapped.
    pub fn cores(self) -> Option<f64> {
        match self {
            Self::Cores(cores) => Some(cores),
            Self::Unlimited => None,
        }
    }
}

/// Cores a container may use: its limit, else every online host core, else
/// one core when Docker reported no core count. Same ceiling deployment
/// containers use for `container.cpu_utilization_percent`.
pub fn cpu_ceiling_cores(limit: CpuLimit, online_cpus: Option<u32>) -> f64 {
    match limit {
        CpuLimit::Cores(cores) if cores > 0.0 => cores,
        _ => match online_cpus {
            Some(cpus) if cpus > 0 => f64::from(cpus),
            _ => 1.0,
        },
    }
}

/// `cpu_percent` (Docker's convention, 100 == one core) as a percentage of
/// the cores the container may use (100 == saturated). This is the number a
/// CPU alert should threshold: 80 means 80% of the limit whatever the limit
/// is, where 80 on `container.cpu_percent` means 0.8 of one core.
pub fn cpu_utilization_percent(cpu_percent: f64, limit: CpuLimit, online_cpus: Option<u32>) -> f64 {
    cpu_percent / cpu_ceiling_cores(limit, online_cpus)
}

/// The container's CPU limit as last read from Docker.
#[derive(Debug, Clone, Copy)]
struct CachedCpuLimit {
    limit: CpuLimit,
    read_at: Instant,
    /// `ExternalServiceManager::cpu_limits_epoch` when it was read; a later
    /// epoch means Temps has changed some service's limits since.
    epoch: u64,
}

/// What a poller keeps per container between ticks: the container's ID, its
/// cumulative CPU counters (the next tick's rate baseline) and its cached CPU
/// limit. Only the CPU stats are retained, not the whole stats response.
#[derive(Debug, Clone)]
pub struct ContainerStatsBaseline {
    container_id: Option<String>,
    cpu_stats: Option<ContainerCpuStats>,
    cpu_limit: Option<CachedCpuLimit>,
}

#[cfg(test)]
impl ContainerStatsBaseline {
    /// A baseline with no counters, for tests of the code that stores them.
    pub(crate) fn seeded_for_test() -> Self {
        Self {
            container_id: None,
            cpu_stats: None,
            cpu_limit: None,
        }
    }
}

/// One container's result from a polling tick.
#[derive(Debug, Clone)]
pub struct PolledContainerStats {
    /// CPU and memory, same shape as the live stats endpoint. `cpu_percent`
    /// is the average over the time since this poller's previous sample.
    pub sample: ContainerStatsSample,
    /// The container's CPU limit; `None` when it could not be read.
    pub cpu_limit: Option<CpuLimit>,
}

impl PolledContainerStats {
    fn unavailable(role: String, container_name: String) -> Self {
        Self {
            sample: ContainerStatsSample {
                role,
                container_name,
                cpu_percent: None,
                memory_usage_bytes: None,
                memory_limit_bytes: None,
                memory_percent: None,
                online_cpus: None,
            },
            cpu_limit: None,
        }
    }

    /// CPU as a percentage of the cores the container may use. `None` without
    /// a CPU rate or a known limit: an unknown limit is not read as uncapped,
    /// which would understate the utilization of a capped container.
    pub fn cpu_utilization_percent(&self) -> Option<f64> {
        let cpu_percent = self.sample.cpu_percent?;
        let limit = self.cpu_limit?;
        Some(cpu_utilization_percent(
            cpu_percent,
            limit,
            self.sample.online_cpus,
        ))
    }
}

/// The previous CPU counters usable as this tick's rate baseline: only those
/// of the same container. A recreated container (new ID, same name) restarts
/// its counters, and diffing against the old container's total yields no
/// meaningful rate. A restart in place (same ID) shows up as a counter going
/// backwards, which `cpu_percent_from_delta` rejects.
fn cpu_baseline<'a>(
    previous: Option<&'a ContainerStatsBaseline>,
    current: &ContainerStatsResponse,
) -> Option<&'a ContainerCpuStats> {
    let previous = previous?;
    if previous.container_id != current.id {
        return None;
    }
    previous.cpu_stats.as_ref()
}

/// The cached CPU limit while it is still current for this container: same
/// container, no limit change applied through Temps since (`epoch`), and
/// younger than `CPU_LIMIT_REFRESH`.
fn fresh_cpu_limit(
    previous: Option<&ContainerStatsBaseline>,
    current: &ContainerStatsResponse,
    now: Instant,
    epoch: u64,
) -> Option<CachedCpuLimit> {
    let previous = previous?;
    if previous.container_id != current.id {
        return None;
    }
    previous.cpu_limit.filter(|cached| {
        cached.epoch == epoch && now.saturating_duration_since(cached.read_at) < CPU_LIMIT_REFRESH
    })
}

/// One container's tick: the reported stats and the baseline for the next
/// tick. The first tick for a container (or a recreated one) has no rate.
fn poll_step(
    role: String,
    container_name: String,
    previous: Option<&ContainerStatsBaseline>,
    current: ContainerStatsResponse,
    cpu_limit: Option<CachedCpuLimit>,
) -> (PolledContainerStats, ContainerStatsBaseline) {
    let sample = compute_stats_sample(
        role,
        container_name,
        &current,
        cpu_baseline(previous, &current),
    );
    let polled = PolledContainerStats {
        sample,
        cpu_limit: cpu_limit.map(|cached| cached.limit),
    };
    let baseline = ContainerStatsBaseline {
        container_id: current.id,
        cpu_stats: current.cpu_stats,
        cpu_limit,
    };
    (polled, baseline)
}

/// Read the CPU limit Docker applies to `name` right now.
async fn read_cpu_limit(
    docker: &bollard::Docker,
    name: &str,
) -> Result<CpuLimit, bollard::errors::Error> {
    let info = docker
        .inspect_container(
            name,
            None::<bollard::query_parameters::InspectContainerOptions>,
        )
        .await?;
    Ok(info
        .host_config
        .as_ref()
        .map(CpuLimit::from_host_config)
        .unwrap_or(CpuLimit::Unlimited))
}

impl ExternalServiceManager {
    /// The current limits epoch; see `note_cpu_limits_changed`.
    fn cpu_limits_epoch(&self) -> u64 {
        self.cpu_limits_epoch.load(Ordering::Acquire)
    }

    /// Record that Temps has just applied resource limits to some service's
    /// containers: every cached CPU limit is re-read on the next tick, so a
    /// removed or changed cap is reported at once instead of after
    /// `CPU_LIMIT_REFRESH`. Process-wide, as limit changes are rare.
    pub(super) fn note_cpu_limits_changed(&self) {
        self.cpu_limits_epoch.fetch_add(1, Ordering::AcqRel);
    }

    /// Sample stats for every container in this service against a
    /// caller-held baseline map (`container_name` → previous tick).
    ///
    /// Designed for periodic pollers (the health monitor's 30 s loop): the
    /// poll interval itself is the CPU rate window, so unlike
    /// `get_service_stats` no artificial 1 s sleep per container is needed.
    /// The first tick for a container has no baseline, so `cpu_percent` is
    /// `None` (memory is still reported) and the baseline is seeded.
    ///
    /// The map is replaced once the pass completes, so it never holds more
    /// entries than the service has containers: entries for containers that no
    /// longer back the service are dropped, and entries whose sample failed
    /// this tick (container stopped / remote node) are carried over unchanged,
    /// since cumulative counters stay valid across a longer window. It is not
    /// touched before then, so a caller whose future is dropped mid-pass (a
    /// manual check whose client disconnected) keeps its baselines.
    pub async fn sample_service_stats(
        &self,
        service: &external_services::Model,
        baselines: &mut HashMap<String, ContainerStatsBaseline>,
    ) -> Result<Vec<PolledContainerStats>, ExternalServiceError> {
        let containers = self.resolve_member_containers(service).await?;
        let docker = self.docker.get();

        let mut members = Vec::with_capacity(containers.len());
        let mut next_baselines = HashMap::with_capacity(containers.len());

        for (role, name) in containers {
            let previous = baselines.get(&name);
            let current = match docker {
                Some(docker) => sample_container_stats_once(docker, &name).await,
                None => None,
            };
            let (Some(docker), Some(current)) = (docker, current) else {
                // Container missing/stopped, on a remote node, or no local
                // Docker: keep the old baseline so a later success still has a
                // valid rate window.
                if let Some(previous) = previous {
                    next_baselines.insert(name.clone(), previous.clone());
                }
                members.push(PolledContainerStats::unavailable(role, name));
                continue;
            };

            let now = Instant::now();
            // Read before inspecting: a limit applied while the inspect is in
            // flight bumps the epoch past this one and is re-read next tick.
            let epoch = self.cpu_limits_epoch();
            let cpu_limit = match fresh_cpu_limit(previous, &current, now, epoch) {
                Some(cached) => Some(cached),
                None => match read_cpu_limit(docker, &name).await {
                    Ok(limit) => Some(CachedCpuLimit {
                        limit,
                        read_at: now,
                        epoch,
                    }),
                    Err(e) => {
                        // Left unknown, so the next tick inspects again.
                        debug!(
                            service_id = service.id,
                            container = %name,
                            "Reading the container CPU limit failed: {}", e
                        );
                        None
                    }
                },
            };

            let (polled, baseline) = poll_step(role, name.clone(), previous, current, cpu_limit);
            members.push(polled);
            next_baselines.insert(name, baseline);
        }

        *baselines = next_baselines;
        Ok(members)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const CONTAINER_ID: &str = "4f1c0a9e7d2b6c3a8e5f9d0b1a2c3e4f5a6b7c8d9e0f1a2b3c4d5e6f7a8b9c0d";
    const HOST_CPUS: u32 = 16;
    const SECOND_NS: u64 = 1_000_000_000;
    /// A container that has run for 30 days at an average of 0.05 cores.
    const LIFETIME_SECS: u64 = 30 * 24 * 3600;
    const LIFETIME_CPU_NS: u64 = LIFETIME_SECS * SECOND_NS / 20;
    const LIFETIME_SYSTEM_NS: u64 = LIFETIME_SECS * SECOND_NS * HOST_CPUS as u64;

    /// A Docker `one_shot` stats response: lifetime counters in `cpu_stats`
    /// and the zeroed `precpu_stats` Docker returns for a one-shot read.
    fn one_shot_stats(id: &str, total_usage: u64, system_cpu_usage: u64) -> ContainerStatsResponse {
        let json = serde_json::json!({
            "name": "/service-postgres",
            "id": id,
            "read": "2026-01-01T10:00:30.000000000Z",
            "preread": "0001-01-01T00:00:00Z",
            "pids_stats": { "current": 31 },
            "num_procs": 0,
            "cpu_stats": {
                "cpu_usage": {
                    "total_usage": total_usage,
                    "usage_in_kernelmode": total_usage / 4,
                    "usage_in_usermode": total_usage - total_usage / 4
                },
                "system_cpu_usage": system_cpu_usage,
                "online_cpus": HOST_CPUS,
                "throttling_data": { "periods": 0, "throttled_periods": 0, "throttled_time": 0 }
            },
            "precpu_stats": {
                "cpu_usage": { "total_usage": 0, "usage_in_kernelmode": 0, "usage_in_usermode": 0 },
                "throttling_data": { "periods": 0, "throttled_periods": 0, "throttled_time": 0 }
            },
            "memory_stats": {
                "usage": 10_000_000_000u64,
                "limit": 34_359_738_368u64,
                "stats": { "inactive_file": 1_130_000_000u64, "anon": 8_000_000_000u64 }
            },
            "networks": {
                "eth0": { "rx_bytes": 1_000_000, "tx_bytes": 2_000_000 }
            }
        });
        serde_json::from_value(json).expect("valid Docker stats JSON")
    }

    fn six_core_limit() -> Option<CachedCpuLimit> {
        Some(CachedCpuLimit {
            limit: CpuLimit::Cores(6.0),
            read_at: Instant::now(),
            epoch: 0,
        })
    }

    fn step(
        previous: Option<&ContainerStatsBaseline>,
        current: ContainerStatsResponse,
        cpu_limit: Option<CachedCpuLimit>,
    ) -> (PolledContainerStats, ContainerStatsBaseline) {
        poll_step(
            "primary".into(),
            "service-postgres".into(),
            previous,
            current,
            cpu_limit,
        )
    }

    /// The first tick has only Docker's zeroed `precpu_stats` to diff
    /// against, which would give the lifetime average (5% here). It must
    /// report no CPU rather than that number; memory is reported.
    #[test]
    fn first_one_shot_sample_reports_no_cpu_not_the_lifetime_average() {
        let first = one_shot_stats(CONTAINER_ID, LIFETIME_CPU_NS, LIFETIME_SYSTEM_NS);
        let (polled, _) = step(None, first, six_core_limit());

        assert_eq!(polled.sample.cpu_percent, None);
        assert_eq!(polled.cpu_utilization_percent(), None);
        assert_eq!(polled.sample.memory_usage_bytes, Some(8_870_000_000));
    }

    /// Two ticks 30 s apart: 0.681 cores used in the window reads as
    /// `cpu_percent` 68.1 (0.68 of one core) and 11.35% of a 6-core limit,
    /// not the 5% lifetime average.
    #[test]
    fn second_tick_reports_the_rate_over_the_poll_window() {
        let window_secs = 30;
        let used_ns = 681 * window_secs * SECOND_NS / 1000;
        let first = one_shot_stats(CONTAINER_ID, LIFETIME_CPU_NS, LIFETIME_SYSTEM_NS);
        let second = one_shot_stats(
            CONTAINER_ID,
            LIFETIME_CPU_NS + used_ns,
            LIFETIME_SYSTEM_NS + window_secs * SECOND_NS * HOST_CPUS as u64,
        );

        let (_, baseline) = step(None, first, six_core_limit());
        let (polled, _) = step(Some(&baseline), second, six_core_limit());

        let cpu = polled.sample.cpu_percent.expect("rate after two ticks");
        assert!((cpu - 68.1).abs() < 1e-6, "cpu_percent {cpu}");
        let util = polled.cpu_utilization_percent().expect("limit is known");
        assert!((util - 11.35).abs() < 1e-6, "utilization {util}");
    }

    /// Uncapped, the same usage is measured against the host's 16 cores.
    #[test]
    fn uncapped_container_utilization_is_against_host_cores() {
        let first = one_shot_stats(CONTAINER_ID, LIFETIME_CPU_NS, LIFETIME_SYSTEM_NS);
        let second = one_shot_stats(
            CONTAINER_ID,
            LIFETIME_CPU_NS + 8 * SECOND_NS,
            LIFETIME_SYSTEM_NS + 10 * SECOND_NS * HOST_CPUS as u64,
        );
        let unlimited = Some(CachedCpuLimit {
            limit: CpuLimit::Unlimited,
            read_at: Instant::now(),
            epoch: 0,
        });

        let (_, baseline) = step(None, first, unlimited);
        let (polled, _) = step(Some(&baseline), second, unlimited);

        // 0.8 cores of 16.
        assert_eq!(polled.sample.cpu_percent, Some(80.0));
        assert_eq!(polled.cpu_utilization_percent(), Some(5.0));
    }

    /// An unknown limit (inspect failed) reports cores used but no
    /// utilization, rather than measuring a capped container against the host.
    #[test]
    fn unknown_limit_reports_no_utilization() {
        let first = one_shot_stats(CONTAINER_ID, LIFETIME_CPU_NS, LIFETIME_SYSTEM_NS);
        let second = one_shot_stats(
            CONTAINER_ID,
            LIFETIME_CPU_NS + SECOND_NS,
            LIFETIME_SYSTEM_NS + 10 * SECOND_NS * HOST_CPUS as u64,
        );
        let (_, baseline) = step(None, first, None);
        let (polled, _) = step(Some(&baseline), second, None);

        assert_eq!(polled.sample.cpu_percent, Some(10.0));
        assert_eq!(polled.cpu_utilization_percent(), None);
    }

    /// A restart in place resets the counter. That tick reports no CPU and
    /// reseeds the baseline, so the following tick has a rate again.
    #[test]
    fn counter_reset_skips_one_tick_and_reseeds() {
        let before = one_shot_stats(CONTAINER_ID, LIFETIME_CPU_NS, LIFETIME_SYSTEM_NS);
        let restarted = one_shot_stats(
            CONTAINER_ID,
            2 * SECOND_NS,
            LIFETIME_SYSTEM_NS + 30 * SECOND_NS * HOST_CPUS as u64,
        );
        let after = one_shot_stats(
            CONTAINER_ID,
            2 * SECOND_NS + 3 * SECOND_NS,
            LIFETIME_SYSTEM_NS + 60 * SECOND_NS * HOST_CPUS as u64,
        );

        let (_, b1) = step(None, before, six_core_limit());
        let (polled, b2) = step(Some(&b1), restarted, six_core_limit());
        assert_eq!(polled.sample.cpu_percent, None);

        let (polled, _) = step(Some(&b2), after, six_core_limit());
        // 3 s of CPU over 30 s.
        assert_eq!(polled.sample.cpu_percent, Some(10.0));
    }

    /// A container recreated under the same name starts new counters. Even
    /// when its total already exceeds the old young container's, the old
    /// counters are not a baseline for it.
    #[test]
    fn recreated_container_is_not_diffed_against_the_old_one() {
        let old = one_shot_stats(CONTAINER_ID, SECOND_NS, 100 * SECOND_NS);
        let new_id = "9a8b7c6d5e4f3a2b1c0d9e8f7a6b5c4d3e2f1a0b9c8d7e6f5a4b3c2d1e0f9a8b";
        let recreated = one_shot_stats(new_id, 50 * SECOND_NS, 500 * SECOND_NS);

        let (_, baseline) = step(None, old, six_core_limit());
        assert!(cpu_baseline(Some(&baseline), &recreated).is_none());
        assert!(fresh_cpu_limit(Some(&baseline), &recreated, Instant::now(), 0).is_none());

        let (polled, _) = step(Some(&baseline), recreated, six_core_limit());
        assert_eq!(polled.sample.cpu_percent, None);
    }

    /// The limit is reused within `CPU_LIMIT_REFRESH` and read again after.
    fn baseline_with_limit(read_at: Instant, epoch: u64) -> ContainerStatsBaseline {
        ContainerStatsBaseline {
            container_id: Some(CONTAINER_ID.to_string()),
            cpu_stats: None,
            cpu_limit: Some(CachedCpuLimit {
                limit: CpuLimit::Cores(6.0),
                read_at,
                epoch,
            }),
        }
    }

    /// The limit is reused within `CPU_LIMIT_REFRESH` and read again after.
    #[test]
    fn cached_cpu_limit_expires_after_the_refresh_window() {
        let read_at = Instant::now();
        let current = one_shot_stats(CONTAINER_ID, LIFETIME_CPU_NS, LIFETIME_SYSTEM_NS);
        let baseline = baseline_with_limit(read_at, 4);

        let cached = fresh_cpu_limit(
            Some(&baseline),
            &current,
            read_at + Duration::from_secs(30),
            4,
        );
        assert_eq!(cached.map(|c| c.limit), Some(CpuLimit::Cores(6.0)));
        assert!(
            fresh_cpu_limit(Some(&baseline), &current, read_at + CPU_LIMIT_REFRESH, 4).is_none()
        );
        assert!(fresh_cpu_limit(None, &current, read_at, 4).is_none());
    }

    /// Removing or changing a cap through Temps bumps the epoch: the very
    /// next tick reads the limit again instead of reporting the old cap for
    /// up to `CPU_LIMIT_REFRESH`.
    #[test]
    fn a_limit_change_through_temps_invalidates_the_cached_limit() {
        let read_at = Instant::now();
        let current = one_shot_stats(CONTAINER_ID, LIFETIME_CPU_NS, LIFETIME_SYSTEM_NS);
        let baseline = baseline_with_limit(read_at, 4);

        let next_tick = read_at + Duration::from_secs(30);
        assert!(fresh_cpu_limit(Some(&baseline), &current, next_tick, 4).is_some());
        assert!(fresh_cpu_limit(Some(&baseline), &current, next_tick, 5).is_none());
    }

    #[test]
    fn cpu_limit_reads_nano_cpus_then_quota() {
        let nano = HostConfig {
            nano_cpus: Some(6_000_000_000),
            ..Default::default()
        };
        assert_eq!(CpuLimit::from_host_config(&nano), CpuLimit::Cores(6.0));

        let quota = HostConfig {
            nano_cpus: Some(0),
            cpu_quota: Some(150_000),
            cpu_period: Some(100_000),
            ..Default::default()
        };
        assert_eq!(CpuLimit::from_host_config(&quota), CpuLimit::Cores(1.5));

        let none = HostConfig {
            nano_cpus: Some(0),
            cpu_quota: Some(-1),
            ..Default::default()
        };
        assert_eq!(CpuLimit::from_host_config(&none), CpuLimit::Unlimited);
        assert_eq!(CpuLimit::Unlimited.cores(), None);
    }

    #[test]
    fn ceiling_is_limit_then_host_cores_then_one() {
        assert_eq!(cpu_ceiling_cores(CpuLimit::Cores(6.0), Some(16)), 6.0);
        assert_eq!(cpu_ceiling_cores(CpuLimit::Unlimited, Some(16)), 16.0);
        assert_eq!(cpu_ceiling_cores(CpuLimit::Unlimited, None), 1.0);
        assert_eq!(cpu_ceiling_cores(CpuLimit::Cores(0.0), Some(4)), 4.0);
        assert_eq!(
            cpu_utilization_percent(300.0, CpuLimit::Cores(6.0), Some(16)),
            50.0
        );
    }
}
