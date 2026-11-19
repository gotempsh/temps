// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Per-sandbox isolated networks: creating, reattaching, parking and
//! reaping them, and connecting the preview gateway.

use super::*;

pub(super) fn docker_network_matches_policy(
    network: &bollard::models::NetworkInspect,
    internal: bool,
    require_sandbox_data_label: bool,
) -> bool {
    let managed_sandbox_data_network = network
        .labels
        .as_ref()
        .and_then(|labels| labels.get("sh.temps.sandbox-data-network"))
        .is_some_and(|value| value == "true");
    network.enable_ipv6 == Some(false)
        && network.internal == Some(internal)
        && network.driver.as_deref() == Some("bridge")
        && (!require_sandbox_data_label || managed_sandbox_data_network)
}

/// Resolve every network-enabled product setting to the one managed sandbox
/// data-plane. Historical rows contain values such as `full`, `restricted`,
/// `host`, or the old bridge name. None of those may become a literal Docker
/// network: doing so would either fail startup or silently restore unfiltered
/// egress. `none` remains the explicit no-network mode.
pub(super) fn managed_sandbox_network(network_mode: &str, container_name: &str) -> String {
    if network_mode == "none" {
        "none".to_string()
    } else {
        sandbox_network_name(container_name)
    }
}

pub(super) fn effective_sandbox_network(
    provider_network_mode: &str,
    requested_network_mode: Option<&str>,
    container_name: &str,
) -> String {
    managed_sandbox_network(
        requested_network_mode.unwrap_or(provider_network_mode),
        container_name,
    )
}

pub fn sandbox_network_name(container_name: &str) -> String {
    format!("{SANDBOX_NETWORK_PREFIX}{container_name}")
}

pub(crate) fn is_managed_sandbox_network(name: &str) -> bool {
    name.starts_with(SANDBOX_NETWORK_PREFIX)
}

pub(super) fn is_managed_preview_gateway(
    container: &bollard::models::ContainerInspectResponse,
) -> bool {
    container
        .config
        .as_ref()
        .and_then(|config| config.labels.as_ref())
        .and_then(|labels| labels.get(crate::preview_gateway::PREVIEW_GATEWAY_LABEL))
        .is_some_and(|value| value == "true")
}

/// Grant service endpoints before reconnecting application compute. Keeping
/// the sandbox last makes a partial reconciliation fail closed: application
/// code cannot observe a half-applied data-plane topology.
pub(super) fn application_network_connection_order(
    sandbox_id: &str,
    service_containers: &[String],
) -> Vec<String> {
    let mut containers = Vec::with_capacity(service_containers.len() + 1);
    containers.extend(service_containers.iter().cloned());
    containers.push(sandbox_id.to_string());
    containers
}

/// Internal-only Docker network for workspace/agent sandboxes, the preview
/// gateway, and the managed egress proxy. The versioned name is intentional:
/// Docker cannot change an existing bridge from external to internal, and
/// reusing the legacy `temps-sandbox-net` would silently preserve direct
/// internet access on upgraded hosts.
///
/// Keep in sync with `preview_gateway::PREVIEW_GATEWAY_NETWORK`.
pub(super) const SANDBOX_NETWORK_PREFIX: &str = "temps-sandbox-net-v3-";
/// A sandbox network is only reaped as orphaned once it is at least this old.
/// `create` makes the network before the container, so a younger network
/// without a container may belong to a sandbox that is still being created.
pub(super) const ORPHANED_NETWORK_MIN_AGE: std::time::Duration =
    std::time::Duration::from_secs(10 * 60);
pub(super) const SANDBOX_NETWORK_OWNER_LABEL: &str = "sh.temps.sandbox-network-for";
pub const SANDBOX_PREVIEW_GATEWAY_LABEL: &str = "sh.temps.preview-gateway-for";

/// The sandbox a network belongs to, when the network is a candidate for
/// being reaped as orphaned: it carries the ownership label, its name is the
/// one this provider derives for that owner (so a user's own network that
/// happens to copy the label is left alone), it is labelled for this
/// instance's preview gateway (another Temps instance sharing this Docker
/// daemon may be recreating that sandbox, which this instance's bookkeeping
/// cannot see), and it is at least `min_age` old.
///
/// The gateway name is the instance scope: `DockerSandboxConfig` documents it
/// as the gateway "owned by this Temps instance", and the gateway reconciler
/// leaves a gateway container carrying another instance's secret alone, so
/// two instances on one daemon are expected to use different names. Two
/// instances configured with the *same* name are not separable by this
/// bookkeeping and must not both run automatic reaping against one daemon.
/// The reaper additionally never removes a network that has any workload
/// attached (`network_has_foreign_endpoints`), so even then a running sandbox
/// is not disconnected: removal detaches only the gateway
/// (`NetworkDetach::OnlyGateway`), so Docker refuses to remove a network a
/// workload has attached; at worst a create in its first seconds fails and is
/// retried.
///
/// Whether the owner's container still exists is checked by the caller.
pub(super) fn orphan_candidate_owner<'a>(
    network: &'a bollard::models::Network,
    gateway: &str,
    now_unix: i64,
    min_age: std::time::Duration,
) -> Option<&'a str> {
    let labels = network.labels.as_ref()?;
    // Positive ownership only: a network that predates the gateway label
    // cannot be attributed to this instance (another one on the same daemon
    // may have reused it as a legacy network), so it is left alone.
    if labels
        .get(SANDBOX_PREVIEW_GATEWAY_LABEL)
        .map(String::as_str)
        != Some(gateway)
    {
        return None;
    }
    let owner = labels.get(SANDBOX_NETWORK_OWNER_LABEL)?.as_str();
    if owner.is_empty() || network.name.as_deref() != Some(sandbox_network_name(owner).as_str()) {
        return None;
    }
    let age_secs = now_unix.checked_sub(network.created?.unix_timestamp())?;
    (u64::try_from(age_secs).ok()? >= min_age.as_secs()).then_some(owner)
}

/// A sandbox container that is not running and is not about to: its isolated
/// network can be released and recreated when it starts. A restarting
/// container counts as running, since Docker is bringing it back up.
pub(super) fn container_is_parked(info: &bollard::models::ContainerInspectResponse) -> bool {
    info.state.as_ref().is_some_and(|state| {
        state.running != Some(true) && state.restarting != Some(true) && state.paused != Some(true)
    })
}

/// Which endpoints [`DockerSandboxProvider::remove_isolated_sandbox_network_with`]
/// disconnects before removing the network.
#[derive(Clone, Copy)]
enum NetworkDetach<'a> {
    /// Every attached endpoint: the caller knows the sandbox is gone.
    Everything,
    /// Only the preview gateway with this container name; anything else is
    /// left attached so Docker refuses to remove a network a workload uses.
    OnlyGateway(&'a str),
}

/// What [`DockerSandboxProvider::remove_isolated_sandbox_network_with`] does
/// with the sandbox's egress proxy.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum EgressProxyDisposal {
    /// Remove it: the sandbox is gone.
    Remove,
    /// Stop it and keep it: the sandbox is only parked, and the proxy holds
    /// its egress policy until the sandbox starts again.
    Park,
}

/// Containers attached to `network` other than the sandbox's own egress proxy
/// and the preview gateway: anything else is a workload, so the network is in
/// use and must not be torn down by the reaper, which would disconnect it.
pub(super) fn network_has_foreign_endpoints(
    network: &bollard::models::NetworkInspect,
    owner: &str,
    gateway: &str,
) -> bool {
    let proxy = sandbox_egress_proxy_name(owner);
    network.containers.as_ref().is_some_and(|containers| {
        containers.values().any(|endpoint| {
            endpoint
                .name
                .as_deref()
                .is_none_or(|name| name != proxy && name != gateway)
        })
    })
}

pub(super) fn isolated_sandbox_network_request(
    name: &str,
    container_name: &str,
    preview_gateway_container_name: &str,
) -> bollard::models::NetworkCreateRequest {
    with_host_isolation(bollard::models::NetworkCreateRequest {
        name: name.to_string(),
        labels: Some(HashMap::from([
            (
                SANDBOX_EGRESS_POLICY_LABEL.to_string(),
                SANDBOX_EGRESS_POLICY_VERSION.to_string(),
            ),
            (
                SANDBOX_NETWORK_OWNER_LABEL.to_string(),
                container_name.to_string(),
            ),
            (
                SANDBOX_PREVIEW_GATEWAY_LABEL.to_string(),
                preview_gateway_container_name.to_string(),
            ),
        ])),
        ..Default::default()
    })
}

pub(super) fn sandbox_network_matches_isolation_policy(
    network: &bollard::models::NetworkInspect,
    container_name: &str,
    preview_gateway_container_name: &str,
    legacy_gateway_attached: bool,
) -> bool {
    let labels = network.labels.as_ref();
    docker_network_matches_policy(network, true, false)
        && labels
            .and_then(|labels| labels.get(SANDBOX_EGRESS_POLICY_LABEL))
            .is_some_and(|value| value == SANDBOX_EGRESS_POLICY_VERSION)
        && labels
            .and_then(|labels| labels.get(SANDBOX_NETWORK_OWNER_LABEL))
            .is_some_and(|value| value == container_name)
        && labels
            .and_then(|labels| labels.get(SANDBOX_PREVIEW_GATEWAY_LABEL))
            .map_or(
                // Before this ownership label existed, the product exposed
                // one singleton gateway with this exact default name. Adopt
                // those networks only for that historical singleton; custom
                // multi-instance gateways must have an explicit match.
                preview_gateway_container_name == crate::preview_gateway::PREVIEW_GATEWAY_CONTAINER
                    || legacy_gateway_attached,
                |value| value == preview_gateway_container_name,
            )
        && has_host_isolation(network.options.as_ref())
}

/// The preview gateway a sandbox network is labelled for, when that is not
/// `gateway`: the network predates a rename, or another instance on the same
/// Docker daemon set it up.
pub(super) fn network_labelled_for_another_gateway<'a>(
    network: &'a bollard::models::NetworkInspect,
    gateway: &str,
) -> Option<&'a str> {
    network
        .labels
        .as_ref()?
        .get(SANDBOX_PREVIEW_GATEWAY_LABEL)
        .map(String::as_str)
        .filter(|labelled| *labelled != gateway)
}

impl DockerSandboxProvider {
    /// Ensure a platform-independent, fail-closed sandbox network.
    ///
    /// Sandboxes and the preview gateway live on an internal Docker bridge with
    /// no default internet route. A hardened proxy is the only member that also
    /// joins an ordinary outbound bridge. This provides the same security
    /// boundary on native Linux, Docker Desktop, Colima, and remote daemons
    /// without asking an operator to install host firewall rules or enable an
    /// insecure environment-variable escape hatch.
    ///
    /// `egress_policy` is the policy the proxy must enforce; `None` keeps
    /// the one the sandbox's existing proxy carries (see
    /// [`Self::ensure_egress_proxy`]).
    pub(super) async fn ensure_network(
        &self,
        container_name: &str,
        sandbox_network: &str,
        control_plane_url: &str,
        egress_policy: Option<&SandboxEgressPolicy>,
    ) -> Result<(), AgentError> {
        if sandbox_network == "none" {
            return Ok(());
        }

        let _guard = self.network_lock.lock().await;
        self.ensure_network_locked(
            container_name,
            sandbox_network,
            control_plane_url,
            egress_policy,
        )
        .await
        .map(|_| ())
    }

    /// [`Self::ensure_network`] for a caller that already holds
    /// `network_lock`. Returns whether the egress proxy was (re)created.
    pub(super) async fn ensure_network_locked(
        &self,
        container_name: &str,
        sandbox_network: &str,
        control_plane_url: &str,
        egress_policy: Option<&SandboxEgressPolicy>,
    ) -> Result<bool, AgentError> {
        // Held until the gateway is attached, so a rename cannot land between
        // labelling this network with the gateway's name and attaching it.
        let gateway = self.config.preview_gateway_container_name.read().await;
        self.ensure_managed_bridge(SANDBOX_EGRESS_NETWORK, false)
            .await?;
        self.ensure_isolated_sandbox_bridge(sandbox_network, container_name, &gateway)
            .await?;
        let replaced = self
            .ensure_egress_proxy(
                container_name,
                sandbox_network,
                control_plane_url,
                egress_policy,
            )
            .await?;
        self.connect_preview_gateways(sandbox_network, &gateway)
            .await?;
        Ok(replaced)
    }

    /// Give a stopped sandbox's isolated network back to Docker.
    ///
    /// Every network pins a subnet from the daemon's finite address pools
    /// (about 30 with Docker's defaults), so stopped sandboxes holding theirs
    /// would exhaust the host long before it runs out of CPU or memory. The
    /// stopped container keeps its configuration, which still names the
    /// network, so the isolation policy check passes while it is parked;
    /// [`Self::prepare_network_for_start`] recreates the network and replaces
    /// the stale endpoint before it runs again. The egress proxy is stopped
    /// rather than removed: it carries the sandbox's egress policy, which
    /// the proxy recreated on start inherits. Best effort: a failure only
    /// leaves the network in place for the reaper.
    pub(super) async fn release_parked_sandbox_network(&self, container_name: &str) {
        let _guard = self.network_lock.lock().await;
        let gateway = self
            .config
            .preview_gateway_container_name
            .read()
            .await
            .clone();
        let errors = self
            .remove_isolated_sandbox_network_with(
                container_name,
                NetworkDetach::OnlyGateway(&gateway),
                EgressProxyDisposal::Park,
            )
            .await;
        if errors.is_empty() {
            tracing::info!(
                sandbox = container_name,
                "Released the isolated network of a stopped sandbox"
            );
        } else {
            tracing::warn!(
                sandbox = container_name,
                errors = %errors.join("; "),
                "Could not release the isolated network of a stopped sandbox; it stays reserved until the reaper frees it"
            );
        }
    }

    /// Make sure a sandbox about to (re)start has its isolated network, egress
    /// proxy and gateway, and that the container's endpoint points at the
    /// network that exists now rather than one released while it was stopped.
    ///
    /// Returns the `network_lock` guard: the caller holds it until the
    /// container has started, so neither a reap nor a concurrent stop can
    /// remove the network in between. `None` for a sandbox without a network.
    pub(super) async fn prepare_network_for_start(
        &self,
        handle: &SandboxHandle,
        info: &bollard::models::ContainerInspectResponse,
    ) -> Result<Option<tokio::sync::MutexGuard<'_, ()>>, AgentError> {
        let network_mode = info
            .host_config
            .as_ref()
            .and_then(|config| config.network_mode.as_deref());
        if network_mode == Some("none") {
            return Ok(None);
        }
        let guard = self.network_lock.lock().await;
        let network_name = sandbox_network_name(&handle.sandbox_name);
        self.ensure_network_locked(
            &handle.sandbox_name,
            &network_name,
            &self.config.control_plane_url,
            None,
        )
        .await?;
        self.reattach_sandbox_network(handle, info, &network_name)
            .await?;
        Ok(Some(guard))
    }

    /// Point the container's endpoint at the current `network_name`. Docker
    /// records a network by ID, so a container whose network was released
    /// and recreated would otherwise fail to start with "network ... not
    /// found". A no-op when the endpoint already matches.
    pub(super) async fn reattach_sandbox_network(
        &self,
        handle: &SandboxHandle,
        info: &bollard::models::ContainerInspectResponse,
        network_name: &str,
    ) -> Result<(), AgentError> {
        let current = self
            .docker
            .inspect_network(
                network_name,
                None::<bollard::query_parameters::InspectNetworkOptions>,
            )
            .await
            .map_err(|source| AgentError::SandboxProviderUnavailable {
                provider: "docker".to_string(),
                reason: format!(
                    "inspect isolated network '{network_name}' before starting sandbox '{}': {source}",
                    handle.sandbox_name
                ),
            })?;
        let endpoint_network_id = info
            .network_settings
            .as_ref()
            .and_then(|settings| settings.networks.as_ref())
            .and_then(|networks| networks.get(network_name))
            .and_then(|endpoint| endpoint.network_id.as_deref());
        if endpoint_network_id.is_some() && endpoint_network_id == current.id.as_deref() {
            return Ok(());
        }
        tracing::info!(
            sandbox = %handle.sandbox_name,
            network = network_name,
            "Reattaching sandbox to its recreated isolated network"
        );
        if endpoint_network_id.is_some() {
            self.docker
                .disconnect_network(
                    network_name,
                    bollard::models::NetworkDisconnectRequest {
                        container: handle.sandbox_id.clone(),
                        force: Some(true),
                    },
                )
                .await
                .map_err(|source| AgentError::SandboxProviderUnavailable {
                    provider: "docker".to_string(),
                    reason: format!(
                        "drop the stale endpoint of sandbox '{}' on released network '{network_name}': {source}",
                        handle.sandbox_name
                    ),
                })?;
        }
        self.docker
            .connect_network(
                network_name,
                bollard::models::NetworkConnectRequest {
                    container: handle.sandbox_id.clone(),
                    endpoint_config: Some(bollard::models::EndpointSettings::default()),
                },
            )
            .await
            .map_err(|source| AgentError::SandboxProviderUnavailable {
                provider: "docker".to_string(),
                reason: format!(
                    "attach sandbox '{}' to its recreated network '{network_name}': {source}",
                    handle.sandbox_name
                ),
            })
    }

    pub(super) async fn ensure_isolated_sandbox_bridge(
        &self,
        name: &str,
        container_name: &str,
        gateway: &str,
    ) -> Result<(), AgentError> {
        let inspected = self
            .docker
            .inspect_network(
                name,
                None::<bollard::query_parameters::InspectNetworkOptions>,
            )
            .await;
        let network = match inspected {
            Ok(network) => network,
            Err(error) if docker_error_is_not_found(&error) => {
                let mut created = create_host_isolated_network(
                    &self.docker,
                    isolated_sandbox_network_request(name, container_name, gateway),
                )
                .await;
                // Docker hands out a subnet per network from a finite pool.
                // Networks of sandboxes that no longer exist hold subnets for
                // nothing, so release those and retry once before telling the
                // operator the host is out of address space.
                if created
                    .as_ref()
                    .err()
                    .is_some_and(|source| is_network_pool_exhausted(&source.to_string()))
                    && self
                        .reap_orphaned_sandbox_networks(gateway, ORPHANED_NETWORK_MIN_AGE)
                        .await
                        > 0
                {
                    created = create_host_isolated_network(
                        &self.docker,
                        isolated_sandbox_network_request(name, container_name, gateway),
                    )
                    .await;
                }
                created.map_err(|source| {
                    let cause = source.to_string();
                    let reason = if is_network_pool_exhausted(&cause) {
                        network_pool_exhausted_reason(container_name, &cause)
                    } else {
                        format!("create isolated network for sandbox '{container_name}': {cause}")
                    };
                    AgentError::SandboxProviderUnavailable {
                        provider: "docker".to_string(),
                        reason,
                    }
                })?;
                self.docker
                    .inspect_network(
                        name,
                        None::<bollard::query_parameters::InspectNetworkOptions>,
                    )
                    .await
                    .map_err(|source| AgentError::SandboxProviderUnavailable {
                        provider: "docker".to_string(),
                        reason: format!(
                            "inspect isolated network for sandbox '{container_name}': {source}"
                        ),
                    })?
            }
            Err(source) => {
                return Err(AgentError::SandboxProviderUnavailable {
                    provider: "docker".to_string(),
                    reason: format!(
                        "inspect isolated network for sandbox '{container_name}': {source}"
                    ),
                })
            }
        };

        if let Some(labelled) = network_labelled_for_another_gateway(&network, gateway) {
            return Err(AgentError::SandboxProviderUnavailable {
                provider: "docker".to_string(),
                reason: format!(
                    "sandbox network '{name}' belongs to preview gateway '{labelled}', not this instance's gateway '{gateway}': it was set up before the gateway was renamed, or by another Temps instance sharing this Docker daemon. Docker cannot relabel a network, so this sandbox cannot be used; create a new one"
                ),
            });
        }
        let legacy_gateway_attached = if network
            .labels
            .as_ref()
            .is_some_and(|labels| !labels.contains_key(SANDBOX_PREVIEW_GATEWAY_LABEL))
            && gateway != crate::preview_gateway::PREVIEW_GATEWAY_CONTAINER
        {
            self.verified_gateway_is_attached(&network, gateway).await?
        } else {
            false
        };
        if !sandbox_network_matches_isolation_policy(
            &network,
            container_name,
            gateway,
            legacy_gateway_attached,
        ) {
            return Err(AgentError::SandboxProviderUnavailable {
                provider: "docker".to_string(),
                reason: format!(
                    "sandbox isolation is unavailable because Docker did not preserve the required host-isolation policy for '{name}'"
                ),
            });
        }
        // A gateway rename is refused while this network exists, including
        // for a sandbox without a row, such as a credential check. The caller
        // holds the name's read lock, so no rename is checking right now.
        self.config
            .preview_gateway_container_name
            .network_set_up(name)
            .await;
        Ok(())
    }

    async fn verified_gateway_is_attached(
        &self,
        network: &bollard::models::NetworkInspect,
        gateway: &str,
    ) -> Result<bool, AgentError> {
        let inspected = match self
            .docker
            .inspect_container(
                gateway,
                None::<bollard::query_parameters::InspectContainerOptions>,
            )
            .await
        {
            Ok(inspected) => inspected,
            Err(source) if docker_error_is_not_found(&source) => return Ok(false),
            Err(source) => {
                return Err(AgentError::SandboxProviderUnavailable {
                    provider: "docker".to_string(),
                    reason: format!(
                        "inspect legacy preview gateway '{gateway}' network ownership: {source}"
                    ),
                })
            }
        };
        if !is_managed_preview_gateway(&inspected) {
            return Ok(false);
        }
        let Some(gateway_id) = inspected.id.as_deref() else {
            return Ok(false);
        };
        Ok(network
            .containers
            .as_ref()
            .is_some_and(|containers| containers.contains_key(gateway_id)))
    }

    pub(super) async fn connect_container_to_network(
        &self,
        container: &str,
        network: &str,
        alias: Option<&str>,
    ) -> Result<(), AgentError> {
        let request = bollard::models::NetworkConnectRequest {
            container: container.to_string(),
            endpoint_config: Some(bollard::models::EndpointSettings {
                aliases: alias.map(|value| vec![value.to_string()]),
                ..Default::default()
            }),
        };
        match self.docker.connect_network(network, request).await {
            Ok(()) => Ok(()),
            Err(source)
                if source.to_string().contains("already exists")
                    || source.to_string().contains("already connected") =>
            {
                Ok(())
            }
            Err(source) => Err(AgentError::SandboxProviderUnavailable {
                provider: "docker".to_string(),
                reason: format!(
                    "attach managed container '{container}' to sandbox network '{network}': {source}"
                ),
            }),
        }
    }

    async fn connect_preview_gateways(
        &self,
        network: &str,
        gateway: &str,
    ) -> Result<(), AgentError> {
        match self
            .docker
            .inspect_container(
                gateway,
                None::<bollard::query_parameters::InspectContainerOptions>,
            )
            .await
        {
            Ok(info) if is_managed_preview_gateway(&info) => {
                self.connect_container_to_network(gateway, network, None).await
            }
            Ok(_) => Err(AgentError::SandboxProviderUnavailable {
                provider: "docker".to_string(),
                reason: format!(
                    "refusing to attach unverified container '{gateway}' to sandbox network '{network}'"
                ),
            }),
            Err(source) if docker_error_is_not_found(&source) => Ok(()),
            Err(source) => Err(AgentError::SandboxProviderUnavailable {
                provider: "docker".to_string(),
                reason: format!("inspect managed preview gateway '{gateway}': {source}"),
            }),
        }
    }

    /// Remove isolated sandbox networks whose sandbox container no longer
    /// exists or is stopped, returning how many were removed.
    ///
    /// Every network pins a subnet from Docker's finite address pool, and a
    /// sandbox that was removed behind Temps' back (or whose teardown failed
    /// part-way) leaves its network, and the subnet, behind. A stopped
    /// sandbox does not need its network until `start` recreates it. Only networks
    /// carrying this provider's ownership label whose name matches the owner
    /// are candidates, and a network younger than `min_age` is skipped
    /// because its container may not have been created yet. Networks
    /// not labelled for `gateway` (another Temps instance on this daemon, or
    /// a legacy network with no gateway label) are left alone. Best effort:
    /// failures are logged and never block the caller.
    pub(super) async fn reap_orphaned_sandbox_networks(
        &self,
        gateway: &str,
        min_age: std::time::Duration,
    ) -> usize {
        let networks = match self
            .docker
            .list_networks(Some(
                bollard::query_parameters::ListNetworksOptionsBuilder::new()
                    .filters(&HashMap::from([(
                        "label".to_string(),
                        vec![SANDBOX_NETWORK_OWNER_LABEL.to_string()],
                    )]))
                    .build(),
            ))
            .await
        {
            Ok(networks) => networks,
            Err(error) => {
                tracing::warn!("Could not list sandbox networks to reap orphans: {error}");
                return 0;
            }
        };
        let now_unix = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |elapsed| {
                i64::try_from(elapsed.as_secs()).unwrap_or(i64::MAX)
            });
        let mut reaped = 0;
        for network in &networks {
            let Some(owner) = orphan_candidate_owner(network, gateway, now_unix, min_age) else {
                continue;
            };
            // Claim the owner before looking at its container: from here a
            // create of the same name waits for this reap, so it cannot
            // start reusing the network between the check and the removal.
            let Some(_claim) = ReapClaim::try_new(&self.creates_in_flight, owner) else {
                continue;
            };
            let proxy = match self
                .docker
                .inspect_container(
                    owner,
                    None::<bollard::query_parameters::InspectContainerOptions>,
                )
                .await
            {
                Err(error) if docker_error_is_not_found(&error) => EgressProxyDisposal::Remove,
                // A stopped sandbox gets its network back from `start`, so
                // holding the subnet while it is parked buys nothing. This
                // also frees sandboxes stopped before `stop` released their
                // network, or stopped behind Temps' back (a daemon restart).
                // Its proxy is kept, stopped, for the egress policy it holds.
                Ok(info) if container_is_parked(&info) => EgressProxyDisposal::Park,
                Ok(_) => continue,
                Err(error) => {
                    tracing::warn!(
                        network = ?network.name,
                        "Could not inspect owner '{owner}' of sandbox network: {error}"
                    );
                    continue;
                }
            };
            // The owner's container is absent, but a sandbox another Temps
            // process is creating right now would be attached by the time
            // this looks again. Never force-disconnect a workload: a network
            // with any endpoint besides its own proxy and the gateway is in
            // use, so leave it for a later pass.
            match self
                .docker
                .inspect_network(
                    &sandbox_network_name(owner),
                    None::<bollard::query_parameters::InspectNetworkOptions>,
                )
                .await
            {
                Ok(inspected) if network_has_foreign_endpoints(&inspected, owner, gateway) => {
                    tracing::debug!(
                        network = ?network.name,
                        sandbox = %owner,
                        "Not reaping a sandbox network that still has attached workloads"
                    );
                    continue;
                }
                Ok(_) => {}
                Err(error) if docker_error_is_not_found(&error) => continue,
                Err(error) => {
                    tracing::warn!(
                        network = ?network.name,
                        "Could not inspect sandbox network '{owner}' before reaping it: {error}"
                    );
                    continue;
                }
            }
            if self
                .remove_isolated_sandbox_network_with(
                    owner,
                    NetworkDetach::OnlyGateway(gateway),
                    proxy,
                )
                .await
                .is_empty()
            {
                tracing::info!(
                    network = ?network.name,
                    sandbox = %owner,
                    "Removed isolated network of a sandbox that is stopped or no longer exists"
                );
                reaped += 1;
            }
        }
        reaped
    }

    pub(super) async fn remove_isolated_sandbox_network(
        &self,
        container_name: &str,
    ) -> Vec<String> {
        self.remove_isolated_sandbox_network_with(
            container_name,
            NetworkDetach::Everything,
            EgressProxyDisposal::Remove,
        )
        .await
    }

    /// Remove a sandbox's egress proxy and isolated network.
    ///
    /// `detach` decides which endpoints are disconnected before the network
    /// is removed. [`NetworkDetach::Everything`] is right when the caller
    /// knows the sandbox is gone. The orphan reaper can only infer that, and
    /// another Temps process may have started the sandbox meanwhile, so it
    /// passes [`NetworkDetach::OnlyGateway`]: the preview gateway is always
    /// attached to a sandbox network and would otherwise pin the subnet
    /// forever, while a workload that appeared since the check is left
    /// attached and Docker refuses to remove the network under it.
    async fn remove_isolated_sandbox_network_with(
        &self,
        container_name: &str,
        detach: NetworkDetach<'_>,
        proxy: EgressProxyDisposal,
    ) -> Vec<String> {
        let mut cleanup_errors = Vec::new();
        let proxy_name = sandbox_egress_proxy_name(container_name);
        match proxy {
            EgressProxyDisposal::Remove => {
                if let Err(error) = self
                    .docker
                    .remove_container(
                        &proxy_name,
                        Some(bollard::query_parameters::RemoveContainerOptions {
                            force: true,
                            ..Default::default()
                        }),
                    )
                    .await
                {
                    if !docker_error_is_not_found(&error) {
                        cleanup_errors.push(format!(
                            "remove per-sandbox egress proxy '{proxy_name}': {error}"
                        ));
                        tracing::warn!(
                            container = proxy_name,
                            "Failed to remove per-sandbox egress proxy: {error}"
                        );
                    }
                }
            }
            // A stopped container holds no endpoint, so the network can go
            // while the proxy keeps the sandbox's egress policy for the one
            // `ensure_egress_proxy` recreates when the sandbox starts.
            EgressProxyDisposal::Park => match self
                .docker
                .stop_container(
                    &proxy_name,
                    Some(bollard::query_parameters::StopContainerOptions {
                        t: Some(0),
                        signal: None,
                    }),
                )
                .await
            {
                Ok(()) => {}
                Err(error) if docker_error_is_not_found(&error) => {}
                Err(bollard::errors::Error::DockerResponseServerError {
                    status_code: 304, ..
                }) => {}
                Err(error) => {
                    cleanup_errors.push(format!(
                        "stop per-sandbox egress proxy '{proxy_name}' of a stopped sandbox: {error}"
                    ));
                    tracing::warn!(
                        container = proxy_name,
                        "Failed to stop the egress proxy of a stopped sandbox: {error}"
                    );
                    return cleanup_errors;
                }
            },
        }
        let network_name = sandbox_network_name(container_name);
        let network = match self
            .docker
            .inspect_network(
                &network_name,
                None::<bollard::query_parameters::InspectNetworkOptions>,
            )
            .await
        {
            Ok(network) => network,
            Err(error) if docker_error_is_not_found(&error) => {
                self.config
                    .preview_gateway_container_name
                    .network_removed(&network_name)
                    .await;
                return cleanup_errors;
            }
            Err(error) => {
                cleanup_errors.push(format!(
                    "inspect isolated sandbox network '{network_name}': {error}"
                ));
                tracing::warn!(
                    network = network_name,
                    "Failed to inspect isolated sandbox network during cleanup: {error}"
                );
                return cleanup_errors;
            }
        };
        let owned = network
            .labels
            .as_ref()
            .and_then(|labels| labels.get(SANDBOX_NETWORK_OWNER_LABEL))
            .is_some_and(|owner| owner == container_name);
        if !owned {
            cleanup_errors.push(format!(
                "refused to remove sandbox network '{network_name}' without the expected ownership label"
            ));
            tracing::warn!(
                network = network_name,
                "Refusing to remove sandbox network without the expected ownership label"
            );
            return cleanup_errors;
        }
        let attached: Vec<String> = network
            .containers
            .unwrap_or_default()
            .into_iter()
            .filter(|(_, endpoint)| match detach {
                NetworkDetach::Everything => true,
                NetworkDetach::OnlyGateway(gateway) => endpoint.name.as_deref() == Some(gateway),
            })
            .map(|(container_id, _)| container_id)
            .collect();
        for container_id in attached {
            if let Err(error) = self
                .docker
                .disconnect_network(
                    &network_name,
                    bollard::models::NetworkDisconnectRequest {
                        container: container_id,
                        force: Some(true),
                    },
                )
                .await
            {
                cleanup_errors.push(format!(
                    "disconnect managed sidecar from sandbox network '{network_name}': {error}"
                ));
                tracing::warn!(
                    network = network_name,
                    "Failed to disconnect managed sidecar during sandbox network cleanup: {error}"
                );
            }
        }
        match self.docker.remove_network(&network_name).await {
            Ok(()) => {
                self.config
                    .preview_gateway_container_name
                    .network_removed(&network_name)
                    .await;
            }
            Err(error) => {
                cleanup_errors.push(format!(
                    "remove isolated sandbox network '{network_name}': {error}"
                ));
                tracing::warn!(
                    network = network_name,
                    "Failed to remove isolated sandbox network: {error}"
                );
            }
        }
        cleanup_errors
    }

    pub(super) async fn ensure_managed_bridge(
        &self,
        name: &str,
        internal: bool,
    ) -> Result<(), AgentError> {
        let inspected = self
            .docker
            .inspect_network(
                name,
                None::<bollard::query_parameters::InspectNetworkOptions>,
            )
            .await;
        let network = match inspected {
            Ok(network) => network,
            Err(error) if docker_error_is_not_found(&error) => {
                tracing::info!(network = name, internal, "Creating managed sandbox network");
                self.docker
                    .create_network(bollard::models::NetworkCreateRequest {
                        name: name.to_string(),
                        driver: Some("bridge".to_string()),
                        internal: Some(internal),
                        enable_ipv6: Some(false),
                        labels: Some(HashMap::from([(
                            SANDBOX_EGRESS_POLICY_LABEL.to_string(),
                            SANDBOX_EGRESS_POLICY_VERSION.to_string(),
                        )])),
                        ..Default::default()
                    })
                    .await
                    .map_err(|source| AgentError::SandboxProviderUnavailable {
                        provider: "docker".to_string(),
                        reason: format!("create managed sandbox network '{name}': {source}"),
                    })?;
                self.docker
                    .inspect_network(
                        name,
                        None::<bollard::query_parameters::InspectNetworkOptions>,
                    )
                    .await
                    .map_err(|source| AgentError::SandboxProviderUnavailable {
                        provider: "docker".to_string(),
                        reason: format!("inspect newly created sandbox network '{name}': {source}"),
                    })?
            }
            Err(source) => {
                return Err(AgentError::SandboxProviderUnavailable {
                    provider: "docker".to_string(),
                    reason: format!("inspect sandbox network '{name}': {source}"),
                })
            }
        };

        let policy_label_matches = network
            .labels
            .as_ref()
            .and_then(|labels| labels.get(SANDBOX_EGRESS_POLICY_LABEL))
            .is_some_and(|value| value == SANDBOX_EGRESS_POLICY_VERSION);
        if !docker_network_matches_policy(&network, internal, false) || !policy_label_matches {
            return Err(AgentError::SandboxProviderUnavailable {
                provider: "docker".to_string(),
                reason: format!(
                    "managed sandbox network '{name}' does not match the required internal={internal}, IPv6-disabled bridge policy"
                ),
            });
        }
        Ok(())
    }
}
