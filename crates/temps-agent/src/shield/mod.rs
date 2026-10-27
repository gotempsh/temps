// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Shield on the worker's public ingress (ADR 066, D10, I5 and I8).
//!
//! A worker that declares `shield-v1` gets the routes whose environment
//! acts on verdicts, each with its Shield settings, and makes the same
//! decision the control plane does: one [`ShieldEngine::decide`] with the
//! same keys, so a clearance either side issues is good on the other. It
//! serves the challenge page, the solver and the verify endpoint itself, so
//! clearing a visitor never needs the control plane.
//!
//! [`WorkerShield`] holds what a request reads, all behind `ArcSwap`: the
//! snapshot's [`Index`], the engine (absent until the keys are opened), the
//! node's own lists (`local`, empty until netwatch is wired in) and each
//! route's [`EnvPolicy`]. [`sync`] keeps the snapshot and keys current and
//! on disk; [`uploader`] posts events to the control plane.
//!
//! Without a policy for the host (a control plane that predates Shield on
//! workers, or a route sent without one) nothing here runs and the request
//! is served as before.

pub mod sync;
pub mod uploader;

use std::collections::HashMap;
use std::net::IpAddr;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, OnceLock};
use std::time::Duration;

use arc_swap::{ArcSwap, ArcSwapOption};
use axum::body::Body;
use axum::extract::Request;
use axum::http::{header, HeaderName, HeaderValue, Method, StatusCode};
use axum::response::{IntoResponse, Response};
use base64::engine::general_purpose::STANDARD as BASE64;
use base64::Engine;
use serde::Serialize;
use temps_core::worker_shield::{
    SealedShieldKeys, ShieldKeyMaterial, ShieldRef, ShieldRoutePolicy, ShieldSnapshotResponse,
    SHIELD_KEYS_INFO,
};
use temps_shield::crawler_check::CrawlerVerifier;
use temps_shield::crawlers::Crawler;
pub use temps_shield::netwatch::NetwatchConfig;
use temps_shield::netwatch::{RequestLine, RequestSender};
use temps_shield::signatures::MATCHED_HEADERS;
use temps_shield::{
    assets, bypass, events, Action, ChallengeGate, Decision, EnvPolicy, EventKind, EventSink,
    ExemptPaths, Index, Outcome, ShieldEngine, ShieldEvent, ShieldKeys, ShieldMode, ShieldSnapshot,
    VerifyOutcome,
};
use thiserror::Error;
use tracing::{debug, info, warn};

/// Where the last good Shield snapshot is kept, next to `routes.json`.
pub const SNAPSHOT_FILE: &str = "shield.json";

/// How long a claimed crawler's address may take to verify before the
/// visitor is challenged like anyone else.
const CRAWLER_CHECK_TIMEOUT: Duration = Duration::from_millis(1500);

/// `Retry-After` for a challenge a request cannot show (D7).
const TOO_MANY_RETRY_AFTER_SECS: &str = "30";
/// `Retry-After` while the challenge cannot be issued.
const UNAVAILABLE_RETRY_AFTER_SECS: &str = "5";

/// The mode a route's settings name. A mode this agent does not know is
/// taken as `selective`: an unknown mode never serves a route open.
pub fn policy_mode(policy: &ShieldRoutePolicy) -> ShieldMode {
    ShieldMode::parse(&policy.mode).unwrap_or(ShieldMode::Selective)
}

/// A route's settings as the engine reads them. Exempt paths that do not
/// parse are dropped rather than exempting more than they say.
pub fn env_policy(policy: &ShieldRoutePolicy) -> Arc<EnvPolicy> {
    Arc::new(EnvPolicy {
        project_id: policy.project_id,
        environment_id: policy.environment_id,
        mode: policy_mode(policy),
        exempt_paths: ExemptPaths::lenient(&policy.exempt_paths),
        bypass: bypass::TokenSet::from_hex(&policy.bypass_token_hashes),
    })
}

#[derive(Debug, Error)]
pub enum ShieldLoadError {
    #[error("Shield snapshot revision {revision} is not base64: {reason}")]
    Base64 { revision: u64, reason: String },
    #[error("Shield snapshot revision {revision} has SHA-256 {actual}, not the {expected} it was sent with")]
    Digest {
        revision: u64,
        expected: String,
        actual: String,
    },
    #[error("Shield snapshot revision {revision} does not decode: {reason}")]
    Decode { revision: u64, reason: String },
    #[error("Shield keys do not open with this node's ingress key: {reason}")]
    Keys { reason: String },
}

/// The challenge gate's keys, opened from what the control plane sealed to
/// this node. Every key the gate derives must be there: a control plane on
/// another key generation gives no gate rather than one that disagrees.
pub fn open_keys(
    private_key_b64: &str,
    sealed: &SealedShieldKeys,
) -> Result<ShieldKeys, ShieldLoadError> {
    let keys_error = |reason: String| ShieldLoadError::Keys { reason };
    let plain = temps_core::ecies::decrypt_bundle_with_info(
        private_key_b64,
        &sealed.ephemeral_public_key,
        &temps_core::ecies::EncryptedBundle {
            ciphertext: sealed.ciphertext.clone(),
            nonce: sealed.nonce.clone(),
        },
        SHIELD_KEYS_INFO,
    )
    .map_err(|error| keys_error(error.to_string()))?;
    let material: ShieldKeyMaterial =
        serde_json::from_slice(&plain).map_err(|error| keys_error(error.to_string()))?;
    let mut by_label = HashMap::with_capacity(material.keys.len());
    for entry in material.keys {
        let key: [u8; 32] = BASE64
            .decode(&entry.key)
            .ok()
            .and_then(|bytes| bytes.try_into().ok())
            .ok_or_else(|| keys_error(format!("the key for {} is not 32 bytes", entry.label)))?;
        by_label.insert(entry.label, key);
    }
    let missing = std::cell::RefCell::new(None);
    let keys = ShieldKeys::derive(|label| match by_label.get(label) {
        Some(key) => *key,
        None => {
            missing
                .borrow_mut()
                .get_or_insert_with(|| label.to_string());
            [0; 32]
        }
    });
    match missing.into_inner() {
        Some(label) => Err(keys_error(format!("no key for {label}"))),
        None => Ok(keys),
    }
}

/// The snapshot in a response, checked and decoded. Decoding a large one
/// takes a while; call it off the request threads.
pub fn decode_snapshot(
    response: &ShieldSnapshotResponse,
) -> Result<Option<ShieldSnapshot>, ShieldLoadError> {
    let revision = response.revision;
    let Some(body) = response.body.as_deref() else {
        return Ok(None);
    };
    let body = BASE64
        .decode(body)
        .map_err(|error| ShieldLoadError::Base64 {
            revision,
            reason: error.to_string(),
        })?;
    if let Some(expected) = response.sha256.as_deref() {
        use sha2::Digest;
        let actual = hex::encode(sha2::Sha256::digest(&body));
        if !actual.eq_ignore_ascii_case(expected) {
            return Err(ShieldLoadError::Digest {
                revision,
                expected: expected.to_string(),
                actual,
            });
        }
    }
    ShieldSnapshot::decode(&body)
        .map(Some)
        .map_err(|error| ShieldLoadError::Decode {
            revision,
            reason: error.to_string(),
        })
}

/// Shield's state on this worker.
pub struct WorkerShield {
    index: ArcSwap<Index>,
    engine: ArcSwapOption<ShieldEngine>,
    /// What this node adds on its own (netwatch's bans, D12).
    local: Arc<ArcSwapOption<Index>>,
    /// Each public host's settings; a host without one runs no Shield.
    policies: ArcSwap<HashMap<String, Arc<EnvPolicy>>>,
    /// The revision the route snapshot names, `None` from a control plane
    /// that sends none.
    current: tokio::sync::watch::Sender<Option<ShieldRef>>,
    /// The revision of the loaded snapshot; 0 for the built-in lists.
    revision: AtomicU64,
    sink: OnceLock<Arc<dyn EventSink>>,
    /// Where request lines go when netwatch runs on this node (D12).
    netwatch: OnceLock<RequestSender>,
    crawlers: OnceLock<Option<CrawlerVerifier>>,
    path: PathBuf,
}

impl WorkerShield {
    pub fn new(path: PathBuf) -> Self {
        Self {
            index: ArcSwap::from_pointee(Index::baseline()),
            engine: ArcSwapOption::empty(),
            local: Arc::new(ArcSwapOption::empty()),
            policies: ArcSwap::from_pointee(HashMap::new()),
            current: tokio::sync::watch::Sender::new(None),
            revision: AtomicU64::new(0),
            sink: OnceLock::new(),
            netwatch: OnceLock::new(),
            crawlers: OnceLock::new(),
            path,
        }
    }

    /// This node's own lists: netwatch's bans, empty without netwatch.
    pub fn local(&self) -> Arc<ArcSwapOption<Index>> {
        Arc::clone(&self.local)
    }

    /// Talk to the node's netwatch (D12): its bans join this node's lists
    /// and every request Shield sees is reported to it. Must run inside a
    /// Tokio runtime; a second call is ignored.
    pub fn start_netwatch(&self, config: NetwatchConfig) {
        if self.netwatch.get().is_some() {
            return;
        }
        if config.enabled {
            info!(
                ?config,
                "Shield: reading netwatch bans and reporting requests to it"
            );
        }
        let _ = self
            .netwatch
            .set(temps_shield::netwatch::spawn_into(config, &self.local));
    }

    /// Where events go; set once, by the uploader.
    pub fn set_sink(&self, sink: Arc<dyn EventSink>) {
        if self.sink.set(sink).is_err() {
            warn!("a Shield event sink was already installed; keeping the first");
        }
    }

    pub(crate) fn set_routes(
        &self,
        policies: HashMap<String, Arc<EnvPolicy>>,
        current: Option<ShieldRef>,
    ) {
        self.policies.store(Arc::new(policies));
        self.current.send_if_modified(|value| {
            let changed = *value != current;
            *value = current;
            changed
        });
    }

    /// The settings for `host`, as `extract_host` normalised it.
    pub fn policy(&self, host: &str) -> Option<Arc<EnvPolicy>> {
        self.policies.load().get(host).cloned()
    }

    pub fn revision(&self) -> u64 {
        self.revision.load(Ordering::Acquire)
    }

    pub fn has_engine(&self) -> bool {
        self.engine.load().is_some()
    }

    pub(crate) fn subscribe(&self) -> tokio::sync::watch::Receiver<Option<ShieldRef>> {
        self.current.subscribe()
    }

    pub(crate) fn path(&self) -> &std::path::Path {
        &self.path
    }

    /// Swap in a snapshot's index and, the first time keys open, the
    /// engine. The engine is never replaced once built: the keys do not
    /// change while the master key does not, and a new gate would forget
    /// which tokens were already redeemed.
    pub fn install(&self, revision: u64, index: Index, keys: Option<ShieldKeys>) {
        self.index.store(Arc::new(index));
        self.revision.store(revision, Ordering::Release);
        if let (Some(keys), false) = (keys, self.has_engine()) {
            let gate = Arc::new(ChallengeGate::new(keys));
            self.engine.store(Some(Arc::new(ShieldEngine::new(gate))));
        }
    }

    fn submit(&self, events: Vec<ShieldEvent>) {
        if let Some(sink) = self.sink.get() {
            sink.submit(events);
        }
    }

    async fn verified_crawler(&self, ip: IpAddr, crawler: &'static Crawler) -> bool {
        let verifier = self.crawlers.get_or_init(|| match CrawlerVerifier::system() {
            Ok(verifier) => Some(verifier),
            Err(error) => {
                warn!(%error, "cannot verify crawlers on this worker; claimed crawlers are challenged");
                None
            }
        });
        let Some(verifier) = verifier else {
            return false;
        };
        tokio::time::timeout(CRAWLER_CHECK_TIMEOUT, verifier.verify(ip, crawler))
            .await
            .unwrap_or(false)
    }
}

/// Who the listener says the visitor is: never what the request claims.
#[derive(Debug, Clone, Copy, Default)]
pub(crate) struct Visitor {
    pub ip: Option<IpAddr>,
    pub fingerprint: Option<temps_shield::ja4::ClientFingerprint>,
    /// Whether the browser reached us over TLS (a tunnel origin's provider
    /// ended it).
    pub tls: bool,
}

/// Shield ran for this request.
pub(crate) struct Applied {
    engine: Arc<ShieldEngine>,
    environment_id: i32,
    /// The request's line for netwatch, sent once its status is known.
    netwatch: Option<(RequestSender, RequestLine)>,
}

impl Applied {
    /// Counts the response for the adaptive difficulty (D6) and reports
    /// the request to netwatch (D12).
    pub fn record_status(self, status: StatusCode) {
        self.engine.record_status(
            self.environment_id,
            status.as_u16(),
            temps_shield::unix_now(),
        );
        if let Some((sender, mut line)) = self.netwatch {
            line.status = status.as_u16();
            sender.send(line);
        }
    }
}

/// An RFC 7807 body for what Shield refuses.
#[derive(Serialize)]
struct Problem<'a> {
    #[serde(rename = "type")]
    type_: &'a str,
    title: &'a str,
    status: u16,
    detail: &'a str,
}

fn problem(status: StatusCode, title: &str, detail: &str) -> Response {
    let body = serde_json::to_vec(&Problem {
        type_: "about:blank",
        title,
        status: status.as_u16(),
        detail,
    })
    .unwrap_or_default();
    let mut response = (status, body).into_response();
    let headers = response.headers_mut();
    headers.insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static("application/problem+json"),
    );
    headers.insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    response
}

const HTTPS_REQUIRED_PAGE: &str = r#"<!DOCTYPE html>
<html lang="en">
<head><meta charset="UTF-8"><meta name="viewport" content="width=device-width, initial-scale=1.0"><title>HTTPS Required</title></head>
<body style="font-family: system-ui, -apple-system, sans-serif; text-align: center; padding: 48px;">
<h1>HTTPS Required</h1>
<p>This site checks visitors before serving them, which needs a secure connection.</p>
<p>Please use <strong>https://</strong> instead of http://</p>
</body>
</html>"#;

fn with_header(mut response: Response, name: HeaderName, value: &str) -> Response {
    if let Ok(value) = HeaderValue::from_str(value) {
        response.headers_mut().insert(name, value);
    }
    response
}

/// The response for a decision Shield answers, as the control plane gives
/// it: 403 for a block, the challenge page (403) for a page load, 429 with
/// `Retry-After` for a request that cannot show one, 426 over plain HTTP,
/// 503 when no challenge can be issued.
pub(crate) fn outcome_response(outcome: &Outcome, client_ip: Option<IpAddr>) -> Option<Response> {
    let response = match outcome {
        Outcome::Pass => return None,
        Outcome::Block => problem(
            StatusCode::FORBIDDEN,
            "Blocked",
            "This request was blocked by the site's protection.",
        ),
        Outcome::Challenge {
            token, difficulty, ..
        } => {
            let ip = client_ip.map(|ip| ip.to_string());
            let page =
                assets::challenge_page(token, *difficulty, ip.as_deref().unwrap_or("unknown"));
            let mut response = (StatusCode::FORBIDDEN, page).into_response();
            let headers = response.headers_mut();
            headers.insert(
                header::CONTENT_TYPE,
                HeaderValue::from_static("text/html; charset=utf-8"),
            );
            // The token is bound to this visitor: no cache may keep it.
            headers.insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
            headers.insert(
                HeaderName::from_static("x-challenge-required"),
                HeaderValue::from_static("true"),
            );
            response
        }
        Outcome::TooManyRequests => with_header(
            problem(
                StatusCode::TOO_MANY_REQUESTS,
                "Too Many Requests",
                "This site is checking visitors; load the page in a browser, or retry later.",
            ),
            header::RETRY_AFTER,
            TOO_MANY_RETRY_AFTER_SECS,
        ),
        Outcome::HttpsRequired => {
            let mut response = (StatusCode::UPGRADE_REQUIRED, HTTPS_REQUIRED_PAGE).into_response();
            let headers = response.headers_mut();
            headers.insert(
                header::CONTENT_TYPE,
                HeaderValue::from_static("text/html; charset=utf-8"),
            );
            headers.insert(
                header::UPGRADE,
                HeaderValue::from_static("TLS/1.2, TLS/1.3"),
            );
            headers.insert(header::CONNECTION, HeaderValue::from_static("Upgrade"));
            headers.insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
            response
        }
        Outcome::Unavailable => unavailable(),
    };
    Some(response)
}

fn unavailable() -> Response {
    with_header(
        problem(
            StatusCode::SERVICE_UNAVAILABLE,
            "Service Unavailable",
            "This site's visitor check is not available on this server yet; retry shortly.",
        ),
        header::RETRY_AFTER,
        UNAVAILABLE_RETRY_AFTER_SECS,
    )
}

fn request_id() -> String {
    uuid::Uuid::new_v4().to_string()
}

fn mark(response: &mut Response, verdict: &str, request_id: &str) {
    if let Ok(value) =
        HeaderValue::from_str(&temps_shield::response_header_value(verdict, request_id))
    {
        response.headers_mut().insert(
            HeaderName::from_static(temps_shield::RESPONSE_HEADER),
            value,
        );
    }
}

struct EventContext<'a> {
    policy: &'a EnvPolicy,
    request_id: &'a str,
    ip: Option<IpAddr>,
    ja4: Option<&'a str>,
    host: &'a str,
    path: &'a str,
    user_agent: &'a str,
}

impl EventContext<'_> {
    fn event(&self, kind: EventKind, decision: Option<&Decision>) -> ShieldEvent {
        ShieldEvent {
            timestamp_ms: chrono::Utc::now().timestamp_millis(),
            project_id: self.policy.project_id,
            environment_id: self.policy.environment_id,
            node_id: None,
            request_id: self.request_id.to_string(),
            kind,
            ip: self.ip.map(|ip| ip.to_string()).unwrap_or_default(),
            ja4: self.ja4.map(str::to_string),
            verdict: decision
                .map(|d| d.verdict.action.as_str().to_string())
                .unwrap_or_default(),
            mode: self.policy.mode.as_str().to_string(),
            reasons: decision
                .map(|d| d.verdict.reason_strings())
                .unwrap_or_default(),
            signature: decision
                .and_then(|d| d.verdict.signature())
                .map(str::to_string),
            host: events::bounded(self.host),
            path: events::bounded(self.path),
            user_agent: events::bounded(self.user_agent),
            difficulty: decision.and_then(Decision::difficulty),
            solve_ms: None,
        }
    }
}

/// Header values Shield reads, borrowed from the request.
fn header<'a>(request: &'a Request, name: &str) -> &'a str {
    request
        .headers()
        .get(name)
        .and_then(|value| value.to_str().ok())
        .unwrap_or("")
}

/// The value of cookie `name` in the request's `Cookie` headers.
fn cookie<'a>(request: &'a Request, name: &str) -> Option<&'a str> {
    request
        .headers()
        .get_all(header::COOKIE)
        .iter()
        .filter_map(|value| value.to_str().ok())
        .flat_map(|value| value.split(';'))
        .find_map(|pair| {
            let (key, value) = pair.trim().split_once('=')?;
            (key == name).then_some(value)
        })
}

fn solver_asset(path: &str) -> Option<Response> {
    let (body, content_type): (Body, &'static str) = if path == assets::SOLVER_JS_PATH {
        (
            Body::from(assets::SOLVER_JS),
            "application/javascript; charset=utf-8",
        )
    } else if path == assets::SOLVER_WASM_PATH {
        (Body::from(assets::SOLVER_WASM), "application/wasm")
    } else {
        return None;
    };
    let mut response = (StatusCode::OK, body).into_response();
    let headers = response.headers_mut();
    headers.insert(header::CONTENT_TYPE, HeaderValue::from_static(content_type));
    headers.insert(
        header::CACHE_CONTROL,
        HeaderValue::from_static("public, max-age=3600"),
    );
    Some(response)
}

fn verify_json(status: StatusCode, body: &'static str) -> Response {
    let mut response = (status, body).into_response();
    let headers = response.headers_mut();
    headers.insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static("application/json"),
    );
    headers.insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    response
}

/// The challenge page's verify POST, answered here: the clearance is for
/// the environment the signed token names.
async fn verify(
    shield: &WorkerShield,
    engine: &ShieldEngine,
    policy: &EnvPolicy,
    host: &str,
    request: Request,
    visitor: Visitor,
) -> Response {
    if request.method() != Method::POST {
        let mut response = verify_json(
            StatusCode::METHOD_NOT_ALLOWED,
            r#"{"ok":false,"error":"method"}"#,
        );
        response
            .headers_mut()
            .insert(header::ALLOW, HeaderValue::from_static("POST"));
        return response;
    }
    let user_agent = header(&request, "user-agent").to_string();
    let path = request.uri().path().to_string();
    let Ok(body) = axum::body::to_bytes(request.into_body(), assets::MAX_VERIFY_BODY).await else {
        return verify_json(
            StatusCode::PAYLOAD_TOO_LARGE,
            r#"{"ok":false,"error":"too_large"}"#,
        );
    };
    let ja4 = visitor
        .fingerprint
        .and_then(|fingerprint| fingerprint.ja4)
        .map(|ja4| ja4.as_str().to_string());
    let source = visitor
        .fingerprint
        .map_or("direct", |fingerprint| fingerprint.source.as_str());
    let request = temps_shield::Request {
        ip: visitor.ip,
        ja4: ja4.as_deref(),
        source,
        user_agent: &user_agent,
        method: "POST",
        path: &path,
        path_and_query: &path,
        tls: visitor.tls,
        ..temps_shield::Request::default()
    };
    let now = temps_shield::unix_now();
    let request_id = request_id();
    let context = EventContext {
        policy,
        request_id: &request_id,
        ip: visitor.ip,
        ja4: ja4.as_deref(),
        host,
        path: &path,
        user_agent: &user_agent,
    };
    match engine.verify(&body, &request, now) {
        VerifyOutcome::Cleared {
            env_id,
            set_cookie,
            solve_ms,
        } => {
            let mut event = context.event(EventKind::ChallengeSolved, None);
            event.environment_id = env_id;
            event.solve_ms = solve_ms.map(|ms| u32::try_from(ms).unwrap_or(u32::MAX));
            shield.submit(vec![event]);
            let mut response = verify_json(StatusCode::OK, r#"{"ok":true}"#);
            if let Ok(value) = HeaderValue::from_str(&set_cookie) {
                response.headers_mut().insert(header::SET_COOKIE, value);
            }
            response
        }
        VerifyOutcome::Rejected(error) => {
            let (status, code) = temps_shield::rejection_response(error);
            let mut event = context.event(EventKind::ChallengeFailed, None);
            event.reasons = vec![code.to_string()];
            shield.submit(vec![event]);
            let body = match code {
                "busy" => r#"{"ok":false,"error":"busy"}"#,
                "rate_limited" => r#"{"ok":false,"error":"rate_limited"}"#,
                "expired" => r#"{"ok":false,"error":"expired"}"#,
                "replayed" => r#"{"ok":false,"error":"replayed"}"#,
                "unsolved" => r#"{"ok":false,"error":"unsolved"}"#,
                _ => r#"{"ok":false,"error":"invalid"}"#,
            };
            verify_json(
                StatusCode::from_u16(status).unwrap_or(StatusCode::FORBIDDEN),
                body,
            )
        }
        VerifyOutcome::BadRequest => verify_json(
            StatusCode::BAD_REQUEST,
            r#"{"ok":false,"error":"bad_request"}"#,
        ),
    }
}

/// Shield for one public request to `host`, after its route was found and
/// before it is forwarded (ADR 066, I5). `Err` is Shield's answer. `Ok`
/// gives the request back, without the bypass token header, which is
/// Shield's alone, and, when Shield ran, the handle to report the
/// response's status with.
// The response goes straight back to the handler, which returns it.
#[allow(clippy::result_large_err)]
pub(crate) async fn guard(
    shield: &WorkerShield,
    host: &str,
    mut request: Request,
    visitor: Visitor,
) -> Result<(Request, Option<Applied>), Response> {
    let Some(policy) = shield.policy(host) else {
        return Ok((request, None));
    };
    if policy.mode == ShieldMode::Off {
        request.headers_mut().remove(bypass::HEADER);
        return Ok((request, None));
    }
    let path = request.uri().path();
    if let Some(asset) = solver_asset(path) {
        return Err(asset);
    }
    let engine = shield.engine.load_full();
    if path == assets::VERIFY_PATH {
        return Err(match &engine {
            Some(engine) => verify(shield, engine, &policy, host, request, visitor).await,
            None => unavailable(),
        });
    }
    let ja4 = visitor.fingerprint.and_then(|fingerprint| fingerprint.ja4);
    let source = visitor
        .fingerprint
        .map_or("direct", |fingerprint| fingerprint.source.as_str());
    let clearance_name = temps_shield::clearance_cookie_name(policy.environment_id);
    let mut signature_headers = [("", ""); MATCHED_HEADERS.len()];
    let mut matched = 0;
    for name in MATCHED_HEADERS {
        if let Some(value) = request.headers().get(name).and_then(|v| v.to_str().ok()) {
            signature_headers[matched] = (name, value);
            matched += 1;
        }
    }
    let shield_request = temps_shield::Request {
        ip: visitor.ip,
        ja4: ja4.as_ref().map(|ja4| ja4.as_str()),
        source,
        user_agent: header(&request, "user-agent"),
        method: request.method().as_str(),
        path,
        path_and_query: request
            .uri()
            .path_and_query()
            .map_or(path, |path_and_query| path_and_query.as_str()),
        accept: header(&request, "accept"),
        headers: &signature_headers[..matched],
        clearance: cookie(&request, &clearance_name),
        bypass_token: request
            .headers()
            .get(bypass::HEADER)
            .and_then(|value| value.to_str().ok()),
        tls: visitor.tls,
    };
    let now = temps_shield::unix_now();
    let index = shield.index.load();
    let local = shield.local.load();
    let Some(engine) = engine else {
        // No keys yet: nothing can be challenged or cleared. What the lists
        // pass is served, what they block is blocked, and the rest waits.
        return match without_gate(&index, local.as_deref(), &policy, &shield_request, now) {
            Outcome::Pass => {
                request.headers_mut().remove(bypass::HEADER);
                Ok((request, None))
            }
            outcome => {
                let mut response =
                    outcome_response(&outcome, visitor.ip).unwrap_or_else(unavailable);
                mark(
                    &mut response,
                    if outcome == Outcome::Block {
                        "block"
                    } else {
                        "error"
                    },
                    &request_id(),
                );
                Err(response)
            }
        };
    };
    let mut decision = engine.decide(&index, local.as_deref(), &policy, &shield_request, now);
    if let Outcome::Challenge {
        crawler: Some(crawler),
        ..
    } = &decision.outcome
    {
        if let Some(ip) = visitor.ip {
            if shield.verified_crawler(ip, crawler).await {
                ShieldEngine::admit_verified_crawler(&mut decision);
            }
        }
    }
    let netwatch = shield
        .netwatch
        .get()
        .filter(|sender| sender.is_enabled())
        .and_then(|sender| {
            let line = RequestLine::new(
                &shield_request,
                host,
                0,
                Some(&decision),
                chrono::Utc::now().timestamp_millis(),
            )?;
            Some((sender.clone(), line))
        });
    let applied = Applied {
        engine: Arc::clone(&engine),
        environment_id: policy.environment_id,
        netwatch,
    };
    if decision.is_event() {
        let request_id = request_id();
        let context = EventContext {
            policy: &policy,
            request_id: &request_id,
            ip: visitor.ip,
            ja4: shield_request.ja4,
            host,
            path,
            user_agent: shield_request.user_agent,
        };
        // One event per decision, as on the control plane: a served
        // challenge is its `challenge_issued`.
        let kind = if matches!(decision.outcome, Outcome::Challenge { .. }) {
            EventKind::ChallengeIssued
        } else {
            EventKind::Verdict
        };
        let recorded = vec![context.event(kind, Some(&decision))];
        if let Some(admitted) = decision.admitted {
            debug!(
                host,
                admitted = admitted.as_str(),
                "Shield served a request with a verdict"
            );
        }
        shield.submit(recorded);
        if let Some(mut response) = outcome_response(&decision.outcome, visitor.ip) {
            if let Some(verdict) = decision.header_verdict() {
                mark(&mut response, verdict, &request_id);
            }
            applied.record_status(response.status());
            return Err(response);
        }
    }
    request.headers_mut().remove(bypass::HEADER);
    Ok((request, Some(applied)))
}

/// What can be decided without the challenge gate, in the order
/// [`ShieldEngine::decide`] decides it: what passes is served, a block or a
/// bypass token holds, an exempt path is served, and anything that would
/// need a challenge cannot be answered.
fn without_gate(
    index: &Index,
    local: Option<&Index>,
    policy: &EnvPolicy,
    request: &temps_shield::Request<'_>,
    now: u64,
) -> Outcome {
    if !policy.mode.enforces() {
        return Outcome::Pass;
    }
    let listed =
        temps_shield::evaluate(index, local, policy.project_id, request, now as i64).action;
    let needs_challenge = listed == Action::Challenge || policy.mode == ShieldMode::Attack;
    if listed != Action::Block && !needs_challenge {
        return Outcome::Pass;
    }
    if request
        .bypass_token
        .and_then(|token| policy.bypass.check(token))
        .is_some()
    {
        return Outcome::Pass;
    }
    if listed == Action::Block {
        return Outcome::Block;
    }
    if policy.exempt_paths.matches(request.path) {
        return Outcome::Pass;
    }
    Outcome::Unavailable
}

#[cfg(test)]
mod tests;
