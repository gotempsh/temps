// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Recording a sandbox's outbound connections, end to end on Docker.
//!
//! The per-node egress proxy runs as the probe binary in a container, as it
//! does in the ADR-055 proofs, with a recorder that delivers batches over
//! HTTP. Everything on the control plane side is the real code over a real
//! TimescaleDB:
//!
//! 1. the owner turns recording on through the sandbox API handler, which
//!    checks the node's own capability report;
//! 2. the node's policy snapshot (`NetworkPolicyService::node_snapshot`)
//!    carries the sandbox's proxy rules and its `record` flag, and becomes
//!    the proxy's configuration;
//! 3. a client container with the sandbox's address makes an allowed HTTPS
//!    (CONNECT) request, an allowed plain-HTTP request and a refused one;
//! 4. the proxy posts the records to `POST /internal/nodes/{id}/egress-records`
//!    with the node token, served by the real handler on this host;
//! 5. the records are read back with `OtelService::query_logs`, the call
//!    the logs API makes, filtered by `temps.sandbox.id`.
//!
//! Then recording is turned off: records the proxy still takes (its config
//! is stale) are dropped at ingest, and a proxy built from the new snapshot
//! records nothing.
//!
//! Skips, saying why, without Docker, `alpine:3.21`, TimescaleDB or the
//! probe binary cross-built for Linux:
//! `cargo build -p temps-netpol-probe --release --target <arch>-unknown-linux-musl --target-dir target/musl`.
//! Every container and network is named and labelled for this run and
//! removed at the end; Docker picks the subnet, so runs do not collide.

#![allow(
    clippy::disallowed_methods,
    reason = "integration test: reads fixture configuration from the environment"
)]
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "integration test helpers: a failed fixture step must fail the test loudly"
)]

use std::collections::HashMap;
use std::net::IpAddr;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use bollard::exec::{StartExecOptions, StartExecResults};
use bollard::models::{ContainerCreateBody, ExecConfig, HostConfig, NetworkCreateRequest};
use bollard::query_parameters::{
    CreateContainerOptionsBuilder, LogsOptions, RemoveContainerOptions,
};
use bollard::Docker;
use futures::StreamExt;
use sea_orm::{ConnectionTrait, DatabaseBackend, Statement};
use temps_auth::{AuthContext, Role};
use temps_netpol_probe::{RecordingConfig, ServeConfig};
use temps_otel::services::otel_service::DEFAULT_MAX_CONCURRENT_INGEST_REQUESTS;
use temps_otel::types::{LogAttributeFilter, LogQuery, LogRecord};
use temps_otel::OtelService;
use temps_sandbox::handlers::egress_recording::{routes, EgressRecordingAppState};
use temps_sandbox::services::egress_recording::{SandboxEgress, SandboxEgressAdapter};
use temps_test_utils::TestDatabase;
use temps_workload::{WorkloadKind, WorkloadRef};
use temps_workload_egress::handlers::{node_routes, EgressRecordingNodeState};
use temps_workload_egress::{EgressLogStore, EgressRecordingService};
use temps_workload_netpol::EgressRecordingCapability;
use tower::ServiceExt;

const IMAGE: &str = "alpine:3.21";
const LABEL: &str = "sh.temps.test";
const LABEL_VALUE: &str = "egress-recording-e2e";
const OWNER: i32 = 5101;
const PROJECT: i32 = 5101;
const SANDBOX: (i32, &str) = (5101, "sbx_00000000000b5101");
const NODE_TOKEN: &str = "egress-recording-e2e-node-token";
const PROXY_PORT: u16 = 3128;

fn bin_path() -> PathBuf {
    let base = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../target/musl")
        .join(format!("{}-unknown-linux-musl", std::env::consts::ARCH));
    let release = base.join("release/temps-netpol-probe");
    if release.exists() {
        release
    } else {
        base.join("debug/temps-netpol-probe")
    }
}

fn run_id() -> String {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.subsec_nanos())
        .unwrap_or(0);
    format!("{}{:04x}", std::process::id(), nanos & 0xffff)
}

fn owner() -> temps_entities::users::Model {
    let now = chrono::Utc::now();
    temps_entities::users::Model {
        id: OWNER,
        name: "Owner".to_string(),
        email: "owner@example.com".to_string(),
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
    }
}

/// This run's Docker objects, removed on drop of [`Run::cleanup`].
struct Run {
    docker: Docker,
    id: String,
    network: String,
    config_dir: PathBuf,
    containers: Vec<String>,
}

impl Run {
    fn name(&self, role: &str) -> String {
        format!("temps-egress-rec-{}-{role}", self.id)
    }

    fn labels(&self) -> HashMap<String, String> {
        HashMap::from([
            (LABEL.to_string(), LABEL_VALUE.to_string()),
            (format!("{LABEL}.run"), self.id.clone()),
        ])
    }

    async fn container(
        &mut self,
        role: &str,
        cmd: Vec<String>,
        extra_hosts: Vec<String>,
    ) -> IpAddr {
        let name = self.name(role);
        let body = ContainerCreateBody {
            image: Some(IMAGE.to_string()),
            cmd: Some(cmd),
            labels: Some(self.labels()),
            host_config: Some(HostConfig {
                network_mode: Some(self.network.clone()),
                binds: Some(vec![
                    format!("{}:/probe:ro", bin_path().display()),
                    format!("{}:/config:ro", self.config_dir.display()),
                ]),
                extra_hosts: Some(extra_hosts),
                cap_drop: Some(vec!["ALL".to_string()]),
                security_opt: Some(vec!["no-new-privileges:true".to_string()]),
                ..Default::default()
            }),
            ..Default::default()
        };
        self.docker
            .create_container(
                Some(CreateContainerOptionsBuilder::new().name(&name).build()),
                body,
            )
            .await
            .unwrap_or_else(|e| panic!("create container {name}: {e}"));
        self.containers.push(name.clone());
        self.docker
            .start_container(&name, None)
            .await
            .unwrap_or_else(|e| panic!("start container {name}: {e}"));
        let inspect = self.docker.inspect_container(&name, None).await.unwrap();
        inspect
            .network_settings
            .and_then(|n| n.networks)
            .and_then(|n| n.get(&self.network).cloned())
            .and_then(|e| e.ip_address)
            .and_then(|ip| ip.parse().ok())
            .unwrap_or_else(|| panic!("container {name} has no address on {}", self.network))
    }

    async fn remove(&mut self, role: &str) {
        let name = self.name(role);
        let _ = self
            .docker
            .remove_container(
                &name,
                Some(RemoveContainerOptions {
                    force: true,
                    ..Default::default()
                }),
            )
            .await;
        self.containers.retain(|c| c != &name);
    }

    async fn exec(&self, role: &str, script: &str) -> String {
        let exec = self
            .docker
            .create_exec(
                &self.name(role),
                ExecConfig {
                    cmd: Some(vec!["sh".into(), "-c".into(), script.into()]),
                    attach_stdout: Some(true),
                    attach_stderr: Some(true),
                    ..Default::default()
                },
            )
            .await
            .expect("create exec");
        let mut text = String::new();
        if let Ok(StartExecResults::Attached { mut output, .. }) = self
            .docker
            .start_exec(
                &exec.id,
                Some(StartExecOptions {
                    detach: false,
                    ..Default::default()
                }),
            )
            .await
        {
            while let Some(Ok(chunk)) = output.next().await {
                text.push_str(&String::from_utf8_lossy(&chunk.into_bytes()));
            }
        }
        text
    }

    async fn logs(&self, role: &str) -> String {
        let mut stream = self.docker.logs(
            &self.name(role),
            Some(LogsOptions {
                stdout: true,
                stderr: true,
                ..Default::default()
            }),
        );
        let mut text = String::new();
        while let Some(Ok(chunk)) = stream.next().await {
            text.push_str(&String::from_utf8_lossy(&chunk.into_bytes()));
        }
        text
    }

    async fn cleanup(&mut self) {
        for name in std::mem::take(&mut self.containers) {
            let _ = self
                .docker
                .remove_container(
                    &name,
                    Some(RemoveContainerOptions {
                        force: true,
                        ..Default::default()
                    }),
                )
                .await;
        }
        let _ = self.docker.remove_network(&self.network).await;
        let _ = std::fs::remove_dir_all(&self.config_dir);
    }
}

/// Docker, the image and the probe binary, or why the test skips.
async fn docker_run() -> Result<Run, String> {
    let docker = Docker::connect_with_defaults().map_err(|e| format!("no Docker client: {e}"))?;
    docker
        .ping()
        .await
        .map_err(|e| format!("Docker not available: {e}"))?;
    docker
        .inspect_image(IMAGE)
        .await
        .map_err(|_| format!("{IMAGE} not present"))?;
    if !bin_path().exists() {
        return Err(format!(
            "{} is missing (cross-build the probe for Linux)",
            bin_path().display()
        ));
    }
    let id = run_id();
    let config_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join(format!("../../target/musl/egress-recording-e2e-{id}"));
    std::fs::create_dir_all(&config_dir).map_err(|e| format!("config dir: {e}"))?;
    let network = format!("temps-egress-rec-{id}");
    let run = Run {
        docker,
        id,
        network,
        config_dir,
        containers: Vec::new(),
    };
    let labels = run.labels();
    run.docker
        .create_network(NetworkCreateRequest {
            name: run.network.clone(),
            driver: Some("bridge".to_string()),
            labels: Some(labels),
            ..Default::default()
        })
        .await
        .map_err(|e| format!("create network: {e}"))?;
    Ok(run)
}

async fn gateway(run: &Run) -> IpAddr {
    let network = run
        .docker
        .inspect_network(&run.network, None)
        .await
        .expect("inspect network");
    network
        .ipam
        .and_then(|i| i.config)
        .and_then(|c| c.into_iter().find_map(|c| c.gateway))
        .and_then(|g| g.parse().ok())
        .expect("the network has a gateway")
}

struct ControlPlane {
    db: TestDatabase,
    node_id: i32,
    otel: Arc<OtelService>,
    policy: Arc<temps_network_policy::NetworkPolicyService>,
    api: axum::Router,
}

async fn control_plane() -> Option<ControlPlane> {
    let db = match TestDatabase::with_migrations().await {
        Ok(db) => db,
        Err(error) => {
            eprintln!("TimescaleDB unavailable, skipping egress recording e2e: {error}");
            return None;
        }
    };
    let conn = db.connection();
    conn.execute_unprepared(&format!(
        "INSERT INTO users (id, name, email) VALUES ({OWNER}, 'Owner', 'owner@example.com'); \
         INSERT INTO projects (id, name, slug, repo_name, repo_owner, directory, main_branch, \
           preset, created_at, updated_at) \
         VALUES ({PROJECT}, 'Agent', 'agent-e2e', 'agent', 'example', '/', 'main', 'static', \
           now(), now());"
    ))
    .await
    .expect("seed");
    let node_id: i32 = conn
        .query_one(Statement::from_sql_and_values(
            DatabaseBackend::Postgres,
            "INSERT INTO nodes (name, token_hash, address, private_address, role, status, \
             labels, capacity) VALUES ('worker-e2e', $1, '127.0.0.1', '10.0.0.1', 'worker', \
             'active', '{}', '{}') RETURNING id",
            [temps_core::crypto::sha256_hex(NODE_TOKEN).into()],
        ))
        .await
        .expect("insert node")
        .expect("node row")
        .try_get("", "id")
        .expect("node id");
    conn.execute(Statement::from_sql_and_values(
        DatabaseBackend::Postgres,
        "INSERT INTO sandboxes (id, public_id, name, user_id, project_id, status, backend, \
         node_id) VALUES ($1, $2, $2, $3, $4, 'running', 'docker', $5)",
        [
            SANDBOX.0.into(),
            SANDBOX.1.into(),
            OWNER.into(),
            PROJECT.into(),
            node_id.into(),
        ],
    ))
    .await
    .expect("insert sandbox");
    // The sandbox's proxy rules, as the network-policy API stores them: the
    // allowed API host on the web ports, and its address (a private one on
    // this Docker network, so the public-address check needs a CIDR allow).
    let otel = Arc::new(OtelService::new(
        Arc::new(temps_otel::storage::timescaledb::TimescaleDbStorage::new(
            db.connection_arc(),
            None,
        )),
        Arc::new(temps_otel::ingest::auth::OtelAuthService::new(
            db.connection_arc(),
        )),
        Arc::new(temps_otel::ingest::rate_limit::RateLimiter::new(
            10_000,
            Duration::from_secs(60),
        )),
        DEFAULT_MAX_CONCURRENT_INGEST_REQUESTS,
    ));
    let policy = Arc::new(temps_network_policy::NetworkPolicyService::new(
        db.connection_arc(),
    ));
    let service = Arc::new(EgressRecordingService::new(
        db.connection_arc(),
        policy.clone(),
        Some(otel.clone() as Arc<dyn EgressLogStore>),
    ));
    // As the plugins wire it: sandboxes are one kind the shared service
    // records, with their own routes over it.
    service.register(Arc::new(SandboxEgressAdapter::new(db.connection_arc())));
    let api = routes()
        .with_state(Arc::new(EgressRecordingAppState {
            service: Arc::new(SandboxEgress::new(db.connection_arc(), service.clone())),
            audit_service: None,
        }))
        .merge(node_routes(Arc::new(EgressRecordingNodeState { service })));
    Some(ControlPlane {
        db,
        node_id,
        otel,
        policy,
        api,
    })
}

impl ControlPlane {
    async fn add_rule(&self, peer_kind: &str, peer_ref: &str, ports: Option<(i32, i32)>) {
        self.db
            .connection()
            .execute(Statement::from_sql_and_values(
                DatabaseBackend::Postgres,
                "INSERT INTO network_policy_rules (subject_kind, subject_id, direction, \
                 peer_kind, peer_ref, port_start, port_end, protocol, action, origin, \
                 created_at, updated_at) VALUES ('sandbox', $1, 'egress', $2, $3, $4, $5, \
                 'tcp', 'allow', 'operator', now(), now())",
                [
                    i64::from(SANDBOX.0).into(),
                    peer_kind.into(),
                    peer_ref.into(),
                    ports.map(|p| p.0).into(),
                    ports.map(|p| p.1).into(),
                ],
            ))
            .await
            .expect("insert network policy rule");
    }

    async fn set_recording(&self, enabled: bool) -> (StatusCode, serde_json::Value) {
        let mut request = Request::builder()
            .method("PUT")
            .uri(format!("/v1/sandboxes/{}/egress-recording", SANDBOX.1))
            .header("content-type", "application/json")
            .body(Body::from(format!(r#"{{"enabled":{enabled}}}"#)))
            .unwrap();
        request
            .extensions_mut()
            .insert(temps_core::RequestMetadata {
                ip_address: "198.51.100.30".to_string(),
                user_agent: "egress-recording-e2e".to_string(),
                headers: Default::default(),
                visitor_id_cookie: None,
                session_id_cookie: None,
                base_url: "http://localhost".to_string(),
                scheme: "http".to_string(),
                host: "localhost".to_string(),
                is_secure: false,
            });
        request
            .extensions_mut()
            .insert(AuthContext::new_session(owner(), Role::User));
        let response = self.api.clone().oneshot(request).await.unwrap();
        let status = response.status();
        let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        (
            status,
            serde_json::from_slice(&bytes).unwrap_or(serde_json::Value::Null),
        )
    }

    /// The proxy configuration a node would build from its snapshot.
    async fn proxy_config(&self, sandbox_ip: IpAddr, recording: RecordingConfig) -> ServeConfig {
        let snapshot = self.policy.node_snapshot(self.node_id).await.unwrap();
        let subject = WorkloadRef::new(WorkloadKind::Sandbox, i64::from(SANDBOX.0));
        ServeConfig {
            proxy_listen: Some(format!("0.0.0.0:{PROXY_PORT}").parse().unwrap()),
            identities: vec![(subject, sandbox_ip)],
            egress: snapshot
                .egress
                .into_iter()
                .filter(|p| p.subject == subject)
                .collect(),
            stats_every_secs: Some(1),
            recording: Some(recording),
            ..Default::default()
        }
    }

    async fn records(&self) -> Vec<LogRecord> {
        let mut records = self
            .otel
            .query_logs(LogQuery {
                project_id: PROJECT,
                attribute_filter: Some(LogAttributeFilter {
                    key: "temps.sandbox.id".to_string(),
                    value: SANDBOX.1.to_string(),
                    ty: None,
                    operator: Default::default(),
                }),
                start_time: Some(chrono::Utc::now() - chrono::Duration::days(365 * 10)),
                end_time: Some(chrono::Utc::now() + chrono::Duration::hours(1)),
                limit: Some(100),
                ..Default::default()
            })
            .await
            .expect("query logs");
        records.sort_by_key(|r| r.timestamp);
        records
    }
}

/// The three requests, from the sandbox's container.
async fn make_requests(run: &Run, proxy: IpAddr) -> (String, String, String) {
    let https = run
        .exec(
            "sandbox",
            &format!(
                "printf 'CONNECT api.example.com:443 HTTP/1.1\\r\\nHost: api.example.com:443\\r\\n\\r\\nGET / HTTP/1.1\\r\\nHost: api.example.com\\r\\n\\r\\n' | nc -w 3 {proxy} {PROXY_PORT}"
            ),
        )
        .await;
    let http = run
        .exec(
            "sandbox",
            &format!(
                "http_proxy=http://{proxy}:{PROXY_PORT} wget -q -O- 'http://api.example.com/v1/items?api_key=secret-value' 2>&1"
            ),
        )
        .await;
    let refused = run
        .exec(
            "sandbox",
            &format!(
                "printf 'CONNECT blocked.example.com:443 HTTP/1.1\\r\\n\\r\\n' | nc -w 3 {proxy} {PROXY_PORT}"
            ),
        )
        .await;
    (https, http, refused)
}

async fn wait_for<F, Fut>(what: &str, mut check: F)
where
    F: FnMut() -> Fut,
    Fut: std::future::Future<Output = bool>,
{
    for _ in 0..60 {
        if check().await {
            return;
        }
        tokio::time::sleep(Duration::from_millis(500)).await;
    }
    panic!("timed out waiting for {what}");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn an_opted_in_sandbox_records_its_connections_and_an_opted_out_one_does_not() {
    let mut run = match docker_run().await {
        Ok(run) => run,
        Err(reason) => {
            println!("skipping the egress recording e2e: {reason}");
            return;
        }
    };
    let Some(cp) = control_plane().await else {
        run.cleanup().await;
        return;
    };
    let outcome = std::panic::AssertUnwindSafe(scenario(&mut run, &cp));
    let result = futures::FutureExt::catch_unwind(outcome).await;
    run.cleanup().await;
    if let Err(panic) = result {
        std::panic::resume_unwind(panic);
    }
}

async fn scenario(run: &mut Run, cp: &ControlPlane) {
    // The control plane's node endpoint, on this host, reached from the
    // proxy's container through the network gateway.
    let listener = tokio::net::TcpListener::bind("0.0.0.0:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let api = cp.api.clone();
    tokio::spawn(async move { axum::serve(listener, api).await });
    let gateway = gateway(run).await;
    let recording = RecordingConfig {
        url: format!(
            "http://{gateway}:{port}/internal/nodes/{}/egress-records",
            cp.node_id
        ),
        token: NODE_TOKEN.to_string(),
        max_batch_age_ms: Some(500),
        channel_capacity: None,
    };

    // The destination: the probe's sink answers HTTP on 80 and 443.
    let upstream = run
        .container(
            "upstream",
            vec!["/probe".into(), "sink".into(), "-".into(), "80,443".into()],
            vec![],
        )
        .await;
    let sandbox_ip = run
        .container("sandbox", vec!["sleep".into(), "600".into()], vec![])
        .await;
    cp.add_rule("host", "api.example.com", None).await;
    cp.add_rule("cidr", &format!("{upstream}/32"), Some((80, 443)))
        .await;

    // Before the node reports that it can record, turning recording on is
    // refused and says why.
    let (status, body) = cp.set_recording(true).await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{body}");
    assert_eq!(body["reason_code"], "node_not_reporting", "{body}");

    // The node's report: this proxy serves with a recorder on Linux.
    cp.policy
        .record_egress_recording(cp.node_id, Some(EgressRecordingCapability::detect(true)));
    let (status, body) = cp.set_recording(true).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["enabled"], true);
    assert_eq!(body["configured"], true);

    let config = cp.proxy_config(sandbox_ip, recording.clone()).await;
    assert!(
        config.egress.first().is_some_and(|p| p.record),
        "the snapshot tells the proxy to record: {:?}",
        config.egress
    );
    let config_dir = run.config_dir.clone();
    let start_proxy = |config: &ServeConfig, name: &str| {
        std::fs::write(config_dir.join(name), serde_json::to_vec(config).unwrap()).unwrap();
    };
    start_proxy(&config, "on.json");
    let hosts = vec![
        format!("api.example.com:{upstream}"),
        // Outside every rule: the CIDR allow admits the upstream's address
        // by address, so a name resolving there would be allowed.
        "blocked.example.com:203.0.113.10".to_string(),
    ];
    let proxy = run
        .container(
            "proxy",
            vec!["/probe".into(), "serve".into(), "/config/on.json".into()],
            hosts.clone(),
        )
        .await;
    wait_for("the proxy to listen", || async {
        run.logs("proxy").await.contains("egress proxy listening")
    })
    .await;

    let (https, http, refused) = make_requests(run, proxy).await;
    assert!(https.contains("200 Connection Established"), "{https}");
    assert!(
        https.contains("ok"),
        "the tunnel carried the request: {https}"
    );
    assert!(http.trim_end().ends_with("ok"), "{http}");
    assert!(refused.contains("403 Forbidden"), "{refused}");

    wait_for("three records", || async { cp.records().await.len() >= 3 }).await;
    let records = cp.records().await;
    assert_eq!(records.len(), 3, "one record per connection: {records:#?}");
    let find = |kind: &str, host: &str| {
        records
            .iter()
            .find(|r| {
                r.attributes.get("temps.egress.kind").map(String::as_str) == Some(kind)
                    && r.attributes.get("server.address").map(String::as_str) == Some(host)
            })
            .unwrap_or_else(|| panic!("no {kind} record for {host}: {records:#?}"))
    };
    let attr = |r: &LogRecord, k: &str| r.attributes.get(k).cloned().unwrap_or_default();

    let tunnel = find("tunnel", "api.example.com");
    assert_eq!(attr(tunnel, "server.port"), "443");
    assert_eq!(attr(tunnel, "temps.egress.outcome"), "allowed");
    assert_eq!(attr(tunnel, "network.peer.address"), upstream.to_string());
    assert!(
        attr(tunnel, "temps.egress.bytes_sent")
            .parse::<u64>()
            .unwrap()
            > 0
    );
    assert!(
        attr(tunnel, "temps.egress.bytes_received")
            .parse::<u64>()
            .unwrap()
            > 0
    );
    assert_eq!(
        attr(tunnel, "http.request.method"),
        "",
        "a tunnel shows no method"
    );

    let forward = find("forward", "api.example.com");
    assert_eq!(attr(forward, "server.port"), "80");
    assert_eq!(attr(forward, "http.request.method"), "GET");
    assert_eq!(attr(forward, "url.path"), "/v1/items");
    assert_eq!(attr(forward, "temps.egress.outcome"), "allowed");
    assert!(
        attr(forward, "temps.egress.bytes_sent")
            .parse::<u64>()
            .unwrap()
            > 0
    );

    let denied = find("tunnel", "blocked.example.com");
    assert_eq!(attr(denied, "temps.egress.outcome"), "refused");
    assert_eq!(attr(denied, "temps.egress.reason"), "no_allow_rule");
    assert_eq!(denied.severity_text, "WARN");

    for record in &records {
        assert_eq!(record.project_id, Some(PROJECT));
        assert_eq!(record.resource.service_name, "temps-egress-proxy");
        assert_eq!(attr(record, "otel.scope.name"), "temps.egress");
        assert_eq!(attr(record, "temps.platform.source"), "egress-proxy");
        assert_eq!(attr(record, "temps.node.id"), cp.node_id.to_string());
        assert_eq!(attr(record, "temps.node.name"), "worker-e2e");
        let all = format!("{} {:?}", record.body, record.attributes);
        assert!(
            !all.contains("secret-value"),
            "no query string is stored: {all}"
        );
    }
    println!("records stored:");
    for record in &records {
        println!("  {} | {}", record.severity_text, record.body);
    }

    // Turned off. The running proxy's config is stale and still records,
    // but the control plane drops what it delivers.
    let (status, body) = cp.set_recording(false).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let delivered_before = run.logs("proxy").await.matches("records delivered").count();
    let (https, _, _) = make_requests(run, proxy).await;
    assert!(https.contains("200 Connection Established"), "{https}");
    wait_for("the stale proxy to deliver", || async {
        run.logs("proxy").await.matches("records delivered").count() > delivered_before
    })
    .await;
    assert_eq!(
        cp.records().await.len(),
        3,
        "opted-out records are dropped at ingest"
    );

    // A proxy built from the new snapshot records nothing at all.
    let config = cp.proxy_config(sandbox_ip, recording).await;
    assert!(
        config.egress.iter().all(|p| !p.record),
        "{:?}",
        config.egress
    );
    start_proxy(&config, "off.json");
    run.remove("proxy").await;
    let proxy = run
        .container(
            "proxy",
            vec!["/probe".into(), "serve".into(), "/config/off.json".into()],
            hosts,
        )
        .await;
    wait_for("the proxy to listen", || async {
        run.logs("proxy").await.contains("egress proxy listening")
    })
    .await;
    let (https, http, refused) = make_requests(run, proxy).await;
    assert!(https.contains("200 Connection Established"), "{https}");
    assert!(http.trim_end().ends_with("ok"), "{http}");
    assert!(refused.contains("403 Forbidden"), "{refused}");
    // Two stats intervals, so the counters below are after the requests.
    tokio::time::sleep(Duration::from_millis(2500)).await;
    let logs = run.logs("proxy").await;
    let stats = logs
        .lines()
        .filter_map(|l| l.strip_prefix("stats "))
        .next_back()
        .expect("a stats line");
    let stats: serde_json::Value = serde_json::from_str(stats).unwrap();
    assert_eq!(stats["tunnels"], 1, "{stats}");
    assert_eq!(stats["forwarded"], 1, "{stats}");
    assert_eq!(
        stats["records_emitted"], 0,
        "recording off: nothing recorded: {stats}"
    );
    assert!(!logs.contains("records delivered"), "{logs}");
    assert_eq!(cp.records().await.len(), 3);
}
