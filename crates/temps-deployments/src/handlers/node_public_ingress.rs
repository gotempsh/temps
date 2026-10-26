// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Direct worker ingress from the console (ADR 058 D7).
//!
//! - `GET /internal/nodes/{id}/public-ingress/settings`: what is configured,
//!   what the agent can bind and what it bound.
//! - `PUT` the same path: turn ingress on or off and choose where it listens
//!   and what DNS points at. The agent applies it from its next snapshot.
//! - `POST /internal/nodes/{id}/public-ingress/check`: dial the public
//!   address from the control plane and say what is in the way.

use std::net::{IpAddr, SocketAddr};
use std::sync::Arc;
use std::time::Duration;

use axum::extract::{Extension, Path, State};
use axum::{http::StatusCode, response::IntoResponse, Json};
use serde::{Deserialize, Serialize};
use temps_auth::{permission_guard, RequireAuth};
use temps_core::problemdetails::{self, Problem};
use temps_core::worker_ingress::{
    address_scope, probe_answer, AddressScope, IngressMode, ReportedInterface, PROBE_NONCE_LEN,
    PROBE_PATH_PREFIX,
};
use temps_core::{AuditContext, RequestMetadata};
use temps_entities::{node_public_ingress, nodes};
use tracing::error;
use utoipa::ToSchema;

use crate::handlers::audit::{
    NodePublicIngressCheckedAudit, NodePublicIngressSettingsChangedAudit,
};
use crate::handlers::types::AppState;
use crate::services::node_public_ingress::{NodePublicIngressError, PublicIngressChange};

#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct PublicIngressInterface {
    pub name: String,
    pub address: String,
    pub default_route: bool,
    pub scope: AddressScope,
    /// The worker's Tailscale address (`tailscale0`, in 100.64.0.0/10).
    #[serde(default)]
    pub tailscale: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct PublicIngressCheckStep {
    /// `address`, `http` or `https`.
    pub step: String,
    pub ok: bool,
    pub message: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct PublicIngressCheck {
    pub ok: bool,
    /// The address that was dialled.
    pub target: Option<String>,
    pub summary: String,
    pub steps: Vec<PublicIngressCheckStep>,
}

#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct PublicIngressSettings {
    pub node_id: i32,
    pub enabled: bool,
    /// Configured listen address; `None` means automatic.
    pub listen_address: Option<String>,
    pub http_port: u16,
    pub https_port: u16,
    /// Configured public address; `None` means the listen address.
    pub public_address: Option<String>,
    /// The address DNS should point at, as far as Temps can tell.
    pub effective_public_address: Option<String>,
    pub effective_public_scope: Option<AddressScope>,
    /// Whether the agent reported its addresses: an agent that predates
    /// console-managed ingress never does, and keeps its flag.
    pub agent_reports: bool,
    pub interfaces: Vec<PublicIngressInterface>,
    /// What the worker allows, as it last reported: `off` (no public ports),
    /// `fixed` (the worker chose its listen address and ports) or `delegated`
    /// (Temps picks among the worker's addresses). `None` from agents that
    /// predate ingress modes.
    pub mode: Option<IngressMode>,
    pub listening_http: Option<String>,
    pub listening_https: Option<String>,
    pub listener_error: Option<String>,
    /// In `private` mode (ADR 064 D5): the private address the listener is
    /// bound on, when it is a private one.
    pub private_listener: Option<String>,
    /// That address is the worker's Tailscale one.
    pub private_on_tailscale: bool,
    pub running: Option<bool>,
    pub last_error: Option<String>,
    /// The listen address is not public but the public address is: traffic
    /// crosses a NAT, so the router must forward the ports.
    pub behind_nat: bool,
    pub last_check: Option<PublicIngressCheck>,
    pub checked_at: Option<chrono::DateTime<chrono::Utc>>,
}

#[derive(Debug, Clone, Deserialize, ToSchema)]
pub struct UpdatePublicIngressSettingsRequest {
    pub enabled: bool,
    /// Local address to bind, or `None` for automatic.
    pub listen_address: Option<String>,
    pub http_port: Option<u16>,
    pub https_port: Option<u16>,
    /// What DNS points at, or `None` for the listen address.
    pub public_address: Option<String>,
}

fn bad_request(detail: impl Into<String>) -> Problem {
    problemdetails::new(StatusCode::BAD_REQUEST)
        .with_title("Invalid Public Ingress Settings")
        .with_detail(detail.into())
}

fn conflict(detail: impl Into<String>) -> Problem {
    problemdetails::new(StatusCode::CONFLICT)
        .with_title("Worker Does Not Allow This Change")
        .with_detail(detail.into())
}

fn internal(detail: impl Into<String>) -> Problem {
    problemdetails::new(StatusCode::INTERNAL_SERVER_ERROR)
        .with_title("Public Ingress Settings Unavailable")
        .with_detail(detail.into())
}

fn port(value: i32, default: u16) -> u16 {
    u16::try_from(value)
        .ok()
        .filter(|p| *p != 0)
        .unwrap_or(default)
}

fn parse_ip(value: &str) -> Option<IpAddr> {
    value
        .parse::<IpAddr>()
        .ok()
        .or_else(|| value.parse::<SocketAddr>().ok().map(|address| address.ip()))
}

/// The address DNS should point at: the configured public address, else the
/// address the agent actually bound, else the configured listen address, else
/// the default-route interface the agent reported.
fn effective_public_address(
    row: Option<&node_public_ingress::Model>,
    interfaces: &[ReportedInterface],
) -> Option<IpAddr> {
    let row = row?;
    let listening_http = row
        .listening
        .as_ref()
        .and_then(|l| l.get("http"))
        .and_then(|v| v.as_str());
    temps_core::worker_ingress::effective_public_address(
        row.public_address.as_deref(),
        listening_http,
        row.listen_address.as_deref(),
        interfaces,
    )
}

fn view(node: &nodes::Model, row: Option<&node_public_ingress::Model>) -> PublicIngressSettings {
    let interfaces: Vec<ReportedInterface> = row
        .and_then(|r| r.interfaces.clone())
        .and_then(|v| serde_json::from_value(v).ok())
        .unwrap_or_default();
    let listening = row.and_then(|r| r.listening.clone());
    let listening_str = |key: &str| {
        listening
            .as_ref()
            .and_then(|l| l.get(key))
            .and_then(|v| v.as_str())
            .map(str::to_string)
    };
    let effective = effective_public_address(row, &interfaces);
    let bound_ip = listening_str("http")
        .as_deref()
        .and_then(parse_ip)
        .or_else(|| {
            row.and_then(|r| r.listen_address.as_deref())
                .and_then(parse_ip)
        })
        .or_else(|| {
            interfaces
                .iter()
                .find(|i| i.default_route)
                .and_then(|i| parse_ip(&i.address))
        });
    let behind_nat = matches!(
        (bound_ip.map(address_scope), effective.map(address_scope)),
        (
            Some(AddressScope::Private | AddressScope::Cgnat),
            Some(AddressScope::Public)
        )
    );
    let private_listener = (row.and_then(|r| r.mode.as_deref()) == Some("private"))
        .then(|| listening_str("https").as_deref().and_then(parse_ip))
        .flatten()
        .filter(|ip| temps_core::worker_ingress::check_private_listen(*ip).is_ok());
    let private_on_tailscale = private_listener.is_some_and(|ip| {
        interfaces
            .iter()
            .any(|i| i.tailscale && parse_ip(&i.address) == Some(ip))
    });
    PublicIngressSettings {
        node_id: node.id,
        enabled: node.public_ingress_enabled,
        listen_address: row.and_then(|r| r.listen_address.clone()),
        http_port: row.map_or(80, |r| port(r.http_port, 80)),
        https_port: row.map_or(443, |r| port(r.https_port, 443)),
        public_address: row.and_then(|r| r.public_address.clone()),
        effective_public_address: effective.map(|ip| ip.to_string()),
        effective_public_scope: effective.map(address_scope),
        agent_reports: row.is_some_and(|r| r.interfaces.is_some()),
        interfaces: interfaces
            .into_iter()
            .map(|i| PublicIngressInterface {
                scope: parse_ip(&i.address).map_or(AddressScope::Local, address_scope),
                name: i.name,
                address: i.address,
                default_route: i.default_route,
                tailscale: i.tailscale,
            })
            .collect(),
        mode: row
            .and_then(|r| r.mode.as_deref())
            .and_then(|m| m.parse().ok()),
        listening_http: listening_str("http"),
        listening_https: listening_str("https"),
        listener_error: listening_str("error"),
        private_listener: private_listener.map(|ip| ip.to_string()),
        private_on_tailscale,
        running: node.public_ingress_running,
        last_error: node.public_ingress_last_error.clone(),
        behind_nat,
        last_check: row
            .and_then(|r| r.last_check.clone())
            .and_then(|v| serde_json::from_value(v).ok()),
        checked_at: row.and_then(|r| r.checked_at),
    }
}

impl From<NodePublicIngressError> for Problem {
    fn from(error: NodePublicIngressError) -> Self {
        match error {
            NodePublicIngressError::Node(error) => Problem::from(error),
            error @ (NodePublicIngressError::Load { .. } | NodePublicIngressError::Save { .. }) => {
                internal(error.to_string())
            }
        }
    }
}

async fn load(
    state: &AppState,
    node_id: i32,
) -> Result<(nodes::Model, Option<node_public_ingress::Model>), Problem> {
    Ok(state.node_public_ingress_service.load(node_id).await?)
}

#[utoipa::path(
    tag = "Nodes", get, path = "/internal/nodes/{node_id}/public-ingress/settings",
    operation_id = "admin_get_node_public_ingress_settings",
    params(("node_id" = i32, Path, description = "Worker node ID")),
    responses((status = 200, body = PublicIngressSettings), (status = 404, description = "Node not found")),
    security(("bearer_auth" = []))
)]
pub async fn get_public_ingress_settings(
    RequireAuth(auth): RequireAuth,
    State(state): State<Arc<AppState>>,
    Path(node_id): Path<i32>,
) -> Result<impl IntoResponse, Problem> {
    permission_guard!(auth, SettingsRead);
    let (node, row) = load(&state, node_id).await?;
    Ok(Json(view(&node, row.as_ref())))
}

#[utoipa::path(
    tag = "Nodes", put, path = "/internal/nodes/{node_id}/public-ingress/settings",
    operation_id = "admin_update_node_public_ingress_settings",
    params(("node_id" = i32, Path, description = "Worker node ID")),
    request_body = UpdatePublicIngressSettingsRequest,
    responses((status = 200, body = PublicIngressSettings),
        (status = 400, description = "Invalid address or ports, or not a worker"),
        (status = 404, description = "Node not found")),
    security(("bearer_auth" = []))
)]
pub async fn update_public_ingress_settings(
    RequireAuth(auth): RequireAuth,
    State(state): State<Arc<AppState>>,
    Extension(metadata): Extension<RequestMetadata>,
    Path(node_id): Path<i32>,
    Json(request): Json<UpdatePublicIngressSettingsRequest>,
) -> Result<impl IntoResponse, Problem> {
    permission_guard!(auth, SettingsWrite);
    let (node, existing) = load(&state, node_id).await?;
    if node.role != "worker" {
        return Err(bad_request(format!(
            "Node '{}' is not a worker. Public ingress settings apply to worker nodes; this server takes public traffic through its own proxy.",
            node.name
        )));
    }
    let normalize = |value: Option<String>| {
        value
            .map(|v| v.trim().to_string())
            .filter(|v| !v.is_empty())
    };
    let listen_address = normalize(request.listen_address);
    let public_address = normalize(request.public_address);
    if let Some(address) = &listen_address {
        let ip: IpAddr = address.parse().map_err(|_| {
            bad_request(format!("Listen address '{address}' is not an IP address. Pick one of the addresses the worker reported, or leave it on automatic."))
        })?;
        if ip.is_unspecified() || ip.is_loopback() {
            return Err(bad_request(format!(
                "Listen address '{address}' would not take traffic from outside this machine (or would collide with the overlay listener). Pick the address of a network interface."
            )));
        }
    }
    if let Some(address) = &public_address {
        let ip: IpAddr = address.parse().map_err(|_| {
            bad_request(format!("Public address '{address}' is not an IP address. Use the IP your DNS A or AAAA record points at."))
        })?;
        match address_scope(ip) {
            AddressScope::Public => {}
            AddressScope::Cgnat => return Err(bad_request(format!(
                "Public address '{address}' is in the carrier-grade NAT range (100.64.0.0/10): your provider's NAT, which no port forward can get through. Use a tunnel for this worker instead."
            ))),
            AddressScope::Private | AddressScope::Local => return Err(bad_request(format!(
                "Public address '{address}' is a private or local address, so visitors on the internet cannot reach it. Use your router's public IP, and forward ports 80 and 443 to this worker."
            ))),
        }
    }
    let http_port = request.http_port.unwrap_or(80);
    let https_port = request.https_port.unwrap_or(443);
    if http_port == 0 || https_port == 0 || http_port == https_port {
        return Err(bad_request(
            "HTTP and HTTPS ports must be two different, non-zero ports.",
        ));
    }

    // The worker decides what the console may change (ADR 058 D7): `off`
    // keeps it off the internet, `fixed` keeps its own listen address and
    // ports. Agents that never reported a mode predate the rule.
    let mode: Option<IngressMode> = existing
        .as_ref()
        .and_then(|r| r.mode.as_deref())
        .and_then(|m| m.parse().ok());
    match mode {
        Some(IngressMode::Off) if request.enabled => {
            return Err(conflict(
                "This worker does not allow public traffic. On the worker, run `temps agent ingress delegated` to let Temps choose how it listens, or `temps agent ingress fixed` to choose yourself, then enable it here.",
            ));
        }
        // ADR 064 D5: a private-mode worker listens only on its private
        // network, on the address and ports its owner chose.
        Some(IngressMode::Private) if request.enabled => {
            return Err(conflict(
                "This worker listens only on its private network (`private` mode), so it takes no public traffic. To take public traffic too, run `temps agent ingress delegated` or `temps agent ingress fixed` on the worker. Private addresses are set up with Make it reachable → Only my private network.",
            ));
        }
        Some(IngressMode::Off | IngressMode::Fixed) => {
            let stored_listen = existing.as_ref().and_then(|r| r.listen_address.clone());
            let stored_http = existing.as_ref().map_or(80, |r| port(r.http_port, 80));
            let stored_https = existing.as_ref().map_or(443, |r| port(r.https_port, 443));
            if listen_address != stored_listen
                || http_port != stored_http
                || https_port != stored_https
            {
                return Err(conflict(
                    "This worker chooses its own listen address and ports. Change them on the worker with `temps agent ingress fixed --listen <address|auto> --http-port <port> --https-port <port>`, or run `temps agent ingress delegated` to manage them here.",
                ));
            }
        }
        _ => {}
    }

    let change = PublicIngressChange {
        enabled: request.enabled,
        listen_address,
        http_port,
        https_port,
        public_address,
    };
    state
        .node_public_ingress_service
        .save(node_id, &change)
        .await?;

    let audit = NodePublicIngressSettingsChangedAudit {
        context: AuditContext {
            user_id: auth.user_id(),
            ip_address: Some(metadata.ip_address.clone()),
            user_agent: metadata.user_agent.clone(),
        },
        node_id,
        enabled: change.enabled,
        listen_address: change.listen_address,
        http_port: change.http_port,
        https_port: change.https_port,
        public_address: change.public_address,
    };
    if let Err(error) = state.audit_service.create_audit_log(&audit).await {
        error!(node_id, %error, "public ingress settings changed but audit record failed");
    }
    let (node, row) = load(&state, node_id).await?;
    Ok(Json(view(&node, row.as_ref())))
}

#[utoipa::path(
    tag = "Nodes", post, path = "/internal/nodes/{node_id}/public-ingress/check",
    operation_id = "admin_check_node_public_ingress",
    params(("node_id" = i32, Path, description = "Worker node ID")),
    responses((status = 200, body = PublicIngressCheck), (status = 404, description = "Node not found")),
    security(("bearer_auth" = []))
)]
pub async fn check_public_ingress(
    RequireAuth(auth): RequireAuth,
    State(state): State<Arc<AppState>>,
    Extension(metadata): Extension<RequestMetadata>,
    Path(node_id): Path<i32>,
) -> Result<impl IntoResponse, Problem> {
    permission_guard!(auth, SettingsWrite);
    let (node, row) = load(&state, node_id).await?;
    let settings = view(&node, row.as_ref());
    let check = run_check(&node, &settings).await;
    let audit = NodePublicIngressCheckedAudit {
        context: AuditContext {
            user_id: auth.user_id(),
            ip_address: Some(metadata.ip_address.clone()),
            user_agent: metadata.user_agent.clone(),
        },
        node_id,
        target: check.target.clone(),
        ok: check.ok,
    };
    if let Err(error) = state.audit_service.create_audit_log(&audit).await {
        error!(node_id, %error, "public ingress checked but audit record failed");
    }

    if let Err(e) = state
        .node_public_ingress_service
        .record_check(node_id, serde_json::json!(check))
        .await
    {
        error!(node_id, error = %e, "could not store the ingress reachability check");
    }
    Ok(Json(check))
}

fn step(step: &str, ok: bool, message: impl Into<String>) -> PublicIngressCheckStep {
    PublicIngressCheckStep {
        step: step.to_string(),
        ok,
        message: message.into(),
    }
}

fn fail(
    target: Option<String>,
    summary: String,
    steps: Vec<PublicIngressCheckStep>,
) -> PublicIngressCheck {
    PublicIngressCheck {
        ok: false,
        target,
        summary,
        steps,
    }
}

async fn run_check(node: &nodes::Model, settings: &PublicIngressSettings) -> PublicIngressCheck {
    if !settings.enabled {
        return fail(
            None,
            "Public ingress is off for this worker. Turn it on, then check again.".into(),
            vec![],
        );
    }
    let Some(ip) = settings
        .effective_public_address
        .as_deref()
        .and_then(parse_ip)
    else {
        return fail(
            None,
            "Temps doesn't know this worker's public address yet. Enter the IP your DNS points at."
                .into(),
            vec![],
        );
    };
    let target = Some(ip.to_string());
    let forward_hint =
        |port: u16, local: u16| match settings.listening_http.as_deref().and_then(parse_ip) {
            Some(local_ip) if settings.behind_nat => {
                format!(" On your router, forward TCP {port} to {local_ip}:{local}.")
            }
            _ => String::new(),
        };
    match address_scope(ip) {
        AddressScope::Public => {}
        AddressScope::Cgnat => {
            return fail(target, format!("{ip} is behind carrier-grade NAT: the internet cannot open connections to it and no port forward helps. Use a tunnel for this worker."), vec![step("address", false, "carrier-grade NAT (100.64.0.0/10)")]);
        }
        AddressScope::Private | AddressScope::Local => {
            return fail(target, format!("{ip} is a private address, reachable only inside its own network. Enter the public IP your router has, and forward ports 80 and 443 to this worker."), vec![step("address", false, "private address")]);
        }
    }
    let mut steps = vec![step("address", true, format!("{ip} is a public address"))];

    if settings.listening_http.is_none() {
        let why = settings
            .listener_error
            .clone()
            .or_else(|| settings.last_error.clone())
            .unwrap_or_else(|| "the worker hasn't reported a bound listener yet".into());
        steps.push(step("http", false, format!("not listening: {why}")));
        return fail(
            target,
            format!("The worker isn't listening yet: {why}"),
            steps,
        );
    }

    // HTTP: the nonce must come back as this node's keyed answer, or the
    // forward points at some other machine.
    let nonce: String = {
        use rand::RngExt;
        let bytes: [u8; PROBE_NONCE_LEN / 2] = rand::rng().random();
        hex::encode(bytes)
    };
    let http_target = SocketAddr::new(ip, 80);
    let client = reqwest::Client::builder()
        .no_proxy()
        .redirect(reqwest::redirect::Policy::none())
        .connect_timeout(Duration::from_secs(5))
        .timeout(Duration::from_secs(10))
        .build();
    let mut http_timed_out = false;
    let http_ok = match client {
        Err(e) => {
            steps.push(step(
                "http",
                false,
                format!("could not build an HTTP client: {e}"),
            ));
            false
        }
        Ok(client) => {
            let url = format!("http://{http_target}{PROBE_PATH_PREFIX}{nonce}");
            match client.get(&url).send().await {
                Ok(response) if response.status().is_success() => {
                    let body = response.text().await.unwrap_or_default();
                    if body.trim() == probe_answer(&node.token_hash, &nonce) {
                        steps.push(step(
                            "http",
                            true,
                            format!("{http_target} reaches this worker"),
                        ));
                        true
                    } else {
                        steps.push(step("http", false, format!("{http_target} answered, but it isn't this worker: the address or port forward points at another machine.{}", forward_hint(80, settings.http_port))));
                        false
                    }
                }
                Ok(response) => {
                    steps.push(step("http", false, format!("{http_target} answered HTTP {} instead of this worker: something else is in front of port 80 (another web server, or a forward to the wrong machine).{}", response.status().as_u16(), forward_hint(80, settings.http_port))));
                    false
                }
                Err(e) => {
                    let detail = error_chain(&e);
                    http_timed_out =
                        e.is_timeout() || detail.to_ascii_lowercase().contains("timed out");
                    steps.push(step(
                        "http",
                        false,
                        describe_dial_error(
                            http_target,
                            &detail,
                            e.is_timeout(),
                            forward_hint(80, settings.http_port),
                        ),
                    ));
                    false
                }
            }
        }
    };

    let https_target = SocketAddr::new(ip, 443);
    let https_ok = match tokio::time::timeout(
        Duration::from_secs(5),
        tokio::net::TcpStream::connect(https_target),
    )
    .await
    {
        Ok(Ok(_)) => {
            steps.push(step(
                "https",
                true,
                format!("{https_target} accepts connections"),
            ));
            true
        }
        Ok(Err(e)) => {
            steps.push(step(
                "https",
                false,
                describe_dial_error(
                    https_target,
                    &e.to_string(),
                    false,
                    forward_hint(443, settings.https_port),
                ),
            ));
            false
        }
        Err(_) => {
            steps.push(step(
                "https",
                false,
                describe_dial_error(
                    https_target,
                    "timed out",
                    true,
                    forward_hint(443, settings.https_port),
                ),
            ));
            false
        }
    };

    let ok = http_ok && https_ok;
    let summary = if ok {
        let record = if ip.is_ipv6() { "AAAA" } else { "A" };
        format!(
            "Visitors can reach this worker at {ip}. Point your domain's {record} record at it."
        )
    } else if http_timed_out && https_ok {
        // 443 answers on the same address, so the path is up: either port 80
        // is filtered on the way in, or this control plane may not dial out
        // on port 80 (a hosted control plane may only reach port 443).
        format!("{ip} accepts connections on 443 but port 80 timed out. Check that inbound TCP 80 is allowed and forwarded; if it is, this control plane may not be allowed to dial port 80, so test from outside with `curl -i http://{ip}/` (any answer from this worker means it is reachable).")
    } else {
        "The internet can't fully reach this worker yet; see the failing step. Checked from the control plane: if it shares this worker's network, a router without hairpin NAT can fail this even when outside visitors get through.".to_string()
    };
    PublicIngressCheck {
        ok,
        target,
        summary,
        steps,
    }
}

/// The whole cause chain: reqwest's own message rarely says why.
fn error_chain(error: &dyn std::error::Error) -> String {
    let mut parts = vec![error.to_string()];
    let mut source = error.source();
    while let Some(cause) = source {
        parts.push(cause.to_string());
        source = cause.source();
    }
    parts.join(": ")
}

fn describe_dial_error(target: SocketAddr, error: &str, timed_out: bool, hint: String) -> String {
    let lower = error.to_ascii_lowercase();
    if timed_out || lower.contains("timed out") || lower.contains("timeout") {
        format!("{target} did not answer: a firewall, security group or the router drops the connection. Allow inbound TCP {}.{hint}", target.port())
    } else if lower.contains("refused") {
        format!("{target} refused the connection: a machine answered but nothing listens on port {}. The port forward is missing or points at the wrong port.{hint}", target.port())
    } else if lower.contains("unreachable") {
        format!("{target} is unreachable from the control plane: no route to it.{hint}")
    } else {
        format!("{target}: {error}.{hint}")
    }
}
