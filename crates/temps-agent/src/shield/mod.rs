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
    if let Some(crawler) = decision.crawler {
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
mod tests {
    use std::net::{IpAddr, Ipv4Addr};
    use std::sync::{Arc, Mutex};
    use std::time::Duration;

    use axum::body::Body;
    use axum::extract::{Query, Request, State};
    use axum::http::{header, StatusCode};
    use axum::response::{IntoResponse, Response};
    use axum::routing::{get, post};
    use axum::Router;
    use base64::engine::general_purpose::STANDARD as BASE64;
    use base64::Engine;
    use sha2::{Digest, Sha256};
    use tempfile::TempDir;
    use temps_core::worker_shield::{
        SealedShieldKeys, ShieldKeyEntry, ShieldKeyMaterial, ShieldRoutePolicy,
        ShieldSnapshotQuery, ShieldSnapshotResponse, SHIELD_KEYS_INFO,
    };
    use temps_shield::{
        bypass, has_leading_zero_bits, EventKind, EventSink, Index, ListVerdict, Network, Outcome,
        ShieldEvent, ShieldKeys, ShieldSnapshot, SourceList,
    };

    use super::{env_policy, guard, open_keys, outcome_response, Visitor, WorkerShield};

    pub(super) const HOST: &str = "app.example.test";
    pub(super) const CLEAN: IpAddr = IpAddr::V4(Ipv4Addr::new(8, 8, 8, 8));
    pub(super) const LISTED: Ipv4Addr = Ipv4Addr::new(9, 9, 9, 9);
    pub(super) const BROWSER: &str = "Mozilla/5.0 (X11; Linux x86_64) Firefox/131.0";

    pub(super) fn listed() -> IpAddr {
        IpAddr::V4(LISTED)
    }

    pub(super) fn policy(mode: &str, bypass_hash: Option<[u8; 32]>) -> ShieldRoutePolicy {
        ShieldRoutePolicy {
            project_id: 1,
            environment_id: 2,
            mode: mode.to_string(),
            exempt_paths: vec!["/health".to_string()],
            bypass_token_hashes: bypass_hash.iter().map(bypass::to_hex).collect(),
        }
    }

    pub(super) fn test_key(label: &str) -> [u8; 32] {
        Sha256::digest(label.as_bytes()).into()
    }

    pub(super) fn block_list_snapshot(revision: u64) -> ShieldSnapshot {
        let network = Network::v4(u32::from(LISTED) & 0xffff_ff00, 24).unwrap();
        ShieldSnapshot {
            revision,
            lists: vec![SourceList::new(
                "test_block",
                ListVerdict::Block,
                0,
                None,
                [network],
            )],
            ..ShieldSnapshot::default()
        }
    }

    #[derive(Default)]
    pub(super) struct Captured(Mutex<Vec<ShieldEvent>>);

    impl Captured {
        fn all(&self) -> Vec<ShieldEvent> {
            self.0.lock().unwrap().clone()
        }

        fn kinds(&self) -> Vec<EventKind> {
            self.all().iter().map(|event| event.kind).collect()
        }
    }

    impl EventSink for Captured {
        fn submit(&self, events: Vec<ShieldEvent>) {
            self.0.lock().unwrap().extend(events);
        }
    }

    pub(super) struct Fixture {
        _dir: TempDir,
        shield: WorkerShield,
        events: Arc<Captured>,
    }

    /// A worker with `policy` for [`HOST`], the test block list loaded and,
    /// when `engine`, the keys open.
    pub(super) fn fixture(policy: ShieldRoutePolicy, engine: bool) -> Fixture {
        let dir = TempDir::new().unwrap();
        let shield = WorkerShield::new(dir.path().join(super::SNAPSHOT_FILE));
        let events = Arc::new(Captured::default());
        shield.set_sink(events.clone());
        shield.set_routes(
            [(HOST.to_string(), env_policy(&policy))]
                .into_iter()
                .collect(),
            None,
        );
        let keys = engine.then(|| ShieldKeys::derive(test_key));
        shield.install(1, Index::new(&block_list_snapshot(1)), keys);
        Fixture {
            _dir: dir,
            shield,
            events,
        }
    }

    pub(super) fn page(path: &str) -> axum::http::request::Builder {
        Request::builder()
            .method("GET")
            .uri(path)
            .header(header::HOST, HOST)
            .header(header::ACCEPT, "text/html,application/xhtml+xml")
            .header(header::USER_AGENT, BROWSER)
    }

    pub(super) fn get_page(path: &str) -> Request {
        page(path).body(Body::empty()).unwrap()
    }

    pub(super) fn visitor(ip: IpAddr) -> Visitor {
        Visitor {
            ip: Some(ip),
            fingerprint: None,
            tls: true,
        }
    }

    #[allow(clippy::result_large_err)]
    pub(super) async fn run(
        fixture: &Fixture,
        request: Request,
        visitor: Visitor,
    ) -> Result<Request, Response> {
        guard(&fixture.shield, HOST, request, visitor)
            .await
            .map(|(request, _)| request)
    }

    pub(super) fn shield_header(response: &Response) -> String {
        response
            .headers()
            .get(temps_shield::RESPONSE_HEADER)
            .map(|value| value.to_str().unwrap().to_string())
            .unwrap_or_default()
    }

    pub(super) async fn body_text(response: Response) -> String {
        let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        String::from_utf8_lossy(&bytes).into_owned()
    }

    pub(super) fn attribute<'a>(html: &'a str, name: &str) -> &'a str {
        let start = html.find(&format!("{name}=\"")).unwrap() + name.len() + 2;
        let end = start + html[start..].find('"').unwrap();
        &html[start..end]
    }

    #[test]
    fn each_outcome_gets_the_control_planes_response() {
        assert!(outcome_response(&Outcome::Pass, None).is_none());

        let block = outcome_response(&Outcome::Block, None).unwrap();
        assert_eq!(block.status(), StatusCode::FORBIDDEN);
        assert_eq!(
            block.headers()[header::CONTENT_TYPE],
            "application/problem+json"
        );

        let challenge = outcome_response(
            &Outcome::Challenge {
                token: "tok".to_string(),
                difficulty: 14,
            },
            Some(CLEAN),
        )
        .unwrap();
        assert_eq!(challenge.status(), StatusCode::FORBIDDEN);
        assert_eq!(challenge.headers()[header::CACHE_CONTROL], "no-store");
        assert_eq!(challenge.headers()["x-challenge-required"], "true");

        let too_many = outcome_response(&Outcome::TooManyRequests, None).unwrap();
        assert_eq!(too_many.status(), StatusCode::TOO_MANY_REQUESTS);
        assert!(too_many.headers().contains_key(header::RETRY_AFTER));
        assert_eq!(
            too_many.headers()[header::CONTENT_TYPE],
            "application/problem+json"
        );

        let https = outcome_response(&Outcome::HttpsRequired, None).unwrap();
        assert_eq!(https.status(), StatusCode::UPGRADE_REQUIRED);
        assert!(https.headers().contains_key(header::UPGRADE));

        let unavailable = outcome_response(&Outcome::Unavailable, None).unwrap();
        assert_eq!(unavailable.status(), StatusCode::SERVICE_UNAVAILABLE);
        assert!(unavailable.headers().contains_key(header::RETRY_AFTER));
    }

    #[tokio::test]
    async fn a_host_without_settings_is_served_as_before() {
        let fixture = fixture(policy("attack", None), true);
        let request = Request::builder()
            .uri("/")
            .header(bypass::HEADER, "kept")
            .body(Body::empty())
            .unwrap();
        let (request, applied) = guard(
            &fixture.shield,
            "other.example.test",
            request,
            visitor(listed()),
        )
        .await
        .unwrap();
        assert!(applied.is_none());
        assert!(request.headers().contains_key(bypass::HEADER));
        assert!(fixture.events.kinds().is_empty());
    }

    #[tokio::test]
    async fn selective_serves_clean_visitors_and_blocks_listed_ones() {
        let (token, hash) = bypass::generate(42);
        let fixture = fixture(policy("selective", Some(hash)), true);

        let request = page("/")
            .header(bypass::HEADER, "not-a-token")
            .body(Body::empty())
            .unwrap();
        let (forwarded, applied) = guard(&fixture.shield, HOST, request, visitor(CLEAN))
            .await
            .unwrap();
        assert!(applied.is_some(), "Shield ran and records the status");
        assert!(
            !forwarded.headers().contains_key(bypass::HEADER),
            "the bypass header is Shield's alone"
        );
        assert!(
            fixture.events.kinds().is_empty(),
            "a clean pass is no event"
        );

        let blocked = run(&fixture, get_page("/"), visitor(listed()))
            .await
            .unwrap_err();
        assert_eq!(blocked.status(), StatusCode::FORBIDDEN);
        assert!(shield_header(&blocked).starts_with("block; ref="));
        let events = fixture.events.all();
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].kind, EventKind::Verdict);
        assert_eq!(events[0].verdict, "block");
        assert_eq!(events[0].ip, listed().to_string());
        assert_eq!((events[0].project_id, events[0].environment_id), (1, 2));
        assert_eq!(events[0].mode, "selective");
        assert_eq!(events[0].host, HOST);
        assert!(!events[0].reasons.is_empty());
        assert!(
            shield_header(&blocked).ends_with(&events[0].request_id),
            "the header's ref is the event's request id"
        );

        // A bypass token passes even a listed address.
        let bypassed = page("/")
            .header(bypass::HEADER, &token)
            .body(Body::empty())
            .unwrap();
        let forwarded = run(&fixture, bypassed, visitor(listed())).await.unwrap();
        assert!(!forwarded.headers().contains_key(bypass::HEADER));
    }

    #[tokio::test]
    async fn log_mode_records_and_serves() {
        let fixture = fixture(policy("log", None), true);
        assert!(run(&fixture, get_page("/"), visitor(listed()))
            .await
            .is_ok());
        let events = fixture.events.all();
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].verdict, "block");
        assert_eq!(events[0].mode, "log");
    }

    #[tokio::test]
    async fn attack_answers_what_cannot_show_a_page_and_plain_http() {
        let fixture = fixture(policy("attack", None), true);

        let api = Request::builder()
            .uri("/api/items")
            .header(header::ACCEPT, "application/json")
            .header(header::USER_AGENT, BROWSER)
            .body(Body::empty())
            .unwrap();
        let api = run(&fixture, api, visitor(CLEAN)).await.unwrap_err();
        assert_eq!(api.status(), StatusCode::TOO_MANY_REQUESTS);
        assert!(api.headers().contains_key(header::RETRY_AFTER));
        assert!(shield_header(&api).starts_with("challenge; ref="));

        let plain = Visitor {
            tls: false,
            ..visitor(CLEAN)
        };
        let plain = run(&fixture, get_page("/"), plain).await.unwrap_err();
        assert_eq!(plain.status(), StatusCode::UPGRADE_REQUIRED);

        // An exempt path is served.
        assert!(run(&fixture, get_page("/health"), visitor(CLEAN))
            .await
            .is_ok());
    }

    pub(super) fn solve(token: &str, difficulty: u8) -> String {
        (0u64..)
            .map(|nonce| nonce.to_string())
            .find(|nonce| {
                let hash = Sha256::digest(format!("{token}{nonce}").as_bytes());
                has_leading_zero_bits(&hash, u32::from(difficulty))
            })
            .unwrap()
    }

    pub(super) fn verify_request(body: serde_json::Value) -> Request {
        Request::builder()
            .method("POST")
            .uri(temps_shield::assets::VERIFY_PATH)
            .header(header::HOST, HOST)
            .header(header::USER_AGENT, BROWSER)
            .header(header::CONTENT_TYPE, "application/json")
            .body(Body::from(body.to_string()))
            .unwrap()
    }

    /// D10: the worker issues, verifies and admits on its own, end to end.
    #[tokio::test]
    async fn attack_challenges_a_page_load_and_admits_it_once_solved() {
        let fixture = fixture(policy("attack", None), true);

        let challenged = run(&fixture, get_page("/"), visitor(CLEAN))
            .await
            .unwrap_err();
        assert_eq!(challenged.status(), StatusCode::FORBIDDEN);
        assert!(shield_header(&challenged).starts_with("challenge; ref="));
        let html = body_text(challenged).await;
        let token = attribute(&html, "data-token").to_string();
        let difficulty: u8 = attribute(&html, "data-difficulty").parse().unwrap();
        // One event per decision, as on the control plane.
        assert_eq!(fixture.events.kinds(), vec![EventKind::ChallengeIssued]);

        let nonce = solve(&token, difficulty);
        let verified = run(
            &fixture,
            verify_request(serde_json::json!({"token": token, "nonce": nonce, "solve_ms": 120})),
            visitor(CLEAN),
        )
        .await
        .unwrap_err();
        assert_eq!(verified.status(), StatusCode::OK);
        let cookie = verified.headers()[header::SET_COOKIE]
            .to_str()
            .unwrap()
            .split(';')
            .next()
            .unwrap()
            .to_string();
        assert!(cookie.starts_with(&temps_shield::clearance_cookie_name(2)));
        let solved = fixture.events.all().pop().unwrap();
        assert_eq!(solved.kind, EventKind::ChallengeSolved);
        assert_eq!(solved.solve_ms, Some(120));

        let cleared = page("/")
            .header(header::COOKIE, format!("theme=dark; {cookie}"))
            .body(Body::empty())
            .unwrap();
        assert!(run(&fixture, cleared, visitor(CLEAN)).await.is_ok());

        // The same solution again is refused.
        let replay = run(
            &fixture,
            verify_request(serde_json::json!({"token": token, "nonce": nonce})),
            visitor(CLEAN),
        )
        .await
        .unwrap_err();
        assert_eq!(replay.status(), StatusCode::FORBIDDEN);
        assert_eq!(
            fixture.events.all().pop().unwrap().kind,
            EventKind::ChallengeFailed
        );
    }

    #[tokio::test]
    async fn the_verify_endpoint_refuses_what_is_not_a_solution() {
        let verify = |method: &str, body: Body| {
            Request::builder()
                .method(method)
                .uri(temps_shield::assets::VERIFY_PATH)
                .body(body)
                .unwrap()
        };
        let keyed = fixture(policy("selective", None), true);
        let get = run(&keyed, verify("GET", Body::empty()), visitor(CLEAN))
            .await
            .unwrap_err();
        assert_eq!(get.status(), StatusCode::METHOD_NOT_ALLOWED);
        let garbage = run(&keyed, verify("POST", Body::from("nope")), visitor(CLEAN))
            .await
            .unwrap_err();
        assert_eq!(garbage.status(), StatusCode::BAD_REQUEST);
        let large = vec![b'x'; temps_shield::assets::MAX_VERIFY_BODY + 1];
        let large = run(&keyed, verify("POST", Body::from(large)), visitor(CLEAN))
            .await
            .unwrap_err();
        assert_eq!(large.status(), StatusCode::PAYLOAD_TOO_LARGE);

        let keyless = fixture(policy("selective", None), false);
        let keyless = run(&keyless, verify("POST", Body::from("{}")), visitor(CLEAN))
            .await
            .unwrap_err();
        assert_eq!(keyless.status(), StatusCode::SERVICE_UNAVAILABLE);
    }

    #[tokio::test]
    async fn the_solver_is_served_from_the_worker() {
        let fixture = fixture(policy("attack", None), true);
        let js = run(
            &fixture,
            get_page(temps_shield::assets::SOLVER_JS_PATH),
            visitor(CLEAN),
        )
        .await
        .unwrap_err();
        assert_eq!(js.status(), StatusCode::OK);
        let wasm = run(
            &fixture,
            get_page(temps_shield::assets::SOLVER_WASM_PATH),
            visitor(CLEAN),
        )
        .await
        .unwrap_err();
        assert_eq!(wasm.headers()[header::CONTENT_TYPE], "application/wasm");
    }

    /// Before the keys open nothing can be challenged: what the lists pass is
    /// served, what they block is blocked, the rest waits with a 503.
    #[tokio::test]
    async fn without_keys_only_what_needs_no_challenge_is_decided() {
        let selective = fixture(policy("selective", None), false);
        assert!(run(&selective, get_page("/"), visitor(CLEAN)).await.is_ok());
        let blocked = run(&selective, get_page("/"), visitor(listed()))
            .await
            .unwrap_err();
        assert_eq!(blocked.status(), StatusCode::FORBIDDEN);
        assert!(shield_header(&blocked).starts_with("block; ref="));

        let attack = fixture(policy("attack", None), false);
        let waiting = run(&attack, get_page("/"), visitor(CLEAN))
            .await
            .unwrap_err();
        assert_eq!(waiting.status(), StatusCode::SERVICE_UNAVAILABLE);
        assert!(shield_header(&waiting).starts_with("error; ref="));
        assert!(run(&attack, get_page("/health"), visitor(CLEAN))
            .await
            .is_ok());

        let log = fixture(policy("log", None), false);
        assert!(run(&log, get_page("/"), visitor(listed())).await.is_ok());
    }

    // --- Keys and snapshots ---------------------------------------------------

    pub(super) fn keypair() -> (String, String) {
        let secret = temps_core::ecies::generate_x25519_static_secret().unwrap();
        let public = x25519_dalek::PublicKey::from(&secret);
        (
            BASE64.encode(secret.as_bytes()),
            BASE64.encode(public.as_bytes()),
        )
    }

    /// The keys as the control plane seals them, leaving out entry `skip`.
    pub(super) fn seal(public_key: &str, skip: Option<usize>) -> SealedShieldKeys {
        let entries = std::cell::RefCell::new(Vec::new());
        ShieldKeys::derive(|label| {
            let key = test_key(label);
            entries.borrow_mut().push(ShieldKeyEntry {
                label: label.to_string(),
                key: BASE64.encode(key),
            });
            key
        });
        let mut keys = entries.into_inner();
        if let Some(skip) = skip {
            keys.remove(skip);
        }
        let plaintext = serde_json::to_vec(&ShieldKeyMaterial { keys }).unwrap();
        let session =
            temps_core::ecies::EncryptionSession::with_info(public_key, SHIELD_KEYS_INFO).unwrap();
        let bundle = session.encrypt(&plaintext).unwrap();
        SealedShieldKeys {
            ephemeral_public_key: session.ephemeral_public_key().to_string(),
            ciphertext: bundle.ciphertext,
            nonce: bundle.nonce,
        }
    }

    #[test]
    fn keys_open_only_with_this_nodes_key_and_only_when_complete() {
        let (private_key, public_key) = keypair();
        let (other_private, _) = keypair();
        assert!(open_keys(&private_key, &seal(&public_key, None)).is_ok());
        assert!(open_keys(&other_private, &seal(&public_key, None)).is_err());
        let incomplete = open_keys(&private_key, &seal(&public_key, Some(0)));
        assert!(incomplete.err().unwrap().to_string().contains("no key for"));
    }

    #[test]
    fn a_snapshot_that_does_not_match_its_digest_is_refused() {
        let body = block_list_snapshot(4).encode().unwrap();
        let mut response = ShieldSnapshotResponse {
            revision: 4,
            sha256: Some(hex::encode(Sha256::digest(&body))),
            body: Some(BASE64.encode(&body)),
            keys: None,
        };
        assert_eq!(
            super::decode_snapshot(&response).unwrap().unwrap().revision,
            4
        );
        response.sha256 = Some(hex::encode([0u8; 32]));
        assert!(matches!(
            super::decode_snapshot(&response),
            Err(super::ShieldLoadError::Digest { .. })
        ));
        response.body = None;
        assert!(super::decode_snapshot(&response).unwrap().is_none());
    }

    pub(super) fn stored_response(revision: u64, public_key: &str) -> ShieldSnapshotResponse {
        let body = block_list_snapshot(revision).encode().unwrap();
        ShieldSnapshotResponse {
            revision,
            sha256: Some(hex::encode(Sha256::digest(&body))),
            body: Some(BASE64.encode(&body)),
            keys: Some(seal(public_key, None)),
        }
    }

    /// What the test control plane answers to the snapshot endpoint.
    #[derive(Clone, Copy)]
    pub(super) enum Answer {
        Snapshot,
        NotFound,
        Failing,
    }

    pub(super) struct ControlPlane {
        pub(super) answer: Mutex<Answer>,
        pub(super) response: ShieldSnapshotResponse,
        /// The `revision` each snapshot request asked with.
        pub(super) asked: Mutex<Vec<u64>>,
        /// Each event post: how many events, and the status it got.
        pub(super) posts: Mutex<Vec<(usize, StatusCode)>>,
        /// Statuses for the next event posts; 202 once empty.
        pub(super) statuses: Mutex<Vec<StatusCode>>,
        pub(super) received: Mutex<Vec<ShieldEvent>>,
    }

    impl ControlPlane {
        pub(super) fn received_ids(&self) -> Vec<String> {
            self.received
                .lock()
                .unwrap()
                .iter()
                .map(|event| event.request_id.clone())
                .collect()
        }
    }

    pub(super) async fn snapshot_endpoint(
        State(plane): State<Arc<ControlPlane>>,
        Query(query): Query<ShieldSnapshotQuery>,
    ) -> Response {
        plane.asked.lock().unwrap().push(query.revision);
        let answer = *plane.answer.lock().unwrap();
        match answer {
            Answer::NotFound => StatusCode::NOT_FOUND.into_response(),
            Answer::Failing => StatusCode::BAD_GATEWAY.into_response(),
            Answer::Snapshot
                if query.revision != 0 && query.revision == plane.response.revision =>
            {
                StatusCode::NOT_MODIFIED.into_response()
            }
            Answer::Snapshot => axum::Json(plane.response.clone()).into_response(),
        }
    }

    pub(super) async fn events_endpoint(
        State(plane): State<Arc<ControlPlane>>,
        headers: axum::http::HeaderMap,
        body: axum::body::Bytes,
    ) -> Response {
        use std::io::Read;
        assert_eq!(headers[header::AUTHORIZATION], "Bearer node-token");
        let mut json = Vec::new();
        flate2::read::GzDecoder::new(&body[..])
            .read_to_end(&mut json)
            .unwrap();
        let events: Vec<ShieldEvent> = serde_json::from_slice(&json).unwrap();
        let status = {
            let mut statuses = plane.statuses.lock().unwrap();
            if statuses.is_empty() {
                StatusCode::ACCEPTED
            } else {
                statuses.remove(0)
            }
        };
        plane.posts.lock().unwrap().push((events.len(), status));
        if status.is_success() {
            plane.received.lock().unwrap().extend(events);
        }
        status.into_response()
    }

    pub(super) async fn control_plane(
        response: ShieldSnapshotResponse,
    ) -> (Arc<ControlPlane>, String) {
        let plane = Arc::new(ControlPlane {
            answer: Mutex::new(Answer::Snapshot),
            response,
            asked: Mutex::new(Vec::new()),
            posts: Mutex::new(Vec::new()),
            statuses: Mutex::new(Vec::new()),
            received: Mutex::new(Vec::new()),
        });
        let router = Router::new()
            .route(
                "/api/internal/nodes/7/shield/snapshot",
                get(snapshot_endpoint),
            )
            .route("/api/internal/nodes/7/shield/events", post(events_endpoint))
            .with_state(plane.clone());
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        tokio::spawn(async move {
            let _ = axum::serve(listener, router).await;
        });
        (plane, format!("http://{address}"))
    }

    #[tokio::test]
    async fn netwatch_gets_each_request_with_its_status() {
        let fixture = fixture(policy("log", None), true);
        let socket_path = fixture._dir.path().join("requests.sock");
        let socket = tokio::net::UnixDatagram::bind(&socket_path).unwrap();
        fixture.shield.start_netwatch(super::NetwatchConfig {
            enabled: true,
            // Nobody answers here: the bans reader only backs off.
            api_url: "http://127.0.0.1:9".to_string(),
            api_token: None,
            socket_path,
        });

        let (_, applied) = guard(&fixture.shield, HOST, get_page("/.env"), visitor(CLEAN))
            .await
            .unwrap();
        applied
            .expect("Shield ran")
            .record_status(StatusCode::NOT_FOUND);

        let mut datagram = vec![0u8; 4096];
        let length = tokio::time::timeout(Duration::from_secs(5), socket.recv(&mut datagram))
            .await
            .expect("netwatch got a line")
            .unwrap();
        let line: serde_json::Value = serde_json::from_slice(&datagram[..length]).unwrap();
        assert_eq!(line["v"], 1);
        assert_eq!(line["ip"], CLEAN.to_string());
        assert_eq!(line["host"], HOST);
        assert_eq!(line["path"], "/.env");
        assert_eq!(line["status"], 404);
    }
}
