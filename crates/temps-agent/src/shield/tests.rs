// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

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
    SealedShieldKeys, ShieldKeyEntry, ShieldKeyMaterial, ShieldRef, ShieldRoutePolicy,
    ShieldSnapshotQuery, ShieldSnapshotResponse, SHIELD_KEYS_INFO,
};
use temps_shield::{
    bypass, has_leading_zero_bits, EventKind, EventSink, Index, ListVerdict, Network, Outcome,
    ShieldEvent, ShieldKeys, ShieldMode, ShieldSnapshot, SourceList,
};
use tokio::sync::Notify;

use super::sync::{fetch_once, load_from_disk, Fetched, ShieldSyncConfig};
use super::uploader::{EventUploader, UploaderSettings};
use super::{env_policy, guard, open_keys, outcome_response, Visitor, WorkerShield};

const HOST: &str = "app.example.test";
const CLEAN: IpAddr = IpAddr::V4(Ipv4Addr::new(8, 8, 8, 8));
const LISTED: Ipv4Addr = Ipv4Addr::new(9, 9, 9, 9);
const BROWSER: &str = "Mozilla/5.0 (X11; Linux x86_64) Firefox/131.0";

fn listed() -> IpAddr {
    IpAddr::V4(LISTED)
}

fn policy(mode: &str, bypass_hash: Option<[u8; 32]>) -> ShieldRoutePolicy {
    ShieldRoutePolicy {
        project_id: 1,
        environment_id: 2,
        mode: mode.to_string(),
        exempt_paths: vec!["/health".to_string()],
        bypass_token_hashes: bypass_hash.iter().map(bypass::to_hex).collect(),
    }
}

fn test_key(label: &str) -> [u8; 32] {
    Sha256::digest(label.as_bytes()).into()
}

fn block_list_snapshot(revision: u64) -> ShieldSnapshot {
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
struct Captured(Mutex<Vec<ShieldEvent>>);

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

struct Fixture {
    _dir: TempDir,
    shield: WorkerShield,
    events: Arc<Captured>,
}

/// A worker with `policy` for [`HOST`], the test block list loaded and,
/// when `engine`, the keys open.
fn fixture(policy: ShieldRoutePolicy, engine: bool) -> Fixture {
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

fn page(path: &str) -> axum::http::request::Builder {
    Request::builder()
        .method("GET")
        .uri(path)
        .header(header::HOST, HOST)
        .header(header::ACCEPT, "text/html,application/xhtml+xml")
        .header(header::USER_AGENT, BROWSER)
}

fn get_page(path: &str) -> Request {
    page(path).body(Body::empty()).unwrap()
}

fn visitor(ip: IpAddr) -> Visitor {
    Visitor {
        ip: Some(ip),
        fingerprint: None,
        tls: true,
    }
}

#[allow(clippy::result_large_err)]
async fn run(fixture: &Fixture, request: Request, visitor: Visitor) -> Result<Request, Response> {
    guard(&fixture.shield, HOST, request, visitor)
        .await
        .map(|(request, _)| request)
}

fn shield_header(response: &Response) -> String {
    response
        .headers()
        .get(temps_shield::RESPONSE_HEADER)
        .map(|value| value.to_str().unwrap().to_string())
        .unwrap_or_default()
}

async fn body_text(response: Response) -> String {
    let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    String::from_utf8_lossy(&bytes).into_owned()
}

fn attribute<'a>(html: &'a str, name: &str) -> &'a str {
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
            crawler: None,
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

fn solve(token: &str, difficulty: u8) -> String {
    (0u64..)
        .map(|nonce| nonce.to_string())
        .find(|nonce| {
            let hash = Sha256::digest(format!("{token}{nonce}").as_bytes());
            has_leading_zero_bits(&hash, u32::from(difficulty))
        })
        .unwrap()
}

fn verify_request(body: serde_json::Value) -> Request {
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

fn keypair() -> (String, String) {
    let secret = temps_core::ecies::generate_x25519_static_secret().unwrap();
    let public = x25519_dalek::PublicKey::from(&secret);
    (
        BASE64.encode(secret.as_bytes()),
        BASE64.encode(public.as_bytes()),
    )
}

/// The keys as the control plane seals them, leaving out entry `skip`.
fn seal(public_key: &str, skip: Option<usize>) -> SealedShieldKeys {
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

/// What the test control plane answers to the snapshot endpoint.
#[derive(Clone, Copy)]
enum Answer {
    Snapshot,
    NotFound,
    Failing,
}

struct ControlPlane {
    answer: Mutex<Answer>,
    response: ShieldSnapshotResponse,
    /// The `revision` each snapshot request asked with.
    asked: Mutex<Vec<u64>>,
    /// Each event post: how many events, and the status it got.
    posts: Mutex<Vec<(usize, StatusCode)>>,
    /// Statuses for the next event posts; 202 once empty.
    statuses: Mutex<Vec<StatusCode>>,
    received: Mutex<Vec<ShieldEvent>>,
}

impl ControlPlane {
    fn received_ids(&self) -> Vec<String> {
        self.received
            .lock()
            .unwrap()
            .iter()
            .map(|event| event.request_id.clone())
            .collect()
    }
}

async fn snapshot_endpoint(
    State(plane): State<Arc<ControlPlane>>,
    Query(query): Query<ShieldSnapshotQuery>,
) -> Response {
    plane.asked.lock().unwrap().push(query.revision);
    let answer = *plane.answer.lock().unwrap();
    match answer {
        Answer::NotFound => StatusCode::NOT_FOUND.into_response(),
        Answer::Failing => StatusCode::BAD_GATEWAY.into_response(),
        Answer::Snapshot if query.revision != 0 && query.revision == plane.response.revision => {
            StatusCode::NOT_MODIFIED.into_response()
        }
        Answer::Snapshot => axum::Json(plane.response.clone()).into_response(),
    }
}

async fn events_endpoint(
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

async fn control_plane(response: ShieldSnapshotResponse) -> (Arc<ControlPlane>, String) {
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

fn sync_config(url: &str, private_key: Option<String>) -> ShieldSyncConfig {
    ShieldSyncConfig {
        control_plane_url: url.to_string(),
        node_id: 7,
        node_token: "node-token".to_string(),
        http: reqwest::Client::new(),
        private_key_b64: private_key,
        uploader: UploaderSettings::default(),
    }
}

fn stored_response(revision: u64, public_key: &str) -> ShieldSnapshotResponse {
    let body = block_list_snapshot(revision).encode().unwrap();
    ShieldSnapshotResponse {
        revision,
        sha256: Some(hex::encode(Sha256::digest(&body))),
        body: Some(BASE64.encode(&body)),
        keys: Some(seal(public_key, None)),
    }
}

/// A worker with a `selective` route and nothing loaded.
fn routed_shield(dir: &TempDir) -> WorkerShield {
    let shield = WorkerShield::new(dir.path().join(super::SNAPSHOT_FILE));
    shield.set_routes(
        [(HOST.to_string(), env_policy(&policy("selective", None)))]
            .into_iter()
            .collect(),
        Some(ShieldRef { revision: 5 }),
    );
    shield
}

async fn blocks_listed(shield: &WorkerShield) -> bool {
    guard(shield, HOST, get_page("/"), visitor(listed()))
        .await
        .is_err_and(|response| response.status() == StatusCode::FORBIDDEN)
}

#[tokio::test]
async fn a_fetched_snapshot_is_swapped_in_kept_on_disk_and_loaded_offline() {
    let (private_key, public_key) = keypair();
    let (plane, url) = control_plane(stored_response(5, &public_key)).await;
    let config = sync_config(&url, Some(private_key.clone()));
    let dir = TempDir::new().unwrap();
    let shield = routed_shield(&dir);
    assert!(
        !blocks_listed(&shield).await,
        "the built-in lists do not list it"
    );

    let wanted = Some(ShieldRef { revision: 5 });
    assert_eq!(
        fetch_once(&config, &shield, wanted).await.unwrap(),
        Fetched::Installed(5)
    );
    assert_eq!(shield.revision(), 5);
    assert!(shield.has_engine());
    assert!(blocks_listed(&shield).await);
    // With no engine yet, the worker asks for everything (revision 0).
    assert_eq!(*plane.asked.lock().unwrap(), vec![0]);

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let path = dir.path().join(super::SNAPSHOT_FILE);
        let mode = std::fs::metadata(path).unwrap().permissions().mode();
        assert_eq!(mode & 0o777, 0o600);
    }

    // The revision the routes name is loaded: nothing to ask.
    assert_eq!(
        fetch_once(&config, &shield, wanted).await.unwrap(),
        Fetched::Unchanged
    );
    assert_eq!(plane.asked.lock().unwrap().len(), 1);
    // A newer one named, but the control plane has nothing newer: 304.
    assert_eq!(
        fetch_once(&config, &shield, Some(ShieldRef { revision: 6 }))
            .await
            .unwrap(),
        Fetched::Unchanged
    );
    assert_eq!(*plane.asked.lock().unwrap(), vec![0, 5]);

    // Restarted without the control plane: the same lists and keys.
    let restarted = routed_shield(&dir);
    assert_eq!(
        load_from_disk(&restarted, Some(&private_key))
            .await
            .unwrap(),
        Some(5)
    );
    assert!(restarted.has_engine());
    assert!(blocks_listed(&restarted).await);

    // Nothing on disk is nothing loaded.
    let empty = TempDir::new().unwrap();
    let fresh = routed_shield(&empty);
    assert_eq!(
        load_from_disk(&fresh, Some(&private_key)).await.unwrap(),
        None
    );
    assert!(!fresh.has_engine());
}

#[tokio::test]
async fn an_old_control_plane_or_a_failing_one_leaves_what_is_loaded() {
    let (private_key, public_key) = keypair();
    let (plane, url) = control_plane(stored_response(5, &public_key)).await;
    let config = sync_config(&url, Some(private_key));
    let dir = TempDir::new().unwrap();
    let shield = routed_shield(&dir);

    *plane.answer.lock().unwrap() = Answer::NotFound;
    assert_eq!(
        fetch_once(&config, &shield, None).await.unwrap(),
        Fetched::Unsupported
    );
    *plane.answer.lock().unwrap() = Answer::Failing;
    assert!(fetch_once(&config, &shield, None).await.is_err());
    assert_eq!(shield.revision(), 0);
    assert!(!shield.has_engine());
    assert!(!dir.path().join(super::SNAPSHOT_FILE).exists());
}

#[tokio::test]
async fn lists_load_even_when_the_keys_are_for_another_node() {
    let (_, public_key) = keypair();
    let (other_private, _) = keypair();
    let (_plane, url) = control_plane(stored_response(5, &public_key)).await;
    let dir = TempDir::new().unwrap();
    let shield = routed_shield(&dir);
    assert_eq!(
        fetch_once(&sync_config(&url, Some(other_private)), &shield, None)
            .await
            .unwrap(),
        Fetched::Installed(5)
    );
    assert!(!shield.has_engine());
    assert!(blocks_listed(&shield).await);
}

// --- Event uploader -------------------------------------------------------

fn event(n: usize) -> ShieldEvent {
    ShieldEvent {
        timestamp_ms: 1_700_000_000_000 + n as i64,
        project_id: 1,
        environment_id: 2,
        node_id: None,
        request_id: format!("req-{n}"),
        kind: EventKind::Verdict,
        ip: CLEAN.to_string(),
        ja4: None,
        verdict: "block".to_string(),
        mode: ShieldMode::Selective.as_str().to_string(),
        reasons: vec!["test_block".to_string()],
        signature: None,
        host: HOST.to_string(),
        path: "/".to_string(),
        user_agent: BROWSER.to_string(),
        difficulty: None,
        solve_ms: None,
    }
}

fn uploader(url: &str, settings: UploaderSettings) -> (Arc<EventUploader>, Arc<Notify>) {
    let shutdown = Arc::new(Notify::new());
    let uploader = EventUploader::spawn(
        reqwest::Client::new(),
        format!("{url}/api/internal/nodes/7/shield/events"),
        "node-token".to_string(),
        settings,
        shutdown.clone(),
    );
    (uploader, shutdown)
}

async fn until(condition: impl Fn() -> bool) {
    tokio::time::timeout(Duration::from_secs(10), async {
        while !condition() {
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .expect("condition not met in time");
}

fn quick(batch: usize, interval: Duration, backoff: Duration) -> UploaderSettings {
    UploaderSettings {
        batch,
        interval,
        initial_backoff: backoff,
        max_backoff: backoff,
        ..UploaderSettings::default()
    }
}

#[tokio::test]
async fn a_full_batch_is_sent_at_once_and_the_rest_on_the_interval() {
    let (plane, url) = control_plane(stored_response(1, &keypair().1)).await;
    let settings = quick(3, Duration::from_millis(300), Duration::from_millis(20));
    let (uploader, _shutdown) = uploader(&url, settings);
    uploader.submit((0..4).map(event).collect());
    until(|| plane.received.lock().unwrap().len() == 4).await;
    assert_eq!(
        *plane.posts.lock().unwrap(),
        vec![(3, StatusCode::ACCEPTED), (1, StatusCode::ACCEPTED)],
        "the full batch at once, the rest on the interval"
    );
    assert_eq!(
        plane.received_ids(),
        vec!["req-0", "req-1", "req-2", "req-3"]
    );
    assert_eq!(uploader.dropped(), 0);
}

#[tokio::test]
async fn a_failed_post_is_retried_and_a_refused_one_dropped() {
    let (plane, url) = control_plane(stored_response(1, &keypair().1)).await;
    *plane.statuses.lock().unwrap() = vec![StatusCode::SERVICE_UNAVAILABLE];
    let settings = quick(2, Duration::from_millis(30), Duration::from_millis(20));
    let (uploader, _shutdown) = uploader(&url, settings);
    uploader.submit((0..2).map(event).collect());
    until(|| plane.received.lock().unwrap().len() == 2).await;
    assert_eq!(
        plane.posts.lock().unwrap()[..2],
        [
            (2, StatusCode::SERVICE_UNAVAILABLE),
            (2, StatusCode::ACCEPTED)
        ]
    );

    *plane.statuses.lock().unwrap() = vec![StatusCode::BAD_REQUEST];
    uploader.submit((2..4).map(event).collect());
    until(|| uploader.dropped() == 2).await;
    tokio::time::sleep(Duration::from_millis(200)).await;
    assert_eq!(
        plane.posts.lock().unwrap().len(),
        3,
        "a refusal is not retried"
    );
    assert_eq!(plane.received_ids(), vec!["req-0", "req-1"]);
}

#[tokio::test]
async fn a_full_queue_drops_events_without_waiting() {
    let (_plane, url) = control_plane(stored_response(1, &keypair().1)).await;
    let settings = UploaderSettings {
        queue: 2,
        ..quick(500, Duration::from_secs(60), Duration::from_millis(20))
    };
    let (uploader, _shutdown) = uploader(&url, settings);
    // The test runtime has one thread: the uploader task does not run
    // while `submit` does.
    uploader.submit((0..5).map(event).collect());
    assert_eq!(uploader.dropped(), 3);
}

#[tokio::test]
async fn the_buffer_keeps_the_newest_while_the_control_plane_is_down() {
    let (plane, url) = control_plane(stored_response(1, &keypair().1)).await;
    *plane.statuses.lock().unwrap() = vec![StatusCode::BAD_GATEWAY; 3];
    let settings = UploaderSettings {
        buffer: 3,
        ..quick(1, Duration::from_millis(20), Duration::from_millis(300))
    };
    let (uploader, _shutdown) = uploader(&url, settings);
    uploader.submit(vec![event(0)]);
    until(|| !plane.posts.lock().unwrap().is_empty()).await;
    // Within the backoff: four more into a buffer of three.
    uploader.submit((1..5).map(event).collect());
    until(|| plane.received.lock().unwrap().len() == 3).await;
    assert_eq!(plane.received_ids(), vec!["req-2", "req-3", "req-4"]);
    assert_eq!(uploader.dropped(), 2);
}

#[tokio::test]
async fn shutdown_sends_what_waits() {
    let (plane, url) = control_plane(stored_response(1, &keypair().1)).await;
    let settings = quick(500, Duration::from_secs(60), Duration::from_millis(20));
    let (uploader, shutdown) = uploader(&url, settings);
    // Let the task reach its wait, past the interval's first tick.
    tokio::time::sleep(Duration::from_millis(20)).await;
    uploader.submit((0..2).map(event).collect());
    tokio::time::sleep(Duration::from_millis(20)).await;
    shutdown.notify_waiters();
    until(|| plane.received.lock().unwrap().len() == 2).await;
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
