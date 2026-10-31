// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Cluster creation: member placement, local member containers and cluster
//! initialization.

use crate::externalsvc::{
    postgres_cluster::PostgresClusterService, ClusterMemberResult, ClusterMemberSpec,
    ExternalService, ServiceConfig, ServiceType,
};
use crate::remote_service_client::{RemotePortMapping, RemoteServiceCreateParams};
use crate::services::*;
use chrono::Utc;
use sea_orm::{
    sea_query::Expr, ActiveModelTrait, ColumnTrait, DatabaseConnection, EntityTrait, QueryFilter,
    Set, TransactionTrait,
};
use std::collections::{BTreeSet, HashMap};
use temps_entities::{external_services, nodes, service_members, settings};
use tracing::{debug, error, info, warn};

/// Whether a cluster member's container is ready: running, and healthy when
/// its image defines a health check. `health` is Docker's status in any of
/// the spellings the agent and the local inspect produce (`healthy`,
/// `HEALTHY`, `"healthy"`); `none` or an empty status means the image has no
/// health check, so running is enough.
fn member_container_ready(running: bool, health: Option<&str>) -> bool {
    if !running {
        return false;
    }
    match health {
        None => true,
        Some(status) => {
            let status = status.trim().trim_matches('"').to_ascii_lowercase();
            matches!(status.as_str(), "healthy" | "none" | "")
        }
    }
}

impl ExternalServiceManager {
    /// The address control-plane members of a cluster that also has worker
    /// members advertise to those workers, and publish their ports on.
    ///
    /// Read from the operator's configuration (`multi_node.private_address`,
    /// set by `temps serve --private-address`, or the control plane's mesh
    /// address) — the same address deployments already give workers to reach
    /// the control plane. This used to be guessed from the default route,
    /// which on a VPC host is the public interface, and the member's port was
    /// only published on loopback anyway, so worker data nodes could never
    /// reach a control-plane monitor and the cluster stayed at zero
    /// registered data nodes.
    pub(super) async fn control_plane_member_address(
        &self,
        cluster_name: &str,
    ) -> Result<std::net::IpAddr, ExternalServiceError> {
        let private_address = settings::Entity::find_by_id(1)
            .one(self.db.as_ref())
            .await?
            .and_then(|row| {
                temps_core::AppSettings::from_json(row.data)
                    .multi_node
                    .private_address
            });
        let mesh_address = temps_entities::network_config::Entity::find_by_id(1)
            .one(self.db.as_ref())
            .await?
            .filter(|cfg| cfg.wireguard_enabled)
            .map(|cfg| {
                std::net::IpAddr::V4(temps_network::mesh::control_plane_mesh_address(
                    temps_network::mesh::reserved_mesh_pool(&cfg.wireguard_cidr),
                ))
            });

        let address = select_control_plane_member_address(private_address.as_deref(), mesh_address)
            .map_err(|reason| ExternalServiceError::ControlPlaneAddressRequired {
                name: cluster_name.to_string(),
                reason,
            })?;
        if !address.private {
            warn!(
                cluster = cluster_name,
                address = %address.ip,
                "Control-plane cluster members will publish their ports on a publicly routable \
                 address; enable the WireGuard mesh or use a private address to keep them off \
                 the internet"
            );
        }
        Ok(address.ip)
    }

    /// Whether a cluster member placement mixes the control plane
    /// (`node_id` `None`, or the control-plane pseudo-node) with worker
    /// nodes — the only topology in which control-plane members must be
    /// reachable from another host.
    pub(super) fn cluster_spans_control_plane_and_workers(
        members: &[ClusterMemberRequest],
    ) -> bool {
        let is_local = |m: &ClusterMemberRequest| {
            matches!(m.node_id, None | Some(Self::CONTROL_PLANE_NODE_ID))
        };
        members.iter().any(is_local) && members.iter().any(|m| !is_local(m))
    }

    /// Create a cluster-aware service instance for the given service type.
    fn create_cluster_service_instance(
        &self,
        name: String,
        service_type: ServiceType,
    ) -> Result<Option<Box<dyn ExternalService>>, ExternalServiceError> {
        Ok(match service_type {
            ServiceType::Postgres => {
                let docker = self.require_docker()?;
                Some(Box::new(PostgresClusterService::new(name, docker)))
            }
            // Future: Redis Sentinel, MongoDB Replica Set, RustFS distributed
            _ => None,
        })
    }

    /// Node id the API uses for the control plane in the node list.
    ///
    /// It is synthetic — there is no `nodes` row for the control plane, and
    /// containers it runs are stored with `node_id = NULL`. Mirrors
    /// `CONTROL_PLANE_NODE_ID` in `temps-deployments`.
    pub(super) const CONTROL_PLANE_NODE_ID: i32 = 0;

    /// Pure decision for what a freshly-created cluster member's
    /// `service_members.hostname` should hold, given whether the cluster
    /// (as a whole) has any remote member.
    ///
    /// A plain Docker container name only resolves via Docker's embedded
    /// DNS on the *same* Docker host. The `*.temps.local` FQDN resolves
    /// everywhere, but only once the per-host Hickory resolver is wired
    /// into a container's `/etc/resolv.conf` — gated behind
    /// `AppSettings.cluster_dns.enabled`, an experimental flag that
    /// defaults OFF. Unconditionally storing the FQDN here meant every
    /// single-host cluster (no worker nodes, one Docker daemon) injected a
    /// `POSTGRES_URL` whose hosts could never resolve, breaking the
    /// feature by default from a fresh install even though the cluster
    /// itself formed correctly.
    ///
    /// So: only trust the FQDN once there's a remote member in the mix —
    /// the one case where a container name can't cross the host boundary
    /// and FQDN resolution is actually required infrastructure. Every
    /// local (single-Docker-host) member keeps the plain container name,
    /// which every other container on `temps-app-network` — including a
    /// deployed app — already resolves via Docker's own embedded DNS with
    /// zero extra infrastructure.
    ///
    /// No I/O — kept as its own function so this decision can be exercised
    /// directly in tests without standing up a real cluster.
    fn resolve_member_hostname(
        has_remote_members: bool,
        member_fqdn: &str,
        container_name: &str,
    ) -> String {
        if has_remote_members {
            member_fqdn.to_string()
        } else {
            container_name.to_string()
        }
    }

    /// Pure decision for `add_cluster_member`: which address should the
    /// member being added dial to reach the cluster's monitor, based on
    /// the actual node topology of *this specific add* (not on a
    /// previously-persisted string that can go stale — see the call
    /// site's doc comment).
    pub(super) fn monitor_reachability_for_add(
        monitor_node_id: Option<i32>,
        new_member_node_id: Option<i32>,
    ) -> MonitorReachability {
        match (monitor_node_id, new_member_node_id) {
            (Some(nid), _) => MonitorReachability::MonitorNode(nid),
            (None, Some(_)) => MonitorReachability::LocalControlPlane,
            (None, None) => MonitorReachability::SameHost,
        }
    }

    /// Normalize and validate the node placement of every requested member.
    ///
    /// Returns the requests with the control-plane pseudo-node collapsed to
    /// `None` (which is how local placement is represented everywhere else),
    /// and fails with a validation error naming the offending member if any
    /// remaining id has no `nodes` row.
    ///
    /// Runs before any container is created so an unknown node is a rejected
    /// request rather than a half-built cluster.
    async fn resolve_member_placement(
        db: &DatabaseConnection,
        service_id: i32,
        member_requests: &[ClusterMemberRequest],
    ) -> Result<Vec<ClusterMemberRequest>, ExternalServiceError> {
        let normalized: Vec<ClusterMemberRequest> = member_requests
            .iter()
            .map(|m| ClusterMemberRequest {
                role: m.role.clone(),
                node_id: match m.node_id {
                    Some(Self::CONTROL_PLANE_NODE_ID) | None => None,
                    Some(id) => Some(id),
                },
            })
            .collect();

        // One query for every distinct remote id rather than a lookup per
        // member.
        let mut wanted: Vec<i32> = normalized.iter().filter_map(|m| m.node_id).collect();
        wanted.sort_unstable();
        wanted.dedup();

        if wanted.is_empty() {
            return Ok(normalized);
        }

        let found_nodes = nodes::Entity::find()
            .filter(nodes::Column::Id.is_in(wanted.clone()))
            .all(db)
            .await?;
        let found: Vec<i32> = found_nodes.iter().map(|n| n.id).collect();

        let missing: Vec<String> = wanted
            .iter()
            .filter(|id| !found.contains(id))
            .map(|id| id.to_string())
            .collect();

        if !missing.is_empty() {
            return Err(ExternalServiceError::ParameterValidationFailed {
                service_id,
                reason: format!(
                    "Unknown node id(s) [{}] requested for cluster members. Use an id from \
                     the node list, or omit it (or use {}) to place the member on the \
                     control plane.",
                    missing.join(", "),
                    Self::CONTROL_PLANE_NODE_ID
                ),
            });
        }

        let placements: Vec<Option<i32>> = normalized.iter().map(|m| m.node_id).collect();
        Self::refuse_nodes_without_overlay(service_id, &placements, &found_nodes)?;

        Ok(normalized)
    }

    /// ADR 056: members of a cluster that spans hosts find each other by
    /// their `*.temps.local` names, which resolve to overlay IPs and are
    /// served by each node's resolver on its overlay bridge. A node whose
    /// agent reports the cross-node overlay unavailable cannot take part, so
    /// refuse the placement up front with the node's cause. A node that never
    /// reported (`overlay_status` NULL, an older agent) is not refused.
    pub(super) fn refuse_nodes_without_overlay(
        service_id: i32,
        placements: &[Option<i32>],
        candidates: &[nodes::Model],
    ) -> Result<(), ExternalServiceError> {
        let hosts: std::collections::HashSet<Option<i32>> = placements.iter().copied().collect();
        if hosts.len() <= 1 {
            return Ok(());
        }
        let blocked: Vec<String> = candidates
            .iter()
            .filter(|node| hosts.contains(&Some(node.id)))
            .filter(|node| node.overlay_status.as_deref() == Some("unavailable"))
            .map(|node| {
                format!(
                    "'{}' ({})",
                    node.name,
                    node.overlay_last_error
                        .as_deref()
                        .unwrap_or("no cause reported")
                )
            })
            .collect();
        if blocked.is_empty() {
            return Ok(());
        }
        Err(ExternalServiceError::ParameterValidationFailed {
            service_id,
            reason: format!(
                "This cluster spans hosts, and members on different hosts reach each other over \
                 the cross-node network, which is unavailable on node(s) {}. Fix that node's \
                 network (the cause above names the fix; see Worker Nodes -> Cluster network), or \
                 place these members on hosts where it is up.",
                blocked.join("; ")
            ),
        })
    }

    /// Initialize a cluster service: create member containers across nodes,
    /// then record them in the service_members table.
    pub(super) async fn initialize_cluster(
        &self,
        service_id: i32,
        member_requests: &[ClusterMemberRequest],
    ) -> Result<(), ExternalServiceError> {
        info!("Initializing cluster for service {}", service_id);
        let service = self.get_service(service_id).await?;
        let service_type = ServiceType::from_str(&service.service_type).map_err(|_| {
            ExternalServiceError::InvalidServiceType {
                id: service_id,
                service_type: service.service_type.clone(),
            }
        })?;

        // Topology + role validation happens BEFORE parameter decryption so
        // bad-input requests fail with the correct error variant (and a
        // helpful message) instead of a generic "Service has no config".
        // Older ordering decrypted first and ate the validation error.
        let cluster_instance = self
            .create_cluster_service_instance(service.name.clone(), service_type)?
            .ok_or_else(|| ExternalServiceError::InitializationFailed {
                id: service_id,
                reason: format!(
                    "Service type '{}' does not support cluster topology",
                    service.service_type
                ),
            })?;

        // Validate roles
        let valid_roles = cluster_instance.valid_cluster_roles();
        for (i, member) in member_requests.iter().enumerate() {
            if !valid_roles.contains(&member.role.as_str()) {
                return Err(ExternalServiceError::ParameterValidationFailed {
                    service_id,
                    reason: format!(
                        "Invalid role '{}' for member {}. Valid roles: {:?}",
                        member.role, i, valid_roles
                    ),
                });
            }
        }

        // Resolve placement before anything is created.
        //
        // Two things go wrong without this. The node list API surfaces the
        // control plane as a synthetic node with id 0 — it has no `nodes` row,
        // because containers it runs are stored with `node_id = NULL` — so a
        // member placed on it used to reach the node lookup below and fail
        // with `Internal error: Node 0 not found`. And an id that simply
        // doesn't exist failed the same way, mid-creation, after other members
        // had already been built.
        //
        // Node 0 is normalized to `None` (the local/control-plane placement it
        // actually denotes), and every other id is checked up front so a bad
        // request is a validation error before any container exists.
        let member_requests =
            &Self::resolve_member_placement(self.db.as_ref(), service_id, member_requests).await?;

        // Parameter decryption only after validation has passed; otherwise
        // operators creating a cluster with an unsupported type or invalid
        // role get a misleading "service has no config" surface error.
        // New members always start with SCRAM auth for the infrastructure
        // roles, so the secrets must exist before any member is built.
        let (parameters, cluster_auth, _) = self.ensure_cluster_auth_secrets(service_id).await?;

        // Build member specs with ordinals and hostnames.
        //
        // When the cluster spans multiple nodes (has any remote members),
        // local members must advertise a routable IP instead of a Docker
        // container name — remote workers cannot resolve container names
        // from another host's Docker network — and must publish their port
        // on that IP, not only on loopback.
        let has_remote_members = member_requests.iter().any(|m| m.node_id.is_some());
        let control_plane_address: Option<std::net::IpAddr> =
            if Self::cluster_spans_control_plane_and_workers(member_requests) {
                Some(self.control_plane_member_address(&service.name).await?)
            } else {
                None
            };
        let local_private_ip = control_plane_address.map(|ip| ip.to_string());

        let mut member_specs = Vec::new();
        for (i, member) in member_requests.iter().enumerate() {
            let hostname: Option<String> = if let Some(node_id) = member.node_id {
                // Look up the node's private address for inter-member communication
                let node = nodes::Entity::find_by_id(node_id)
                    .one(self.db.as_ref())
                    .await?
                    .ok_or(ExternalServiceError::InternalError {
                        reason: format!("Node {} not found", node_id),
                    })?;
                Some(node.data_address().to_string())
            } else {
                // Local member: use control plane's private IP if available
                // (so remote workers can reach it), otherwise None (Docker DNS)
                local_private_ip.clone()
            };

            member_specs.push(ClusterMemberSpec {
                role: member.role.clone(),
                node_id: member.node_id,
                ordinal: i as i32,
                hostname,
            });
        }

        // Get the cluster config for building member-specific params
        let service_config = ServiceConfig {
            name: service.name.clone(),
            service_type,
            version: service.version.clone(),
            parameters: serde_json::to_value(&parameters).map_err(|e| {
                ExternalServiceError::InternalError {
                    reason: format!("Failed to serialize parameters: {}", e),
                }
            })?,
        };

        // Call init_cluster to get the container specs (names, ports)
        let member_results = cluster_instance
            .init_cluster(service_config.clone(), member_specs.clone())
            .await
            .map_err(|e| ExternalServiceError::InitializationFailed {
                id: service_id,
                reason: format!("Cluster init_cluster failed: {}", e),
            })?;

        // Record the intended membership before building anything.
        //
        // These rows used to be inserted one at a time inside the creation
        // loop below, which meant a failure before the first container — a bad
        // config, an unreachable node, a parse error — left the service
        // `failed` with zero `service_members`. Retry reconstructs its member
        // list from exactly those rows, so it had nothing to work from and
        // dead-ended on "no previous member records found", telling the
        // operator to supply a members array the console has no way to send.
        // Delete-and-recreate was the only way out.
        //
        // Writing them up front makes the requested topology durable from the
        // start, so every later failure is retryable. Rows are `pending` until
        // their container exists. The same transaction reserves the cluster's
        // host-port block: each row is written with its port.
        let (pre_created, base_port) =
            precreate_cluster_members(self.db.as_ref(), service_id, &member_results, &member_specs)
                .await?;

        // Get the Postgres cluster service for building member params.
        // The guard for local members (LocalWorkloadsDisabled) is below,
        // where we know each member's placement. Requiring docker here is
        // safe because create_cluster_service_instance already did so above.
        let pg_cluster = match service_type {
            ServiceType::Postgres => {
                let docker = self.require_docker()?;
                Some(PostgresClusterService::new(service.name.clone(), docker))
            }
            _ => None,
        };

        let cluster_config_parsed: crate::externalsvc::postgres_cluster::PostgresClusterConfig =
            serde_json::from_value(service_config.parameters.clone()).map_err(|e| {
                ExternalServiceError::InternalError {
                    reason: format!("Failed to parse cluster config: {}", e),
                }
            })?;

        // Pull resource limits once for the whole cluster — every member
        // (monitor + data nodes) gets the same caps. Defaults to unlimited
        // when the operator hasn't set a `resources` block.
        let cluster_resource_limits =
            crate::externalsvc::ServiceResourceLimits::from_parameters(&service_config.parameters);
        if let Err(e) = cluster_resource_limits.validate() {
            return Err(ExternalServiceError::InternalError {
                reason: format!("Invalid cluster resource limits: {}", e),
            });
        }

        // Find the monitor hostname for data node configuration.
        // For remote workers, use the node's private/WireGuard address.
        // For local (no node_id), use the monitor container name so Docker DNS resolves it.
        let monitor_spec = member_specs.iter().find(|m| is_role_monitor(&m.role));
        let pg_cluster_name = service.name.clone();
        let monitor_container_fallback = format!("postgres-{}-monitor", pg_cluster_name);
        let monitor_hostname = monitor_spec
            .and_then(|m| m.hostname.as_deref())
            .unwrap_or(&monitor_container_fallback);

        // Each cluster owns a 10-port block, reserved with its member rows
        // above: the monitor at its base, data nodes at base + ordinal. See
        // `select_cluster_port_base`.
        // Monitor gets base_port, data nodes get base_port + 1, +2, etc.
        let monitor_port = base_port;
        info!(
            "Cluster '{}' port assignment: monitor={}, data nodes start at {}",
            pg_cluster_name,
            monitor_port,
            base_port.saturating_add(1)
        );

        // Track successfully created members for rollback on failure
        struct CreatedMember {
            container_name: String,
            node_id: Option<i32>,
        }
        let mut created_members: Vec<CreatedMember> = Vec::new();
        // The member being worked on, so a failure is recorded on its row.
        let mut current_member_id: Option<i32> = None;

        // Create each member container (in order: monitor first, then data nodes)
        let create_result: Result<(), ExternalServiceError> = async {
            for (result, spec) in member_results.iter().zip(member_specs.iter()) {
                info!(
                    "Creating cluster member: {} (role: {}, ordinal: {}, node: {:?})",
                    result.container_name, result.role, result.ordinal, spec.node_id
                );

                // `service_members.role` is config-state — `monitor` for the
                // singleton orchestrator, `replica` for every data node.
                // "Primary" is a *runtime* fact owned by pg_auto_failover and
                // is surfaced via `live_state` (see
                // `get_service_members_with_live_state`). Storing one row as
                // `primary` would have to be reconciled on every failover,
                // and the lag between the monitor flipping and our row
                // catching up was the bug behind the "two primaries"
                // display. Treating roles as static config eliminates the
                // class.
                // The row already exists — it was written before any container
                // work started so a failure here is still retryable. Move it
                // from `pending` to `creating`.
                let member_model = {
                    let existing = pre_created.get(&result.ordinal).cloned().ok_or(
                        ExternalServiceError::InternalError {
                            reason: format!(
                                "No pre-created member record for ordinal {} of service {}",
                                result.ordinal, service_id
                            ),
                        },
                    )?;
                    let mut active: service_members::ActiveModel = existing.into();
                    active.status = Set("creating".to_string());
                    active.provisioning_step = Set(Some(
                        member_provisioning_step::PROVISIONING_CONTAINER.to_string(),
                    ));
                    active.provisioning_error = Set(None);
                    active.updated_at = Set(Utc::now());
                    active.update(self.db.as_ref()).await?
                };
                current_member_id = Some(member_model.id);

                // Assign port: monitor gets base_port, data nodes get base + ordinal
                // Reserved (and validated against the block) with the rows.
                let member_port =
                    cluster_member_host_port(base_port, spec.ordinal, is_role_monitor(&spec.role))
                        .unwrap_or(monitor_port);

                let (container_id, host_port, compute_ip) = if let Some(node_id) = spec.node_id {
                    // Remote: dispatch to agent
                    let client = self.get_remote_client(node_id).await?;

                    // Build member-specific create params
                    let member_params = if let Some(ref pg) = pg_cluster {
                        pg.build_member_params(
                            spec,
                            &cluster_config_parsed,
                            monitor_hostname,
                            monitor_port,
                            member_port,
                            cluster_resource_limits.clone(),
                            &cluster_auth,
                        )
                    } else {
                        return Err(ExternalServiceError::InitializationFailed {
                            id: service_id,
                            reason: "Only Postgres clusters are currently supported".to_string(),
                        });
                    };

                    // Each cluster member uses a unique port assigned by the
                    // manager. Map container_port = host_port to avoid conflicts.
                    let volume_name = format!("{}_data", result.container_name);
                    let limits_for_remote = if member_params.resource_limits.is_unlimited() {
                        None
                    } else {
                        Some(member_params.resource_limits.clone())
                    };
                    let remote_params = RemoteServiceCreateParams {
                        name: result.container_name.clone(),
                        service_type: "postgres".to_string(),
                        image: member_params.image,
                        environment: member_params.environment,
                        port_mappings: vec![RemotePortMapping {
                            host_port: member_params.container_port,
                            container_port: member_params.container_port,
                        }],
                        volumes: HashMap::from([(volume_name, member_params.volume_path)]),
                        network: Some(temps_core::NETWORK_NAME.to_string()),
                        command: member_params.command,
                        resource_limits: limits_for_remote,
                    };

                    let response = client.create_service(remote_params).await.map_err(|e| {
                        ExternalServiceError::InitializationFailed {
                            id: service_id,
                            reason: format!(
                                "Failed to create cluster member '{}' on node {}: {}",
                                result.container_name, node_id, e
                            ),
                        }
                    })?;

                    (
                        response.container_id,
                        Some(response.host_port as i32),
                        response.compute_ip,
                    )
                } else {
                    // Local: create container directly via Docker. Guard the
                    // profile contract before touching the daemon — even if a
                    // socket is mounted, a control-plane profile forbids local
                    // container creation.
                    if !self.local_workloads_enabled {
                        return Err(ExternalServiceError::LocalWorkloadsDisabled {
                            name: service.name.clone(),
                        });
                    }
                    let member_params = if let Some(ref pg) = pg_cluster {
                        pg.build_member_params(
                            spec,
                            &cluster_config_parsed,
                            monitor_hostname,
                            monitor_port,
                            member_port,
                            cluster_resource_limits.clone(),
                            &cluster_auth,
                        )
                    } else {
                        return Err(ExternalServiceError::InitializationFailed {
                            id: service_id,
                            reason: "Only Postgres clusters are currently supported".to_string(),
                        });
                    };

                    // Pull image, create and start container locally
                    self.create_local_cluster_member(
                        &result.container_name,
                        &member_params,
                        control_plane_address,
                    )
                    .await
                    .map_err(|e| {
                        ExternalServiceError::InitializationFailed {
                            id: service_id,
                            reason: format!(
                                "Failed to create local cluster member '{}': {}",
                                result.container_name, e
                            ),
                        }
                    })?
                };

                // Track this member for potential rollback
                created_members.push(CreatedMember {
                    container_name: result.container_name.clone(),
                    node_id: spec.node_id,
                });

                // Wait for the member to be healthy before proceeding to the next
                // This is important: monitor must be healthy before data nodes register
                if is_role_monitor(&spec.role) {
                    info!(
                        "Waiting for monitor '{}' to become healthy...",
                        result.container_name
                    );
                    self.claim_provisioning_step(
                        member_model.id,
                        member_provisioning_step::WAITING_FOR_HEALTH,
                    )
                    .await?;
                    self.wait_for_member_ready(
                        service_id,
                        spec.node_id,
                        &result.container_name,
                        60,
                    )
                    .await
                    .map_err(|e| {
                        ExternalServiceError::InitializationFailed {
                            id: service_id,
                            reason: format!("Monitor failed health check: {}", e),
                        }
                    })?;
                }

                // Compute the FQDN for this member (ADR-011). Registered in the
                // internal DNS registry below regardless of topology — cheap,
                // and useful the moment an operator later flips
                // `AppSettings.cluster_dns.enabled` on.
                let member_fqdn = format!(
                    "{}-{}.{}.temps.local",
                    service.name, spec.ordinal, service.name
                );

                // What we actually persist as `service_members.hostname` --
                // and therefore what `build_cluster_env_vars_for_resource`
                // puts in the multi-host `POSTGRES_URL` every linked app
                // gets -- must be something a *client container* can
                // actually resolve today, not just something registered in
                // a DNS zone. See `resolve_member_hostname`'s doc comment
                // for the full reasoning (FQDN only once the cluster spans
                // hosts; plain container name otherwise).
                let member_hostname = Self::resolve_member_hostname(
                    has_remote_members,
                    &member_fqdn,
                    &result.container_name,
                );

                // Update member record with container info and "running" status,
                // plus the resolvable hostname and overlay IP (if any).
                let member_id = member_model.id;
                let mut member_update: service_members::ActiveModel = member_model.into();
                member_update.container_id = Set(Some(container_id));
                member_update.port = Set(host_port);
                member_update.status = Set("running".to_string());
                member_update.provisioning_step =
                    Set(Some(member_provisioning_step::REGISTERING_DNS.to_string()));
                member_update.hostname = Set(Some(member_hostname));
                member_update.compute_ip = Set(compute_ip.clone());
                member_update.updated_at = Set(Utc::now());
                member_update.update(self.db.as_ref()).await?;

                // Register the per-member A record (ADR-011, Tier 2).
                //
                // Local members are directly reachable from application
                // containers on `temps-app-network`; their loopback-only host
                // port is intentionally *not* reachable through the node's
                // underlay address. Remote members still prefer their overlay
                // IP and otherwise use the underlay/host-port fallback.
                let (record_ip, record_port) = self
                    .resolve_member_dns_endpoint(
                        spec.node_id,
                        compute_ip.as_deref(),
                        &result.container_name,
                        host_port,
                        member_port,
                    )
                    .await
                    .map(|(ip, port)| (Some(ip), port))
                    .unwrap_or((None, member_port as i32));

                if let Some(ip) = record_ip {
                    let draft = temps_dns::EndpointDraft {
                        fqdn: member_fqdn.clone(),
                        record_type: temps_dns::InternalRecordType::A,
                        target_ip: Some(ip.clone()),
                        target_port: Some(record_port),
                        ttl: 30,
                        owner_kind: temps_dns::InternalOwnerKind::ServiceMember,
                        owner_id: member_id as i64,
                        node_id: spec.node_id,
                    };
                    if let Err(e) = self
                        .dns_registry
                        .replace_endpoints_for_owner(
                            temps_dns::InternalOwnerKind::ServiceMember,
                            member_id as i64,
                            &[draft],
                        )
                        .await
                    {
                        warn!(
                            service_id,
                            member_id,
                            fqdn = %member_fqdn,
                            ip = %ip,
                            error = %e,
                            "Failed to register DNS record for cluster member"
                        );
                    } else {
                        info!(
                            service_id,
                            member_id,
                            fqdn = %member_fqdn,
                            ip = %ip,
                            port = record_port,
                            "Registered DNS A record for cluster member"
                        );
                    }
                }
                self.claim_provisioning_step(member_id, member_provisioning_step::DONE)
                    .await?;
            }
            Ok(())
        }
        .await;

        // If any member failed, roll back all previously created containers
        if let Err(e) = create_result {
            error!(
                "Cluster member creation failed for service {}: {}. Rolling back {} created container(s).",
                service_id, e, created_members.len()
            );

            // Say which member failed and why, on its own row, so the console
            // shows the reason next to it rather than only on the service.
            if let Some(member_id) = current_member_id {
                if let Err(db_err) = service_members::Entity::update_many()
                    .col_expr(
                        service_members::Column::ProvisioningStep,
                        Expr::value(member_provisioning_step::FAILED),
                    )
                    .col_expr(
                        service_members::Column::ProvisioningError,
                        Expr::value(e.to_string()),
                    )
                    .col_expr(service_members::Column::UpdatedAt, Expr::value(Utc::now()))
                    .filter(service_members::Column::Id.eq(member_id))
                    .exec(self.db.as_ref())
                    .await
                {
                    error!(
                        service_id,
                        member_id,
                        "Failed to record the cluster member's provisioning error: {}",
                        db_err
                    );
                }
            }

            for member in &created_members {
                if let Some(node_id) = member.node_id {
                    // Remote: ask agent to remove the container
                    match self.get_remote_client(node_id).await {
                        Ok(client) => {
                            if let Err(rm_err) = client.remove_service(&member.container_name).await
                            {
                                error!(
                                    "Rollback: failed to remove remote container '{}' on node {}: {}",
                                    member.container_name, node_id, rm_err
                                );
                            } else {
                                info!(
                                    "Rollback: removed remote container '{}' on node {}",
                                    member.container_name, node_id
                                );
                            }
                        }
                        Err(client_err) => {
                            error!(
                                "Rollback: failed to get remote client for node {}: {}",
                                node_id, client_err
                            );
                        }
                    }
                } else {
                    // Local: remove container directly via Docker. If this
                    // process has no local Docker daemon (control-plane
                    // profile), there is nothing local to roll back — the
                    // member was never actually created here.
                    match self.docker.get() {
                        Some(docker) => {
                            if let Err(rm_err) = docker
                                .remove_container(
                                    &member.container_name,
                                    Some(bollard::query_parameters::RemoveContainerOptions {
                                        force: true,
                                        ..Default::default()
                                    }),
                                )
                                .await
                            {
                                error!(
                                    "Rollback: failed to remove local container '{}': {}",
                                    member.container_name, rm_err
                                );
                            } else {
                                info!(
                                    "Rollback: removed local container '{}'",
                                    member.container_name
                                );
                            }

                            // Also remove the volume
                            let volume_name = format!("{}_data", member.container_name);
                            if let Err(vol_err) = docker
                                .remove_volume(
                                    &volume_name,
                                    None::<bollard::query_parameters::RemoveVolumeOptions>,
                                )
                                .await
                            {
                                warn!(
                                    "Rollback: failed to remove volume '{}': {}",
                                    volume_name, vol_err
                                );
                            }
                        }
                        None => {
                            debug!(
                                container_name = %member.container_name,
                                "Rollback: no local Docker daemon in this process; nothing to remove locally"
                            );
                        }
                    }
                }
            }

            // Mark remaining service_members as "failed" instead of deleting them.
            // This preserves the original member topology so the retry endpoint can
            // reconstruct the member specs without user re-input.
            if let Err(db_err) = service_members::Entity::update_many()
                .col_expr(service_members::Column::Status, Expr::value("failed"))
                .col_expr(service_members::Column::UpdatedAt, Expr::value(Utc::now()))
                .filter(service_members::Column::ServiceId.eq(service_id))
                .exec(self.db.as_ref())
                .await
            {
                error!(
                    "Rollback: failed to update service_members status for service {}: {}",
                    service_id, db_err
                );
            }

            return Err(e);
        }

        // Capture name before we move `service` into the ActiveModel below.
        let service_name = service.name.clone();

        // Update parent service status
        let mut service_update: external_services::ActiveModel = service.into();
        service_update.status = Set("running".to_string());
        service_update.updated_at = Set(Utc::now());
        service_update.update(self.db.as_ref()).await?;

        // Every member was just built by this code (a retry removes the
        // leftovers first), so the infrastructure roles already use SCRAM.
        // Record that, or the next `add_cluster_member` treats this cluster
        // as legacy and re-runs the in-place upgrade against members that may
        // still be taking their initial base backup.
        if let Err(e) = self
            .record_cluster_auth_version(service_id, CLUSTER_INFRA_AUTH_VERSION)
            .await
        {
            warn!(
                service_id,
                error = %e,
                "Cluster created with SCRAM-native members, but its authentication version \
                 could not be recorded; the next start re-runs the idempotent upgrade"
            );
        }

        // Start the per-cluster role reconciler (ADR-011 Phase 4). Best-effort:
        // skipped if no DnsRegistry is wired (legacy plugin) or if a reconciler
        // is already running for this service_id (idempotent retry).
        self.spawn_role_reconciler(service_id, service_name).await;

        info!("Cluster service {} initialized successfully", service_id);
        Ok(())
    }

    pub(super) async fn exec_in_local_container(
        &self,
        container_name: &str,
        cmd: &[String],
        user: Option<&str>,
    ) -> Result<(i64, String, String), ExternalServiceError> {
        use bollard::exec::{CreateExecOptions, StartExecOptions};
        use futures::StreamExt;

        let docker = self.require_docker()?;
        let cmd_refs: Vec<&str> = cmd.iter().map(|s| s.as_str()).collect();
        let exec = docker
            .create_exec(
                container_name,
                CreateExecOptions {
                    cmd: Some(cmd_refs),
                    attach_stdout: Some(true),
                    attach_stderr: Some(true),
                    user,
                    ..Default::default()
                },
            )
            .await
            .map_err(|e| ExternalServiceError::DockerError {
                id: 0,
                reason: format!("Failed to create exec in '{}': {}", container_name, e),
            })?;

        let output = docker
            .start_exec(
                &exec.id,
                Some(StartExecOptions {
                    detach: false,
                    ..Default::default()
                }),
            )
            .await
            .map_err(|e| ExternalServiceError::DockerError {
                id: 0,
                reason: format!("Failed to start exec in '{}': {}", container_name, e),
            })?;

        // Capture stdout + stderr separately so the caller can decide
        // which to surface in error messages.
        let mut stdout = String::new();
        let mut stderr = String::new();
        if let bollard::exec::StartExecResults::Attached { mut output, .. } = output {
            while let Some(chunk) = output.next().await {
                match chunk {
                    Ok(bollard::container::LogOutput::StdOut { message }) => {
                        stdout.push_str(&String::from_utf8_lossy(&message));
                    }
                    Ok(bollard::container::LogOutput::StdErr { message }) => {
                        stderr.push_str(&String::from_utf8_lossy(&message));
                    }
                    Ok(other) => {
                        // Console / StdIn never appear here, but include
                        // them in stdout for completeness rather than
                        // dropping silently.
                        stdout.push_str(&other.to_string());
                    }
                    Err(e) => {
                        return Err(ExternalServiceError::DockerError {
                            id: 0,
                            reason: format!("Exec stream error: {}", e),
                        });
                    }
                }
            }
        }

        let inspect =
            docker
                .inspect_exec(&exec.id)
                .await
                .map_err(|e| ExternalServiceError::DockerError {
                    id: 0,
                    reason: format!("Failed to inspect exec result: {}", e),
                })?;
        let exit_code = inspect.exit_code.unwrap_or(-1);
        Ok((exit_code, stdout, stderr))
    }

    /// Resolve a fallback `(ip, port)` for a cluster member that doesn't
    /// have an overlay IP. Used by the DNS registration path so the
    /// member's FQDN still points *somewhere* — even when the overlay
    /// isn't attached. Returns `(node.private_address, host_port)`
    /// because that's the address+port docker-proxy listens on for the
    /// container. Returns `None` if we can't determine either piece.
    ///
    /// Worker members only: a control-plane member is resolved from its
    /// application-network address by `resolve_member_dns_endpoint`.
    async fn resolve_member_underlay(
        &self,
        node_id: i32,
        host_port: Option<i32>,
        container_port: u16,
    ) -> Option<(String, i32)> {
        // Without a host port we have nothing useful to publish — the
        // FQDN can't point at the container's internal port without an
        // overlay IP.
        let port = host_port.unwrap_or(container_port as i32);

        let ip = nodes::Entity::find_by_id(node_id)
            .one(self.db.as_ref())
            .await
            .ok()
            .flatten()
            .map(|n| n.data_address().to_string())?;

        Some((ip, port))
    }

    /// Resolve the address published for a service-member FQDN.
    ///
    /// Local managed-service ports bind to `127.0.0.1` for security, so the
    /// control-plane underlay address plus host port is not reachable from an
    /// application container. Local members instead publish their container
    /// address on the shared application network and the container port.
    /// Remote members retain the overlay-first, underlay-fallback behavior.
    pub(super) async fn resolve_member_dns_endpoint(
        &self,
        node_id: Option<i32>,
        overlay_ip: Option<&str>,
        container_name: &str,
        host_port: Option<i32>,
        container_port: u16,
    ) -> Option<(String, i32)> {
        if let Some(endpoint) =
            select_member_dns_endpoint(node_id, overlay_ip, None, None, container_port)
        {
            return Some(endpoint);
        }

        if node_id.is_none() {
            let local_network_ip = self
                .lookup_container_network_ip(container_name, &temps_core::NETWORK_NAME)
                .await;
            if let Some(endpoint) = select_member_dns_endpoint(
                node_id,
                None,
                local_network_ip.as_deref(),
                None,
                container_port,
            ) {
                return Some(endpoint);
            }
        }

        if let Some(remote_node_id) = node_id {
            let underlay = self
                .resolve_member_underlay(remote_node_id, host_port, container_port)
                .await;
            return select_member_dns_endpoint(node_id, None, None, underlay, container_port);
        }

        // Publishing the control plane's underlay address here would be
        // actively misleading: local managed-service ports bind only to
        // 127.0.0.1, so application containers cannot reach that address.
        None
    }

    pub(super) async fn lookup_container_network_ip(
        &self,
        container_name: &str,
        network_name: &str,
    ) -> Option<String> {
        use bollard::query_parameters::InspectContainerOptions;

        let docker = self.docker.get()?;
        match docker
            .inspect_container(container_name, None::<InspectContainerOptions>)
            .await
        {
            Ok(info) => info
                .network_settings
                .as_ref()
                .and_then(|settings| settings.networks.as_ref())
                .and_then(|networks| networks.get(network_name))
                .and_then(|endpoint| endpoint.ip_address.as_deref())
                .map(str::trim)
                .filter(|ip| !ip.is_empty())
                .map(str::to_string),
            Err(error) => {
                warn!(
                    container = container_name,
                    network = network_name,
                    error = %error,
                    "Failed to inspect local cluster member network address"
                );
                None
            }
        }
    }

    /// Whether a control-plane cluster member's container publishes `port`
    /// on `address`, i.e. whether a member on a worker node can reach it.
    pub(super) async fn local_member_published_on(
        &self,
        container_name: &str,
        port: u16,
        address: std::net::IpAddr,
    ) -> Result<bool, ExternalServiceError> {
        use bollard::query_parameters::InspectContainerOptions;

        let docker = self.require_docker()?;
        let info = docker
            .inspect_container(container_name, None::<InspectContainerOptions>)
            .await
            .map_err(|e| ExternalServiceError::DockerError {
                id: 0,
                reason: format!(
                    "Failed to inspect control-plane cluster member '{}' to check whether port \
                     {} is published on {}: {}",
                    container_name, port, address, e
                ),
            })?;
        Ok(port_bindings_publish_on(
            info.host_config
                .as_ref()
                .and_then(|hc| hc.port_bindings.as_ref()),
            port,
            address,
        ))
    }

    /// Create a cluster member container on the local Docker daemon.
    ///
    /// Returns `(container_id, host_port, compute_ip)`:
    /// - `container_id` — Docker's internal id for the new container.
    /// - `host_port` — the host port the member's port maps to.
    /// - `compute_ip` — the container's IP on the multi-host overlay
    ///   (`temps-overlay`), or `None` on single-host clusters where the
    ///   overlay isn't attached. Read by the caller into
    ///   `service_members.compute_ip` and the DNS registry (ADR-011).
    ///
    /// `control_plane_address` is set when the cluster also has members on
    /// worker nodes: the member's port is then published on that address as
    /// well as on loopback, so those members can reach it.
    pub(super) async fn create_local_cluster_member(
        &self,
        container_name: &str,
        params: &crate::externalsvc::postgres_cluster::ClusterMemberCreateParams,
        control_plane_address: Option<std::net::IpAddr>,
    ) -> Result<(String, Option<i32>, Option<String>), ExternalServiceError> {
        use bollard::models::*;
        use bollard::query_parameters::*;

        let docker = self.require_docker()?;

        // Ensure network exists
        crate::utils::ensure_network_exists(&docker)
            .await
            .map_err(|e| ExternalServiceError::DockerError {
                id: 0,
                reason: format!("Failed to ensure network: {}", e),
            })?;

        // Pull image
        crate::utils::pull_image_with_retry(&docker, &params.image, None)
            .await
            .map_err(|e| ExternalServiceError::DockerError { id: 0, reason: e })?;

        // Create volume
        let volume_name = format!("{}_data", container_name);
        let _ = docker
            .create_volume(bollard::models::VolumeCreateRequest {
                name: Some(volume_name.clone()),
                ..Default::default()
            })
            .await;

        // Build env vars
        let env: Vec<String> = params
            .environment
            .iter()
            .map(|(k, v)| format!("{}={}", k, v))
            .collect();

        // Port bindings: map the container port to the same host port.
        // Each cluster member uses a unique port assigned by the manager so
        // there are no conflicts even when multiple members run on the same host.
        let (exposed_ports, port_bindings) =
            cluster_member_port_config(params.container_port, control_plane_address);
        if let Some(address) = control_plane_address {
            info!(
                container = container_name,
                port = params.container_port,
                address = %address,
                "Publishing control-plane cluster member for worker members"
            );
        }

        // The control plane's cluster resolver, so the member resolves
        // `*.temps.local` (ADR-024), with the host's servers after it. Only
        // while cluster DNS is enabled and the resolver answers; otherwise
        // Docker's embedded DNS, as for every other container here.
        let dns_servers = self.cluster_dns_servers();

        // Create container
        let mut cluster_host_config = HostConfig {
            binds: Some(vec![format!("{}:{}", volume_name, params.volume_path)]),
            port_bindings: Some(port_bindings),
            dns: dns_servers,
            restart_policy: Some(RestartPolicy {
                name: Some(RestartPolicyNameEnum::UNLESS_STOPPED),
                maximum_retry_count: None,
            }),
            network_mode: Some(temps_core::NETWORK_NAME.to_string()),
            ..Default::default()
        };
        params
            .resource_limits
            .apply_to_host_config(&mut cluster_host_config);
        let container_config = ContainerCreateBody {
            image: Some(params.image.clone()),
            env: Some(env),
            cmd: params.command.clone(),
            // The postgres-ha image only declares 5432/tcp, while HA members
            // listen on dynamically assigned ports (for example 6040-6042).
            // Docker's create API requires the dynamic port in ExposedPorts as
            // well as HostConfig.PortBindings. Docker Desktop happens to
            // tolerate the binding alone, but Linux engines may leave it
            // unpublished, producing a healthy container behind a refused
            // localhost socket.
            exposed_ports: Some(exposed_ports),
            host_config: Some(cluster_host_config),
            labels: Some(HashMap::from([
                ("sh.temps.managed".to_string(), "true".to_string()),
                ("sh.temps.service".to_string(), "true".to_string()),
                (
                    "sh.temps.service.type".to_string(),
                    "postgres-cluster".to_string(),
                ),
                (
                    "sh.temps.service.name".to_string(),
                    container_name.to_string(),
                ),
            ])),
            ..Default::default()
        };

        let response = docker
            .create_container(
                Some(
                    CreateContainerOptionsBuilder::new()
                        .name(container_name)
                        .build(),
                ),
                container_config,
            )
            .await
            .map_err(|e| ExternalServiceError::DockerError {
                id: 0,
                reason: format!("Failed to create container {}: {}", container_name, e),
            })?;

        // Best-effort dual-attach to the multi-host overlay (ADR-011).
        // The container was created on temps-app-network for legacy
        // routing; this also attaches it to temps-overlay so it has a
        // routable cross-node IP and the DNS registry can write A
        // records pointing at it. Skipped silently when the overlay
        // isn't bootstrapped on this host (single-host mode).
        let overlay_name = temps_network::overlay_name::current().docker_network;
        match docker
            .list_networks(None::<bollard::query_parameters::ListNetworksOptions>)
            .await
        {
            Ok(networks)
                if networks
                    .iter()
                    .any(|n| n.name.as_deref() == Some(overlay_name.as_str())) =>
            {
                let req = bollard::models::NetworkConnectRequest {
                    container: response.id.clone(),
                    ..Default::default()
                };
                match docker.connect_network(&overlay_name, req).await {
                    Ok(()) => {
                        info!(
                            container = container_name,
                            overlay = %overlay_name,
                            "attached cluster member to overlay"
                        );
                    }
                    // 403 = already connected — no-op.
                    Err(bollard::errors::Error::DockerResponseServerError {
                        status_code: 403,
                        ..
                    }) => {}
                    Err(e) => {
                        warn!(
                            container = container_name,
                            overlay = %overlay_name,
                            error = %e,
                            "Failed to attach cluster member to overlay; continuing single-host"
                        );
                    }
                }
            }
            Ok(_) => {
                debug!(
                    container = container_name,
                    overlay = %overlay_name,
                    "overlay not present on this host; skipping attach"
                );
            }
            Err(e) => {
                warn!(error = %e, "list_networks failed during overlay-attach probe");
            }
        }

        // Start container
        docker
            .start_container(container_name, None::<StartContainerOptions>)
            .await
            .map_err(|e| ExternalServiceError::DockerError {
                id: 0,
                reason: format!("Failed to start container {}: {}", container_name, e),
            })?;

        // Each member uses a unique port — container_port == host_port
        let host_port = Some(params.container_port as i32);

        // Best-effort overlay-IP discovery for the DNS registry (ADR-011).
        // Failure here is non-fatal — the member still starts; the DNS
        // record is just not written for this generation.
        let compute_ip = match docker
            .inspect_container(container_name, None::<InspectContainerOptions>)
            .await
        {
            Ok(info) => {
                let overlay_name = temps_network::overlay_name::current().docker_network;
                info.network_settings
                    .as_ref()
                    .and_then(|ns| ns.networks.as_ref())
                    .and_then(|nets| nets.get(&overlay_name))
                    .and_then(|ep| ep.ip_address.as_deref())
                    .map(str::trim)
                    .filter(|s| !s.is_empty())
                    .map(str::to_string)
            }
            Err(e) => {
                warn!(
                    container = %container_name,
                    "Failed to inspect new cluster member for overlay IP: {}",
                    e
                );
                None
            }
        };

        Ok((response.id, host_port, compute_ip))
    }

    /// Wait until a cluster member's container is running and, when its
    /// image defines a health check, healthy.
    ///
    /// A member on a worker is asked about through that worker's agent: the
    /// control plane's own Docker daemon does not have the container, so
    /// inspecting it locally never succeeds and the wait only ever timed
    /// out. The timeout error carries the last state seen, so the operator
    /// learns whether the container never started, kept restarting or stayed
    /// unhealthy.
    async fn wait_for_member_ready(
        &self,
        service_id: i32,
        node_id: Option<i32>,
        container_name: &str,
        timeout_secs: u64,
    ) -> Result<(), ExternalServiceError> {
        use bollard::query_parameters::InspectContainerOptions;
        use std::time::{Duration, Instant};

        let remote = match node_id {
            Some(node_id) => Some((node_id, self.get_remote_client(node_id).await?)),
            None => None,
        };
        let docker = match remote {
            Some(_) => None,
            None => Some(self.require_docker()?),
        };
        let start = Instant::now();
        let timeout = Duration::from_secs(timeout_secs);
        let mut last_state: String;

        loop {
            let observed = match (&remote, &docker) {
                (Some((_, client)), _) => match client.service_status(container_name).await {
                    Ok(status) => Ok((status.running, status.health)),
                    Err(e) => Err(format!("status check failed: {}", e)),
                },
                (None, Some(docker)) => {
                    match docker
                        .inspect_container(container_name, None::<InspectContainerOptions>)
                        .await
                    {
                        Ok(info) => {
                            let state = info.state.as_ref();
                            Ok((
                                state.and_then(|s| s.running).unwrap_or(false),
                                state
                                    .and_then(|s| s.health.as_ref())
                                    .and_then(|h| h.status.as_ref())
                                    .map(|s| s.to_string()),
                            ))
                        }
                        Err(e) => Err(format!("inspect failed: {}", e)),
                    }
                }
                (None, None) => Err("no Docker client".to_string()),
            };

            match observed {
                Ok((running, health)) => {
                    if member_container_ready(running, health.as_deref()) {
                        return Ok(());
                    }
                    last_state = format!(
                        "running: {}, health: {}",
                        running,
                        health.as_deref().unwrap_or("none")
                    );
                }
                Err(reason) => last_state = reason,
            }

            if start.elapsed() > timeout {
                let place = match &remote {
                    Some((node_id, _)) => format!("on node {}", node_id),
                    None => "on the control plane".to_string(),
                };
                return Err(ExternalServiceError::InitializationFailed {
                    id: service_id,
                    reason: format!(
                        "Container {} {} did not become ready within {}s (last state: {})",
                        container_name, place, timeout_secs, last_state
                    ),
                });
            }

            tokio::time::sleep(Duration::from_secs(2)).await;
        }
    }
}

/// Persist the complete intended topology as one transaction so a database
/// failure cannot leave a retry with only a prefix of the requested members.
///
/// The same transaction reserves the cluster's host-port block. Selecting a
/// block only reads other clusters' ports, so two concurrent creations could
/// otherwise pick the same one and the second would fail binding it. The
/// advisory lock serializes selection, and writing each row's port before
/// commit makes the block visible to the next creation. Returns the rows by
/// ordinal and the block's base port.
async fn precreate_cluster_members(
    db: &DatabaseConnection,
    service_id: i32,
    member_results: &[ClusterMemberResult],
    member_specs: &[ClusterMemberSpec],
) -> Result<(HashMap<i32, service_members::Model>, u16), ExternalServiceError> {
    use sea_orm::ConnectionTrait;

    let transaction = db.begin().await?;
    transaction
        .execute(sea_orm::Statement::from_string(
            sea_orm::DatabaseBackend::Postgres,
            format!("SELECT pg_advisory_xact_lock({CLUSTER_PORT_LOCK_KEY})"),
        ))
        .await?;
    let used = used_cluster_ports(&transaction, service_id).await?;
    let base_port = select_cluster_port_base(service_id, &used).ok_or_else(|| {
        ExternalServiceError::ClusterPortsUnavailable {
            service_id,
            reason: format!(
                "every {}-port block from {} to {} is held by another cluster's members",
                CLUSTER_PORT_BLOCK_SIZE,
                CLUSTER_PORT_RANGE_START,
                u16::MAX
            ),
        }
    })?;
    let mut pre_created = HashMap::new();
    let mut assigned: BTreeSet<u16> = BTreeSet::new();

    for (result, spec) in member_results.iter().zip(member_specs.iter()) {
        let port =
            cluster_member_host_port(base_port, result.ordinal, is_role_monitor(&result.role))
                .ok_or_else(|| ExternalServiceError::ClusterPortsUnavailable {
                    service_id,
                    reason: format!(
                    "member ordinal {} is outside the cluster's {}-port block starting at {}; a \
                     cluster has at most {} members",
                    result.ordinal, CLUSTER_PORT_BLOCK_SIZE, base_port, CLUSTER_PORT_BLOCK_SIZE
                ),
                })?;
        // The monitor holds the block's base, which is also ordinal 0's
        // data-member port: a member list that does not start with the
        // monitor would give two members one host port.
        if !assigned.insert(port) {
            return Err(ExternalServiceError::ClusterPortsUnavailable {
                service_id,
                reason: format!(
                    "member '{}' (ordinal {}) would share host port {} with another member; \
                     list the monitor first",
                    result.container_name, result.ordinal, port
                ),
            });
        }
        let stored_role = if is_role_monitor(&result.role) {
            "monitor".to_string()
        } else {
            "replica".to_string()
        };
        let now = Utc::now();
        let record = service_members::ActiveModel {
            service_id: Set(service_id),
            node_id: Set(spec.node_id),
            role: Set(stored_role),
            container_id: Set(None),
            container_name: Set(result.container_name.clone()),
            hostname: Set(spec.hostname.clone()),
            port: Set(Some(i32::from(port))),
            status: Set("pending".to_string()),
            ordinal: Set(result.ordinal),
            config: Set(None),
            created_at: Set(now),
            updated_at: Set(now),
            ..Default::default()
        };
        let model = record.insert(&transaction).await?;
        pre_created.insert(result.ordinal, model);
    }

    transaction.commit().await?;
    Ok((pre_created, base_port))
}

#[cfg(test)]
mod tests;

/// Docker-backed regression for a reported HA cluster stuck at "Monitor ...
/// reports zero data nodes": the console's default layout puts the
/// pg_auto_failover monitor on the control plane and the data members on
/// workers, and the monitor was only published on loopback, so no worker
/// could ever reach it to register.
///
/// The control-plane monitor is created by the real
/// `create_local_cluster_member`. The "worker" data member runs in the Docker
/// host's network namespace, where — like a container on another machine —
/// the monitor's container name means nothing and its only route to the
/// monitor is the control plane's private address. That address is the
/// gateway of a test bridge: an IP that lives on the Docker host and is not
/// loopback, which is exactly what `--private-address` is on a real host.
///
/// Run: `cargo test --lib -p temps-providers --features docker-tests -- cross_host_cluster_docker`
// Two attributes rather than `cfg(all(test, ..))`: clippy only applies
// `allow-unwrap-in-tests` when it sees a top-level `cfg(test)`.
#[cfg(test)]
#[cfg(feature = "docker-tests")]
mod cross_host_cluster_docker_tests;
