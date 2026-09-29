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

use axum::{
    extract::{Path, State},
    http::StatusCode,
    response::IntoResponse,
    Json,
};
use serde::{Deserialize, Serialize};
use temps_auth::{permission_guard, require_sensitive_action, RequireAuth};
use temps_core::problemdetails::{self, Problem};
use temps_core::{AuditContext, SensitiveAction};
use temps_entities::node_ssh_enrollments;
use temps_network::mesh::MeshError;
use tracing::error;
use utoipa::ToSchema;
use zeroize::Zeroizing;

use crate::handlers::audit::NodeSshEnrollmentStartedAudit;
use crate::handlers::node_pairings::{parse_node_endpoint, problem, start_pairing};
use crate::handlers::types::AppState;
use crate::services::node_ssh::{self, Enrollment, SshAuth, SshError};
use crate::services::node_ssh_enrollment::{self, NewEnrollment, MAX_RUNNING};

const DEFAULT_SSH_PORT: u16 = 22;
/// How long `temps join --pair` may wait on the server for this control plane
/// to reach it.
const PAIRING_TIMEOUT: Duration = Duration::from_secs(10 * 60);

/// A server to read the SSH host key of.
#[derive(Debug, Clone, Deserialize, ToSchema)]
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
    /// What it did, with the server's output.
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

#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct NodeSshEnrollmentListResponse {
    pub enrollments: Vec<NodeSshEnrollmentResponse>,
}

fn bad_request(title: &str, detail: impl Into<String>) -> Problem {
    problemdetails::new(StatusCode::BAD_REQUEST)
        .with_title(title)
        .with_detail(detail.into())
}

fn database(what: &str) -> impl FnOnce(sea_orm::DbErr) -> Problem + '_ {
    move |error| {
        error!("SSH enrollment: {what}: {error}");
        problemdetails::new(StatusCode::INTERNAL_SERVER_ERROR)
            .with_title("SSH Enrollment Error")
            .with_detail(format!("could not {what}; see the server logs"))
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
        SshError::Auth(_) => StatusCode::BAD_REQUEST,
        SshError::HostKeyChanged { .. } => StatusCode::CONFLICT,
        _ => StatusCode::BAD_GATEWAY,
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
    State(app_state): State<Arc<AppState>>,
    Json(request): Json<SshHostKeyRequest>,
) -> Result<impl IntoResponse, Problem> {
    permission_guard!(auth, SettingsWrite);
    // It opens a connection from this server to an address the caller
    // chooses: the same step-up as the enrollment it precedes.
    require_sensitive_action(
        app_state.sensitive_action_authorizer.as_ref(),
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
    State(app_state): State<Arc<AppState>>,
    Json(request): Json<CreateSshEnrollmentRequest>,
) -> Result<impl IntoResponse, Problem> {
    permission_guard!(auth, SettingsWrite);
    require_sensitive_action(
        app_state.sensitive_action_authorizer.as_ref(),
        &auth,
        SensitiveAction::AddNodeOverSsh,
    )
    .await?;
    let db = app_state.db.as_ref();

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
    let settings = temps_network::mesh::load_settings(db)
        .await
        .map_err(problem)?
        .ok_or_else(|| problem(MeshError::Disabled))?;
    let running = node_ssh_enrollment::running_count(db)
        .await
        .map_err(database("count running enrollments"))?;
    if running >= MAX_RUNNING {
        return Err(problemdetails::new(StatusCode::CONFLICT)
            .with_title("Too Many Enrollments Running")
            .with_detail(format!(
                "{MAX_RUNNING} servers are already being added over SSH; wait for one to finish."
            )));
    }
    let ssh_address = resolve(&request.host, request.port).await?;
    let node_endpoint = match request
        .node_address
        .as_deref()
        .map(str::trim)
        .filter(|a| !a.is_empty())
    {
        Some(address) => parse_node_endpoint(address, settings.port)?,
        None => parse_node_endpoint(&ssh_address.ip().to_string(), settings.port)?,
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

    let (pairing, code) = start_pairing(
        &app_state,
        auth.user_id(),
        node_endpoint,
        request.name.as_deref(),
    )
    .await?;
    let enrollment = match node_ssh_enrollment::create(
        db,
        NewEnrollment {
            name: pairing.name.clone(),
            host: request.host.trim().to_string(),
            ssh_address: ssh_address.to_string(),
            ssh_user: user.clone(),
            auth_method: auth_method.method().to_string(),
            host_key_fingerprint: fingerprint.clone(),
            pairing_id: pairing.id,
            created_by_user_id: Some(auth.user_id()),
        },
    )
    .await
    {
        Ok(enrollment) => enrollment,
        Err(error) => {
            node_ssh_enrollment::abandon_pairing(&app_state.db, pairing.id).await;
            return Err(database("record the enrollment")(error));
        }
    };

    let audit = NodeSshEnrollmentStartedAudit {
        context: AuditContext {
            user_id: auth.user_id(),
            ip_address: None,
            user_agent: "temps-api".to_string(),
        },
        enrollment_id: enrollment.id,
        pairing_id: pairing.id,
        name: enrollment.name.clone(),
        ssh_address: enrollment.ssh_address.clone(),
        ssh_user: enrollment.ssh_user.clone(),
        auth_method: enrollment.auth_method.clone(),
        host_key_fingerprint: enrollment.host_key_fingerprint.clone(),
    };
    if let Err(error) = app_state.audit_service.create_audit_log(&audit).await {
        error!(%error, "SSH enrollment started but audit record failed");
    }

    node_ssh_enrollment::spawn(
        app_state.db.clone(),
        enrollment.id,
        pairing.id,
        Enrollment {
            address: ssh_address,
            user,
            auth: auth_method,
            host_key_fingerprint: fingerprint,
            pairing_code: code,
            pairing_timeout: PAIRING_TIMEOUT,
        },
    );
    Ok((
        StatusCode::ACCEPTED,
        Json(NodeSshEnrollmentResponse::from(enrollment)),
    ))
}

/// Recent servers added over SSH, newest first.
#[utoipa::path(
    tag = "Nodes",
    get,
    path = "/nodes/ssh/enrollments",
    operation_id = "NodeSshEnrollmentList",
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
    State(app_state): State<Arc<AppState>>,
) -> Result<impl IntoResponse, Problem> {
    permission_guard!(auth, SettingsRead);
    let enrollments = node_ssh_enrollment::list(app_state.db.as_ref())
        .await
        .map_err(database("list enrollments"))?;
    Ok(Json(NodeSshEnrollmentListResponse {
        enrollments: enrollments.into_iter().map(Into::into).collect(),
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
    State(app_state): State<Arc<AppState>>,
    Path(enrollment_id): Path<i32>,
) -> Result<impl IntoResponse, Problem> {
    permission_guard!(auth, SettingsRead);
    let enrollment = node_ssh_enrollment::get(app_state.db.as_ref(), enrollment_id)
        .await
        .map_err(database("load the enrollment"))?
        .ok_or_else(|| {
            problemdetails::new(StatusCode::NOT_FOUND)
                .with_title("Enrollment Not Found")
                .with_detail(format!("No SSH enrollment with id {enrollment_id}"))
        })?;
    Ok(Json(NodeSshEnrollmentResponse::from(enrollment)))
}

#[cfg(test)]
mod tests {
    use super::*;

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
}
