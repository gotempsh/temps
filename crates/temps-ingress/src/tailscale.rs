// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! The control plane's side of Tailscale (ADR 065 D2, D4): a Tailscale OAuth
//! client, registered as the `tailscale` provider, exchanged for short-lived
//! access tokens that are kept in memory only, and the Services calls a run
//! makes with them.
//!
//! The scopes asked for are `services` and `devices:core:read`, never
//! `devices:core`: approving a host stays with the tailnet admin. Each scope
//! is proven before anything is created ([`TailscaleClient::check_scopes`]),
//! the ADR 058 D8 way: a read for the read scope, and for `services` a `PUT`
//! of a name Tailscale cannot accept, where "forbidden" proves the scope
//! missing and a validation error proves it present. Reading a Service's
//! hosts is probed too: the Service way in is offered only when it works.
//!
//! The request paths and response shapes follow Tailscale's published API
//! spec (ADR 065 Context): a Service's hosts come from
//! `GET …/services/{name}/devices` with an `approvalLevel` of `not-approved`,
//! `approved:auto` or `approved:manual`. The spec lists `devices:core` for
//! that read, which Temps never asks for, so whether `devices:core:read`
//! suffices is what the hosts probe decides; the tests pin how each recorded
//! answer is read. Anything else
//! is [`TailscaleApiError::NotUnderstood`]: the run stops rather than guess.

use std::collections::{BTreeMap, HashMap};
use std::time::{Duration, Instant};

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};
use temps_core::tailnet::{messages, valid_service_label};
use thiserror::Error;
use utoipa::ToSchema;

use crate::error::IngressError;
use crate::registry::{
    DnsHandling, FieldKind, ProviderDeps, ProviderInfo, ProviderSetup, SetupField,
};

/// The provider kind, stored on the credential's row.
pub const KIND: &str = "tailscale";
const API: &str = "https://api.tailscale.com/api/v2";

pub const FIELD_CLIENT_ID: &str = "tailscale_client_id";
pub const FIELD_CLIENT_SECRET: &str = "tailscale_client_secret";
pub const FIELD_TAILNET: &str = "tailscale_tailnet";

/// The OAuth client's own tailnet.
pub const DEFAULT_TAILNET: &str = "-";
/// The tag a Service's hosts carry unless the plan picks another.
pub const DEFAULT_TAG: &str = "tag:temps";
/// The one port every Service Temps makes listens on (D4's fixed
/// definition).
pub const SERVICE_PORT: &str = "tcp:443";
/// What the marker in a Service's `comment` starts with.
const MARKER_PREFIX: &str = "temps:";

/// A Service name no tailnet accepts (a label cannot start with `-`), used
/// to prove the `services` scope without creating anything.
const PROBE_SERVICE: &str = "svc:-temps-scope-check-";

const REQUEST_TIMEOUT: Duration = Duration::from_secs(20);
/// Renew an access token this long before Tailscale says it expires.
const TOKEN_MARGIN: Duration = Duration::from_secs(60);
/// Largest answer read: a Service, a token or an error is a few hundred
/// bytes; a device list of a large tailnet stays well under this.
const MAX_BODY: usize = 8 * 1024 * 1024;

/// A Tailscale OAuth client, decrypted for the length of a call.
#[derive(Clone, PartialEq, Eq)]
pub struct TailscaleCredential {
    pub client_id: String,
    pub client_secret: String,
    /// The tailnet's API name, or `-` for the client's own.
    pub tailnet: String,
}

impl std::fmt::Debug for TailscaleCredential {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TailscaleCredential")
            .field("client_id", &self.client_id)
            .field("client_secret", &"***")
            .field("tailnet", &self.tailnet)
            .finish()
    }
}

impl TailscaleCredential {
    /// The credential from a setup form's answers.
    pub fn from_answers(
        settings: &BTreeMap<String, String>,
        secrets: &BTreeMap<String, String>,
    ) -> Result<Self, IngressError> {
        let client_id = settings
            .get(FIELD_CLIENT_ID)
            .map(|v| v.trim().to_string())
            .filter(|v| !v.is_empty())
            .ok_or_else(|| {
                IngressError::Validation("Paste the Tailscale OAuth client's ID.".into())
            })?;
        let client_secret = secrets
            .get(FIELD_CLIENT_SECRET)
            .map(|v| v.trim().to_string())
            .filter(|v| !v.is_empty())
            .ok_or_else(|| {
                IngressError::Validation("Paste the Tailscale OAuth client's secret.".into())
            })?;
        let tailnet = settings
            .get(FIELD_TAILNET)
            .map(|v| v.trim().to_string())
            .filter(|v| !v.is_empty())
            .unwrap_or_else(|| DEFAULT_TAILNET.to_string());
        // Only dots would be `.` or `..` in the API path, which URL
        // normalisation resolves out of `/tailnet/`.
        if tailnet.len() > 253
            || tailnet.bytes().all(|b| b == b'.')
            || !tailnet
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'.' | b'_' | b'@'))
        {
            return Err(IngressError::Validation(format!(
                "`{tailnet}` is not a tailnet name. Use the name from the admin console's \
                 General settings, or `-` for the OAuth client's own tailnet."
            )));
        }
        Ok(Self {
            client_id,
            client_secret,
            tailnet,
        })
    }

    /// Where the access token of this client is cached: never the secret
    /// itself, so the cache holds nothing a leak of it would reveal.
    fn cache_key(&self) -> String {
        let digest = Sha256::digest(format!("{}\n{}", self.client_id, self.client_secret));
        hex::encode(&digest[..16])
    }
}

#[derive(Debug, Clone, Error, PartialEq, Eq)]
pub enum TailscaleApiError {
    #[error(
        "Tailscale refused the OAuth client {client_id}: {reason}. Check the client ID and \
         secret, or create a new client."
    )]
    Credential { client_id: String, reason: String },
    #[error("Tailscale refused to {operation}: {}", messages::CREDENTIAL_SCOPE)]
    Forbidden { operation: String },
    #[error("Tailscale answered {operation} with HTTP {status}: {reason}")]
    Status {
        operation: String,
        status: u16,
        reason: String,
        /// A server error after a write: it may have been applied.
        maybe_applied: bool,
    },
    #[error("Could not reach Tailscale to {operation}: {reason}")]
    Transport {
        operation: String,
        reason: String,
        maybe_applied: bool,
    },
    #[error(
        "Tailscale answered {operation} in a way this version of Temps does not understand \
         ({reason}); update Temps or report it"
    )]
    NotUnderstood { operation: String, reason: String },
}

impl TailscaleApiError {
    /// Whether a write that failed this way may have been applied anyway.
    pub fn maybe_applied(&self) -> bool {
        match self {
            TailscaleApiError::Status { maybe_applied, .. }
            | TailscaleApiError::Transport { maybe_applied, .. } => *maybe_applied,
            TailscaleApiError::Credential { .. }
            | TailscaleApiError::Forbidden { .. }
            | TailscaleApiError::NotUnderstood { .. } => false,
        }
    }
}

/// A Tailscale Service as the API returns it, with the fields D4 compares.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema, Default)]
pub struct ServiceDefinition {
    /// `svc:myapp`.
    pub name: String,
    /// The TailVIPs Tailscale assigned.
    #[serde(default)]
    pub addrs: Vec<String>,
    #[serde(default)]
    pub comment: String,
    #[serde(default)]
    pub ports: Vec<String>,
    #[serde(default)]
    pub tags: Vec<String>,
}

impl ServiceDefinition {
    /// The definition of every Service Temps makes (D4): one port, the
    /// worker tag, the marker. Tailscale assigns the addresses.
    pub fn fixed(name: &str, tag: &str, marker: &str) -> Self {
        Self {
            name: service_name(name),
            addrs: Vec::new(),
            comment: marker.to_string(),
            ports: vec![SERVICE_PORT.to_string()],
            tags: vec![tag.to_string()],
        }
    }

    fn sorted(values: &[String]) -> Vec<String> {
        let mut values: Vec<String> = values.iter().map(|v| v.to_ascii_lowercase()).collect();
        values.sort();
        values.dedup();
        values
    }

    /// What differs between this read and `planned` (comment, ports, tags,
    /// addrs), in words; `None` when they are equal. Order does not count.
    pub fn differs_from(&self, planned: &ServiceDefinition) -> Option<String> {
        let name = &self.name;
        if self.comment != planned.comment {
            return Some(match parse_marker(&self.comment) {
                Some(_) => format!(
                    "`{name}` now carries the marker `{}`, not `{}`",
                    self.comment, planned.comment
                ),
                None => format!("`{name}` has no Temps marker any more"),
            });
        }
        if let Some(found) = Self::changed(&self.ports, &planned.ports) {
            return Some(format!(
                "`{name}` now listens on {found}, which Temps did not set"
            ));
        }
        if let Some(found) = Self::changed(&self.tags, &planned.tags) {
            return Some(format!(
                "`{name}` is now for hosts tagged {found}, which Temps did not set"
            ));
        }
        if let Some(found) = Self::changed(&self.addrs, &planned.addrs) {
            return Some(format!("`{name}` now has the addresses {found}"));
        }
        None
    }

    /// Whether the fixed fields (ports and tags) are Temps' definition.
    pub fn differs_from_fixed(&self, tag: &str) -> Option<String> {
        let fixed = ServiceDefinition::fixed(&self.name, tag, &self.comment);
        let name = &self.name;
        if let Some(found) = Self::changed(&self.ports, &fixed.ports) {
            return Some(format!(
                "`{name}` now listens on {found}, which Temps did not set"
            ));
        }
        if let Some(found) = Self::changed(&self.tags, &fixed.tags) {
            return Some(format!(
                "`{name}` is now for hosts tagged {found}, which Temps did not set"
            ));
        }
        None
    }

    fn changed(found: &[String], planned: &[String]) -> Option<String> {
        let found = Self::sorted(found);
        (found != Self::sorted(planned)).then(|| {
            if found.is_empty() {
                "nothing".to_string()
            } else {
                found
                    .iter()
                    .map(|v| format!("`{v}`"))
                    .collect::<Vec<_>>()
                    .join(", ")
            }
        })
    }
}

/// `myapp` or `svc:myapp` → `svc:myapp`.
pub fn service_name(name: &str) -> String {
    let label = name.trim().to_ascii_lowercase();
    match label.strip_prefix("svc:") {
        Some(_) => label,
        None => format!("svc:{label}"),
    }
}

/// `svc:myapp` → `myapp`, the DNS label of `myapp.<tailnet>.ts.net`.
pub fn service_label(name: &str) -> &str {
    name.strip_prefix("svc:").unwrap_or(name)
}

/// The marker a Service Temps makes carries (D4): ownership is the
/// install, the run id says who created it and is never rewritten.
pub fn marker(install: &str, creator_run_id: i32) -> String {
    format!("{MARKER_PREFIX}{install}:{creator_run_id}")
}

/// `temps:<install>:<run>` → `(install, run)`.
pub fn parse_marker(comment: &str) -> Option<(String, i32)> {
    let rest = comment.trim().strip_prefix(MARKER_PREFIX)?;
    let (install, run) = rest.rsplit_once(':')?;
    if install.is_empty() {
        return None;
    }
    Some((install.to_string(), run.parse().ok()?))
}

/// Who a Service read belongs to, from this install's point of view.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Ownership {
    /// This install's marker, created by `run`.
    Ours { creator_run_id: i32 },
    /// Another install's marker, or none: never written to or deleted.
    Foreign,
}

pub fn ownership(service: &ServiceDefinition, install: &str) -> Ownership {
    match parse_marker(&service.comment) {
        Some((owner, creator_run_id)) if owner == install => Ownership::Ours { creator_run_id },
        _ => Ownership::Foreign,
    }
}

/// One device advertising a Service, as the API lists it: a device, no
/// address (D4).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct ServiceHost {
    /// The stable node id (`nXXXXCNTRL`), matching tailscaled's `Self.ID`.
    pub node_id: Option<String>,
    /// The machine's MagicDNS name or host name.
    pub name: Option<String>,
    /// Whether the tailnet admin approved it; `None` when the answer does not
    /// say.
    pub approved: Option<bool>,
}

impl ServiceHost {
    /// The machine name to show: `master` of `master.example.ts.net`.
    pub fn short_name(&self) -> String {
        self.name
            .as_deref()
            .map(|n| n.trim_end_matches('.').split('.').next().unwrap_or(n))
            .or(self.node_id.as_deref())
            .unwrap_or("an unnamed device")
            .to_string()
    }

    /// Whether this host is the worker that reported `node_id` (its
    /// `Self.ID`) and `dns_name`.
    pub fn is(&self, node_id: Option<&str>, dns_name: Option<&str>) -> bool {
        if let (Some(mine), Some(theirs)) = (self.node_id.as_deref(), node_id) {
            return mine == theirs;
        }
        match (self.name.as_deref(), dns_name) {
            (Some(name), Some(dns)) => {
                let name = temps_core::tailnet::normalize_name(name);
                let dns = temps_core::tailnet::normalize_name(dns);
                name == dns || dns.split('.').next() == Some(name.as_str())
            }
            _ => false,
        }
    }
}

/// Whether one scope was proven.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum ScopeState {
    Present,
    Missing,
    /// The answer could not tell: the run refuses the credential.
    Unverified,
}

/// What the credential was proven able to do (D2).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct ScopeCheck {
    /// The client exchanged its secret for a token.
    pub valid: bool,
    /// `devices:core:read`: listing devices.
    pub devices_read: ScopeState,
    /// `services`: creating and deleting Services.
    pub services: ScopeState,
    /// Reading a Service's hosts, which the D4 host check needs.
    pub service_hosts: ScopeState,
    /// What went wrong, when something did.
    pub error: Option<String>,
}

impl ScopeCheck {
    fn refused(error: String) -> Self {
        Self {
            valid: false,
            devices_read: ScopeState::Unverified,
            services: ScopeState::Unverified,
            service_hosts: ScopeState::Unverified,
            error: Some(error),
        }
    }

    pub fn ready(&self) -> bool {
        self.valid
            && self.devices_read == ScopeState::Present
            && self.services == ScopeState::Present
            && self.service_hosts == ScopeState::Present
    }

    /// Why a run cannot use the credential, naming what to do.
    pub fn refusal(&self) -> Option<String> {
        if self.ready() {
            return None;
        }
        if let (false, Some(error)) = (self.valid, &self.error) {
            return Some(error.clone());
        }
        if self.devices_read == ScopeState::Missing || self.services == ScopeState::Missing {
            return Some(messages::CREDENTIAL_SCOPE.to_string());
        }
        if self.service_hosts == ScopeState::Missing {
            return Some(
                "This Tailscale OAuth client cannot read which machines host a Service, which \
                 Temps checks before trusting one. Temps does not ask for the wider \
                 `devices:core` scope, so the Service way in is unavailable; use the machine \
                 name instead."
                    .to_string(),
            );
        }
        Some(format!(
            "Tailscale's answers did not prove this OAuth client's scopes{}. Try again, or \
             create a client with the `services` and `devices:core:read` scopes.",
            self.error
                .as_deref()
                .map(|e| format!(" ({e})"))
                .unwrap_or_default()
        ))
    }
}

/// Access tokens by OAuth client, in memory only (D2).
#[derive(Default)]
pub struct TokenCache {
    tokens: std::sync::Mutex<HashMap<String, (String, Instant)>>,
}

impl TokenCache {
    fn get(&self, key: &str) -> Option<String> {
        let tokens = self.tokens.lock().ok()?;
        tokens
            .get(key)
            .filter(|(_, until)| Instant::now() < *until)
            .map(|(token, _)| token.clone())
    }

    fn put(&self, key: String, token: String, lifetime: Duration) {
        if let Ok(mut tokens) = self.tokens.lock() {
            let now = Instant::now();
            tokens.retain(|_, (_, until)| now < *until);
            tokens.insert(key, (token, now + lifetime.saturating_sub(TOKEN_MARGIN)));
        }
    }

    fn forget(&self, key: &str) {
        if let Ok(mut tokens) = self.tokens.lock() {
            tokens.remove(key);
        }
    }
}

/// One HTTP answer, reduced to what the parsers read.
#[derive(Debug, Clone, PartialEq)]
pub struct Answer {
    pub status: u16,
    pub body: Value,
}

impl Answer {
    pub fn new(status: u16, body: Value) -> Self {
        Self { status, body }
    }

    fn message(&self) -> String {
        self.body
            .get("message")
            .and_then(Value::as_str)
            .map(str::to_string)
            .unwrap_or_else(|| match &self.body {
                Value::Null => format!("HTTP {}", self.status),
                other => other.to_string().chars().take(200).collect(),
            })
    }

    fn ok(&self) -> bool {
        (200..300).contains(&self.status)
    }
}

/// The access token in an `oauth/token` answer, with its lifetime.
pub fn parse_token(
    answer: &Answer,
    client_id: &str,
) -> Result<(String, Duration), TailscaleApiError> {
    if matches!(answer.status, 400 | 401 | 403) {
        return Err(TailscaleApiError::Credential {
            client_id: client_id.to_string(),
            reason: answer.message(),
        });
    }
    if !answer.ok() {
        return Err(TailscaleApiError::Status {
            operation: "exchange the OAuth client for a token".into(),
            status: answer.status,
            reason: answer.message(),
            maybe_applied: false,
        });
    }
    let token = answer
        .body
        .get("access_token")
        .and_then(Value::as_str)
        .filter(|t| !t.is_empty())
        .ok_or_else(|| TailscaleApiError::NotUnderstood {
            operation: "exchange the OAuth client for a token".into(),
            reason: "no access_token".into(),
        })?;
    let lifetime = answer
        .body
        .get("expires_in")
        .and_then(Value::as_u64)
        .map(Duration::from_secs)
        .unwrap_or(Duration::from_secs(3600));
    Ok((token.to_string(), lifetime))
}

/// A refusal for missing permission, or the generic status error.
fn refusal(answer: &Answer, operation: &str, write: bool) -> TailscaleApiError {
    match answer.status {
        401 | 403 => TailscaleApiError::Forbidden {
            operation: operation.to_string(),
        },
        status => TailscaleApiError::Status {
            operation: operation.to_string(),
            status,
            reason: answer.message(),
            maybe_applied: write && status >= 500,
        },
    }
}

/// `GET …/services/{name}`: the Service, `None` when absent.
pub fn parse_service(
    answer: &Answer,
    name: &str,
) -> Result<Option<ServiceDefinition>, TailscaleApiError> {
    let operation = format!("read `{name}`");
    if answer.status == 404 {
        return Ok(None);
    }
    if !answer.ok() {
        return Err(refusal(answer, &operation, false));
    }
    let service: ServiceDefinition = serde_json::from_value(answer.body.clone()).map_err(|e| {
        TailscaleApiError::NotUnderstood {
            operation: operation.clone(),
            reason: e.to_string(),
        }
    })?;
    if service_name(&service.name) != service_name(name) {
        return Err(TailscaleApiError::NotUnderstood {
            operation,
            reason: format!("the answer is for `{}`", service.name),
        });
    }
    Ok(Some(service))
}

fn text(item: &Value, keys: &[&str]) -> Option<String> {
    keys.iter()
        .find_map(|k| item.get(*k).and_then(Value::as_str))
        .filter(|v| !v.is_empty())
        .map(str::to_string)
}

/// `GET /device/{nodeId}`: whether it is a device of the credential's
/// tailnet. Tailscale answers 404 for a device of any other tailnet; a 200
/// naming another node counts as not found, never as found.
pub fn parse_device(answer: &Answer, node_id: &str) -> Result<bool, TailscaleApiError> {
    match answer.status {
        200..=299 => Ok(text(&answer.body, &["nodeId"]).as_deref() == Some(node_id)),
        404 => Ok(false),
        _ => Err(refusal(
            answer,
            &format!("look up the worker's device `{node_id}`"),
            false,
        )),
    }
}

/// `GET …/services/{name}/devices`: the devices advertising it. A Service
/// that does not exist has none.
pub fn parse_hosts(answer: &Answer, name: &str) -> Result<Vec<ServiceHost>, TailscaleApiError> {
    let operation = format!("list the hosts of `{name}`");
    if answer.status == 404 {
        return Ok(Vec::new());
    }
    if !answer.ok() {
        return Err(refusal(answer, &operation, false));
    }
    let items = answer
        .body
        .get("devices")
        .or_else(|| answer.body.get("hosts"))
        .and_then(Value::as_array)
        .or_else(|| answer.body.as_array())
        .ok_or_else(|| TailscaleApiError::NotUnderstood {
            operation: operation.clone(),
            reason: "no list of devices".into(),
        })?;
    items
        .iter()
        .map(|item| {
            let approved = match item.get("approvalLevel") {
                None | Some(Value::Null) => None,
                Some(level) => match level.as_str() {
                    Some("approved:auto" | "approved:manual") => Some(true),
                    Some("not-approved") => Some(false),
                    _ => {
                        return Err(TailscaleApiError::NotUnderstood {
                            operation: operation.clone(),
                            reason: format!("approvalLevel {level}"),
                        })
                    }
                },
            };
            let host = ServiceHost {
                node_id: text(item, &["nodeId", "stableNodeId", "deviceId", "id"]),
                name: text(item, &["name", "hostname"]),
                approved,
            };
            if host.node_id.is_none() && host.name.is_none() {
                return Err(TailscaleApiError::NotUnderstood {
                    operation: operation.clone(),
                    reason: "a host with neither an id nor a name".into(),
                });
            }
            Ok(host)
        })
        .collect()
}

/// The state a probe's answer proves.
fn probe_state(answer: &Answer, proves_present: &[u16]) -> ScopeState {
    match answer.status {
        401 | 403 => ScopeState::Missing,
        status if proves_present.contains(&status) => ScopeState::Present,
        _ => ScopeState::Unverified,
    }
}

/// Combine the three probes (D2).
pub fn assemble_scopes(devices: &Answer, services: &Answer, hosts: &Answer) -> ScopeCheck {
    // A device list read proves the read scope.
    let devices_read = match devices.status {
        200 if devices
            .body
            .get("devices")
            .and_then(Value::as_array)
            .is_some() =>
        {
            ScopeState::Present
        }
        _ => probe_state(devices, &[]),
    };
    // A name no tailnet accepts: a validation refusal proves the scope; a
    // success would mean Tailscale took it, which the check cannot read as
    // anything.
    let services_state = probe_state(services, &[400, 422]);
    // The probe name exists nowhere: "not found" (or a refusal of the name)
    // proves the hosts read is allowed.
    let hosts_state = probe_state(hosts, &[400, 404, 422]);
    let error = [
        (devices_read, devices, "listing devices"),
        (services_state, services, "the Services probe"),
        (hosts_state, hosts, "reading a Service's hosts"),
    ]
    .iter()
    .find(|(state, _, _)| *state == ScopeState::Unverified)
    .map(|(_, answer, what)| {
        format!(
            "{what} answered HTTP {}: {}",
            answer.status,
            answer.message()
        )
    });
    ScopeCheck {
        valid: true,
        devices_read,
        services: services_state,
        service_hosts: hosts_state,
        error,
    }
}

/// What the run, plan and checks do with Tailscale's API; a fake in tests.
#[async_trait]
pub trait TailnetApi: Send + Sync {
    async fn check_scopes(&self, credential: &TailscaleCredential) -> ScopeCheck;
    async fn get_service(
        &self,
        credential: &TailscaleCredential,
        name: &str,
    ) -> Result<Option<ServiceDefinition>, TailscaleApiError>;
    /// `PUT`: only ever to create (D4).
    async fn create_service(
        &self,
        credential: &TailscaleCredential,
        definition: &ServiceDefinition,
    ) -> Result<(), TailscaleApiError>;
    async fn delete_service(
        &self,
        credential: &TailscaleCredential,
        name: &str,
    ) -> Result<(), TailscaleApiError>;
    async fn service_hosts(
        &self,
        credential: &TailscaleCredential,
        name: &str,
    ) -> Result<Vec<ServiceHost>, TailscaleApiError>;
    /// Whether the node with stable id `node_id` is a device of the
    /// credential's tailnet: the check that the worker hosting a Service is
    /// in the tailnet the Service is created in.
    async fn has_device(
        &self,
        credential: &TailscaleCredential,
        node_id: &str,
    ) -> Result<bool, TailscaleApiError>;
}

/// Tailscale's API over HTTPS.
pub struct TailscaleClient {
    /// Its own client, which never follows a redirect: a 307/308 would
    /// re-send the token exchange's body, `client_secret` included, to
    /// wherever it points. `Err` when it could not be built, and then every
    /// call fails with why.
    http: Result<reqwest::Client, String>,
    base: String,
    tokens: TokenCache,
}

impl Default for TailscaleClient {
    fn default() -> Self {
        Self::new()
    }
}

impl TailscaleClient {
    pub fn new() -> Self {
        Self::with_base(API.to_string())
    }

    /// Against another server, for tests.
    pub fn with_base(base: String) -> Self {
        let http = reqwest::Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .timeout(REQUEST_TIMEOUT)
            .build()
            .map_err(|e| format!("the HTTPS client for Tailscale's API could not be built: {e}"));
        Self {
            http,
            base,
            tokens: TokenCache::default(),
        }
    }

    fn http(&self, operation: &str) -> Result<&reqwest::Client, TailscaleApiError> {
        self.http
            .as_ref()
            .map_err(|reason| TailscaleApiError::Transport {
                operation: operation.into(),
                reason: reason.clone(),
                maybe_applied: false,
            })
    }

    fn tailnet_url(&self, credential: &TailscaleCredential, rest: &str) -> String {
        let tailnet: String =
            url::form_urlencoded::byte_serialize(credential.tailnet.as_bytes()).collect();
        format!("{}/tailnet/{tailnet}/{rest}", self.base)
    }

    fn service_path(name: &str) -> String {
        url::form_urlencoded::byte_serialize(service_name(name).as_bytes()).collect()
    }

    async fn read(response: reqwest::Response) -> Result<Answer, String> {
        let status = response.status().as_u16();
        let mut response = response;
        let mut body = Vec::new();
        while let Some(chunk) = response.chunk().await.map_err(|e| e.to_string())? {
            if body.len() + chunk.len() > MAX_BODY {
                return Err(format!("the answer is larger than {MAX_BODY} bytes"));
            }
            body.extend_from_slice(&chunk);
        }
        let body = if body.is_empty() {
            Value::Null
        } else {
            serde_json::from_slice(&body)
                .unwrap_or_else(|_| Value::String(String::from_utf8_lossy(&body).into_owned()))
        };
        Ok(Answer { status, body })
    }

    async fn token(&self, credential: &TailscaleCredential) -> Result<String, TailscaleApiError> {
        let key = credential.cache_key();
        if let Some(token) = self.tokens.get(&key) {
            return Ok(token);
        }
        let operation = "exchange the OAuth client for a token";
        let form: String = url::form_urlencoded::Serializer::new(String::new())
            .append_pair("client_id", &credential.client_id)
            .append_pair("client_secret", &credential.client_secret)
            .append_pair("grant_type", "client_credentials")
            .finish();
        let response = self
            .http(operation)?
            .post(format!("{}/oauth/token", self.base))
            .header("content-type", "application/x-www-form-urlencoded")
            .body(form)
            .timeout(REQUEST_TIMEOUT)
            .send()
            .await
            .map_err(|e| TailscaleApiError::Transport {
                operation: operation.into(),
                reason: e.to_string(),
                maybe_applied: false,
            })?;
        let answer = Self::read(response)
            .await
            .map_err(|reason| TailscaleApiError::Transport {
                operation: operation.into(),
                reason,
                maybe_applied: false,
            })?;
        let (token, lifetime) = parse_token(&answer, &credential.client_id)?;
        self.tokens.put(key, token.clone(), lifetime);
        Ok(token)
    }

    async fn call(
        &self,
        credential: &TailscaleCredential,
        method: reqwest::Method,
        url: String,
        body: Option<&Value>,
        operation: &str,
    ) -> Result<Answer, TailscaleApiError> {
        let write = method != reqwest::Method::GET;
        let token = self.token(credential).await?;
        let mut request = self
            .http(operation)?
            .request(method, &url)
            .bearer_auth(&token)
            .timeout(REQUEST_TIMEOUT);
        if let Some(body) = body {
            request = request.json(body);
        }
        let response = request
            .send()
            .await
            .map_err(|e| TailscaleApiError::Transport {
                operation: operation.into(),
                reason: e.to_string(),
                // A write whose request may have reached Tailscale.
                maybe_applied: write && !e.is_connect(),
            })?;
        let answer = Self::read(response)
            .await
            .map_err(|reason| TailscaleApiError::Transport {
                operation: operation.into(),
                reason,
                maybe_applied: write,
            })?;
        if answer.status == 401 {
            // An expired or revoked token: the next call exchanges again.
            self.tokens.forget(&credential.cache_key());
        }
        Ok(answer)
    }
}

#[async_trait]
impl TailnetApi for TailscaleClient {
    async fn check_scopes(&self, credential: &TailscaleCredential) -> ScopeCheck {
        if let Err(error) = self.token(credential).await {
            return ScopeCheck::refused(error.to_string());
        }
        let answer = |result: Result<Answer, TailscaleApiError>| match result {
            Ok(answer) => answer,
            Err(error) => Answer::new(0, Value::String(error.to_string())),
        };
        let devices = answer(
            self.call(
                credential,
                reqwest::Method::GET,
                self.tailnet_url(credential, "devices?fields=default"),
                None,
                "list devices",
            )
            .await,
        );
        let probe = ServiceDefinition::fixed(PROBE_SERVICE, DEFAULT_TAG, "temps:scope-check:0");
        let services = answer(
            self.call(
                credential,
                reqwest::Method::PUT,
                self.tailnet_url(
                    credential,
                    &format!("services/{}", Self::service_path(PROBE_SERVICE)),
                ),
                serde_json::to_value(&probe).ok().as_ref(),
                "check the services scope",
            )
            .await,
        );
        if services.ok() {
            // Tailscale took a name it should refuse. Nothing can be read
            // from that; take it away again and leave the scope unverified.
            let _ = self.delete_service(credential, PROBE_SERVICE).await;
        }
        let hosts = answer(
            self.call(
                credential,
                reqwest::Method::GET,
                self.tailnet_url(
                    credential,
                    &format!("services/{}/devices", Self::service_path(PROBE_SERVICE)),
                ),
                None,
                "check reading a Service's hosts",
            )
            .await,
        );
        assemble_scopes(&devices, &services, &hosts)
    }

    async fn get_service(
        &self,
        credential: &TailscaleCredential,
        name: &str,
    ) -> Result<Option<ServiceDefinition>, TailscaleApiError> {
        let answer = self
            .call(
                credential,
                reqwest::Method::GET,
                self.tailnet_url(
                    credential,
                    &format!("services/{}", Self::service_path(name)),
                ),
                None,
                &format!("read `{}`", service_name(name)),
            )
            .await?;
        parse_service(&answer, name)
    }

    async fn create_service(
        &self,
        credential: &TailscaleCredential,
        definition: &ServiceDefinition,
    ) -> Result<(), TailscaleApiError> {
        let operation = format!("create `{}`", definition.name);
        let body = serde_json::json!({
            "name": definition.name,
            "comment": definition.comment,
            "ports": definition.ports,
            "tags": definition.tags,
        });
        let answer = self
            .call(
                credential,
                reqwest::Method::PUT,
                self.tailnet_url(
                    credential,
                    &format!("services/{}", Self::service_path(&definition.name)),
                ),
                Some(&body),
                &operation,
            )
            .await?;
        if answer.ok() {
            Ok(())
        } else {
            Err(refusal(&answer, &operation, true))
        }
    }

    async fn delete_service(
        &self,
        credential: &TailscaleCredential,
        name: &str,
    ) -> Result<(), TailscaleApiError> {
        let operation = format!("delete `{}`", service_name(name));
        let answer = self
            .call(
                credential,
                reqwest::Method::DELETE,
                self.tailnet_url(
                    credential,
                    &format!("services/{}", Self::service_path(name)),
                ),
                None,
                &operation,
            )
            .await?;
        if answer.ok() || answer.status == 404 {
            Ok(())
        } else {
            Err(refusal(&answer, &operation, true))
        }
    }

    async fn service_hosts(
        &self,
        credential: &TailscaleCredential,
        name: &str,
    ) -> Result<Vec<ServiceHost>, TailscaleApiError> {
        let answer = self
            .call(
                credential,
                reqwest::Method::GET,
                self.tailnet_url(
                    credential,
                    &format!("services/{}/devices", Self::service_path(name)),
                ),
                None,
                &format!("list the hosts of `{}`", service_name(name)),
            )
            .await?;
        parse_hosts(&answer, name)
    }

    async fn has_device(
        &self,
        credential: &TailscaleCredential,
        node_id: &str,
    ) -> Result<bool, TailscaleApiError> {
        let id: String = url::form_urlencoded::byte_serialize(node_id.as_bytes()).collect();
        let answer = self
            .call(
                credential,
                reqwest::Method::GET,
                format!("{}/device/{id}", self.base),
                None,
                &format!("look up the worker's device `{node_id}`"),
            )
            .await?;
        parse_device(&answer, node_id)
    }
}

/// The policy entry that lets the tailnet approve Temps' hosts by itself
/// (D4): one per Service, since a pattern is not documented.
pub fn auto_approvers_snippet(service: &str, tag: &str) -> String {
    format!(
        "\"autoApprovers\": {{\"services\": {{\"{}\": [\"{tag}\"]}}}}",
        service_name(service)
    )
}

/// Whether `name` can be a Service name.
pub fn valid_name(name: &str) -> bool {
    valid_service_label(name)
}

/// The `tailscale` provider in the ADR 058 D8 registry: a credential, not a
/// tunnel. Its row holds the OAuth client sealed in `setup_secrets`.
pub struct TailscaleFactory;

impl TailscaleFactory {
    pub fn fields() -> Vec<SetupField> {
        vec![
            SetupField {
                key: FIELD_CLIENT_ID.into(),
                label: "OAuth client ID".into(),
                help: Some(
                    "Create it at login.tailscale.com/admin/settings/oauth with the `services` \
                     and `devices:core:read` scopes, and the tag your workers use."
                        .into(),
                ),
                kind: FieldKind::Text,
                required: true,
                placeholder: Some("k123AbCdEf11CNTRL".into()),
            },
            SetupField {
                key: FIELD_CLIENT_SECRET.into(),
                label: "OAuth client secret".into(),
                help: Some(
                    "Stored encrypted and never shown again. The `services` scope covers the \
                     whole tailnet: whoever holds this secret can create, change and delete \
                     every Service in it."
                        .into(),
                ),
                kind: FieldKind::Secret,
                required: true,
                placeholder: Some("tskey-client-…".into()),
            },
            SetupField {
                key: FIELD_TAILNET.into(),
                label: "Tailnet".into(),
                help: Some("Leave `-` for the OAuth client's own tailnet.".into()),
                kind: FieldKind::Text,
                required: false,
                placeholder: Some(DEFAULT_TAILNET.into()),
            },
        ]
    }

    pub fn info() -> ProviderInfo {
        ProviderInfo {
            kind: KIND.into(),
            name: "Tailscale".into(),
            summary: "Gives a private app its own name in your tailnet, as a Tailscale Service."
                .into(),
            requirements: vec![
                "A Tailscale OAuth client with the `services` and `devices:core:read` scopes"
                    .into(),
                "Workers signed in with a tag (`tag:temps`) and allowed to use Tailscale Serve"
                    .into(),
                "MagicDNS and HTTPS certificates on in the tailnet".into(),
            ],
            needs_public_ip: false,
            works_behind_cgnat: true,
            dns: DnsHandling::ProviderHostname,
            fields: Self::fields(),
            docs_url: Some("https://tailscale.com/kb/1552/tailscale-services".into()),
        }
    }
}

#[async_trait]
impl crate::registry::CredentialFactory for TailscaleFactory {
    fn info(&self) -> ProviderInfo {
        Self::info()
    }

    async fn validate(
        &self,
        _deps: &ProviderDeps<'_>,
        setup: &ProviderSetup,
    ) -> Result<(), IngressError> {
        let credential = TailscaleCredential::from_answers(&setup.settings, &setup.secrets)?;
        // Not `deps.http`: the Tailscale client never follows redirects.
        let check = TailscaleClient::new().check_scopes(&credential).await;
        match check.refusal() {
            None => Ok(()),
            Some(reason) => Err(IngressError::Validation(reason)),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    // Answers in the shape Tailscale's v2 API documents: errors are
    // `{"message": …}`, a Service is `{name, addrs, comment, annotations,
    // ports, tags}`. They pin how each answer is read.
    fn answer(status: u16, body: Value) -> Answer {
        Answer::new(status, body)
    }

    fn service_body() -> Value {
        json!({
            "name": "svc:myapp",
            "addrs": ["100.100.10.20", "fd7a:115c:a1e0::aa01"],
            "comment": "temps:inst-1:7",
            "annotations": {},
            "ports": ["tcp:443"],
            "tags": ["tag:temps"]
        })
    }

    #[test]
    fn a_token_answer_gives_the_token_and_its_lifetime() {
        let (token, lifetime) = parse_token(
            &answer(
                200,
                json!({"access_token": "tskey-api-abc", "token_type": "Bearer", "expires_in": 3600, "scope": "services devices:core:read"}),
            ),
            "k1",
        )
        .expect("token");
        assert_eq!(token, "tskey-api-abc");
        assert_eq!(lifetime, Duration::from_secs(3600));
    }

    #[test]
    fn a_refused_client_names_it_and_what_to_do() {
        let error = parse_token(&answer(401, json!({"message": "invalid client"})), "k1")
            .expect_err("refused");
        assert!(matches!(error, TailscaleApiError::Credential { .. }));
        let text = error.to_string();
        assert!(
            text.contains("k1") && text.contains("invalid client"),
            "{text}"
        );
        assert!(matches!(
            parse_token(&answer(200, json!({"token_type": "Bearer"})), "k1"),
            Err(TailscaleApiError::NotUnderstood { .. })
        ));
    }

    #[test]
    fn a_service_is_read_and_absent_is_none() {
        let service = parse_service(&answer(200, service_body()), "myapp")
            .expect("read")
            .expect("present");
        assert_eq!(service.name, "svc:myapp");
        assert_eq!(service.addrs.len(), 2);
        assert_eq!(
            parse_service(&answer(404, json!({"message": "not found"})), "myapp").expect("read"),
            None
        );
        assert!(matches!(
            parse_service(&answer(403, json!({"message": "forbidden"})), "myapp"),
            Err(TailscaleApiError::Forbidden { .. })
        ));
        // An answer about another name is not understood, never accepted.
        assert!(matches!(
            parse_service(&answer(200, service_body()), "other"),
            Err(TailscaleApiError::NotUnderstood { .. })
        ));
        assert!(matches!(
            parse_service(&answer(200, json!(["not", "a", "service"])), "myapp"),
            Err(TailscaleApiError::NotUnderstood { .. })
        ));
    }

    #[test]
    fn the_marker_names_the_install_and_the_creator() {
        assert_eq!(marker("inst-1", 7), "temps:inst-1:7");
        assert_eq!(parse_marker("temps:inst-1:7"), Some(("inst-1".into(), 7)));
        assert_eq!(parse_marker("made by hand"), None);
        assert_eq!(parse_marker("temps::7"), None);
        assert_eq!(parse_marker("temps:inst-1:seven"), None);
        let ours = ServiceDefinition {
            comment: marker("inst-1", 7),
            ..Default::default()
        };
        assert_eq!(
            ownership(&ours, "inst-1"),
            Ownership::Ours { creator_run_id: 7 }
        );
        assert_eq!(ownership(&ours, "inst-2"), Ownership::Foreign);
        assert_eq!(
            ownership(&ServiceDefinition::default(), "inst-1"),
            Ownership::Foreign
        );
    }

    #[test]
    fn a_definition_names_the_field_that_differs() {
        let planned: ServiceDefinition =
            serde_json::from_value(service_body()).expect("definition");
        let mut read = planned.clone();
        read.ports.reverse();
        assert_eq!(read.differs_from(&planned), None, "order does not count");
        read.ports.push("tcp:8443".into());
        let what = read.differs_from(&planned).expect("differs");
        assert!(
            what.contains("tcp:8443") && what.contains("did not set"),
            "{what}"
        );
        let mut read = planned.clone();
        read.comment = "made by hand".into();
        assert!(read
            .differs_from(&planned)
            .expect("differs")
            .contains("no Temps marker"));
        let mut read = planned.clone();
        read.addrs = vec!["100.100.10.21".into()];
        assert!(read
            .differs_from(&planned)
            .expect("differs")
            .contains("100.100.10.21"));
        assert_eq!(planned.differs_from_fixed(DEFAULT_TAG), None);
        assert!(planned.differs_from_fixed("tag:web").is_some());
    }

    #[test]
    fn hosts_are_devices_matched_by_node_id_or_name() {
        let hosts = parse_hosts(
            &answer(
                200,
                json!({"devices": [
                    {"nodeId": "nAAA1CNTRL", "name": "master.example.ts.net", "approvalLevel": "approved:auto"},
                    {"nodeId": "nBBB2CNTRL", "name": "nas.example.ts.net", "approvalLevel": "not-approved"}
                ]}),
            ),
            "myapp",
        )
        .expect("hosts");
        assert_eq!(hosts.len(), 2);
        assert!(hosts[0].is(Some("nAAA1CNTRL"), None));
        assert!(!hosts[0].is(Some("nBBB2CNTRL"), Some("master.example.ts.net")));
        assert!(hosts[1].is(None, Some("nas.example.ts.net")));
        assert_eq!(hosts[1].short_name(), "nas");
        assert_eq!(hosts[1].approved, Some(false));
        assert_eq!(
            parse_hosts(&answer(404, json!({"message": "not found"})), "myapp").expect("none"),
            Vec::new()
        );
        assert!(matches!(
            parse_hosts(&answer(200, json!({"items": 3})), "myapp"),
            Err(TailscaleApiError::NotUnderstood { .. })
        ));
        assert!(matches!(
            parse_hosts(
                &answer(
                    200,
                    json!({"devices": [{"approvalLevel": "approved:auto"}]})
                ),
                "myapp"
            ),
            Err(TailscaleApiError::NotUnderstood { .. })
        ));
        // An approval level this version does not know is never read as
        // approved.
        assert!(matches!(
            parse_hosts(
                &answer(
                    200,
                    json!({"devices": [{"nodeId": "n1", "approvalLevel": "approved:later"}]})
                ),
                "myapp"
            ),
            Err(TailscaleApiError::NotUnderstood { .. })
        ));
        assert_eq!(
            parse_hosts(
                &answer(200, json!({"devices": [{"nodeId": "n1"}]})),
                "myapp"
            )
            .expect("hosts")[0]
                .approved,
            None
        );
    }

    #[test]
    fn forbidden_means_missing_and_validation_means_present() {
        let devices = answer(200, json!({"devices": []}));
        let invalid = answer(400, json!({"message": "invalid service name"}));
        let absent = answer(404, json!({"message": "not found"}));
        let forbidden = answer(403, json!({"message": "insufficient scope"}));

        let all = assemble_scopes(&devices, &invalid, &absent);
        assert!(all.ready(), "{all:?}");
        assert_eq!(all.refusal(), None);

        let no_services = assemble_scopes(&devices, &forbidden, &absent);
        assert_eq!(no_services.services, ScopeState::Missing);
        assert_eq!(
            no_services.refusal().as_deref(),
            Some(messages::CREDENTIAL_SCOPE)
        );

        let no_read = assemble_scopes(&forbidden, &invalid, &absent);
        assert_eq!(no_read.devices_read, ScopeState::Missing);
        assert!(!no_read.ready());

        // devices:core:read that cannot read a Service's hosts keeps the
        // Service way in unavailable, and never asks for devices:core.
        let no_hosts = assemble_scopes(&devices, &invalid, &forbidden);
        assert_eq!(no_hosts.service_hosts, ScopeState::Missing);
        let reason = no_hosts.refusal().expect("refused");
        assert!(
            reason.contains("unavailable") && reason.contains("machine name"),
            "{reason}"
        );
    }

    #[test]
    fn an_answer_that_cannot_tell_leaves_the_scope_unverified() {
        let devices = answer(200, json!({"devices": []}));
        let absent = answer(404, json!({"message": "not found"}));
        // Tailscale accepted a name it must refuse, or failed.
        for services in [
            answer(200, service_body()),
            answer(500, json!({"message": "internal"})),
            answer(0, Value::String("connection reset".into())),
        ] {
            let check = assemble_scopes(&devices, &services, &absent);
            assert_eq!(check.services, ScopeState::Unverified);
            assert!(!check.ready());
            assert!(check.refusal().expect("refused").contains("did not prove"));
        }
    }

    #[test]
    fn the_probe_name_is_one_no_tailnet_accepts() {
        assert!(!valid_name(PROBE_SERVICE));
        assert!(valid_name("myapp-staging"));
    }

    #[test]
    fn a_credential_needs_an_id_and_a_secret_and_never_shows_the_secret() {
        let mut settings = BTreeMap::new();
        let mut secrets = BTreeMap::new();
        assert!(TailscaleCredential::from_answers(&settings, &secrets).is_err());
        settings.insert(FIELD_CLIENT_ID.to_string(), "k1".to_string());
        assert!(TailscaleCredential::from_answers(&settings, &secrets).is_err());
        secrets.insert(
            FIELD_CLIENT_SECRET.to_string(),
            "tskey-client-secret".to_string(),
        );
        let credential = TailscaleCredential::from_answers(&settings, &secrets).expect("valid");
        assert_eq!(credential.tailnet, DEFAULT_TAILNET);
        assert!(!format!("{credential:?}").contains("tskey-client-secret"));
        settings.insert(FIELD_TAILNET.to_string(), "example.com/../x".to_string());
        assert!(TailscaleCredential::from_answers(&settings, &secrets).is_err());
        // `.` and `..` would be resolved out of `/tailnet/` in the API path.
        for dots in [".", "..", "..."] {
            settings.insert(FIELD_TAILNET.to_string(), dots.to_string());
            assert!(
                TailscaleCredential::from_answers(&settings, &secrets).is_err(),
                "{dots}"
            );
        }
        settings.insert(FIELD_TAILNET.to_string(), "example.com".to_string());
        assert!(TailscaleCredential::from_answers(&settings, &secrets).is_ok());
    }

    #[test]
    fn a_device_counts_only_when_its_tailnet_answers_with_that_node() {
        let found = answer(200, json!({"nodeId": "nWORKER"}));
        assert!(parse_device(&found, "nWORKER").unwrap());
        assert!(!parse_device(&found, "nOTHER").unwrap());
        assert!(!parse_device(&answer(200, json!({})), "nWORKER").unwrap());
        assert!(!parse_device(&answer(404, json!({"message": "not found"})), "nWORKER").unwrap());
        assert!(matches!(
            parse_device(&answer(403, json!({})), "nWORKER"),
            Err(TailscaleApiError::Forbidden { .. })
        ));
        assert!(parse_device(&answer(500, json!({})), "nWORKER").is_err());
    }

    #[tokio::test]
    async fn the_worker_is_looked_up_in_the_credentials_tailnet() {
        let (base, _) = fake_api().await;
        let client = TailscaleClient::with_base(base);
        let credential = credential("right");
        assert!(client.has_device(&credential, "nWORKER").await.unwrap());
        assert!(!client.has_device(&credential, "nELSEWHERE").await.unwrap());
    }

    #[test]
    fn the_auto_approvers_snippet_names_the_service_and_tag() {
        assert_eq!(
            auto_approvers_snippet("myapp", "tag:temps"),
            r#""autoApprovers": {"services": {"svc:myapp": ["tag:temps"]}}"#
        );
    }

    /// A Tailscale API on a local port: the token endpoint counts its
    /// calls, so caching shows.
    async fn fake_api() -> (String, std::sync::Arc<std::sync::atomic::AtomicUsize>) {
        use axum::routing::{get, post};
        use std::sync::atomic::{AtomicUsize, Ordering};
        let exchanges = std::sync::Arc::new(AtomicUsize::new(0));
        let counted = exchanges.clone();
        let app = axum::Router::new()
            .route(
                "/api/v2/oauth/token",
                post(move |body: String| {
                    let counted = counted.clone();
                    async move {
                        counted.fetch_add(1, Ordering::SeqCst);
                        if body.contains("client_secret=right") {
                            (
                                axum::http::StatusCode::OK,
                                axum::Json(
                                    json!({"access_token": "tskey-api-1", "expires_in": 3600}),
                                ),
                            )
                        } else {
                            (
                                axum::http::StatusCode::UNAUTHORIZED,
                                axum::Json(json!({"message": "invalid client credentials"})),
                            )
                        }
                    }
                }),
            )
            .route(
                "/api/v2/tailnet/{tailnet}/services/{name}",
                get(
                    |axum::extract::Path((_, name)): axum::extract::Path<(String, String)>,
                     headers: axum::http::HeaderMap| async move {
                        let authorized = headers.get("authorization").and_then(|v| v.to_str().ok())
                            == Some("Bearer tskey-api-1");
                        if !authorized {
                            return (
                                axum::http::StatusCode::UNAUTHORIZED,
                                axum::Json(json!({"message": "unauthorized"})),
                            );
                        }
                        if name == "svc:myapp" {
                            (axum::http::StatusCode::OK, axum::Json(service_body()))
                        } else {
                            (
                                axum::http::StatusCode::NOT_FOUND,
                                axum::Json(json!({"message": "not found"})),
                            )
                        }
                    },
                ),
            );
        // Only `nWORKER` is a device of the credential's tailnet.
        let app = app.route(
            "/api/v2/device/{id}",
            get(
                |axum::extract::Path(id): axum::extract::Path<String>| async move {
                    if id == "nWORKER" {
                        (
                            axum::http::StatusCode::OK,
                            axum::Json(
                                json!({"nodeId": "nWORKER", "name": "master.example.ts.net"}),
                            ),
                        )
                    } else {
                        (
                            axum::http::StatusCode::NOT_FOUND,
                            axum::Json(json!({"message": "not found"})),
                        )
                    }
                },
            ),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind");
        let address = listener.local_addr().expect("address");
        tokio::spawn(async move {
            let _ = axum::serve(listener, app).await;
        });
        (format!("http://{address}/api/v2"), exchanges)
    }

    fn credential(secret: &str) -> TailscaleCredential {
        TailscaleCredential {
            client_id: "k1".into(),
            client_secret: secret.into(),
            tailnet: DEFAULT_TAILNET.into(),
        }
    }

    #[tokio::test]
    async fn the_token_is_exchanged_once_and_kept_in_memory() {
        let (base, exchanges) = fake_api().await;
        let client = TailscaleClient::with_base(base);
        let right = credential("right");
        let found = client.get_service(&right, "myapp").await.expect("read");
        assert_eq!(found.expect("present").comment, "temps:inst-1:7");
        assert_eq!(
            client.get_service(&right, "other").await.expect("read"),
            None
        );
        assert_eq!(
            exchanges.load(std::sync::atomic::Ordering::SeqCst),
            1,
            "one exchange for both calls"
        );
        let error = client
            .get_service(&credential("wrong"), "myapp")
            .await
            .expect_err("refused");
        assert!(
            matches!(error, TailscaleApiError::Credential { .. }),
            "{error}"
        );
    }

    /// A redirect is never followed: on a 307/308 the token exchange's
    /// body, `client_secret` included, would be sent to wherever it points.
    #[tokio::test]
    async fn a_redirect_from_the_token_exchange_is_never_followed() {
        use axum::routing::post;
        use std::sync::atomic::{AtomicUsize, Ordering};
        let followed = std::sync::Arc::new(AtomicUsize::new(0));
        let counted = followed.clone();
        let app = axum::Router::new()
            .route(
                "/api/v2/oauth/token",
                post(|| async {
                    (
                        axum::http::StatusCode::PERMANENT_REDIRECT,
                        [(axum::http::header::LOCATION, "/elsewhere")],
                    )
                }),
            )
            .route(
                "/elsewhere",
                post(move |_body: String| {
                    let counted = counted.clone();
                    async move {
                        counted.fetch_add(1, Ordering::SeqCst);
                        axum::Json(json!({"access_token": "tskey-api-1", "expires_in": 3600}))
                    }
                }),
            );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind");
        let address = listener.local_addr().expect("address");
        tokio::spawn(async move {
            let _ = axum::serve(listener, app).await;
        });
        let client = TailscaleClient::with_base(format!("http://{address}/api/v2"));
        let error = client
            .get_service(&credential("right"), "myapp")
            .await
            .expect_err("a redirect is not a token");
        assert_eq!(followed.load(Ordering::SeqCst), 0, "{error}");
    }
}
