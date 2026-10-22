// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! A fake Docker Engine API for golden tests (ADR-055, phases 0 and 1).
//!
//! Every stack that starts a container builds its own `POST
//! /containers/create` request. Phase 1 replaces those builders with one
//! driver, and the requirement is that the driver sends the same request.
//! This crate records what a code path actually puts on the wire, without a
//! Docker daemon:
//!
//! - [`FakeDocker::start`] serves the Engine API on a loopback port and
//!   hands out a [`bollard::Docker`] client pointed at it.
//! - Every request is logged ([`FakeDocker::calls`]). Answers come from
//!   rules ([`FakeDocker::respond`]), then from defaults that let a create
//!   flow proceed: empty lists, "not found" for inspects, success for
//!   network and volume creates and image pulls.
//! - `POST /containers/create` is recorded ([`FakeDocker::creates`]) and
//!   answered with a 500 by default, so the flow under test stops right
//!   after building its request.
//! - [`assert_golden`] compares a recorded body with a fixture in a
//!   canonical form: keys sorted, two-space indent. Docker map fields
//!   (labels, port bindings) are serialised from hash maps in an unstable
//!   order, so the raw bytes are compared after canonicalisation. Set
//!   `TEMPS_UPDATE_GOLDEN=1` to (re)write a fixture.
//!
//! Test-only. Nothing in a release binary depends on it.

use std::path::Path;
use std::sync::{Arc, Mutex, MutexGuard};

use axum::body::{to_bytes, Body};
use axum::http::{Request, StatusCode};
use axum::response::{IntoResponse, Response};
use serde_json::{json, Map, Value};

/// The method type [`FakeDocker::respond`] takes.
pub use axum::http::Method;

/// One recorded `POST /containers/create`.
#[derive(Debug, Clone)]
pub struct CapturedCreate {
    /// `name` query parameter.
    pub name: Option<String>,
    /// `platform` query parameter.
    pub platform: Option<String>,
    /// The request body, parsed.
    pub body: Value,
}

/// One recorded request: method, path without the `/v1.xx` prefix, query.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Call {
    pub method: String,
    pub path: String,
    pub query: String,
}

impl std::fmt::Display for Call {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        if self.query.is_empty() {
            write!(f, "{} {}", self.method, self.path)
        } else {
            write!(f, "{} {}?{}", self.method, self.path, self.query)
        }
    }
}

#[derive(Clone)]
struct Rule {
    method: Method,
    path_prefix: String,
    query_contains: Option<String>,
    status: StatusCode,
    body: Value,
}

#[derive(Default)]
struct State {
    calls: Vec<Call>,
    creates: Vec<CapturedCreate>,
    rules: Vec<Rule>,
}

/// A running fake Engine API. Stops when dropped.
pub struct FakeDocker {
    addr: std::net::SocketAddr,
    state: Arc<Mutex<State>>,
    server: tokio::task::JoinHandle<()>,
}

/// The message the default create answer carries, so a test can tell the
/// expected stop from a real failure.
pub const CAPTURED_MESSAGE: &str = "temps-docker-capture: create recorded";

fn lock(state: &Mutex<State>) -> MutexGuard<'_, State> {
    // A panicking handler poisons the lock; the recorded data is still
    // what the test needs to read.
    state
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

impl FakeDocker {
    /// Serve the fake API on `127.0.0.1:<free port>`.
    pub async fn start() -> std::io::Result<Self> {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
        let addr = listener.local_addr()?;
        let state = Arc::new(Mutex::new(State::default()));
        let app = axum::Router::new().fallback({
            let state = state.clone();
            move |request: Request<Body>| handle(state.clone(), request)
        });
        let server = tokio::spawn(async move {
            let _ = axum::serve(listener, app).await;
        });
        Ok(Self {
            addr,
            state,
            server,
        })
    }

    /// A client for this fake, at bollard's default API version.
    pub fn docker(&self) -> bollard::Docker {
        // `connect_with_http` only builds a client; it cannot fail for a
        // well-formed address.
        match bollard::Docker::connect_with_http(
            &format!("http://{}", self.addr),
            30,
            bollard::API_DEFAULT_VERSION,
        ) {
            Ok(docker) => docker,
            Err(error) => panic!("bollard rejected the fake's address {}: {error}", self.addr),
        }
    }

    /// Answer `method` requests whose path starts with `path_prefix`
    /// (without the `/v1.xx` prefix) with `status` and `body`. Later rules
    /// win over earlier ones and over the defaults.
    pub fn respond(&self, method: Method, path_prefix: &str, status: u16, body: Value) {
        self.push_rule(method, path_prefix, None, status, body);
    }

    /// Like [`Self::respond`], for requests whose raw query string also
    /// contains `query_contains` (e.g. `name=temps-sandbox-egress-proxy`).
    pub fn respond_when(
        &self,
        method: Method,
        path_prefix: &str,
        query_contains: &str,
        status: u16,
        body: Value,
    ) {
        self.push_rule(
            method,
            path_prefix,
            Some(query_contains.to_string()),
            status,
            body,
        );
    }

    /// Let creates whose name contains `name_part` succeed (they are still
    /// recorded), for flows that create a helper container first.
    pub fn allow_create(&self, name_part: &str) {
        self.respond_when(
            Method::POST,
            "/containers/create",
            name_part,
            201,
            json!({ "Id": format!("fake-{name_part}"), "Warnings": [] }),
        );
    }

    fn push_rule(
        &self,
        method: Method,
        path_prefix: &str,
        query_contains: Option<String>,
        status: u16,
        body: Value,
    ) {
        let status = StatusCode::from_u16(status).unwrap_or(StatusCode::INTERNAL_SERVER_ERROR);
        lock(&self.state).rules.push(Rule {
            method,
            path_prefix: path_prefix.to_string(),
            query_contains,
            status,
            body,
        });
    }

    /// Every request received, in order.
    pub fn calls(&self) -> Vec<Call> {
        lock(&self.state).calls.clone()
    }

    /// Every container-create request received, in order.
    pub fn creates(&self) -> Vec<CapturedCreate> {
        lock(&self.state).creates.clone()
    }
}

impl Drop for FakeDocker {
    fn drop(&mut self) {
        self.server.abort();
    }
}

/// Strip bollard's `/v1.xx` version prefix.
fn unversioned(path: &str) -> &str {
    let Some(rest) = path.strip_prefix("/v") else {
        return path;
    };
    match rest.find('/') {
        Some(slash)
            if rest[..slash]
                .chars()
                .all(|c| c.is_ascii_digit() || c == '.') =>
        {
            &rest[slash..]
        }
        _ => path,
    }
}

fn query_param(query: &str, key: &str) -> Option<String> {
    query.split('&').find_map(|pair| {
        let (k, v) = pair.split_once('=')?;
        (k == key).then(|| percent_decode(v))
    })
}

fn percent_decode(value: &str) -> String {
    let bytes = value.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'%' if i + 2 < bytes.len() => {
                let hex = std::str::from_utf8(&bytes[i + 1..i + 3]).ok();
                match hex.and_then(|h| u8::from_str_radix(h, 16).ok()) {
                    Some(b) => {
                        out.push(b);
                        i += 3;
                        continue;
                    }
                    None => out.push(b'%'),
                }
            }
            b'+' => out.push(b' '),
            b => out.push(b),
        }
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

fn not_found(path: &str) -> (StatusCode, Value) {
    (
        StatusCode::NOT_FOUND,
        json!({ "message": format!("temps-docker-capture: no such object: {path}") }),
    )
}

/// Answers that let a create flow reach `POST /containers/create`.
fn default_answer(method: &Method, path: &str) -> (StatusCode, Value) {
    match (method.as_str(), path) {
        ("GET", "/_ping") | ("HEAD", "/_ping") => (StatusCode::OK, Value::String("OK".into())),
        ("GET", "/version") => (
            StatusCode::OK,
            json!({ "Version": "27.0.0", "ApiVersion": "1.47", "Os": "linux", "Arch": "amd64" }),
        ),
        ("GET", "/info") => (
            StatusCode::OK,
            json!({ "OSType": "linux", "Architecture": "x86_64", "NCPU": 2, "MemTotal": 4294967296u64 }),
        ),
        ("GET", "/containers/json") | ("GET", "/networks") | ("GET", "/images/json") => {
            (StatusCode::OK, json!([]))
        }
        ("GET", "/volumes") => (StatusCode::OK, json!({ "Volumes": [], "Warnings": [] })),
        ("POST", "/networks/create") => (
            StatusCode::CREATED,
            json!({ "Id": "fake-network", "Warning": "" }),
        ),
        // bollard's `Volume` requires these fields to be present.
        ("POST", "/volumes/create") => (
            StatusCode::CREATED,
            json!({
                "Name": "fake-volume", "Driver": "local", "Mountpoint": "/fake",
                "Labels": {}, "Scope": "local", "Options": {}
            }),
        ),
        // A pull is a JSON-lines progress stream; one status line is a
        // complete, successful one.
        ("POST", "/images/create") => (StatusCode::OK, json!({ "status": "Downloaded" })),
        ("POST", "/containers/create") => (
            StatusCode::INTERNAL_SERVER_ERROR,
            json!({ "message": CAPTURED_MESSAGE }),
        ),
        _ => not_found(path),
    }
}

async fn handle(state: Arc<Mutex<State>>, request: Request<Body>) -> Response {
    let method = request.method().clone();
    let path = unversioned(request.uri().path()).to_string();
    let query = request.uri().query().unwrap_or_default().to_string();
    let body = to_bytes(request.into_body(), 16 * 1024 * 1024)
        .await
        .unwrap_or_default();

    let (status, answer) = {
        let mut state = lock(&state);
        state.calls.push(Call {
            method: method.to_string(),
            path: path.clone(),
            query: query.clone(),
        });
        if method == Method::POST && path == "/containers/create" {
            let parsed = serde_json::from_slice(&body)
                .unwrap_or_else(|_| Value::String(String::from_utf8_lossy(&body).into_owned()));
            state.creates.push(CapturedCreate {
                name: query_param(&query, "name"),
                platform: query_param(&query, "platform").filter(|p| !p.is_empty()),
                body: parsed,
            });
        }
        let rule = state
            .rules
            .iter()
            .rev()
            .find(|r| {
                r.method == method
                    && path.starts_with(&r.path_prefix)
                    && r.query_contains
                        .as_deref()
                        .is_none_or(|part| query.contains(part))
            })
            .cloned();
        match rule {
            Some(rule) => (rule.status, rule.body),
            None => default_answer(&method, &path),
        }
    };

    let mut response = match answer {
        // `_ping` answers plain text.
        Value::String(text) if path == "/_ping" => text.into_response(),
        // No body (`204`, or a stream that ends at once).
        Value::Null => Body::empty().into_response(),
        value => axum::Json(value).into_response(),
    };
    *response.status_mut() = status;
    response
}

/// `value` with object keys sorted at every level, as two-space-indented
/// JSON with a trailing newline.
pub fn canonical_json(value: &Value) -> String {
    fn sort(value: &Value) -> Value {
        match value {
            Value::Object(map) => {
                let mut keys: Vec<_> = map.keys().collect();
                keys.sort();
                let mut sorted = Map::new();
                for key in keys {
                    sorted.insert(key.clone(), sort(&map[key]));
                }
                Value::Object(sorted)
            }
            Value::Array(items) => Value::Array(items.iter().map(sort).collect()),
            other => other.clone(),
        }
    }
    let mut out = serde_json::to_string_pretty(&sort(value)).unwrap_or_default();
    out.push('\n');
    out
}

/// Arrays whose order today comes from iterating a `HashMap`, so it
/// carries no meaning and changes between runs: `Env`, and the `Binds` of
/// a host config. Sorted in the golden form.
const UNORDERED_ARRAYS: [&[&str]; 2] = [&["Env"], &["HostConfig", "Binds"]];

/// A recorded create, as the fixture stores it.
pub fn golden_value(create: &CapturedCreate) -> Value {
    let mut body = create.body.clone();
    for path in UNORDERED_ARRAYS {
        let mut cursor = Some(&mut body);
        for key in path {
            cursor = cursor.and_then(|v| v.get_mut(*key));
        }
        if let Some(Value::Array(items)) = cursor {
            items.sort_by_key(|item| item.to_string());
        }
    }
    json!({
        "name": create.name,
        "platform": create.platform,
        "body": body,
    })
}

/// Compare `actual` with the fixture at `path`, byte for byte in canonical
/// form. With `TEMPS_UPDATE_GOLDEN=1` the fixture is written instead.
pub fn assert_golden(path: &Path, actual: &Value) {
    let actual = canonical_json(actual);
    if std::env::var_os("TEMPS_UPDATE_GOLDEN").is_some() {
        if let Some(dir) = path.parent() {
            if let Err(error) = std::fs::create_dir_all(dir) {
                panic!("create golden directory {}: {error}", dir.display());
            }
        }
        if let Err(error) = std::fs::write(path, &actual) {
            panic!("write golden fixture {}: {error}", path.display());
        }
        return;
    }
    let expected = match std::fs::read_to_string(path) {
        Ok(expected) => expected,
        Err(error) => panic!(
            "read golden fixture {}: {error}. Run with TEMPS_UPDATE_GOLDEN=1 to create it.",
            path.display()
        ),
    };
    if expected != actual {
        panic!(
            "container-create request differs from golden fixture {}\n--- expected\n{expected}\n--- actual\n{actual}",
            path.display()
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use bollard::models::ContainerCreateBody;
    use bollard::query_parameters::CreateContainerOptionsBuilder;
    use std::collections::HashMap;

    #[test]
    fn version_prefix_is_stripped() {
        assert_eq!(
            unversioned("/v1.47/containers/create"),
            "/containers/create"
        );
        assert_eq!(unversioned("/containers/json"), "/containers/json");
        assert_eq!(unversioned("/volumes/v1/x"), "/volumes/v1/x");
    }

    #[test]
    fn query_values_are_decoded() {
        assert_eq!(
            query_param("name=a%2Fb&platform=linux%2Famd64", "platform").as_deref(),
            Some("linux/amd64")
        );
        assert_eq!(query_param("name=x", "platform"), None);
        assert_eq!(percent_decode("100%"), "100%");
    }

    #[test]
    fn canonical_json_sorts_keys_at_every_level() {
        let value = json!({ "b": 1, "a": { "d": [ { "z": 1, "y": 2 } ], "c": null } });
        assert_eq!(
            canonical_json(&value),
            "{\n  \"a\": {\n    \"c\": null,\n    \"d\": [\n      {\n        \"y\": 2,\n        \"z\": 1\n      }\n    ]\n  },\n  \"b\": 1\n}\n"
        );
    }

    #[tokio::test]
    async fn records_a_create_and_stops_the_flow() {
        let fake = FakeDocker::start().await.expect("start the fake");
        let docker = fake.docker();
        docker.ping().await.expect("ping");
        let error = docker
            .create_container(
                Some(
                    CreateContainerOptionsBuilder::new()
                        .name("app-1")
                        .platform("linux/arm64")
                        .build(),
                ),
                ContainerCreateBody {
                    image: Some("nginx:1".into()),
                    labels: Some(HashMap::from([("k".into(), "v".into())])),
                    ..Default::default()
                },
            )
            .await
            .expect_err("the default create answer stops the flow");
        assert!(error.to_string().contains(CAPTURED_MESSAGE), "{error}");

        let creates = fake.creates();
        assert_eq!(creates.len(), 1);
        assert_eq!(creates[0].name.as_deref(), Some("app-1"));
        assert_eq!(creates[0].platform.as_deref(), Some("linux/arm64"));
        assert_eq!(creates[0].body["Image"], "nginx:1");
        assert_eq!(creates[0].body["Labels"]["k"], "v");
        let calls: Vec<String> = fake.calls().iter().map(ToString::to_string).collect();
        assert_eq!(calls[0], "GET /_ping");
        assert!(
            calls[1].starts_with("POST /containers/create?"),
            "{calls:?}"
        );
    }

    #[tokio::test]
    async fn rules_override_defaults_and_inspects_are_not_found() {
        let fake = FakeDocker::start().await.expect("start the fake");
        let docker = fake.docker();
        assert!(docker
            .inspect_network(
                "temps",
                None::<bollard::query_parameters::InspectNetworkOptions>
            )
            .await
            .is_err());
        fake.respond(
            Method::GET,
            "/networks/temps",
            200,
            json!({ "Name": "temps", "Id": "n1" }),
        );
        let network = docker
            .inspect_network(
                "temps",
                None::<bollard::query_parameters::InspectNetworkOptions>,
            )
            .await
            .expect("the rule answers");
        assert_eq!(network.id.as_deref(), Some("n1"));
    }

    #[test]
    fn golden_value_sorts_hash_map_ordered_arrays_only() {
        let create = CapturedCreate {
            name: Some("x".into()),
            platform: None,
            body: json!({
                "Env": ["B=2", "A=1"],
                "Cmd": ["b", "a"],
                "HostConfig": { "Binds": ["/z:/z", "/a:/a"] }
            }),
        };
        let golden = golden_value(&create);
        assert_eq!(golden["body"]["Env"], json!(["A=1", "B=2"]));
        assert_eq!(
            golden["body"]["HostConfig"]["Binds"],
            json!(["/a:/a", "/z:/z"])
        );
        assert_eq!(
            golden["body"]["Cmd"],
            json!(["b", "a"]),
            "a command keeps its order"
        );
    }

    #[test]
    fn golden_round_trip() {
        let dir = std::env::temp_dir().join(format!("temps-golden-{}", std::process::id()));
        let path = dir.join("x.json");
        std::fs::create_dir_all(&dir).expect("temp dir");
        std::fs::write(&path, canonical_json(&json!({ "a": 1 }))).expect("write fixture");
        assert_golden(&path, &json!({ "a": 1 }));
        let mismatch = std::panic::catch_unwind(|| assert_golden(&path, &json!({ "a": 2 })));
        assert!(mismatch.is_err(), "a different body must fail");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
