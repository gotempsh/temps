// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Build concurrency and per-build resource caps for [`DockerRuntime`].

use super::DockerRuntime;
use crate::build_gate::BuildGate;
use std::sync::Arc;
use sysinfo::System;
use tracing::{debug, info, warn};

/// Explicit per-build resource caps, set by the control plane from
/// `AppSettings.build_limits`. Both fields use the same units the rest of
/// the deployer code already uses (cores + MB) so there's no conversion
/// surface between settings and the Docker API call.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct BuildResourceLimits {
    /// CPU cores allowed per build, e.g. `2.0` = 2 full cores.
    pub cpu_cores: f32,
    /// Memory allowed per build, in megabytes.
    pub memory_mb: u32,
}

impl BuildResourceLimits {
    /// The per-build caps an `AppSettings.build_limits` section asks for.
    ///
    /// `None` unless `cpu_limit_cores > 0` AND `memory_limit_mb > 0`: either
    /// zero means "leave that dimension on the legacy heuristic", and a
    /// half-set pair would pin one dimension and starve the build on the
    /// other.
    pub fn from_settings(settings: &temps_core::BuildLimitsSettings) -> Option<Self> {
        Self {
            cpu_cores: settings.cpu_limit_cores,
            memory_mb: settings.memory_limit_mb,
        }
        .effective()
    }

    /// `self` if both dimensions are set, otherwise `None`. The one place
    /// the pairing rule lives.
    fn effective(self) -> Option<Self> {
        (self.cpu_cores > 0.0 && self.memory_mb > 0).then_some(self)
    }
}

/// Build limits as the runtime currently enforces them.
///
/// Shared between the runtime and the task that follows the settings, so a
/// change reaches every later build without restarting the process. Builds
/// already running keep the caps they started with: the caps are read once,
/// when a build starts.
#[derive(Default)]
pub(super) struct LiveBuildLimits {
    /// Concurrency gate. Absent until limits are first applied, which is the
    /// legacy unbounded behaviour.
    gate: std::sync::OnceLock<Arc<BuildGate>>,
    /// Per-build caps, or none for the legacy 50%-of-host heuristic.
    resource_caps: arc_swap::ArcSwapOption<BuildResourceLimits>,
}

/// What [`LiveBuildLimits`] enforced before an update.
#[derive(Debug, Clone, Copy, PartialEq)]
struct BuildLimitsSnapshot {
    max_concurrent: usize,
    resource_caps: Option<BuildResourceLimits>,
}

impl std::fmt::Display for BuildLimitsSnapshot {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self.resource_caps {
            Some(caps) => write!(
                f,
                "concurrency={}, per-build cpu={} cores, mem={} MB",
                self.max_concurrent, caps.cpu_cores, caps.memory_mb
            ),
            None => write!(
                f,
                "concurrency={}, per-build caps=legacy 50%-of-host heuristic",
                self.max_concurrent
            ),
        }
    }
}

impl LiveBuildLimits {
    /// Per-build caps for a build starting now.
    pub(super) fn resource_caps(&self) -> Option<BuildResourceLimits> {
        self.resource_caps.load().as_deref().copied()
    }

    /// Concurrency gate, once limits have been applied.
    pub(super) fn gate(&self) -> Option<&Arc<BuildGate>> {
        self.gate.get()
    }

    /// Replace the limits. `max_concurrent` is clamped to at least 1 (a gate
    /// that admits nobody would block every build). Returns what was in force
    /// before, or `None` the first time.
    fn apply(
        &self,
        max_concurrent: u32,
        resource_caps: Option<BuildResourceLimits>,
    ) -> Option<BuildLimitsSnapshot> {
        let permits = max_concurrent.max(1) as usize;
        let previous_caps = self
            .resource_caps
            .swap(resource_caps.map(Arc::new))
            .as_deref()
            .copied();
        let mut created = false;
        let gate = self.gate.get_or_init(|| {
            created = true;
            BuildGate::new(permits)
        });
        if created {
            return None;
        }
        Some(BuildLimitsSnapshot {
            max_concurrent: gate.set_limit(permits),
            resource_caps: previous_caps,
        })
    }
}

/// Apply `AppSettings.build_limits` to `limits` and report it.
///
/// A change to limits that were already in force is logged at info with the
/// old and new values. Caps that BuildKit would ignore are called out
/// whenever they are first set or changed.
fn apply_build_limits(
    limits: &LiveBuildLimits,
    use_buildkit: bool,
    max_concurrent: u32,
    resource_limits: Option<BuildResourceLimits>,
) {
    // A partial pair is discarded, whichever way the caller built it.
    let resource_caps = resource_limits.and_then(BuildResourceLimits::effective);
    let permits = max_concurrent.max(1) as usize;
    let previous = limits.apply(max_concurrent, resource_caps);
    let current = BuildLimitsSnapshot {
        max_concurrent: permits,
        resource_caps,
    };
    let caps_changed = match previous {
        Some(previous) => {
            if previous == current {
                debug!("Build limits unchanged ({})", current);
                return;
            }
            info!(
                "Build limits changed at runtime: {} -> {}. Running builds keep the limits they \
                 started with; queued and new builds use the new ones.",
                previous, current
            );
            previous.resource_caps != resource_caps
        }
        None => true,
    };
    // Only when the caps themselves are new: a change to concurrency alone
    // must not repeat a warning about caps that did not move.
    if let (true, true, Some(caps)) = (caps_changed, use_buildkit, resource_caps) {
        warn!(
            "Per-build caps of {} cores and {} MB are configured, but this host builds with \
             BuildKit, which ignores the memory and CPU options of the Docker image build \
             API: build steps run uncapped. Only the concurrency limit of {} is enforced.",
            caps.cpu_cores, caps.memory_mb, permits
        );
    }
}

/// Largest per-build memory cap the Docker build API accepts through this
/// client: bollard declares `BuildImageOptions.memory` as `Option<i32>`.
const MAX_REQUESTABLE_BUILD_MEMORY_BYTES: i64 = i32::MAX as i64;

/// Legacy per-build caps used when no override is configured: half of the
/// host's CPUs and half of its RAM in whole GiB, each with a floor of 2.
/// `total_memory_bytes` is what `sysinfo::System::total_memory` returns.
fn legacy_build_caps(cpu_count: usize, total_memory_bytes: u64) -> (usize, u64) {
    let total_memory_gib = total_memory_bytes / (1024 * 1024 * 1024);
    (
        std::cmp::max(2, cpu_count / 2),
        std::cmp::max(2, total_memory_gib / 2),
    )
}

/// Reduce a requested per-build memory cap to what the build API accepts.
/// Returns the value to send and whether it had to be reduced.
fn clamp_build_memory(requested_bytes: i64) -> (i32, bool) {
    if requested_bytes > MAX_REQUESTABLE_BUILD_MEMORY_BYTES {
        (i32::MAX, true)
    } else {
        (requested_bytes.max(0) as i32, false)
    }
}

/// Explain why a per-build memory cap was lowered before it was sent.
///
/// `requested_bytes` is what Settings > Build Limits (or, when no memory cap
/// is set there, the half-of-host-RAM default) asked for; `sent_bytes` is
/// the value actually sent. The message names both, where the requested value
/// came from, and what the builder does with the sent value, so it never reads
/// as if the larger number were still being requested.
fn describe_clamped_build_memory(
    image_name: &str,
    requested_bytes: i64,
    sent_bytes: i32,
    from_settings: bool,
    use_buildkit: bool,
) -> String {
    let requested_mb = requested_bytes / (1024 * 1024);
    let sent_mb = i64::from(sent_bytes) / (1024 * 1024);
    let source = if from_settings {
        "set in Settings > Build Limits"
    } else {
        "the default of half this host's RAM, since Settings > Build Limits sets no memory cap"
    };
    let effect = if use_buildkit {
        "BuildKit ignores this value, so the build is not memory-capped".to_string()
    } else {
        format!(
            "the build is capped at {sent_mb} MB. To choose the cap explicitly, set a memory \
             cap of at most {sent_mb} MB in Settings > Build Limits"
        )
    };
    format!(
        "Build {image_name}: the per-build memory cap of {requested_mb} MB ({source}) is larger \
         than the {sent_mb} MB maximum the Docker build API client can send, so {sent_mb} MB \
         was sent instead; {effect}."
    )
}

/// The one-line build log summary of the memory cap sent with a build.
pub(super) fn describe_build_memory_request(
    image_name: &str,
    sent_bytes: i32,
    use_buildkit: bool,
) -> String {
    let sent_mb = i64::from(sent_bytes) / (1024 * 1024);
    if use_buildkit {
        format!(
            "Build {image_name}: sending a per-build memory cap of {sent_mb} MB; this host builds \
             with BuildKit, which does not enforce it"
        )
    } else {
        format!("Build {image_name}: per-build memory cap of {sent_mb} MB")
    }
}

impl DockerRuntime {
    /// Apply build concurrency + per-build resource caps. Called by the
    /// control-plane deployer plugin after reading `AppSettings.build_limits`;
    /// worker nodes and tests leave this unset and inherit the legacy
    /// 50%-of-host heuristic.
    ///
    /// `max_concurrent` is clamped to a minimum of 1 — a gate that admits
    /// nobody would deadlock every build. `resource_limits` is only honoured
    /// when `cpu_cores > 0` AND `memory_mb > 0`; either zero means "leave that
    /// dimension on the legacy heuristic" so admins can tune one without
    /// the other.
    ///
    /// These are the limits at startup; later changes arrive through
    /// [`Self::spawn_build_limits_watcher`].
    pub fn with_build_limits(
        self,
        max_concurrent: u32,
        resource_limits: Option<BuildResourceLimits>,
    ) -> Self {
        apply_build_limits(
            &self.build_limits,
            self.use_buildkit,
            max_concurrent,
            resource_limits,
        );
        self
    }

    /// Follow `AppSettings.build_limits` for the life of the process.
    ///
    /// The current value is applied first, then every change as it is
    /// published, so saving the settings takes effect without restarting
    /// `temps serve`. A raised concurrency limit starts queued builds
    /// immediately; a lowered one leaves running builds alone and gates new
    /// ones until enough finish. Per-build caps are read as each build
    /// starts, so builds already running keep the caps they started with.
    ///
    /// The task ends when the sender (the settings service) is dropped.
    pub fn spawn_build_limits_watcher(
        &self,
        mut updates: tokio::sync::watch::Receiver<Option<temps_core::BuildLimitsSettings>>,
    ) -> tokio::task::JoinHandle<()> {
        let limits = self.build_limits.clone();
        let use_buildkit = self.use_buildkit;
        tokio::spawn(async move {
            loop {
                // `None` until the settings have been read once; keep the
                // limits the runtime started with until then.
                let latest = updates.borrow_and_update().clone();
                if let Some(settings) = latest {
                    apply_build_limits(
                        &limits,
                        use_buildkit,
                        settings.max_concurrent,
                        BuildResourceLimits::from_settings(&settings),
                    );
                }
                if updates.changed().await.is_err() {
                    debug!("Settings service stopped; no longer following build limits");
                    break;
                }
            }
        })
    }

    fn get_resource_limits() -> (usize, u64) {
        let mut sys = System::new();
        sys.refresh_memory();
        legacy_build_caps(num_cpus::get(), sys.total_memory())
    }

    /// The `memory` value to put on `BuildImageOptions` for a requested cap.
    ///
    /// Logs the reduction when the request had to be lowered to what the API
    /// accepts: as a warning on the legacy builder, which enforces the value,
    /// and only at debug level on BuildKit, which ignores it (the build runs
    /// uncapped either way, so a warning there would be noise on every build).
    pub(super) fn effective_build_memory(
        &self,
        requested_bytes: i64,
        caps_configured: bool,
        image_name: &str,
    ) -> i32 {
        let (memory, clamped) = clamp_build_memory(requested_bytes);
        if clamped {
            let message = describe_clamped_build_memory(
                image_name,
                requested_bytes,
                memory,
                caps_configured,
                self.use_buildkit,
            );
            if self.use_buildkit {
                debug!("{}", message);
            } else {
                warn!("{}", message);
            }
        }
        memory
    }

    /// Resolve the per-build `(memory_bytes, cpu_quota_us, cpu_period_us)`
    /// triplet that gets forwarded to `BuildImageOptions`.
    ///
    /// Priority:
    /// 1. Explicit override set via [`Self::with_build_limits`] or the
    ///    settings watcher (control plane reads `AppSettings.build_limits`).
    ///    Read on every call, so a change applies to the next build.
    /// 2. Legacy 50%-of-host heuristic in `get_resource_limits` — kept so
    ///    worker nodes, tests, and fresh installs that haven't visited
    ///    the settings page see the same behaviour as before.
    ///
    /// Always uses `cpu_period_us = 100_000` (100 ms) — Docker's standard
    /// period. CPU quota for N cores is `N * cpu_period_us`.
    #[cfg(test)]
    fn resolve_build_resource_caps(&self) -> (i64, i32, i32) {
        Self::resolve_build_resource_caps_for(self.build_limits.resource_caps())
    }

    /// [`Self::resolve_build_resource_caps`] for caps the caller already
    /// read, so one build uses a single reading for every decision.
    pub(super) fn resolve_build_resource_caps_for(
        configured: Option<BuildResourceLimits>,
    ) -> (i64, i32, i32) {
        const CPU_PERIOD_US: i32 = 100_000;
        if let Some(override_caps) = configured {
            let memory_bytes = (override_caps.memory_mb as i64) * 1024 * 1024;
            let cpu_quota_us = (override_caps.cpu_cores * CPU_PERIOD_US as f32).round() as i32;
            return (memory_bytes, cpu_quota_us, CPU_PERIOD_US);
        }
        let (cpu_cores, memory_gb) = Self::get_resource_limits();
        let memory_bytes = (memory_gb as i64) * 1024 * 1024 * 1024;
        let cpu_quota_us = (cpu_cores as i32) * CPU_PERIOD_US;
        (memory_bytes, cpu_quota_us, CPU_PERIOD_US)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::docker::DockerRuntime;
    use bollard::Docker;

    /// Without an override, build caps follow the legacy 50%-of-host
    /// heuristic — preserving behaviour for worker nodes and tests that
    /// don't go through the deployer plugin.
    #[test]
    fn resolve_build_resource_caps_falls_back_to_legacy_heuristic() {
        let docker = match Docker::connect_with_local_defaults() {
            Ok(d) => Arc::new(d),
            Err(_) => return, // No docker, skip
        };
        let rt = DockerRuntime::new(docker, false, "test-network".to_string());
        let (memory_bytes, cpu_quota_us, cpu_period_us) = rt.resolve_build_resource_caps();

        // Legacy heuristic uses GB integer × 1 GiB. So memory_bytes must
        // be a multiple of 1 GiB, and cpu_quota_us a multiple of 100_000.
        assert_eq!(cpu_period_us, 100_000);
        assert_eq!(memory_bytes % (1024 * 1024 * 1024), 0);
        assert_eq!(cpu_quota_us % 100_000, 0);
        // Heuristic enforces a 2 core / 2 GiB floor.
        assert!(memory_bytes >= 2 * 1024 * 1024 * 1024);
        assert!(cpu_quota_us >= 200_000); // 2 cores at 100ms period
    }

    /// An explicit override (control plane reading `AppSettings.build_limits`)
    /// produces exact byte/microsecond values regardless of host size.
    #[test]
    fn resolve_build_resource_caps_honours_override() {
        let docker = match Docker::connect_with_local_defaults() {
            Ok(d) => Arc::new(d),
            Err(_) => return, // No docker, skip
        };
        let rt = DockerRuntime::new(docker, false, "test-network".to_string()).with_build_limits(
            4,
            Some(BuildResourceLimits {
                cpu_cores: 1.5,
                memory_mb: 1024,
            }),
        );
        let (memory_bytes, cpu_quota_us, cpu_period_us) = rt.resolve_build_resource_caps();
        assert_eq!(memory_bytes, 1024 * 1024 * 1024); // 1024 MB exactly
        assert_eq!(cpu_quota_us, 150_000); // 1.5 × 100ms
        assert_eq!(cpu_period_us, 100_000);
    }

    /// A runtime whose client never connects (port 1 refuses), so the
    /// build-limit tests run on hosts without a daemon instead of skipping.
    fn offline_runtime(use_buildkit: bool) -> DockerRuntime {
        let unreachable =
            Docker::connect_with_http("http://127.0.0.1:1", 1, bollard::API_DEFAULT_VERSION)
                .expect("client construction makes no connection");
        DockerRuntime::new(
            Arc::new(unreachable),
            use_buildkit,
            "test-network".to_string(),
        )
    }

    fn caps(cpu_cores: f32, memory_mb: u32) -> Option<BuildResourceLimits> {
        Some(BuildResourceLimits {
            cpu_cores,
            memory_mb,
        })
    }

    fn gate_limit(rt: &DockerRuntime) -> usize {
        rt.build_limits
            .gate()
            .expect("build limits were applied")
            .limit()
    }

    /// `with_build_limits(0, ..)` must NOT create a gate that admits nobody
    /// — that would deadlock every build. Clamp to 1.
    #[test]
    fn with_build_limits_clamps_max_concurrent_to_at_least_one() {
        let rt = offline_runtime(false).with_build_limits(0, None);
        assert_eq!(gate_limit(&rt), 1);
    }

    /// A partial override (cpu set, memory zero) must be discarded entirely
    /// so admins don't accidentally pin only one dimension and starve the
    /// build on the other.
    #[test]
    fn with_build_limits_drops_override_when_either_dimension_is_zero() {
        let rt = offline_runtime(false).with_build_limits(2, caps(1.5, 0));
        assert!(rt.build_limits.resource_caps().is_none());
        let rt = offline_runtime(false).with_build_limits(2, caps(0.0, 512));
        assert!(rt.build_limits.resource_caps().is_none());
    }

    #[test]
    fn from_settings_needs_both_cpu_and_memory() {
        let both = temps_core::BuildLimitsSettings {
            max_concurrent: 2,
            cpu_limit_cores: 1.5,
            memory_limit_mb: 1024,
            ..temps_core::BuildLimitsSettings::default()
        };
        assert_eq!(BuildResourceLimits::from_settings(&both), caps(1.5, 1024));
        let no_memory = temps_core::BuildLimitsSettings {
            memory_limit_mb: 0,
            ..both.clone()
        };
        assert_eq!(BuildResourceLimits::from_settings(&no_memory), None);
        let no_cpu = temps_core::BuildLimitsSettings {
            cpu_limit_cores: 0.0,
            ..both
        };
        assert_eq!(BuildResourceLimits::from_settings(&no_cpu), None);
    }

    /// Per-build caps are read each time a build resolves them, so a change
    /// applies to the next build with no restart.
    #[test]
    fn resolve_build_resource_caps_follows_live_changes() {
        let rt = offline_runtime(false).with_build_limits(2, caps(1.0, 512));
        assert_eq!(
            rt.resolve_build_resource_caps(),
            (512 * 1024 * 1024, 100_000, 100_000)
        );

        apply_build_limits(&rt.build_limits, false, 2, caps(2.5, 2000));
        assert_eq!(
            rt.resolve_build_resource_caps(),
            (2000 * 1024 * 1024, 250_000, 100_000)
        );

        // Clearing a dimension returns to the legacy heuristic.
        apply_build_limits(&rt.build_limits, false, 2, caps(2.5, 0));
        let (memory_bytes, _, _) = rt.resolve_build_resource_caps();
        assert_eq!(memory_bytes % (1024 * 1024 * 1024), 0);
    }

    #[test]
    fn live_apply_reports_previous_limits_only_after_the_first_time() {
        let limits = LiveBuildLimits::default();
        assert!(limits.apply(2, caps(1.0, 512)).is_none());
        assert_eq!(
            limits.apply(4, None),
            Some(BuildLimitsSnapshot {
                max_concurrent: 2,
                resource_caps: caps(1.0, 512),
            })
        );
        assert_eq!(
            limits.apply(0, None),
            Some(BuildLimitsSnapshot {
                max_concurrent: 4,
                resource_caps: None,
            })
        );
        assert_eq!(limits.gate().map(|gate| gate.limit()), Some(1));
    }

    /// Raising `max_concurrent` while builds are queued lets them run.
    #[tokio::test]
    async fn raising_max_concurrent_lets_queued_builds_proceed() {
        let rt = offline_runtime(false).with_build_limits(1, None);
        let gate = rt.build_limits.gate().expect("gate").clone();
        let _running = gate.acquire().await;

        let queued = {
            let gate = gate.clone();
            tokio::spawn(async move { gate.acquire().await })
        };
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        assert!(!queued.is_finished(), "the only slot is taken");

        apply_build_limits(&rt.build_limits, false, 2, None);
        let permit = tokio::time::timeout(std::time::Duration::from_secs(5), queued)
            .await
            .expect("queued build runs after the limit is raised")
            .expect("task joins");
        assert_eq!(gate.running(), 2);
        drop(permit);
    }

    /// Lowering `max_concurrent` leaves running builds alone and makes new
    /// ones wait for the new limit.
    #[tokio::test]
    async fn lowering_max_concurrent_keeps_running_builds_and_gates_new_ones() {
        let rt = offline_runtime(false).with_build_limits(3, None);
        let gate = rt.build_limits.gate().expect("gate").clone();
        let a = gate.acquire().await;
        let b = gate.acquire().await;
        let c = gate.acquire().await;

        apply_build_limits(&rt.build_limits, false, 1, None);
        assert_eq!(gate.running(), 3, "no running build is cancelled");

        let queued = {
            let gate = gate.clone();
            tokio::spawn(async move { gate.acquire().await })
        };
        drop(a);
        drop(b);
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        assert!(!queued.is_finished(), "one build still runs at limit 1");

        drop(c);
        let permit = tokio::time::timeout(std::time::Duration::from_secs(5), queued)
            .await
            .expect("queued build runs once under the new limit")
            .expect("task joins");
        assert_eq!(gate.running(), 1);
        drop(permit);
    }

    /// The diagnostics count other builds from the live permit count, so they
    /// stay right after the limit changes.
    #[tokio::test]
    async fn other_builds_at_start_tracks_running_builds_across_limit_changes() {
        let rt = offline_runtime(false).with_build_limits(2, None);
        let gate = rt.build_limits.gate().expect("gate").clone();
        let _first = gate.acquire().await;
        let _second = gate.acquire().await;
        assert_eq!(rt.sample_build_start().others_at_start, Some(1));

        apply_build_limits(&rt.build_limits, false, 1, None);
        assert_eq!(rt.sample_build_start().others_at_start, Some(1));

        apply_build_limits(&rt.build_limits, false, 8, None);
        let _third = gate.acquire().await;
        assert_eq!(rt.sample_build_start().others_at_start, Some(2));
    }

    /// The watcher applies the current settings, then each change.
    #[tokio::test]
    async fn watcher_applies_current_and_later_settings() {
        let rt = offline_runtime(false).with_build_limits(2, None);
        let settings = |max_concurrent, cpu_limit_cores, memory_limit_mb| {
            Some(temps_core::BuildLimitsSettings {
                max_concurrent,
                cpu_limit_cores,
                memory_limit_mb,
                ..temps_core::BuildLimitsSettings::default()
            })
        };
        let (tx, rx) = tokio::sync::watch::channel(settings(3, 1.0, 256));
        let watcher = rt.spawn_build_limits_watcher(rx);

        wait_for(|| gate_limit(&rt) == 3 && rt.build_limits.resource_caps() == caps(1.0, 256))
            .await;

        tx.send_replace(settings(1, 0.0, 0));
        wait_for(|| gate_limit(&rt) == 1 && rt.build_limits.resource_caps().is_none()).await;

        // A value of `None` (settings not read yet) changes nothing.
        tx.send_replace(None);
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        assert_eq!(gate_limit(&rt), 1);

        drop(tx);
        tokio::time::timeout(std::time::Duration::from_secs(5), watcher)
            .await
            .expect("watcher ends with its sender")
            .expect("watcher task joins");
    }

    /// A runtime that started without limits (settings unreadable at boot)
    /// picks them up when the first settings arrive.
    #[tokio::test]
    async fn watcher_enables_limits_on_a_runtime_that_started_without() {
        let rt = offline_runtime(false);
        assert!(rt.build_limits.gate().is_none());
        let (tx, rx) = tokio::sync::watch::channel(None);
        let _watcher = rt.spawn_build_limits_watcher(rx);

        tx.send_replace(Some(temps_core::BuildLimitsSettings {
            max_concurrent: 2,
            cpu_limit_cores: 0.0,
            memory_limit_mb: 0,
            ..temps_core::BuildLimitsSettings::default()
        }));
        wait_for(|| rt.build_limits.gate().is_some()).await;
        assert_eq!(gate_limit(&rt), 2);
    }

    async fn wait_for(condition: impl Fn() -> bool) {
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        while !condition() {
            assert!(
                std::time::Instant::now() < deadline,
                "condition not reached within 5s"
            );
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
    }

    #[test]
    fn legacy_build_caps_use_half_of_host_memory_in_gib() {
        // sysinfo reports bytes; a 16 GiB host must yield 8 GiB, not the 8192
        // "GB" the old KB-to-GB arithmetic produced.
        assert_eq!(legacy_build_caps(16, 16 * 1024 * 1024 * 1024), (8, 8));
        // The reference box (3 vCPU / 4 GB) lands on both floors.
        assert_eq!(legacy_build_caps(3, 4_092_583_936), (2, 2));
        assert_eq!(legacy_build_caps(1, 1024 * 1024 * 1024), (2, 2));
    }

    #[test]
    fn clamp_build_memory_reduces_only_above_the_api_maximum() {
        assert_eq!(clamp_build_memory(512 * 1024 * 1024), (536_870_912, false));
        assert_eq!(clamp_build_memory(i32::MAX as i64), (i32::MAX, false));
        assert_eq!(clamp_build_memory(8 * 1024 * 1024 * 1024), (i32::MAX, true));
        assert_eq!(clamp_build_memory(-1), (0, false));
    }

    #[test]
    fn clamped_build_memory_message_names_the_sent_value_not_the_requested_one() {
        let requested = 262_144_i64 * 1024 * 1024;
        let (sent, clamped) = clamp_build_memory(requested);
        assert!(clamped);

        let legacy = describe_clamped_build_memory("app:1", requested, sent, false, false);
        assert!(legacy.contains("cap of 262144 MB"), "{legacy}");
        assert!(legacy.contains("half this host's RAM"), "{legacy}");
        assert!(legacy.contains("so 2047 MB was sent instead"), "{legacy}");
        assert!(
            legacy.contains("the build is capped at 2047 MB"),
            "{legacy}"
        );
        // The old wording ended "requesting <requested> MB instead", which
        // contradicted the reduction it was reporting.
        assert!(!legacy.contains("requesting 262144 MB"), "{legacy}");

        let configured = describe_clamped_build_memory("app:1", requested, sent, true, false);
        assert!(
            configured.contains("set in Settings > Build Limits"),
            "{configured}"
        );

        let buildkit = describe_clamped_build_memory("app:1", requested, sent, false, true);
        assert!(
            buildkit.contains("BuildKit ignores this value, so the build is not memory-capped"),
            "{buildkit}"
        );
        assert!(!buildkit.contains("capped at 2047 MB"), "{buildkit}");
    }

    #[test]
    fn build_memory_request_summary_says_whether_the_cap_is_enforced() {
        let sent = i32::MAX;
        assert_eq!(
            describe_build_memory_request("app:1", sent, false),
            "Build app:1: per-build memory cap of 2047 MB"
        );
        assert!(describe_build_memory_request("app:1", sent, true)
            .ends_with("BuildKit, which does not enforce it"));
    }
}
