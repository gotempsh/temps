// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Rules, profiles and per-node modes (ADR-052 §4).

use std::collections::HashMap;
use std::sync::Arc;

use chrono::Utc;
use sea_orm::{
    ActiveModelTrait, ColumnTrait, ConnectionTrait, DatabaseBackend, DatabaseConnection,
    EntityTrait, FromQueryResult, PaginatorTrait, QueryFilter, QueryOrder, Set, Statement,
};
use temps_entities::{
    control_plane_node_settings, network_policy_profiles, network_policy_rules,
    network_policy_state, node_network_policy_modes,
};
use temps_workload::{WorkloadKind, WorkloadRef};
use temps_workload_netpol::dns::NamedWorkload;
use temps_workload_netpol::{
    Action, Cidr, Direction, IdentityMap, L4Protocol, NodeMode, NodePolicy, NodePolicyReport,
    PolicySnapshot, PortRange, Profile, SystemContainer,
};

use crate::error::NetworkPolicyError;
use crate::model::{compile_policies, profile_str, profiles_for, ParsedRule, PeerSpec, Profiles};

/// Container-name prefix of today's per-sandbox egress proxy sidecar
/// (`temps-agents` `SANDBOX_EGRESS_PROXY_PREFIX`). Restricted sandboxes keep
/// reaching it on its port until the shared proxy replaces it.
pub const SANDBOX_SIDECAR_PREFIX: &str = "temps-sandbox-egress-proxy-";
pub const SANDBOX_SIDECAR_PORT: u16 = 3128;
/// `temps-agents` `SANDBOX_NAME_PREFIX`.
pub const SANDBOX_CONTAINER_PREFIX: &str = "temps-sandbox-";
/// `temps-sandbox` `PUBLIC_ID_PREFIX`, stripped from the container name.
pub const SANDBOX_PUBLIC_ID_PREFIX: &str = "sbx_";

pub const DEFAULT_PAGE_SIZE: u64 = 20;
pub const MAX_PAGE_SIZE: u64 = 100;
const MAX_DESCRIPTION: usize = 500;

/// The control plane's node id.
pub const CONTROL_PLANE_NODE_ID: i32 = 0;

#[derive(Debug, Clone, Default)]
pub struct RuleFilter {
    pub subject: Option<WorkloadRef>,
    pub origin: Option<String>,
}

/// A rule an operator asks for.
#[derive(Debug, Clone)]
pub struct NewRule {
    pub subject: WorkloadRef,
    pub direction: Direction,
    pub peer: PeerSpec,
    pub ports: Option<PortRange>,
    pub protocol: L4Protocol,
    pub action: Action,
    pub description: Option<String>,
}

/// A subject's stored rules, the link rules that point at it, and its
/// profiles.
#[derive(Debug, Clone)]
pub struct EffectivePolicy {
    pub subject: WorkloadRef,
    pub profiles: Profiles,
    pub rules: Vec<network_policy_rules::Model>,
    /// Link rules of other subjects that name this one as the peer. They
    /// become ingress allows when its ingress is restricted.
    pub inbound_links: Vec<network_policy_rules::Model>,
    /// Nodes in `enforce`; empty means nothing is enforced anywhere.
    pub enforcing_nodes: Vec<i32>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NodeModeEntry {
    pub node_id: i32,
    pub name: String,
    pub role: String,
    pub status: String,
    /// The stored mode. On a node without a live enforcer anything but
    /// `legacy` is stale and is never counted as enforcing.
    pub mode: NodeMode,
    /// Whether the node runs an enforcer: always on node 0, on a worker
    /// when its agent reported one within [`ENFORCER_REPORT_TTL`].
    pub supports_enforcement: bool,
    /// Why `shadow`/`enforce` is unavailable, when it is.
    pub enforcement_unavailable_reason: Option<String>,
    /// The datapath the node enforces with (`ebpf` or `nftables`), when
    /// known.
    pub backend: Option<String>,
}

/// How long a worker's enforcer report counts. Agents fetch their policy
/// every pass (15 s), so a live one is never close to it.
pub const ENFORCER_REPORT_TTL: std::time::Duration = std::time::Duration::from_secs(120);

/// What a worker's agent said about its enforcer on its last policy fetch.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EnforcerReport {
    /// `ebpf` or `nftables`.
    pub backend: String,
    pub seen: std::time::Instant,
}

#[derive(Debug, FromQueryResult)]
struct SandboxRow {
    id: i32,
    public_id: String,
}

#[derive(Debug, FromQueryResult)]
struct ServiceRow {
    id: i32,
    service_type: String,
    name: String,
}

#[derive(Debug, FromQueryResult)]
struct MemberRow {
    service_id: i32,
    container_name: String,
}

/// An address of a service member on another node, recorded by the
/// control plane (its overlay IP and its A/AAAA name records).
#[derive(Debug, FromQueryResult)]
struct RemoteAddressRow {
    kind: String,
    owner_id: i64,
    ip: String,
}

/// Every service member address the database knows on nodes other than
/// `$1`, in one query: overlay IPs and A/AAAA records (a `NULL` node is the
/// control plane). Stopped or failed members are left out, since their
/// addresses may already belong to someone else.
///
/// Environments are not here: their name records point at each node's
/// internal edge proxy (the bridge gateway, shared by every environment),
/// not at a replica. Replica addresses come from the nodes themselves
/// ([`NetworkPolicyService::record_observed`]).
const REMOTE_ADDRESSES_SQL: &str = "\
    SELECT 'service' AS kind, sm.service_id::bigint AS owner_id, sm.compute_ip AS ip \
    FROM service_members sm \
    WHERE COALESCE(sm.node_id, 0) <> $1 \
      AND sm.compute_ip IS NOT NULL AND sm.status NOT IN ('stopped', 'failed') \
    UNION ALL \
    SELECT 'service' AS kind, sm.service_id::bigint AS owner_id, se.target_ip AS ip \
    FROM service_endpoints se \
    JOIN service_members sm ON sm.id = se.owner_id \
    WHERE se.owner_kind = 'service_member' AND COALESCE(se.node_id, 0) <> $1 \
      AND se.record_type IN ('A', 'AAAA') AND se.target_ip IS NOT NULL \
      AND sm.status NOT IN ('stopped', 'failed')";

/// A node's compute subnet: the only addresses its reported workloads may
/// hold as far as other nodes are concerned.
#[derive(Debug, FromQueryResult)]
struct ComputeSubnetRow {
    id: i32,
    compute_cidr: Option<String>,
}

/// Most addresses one node may report; a node never runs more than its
/// `max_workloads` (110 by default) containers, each with a few addresses.
pub const MAX_REPORTED_ADDRESSES: usize = 4096;

/// The workloads a node observed on its last pass outside `legacy`.
#[derive(Debug, Clone)]
struct ObservedReport {
    identities: IdentityMap,
    seen: std::time::Instant,
}

#[derive(Debug, FromQueryResult)]
struct NameRow {
    fqdn: String,
    owner_kind: String,
    owner_id: i64,
    service_id: Option<i32>,
}

impl NameRow {
    /// The workload a published name belongs to, so a restricted workload
    /// may resolve the names of what its rules allow. A standalone service
    /// publishes as `service_role` (owner: the service), a cluster member as
    /// `service_member` (owner: the member row, joined to its service), an
    /// environment as `environment`. Node and static names belong to no
    /// workload.
    fn named_workload(self) -> Option<NamedWorkload> {
        let owner = match (self.owner_kind.as_str(), self.service_id) {
            ("environment", _) => WorkloadRef::new(WorkloadKind::Deployment, self.owner_id),
            ("service_role", _) => WorkloadRef::new(WorkloadKind::Service, self.owner_id),
            ("service_member", Some(service_id)) => {
                WorkloadRef::new(WorkloadKind::Service, i64::from(service_id))
            }
            _ => return None,
        };
        Some(NamedWorkload {
            fqdn: self.fqdn.trim_end_matches('.').to_ascii_lowercase(),
            owner,
        })
    }
}

pub struct NetworkPolicyService {
    db: Arc<DatabaseConnection>,
    /// Worker id → its agent's last enforcer report. In memory: agents
    /// report on every pass, so a restarted control plane relearns them
    /// within one.
    reports: std::sync::Mutex<HashMap<i32, EnforcerReport>>,
    /// Node id (0 included) → the workloads it observed, with their
    /// addresses. In memory for the same reason as `reports`; until a node
    /// reports again after a control plane restart, other nodes treat its
    /// workloads' addresses as unknown.
    observed: std::sync::Mutex<HashMap<i32, ObservedReport>>,
}

fn parse_mode(node_id: i32, value: &str) -> Result<NodeMode, NetworkPolicyError> {
    value
        .parse::<NodeMode>()
        .map_err(|e| NetworkPolicyError::Validation {
            message: format!("node {node_id} has a stored mode the server does not know: {e}"),
        })
}

impl NetworkPolicyService {
    pub fn new(db: Arc<DatabaseConnection>) -> Self {
        Self {
            db,
            reports: std::sync::Mutex::new(HashMap::new()),
            observed: std::sync::Mutex::new(HashMap::new()),
        }
    }

    /// Records that `node_id`'s agent runs an enforcer with `backend`.
    pub fn record_enforcer_report(&self, node_id: i32, backend: &str) {
        let backend = match backend {
            "ebpf" | "nftables" => backend.to_string(),
            _ => "unknown".to_string(),
        };
        let mut reports = self.reports.lock().unwrap_or_else(|e| e.into_inner());
        reports.insert(
            node_id,
            EnforcerReport {
                backend,
                seen: std::time::Instant::now(),
            },
        );
    }

    /// Records the workloads `node_id` observed on its last pass, which
    /// become remote peers of every other node (only the addresses inside
    /// `node_id`'s compute subnet, see [`Self::node_snapshot`]). A report
    /// over [`MAX_REPORTED_ADDRESSES`] is refused with a warning, so one
    /// broken node cannot grow every other node's maps.
    pub fn record_observed(&self, node_id: i32, identities: IdentityMap) {
        let count = identities.address_count();
        if count > MAX_REPORTED_ADDRESSES {
            tracing::warn!(
                node_id,
                addresses = count,
                limit = MAX_REPORTED_ADDRESSES,
                "node reported more workload addresses than any node can hold; report ignored"
            );
            return;
        }
        let mut observed = self.observed.lock().unwrap_or_else(|e| e.into_inner());
        observed.insert(
            node_id,
            ObservedReport {
                identities,
                seen: std::time::Instant::now(),
            },
        );
    }

    /// What every node but `node_id` observed within
    /// [`ENFORCER_REPORT_TTL`], by node.
    fn observed_elsewhere(&self, node_id: i32) -> Vec<(i32, IdentityMap)> {
        let observed = self.observed.lock().unwrap_or_else(|e| e.into_inner());
        observed
            .iter()
            .filter(|(id, report)| **id != node_id && report.seen.elapsed() <= ENFORCER_REPORT_TTL)
            .map(|(id, report)| (*id, report.identities.clone()))
            .collect()
    }

    /// The node's live enforcer report; node 0's enforcer runs in this
    /// process and needs none.
    fn live_report(&self, node_id: i32) -> Option<EnforcerReport> {
        let reports = self.reports.lock().unwrap_or_else(|e| e.into_inner());
        reports
            .get(&node_id)
            .filter(|r| r.seen.elapsed() <= ENFORCER_REPORT_TTL)
            .cloned()
    }

    /// Whether a node runs a network policy enforcer.
    pub fn supports_enforcement(&self, node_id: i32) -> bool {
        node_id == CONTROL_PLANE_NODE_ID || self.live_report(node_id).is_some()
    }

    /// Why `shadow`/`enforce` is unavailable on a node, for the console.
    pub fn enforcement_unavailable_reason(&self, node_id: i32) -> Option<String> {
        (!self.supports_enforcement(node_id)).then(|| {
            format!(
                "No network policy enforcer has reported from node {node_id} in the last {} \
                 seconds: its agent is offline or predates worker enforcement. Upgrade and \
                 restart `temps agent` on the node.",
                ENFORCER_REPORT_TTL.as_secs()
            )
        })
    }

    fn mode_entry(
        &self,
        node_id: i32,
        name: String,
        role: String,
        status: String,
        mode: NodeMode,
    ) -> NodeModeEntry {
        NodeModeEntry {
            supports_enforcement: self.supports_enforcement(node_id),
            enforcement_unavailable_reason: self.enforcement_unavailable_reason(node_id),
            backend: self.live_report(node_id).map(|r| r.backend),
            node_id,
            name,
            role,
            status,
            mode,
        }
    }

    pub async fn list_rules(
        &self,
        filter: RuleFilter,
        page: Option<u64>,
        page_size: Option<u64>,
    ) -> Result<(Vec<network_policy_rules::Model>, u64), NetworkPolicyError> {
        let page = page.unwrap_or(1).max(1);
        let page_size = page_size
            .unwrap_or(DEFAULT_PAGE_SIZE)
            .clamp(1, MAX_PAGE_SIZE);
        let mut query = network_policy_rules::Entity::find();
        if let Some(subject) = filter.subject {
            query = query
                .filter(network_policy_rules::Column::SubjectKind.eq(subject.kind.as_str()))
                .filter(network_policy_rules::Column::SubjectId.eq(subject.id));
        }
        if let Some(origin) = filter.origin {
            crate::model::parse_origin(&origin).map_err(NetworkPolicyError::validation)?;
            query = query.filter(network_policy_rules::Column::Origin.eq(origin));
        }
        let paginator = query
            .order_by_desc(network_policy_rules::Column::CreatedAt)
            .order_by_desc(network_policy_rules::Column::Id)
            .paginate(self.db.as_ref(), page_size);
        let total = paginator
            .num_items()
            .await
            .map_err(NetworkPolicyError::db("count network policy rules"))?;
        let items = paginator
            .fetch_page(page - 1)
            .await
            .map_err(NetworkPolicyError::db("list network policy rules"))?;
        Ok((items, total))
    }

    pub async fn get_rule(
        &self,
        rule_id: i64,
    ) -> Result<network_policy_rules::Model, NetworkPolicyError> {
        network_policy_rules::Entity::find_by_id(rule_id)
            .one(self.db.as_ref())
            .await
            .map_err(NetworkPolicyError::db("read a network policy rule"))?
            .ok_or(NetworkPolicyError::RuleNotFound { rule_id })
    }

    async fn exists(&self, sql: &str, id: i64) -> Result<bool, NetworkPolicyError> {
        let row = self
            .db
            .query_one(Statement::from_sql_and_values(
                DatabaseBackend::Postgres,
                sql,
                [id.into()],
            ))
            .await
            .map_err(NetworkPolicyError::db("check that a workload exists"))?;
        Ok(row.is_some())
    }

    /// Whether the product object behind a workload exists. Jobs have no
    /// table of their own and are accepted.
    async fn workload_exists(&self, workload: WorkloadRef) -> Result<bool, NetworkPolicyError> {
        // `functions.id` is a BIGINT; the other tables key on INTEGER.
        let (sql, int4_key) = match workload.kind {
            WorkloadKind::Deployment => ("SELECT 1 FROM environments WHERE id = $1", true),
            WorkloadKind::Sandbox => ("SELECT 1 FROM sandboxes WHERE id = $1", true),
            WorkloadKind::Service => ("SELECT 1 FROM external_services WHERE id = $1", true),
            WorkloadKind::Function => ("SELECT 1 FROM functions WHERE id = $1", false),
            WorkloadKind::Job => return Ok(true),
        };
        if int4_key && workload.id > i64::from(i32::MAX) {
            return Ok(false);
        }
        self.exists(sql, workload.id).await
    }

    pub async fn create_rule(
        &self,
        rule: NewRule,
        created_by: Option<i32>,
    ) -> Result<network_policy_rules::Model, NetworkPolicyError> {
        if rule.subject.id <= 0 {
            return Err(NetworkPolicyError::validation(format!(
                "subject id {} is not positive",
                rule.subject.id
            )));
        }
        if matches!(rule.peer, PeerSpec::Host(_)) && rule.direction != Direction::Egress {
            return Err(NetworkPolicyError::validation(
                "a host rule is egress only: incoming connections carry no hostname",
            ));
        }
        // Hostname rules compile only into the egress proxy's table, and no
        // running component consumes it yet: accepting one would report a
        // restriction (a deny in particular) that nothing enforces.
        if let PeerSpec::Host(host) = &rule.peer {
            return Err(NetworkPolicyError::HostRuleNotEnforceable {
                subject: rule.subject.to_string(),
                host: host.as_str().to_string(),
            });
        }
        if let Some(ports) = rule.ports {
            if !ports.is_valid() {
                return Err(NetworkPolicyError::validation(format!(
                    "port range {}-{} is not within 1-65535",
                    ports.start, ports.end
                )));
            }
        }
        let description = rule
            .description
            .map(|d| d.trim().to_string())
            .filter(|d| !d.is_empty());
        if description
            .as_ref()
            .is_some_and(|d| d.chars().count() > MAX_DESCRIPTION)
        {
            return Err(NetworkPolicyError::validation(format!(
                "description is longer than {MAX_DESCRIPTION} characters"
            )));
        }
        if !self.workload_exists(rule.subject).await? {
            return Err(NetworkPolicyError::validation(format!(
                "subject {} does not exist",
                rule.subject
            )));
        }
        let peer_workload = match &rule.peer {
            PeerSpec::Service(id) => Some(WorkloadRef::new(WorkloadKind::Service, *id)),
            PeerSpec::Workload(w) => Some(*w),
            PeerSpec::Cidr(_) | PeerSpec::Host(_) => None,
        };
        if let Some(peer) = peer_workload {
            if peer == rule.subject {
                return Err(NetworkPolicyError::validation(format!(
                    "{peer} cannot be its own peer"
                )));
            }
            if !self.workload_exists(peer).await? {
                return Err(NetworkPolicyError::validation(format!(
                    "peer {peer} does not exist"
                )));
            }
        }
        let now = Utc::now();
        let model = network_policy_rules::ActiveModel {
            subject_kind: Set(rule.subject.kind.as_str().to_string()),
            subject_id: Set(rule.subject.id),
            direction: Set(match rule.direction {
                Direction::Egress => "egress",
                Direction::Ingress => "ingress",
            }
            .to_string()),
            peer_kind: Set(rule.peer.kind().to_string()),
            peer_ref: Set(rule.peer.reference()),
            port_start: Set(rule.ports.map(|p| i32::from(p.start))),
            port_end: Set(rule.ports.map(|p| i32::from(p.end))),
            protocol: Set(match rule.protocol {
                L4Protocol::Tcp => "tcp",
                L4Protocol::Udp => "udp",
                L4Protocol::Any => "any",
            }
            .to_string()),
            action: Set(match rule.action {
                Action::Allow => "allow",
                Action::Deny => "deny",
            }
            .to_string()),
            origin: Set("operator".to_string()),
            link_table: Set(None),
            link_id: Set(None),
            description: Set(description),
            created_by: Set(created_by),
            created_at: Set(now),
            updated_at: Set(now),
            ..Default::default()
        };
        model
            .insert(self.db.as_ref())
            .await
            .map_err(NetworkPolicyError::db("insert a network policy rule"))
    }

    /// Deletes an operator or system rule. Link rules belong to their link.
    pub async fn delete_rule(
        &self,
        rule_id: i64,
    ) -> Result<network_policy_rules::Model, NetworkPolicyError> {
        let rule = self.get_rule(rule_id).await?;
        if let (Some(link_table), Some(link_id)) = (rule.link_table.clone(), rule.link_id) {
            return Err(NetworkPolicyError::LinkOwned {
                rule_id,
                link_table,
                link_id,
            });
        }
        network_policy_rules::Entity::delete_by_id(rule_id)
            .exec(self.db.as_ref())
            .await
            .map_err(NetworkPolicyError::db("delete a network policy rule"))?;
        Ok(rule)
    }

    async fn stored_profile(
        &self,
        subject: WorkloadRef,
    ) -> Result<Option<network_policy_profiles::Model>, NetworkPolicyError> {
        network_policy_profiles::Entity::find_by_id((subject.kind.as_str().to_string(), subject.id))
            .one(self.db.as_ref())
            .await
            .map_err(NetworkPolicyError::db("read a network policy profile"))
    }

    /// Sets (or, with both `None`, clears) a subject's profile override.
    pub async fn set_profile(
        &self,
        subject: WorkloadRef,
        egress: Option<Profile>,
        ingress: Option<Profile>,
        updated_by: Option<i32>,
    ) -> Result<Profiles, NetworkPolicyError> {
        if !self.workload_exists(subject).await? {
            return Err(NetworkPolicyError::validation(format!(
                "subject {subject} does not exist"
            )));
        }
        let existing = self.stored_profile(subject).await?;
        if egress.is_none() && ingress.is_none() {
            if existing.is_some() {
                network_policy_profiles::Entity::delete_by_id((
                    subject.kind.as_str().to_string(),
                    subject.id,
                ))
                .exec(self.db.as_ref())
                .await
                .map_err(NetworkPolicyError::db("clear a network policy profile"))?;
            }
            return profiles_for(subject, None);
        }
        let now = Utc::now();
        let model = match existing {
            Some(row) => {
                let mut active: network_policy_profiles::ActiveModel = row.into();
                active.egress = Set(egress.map(|p| profile_str(p).to_string()));
                active.ingress = Set(ingress.map(|p| profile_str(p).to_string()));
                active.updated_by = Set(updated_by);
                active.updated_at = Set(now);
                active
                    .update(self.db.as_ref())
                    .await
                    .map_err(NetworkPolicyError::db("update a network policy profile"))?
            }
            None => network_policy_profiles::ActiveModel {
                subject_kind: Set(subject.kind.as_str().to_string()),
                subject_id: Set(subject.id),
                egress: Set(egress.map(|p| profile_str(p).to_string())),
                ingress: Set(ingress.map(|p| profile_str(p).to_string())),
                updated_by: Set(updated_by),
                created_at: Set(now),
                updated_at: Set(now),
            }
            .insert(self.db.as_ref())
            .await
            .map_err(NetworkPolicyError::db("insert a network policy profile"))?,
        };
        profiles_for(subject, Some(&model))
    }

    pub async fn effective(
        &self,
        subject: WorkloadRef,
    ) -> Result<EffectivePolicy, NetworkPolicyError> {
        if !self.workload_exists(subject).await? {
            return Err(NetworkPolicyError::validation(format!(
                "subject {subject} does not exist"
            )));
        }
        let profiles = profiles_for(subject, self.stored_profile(subject).await?.as_ref())?;
        let rules = network_policy_rules::Entity::find()
            .filter(network_policy_rules::Column::SubjectKind.eq(subject.kind.as_str()))
            .filter(network_policy_rules::Column::SubjectId.eq(subject.id))
            .order_by_asc(network_policy_rules::Column::Id)
            .all(self.db.as_ref())
            .await
            .map_err(NetworkPolicyError::db(
                "read a subject's network policy rules",
            ))?;
        let mut inbound = network_policy_rules::Entity::find()
            .filter(network_policy_rules::Column::Origin.eq("link"))
            .filter(network_policy_rules::Column::PeerKind.eq("workload"))
            .filter(network_policy_rules::Column::PeerRef.eq(subject.to_string()));
        if subject.kind == WorkloadKind::Service {
            inbound = network_policy_rules::Entity::find()
                .filter(network_policy_rules::Column::Origin.eq("link"))
                .filter(
                    sea_orm::Condition::any()
                        .add(
                            sea_orm::Condition::all()
                                .add(network_policy_rules::Column::PeerKind.eq("service"))
                                .add(
                                    network_policy_rules::Column::PeerRef
                                        .eq(subject.id.to_string()),
                                ),
                        )
                        .add(
                            sea_orm::Condition::all()
                                .add(network_policy_rules::Column::PeerKind.eq("workload"))
                                .add(network_policy_rules::Column::PeerRef.eq(subject.to_string())),
                        ),
                );
        }
        let inbound_links = inbound
            .order_by_asc(network_policy_rules::Column::Id)
            .all(self.db.as_ref())
            .await
            .map_err(NetworkPolicyError::db(
                "read the links that point at a subject",
            ))?;
        // A stored `enforce` on a node without a live enforcer is not real.
        let enforcing_nodes = self
            .node_modes()
            .await?
            .into_iter()
            .filter(|n| n.mode == NodeMode::Enforce && n.supports_enforcement)
            .map(|n| n.node_id)
            .collect();
        Ok(EffectivePolicy {
            subject,
            profiles,
            rules,
            inbound_links,
            enforcing_nodes,
        })
    }

    async fn control_plane_mode(
        &self,
    ) -> Result<control_plane_node_settings::Model, NetworkPolicyError> {
        if let Some(row) = control_plane_node_settings::Entity::find_by_id(0i16)
            .one(self.db.as_ref())
            .await
            .map_err(NetworkPolicyError::db(
                "read the control plane's network policy mode",
            ))?
        {
            return Ok(row);
        }
        // The migration inserts the row; recreate it if someone removed it,
        // with the default.
        control_plane_node_settings::ActiveModel {
            id: Set(0),
            network_policy_mode: Set(NodeMode::Legacy.as_str().to_string()),
            updated_at: Set(Utc::now()),
        }
        .insert(self.db.as_ref())
        .await
        .map_err(NetworkPolicyError::db(
            "create the control plane's settings row",
        ))
    }

    /// Node 0 first, then every `nodes` row by id.
    pub async fn node_modes(&self) -> Result<Vec<NodeModeEntry>, NetworkPolicyError> {
        let control = self.control_plane_mode().await?;
        let mut entries = vec![self.mode_entry(
            CONTROL_PLANE_NODE_ID,
            "control-plane".to_string(),
            "control".to_string(),
            "active".to_string(),
            parse_mode(CONTROL_PLANE_NODE_ID, &control.network_policy_mode)?,
        )];
        let nodes = node_network_policy_modes::Entity::find()
            .order_by_asc(node_network_policy_modes::Column::Id)
            .all(self.db.as_ref())
            .await
            .map_err(NetworkPolicyError::db("list node network policy modes"))?;
        for node in nodes {
            let mode = parse_mode(node.id, &node.network_policy_mode)?;
            entries.push(self.mode_entry(node.id, node.name, node.role, node.status, mode));
        }
        Ok(entries)
    }

    pub async fn node_mode(&self, node_id: i32) -> Result<NodeMode, NetworkPolicyError> {
        if node_id == CONTROL_PLANE_NODE_ID {
            let control = self.control_plane_mode().await?;
            return parse_mode(node_id, &control.network_policy_mode);
        }
        let node = node_network_policy_modes::Entity::find_by_id(node_id)
            .one(self.db.as_ref())
            .await
            .map_err(NetworkPolicyError::db("read a node's network policy mode"))?
            .ok_or(NetworkPolicyError::NodeNotFound { node_id })?;
        parse_mode(node_id, &node.network_policy_mode)
    }

    /// Returns the previous mode and the node as it is now.
    pub async fn set_node_mode(
        &self,
        node_id: i32,
        mode: NodeMode,
    ) -> Result<(NodeMode, NodeModeEntry), NetworkPolicyError> {
        let now = Utc::now();
        if node_id == CONTROL_PLANE_NODE_ID {
            let control = self.control_plane_mode().await?;
            let previous = parse_mode(node_id, &control.network_policy_mode)?;
            let mut active: control_plane_node_settings::ActiveModel = control.into();
            active.network_policy_mode = Set(mode.as_str().to_string());
            active.updated_at = Set(now);
            active
                .update(self.db.as_ref())
                .await
                .map_err(NetworkPolicyError::db(
                    "set the control plane's network policy mode",
                ))?;
            let entry = self
                .node_modes()
                .await?
                .into_iter()
                .find(|n| n.node_id == node_id)
                .ok_or(NetworkPolicyError::NodeNotFound { node_id })?;
            return Ok((previous, entry));
        }
        let node = node_network_policy_modes::Entity::find_by_id(node_id)
            .one(self.db.as_ref())
            .await
            .map_err(NetworkPolicyError::db("read a node's network policy mode"))?
            .ok_or(NetworkPolicyError::NodeNotFound { node_id })?;
        // `legacy` stays accepted so a stale stored mode can be cleared.
        if mode != NodeMode::Legacy && !self.supports_enforcement(node_id) {
            return Err(NetworkPolicyError::WorkerEnforcementUnavailable {
                node_id,
                mode: mode.as_str().to_string(),
            });
        }
        let previous = parse_mode(node_id, &node.network_policy_mode)?;
        let mut active: node_network_policy_modes::ActiveModel = node.into();
        active.network_policy_mode = Set(mode.as_str().to_string());
        active.updated_at = Set(now);
        let node = active
            .update(self.db.as_ref())
            .await
            .map_err(NetworkPolicyError::db("set a node's network policy mode"))?;
        let stored = parse_mode(node.id, &node.network_policy_mode)?;
        Ok((
            previous,
            self.mode_entry(node.id, node.name, node.role, node.status, stored),
        ))
    }

    /// Bumped by the database on every change a policy depends on.
    pub async fn revision(&self) -> Result<i64, NetworkPolicyError> {
        Ok(network_policy_state::Entity::find_by_id(0i16)
            .one(self.db.as_ref())
            .await
            .map_err(NetworkPolicyError::db("read the network policy revision"))?
            .map(|s| s.revision)
            .unwrap_or(0))
    }

    async fn query<T: FromQueryResult>(
        &self,
        operation: &'static str,
        sql: &str,
    ) -> Result<Vec<T>, NetworkPolicyError> {
        T::find_by_statement(Statement::from_string(DatabaseBackend::Postgres, sql))
            .all(self.db.as_ref())
            .await
            .map_err(NetworkPolicyError::db(operation))
    }

    /// Node 0's snapshot.
    pub async fn snapshot(&self) -> Result<PolicySnapshot, NetworkPolicyError> {
        self.node_snapshot(CONTROL_PLANE_NODE_ID).await
    }

    /// What `node_id` returns to its agent: its mode, and outside `legacy`
    /// its snapshot. Calling it is the agent's enforcer report.
    pub async fn node_policy(
        &self,
        node_id: i32,
        report: NodePolicyReport,
    ) -> Result<NodePolicy, NetworkPolicyError> {
        let mode = self.node_mode(node_id).await?;
        self.record_enforcer_report(node_id, &report.backend);
        self.record_observed(node_id, report.identities);
        let snapshot = match mode {
            NodeMode::Legacy => None,
            NodeMode::Shadow | NodeMode::Enforce => Some(self.node_snapshot(node_id).await?),
        };
        Ok(NodePolicy { mode, snapshot })
    }

    /// Everything `node_id` needs to enforce, for every workload. A node
    /// keeps only the workloads it observes locally. Addresses recorded for
    /// other nodes are its remote peers, and other nodes' host addresses
    /// (their proxies and health checks) are sources every workload
    /// accepts.
    pub async fn node_snapshot(&self, node_id: i32) -> Result<PolicySnapshot, NetworkPolicyError> {
        let rows = network_policy_rules::Entity::find()
            .order_by_asc(network_policy_rules::Column::Id)
            .all(self.db.as_ref())
            .await
            .map_err(NetworkPolicyError::db("read every network policy rule"))?;
        let rules = rows
            .iter()
            .map(ParsedRule::from_row)
            .collect::<Result<Vec<_>, _>>()?;
        let profiles = network_policy_profiles::Entity::find()
            .all(self.db.as_ref())
            .await
            .map_err(NetworkPolicyError::db("read every network policy profile"))?;
        let sandboxes: Vec<SandboxRow> = self
            .query(
                "list live sandboxes",
                "SELECT id, public_id FROM sandboxes WHERE status <> 'destroyed'",
            )
            .await?;
        let services: Vec<ServiceRow> = self
            .query(
                "list services",
                "SELECT id, service_type, name FROM external_services",
            )
            .await?;
        let members: Vec<MemberRow> = self
            .query(
                "list service members",
                "SELECT service_id, container_name FROM service_members",
            )
            .await?;
        let names: Vec<NameRow> = self
            .query(
                "list workload names",
                "SELECT se.fqdn, se.owner_kind, se.owner_id, sm.service_id \
                 FROM service_endpoints se \
                 LEFT JOIN service_members sm \
                   ON se.owner_kind = 'service_member' AND sm.id = se.owner_id \
                 WHERE se.owner_kind IN ('service_member', 'service_role', 'environment')",
            )
            .await?;

        let mut container_names = HashMap::new();
        for sandbox in &sandboxes {
            let label = sandbox
                .public_id
                .strip_prefix(SANDBOX_PUBLIC_ID_PREFIX)
                .unwrap_or(&sandbox.public_id);
            container_names.insert(
                format!("{SANDBOX_CONTAINER_PREFIX}{label}"),
                WorkloadRef::new(WorkloadKind::Sandbox, i64::from(sandbox.id)),
            );
        }
        for service in &services {
            container_names.insert(
                format!("{}-{}", service.service_type, service.name),
                WorkloadRef::new(WorkloadKind::Service, i64::from(service.id)),
            );
        }
        for member in &members {
            container_names.insert(
                member.container_name.clone(),
                WorkloadRef::new(WorkloadKind::Service, i64::from(member.service_id)),
            );
        }
        let remote_rows: Vec<RemoteAddressRow> =
            RemoteAddressRow::find_by_statement(Statement::from_sql_and_values(
                DatabaseBackend::Postgres,
                REMOTE_ADDRESSES_SQL,
                [node_id.into()],
            ))
            .all(self.db.as_ref())
            .await
            .map_err(NetworkPolicyError::db(
                "list workload addresses on other nodes",
            ))?;
        let system_ingress = if node_id == CONTROL_PLANE_NODE_ID {
            Vec::new()
        } else {
            let mut sources = self.other_node_addresses(node_id).await?;
            sources.extend(self.control_plane_sources_toward(node_id).await?);
            sources.sort();
            sources.dedup();
            sources
        };
        let mut remote = remote_identities(remote_rows);
        let elsewhere = self.observed_elsewhere(node_id);
        if !elsewhere.is_empty() {
            let subnets = self.compute_subnets().await?;
            merge_observed(&mut remote, elsewhere, &subnets);
        }
        let named = names
            .into_iter()
            .filter_map(NameRow::named_workload)
            .collect();

        let compiled = compile_policies(
            &rules,
            &profiles,
            sandboxes
                .iter()
                .map(|s| WorkloadRef::new(WorkloadKind::Sandbox, i64::from(s.id))),
        )?;
        Ok(PolicySnapshot {
            policies: compiled.policies,
            egress: compiled.egress,
            names: named,
            container_names,
            remote,
            system_containers: vec![SystemContainer {
                name_prefix: SANDBOX_SIDECAR_PREFIX.to_string(),
                ports: Some(PortRange::single(SANDBOX_SIDECAR_PORT)),
                protocol: L4Protocol::Tcp,
            }],
            system_ingress,
            ..Default::default()
        })
    }

    /// Every node's compute subnet, node 0's from the cluster network
    /// config.
    async fn compute_subnets(&self) -> Result<HashMap<i32, Cidr>, NetworkPolicyError> {
        let rows: Vec<ComputeSubnetRow> = self
            .query(
                "list node compute subnets",
                "SELECT id, compute_cidr FROM nodes WHERE status <> 'removed'",
            )
            .await?;
        let mut subnets: HashMap<i32, Cidr> = rows
            .into_iter()
            .filter_map(|row| Some((row.id, row.compute_cidr?.trim().parse().ok()?)))
            .collect();
        let config = temps_entities::network_config::Entity::find_by_id(1)
            .one(self.db.as_ref())
            .await
            .map_err(NetworkPolicyError::db("read the cluster network config"))?;
        if let Some(cidr) = config
            .and_then(|c| c.control_plane_compute_cidr)
            .and_then(|c| c.trim().parse().ok())
        {
            subnets.insert(CONTROL_PLANE_NODE_ID, cidr);
        }
        Ok(subnets)
    }

    /// Host addresses of every node but `node_id`, the control plane
    /// included: their private, underlay and mesh addresses and the gateway
    /// of their workload subnet, plus the whole mesh range. None of these is
    /// ever a workload's, so admitting them never admits a workload.
    async fn other_node_addresses(&self, node_id: i32) -> Result<Vec<Cidr>, NetworkPolicyError> {
        let rows: Vec<NodeAddressRow> =
            NodeAddressRow::find_by_statement(Statement::from_sql_and_values(
                DatabaseBackend::Postgres,
                OTHER_NODE_ADDRESSES_SQL,
                [node_id.into()],
            ))
            .all(self.db.as_ref())
            .await
            .map_err(NetworkPolicyError::db("list other nodes' host addresses"))?;
        let mut out: Vec<Cidr> = Vec::new();
        for row in rows {
            for value in [
                row.private_address,
                row.underlay_address,
                row.mesh_wg_address,
            ]
            .into_iter()
            .flatten()
            {
                if let Some(addr) = host_of(&value) {
                    out.push(Cidr::host(addr));
                }
            }
            if let Some(gateway) = row.compute_cidr.as_deref().and_then(subnet_gateway) {
                out.push(Cidr::host(gateway));
            }
        }
        let config = temps_entities::network_config::Entity::find_by_id(1)
            .one(self.db.as_ref())
            .await
            .map_err(NetworkPolicyError::db("read the cluster network config"))?;
        if let Some(config) = config {
            if let Some(gateway) = config
                .control_plane_compute_cidr
                .as_deref()
                .and_then(subnet_gateway)
            {
                out.push(Cidr::host(gateway));
            }
            if let Some(addr) = config
                .control_plane_underlay_address
                .as_deref()
                .and_then(host_of)
            {
                out.push(Cidr::host(addr));
            }
            if config.wireguard_enabled {
                if let Ok(mesh) = config.wireguard_cidr.parse::<Cidr>() {
                    out.push(mesh);
                }
            }
        }
        out.sort();
        out.dedup();
        Ok(out)
    }
}

impl NetworkPolicyService {
    /// The addresses this host sends from when it connects to node `node_id`.
    ///
    /// The control plane is not a row in `nodes`, so `other_node_addresses`
    /// only knows its mesh and underlay addresses. When it reaches a worker on
    /// that worker's private address (the edge proxy and deployment health
    /// checks dialing a published port), the kernel picks the source address,
    /// usually one on the shared private network that is recorded nowhere.
    /// Asking the kernel for that source, per address the worker registered,
    /// lets the control plane's own traffic into workloads whose ingress is
    /// restricted. Only this host's own addresses come out of it, whatever
    /// the worker registered.
    async fn control_plane_sources_toward(
        &self,
        node_id: i32,
    ) -> Result<Vec<Cidr>, NetworkPolicyError> {
        let row: Option<NodeAddressRow> =
            NodeAddressRow::find_by_statement(Statement::from_sql_and_values(
                DatabaseBackend::Postgres,
                NODE_ADDRESSES_SQL,
                [node_id.into()],
            ))
            .one(self.db.as_ref())
            .await
            .map_err(NetworkPolicyError::db("read the node's own addresses"))?;
        let Some(row) = row else {
            return Ok(Vec::new());
        };
        let peers = [
            row.private_address,
            row.underlay_address,
            row.mesh_wg_address,
        ]
        .into_iter()
        .flatten()
        .filter_map(|value| host_of(&value));
        Ok(local_sources(peers, source_address_toward))
    }
}

/// The source address the kernel picks for a connection from this host to
/// `peer`. Connecting a UDP socket only consults the routing table; no
/// packet is sent.
fn source_address_toward(peer: std::net::IpAddr) -> Option<std::net::IpAddr> {
    let bind: std::net::SocketAddr = match peer {
        std::net::IpAddr::V4(_) => (std::net::Ipv4Addr::UNSPECIFIED, 0).into(),
        std::net::IpAddr::V6(_) => (std::net::Ipv6Addr::UNSPECIFIED, 0).into(),
    };
    let socket = std::net::UdpSocket::bind(bind).ok()?;
    socket.connect((peer, 9)).ok()?;
    socket.local_addr().ok().map(|local| local.ip())
}

/// The sources `pick` returns for `peers`, as host CIDRs. Loopback and
/// unspecified answers never reach another host and are dropped.
fn local_sources(
    peers: impl Iterator<Item = std::net::IpAddr>,
    pick: impl Fn(std::net::IpAddr) -> Option<std::net::IpAddr>,
) -> Vec<Cidr> {
    let mut out: Vec<Cidr> = peers
        .filter_map(pick)
        .filter(|source| !source.is_loopback() && !source.is_unspecified())
        .map(Cidr::host)
        .collect();
    out.sort();
    out.dedup();
    out
}

/// The address in `value`: an IP literal, `ip:port`, `[ip]:port` or a URL
/// with an IP host. `None` for hostnames.
fn host_of(value: &str) -> Option<std::net::IpAddr> {
    let value = value.trim();
    let value = value
        .split_once("://")
        .map(|(_, rest)| rest)
        .unwrap_or(value);
    let value = value.split('/').next().unwrap_or(value);
    if let Ok(addr) = value.parse() {
        return Some(addr);
    }
    if let Ok(socket) = value.parse::<std::net::SocketAddr>() {
        return Some(socket.ip());
    }
    let host = value.rsplit_once(':').map(|(h, _)| h).unwrap_or(value);
    host.trim_matches(|c| c == '[' || c == ']').parse().ok()
}

/// The first host of an IPv4 subnet (its bridge gateway): `10.202.5.1`
/// for `10.202.5.0/24`.
fn subnet_gateway(cidr: &str) -> Option<std::net::IpAddr> {
    let (addr, _) = cidr.trim().split_once('/')?;
    let addr: std::net::Ipv4Addr = addr.parse().ok()?;
    Some(std::net::IpAddr::V4(std::net::Ipv4Addr::from(
        u32::from(addr).checked_add(1)?,
    )))
}

#[derive(Debug, FromQueryResult)]
struct NodeAddressRow {
    private_address: Option<String>,
    underlay_address: Option<String>,
    mesh_wg_address: Option<String>,
    compute_cidr: Option<String>,
}

const NODE_ADDRESSES_SQL: &str = "\
    SELECT NULLIF(private_address, '') AS private_address, underlay_address, \
           mesh_wg_address, compute_cidr \
    FROM nodes WHERE id = $1 AND status <> 'removed'";

const OTHER_NODE_ADDRESSES_SQL: &str = "\
    SELECT NULLIF(private_address, '') AS private_address, underlay_address, \
           mesh_wg_address, compute_cidr \
    FROM nodes WHERE id <> $1 AND status <> 'removed'";

/// Adds what other nodes observed to `remote`, keeping only addresses
/// inside the reporting node's own compute subnet. That drops node-local
/// addresses (a container's address on a local Docker bridge, which the
/// same address on another node may also be), and it means a node can only
/// ever name addresses it allocates: a compromised worker cannot claim
/// another node's workloads. A node without a known subnet contributes
/// nothing.
fn merge_observed(
    remote: &mut IdentityMap,
    observed: Vec<(i32, IdentityMap)>,
    subnets: &HashMap<i32, Cidr>,
) {
    for (node_id, mut identities) in observed {
        let Some(subnet) = subnets.get(&node_id).copied() else {
            continue;
        };
        identities.retain_addresses(|addr| subnet.contains(addr));
        remote.extend(&identities);
    }
}

/// Worker-hosted addresses as identities. Rows that do not parse (an
/// unknown kind, a value that is not an IP literal) are skipped with a
/// warning: one bad row must not stop the node from enforcing the rest.
fn remote_identities(rows: Vec<RemoteAddressRow>) -> IdentityMap {
    let mut remote = IdentityMap::new();
    for row in rows {
        let kind = match row.kind.as_str() {
            "service" => WorkloadKind::Service,
            "deployment" => WorkloadKind::Deployment,
            other => {
                tracing::warn!(
                    kind = other,
                    owner_id = row.owner_id,
                    "unknown remote workload kind; address skipped"
                );
                continue;
            }
        };
        match row.ip.trim().parse::<std::net::IpAddr>() {
            Ok(addr) => {
                remote.insert(WorkloadRef::new(kind, row.owner_id), addr);
            }
            Err(e) => tracing::warn!(
                kind = kind.as_str(),
                owner_id = row.owner_id,
                ip = %row.ip,
                error = %e,
                "worker-hosted workload address is not an IP literal; skipped"
            ),
        }
    }
    remote
}

#[cfg(test)]
mod remote_tests {
    use super::*;

    fn row(kind: &str, owner_id: i64, ip: &str) -> RemoteAddressRow {
        RemoteAddressRow {
            kind: kind.to_string(),
            owner_id,
            ip: ip.to_string(),
        }
    }

    #[test]
    fn remote_rows_become_identities_and_bad_rows_are_skipped() {
        let remote = remote_identities(vec![
            row("service", 5, "172.20.5.42"),
            row("deployment", 7, " fd00::7 "),
            row("service", 5, "orders.internal"),
            row("sandbox", 1, "172.20.5.9"),
        ]);
        let service: Vec<String> = remote
            .addresses(&WorkloadRef::new(WorkloadKind::Service, 5))
            .map(|a| a.to_string())
            .collect();
        assert_eq!(service, vec!["172.20.5.42".to_string()]);
        let environment: Vec<String> = remote
            .addresses(&WorkloadRef::new(WorkloadKind::Deployment, 7))
            .map(|a| a.to_string())
            .collect();
        assert_eq!(environment, vec!["fd00::7".to_string()]);
        assert_eq!(remote.workloads().count(), 2, "unknown kinds are skipped");
    }

    #[test]
    fn a_worker_supports_enforcement_only_while_its_agent_reports() {
        let db = sea_orm::MockDatabase::new(DatabaseBackend::Postgres).into_connection();
        let service = NetworkPolicyService::new(Arc::new(db));
        assert!(service.supports_enforcement(CONTROL_PLANE_NODE_ID));
        assert!(service
            .enforcement_unavailable_reason(CONTROL_PLANE_NODE_ID)
            .is_none());
        assert!(!service.supports_enforcement(3));
        assert!(service
            .enforcement_unavailable_reason(3)
            .is_some_and(|r| r.contains("node 3")));
        service.record_enforcer_report(3, "ebpf");
        assert!(service.supports_enforcement(3));
        assert_eq!(
            service.live_report(3).map(|r| r.backend),
            Some("ebpf".into())
        );
        service.record_enforcer_report(3, "<script>");
        assert_eq!(
            service.live_report(3).map(|r| r.backend),
            Some("unknown".into()),
            "a reported backend is never echoed verbatim"
        );
        // A stale report no longer counts.
        service.reports.lock().unwrap().insert(
            3,
            EnforcerReport {
                backend: "ebpf".into(),
                seen: std::time::Instant::now()
                    - ENFORCER_REPORT_TTL
                    - std::time::Duration::from_secs(1),
            },
        );
        assert!(!service.supports_enforcement(3));
    }

    fn ip(value: &str) -> std::net::IpAddr {
        value.parse().unwrap()
    }

    /// A node's report reaches the other nodes, never itself, and only
    /// while it is fresh; an oversized one is ignored.
    #[test]
    fn observed_workloads_are_shared_with_the_other_nodes_while_fresh() {
        let db = sea_orm::MockDatabase::new(DatabaseBackend::Postgres).into_connection();
        let service = NetworkPolicyService::new(Arc::new(db));
        let alpha = WorkloadRef::new(WorkloadKind::Deployment, 1);
        service.record_observed(3, IdentityMap::new().with(alpha, ip("10.202.0.2")));

        let seen_by = |node| {
            service
                .observed_elsewhere(node)
                .into_iter()
                .map(|(id, map)| (id, map.address_count()))
                .collect::<Vec<_>>()
        };
        assert_eq!(seen_by(4), [(3, 1)]);
        assert!(seen_by(3).is_empty(), "a node is never its own remote peer");

        service.observed.lock().unwrap().get_mut(&3).unwrap().seen =
            std::time::Instant::now() - ENFORCER_REPORT_TTL - std::time::Duration::from_secs(1);
        assert!(seen_by(4).is_empty(), "a stale report no longer counts");

        let mut flood = IdentityMap::new();
        for n in 0..=MAX_REPORTED_ADDRESSES as u32 {
            flood.insert(alpha, std::net::IpAddr::V4(std::net::Ipv4Addr::from(n)));
        }
        service.record_observed(4, flood);
        assert!(seen_by(3).is_empty(), "an oversized report is ignored");
    }

    /// Only addresses inside the reporting node's own compute subnet are
    /// shared: a container's node-local bridge address is not, and a node
    /// cannot claim an address another node allocates.
    #[test]
    fn control_plane_sources_keep_this_hosts_addresses_and_drop_loopback() {
        let ip = |s: &str| s.parse::<std::net::IpAddr>().unwrap();
        let routes: HashMap<std::net::IpAddr, std::net::IpAddr> = [
            // The worker's private address, reached on the shared network.
            (ip("10.77.0.12"), ip("10.77.0.10")),
            // Its mesh address, reached over WireGuard.
            (ip("10.201.0.3"), ip("10.201.0.1")),
            // A second registered address on the same network: same source.
            (ip("10.77.0.13"), ip("10.77.0.10")),
            // A worker registered under a loopback address.
            (ip("127.0.0.1"), ip("127.0.0.1")),
        ]
        .into_iter()
        .collect();
        let peers = [
            ip("10.77.0.12"),
            ip("10.201.0.3"),
            ip("10.77.0.13"),
            ip("127.0.0.1"),
            // No route: contributes nothing.
            ip("192.0.2.9"),
        ];

        let sources = local_sources(peers.into_iter(), |peer| routes.get(&peer).copied());

        assert_eq!(
            sources,
            vec![Cidr::host(ip("10.77.0.10")), Cidr::host(ip("10.201.0.1"))]
        );
    }

    #[test]
    fn the_kernel_names_the_source_address_without_sending_anything() {
        let loopback: std::net::IpAddr = "127.0.0.1".parse().unwrap();
        assert_eq!(source_address_toward(loopback), Some(loopback));
    }

    #[test]
    fn observed_addresses_are_kept_only_inside_the_reporting_nodes_subnet() {
        let alpha = WorkloadRef::new(WorkloadKind::Deployment, 1);
        let gamma = WorkloadRef::new(WorkloadKind::Deployment, 3);
        let mallory = WorkloadRef::new(WorkloadKind::Deployment, 9);
        let subnets = HashMap::from([
            (3, "10.202.0.0/24".parse::<Cidr>().unwrap()),
            (CONTROL_PLANE_NODE_ID, "10.202.255.0/24".parse().unwrap()),
        ]);
        let observed = vec![
            (
                3,
                IdentityMap::new()
                    .with(alpha, ip("10.202.0.2"))
                    .with(alpha, ip("172.18.0.3"))
                    // node 4's address, claimed by node 3
                    .with(mallory, ip("10.202.1.2")),
            ),
            (
                CONTROL_PLANE_NODE_ID,
                IdentityMap::new().with(gamma, ip("172.20.0.2")),
            ),
            // no known subnet: contributes nothing
            (7, IdentityMap::new().with(mallory, ip("10.202.7.2"))),
        ];
        let mut remote = IdentityMap::new();
        merge_observed(&mut remote, observed, &subnets);
        assert_eq!(remote.workloads().copied().collect::<Vec<_>>(), [alpha]);
        assert_eq!(
            remote.addresses(&alpha).collect::<Vec<_>>(),
            [ip("10.202.0.2")]
        );
    }

    #[test]
    fn host_addresses_are_read_from_every_stored_shape() {
        for (value, expected) in [
            ("10.201.0.9", Some("10.201.0.9")),
            ("10.201.0.9:3100", Some("10.201.0.9")),
            ("https://10.201.0.9:3100", Some("10.201.0.9")),
            ("[fd00::9]:3100", Some("fd00::9")),
            ("worker.example.internal", None),
        ] {
            assert_eq!(
                host_of(value).map(|a| a.to_string()),
                expected.map(str::to_string),
                "{value}"
            );
        }
        assert_eq!(
            subnet_gateway("10.202.5.0/24").map(|a| a.to_string()),
            Some("10.202.5.1".to_string())
        );
        assert_eq!(subnet_gateway("fd00::/64"), None);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::source::DbPolicySource;

    fn name_row(fqdn: &str, owner_kind: &str, owner_id: i64, service_id: Option<i32>) -> NameRow {
        NameRow {
            fqdn: fqdn.into(),
            owner_kind: owner_kind.into(),
            owner_id,
            service_id,
        }
    }

    #[test]
    fn every_workload_name_kind_maps_to_its_owner() {
        let owner = |row: NameRow| row.named_workload().map(|named| (named.fqdn, named.owner));
        assert_eq!(
            owner(name_row("MariaW2.temps.local.", "service_role", 3, None)),
            Some((
                "mariaw2.temps.local".into(),
                WorkloadRef::new(WorkloadKind::Service, 3)
            )),
            "a standalone service is named by its service id"
        );
        assert_eq!(
            owner(name_row(
                "primary.pg.temps.local",
                "service_member",
                41,
                Some(5)
            )),
            Some((
                "primary.pg.temps.local".into(),
                WorkloadRef::new(WorkloadKind::Service, 5)
            )),
            "a cluster member is named by its service, not its member row"
        );
        assert_eq!(
            owner(name_row(
                "production.alpha.temps.local",
                "environment",
                1,
                None
            )),
            Some((
                "production.alpha.temps.local".into(),
                WorkloadRef::new(WorkloadKind::Deployment, 1)
            ))
        );
        assert_eq!(owner(name_row("w1.temps.local", "node", 3, None)), None);
        assert_eq!(
            owner(name_row("orphan.temps.local", "service_member", 9, None)),
            None,
            "a member whose row is gone names nothing"
        );
    }
    use temps_database::test_utils::TestDatabase;
    use temps_workload_netpol::{Peer, PolicySource};

    async fn exec(db: &DatabaseConnection, sql: &str) -> i64 {
        let row = db
            .query_one(Statement::from_string(DatabaseBackend::Postgres, sql))
            .await
            .unwrap_or_else(|e| panic!("{sql}: {e}"))
            .unwrap_or_else(|| panic!("{sql}: no row"));
        row.try_get_by_index::<i32>(0)
            .map(i64::from)
            .unwrap_or_else(|e| panic!("{sql}: {e}"))
    }

    async fn rules_of(
        service: &NetworkPolicyService,
        subject: WorkloadRef,
    ) -> Vec<network_policy_rules::Model> {
        service
            .list_rules(
                RuleFilter {
                    subject: Some(subject),
                    origin: None,
                },
                None,
                None,
            )
            .await
            .unwrap()
            .0
    }

    fn rule(subject: WorkloadRef, direction: Direction, peer: PeerSpec) -> NewRule {
        NewRule {
            subject,
            direction,
            peer,
            ports: None,
            protocol: L4Protocol::Tcp,
            action: Action::Allow,
            description: None,
        }
    }

    #[tokio::test]
    async fn links_rules_modes_and_snapshots_round_trip_through_the_database() {
        let db = match TestDatabase::with_migrations().await {
            Ok(db) => db,
            Err(e) => {
                eprintln!("Docker/TimescaleDB unavailable, skipping: {e}");
                return;
            }
        };
        let conn = db.connection_arc();
        let service = Arc::new(NetworkPolicyService::new(conn.clone()));
        let c = conn.as_ref();

        // Today's defaults: every node legacy.
        let modes = service.node_modes().await.unwrap();
        assert_eq!(modes[0].node_id, 0);
        assert!(modes.iter().all(|n| n.mode == NodeMode::Legacy));

        let project = exec(
            c,
            "INSERT INTO projects (name, repo_name, repo_owner, directory, main_branch, preset, \
                 created_at, updated_at, slug, is_deleted, is_public_repo, attack_mode, \
                 enable_preview_environments, source_type) \
             VALUES ('shop', 'r', 'o', '.', 'main', 'nextjs', NOW(), NOW(), 'shop', FALSE, TRUE, \
                 FALSE, FALSE, 'git') RETURNING id",
        )
        .await;
        let environment = |slug: &str| {
            format!(
                "INSERT INTO environments (name, slug, subdomain, host, upstreams, project_id, \
                     is_preview, created_at, updated_at) \
                 VALUES ('{slug}', '{slug}', 'shop-{slug}', 'shop-{slug}', '[]'::json, {project}, \
                     FALSE, NOW(), NOW()) RETURNING id"
            )
        };
        let env = exec(c, &environment("production")).await;
        let db_id = exec(
            c,
            "INSERT INTO external_services (name, service_type, status, created_at, updated_at) \
             VALUES ('orders', 'postgres', 'running', NOW(), NOW()) RETURNING id",
        )
        .await;
        let sandbox = exec(
            c,
            "INSERT INTO sandboxes (public_id, name, status, work_dir, timeout_secs, \
                 created_at, last_activity_at, expires_at) \
             VALUES ('sbx_abc123', 'app', 'running', '/workspace', 3600, \
                 NOW(), NOW(), NOW() + INTERVAL '1 hour') \
             RETURNING id",
        )
        .await;
        let deployment = WorkloadRef::new(WorkloadKind::Deployment, env);
        let sandbox_ref = WorkloadRef::new(WorkloadKind::Sandbox, sandbox);
        let db_ref = WorkloadRef::new(WorkloadKind::Service, db_id);

        // Linking creates the rule in the same statement.
        let revision = service.revision().await.unwrap();
        let link = exec(
            c,
            &format!(
                "INSERT INTO project_services (project_id, service_id, created_at, updated_at) \
                 VALUES ({project}, {db_id}, NOW(), NOW()) RETURNING id"
            ),
        )
        .await;
        let rules = rules_of(&service, deployment).await;
        assert_eq!(rules.len(), 1);
        assert_eq!(rules[0].origin, "link");
        assert_eq!(rules[0].peer_ref, db_id.to_string());
        assert_eq!(rules[0].link_id, Some(link));
        assert!(
            service.revision().await.unwrap() > revision,
            "the link bumped the revision"
        );

        // A new environment of a linked project inherits the link.
        let staging = exec(c, &environment("staging")).await;
        let staging_ref = WorkloadRef::new(WorkloadKind::Deployment, staging);
        assert_eq!(rules_of(&service, staging_ref).await.len(), 1);

        let sandbox_link = exec(
            c,
            &format!(
                "INSERT INTO sandbox_services (sandbox_id, service_id, created_at) \
                 VALUES ({sandbox}, {db_id}, NOW()) RETURNING id"
            ),
        )
        .await;
        let sandbox_rules = rules_of(&service, sandbox_ref).await;
        assert_eq!(sandbox_rules.len(), 1);

        // A link rule is removed by unlinking, not deleted directly.
        assert!(matches!(
            service.delete_rule(sandbox_rules[0].id).await,
            Err(NetworkPolicyError::LinkOwned { link_id, .. }) if link_id == sandbox_link
        ));

        // Operator rules: validated, then stored.
        let host_peer = PeerSpec::parse("host", "registry.example.test").unwrap();
        // Hostname rules are refused until something enforces them: an
        // accepted deny would otherwise be reported but never applied.
        for action in [Action::Allow, Action::Deny] {
            let refused = service
                .create_rule(
                    NewRule {
                        action,
                        ..rule(sandbox_ref, Direction::Egress, host_peer.clone())
                    },
                    None,
                )
                .await;
            assert!(
                matches!(
                    &refused,
                    Err(NetworkPolicyError::HostRuleNotEnforceable { host, .. })
                        if host == "registry.example.test"
                ),
                "{refused:?}"
            );
        }
        let cidr_peer = PeerSpec::parse("cidr", "10.20.0.0/16").unwrap();
        let host = service
            .create_rule(rule(sandbox_ref, Direction::Egress, cidr_peer), None)
            .await
            .unwrap();
        assert_eq!(host.origin, "operator");
        let missing = WorkloadRef::new(WorkloadKind::Sandbox, 999_999);
        assert!(matches!(
            service
                .create_rule(
                    rule(missing, Direction::Egress, PeerSpec::Service(db_id)),
                    None
                )
                .await,
            Err(NetworkPolicyError::Validation { .. })
        ));
        assert!(matches!(
            service
                .create_rule(rule(sandbox_ref, Direction::Ingress, host_peer), None)
                .await,
            Err(NetworkPolicyError::Validation { .. })
        ));
        assert!(matches!(
            service
                .create_rule(
                    rule(db_ref, Direction::Egress, PeerSpec::Service(db_id)),
                    None
                )
                .await,
            Err(NetworkPolicyError::Validation { .. })
        ));

        // Effective policy: the sandbox is restricted by default, and the
        // service sees every link to it as inbound.
        let effective = service.effective(sandbox_ref).await.unwrap();
        assert_eq!(effective.profiles.egress, Profile::Restricted);
        assert_eq!(effective.rules.len(), 2);
        assert!(effective.enforcing_nodes.is_empty());
        let inbound = service.effective(db_ref).await.unwrap().inbound_links;
        assert_eq!(
            inbound.len(),
            3,
            "two environments and the sandbox link to it"
        );

        // The snapshot a node enforces.
        let source = DbPolicySource::new(service.clone(), CONTROL_PLANE_NODE_ID);
        assert_eq!(source.mode().await.unwrap(), NodeMode::Legacy);
        let snapshot = source.snapshot().await.unwrap();
        let sandbox_policy = snapshot
            .policies
            .iter()
            .find(|p| p.subject == sandbox_ref)
            .expect("the sandbox has a policy");
        assert_eq!(sandbox_policy.egress, Profile::Restricted);
        assert!(sandbox_policy
            .rules
            .iter()
            .any(|r| r.peer == Peer::Workload(db_ref)));
        assert_eq!(
            snapshot.egress.len(),
            1,
            "the CIDR rule goes to the proxy too"
        );
        assert_eq!(
            snapshot.container_names.get("temps-sandbox-abc123"),
            Some(&sandbox_ref)
        );
        assert_eq!(
            snapshot.container_names.get("postgres-orders"),
            Some(&db_ref)
        );
        assert!(
            !snapshot.policies.iter().any(|p| p.subject == deployment),
            "an open deployment has nothing to enforce"
        );

        // Modes: node 0 and a worker row; an unknown node is refused.
        let (previous, now) = service.set_node_mode(0, NodeMode::Shadow).await.unwrap();
        assert_eq!((previous, now.mode), (NodeMode::Legacy, NodeMode::Shadow));
        assert_eq!(source.mode().await.unwrap(), NodeMode::Shadow);
        let worker = exec(
            c,
            "INSERT INTO nodes (name, token_hash, address, private_address) \
             VALUES ('worker-1', 'x', 'https://10.201.0.9:3100', '10.201.0.9') RETURNING id",
        )
        .await as i32;
        assert_eq!(service.node_mode(worker).await.unwrap(), NodeMode::Legacy);
        // No enforcer runs on workers: shadow and enforce are refused, and
        // legacy stays settable so a stale mode can be cleared.
        for mode in [NodeMode::Enforce, NodeMode::Shadow] {
            let refused = service.set_node_mode(worker, mode).await;
            assert!(
                matches!(
                    &refused,
                    Err(NetworkPolicyError::WorkerEnforcementUnavailable { node_id, .. })
                        if *node_id == worker
                ),
                "{refused:?}"
            );
        }
        assert_eq!(service.node_mode(worker).await.unwrap(), NodeMode::Legacy);
        service
            .set_node_mode(worker, NodeMode::Legacy)
            .await
            .unwrap();
        // A worker `enforce` stored before the refusal is never reported
        // as enforcing; the control plane is.
        c.execute_unprepared(&format!(
            "UPDATE nodes SET network_policy_mode = 'enforce' WHERE id = {worker}"
        ))
        .await
        .unwrap();
        assert!(service
            .effective(sandbox_ref)
            .await
            .unwrap()
            .enforcing_nodes
            .is_empty());
        service.set_node_mode(0, NodeMode::Enforce).await.unwrap();
        assert_eq!(
            service
                .effective(sandbox_ref)
                .await
                .unwrap()
                .enforcing_nodes,
            vec![0]
        );
        service.set_node_mode(0, NodeMode::Shadow).await.unwrap();

        // Worker-hosted addresses reach the snapshot, so a link to a
        // service running on a worker is an effective allow on node 0.
        c.execute_unprepared(&format!(
            "INSERT INTO service_members (service_id, node_id, role, container_name, \
                 compute_ip, status, ordinal) \
             VALUES ({db_id}, {worker}, 'primary', 'postgres-orders-0', '172.20.5.42', 'running', 0), \
                    ({db_id}, {worker}, 'replica', 'postgres-orders-1', '172.20.5.43', 'stopped', 1)"
        ))
        .await
        .unwrap();
        c.execute_unprepared(&format!(
            "INSERT INTO service_endpoints (fqdn, record_type, target_ip, owner_kind, owner_id, \
                 node_id, generation) \
             SELECT 'orders-0.orders.temps.local', 'A', '172.20.5.44', 'service_member', id, \
                 {worker}, 1 \
             FROM service_members WHERE container_name = 'postgres-orders-0'"
        ))
        .await
        .unwrap();
        let snapshot = source.snapshot().await.unwrap();
        let mut remote: Vec<String> = snapshot
            .remote
            .addresses(&db_ref)
            .map(|a| a.to_string())
            .collect();
        remote.sort();
        assert_eq!(
            remote,
            vec!["172.20.5.42".to_string(), "172.20.5.44".to_string()],
            "the running member's overlay IP and its name record; never the stopped member"
        );
        assert!(snapshot
            .policies
            .iter()
            .find(|p| p.subject == sandbox_ref)
            .is_some_and(|p| p.rules.iter().any(|r| r.peer == Peer::Workload(db_ref))));
        assert!(matches!(
            service.set_node_mode(987_654, NodeMode::Enforce).await,
            Err(NetworkPolicyError::NodeNotFound { node_id: 987_654 })
        ));

        // Unlinking removes the link rule; operator rules are deleted.
        c.execute_unprepared(&format!(
            "DELETE FROM sandbox_services WHERE id = {sandbox_link}"
        ))
        .await
        .unwrap();
        assert_eq!(rules_of(&service, sandbox_ref).await.len(), 1);
        service.delete_rule(host.id).await.unwrap();
        assert!(rules_of(&service, sandbox_ref).await.is_empty());
        assert!(matches!(
            service.get_rule(host.id).await,
            Err(NetworkPolicyError::RuleNotFound { .. })
        ));
        c.execute_unprepared(&format!("DELETE FROM project_services WHERE id = {link}"))
            .await
            .unwrap();
        assert!(rules_of(&service, deployment).await.is_empty());

        // Profiles.
        let profiles = service
            .set_profile(deployment, Some(Profile::Restricted), None, None)
            .await
            .unwrap();
        assert!(profiles.egress_overridden && !profiles.ingress_overridden);
        let cleared = service
            .set_profile(deployment, None, None, None)
            .await
            .unwrap();
        assert_eq!(cleared.egress, Profile::Open);
    }
}
