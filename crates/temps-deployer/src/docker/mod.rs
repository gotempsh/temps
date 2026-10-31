// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Docker implementation of the `ImageBuilder`, `ContainerDeployer` and
//! `ContainerRuntime` traits.
//!
//! [`DockerRuntime`] is defined here; its behaviour is split by concern:
//!
//! - `runtime`: construction, configuration and the `ContainerRuntime` impl
//! - `network`: primary, overlay and operator-required networks
//! - `image_builder`: the `ImageBuilder` impl and build-context packing
//! - `build_limits`: build concurrency and per-build resource caps
//! - `build_failure`: classifying failed builds (OOM attribution)
//! - `deployer`: the `ContainerDeployer` impl and container inspection
//! - `static_archive`: bounded extraction of static files from images
//! - `log_tail`: bounded container log capture
//! - `guards`: drop guards that clean up probe execs and helper containers

mod build_failure;
mod build_limits;
mod deployer;
mod guards;
mod image_builder;
mod log_tail;
mod network;
mod runtime;
mod static_archive;
#[cfg(test)]
mod test_support;

pub use build_limits::BuildResourceLimits;
pub(crate) use static_archive::unpack_static_archive_stream;

use build_limits::LiveBuildLimits;
use network::OverlayNetwork;
use std::path::PathBuf;
use std::sync::Arc;
use temps_core::docker_socket_grant::DockerSocketGrant;
use temps_core::DockerHandle;

/// Appends the host's own DNS servers to the cluster resolver, so it is never
/// a container's only nameserver. Shared by the app-container path
/// ([`DockerRuntime::dns_for_container`]), the workload plane and managed
/// services; lives in `temps-node-dns` next to the resolver's own
/// upstream selection.
pub use temps_node_dns::host_dns::dns_with_fallback;

/// The one bind a project granted host Docker access receives, or `None`.
///
/// Pure so the decision — the single most security-relevant branch in the
/// deployer — is unit-testable without a daemon. An absent `project_slug`
/// (a pre-ADR-045 caller) can never match, and an empty grant (every install
/// that never set the variable) can never match either.
///
/// **Both ends must agree (ADR 045).** `grant` is this process's own
/// environment — the host's decision to provide the socket — and
/// `control_plane_grants_socket` is the control plane's decision that the
/// project requires it, carried in the `DeployRequest`. Neither alone is
/// enough:
///
/// - Without the host's grant, a control plane (or anything that can forge a
///   request to this agent) could turn any container root-equivalent here.
///   This was always enforced and still is.
/// - Without the control plane's declaration, a worker whose operator set the
///   variable for some slug would mount the socket for *whoever* manages to
///   get a project of that name scheduled onto it — and because the control
///   plane never declared the slug, neither the admin-only claim guard nor the
///   placement gate would have applied. That is the hole this parameter
///   closes.
///
/// For local placement the two are the same process's grant, so this is a
/// no-op there: `declares()` delegates to `allows()`.
pub fn docker_socket_bind_for(
    grant: &DockerSocketGrant,
    project_slug: Option<&str>,
    control_plane_grants_socket: bool,
) -> Option<&'static str> {
    if !control_plane_grants_socket {
        return None;
    }
    project_slug
        .filter(|slug| grant.allows(slug))
        .map(|_| temps_core::docker_socket_grant::DOCKER_SOCKET_BIND)
}

/// The security-hardening half of every application container's `HostConfig`.
///
/// Split out from the single build site so the invariant that matters can be
/// asserted in a test: adding the Docker socket bind does **not** relax
/// `cap_drop: ALL`, `no-new-privileges`, the PID limit or the init process.
/// The socket is one extra file descriptor, not a privileged container.
///
/// Binds are collected rather than assigned so adding a second bind here can
/// never silently drop the secrets mount.
pub fn hardened_host_config(
    secrets_bind: Option<String>,
    docker_socket_bind: Option<&str>,
) -> bollard::models::HostConfig {
    // ADR-055 phase 1: the hardening lives with the Docker driver, which
    // every workload container is created through.
    temps_workload_docker::hardened_host_config(
        secrets_bind
            .into_iter()
            .chain(docker_socket_bind.map(str::to_string))
            .collect(),
    )
}

/// Host address published container ports bind to, shared with whoever may
/// move it at runtime (the agent's `network_sync` loop moves a node that
/// joined with a public address onto its WireGuard mesh address). Read at
/// container-creation time, so every container created after a move
/// publishes on the new address.
pub type SharedHostBindAddress = Arc<std::sync::RwLock<String>>;

/// Where published ports go when the configured bind address is unusable.
const FALLBACK_HOST_BIND_ADDRESS: &str = "127.0.0.1";

pub struct DockerRuntime {
    /// The process-wide Docker client, which may be unavailable on a
    /// control-plane node that has no local daemon. All operations that
    /// require the daemon call [`Self::require_docker`] or
    /// [`Self::require_docker_for_build`] as late as possible.
    docker: Arc<DockerHandle>,
    use_buildkit: bool,
    network_name: String,
    /// Address to bind host ports to: "127.0.0.1" for the control plane's
    /// own local containers, or a worker agent's private/overlay address
    /// (never "0.0.0.0" — see [`Self::with_host_bind_address`]). Shared via
    /// [`Self::with_host_bind_slot`] when the address can move at runtime;
    /// always read through [`Self::current_host_bind_address`].
    host_bind_address: SharedHostBindAddress,
    /// Optional secondary network for multi-host overlay (e.g. "temps-overlay").
    /// When set, every container is additionally connected to this network
    /// after creation. Skipped silently when the network doesn't exist —
    /// that's the legitimate "overlay not yet bootstrapped on this node"
    /// state, not an error. Set via [`Self::with_overlay_network`] or
    /// [`Self::with_current_overlay_network`]; read through
    /// [`Self::overlay_network_name`].
    overlay_network: Option<OverlayNetwork>,
    /// Operator-level dependency networks that every app container must join
    /// before start. Unlike the optional overlay network, these are required:
    /// missing or failing attachments fail the deploy because the app is
    /// expected to depend on DNS/services from these networks.
    extra_networks: Vec<String>,
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
    /// Projects this host grants `/var/run/docker.sock` to (ADR 045).
    ///
    /// Read once from this process's own environment at startup and injected
    /// via [`Self::with_docker_socket_grant`]; empty by default, which is the
    /// behaviour of every install that never sets the variable. The comparison
    /// happens here, in the process that creates the container, so a control
    /// plane can never talk a worker into mounting the socket.
    docker_socket_grant: DockerSocketGrant,
    /// Build concurrency cap and per-build resource caps. Set via
    /// [`Self::with_build_limits`] on the control plane to prevent N
    /// simultaneous deploys from each grabbing 50% of host CPU/RAM and
    /// taking down the box, and kept current by
    /// [`Self::spawn_build_limits_watcher`] when the operator changes the
    /// settings. Unset on worker nodes and in tests — they get the legacy
    /// unbounded behaviour.
    build_limits: Arc<LiveBuildLimits>,
    /// Builds started on this runtime so far, so a build can tell how many
    /// others started while it ran.
    builds_started: Arc<std::sync::atomic::AtomicU64>,
    /// Whether the daemon runs on this machine (unset or `unix://`
    /// `DOCKER_HOST`, the same convention bollard connects with). Host-level
    /// facts such as `/proc/vmstat` and total RAM describe the build host only
    /// when this is true.
    daemon_is_local: bool,
    /// Platform of the Docker *daemon* this runtime talks to, cached after the
    /// first `docker info`. This is deliberately not the platform of the
    /// binary: with `DOCKER_HOST` set (or a QEMU-emulated `docker:dind`), the
    /// daemon can be a different architecture than the process, and what
    /// decides whether an image will run is the daemon's. Populated by
    /// [`Self::refresh_daemon_platform`]; until then `get_native_platform`
    /// falls back to the compiled-in architecture.
    daemon_platform: Arc<std::sync::OnceLock<String>>,
}

#[cfg(test)]
mod tests {
    use super::*;

    /// ADR 045: the grant is evaluated here, by the process that builds the
    /// container, and it adds exactly one bind without relaxing anything else.
    mod docker_socket_grant {
        use super::*;
        use temps_core::docker_socket_grant::{DockerSocketGrant, DOCKER_SOCKET_BIND};

        fn binds_of(config: &bollard::models::HostConfig) -> Vec<String> {
            config.binds.clone().unwrap_or_default()
        }

        fn assert_still_hardened(config: &bollard::models::HostConfig) {
            assert_eq!(config.cap_drop, Some(vec!["ALL".to_string()]));
            assert_eq!(
                config.security_opt,
                Some(vec!["no-new-privileges:true".to_string()])
            );
            assert_eq!(config.pids_limit, Some(512));
            assert_eq!(config.init, Some(true));
            assert_ne!(config.privileged, Some(true));
        }

        #[test]
        fn granted_slug_gets_the_socket_bind() {
            let grant = DockerSocketGrant::parse(Some("node-daemon,infra-agent"));
            let bind = docker_socket_bind_for(&grant, Some("node-daemon"), true);
            assert_eq!(bind, Some(DOCKER_SOCKET_BIND));

            let config = hardened_host_config(None, bind);
            assert_eq!(binds_of(&config), vec![DOCKER_SOCKET_BIND.to_string()]);
            assert_still_hardened(&config);
        }

        #[test]
        fn granted_slug_keeps_the_secrets_bind_alongside_the_socket() {
            let grant = DockerSocketGrant::parse(Some("node-daemon"));
            let bind = docker_socket_bind_for(&grant, Some("node-daemon"), true);
            let config = hardened_host_config(
                Some("/var/lib/temps/secrets/c:/run/secrets:ro".into()),
                bind,
            );

            let binds = binds_of(&config);
            assert!(binds.contains(&"/var/lib/temps/secrets/c:/run/secrets:ro".to_string()));
            assert!(binds.contains(&DOCKER_SOCKET_BIND.to_string()));
            assert_eq!(binds.len(), 2);
            assert_still_hardened(&config);
        }

        #[test]
        fn ungranted_slug_gets_no_socket_bind() {
            let grant = DockerSocketGrant::parse(Some("node-daemon"));
            assert_eq!(
                docker_socket_bind_for(&grant, Some("infra-agent"), true),
                None
            );

            let config = hardened_host_config(None, None);
            assert!(config.binds.is_none());
            assert_still_hardened(&config);
        }

        #[test]
        fn absent_slug_gets_no_socket_bind_even_when_the_host_grants_something() {
            // A pre-ADR-045 control plane sends no slug at all. It must never
            // be interpreted as "any project".
            let grant = DockerSocketGrant::parse(Some("node-daemon"));
            assert_eq!(docker_socket_bind_for(&grant, None, true), None);
        }

        #[test]
        fn a_host_that_grants_nothing_never_mounts_the_socket() {
            let grant = DockerSocketGrant::default();
            assert_eq!(
                docker_socket_bind_for(&grant, Some("node-daemon"), true),
                None
            );
            assert!(hardened_host_config(None, None).binds.is_none());
        }

        #[test]
        fn a_host_grant_alone_never_mounts_without_the_control_plane_declaration() {
            // The finding this parameter exists for: an operator sets
            // TEMPS_DOCKER_SOCKET_PROJECTS on a worker, the control plane does
            // not declare the slug (removed, forgotten, never set), so the
            // admin-only claim guard and the placement gate are both inert —
            // anyone can create a project with that name and have it land
            // here. The worker must refuse to mount from its own environment
            // alone.
            let grant = DockerSocketGrant::parse(Some("node-daemon"));
            assert_eq!(
                docker_socket_bind_for(&grant, Some("node-daemon"), false),
                None
            );
            assert!(hardened_host_config(None, None).binds.is_none());
        }

        #[test]
        fn the_control_plane_declaration_alone_never_mounts_either() {
            // The symmetric half, which was always true and must stay true:
            // authorization is not instruction. A control plane (or anything
            // that can forge a request to this agent) cannot make this host
            // mount a socket its own environment does not grant.
            let grant = DockerSocketGrant::default();
            assert_eq!(
                docker_socket_bind_for(&grant, Some("node-daemon"), true),
                None
            );
        }

        #[test]
        fn both_halves_together_mount_exactly_one_bind() {
            let grant = DockerSocketGrant::parse(Some("node-daemon"));
            let bind = docker_socket_bind_for(&grant, Some("node-daemon"), true);
            assert_eq!(bind, Some(DOCKER_SOCKET_BIND));
            assert_eq!(
                binds_of(&hardened_host_config(None, bind)),
                vec![DOCKER_SOCKET_BIND.to_string()]
            );
        }

        #[test]
        fn a_request_that_lost_the_authorization_field_fails_closed() {
            // `#[serde(default)]` is `false`: a pre-ADR-045 control plane, or
            // a request whose field was dropped in transit, must deploy
            // without the socket rather than with it.
            let deserialized: serde_json::Value = serde_json::json!({});
            assert!(!deserialized
                .get("control_plane_grants_socket")
                .and_then(serde_json::Value::as_bool)
                .unwrap_or(false));
            let grant = DockerSocketGrant::parse(Some("node-daemon"));
            assert_eq!(
                docker_socket_bind_for(&grant, Some("node-daemon"), false),
                None
            );
        }

        #[test]
        fn runtime_defaults_to_granting_nothing() {
            let runtime = DockerRuntime::new_with_handle(
                Arc::new(DockerHandle::disabled(
                    temps_core::PROFILE_CONTROL_PLANE,
                    temps_core::CONTROL_PLANE_DOCKER_REASON,
                )),
                false,
                "temps".to_string(),
            );
            assert!(runtime.docker_socket_grant().is_empty());

            let runtime =
                runtime.with_docker_socket_grant(DockerSocketGrant::parse(Some("node-daemon")));
            assert!(runtime.docker_socket_grant().allows("node-daemon"));
        }
    }
}
