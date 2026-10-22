// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! HTTP surface: rules, a subject's effective policy and profile, and the
//! per-node mode. Reads need `settings:read`; every write is instance-admin
//! only, because a rule or a mode decides what a workload can reach.

use std::sync::Arc;

use axum::{
    extract::{Extension, Path, Query, State},
    http::StatusCode,
    response::IntoResponse,
    routing::{get, patch, put},
    Json, Router,
};
use serde::{Deserialize, Serialize};
use temps_auth::{permission_guard, AuthContext, RequireAuth};
use temps_core::problemdetails::{self, Problem};
use temps_core::{AuditContext, AuditLogger, AuditOperation, RequestMetadata, UtcDateTime};
use temps_entities::network_policy_rules;
use temps_workload::WorkloadRef;
use temps_workload_netpol::{NodeMode, PortRange};
use tracing::error;
use utoipa::{IntoParams, OpenApi, ToSchema};

use crate::error::NetworkPolicyError;
use crate::model::{
    parse_action, parse_direction, parse_profile, parse_protocol, parse_subject, profile_str,
    PeerSpec, Profiles,
};
use crate::service::{EffectivePolicy, NetworkPolicyService, NewRule, NodeModeEntry, RuleFilter};

#[derive(Clone)]
pub struct NetworkPolicyAppState {
    pub service: Arc<NetworkPolicyService>,
    pub audit: Arc<dyn AuditLogger>,
}

pub fn router(state: Arc<NetworkPolicyAppState>) -> Router {
    Router::new()
        .route("/network-policy/rules", get(list_rules).post(create_rule))
        .route(
            "/network-policy/rules/{rule_id}",
            get(get_rule).delete(delete_rule),
        )
        .route(
            "/network-policy/subjects/{subject_kind}/{subject_id}",
            get(get_effective_policy),
        )
        .route(
            "/network-policy/subjects/{subject_kind}/{subject_id}/profile",
            put(set_profile),
        )
        .route("/network-policy/nodes", get(list_node_modes))
        .route("/network-policy/nodes/{node_id}", patch(set_node_mode))
        .with_state(state)
}

#[derive(OpenApi)]
#[openapi(
    paths(
        list_rules,
        create_rule,
        get_rule,
        delete_rule,
        get_effective_policy,
        set_profile,
        list_node_modes,
        set_node_mode
    ),
    components(schemas(
        NetworkPolicyRuleResponse,
        NetworkPolicyRuleListResponse,
        CreateNetworkPolicyRuleRequest,
        EffectivePolicyResponse,
        SetProfileRequest,
        ProfileResponse,
        NodeNetworkPolicyModeResponse,
        NodeNetworkPolicyModeListResponse,
        SetNodeNetworkPolicyModeRequest
    )),
    tags((name = "Network Policy", description = "What each workload may reach, and whether a node enforces it"))
)]
pub struct NetworkPolicyApiDoc;

// ---------------------------------------------------------------- errors

impl From<NetworkPolicyError> for Problem {
    fn from(error: NetworkPolicyError) -> Self {
        match error {
            NetworkPolicyError::RuleNotFound { .. } => problemdetails::new(StatusCode::NOT_FOUND)
                .with_title("Network Policy Rule Not Found")
                .with_detail(error.to_string()),
            NetworkPolicyError::NodeNotFound { .. } => problemdetails::new(StatusCode::NOT_FOUND)
                .with_title("Node Not Found")
                .with_detail(error.to_string()),
            NetworkPolicyError::Validation { .. } => problemdetails::new(StatusCode::BAD_REQUEST)
                .with_title("Invalid Network Policy")
                .with_detail(error.to_string()),
            NetworkPolicyError::LinkOwned { .. } => problemdetails::new(StatusCode::CONFLICT)
                .with_title("Rule Owned By A Link")
                .with_detail(error.to_string()),
            NetworkPolicyError::InvalidStoredRule { .. }
            | NetworkPolicyError::InvalidStoredProfile { .. } => {
                problemdetails::new(StatusCode::INTERNAL_SERVER_ERROR)
                    .with_title("Stored Network Policy Invalid")
                    .with_detail(error.to_string())
            }
            NetworkPolicyError::Database { .. } => {
                problemdetails::new(StatusCode::INTERNAL_SERVER_ERROR)
                    .with_title("Internal Server Error")
                    .with_detail(error.to_string())
            }
        }
    }
}

fn bad_request(message: String) -> Problem {
    Problem::from(NetworkPolicyError::Validation { message })
}

pub(crate) fn require_reader(auth: &AuthContext) -> Result<(), Problem> {
    permission_guard!(auth, SettingsRead);
    Ok(())
}

pub(crate) fn require_admin(auth: &AuthContext) -> Result<(), Problem> {
    permission_guard!(auth, SystemAdmin);
    Ok(())
}

// ------------------------------------------------------------------ DTOs

#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct NetworkPolicyRuleResponse {
    pub id: i64,
    #[schema(example = "sandbox")]
    pub subject_kind: String,
    pub subject_id: i64,
    #[schema(example = "egress")]
    pub direction: String,
    #[schema(example = "service")]
    pub peer_kind: String,
    #[schema(example = "12")]
    pub peer_ref: String,
    pub port_start: Option<i32>,
    pub port_end: Option<i32>,
    #[schema(example = "tcp")]
    pub protocol: String,
    #[schema(example = "allow")]
    pub action: String,
    /// `link` rules are removed by unlinking, not deleted directly.
    #[schema(example = "operator")]
    pub origin: String,
    pub link_table: Option<String>,
    pub link_id: Option<i64>,
    pub description: Option<String>,
    pub created_by: Option<i32>,
    #[schema(value_type = String, format = DateTime)]
    pub created_at: UtcDateTime,
    #[schema(value_type = String, format = DateTime)]
    pub updated_at: UtcDateTime,
}

impl From<network_policy_rules::Model> for NetworkPolicyRuleResponse {
    fn from(rule: network_policy_rules::Model) -> Self {
        Self {
            id: rule.id,
            subject_kind: rule.subject_kind,
            subject_id: rule.subject_id,
            direction: rule.direction,
            peer_kind: rule.peer_kind,
            peer_ref: rule.peer_ref,
            port_start: rule.port_start,
            port_end: rule.port_end,
            protocol: rule.protocol,
            action: rule.action,
            origin: rule.origin,
            link_table: rule.link_table,
            link_id: rule.link_id,
            description: rule.description,
            created_by: rule.created_by,
            created_at: rule.created_at,
            updated_at: rule.updated_at,
        }
    }
}

#[derive(Debug, Serialize, ToSchema)]
pub struct NetworkPolicyRuleListResponse {
    pub items: Vec<NetworkPolicyRuleResponse>,
    pub total: u64,
    pub page: u64,
    pub page_size: u64,
}

#[derive(Debug, Deserialize, IntoParams)]
pub struct ListRulesQuery {
    pub subject_kind: Option<String>,
    pub subject_id: Option<i64>,
    /// `link`, `operator` or `system`.
    pub origin: Option<String>,
    /// Default 1.
    pub page: Option<u64>,
    /// Default 20, max 100.
    pub page_size: Option<u64>,
}

#[derive(Debug, Clone, Deserialize, ToSchema)]
pub struct CreateNetworkPolicyRuleRequest {
    /// `deployment` (an environment), `sandbox`, `service`, `function` or `job`.
    #[schema(example = "sandbox")]
    pub subject_kind: String,
    /// Environment id for `deployment`; the row id for the other kinds.
    pub subject_id: i64,
    /// `egress` or `ingress`.
    #[schema(example = "egress")]
    pub direction: String,
    /// `service` (by id), `workload` (`<kind>/<id>`), `cidr`, or `host`
    /// (egress only, enforced by the egress proxy).
    #[schema(example = "host")]
    pub peer_kind: String,
    #[schema(example = "registry.example.test")]
    pub peer_ref: String,
    /// Omit both ports for every port (80/443 for a `host` rule).
    pub port_start: Option<u16>,
    pub port_end: Option<u16>,
    /// `tcp` (default), `udp` or `any`.
    pub protocol: Option<String>,
    /// `allow` or `deny`.
    #[schema(example = "allow")]
    pub action: String,
    pub description: Option<String>,
}

impl CreateNetworkPolicyRuleRequest {
    fn into_rule(self) -> Result<NewRule, Problem> {
        let subject = parse_subject(&self.subject_kind, self.subject_id).map_err(bad_request)?;
        let ports = match (self.port_start, self.port_end) {
            (None, None) => None,
            (Some(start), end) => Some(PortRange {
                start,
                end: end.unwrap_or(start),
            }),
            (None, Some(end)) => {
                return Err(bad_request(format!("port_end {end} without port_start")))
            }
        };
        Ok(NewRule {
            subject,
            direction: parse_direction(&self.direction).map_err(bad_request)?,
            peer: PeerSpec::parse(&self.peer_kind, &self.peer_ref).map_err(bad_request)?,
            ports,
            protocol: parse_protocol(self.protocol.as_deref().unwrap_or("tcp"))
                .map_err(bad_request)?,
            action: parse_action(&self.action).map_err(bad_request)?,
            description: self.description,
        })
    }
}

#[derive(Debug, Serialize, ToSchema)]
pub struct ProfileResponse {
    #[schema(example = "restricted")]
    pub egress: String,
    #[schema(example = "open")]
    pub ingress: String,
    /// `default` (the kind's) or `override`.
    pub egress_source: String,
    pub ingress_source: String,
}

impl From<Profiles> for ProfileResponse {
    fn from(p: Profiles) -> Self {
        let source = |overridden: bool| if overridden { "override" } else { "default" }.to_string();
        Self {
            egress: profile_str(p.egress).to_string(),
            ingress: profile_str(p.ingress).to_string(),
            egress_source: source(p.egress_overridden),
            ingress_source: source(p.ingress_overridden),
        }
    }
}

#[derive(Debug, Serialize, ToSchema)]
pub struct EffectivePolicyResponse {
    pub subject_kind: String,
    pub subject_id: i64,
    pub profile: ProfileResponse,
    /// Rules whose subject is this workload.
    pub rules: Vec<NetworkPolicyRuleResponse>,
    /// Link rules of other workloads that name this one; they are accepted
    /// even when this workload's ingress is restricted.
    pub inbound_links: Vec<NetworkPolicyRuleResponse>,
    /// Nodes whose mode is `enforce`. Empty: the policy is shown but not
    /// enforced anywhere yet (every node is `legacy` or `shadow`).
    pub enforcing_nodes: Vec<i32>,
    pub enforced: bool,
}

impl From<EffectivePolicy> for EffectivePolicyResponse {
    fn from(p: EffectivePolicy) -> Self {
        Self {
            subject_kind: p.subject.kind.as_str().to_string(),
            subject_id: p.subject.id,
            profile: p.profiles.into(),
            rules: p.rules.into_iter().map(Into::into).collect(),
            inbound_links: p.inbound_links.into_iter().map(Into::into).collect(),
            enforced: !p.enforcing_nodes.is_empty(),
            enforcing_nodes: p.enforcing_nodes,
        }
    }
}

#[derive(Debug, Deserialize, ToSchema)]
pub struct SetProfileRequest {
    /// `open`, `restricted`, `none`, or omitted for the kind's default.
    pub egress: Option<String>,
    pub ingress: Option<String>,
}

#[derive(Debug, Serialize, ToSchema)]
pub struct NodeNetworkPolicyModeResponse {
    /// 0 is the control plane.
    pub node_id: i32,
    pub name: String,
    pub role: String,
    pub status: String,
    /// `legacy` (today's behaviour), `shadow` (computed and logged, not
    /// applied) or `enforce`.
    #[schema(example = "legacy")]
    pub mode: String,
}

impl From<NodeModeEntry> for NodeNetworkPolicyModeResponse {
    fn from(n: NodeModeEntry) -> Self {
        Self {
            node_id: n.node_id,
            name: n.name,
            role: n.role,
            status: n.status,
            mode: n.mode.as_str().to_string(),
        }
    }
}

#[derive(Debug, Serialize, ToSchema)]
pub struct NodeNetworkPolicyModeListResponse {
    pub nodes: Vec<NodeNetworkPolicyModeResponse>,
}

#[derive(Debug, Deserialize, ToSchema)]
pub struct SetNodeNetworkPolicyModeRequest {
    #[schema(example = "shadow")]
    pub mode: String,
}

// ----------------------------------------------------------------- audit

#[derive(Debug, Serialize)]
struct NetworkPolicyAudit {
    #[serde(skip)]
    operation: &'static str,
    #[serde(flatten)]
    context: AuditContext,
    #[serde(skip_serializing_if = "Option::is_none")]
    rule_id: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    subject: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    peer: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    action: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    node_id: Option<i32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    previous_mode: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    mode: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    egress: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    ingress: Option<String>,
}

impl AuditOperation for NetworkPolicyAudit {
    fn operation_type(&self) -> String {
        self.operation.to_string()
    }
    fn user_id(&self) -> Option<i32> {
        Some(self.context.user_id)
    }
    fn ip_address(&self) -> Option<String> {
        self.context.ip_address.clone()
    }
    fn user_agent(&self) -> &str {
        &self.context.user_agent
    }
    fn serialize(&self) -> anyhow::Result<String> {
        serde_json::to_string(self)
            .map_err(|e| anyhow::anyhow!("failed to serialize {} audit event: {e}", self.operation))
    }
}

impl NetworkPolicyAudit {
    fn new(operation: &'static str, auth: &AuthContext, metadata: &RequestMetadata) -> Self {
        Self {
            operation,
            context: AuditContext {
                user_id: auth.user_id(),
                ip_address: Some(metadata.ip_address.clone()),
                user_agent: metadata.user_agent.clone(),
            },
            rule_id: None,
            subject: None,
            peer: None,
            action: None,
            node_id: None,
            previous_mode: None,
            mode: None,
            egress: None,
            ingress: None,
        }
    }

    fn for_rule(mut self, rule: &network_policy_rules::Model) -> Self {
        self.rule_id = Some(rule.id);
        self.subject = Some(format!("{}/{}", rule.subject_kind, rule.subject_id));
        self.peer = Some(format!(
            "{} {} {}",
            rule.direction, rule.peer_kind, rule.peer_ref
        ));
        self.action = Some(rule.action.clone());
        self
    }

    async fn record(self, audit: &Arc<dyn AuditLogger>) {
        if let Err(e) = audit.create_audit_log(&self).await {
            error!(
                error = %e,
                operation = self.operation,
                "Failed to create network policy audit log"
            );
        }
    }
}

// -------------------------------------------------------------- handlers

#[utoipa::path(
    tag = "Network Policy",
    get,
    path = "/network-policy/rules",
    params(ListRulesQuery),
    responses(
        (status = 200, description = "Rules, newest first", body = NetworkPolicyRuleListResponse),
        (status = 400, description = "Invalid filter", body = problemdetails::ProblemDetails),
        (status = 401, description = "Unauthorized", body = problemdetails::ProblemDetails),
        (status = 403, description = "Insufficient permissions", body = problemdetails::ProblemDetails),
        (status = 500, description = "Internal server error", body = problemdetails::ProblemDetails)
    ),
    security(("bearer_auth" = []))
)]
async fn list_rules(
    RequireAuth(auth): RequireAuth,
    State(state): State<Arc<NetworkPolicyAppState>>,
    Query(query): Query<ListRulesQuery>,
) -> Result<impl IntoResponse, Problem> {
    require_reader(&auth)?;
    let subject = match (query.subject_kind.as_deref(), query.subject_id) {
        (None, None) => None,
        (Some(kind), Some(id)) => Some(parse_subject(kind, id).map_err(bad_request)?),
        _ => {
            return Err(bad_request(
                "subject_kind and subject_id filter together".to_string(),
            ))
        }
    };
    let page = query.page.unwrap_or(1).max(1);
    let page_size = query
        .page_size
        .unwrap_or(crate::service::DEFAULT_PAGE_SIZE)
        .clamp(1, crate::service::MAX_PAGE_SIZE);
    let (items, total) = state
        .service
        .list_rules(
            RuleFilter {
                subject,
                origin: query.origin,
            },
            Some(page),
            Some(page_size),
        )
        .await?;
    Ok(Json(NetworkPolicyRuleListResponse {
        items: items.into_iter().map(Into::into).collect(),
        total,
        page,
        page_size,
    }))
}

#[utoipa::path(
    tag = "Network Policy",
    post,
    path = "/network-policy/rules",
    request_body = CreateNetworkPolicyRuleRequest,
    responses(
        (status = 201, description = "Rule created", body = NetworkPolicyRuleResponse),
        (status = 400, description = "Invalid rule", body = problemdetails::ProblemDetails),
        (status = 401, description = "Unauthorized", body = problemdetails::ProblemDetails),
        (status = 403, description = "Instance administrators only", body = problemdetails::ProblemDetails),
        (status = 500, description = "Internal server error", body = problemdetails::ProblemDetails)
    ),
    security(("bearer_auth" = []))
)]
async fn create_rule(
    RequireAuth(auth): RequireAuth,
    State(state): State<Arc<NetworkPolicyAppState>>,
    Extension(metadata): Extension<RequestMetadata>,
    Json(request): Json<CreateNetworkPolicyRuleRequest>,
) -> Result<impl IntoResponse, Problem> {
    require_admin(&auth)?;
    let rule = request.into_rule()?;
    let created = state
        .service
        .create_rule(rule, Some(auth.user_id()))
        .await?;
    NetworkPolicyAudit::new("NETWORK_POLICY_RULE_CREATED", &auth, &metadata)
        .for_rule(&created)
        .record(&state.audit)
        .await;
    Ok((
        StatusCode::CREATED,
        Json(NetworkPolicyRuleResponse::from(created)),
    ))
}

#[utoipa::path(
    tag = "Network Policy",
    get,
    path = "/network-policy/rules/{rule_id}",
    params(("rule_id" = i64, Path, description = "Rule id")),
    responses(
        (status = 200, description = "The rule", body = NetworkPolicyRuleResponse),
        (status = 401, description = "Unauthorized", body = problemdetails::ProblemDetails),
        (status = 403, description = "Insufficient permissions", body = problemdetails::ProblemDetails),
        (status = 404, description = "No such rule", body = problemdetails::ProblemDetails),
        (status = 500, description = "Internal server error", body = problemdetails::ProblemDetails)
    ),
    security(("bearer_auth" = []))
)]
async fn get_rule(
    RequireAuth(auth): RequireAuth,
    State(state): State<Arc<NetworkPolicyAppState>>,
    Path(rule_id): Path<i64>,
) -> Result<impl IntoResponse, Problem> {
    require_reader(&auth)?;
    let rule = state.service.get_rule(rule_id).await?;
    Ok(Json(NetworkPolicyRuleResponse::from(rule)))
}

#[utoipa::path(
    tag = "Network Policy",
    delete,
    path = "/network-policy/rules/{rule_id}",
    params(("rule_id" = i64, Path, description = "Rule id")),
    responses(
        (status = 204, description = "Rule deleted"),
        (status = 401, description = "Unauthorized", body = problemdetails::ProblemDetails),
        (status = 403, description = "Instance administrators only", body = problemdetails::ProblemDetails),
        (status = 404, description = "No such rule", body = problemdetails::ProblemDetails),
        (status = 409, description = "The rule belongs to a link; unlink instead", body = problemdetails::ProblemDetails),
        (status = 500, description = "Internal server error", body = problemdetails::ProblemDetails)
    ),
    security(("bearer_auth" = []))
)]
async fn delete_rule(
    RequireAuth(auth): RequireAuth,
    State(state): State<Arc<NetworkPolicyAppState>>,
    Extension(metadata): Extension<RequestMetadata>,
    Path(rule_id): Path<i64>,
) -> Result<impl IntoResponse, Problem> {
    require_admin(&auth)?;
    let deleted = state.service.delete_rule(rule_id).await?;
    NetworkPolicyAudit::new("NETWORK_POLICY_RULE_DELETED", &auth, &metadata)
        .for_rule(&deleted)
        .record(&state.audit)
        .await;
    Ok(StatusCode::NO_CONTENT)
}

fn subject_from_path(kind: &str, id: i64) -> Result<WorkloadRef, Problem> {
    parse_subject(kind, id).map_err(bad_request)
}

#[utoipa::path(
    tag = "Network Policy",
    get,
    path = "/network-policy/subjects/{subject_kind}/{subject_id}",
    params(
        ("subject_kind" = String, Path, description = "deployment, sandbox, service, function or job"),
        ("subject_id" = i64, Path, description = "Environment id for deployments; row id otherwise")
    ),
    responses(
        (status = 200, description = "What this workload may reach, and where it is enforced", body = EffectivePolicyResponse),
        (status = 400, description = "Unknown kind or missing workload", body = problemdetails::ProblemDetails),
        (status = 401, description = "Unauthorized", body = problemdetails::ProblemDetails),
        (status = 403, description = "Insufficient permissions", body = problemdetails::ProblemDetails),
        (status = 500, description = "Internal server error", body = problemdetails::ProblemDetails)
    ),
    security(("bearer_auth" = []))
)]
async fn get_effective_policy(
    RequireAuth(auth): RequireAuth,
    State(state): State<Arc<NetworkPolicyAppState>>,
    Path((subject_kind, subject_id)): Path<(String, i64)>,
) -> Result<impl IntoResponse, Problem> {
    require_reader(&auth)?;
    let subject = subject_from_path(&subject_kind, subject_id)?;
    let policy = state.service.effective(subject).await?;
    Ok(Json(EffectivePolicyResponse::from(policy)))
}

#[utoipa::path(
    tag = "Network Policy",
    put,
    path = "/network-policy/subjects/{subject_kind}/{subject_id}/profile",
    params(
        ("subject_kind" = String, Path, description = "deployment, sandbox, service, function or job"),
        ("subject_id" = i64, Path, description = "Environment id for deployments; row id otherwise")
    ),
    request_body = SetProfileRequest,
    responses(
        (status = 200, description = "The profile now in effect", body = ProfileResponse),
        (status = 400, description = "Invalid profile or missing workload", body = problemdetails::ProblemDetails),
        (status = 401, description = "Unauthorized", body = problemdetails::ProblemDetails),
        (status = 403, description = "Instance administrators only", body = problemdetails::ProblemDetails),
        (status = 500, description = "Internal server error", body = problemdetails::ProblemDetails)
    ),
    security(("bearer_auth" = []))
)]
async fn set_profile(
    RequireAuth(auth): RequireAuth,
    State(state): State<Arc<NetworkPolicyAppState>>,
    Extension(metadata): Extension<RequestMetadata>,
    Path((subject_kind, subject_id)): Path<(String, i64)>,
    Json(request): Json<SetProfileRequest>,
) -> Result<impl IntoResponse, Problem> {
    require_admin(&auth)?;
    let subject = subject_from_path(&subject_kind, subject_id)?;
    let egress = request
        .egress
        .as_deref()
        .map(parse_profile)
        .transpose()
        .map_err(bad_request)?;
    let ingress = request
        .ingress
        .as_deref()
        .map(parse_profile)
        .transpose()
        .map_err(bad_request)?;
    let profiles = state
        .service
        .set_profile(subject, egress, ingress, Some(auth.user_id()))
        .await?;
    let mut audit = NetworkPolicyAudit::new("NETWORK_POLICY_PROFILE_SET", &auth, &metadata);
    audit.subject = Some(subject.to_string());
    audit.egress = Some(profile_str(profiles.egress).to_string());
    audit.ingress = Some(profile_str(profiles.ingress).to_string());
    audit.record(&state.audit).await;
    Ok(Json(ProfileResponse::from(profiles)))
}

#[utoipa::path(
    tag = "Network Policy",
    get,
    path = "/network-policy/nodes",
    responses(
        (status = 200, description = "Every node's mode; node 0 is the control plane", body = NodeNetworkPolicyModeListResponse),
        (status = 401, description = "Unauthorized", body = problemdetails::ProblemDetails),
        (status = 403, description = "Insufficient permissions", body = problemdetails::ProblemDetails),
        (status = 500, description = "Internal server error", body = problemdetails::ProblemDetails)
    ),
    security(("bearer_auth" = []))
)]
async fn list_node_modes(
    RequireAuth(auth): RequireAuth,
    State(state): State<Arc<NetworkPolicyAppState>>,
) -> Result<impl IntoResponse, Problem> {
    require_reader(&auth)?;
    let nodes = state.service.node_modes().await?;
    Ok(Json(NodeNetworkPolicyModeListResponse {
        nodes: nodes.into_iter().map(Into::into).collect(),
    }))
}

#[utoipa::path(
    tag = "Network Policy",
    patch,
    path = "/network-policy/nodes/{node_id}",
    params(("node_id" = i32, Path, description = "Node id; 0 is the control plane")),
    request_body = SetNodeNetworkPolicyModeRequest,
    responses(
        (status = 200, description = "The node's mode now", body = NodeNetworkPolicyModeResponse),
        (status = 400, description = "Unknown mode", body = problemdetails::ProblemDetails),
        (status = 401, description = "Unauthorized", body = problemdetails::ProblemDetails),
        (status = 403, description = "Instance administrators only", body = problemdetails::ProblemDetails),
        (status = 404, description = "No such node", body = problemdetails::ProblemDetails),
        (status = 500, description = "Internal server error", body = problemdetails::ProblemDetails)
    ),
    security(("bearer_auth" = []))
)]
async fn set_node_mode(
    RequireAuth(auth): RequireAuth,
    State(state): State<Arc<NetworkPolicyAppState>>,
    Extension(metadata): Extension<RequestMetadata>,
    Path(node_id): Path<i32>,
    Json(request): Json<SetNodeNetworkPolicyModeRequest>,
) -> Result<impl IntoResponse, Problem> {
    require_admin(&auth)?;
    let mode = request
        .mode
        .parse::<NodeMode>()
        .map_err(|e| bad_request(e.to_string()))?;
    let (previous, node) = state.service.set_node_mode(node_id, mode).await?;
    let mut audit = NetworkPolicyAudit::new("NETWORK_POLICY_NODE_MODE_SET", &auth, &metadata);
    audit.node_id = Some(node_id);
    audit.previous_mode = Some(previous.as_str().to_string());
    audit.mode = Some(mode.as_str().to_string());
    audit.record(&state.audit).await;
    Ok(Json(NodeNetworkPolicyModeResponse::from(node)))
}

#[cfg(test)]
mod tests {
    use super::*;
    use temps_auth::permissions::Role;
    use temps_entities::users;

    fn auth(role: Role) -> AuthContext {
        let now = chrono::Utc::now();
        AuthContext::new_session(
            users::Model {
                id: 7,
                name: "Policy Tester".to_string(),
                email: "policy-tester@example.test".to_string(),
                password_hash: None,
                email_verified: true,
                email_verification_token: None,
                email_verification_expires: None,
                password_reset_token: None,
                password_reset_expires: None,
                must_change_password: false,
                deleted_at: None,
                mfa_secret: None,
                mfa_enabled: false,
                mfa_recovery_codes: None,
                oidc_subject: None,
                oidc_provider_id: None,
                created_at: now,
                updated_at: now,
            },
            role,
        )
    }

    #[test]
    fn only_instance_admins_change_policy_or_modes() {
        assert!(require_admin(&auth(Role::Admin)).is_ok());
        for role in [Role::User, Role::Reader] {
            let err = require_admin(&auth(role)).expect_err("writes are admin only");
            assert_eq!(err.status_code, StatusCode::FORBIDDEN);
        }
    }

    #[test]
    fn readers_need_settings_read() {
        assert!(require_reader(&auth(Role::Admin)).is_ok());
    }

    #[test]
    fn errors_map_to_their_status() {
        let cases = [
            (
                NetworkPolicyError::RuleNotFound { rule_id: 3 },
                StatusCode::NOT_FOUND,
            ),
            (
                NetworkPolicyError::NodeNotFound { node_id: 9 },
                StatusCode::NOT_FOUND,
            ),
            (NetworkPolicyError::validation("x"), StatusCode::BAD_REQUEST),
            (
                NetworkPolicyError::LinkOwned {
                    rule_id: 1,
                    link_table: "sandbox_services".into(),
                    link_id: 2,
                },
                StatusCode::CONFLICT,
            ),
            (
                NetworkPolicyError::InvalidStoredRule {
                    rule_id: 1,
                    reason: "bad".into(),
                },
                StatusCode::INTERNAL_SERVER_ERROR,
            ),
        ];
        for (error, status) in cases {
            let message = error.to_string();
            let problem = Problem::from(error);
            assert_eq!(problem.status_code, status, "{message}");
        }
    }

    #[test]
    fn requests_are_validated_before_the_service() {
        let base = CreateNetworkPolicyRuleRequest {
            subject_kind: "sandbox".into(),
            subject_id: 1,
            direction: "egress".into(),
            peer_kind: "host".into(),
            peer_ref: "registry.example.test".into(),
            port_start: Some(443),
            port_end: None,
            protocol: None,
            action: "allow".into(),
            description: None,
        };
        let rule = base.clone().into_rule().unwrap();
        assert_eq!(rule.ports, Some(PortRange::single(443)));
        for broken in [
            CreateNetworkPolicyRuleRequest {
                subject_kind: "vm".into(),
                ..base.clone()
            },
            CreateNetworkPolicyRuleRequest {
                direction: "out".into(),
                ..base.clone()
            },
            CreateNetworkPolicyRuleRequest {
                peer_ref: "not a host".into(),
                ..base.clone()
            },
            CreateNetworkPolicyRuleRequest {
                action: "maybe".into(),
                ..base.clone()
            },
            CreateNetworkPolicyRuleRequest {
                port_start: None,
                port_end: Some(80),
                ..base.clone()
            },
        ] {
            let err = broken.into_rule().expect_err("rejected");
            assert_eq!(err.status_code, StatusCode::BAD_REQUEST);
        }
    }
}
