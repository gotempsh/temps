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
    Action, Direction, L4Protocol, NodeMode, PolicySnapshot, PortRange, Profile, SystemContainer,
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
    pub mode: NodeMode,
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

#[derive(Debug, FromQueryResult)]
struct NameRow {
    fqdn: String,
    owner_kind: String,
    owner_id: i64,
    service_id: Option<i32>,
}

pub struct NetworkPolicyService {
    db: Arc<DatabaseConnection>,
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
        Self { db }
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
        let enforcing_nodes = self
            .node_modes()
            .await?
            .into_iter()
            .filter(|n| n.mode == NodeMode::Enforce)
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
        let mut entries = vec![NodeModeEntry {
            node_id: CONTROL_PLANE_NODE_ID,
            name: "control-plane".to_string(),
            role: "control".to_string(),
            status: "active".to_string(),
            mode: parse_mode(CONTROL_PLANE_NODE_ID, &control.network_policy_mode)?,
        }];
        let nodes = node_network_policy_modes::Entity::find()
            .order_by_asc(node_network_policy_modes::Column::Id)
            .all(self.db.as_ref())
            .await
            .map_err(NetworkPolicyError::db("list node network policy modes"))?;
        for node in nodes {
            entries.push(NodeModeEntry {
                node_id: node.id,
                mode: parse_mode(node.id, &node.network_policy_mode)?,
                name: node.name,
                role: node.role,
                status: node.status,
            });
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
        let previous = parse_mode(node_id, &node.network_policy_mode)?;
        let mut active: node_network_policy_modes::ActiveModel = node.into();
        active.network_policy_mode = Set(mode.as_str().to_string());
        active.updated_at = Set(now);
        let node = active
            .update(self.db.as_ref())
            .await
            .map_err(NetworkPolicyError::db("set a node's network policy mode"))?;
        Ok((
            previous,
            NodeModeEntry {
                node_id: node.id,
                mode: parse_mode(node.id, &node.network_policy_mode)?,
                name: node.name,
                role: node.role,
                status: node.status,
            },
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

    /// Everything a node needs to enforce, for every workload. A node keeps
    /// only the workloads it observes locally.
    pub async fn snapshot(&self) -> Result<PolicySnapshot, NetworkPolicyError> {
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
                 WHERE se.owner_kind IN ('service_member', 'environment')",
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
        let named = names
            .into_iter()
            .filter_map(|row| {
                let owner = match (row.owner_kind.as_str(), row.service_id) {
                    ("environment", _) => WorkloadRef::new(WorkloadKind::Deployment, row.owner_id),
                    ("service_member", Some(service_id)) => {
                        WorkloadRef::new(WorkloadKind::Service, i64::from(service_id))
                    }
                    _ => return None,
                };
                Some(NamedWorkload {
                    fqdn: row.fqdn.trim_end_matches('.').to_ascii_lowercase(),
                    owner,
                })
            })
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
            system_containers: vec![SystemContainer {
                name_prefix: SANDBOX_SIDECAR_PREFIX.to_string(),
                ports: Some(PortRange::single(SANDBOX_SIDECAR_PORT)),
                protocol: L4Protocol::Tcp,
            }],
            ..Default::default()
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::source::DbPolicySource;
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
        let host = service
            .create_rule(
                rule(sandbox_ref, Direction::Egress, host_peer.clone()),
                None,
            )
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
        assert_eq!(snapshot.egress.len(), 1, "the host rule goes to the proxy");
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
        service
            .set_node_mode(worker, NodeMode::Enforce)
            .await
            .unwrap();
        assert_eq!(
            service
                .effective(sandbox_ref)
                .await
                .unwrap()
                .enforcing_nodes,
            vec![worker]
        );
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
