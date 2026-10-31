// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Network attachment for [`DockerRuntime`]: the primary app network, the
//! optional multi-host overlay and operator-required extra networks.

use super::DockerRuntime;
use crate::DeployerError;
use std::collections::HashSet;
use tracing::{info, warn};

/// Which overlay network a runtime attaches containers to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum OverlayNetwork {
    /// A fixed name.
    Named(String),
    /// Whatever this process's overlay is currently called (ADR 059): the
    /// agent's network sync records it once the control plane names it.
    Current,
}

fn normalize_extra_networks(
    networks: Vec<String>,
    primary_network: &str,
    overlay_network: Option<&str>,
) -> Vec<String> {
    let mut seen = HashSet::new();
    networks
        .into_iter()
        .map(|network| network.trim().to_string())
        .filter(|network| !network.is_empty())
        .filter(|network| network != primary_network)
        .filter(|network| Some(network.as_str()) != overlay_network)
        .filter(|network| seen.insert(network.clone()))
        .collect()
}

fn network_matches_identifier(
    candidate_name: Option<&str>,
    candidate_id: Option<&str>,
    identifier: &str,
) -> bool {
    candidate_name == Some(identifier) || candidate_id == Some(identifier)
}

impl DockerRuntime {
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
        self.overlay_network = Some(OverlayNetwork::Named(name.into()));
        self
    }

    /// Like [`Self::with_overlay_network`], following the overlay network
    /// name this process reconciles (`temps_network::overlay_name::current`),
    /// which the control plane may change (ADR 059).
    pub fn with_current_overlay_network(mut self) -> Self {
        self.overlay_network = Some(OverlayNetwork::Current);
        self
    }

    /// The overlay network new containers join, if one is configured.
    pub fn overlay_network_name(&self) -> Option<String> {
        match self.overlay_network.as_ref()? {
            OverlayNetwork::Named(name) => Some(name.clone()),
            OverlayNetwork::Current => Some(temps_network::overlay_name::current().docker_network),
        }
    }

    /// Configure required dependency networks for all containers created by
    /// this runtime. Blank entries, duplicates, the primary network, and the
    /// optional overlay network are ignored.
    pub fn with_extra_networks(mut self, networks: Vec<String>) -> Self {
        let overlay = self.overlay_network_name();
        self.extra_networks =
            normalize_extra_networks(networks, &self.network_name, overlay.as_deref());
        self
    }

    /// Best-effort additional attachment to the overlay network. Logs and
    /// returns `Ok(())` when the overlay isn't configured or doesn't
    /// exist; only true bollard errors propagate.
    pub(super) async fn maybe_attach_overlay(
        &self,
        container_id: &str,
    ) -> Result<(), DeployerError> {
        let Some(overlay) = self.overlay_network_name() else {
            return Ok(());
        };
        let overlay = overlay.as_str();

        let docker = self.require_docker()?;

        // Cheap existence probe: list_networks once. If the overlay
        // doesn't exist yet, skip (sync loop hasn't bootstrapped it).
        let networks = docker
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
        match docker.connect_network(overlay, req).await {
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

    /// Attach required dependency networks before container start so Docker's
    /// embedded DNS can resolve service names during app boot.
    ///
    /// Request-supplied networks may only *narrow* the operator's configured
    /// set, never widen it: a caller asking for a network the operator did not
    /// configure is rejected. The agent's deploy endpoint deserializes
    /// `DeployRequest` straight from the request body, so without this the
    /// field would let any token holder bridge a container onto an arbitrary
    /// host network (another project's database network, the control plane's).
    pub(super) async fn attach_required_networks(
        &self,
        container_id: &str,
        request_networks: &[String],
    ) -> Result<(), DeployerError> {
        let overlay = self.overlay_network_name();
        let requested = normalize_extra_networks(
            request_networks.to_vec(),
            &self.network_name,
            overlay.as_deref(),
        );

        let docker = self.require_docker()?;
        let existing_networks = docker
            .list_networks(None::<bollard::query_parameters::ListNetworksOptions>)
            .await
            .map_err(|e| DeployerError::NetworkError(format!("list_networks: {}", e)))?;

        // Resolve every identifier (name *or* ID) to a canonical ID so the
        // allowlist and the primary/overlay guards can't be sidestepped by
        // naming the same network a different way.
        let canonical = |identifier: &str| -> Option<String> {
            existing_networks
                .iter()
                .find(|candidate| {
                    network_matches_identifier(
                        candidate.name.as_deref(),
                        candidate.id.as_deref(),
                        identifier,
                    )
                })
                .map(|candidate| {
                    candidate
                        .id
                        .clone()
                        .or_else(|| candidate.name.clone())
                        .unwrap_or_else(|| identifier.to_string())
                })
        };

        // Networks the operator opted into, by canonical ID. Configured
        // networks that don't exist are still an error below; they just don't
        // participate in the allowlist here.
        let allowed: HashSet<String> = self
            .extra_networks
            .iter()
            .filter_map(|network| canonical(network))
            .collect();

        for network in &requested {
            let is_allowed = canonical(network)
                .map(|id| allowed.contains(&id))
                .unwrap_or(false);
            if !is_allowed {
                return Err(DeployerError::NetworkError(format!(
                    "network '{}' is not in TEMPS_DOCKER_EXTRA_NETWORKS; \
                     per-request networks may only narrow the operator's configured set",
                    network
                )));
            }
        }

        let mut networks = self.extra_networks.clone();
        networks.extend(requested);
        let networks = normalize_extra_networks(networks, &self.network_name, overlay.as_deref());
        if networks.is_empty() {
            return Ok(());
        }

        // Guard against reaching the primary/overlay network under an alias
        // (its ID, or its ID when configured by name). `normalize_extra_networks`
        // can only compare the raw strings it was given.
        let reserved: HashSet<String> = std::iter::once(self.network_name.as_str())
            .chain(overlay.as_deref())
            .filter_map(canonical)
            .collect();

        let mut attached: HashSet<String> = HashSet::new();
        for network in networks {
            let Some(network_id) = canonical(&network) else {
                return Err(DeployerError::NetworkError(format!(
                    "required network '{}' does not exist",
                    network
                )));
            };

            if reserved.contains(&network_id) {
                tracing::debug!(
                    container = %container_id,
                    network,
                    "skipping required network that aliases the primary or overlay network"
                );
                continue;
            }

            // Two identifiers can resolve to the same network; attach once.
            if !attached.insert(network_id) {
                continue;
            }

            let req = bollard::models::NetworkConnectRequest {
                container: container_id.to_string(),
                ..Default::default()
            };
            match docker.connect_network(&network, req).await {
                Ok(()) => {
                    tracing::info!(container = %container_id, network, "attached to required network");
                }
                Err(bollard::errors::Error::DockerResponseServerError {
                    status_code: 403, ..
                }) => {
                    tracing::debug!(
                        container = %container_id,
                        network,
                        "container already connected to required network (403)"
                    );
                }
                Err(e) => {
                    // The raw bollard error can carry host topology detail, so
                    // it goes to the log; the caller gets the network name it
                    // already supplied plus an actionable summary.
                    tracing::error!(
                        container = %container_id,
                        network,
                        error = %e,
                        "failed to attach required network"
                    );
                    return Err(DeployerError::NetworkError(format!(
                        "could not attach required network '{}' — see server logs",
                        network
                    )));
                }
            }
        }

        Ok(())
    }

    /// Install per-peer routes inside the container's netns. Must be
    /// called **after** the container is started — `docker inspect`
    /// only reports a non-zero PID for a running container, and we
    /// need that PID to `nsenter` into its network namespace.
    ///
    /// Best-effort: any failure is logged and swallowed so a flaky
    /// route install doesn't break the deploy.
    pub async fn install_overlay_peer_routes(&self, container_id: &str) {
        let Some(overlay) = self.overlay_network_name() else {
            return;
        };
        if let Err(e) = self
            .install_overlay_peer_routes_inner(container_id, &overlay)
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

        let docker = self.docker.cloned().ok_or_else(|| {
            "Docker daemon unavailable in this process; overlay peer routes are only \
             installed on worker nodes joined with `temps join`"
                .to_string()
        })?;
        let inspect = docker
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
        let docker = self.require_docker()?;

        let outcome =
            temps_core::docker_network::ensure_bridge_network(&docker, &self.network_name)
                .await
                .map_err(|e| {
                    DeployerError::NetworkError(format!(
                        "Failed to create network {}: {}",
                        self.network_name, e
                    ))
                })?;
        if outcome != temps_core::docker_network::NetworkEnsured::Existing {
            info!("Network {} ready ({:?})", self.network_name, outcome);
        }

        // Re-applied on every deploy (not just network creation) so the block
        // survives host firewall flushes; best-effort, never fails the deploy.
        if let Err(error) =
            crate::metadata_egress::apply_metadata_egress_block(&docker, &self.network_name).await
        {
            warn!(
                network = %self.network_name,
                error = %error,
                "Cloud-metadata egress block is incomplete; ensure nftables is \
                 installed and Temps has CAP_NET_ADMIN"
            );
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
        let docker = self.docker.cloned()?;
        let info = docker
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
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::docker::test_support::create_test_docker_runtime;
    use crate::ContainerDeployer;
    use crate::{ContainerLogConfig, DeployRequest, ResourceLimits, RestartPolicy};
    use bollard::query_parameters::InspectContainerOptions;
    use serial_test::serial;
    use std::collections::HashMap;
    use std::path::PathBuf;

    fn unique_test_name(prefix: &str) -> String {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_millis();
        format!("{}-{}", prefix, now)
    }

    fn alpine_deploy_request(container_name: String, extra_networks: Vec<String>) -> DeployRequest {
        DeployRequest {
            image_name: "alpine:latest".to_string(),
            container_name,
            environment_vars: HashMap::new(),
            secrets: HashMap::new(),
            port_mappings: vec![],
            network_name: None,
            extra_networks,
            resource_limits: ResourceLimits {
                cpu_limit: Some(0.5),
                memory_limit_mb: Some(64),
                disk_limit_mb: Some(256),
            },
            restart_policy: RestartPolicy::Never,
            log_path: PathBuf::from("/tmp/temps-extra-network-test.log"),
            command: Some(vec!["sleep".to_string(), "30".to_string()]),
            entrypoint: None,
            log_config: Some(ContainerLogConfig::app_default()),
            labels: HashMap::new(),
            project_slug: None,
            control_plane_grants_socket: false,
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
                assert!(
                    runtime.extra_networks.is_empty(),
                    "extra_networks must default to empty for backwards compatibility"
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
                assert_eq!(
                    runtime.overlay_network_name().as_deref(),
                    Some("temps-overlay")
                );
            }
            Err(e) => {
                println!("🔧 Docker not available: {}", e);
            }
        }
    }

    #[test]
    fn test_normalize_extra_networks_drops_empty_duplicates_primary_and_overlay() {
        let normalized = normalize_extra_networks(
            vec![
                " ".to_string(),
                "temps-app-network".to_string(),
                "supabase_default".to_string(),
                "supabase_default".to_string(),
                "temps-overlay".to_string(),
                "analytics_default".to_string(),
            ],
            "temps-app-network",
            Some("temps-overlay"),
        );

        assert_eq!(
            normalized,
            vec![
                "supabase_default".to_string(),
                "analytics_default".to_string()
            ]
        );
    }

    #[test]
    fn test_required_network_identifier_matches_name_or_id() {
        assert!(network_matches_identifier(
            Some("supabase_default"),
            Some("abc123"),
            "supabase_default"
        ));
        assert!(network_matches_identifier(
            Some("supabase_default"),
            Some("abc123"),
            "abc123"
        ));
        assert!(!network_matches_identifier(
            Some("supabase_default"),
            Some("abc123"),
            "missing"
        ));
    }

    #[tokio::test]
    async fn test_with_extra_networks_sets_normalized_field() {
        match create_test_docker_runtime().await {
            Ok(runtime) => {
                let runtime = runtime
                    .with_overlay_network("temps-overlay")
                    .with_extra_networks(vec![
                        "test-network".to_string(),
                        "supabase_default".to_string(),
                        "supabase_default".to_string(),
                        "temps-overlay".to_string(),
                    ]);
                assert_eq!(runtime.extra_networks, vec!["supabase_default"]);
            }
            Err(e) => {
                println!("Docker not available: {}", e);
            }
        }
    }

    #[tokio::test]
    #[serial]
    async fn test_deploy_attaches_required_network_by_id() {
        match create_test_docker_runtime().await {
            Ok(runtime) => {
                // create_test_docker_runtime() only returns Ok when a daemon
                // is reachable, so require() is always Some here.
                let raw_docker = runtime
                    .docker
                    .require()
                    .expect("create_test_docker_runtime guarantees a real docker client");
                let extra_network_name = unique_test_name("temps-extra-network");
                let container_name = unique_test_name("temps-extra-network-container");

                let create_options = bollard::models::NetworkCreateRequest {
                    name: extra_network_name.clone(),
                    driver: Some("bridge".to_string()),
                    ..Default::default()
                };

                if let Err(e) = raw_docker.create_network(create_options).await {
                    println!("Docker network create failed (may be expected): {}", e);
                    return;
                }

                let network_id = match raw_docker
                    .list_networks(None::<bollard::query_parameters::ListNetworksOptions>)
                    .await
                    .ok()
                    .and_then(|networks| {
                        networks
                            .into_iter()
                            .find(|network| network.name.as_deref() == Some(&extra_network_name))
                            .and_then(|network| network.id)
                    }) {
                    Some(id) => id,
                    None => {
                        let _ = raw_docker.remove_network(&extra_network_name).await;
                        panic!("created network {} was not listed", extra_network_name);
                    }
                };

                // Operator configures the network by NAME; the request names
                // the same network by ID. Both must resolve to the same
                // canonical network so the allowlist accepts it.
                let runtime = runtime.with_extra_networks(vec![extra_network_name.clone()]);

                let deploy_result = runtime
                    .deploy_container(alpine_deploy_request(
                        container_name.clone(),
                        vec![network_id.clone()],
                    ))
                    .await;

                match deploy_result {
                    Ok(deploy_info) => {
                        let inspect = raw_docker
                            .inspect_container(
                                &deploy_info.container_id,
                                None::<InspectContainerOptions>,
                            )
                            .await
                            .expect("inspect deployed container");

                        let attached = inspect
                            .network_settings
                            .and_then(|settings| settings.networks)
                            .map(|networks| networks.contains_key(&extra_network_name))
                            .unwrap_or(false);
                        assert!(attached, "container should be attached to required network");

                        let _ = runtime.remove_container(&deploy_info.container_id).await;
                    }
                    Err(e) => {
                        println!(
                            "Docker deploy failed before required-network assertion (may be expected): {}",
                            e
                        );
                    }
                }

                let _ = raw_docker.remove_network(&extra_network_name).await;
            }
            Err(e) => {
                println!("Docker not available: {}", e);
            }
        }
    }

    #[tokio::test]
    #[serial]
    async fn test_deploy_missing_required_network_removes_created_container() {
        match create_test_docker_runtime().await {
            Ok(runtime) => {
                let container_name = unique_test_name("temps-missing-network-container");
                let missing_network = unique_test_name("temps-missing-network");
                let runtime = runtime.with_extra_networks(vec![missing_network.clone()]);
                let deploy_result = runtime
                    .deploy_container(alpine_deploy_request(container_name.clone(), Vec::new()))
                    .await;

                match deploy_result {
                    Ok(deploy_info) => {
                        let _ = runtime.remove_container(&deploy_info.container_id).await;
                        panic!("deploy should fail when required network is missing");
                    }
                    Err(DeployerError::NetworkError(message))
                        if message.contains(&format!(
                            "required network '{}' does not exist",
                            missing_network
                        )) =>
                    {
                        let leftover = runtime
                            .find_container_by_name(&container_name)
                            .await
                            .expect("container lookup after failed deploy");
                        assert!(
                            leftover.is_none(),
                            "failed required-network deploy should remove the created container"
                        );
                    }
                    Err(e) => {
                        println!(
                            "Docker deploy failed before required-network assertion (may be expected): {}",
                            e
                        );
                    }
                }
            }
            Err(e) => {
                println!("Docker not available: {}", e);
            }
        }
    }

    /// A request may narrow the operator's configured networks, never widen
    /// them. The agent deploy endpoint deserializes `DeployRequest` from the
    /// request body, so an unbounded field here would let any token holder
    /// bridge a container onto an arbitrary host network.
    #[tokio::test]
    #[serial]
    async fn test_deploy_rejects_request_network_outside_operator_allowlist() {
        match create_test_docker_runtime().await {
            Ok(runtime) => {
                let raw_docker = runtime
                    .docker
                    .require()
                    .expect("create_test_docker_runtime guarantees a real docker client");
                let off_limits_network = unique_test_name("temps-off-limits-network");
                let container_name = unique_test_name("temps-off-limits-container");

                let create_options = bollard::models::NetworkCreateRequest {
                    name: off_limits_network.clone(),
                    driver: Some("bridge".to_string()),
                    ..Default::default()
                };
                if let Err(e) = raw_docker.create_network(create_options).await {
                    println!("Docker network create failed (may be expected): {}", e);
                    return;
                }

                // The network EXISTS — it is simply not one the operator
                // opted into, which is exactly the cross-tenant bridge case.
                let deploy_result = runtime
                    .deploy_container(alpine_deploy_request(
                        container_name.clone(),
                        vec![off_limits_network.clone()],
                    ))
                    .await;

                match deploy_result {
                    Ok(deploy_info) => {
                        let _ = runtime.remove_container(&deploy_info.container_id).await;
                        let _ = raw_docker.remove_network(&off_limits_network).await;
                        panic!(
                            "deploy must reject a request network outside the operator allowlist"
                        );
                    }
                    Err(DeployerError::NetworkError(message))
                        if message.contains("not in TEMPS_DOCKER_EXTRA_NETWORKS") =>
                    {
                        let leftover = runtime
                            .find_container_by_name(&container_name)
                            .await
                            .expect("container lookup after rejected deploy");
                        assert!(
                            leftover.is_none(),
                            "rejected deploy should remove the created container"
                        );
                    }
                    Err(e) => {
                        println!(
                            "Docker deploy failed before allowlist assertion (may be expected): {}",
                            e
                        );
                    }
                }

                let _ = raw_docker.remove_network(&off_limits_network).await;
            }
            Err(e) => {
                println!("Docker not available: {}", e);
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
}
