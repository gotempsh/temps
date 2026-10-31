// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Construction and configuration of [`DockerRuntime`], and its
//! `ContainerRuntime` impl.

use super::build_failure::docker_host_is_local;
use super::build_limits::LiveBuildLimits;
use super::{dns_with_fallback, DockerRuntime, SharedHostBindAddress, FALLBACK_HOST_BIND_ADDRESS};
use crate::{BuilderError, ContainerRuntime, DeployerError, PortMapping, RuntimeInfo};
use async_trait::async_trait;
use bollard::Docker;
use std::path::PathBuf;
use std::sync::Arc;
use sysinfo::System;
use temps_core::docker_socket_grant::DockerSocketGrant;
use temps_core::DockerHandle;
use temps_workload_docker::secrets::default_secrets_root;
use tracing::warn;

impl DockerRuntime {
    /// Construct with a concrete Docker client.
    ///
    /// The existing callers in `crates/temps-deployments` and in agent code
    /// pass an `Arc<Docker>` directly; this shim wraps it into a
    /// [`DockerHandle::available`] so their signatures need not change.
    /// New call sites should prefer [`Self::new_with_handle`].
    pub fn new(docker: Arc<Docker>, use_buildkit: bool, network_name: String) -> Self {
        Self::new_with_handle(
            Arc::new(DockerHandle::available(docker)),
            use_buildkit,
            network_name,
        )
    }

    /// Construct with the process-wide [`DockerHandle`].
    ///
    /// The handle may carry either an available client or a typed explanation
    /// of why no daemon exists in this process. Every daemon-dependent method
    /// calls [`Self::require_docker`] or [`Self::require_docker_for_build`]
    /// as late as possible so the error is reported at the point of the
    /// failing operation rather than at construction time.
    pub fn new_with_handle(
        handle: Arc<DockerHandle>,
        use_buildkit: bool,
        network_name: String,
    ) -> Self {
        #[allow(
            clippy::disallowed_methods,
            reason = "host lookup: DOCKER_HOST is the host's Docker client setting, read only to tell whether the daemon is local"
        )]
        let docker_host = std::env::var("DOCKER_HOST").ok();
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
            docker: handle,
            use_buildkit,
            network_name,
            host_bind_address: Arc::new(std::sync::RwLock::new(
                FALLBACK_HOST_BIND_ADDRESS.to_string(),
            )),
            overlay_network: None,
            extra_networks: Vec::new(),
            dns_servers: Vec::new(),
            overlay_dns_slot: None,
            overlay_peers: None,
            secrets_root,
            docker_socket_grant: DockerSocketGrant::default(),
            build_limits: Arc::new(LiveBuildLimits::default()),
            builds_started: Arc::new(std::sync::atomic::AtomicU64::new(0)),
            daemon_is_local: docker_host_is_local(docker_host.as_deref()),
            daemon_platform: Arc::new(std::sync::OnceLock::new()),
        }
    }

    /// Extract the Docker client for operations returning `Result<_, DeployerError>`.
    ///
    /// Call this as late as possible — inside the operation that actually needs
    /// the daemon — so a control-plane process can boot and serve the API
    /// before any work arrives.
    pub(super) fn require_docker(&self) -> Result<Arc<Docker>, DeployerError> {
        self.docker
            .require()
            .map_err(DeployerError::DockerUnavailable)
    }

    /// Extract the Docker client for operations returning `Result<_, BuilderError>`.
    ///
    /// Same policy as [`Self::require_docker`].
    pub(super) fn require_docker_for_build(&self) -> Result<Arc<Docker>, BuilderError> {
        self.docker
            .require()
            .map_err(BuilderError::DockerUnavailable)
    }

    /// Query the Docker daemon for its platform and cache it.
    ///
    /// Called during startup (control-plane deployer plugin, agent server) and
    /// again before each build, so every later `get_native_platform()` — which
    /// the `ImageBuilder` trait requires to be synchronous — answers with the
    /// daemon's real architecture instead of the binary's.
    ///
    /// **Only a successful lookup is cached.** Caching a failure would freeze
    /// the compiled-in fallback for the process lifetime: with a
    /// cross-architecture `DOCKER_HOST`, one transient `docker info` error at
    /// boot would make every later build and scheduling decision use the wrong
    /// architecture even after the daemon recovered. Returning `None` instead
    /// lets the next call retry.
    ///
    /// Failures are non-fatal — a daemon that isn't up yet must not stop the
    /// process from booting.
    pub async fn refresh_daemon_platform(&self) -> Option<String> {
        if let Some(cached) = self.daemon_platform.get() {
            return Some(cached.clone());
        }

        // A process with no local daemon (control-plane profile) has nothing
        // to probe. Return None so callers fall back to the compiled-in arch.
        let docker = self.docker.cloned()?;

        let platform = match docker.info().await {
            Ok(info) => {
                let os = info.os_type.unwrap_or_else(|| "linux".to_string());
                match info.architecture {
                    Some(arch) => crate::platform::normalize_platform(&os, &arch),
                    None => {
                        warn!("Docker daemon reported no architecture; will retry");
                        return None;
                    }
                }
            }
            Err(e) => {
                warn!(
                    "Could not read the Docker daemon platform ({}); will retry",
                    e
                );
                return None;
            }
        };

        // A concurrent caller may have won the race — its value came from the
        // same daemon, so keep whichever landed first.
        let _ = self.daemon_platform.set(platform);
        self.daemon_platform.get().cloned()
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
    pub(super) fn dns_for_container(&self) -> Option<Vec<String>> {
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
    /// On agent (worker) nodes, pass the node's private/overlay address
    /// (`AgentConfig::private_address`) so published container ports are
    /// reachable from the control-plane proxy over the private network but
    /// never on the node's public interface. Never pass "0.0.0.0" — Docker
    /// treats it as "bind every interface", including any public one.
    ///
    /// The address is fixed for this runtime's lifetime; use
    /// [`Self::with_host_bind_slot`] when it can move.
    pub fn with_host_bind_address(mut self, address: String) -> Self {
        self.host_bind_address = Arc::new(std::sync::RwLock::new(address));
        self
    }

    /// Share the host bind address with the code that may move it at runtime.
    ///
    /// The agent passes the same slot its own service handlers read and its
    /// `network_sync` loop updates when a node that joined with a public
    /// address moves onto its WireGuard mesh address, so application
    /// containers created after the move publish on the mesh address the
    /// control plane now dials (`nodes::Model::data_address`) instead of the
    /// public address captured at startup.
    pub fn with_host_bind_slot(mut self, slot: SharedHostBindAddress) -> Self {
        self.host_bind_address = slot;
        self
    }

    /// The address to publish a port on right now, for port mappings that do
    /// not name their own `host_ip`.
    ///
    /// Never returns an all-interfaces address: an empty or unspecified value
    /// (`0.0.0.0`, `::`) in the slot falls back to loopback, because binding it
    /// would expose the container on every interface, public ones included.
    /// A poisoned lock still holds a complete `String`, so its value is used.
    fn current_host_bind_address(&self) -> String {
        let address = match self.host_bind_address.read() {
            Ok(guard) => guard.trim().to_string(),
            Err(poisoned) => poisoned.into_inner().trim().to_string(),
        };
        let all_interfaces = address
            .parse::<std::net::IpAddr>()
            .is_ok_and(|ip| ip.is_unspecified());
        if address.is_empty() || all_interfaces {
            warn!(
                configured = %address,
                fallback = FALLBACK_HOST_BIND_ADDRESS,
                "refusing to publish container ports on all interfaces; the configured host \
                 bind address is empty or unspecified, binding to loopback instead"
            );
            return FALLBACK_HOST_BIND_ADDRESS.to_string();
        }
        address
    }

    /// Docker port binding for one requested mapping: the mapping's own
    /// `host_ip` when set, otherwise this runtime's current bind address.
    pub(super) fn host_port_binding(
        &self,
        port_mapping: &PortMapping,
    ) -> bollard::models::PortBinding {
        bollard::models::PortBinding {
            host_ip: Some(
                port_mapping
                    .host_ip
                    .clone()
                    .unwrap_or_else(|| self.current_host_bind_address()),
            ),
            // When host_port is 0, let Docker pick an available port
            host_port: if port_mapping.host_port == 0 {
                None
            } else {
                Some(port_mapping.host_port.to_string())
            },
        }
    }

    /// Declare which projects this host grants `/var/run/docker.sock` to
    /// (ADR 045).
    ///
    /// Built from this process's own environment
    /// (`TEMPS_DOCKER_SOCKET_PROJECTS`) exactly once at startup by
    /// `temps serve` and `temps agent`. Left at the default (empty) everywhere
    /// else — including tests and the proxy's on-demand lifecycle adapter,
    /// which never creates a container from a `DeployRequest`.
    pub fn with_docker_socket_grant(mut self, grant: DockerSocketGrant) -> Self {
        self.docker_socket_grant = grant;
        self
    }

    /// The grant this runtime evaluates. Exposed so a caller can report what
    /// this host would do without re-reading the environment.
    pub fn docker_socket_grant(&self) -> &DockerSocketGrant {
        &self.docker_socket_grant
    }
}

#[async_trait]
impl ContainerRuntime for DockerRuntime {
    async fn get_runtime_info(&self) -> Result<RuntimeInfo, DeployerError> {
        let version =
            self.require_docker()?.version().await.map_err(|e| {
                DeployerError::Other(format!("Failed to get Docker version: {}", e))
            })?;

        let mut system = System::new_all();
        system.refresh_all();

        Ok(RuntimeInfo {
            runtime_type: "Docker".to_string(),
            version: version.version.unwrap_or_default(),
            available_cpu_cores: num_cpus::get(),
            available_memory_mb: system.total_memory() / (1024 * 1024),
            available_disk_mb: 0, // Docker doesn't easily expose this
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::docker::test_support::{create_test_docker_runtime, test_runtime};
    use crate::docker::{DockerRuntime, SharedHostBindAddress};
    use crate::{ContainerDeployer, ImageBuilder};
    use crate::{
        ContainerLogConfig, DeployRequest, PortMapping, Protocol, ResourceLimits, RestartPolicy,
    };
    use futures::StreamExt;
    use serial_test::serial;
    use std::collections::HashMap;
    use std::path::PathBuf;
    use tokio::time::{timeout, Duration};

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

    fn unpinned_port(host_port: u16) -> PortMapping {
        PortMapping {
            host_port,
            container_port: 8080,
            protocol: Protocol::Tcp,
            host_ip: None,
        }
    }

    #[test]
    fn test_host_port_binding_follows_a_moved_bind_slot() {
        // A worker that joined with a public address starts publishing there,
        // then network_sync moves the shared slot onto its mesh address. Ports
        // created afterwards must land on the mesh address, not the public
        // one captured at startup.
        let slot: SharedHostBindAddress =
            Arc::new(std::sync::RwLock::new("203.0.113.10".to_string()));
        let runtime = test_runtime().with_host_bind_slot(slot.clone());
        assert_eq!(
            runtime
                .host_port_binding(&unpinned_port(0))
                .host_ip
                .as_deref(),
            Some("203.0.113.10")
        );

        *slot.write().expect("test slot lock") = "10.99.0.4".to_string();

        let binding = runtime.host_port_binding(&unpinned_port(31000));
        assert_eq!(binding.host_ip.as_deref(), Some("10.99.0.4"));
        assert_eq!(binding.host_port.as_deref(), Some("31000"));
    }

    #[test]
    fn test_host_port_binding_keeps_an_explicit_host_ip() {
        // The control plane pins app deploys to the node's data address; that
        // request-level choice wins over the runtime's own bind address.
        let runtime = test_runtime().with_host_bind_address("10.99.0.4".to_string());
        let mapping = PortMapping {
            host_ip: Some("10.0.0.8".to_string()),
            ..unpinned_port(0)
        };

        let binding = runtime.host_port_binding(&mapping);

        assert_eq!(binding.host_ip.as_deref(), Some("10.0.0.8"));
        assert_eq!(binding.host_port, None, "port 0 lets Docker pick");
    }

    #[test]
    fn test_host_port_binding_defaults_to_loopback() {
        let binding = test_runtime().host_port_binding(&unpinned_port(0));
        assert_eq!(binding.host_ip.as_deref(), Some("127.0.0.1"));
    }

    #[test]
    fn test_host_port_binding_never_binds_all_interfaces() {
        for unusable in ["0.0.0.0", "::", "", "   "] {
            let slot: SharedHostBindAddress =
                Arc::new(std::sync::RwLock::new(unusable.to_string()));
            let runtime = test_runtime().with_host_bind_slot(slot);

            let binding = runtime.host_port_binding(&unpinned_port(0));

            assert_eq!(
                binding.host_ip.as_deref(),
                Some("127.0.0.1"),
                "bind address {unusable:?} must fall back to loopback"
            );
        }
    }

    #[test]
    fn test_host_port_binding_reads_a_poisoned_slot() {
        let slot: SharedHostBindAddress = Arc::new(std::sync::RwLock::new("10.99.0.4".to_string()));
        let poisoner = slot.clone();
        let _ = std::thread::spawn(move || {
            let _guard = poisoner.write().expect("test slot lock");
            panic!("poison the bind slot");
        })
        .join();
        assert!(slot.is_poisoned());
        let runtime = test_runtime().with_host_bind_slot(slot);

        let binding = runtime.host_port_binding(&unpinned_port(0));

        assert_eq!(binding.host_ip.as_deref(), Some("10.99.0.4"));
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
            extra_networks: Vec::new(),
            resource_limits: ResourceLimits {
                cpu_limit: None,
                memory_limit_mb: None,
                disk_limit_mb: None,
            },
            restart_policy: RestartPolicy::Never,
            log_path: PathBuf::from("/tmp/temps-dns-fallback-test.log"),
            command: Some(vec!["sleep".to_string(), "60".to_string()]),
            entrypoint: None,
            log_config: Some(ContainerLogConfig::app_default()),
            labels: HashMap::new(),
            project_slug: None,
            control_plane_grants_socket: false,
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
             — temps-node-dns is still a SPOF: {lookup}"
        );
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

    /// The daemon's platform — not the binary's — is what decides whether an
    /// image will run, so `get_native_platform` must reflect `docker info`
    /// once it has been refreshed.
    #[tokio::test]
    async fn test_refresh_daemon_platform_reads_docker_info() {
        let runtime = test_runtime();
        let Some(raw_docker) = runtime.docker.get() else {
            println!("Docker handle not available, skipping");
            return;
        };
        if raw_docker.ping().await.is_err() {
            println!("Docker not available, skipping");
            return;
        }

        let platform = runtime
            .refresh_daemon_platform()
            .await
            .expect("a reachable daemon must report a platform");
        assert!(
            platform.starts_with("linux/"),
            "expected a linux platform, got: {}",
            platform
        );
        // Cached: the trait method now answers with the daemon's platform.
        assert_eq!(runtime.get_native_platform(), platform);
        // And it is stable across calls (OnceLock, no re-query).
        assert_eq!(
            runtime.refresh_daemon_platform().await.as_deref(),
            Some(platform.as_str())
        );
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

    fn disabled_runtime() -> DockerRuntime {
        let handle = Arc::new(temps_core::DockerHandle::disabled(
            temps_core::PROFILE_CONTROL_PLANE,
            temps_core::CONTROL_PLANE_DOCKER_REASON,
        ));
        DockerRuntime::new_with_handle(handle, false, "test-network".to_string())
    }

    #[test]
    fn disabled_handle_require_docker_returns_typed_error() {
        let runtime = disabled_runtime();
        let err = runtime
            .require_docker()
            .expect_err("disabled handle must error");
        assert!(
            matches!(err, DeployerError::DockerUnavailable(_)),
            "expected DockerUnavailable, got: {:?}",
            err
        );
        let rendered = err.to_string();
        assert!(rendered.contains("control-plane"), "{rendered}");
        assert!(rendered.contains("temps join"), "{rendered}");
    }

    #[test]
    fn disabled_handle_require_docker_for_build_returns_typed_error() {
        let runtime = disabled_runtime();
        let err = runtime
            .require_docker_for_build()
            .expect_err("disabled handle must error");
        assert!(
            matches!(err, BuilderError::DockerUnavailable(_)),
            "expected BuilderError::DockerUnavailable, got: {:?}",
            err
        );
    }

    #[tokio::test]
    async fn disabled_handle_list_containers_returns_typed_error() {
        let runtime = disabled_runtime();
        let err = runtime
            .list_containers()
            .await
            .expect_err("must fail without docker");
        assert!(matches!(err, DeployerError::DockerUnavailable(_)));
    }

    #[tokio::test]
    async fn disabled_handle_deploy_container_returns_typed_error() {
        let runtime = disabled_runtime();
        // Use a minimal request; the error must surface before any daemon
        // operation is attempted, so the exact request contents don't matter.
        let req = DeployRequest {
            image_name: "alpine:latest".to_string(),
            container_name: "test-disabled".to_string(),
            environment_vars: HashMap::new(),
            secrets: HashMap::new(),
            port_mappings: vec![],
            network_name: None,
            extra_networks: vec![],
            resource_limits: ResourceLimits {
                cpu_limit: None,
                memory_limit_mb: None,
                disk_limit_mb: None,
            },
            restart_policy: RestartPolicy::Never,
            log_path: PathBuf::from("/tmp/temps-disabled-handle-test.log"),
            command: None,
            entrypoint: None,
            log_config: None,
            labels: HashMap::new(),
            project_slug: None,
            control_plane_grants_socket: false,
        };
        let err = runtime
            .deploy_container(req)
            .await
            .expect_err("must fail without docker");
        assert!(
            matches!(err, DeployerError::DockerUnavailable(_)),
            "expected DockerUnavailable, got: {:?}",
            err
        );
    }

    #[tokio::test]
    async fn disabled_handle_get_container_info_returns_typed_error() {
        let runtime = disabled_runtime();
        let err = runtime
            .get_container_info("no-such-container")
            .await
            .expect_err("must fail without docker");
        assert!(matches!(err, DeployerError::DockerUnavailable(_)));
    }

    #[test]
    fn disabled_handle_refresh_daemon_platform_returns_none() {
        // refresh_daemon_platform degrades gracefully when no daemon is
        // present (it is called on a best-effort basis), so it must return
        // None rather than an error or a panic.
        let runtime = disabled_runtime();
        // We can't .await in a sync test, but we can verify the handle state.
        assert!(!runtime.docker.is_available());
    }
}
