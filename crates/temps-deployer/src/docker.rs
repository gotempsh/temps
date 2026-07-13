//! Docker implementation of ImageBuilder and ContainerDeployer traits

use crate::{
    BuildRequest, BuildResult, BuilderError, ContainerDeployer, ContainerInfo, ContainerRuntime,
    ContainerStatus, DeployRequest, DeployResult, DeployerError, ImageBuilder, PortMapping,
    Protocol, RuntimeInfo,
};
use async_trait::async_trait;
use bollard::{
    query_parameters::{
        BuilderVersion, InspectContainerOptions, ListContainersOptions, LogsOptions,
        RemoveContainerOptions, StartContainerOptions, StopContainerOptions, TagImageOptions,
    },
    Docker,
};
use futures::{Stream, StreamExt, TryStreamExt};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Instant;
use sysinfo::System;
use tempfile::TempDir;
use tokio::io::AsyncWriteExt;
use tracing::{debug, error, info, warn};

/// Extracts `nameserver` entries from resolv.conf-formatted text, excluding
/// loopback addresses — meaningless inside a container's own network
/// namespace, and the classic case for systemd-resolved's `127.0.0.53`
/// stub. Pure/deterministic so it's unit-testable without touching the
/// filesystem; see [`host_default_dns_servers`] for the I/O side.
fn parse_non_loopback_nameservers(resolv_conf: &str) -> Vec<String> {
    resolv_conf
        .lines()
        .filter_map(|line| line.trim().strip_prefix("nameserver"))
        .map(str::trim)
        .filter(|ip| {
            ip.parse::<std::net::IpAddr>()
                .map(|addr| !addr.is_loopback())
                .unwrap_or(false)
        })
        .map(str::to_string)
        .collect()
}

/// Best-effort read of the host's own already-configured DNS servers, so
/// they — not a hardcoded public resolver — can be appended as a fallback
/// after the per-node Hickory resolver. Air-gapped/on-prem installs often
/// route DNS to an internal-only server; assuming 1.1.1.1/8.8.8.8 would be
/// unreachable there, and would leak queries externally for operators who
/// specifically kept resolution internal.
///
/// Checks `/etc/resolv.conf` first. On systemd-resolved hosts that file is
/// usually just the loopback stub (`127.0.0.53`, unreachable from inside a
/// container's own netns), so this falls through to
/// `/run/systemd/resolve/resolv.conf`, which systemd-resolved maintains
/// specifically to expose the *real* upstream servers to tools — Docker's
/// own embedded-DNS "ExtServers" detection reads the same file for the same
/// reason.
fn host_default_dns_servers() -> Vec<String> {
    for path in ["/etc/resolv.conf", "/run/systemd/resolve/resolv.conf"] {
        if let Ok(contents) = std::fs::read_to_string(path) {
            let servers = parse_non_loopback_nameservers(&contents);
            if !servers.is_empty() {
                return servers;
            }
        }
    }
    Vec::new()
}

/// Appends entries from `fallback_pool` to `primary`, deduplicated and
/// capped at 3 total entries — glibc and musl both fail over to the next
/// `nameserver` line once the current one times out, but both also ignore
/// anything past the third line in `/etc/resolv.conf`.
fn merge_dns_with_fallback(mut primary: Vec<String>, fallback_pool: &[String]) -> Vec<String> {
    for fallback in fallback_pool {
        if primary.len() >= 3 {
            break;
        }
        if !primary.contains(fallback) {
            primary.push(fallback.clone());
        }
    }
    primary
}

/// Appends the host's own already-configured DNS servers (see
/// [`host_default_dns_servers`]) to `primary`, so `temps-dns-resolver` is
/// never a single point of failure for outbound DNS.
///
/// Shared by the app-container path ([`DockerRuntime::dns_for_container`])
/// and the managed-service path (`temps_agent::service_handlers`) so
/// neither one can silently regress into wiring the Hickory resolver as the
/// sole nameserver.
pub fn dns_with_fallback(primary: Vec<String>) -> Vec<String> {
    merge_dns_with_fallback(primary, &host_default_dns_servers())
}

pub struct DockerRuntime {
    docker: Arc<Docker>,
    use_buildkit: bool,
    network_name: String,
    /// Address to bind host ports to (e.g. "127.0.0.1" for local, "0.0.0.0" for remote agents)
    host_bind_address: String,
    /// Optional secondary network for multi-host overlay (e.g. "temps-overlay").
    /// When set, every container is additionally connected to this network
    /// after creation. Skipped silently when the network doesn't exist —
    /// that's the legitimate "overlay not yet bootstrapped on this node"
    /// state, not an error. Set via [`Self::with_overlay_network`].
    overlay_network: Option<String>,
    /// Static resolvers to write into each new container's
    /// `/etc/resolv.conf`. Use [`Self::with_dns_servers`] for tests or
    /// fixed-IP setups; in the live agent we use
    /// [`Self::with_overlay_dns_slot`] instead, which reads the bridge
    /// IP dynamically from the network_sync loop's shared slot (the
    /// bridge IP isn't known until the overlay has bootstrapped). Never
    /// used as-is: [`Self::dns_for_container`] appends the host's own
    /// default DNS servers ([`dns_with_fallback`]) so this is never the
    /// sole nameserver.
    dns_servers: Vec<String>,
    /// Optional dynamic DNS slot — populated by the agent's
    /// `network_sync` loop after the overlay bridge is up. Read on
    /// every `deploy_container` so containers booted after the
    /// overlay is up get the per-node Hickory resolver wired in. The
    /// static `dns_servers` field takes precedence when both are set.
    /// Never used as-is: [`Self::dns_for_container`] appends the host's
    /// own default DNS servers ([`dns_with_fallback`]) so this is never the
    /// sole nameserver.
    overlay_dns_slot: Option<Arc<std::sync::RwLock<Option<std::net::IpAddr>>>>,
    /// Shared snapshot of overlay peers, refreshed by the agent's
    /// `network_sync` loop. After dual-attaching a container to the
    /// overlay, we install one route per peer inside the container's
    /// netns so traffic for *other* workers' overlay /24s leaves
    /// through the overlay interface (eth1) rather than falling
    /// through the container's default route on the primary network.
    /// Wrapped in `Option` so non-overlay deployers (e.g. tests) can
    /// skip the wiring entirely.
    overlay_peers: Option<Arc<std::sync::RwLock<Vec<temps_network::Peer>>>>,
    /// Root directory on the host where per-container secret files are
    /// materialized for bind-mounting into `/run/secrets`. Defaults to
    /// `$TEMPS_DATA_DIR/secrets` (falling back to `$HOME/.temps/secrets`
    /// and finally `./.temps/secrets`) so secret mounts SURVIVE A HOST
    /// REBOOT — historically this lived under `std::env::temp_dir()`,
    /// which is tmpfs on most Linux distros and got wiped on every
    /// reboot, forcing a redeploy. Override via [`Self::with_secrets_root`].
    secrets_root: PathBuf,
    /// Optional global cap on concurrent `build_image` calls. Set via
    /// [`Self::with_build_limits`] on the control plane to prevent N
    /// simultaneous deploys from each grabbing 50% of host CPU/RAM and
    /// taking down the box. None on worker nodes and in tests — they get
    /// the legacy unbounded behaviour.
    build_semaphore: Option<Arc<tokio::sync::Semaphore>>,
    /// Per-build resource override forwarded to `BuildImageOptions`. None
    /// preserves the legacy 50%-of-host heuristic in `get_resource_limits`.
    build_resource_override: Option<BuildResourceLimits>,
}

/// Explicit per-build resource caps, set by the control plane from
/// `AppSettings.build_limits`. Both fields use the same units the rest of
/// the deployer code already uses (cores + MB) so there's no conversion
/// surface between settings and the Docker API call.
#[derive(Debug, Clone, Copy)]
pub struct BuildResourceLimits {
    /// CPU cores allowed per build, e.g. `2.0` = 2 full cores.
    pub cpu_cores: f32,
    /// Memory allowed per build, in megabytes.
    pub memory_mb: u32,
}

/// Map a POSIX exit code (>= 128 means "killed by signal N - 128") to a human
/// signal name. Returns None for normal-range exit codes.
fn signal_name_from_exit_code(code: i64) -> Option<&'static str> {
    if !(128..=192).contains(&code) {
        return None;
    }
    match (code - 128) as i32 {
        1 => Some("SIGHUP"),
        2 => Some("SIGINT"),
        3 => Some("SIGQUIT"),
        4 => Some("SIGILL"),
        6 => Some("SIGABRT"),
        8 => Some("SIGFPE"),
        9 => Some("SIGKILL"),
        11 => Some("SIGSEGV"),
        13 => Some("SIGPIPE"),
        14 => Some("SIGALRM"),
        15 => Some("SIGTERM"),
        _ => None,
    }
}

/// Build a short human-readable explanation of why a container is in its
/// current state. Returns None for containers that are still running, have
/// never been started, or exited cleanly with status 0 and no Docker error.
pub(crate) fn build_exit_reason(
    status: &ContainerStatus,
    oom_killed: Option<bool>,
    exit_code: Option<i64>,
    error: Option<&str>,
) -> Option<String> {
    let is_terminal = matches!(
        status,
        ContainerStatus::Exited | ContainerStatus::Dead | ContainerStatus::Stopped
    );
    if !is_terminal {
        return None;
    }
    if oom_killed == Some(true) {
        return Some(match exit_code {
            Some(code) => format!("OOMKilled (exit code {})", code),
            None => "OOMKilled".to_string(),
        });
    }
    if let Some(err) = error.filter(|e| !e.is_empty()) {
        return Some(match exit_code {
            Some(code) => format!("Error (exit code {}): {}", code, err),
            None => format!("Error: {}", err),
        });
    }
    match exit_code {
        Some(0) => None,
        Some(code) => Some(match signal_name_from_exit_code(code) {
            Some(sig) => format!("Killed by {} (exit code {})", sig, code),
            None => format!("Exit code {}", code),
        }),
        None => None,
    }
}

/// Sample container stats twice ~1s apart so the CPU delta formula has a real
/// window to work with. Docker's `one_shot: true` returns immediately but
/// leaves `precpu_stats` empty, which makes the single-sample CPU math
/// collapse to ~0% for any container that's been running long enough that
/// (cumulative_cpu_time / system_time_since_boot) rounds to zero. The 1s
/// interval matches the Docker CLI default.
async fn sample_container_stats_twice(
    docker: &Docker,
    container_id: &str,
) -> Result<
    (
        bollard::models::ContainerStatsResponse,
        bollard::models::ContainerStatsResponse,
    ),
    bollard::errors::Error,
> {
    use bollard::query_parameters::StatsOptions;

    let opts = StatsOptions {
        stream: false,
        one_shot: true,
    };

    let mut first_stream = docker.stats(container_id, Some(opts.clone()));
    let first = first_stream
        .try_next()
        .await?
        .ok_or_else(|| bollard::errors::Error::IOError {
            err: std::io::Error::new(
                std::io::ErrorKind::UnexpectedEof,
                "first stats sample produced no frames",
            ),
        })?;
    drop(first_stream);

    tokio::time::sleep(std::time::Duration::from_secs(1)).await;

    let mut second_stream = docker.stats(container_id, Some(opts));
    let second =
        second_stream
            .try_next()
            .await?
            .ok_or_else(|| bollard::errors::Error::IOError {
                err: std::io::Error::new(
                    std::io::ErrorKind::UnexpectedEof,
                    "second stats sample produced no frames",
                ),
            })?;

    Ok((first, second))
}

/// Compute the Docker-CLI-equivalent CPU percent from two consecutive stats
/// samples.
///
/// Formula (matches `docker stats`):
/// ```text
/// cpu_delta    = current.total_usage     - previous.total_usage
/// system_delta = current.system_cpu_usage - previous.system_cpu_usage
/// percent      = (cpu_delta / system_delta) * online_cpus * 100
/// ```
///
/// Returns `None` when counters are missing or the delta is zero/negative
/// (container just started, stopped, or clock skew). A fully pinned 4-core
/// container correctly reads as 400% — we deliberately do NOT clamp to 100,
/// because the UI shows "CPU x.x% / N cores" and capping the numerator hides
/// real over-saturation.
fn cpu_percent_from_samples(
    current: &bollard::models::ContainerStatsResponse,
    previous: &bollard::models::ContainerStatsResponse,
) -> Option<f64> {
    let cur_cpu = current.cpu_stats.as_ref()?;
    let prev_cpu = previous.cpu_stats.as_ref()?;

    let cur_total = cur_cpu.cpu_usage.as_ref()?.total_usage? as i128;
    let prev_total = prev_cpu.cpu_usage.as_ref()?.total_usage? as i128;
    let cur_system = cur_cpu.system_cpu_usage? as i128;
    let prev_system = prev_cpu.system_cpu_usage? as i128;

    let cpu_delta = cur_total - prev_total;
    let system_delta = cur_system - prev_system;

    if cpu_delta <= 0 || system_delta <= 0 {
        return None;
    }

    let cpus = cur_cpu.online_cpus.unwrap_or(1).max(1) as f64;
    let percent = (cpu_delta as f64 / system_delta as f64) * cpus * 100.0;
    if percent.is_finite() && percent >= 0.0 {
        Some(percent)
    } else {
        None
    }
}

/// Subtract reclaimable page cache from raw memory usage so the number
/// matches `docker stats`'s "MEM USAGE" column.
///
/// Docker reports `usage` straight from cgroups, which includes the page
/// cache. A Postgres container with an 8 GB working set + 8 GB of file
/// cache reads back as `usage == limit` on a 16 GB cap even though only
/// half of that is real RSS. The Docker CLI compensates by subtracting:
/// - cgroup v2: `stats.inactive_file`
/// - cgroup v1: `stats.cache`
///
/// We prefer `inactive_file` (cgroup v2 is the modern default) and fall
/// back to `cache`. If neither key is present, return the raw usage —
/// better to slightly over-report than to crash on a missing field.
fn memory_usage_excluding_cache(mem: &bollard::models::ContainerMemoryStats) -> Option<u64> {
    let raw_usage = mem.usage?;
    let cache = mem
        .stats
        .as_ref()
        .and_then(|s| s.get("inactive_file").or_else(|| s.get("cache")).copied());
    match cache {
        Some(c) if c <= raw_usage => Some(raw_usage - c),
        _ => Some(raw_usage),
    }
}

impl DockerRuntime {
    /// Per-container directory that holds plaintext secret files for bind-mounting
    /// into `/run/secrets`. Lives outside `TempDir` because Docker keeps the
    /// directory open for the container lifetime; cleanup happens in
    /// `remove_container`.
    ///
    /// Lives under `secrets_root` (defaults to `$TEMPS_DATA_DIR/secrets`)
    /// rather than `/tmp` so the bind mount survives a host reboot —
    /// `/tmp` is tmpfs on most Linux distros and gets wiped, leaving
    /// containers with empty `/run/secrets` until the next redeploy.
    fn secrets_host_dir(&self, container_name: &str) -> PathBuf {
        self.secrets_root.join(container_name)
    }

    /// Resolves the numeric (uid, gid) that the container will run as,
    /// by inspecting the image's `Config.User`. Used to `chown` the
    /// bind-mounted secrets directory so the app can read its secret
    /// files even when the image runs as a non-root user (e.g. distroless
    /// nonroot = 65532:65532).
    ///
    /// Falls back to `(0, 0)` — matching Docker's default when no USER is
    /// set — on inspect failure or when the USER is a named user we can't
    /// resolve without reading `/etc/passwd` from inside the image. Named
    /// users are logged as a warning so it's obvious why secrets might be
    /// unreadable for images like `node:alpine` (USER=node).
    async fn resolve_image_user(&self, image_name: &str) -> (u32, u32) {
        let inspect = match self.docker.inspect_image(image_name).await {
            Ok(i) => i,
            Err(e) => {
                warn!(
                    "Failed to inspect image '{}' for secrets chown; defaulting to root: {}",
                    image_name, e
                );
                return (0, 0);
            }
        };

        let user_spec = inspect
            .config
            .as_ref()
            .and_then(|c| c.user.as_ref())
            .map(|u| u.as_str())
            .unwrap_or("");

        match parse_numeric_user_spec(user_spec) {
            Some(pair) => pair,
            None => {
                warn!(
                    "Image '{}' declares USER='{}' which is not numeric; \
                     secrets will be owned by root and may be unreadable. \
                     Use numeric UIDs (e.g. USER 1000:1000) for secrets \
                     to work with this image.",
                    image_name, user_spec
                );
                (0, 0)
            }
        }
    }

    pub fn new(docker: Arc<Docker>, use_buildkit: bool, network_name: String) -> Self {
        let secrets_root = default_secrets_root();
        // Best-effort: ensure the root exists with restrictive perms so the
        // first deploy after a fresh install doesn't race with the per-container
        // mkdir. Failures are logged and not fatal — the per-deploy code path
        // creates the leaf directory regardless.
        if let Err(e) = std::fs::create_dir_all(&secrets_root) {
            warn!(
                "Failed to pre-create secrets root {}: {} (will retry per-deploy)",
                secrets_root.display(),
                e
            );
        } else {
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                let _ =
                    std::fs::set_permissions(&secrets_root, std::fs::Permissions::from_mode(0o700));
            }
        }
        Self {
            docker,
            use_buildkit,
            network_name,
            host_bind_address: "127.0.0.1".to_string(),
            overlay_network: None,
            dns_servers: Vec::new(),
            overlay_dns_slot: None,
            overlay_peers: None,
            secrets_root,
            build_semaphore: None,
            build_resource_override: None,
        }
    }

    /// Apply build concurrency + per-build resource caps. Called by the
    /// control-plane deployer plugin after reading `AppSettings.build_limits`;
    /// worker nodes and tests leave this unset and inherit the legacy
    /// 50%-of-host heuristic.
    ///
    /// `max_concurrent` is clamped to a minimum of 1 — a zero-size semaphore
    /// would deadlock every build. `resource_limits` is only honoured when
    /// `cpu_cores > 0` AND `memory_mb > 0`; either zero means "leave that
    /// dimension on the legacy heuristic" so admins can tune one without
    /// the other.
    pub fn with_build_limits(
        mut self,
        max_concurrent: u32,
        resource_limits: Option<BuildResourceLimits>,
    ) -> Self {
        let permits = max_concurrent.max(1) as usize;
        self.build_semaphore = Some(Arc::new(tokio::sync::Semaphore::new(permits)));
        self.build_resource_override =
            resource_limits.filter(|r| r.cpu_cores > 0.0 && r.memory_mb > 0);
        self
    }

    /// Override the secrets host root. Tests use this to scope writes to
    /// a temp dir; the agent/serve paths leave it at the default.
    pub fn with_secrets_root(mut self, root: impl Into<PathBuf>) -> Self {
        self.secrets_root = root.into();
        self
    }

    /// Read the per-node Hickory resolver IP from a shared slot
    /// populated by the agent's `network_sync` loop after the overlay
    /// bridge is up. Required for app containers to resolve
    /// `*.temps.local` (cluster member FQDNs, service VIPs).
    pub fn with_overlay_dns_slot(
        mut self,
        slot: Arc<std::sync::RwLock<Option<std::net::IpAddr>>>,
    ) -> Self {
        self.overlay_dns_slot = Some(slot);
        self
    }

    /// Wire the agent's shared peer-list snapshot into the deployer.
    /// Used after `connect_network` to add per-peer routes inside the
    /// container's netns so cross-worker overlay traffic actually
    /// flows. Without this, containers can dial peers in their own /24
    /// but anything in a peer worker's /24 falls through to the
    /// primary network and gets dropped at iptables.
    pub fn with_overlay_peers(
        mut self,
        peers: Arc<std::sync::RwLock<Vec<temps_network::Peer>>>,
    ) -> Self {
        self.overlay_peers = Some(peers);
        self
    }

    /// Configure resolvers (typically the per-node Hickory bridge IP) to
    /// write into every new container's `/etc/resolv.conf`. Without this
    /// containers default to Docker's embedded DNS at 127.0.0.11, which
    /// only knows other containers on the same Docker network — it can't
    /// resolve `*.temps.local` cluster FQDNs. Setting this aligns the
    /// app-deploy path with the service-deploy path on the agent.
    pub fn with_dns_servers(mut self, servers: Vec<String>) -> Self {
        self.dns_servers = servers;
        self
    }

    /// Nameservers to write into a new container's `/etc/resolv.conf`.
    ///
    /// The per-node Hickory resolver goes first (static `dns_servers` wins
    /// when set — test/manual setups — otherwise the dynamic
    /// `overlay_dns_slot` the agent's `network_sync` loop publishes), but
    /// [`dns_with_fallback`] ensures it's never the *only* entry, so a
    /// crashed or unreachable resolver degrades to "no `*.temps.local`"
    /// instead of "no DNS at all" for every container on the node.
    ///
    /// Returns `None` when no primary resolver is configured yet (overlay
    /// not bootstrapped), leaving `HostConfig.dns` unset so Docker falls
    /// back to its embedded resolver, which already forwards to the host's
    /// own default DNS — i.e. the safe behaviour, not a regression.
    fn dns_for_container(&self) -> Option<Vec<String>> {
        let primary = if !self.dns_servers.is_empty() {
            self.dns_servers.clone()
        } else {
            self.overlay_dns_slot
                .as_ref()
                .and_then(|slot| slot.read().ok().and_then(|guard| *guard))
                .map(|ip| vec![ip.to_string()])?
        };

        Some(dns_with_fallback(primary))
    }

    /// Set the host bind address for container port mappings.
    /// Use "0.0.0.0" on agent nodes so containers are reachable from the private network.
    pub fn with_host_bind_address(mut self, address: String) -> Self {
        self.host_bind_address = address;
        self
    }

    /// Configure a secondary multi-host overlay network. When set, every
    /// new container is additionally connected to this network *after*
    /// creation, so it ends up with two interfaces:
    ///   eth0 → primary `network_name` (existing behavior, unchanged)
    ///   eth1 → `overlay_network` (cross-node traffic via VXLAN)
    ///
    /// If the overlay network does not exist on this host (the agent's
    /// `network_sync` loop has not bootstrapped it yet, or this node has
    /// no `compute_cidr` allocated), the dual-attach is skipped silently
    /// — the container still boots normally on the primary network.
    pub fn with_overlay_network(mut self, name: impl Into<String>) -> Self {
        self.overlay_network = Some(name.into());
        self
    }

    /// Best-effort additional attachment to the overlay network. Logs and
    /// returns `Ok(())` when the overlay isn't configured or doesn't
    /// exist; only true bollard errors propagate.
    async fn maybe_attach_overlay(&self, container_id: &str) -> Result<(), DeployerError> {
        let Some(overlay) = self.overlay_network.as_deref() else {
            return Ok(());
        };

        // Cheap existence probe: list_networks once. If the overlay
        // doesn't exist yet, skip (sync loop hasn't bootstrapped it).
        let networks = self
            .docker
            .list_networks(None::<bollard::query_parameters::ListNetworksOptions>)
            .await
            .map_err(|e| DeployerError::NetworkError(format!("list_networks: {}", e)))?;
        let exists = networks.iter().any(|n| n.name.as_deref() == Some(overlay));
        if !exists {
            tracing::debug!(
                container = %container_id,
                overlay,
                "overlay network not present; skipping dual-attach"
            );
            return Ok(());
        }

        let req = bollard::models::NetworkConnectRequest {
            container: container_id.to_string(),
            ..Default::default()
        };
        match self.docker.connect_network(overlay, req).await {
            Ok(()) => {
                tracing::info!(container = %container_id, overlay, "attached to overlay");
                Ok(())
            }
            Err(bollard::errors::Error::DockerResponseServerError {
                status_code: 403, ..
            }) => {
                // 403 from /networks/<id>/connect typically means "already
                // connected" — that's a no-op for our purposes.
                tracing::debug!(
                    container = %container_id,
                    overlay,
                    "container already connected to overlay (403)"
                );
                Ok(())
            }
            Err(e) => Err(DeployerError::NetworkError(format!(
                "connect_network({}): {}",
                overlay, e
            ))),
        }
    }

    /// Install per-peer routes inside the container's netns. Must be
    /// called **after** the container is started — `docker inspect`
    /// only reports a non-zero PID for a running container, and we
    /// need that PID to `nsenter` into its network namespace.
    ///
    /// Best-effort: any failure is logged and swallowed so a flaky
    /// route install doesn't break the deploy.
    pub async fn install_overlay_peer_routes(&self, container_id: &str) {
        let Some(overlay) = self.overlay_network.as_deref() else {
            return;
        };
        if let Err(e) = self
            .install_overlay_peer_routes_inner(container_id, overlay)
            .await
        {
            tracing::warn!(
                container = %container_id,
                overlay,
                error = %e,
                "Failed to install overlay peer routes; cross-worker traffic to other CIDRs will fall through the primary network and be dropped"
            );
        }
    }

    async fn install_overlay_peer_routes_inner(
        &self,
        container_id: &str,
        overlay: &str,
    ) -> Result<(), String> {
        let Some(shared) = self.overlay_peers.as_ref() else {
            return Ok(());
        };
        let peers = shared.read().map(|guard| guard.clone()).unwrap_or_default();
        if peers.is_empty() {
            return Ok(());
        }

        let inspect = self
            .docker
            .inspect_container(
                container_id,
                None::<bollard::query_parameters::InspectContainerOptions>,
            )
            .await
            .map_err(|e| format!("inspect_container: {}", e))?;

        let pid = inspect
            .state
            .as_ref()
            .and_then(|s| s.pid)
            .filter(|p| *p > 0)
            .ok_or_else(|| "container PID not yet available".to_string())? as i32;

        let gateway = inspect
            .network_settings
            .as_ref()
            .and_then(|ns| ns.networks.as_ref())
            .and_then(|nets| nets.get(overlay))
            .and_then(|net| net.gateway.clone())
            .filter(|g| !g.is_empty())
            .ok_or_else(|| format!("no gateway recorded for overlay '{}' on container", overlay))?;

        // Convention: Docker assigns interface names in attach order.
        // Primary network = eth0, overlay attach = eth1.
        temps_network::overlay_routes::install_peer_routes_in_container(
            pid, "eth1", &gateway, &peers,
        )
        .await
        .map_err(|e| e.to_string())
    }

    pub async fn ensure_network_exists(&self) -> Result<(), DeployerError> {
        // Check if network exists
        let networks = self
            .docker
            .list_networks(None::<bollard::query_parameters::ListNetworksOptions>)
            .await
            .map_err(|e| DeployerError::NetworkError(format!("Failed to list networks: {}", e)))?;

        let network_exists = networks
            .iter()
            .any(|network| network.name.as_ref() == Some(&self.network_name));

        if !network_exists {
            info!("Creating network: {}", self.network_name);
            let create_options = bollard::models::NetworkCreateRequest {
                name: self.network_name.clone(),
                driver: Some("bridge".to_string()),
                ..Default::default()
            };

            self.docker
                .create_network(create_options)
                .await
                .map_err(|e| {
                    DeployerError::NetworkError(format!("Failed to create network: {}", e))
                })?;
        }

        Ok(())
    }

    /// Read the IPv4 gateway of the app bridge network (`self.network_name`),
    /// e.g. `172.19.0.1`. Used to bind the control-plane DNS resolver on the
    /// bridge gateway (ADR-024). Returns `None` on any inspect failure or if
    /// Docker assigned no IPv4 gateway. We explicitly skip any IPv6 IPAM
    /// config: the resolver binds IPv4, and a dual-stack network can list its
    /// configs in either order — taking the first gateway regardless of family
    /// could hand back an IPv6 address we then fail to bind.
    pub async fn inspect_app_network_gateway(&self) -> Option<std::net::IpAddr> {
        let info = self
            .docker
            .inspect_network(
                &self.network_name,
                None::<bollard::query_parameters::InspectNetworkOptions>,
            )
            .await
            .ok()?;
        info.ipam?
            .config?
            .into_iter()
            .filter_map(|c| c.gateway)
            .filter_map(|gw| gw.parse::<std::net::IpAddr>().ok())
            .find(|ip| ip.is_ipv4())
    }

    async fn create_tar_context_body(
        &self,
        context_path: PathBuf,
    ) -> Result<http_body_util::Full<bytes::Bytes>, BuilderError> {
        use bytes::Bytes;
        use http_body_util::Full;

        // Write the tar archive to a temporary file to avoid holding the entire
        // build context in memory.  The temp file is cleaned up when `_tmp` drops.
        let tmp = tempfile::NamedTempFile::new().map_err(BuilderError::IoError)?;
        let tmp_path = tmp.path().to_path_buf();

        // Tar creation is synchronous and CPU-bound — run it on a blocking thread.
        let ctx = context_path.clone();
        let out_path = tmp_path.clone();
        tokio::task::spawn_blocking(move || {
            let file = std::fs::File::create(&out_path).map_err(BuilderError::IoError)?;
            let mut tar_builder = tar::Builder::new(file);
            tar_builder
                .append_dir_all(".", ctx)
                .map_err(BuilderError::IoError)?;
            tar_builder.finish().map_err(BuilderError::IoError)?;
            Ok::<(), BuilderError>(())
        })
        .await
        .map_err(|e| BuilderError::Other(format!("Tar task panicked: {}", e)))??;

        // Read the completed tar file back into memory for Bollard.
        let tar_data = tokio::fs::read(&tmp_path)
            .await
            .map_err(BuilderError::IoError)?;

        Ok(Full::new(Bytes::from(tar_data)))
    }

    fn get_resource_limits() -> (usize, u64) {
        let cpu_num = num_cpus::get();
        let mut sys = System::new_all();
        sys.refresh_all();
        let total_memory_gb = sys.total_memory() / 1024 / 1024; // Convert KB to GB

        // Use half of CPUs with minimum of 2
        let cpu_limit = std::cmp::max(2, cpu_num / 2);
        // Use half of memory with minimum of 2GB
        let memory_limit = std::cmp::max(2, total_memory_gb / 2);

        (cpu_limit, memory_limit)
    }

    /// Resolve the per-build `(memory_bytes, cpu_quota_us, cpu_period_us)`
    /// triplet that gets forwarded to `BuildImageOptions`.
    ///
    /// Priority:
    /// 1. Explicit override set via [`Self::with_build_limits`] (control
    ///    plane reads `AppSettings.build_limits`).
    /// 2. Legacy 50%-of-host heuristic in `get_resource_limits` — kept so
    ///    worker nodes, tests, and fresh installs that haven't visited
    ///    the settings page see the same behaviour as before.
    ///
    /// Always uses `cpu_period_us = 100_000` (100 ms) — Docker's standard
    /// period. CPU quota for N cores is `N * cpu_period_us`.
    fn resolve_build_resource_caps(&self) -> (i64, i32, i32) {
        const CPU_PERIOD_US: i32 = 100_000;
        if let Some(override_caps) = self.build_resource_override {
            let memory_bytes = (override_caps.memory_mb as i64) * 1024 * 1024;
            let cpu_quota_us = (override_caps.cpu_cores * CPU_PERIOD_US as f32).round() as i32;
            return (memory_bytes, cpu_quota_us, CPU_PERIOD_US);
        }
        let (cpu_cores, memory_gb) = Self::get_resource_limits();
        let memory_bytes = (memory_gb as i64) * 1024 * 1024 * 1024;
        let cpu_quota_us = (cpu_cores as i32) * CPU_PERIOD_US;
        (memory_bytes, cpu_quota_us, CPU_PERIOD_US)
    }

    /// Detect the native platform for Docker builds
    /// Returns the platform string in the format "linux/arch"
    fn detect_native_platform() -> String {
        #[cfg(target_arch = "x86_64")]
        {
            "linux/amd64".to_string()
        }
        #[cfg(target_arch = "aarch64")]
        {
            "linux/arm64".to_string()
        }
        #[cfg(not(any(target_arch = "x86_64", target_arch = "aarch64")))]
        {
            // Fallback to amd64 for other architectures
            "linux/amd64".to_string()
        }
    }

    async fn concat_byte_stream<S>(s: S) -> Result<Vec<u8>, bollard::errors::Error>
    where
        S: Stream<Item = Result<bytes::Bytes, bollard::errors::Error>>,
    {
        s.try_fold(Vec::new(), |mut acc, chunk| async move {
            acc.extend_from_slice(&chunk[..]);
            Ok(acc)
        })
        .await
    }

    fn map_container_status(status: &str) -> ContainerStatus {
        match status {
            "created" => ContainerStatus::Created,
            "running" => ContainerStatus::Running,
            "paused" => ContainerStatus::Paused,
            "restarting" => ContainerStatus::Running,
            "removing" => ContainerStatus::Stopped,
            "exited" => ContainerStatus::Exited,
            "dead" => ContainerStatus::Dead,
            _ => ContainerStatus::Stopped,
        }
    }

    fn map_restart_policy(policy: &crate::RestartPolicy) -> bollard::models::RestartPolicyNameEnum {
        match policy {
            crate::RestartPolicy::Never => bollard::models::RestartPolicyNameEnum::NO,
            crate::RestartPolicy::Always => bollard::models::RestartPolicyNameEnum::ALWAYS,
            crate::RestartPolicy::OnFailure => bollard::models::RestartPolicyNameEnum::ON_FAILURE,
            crate::RestartPolicy::UnlessStopped => {
                bollard::models::RestartPolicyNameEnum::UNLESS_STOPPED
            }
        }
    }

    /// Find a container by its name
    /// Returns the container ID if found, or None if not found
    async fn find_container_by_name(
        &self,
        container_name: &str,
    ) -> Result<Option<String>, DeployerError> {
        use std::collections::HashMap;

        let mut filters = HashMap::new();
        filters.insert("name".to_string(), vec![container_name.to_string()]);

        let options = Some(ListContainersOptions {
            all: true, // Include stopped containers
            filters: Some(filters),
            ..Default::default()
        });

        let containers = self
            .docker
            .list_containers(options)
            .await
            .map_err(|e| DeployerError::Other(format!("Failed to list containers: {}", e)))?;

        // Docker prefixes container names with "/", so we need to match both formats
        for container in containers {
            if let Some(ref names) = container.names {
                for name in names {
                    // Remove the leading "/" that Docker adds
                    let clean_name = name.trim_start_matches('/');
                    if clean_name == container_name {
                        return Ok(container.id.clone());
                    }
                }
            }
        }

        Ok(None)
    }
}

#[async_trait]
impl ImageBuilder for DockerRuntime {
    async fn build_image(&self, request: BuildRequest) -> Result<BuildResult, BuilderError> {
        // BuildKit is automatically detected and enabled if supported by Docker daemon
        // The standard Docker build API will use BuildKit when available (Docker 18.09+)
        info!(
            "Building image {} (BuildKit: {})",
            request.image_name,
            if self.use_buildkit {
                "enabled"
            } else {
                "disabled"
            }
        );

        // Gate on the build concurrency semaphore when one is configured.
        // The permit is held for the entire build duration — dropped at end
        // of function via `_build_permit` so the next queued build can
        // start as soon as this one finishes (success OR failure). When no
        // semaphore is set (worker nodes, tests), builds run unbounded as
        // before.
        let _build_permit = if let Some(sem) = self.build_semaphore.as_ref() {
            let available = sem.available_permits();
            if available == 0 {
                info!(
                    "Build for {} queued: all build slots in use, waiting for a slot",
                    request.image_name
                );
            }
            Some(
                sem.clone()
                    .acquire_owned()
                    .await
                    .map_err(|e| BuilderError::Other(format!("Build semaphore closed: {}", e)))?,
            )
        } else {
            None
        };

        let start_time = Instant::now();

        self.ensure_network_exists()
            .await
            .map_err(|e| BuilderError::Other(format!("Network setup failed: {}", e)))?;

        info!(
            "Building image {} from context: {:?}",
            request.image_name, request.context_path
        );

        // Create tar archive body from build context
        let tar_body = self
            .create_tar_context_body(request.context_path.clone())
            .await?;

        // Prepare build options using Bollard
        let mut build_args = HashMap::new();
        for (key, value) in request.build_args.iter().filter(|(_, v)| !v.is_empty()) {
            build_args.insert(key.to_string(), value.to_string());
        }

        // Resolve effective build caps from settings (or fall back to the
        // legacy 50%-of-host heuristic when no override is set). Note that
        // Bollard's `BuildImageOptions.memory` field is `Option<i32>` so
        // any limit above i32::MAX (≈ 2 GiB) gets silently clamped here —
        // matches the historical behaviour (the `& 0x7FFFFFFF` mask) and
        // is a known upstream Bollard limitation.
        let (memory_bytes, cpu_quota_us, cpu_period_us) = self.resolve_build_resource_caps();
        let memory_i32 = memory_bytes.min(i32::MAX as i64) as i32;

        let mut labels = HashMap::new();
        labels.insert("built-by".to_string(), "temps".to_string());
        let mut build_args = Some(build_args.clone());
        if self.use_buildkit && !request.build_args_buildkit.is_empty() {
            build_args = Some(request.build_args_buildkit.clone());
        }
        let build_options = bollard::query_parameters::BuildImageOptions {
            dockerfile: request
                .dockerfile_path
                .as_ref()
                .and_then(|p| p.strip_prefix(&request.context_path).ok())
                .map(|p| p.to_string_lossy().to_string())
                .unwrap_or_else(|| "Dockerfile".to_string()),
            t: Some(request.image_name.clone()),
            buildargs: build_args,
            labels: Some(labels),
            networkmode: if self.use_buildkit {
                // BuildKit only supports "default", "host", or "none"
                Some("host".to_string())
            } else {
                // Legacy builder supports custom networks
                Some(self.network_name.clone())
            },
            platform: request
                .platform
                .unwrap_or_else(Self::detect_native_platform),
            memory: Some(memory_i32),
            cpuquota: Some(cpu_quota_us),
            cpuperiod: Some(cpu_period_us),
            version: if self.use_buildkit {
                BuilderVersion::BuilderBuildKit
            } else {
                BuilderVersion::BuilderV1
            },
            session: if self.use_buildkit {
                // Generate unique session ID for BuildKit to avoid conflicts
                Some(uuid::Uuid::new_v4().to_string())
            } else {
                None
            },
            ..Default::default()
        };

        // Open log file for streaming
        let mut log_file = tokio::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&request.log_path)
            .await
            .map_err(BuilderError::IoError)?;

        let mut build_stream = self.docker.build_image(
            build_options,
            None,
            Some(http_body_util::Either::Left(tar_body)),
        );

        // Stream build output and write to log
        while let Some(build_info) = build_stream.next().await {
            match build_info {
                Ok(info) => {
                    if let Some(stream) = info.stream {
                        let _ = log_file.write_all(stream.as_bytes()).await;
                        debug!("Build: {}", stream.trim());
                    }
                    if let Some(error_detail) = info.error_detail {
                        let error = error_detail
                            .message
                            .unwrap_or_else(|| "Unknown build error".to_string());
                        error!("Build error: {}", error);
                        let _ = log_file
                            .write_all(format!("ERROR: {}\n", error).as_bytes())
                            .await;
                        return Err(BuilderError::BuildFailed(error));
                    }
                }
                Err(e) => {
                    let error_msg = format!("Build failed: {}", e);
                    error!("{}", error_msg);
                    let _ = log_file
                        .write_all(format!("ERROR: {}\n", error_msg).as_bytes())
                        .await;
                    return Err(BuilderError::BuildFailed(error_msg));
                }
            }
        }

        let _ = log_file.flush().await;

        let build_duration = start_time.elapsed().as_millis() as u64;

        // Get image info for size
        let images = self
            .docker
            .list_images(Some(bollard::query_parameters::ListImagesOptions {
                filters: {
                    let mut filters = HashMap::new();
                    filters.insert("reference".to_string(), vec![request.image_name.clone()]);
                    Some(filters)
                },
                ..Default::default()
            }))
            .await
            .map_err(|e| BuilderError::Other(format!("Failed to get image info: {}", e)))?;

        let image = images
            .first()
            .ok_or_else(|| BuilderError::Other("Built image not found".to_string()))?;

        Ok(BuildResult {
            image_id: image.id.clone(),
            image_name: request.image_name,
            size_bytes: image.size as u64,
            build_duration_ms: build_duration,
        })
    }

    async fn build_image_with_callback(
        &self,
        request_with_callback: crate::BuildRequestWithCallback,
    ) -> Result<BuildResult, BuilderError> {
        let request = request_with_callback.request;
        let log_callback = request_with_callback.log_callback;

        // BuildKit is automatically detected and enabled if supported by Docker daemon
        // The standard Docker build API will use BuildKit when available (Docker 18.09+)
        info!(
            "Building image {} with callback (BuildKit: {})",
            request.image_name,
            if self.use_buildkit {
                "enabled"
            } else {
                "disabled"
            }
        );

        // Gate on the build concurrency semaphore (see build_image above for
        // the rationale). This is the path the workflow executor actually
        // uses, so the semaphore would be ineffective without this branch.
        let _build_permit = if let Some(sem) = self.build_semaphore.as_ref() {
            if sem.available_permits() == 0 {
                info!(
                    "Build for {} queued: all build slots in use, waiting for a slot",
                    request.image_name
                );
                if let Some(ref cb) = log_callback {
                    cb("[BUILD QUEUED] Waiting for an available build slot...".to_string()).await;
                }
            }
            Some(
                sem.clone()
                    .acquire_owned()
                    .await
                    .map_err(|e| BuilderError::Other(format!("Build semaphore closed: {}", e)))?,
            )
        } else {
            None
        };

        let start_time = Instant::now();

        self.ensure_network_exists()
            .await
            .map_err(|e| BuilderError::Other(format!("Network setup failed: {}", e)))?;

        info!(
            "Building image {} from context: {:?} with log callback",
            request.image_name, request.context_path
        );

        // Create tar archive body from build context
        let tar_body = self
            .create_tar_context_body(request.context_path.clone())
            .await?;

        // Prepare build options using Bollard
        let mut build_args = HashMap::new();
        for (key, value) in request.build_args.iter().filter(|(_, v)| !v.is_empty()) {
            build_args.insert(key.to_string(), value.to_string());
        }

        let (memory_bytes, cpu_quota_us, cpu_period_us) = self.resolve_build_resource_caps();
        let memory_i32 = memory_bytes.min(i32::MAX as i64) as i32;

        let mut labels = HashMap::new();
        labels.insert("built-by".to_string(), "temps".to_string());

        let build_options = bollard::query_parameters::BuildImageOptions {
            dockerfile: request
                .dockerfile_path
                .as_ref()
                .and_then(|p| p.strip_prefix(&request.context_path).ok())
                .map(|p| p.to_string_lossy().to_string())
                .unwrap_or_else(|| "Dockerfile".to_string()),
            t: Some(request.image_name.clone()),
            buildargs: Some(build_args),
            labels: Some(labels),
            networkmode: if self.use_buildkit {
                // BuildKit only supports "default", "host", or "none"
                Some("default".to_string())
            } else {
                // Legacy builder supports custom networks
                Some(self.network_name.clone())
            },
            platform: request
                .platform
                .unwrap_or_else(Self::detect_native_platform),
            memory: Some(memory_i32),
            cpuquota: Some(cpu_quota_us),
            cpuperiod: Some(cpu_period_us),
            version: if self.use_buildkit {
                BuilderVersion::BuilderBuildKit
            } else {
                BuilderVersion::BuilderV1
            },
            session: if self.use_buildkit {
                // Generate unique session ID for BuildKit to avoid conflicts
                Some(uuid::Uuid::new_v4().to_string())
            } else {
                None
            },
            ..Default::default()
        };

        // Open log file for streaming
        let mut log_file = tokio::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&request.log_path)
            .await
            .map_err(BuilderError::IoError)?;

        // Execute build using Bollard
        let mut build_stream = self.docker.build_image(
            build_options,
            None,
            Some(http_body_util::Either::Left(tar_body)),
        );

        // Stream build output and write to log and callback
        while let Some(build_info) = build_stream.next().await {
            match build_info {
                Ok(info) => {
                    if let Some(stream) = info.stream {
                        // Write to file
                        let _ = log_file.write_all(stream.as_bytes()).await;
                        debug!("Build: {}", stream.trim());

                        // Call log callback if provided
                        if let Some(ref callback) = log_callback {
                            callback(stream.clone()).await;
                        }
                    }
                    if let Some(error_detail) = info.error_detail {
                        let error = error_detail
                            .message
                            .unwrap_or_else(|| "Unknown build error".to_string());
                        error!("Build error: {}", error);
                        let error_line = format!("ERROR: {}\n", error);
                        let _ = log_file.write_all(error_line.as_bytes()).await;

                        // Call log callback with error
                        if let Some(ref callback) = log_callback {
                            callback(error_line.clone()).await;
                        }

                        return Err(BuilderError::BuildFailed(error));
                    }
                    if let Some(bollard::models::BuildInfoAux::BuildKit(res)) = info.aux {
                        // Emit vertex names (build step descriptions) when they
                        // start or complete.  This gives visibility into cached
                        // layers and overall build progress even when there is
                        // no command output (logs).
                        for vertex in &res.vertexes {
                            if vertex.name.is_empty() {
                                continue;
                            }
                            let status = if vertex.error.is_empty() {
                                if vertex.completed.is_some() {
                                    if vertex.cached {
                                        "CACHED"
                                    } else {
                                        "DONE"
                                    }
                                } else if vertex.started.is_some() {
                                    "RUNNING"
                                } else {
                                    continue; // Not started yet, skip
                                }
                            } else {
                                "ERROR"
                            };
                            let line = format!("[{}] {}\n", status, vertex.name);
                            let _ = log_file.write_all(line.as_bytes()).await;
                            debug!("BuildKit vertex: {}", line.trim());

                            if let Some(ref callback) = log_callback {
                                callback(line).await;
                            }
                        }

                        // Emit actual command output from build steps
                        for log in res.logs {
                            let _ = log_file.write_all(&log.msg[..]).await;
                            debug!("BuildKit: {}", String::from_utf8_lossy(&log.msg));

                            if let Some(ref callback) = log_callback {
                                callback(String::from_utf8_lossy(&log.msg[..]).to_string()).await;
                            }
                        }
                    }
                }
                Err(e) => {
                    let error_msg = format!("Build failed: {}", e);
                    error!("{}", error_msg);
                    let error_line = format!("ERROR: {}\n", error_msg);
                    let _ = log_file.write_all(error_line.as_bytes()).await;

                    // Call log callback with error
                    if let Some(ref callback) = log_callback {
                        callback(error_line).await;
                    }

                    return Err(BuilderError::BuildFailed(error_msg));
                }
            }
        }

        let _ = log_file.flush().await;

        let build_duration = start_time.elapsed().as_millis() as u64;

        // Get image info
        let images = self
            .docker
            .list_images(Some(bollard::query_parameters::ListImagesOptions {
                filters: {
                    let mut filters = HashMap::new();
                    filters.insert("reference".to_string(), vec![request.image_name.clone()]);
                    Some(filters)
                },
                ..Default::default()
            }))
            .await
            .map_err(|e| BuilderError::Other(format!("Failed to get image info: {}", e)))?;

        let image = images
            .first()
            .ok_or_else(|| BuilderError::Other("Built image not found".to_string()))?;

        Ok(BuildResult {
            image_id: image.id.clone(),
            image_name: request.image_name,
            size_bytes: image.size as u64,
            build_duration_ms: build_duration,
        })
    }

    async fn import_image(&self, image_path: PathBuf, tag: &str) -> Result<String, BuilderError> {
        info!("Importing image from {:?} with tag: {}", image_path, tag);

        let file = tokio::fs::File::open(&image_path)
            .await
            .map_err(BuilderError::IoError)?;

        let byte_stream =
            tokio_util::codec::FramedRead::new(file, tokio_util::codec::BytesCodec::new())
                .map(|r| r.map(|b| b.freeze()));

        let import_stream = self.docker.import_image_stream(
            bollard::query_parameters::ImportImageOptions {
                quiet: false,
                ..Default::default()
            },
            byte_stream,
            None,
        );

        let mut image_id = None;
        let mut stream = std::pin::Pin::new(Box::new(import_stream));

        while let Some(result) = futures::StreamExt::next(&mut stream).await {
            match result {
                Ok(info) => {
                    if let Some(stream_msg) = info.stream {
                        info!("Import progress: {}", stream_msg.trim());
                        if stream_msg.contains("Loaded image:") {
                            image_id = stream_msg
                                .split("Loaded image: ")
                                .nth(1)
                                .map(|s| s.trim().to_string());
                        }
                    }
                }
                Err(e) => {
                    return Err(BuilderError::Other(format!(
                        "Failed to import image: {}",
                        e
                    )));
                }
            }
        }

        let id = image_id.ok_or_else(|| BuilderError::Other("No image ID found".to_string()))?;

        // Tag the image
        self.docker
            .tag_image(
                &id,
                Some(TagImageOptions {
                    repo: Some(tag.split(':').next().unwrap_or(tag).to_string()),
                    tag: Some(tag.split(':').nth(1).unwrap_or("latest").to_string()),
                }),
            )
            .await
            .map_err(|e| BuilderError::Other(format!("Failed to tag image: {}", e)))?;

        Ok(id)
    }

    async fn save_image(&self, image_name: &str, output_path: &Path) -> Result<(), BuilderError> {
        info!("Exporting image '{}' to {:?}", image_name, output_path);

        let stream = self.docker.export_image(image_name);

        let mut file = tokio::fs::File::create(output_path).await.map_err(|e| {
            BuilderError::IoError(std::io::Error::new(
                e.kind(),
                format!("Failed to create tar file {:?}: {}", output_path, e),
            ))
        })?;

        let mut stream = std::pin::Pin::new(Box::new(stream));

        while let Some(chunk) = StreamExt::next(&mut stream).await {
            let bytes = chunk.map_err(|e| {
                BuilderError::Other(format!("Failed to export image '{}': {}", image_name, e))
            })?;
            file.write_all(&bytes).await.map_err(|e| {
                BuilderError::IoError(std::io::Error::new(
                    e.kind(),
                    format!("Failed to write image tar: {}", e),
                ))
            })?;
        }

        file.flush().await.map_err(BuilderError::IoError)?;

        let metadata = tokio::fs::metadata(output_path)
            .await
            .map_err(BuilderError::IoError)?;
        info!(
            "Exported image '{}' ({:.1} MB)",
            image_name,
            metadata.len() as f64 / 1_048_576.0
        );

        Ok(())
    }

    async fn extract_from_image(
        &self,
        image_name: &str,
        source_path: &str,
        destination_path: &Path,
    ) -> Result<(), BuilderError> {
        // Skip pull for local images (temps-* are built locally, not from a registry)
        if !image_name.starts_with("temps-") {
            let _ = self
                .docker
                .create_image(
                    Some(bollard::query_parameters::CreateImageOptions {
                        from_image: Some(image_name.to_string()),
                        ..Default::default()
                    }),
                    None,
                    None,
                )
                .for_each(|_| async {})
                .await;
        }

        // Create container
        let container_config = bollard::models::ContainerCreateBody {
            image: Some(image_name.to_string()),
            cmd: Some(vec!["/bin/sh".to_string()]),
            tty: Some(true),
            ..Default::default()
        };

        let container = self
            .docker
            .create_container(
                Some(bollard::query_parameters::CreateContainerOptionsBuilder::new().build()),
                container_config,
            )
            .await
            .map_err(|e| BuilderError::Other(format!("Failed to create container: {}", e)))?;

        let container_id = container.id.clone();

        // Cleanup function
        let cleanup = || async {
            let _ = self
                .docker
                .remove_container(
                    &container_id,
                    Some(RemoveContainerOptions {
                        force: true,
                        ..Default::default()
                    }),
                )
                .await;
        };

        // Download from container
        let temp_dir = TempDir::new().map_err(BuilderError::IoError)?;
        let temp_path = temp_dir.path();

        let response_stream = self.docker.download_from_container(
            &container_id,
            Some(bollard::query_parameters::DownloadFromContainerOptions {
                path: source_path.to_string(),
            }),
        );

        let bytes = match Self::concat_byte_stream(response_stream).await {
            Ok(bytes) => bytes,
            Err(e) => {
                cleanup().await;
                return Err(BuilderError::Other(format!(
                    "Failed to download from container: {}",
                    e
                )));
            }
        };

        let mut archive_reader = tar::Archive::new(&bytes[..]);
        if let Err(e) = archive_reader.unpack(temp_path) {
            cleanup().await;
            return Err(BuilderError::Other(format!(
                "Failed to extract archive: {}",
                e
            )));
        }

        let last_path_component = std::path::Path::new(source_path)
            .file_name()
            .and_then(|os_str| os_str.to_str())
            .unwrap_or("");

        let extracted_dir = temp_path.join(last_path_component);

        if let Err(e) = std::fs::rename(&extracted_dir, destination_path) {
            cleanup().await;
            return Err(BuilderError::Other(format!(
                "Failed to move extracted files: {}",
                e
            )));
        }

        cleanup().await;
        Ok(())
    }

    async fn list_images(&self) -> Result<Vec<String>, BuilderError> {
        let images = self
            .docker
            .list_images(Some(bollard::query_parameters::ListImagesOptions {
                all: true,
                ..Default::default()
            }))
            .await
            .map_err(|e| BuilderError::Other(format!("Failed to list images: {}", e)))?;

        let mut all_tags = Vec::new();
        for img in images {
            // repo_tags is Vec<String> in this version of bollard
            all_tags.extend(img.repo_tags);
        }
        Ok(all_tags)
    }

    async fn remove_image(&self, image_name: &str) -> Result<(), BuilderError> {
        // Remove image - ignore any errors for now since it returns a stream
        let _stream = self.docker.remove_image(
            image_name,
            Some(bollard::query_parameters::RemoveImageOptions {
                force: true,
                ..Default::default()
            }),
            None,
        );

        Ok(())
    }

    async fn inspect_image(&self, image_name: &str) -> Result<crate::ImageInfo, BuilderError> {
        let inspect = self.docker.inspect_image(image_name).await.map_err(|e| {
            BuilderError::ImageNotFound(format!("Failed to inspect image '{}': {}", image_name, e))
        })?;

        let architecture = inspect
            .architecture
            .unwrap_or_else(|| "unknown".to_string());
        let os = inspect.os.unwrap_or_else(|| "linux".to_string());
        let platform = format!("{}/{}", os, architecture);

        let size_bytes = inspect.size.map(|s| s as u64).unwrap_or(0);

        // Get tags from repo_tags
        let tags = inspect.repo_tags.unwrap_or_default();

        let created = inspect.created.and_then(|dt| {
            chrono::DateTime::from_timestamp(dt.unix_timestamp(), dt.nanosecond())
                .map(|c| c.to_rfc3339())
        });

        // Extract WORKDIR from the image config
        let working_dir = inspect
            .config
            .as_ref()
            .and_then(|c| c.working_dir.clone())
            .filter(|w| !w.is_empty());

        Ok(crate::ImageInfo {
            id: inspect.id.unwrap_or_default(),
            architecture,
            os,
            platform,
            size_bytes,
            tags,
            created,
            working_dir,
        })
    }

    fn get_native_platform(&self) -> String {
        Self::detect_native_platform()
    }
}

#[async_trait]
impl ContainerDeployer for DockerRuntime {
    async fn deploy_container(
        &self,
        request: DeployRequest,
    ) -> Result<DeployResult, DeployerError> {
        info!(
            "Deploying container {} from image {}",
            request.container_name, request.image_name
        );

        self.ensure_network_exists().await?;

        // Check if a container with this name already exists and remove it
        match self.find_container_by_name(&request.container_name).await {
            Ok(Some(existing_container_id)) => {
                info!(
                    "🔄 Container {} already exists ({}), removing it before redeployment",
                    request.container_name, existing_container_id
                );

                // Stop the container if it's running
                if let Err(e) = self.stop_container(&existing_container_id).await {
                    warn!(
                        "⚠️  Failed to stop existing container {}: {}",
                        existing_container_id, e
                    );
                }

                // Remove the container
                if let Err(e) = self.remove_container(&existing_container_id).await {
                    return Err(DeployerError::DeploymentFailed(format!(
                        "Failed to remove existing container {}: {}",
                        existing_container_id, e
                    )));
                }

                info!("✅ Removed existing container {}", existing_container_id);
            }
            Ok(None) => {
                debug!("No existing container with name {}", request.container_name);
            }
            Err(e) => {
                warn!("⚠️  Error checking for existing container: {}", e);
            }
        }

        // Create port bindings
        let mut port_bindings = HashMap::new();
        let mut exposed_ports = Vec::new();

        for port_mapping in &request.port_mappings {
            let container_port_key =
                format!("{}/{}", port_mapping.container_port, port_mapping.protocol);
            let host_port_binding = bollard::models::PortBinding {
                host_ip: Some(self.host_bind_address.clone()),
                // When host_port is 0, let Docker pick an available port
                host_port: if port_mapping.host_port == 0 {
                    None
                } else {
                    Some(port_mapping.host_port.to_string())
                },
            };

            port_bindings.insert(container_port_key.clone(), Some(vec![host_port_binding]));
            exposed_ports.push(container_port_key);
        }

        // Create host config with log rotation to prevent unbounded disk growth
        let log_config = request
            .log_config
            .as_ref()
            .map(|lc| lc.to_bollard_log_config());

        // When secrets are present, materialize them as files in a per-container
        // host directory (mode 0700) and bind-mount that directory into the
        // container at /run/secrets (read-only). This matches how Docker Swarm
        // delivers secrets: directory is created by the mount itself, so it
        // works with any image (no requirement that /run/secrets pre-exist),
        // and files are visible from container start (no race with start).
        //
        // Trade-off vs tmpfs: plaintext lives on the host filesystem under
        // `secrets_root` ($TEMPS_DATA_DIR/secrets by default) until the
        // container is removed. We deliberately use a persistent path
        // rather than `std::env::temp_dir()` so the bind mount survives a
        // host reboot — `/tmp` is tmpfs on most Linux distros, and the
        // mount would otherwise point at an empty directory after a
        // restart, forcing a redeploy. The directory is mode 0700
        // root-owned; individual files are mode 0400. Cleanup is handled
        // in `remove_container`.
        let secrets_bind = if request.secrets.is_empty() {
            None
        } else {
            let host_dir = self.secrets_host_dir(&request.container_name);
            // Resolve the image's USER so we can chown the secret files to
            // the uid that the container will actually run as — otherwise
            // mode-0400 root-owned files are unreadable by nonroot images.
            let owner = self.resolve_image_user(&request.image_name).await;
            write_secrets_to_host_dir(&host_dir, &request.secrets, Some(owner)).map_err(|e| {
                DeployerError::SecretMountFailed {
                    container_name: request.container_name.clone(),
                    reason: format!("write host dir {}: {}", host_dir.display(), e),
                }
            })?;
            // Docker bind-mount syntax: "<host_path>:<container_path>:<options>"
            Some(format!("{}:/run/secrets:ro", host_dir.display()))
        };

        let dns_for_container = self.dns_for_container();

        let host_config = bollard::models::HostConfig {
            port_bindings: Some(port_bindings),
            network_mode: Some(self.network_name.clone()),
            dns: dns_for_container,
            restart_policy: Some(bollard::models::RestartPolicy {
                name: Some(Self::map_restart_policy(&request.restart_policy)),
                ..Default::default()
            }),
            // A limit of 0 is the explicit "uncapped" sentinel → leave Docker's
            // memory cap unset (None) so the container runs unlimited.
            memory: request
                .resource_limits
                .memory_limit_mb
                .filter(|&mb| mb > 0)
                .map(|mb| mb as i64 * 1024 * 1024),
            // Cap swap at the memory limit so the advertised hard cap is real.
            // Docker lets a container use swap up to its memory limit when
            // memory_swap is left unset, which would let a "512 MB" app reach
            // ~1 GiB of memory+swap.
            // Setting memory_swap == memory disables swap for the container.
            memory_swap: request
                .resource_limits
                .memory_limit_mb
                .filter(|&mb| mb > 0)
                .map(|mb| mb as i64 * 1024 * 1024),
            nano_cpus: request
                .resource_limits
                .cpu_limit
                .map(|cores| (cores * 1_000_000_000.0) as i64),
            log_config,
            // Security hardening: drop all Linux capabilities by default
            cap_drop: Some(vec!["ALL".to_string()]),
            // Security hardening: prevent privilege escalation via setuid/setgid
            security_opt: Some(vec!["no-new-privileges:true".to_string()]),
            // Security hardening: limit number of processes to prevent fork bombs
            pids_limit: Some(512),
            // Security hardening: use init process for proper signal handling and zombie reaping
            init: Some(true),
            binds: secrets_bind.map(|b| vec![b]),
            ..Default::default()
        };

        // Build container labels (used by log aggregator for container discovery)
        let container_labels = if request.labels.is_empty() {
            None
        } else {
            Some(request.labels.clone())
        };

        // Create container config
        let container_config = bollard::models::ContainerCreateBody {
            image: Some(request.image_name.clone()),
            env: Some(
                request
                    .environment_vars
                    .iter()
                    .map(|(k, v)| format!("{}={}", k, v))
                    .collect(),
            ),
            exposed_ports: Some(exposed_ports),
            host_config: Some(host_config),
            cmd: request.command.clone(),
            labels: container_labels,
            ..Default::default()
        };

        // Create container
        let container = self
            .docker
            .create_container(
                Some(
                    bollard::query_parameters::CreateContainerOptionsBuilder::new()
                        .name(&request.container_name)
                        .build(),
                ),
                container_config,
            )
            .await
            .map_err(|e| {
                DeployerError::DeploymentFailed(format!("Failed to create container: {}", e))
            })?;

        // Multi-host overlay: best-effort additional attach to `temps-overlay`
        // (or whatever the operator configured). Containers always boot with
        // their primary network interface (`temps-app-network`); the overlay
        // attachment is purely additive and silently no-ops when the overlay
        // network isn't present yet on this node.
        self.maybe_attach_overlay(&container.id).await?;

        // Start container
        self.docker
            .start_container(&container.id, None::<StartContainerOptions>)
            .await
            .map_err(|e| {
                DeployerError::DeploymentFailed(format!("Failed to start container: {}", e))
            })?;

        // Install overlay peer routes inside the container's netns.
        // Must run *after* start_container — `docker inspect` only
        // reports a non-zero PID for a running container, and route
        // injection uses `nsenter -t <pid> -n ip route ...`.
        self.install_overlay_peer_routes(&container.id).await;

        // Get the first port mapping for the result
        let (container_port, requested_host_port) = request
            .port_mappings
            .first()
            .map(|pm| (pm.container_port, pm.host_port))
            .unwrap_or((0, 0));

        // When host_port was 0 (Docker picks), inspect the container to get the actual port
        let host_port = if requested_host_port == 0 && container_port > 0 {
            let inspect = self
                .docker
                .inspect_container(&container.id, None::<InspectContainerOptions>)
                .await
                .map_err(|e| {
                    DeployerError::DeploymentFailed(format!(
                        "Failed to inspect container {} for port mapping: {}",
                        container.id, e
                    ))
                })?;

            let port_key = format!("{}/tcp", container_port);
            inspect
                .network_settings
                .and_then(|ns| ns.ports)
                .and_then(|ports| ports.get(&port_key).cloned())
                .flatten()
                .and_then(|bindings| bindings.first().cloned())
                .and_then(|binding| binding.host_port)
                .and_then(|p| p.parse::<u16>().ok())
                .ok_or_else(|| {
                    DeployerError::DeploymentFailed(format!(
                        "Container {} has no host port binding for {}",
                        container.id, port_key
                    ))
                })?
        } else {
            requested_host_port
        };

        Ok(DeployResult {
            container_id: container.id,
            container_name: request.container_name,
            container_port,
            host_port,
            status: ContainerStatus::Running,
        })
    }

    async fn start_container(&self, container_id: &str) -> Result<(), DeployerError> {
        self.docker
            .start_container(container_id, None::<StartContainerOptions>)
            .await
            .map_err(|e| DeployerError::Other(format!("Failed to start container: {}", e)))?;
        Ok(())
    }

    async fn stop_container(&self, container_id: &str) -> Result<(), DeployerError> {
        self.docker
            .stop_container(
                container_id,
                Some(StopContainerOptions {
                    t: Some(10),
                    signal: None,
                }),
            )
            .await
            .map_err(|e| {
                DeployerError::Other(format!("Failed to stop container {}: {}", container_id, e))
            })?;
        Ok(())
    }

    async fn pause_container(&self, container_id: &str) -> Result<(), DeployerError> {
        self.docker
            .pause_container(container_id)
            .await
            .map_err(|e| DeployerError::Other(format!("Failed to pause container: {}", e)))?;
        Ok(())
    }

    async fn resume_container(&self, container_id: &str) -> Result<(), DeployerError> {
        self.docker
            .unpause_container(container_id)
            .await
            .map_err(|e| DeployerError::Other(format!("Failed to resume container: {}", e)))?;
        Ok(())
    }

    async fn remove_container(&self, container_id: &str) -> Result<(), DeployerError> {
        // Look up the container name before removal so we can clean up its
        // per-container secrets host directory (if any). Inspect failures are
        // non-fatal: we still try to remove the container.
        let container_name = self
            .docker
            .inspect_container(container_id, None::<InspectContainerOptions>)
            .await
            .ok()
            .and_then(|c| c.name)
            // Docker prefixes inspect names with a leading '/'.
            .map(|n| n.trim_start_matches('/').to_string());

        self.docker
            .remove_container(
                container_id,
                Some(RemoveContainerOptions {
                    force: true,
                    ..Default::default()
                }),
            )
            .await
            .map_err(|e| DeployerError::Other(format!("Failed to remove container: {}", e)))?;

        if let Some(name) = container_name {
            let dir = self.secrets_host_dir(&name);
            if dir.exists() {
                if let Err(e) = std::fs::remove_dir_all(&dir) {
                    warn!(
                        "Failed to clean up secrets host dir {}: {}",
                        dir.display(),
                        e
                    );
                }
            }
        }

        Ok(())
    }

    async fn get_container_info(&self, container_id: &str) -> Result<ContainerInfo, DeployerError> {
        let container = self
            .docker
            .inspect_container(container_id, None::<InspectContainerOptions>)
            .await
            .map_err(|e| DeployerError::ContainerNotFound(format!("Container not found: {}", e)))?;

        // Pull host_config out before state/config so we can read the CPU
        // limit (nano_cpus) without re-borrowing the moved container value.
        let host_config = container.host_config.clone();
        let state = container.state.unwrap_or_default();
        let config = container.config.unwrap_or_default();
        let container_labels = config.labels.clone().unwrap_or_default();

        // Parse environment variables
        let env_vars = config
            .env
            .unwrap_or_default()
            .into_iter()
            .filter_map(|env_str| {
                let parts: Vec<&str> = env_str.splitn(2, '=').collect();
                if parts.len() == 2 {
                    Some((parts[0].to_string(), parts[1].to_string()))
                } else {
                    None
                }
            })
            .collect();

        // Parse port mappings
        let port_mappings = container
            .network_settings
            .and_then(|ns| ns.ports)
            .unwrap_or_default()
            .into_iter()
            .filter_map(|(port_key, bindings)| {
                if let Some(bindings) = bindings {
                    if let Some(binding) = bindings.first() {
                        let parts: Vec<&str> = port_key.split('/').collect();
                        if parts.len() == 2 {
                            let container_port = parts[0].parse().ok()?;
                            let protocol = match parts[1] {
                                "tcp" => Protocol::Tcp,
                                "udp" => Protocol::Udp,
                                _ => Protocol::Tcp,
                            };
                            let host_port = binding.host_port.as_ref()?.parse().ok()?;

                            return Some(PortMapping {
                                host_port,
                                container_port,
                                protocol,
                            });
                        }
                    }
                }
                None
            })
            .collect();

        let status =
            Self::map_container_status(&state.status.map(|s| s.to_string()).unwrap_or_default());

        // Capture exit metadata so the API/UI can show *why* a container is
        // gone instead of a bare "Exited". Docker only populates these once
        // the container has actually exited; for running containers they
        // remain None.
        let oom_killed = state.oom_killed;
        let exit_code_i64 = state.exit_code;
        let exit_code: Option<i32> = exit_code_i64.and_then(|c| i32::try_from(c).ok());
        // Trim Docker's empty-string "no error" sentinel; only surface a real message.
        let error_message = state.error.and_then(|e| {
            let trimmed = e.trim();
            if trimmed.is_empty() {
                None
            } else {
                Some(trimmed.to_string())
            }
        });
        // FinishedAt comes back as RFC3339; "0001-01-01T00:00:00Z" means
        // never-finished, which we treat as None.
        let parse_docker_ts = |s: &str| -> Option<chrono::DateTime<chrono::Utc>> {
            if s.is_empty() || s.starts_with("0001-01-01") {
                None
            } else {
                chrono::DateTime::parse_from_rfc3339(s)
                    .ok()
                    .map(|dt| dt.with_timezone(&chrono::Utc))
            }
        };
        let finished_at = state.finished_at.as_deref().and_then(parse_docker_ts);
        let started_at = state.started_at.as_deref().and_then(parse_docker_ts);

        // Translate Docker's nano_cpus to whole-core units so the UI can
        // render "0.5 / 1.0 cores" without doing the divide. None when no
        // limit was set on the container (host_config absent or nano_cpus 0).
        let cpu_limit_cores = host_config
            .as_ref()
            .and_then(|hc| hc.nano_cpus)
            .filter(|nc| *nc > 0)
            .map(|nc| nc as f64 / 1_000_000_000.0);

        let exit_reason =
            build_exit_reason(&status, oom_killed, exit_code_i64, error_message.as_deref());

        Ok(ContainerInfo {
            container_id: container.id.unwrap_or_default(),
            container_name: container
                .name
                .unwrap_or_default()
                .trim_start_matches('/')
                .to_string(),
            image_name: config.image.unwrap_or_default(),
            status,
            created_at: container
                .created
                .and_then(|dt| {
                    chrono::DateTime::from_timestamp(dt.unix_timestamp(), dt.nanosecond())
                })
                .unwrap_or_else(chrono::Utc::now),
            ports: port_mappings,
            environment_vars: env_vars,
            restart_count: container.restart_count,
            labels: container_labels,
            exit_code,
            exit_reason,
            oom_killed,
            error_message,
            finished_at,
            started_at,
            cpu_limit_cores,
        })
    }

    async fn get_container_stats(
        &self,
        container_id: &str,
    ) -> Result<crate::ContainerStats, DeployerError> {
        // Get container info first to get the name and cpu_limit
        let container_info = self.get_container_info(container_id).await?;

        // Sample twice ~1s apart so CPU% has a real delta window — Docker's
        // `one_shot: true` doesn't populate `precpu_stats`, so the
        // single-sample math collapses to (cumulative cpu time / system time
        // since boot) which is ~0% for any long-running container. This is
        // the same pattern `docker stats` uses internally.
        let (first, second) = sample_container_stats_twice(&self.docker, container_id)
            .await
            .map_err(|e| DeployerError::Other(format!("Failed to get container stats: {}", e)))?;

        let cpu_percent = cpu_percent_from_samples(&second, &first).unwrap_or(0.0);

        // Memory: subtract page cache (matches `docker stats` MEM USAGE).
        // cgroup v2 → `inactive_file`, cgroup v1 → `cache`. Both are
        // reclaimable file pages that the kernel counts as `usage` but
        // shouldn't show up as the container's working set.
        let memory_stats = second.memory_stats.as_ref();
        let memory_bytes = memory_stats
            .and_then(memory_usage_excluding_cache)
            .unwrap_or(0);
        let memory_limit_bytes = memory_stats.and_then(|ms| ms.limit);

        let memory_percent = match memory_limit_bytes {
            Some(limit) if limit > 0 => {
                Some(((memory_bytes as f64 / limit as f64) * 100.0).clamp(0.0, 100.0))
            }
            _ => None,
        };

        // Extract network stats from the latest sample.
        let default_networks = Default::default();
        let networks_stats = second.networks.as_ref().unwrap_or(&default_networks);
        let (network_rx_bytes, network_tx_bytes) =
            if let Some(net_stat) = networks_stats.values().next() {
                (
                    net_stat.rx_bytes.unwrap_or(0),
                    net_stat.tx_bytes.unwrap_or(0),
                )
            } else {
                (0, 0)
            };

        Ok(crate::ContainerStats {
            container_id: container_info.container_id,
            container_name: container_info.container_name,
            cpu_percent,
            cpu_limit_cores: container_info.cpu_limit_cores,
            memory_bytes,
            memory_limit_bytes,
            memory_percent,
            network_rx_bytes,
            network_tx_bytes,
            restart_count: container_info.restart_count,
            started_at: container_info.started_at,
            timestamp: chrono::Utc::now(),
        })
    }

    async fn list_containers(&self) -> Result<Vec<ContainerInfo>, DeployerError> {
        let containers = self
            .docker
            .list_containers(Some(ListContainersOptions {
                all: true,
                ..Default::default()
            }))
            .await
            .map_err(|e| DeployerError::Other(format!("Failed to list containers: {}", e)))?;

        let mut container_infos = Vec::new();

        for container in containers {
            if let Some(id) = container.id {
                match self.get_container_info(&id).await {
                    Ok(info) => container_infos.push(info),
                    Err(e) => warn!("Failed to get info for container {}: {}", id, e),
                }
            }
        }

        Ok(container_infos)
    }

    async fn get_container_logs(&self, container_id: &str) -> Result<String, DeployerError> {
        let logs_stream = self
            .docker
            .logs(
                container_id,
                Some(LogsOptions {
                    stdout: true,
                    stderr: true,
                    tail: "10000".to_string(),
                    ..Default::default()
                }),
            )
            .map(|chunk| chunk.map(|c| String::from_utf8_lossy(&c.into_bytes()).to_string()))
            .try_collect::<Vec<_>>()
            .await
            .map_err(|e| DeployerError::Other(format!("Failed to get logs: {}", e)))?;

        Ok(logs_stream.join(""))
    }

    async fn stream_container_logs(
        &self,
        container_id: &str,
    ) -> Result<Box<dyn futures::Stream<Item = String> + Unpin + Send>, DeployerError> {
        let logs_stream = self
            .docker
            .logs(
                container_id,
                Some(LogsOptions {
                    stdout: true,
                    stderr: true,
                    follow: true,
                    ..Default::default()
                }),
            )
            .map(|chunk| match chunk {
                Ok(c) => String::from_utf8_lossy(&c.into_bytes()).to_string(),
                Err(e) => format!("Error reading logs: {}", e),
            });

        Ok(Box::new(Box::pin(logs_stream)))
    }

    async fn image_exists(&self, image_name: &str) -> Result<bool, DeployerError> {
        match self.docker.inspect_image(image_name).await {
            Ok(_) => Ok(true),
            Err(bollard::errors::Error::DockerResponseServerError {
                status_code: 404, ..
            }) => Ok(false),
            Err(e) => Err(DeployerError::Other(format!(
                "Failed to check image '{}': {}",
                image_name, e
            ))),
        }
    }
}

#[async_trait]
impl ContainerRuntime for DockerRuntime {
    async fn get_runtime_info(&self) -> Result<RuntimeInfo, DeployerError> {
        let version =
            self.docker.version().await.map_err(|e| {
                DeployerError::Other(format!("Failed to get Docker version: {}", e))
            })?;

        let mut system = System::new_all();
        system.refresh_all();

        Ok(RuntimeInfo {
            runtime_type: "Docker".to_string(),
            version: version.version.unwrap_or_default(),
            available_cpu_cores: num_cpus::get(),
            available_memory_mb: system.total_memory() / 1024,
            available_disk_mb: 0, // Docker doesn't easily expose this
        })
    }
}

/// Resolve the persistent root directory for materialized secret files.
///
/// Order of preference, mirroring the rest of the codebase
/// (`temps-sandbox`, agent session storage):
///   1. `$TEMPS_DATA_DIR/secrets`
///   2. `$HOME/.temps/secrets`
///   3. `./.temps/secrets` (last-resort fallback for headless container
///      runtimes where neither env var is set)
///
/// Crucially, NONE of these resolve to `std::env::temp_dir()`. On most
/// Linux distros `/tmp` is mounted as tmpfs and wiped at boot, which
/// caused container `/run/secrets` mounts to point at empty/missing
/// directories after a host reboot and forced a redeploy.
fn default_secrets_root() -> PathBuf {
    let base = std::env::var("TEMPS_DATA_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|_| {
            std::env::var("HOME")
                .map(PathBuf::from)
                .unwrap_or_else(|_| PathBuf::from("."))
                .join(".temps")
        });
    base.join("secrets")
}

/// Writes secrets as files into a per-container host directory for Docker
/// to bind-mount into `/run/secrets`. The directory is created (or recreated)
/// fresh on each call so stale entries from a previous deployment of the same
/// container name don't leak through. Directory mode 0700, file mode 0400.
///
/// When `owner` is provided, the directory and every file inside are
/// `chown`ed to that (uid, gid). Combined with 0700/0400 this means only
/// that UID inside the container can read the secrets — matching the
/// image's declared `USER` so distroless/nonroot images work without
/// world-readable files.
///
/// Rejects keys that would escape the directory (path separators, `.`, `..`)
/// to defend against a maliciously-crafted secret name.
fn write_secrets_to_host_dir(
    dir: &Path,
    secrets: &HashMap<String, String>,
    owner: Option<(u32, u32)>,
) -> std::io::Result<()> {
    use std::fs;
    use std::io::Write;

    // Ensure the parent (secrets root) exists. The runtime's `new()` already
    // best-effort creates it, but a hostile chmod or fresh install between
    // restarts could have left it missing — recreate so we don't fail with
    // ENOENT. We deliberately do NOT chmod the parent here: that's the
    // runtime's job, and on shared hosts the parent may be intentionally
    // owned by a different user.
    if let Some(parent) = dir.parent() {
        fs::create_dir_all(parent)?;
    }

    // Recreate fresh — wipes any stale files from a previous container with
    // the same name (rolling deploys, retries after failure, etc.).
    //
    // After a host reboot we may inherit a leaf directory whose contents
    // were chowned to the previous container's USER (e.g. nonroot uid
    // 65532) while the parent is owned by the temps user. `remove_dir_all`
    // on such a tree fails with EACCES because we can't unlink files we
    // don't own inside a directory we *do* own (sticky-bit semantics on
    // some filesystems). Reset the leaf's perms to 0700 owned-by-us first
    // so the recursive remove can succeed.
    if dir.exists() {
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            // Best-effort: if this chmod fails (we don't own the dir), the
            // remove below will still surface the real error.
            let _ = fs::set_permissions(dir, fs::Permissions::from_mode(0o700));
        }
        if let Err(e) = fs::remove_dir_all(dir) {
            // If the leaf itself is owned by another uid (e.g. previous
            // container's USER) and we can't traverse into it, fall back
            // to renaming it aside so the deploy can proceed. The orphan
            // is logged for operator cleanup; it's not in /tmp anymore so
            // it won't auto-vanish on reboot, but it's also not blocking
            // the deploy.
            if e.kind() == std::io::ErrorKind::PermissionDenied {
                let orphan = dir.with_extension(format!(
                    "orphan-{}",
                    std::time::SystemTime::now()
                        .duration_since(std::time::UNIX_EPOCH)
                        .map(|d| d.as_secs())
                        .unwrap_or(0)
                ));
                fs::rename(dir, &orphan).map_err(|rename_err| {
                    std::io::Error::new(
                        rename_err.kind(),
                        format!(
                            "failed to clear stale secrets dir {} \
                             (rename to {} also failed: {}; original error: {})",
                            dir.display(),
                            orphan.display(),
                            rename_err,
                            e
                        ),
                    )
                })?;
                warn!(
                    "Renamed unowned stale secrets dir to {} so deploy could proceed; \
                     please remove manually",
                    orphan.display()
                );
            } else {
                return Err(e);
            }
        }
    }
    fs::create_dir_all(dir)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(dir, fs::Permissions::from_mode(0o700))?;
    }

    for (key, value) in secrets {
        // Keys must be plain identifiers — defense-in-depth even though
        // SecretService::validate_secret_key already enforces this.
        if key.is_empty()
            || key == "."
            || key == ".."
            || key.contains('/')
            || key.contains('\\')
            || key.contains('\0')
        {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                format!(
                    "invalid secret key '{}': contains path-separator characters",
                    key
                ),
            ));
        }
        let path = dir.join(key);
        let mut f = fs::File::create(&path)?;
        f.write_all(value.as_bytes())?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(&path, fs::Permissions::from_mode(0o400))?;
        }
    }

    // Chown after files are written so we don't have to re-chown on every
    // write. Dir chown happens last so we still have permission to create
    // files in it while writing (when running as non-root, chown-to-self
    // is a no-op; when running as root we own it either way).
    //
    // chown(2) requires root on every sensible OS, or chown-to-self. When
    // Temps itself runs unprivileged (local macOS dev is the common case)
    // and the image runs as a non-root uid like 1000, the chown will fail
    // with EPERM. Rather than breaking the deploy, fall back to
    // world-readable permissions (0755 dir, 0444 files) so the container
    // can read its secrets. The container still has `cap_drop: ALL` and
    // `no-new-privileges`, and the bind mount is read-only — the trust
    // boundary is still the container itself.
    #[cfg(unix)]
    if let Some((uid, gid)) = owner {
        use std::os::unix::fs::{chown, PermissionsExt};

        let chown_result: std::io::Result<()> = (|| {
            for key in secrets.keys() {
                chown(dir.join(key), Some(uid), Some(gid))?;
            }
            chown(dir, Some(uid), Some(gid))?;
            Ok(())
        })();

        if let Err(e) = chown_result {
            if e.kind() == std::io::ErrorKind::PermissionDenied {
                warn!(
                    "chown of secrets dir {} to {}:{} denied (Temps is not root); \
                     falling back to world-readable mode so nonroot containers can read. \
                     Run Temps as root for strict per-uid ownership.",
                    dir.display(),
                    uid,
                    gid
                );
                // World-readable fallback. Parent dir already restricts
                // access on the host side (only the Temps user can traverse
                // into the secrets root under $TEMPS_DATA_DIR/secrets);
                // these bits are what the container sees.
                fs::set_permissions(dir, fs::Permissions::from_mode(0o755))?;
                for key in secrets.keys() {
                    fs::set_permissions(dir.join(key), fs::Permissions::from_mode(0o444))?;
                }
            } else {
                return Err(e);
            }
        }
    }

    Ok(())
}

/// Parses a Docker image `USER` spec into a numeric `(uid, gid)` pair.
///
/// Handles the forms Docker accepts in a Dockerfile `USER` directive and
/// in `Config.User`: `""` (root), `"0"`, `"1000"`, `"1000:1000"`,
/// `"1000:gname"`, `":1000"`. Named users/groups cannot be resolved
/// without consulting the image's `/etc/passwd` — those return `None`
/// so the caller can decide whether to look them up or fall back.
///
/// When only a uid is given, gid defaults to the same value (matches
/// Docker's behavior: `USER 1000` runs as uid=1000, gid=1000).
fn parse_numeric_user_spec(spec: &str) -> Option<(u32, u32)> {
    let spec = spec.trim();
    if spec.is_empty() || spec == "root" {
        return Some((0, 0));
    }

    let (user_part, group_part) = match spec.split_once(':') {
        Some((u, g)) => (u, Some(g)),
        None => (spec, None),
    };

    let uid = if user_part.is_empty() {
        0
    } else {
        user_part.parse::<u32>().ok()?
    };

    let gid = match group_part {
        Some(g) if !g.is_empty() => g.parse::<u32>().ok()?,
        _ => uid,
    };

    Some((uid, gid))
}

#[cfg(test)]
mod docker_tests {
    use super::*;
    use crate::{
        BuildRequest, ContainerLogConfig, DeployRequest, PortMapping, Protocol, ResourceLimits,
        RestartPolicy,
    };
    use serial_test::serial;
    use std::collections::HashMap;
    use std::path::PathBuf;
    use tempfile::TempDir;
    use tokio::fs;
    use tokio::time::{timeout, Duration};

    async fn create_test_docker_runtime() -> Result<DockerRuntime, Box<dyn std::error::Error>> {
        let docker = Docker::connect_with_local_defaults()?;

        // Every test using this runtime deploys `alpine:latest`, but
        // `create_container` doesn't auto-pull a missing image -- it 404s.
        // These tests only ever worked by luck, relying on some other test
        // happening to pull it first; under parallel test execution (this
        // job doesn't set --test-threads=1) that ordering isn't guaranteed
        // and a test run first hits "No such image". Pull once, up front.
        use bollard::query_parameters::CreateImageOptions;
        use futures::StreamExt;
        let mut pull_stream = docker.create_image(
            Some(CreateImageOptions {
                from_image: Some("alpine".to_string()),
                tag: Some("latest".to_string()),
                ..Default::default()
            }),
            None,
            None,
        );
        while let Some(result) = pull_stream.next().await {
            result?;
        }

        Ok(DockerRuntime::new(
            Arc::new(docker),
            false,
            "test-network".to_string(),
        ))
    }

    /// `Docker::connect_with_local_defaults` only builds the client — it
    /// doesn't dial the daemon — so this is safe to call without Docker
    /// running, unlike `create_test_docker_runtime`'s async siblings.
    fn test_runtime() -> DockerRuntime {
        let docker = Docker::connect_with_local_defaults()
            .expect("bollard client construction (no connection made)");
        DockerRuntime::new(Arc::new(docker), false, "test-network".to_string())
    }

    #[test]
    fn test_dns_for_container_none_when_no_resolver_configured() {
        // Overlay not bootstrapped yet and no static override: `dns` stays
        // unset so Docker falls back to its embedded resolver, which itself
        // forwards to the host's own default DNS. Not a regression.
        assert!(test_runtime().dns_for_container().is_none());
    }

    #[test]
    fn test_dns_for_container_puts_overlay_resolver_first() {
        let slot = Arc::new(std::sync::RwLock::new(Some(
            "172.18.0.1".parse::<std::net::IpAddr>().unwrap(),
        )));
        let runtime = test_runtime().with_overlay_dns_slot(slot);

        let dns = runtime.dns_for_container().expect("resolver is set");

        assert_eq!(dns[0], "172.18.0.1", "Hickory resolver must stay primary");
        assert!(dns.len() <= 3, "glibc/musl ignore nameservers past the 3rd");
    }

    #[test]
    fn test_dns_for_container_overlay_slot_present_but_unpopulated() {
        // Slot exists (agent wired it) but network_sync hasn't published the
        // bridge IP yet — must behave like "no resolver configured", not panic.
        let slot = Arc::new(std::sync::RwLock::new(None));
        let runtime = test_runtime().with_overlay_dns_slot(slot);

        assert!(runtime.dns_for_container().is_none());
    }

    #[test]
    fn test_dns_for_container_static_servers_stay_primary() {
        let runtime = test_runtime().with_dns_servers(vec!["10.0.0.53".to_string()]);

        let dns = runtime.dns_for_container().expect("static servers set");

        assert_eq!(dns[0], "10.0.0.53");
        assert!(dns.len() <= 3);
    }

    // --- parse_non_loopback_nameservers: pure parsing, deterministic ---

    #[test]
    fn test_parse_non_loopback_nameservers_skips_systemd_resolved_stub() {
        let resolv_conf = "nameserver 127.0.0.53\noptions edns0 trust-ad\n";
        assert!(parse_non_loopback_nameservers(resolv_conf).is_empty());
    }

    #[test]
    fn test_parse_non_loopback_nameservers_keeps_real_servers() {
        let resolv_conf = "nameserver 10.0.0.53\nnameserver 8.8.8.8\nsearch example.com\n";
        assert_eq!(
            parse_non_loopback_nameservers(resolv_conf),
            vec!["10.0.0.53", "8.8.8.8"]
        );
    }

    #[test]
    fn test_parse_non_loopback_nameservers_skips_ipv6_loopback_and_malformed() {
        let resolv_conf = "nameserver ::1\nnameserver not-an-ip\nnameserver 2001:4860:4860::8888\n";
        assert_eq!(
            parse_non_loopback_nameservers(resolv_conf),
            vec!["2001:4860:4860::8888"]
        );
    }

    // --- merge_dns_with_fallback: pure merge, deterministic ---

    #[test]
    fn test_merge_dns_with_fallback_appends_pool_after_primary() {
        let merged = merge_dns_with_fallback(
            vec!["172.18.0.1".to_string()],
            &["10.0.0.53".to_string(), "10.0.0.54".to_string()],
        );
        assert_eq!(merged, vec!["172.18.0.1", "10.0.0.53", "10.0.0.54"]);
    }

    #[test]
    fn test_merge_dns_with_fallback_caps_at_three() {
        let merged = merge_dns_with_fallback(
            vec!["172.18.0.1".to_string()],
            &[
                "10.0.0.53".to_string(),
                "10.0.0.54".to_string(),
                "10.0.0.55".to_string(),
            ],
        );
        assert_eq!(merged, vec!["172.18.0.1", "10.0.0.53", "10.0.0.54"]);
    }

    #[test]
    fn test_merge_dns_with_fallback_dedupes_against_primary() {
        // Primary already equals the host's configured server (single-host
        // setup where the resolver IP happens to match) — must not repeat it.
        let merged = merge_dns_with_fallback(
            vec!["172.18.0.1".to_string(), "10.0.0.53".to_string()],
            &["10.0.0.53".to_string()],
        );
        assert_eq!(merged, vec!["172.18.0.1", "10.0.0.53"]);
    }

    #[test]
    fn test_merge_dns_with_fallback_empty_pool_leaves_primary_untouched() {
        // Host default DNS undiscoverable (e.g. no readable resolv.conf) —
        // must not panic or invent servers, just keep the primary resolver.
        let merged = merge_dns_with_fallback(vec!["172.18.0.1".to_string()], &[]);
        assert_eq!(merged, vec!["172.18.0.1"]);
    }

    /// Runs `cmd` inside `container_id` and returns combined stdout+stderr,
    /// bounded so a hung DNS query (the exact failure mode this crate
    /// exists to prevent) can't hang the test suite.
    async fn exec_capture(docker: &Docker, container_id: &str, cmd: Vec<&str>) -> String {
        let exec_config = bollard::models::ExecConfig {
            attach_stdout: Some(true),
            attach_stderr: Some(true),
            cmd: Some(cmd.into_iter().map(str::to_string).collect()),
            ..Default::default()
        };
        let exec = docker
            .create_exec(container_id, exec_config)
            .await
            .expect("create_exec");

        timeout(Duration::from_secs(15), async {
            let mut combined = String::new();
            if let Ok(bollard::exec::StartExecResults::Attached { mut output, .. }) = docker
                .start_exec(&exec.id, None::<bollard::exec::StartExecOptions>)
                .await
            {
                while let Some(Ok(msg)) = output.next().await {
                    match msg {
                        bollard::container::LogOutput::StdOut { message }
                        | bollard::container::LogOutput::StdErr { message } => {
                            combined.push_str(&String::from_utf8_lossy(&message));
                        }
                        _ => {}
                    }
                }
            }
            combined
        })
        .await
        .unwrap_or_else(|_| "TIMED OUT".to_string())
    }

    /// End-to-end reproduction of the production incident this change
    /// fixes: the per-node Hickory resolver becomes unreachable, and a
    /// container that only had it as `nameserver` hung on every DNS query
    /// (`wget api.stripe.com` never returning). Uses a real container
    /// against the local Docker daemon — no mocks — because the bug lived
    /// in what Docker actually writes to `/etc/resolv.conf`, not in our
    /// in-memory list-building logic (already covered by the pure
    /// `dns_for_container` / `merge_dns_with_fallback` tests above).
    #[tokio::test]
    #[serial]
    async fn test_dns_fallback_survives_unreachable_primary_resolver() {
        let runtime = match create_test_docker_runtime().await {
            Ok(r) => r,
            Err(e) => {
                println!("🔧 Docker not available, skipping: {}", e);
                return;
            }
        };
        let docker = match Docker::connect_with_local_defaults() {
            Ok(d) => d,
            Err(e) => {
                println!("🔧 Docker not available, skipping: {}", e);
                return;
            }
        };
        if docker.ping().await.is_err() {
            println!("🔧 Docker ping failed, skipping");
            return;
        }

        // RFC 5737 TEST-NET-3 — guaranteed non-routable, so a query to it
        // times out exactly like a crashed/hung resolver process would,
        // without depending on any real IP actually being unreachable.
        let dead_resolver = "203.0.113.1";
        let slot = Arc::new(std::sync::RwLock::new(Some(
            dead_resolver.parse::<std::net::IpAddr>().unwrap(),
        )));
        let runtime = runtime.with_overlay_dns_slot(slot);

        let name = "temps-dns-fallback-test";
        let _ = runtime.remove_container(name).await;

        let deploy_request = DeployRequest {
            image_name: "alpine:latest".to_string(),
            container_name: name.to_string(),
            environment_vars: HashMap::new(),
            secrets: HashMap::new(),
            port_mappings: vec![],
            network_name: None,
            resource_limits: ResourceLimits {
                cpu_limit: None,
                memory_limit_mb: None,
                disk_limit_mb: None,
            },
            restart_policy: RestartPolicy::Never,
            log_path: PathBuf::from("/tmp/temps-dns-fallback-test.log"),
            command: Some(vec!["sleep".to_string(), "60".to_string()]),
            log_config: Some(ContainerLogConfig::app_default()),
            labels: HashMap::new(),
        };

        let info = match runtime.deploy_container(deploy_request).await {
            Ok(i) => i,
            Err(e) => {
                println!("🔧 Container deployment failed (may be expected): {}", e);
                return;
            }
        };

        let resolv_conf =
            exec_capture(&docker, &info.container_id, vec!["cat", "/etc/resolv.conf"]).await;
        println!("container /etc/resolv.conf:\n{resolv_conf}");
        assert!(
            resolv_conf.contains(dead_resolver),
            "simulated dead resolver missing from resolv.conf — production \
             wiring wouldn't match what this test exercises: {resolv_conf}"
        );

        // The actual incident repro: a real query must still succeed even
        // though the FIRST nameserver never answers. Bounded well under the
        // suite-level 15s exec timeout so a regression fails fast instead
        // of hanging CI.
        let lookup =
            exec_capture(&docker, &info.container_id, vec!["nslookup", "google.com"]).await;
        println!("nslookup google.com ->\n{lookup}");

        let _ = runtime.remove_container(&info.container_id).await;

        assert!(
            lookup.contains("Address") && !lookup.contains("TIMED OUT"),
            "external DNS resolution did not fail over to the host's own \
             default DNS servers when the primary resolver was unreachable \
             — temps-dns-resolver is still a SPOF: {lookup}"
        );
    }

    #[test]
    fn test_write_secrets_to_host_dir_creates_files_with_correct_perms() {
        let tmp = TempDir::new().unwrap();
        let dir = tmp.path().join("c1");

        let mut secrets = HashMap::new();
        secrets.insert("DB_PASSWORD".to_string(), "s3cret".to_string());
        secrets.insert("API_KEY".to_string(), "abc\ndef".to_string());

        write_secrets_to_host_dir(&dir, &secrets, None).expect("write");

        let db = std::fs::read_to_string(dir.join("DB_PASSWORD")).unwrap();
        assert_eq!(db, "s3cret");
        let api = std::fs::read_to_string(dir.join("API_KEY")).unwrap();
        assert_eq!(api, "abc\ndef");

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let dir_mode = std::fs::metadata(&dir).unwrap().permissions().mode() & 0o777;
            assert_eq!(dir_mode, 0o700);
            let file_mode = std::fs::metadata(dir.join("DB_PASSWORD"))
                .unwrap()
                .permissions()
                .mode()
                & 0o777;
            assert_eq!(file_mode, 0o400);
        }
    }

    #[test]
    fn test_write_secrets_to_host_dir_overwrites_stale_dir() {
        let tmp = TempDir::new().unwrap();
        let dir = tmp.path().join("c2");

        let mut first = HashMap::new();
        first.insert("OLD".to_string(), "old".to_string());
        write_secrets_to_host_dir(&dir, &first, None).unwrap();
        assert!(dir.join("OLD").exists());

        let mut second = HashMap::new();
        second.insert("NEW".to_string(), "new".to_string());
        write_secrets_to_host_dir(&dir, &second, None).unwrap();
        // Stale file from first call must be gone
        assert!(!dir.join("OLD").exists());
        assert!(dir.join("NEW").exists());
    }

    #[test]
    fn test_default_secrets_root_uses_temps_data_dir() {
        // Smoke test the env precedence. We can't unset HOME without
        // breaking other tests, so verify TEMPS_DATA_DIR wins when set.
        let tmp = TempDir::new().unwrap();
        let prev = std::env::var("TEMPS_DATA_DIR").ok();
        std::env::set_var("TEMPS_DATA_DIR", tmp.path());
        let root = default_secrets_root();
        assert_eq!(root, tmp.path().join("secrets"));
        // Critically, this must NOT live under /tmp (unless TEMPS_DATA_DIR
        // points there explicitly) — that was the bug we just fixed.
        match prev {
            Some(v) => std::env::set_var("TEMPS_DATA_DIR", v),
            None => std::env::remove_var("TEMPS_DATA_DIR"),
        }
    }

    #[test]
    fn test_default_secrets_root_is_persistent_path() {
        // Without TEMPS_DATA_DIR, the resolved root must live under
        // $HOME/.temps/secrets — i.e. NOT under std::env::temp_dir(),
        // which is tmpfs on most Linux distros and was the source of
        // the post-reboot redeploy regression.
        let prev_data = std::env::var("TEMPS_DATA_DIR").ok();
        std::env::remove_var("TEMPS_DATA_DIR");
        let root = default_secrets_root();
        assert!(
            !root.starts_with(std::env::temp_dir()),
            "secrets root unexpectedly resolved to a tmpfs path: {}",
            root.display()
        );
        if let Some(v) = prev_data {
            std::env::set_var("TEMPS_DATA_DIR", v);
        }
    }

    #[test]
    fn test_write_secrets_to_host_dir_rejects_path_traversal() {
        let tmp = TempDir::new().unwrap();
        let dir = tmp.path().join("c3");

        for bad in ["..", ".", "../escape", "a/b", "a\\b", "with\0null"] {
            let mut secrets = HashMap::new();
            secrets.insert(bad.to_string(), "v".to_string());
            let err = write_secrets_to_host_dir(&dir, &secrets, None).unwrap_err();
            assert_eq!(err.kind(), std::io::ErrorKind::InvalidInput, "key={}", bad);
        }
    }

    #[test]
    fn test_write_secrets_to_host_dir_empty_creates_empty_dir() {
        let tmp = TempDir::new().unwrap();
        let dir = tmp.path().join("c4");
        let secrets: HashMap<String, String> = HashMap::new();
        write_secrets_to_host_dir(&dir, &secrets, None).unwrap();
        assert!(dir.exists());
        assert_eq!(std::fs::read_dir(&dir).unwrap().count(), 0);
    }

    #[test]
    fn test_parse_numeric_user_spec() {
        // Empty / root → (0, 0)
        assert_eq!(parse_numeric_user_spec(""), Some((0, 0)));
        assert_eq!(parse_numeric_user_spec("  "), Some((0, 0)));
        assert_eq!(parse_numeric_user_spec("root"), Some((0, 0)));
        assert_eq!(parse_numeric_user_spec("0"), Some((0, 0)));

        // Uid only → gid defaults to uid (Docker's behavior)
        assert_eq!(parse_numeric_user_spec("1000"), Some((1000, 1000)));
        assert_eq!(parse_numeric_user_spec("65532"), Some((65532, 65532)));

        // uid:gid
        assert_eq!(parse_numeric_user_spec("1000:2000"), Some((1000, 2000)));
        assert_eq!(parse_numeric_user_spec("65532:65532"), Some((65532, 65532)));

        // :gid (uid defaults to 0 — matches Docker)
        assert_eq!(parse_numeric_user_spec(":1000"), Some((0, 1000)));

        // uid: (trailing colon → gid falls back to uid)
        assert_eq!(parse_numeric_user_spec("1000:"), Some((1000, 1000)));

        // Named users/groups are not numeric — caller falls back
        assert_eq!(parse_numeric_user_spec("node"), None);
        assert_eq!(parse_numeric_user_spec("node:node"), None);
        assert_eq!(parse_numeric_user_spec("1000:node"), None);
        assert_eq!(parse_numeric_user_spec("node:1000"), None);

        // Malformed
        assert_eq!(parse_numeric_user_spec("abc"), None);
        assert_eq!(parse_numeric_user_spec("-1"), None);
        assert_eq!(parse_numeric_user_spec("1:2:3"), None);
    }

    #[test]
    fn test_write_secrets_to_host_dir_chown_to_self_is_noop() {
        // chown(2) only succeeds when chown-ing to yourself (unless root).
        // We verify the happy path by chown-ing to our own uid/gid: the
        // function must succeed and leave the mode bits intact. This guards
        // against the chown being accidentally reordered before the file
        // writes (which would fail) or changing the permission bits.
        #[cfg(unix)]
        {
            use std::os::unix::fs::MetadataExt;
            use std::os::unix::fs::PermissionsExt;

            let tmp = TempDir::new().unwrap();
            let dir = tmp.path().join("c5");

            let mut secrets = HashMap::new();
            secrets.insert("DB_PASSWORD".to_string(), "s3cret".to_string());

            let my_uid = std::fs::metadata(tmp.path()).unwrap().uid();
            let my_gid = std::fs::metadata(tmp.path()).unwrap().gid();

            write_secrets_to_host_dir(&dir, &secrets, Some((my_uid, my_gid))).unwrap();

            let dir_mode = std::fs::metadata(&dir).unwrap().permissions().mode() & 0o777;
            assert_eq!(dir_mode, 0o700);
            let file_mode = std::fs::metadata(dir.join("DB_PASSWORD"))
                .unwrap()
                .permissions()
                .mode()
                & 0o777;
            assert_eq!(file_mode, 0o400);
        }
    }

    #[test]
    fn test_write_secrets_to_host_dir_falls_back_to_world_readable_on_eperm() {
        // When Temps runs unprivileged and is asked to chown to a uid that
        // isn't its own, chown returns EPERM. The function must not fail
        // the deploy — it must downgrade permissions so the container can
        // still read its secrets. Only runs as non-root (skipped under sudo).
        #[cfg(unix)]
        {
            use std::os::unix::fs::MetadataExt;
            use std::os::unix::fs::PermissionsExt;

            let tmp = TempDir::new().unwrap();
            let my_uid = std::fs::metadata(tmp.path()).unwrap().uid();
            if my_uid == 0 {
                eprintln!("running as root; skipping EPERM fallback test");
                return;
            }

            let dir = tmp.path().join("c6");
            let mut secrets = HashMap::new();
            secrets.insert("DB_PASSWORD".to_string(), "s3cret".to_string());

            // Ask for a uid/gid we definitely don't own — forces EPERM.
            write_secrets_to_host_dir(&dir, &secrets, Some((65532, 65532)))
                .expect("fallback must succeed, not error");

            let dir_mode = std::fs::metadata(&dir).unwrap().permissions().mode() & 0o777;
            assert_eq!(dir_mode, 0o755, "dir must be world-traversable on fallback");

            let file_mode = std::fs::metadata(dir.join("DB_PASSWORD"))
                .unwrap()
                .permissions()
                .mode()
                & 0o777;
            assert_eq!(file_mode, 0o444, "file must be world-readable on fallback");

            // Content still intact.
            assert_eq!(
                std::fs::read_to_string(dir.join("DB_PASSWORD")).unwrap(),
                "s3cret"
            );
        }
    }

    #[tokio::test]
    async fn test_docker_runtime_creation() {
        match create_test_docker_runtime().await {
            Ok(runtime) => {
                assert_eq!(runtime.network_name, "test-network");
                assert!(!runtime.use_buildkit);
                println!("✅ Docker runtime created successfully");
            }
            Err(e) => {
                println!(
                    "🔧 Docker not available (expected in some test environments): {}",
                    e
                );
            }
        }
    }

    #[tokio::test]
    async fn test_overlay_network_default_unset() {
        match create_test_docker_runtime().await {
            Ok(runtime) => {
                assert!(
                    runtime.overlay_network.is_none(),
                    "overlay_network must default to None for backwards compatibility"
                );
            }
            Err(e) => {
                println!("🔧 Docker not available: {}", e);
            }
        }
    }

    #[tokio::test]
    async fn test_with_overlay_network_sets_field() {
        match create_test_docker_runtime().await {
            Ok(runtime) => {
                let runtime = runtime.with_overlay_network("temps-overlay".to_string());
                assert_eq!(runtime.overlay_network.as_deref(), Some("temps-overlay"));
            }
            Err(e) => {
                println!("🔧 Docker not available: {}", e);
            }
        }
    }

    #[tokio::test]
    async fn test_maybe_attach_overlay_noop_when_unset() {
        // When `overlay_network` is None, the function returns Ok(()) without
        // ever calling Docker. We can prove that by passing a runtime to a
        // bogus container_id — Docker would 404 if we tried to attach, but
        // we never call it.
        match create_test_docker_runtime().await {
            Ok(runtime) => {
                let r = runtime.maybe_attach_overlay("non-existent-container").await;
                assert!(r.is_ok(), "must be Ok(()) when overlay_network is None");
            }
            Err(e) => {
                println!("🔧 Docker not available: {}", e);
            }
        }
    }

    #[tokio::test]
    async fn test_maybe_attach_overlay_skips_when_network_missing() {
        // When the overlay network does not exist on the host, the function
        // logs and returns Ok(()) — it does NOT propagate an error. This is
        // the "agent started before sync_loop bootstrapped the overlay"
        // case.
        match create_test_docker_runtime().await {
            Ok(runtime) => {
                let runtime =
                    runtime.with_overlay_network("temps-overlay-does-not-exist-xyz".to_string());
                let r = runtime.maybe_attach_overlay("non-existent-container").await;
                assert!(
                    r.is_ok(),
                    "must skip silently when overlay network missing, got {:?}",
                    r
                );
            }
            Err(e) => {
                println!("🔧 Docker not available: {}", e);
            }
        }
    }

    #[test]
    fn test_native_platform_detection() {
        let platform = DockerRuntime::detect_native_platform();

        // Verify platform format
        assert!(platform.starts_with("linux/"));

        // Verify it matches the current architecture
        #[cfg(target_arch = "x86_64")]
        {
            assert_eq!(platform, "linux/amd64");
            println!("✅ Detected x86_64 platform: {}", platform);
        }

        #[cfg(target_arch = "aarch64")]
        {
            assert_eq!(platform, "linux/arm64");
            println!("✅ Detected ARM64 platform: {}", platform);
        }

        // Verify platform is one of the supported architectures
        assert!(
            platform == "linux/amd64" || platform == "linux/arm64",
            "Platform should be either linux/amd64 or linux/arm64, got: {}",
            platform
        );
    }

    #[tokio::test]
    #[serial]
    async fn test_docker_build_with_dockerfile() {
        let temp_dir = TempDir::new().unwrap();
        let context_path = temp_dir.path().to_path_buf();

        // Create a simple Dockerfile
        let dockerfile = r#"FROM alpine:latest
RUN echo "Hello from Docker test" > /hello.txt
CMD ["cat", "/hello.txt"]
"#;
        fs::write(context_path.join("Dockerfile"), dockerfile)
            .await
            .unwrap();

        match create_test_docker_runtime().await {
            Ok(runtime) => {
                let request = BuildRequest {
                    image_name: "docker-test:latest".to_string(),
                    context_path,
                    dockerfile_path: None,
                    build_args: HashMap::new(),
                    build_args_buildkit: HashMap::new(),
                    platform: None,
                    log_path: temp_dir.path().join("build.log"),
                };

                let result = timeout(Duration::from_secs(60), runtime.build_image(request)).await;

                match result {
                    Ok(Ok(build_result)) => {
                        println!("✅ Docker build succeeded: {}", build_result.image_name);
                        assert_eq!(build_result.image_name, "docker-test:latest");
                        assert!(build_result.build_duration_ms > 0);
                    }
                    Ok(Err(e)) => {
                        println!("🔧 Docker build failed (may be expected): {}", e);
                    }
                    Err(_) => {
                        println!("⏰ Docker build timed out");
                    }
                }
            }
            Err(e) => {
                println!("🔧 Docker not available: {}", e);
            }
        }
    }

    #[tokio::test]
    async fn test_runtime_info() {
        match create_test_docker_runtime().await {
            Ok(runtime) => match runtime.get_runtime_info().await {
                Ok(info) => {
                    println!("✅ Runtime info retrieved:");
                    println!("  Type: {}", info.runtime_type);
                    println!("  Version: {}", info.version);
                    println!("  CPU cores: {}", info.available_cpu_cores);
                    println!("  Memory: {} MB", info.available_memory_mb);

                    assert_eq!(info.runtime_type, "Docker");
                    assert!(info.available_cpu_cores > 0);
                    assert!(info.available_memory_mb > 0);
                }
                Err(e) => {
                    println!("🔧 Failed to get runtime info: {}", e);
                }
            },
            Err(e) => {
                println!("🔧 Docker not available: {}", e);
            }
        }
    }

    #[tokio::test]
    #[serial]
    async fn test_container_lifecycle() {
        match create_test_docker_runtime().await {
            Ok(runtime) => {
                // First ensure we have an image to work with (alpine is usually available)
                let deploy_request = DeployRequest {
                    image_name: "alpine:latest".to_string(),
                    container_name: "lifecycle-test".to_string(),
                    environment_vars: {
                        let mut env = HashMap::new();
                        env.insert("TEST_VAR".to_string(), "test_value".to_string());
                        env
                    },
                    secrets: HashMap::new(),
                    port_mappings: vec![],
                    network_name: None,
                    resource_limits: ResourceLimits {
                        cpu_limit: Some(0.5),
                        memory_limit_mb: Some(64),
                        disk_limit_mb: Some(256),
                    },
                    restart_policy: RestartPolicy::Never,
                    log_path: PathBuf::from("/tmp/lifecycle-test.log"),
                    command: Some(vec!["sleep".to_string(), "30".to_string()]),
                    log_config: Some(ContainerLogConfig::app_default()),
                    labels: HashMap::new(),
                };

                let deploy_result = runtime.deploy_container(deploy_request).await;

                match deploy_result {
                    Ok(deploy_info) => {
                        println!("✅ Container deployed: {}", deploy_info.container_name);

                        // Test container operations
                        let container_id = &deploy_info.container_id;

                        // Test getting container info
                        if let Ok(info) = runtime.get_container_info(container_id).await {
                            println!("📋 Container info: {:?}", info.status);
                        }

                        // Test pause/resume
                        if let Ok(()) = runtime.pause_container(container_id).await {
                            println!("⏸️  Container paused");
                        }

                        if let Ok(()) = runtime.resume_container(container_id).await {
                            println!("▶️  Container resumed");
                        }

                        // Test stop
                        if let Ok(()) = runtime.stop_container(container_id).await {
                            println!("⏹️  Container stopped");
                        }

                        // Test remove
                        if let Ok(()) = runtime.remove_container(container_id).await {
                            println!("🗑️  Container removed");
                        }

                        println!("✅ Container lifecycle test completed");
                    }
                    Err(e) => {
                        println!("🔧 Container deployment failed (may be expected): {}", e);
                    }
                }
            }
            Err(e) => {
                println!("🔧 Docker not available: {}", e);
            }
        }
    }

    /// Empirical proof of the "no limit unless opted in" contract: a deploy
    /// with `cpu_limit: None` / `memory_limit_mb: None` must produce a Docker
    /// container whose `HostConfig.NanoCpus` and `Memory` are 0 (unset =
    /// uncapped), and an explicit limit must actually reach Docker. Inspects
    /// the live container via bollard rather than trusting the request struct.
    #[tokio::test]
    #[serial]
    async fn test_resource_limits_none_means_uncapped() {
        let runtime = match create_test_docker_runtime().await {
            Ok(r) => r,
            Err(e) => {
                println!("🔧 Docker not available, skipping: {}", e);
                return;
            }
        };
        let docker = match Docker::connect_with_local_defaults() {
            Ok(d) => d,
            Err(e) => {
                println!("🔧 Docker not available, skipping: {}", e);
                return;
            }
        };
        if docker.ping().await.is_err() {
            println!("🔧 Docker ping failed, skipping");
            return;
        }

        let base = |name: &str, limits: ResourceLimits| DeployRequest {
            image_name: "alpine:latest".to_string(),
            container_name: name.to_string(),
            environment_vars: HashMap::new(),
            secrets: HashMap::new(),
            port_mappings: vec![],
            network_name: None,
            resource_limits: limits,
            restart_policy: RestartPolicy::Never,
            log_path: PathBuf::from(format!("/tmp/{}.log", name)),
            command: Some(vec!["sleep".to_string(), "30".to_string()]),
            log_config: Some(ContainerLogConfig::app_default()),
            labels: HashMap::new(),
        };

        let inspect_caps = |id: String| {
            let docker = docker.clone();
            async move {
                let i = docker
                    .inspect_container(
                        &id,
                        None::<bollard::query_parameters::InspectContainerOptions>,
                    )
                    .await
                    .expect("inspect");
                let hc = i.host_config.expect("host_config");
                (hc.nano_cpus.unwrap_or(0), hc.memory.unwrap_or(0))
            }
        };

        // --- Case 1: no limits configured -> container must be UNCAPPED ---
        let uncapped_name = "temps-rl-uncapped-test";
        let _ = runtime.remove_container(uncapped_name).await;
        let info = runtime
            .deploy_container(base(
                uncapped_name,
                ResourceLimits {
                    cpu_limit: None,
                    memory_limit_mb: None,
                    disk_limit_mb: None,
                },
            ))
            .await
            .expect("deploy uncapped");
        let (nano_cpus, memory) = inspect_caps(info.container_id.clone()).await;
        let _ = runtime.remove_container(&info.container_id).await;
        println!(
            "UNCAPPED deploy -> NanoCpus={} Memory={} (both must be 0)",
            nano_cpus, memory
        );
        assert_eq!(
            nano_cpus, 0,
            "None cpu_limit leaked a CPU cap into the container ({})",
            nano_cpus
        );
        assert_eq!(
            memory, 0,
            "None memory_limit_mb leaked a memory cap into the container ({})",
            memory
        );

        // --- Case 2 (control): explicit limits MUST reach Docker ---
        let capped_name = "temps-rl-capped-test";
        let _ = runtime.remove_container(capped_name).await;
        let info = runtime
            .deploy_container(base(
                capped_name,
                ResourceLimits {
                    cpu_limit: Some(0.5),
                    memory_limit_mb: Some(64),
                    disk_limit_mb: None,
                },
            ))
            .await
            .expect("deploy capped");
        let (nano_cpus, memory) = inspect_caps(info.container_id.clone()).await;
        let _ = runtime.remove_container(&info.container_id).await;
        println!(
            "CAPPED deploy -> NanoCpus={} Memory={} (expect 500000000 / 67108864)",
            nano_cpus, memory
        );
        assert_eq!(nano_cpus, 500_000_000, "explicit 0.5 CPU not applied");
        assert_eq!(memory, 64 * 1024 * 1024, "explicit 64MB not applied");
    }

    #[tokio::test]
    async fn test_image_operations() {
        match create_test_docker_runtime().await {
            Ok(runtime) => {
                // Test list images
                match runtime.list_images().await {
                    Ok(images) => {
                        println!("✅ Found {} images", images.len());
                        // Don't assert specific count as it depends on system state
                    }
                    Err(e) => {
                        println!("🔧 Failed to list images: {}", e);
                    }
                }

                // Test remove non-existent image
                match runtime
                    .remove_image("definitely-does-not-exist:latest")
                    .await
                {
                    Ok(()) => println!("⚠️ Unexpectedly succeeded removing non-existent image"),
                    Err(e) => println!("✅ Correctly failed to remove non-existent image: {}", e),
                }
            }
            Err(e) => {
                println!("🔧 Docker not available: {}", e);
            }
        }
    }

    #[tokio::test]
    async fn test_list_containers() {
        match create_test_docker_runtime().await {
            Ok(runtime) => match runtime.list_containers().await {
                Ok(containers) => {
                    println!("✅ Found {} containers", containers.len());
                    for container in containers.iter().take(3) {
                        println!("  📦 {}: {:?}", container.container_name, container.status);
                    }
                }
                Err(e) => {
                    println!("🔧 Failed to list containers: {}", e);
                }
            },
            Err(e) => {
                println!("🔧 Docker not available: {}", e);
            }
        }
    }

    #[tokio::test]
    async fn test_network_operations() {
        match create_test_docker_runtime().await {
            Ok(runtime) => {
                // Test network existence (this will try to create if not exists)
                match runtime.ensure_network_exists().await {
                    Ok(()) => {
                        println!("✅ Network operations test passed");
                    }
                    Err(e) => {
                        println!("🔧 Network operations failed: {}", e);
                    }
                }
            }
            Err(e) => {
                println!("🔧 Docker not available: {}", e);
            }
        }
    }

    #[tokio::test]
    async fn test_resource_limits_validation() {
        let resource_limits = ResourceLimits {
            cpu_limit: Some(2.0),
            memory_limit_mb: Some(512),
            disk_limit_mb: Some(1024),
        };

        // Test that resource limits are properly structured
        assert!(resource_limits.cpu_limit.unwrap() > 0.0);
        assert!(resource_limits.memory_limit_mb.unwrap() > 0);
        assert!(resource_limits.disk_limit_mb.unwrap() > 0);

        println!("✅ Resource limits validation passed");
    }

    #[tokio::test]
    async fn test_port_mapping_validation() {
        let port_mapping = PortMapping {
            host_port: 8080,
            container_port: 80,
            protocol: Protocol::Tcp,
        };

        assert_eq!(port_mapping.host_port, 8080);
        assert_eq!(port_mapping.container_port, 80);
        assert!(matches!(port_mapping.protocol, Protocol::Tcp));

        println!("✅ Port mapping validation passed");
    }

    #[tokio::test]
    async fn test_restart_policy_enum() {
        let policies = [
            RestartPolicy::Never,
            RestartPolicy::Always,
            RestartPolicy::OnFailure,
            RestartPolicy::UnlessStopped,
        ];

        // Just test that all enum variants exist and can be created
        assert_eq!(policies.len(), 4);
        println!("✅ Restart policy enum validation passed");
    }

    #[tokio::test]
    async fn test_error_types() {
        // Test that error types can be created and match properly
        let build_error = BuilderError::BuildFailed("test error".to_string());
        let deploy_error = DeployerError::DeploymentFailed("test deploy error".to_string());

        assert!(matches!(build_error, BuilderError::BuildFailed(_)));
        assert!(matches!(deploy_error, DeployerError::DeploymentFailed(_)));

        println!("✅ Error types validation passed");
    }

    // ── Container stats helpers ──────────────────────────────────────────────

    fn cpu_sample(
        total: u64,
        system: u64,
        online_cpus: u32,
    ) -> bollard::models::ContainerStatsResponse {
        bollard::models::ContainerStatsResponse {
            cpu_stats: Some(bollard::models::ContainerCpuStats {
                cpu_usage: Some(bollard::models::ContainerCpuUsage {
                    total_usage: Some(total),
                    ..Default::default()
                }),
                system_cpu_usage: Some(system),
                online_cpus: Some(online_cpus),
                ..Default::default()
            }),
            ..Default::default()
        }
    }

    /// 50% on a 2-CPU host: cpu_delta=1e9 (1s of cpu-time), system_delta=4e9
    /// (wall_ticks * cpus). (cpu_delta/system_delta)*online_cpus = 0.25*2 = 50%.
    #[test]
    fn cpu_percent_50pct_two_cpus() {
        let prev = cpu_sample(0, 0, 2);
        let curr = cpu_sample(1_000_000_000, 4_000_000_000, 2);
        let pct = cpu_percent_from_samples(&curr, &prev).unwrap();
        assert!((pct - 50.0).abs() < 0.01, "expected ~50%, got {pct}");
    }

    /// A 2-core container fully pinned reads as 200% — we intentionally
    /// don't clamp to 100, since the UI shows "x.x% / N cores".
    #[test]
    fn cpu_percent_fully_pinned_two_cpus_reads_200pct() {
        let prev = cpu_sample(0, 0, 2);
        let curr = cpu_sample(2_000_000_000, 2_000_000_000, 2);
        let pct = cpu_percent_from_samples(&curr, &prev).unwrap();
        assert!((pct - 200.0).abs() < 0.01, "expected ~200%, got {pct}");
    }

    /// Identical samples (container idle or clock didn't advance) must
    /// produce None rather than 0% — the old code returned a spurious
    /// near-zero value here, which is what made the UI read "CPU 0.0%".
    #[test]
    fn cpu_percent_identical_samples_returns_none() {
        let prev = cpu_sample(5_000_000_000, 10_000_000_000, 4);
        let same = cpu_sample(5_000_000_000, 10_000_000_000, 4);
        assert!(cpu_percent_from_samples(&same, &prev).is_none());
    }

    #[test]
    fn cpu_percent_missing_counters_returns_none() {
        let prev = bollard::models::ContainerStatsResponse {
            cpu_stats: Some(bollard::models::ContainerCpuStats {
                cpu_usage: None,
                system_cpu_usage: Some(0),
                online_cpus: Some(1),
                ..Default::default()
            }),
            ..Default::default()
        };
        let curr = cpu_sample(1_000_000_000, 1_000_000_000, 1);
        assert!(cpu_percent_from_samples(&curr, &prev).is_none());
    }

    fn mem_stats(
        usage: u64,
        limit: u64,
        cache: Option<(&'static str, u64)>,
    ) -> bollard::models::ContainerMemoryStats {
        let mut stats_map = std::collections::HashMap::new();
        if let Some((key, val)) = cache {
            stats_map.insert(key.to_string(), val);
        }
        bollard::models::ContainerMemoryStats {
            usage: Some(usage),
            limit: Some(limit),
            stats: if cache.is_some() {
                Some(stats_map)
            } else {
                None
            },
            ..Default::default()
        }
    }

    /// cgroup v2 hosts (Docker Desktop, modern Linux) report reclaimable
    /// file pages as `inactive_file`. Subtract it so the number matches
    /// `docker stats`.
    #[test]
    fn memory_usage_subtracts_inactive_file_cgroup_v2() {
        let mem = mem_stats(
            16 * 1024 * 1024 * 1024,
            16 * 1024 * 1024 * 1024,
            Some(("inactive_file", 8 * 1024 * 1024 * 1024)),
        );
        assert_eq!(
            memory_usage_excluding_cache(&mem).unwrap(),
            8 * 1024 * 1024 * 1024
        );
    }

    /// cgroup v1 surfaces the same data as `cache`. Subtract it.
    #[test]
    fn memory_usage_subtracts_cache_cgroup_v1() {
        let mem = mem_stats(
            10 * 1024 * 1024 * 1024,
            16 * 1024 * 1024 * 1024,
            Some(("cache", 3 * 1024 * 1024 * 1024)),
        );
        assert_eq!(
            memory_usage_excluding_cache(&mem).unwrap(),
            7 * 1024 * 1024 * 1024
        );
    }

    /// Hosts that report both should prefer `inactive_file` — matches the
    /// Docker CLI exactly and is the more accurate signal.
    #[test]
    fn memory_usage_prefers_inactive_file_when_both_present() {
        let mut stats_map = std::collections::HashMap::new();
        stats_map.insert("inactive_file".to_string(), 4 * 1024 * 1024 * 1024);
        stats_map.insert("cache".to_string(), 6 * 1024 * 1024 * 1024);
        let mem = bollard::models::ContainerMemoryStats {
            usage: Some(10 * 1024 * 1024 * 1024),
            limit: Some(16 * 1024 * 1024 * 1024),
            stats: Some(stats_map),
            ..Default::default()
        };
        assert_eq!(
            memory_usage_excluding_cache(&mem).unwrap(),
            6 * 1024 * 1024 * 1024
        );
    }

    /// Without cache info, return raw usage rather than crashing.
    #[test]
    fn memory_usage_returns_raw_when_no_cache_info() {
        let mem = mem_stats(5 * 1024 * 1024 * 1024, 16 * 1024 * 1024 * 1024, None);
        assert_eq!(
            memory_usage_excluding_cache(&mem).unwrap(),
            5u64 * 1024 * 1024 * 1024
        );
    }

    /// Defensive: if `cache` is somehow larger than `usage` (sentinel
    /// values, stat skew), don't underflow — return raw usage.
    #[test]
    fn memory_usage_handles_cache_larger_than_usage() {
        let mem = mem_stats(
            1024 * 1024,
            16 * 1024 * 1024 * 1024,
            Some(("cache", 10 * 1024 * 1024 * 1024)),
        );
        assert_eq!(memory_usage_excluding_cache(&mem).unwrap(), 1024 * 1024);
    }

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

    /// `with_build_limits(0, ..)` must NOT create a zero-permit semaphore
    /// — that would deadlock every build. Clamp to 1.
    #[test]
    fn with_build_limits_clamps_max_concurrent_to_at_least_one() {
        let docker = match Docker::connect_with_local_defaults() {
            Ok(d) => Arc::new(d),
            Err(_) => return,
        };
        let rt = DockerRuntime::new(docker, false, "test-network".to_string())
            .with_build_limits(0, None);
        let sem = rt
            .build_semaphore
            .as_ref()
            .expect("with_build_limits sets a semaphore");
        assert_eq!(sem.available_permits(), 1);
    }

    /// A partial override (cpu set, memory zero) must be discarded entirely
    /// so admins don't accidentally pin only one dimension and starve the
    /// build on the other.
    #[test]
    fn with_build_limits_drops_override_when_either_dimension_is_zero() {
        let docker = match Docker::connect_with_local_defaults() {
            Ok(d) => Arc::new(d),
            Err(_) => return,
        };
        let rt = DockerRuntime::new(docker, false, "test-network".to_string()).with_build_limits(
            2,
            Some(BuildResourceLimits {
                cpu_cores: 1.5,
                memory_mb: 0,
            }),
        );
        assert!(rt.build_resource_override.is_none());
    }
}
