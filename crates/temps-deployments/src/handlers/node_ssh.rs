// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Add a server over SSH (ADR 048 D2c).
//!
//! The operator first reads the server's host key (`POST /nodes/ssh/host-key`)
//! and confirms it, then starts the enrollment with credentials that are
//! used for it and dropped. The control plane logs in, makes sure `temps` is
//! installed, runs a pairing (D2b) on the server and starts its agent; the
//! enrollment's row shows each step and the server's output.

use std::net::{IpAddr, SocketAddr};
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use axum::{
    extract::{FromRef, Path, Query, State},
    http::StatusCode,
    response::IntoResponse,
    Extension, Json,
};
use serde::{Deserialize, Serialize};
use temps_auth::{permission_guard, require_sensitive_action, RequireAuth};
use temps_core::problemdetails::{self, Problem};
use temps_core::{
    AuditContext, AuditLogger, RequestMetadata, SensitiveAction, SensitiveActionAuthorizer,
};
use temps_entities::node_ssh_enrollments;
use temps_network::mesh::MeshError;
use tracing::error;
use utoipa::{IntoParams, ToSchema};
use zeroize::Zeroizing;

use crate::handlers::audit::NodeSshEnrollmentStartedAudit;
use crate::handlers::node_pairings::{pairing_problem, parse_node_endpoint, start_pairing_audited};
use crate::handlers::types::AppState;
use crate::services::node_pairing_admin as admin;
use crate::services::node_ssh::{self, Enrollment, SshAuth, SshError};
use crate::services::node_ssh_enrollment::{
    EnrollmentJob, EnrollmentSummary, NewEnrollment, NodeSshEnrollmentError,
    NodeSshEnrollmentService,
};

const DEFAULT_SSH_PORT: u16 = 22;
/// Enrollments per page unless asked otherwise, and at most.
const DEFAULT_PER_PAGE: u64 = 20;
const MAX_PER_PAGE: u64 = 100;

/// A server to read the SSH host key of.
#[derive(Debug, Clone, Deserialize, ToSchema)]
#[schema(as = NodeSshHostKeyRequest)]
pub struct SshHostKeyRequest {
    /// Hostname or IP address.
    pub host: String,
    /// SSH port (default 22).
    pub port: Option<u16>,
}

/// The server's SSH host key. Compare the fingerprint with the server's
/// (`ssh-keygen -lf /etc/ssh/ssh_host_ed25519_key.pub` on it) before
/// enrolling it.
#[derive(Debug, Clone, Serialize, ToSchema)]
#[schema(as = NodeSshHostKeyResponse)]
pub struct SshHostKeyResponse {
    /// `ip:port` the control plane connected to.
    pub address: String,
    /// e.g. `ssh-ed25519`.
    pub algorithm: String,
    /// `SHA256:…`, as `ssh-keygen -l` prints it.
    pub fingerprint: String,
}

/// How to log in to the server. Used for this enrollment only, never stored.
/// Its `Debug` never prints the credentials.
#[derive(Clone, Deserialize, ToSchema)]
#[serde(tag = "method", rename_all = "snake_case")]
#[schema(as = NodeSshCredentials)]
pub enum SshCredentials {
    /// The user's password; also used for `sudo` if it asks for one.
    Password { password: String },
    /// An OpenSSH or PEM private key, with its passphrase if it has one.
    PrivateKey {
        private_key: String,
        passphrase: Option<String>,
    },
    /// The SSH agent of the control plane's `temps serve` process.
    Agent,
}

impl std::fmt::Debug for SshCredentials {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::Password { .. } => "SshCredentials::Password",
            Self::PrivateKey { .. } => "SshCredentials::PrivateKey",
            Self::Agent => "SshCredentials::Agent",
        })
    }
}

/// Add a server over SSH.
#[derive(Clone, Deserialize, ToSchema)]
#[schema(as = NodeSshEnrollmentCreateRequest)]
pub struct CreateSshEnrollmentRequest {
    /// Hostname or IP address.
    pub host: String,
    /// SSH port (default 22).
    pub port: Option<u16>,
    /// User to log in as: root, or a user with sudo.
    pub user: String,
    pub credentials: SshCredentials,
    /// The host-key fingerprint the operator confirmed (from
    /// `POST /nodes/ssh/host-key`). The enrollment stops if the server
    /// presents another key.
    pub host_key_fingerprint: String,
    /// Name the node registers under. Defaults to `worker-<random>`.
    pub name: Option<String>,
    /// The server's public address for WireGuard, `ip` or `ip:port`, when it
    /// is not the address SSH connects to.
    pub node_address: Option<String>,
}

/// An "add server over SSH" as the Worker Nodes page shows it.
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct NodeSshEnrollmentResponse {
    pub id: i32,
    pub name: String,
    pub host: String,
    pub ssh_address: String,
    pub ssh_user: String,
    /// `password`, `private_key` or `agent`.
    pub auth_method: String,
    pub host_key_fingerprint: String,
    /// The pairing it runs on the server.
    pub pairing_id: Option<i32>,
    /// `running`, `succeeded` or `failed`.
    pub status: String,
    /// What it is doing, or was doing when it stopped.
    pub step: String,
    /// What it did, with the server's output (its last 64 KiB).
    pub log: String,
    /// Why it failed, and what to do.
    pub error: Option<String>,
    /// `service` (systemd unit) or `detached` (started without a service
    /// manager: it does not come back after a reboot).
    pub agent_mode: Option<String>,
    pub node_id: Option<i32>,
    pub created_at: String,
    pub finished_at: Option<String>,
}

impl From<node_ssh_enrollments::Model> for NodeSshEnrollmentResponse {
    fn from(model: node_ssh_enrollments::Model) -> Self {
        Self {
            id: model.id,
            name: model.name,
            host: model.host,
            ssh_address: model.ssh_address,
            ssh_user: model.ssh_user,
            auth_method: model.auth_method,
            host_key_fingerprint: model.host_key_fingerprint,
            pairing_id: model.pairing_id,
            status: model.status,
            step: model.step,
            log: model.log,
            error: model.error,
            agent_mode: model.agent_mode,
            node_id: model.node_id,
            created_at: model.created_at.to_rfc3339(),
            finished_at: model.finished_at.map(|at| at.to_rfc3339()),
        }
    }
}

/// An "add server over SSH" in a list: everything but its log, which
/// `GET /nodes/ssh/enrollments/{enrollment_id}` returns.
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct NodeSshEnrollmentSummary {
    pub id: i32,
    pub name: String,
    pub host: String,
    pub ssh_address: String,
    pub ssh_user: String,
    /// `password`, `private_key` or `agent`.
    pub auth_method: String,
    pub host_key_fingerprint: String,
    /// The pairing it runs on the server.
    pub pairing_id: Option<i32>,
    /// `running`, `succeeded` or `failed`.
    pub status: String,
    /// What it is doing, or was doing when it stopped.
    pub step: String,
    /// Why it failed, and what to do.
    pub error: Option<String>,
    /// `service` or `detached`.
    pub agent_mode: Option<String>,
    pub node_id: Option<i32>,
    pub created_at: String,
    pub finished_at: Option<String>,
}

impl From<EnrollmentSummary> for NodeSshEnrollmentSummary {
    fn from(summary: EnrollmentSummary) -> Self {
        Self {
            id: summary.id,
            name: summary.name,
            host: summary.host,
            ssh_address: summary.ssh_address,
            ssh_user: summary.ssh_user,
            auth_method: summary.auth_method,
            host_key_fingerprint: summary.host_key_fingerprint,
            pairing_id: summary.pairing_id,
            status: summary.status,
            step: summary.step,
            error: summary.error,
            agent_mode: summary.agent_mode,
            node_id: summary.node_id,
            created_at: summary.created_at.to_rfc3339(),
            finished_at: summary.finished_at.map(|at| at.to_rfc3339()),
        }
    }
}

/// A page of servers added over SSH, newest first.
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct NodeSshEnrollmentListResponse {
    pub enrollments: Vec<NodeSshEnrollmentSummary>,
    /// Enrollments on every page.
    pub total: u64,
    pub page: u64,
    pub per_page: u64,
}

#[derive(Debug, Clone, Default, Deserialize, IntoParams)]
#[into_params(parameter_in = Query)]
pub struct NodeSshEnrollmentListQuery {
    /// Page, from 1 (default 1).
    pub page: Option<u64>,
    /// Enrollments per page (default 20, at most 100).
    pub per_page: Option<u64>,
}

/// A pairing started for an enrollment.
pub struct StartedPairing {
    pub id: i32,
    /// The name the node registers under.
    pub name: String,
    /// The `tpair1.` code; secret.
    pub code: Zeroizing<String>,
}

/// The mesh pairing (D2b) an enrollment runs on the server.
#[async_trait]
pub trait SshEnrollmentPairings: Send + Sync {
    /// The mesh's WireGuard port; a problem when the mesh is off.
    async fn mesh_port(&self) -> Result<u16, Problem>;
    /// Start a pairing, audited as done by `context`.
    async fn start(
        &self,
        context: AuditContext,
        node_endpoint: SocketAddr,
        name: Option<&str>,
    ) -> Result<StartedPairing, Problem>;
}

struct MeshPairings(Arc<AppState>);

#[async_trait]
impl SshEnrollmentPairings for MeshPairings {
    async fn mesh_port(&self) -> Result<u16, Problem> {
        self.0
            .node_pairing_admin
            .mesh_port()
            .await
            .map_err(pairing_problem)
    }

    async fn start(
        &self,
        context: AuditContext,
        node_endpoint: SocketAddr,
        name: Option<&str>,
    ) -> Result<StartedPairing, Problem> {
        let admin::StartedPairing { pairing, code } = start_pairing_audited(
            &self.0.node_pairing_admin,
            self.0.audit_service.as_ref(),
            context,
            node_endpoint,
            name,
        )
        .await?;
        Ok(StartedPairing {
            id: pairing.id,
            name: pairing.name,
            code,
        })
    }
}

/// What the SSH enrollment handlers use of the deployments state.
#[derive(Clone)]
pub struct NodeSshState {
    pub enrollments: NodeSshEnrollmentService,
    pub pairings: Arc<dyn SshEnrollmentPairings>,
    pub audit_service: Arc<dyn AuditLogger>,
    pub sensitive_action_authorizer: Arc<dyn SensitiveActionAuthorizer>,
}

impl FromRef<Arc<AppState>> for NodeSshState {
    fn from_ref(app_state: &Arc<AppState>) -> Self {
        Self {
            enrollments: NodeSshEnrollmentService::new(
                app_state.db.clone(),
                app_state.enrollment_token_service.clone(),
                app_state.audit_service.clone(),
            ),
            pairings: Arc::new(MeshPairings(app_state.clone())),
            audit_service: app_state.audit_service.clone(),
            sensitive_action_authorizer: app_state.sensitive_action_authorizer.clone(),
        }
    }
}

fn bad_request(title: &str, detail: impl Into<String>) -> Problem {
    problemdetails::new(StatusCode::BAD_REQUEST)
        .with_title(title)
        .with_detail(detail.into())
}

fn enrollment_problem(error: NodeSshEnrollmentError) -> Problem {
    match error {
        NodeSshEnrollmentError::Database { operation, source } => {
            error!("SSH enrollment: could not {operation}: {source}");
            problemdetails::new(StatusCode::INTERNAL_SERVER_ERROR)
                .with_title("SSH Enrollment Error")
                .with_detail(format!("Could not {operation}; see the server logs."))
        }
        NodeSshEnrollmentError::NotFound { id } => problemdetails::new(StatusCode::NOT_FOUND)
            .with_title("Enrollment Not Found")
            .with_detail(format!("No SSH enrollment with id {id}")),
        NodeSshEnrollmentError::TooManyRunning { running, limit } => {
            problemdetails::new(StatusCode::CONFLICT)
                .with_title("Too Many Enrollments Running")
                .with_detail(format!(
                    "{running} servers are already being added over SSH (at most {limit} at \
                     once); wait for one to finish."
                ))
        }
    }
}

/// The address to connect to for `host`. Hostnames are resolved; like node
/// endpoints, loopback, link-local and cloud metadata addresses are refused,
/// and private ones allowed (servers on a LAN or VPC are added by those).
async fn resolve(host: &str, port: Option<u16>) -> Result<SocketAddr, Problem> {
    let host = host.trim();
    let bare = host.trim_start_matches('[').trim_end_matches(']');
    let valid_hostname = (1..=253).contains(&bare.len())
        && bare
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'-' | b':'))
        && !bare.starts_with('-');
    if !valid_hostname {
        return Err(bad_request(
            "Invalid Host",
            format!("'{host}' is not a hostname or IP address"),
        ));
    }
    let port = port.unwrap_or(DEFAULT_SSH_PORT);
    if port == 0 {
        return Err(bad_request("Invalid Port", "The SSH port must not be 0."));
    }
    let literal = bare.parse::<IpAddr>().ok();
    let ip = match literal {
        Some(ip) => ip,
        None => {
            let addresses: Vec<SocketAddr> = tokio::time::timeout(
                Duration::from_secs(5),
                tokio::net::lookup_host((bare, port)),
            )
            .await
            .map_err(|_| bad_request("Unknown Host", format!("Resolving '{bare}' timed out.")))?
            .map_err(|error| {
                bad_request(
                    "Unknown Host",
                    format!("'{bare}' does not resolve ({error})."),
                )
            })?
            .collect();
            addresses
                .iter()
                .find(|address| address.is_ipv4())
                .or_else(|| addresses.first())
                .map(|address| address.ip())
                .ok_or_else(|| bad_request("Unknown Host", format!("'{bare}' has no addresses.")))?
        }
    };
    temps_network::mesh::parse_endpoint(&SocketAddr::new(ip.to_canonical(), port).to_string())
        .map_err(|error| match error {
            MeshError::InvalidEndpoint { reason, .. } => bad_request(
                "Cannot Connect To This Host",
                match literal {
                    Some(_) => format!("{ip} {reason}"),
                    None => format!("{host} resolves to {ip}, which {reason}"),
                },
            ),
            other => bad_request("Cannot Connect To This Host", other.to_string()),
        })
}

fn ssh_problem(error: SshError) -> Problem {
    let status = match &error {
        // The credentials or the key the operator gave were refused.
        SshError::Auth(_) => StatusCode::BAD_REQUEST,
        // Not the key the operator confirmed.
        SshError::HostKeyChanged { .. } => StatusCode::CONFLICT,
        // The server could not be reached or talked to.
        SshError::Connect { .. } | SshError::Remote(_) | SshError::Session(_) => {
            StatusCode::BAD_GATEWAY
        }
    };
    problemdetails::new(status)
        .with_title("SSH Connection Failed")
        .with_detail(error.to_string())
}

/// A login name as SSH carries it: no whitespace or control characters.
fn valid_user(user: &str) -> bool {
    (1..=64).contains(&user.len())
        && user
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'.' | b'-' | b'@'))
        && !user.starts_with('-')
}

/// `SHA256:` and 43 unpadded base64 characters.
fn valid_fingerprint(fingerprint: &str) -> bool {
    fingerprint.strip_prefix("SHA256:").is_some_and(|hash| {
        hash.len() == 43
            && hash
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'+' | b'/'))
    })
}

/// Read a server's SSH host key, without logging in, so the operator can
/// confirm it before adding the server.
#[utoipa::path(
    tag = "Nodes",
    post,
    path = "/nodes/ssh/host-key",
    operation_id = "NodeSshHostKey",
    request_body = SshHostKeyRequest,
    responses(
        (status = 200, description = "The server's host key", body = SshHostKeyResponse),
        (status = 400, description = "Invalid or unusable host"),
        (status = 401, description = "Unauthorized"),
        (status = 403, description = "Insufficient permissions"),
        (status = 428, description = "Re-authentication required"),
        (status = 502, description = "The server could not be reached over SSH")
    ),
    security(("bearer_auth" = []))
)]
pub async fn node_ssh_host_key(
    RequireAuth(auth): RequireAuth,
    State(state): State<NodeSshState>,
    Json(request): Json<SshHostKeyRequest>,
) -> Result<impl IntoResponse, Problem> {
    permission_guard!(auth, SettingsWrite);
    // It opens a connection from this server to an address the caller
    // chooses: the same step-up as the enrollment it precedes.
    require_sensitive_action(
        state.sensitive_action_authorizer.as_ref(),
        &auth,
        SensitiveAction::AddNodeOverSsh,
    )
    .await?;
    let address = resolve(&request.host, request.port).await?;
    let key = node_ssh::host_key(address).await.map_err(ssh_problem)?;
    Ok(Json(SshHostKeyResponse {
        address: address.to_string(),
        algorithm: key.algorithm,
        fingerprint: key.fingerprint,
    }))
}

/// Add a server over SSH: log in with the given credentials, install `temps`
/// if needed, pair the server and start its agent. Runs in the background;
/// follow it with `GET /nodes/ssh/enrollments/{enrollment_id}`.
#[utoipa::path(
    tag = "Nodes",
    post,
    path = "/nodes/ssh/enrollments",
    operation_id = "NodeSshEnrollmentCreate",
    request_body = CreateSshEnrollmentRequest,
    responses(
        (status = 202, description = "Enrollment started", body = NodeSshEnrollmentResponse),
        (status = 400, description = "Invalid host, user, fingerprint or name"),
        (status = 401, description = "Unauthorized"),
        (status = 403, description = "Insufficient permissions"),
        (status = 409, description = "The mesh is off or not ready, or too many enrollments are running"),
        (status = 428, description = "Re-authentication required"),
        (status = 500, description = "Internal server error")
    ),
    security(("bearer_auth" = []))
)]
pub async fn create_node_ssh_enrollment(
    RequireAuth(auth): RequireAuth,
    State(state): State<NodeSshState>,
    Extension(metadata): Extension<RequestMetadata>,
    Json(request): Json<CreateSshEnrollmentRequest>,
) -> Result<impl IntoResponse, Problem> {
    permission_guard!(auth, SettingsWrite);
    require_sensitive_action(
        state.sensitive_action_authorizer.as_ref(),
        &auth,
        SensitiveAction::AddNodeOverSsh,
    )
    .await?;

    let user = request.user.trim().to_string();
    if !valid_user(&user) {
        return Err(bad_request(
            "Invalid User",
            "Use the login name only: letters, digits, '.', '_', '-' or '@'.",
        ));
    }
    let fingerprint = request.host_key_fingerprint.trim().to_string();
    if !valid_fingerprint(&fingerprint) {
        return Err(bad_request(
            "Invalid Host Key Fingerprint",
            "Expected SHA256:… as returned by POST /nodes/ssh/host-key.",
        ));
    }
    let mesh_port = state.pairings.mesh_port().await?;
    // A quick check before starting a pairing; `create` checks again under a
    // lock.
    let running = state
        .enrollments
        .running_count()
        .await
        .map_err(enrollment_problem)?;
    if running >= crate::services::node_ssh_enrollment::MAX_RUNNING {
        return Err(enrollment_problem(NodeSshEnrollmentError::TooManyRunning {
            running,
            limit: crate::services::node_ssh_enrollment::MAX_RUNNING,
        }));
    }
    let ssh_address = resolve(&request.host, request.port).await?;
    let node_endpoint = match request
        .node_address
        .as_deref()
        .map(str::trim)
        .filter(|a| !a.is_empty())
    {
        Some(address) => parse_node_endpoint(address, mesh_port)?,
        None => parse_node_endpoint(&ssh_address.ip().to_string(), mesh_port)?,
    };
    let auth_method = match request.credentials {
        SshCredentials::Password { password } => SshAuth::Password(Zeroizing::new(password)),
        SshCredentials::PrivateKey {
            private_key,
            passphrase,
        } => SshAuth::PrivateKey {
            key: Zeroizing::new(private_key),
            passphrase: passphrase.filter(|p| !p.is_empty()).map(Zeroizing::new),
        },
        SshCredentials::Agent => SshAuth::Agent,
    };

    let context = AuditContext {
        user_id: auth.user_id(),
        ip_address: Some(metadata.ip_address.clone()),
        user_agent: metadata.user_agent.clone(),
    };
    let pairing = state
        .pairings
        .start(context.clone(), node_endpoint, request.name.as_deref())
        .await?;
    let enrollment = match state
        .enrollments
        .create(NewEnrollment {
            name: pairing.name.clone(),
            host: request.host.trim().to_string(),
            ssh_address: ssh_address.to_string(),
            ssh_user: user.clone(),
            auth_method: auth_method.method().to_string(),
            host_key_fingerprint: fingerprint.clone(),
            pairing_id: pairing.id,
            created_by_user_id: Some(auth.user_id()),
        })
        .await
    {
        Ok(enrollment) => enrollment,
        Err(error) => {
            state.enrollments.abandon_pairing(pairing.id).await;
            return Err(enrollment_problem(error));
        }
    };

    let audit = NodeSshEnrollmentStartedAudit {
        context: context.clone(),
        enrollment_id: enrollment.id,
        pairing_id: pairing.id,
        name: enrollment.name.clone(),
        ssh_address: enrollment.ssh_address.clone(),
        ssh_user: enrollment.ssh_user.clone(),
        auth_method: enrollment.auth_method.clone(),
        host_key_fingerprint: enrollment.host_key_fingerprint.clone(),
    };
    if let Err(error) = state.audit_service.create_audit_log(&audit).await {
        error!(%error, "SSH enrollment started but audit record failed");
    }

    state.enrollments.spawn(EnrollmentJob {
        id: enrollment.id,
        pairing_id: pairing.id,
        name: enrollment.name.clone(),
        ssh_address: enrollment.ssh_address.clone(),
        request: Enrollment {
            address: ssh_address,
            user,
            auth: auth_method,
            host_key_fingerprint: fingerprint,
            pairing_code: pairing.code,
            pairing_timeout: node_ssh::PAIRING_TIMEOUT,
        },
        audit_context: context,
    });
    Ok((
        StatusCode::ACCEPTED,
        Json(NodeSshEnrollmentResponse::from(enrollment)),
    ))
}

/// Servers added over SSH, newest first, a page at a time. Items leave out
/// the log; `GET /nodes/ssh/enrollments/{enrollment_id}` has it.
#[utoipa::path(
    tag = "Nodes",
    get,
    path = "/nodes/ssh/enrollments",
    operation_id = "NodeSshEnrollmentList",
    params(NodeSshEnrollmentListQuery),
    responses(
        (status = 200, description = "SSH enrollments", body = NodeSshEnrollmentListResponse),
        (status = 401, description = "Unauthorized"),
        (status = 403, description = "Insufficient permissions"),
        (status = 500, description = "Internal server error")
    ),
    security(("bearer_auth" = []))
)]
pub async fn list_node_ssh_enrollments(
    RequireAuth(auth): RequireAuth,
    State(state): State<NodeSshState>,
    Query(query): Query<NodeSshEnrollmentListQuery>,
) -> Result<impl IntoResponse, Problem> {
    permission_guard!(auth, SettingsRead);
    let page = query.page.unwrap_or(1).max(1);
    let per_page = query
        .per_page
        .unwrap_or(DEFAULT_PER_PAGE)
        .clamp(1, MAX_PER_PAGE);
    let listed = state
        .enrollments
        .list(page, per_page)
        .await
        .map_err(enrollment_problem)?;
    Ok(Json(NodeSshEnrollmentListResponse {
        enrollments: listed.enrollments.into_iter().map(Into::into).collect(),
        total: listed.total,
        page,
        per_page,
    }))
}

/// One server being added over SSH, with its progress and log.
#[utoipa::path(
    tag = "Nodes",
    get,
    path = "/nodes/ssh/enrollments/{enrollment_id}",
    operation_id = "NodeSshEnrollmentGet",
    params(("enrollment_id" = i32, Path, description = "Enrollment ID")),
    responses(
        (status = 200, description = "The enrollment", body = NodeSshEnrollmentResponse),
        (status = 401, description = "Unauthorized"),
        (status = 403, description = "Insufficient permissions"),
        (status = 404, description = "No such enrollment"),
        (status = 500, description = "Internal server error")
    ),
    security(("bearer_auth" = []))
)]
pub async fn get_node_ssh_enrollment(
    RequireAuth(auth): RequireAuth,
    State(state): State<NodeSshState>,
    Path(enrollment_id): Path<i32>,
) -> Result<impl IntoResponse, Problem> {
    permission_guard!(auth, SettingsRead);
    let enrollment = state
        .enrollments
        .get(enrollment_id)
        .await
        .map_err(enrollment_problem)?;
    Ok(Json(NodeSshEnrollmentResponse::from(enrollment)))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::handlers::node_pairings::problem;
    use axum::body::Body;
    use axum::http::Request;
    use axum::routing::{get, post};
    use axum::Router;
    use sea_orm::{DatabaseBackend, DatabaseConnection, MockDatabase, MockExecResult, Value};
    use std::collections::BTreeMap;
    use std::sync::Mutex;
    use temps_auth::{AuthContext, Role};
    use temps_core::{
        SensitiveActionAuthorizationError, SensitiveActionDecision, SensitiveActionPrincipal,
    };
    use tower::ServiceExt;

    #[tokio::test]
    async fn hosts_resolve_to_connectable_addresses() {
        assert_eq!(
            resolve("198.51.100.7", None).await.unwrap(),
            "198.51.100.7:22".parse().unwrap()
        );
        assert_eq!(
            resolve(" [2001:db8::1] ", Some(2222)).await.unwrap(),
            "[2001:db8::1]:2222".parse().unwrap()
        );
        for refused in [
            "127.0.0.1",
            "::ffff:127.0.0.1",
            "169.254.169.254",
            "0.0.0.0",
            "localhost",
            "a b",
            "host;id",
            "-oProxyCommand=x",
        ] {
            assert!(resolve(refused, None).await.is_err(), "{refused}");
        }
        assert!(resolve("198.51.100.7", Some(0)).await.is_err());
    }

    #[test]
    fn credentials_never_reach_debug_output() {
        let credentials = SshCredentials::Password {
            password: "hunter2-password".to_string(),
        };
        assert!(!format!("{credentials:?}").contains("hunter2"));
    }

    #[test]
    fn users_are_plain_login_names() {
        assert!(valid_user("root"));
        assert!(valid_user("deploy-user.1"));
        assert!(!valid_user(""));
        assert!(!valid_user("-oProxyCommand"));
        assert!(!valid_user("a b"));
        assert!(!valid_user("root\n"));
    }

    #[test]
    fn fingerprints_are_openssh_sha256() {
        let fingerprint = format!("SHA256:{}", "A".repeat(43));
        assert!(valid_fingerprint(&fingerprint));
        assert!(valid_fingerprint(
            "SHA256:gGcMqXcQ/xFPz1fOY6ZhqcN0Un244iBB3Zkg5AcToJw"
        ));
        assert!(valid_fingerprint(&format!("SHA256:+/{}", "a".repeat(41))));
        assert!(!valid_fingerprint(&"A".repeat(43)));
        assert!(!valid_fingerprint("SHA256:short"));
        assert!(!valid_fingerprint(&format!("MD5:{}", "A".repeat(43))));
    }

    #[test]
    fn ssh_errors_map_to_what_the_operator_must_do() {
        let cases = [
            (SshError::Auth("refused".into()), StatusCode::BAD_REQUEST),
            (
                SshError::HostKeyChanged {
                    expected: "SHA256:a".into(),
                    presented: "SHA256:b".into(),
                },
                StatusCode::CONFLICT,
            ),
            (
                SshError::Connect {
                    address: "198.51.100.7:22".parse().unwrap(),
                    reason: "timed out".into(),
                },
                StatusCode::BAD_GATEWAY,
            ),
            (SshError::Remote("exit 1".into()), StatusCode::BAD_GATEWAY),
            (SshError::Session("reset".into()), StatusCode::BAD_GATEWAY),
        ];
        for (error, status) in cases {
            let detail = error.to_string();
            let problem = ssh_problem(error);
            assert_eq!(problem.status_code, status, "{detail}");
            assert_eq!(problem.body.get("detail"), Some(&serde_json::json!(detail)));
        }
    }

    #[test]
    fn enrollment_errors_map_to_statuses() {
        let problem = enrollment_problem(NodeSshEnrollmentError::NotFound { id: 4 });
        assert_eq!(problem.status_code, StatusCode::NOT_FOUND);
        let problem = enrollment_problem(NodeSshEnrollmentError::TooManyRunning {
            running: 5,
            limit: 5,
        });
        assert_eq!(problem.status_code, StatusCode::CONFLICT);
        let problem = enrollment_problem(NodeSshEnrollmentError::Database {
            operation: "list enrollments",
            source: sea_orm::DbErr::Custom("secret connection detail".into()),
        });
        assert_eq!(problem.status_code, StatusCode::INTERNAL_SERVER_ERROR);
        // The database error stays in the server log.
        assert!(!format!("{:?}", problem.body).contains("secret connection detail"));
    }

    #[test]
    fn schemas_carry_a_node_ssh_prefix() {
        use utoipa::OpenApi;
        let doc = crate::handlers::nodes::NodesApiDoc::openapi();
        let schemas = doc.components.expect("components").schemas;
        for name in [
            "NodeSshCredentials",
            "NodeSshHostKeyRequest",
            "NodeSshHostKeyResponse",
            "NodeSshEnrollmentCreateRequest",
            "NodeSshEnrollmentResponse",
            "NodeSshEnrollmentListResponse",
            "NodeSshEnrollmentSummary",
        ] {
            assert!(schemas.contains_key(name), "{name} missing");
        }
        for old in [
            "SshCredentials",
            "SshHostKeyRequest",
            "SshHostKeyResponse",
            "CreateSshEnrollmentRequest",
        ] {
            assert!(!schemas.contains_key(old), "{old} still published");
        }
        let summary = serde_json::to_value(&schemas["NodeSshEnrollmentSummary"]).unwrap();
        assert!(summary["properties"].get("log").is_none());
    }

    // ── Handlers ─────────────────────────────────────────────────────────

    const FINGERPRINT: &str = "SHA256:gGcMqXcQ/xFPz1fOY6ZhqcN0Un244iBB3Zkg5AcToJw";

    struct Authorizer(SensitiveActionDecision);

    #[async_trait]
    impl SensitiveActionAuthorizer for Authorizer {
        async fn authorize(
            &self,
            _action: &SensitiveAction,
            _principal: &SensitiveActionPrincipal,
        ) -> Result<SensitiveActionDecision, SensitiveActionAuthorizationError> {
            Ok(self.0.clone())
        }
    }

    /// (operation, ip, user agent) of every audit record.
    #[derive(Default)]
    struct Audits(Mutex<Vec<(String, Option<String>, String)>>);

    #[async_trait]
    impl AuditLogger for Audits {
        async fn create_audit_log(
            &self,
            operation: &dyn temps_core::AuditOperation,
        ) -> anyhow::Result<()> {
            self.0.lock().unwrap().push((
                operation.operation_type(),
                operation.ip_address(),
                operation.user_agent().to_string(),
            ));
            Ok(())
        }
    }

    /// A mesh that is on, whose pairings always start.
    struct Pairings {
        mesh_on: bool,
    }

    #[async_trait]
    impl SshEnrollmentPairings for Pairings {
        async fn mesh_port(&self) -> Result<u16, Problem> {
            if self.mesh_on {
                Ok(51820)
            } else {
                Err(problem(MeshError::Disabled))
            }
        }
        async fn start(
            &self,
            _context: AuditContext,
            _node_endpoint: SocketAddr,
            _name: Option<&str>,
        ) -> Result<StartedPairing, Problem> {
            Ok(StartedPairing {
                id: 3,
                name: "worker-a".into(),
                code: Zeroizing::new("tpair1.test".into()),
            })
        }
    }

    fn user(role_name: &str) -> temps_entities::users::Model {
        temps_entities::users::Model {
            id: 1,
            name: role_name.to_string(),
            email: "operator@example.com".to_string(),
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
            created_at: chrono::Utc::now(),
            updated_at: chrono::Utc::now(),
        }
    }

    fn admin() -> AuthContext {
        AuthContext::new_persisted_session(user("admin"), Role::Admin, 1)
    }

    fn metadata() -> RequestMetadata {
        RequestMetadata {
            ip_address: "203.0.113.9".to_string(),
            user_agent: "operator-browser/1.0".to_string(),
            headers: axum::http::HeaderMap::new(),
            visitor_id_cookie: None,
            session_id_cookie: None,
            base_url: "https://temps.example.com".to_string(),
            scheme: "https".to_string(),
            host: "temps.example.com".to_string(),
            is_secure: true,
        }
    }

    struct Harness {
        db: DatabaseConnection,
        decision: SensitiveActionDecision,
        mesh_on: bool,
        audits: Arc<Audits>,
    }

    impl Harness {
        fn new(db: MockDatabase) -> Self {
            Self {
                db: db.into_connection(),
                decision: SensitiveActionDecision::Allow,
                mesh_on: true,
                audits: Arc::new(Audits::default()),
            }
        }

        fn app(self, auth: Option<AuthContext>) -> (Router, Arc<Audits>) {
            let db = Arc::new(self.db);
            let audits = self.audits.clone();
            let state = NodeSshState {
                enrollments: NodeSshEnrollmentService::new(
                    db.clone(),
                    Arc::new(temps_config::EnrollmentTokenService::new(db)),
                    self.audits.clone(),
                ),
                pairings: Arc::new(Pairings {
                    mesh_on: self.mesh_on,
                }),
                audit_service: self.audits,
                sensitive_action_authorizer: Arc::new(Authorizer(self.decision)),
            };
            let router = Router::new()
                .route("/nodes/ssh/host-key", post(node_ssh_host_key))
                .route(
                    "/nodes/ssh/enrollments",
                    get(list_node_ssh_enrollments).post(create_node_ssh_enrollment),
                )
                .route(
                    "/nodes/ssh/enrollments/{enrollment_id}",
                    get(get_node_ssh_enrollment),
                )
                .layer(axum::middleware::from_fn(
                    move |mut request: axum::extract::Request, next: axum::middleware::Next| {
                        let auth = auth.clone();
                        async move {
                            if let Some(auth) = auth {
                                request.extensions_mut().insert(auth);
                            }
                            request.extensions_mut().insert(metadata());
                            next.run(request).await
                        }
                    },
                ))
                .with_state(state);
            (router, audits)
        }
    }

    fn empty_db() -> MockDatabase {
        MockDatabase::new(DatabaseBackend::Postgres)
    }

    fn post_json(uri: &str, body: serde_json::Value) -> Request<Body> {
        Request::post(uri)
            .header("content-type", "application/json")
            .body(Body::from(body.to_string()))
            .unwrap()
    }

    fn get_request(uri: &str) -> Request<Body> {
        Request::get(uri).body(Body::empty()).unwrap()
    }

    fn enrollment_body(host: &str) -> serde_json::Value {
        serde_json::json!({
            "host": host,
            "user": "root",
            "credentials": {"method": "password", "password": "hunter2-password"},
            "host_key_fingerprint": FINGERPRINT,
        })
    }

    async fn call(app: Router, request: Request<Body>) -> (StatusCode, serde_json::Value) {
        let response = app.oneshot(request).await.unwrap();
        let status = response.status();
        let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        let body = serde_json::from_slice(&bytes).unwrap_or(serde_json::Value::Null);
        (status, body)
    }

    fn sample_model(id: i32) -> node_ssh_enrollments::Model {
        let now = chrono::Utc::now();
        node_ssh_enrollments::Model {
            id,
            name: "worker-a".into(),
            host: "198.51.100.7".into(),
            ssh_address: "198.51.100.7:22".into(),
            ssh_user: "root".into(),
            auth_method: "password".into(),
            host_key_fingerprint: FINGERPRINT.into(),
            pairing_id: Some(3),
            status: "running".into(),
            step: "pairing".into(),
            log: "Connected.\n".into(),
            error: None,
            agent_mode: None,
            node_id: None,
            created_by_user_id: Some(1),
            created_at: now,
            updated_at: now,
            heartbeat_at: now,
            finished_at: None,
        }
    }

    fn count_row(column: &str, count: i64) -> BTreeMap<String, Value> {
        BTreeMap::from([(column.to_string(), Value::BigInt(Some(count)))])
    }

    fn exec(rows: u64) -> MockExecResult {
        MockExecResult {
            last_insert_id: 0,
            rows_affected: rows,
        }
    }

    /// Every route, and what each needs.
    fn routes() -> Vec<(&'static str, Request<Body>)> {
        vec![
            (
                "host-key",
                post_json(
                    "/nodes/ssh/host-key",
                    serde_json::json!({"host": "198.51.100.7"}),
                ),
            ),
            (
                "create",
                post_json("/nodes/ssh/enrollments", enrollment_body("198.51.100.7")),
            ),
            ("list", get_request("/nodes/ssh/enrollments")),
            ("get", get_request("/nodes/ssh/enrollments/1")),
        ]
    }

    #[tokio::test]
    async fn every_route_needs_a_login() {
        for (name, request) in routes() {
            let (app, _) = Harness::new(empty_db()).app(None);
            let (status, _) = call(app, request).await;
            assert_eq!(status, StatusCode::UNAUTHORIZED, "{name}");
        }
    }

    #[tokio::test]
    async fn every_route_needs_settings_permissions() {
        for (name, request) in routes() {
            let (app, _) = Harness::new(empty_db()).app(Some(AuthContext::new_persisted_session(
                user("user"),
                Role::User,
                1,
            )));
            let (status, _) = call(app, request).await;
            assert_eq!(status, StatusCode::FORBIDDEN, "{name}");
        }
    }

    #[tokio::test]
    async fn reading_a_host_key_and_enrolling_need_a_recent_verification() {
        for (name, request) in routes().into_iter().take(2) {
            let mut harness = Harness::new(empty_db());
            harness.decision = SensitiveActionDecision::RequireVerification {
                mfa_setup_required: false,
            };
            let (app, audits) = harness.app(Some(admin()));
            let (status, body) = call(app, request).await;
            assert_eq!(status, StatusCode::PRECONDITION_REQUIRED, "{name}");
            assert_eq!(body["error_code"], "STEP_UP_REQUIRED", "{name}");
            assert!(audits.0.lock().unwrap().is_empty());
        }
    }

    #[tokio::test]
    async fn reading_a_host_key_refuses_loopback_and_metadata_hosts() {
        for host in [
            "127.0.0.1",
            "169.254.169.254",
            "localhost",
            "-oProxyCommand=x",
        ] {
            let (app, _) = Harness::new(empty_db()).app(Some(admin()));
            let (status, body) = call(
                app,
                post_json("/nodes/ssh/host-key", serde_json::json!({"host": host})),
            )
            .await;
            assert_eq!(status, StatusCode::BAD_REQUEST, "{host}: {body}");
        }
    }

    #[tokio::test]
    async fn enrolling_refuses_loopback_and_metadata_hosts() {
        for host in ["127.0.0.1", "169.254.169.254", "[::1]"] {
            // The quick running count happens before the host is checked.
            let db = empty_db().append_query_results([vec![count_row("running", 0)]]);
            let (app, audits) = Harness::new(db).app(Some(admin()));
            let (status, body) = call(
                app,
                post_json("/nodes/ssh/enrollments", enrollment_body(host)),
            )
            .await;
            assert_eq!(status, StatusCode::BAD_REQUEST, "{host}: {body}");
            assert_eq!(body["title"], "Cannot Connect To This Host", "{host}");
            assert!(audits.0.lock().unwrap().is_empty());
        }
    }

    #[tokio::test]
    async fn enrolling_refuses_bad_users_and_fingerprints() {
        let mut bad_user = enrollment_body("198.51.100.7");
        bad_user["user"] = serde_json::json!("root; id");
        let mut bad_fingerprint = enrollment_body("198.51.100.7");
        bad_fingerprint["host_key_fingerprint"] = serde_json::json!("MD5:ab:cd");
        for (body, title) in [
            (bad_user, "Invalid User"),
            (bad_fingerprint, "Invalid Host Key Fingerprint"),
        ] {
            let (app, _) = Harness::new(empty_db()).app(Some(admin()));
            let (status, problem) = call(app, post_json("/nodes/ssh/enrollments", body)).await;
            assert_eq!(status, StatusCode::BAD_REQUEST);
            assert_eq!(problem["title"], title);
        }
    }

    #[tokio::test]
    async fn enrolling_needs_the_mesh() {
        let mut harness = Harness::new(empty_db());
        harness.mesh_on = false;
        let (app, _) = harness.app(Some(admin()));
        let (status, _) = call(
            app,
            post_json("/nodes/ssh/enrollments", enrollment_body("198.51.100.7")),
        )
        .await;
        assert_eq!(status, StatusCode::CONFLICT);
    }

    #[tokio::test]
    async fn enrolling_past_the_limit_is_refused_before_a_pairing_starts() {
        let db = empty_db().append_query_results([vec![count_row("running", 5)]]);
        let (app, audits) = Harness::new(db).app(Some(admin()));
        let (status, body) = call(
            app,
            post_json("/nodes/ssh/enrollments", enrollment_body("198.51.100.7")),
        )
        .await;
        assert_eq!(status, StatusCode::CONFLICT);
        assert_eq!(body["title"], "Too Many Enrollments Running");
        assert!(audits.0.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn enrolling_starts_and_audits_with_the_callers_address() {
        let db = empty_db()
            .append_query_results([vec![count_row("running", 1)]])
            .append_exec_results([exec(1), exec(0)])
            .append_query_results([vec![count_row("running", 1)]])
            .append_query_results([vec![sample_model(7)]]);
        let (app, audits) = Harness::new(db).app(Some(admin()));
        let (status, body) = call(
            app,
            post_json("/nodes/ssh/enrollments", enrollment_body("198.51.100.7")),
        )
        .await;
        assert_eq!(status, StatusCode::ACCEPTED, "{body}");
        assert_eq!(body["id"], 7);
        assert_eq!(body["status"], "running");
        assert!(!body.to_string().contains("hunter2"));
        let audits = audits.0.lock().unwrap();
        assert_eq!(
            audits.first(),
            Some(&(
                "NODE_SSH_ENROLLMENT_STARTED".to_string(),
                Some("203.0.113.9".to_string()),
                "operator-browser/1.0".to_string()
            ))
        );
    }

    #[tokio::test]
    async fn listing_pages_without_logs() {
        let db = empty_db()
            .append_exec_results([exec(0)])
            .append_query_results([vec![count_row("num_items", 42)]])
            .append_query_results([vec![sample_model(9), sample_model(8)]]);
        let (app, _) = Harness::new(db).app(Some(admin()));
        let (status, body) =
            call(app, get_request("/nodes/ssh/enrollments?page=2&per_page=2")).await;
        assert_eq!(status, StatusCode::OK, "{body}");
        assert_eq!(body["total"], 42);
        assert_eq!(body["page"], 2);
        assert_eq!(body["per_page"], 2);
        let items = body["enrollments"].as_array().unwrap();
        assert_eq!(items.len(), 2);
        assert_eq!(items[0]["id"], 9);
        assert!(items[0].get("log").is_none());
    }

    #[tokio::test]
    async fn listing_defaults_and_caps_the_page_size() {
        for (query, page, per_page) in [
            ("", 1, 20),
            ("?per_page=1000", 1, 100),
            ("?page=0&per_page=0", 1, 1),
        ] {
            let db = empty_db()
                .append_exec_results([exec(0)])
                .append_query_results([vec![count_row("num_items", 0)]])
                .append_query_results([Vec::<node_ssh_enrollments::Model>::new()]);
            let (app, _) = Harness::new(db).app(Some(admin()));
            let (status, body) =
                call(app, get_request(&format!("/nodes/ssh/enrollments{query}"))).await;
            assert_eq!(status, StatusCode::OK, "{query}");
            assert_eq!(
                (body["page"].as_u64(), body["per_page"].as_u64()),
                (Some(page), Some(per_page)),
                "{query}"
            );
        }
    }

    #[tokio::test]
    async fn one_enrollment_comes_with_its_log() {
        let db = empty_db()
            .append_exec_results([exec(0)])
            .append_query_results([vec![sample_model(7)]]);
        let (app, _) = Harness::new(db).app(Some(admin()));
        let (status, body) = call(app, get_request("/nodes/ssh/enrollments/7")).await;
        assert_eq!(status, StatusCode::OK, "{body}");
        assert_eq!(body["id"], 7);
        assert_eq!(body["log"], "Connected.\n");
    }

    #[tokio::test]
    async fn a_missing_enrollment_is_404() {
        let db = empty_db()
            .append_exec_results([exec(0)])
            .append_query_results([Vec::<node_ssh_enrollments::Model>::new()]);
        let (app, _) = Harness::new(db).app(Some(admin()));
        let (status, body) = call(app, get_request("/nodes/ssh/enrollments/99")).await;
        assert_eq!(status, StatusCode::NOT_FOUND);
        assert_eq!(body["title"], "Enrollment Not Found");
    }
}
