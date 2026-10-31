// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! tailscaled's LocalAPI, over its unix socket (ADR 065 D2).
//!
//! Only what the agent needs: `status`, `serve-config` (read with its `Etag`,
//! write with `If-Match`; a lost race is `412`), `prefs` (`AdvertiseServices`)
//! and `whois`. Tailscale does not document the LocalAPI as stable, so every
//! answer is parsed into the fields Temps relies on and nothing else is
//! assumed: a document that does not parse is [`LocalApiError::NotUnderstood`],
//! which stops the Tailscale way in on the worker instead of guessing.
//!
//! The Serve config is edited, never rebuilt: [`ServeConfig`] keeps every
//! field it does not model and writes it back unchanged, and the edit
//! functions here touch only handlers whose target is one of the agent's own
//! sockets ([`temps_core::tailnet::is_owned_target`]). They never set
//! `AllowFunnel` and there is no "reset".

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::time::Duration;

use async_trait::async_trait;
use bytes::Bytes;
use http_body_util::{BodyExt, Full, Limited};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use temps_core::tailnet::{is_owned_target_in, SERVE_HTTPS_PORT};

/// Each LocalAPI call, end to end.
const CALL_TIMEOUT: Duration = Duration::from_secs(10);
/// Largest answer read. `status` without peers and the Serve config are a few
/// KiB; a whois is smaller.
const MAX_BODY: usize = 4 * 1024 * 1024;

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum LocalApiError {
    #[error("tailscaled socket {path} is not there")]
    NoSocket { path: String },
    #[error("tailscaled LocalAPI {call} failed: {reason}")]
    Transport { call: &'static str, reason: String },
    #[error("tailscaled LocalAPI {call} answered HTTP {status}: {body}")]
    Status {
        call: &'static str,
        status: u16,
        body: String,
    },
    #[error(
        "tailscaled LocalAPI {call} answered something this agent does not understand: {reason}"
    )]
    NotUnderstood { call: &'static str, reason: String },
}

/// The parts of `ipnstate.Status` the agent reads.
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "PascalCase")]
pub struct Status {
    #[serde(default)]
    pub version: String,
    pub backend_state: String,
    #[serde(rename = "Self", default)]
    pub self_node: Option<PeerStatus>,
    #[serde(default)]
    pub cert_domains: Option<Vec<String>>,
    #[serde(default)]
    pub current_tailnet: Option<TailnetStatus>,
    /// Profiles by user id, for the login of a device signed in as a user.
    #[serde(default)]
    pub user: Option<BTreeMap<String, UserProfile>>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "PascalCase")]
pub struct PeerStatus {
    /// The stable node id (`nXXXXCNTRL`).
    #[serde(rename = "ID", default)]
    pub id: String,
    #[serde(rename = "DNSName", default)]
    pub dns_name: String,
    #[serde(rename = "TailscaleIPs", default)]
    pub tailscale_ips: Option<Vec<String>>,
    #[serde(default)]
    pub tags: Option<Vec<String>>,
    #[serde(rename = "UserID", default)]
    pub user_id: Option<i64>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "PascalCase")]
pub struct TailnetStatus {
    #[serde(default)]
    pub name: String,
    #[serde(rename = "MagicDNSSuffix", default)]
    pub magic_dns_suffix: String,
    #[serde(rename = "MagicDNSEnabled", default)]
    pub magic_dns_enabled: bool,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "PascalCase")]
pub struct UserProfile {
    #[serde(default)]
    pub login_name: String,
}

impl Status {
    /// The login the device is signed in as, when it is not tagged.
    pub fn login(&self) -> Option<String> {
        let node = self.self_node.as_ref()?;
        if node.tags.as_ref().is_some_and(|tags| !tags.is_empty()) {
            return None;
        }
        let id = node.user_id?;
        self.user
            .as_ref()?
            .get(&id.to_string())
            .map(|profile| profile.login_name.clone())
            .filter(|login| !login.is_empty())
    }
}

/// A `whois` answer: the node and, for an untagged one, its user.
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "PascalCase")]
pub struct WhoIs {
    pub node: WhoIsNode,
    #[serde(default)]
    pub user_profile: Option<UserProfile>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "PascalCase")]
pub struct WhoIsNode {
    /// The stable node id (`nXXXXCNTRL`), as `status` names `Self`.
    #[serde(rename = "StableID", default)]
    pub stable_id: String,
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub tags: Option<Vec<String>>,
    /// The user who shared this node in from another tailnet; absent or 0
    /// for a node of this tailnet.
    #[serde(default)]
    pub sharer: Option<i64>,
}

impl WhoIs {
    /// Shared in from another tailnet: its tags are that tailnet's, not
    /// this one's.
    pub fn shared_in(&self) -> bool {
        self.node.sharer.is_some_and(|sharer| sharer != 0)
    }

    pub fn tags(&self) -> &[String] {
        self.node.tags.as_deref().unwrap_or_default()
    }

    /// The user's login, for a device signed in as a user.
    pub fn login(&self) -> Option<&str> {
        if !self.tags().is_empty() {
            return None;
        }
        self.user_profile
            .as_ref()
            .map(|profile| profile.login_name.as_str())
            .filter(|login| !login.is_empty())
    }
}

fn is_false(value: &bool) -> bool {
    !*value
}

/// `ipn.ServeConfig`. What it does not model is kept in `rest` and written
/// back as it was.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct ServeConfig {
    #[serde(rename = "TCP", default, skip_serializing_if = "BTreeMap::is_empty")]
    pub tcp: BTreeMap<String, TcpPortHandler>,
    #[serde(rename = "Web", default, skip_serializing_if = "BTreeMap::is_empty")]
    pub web: BTreeMap<String, WebServerConfig>,
    #[serde(
        rename = "Services",
        default,
        skip_serializing_if = "BTreeMap::is_empty"
    )]
    pub services: BTreeMap<String, ServiceConfig>,
    #[serde(
        rename = "AllowFunnel",
        default,
        skip_serializing_if = "BTreeMap::is_empty"
    )]
    pub allow_funnel: BTreeMap<String, bool>,
    #[serde(flatten)]
    pub rest: Map<String, Value>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct TcpPortHandler {
    #[serde(rename = "HTTPS", default, skip_serializing_if = "is_false")]
    pub https: bool,
    #[serde(rename = "HTTP", default, skip_serializing_if = "is_false")]
    pub http: bool,
    #[serde(
        rename = "TCPForward",
        default,
        skip_serializing_if = "String::is_empty"
    )]
    pub tcp_forward: String,
    #[serde(flatten)]
    pub rest: Map<String, Value>,
}

impl TcpPortHandler {
    fn https_only() -> Self {
        Self {
            https: true,
            ..Self::default()
        }
    }

    /// Exactly the handler the agent adds: HTTPS, nothing else.
    fn is_plain_https(&self) -> bool {
        self.https
            && !self.http
            && self.tcp_forward.is_empty()
            && self
                .rest
                .values()
                .all(|v| v.is_null() || v == &Value::Bool(false) || v == &Value::from(0))
    }
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct WebServerConfig {
    #[serde(rename = "Handlers", default)]
    pub handlers: BTreeMap<String, HttpHandler>,
    #[serde(flatten)]
    pub rest: Map<String, Value>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct HttpHandler {
    #[serde(rename = "Path", default, skip_serializing_if = "String::is_empty")]
    pub path: String,
    #[serde(rename = "Proxy", default, skip_serializing_if = "String::is_empty")]
    pub proxy: String,
    #[serde(rename = "Text", default, skip_serializing_if = "String::is_empty")]
    pub text: String,
    #[serde(flatten)]
    pub rest: Map<String, Value>,
}

impl HttpHandler {
    /// What it serves, in words, for a conflict message.
    pub fn describe(&self) -> String {
        if !self.proxy.is_empty() {
            self.proxy.clone()
        } else if !self.path.is_empty() {
            format!("files from {}", self.path)
        } else if !self.text.is_empty() {
            "a text reply".to_string()
        } else {
            "a handler".to_string()
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct ServiceConfig {
    #[serde(rename = "TCP", default, skip_serializing_if = "BTreeMap::is_empty")]
    pub tcp: BTreeMap<String, TcpPortHandler>,
    #[serde(rename = "Web", default, skip_serializing_if = "BTreeMap::is_empty")]
    pub web: BTreeMap<String, WebServerConfig>,
    #[serde(rename = "Tun", default, skip_serializing_if = "is_false")]
    pub tun: bool,
    #[serde(flatten)]
    pub rest: Map<String, Value>,
}

/// The parts of `ipn.Prefs` the agent edits.
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "PascalCase")]
pub struct Prefs {
    #[serde(default)]
    pub advertise_services: Option<Vec<String>>,
}

/// What one declared entry asks the Serve config for.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EntrySpec {
    /// The name visitors open, `master.example.ts.net` or
    /// `myapp.example.ts.net`.
    pub hostname: String,
    /// `svc:myapp` for a Service, `None` for the machine name.
    pub service: Option<String>,
    /// `unix:/run/temps/serve/custom-12.sock`.
    pub target: String,
    /// The directory the agent's sockets live in: a handler pointing at a
    /// socket there is the agent's own, anything else is foreign.
    pub socket_dir: String,
}

impl EntrySpec {
    fn owns(&self, target: &str) -> bool {
        is_owned_target_in(&self.socket_dir, target)
    }

    fn host_port(&self) -> String {
        format!("{}:{}", self.hostname, SERVE_HTTPS_PORT)
    }
}

/// What reading the config said about one entry.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EntryState {
    /// The agent's handler is there, pointing at the entry's socket.
    Present { funnel: bool },
    /// Nothing holds the slot.
    Free,
    /// Something the agent did not write holds it, in words.
    Taken(String),
}

fn port_key() -> String {
    SERVE_HTTPS_PORT.to_string()
}

/// One owner's port handlers and web handlers in a Serve config.
type ServeMaps<'a> = (
    &'a BTreeMap<String, TcpPortHandler>,
    &'a BTreeMap<String, WebServerConfig>,
);

/// Where the entry lives in the config: the machine's own maps, or its
/// Service's.
fn maps<'a>(config: &'a ServeConfig, service: Option<&str>) -> Option<ServeMaps<'a>> {
    match service {
        None => Some((&config.tcp, &config.web)),
        Some(name) => config.services.get(name).map(|s| (&s.tcp, &s.web)),
    }
}

/// Read what the config holds for `spec`'s slot (`<name>:443`).
pub fn entry_state(config: &ServeConfig, spec: &EntrySpec) -> EntryState {
    let funnel = config
        .allow_funnel
        .get(&spec.host_port())
        .copied()
        .unwrap_or(false);
    let Some((tcp, web)) = maps(config, spec.service.as_deref()) else {
        return EntryState::Free;
    };
    if let Some(handler) = tcp.get(&port_key()) {
        if !handler.tcp_forward.is_empty() {
            return EntryState::Taken(format!("a TCP forward to {}", handler.tcp_forward));
        }
        if handler.http && !handler.https {
            return EntryState::Taken("plain HTTP on port 443".to_string());
        }
    }
    let Some(site) = web.get(&spec.host_port()) else {
        return EntryState::Free;
    };
    if let Some((_, foreign)) = site
        .handlers
        .iter()
        .find(|(_, handler)| !spec.owns(&handler.proxy))
    {
        return EntryState::Taken(foreign.describe());
    }
    match site.handlers.get("/") {
        Some(handler) if handler.proxy == spec.target && site.handlers.len() == 1 => {
            EntryState::Present { funnel }
        }
        Some(handler) => EntryState::Taken(format!(
            "another Temps address's socket {}",
            handler.proxy.trim_start_matches("unix:")
        )),
        None if site.handlers.is_empty() => EntryState::Free,
        None => EntryState::Taken("handlers on paths other than /".to_string()),
    }
}

/// Add the agent's handler for `spec`. Only call on [`EntryState::Free`].
pub fn add_entry(config: &mut ServeConfig, spec: &EntrySpec) {
    let (tcp, web) = match &spec.service {
        None => (&mut config.tcp, &mut config.web),
        Some(name) => {
            let service = config.services.entry(name.clone()).or_default();
            (&mut service.tcp, &mut service.web)
        }
    };
    tcp.entry(port_key())
        .or_insert_with(TcpPortHandler::https_only)
        .https = true;
    let site = web.entry(spec.host_port()).or_default();
    site.handlers.insert(
        "/".to_string(),
        HttpHandler {
            proxy: spec.target.clone(),
            ..HttpHandler::default()
        },
    );
}

/// Remove the agent's handler for `spec`, and only that: a handler whose
/// target is `spec.target`. Returns whether one was there. The port's
/// `HTTPS` handler goes with the last site on it, when it is exactly what
/// the agent adds; an emptied Service goes too.
pub fn remove_entry(config: &mut ServeConfig, spec: &EntrySpec) -> bool {
    let (tcp, web) = match &spec.service {
        None => (&mut config.tcp, &mut config.web),
        Some(name) => match config.services.get_mut(name) {
            Some(service) => (&mut service.tcp, &mut service.web),
            None => return false,
        },
    };
    let key = spec.host_port();
    let Some(site) = web.get_mut(&key) else {
        return false;
    };
    let before = site.handlers.len();
    site.handlers
        .retain(|_, handler| handler.proxy != spec.target || !spec.owns(&handler.proxy));
    let removed = site.handlers.len() != before;
    if site.handlers.is_empty() && site.rest.is_empty() {
        web.remove(&key);
    }
    let suffix = format!(":{}", SERVE_HTTPS_PORT);
    if removed
        && !web.keys().any(|k| k.ends_with(&suffix))
        && tcp
            .get(&port_key())
            .is_some_and(TcpPortHandler::is_plain_https)
    {
        tcp.remove(&port_key());
    }
    if let Some(name) = &spec.service {
        if config.services.get(name).is_some_and(|service| {
            service.tcp.is_empty()
                && service.web.is_empty()
                && !service.tun
                && service.rest.is_empty()
        }) {
            config.services.remove(name);
        }
    }
    removed
}

/// Whether any handler is left for `service`.
pub fn service_has_handlers(config: &ServeConfig, service: &str) -> bool {
    config
        .services
        .get(service)
        .is_some_and(|s| s.web.values().any(|site| !site.handlers.is_empty()) || !s.tcp.is_empty())
}

/// Who holds `<machine>:443` that the agent did not write, in words.
pub fn port_443_holder(config: &ServeConfig, machine: &str, socket_dir: &str) -> Option<String> {
    let probe = EntrySpec {
        hostname: machine.to_string(),
        service: None,
        target: String::new(),
        socket_dir: socket_dir.to_string(),
    };
    match entry_state(config, &probe) {
        EntryState::Taken(what) if !what.starts_with("another Temps address") => Some(what),
        _ => None,
    }
}

/// One HTTP/1.1 request over a unix socket, bounded in time and size.
pub(crate) async fn unix_call(
    socket: &std::path::Path,
    call: &'static str,
    method: http::Method,
    path_and_query: &str,
    host: &str,
    headers: &[(&str, &str)],
    body: Option<Vec<u8>>,
) -> Result<(http::StatusCode, http::HeaderMap, Bytes), LocalApiError> {
    let transport = |reason: String| LocalApiError::Transport { call, reason };
    let run = async {
        let stream = match tokio::net::UnixStream::connect(socket).await {
            Ok(stream) => stream,
            Err(error)
                if matches!(
                    error.kind(),
                    std::io::ErrorKind::NotFound | std::io::ErrorKind::ConnectionRefused
                ) =>
            {
                return Err(LocalApiError::NoSocket {
                    path: socket.display().to_string(),
                })
            }
            Err(error) => return Err(transport(error.to_string())),
        };
        let (mut sender, connection) =
            hyper::client::conn::http1::handshake(hyper_util::rt::TokioIo::new(stream))
                .await
                .map_err(|e| transport(e.to_string()))?;
        let driver = tokio::spawn(connection);
        let mut request = http::Request::builder()
            .method(method)
            .uri(path_and_query)
            .header(http::header::HOST, host);
        for (name, value) in headers {
            request = request.header(*name, *value);
        }
        if body.is_some() {
            request = request.header(http::header::CONTENT_TYPE, "application/json");
        }
        let request = request
            .body(Full::new(Bytes::from(body.unwrap_or_default())))
            .map_err(|e| transport(e.to_string()))?;
        let response = sender
            .send_request(request)
            .await
            .map_err(|e| transport(e.to_string()))?;
        let status = response.status();
        let headers = response.headers().clone();
        let bytes = Limited::new(response.into_body(), MAX_BODY)
            .collect()
            .await
            .map_err(|e| transport(e.to_string()))?
            .to_bytes();
        driver.abort();
        Ok((status, headers, bytes))
    };
    tokio::time::timeout(CALL_TIMEOUT, run)
        .await
        .map_err(|_| transport(format!("no answer within {}s", CALL_TIMEOUT.as_secs())))?
}

/// The LocalAPI calls the agent makes.
#[async_trait]
pub trait LocalApi: Send + Sync {
    async fn status(&self) -> Result<Status, LocalApiError>;
    /// The Serve config and its `Etag`.
    async fn serve_config(&self) -> Result<(ServeConfig, String), LocalApiError>;
    /// Write the config if it is still at `etag`. `Ok(false)` on `412`: it
    /// changed since, re-read and re-apply.
    async fn set_serve_config(
        &self,
        config: &ServeConfig,
        etag: &str,
    ) -> Result<bool, LocalApiError>;
    async fn prefs(&self) -> Result<Prefs, LocalApiError>;
    async fn set_advertise_services(&self, services: &[String]) -> Result<(), LocalApiError>;
    /// The node at `addr` (`ip` or `ip:port`); `None` when tailscaled does
    /// not know it.
    async fn whois(&self, addr: &str) -> Result<Option<WhoIs>, LocalApiError>;
}

/// The LocalAPI on a unix socket.
pub struct UnixLocalApi {
    socket: PathBuf,
}

impl UnixLocalApi {
    pub fn new(socket: impl Into<PathBuf>) -> Self {
        Self {
            socket: socket.into(),
        }
    }

    async fn call(
        &self,
        call: &'static str,
        method: http::Method,
        path_and_query: &str,
        headers: &[(&str, &str)],
        body: Option<Vec<u8>>,
    ) -> Result<(http::StatusCode, http::HeaderMap, Bytes), LocalApiError> {
        // What tailscale's own client sends; the LocalAPI checks it.
        let mut all = vec![("Sec-Tailscale", "localapi")];
        all.extend_from_slice(headers);
        unix_call(
            &self.socket,
            call,
            method,
            path_and_query,
            "local-tailscaled.sock",
            &all,
            body,
        )
        .await
    }

    fn refused(call: &'static str, status: http::StatusCode, body: &Bytes) -> LocalApiError {
        LocalApiError::Status {
            call,
            status: status.as_u16(),
            body: String::from_utf8_lossy(&body[..body.len().min(512)])
                .trim()
                .to_string(),
        }
    }

    fn parse<T: serde::de::DeserializeOwned>(
        call: &'static str,
        body: &Bytes,
    ) -> Result<T, LocalApiError> {
        serde_json::from_slice(body).map_err(|e| LocalApiError::NotUnderstood {
            call,
            reason: e.to_string(),
        })
    }
}

#[async_trait]
impl LocalApi for UnixLocalApi {
    async fn status(&self) -> Result<Status, LocalApiError> {
        let call = "status";
        let (status, _, body) = self
            .call(
                call,
                http::Method::GET,
                "/localapi/v0/status?peers=false",
                &[],
                None,
            )
            .await?;
        if !status.is_success() {
            return Err(Self::refused(call, status, &body));
        }
        Self::parse(call, &body)
    }

    async fn serve_config(&self) -> Result<(ServeConfig, String), LocalApiError> {
        let call = "serve-config";
        let (status, headers, body) = self
            .call(
                call,
                http::Method::GET,
                "/localapi/v0/serve-config",
                &[],
                None,
            )
            .await?;
        if !status.is_success() {
            return Err(Self::refused(call, status, &body));
        }
        let etag = headers
            .get(http::header::ETAG)
            .and_then(|v| v.to_str().ok())
            .map(str::to_string)
            .ok_or(LocalApiError::NotUnderstood {
                call,
                reason: "no Etag on the Serve config".to_string(),
            })?;
        // An empty config is `null` or `{}`.
        let config = if body.iter().all(u8::is_ascii_whitespace) || &body[..] == b"null" {
            ServeConfig::default()
        } else {
            Self::parse(call, &body)?
        };
        Ok((config, etag))
    }

    async fn set_serve_config(
        &self,
        config: &ServeConfig,
        etag: &str,
    ) -> Result<bool, LocalApiError> {
        let call = "serve-config write";
        let body = serde_json::to_vec(config).map_err(|e| LocalApiError::Transport {
            call,
            reason: e.to_string(),
        })?;
        let (status, _, answer) = self
            .call(
                call,
                http::Method::POST,
                "/localapi/v0/serve-config",
                &[("If-Match", etag)],
                Some(body),
            )
            .await?;
        if status == http::StatusCode::PRECONDITION_FAILED {
            return Ok(false);
        }
        if !status.is_success() {
            return Err(Self::refused(call, status, &answer));
        }
        Ok(true)
    }

    async fn prefs(&self) -> Result<Prefs, LocalApiError> {
        let call = "prefs";
        let (status, _, body) = self
            .call(call, http::Method::GET, "/localapi/v0/prefs", &[], None)
            .await?;
        if !status.is_success() {
            return Err(Self::refused(call, status, &body));
        }
        Self::parse(call, &body)
    }

    async fn set_advertise_services(&self, services: &[String]) -> Result<(), LocalApiError> {
        let call = "prefs write";
        let body = serde_json::to_vec(&serde_json::json!({
            "AdvertiseServicesSet": true,
            "AdvertiseServices": services,
        }))
        .map_err(|e| LocalApiError::Transport {
            call,
            reason: e.to_string(),
        })?;
        let (status, _, answer) = self
            .call(
                call,
                http::Method::PATCH,
                "/localapi/v0/prefs",
                &[],
                Some(body),
            )
            .await?;
        if !status.is_success() {
            return Err(Self::refused(call, status, &answer));
        }
        Ok(())
    }

    async fn whois(&self, addr: &str) -> Result<Option<WhoIs>, LocalApiError> {
        let call = "whois";
        // Only an address goes into the query: nothing to escape.
        if !addr
            .bytes()
            .all(|b| b.is_ascii_hexdigit() || matches!(b, b'.' | b':' | b'[' | b']'))
        {
            return Ok(None);
        }
        let (status, _, body) = self
            .call(
                call,
                http::Method::GET,
                &format!("/localapi/v0/whois?addr={addr}"),
                &[],
                None,
            )
            .await?;
        if status == http::StatusCode::NOT_FOUND {
            return Ok(None);
        }
        if !status.is_success() {
            return Err(Self::refused(call, status, &body));
        }
        Self::parse(call, &body).map(Some)
    }
}

#[cfg(test)]
pub(crate) mod fake {
    //! A fake tailscaled: a LocalAPI on a unix socket in a temp directory,
    //! answering like tailscaled v1.104 (recorded shapes), with an
    //! `If-Match` check on Serve config writes.

    use super::*;
    use axum::extract::{Query, State};
    use axum::http::{HeaderMap, StatusCode};
    use axum::response::IntoResponse;
    use std::sync::Arc;

    #[derive(Default)]
    pub struct FakeState {
        pub status: Value,
        pub config: Value,
        pub version: u64,
        pub prefs: Value,
        pub whois: BTreeMap<String, Value>,
        /// Writes refused with 412 before one is accepted.
        pub lose_races: u32,
        pub writes: u32,
        /// Answer every call with this body instead.
        pub garbage: Option<String>,
        /// What another writer sets `AdvertiseServices` to right after each
        /// prefs write, one per write, oldest first.
        pub advertise_after_write: std::collections::VecDeque<Value>,
        pub prefs_writes: u32,
        /// When set, reading the Serve config answers with this status.
        pub config_status: Option<u16>,
        /// When set, whois answers only once this is notified.
        pub whois_gate: Option<Arc<tokio::sync::Notify>>,
    }

    pub type Shared = Arc<parking_lot::Mutex<FakeState>>;

    pub fn etag(state: &FakeState) -> String {
        format!("\"v{}\"", state.version)
    }

    pub fn recorded_status() -> Value {
        serde_json::json!({
            "Version": "1.104.2-t0123456789-gabcdef012",
            "TUN": true,
            "BackendState": "Running",
            "HaveNodeKey": true,
            "Self": {
                "ID": "n123CNTRL",
                "HostName": "master",
                "DNSName": "master.example.ts.net.",
                "OS": "linux",
                "UserID": 2,
                "TailscaleIPs": ["100.101.102.103", "fd7a:115c:a1e0::5"],
                "Tags": ["tag:temps"],
                "Online": true
            },
            "MagicDNSSuffix": "example.ts.net",
            "CurrentTailnet": {
                "Name": "example.com",
                "MagicDNSSuffix": "example.ts.net",
                "MagicDNSEnabled": true
            },
            "CertDomains": ["master.example.ts.net"],
            "User": {"2": {"ID": 2, "LoginName": "tagged-devices", "DisplayName": "Tagged Devices"}}
        })
    }

    async fn status(State(state): State<Shared>) -> axum::response::Response {
        let state = state.lock();
        if let Some(garbage) = &state.garbage {
            return garbage.clone().into_response();
        }
        axum::Json(state.status.clone()).into_response()
    }

    async fn get_config(State(state): State<Shared>) -> axum::response::Response {
        let state = state.lock();
        if let Some(garbage) = &state.garbage {
            return garbage.clone().into_response();
        }
        if let Some(status) = state
            .config_status
            .and_then(|s| StatusCode::from_u16(s).ok())
        {
            return (status, "serve config unavailable").into_response();
        }
        let mut response = axum::Json(state.config.clone()).into_response();
        if let Ok(value) = etag(&state).parse() {
            response.headers_mut().insert(http::header::ETAG, value);
        }
        response
    }

    async fn set_config(
        State(state): State<Shared>,
        headers: HeaderMap,
        body: String,
    ) -> axum::response::Response {
        let mut state = state.lock();
        let matches = headers
            .get("if-match")
            .and_then(|v| v.to_str().ok())
            .is_some_and(|v| v == etag(&state));
        if state.lose_races > 0 {
            state.lose_races -= 1;
            // Someone else wrote in between.
            state.version += 1;
            return (StatusCode::PRECONDITION_FAILED, "etag mismatch").into_response();
        }
        if !matches {
            return (StatusCode::PRECONDITION_FAILED, "etag mismatch").into_response();
        }
        match serde_json::from_str::<Value>(&body) {
            Ok(value) => {
                state.config = value;
                state.version += 1;
                state.writes += 1;
                StatusCode::OK.into_response()
            }
            Err(error) => (StatusCode::BAD_REQUEST, error.to_string()).into_response(),
        }
    }

    async fn get_prefs(State(state): State<Shared>) -> axum::response::Response {
        axum::Json(state.lock().prefs.clone()).into_response()
    }

    async fn patch_prefs(
        State(state): State<Shared>,
        axum::Json(body): axum::Json<Value>,
    ) -> axum::response::Response {
        let mut state = state.lock();
        if body["AdvertiseServicesSet"] == Value::Bool(true) {
            state.prefs["AdvertiseServices"] = body["AdvertiseServices"].clone();
            state.prefs_writes += 1;
        }
        let answer = axum::Json(state.prefs.clone()).into_response();
        if let Some(other) = state.advertise_after_write.pop_front() {
            state.prefs["AdvertiseServices"] = other;
        }
        answer
    }

    async fn whois(
        State(state): State<Shared>,
        Query(query): Query<BTreeMap<String, String>>,
    ) -> axum::response::Response {
        let addr = query.get("addr").cloned().unwrap_or_default();
        let ip = addr
            .rsplit_once(':')
            .filter(|(host, _)| !host.contains(':'))
            .map(|(host, _)| host.to_string())
            .unwrap_or(addr);
        let (answer, gate) = {
            let state = state.lock();
            (state.whois.get(&ip).cloned(), state.whois_gate.clone())
        };
        if let Some(gate) = gate {
            gate.notified().await;
        }
        match answer {
            Some(answer) => axum::Json(answer).into_response(),
            None => (StatusCode::NOT_FOUND, "no match for IP:port").into_response(),
        }
    }

    /// Start the fake on `<dir>/tailscaled.sock`.
    pub async fn start(dir: &std::path::Path, state: FakeState) -> (PathBuf, Shared) {
        let path = dir.join("tailscaled.sock");
        let shared: Shared = Arc::new(parking_lot::Mutex::new(state));
        let app = axum::Router::new()
            .route("/localapi/v0/status", axum::routing::get(status))
            .route(
                "/localapi/v0/serve-config",
                axum::routing::get(get_config).post(set_config),
            )
            .route(
                "/localapi/v0/prefs",
                axum::routing::get(get_prefs).patch(patch_prefs),
            )
            .route("/localapi/v0/whois", axum::routing::get(whois))
            .with_state(shared.clone());
        let listener = tokio::net::UnixListener::bind(&path).expect("bind fake tailscaled");
        tokio::spawn(async move {
            let _ = axum::serve(listener, app).await;
        });
        (path, shared)
    }

    pub fn running() -> FakeState {
        FakeState {
            status: recorded_status(),
            config: Value::Null,
            prefs: serde_json::json!({"ControlURL": "https://controlplane.tailscale.com", "AdvertiseServices": null}),
            ..FakeState::default()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use temps_core::tailnet::SERVE_SOCKET_DIR;

    fn machine(target: &str) -> EntrySpec {
        EntrySpec {
            hostname: "master.example.ts.net".into(),
            service: None,
            target: target.into(),
            socket_dir: SERVE_SOCKET_DIR.into(),
        }
    }

    const OURS: &str = "unix:/run/temps/serve/custom-12.sock";

    #[test]
    fn a_recorded_status_parses_into_what_temps_reads() {
        let status: Status = serde_json::from_value(fake::recorded_status()).unwrap();
        assert_eq!(status.backend_state, "Running");
        assert!(temps_core::tailnet::version_supported(&status.version));
        let me = status.self_node.as_ref().unwrap();
        assert_eq!(me.dns_name, "master.example.ts.net.");
        assert_eq!(me.tags.as_deref(), Some(&["tag:temps".to_string()][..]));
        assert!(status.current_tailnet.as_ref().unwrap().magic_dns_enabled);
        // Tagged: no user login.
        assert_eq!(status.login(), None);
    }

    #[test]
    fn unknown_fields_are_written_back_unchanged() {
        let recorded = serde_json::json!({
            "TCP": {"443": {"HTTPS": true}, "8443": {"TCPForward": "127.0.0.1:22", "ProxyProtocol": 1}},
            "Web": {"other.example.ts.net:443": {"Handlers": {"/": {"Proxy": "http://127.0.0.1:3000", "AcceptAppCaps": ["example.com/cap/x"]}}}},
            "AllowFunnel": {"other.example.ts.net:443": true},
            "Foreground": {"session": {"TCP": {"80": {"HTTP": true}}}},
            "SomethingNew": {"x": 1}
        });
        let mut config: ServeConfig = serde_json::from_value(recorded.clone()).unwrap();
        assert_eq!(serde_json::to_value(&config).unwrap(), recorded);
        add_entry(&mut config, &machine(OURS));
        assert!(remove_entry(&mut config, &machine(OURS)));
        // Port 443 still has another site on it: its HTTPS handler stays.
        assert_eq!(serde_json::to_value(&config).unwrap(), recorded);
    }

    #[test]
    fn the_slot_is_read_as_free_ours_or_someone_elses() {
        let mut config = ServeConfig::default();
        assert_eq!(entry_state(&config, &machine(OURS)), EntryState::Free);
        add_entry(&mut config, &machine(OURS));
        assert_eq!(
            entry_state(&config, &machine(OURS)),
            EntryState::Present { funnel: false }
        );
        assert_eq!(
            serde_json::to_value(&config).unwrap(),
            serde_json::json!({
                "TCP": {"443": {"HTTPS": true}},
                "Web": {"master.example.ts.net:443": {"Handlers": {"/": {"Proxy": OURS}}}}
            })
        );
        // Funnel turned on by hand on the agent's own entry.
        config
            .allow_funnel
            .insert("master.example.ts.net:443".into(), true);
        assert_eq!(
            entry_state(&config, &machine(OURS)),
            EntryState::Present { funnel: true }
        );

        let foreign: ServeConfig = serde_json::from_value(serde_json::json!({
            "TCP": {"443": {"HTTPS": true}},
            "Web": {"master.example.ts.net:443": {"Handlers": {"/": {"Proxy": "http://127.0.0.1:3000"}}}}
        }))
        .unwrap();
        assert_eq!(
            entry_state(&foreign, &machine(OURS)),
            EntryState::Taken("http://127.0.0.1:3000".into())
        );
        assert_eq!(
            port_443_holder(&foreign, "master.example.ts.net", SERVE_SOCKET_DIR).as_deref(),
            Some("http://127.0.0.1:3000")
        );
        let forward: ServeConfig = serde_json::from_value(
            serde_json::json!({"TCP": {"443": {"TCPForward": "127.0.0.1:8443"}}}),
        )
        .unwrap();
        assert!(matches!(
            entry_state(&forward, &machine(OURS)),
            EntryState::Taken(what) if what.contains("TCP forward")
        ));
        // Another of Temps' sockets on the same name.
        let mut other = ServeConfig::default();
        add_entry(&mut other, &machine("unix:/run/temps/serve/custom-9.sock"));
        assert!(matches!(
            entry_state(&other, &machine(OURS)),
            EntryState::Taken(what) if what.starts_with("another Temps address")
        ));
    }

    #[test]
    fn removing_never_touches_a_handler_the_agent_did_not_write() {
        let mut config: ServeConfig = serde_json::from_value(serde_json::json!({
            "TCP": {"443": {"HTTPS": true}},
            "Web": {"master.example.ts.net:443": {"Handlers": {
                "/": {"Proxy": "http://127.0.0.1:3000"}
            }}}
        }))
        .unwrap();
        let before = config.clone();
        // Even a spec naming the foreign target is not the agent's.
        assert!(!remove_entry(
            &mut config,
            &machine("http://127.0.0.1:3000")
        ));
        assert!(!remove_entry(&mut config, &machine(OURS)));
        assert_eq!(config, before);
    }

    #[test]
    fn a_service_entry_lives_under_its_service_and_goes_with_it() {
        let spec = EntrySpec {
            hostname: "myapp.example.ts.net".into(),
            service: Some("svc:myapp".into()),
            target: OURS.into(),
            socket_dir: SERVE_SOCKET_DIR.into(),
        };
        let mut config = ServeConfig::default();
        assert_eq!(entry_state(&config, &spec), EntryState::Free);
        add_entry(&mut config, &spec);
        assert_eq!(
            serde_json::to_value(&config).unwrap(),
            serde_json::json!({"Services": {"svc:myapp": {
                "TCP": {"443": {"HTTPS": true}},
                "Web": {"myapp.example.ts.net:443": {"Handlers": {"/": {"Proxy": OURS}}}}
            }}})
        );
        assert!(service_has_handlers(&config, "svc:myapp"));
        assert!(remove_entry(&mut config, &spec));
        assert_eq!(config, ServeConfig::default());
        assert!(!service_has_handlers(&config, "svc:myapp"));
    }

    #[tokio::test]
    async fn the_client_reads_and_writes_conditionally() {
        let dir = tempfile::tempdir().unwrap();
        let (socket, state) = fake::start(dir.path(), fake::running()).await;
        let api = UnixLocalApi::new(&socket);

        let status = api.status().await.unwrap();
        assert_eq!(status.backend_state, "Running");

        let (mut config, etag) = api.serve_config().await.unwrap();
        assert_eq!(config, ServeConfig::default());
        add_entry(&mut config, &machine(OURS));
        assert!(api.set_serve_config(&config, &etag).await.unwrap());
        // The old Etag is stale now: a second write loses.
        assert!(!api.set_serve_config(&config, &etag).await.unwrap());
        let (read, _) = api.serve_config().await.unwrap();
        assert_eq!(read, config);
        assert_eq!(state.lock().writes, 1);

        api.set_advertise_services(&["svc:myapp".into()])
            .await
            .unwrap();
        assert_eq!(
            api.prefs().await.unwrap().advertise_services,
            Some(vec!["svc:myapp".to_string()])
        );

        state.lock().whois.insert(
            "100.64.0.7".into(),
            serde_json::json!({
                "Node": {"Name": "ci.example.ts.net.", "Tags": ["tag:ci"]},
                "UserProfile": {"LoginName": "tagged-devices"}
            }),
        );
        let who = api.whois("100.64.0.7:41234").await.unwrap().unwrap();
        assert_eq!(who.tags(), ["tag:ci".to_string()]);
        assert_eq!(who.login(), None);
        assert_eq!(api.whois("100.64.0.8").await.unwrap(), None);
        // Nothing but an address ever reaches the query string.
        assert_eq!(api.whois("1.2.3.4&x=y").await.unwrap(), None);
    }

    #[tokio::test]
    async fn an_answer_it_does_not_understand_is_not_guessed_at() {
        let dir = tempfile::tempdir().unwrap();
        let mut state = fake::running();
        state.garbage = Some("<html>not json</html>".into());
        let (socket, _) = fake::start(dir.path(), state).await;
        let api = UnixLocalApi::new(&socket);
        assert!(matches!(
            api.status().await,
            Err(LocalApiError::NotUnderstood { call: "status", .. })
        ));
        // No Etag is not understood either: a write could not be conditional.
        assert!(matches!(
            api.serve_config().await,
            Err(LocalApiError::NotUnderstood { .. })
        ));
    }

    #[tokio::test]
    async fn no_socket_is_its_own_error() {
        let dir = tempfile::tempdir().unwrap();
        let api = UnixLocalApi::new(dir.path().join("missing.sock"));
        assert!(matches!(
            api.status().await,
            Err(LocalApiError::NoSocket { .. })
        ));
    }
}
