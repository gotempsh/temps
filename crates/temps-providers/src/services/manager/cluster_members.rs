// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Cluster membership: member queries, primary lookup, and adding, removing and
//! promoting members.

use crate::externalsvc::{
    postgres_cluster::PostgresClusterService, ClusterMemberSpec, ServiceConfig, ServiceType,
};
use crate::remote_service_client::{RemotePortMapping, RemoteServiceCreateParams};
use crate::services::*;
use chrono::Utc;
use sea_orm::{
    sea_query::Expr, ActiveModelTrait, ColumnTrait, EntityTrait, QueryFilter, QueryOrder,
    QuerySelect, Set, TransactionTrait,
};
use std::collections::HashMap;
use std::sync::Arc;
use temps_entities::{external_services, nodes, service_members};
use tracing::{debug, error, info, warn};

/// Validated, fully-resolved input for the background member-creation
/// task. Built once by `plan_add_cluster_member` and handed off to the
/// spawned task; nothing inside it requires a DB lookup, so the task
/// never has to revalidate.
#[derive(Clone)]
struct AddMemberPlan {
    service_id: i32,
    spec: ClusterMemberSpec,
    container_name: String,
    member_fqdn: String,
    member_port: u16,
    member_params: crate::externalsvc::postgres_cluster::ClusterMemberCreateParams,
    /// Where a control-plane member publishes its port for worker members,
    /// in addition to loopback. `None` while the cluster is single-host.
    control_plane_address: Option<std::net::IpAddr>,
}

impl ExternalServiceManager {
    /// Get all members for a cluster service.
    pub async fn get_service_members(
        &self,
        service_id: i32,
    ) -> Result<Vec<ServiceMemberInfo>, ExternalServiceError> {
        let members = service_members::Entity::find()
            .filter(service_members::Column::ServiceId.eq(service_id))
            .order_by_asc(service_members::Column::Ordinal)
            .all(self.db.as_ref())
            .await?;

        Ok(members
            .into_iter()
            .map(|m| ServiceMemberInfo {
                id: m.id,
                role: m.role,
                node_id: m.node_id,
                container_name: m.container_name,
                hostname: m.hostname,
                port: m.port,
                status: m.status,
                ordinal: m.ordinal,
                compute_ip: m.compute_ip,
                provisioning_step: m.provisioning_step,
                provisioning_error: m.provisioning_error,
                // Pure DB read — monitor enrichment is the caller's
                // responsibility via `get_service_members_with_live_state`.
                // Cheap callers (cluster_health, reconciler) avoid the
                // extra network round-trip.
                live_state: None,
            })
            .collect())
    }

    /// Resolve a persisted cluster member to the control plane endpoint that
    /// was authorized during provisioning.
    ///
    /// Monitor rows are deliberately not accepted here. The monitor is queried
    /// over self-signed (unverified) TLS and can report arbitrary
    /// `nodehost`/`nodeport` values if that channel is forged. Those values are
    /// health data, not authorization to send the cluster password somewhere.
    pub(super) async fn stored_member_endpoint(
        &self,
        service_id: i32,
        member: &ServiceMemberInfo,
    ) -> Result<(String, u16), ExternalServiceError> {
        let raw_port =
            member
                .port
                .ok_or_else(|| ExternalServiceError::ParameterValidationFailed {
                    service_id,
                    reason: format!(
                        "Persisted cluster member '{}' has no authorized TCP port",
                        member.container_name
                    ),
                })?;
        let port = u16::try_from(raw_port)
            .ok()
            .filter(|port| *port != 0)
            .ok_or_else(|| ExternalServiceError::ParameterValidationFailed {
                service_id,
                reason: format!(
                    "Persisted port {} for cluster member '{}' is outside the valid TCP range 1-65535",
                    raw_port, member.container_name
                ),
            })?;

        let host = if let Some(node_id) = member.node_id {
            let node = nodes::Entity::find_by_id(node_id)
                .one(self.db.as_ref())
                .await
                .map_err(|error| ExternalServiceError::DatabaseError {
                    reason: format!(
                        "Failed to resolve node {} for cluster member '{}' in service {}: {}",
                        node_id, member.container_name, service_id, error
                    ),
                })?
                .ok_or_else(|| ExternalServiceError::InternalError {
                    reason: format!(
                        "Cannot resolve cluster member '{}' for service {}: node {} was not found",
                        member.container_name, service_id, node_id
                    ),
                })?;
            node.data_address().to_string()
        } else {
            // Local members publish their container port on the control-plane
            // host; Docker-internal names and addresses are not host-routable.
            LOCAL_CLUSTER_HOST.to_string()
        };

        Ok((host, port))
    }

    /// Find the live primary among a cluster's members by asking the
    /// monitor for the current FSM state.
    ///
    /// Returns `Ok(None)` when:
    ///   - the service isn't a cluster
    ///   - the monitor is unreachable (callers should treat this as
    ///     "primary unknown" rather than "no primary")
    ///   - the monitor knows of no node in a writable-primary state
    ///
    /// Replaces the old `members.iter().find(|m| m.role == "primary")`
    /// pattern, which broke the moment we stopped storing the primary
    /// designation in `service_members.role`.
    pub async fn find_live_primary_member<'a>(
        &self,
        service: &external_services::Model,
        members: &'a [temps_entities::service_members::Model],
    ) -> Result<Option<&'a temps_entities::service_members::Model>, ExternalServiceError> {
        if service.topology != "cluster" {
            return Ok(None);
        }
        let health = self.cluster_health(service).await;
        if health.monitor_error.is_some() {
            return Ok(None);
        }
        let Some(name) = healthy_writable_primary_nodename(&health) else {
            return Ok(None);
        };
        let mut matches = members.iter().filter(|member| {
            is_role_data_member(&member.role)
                && member.status == "running"
                && member.container_name == name
        });
        let member = matches.next();
        if matches.next().is_some() {
            return Ok(None);
        }
        Ok(member)
    }

    /// Live primary check: ask the pg_auto_failover monitor whether the
    /// given member is currently the writable node. Returns `Ok(false)`
    /// when the monitor is unreachable so admin actions don't get
    /// blocked by a flaky control plane — callers that need stronger
    /// guarantees should explicitly probe `cluster_health` first and
    /// surface `monitor_error` to the user.
    ///
    /// Use this for "is this the primary?" gates (e.g. block deletion,
    /// reject self-promotion). Don't use for the UI label — that path
    /// reads `ServiceMemberInfo.live_state` and shows the actual
    /// FSM state including transient ones like `wait_primary`.
    pub async fn member_is_live_primary(
        &self,
        service: &external_services::Model,
        member: &temps_entities::service_members::Model,
    ) -> Result<bool, ExternalServiceError> {
        if service.topology != "cluster" {
            return Ok(false);
        }
        let health = self.cluster_health(service).await;
        if health.monitor_error.is_some() {
            // Monitor is unreachable. Fall back to the persisted role label
            // so we still refuse to delete the node that was last known to
            // be primary — otherwise the "monitor down" branch turns
            // `remove_cluster_member` into an unconditional escape hatch
            // and can silently delete the writable node. The operator
            // override path is to first manually flip the role column or
            // run `pg_autoctl perform failover` once the monitor recovers.
            return Ok(is_role_primary(&member.role));
        }
        Ok(Self::primary_member_from_health(
            &health,
            &member.container_name,
        ))
    }

    /// Pure decision backing `member_is_live_primary`'s live-monitor
    /// branch: given an already-fetched health report and the container
    /// name being checked, decide whether that member is the writable
    /// primary right now. No I/O — kept as its own function so
    /// `remove_cluster_member`'s delete-protection gate can be exercised
    /// directly in tests (including `wait_primary`, which only a live
    /// pg_auto_failover monitor would otherwise report) without needing a
    /// real monitor connection.
    ///
    /// Uses `PgAutoFailoverState::is_primary` (not a hand-rolled string
    /// match) so this gate can't drift from the DNS reconciler's
    /// definition of "writable primary" again — that exact drift
    /// previously let a `wait_primary` node (promotion complete, no
    /// standby attached — genuinely writable, and the normal steady state
    /// a 2-node cluster settles into after failover) pass this check as
    /// "not the primary", which would have let `remove_cluster_member`
    /// delete the cluster's only writable node.
    fn primary_member_from_health(health: &ClusterHealthReport, container_name: &str) -> bool {
        health
            .members
            .iter()
            .find(|h| h.nodename == container_name)
            .is_some_and(|h| live_state_is_writable_primary(Some(&h.reported_state)))
    }

    /// Same shape as `get_service_members`, but for cluster topologies
    /// also queries the monitor and fills in `live_state` per member.
    ///
    /// Used by UI-facing endpoints. Falls back to the bare DB result if
    /// the monitor is unreachable so the page still renders — the UI
    /// then displays the stored `role` as a best-effort label.
    ///
    /// Cost: one extra `cluster_health` call (≤5s timeout). Don't use on
    /// the hot path.
    pub async fn get_service_members_with_live_state(
        &self,
        service_id: i32,
    ) -> Result<Vec<ServiceMemberInfo>, ExternalServiceError> {
        let mut members = self.get_service_members(service_id).await?;

        let service = match self.get_service(service_id).await {
            Ok(s) => s,
            Err(_) => return Ok(members),
        };
        if service.topology != "cluster" {
            return Ok(members);
        }

        // Monitor probe — best-effort. `cluster_health` already swallows
        // monitor errors and returns an empty `members` list, so we just
        // skip enrichment when that happens.
        let health = self.cluster_health(&service).await;
        if health.monitor_error.is_some() || health.members.is_empty() {
            return Ok(members);
        }

        // Index live state by container name (== `nodename` in the monitor
        // since the rename in postgres_cluster.rs::container_params).
        let live: HashMap<String, String> = health
            .members
            .into_iter()
            .map(|m| (m.nodename, m.reported_state))
            .collect();

        for member in members.iter_mut() {
            if member.is_monitor() {
                continue;
            }
            if let Some(state) = live.get(&member.container_name) {
                member.live_state = Some(state.clone());
            }
        }

        Ok(members)
    }

    /// Get the primary data node's connection address for a cluster service.
    ///
    /// Returns `Some((host, port))` if the service is a cluster with a running primary.
    /// Returns `None` if the service is standalone (not a cluster).
    ///
    /// For local clusters, `host` is the container name (Docker DNS).
    /// For remote clusters, `host` is the member's hostname (private/WireGuard IP).
    pub async fn get_cluster_primary_address(
        &self,
        service_id: i32,
    ) -> Result<Option<(String, u16)>, ExternalServiceError> {
        Ok(self
            .get_cluster_primary_endpoint(service_id)
            .await?
            .map(|(_, host, port)| (host, port)))
    }

    /// [`Self::get_cluster_primary_address`] plus the primary member's
    /// container name, which identifies the server across failovers.
    async fn get_cluster_primary_endpoint(
        &self,
        service_id: i32,
    ) -> Result<Option<(String, String, u16)>, ExternalServiceError> {
        let Some(primary) = self.get_cluster_primary_member(service_id).await? else {
            return Ok(None);
        };
        self.stored_member_endpoint(service_id, &primary)
            .await
            .map(|(host, port)| Some((primary.container_name.clone(), host, port)))
    }

    /// The member of an HA PostgreSQL cluster that pg_auto_failover currently
    /// reports as its healthy, writable primary.
    ///
    /// Returns `Ok(None)` when the service is not a cluster, and an error when
    /// it is one but no single primary can be trusted right now (monitor
    /// unreachable, failover in progress, stale reports).
    ///
    /// The primary is whichever node the monitor calls primary, never what
    /// `service_members.role` says: that column holds `replica` for every
    /// data node, so a lookup by stored role finds no primary at all, and
    /// before the rework it lagged behind every failover.
    pub async fn get_cluster_primary_member(
        &self,
        service_id: i32,
    ) -> Result<Option<ServiceMemberInfo>, ExternalServiceError> {
        let service = self.get_service(service_id).await?;
        if service.topology != "cluster" {
            return Ok(None);
        }

        let members = self.get_service_members(service_id).await?;
        let health = self.cluster_health(&service).await;
        if let Some(primary) = healthy_writable_primary_nodename(&health)
            .and_then(|nodename| trusted_primary_member(&members, nodename))
        {
            return Ok(Some(primary.clone()));
        }

        let reason = match &health.monitor_error {
            Some(monitor_error) => format!(
                "Cluster service {} '{}' has no known primary: its pg_auto_failover monitor could not be read: {}",
                service_id, service.name, monitor_error
            ),
            None => format!(
                "Cluster service {} '{}' has no unique healthy, recently reporting primary data node (monitor reports: {})",
                service_id,
                service.name,
                describe_monitor_states(&health)
            ),
        };
        Err(ExternalServiceError::InternalError { reason })
    }

    /// Connect to an HA PostgreSQL cluster's current primary as its
    /// application user.
    ///
    /// The primary is the one pg_auto_failover elected, resolved back to its
    /// stored member endpoint (see [`Self::get_cluster_primary_address`]), and
    /// is dialled through the private-only TLS ladder: TLS is required, and an
    /// unverified certificate or cleartext is only ever accepted on a private
    /// address. Used by the metrics scraper, whose stored `host`/`port` for a
    /// cluster point at no member.
    ///
    /// Returns the client and the primary member's container name.
    pub async fn connect_cluster_primary_client(
        &self,
        service_id: i32,
    ) -> Result<(tokio_postgres::Client, String), ExternalServiceError> {
        let (member, host, port) = self.get_cluster_primary_endpoint(service_id).await?.ok_or(
            ExternalServiceError::ParameterValidationFailed {
                service_id,
                reason: format!("Service {service_id} is not a cluster; it has no primary member"),
            },
        )?;
        let params = self.get_service_parameters(service_id).await?;
        let param = |key: &str, default: &str| {
            params
                .get(key)
                .and_then(|v| v.as_str())
                .filter(|v| !v.is_empty())
                .unwrap_or(default)
                .to_string()
        };
        let user = param("username", "postgres");
        let password = param("password", "");
        let database = param("database", "postgres");

        let client = temps_query_postgres::connect_with_private_tls_ladder(
            &host, port, &user, &password, &database,
        )
        .await
        .map_err(|error| ExternalServiceError::InternalError {
            reason: format!(
                "Failed to connect to the primary of cluster {} ({}) at {}:{} (database '{}'): {}",
                service_id, member, host, port, database, error
            ),
        })?;
        Ok((client, member))
    }

    /// Begin adding a single new member (currently only `replica`) to a
    /// running Postgres cluster.
    ///
    /// **Returns immediately** after validating the request, resolving the
    /// existing monitor, and inserting a `service_members` row with
    /// `status='creating'` and `provisioning_step='inserting_row'`. The
    /// long-running container provisioning + DNS registration runs in a
    /// background tokio task that updates `provisioning_step` (and
    /// eventually `status='running'` / `status='failed'` +
    /// `provisioning_error`) so the UI can render a live timeline by
    /// polling the member row.
    ///
    /// Refuses `monitor` (singleton — created once at init) and
    /// `primary` (elected by pg_auto_failover, never declared by the user).
    pub async fn add_cluster_member(
        self: &Arc<Self>,
        service_id: i32,
        role: &str,
        node_id: Option<i32>,
    ) -> Result<ServiceMemberInfo, ExternalServiceError> {
        // Race-resilient insert. Two concurrent `add_cluster_member`
        // calls that observe the same `MAX(ordinal)` would each compute
        // the same next ordinal and try to insert the same
        // `(service_id, ordinal)` row. The unique constraint added in
        // m20260428_000001 makes the second insert fail; we recompute
        // the plan (which re-derives container_name + port + FQDN from
        // the new ordinal) and try again. Bounded at 8 attempts because
        // an explosion past that means something else is wrong.
        const MAX_ORDINAL_RETRIES: usize = 8;
        let (plan, member_model) = {
            let mut last_err = None;
            let mut chosen_plan: Option<AddMemberPlan> = None;
            let mut chosen_model: Option<service_members::Model> = None;
            for attempt in 0..MAX_ORDINAL_RETRIES {
                let plan = self
                    .plan_add_cluster_member(service_id, role, node_id)
                    .await?;
                let now = Utc::now();
                // See note in `initialize_cluster`: data members are stored
                // as `replica`. Promotion is a runtime concern owned by the
                // pg_auto_failover monitor and surfaced via `live_state`.
                let stored_role = if is_role_monitor(&plan.spec.role) {
                    "monitor".to_string()
                } else {
                    "replica".to_string()
                };
                let member_record = service_members::ActiveModel {
                    service_id: Set(service_id),
                    node_id: Set(plan.spec.node_id),
                    role: Set(stored_role),
                    container_id: Set(None),
                    container_name: Set(plan.container_name.clone()),
                    hostname: Set(plan.spec.hostname.clone()),
                    port: Set(None),
                    status: Set("creating".to_string()),
                    ordinal: Set(plan.spec.ordinal),
                    config: Set(None),
                    provisioning_step: Set(Some(
                        member_provisioning_step::INSERTING_ROW.to_string(),
                    )),
                    provisioning_error: Set(None),
                    created_at: Set(now),
                    updated_at: Set(now),
                    ..Default::default()
                };
                // The status check in `plan_add_cluster_member` is repeated
                // under the cluster's row lock, in the transaction that
                // inserts the member. A Stop claims the cluster under the
                // same lock (`claim_cluster`), so it either sees this member
                // or has already moved the cluster to `stopping` and this add
                // is refused. Never a member the Stop did not see.
                let lock = self.db.begin().await?;
                let checked = async {
                    let status = external_services::Entity::find_by_id(service_id)
                        .lock_exclusive()
                        .one(&lock)
                        .await?
                        .ok_or(ExternalServiceError::ServiceNotFound { id: service_id })?
                        .status;
                    if status != "running" {
                        return Err(ExternalServiceError::ParameterValidationFailed {
                            service_id,
                            reason: format!(
                                "Cluster must be in 'running' status to add a member, current: '{}'",
                                status
                            ),
                        });
                    }
                    Ok(())
                }
                .await;
                if let Err(error) = checked {
                    return Err(rollback_before_error(lock, error).await);
                }
                match member_record.insert(&lock).await {
                    Ok(model) => {
                        lock.commit().await?;
                        chosen_plan = Some(plan);
                        chosen_model = Some(model);
                        break;
                    }
                    // The failed insert is rolled back before the next attempt.
                    Err(e) if is_unique_violation(&e) => {
                        if let Err(rollback_error) = lock.rollback().await {
                            warn!(
                                service_id,
                                rollback_error = %rollback_error,
                                "Could not roll back a colliding cluster member insert"
                            );
                        }
                        // Another `add_cluster_member` won this ordinal.
                        // Loop and recompute against the now-larger
                        // member set.
                        warn!(
                            service_id,
                            attempted_ordinal = plan.spec.ordinal,
                            attempt = attempt + 1,
                            "Ordinal collision on cluster member insert; retrying with next free ordinal"
                        );
                        last_err = Some(e);
                        continue;
                    }
                    Err(e) => return Err(rollback_before_error(lock, e.into()).await),
                }
            }
            match (chosen_plan, chosen_model) {
                (Some(p), Some(m)) => (p, m),
                _ => {
                    return Err(ExternalServiceError::InternalError {
                        reason: format!(
                            "Failed to allocate a unique cluster member ordinal after {} attempts: {}",
                            MAX_ORDINAL_RETRIES,
                            last_err
                                .map(|e| e.to_string())
                                .unwrap_or_else(|| "no error captured".to_string())
                        ),
                    });
                }
            }
        };
        let member_id = member_model.id;

        // Spawn the long-running provisioning task. It owns its own Arc
        // clone of the manager so it can run independently of the request.
        let manager = self.clone();
        let plan_for_task = plan.clone();
        tokio::spawn(async move {
            manager
                .complete_add_cluster_member(member_id, plan_for_task)
                .await;
        });

        info!(
            service_id,
            member_id,
            ordinal = plan.spec.ordinal,
            "Cluster member provisioning started — see member.provisioning_step for live status"
        );

        Ok(ServiceMemberInfo {
            id: member_model.id,
            role: member_model.role,
            node_id: member_model.node_id,
            container_name: member_model.container_name,
            hostname: member_model.hostname,
            port: member_model.port,
            status: member_model.status,
            ordinal: member_model.ordinal,
            compute_ip: member_model.compute_ip,
            provisioning_step: member_model.provisioning_step,
            provisioning_error: member_model.provisioning_error,
            // Just-created members never have an FSM state to report
            // yet. The next polling cycle picks it up.
            live_state: None,
        })
    }

    /// Validate the add-member request and resolve everything needed by
    /// the background provisioner. Anything that should fail synchronously
    /// (returning a 400 to the user) belongs here.
    async fn plan_add_cluster_member(
        &self,
        service_id: i32,
        role: &str,
        node_id: Option<i32>,
    ) -> Result<AddMemberPlan, ExternalServiceError> {
        info!(
            service_id,
            role,
            node_id = ?node_id,
            "Adding cluster member (validating)"
        );

        let service = self.get_service(service_id).await?;

        if service.topology != "cluster" {
            return Err(ExternalServiceError::ParameterValidationFailed {
                service_id,
                reason: "add_cluster_member is only valid for cluster topology services"
                    .to_string(),
            });
        }
        if service.status != "running" {
            return Err(ExternalServiceError::ParameterValidationFailed {
                service_id,
                reason: format!(
                    "Cluster must be in 'running' status to add a member, current: '{}'",
                    service.status
                ),
            });
        }

        if role_from_str(role) != Some(crate::ClusterRole::Replica) {
            return Err(ExternalServiceError::ParameterValidationFailed {
                service_id,
                reason: format!(
                    "Only 'replica' members can be added at runtime (got '{}'). \
                     Monitor is a singleton; primary is elected by pg_auto_failover.",
                    role
                ),
            });
        }

        let service_type = ServiceType::from_str(&service.service_type).map_err(|_| {
            ExternalServiceError::InvalidServiceType {
                id: service_id,
                service_type: service.service_type.clone(),
            }
        })?;

        let pg_cluster = match service_type {
            ServiceType::Postgres => {
                let docker = self.require_docker()?;
                PostgresClusterService::new(service.name.clone(), docker)
            }
            _ => {
                return Err(ExternalServiceError::ParameterValidationFailed {
                    service_id,
                    reason: format!(
                        "add_cluster_member is only supported for Postgres clusters (got '{}')",
                        service.service_type
                    ),
                });
            }
        };

        let existing_members = service_members::Entity::find()
            .filter(service_members::Column::ServiceId.eq(service_id))
            .order_by_asc(service_members::Column::Ordinal)
            .all(self.db.as_ref())
            .await?;

        let monitor = existing_members
            .iter()
            .find(|m| is_role_monitor(&m.role))
            .ok_or(ExternalServiceError::InitializationFailed {
                id: service_id,
                reason: "Cannot add member: cluster has no monitor".to_string(),
            })?;

        // ADR 056: refuse a node without the cross-node overlay when the
        // cluster spans hosts once this member is added.
        let placements: Vec<Option<i32>> = existing_members
            .iter()
            .map(|m| m.node_id)
            .chain(std::iter::once(
                node_id.filter(|id| *id != Self::CONTROL_PLANE_NODE_ID),
            ))
            .collect();
        let placement_nodes: Vec<i32> = placements.iter().flatten().copied().collect();
        let candidates = nodes::Entity::find()
            .filter(nodes::Column::Id.is_in(placement_nodes))
            .all(self.db.as_ref())
            .await?;
        Self::refuse_nodes_without_overlay(service_id, &placements, &candidates)?;

        // What address should the member being added dial to reach the
        // monitor? NOT simply "whatever's in `monitor.hostname`": that
        // field only reflects the topology `has_remote_members` decided at
        // the *cluster's* creation time (see `resolve_member_hostname`) and
        // is never retroactively recomputed — for a cluster created
        // all-local it stays the monitor's plain Docker container name
        // forever, even after this exact call adds the cluster's first
        // remote member. A plain container name only resolves via Docker's
        // embedded DNS on the monitor's own host, so trusting it blindly
        // here would hand a cross-host member an address it can never
        // reach.
        //
        // Derive reachability from the actual node topology of *this* add
        // instead — it can't go stale the way a persisted string can:
        //   - monitor is on a remote node: always need its real underlay
        //     address, regardless of where the new member lands.
        //   - monitor is local but the new member is remote: the new
        //     member needs this control-plane host's private IP, not the
        //     monitor's container name (unreachable from another host).
        //   - both local: same Docker host, so whatever's already
        //     persisted (container name, or FQDN if the cluster happens to
        //     be DNS-enabled) resolves natively.
        let monitor_hostname: String =
            match Self::monitor_reachability_for_add(monitor.node_id, node_id) {
                MonitorReachability::MonitorNode(nid) => {
                    let node = nodes::Entity::find_by_id(nid)
                        .one(self.db.as_ref())
                        .await?
                        .ok_or(ExternalServiceError::InternalError {
                            reason: format!("Monitor's node {} not found", nid),
                        })?;
                    node.data_address().to_string()
                }
                MonitorReachability::LocalControlPlane => self
                    .control_plane_member_address(&service.name)
                    .await?
                    .to_string(),
                MonitorReachability::SameHost => monitor
                    .hostname
                    .clone()
                    .unwrap_or_else(|| format!("postgres-{}-monitor", service.name)),
            };
        let monitor_port = monitor
            .port
            .and_then(|port| u16::try_from(port).ok())
            .ok_or(ExternalServiceError::InitializationFailed {
                id: service_id,
                reason: format!(
                    "Monitor '{}' has no valid host port recorded ({:?})",
                    monitor.container_name, monitor.port
                ),
            })?;

        // Reuse the lowest free ordinal (≥ 1 — 0 is reserved for the
        // monitor) so that delete-then-add gives the operator back the
        // same node identity (e.g. node-2 stays node-2). Falling through
        // to MAX+1 here meant a removed node-2 would come back as node-4
        // and pg_auto_failover treated the original :6152 ghost as a new
        // peer, blocking the FSM. Together with the
        // `pg_autoctl drop node` call in `remove_cluster_member`, this
        // makes delete+add idempotent from the cluster's point of view.
        let used_ordinals: std::collections::BTreeSet<i32> =
            existing_members.iter().map(|m| m.ordinal).collect();
        let mut next_ordinal: i32 = 1;
        while used_ordinals.contains(&next_ordinal) {
            next_ordinal += 1;
        }

        let has_any_remote =
            existing_members.iter().any(|m| m.node_id.is_some()) || node_id.is_some();
        let has_any_local =
            existing_members.iter().any(|m| m.node_id.is_none()) || node_id.is_none();
        let control_plane_address: Option<std::net::IpAddr> = if has_any_remote && has_any_local {
            Some(self.control_plane_member_address(&service.name).await?)
        } else {
            None
        };

        // A worker member must be able to reach every control-plane member
        // (the monitor to register, a local primary to replicate from). A
        // control-plane member created while the cluster was single-host —
        // or by a release that only ever published on loopback — is not
        // reachable from a worker, and adding one would just produce a member
        // that never registers. Refuse up front with the reason.
        if let (Some(address), Some(_)) = (control_plane_address, node_id) {
            for local in existing_members.iter().filter(|m| m.node_id.is_none()) {
                let Some(port) = local.port.and_then(|p| u16::try_from(p).ok()) else {
                    continue;
                };
                if !self
                    .local_member_published_on(&local.container_name, port, address)
                    .await?
                {
                    return Err(ExternalServiceError::ControlPlaneMemberUnreachable {
                        name: service.name.clone(),
                        container_name: local.container_name.clone(),
                        port,
                        address: address.to_string(),
                    });
                }
            }
        }
        let local_private_ip: Option<String> = match node_id {
            None => control_plane_address.map(|ip| ip.to_string()),
            Some(_) => None,
        };

        let hostname: Option<String> = if let Some(nid) = node_id {
            let node = nodes::Entity::find_by_id(nid)
                .one(self.db.as_ref())
                .await?
                .ok_or(ExternalServiceError::InternalError {
                    reason: format!("Node {} not found", nid),
                })?;
            Some(node.data_address().to_string())
        } else {
            local_private_ip
        };

        let spec = ClusterMemberSpec {
            role: role.to_string(),
            node_id,
            ordinal: next_ordinal,
            hostname,
        };

        // A member added to a cluster created before SCRAM auth must not be
        // the first SCRAM member: once it is primary, the existing members
        // would have no password for it. Upgrade the running members first.
        let (parameters, cluster_auth, _) = self.ensure_cluster_auth_secrets(service_id).await?;
        // Stored credentials alone do not prove an interrupted upgrade converged.
        self.prepare_cluster_auth_for_new_member(service_id).await?;
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
        let cluster_config: crate::externalsvc::postgres_cluster::PostgresClusterConfig =
            serde_json::from_value(service_config.parameters.clone()).map_err(|e| {
                ExternalServiceError::InternalError {
                    reason: format!("Failed to parse cluster config: {}", e),
                }
            })?;

        // Inherit cluster-wide resource limits when adding a new member so
        // every node ends up with the same caps as the rest of the cluster.
        let member_limits =
            crate::externalsvc::ServiceResourceLimits::from_parameters(&service_config.parameters);
        if let Err(e) = member_limits.validate() {
            return Err(ExternalServiceError::InternalError {
                reason: format!("Invalid cluster resource limits: {}", e),
            });
        }

        // The monitor sits at the base of the cluster's port block, so the
        // block is read from it rather than recomputed from the service id.
        let member_port = cluster_member_port(monitor_port, spec.ordinal).ok_or(
            ExternalServiceError::ClusterPortsUnavailable {
                service_id,
                reason: format!(
                    "member ordinal {} does not fit in the cluster's {}-port block starting at \
                     {}; a cluster has at most {} members",
                    spec.ordinal, CLUSTER_PORT_BLOCK_SIZE, monitor_port, CLUSTER_PORT_BLOCK_SIZE
                ),
            },
        )?;

        let member_params = pg_cluster.build_member_params(
            &spec,
            &cluster_config,
            &monitor_hostname,
            monitor_port,
            member_port,
            member_limits,
            &cluster_auth,
        );
        let container_name = member_params.container_name.clone();
        let member_fqdn = format!(
            "{}-{}.{}.temps.local",
            service.name, spec.ordinal, service.name
        );

        Ok(AddMemberPlan {
            service_id,
            spec,
            container_name,
            member_fqdn,
            member_port,
            member_params,
            control_plane_address: node_id.is_none().then_some(control_plane_address).flatten(),
        })
    }

    /// Background half of `add_cluster_member`. Owns the long-running
    /// container creation + DNS registration. Updates the row's
    /// `provisioning_step` after each phase so the UI's polling loop
    /// can render progress.
    async fn complete_add_cluster_member(self: Arc<Self>, member_id: i32, plan: AddMemberPlan) {
        let service_id = plan.service_id;
        let ordinal = plan.spec.ordinal;

        info!(
            service_id,
            member_id,
            ordinal,
            container = %plan.container_name,
            "Provisioning replica container"
        );
        // Fence the irreversible step: the container is only created while
        // the member row still exists. A row deleted meanwhile (its cluster
        // was deleted) means nothing must be created for it.
        match self
            .claim_provisioning_step(member_id, member_provisioning_step::PROVISIONING_CONTAINER)
            .await
        {
            Ok(true) => {}
            Ok(false) => {
                info!(
                    service_id,
                    member_id,
                    container = %plan.container_name,
                    "Cluster member was deleted before its container was created; not creating it"
                );
                return;
            }
            Err(e) => {
                self.fail_member(
                    member_id,
                    format!("Could not record the provisioning step: {}", e),
                )
                .await;
                return;
            }
        }

        let create_outcome: Result<(String, Option<i32>, Option<String>), ExternalServiceError> =
            if let Some(nid) = plan.spec.node_id {
                let client = match self.get_remote_client(nid).await {
                    Ok(c) => c,
                    Err(e) => {
                        self.fail_member(
                            member_id,
                            format!(
                                "Could not reach worker node {} to provision container: {}",
                                nid, e
                            ),
                        )
                        .await;
                        return;
                    }
                };
                let volume_name = format!("{}_data", plan.container_name);
                let limits_for_remote = if plan.member_params.resource_limits.is_unlimited() {
                    None
                } else {
                    Some(plan.member_params.resource_limits.clone())
                };
                let remote_params = RemoteServiceCreateParams {
                    name: plan.container_name.clone(),
                    service_type: "postgres".to_string(),
                    image: plan.member_params.image.clone(),
                    environment: plan.member_params.environment.clone(),
                    port_mappings: vec![RemotePortMapping {
                        host_port: plan.member_params.container_port,
                        container_port: plan.member_params.container_port,
                    }],
                    volumes: HashMap::from([(volume_name, plan.member_params.volume_path.clone())]),
                    network: Some(temps_core::NETWORK_NAME.to_string()),
                    command: plan.member_params.command.clone(),
                    resource_limits: limits_for_remote,
                };
                client
                    .create_service(remote_params)
                    .await
                    .map(|r| (r.container_id, Some(r.host_port as i32), r.compute_ip))
                    .map_err(|e| ExternalServiceError::InitializationFailed {
                        id: service_id,
                        reason: format!(
                            "Failed to create cluster member '{}' on node {}: {}",
                            plan.container_name, nid, e
                        ),
                    })
            } else {
                self.create_local_cluster_member(
                    &plan.container_name,
                    &plan.member_params,
                    plan.control_plane_address,
                )
                .await
                .map_err(|e| ExternalServiceError::InitializationFailed {
                    id: service_id,
                    reason: format!(
                        "Failed to create local cluster member '{}': {}",
                        plan.container_name, e
                    ),
                })
            };

        let (container_id, host_port, compute_ip) = match create_outcome {
            Ok(t) => t,
            Err(e) => {
                self.fail_member(member_id, e.to_string()).await;
                return;
            }
        };

        // Promote the row to "running" with the live container metadata.
        let updated_at = Utc::now();
        let update_result = service_members::Entity::update_many()
            .col_expr(
                service_members::Column::ContainerId,
                Expr::value(container_id),
            )
            .col_expr(service_members::Column::Port, Expr::value(host_port))
            .col_expr(service_members::Column::Status, Expr::value("running"))
            .col_expr(
                service_members::Column::Hostname,
                Expr::value(plan.member_fqdn.clone()),
            )
            .col_expr(
                service_members::Column::ComputeIp,
                Expr::value(compute_ip.clone()),
            )
            .col_expr(
                service_members::Column::ProvisioningStep,
                Expr::value(member_provisioning_step::REGISTERING_DNS),
            )
            .col_expr(service_members::Column::UpdatedAt, Expr::value(updated_at))
            .filter(service_members::Column::Id.eq(member_id))
            .exec(self.db.as_ref())
            .await;
        match update_result {
            Ok(result) if result.rows_affected == 0 => {
                // The row was deleted while the container was being created
                // (its cluster was deleted): nothing tracks this container,
                // so it would run, restart and keep its volume forever.
                warn!(
                    service_id,
                    member_id,
                    container = %plan.container_name,
                    "Cluster member was deleted while its container was being created; \
                     removing the container and its volume"
                );
                self.discard_member_container(&plan).await;
                return;
            }
            Ok(_) => {}
            Err(e) => {
                self.fail_member(
                    member_id,
                    format!("Container created but DB update failed: {}", e),
                )
                .await;
                return;
            }
        }

        // Register Tier-2 DNS A record using the same topology-aware
        // selection as initial cluster creation.
        let (record_ip, record_port) = self
            .resolve_member_dns_endpoint(
                plan.spec.node_id,
                compute_ip.as_deref(),
                &plan.container_name,
                host_port,
                plan.member_port,
            )
            .await
            .map(|(ip, port)| (Some(ip), port))
            .unwrap_or((None, plan.member_port as i32));
        if let Some(ip) = record_ip {
            let draft = temps_dns::EndpointDraft {
                fqdn: plan.member_fqdn.clone(),
                record_type: temps_dns::InternalRecordType::A,
                target_ip: Some(ip.clone()),
                target_port: Some(record_port),
                ttl: 30,
                owner_kind: temps_dns::InternalOwnerKind::ServiceMember,
                owner_id: member_id as i64,
                node_id: plan.spec.node_id,
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
                    fqdn = %plan.member_fqdn,
                    ip = %ip,
                    error = %e,
                    "Failed to register DNS record for added cluster member"
                );
            } else {
                info!(
                    service_id,
                    member_id,
                    fqdn = %plan.member_fqdn,
                    ip = %ip,
                    port = record_port,
                    "Registered DNS A record for added cluster member"
                );
            }
        }

        if let Ok(false) = self
            .claim_provisioning_step(member_id, member_provisioning_step::DONE)
            .await
        {
            // Removed while its DNS record was being registered: the removal
            // already deleted the container and the row, so the record just
            // written would point at nothing.
            if let Err(e) = self
                .dns_registry
                .delete_by_owner(
                    temps_dns::InternalOwnerKind::ServiceMember,
                    member_id as i64,
                )
                .await
            {
                warn!(
                    service_id,
                    member_id,
                    error = %e,
                    "Failed to drop the DNS record of a cluster member removed during provisioning"
                );
            }
            return;
        }
        info!(
            service_id,
            member_id,
            ordinal,
            "Cluster member added; reconciler will refresh role records on next tick"
        );
    }

    /// Record `step` on the member row. `Ok(false)` when the row no longer
    /// exists, which is what fences each irreversible provisioning step: the
    /// update is a single statement, so it either lands on the live row or
    /// tells the task the member is gone.
    pub(super) async fn claim_provisioning_step(
        &self,
        member_id: i32,
        step: &str,
    ) -> Result<bool, sea_orm::DbErr> {
        let result = service_members::Entity::update_many()
            .col_expr(service_members::Column::ProvisioningStep, Expr::value(step))
            .col_expr(service_members::Column::UpdatedAt, Expr::value(Utc::now()))
            .filter(service_members::Column::Id.eq(member_id))
            .exec(self.db.as_ref())
            .await?;
        Ok(result.rows_affected > 0)
    }

    /// Remove the container (and its data volume) that provisioning created
    /// for a member whose row is gone. Best effort, logged: there is no row
    /// left to record a failure on.
    async fn discard_member_container(&self, plan: &AddMemberPlan) {
        let service_id = plan.service_id;
        if let Some(node_id) = plan.spec.node_id {
            match self.get_remote_client(node_id).await {
                Ok(client) => {
                    if let Err(e) = client.remove_service(&plan.container_name).await {
                        error!(
                            service_id,
                            node_id,
                            container = %plan.container_name,
                            error = %e,
                            "Failed to remove the container of a deleted cluster member on its worker"
                        );
                    }
                }
                Err(e) => error!(
                    service_id,
                    node_id,
                    container = %plan.container_name,
                    error = %e,
                    "Could not reach the worker to remove the container of a deleted cluster member"
                ),
            }
            return;
        }
        let Some(docker) = self.docker.get() else {
            return;
        };
        if let Err(e) = docker
            .remove_container(
                &plan.container_name,
                Some(bollard::query_parameters::RemoveContainerOptions {
                    force: true,
                    ..Default::default()
                }),
            )
            .await
        {
            error!(
                service_id,
                container = %plan.container_name,
                error = %e,
                "Failed to remove the container of a deleted cluster member"
            );
        }
        let volume_name = format!("{}_data", plan.container_name);
        if let Err(e) = docker
            .remove_volume(
                &volume_name,
                None::<bollard::query_parameters::RemoveVolumeOptions>,
            )
            .await
        {
            warn!(
                service_id,
                volume = %volume_name,
                error = %e,
                "Failed to remove the data volume of a deleted cluster member"
            );
        }
    }

    /// Mark the member as failed and stash the error message so the UI
    /// can render it. Best-effort: a DB write failure here is logged but
    /// can't be recovered from.
    async fn fail_member(&self, member_id: i32, error_message: String) {
        warn!(
            member_id,
            error = %error_message,
            "Cluster member provisioning failed"
        );
        let result = service_members::Entity::update_many()
            .col_expr(service_members::Column::Status, Expr::value("failed"))
            .col_expr(
                service_members::Column::ProvisioningStep,
                Expr::value(member_provisioning_step::FAILED),
            )
            .col_expr(
                service_members::Column::ProvisioningError,
                Expr::value(error_message),
            )
            .col_expr(service_members::Column::UpdatedAt, Expr::value(Utc::now()))
            .filter(service_members::Column::Id.eq(member_id))
            .exec(self.db.as_ref())
            .await;
        if let Err(e) = result {
            warn!(member_id, error = %e, "Failed to mark member as failed");
        }
    }

    /// Look up a single cluster member. Returns `NotFound` if the row
    /// doesn't belong to the named service so callers can return a 404
    /// without leaking the existence of unrelated members.
    pub async fn get_cluster_member(
        &self,
        service_id: i32,
        member_id: i32,
    ) -> Result<ServiceMemberInfo, ExternalServiceError> {
        let member = service_members::Entity::find_by_id(member_id)
            .one(self.db.as_ref())
            .await?
            .filter(|m| m.service_id == service_id)
            .ok_or(ExternalServiceError::InitializationFailed {
                id: service_id,
                reason: format!("Cluster member {} not found", member_id),
            })?;

        Ok(ServiceMemberInfo {
            id: member.id,
            role: member.role,
            node_id: member.node_id,
            container_name: member.container_name,
            hostname: member.hostname,
            port: member.port,
            status: member.status,
            ordinal: member.ordinal,
            compute_ip: member.compute_ip,
            provisioning_step: member.provisioning_step,
            provisioning_error: member.provisioning_error,
            // Single-member fetch path. Callers that need live state for
            // a single member should use `member_is_live_primary` or
            // `get_service_members_with_live_state` instead.
            live_state: None,
        })
    }

    /// Remove a single member from a running cluster.
    ///
    /// Safety guarantees (this function refuses to proceed unless they hold):
    ///   * The member must belong to the named service.
    ///   * The member must not be the `monitor` (singleton — would orphan
    ///     every data node).
    ///   * The member must not be the current `primary` (caller must
    ///     trigger a failover via pg_auto_failover first; we never
    ///     forcibly demote a writable primary).
    ///   * The remaining data members (excluding the monitor) must still
    ///     have at least 2 entries — anything fewer drops below quorum
    ///     and the cluster loses HA.
    ///
    /// Steps:
    ///   1. Stop + remove the container (local Docker or remote agent).
    ///   2. Delete the `service_members` row.
    ///   3. Drop the Tier-2 DNS A record for the member (best-effort).
    ///   4. Reconciler will refresh role records on its next tick.
    ///
    /// Also runs `pg_autoctl drop node --formation default --name node-N`
    /// against the monitor before deleting the row. Skipping that call
    /// leaves an orphan node registered with the monitor that will be
    /// asked to participate in quorum decisions (e.g. report_lsn during
    /// failover) and never respond, which deadlocks the FSM. The drop is
    /// best-effort — if the monitor is unreachable we still tear down the
    /// container + DB row, but log loudly so the operator can clean up.
    pub async fn remove_cluster_member(
        &self,
        service_id: i32,
        member_id: i32,
    ) -> Result<(), ExternalServiceError> {
        info!(service_id, member_id, "Removing cluster member");

        let service = self.get_service(service_id).await?;

        if service.topology != "cluster" {
            return Err(ExternalServiceError::ParameterValidationFailed {
                service_id,
                reason: "remove_cluster_member is only valid for cluster topology services"
                    .to_string(),
            });
        }

        let member = service_members::Entity::find_by_id(member_id)
            .one(self.db.as_ref())
            .await?
            .ok_or(ExternalServiceError::InitializationFailed {
                id: service_id,
                reason: format!("Cluster member {} not found", member_id),
            })?;

        if member.service_id != service_id {
            return Err(ExternalServiceError::ParameterValidationFailed {
                service_id,
                reason: format!(
                    "Member {} does not belong to service {} (it belongs to service {})",
                    member_id, service_id, member.service_id
                ),
            });
        }

        if is_role_monitor(&member.role) {
            return Err(ExternalServiceError::ParameterValidationFailed {
                service_id,
                reason: "Cannot remove the monitor — it is required for cluster operation"
                    .to_string(),
            });
        }
        // A member still `creating` has a provisioning task that has not yet
        // recorded its container. Deleting the row now would let that task
        // create the container afterwards, untracked by anything (#1385).
        // The task reaches `running` or `failed` on its own, and a control
        // plane restart moves an abandoned `creating` row to `failed`, so
        // this never blocks removal for good.
        if member.status == "creating" {
            return Err(ExternalServiceError::ClusterMemberProvisioning {
                service_id,
                action: "remove a member from",
                container_name: member.container_name.clone(),
                status: format!(
                    "creating, step '{}'",
                    member.provisioning_step.as_deref().unwrap_or("unknown")
                ),
            });
        }
        // Block removal of whichever node pg_auto_failover *currently*
        // calls primary, regardless of what `service_members.role` says
        // (which is now always `replica` for data members — see the
        // initialize_cluster comment). If the monitor is unreachable we
        // allow the delete, since the operator likely needs an escape
        // hatch in that exact scenario.
        if self.member_is_live_primary(&service, &member).await? {
            return Err(ExternalServiceError::ParameterValidationFailed {
                service_id,
                reason: "Cannot remove the current primary. \
                         Trigger a failover first (pg_autoctl perform failover) \
                         so a replica is promoted, then remove this node once it has \
                         been demoted to a replica or has gone offline."
                    .to_string(),
            });
        }

        // Quorum check: pg_auto_failover needs at least 2 data members
        // (one primary + one replica) to keep HA. Removing this member
        // must not leave fewer than 2.
        let all_members = service_members::Entity::find()
            .filter(service_members::Column::ServiceId.eq(service_id))
            .all(self.db.as_ref())
            .await?;
        let data_member_count = all_members
            .iter()
            .filter(|m| !is_role_monitor(&m.role))
            .count();
        if data_member_count <= 2 {
            return Err(ExternalServiceError::ParameterValidationFailed {
                service_id,
                reason: format!(
                    "Refusing to remove member: cluster has only {} data member(s); \
                     removing this one would drop the cluster below the 2-member \
                     quorum required for HA. Add a replica first, then remove.",
                    data_member_count
                ),
            });
        }

        // 1. Drop the node from pg_auto_failover *first*. If we delete the
        //    container before this, pg_autoctl on the monitor will treat
        //    the node as unreachable but still expect it to participate
        //    in quorum (e.g. report_lsn during a later failover), wedging
        //    the FSM. Best-effort: a monitor that's down shouldn't block
        //    user-initiated cleanup, but we want loud logs.
        //
        // The pg_autoctl node name is the docker container name (set in
        // `PostgresClusterService::container_params`), so the monitor's
        // identifier matches what `service_members.container_name` holds
        // exactly. Older clusters that registered as `node-{ordinal}`
        // need the legacy name for backwards compatibility — try the
        // container name first, fall back to `node-N`.
        let primary_name = member.container_name.clone();
        let legacy_name = format!("node-{}", member.ordinal);
        let drop_result = self.drop_node_from_monitor(service_id, &primary_name).await;
        let drop_result = match drop_result {
            Ok(()) => Ok(()),
            Err(e) => {
                debug!(
                    service_id,
                    member_id,
                    primary_name = %primary_name,
                    error = %e,
                    "drop_node by container name failed; trying legacy node-N alias"
                );
                self.drop_node_from_monitor(service_id, &legacy_name).await
            }
        };
        if let Err(e) = drop_result {
            warn!(
                service_id,
                member_id,
                primary_name = %primary_name,
                legacy_name = %legacy_name,
                error = %e,
                "Failed to drop node from pg_auto_failover monitor; cluster may need manual `pg_autoctl drop node` after cleanup"
            );
        }

        // 2. Stop and remove the container.
        if let Some(node_id) = member.node_id {
            // Remote: dispatch to the worker's agent.
            match self.get_remote_client(node_id).await {
                Ok(client) => {
                    if let Err(e) = client.remove_service(&member.container_name).await {
                        // Log loudly but keep going — the row + DNS still
                        // need to disappear so the cluster's view is
                        // consistent. The container may already be gone.
                        warn!(
                            service_id,
                            member_id,
                            node_id,
                            container = %member.container_name,
                            error = %e,
                            "Failed to remove remote cluster member container; continuing with row + DNS cleanup"
                        );
                    }
                }
                Err(e) => {
                    warn!(
                        service_id,
                        member_id,
                        node_id,
                        error = %e,
                        "Could not reach worker node to remove container; continuing with row + DNS cleanup"
                    );
                }
            }
        } else if let Some(docker) = self.docker.get() {
            // Local container.
            if let Err(e) = docker
                .remove_container(
                    &member.container_name,
                    Some(bollard::query_parameters::RemoveContainerOptions {
                        force: true,
                        ..Default::default()
                    }),
                )
                .await
            {
                warn!(
                    service_id,
                    member_id,
                    container = %member.container_name,
                    error = %e,
                    "Failed to remove local cluster member container; continuing with row + DNS cleanup"
                );
            }

            let volume_name = format!("{}_data", member.container_name);
            if let Err(e) = docker
                .remove_volume(
                    &volume_name,
                    None::<bollard::query_parameters::RemoveVolumeOptions>,
                )
                .await
            {
                // Volume removal failures are common (in-use, missing) and
                // not fatal — log at debug.
                debug!(
                    service_id,
                    member_id,
                    volume = %volume_name,
                    error = %e,
                    "Volume cleanup skipped"
                );
            }
        } else {
            // No local Docker daemon in this process (control-plane
            // profile) — there is nothing local to remove.
            debug!(
                service_id,
                member_id,
                container = %member.container_name,
                "No local Docker daemon in this process; skipping local container/volume cleanup"
            );
        }

        // 3. Delete the service_members row.
        service_members::Entity::delete_by_id(member_id)
            .exec(self.db.as_ref())
            .await?;

        // 4. Drop the Tier-2 DNS record (best-effort — same policy as
        //    delete_service: a stuck DNS plane shouldn't block removal).
        if let Err(e) = self
            .dns_registry
            .delete_by_owner(
                temps_dns::InternalOwnerKind::ServiceMember,
                member_id as i64,
            )
            .await
        {
            warn!(
                service_id,
                member_id,
                error = %e,
                "Failed to drop DNS records for removed cluster member"
            );
        }

        info!(
            service_id,
            member_id,
            role = %member.role,
            ordinal = member.ordinal,
            "Cluster member removed; reconciler will refresh role records on next tick"
        );

        Ok(())
    }

    /// Promote a replica to primary by running `pg_autoctl perform
    /// promotion` inside its container. The monitor coordinates the
    /// failover: it demotes the current primary and the new replica
    /// transitions through `wait_primary` → `single` → `primary`. The
    /// role reconciler refreshes the role-aliased VIPs on its next tick.
    ///
    /// Refuses:
    ///   * member doesn't belong to this service
    ///   * member is the monitor (singletons can't be promoted)
    ///   * member is already the primary
    ///   * member is not running
    ///   * service isn't a cluster
    ///
    /// The command is bounded and takes no user input beyond the
    /// pre-validated `member_id` — same risk profile as the existing
    /// `service_exec` endpoint, much lower than password-reset which
    /// would have crossed user-supplied secrets.
    pub async fn promote_cluster_member(
        &self,
        service_id: i32,
        member_id: i32,
    ) -> Result<(), ExternalServiceError> {
        info!(service_id, member_id, "Promoting cluster member to primary");

        let service = self.get_service(service_id).await?;
        if service.topology != "cluster" {
            return Err(ExternalServiceError::ParameterValidationFailed {
                service_id,
                reason: "promote_cluster_member is only valid for cluster topology services"
                    .to_string(),
            });
        }
        if service.service_type != "postgres" {
            return Err(ExternalServiceError::ParameterValidationFailed {
                service_id,
                reason: format!(
                    "promote_cluster_member is only supported for Postgres clusters (got '{}')",
                    service.service_type
                ),
            });
        }

        let member = service_members::Entity::find_by_id(member_id)
            .one(self.db.as_ref())
            .await?
            .ok_or(ExternalServiceError::InitializationFailed {
                id: service_id,
                reason: format!("Cluster member {} not found", member_id),
            })?;

        if member.service_id != service_id {
            return Err(ExternalServiceError::ParameterValidationFailed {
                service_id,
                reason: format!(
                    "Member {} does not belong to service {} (it belongs to {})",
                    member_id, service_id, member.service_id
                ),
            });
        }

        if is_role_monitor(&member.role) {
            return Err(ExternalServiceError::ParameterValidationFailed {
                service_id,
                reason: "Cannot promote the monitor — it is not a data node".to_string(),
            });
        }
        if self.member_is_live_primary(&service, &member).await? {
            return Err(ExternalServiceError::ParameterValidationFailed {
                service_id,
                reason: format!(
                    "Member {} is already the primary; nothing to do",
                    member.container_name
                ),
            });
        }
        if member.status != "running" {
            return Err(ExternalServiceError::ParameterValidationFailed {
                service_id,
                reason: format!(
                    "Member {} is not running (status: {}); start it before promoting",
                    member.container_name, member.status
                ),
            });
        }

        // The standalone postgres image and the HA `postgres-ha` image
        // both put pgdata under /var/lib/postgresql/pgdata. We pin it
        // here rather than discovering at runtime — every cluster member
        // we provision uses the same path (see PostgresClusterService).
        let cmd = vec![
            "pg_autoctl".to_string(),
            "perform".to_string(),
            "promotion".to_string(),
            "--pgdata".to_string(),
            "/var/lib/postgresql/pgdata".to_string(),
        ];

        let (exit_code, stdout, stderr) = if let Some(node_id) = member.node_id {
            let client = self.get_remote_client(node_id).await?;
            let result = client
                .exec_in_service(crate::remote_service_client::RemoteExecParams {
                    container_name: member.container_name.clone(),
                    command: cmd,
                    environment: HashMap::new(),
                    user: Some("postgres".to_string()),
                    detach: false,
                })
                .await
                .map_err(|e| ExternalServiceError::InternalError {
                    reason: format!(
                        "Failed to promote member '{}' on node {}: {}",
                        member.container_name, node_id, e
                    ),
                })?;
            (result.exit_code, result.stdout, result.stderr)
        } else {
            self.exec_in_local_container(&member.container_name, &cmd, Some("postgres"))
                .await?
        };

        if exit_code != 0 {
            // Surface stderr first because pg_autoctl writes its real
            // error there; stdout is just the progress chatter.
            let detail = if !stderr.is_empty() { stderr } else { stdout };
            return Err(ExternalServiceError::InternalError {
                reason: format!(
                    "pg_autoctl perform promotion failed (exit {}): {}",
                    exit_code,
                    detail.trim()
                ),
            });
        }

        info!(
            service_id,
            member_id,
            container = %member.container_name,
            "Promotion command accepted by monitor; reconciler will flip role records on next tick"
        );

        Ok(())
    }

    /// Run a command inside a locally-managed container. Mirrors the
    /// agent's `service_exec` for the control-plane half of bipartite
    /// cluster operations. Returns `(exit_code, stdout, stderr)`.
    /// Run `pg_autoctl drop node --name <node_name>` inside the cluster's
    /// monitor container. Returns `Ok(())` on success or any explainable
    /// failure (monitor missing, container gone, exec error) — the caller
    /// is expected to log loudly and proceed with row + container cleanup
    /// regardless. The monitor row is the source of truth for
    /// pg_auto_failover; leaving an orphan there blocks FSM transitions.
    async fn drop_node_from_monitor(
        &self,
        service_id: i32,
        node_name: &str,
    ) -> Result<(), ExternalServiceError> {
        let monitor = service_members::Entity::find()
            .filter(service_members::Column::ServiceId.eq(service_id))
            .all(self.db.as_ref())
            .await?
            .into_iter()
            .find(|m| is_role_monitor(&m.role))
            .ok_or_else(|| ExternalServiceError::InternalError {
                reason: format!(
                    "Cluster service {} has no monitor member; cannot drop node {} from pg_auto_failover",
                    service_id, node_name
                ),
            })?;

        // The monitor container's pg_autoctl runs out of
        // `/var/lib/postgresql/monitor` (see `monitor_command` in
        // `postgres_cluster.rs`), NOT the `/var/lib/postgresql/pgdata`
        // path the data nodes use. Using the wrong --pgdata makes
        // pg_autoctl fail with "Expected configuration file does not
        // exist", which is what the original "harmless orphan" comment
        // missed.
        let cmd = vec![
            "pg_autoctl".to_string(),
            "drop".to_string(),
            "node".to_string(),
            "--formation".to_string(),
            "default".to_string(),
            "--name".to_string(),
            node_name.to_string(),
            "--pgdata".to_string(),
            "/var/lib/postgresql/monitor".to_string(),
        ];

        let (exit_code, stdout, stderr) = if let Some(node_id) = monitor.node_id {
            let client = self.get_remote_client(node_id).await?;
            let result = client
                .exec_in_service(crate::remote_service_client::RemoteExecParams {
                    container_name: monitor.container_name.clone(),
                    command: cmd,
                    environment: HashMap::new(),
                    user: Some("postgres".to_string()),
                    detach: false,
                })
                .await
                .map_err(|e| ExternalServiceError::InternalError {
                    reason: format!(
                        "Failed to drop node {} via monitor on node {}: {}",
                        node_name, node_id, e
                    ),
                })?;
            (result.exit_code, result.stdout, result.stderr)
        } else {
            self.exec_in_local_container(&monitor.container_name, &cmd, Some("postgres"))
                .await?
        };

        if exit_code != 0 {
            // Common benign cases: node already dropped, name not found.
            // pg_autoctl writes the actual reason to stderr.
            let detail = if !stderr.is_empty() { stderr } else { stdout };
            let detail = detail.trim();
            if detail.contains("not found") || detail.contains("does not exist") {
                debug!(
                    service_id,
                    node_name, "pg_autoctl drop node reported the node was already absent"
                );
                return Ok(());
            }
            return Err(ExternalServiceError::InternalError {
                reason: format!(
                    "pg_autoctl drop node {} failed (exit {}): {}",
                    node_name, exit_code, detail
                ),
            });
        }

        info!(
            service_id,
            node_name, "Dropped node from pg_auto_failover monitor"
        );
        Ok(())
    }
}

/// Lets the metrics scraper reach HA clusters: `temps-metrics` cannot depend
/// on this crate, so the capability is injected at startup.
#[async_trait::async_trait]
impl temps_metrics::ClusterPrimaryConnector for ExternalServiceManager {
    async fn connect_cluster_primary(
        &self,
        service_id: i32,
    ) -> Result<temps_metrics::ClusterPrimaryConnection, temps_metrics::MetricsError> {
        self.connect_cluster_primary_client(service_id)
            .await
            .map(|(client, member)| temps_metrics::ClusterPrimaryConnection { client, member })
            .map_err(|e| temps_metrics::MetricsError::CollectorConnectionFailed {
                source_id: service_id,
                engine: "postgres".to_string(),
                reason: e.to_string(),
            })
    }
}

#[cfg(test)]
mod tests;
