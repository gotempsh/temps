// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Pins the order of the checks in `request_filter`.
//!
//! Each test sets up a request two checks would both answer and asserts which
//! one does, through the real hook over an in-memory session. The database is
//! a mock serving only the settings row, so none of this needs Docker.

use std::net::IpAddr;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

use async_trait::async_trait;
use pingora_http::RequestHeader;
use pingora_proxy::{ProxyHttp, Session};
use sea_orm::{DatabaseBackend, DatabaseConnection, MockDatabase};
use temps_core::admin_gate::{AdminGateConfig, AdminGateHandle, AdminGateSource};
use temps_core::gate::{GateKind, GatePolicy, TempsGate};
use temps_core::{ProjectIpGate, RequestPolicyContext, RequestPolicyDecision, RequestPolicyGate};
use temps_entities::deployment_config::{
    DeploymentConfig, PasswordProtectionConfig, RateLimitConfig, SecurityConfig,
};
use temps_entities::ingress_mode::IngressMode;
use temps_entities::{deployments, environments, project_edge_rules, projects, settings};
use temps_routes::{BackendEntry, BackendType, CachedPeerTable, PathRoute, RouteInfo};
use tokio::io::{AsyncReadExt, AsyncWriteExt};

use crate::config::ProxyConfig;
use crate::edge_rules::EdgeRuleGate;
use crate::handler::password_wall::PASSWORD_VERIFY_PATH;
use crate::on_demand::SleepingEnvironmentInfo;
use crate::on_demand::{ContainerLifecycle, OnDemandError, OnDemandManager};
use crate::on_demand_cert::{
    CertProvisionFailure, OnDemandCertConfig, OnDemandCertManager, OnDemandCertProvisioner,
    OnDemandCertState,
};
use crate::proxy::{LoadBalancer, ProxyContext};
use crate::service::acme_challenge_cache::{AcmeChallengeCache, PendingChallenge};
use crate::services::{ProjectContextResolverImpl, UpstreamResolverImpl};
use crate::traits::{ProjectContextResolver, UpstreamResolver};

const HOST: &str = "app.example.test";
const CLEAN_IP: &str = "8.8.8.8";
/// On the Shield block list below.
const SHIELD_BLOCKED_IP: &str = "185.93.89.7";
const ACME_TOKEN: &str = "stored-token";
const ACME_PATH: &str = "/.well-known/acme-challenge/stored-token";
const PROJECT: i32 = 10;
const ENVIRONMENT: i32 = 20;
const OTHER_ENVIRONMENT: i32 = 21;

struct Fixture {
    lb: LoadBalancer,
    routes: Arc<CachedPeerTable>,
}

fn mock_db() -> Arc<DatabaseConnection> {
    let now = chrono::Utc::now();
    let row = settings::Model {
        id: 1,
        data: serde_json::to_value(temps_config::AppSettings::default())
            .expect("settings serialize"),
        created_at: now,
        updated_at: now,
    };
    Arc::new(
        MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results(vec![vec![row]; 16])
            .into_connection(),
    )
}

fn fixture_with(config: ProxyConfig) -> Fixture {
    let db = mock_db();
    let server_config = temps_config::ServerConfig::new(
        "127.0.0.1:3000".to_string(),
        "postgresql://test@localhost/test".to_string(),
        None,
        None,
    )
    .expect("server config");
    let config_service = Arc::new(temps_config::ConfigService::new(
        Arc::new(server_config),
        db.clone(),
    ));

    let geoip = Arc::new(temps_geo::GeoIpService::Mock(
        temps_geo::MockGeoIpService::new(),
    ));
    let ip_service = Arc::new(temps_geo::IpAddressService::new(db.clone(), geoip));
    let log_storage: Arc<dyn crate::storage::ProxyLogStorage> = Arc::new(
        crate::storage::TimescaleDbProxyLogStore::new(db.clone(), ip_service.clone()),
    );
    let (proxy_log_handle, tracking_handle, _writer) =
        crate::service::proxy_log_batch_writer::ProxyLogBatchWriter::new(
            db.clone(),
            ip_service,
            log_storage,
        );

    let routes = Arc::new(CachedPeerTable::new(db.clone()));
    routes.mark_loaded_for_test();
    let upstream_resolver = Arc::new(UpstreamResolverImpl::new(
        Arc::new(config),
        Arc::new(crate::service::lb_service::LbService::new(db.clone())),
        routes.clone(),
    )) as Arc<dyn UpstreamResolver>;
    let project_context_resolver = Arc::new(ProjectContextResolverImpl::new(routes.clone()))
        as Arc<dyn ProjectContextResolver>;

    let mut lb = LoadBalancer::new(
        upstream_resolver,
        proxy_log_handle,
        tracking_handle,
        project_context_resolver,
        Arc::new(
            temps_core::CookieCrypto::new("default-32-byte-key-for-testing!").expect("crypto"),
        ),
        db.clone(),
        config_service,
        Arc::new(
            crate::service::ip_access_control_service::IpAccessControlService::new(db.clone()),
        ),
        Arc::new(temps_core::OpenIpGate),
        Arc::new(crate::shield::test_gate()),
        Arc::new(crate::service::cert_host_cache::CertHostCache::new(db)),
        false,
    );
    lb.acme_challenges = Arc::new(AcmeChallengeCache::with_pending_for_tests(vec![
        PendingChallenge {
            domain: HOST.to_string(),
            token: ACME_TOKEN.to_string(),
            key_authorization: "stored-token.thumbprint".to_string(),
        },
    ]));
    let now = temps_shield::unix_now() as i64;
    let lists = crate::shield::ShieldLists::from_snapshot(&temps_shield::ShieldSnapshot {
        revision: 1,
        generated_at: now,
        lists: vec![temps_shield::SourceList::new(
            "test_block_list",
            temps_shield::ListVerdict::Block,
            now,
            None,
            temps_shield::Network::parse("185.93.89.0/24"),
        )],
        ..temps_shield::ShieldSnapshot::default()
    });
    let lb = lb.with_shield_lists(Arc::new(lists));
    Fixture { lb, routes }
}

fn fixture() -> Fixture {
    fixture_with(ProxyConfig::default())
}

fn project(id: i32) -> projects::Model {
    let now = chrono::Utc::now();
    projects::Model {
        id,
        name: format!("project-{id}"),
        repo_name: format!("project-{id}"),
        repo_owner: "test".to_string(),
        directory: String::new(),
        pull_only_root_directory: false,
        main_branch: "main".to_string(),
        preset: temps_entities::preset::Preset::NextJs,
        preset_config: None,
        deployment_config: None,
        created_at: now,
        updated_at: now,
        slug: format!("project-{id}"),
        is_deleted: false,
        deleted_at: None,
        last_deployment: None,
        is_public_repo: false,
        git_url: None,
        git_provider_connection_id: None,
        attack_mode: false,
        shield_mode: "off".to_string(),
        ai_alert_summaries_enabled: None,
        ai_debug_chat_enabled: None,
        ai_write_actions_enabled: false,
        error_source_context_enabled: false,
        vulnerability_scanning_enabled: false,
        error_source_root: None,
        enable_preview_environments: false,
        preview_envs_on_demand: false,
        preview_envs_idle_timeout_seconds: 300,
        preview_envs_wake_timeout_seconds: 30,
        source_type: Default::default(),
        project_type: temps_entities::types::ProjectType::Server,
        allow_alternate_sources: None,
        hosting: Default::default(),
        template_slug: None,
        service_template: None,
        gitlab_webhook_id: None,
        gitlab_webhook_signing_token: None,
        gitea_webhook_signing_token: None,
        github_webhook_id: None,
        github_webhook_signing_token: None,
        bitbucket_webhook_token: None,
        bitbucket_webhook_hook_id: None,
        generic_webhook_token: None,
        cross_project_trace_sharing: false,
        serves_own_temps_api: false,
        ai_api_traffic_summary_enabled: None,
        image_retention_hours: None,
        cloud_telemetry_fidelity: Default::default(),
        cloud_telemetry_attribute_allowlist: Vec::new(),
        cloud_telemetry_write_mode: Default::default(),
        cloud_analytics_write_mode: Default::default(),
        log_ingest_lines_per_sec: None,
        log_ingest_kib_per_sec: None,
        instantiation_token: None,
    }
}

fn environment(id: i32, project_id: i32) -> environments::Model {
    let now = chrono::Utc::now();
    environments::Model {
        id,
        name: "production".into(),
        slug: "production".into(),
        subdomain: format!("app-{id}"),
        last_deployment: None,
        host: String::new(),
        upstreams: Default::default(),
        created_at: now,
        updated_at: now,
        project_id,
        current_deployment_id: None,
        branch: None,
        deleted_at: None,
        deployment_config: None,
        is_preview: false,
        protected: false,
        sleeping: false,
        attack_mode: None,
        shield_mode: None,
        shield_exempt_paths: Default::default(),
        // These requests are plain HTTP; only the redirect tests want HTTPS.
        force_https: Some(false),
        last_activity_at: None,
        ingress_mode: IngressMode::default(),
        workload_mode: Default::default(),
    }
}

fn deployment(id: i32, project_id: i32, environment_id: i32) -> deployments::Model {
    let now = chrono::Utc::now();
    deployments::Model {
        id,
        project_id,
        environment_id,
        created_at: now,
        updated_at: now,
        slug: format!("d{id}"),
        state: "deployed".to_string(),
        metadata: None,
        deploying_at: None,
        ready_at: None,
        started_at: None,
        finished_at: None,
        context_vars: None,
        branch_ref: None,
        tag_ref: None,
        commit_sha: None,
        commit_message: None,
        commit_author: None,
        commit_json: None,
        cancelled_reason: None,
        static_dir_location: None,
        screenshot_location: None,
        image_name: None,
        deployment_config: None,
        promoted_from_deployment_id: None,
        upload_request_id: None,
        docker_socket_mounted: false,
    }
}

fn upstream() -> BackendType {
    BackendType::Upstream {
        backends: vec![BackendEntry {
            address: "127.0.0.1:9".to_string(),
            container_id: None,
            container_name: None,
        }],
        round_robin_counter: Arc::new(AtomicUsize::new(0)),
    }
}

fn route_for(
    environment_id: i32,
    configure: impl FnOnce(&mut projects::Model, &mut environments::Model),
) -> RouteInfo {
    let mut project = project(PROJECT);
    let mut env = environment(environment_id, PROJECT);
    configure(&mut project, &mut env);
    RouteInfo {
        backend: upstream(),
        redirect_to: None,
        status_code: None,
        project: Some(Arc::new(project)),
        environment: Some(Arc::new(env)),
        deployment: Some(Arc::new(deployment(1, PROJECT, environment_id))),
        cert_eligible: false,
    }
}

impl Fixture {
    /// `HOST` is served by the test project, as `configure` sets it up.
    fn route_host(&self, configure: impl FnOnce(&mut projects::Model, &mut environments::Model)) {
        self.routes
            .insert_route_for_test(HOST, route_for(ENVIRONMENT, configure));
    }

    fn deny_all_ips(&mut self) {
        self.lb.project_ip_gate = Arc::new(DenyEnvironment(None));
    }

    /// `host` becomes a signed-in address.
    fn sign_in_gate(&mut self, host: &str) {
        self.routes
            .insert_gate_policy_for_test(host, signed_in_policy());
        self.lb.gate = Some(Arc::new(crate::handler::gate::ProxyGate::new(
            self.routes.clone(),
            Arc::new(temps_routes::gate::GateService::new(
                mock_db(),
                Arc::new(temps_core::EncryptionService::new_from_password(
                    "gate-test",
                )),
            )),
        )));
    }

    fn fix_request_policy(&mut self, decision: RequestPolicyDecision) {
        self.lb.request_policy_gate = Arc::new(FixedPolicy(decision));
    }

    fn with_edge_rules(self, action: serde_json::Value) -> Self {
        let gate = EdgeRuleGate::with_rules_for_tests(vec![every_path_rule(action)]);
        Self {
            lb: self.lb.with_edge_rules(Arc::new(gate), None),
            routes: self.routes,
        }
    }

    fn with_on_demand(self, lifecycle: Arc<CountingLifecycle>) -> (Self, Arc<OnDemandManager>) {
        let manager = Arc::new(OnDemandManager::new_test(
            Arc::new(MockDatabase::new(DatabaseBackend::Postgres).into_connection()),
            lifecycle,
        ));
        let mut lb = self.lb.with_on_demand_manager(manager.clone());
        lb.route_table = Some(self.routes.clone());
        (
            Self {
                lb,
                routes: self.routes,
            },
            manager,
        )
    }
}

/// Denies clients of one environment, or of all of them.
struct DenyEnvironment(Option<i32>);

impl ProjectIpGate for DenyEnvironment {
    fn is_allowed(&self, _project_id: i32, environment_id: i32, _ip: IpAddr) -> bool {
        self.0.is_some_and(|denied| denied != environment_id)
    }
}

struct FixedPolicy(RequestPolicyDecision);

impl RequestPolicyGate for FixedPolicy {
    fn evaluate(&self, _context: &RequestPolicyContext<'_>) -> RequestPolicyDecision {
        self.0
    }
}

const POLICY_DENY: RequestPolicyDecision = RequestPolicyDecision::Deny {
    reason: "test",
    rule_id: None,
    revision: None,
};

fn every_path_rule(action: serde_json::Value) -> project_edge_rules::Model {
    let now = chrono::Utc::now();
    project_edge_rules::Model {
        id: 1,
        project_id: PROJECT,
        environment_id: None,
        position: 1,
        name: "every path".into(),
        enabled: true,
        mode: "enforce".into(),
        expression: serde_json::json!({"combinator": "all", "children": [
            {"kind": "condition", "field": "path", "op": "starts_with", "values": ["/"]}
        ]}),
        action,
        revision: 1,
        created_by: None,
        updated_by: None,
        created_at: now,
        updated_at: now,
    }
}

fn block_rule() -> serde_json::Value {
    serde_json::json!({"type": "block", "status": 403})
}

fn allow_rule() -> serde_json::Value {
    serde_json::json!({"type": "allow"})
}

fn security_config(config: SecurityConfig) -> Option<DeploymentConfig> {
    Some(DeploymentConfig {
        security: Some(config),
        ..Default::default()
    })
}

/// One request per minute and per hour.
fn rate_limited(env: &mut environments::Model) {
    env.deployment_config = security_config(SecurityConfig {
        enabled: Some(true),
        rate_limiting: Some(RateLimitConfig {
            max_requests_per_minute: Some(1),
            max_requests_per_hour: Some(1),
            whitelist_ips: Vec::new(),
            blacklist_ips: Vec::new(),
        }),
        ..Default::default()
    });
}

fn password_walled(env: &mut environments::Model) {
    password_walled_with(env, "not-a-real-hash");
}

fn password_walled_with(env: &mut environments::Model, password_hash: &str) {
    env.deployment_config = security_config(SecurityConfig {
        password_protection: Some(PasswordProtectionConfig {
            enabled: true,
            password_hash: password_hash.to_string(),
        }),
        ..Default::default()
    });
}

/// Counts the containers a wake tried to start.
#[derive(Default)]
struct CountingLifecycle {
    starts: AtomicUsize,
}

#[async_trait]
impl ContainerLifecycle for CountingLifecycle {
    async fn start_container(&self, _container_id: &str) -> Result<(), OnDemandError> {
        self.starts.fetch_add(1, Ordering::SeqCst);
        Ok(())
    }

    async fn stop_container(&self, _container_id: &str) -> Result<(), OnDemandError> {
        Ok(())
    }

    async fn is_container_healthy(&self, _container_id: &str) -> Result<bool, OnDemandError> {
        Ok(true)
    }
}

fn sleeping(environment_id: i32) -> SleepingEnvironmentInfo {
    SleepingEnvironmentInfo {
        environment_id,
        project_id: PROJECT,
        deployment_id: 1,
        wake_timeout_seconds: 1,
    }
}

/// Every wake slot, so the next request cannot get one.
fn all_wake_slots(manager: &OnDemandManager) -> Vec<tokio::sync::OwnedSemaphorePermit> {
    std::iter::from_fn(|| manager.try_acquire_wake_slot()).collect()
}

struct Reply {
    answered: bool,
    head: String,
    body: String,
    ctx: ProxyContext,
    /// The request as it would go upstream.
    request: RequestHeader,
}

impl Reply {
    fn status(&self) -> &str {
        self.ctx.routing_status.as_str()
    }

    fn assert_answered(&self, routing_status: &str, status_line: &str) {
        assert!(self.answered, "must be answered: {}", self.status());
        assert_eq!(self.status(), routing_status, "{}", self.head);
        assert!(
            self.head.starts_with(&format!("HTTP/1.1 {status_line}")),
            "{}",
            self.head
        );
    }

    fn assert_forwarded(&self, routing_status: &str) {
        assert!(
            !self.answered,
            "must be forwarded, answered {}: {}",
            self.status(),
            self.head
        );
        assert_eq!(self.status(), routing_status);
    }

    fn upstream_header(&self, name: &str) -> Option<&str> {
        self.request
            .headers
            .get(name)
            .and_then(|value| value.to_str().ok())
    }
}

/// Runs the request hooks over an in-memory session, for a client at `ip`:
/// the stream has no peer address, so it is set where
/// `early_request_filter` would have found it.
async fn send(lb: &LoadBalancer, raw_request: &str, ip: &str) -> Reply {
    let (mut client, server) = tokio::io::duplex(1024 * 1024);
    client
        .write_all(raw_request.as_bytes())
        .await
        .expect("write request");
    let mut session = Session::new_h1(Box::new(server) as pingora_core::protocols::Stream);
    assert!(
        session.read_request().await.expect("read request"),
        "request must parse"
    );
    let mut ctx = lb.new_ctx();
    lb.early_request_filter(&mut session, &mut ctx)
        .await
        .expect("early_request_filter");
    ctx.ip_address = Some(ip.to_string());
    let answered = lb
        .request_filter(&mut session, &mut ctx)
        .await
        .expect("request_filter");
    let request = session.req_header().clone();
    drop(session);
    let mut raw = Vec::new();
    tokio::time::timeout(
        std::time::Duration::from_secs(5),
        client.read_to_end(&mut raw),
    )
    .await
    .expect("response in time")
    .expect("read response");
    let response = String::from_utf8_lossy(&raw).to_string();
    let (head, body) = response
        .split_once("\r\n\r\n")
        .map(|(head, body)| (head.to_string(), body.to_string()))
        .unwrap_or((response.clone(), String::new()));
    Reply {
        answered,
        head,
        body,
        ctx,
        request,
    }
}

fn get_on(host: &str, path: &str) -> String {
    format!("GET {path} HTTP/1.1\r\nHost: {host}\r\nAccept: text/html\r\n\r\n")
}

fn get(path: &str) -> String {
    get_on(HOST, path)
}

fn post(path: &str, body: &str) -> String {
    format!(
        "POST {path} HTTP/1.1\r\nHost: {HOST}\r\nContent-Type: application/x-www-form-urlencoded\r\nContent-Length: {}\r\n\r\n{body}",
        body.len()
    )
}

// ── Inbound headers ─────────────────────────────────────────────────────────

/// Everything only the proxy may set is gone or overwritten before the
/// request goes upstream or into the request log.
#[tokio::test]
async fn client_supplied_trust_headers_never_reach_the_app_or_the_log() {
    let f = fixture();
    f.route_host(|_, _| {});
    let request = format!(
        "GET / HTTP/1.1\r\nHost: {HOST}\r\n\
         X-Origin-Probe-Authorization: test-credential\r\n\
         X-Temps-Demo-Mode: true\r\n\
         {identity}-email: spoofed@example.test\r\n\
         {status_host}: status.example.test\r\n\
         X-Real-IP: 1.2.3.4\r\n\
         CF-Connecting-IP: 1.2.3.4\r\n\
         Forwarded: for=1.2.3.4\r\n\
         X-Forwarded-For: 1.2.3.4\r\n\
         X-Forwarded-Host: evil.example.test\r\n\
         X-Forwarded-Proto: https\r\n\
         X-Forwarded-Port: 1234\r\n\
         \r\n",
        identity = temps_core::gate::IDENTITY_HEADER_PREFIX,
        status_host = temps_core::public_status_page::STATUS_HOST_HEADER,
    );

    let reply = send(&f.lb, &request, CLEAN_IP).await;

    reply.assert_forwarded("routed");
    for name in [
        "x-temps-demo-mode",
        temps_core::public_status_page::STATUS_HOST_HEADER,
        "x-real-ip",
        "cf-connecting-ip",
        "forwarded",
    ] {
        assert_eq!(reply.upstream_header(name), None, "{name} reached the app");
    }
    let identity = format!("{}-email", temps_core::gate::IDENTITY_HEADER_PREFIX);
    assert_eq!(reply.upstream_header(&identity), None);
    assert_eq!(reply.upstream_header("x-forwarded-for"), Some(CLEAN_IP));
    assert_eq!(reply.upstream_header("x-forwarded-host"), Some(HOST));
    assert_eq!(reply.upstream_header("x-forwarded-proto"), Some("http"));
    assert_eq!(reply.upstream_header("x-forwarded-port"), Some("80"));

    let logged = reply
        .ctx
        .request_headers
        .as_ref()
        .expect("headers captured");
    for name in [
        temps_core::probe_auth::HEADER,
        "x-real-ip",
        "cf-connecting-ip",
        "forwarded",
        identity.as_str(),
    ] {
        assert!(!logged.contains_key(name), "{name} reached the request log");
    }
}

/// The header the status-host branch tags a request with cannot be supplied
/// by a client to reach a status route on an ordinary host.
#[tokio::test]
async fn a_spoofed_status_host_tag_is_stripped_before_the_status_host_branch() {
    let f = fixture();
    let request = format!(
        "GET / HTTP/1.1\r\nHost: {HOST}\r\n{}: {HOST}\r\n\r\n",
        temps_core::public_status_page::STATUS_HOST_HEADER,
    );
    let reply = send(&f.lb, &request, CLEAN_IP).await;
    assert!(!reply.ctx.status_page_host);
    assert_eq!(
        reply.upstream_header(temps_core::public_status_page::STATUS_HOST_HEADER),
        None
    );
}

// ── Status-page hostname ────────────────────────────────────────────────────

/// A status hostname also routed to a locked-down app answers as a status
/// page: nothing the app's route carries is consulted, for any path.
#[tokio::test]
async fn status_page_host_answers_before_everything_the_host_also_routes_to() {
    let mut f = fixture();
    f.deny_all_ips();
    f.route_host(|_, env| password_walled(env));
    f.routes.insert_status_page_host_for_test(HOST, None);

    let reply = send(&f.lb, &get("/anything"), CLEAN_IP).await;

    reply.assert_answered("status_page_host_not_found", "404");
    assert!(reply.ctx.project.is_none(), "project routing must not run");
}

/// A connection negotiated for a status hostname carries no other host. The
/// SNI can only be set by a real handshake, so this drives the check directly:
/// its place in the stages, right after the status host's own, is not pinned.
#[tokio::test]
async fn a_connection_negotiated_for_a_status_hostname_carries_no_other_host() {
    let f = fixture();
    f.routes
        .insert_status_page_host_for_test("status.example.test", None);
    let (mut client, server) = tokio::io::duplex(64 * 1024);
    client
        .write_all(get("/").as_bytes())
        .await
        .expect("write request");
    let mut session = Session::new_h1(Box::new(server) as pingora_core::protocols::Stream);
    assert!(session.read_request().await.expect("read request"));
    let mut ctx = f.lb.new_ctx();

    let flow =
        f.lb.refuse_misdirected_connection(&mut session, &mut ctx, Some(HOST))
            .await
            .expect("no status hostname");
    assert!(matches!(flow, super::Flow::Continue));

    let flow =
        f.lb.refuse_misdirected_connection(&mut session, &mut ctx, Some("status.example.test"))
            .await
            .expect("misdirected");
    assert!(matches!(flow, super::Flow::Answered));
    assert_eq!(ctx.routing_status, "status_page_host_misdirected");
}

// ── Sandbox preview ─────────────────────────────────────────────────────────

/// A preview-shaped host is the sandbox's, whatever else claims the name.
#[tokio::test]
async fn preview_host_answers_before_the_gate_and_project_routing() {
    let mut f = fixture();
    f.deny_all_ips();
    let preview = "ws-7702c56bfb804b49-3000.localho.st";
    f.routes
        .insert_route_for_test(preview, route_for(ENVIRONMENT, |_, _| {}));
    f.sign_in_gate(preview);

    let reply = send(&f.lb, &get_on(preview, "/"), CLEAN_IP).await;

    reply.assert_answered("preview_not_found", "404");
    assert!(!reply.ctx.gated_host, "the gate must not run");
    assert!(reply.ctx.project.is_none(), "project routing must not run");
}

/// A status page's hostname stays one, even shaped like a sandbox preview.
#[tokio::test]
async fn status_page_host_answers_before_a_preview_of_the_same_name() {
    let f = fixture();
    let host = "ws-7702c56bfb804b49-3000.localho.st";
    f.routes.insert_status_page_host_for_test(host, None);

    send(&f.lb, &get_on(host, "/anything"), CLEAN_IP)
        .await
        .assert_answered("status_page_host_not_found", "404");
}

/// The public ingress hostname is never a preview, even shaped like one.
#[tokio::test]
async fn public_ingress_host_is_never_treated_as_a_preview() {
    let f = fixture();
    let ingress = "ws-7702c56bfb804b49-3000.localho.st";
    f.routes.reserve_public_ingress_hostname_for_test(ingress);

    let reply = send(&f.lb, &get_on(ingress, "/"), CLEAN_IP).await;

    assert_ne!(reply.status(), "preview_not_found");
    assert_eq!(reply.ctx.preview_route, None);
}

// ── Signed-in gate and wake ─────────────────────────────────────────────────

fn signed_in_policy() -> GatePolicy {
    let key = temps_core::gate::generate_signing_key().expect("signing key");
    GatePolicy {
        address_id: "custom:7".into(),
        gate: GateKind::Temps,
        temps: Some(TempsGate {
            public_key: temps_core::gate::encode_public_key(&key.verifying_key()),
            authorize_url: "https://console.example.test/_temps/gate/authorize".into(),
            not_before: 0,
            revoked: vec![],
        }),
        access: None,
    }
}

/// A visitor who has not signed in must not be able to wake the environment
/// behind a signed-in address.
#[tokio::test]
async fn gate_answers_before_a_sleeping_environment_wakes() {
    let lifecycle = Arc::new(CountingLifecycle::default());
    let (mut f, manager) = fixture().with_on_demand(lifecycle.clone());
    manager.register_sleeping_domain(HOST.to_string(), sleeping(ENVIRONMENT));
    f.sign_in_gate(HOST);

    let reply = send(&f.lb, &get("/"), CLEAN_IP).await;

    assert!(reply.answered, "the gate answers: {}", reply.head);
    assert!(reply.ctx.gated_host);
    assert!(
        !reply.status().starts_with("wake"),
        "the gate must answer, not the wake path: {}",
        reply.status()
    );
    assert_eq!(lifecycle.starts.load(Ordering::SeqCst), 0);
}

/// The wake path parks requests; without a slot it must refuse, not park.
#[tokio::test]
async fn a_wake_slot_is_reserved_before_a_sleeping_host_parks_the_request() {
    let lifecycle = Arc::new(CountingLifecycle::default());
    let (f, manager) = fixture().with_on_demand(lifecycle.clone());
    manager.register_sleeping_domain(HOST.to_string(), sleeping(ENVIRONMENT));
    let _held = all_wake_slots(&manager);

    let reply = send(&f.lb, &get("/"), CLEAN_IP).await;

    reply.assert_answered("wake_throttled", "503");
    assert_eq!(lifecycle.starts.load(Ordering::SeqCst), 0);
}

/// Sleeping environments have no route, so no project check can run before
/// the wake: a request policy that denies everyone does not stop it.
#[tokio::test]
async fn a_sleeping_host_is_woken_before_any_project_check_can_run() {
    let lifecycle = Arc::new(CountingLifecycle::default());
    let (mut f, manager) = fixture().with_on_demand(lifecycle);
    manager.register_sleeping_domain(HOST.to_string(), sleeping(ENVIRONMENT));
    f.fix_request_policy(POLICY_DENY);

    let reply = send(&f.lb, &get("/"), CLEAN_IP).await;

    // The mock database has no such environment, so the wake fails.
    reply.assert_answered("wake_failed", "503");
}

/// An awake route wins over a sleeping wildcard, so a host belonging to
/// another project cannot wake the wildcard's environment.
#[tokio::test]
async fn an_awake_route_wins_over_a_sleeping_wildcard() {
    let lifecycle = Arc::new(CountingLifecycle::default());
    let (f, manager) = fixture().with_on_demand(lifecycle.clone());
    manager.register_sleeping_domain("*.example.test".to_string(), sleeping(OTHER_ENVIRONMENT));
    f.route_host(|_, _| {});

    let reply = send(&f.lb, &get("/"), CLEAN_IP).await;

    reply.assert_forwarded("routed");
    assert_eq!(lifecycle.starts.load(Ordering::SeqCst), 0);
}

// ── Path routes ─────────────────────────────────────────────────────────────

fn path_route(target: Option<RouteInfo>, sleeping_environment_id: Option<i32>) -> PathRoute {
    PathRoute {
        prefix: "/api".to_string(),
        strip_prefix: false,
        target_service: "api".to_string(),
        target,
        sleeping_environment_id,
    }
}

/// A path route without a target answers 502: the host's own project, whose
/// checks would pass, must not serve the path in the target's place.
#[tokio::test]
async fn a_path_route_without_a_target_answers_before_the_hosts_own_project() {
    let f = fixture();
    f.route_host(|_, _| {});
    f.routes
        .insert_path_routes_for_test(ENVIRONMENT, vec![path_route(None, None)]);

    let reply = send(&f.lb, &get("/api/users"), CLEAN_IP).await;

    reply.assert_answered("path_route_unavailable", "502");
    assert!(reply.ctx.project.is_none());
}

/// A sleeping path target gets a wake slot before the request parks.
#[tokio::test]
async fn a_wake_slot_is_reserved_before_a_sleeping_path_target_parks_the_request() {
    let lifecycle = Arc::new(CountingLifecycle::default());
    let (f, manager) = fixture().with_on_demand(lifecycle.clone());
    f.route_host(|_, _| {});
    f.routes
        .insert_path_routes_for_test(ENVIRONMENT, vec![path_route(None, Some(OTHER_ENVIRONMENT))]);
    manager.register_sleeping_domain(
        "sleeping-target.example.test".to_string(),
        sleeping(OTHER_ENVIRONMENT),
    );
    let _held = all_wake_slots(&manager);

    let reply = send(&f.lb, &get("/api/users"), CLEAN_IP).await;

    reply.assert_answered("wake_throttled", "503");
    assert_eq!(lifecycle.starts.load(Ordering::SeqCst), 0);
}

/// A routed path is policed as its target: a target that refuses the client
/// answers even though the host's own environment would admit it.
#[tokio::test]
async fn a_routed_path_is_policed_by_its_target_not_the_host() {
    let mut f = fixture();
    f.lb.project_ip_gate = Arc::new(DenyEnvironment(Some(OTHER_ENVIRONMENT)));
    f.route_host(|_, _| {});
    f.routes.insert_path_routes_for_test(
        ENVIRONMENT,
        vec![path_route(
            Some(route_for(OTHER_ENVIRONMENT, |_, _| {})),
            None,
        )],
    );

    send(&f.lb, &get("/"), CLEAN_IP)
        .await
        .assert_forwarded("routed");
    send(&f.lb, &get("/api/users"), CLEAN_IP)
        .await
        .assert_answered("project_ip_restricted", "403");
}

// ── The project chain ───────────────────────────────────────────────────────

#[tokio::test]
async fn connection_cap_answers_before_the_request_policy() {
    let mut f = fixture();
    f.fix_request_policy(POLICY_DENY);
    f.route_host(|_, env| {
        env.deployment_config = Some(DeploymentConfig {
            max_concurrent_connections: Some(1),
            ..Default::default()
        });
    });
    let _held =
        f.lb.connection_limiter
            .try_acquire(ENVIRONMENT, 1)
            .expect("free slot");

    send(&f.lb, &get("/"), CLEAN_IP)
        .await
        .assert_answered("connection_limit_exceeded", "503");
}

#[tokio::test]
async fn request_policy_answers_before_edge_rules() {
    let mut f = fixture().with_edge_rules(block_rule());
    f.fix_request_policy(POLICY_DENY);
    f.route_host(|_, _| {});
    send(&f.lb, &get("/"), CLEAN_IP)
        .await
        .assert_answered("request_policy_denied", "403");

    f.fix_request_policy(RequestPolicyDecision::Unavailable { reason: "test" });
    send(&f.lb, &get("/"), CLEAN_IP)
        .await
        .assert_answered("request_policy_unavailable", "503");
}

/// The IP restriction is decided with the request policy, but answered with
/// the access decision, after the edge rules.
#[tokio::test]
async fn edge_rules_answer_before_the_ip_restriction_and_rate_limit() {
    let mut f = fixture().with_edge_rules(block_rule());
    f.deny_all_ips();
    f.route_host(|_, env| rate_limited(env));

    send(&f.lb, &get("/"), CLEAN_IP)
        .await
        .assert_answered("edge_rule_blocked", "403");
}

/// `allow` only ends the edge rules: every later check still runs.
#[tokio::test]
async fn an_edge_rule_allow_skips_none_of_the_later_checks() {
    let mut f = fixture().with_edge_rules(allow_rule());
    f.deny_all_ips();
    f.route_host(|_, _| {});
    send(&f.lb, &get("/"), CLEAN_IP)
        .await
        .assert_answered("project_ip_restricted", "403");

    let f = fixture().with_edge_rules(allow_rule());
    f.route_host(|_, env| rate_limited(env));
    send(&f.lb, &get("/"), CLEAN_IP)
        .await
        .assert_forwarded("routed");
    send(&f.lb, &get("/"), CLEAN_IP)
        .await
        .assert_answered("rate_limited", "429");

    let f = fixture().with_edge_rules(allow_rule());
    f.route_host(|project, _| project.shield_mode = "selective".to_string());
    send(&f.lb, &get("/"), SHIELD_BLOCKED_IP)
        .await
        .assert_answered("shield_block", "403");

    let f = fixture().with_edge_rules(allow_rule());
    f.route_host(|_, env| password_walled(env));
    send(&f.lb, &get("/"), CLEAN_IP)
        .await
        .assert_answered("password_wall", "200");
}

/// A rule must not stop visitors from unlocking a wall; with no wall the path
/// is the app's and the rules apply to it.
#[tokio::test]
async fn edge_rules_skip_the_password_form_post_only_when_there_is_a_wall() {
    let f = fixture().with_edge_rules(block_rule());
    f.route_host(|_, env| password_walled(env));
    send(
        &f.lb,
        &post(PASSWORD_VERIFY_PATH, "password=wrong"),
        CLEAN_IP,
    )
    .await
    .assert_answered("password_wrong", "200");

    let f = fixture().with_edge_rules(block_rule());
    f.route_host(|_, _| {});
    send(
        &f.lb,
        &post(PASSWORD_VERIFY_PATH, "password=wrong"),
        CLEAN_IP,
    )
    .await
    .assert_answered("edge_rule_blocked", "403");
}

#[tokio::test]
async fn ip_restriction_answers_before_the_rate_limit() {
    let mut f = fixture();
    f.deny_all_ips();
    f.route_host(|_, env| rate_limited(env));
    for _ in 0..3 {
        send(&f.lb, &get("/"), CLEAN_IP)
            .await
            .assert_answered("project_ip_restricted", "403");
    }
}

/// The verify endpoint, the solver files and a stored ACME challenge are
/// exempt from Shield, never from the operator's IP restriction.
#[tokio::test]
async fn ip_restriction_answers_before_shields_paths_and_stored_challenges() {
    let mut f = fixture();
    f.deny_all_ips();
    f.route_host(|_, _| {});
    for path in [
        crate::shield::VERIFY_PATH,
        temps_shield::assets::SOLVER_JS_PATH,
        temps_shield::assets::SOLVER_WASM_PATH,
        ACME_PATH,
    ] {
        send(&f.lb, &get(path), CLEAN_IP)
            .await
            .assert_answered("project_ip_restricted", "403");
    }
}

/// A spent rate budget is not a Shield decision.
#[tokio::test]
async fn rate_limit_answers_before_shield() {
    let f = fixture();
    f.route_host(|project, env| {
        project.shield_mode = "selective".to_string();
        rate_limited(env);
    });
    send(&f.lb, &get("/"), SHIELD_BLOCKED_IP)
        .await
        .assert_answered("shield_block", "403");
    send(&f.lb, &get("/"), SHIELD_BLOCKED_IP)
        .await
        .assert_answered("rate_limited", "429");
}

/// Publishes one compiled endpoint for the environment, carrying `stored`,
/// and makes the proxy decide with it.
fn enforce_endpoint(
    f: &mut Fixture,
    env: &environments::Model,
    stored: Vec<temps_workload_ingress::AccessPolicy>,
) {
    use temps_routes::ingress::{environment_endpoint, EnvironmentAccess, IngressSnapshot};
    let project = project(PROJECT);
    let instance = temps_config::RateLimitSettings::default();
    let access = EnvironmentAccess {
        environment: env,
        project: &project,
        instance_rate_limiting: &instance,
        ip_rules: &[],
    };
    f.routes
        .set_ingress_snapshot_for_test(IngressSnapshot::build(
            vec![environment_endpoint(1, &access, None, stored)],
            Default::default(),
            Default::default(),
        ));
    f.lb.route_table = Some(f.routes.clone());
    f.lb.project_ip_gate = Arc::new(ReadsRuleTable);
}

/// An IP gate the compiled endpoint may stand in for.
struct ReadsRuleTable;

impl ProjectIpGate for ReadsRuleTable {
    fn is_allowed(&self, _project_id: i32, _environment_id: i32, _ip: IpAddr) -> bool {
        true
    }

    fn reads_project_ip_access_rules(&self) -> bool {
        true
    }
}

fn temps_login() -> temps_workload_ingress::AccessPolicy {
    temps_workload_ingress::AccessPolicy::TempsLogin {
        permission: temps_routes::ingress::ENVIRONMENT_LOGIN_PERMISSION.into(),
    }
}

fn enforcing_login_route(f: &mut Fixture) {
    let mut env = environment(ENVIRONMENT, PROJECT);
    env.ingress_mode = IngressMode::Enforce;
    enforce_endpoint(f, &env, vec![temps_login()]);
    f.route_host(|project, env| {
        project.shield_mode = "selective".to_string();
        env.ingress_mode = IngressMode::Enforce;
    });
}

#[tokio::test]
async fn a_temps_login_answers_before_shield() {
    let mut f = fixture();
    enforcing_login_route(&mut f);
    send(&f.lb, &get("/"), SHIELD_BLOCKED_IP)
        .await
        .assert_answered("ingress_login_required", "403");
}

/// The challenge page must load and verify before sign-in: a login leaves
/// Shield's own paths to Shield, and nothing else.
#[tokio::test]
async fn a_temps_login_leaves_shields_own_paths_to_shield() {
    let mut f = fixture();
    enforcing_login_route(&mut f);
    let reply = send(
        &f.lb,
        &get(temps_shield::assets::SOLVER_JS_PATH),
        SHIELD_BLOCKED_IP,
    )
    .await;
    assert!(reply.answered);
    assert!(reply.head.starts_with("HTTP/1.1 200"), "{}", reply.head);
    assert_ne!(reply.status(), "ingress_login_required");
}

/// A compiled endpoint the snapshot cannot provide refuses the request:
/// nothing after it may serve an environment whose policy is unknown.
#[tokio::test]
async fn an_unavailable_ingress_endpoint_answers_before_shield_and_the_upstream() {
    let mut f = fixture();
    f.lb.route_table = Some(f.routes.clone());
    f.lb.project_ip_gate = Arc::new(ReadsRuleTable);
    f.route_host(|project, env| {
        project.shield_mode = "selective".to_string();
        env.ingress_mode = IngressMode::Enforce;
    });
    send(&f.lb, &get("/"), SHIELD_BLOCKED_IP)
        .await
        .assert_answered("ingress_endpoint_unavailable", "503");
}

/// Let's Encrypt never solves a challenge, and never unlocks a wall.
#[tokio::test]
async fn a_stored_challenge_answers_before_shield_and_the_password_wall() {
    let f = fixture();
    f.route_host(|project, env| {
        project.shield_mode = "selective".to_string();
        password_walled(env);
    });
    let reply = send(&f.lb, &get(ACME_PATH), SHIELD_BLOCKED_IP).await;
    reply.assert_answered("acme_challenge", "200");
    assert_eq!(reply.body, "stored-token.thumbprint");
}

/// Only a stored challenge skips Shield: the prefix itself is no way past it.
#[tokio::test]
async fn an_unstored_challenge_path_meets_shield() {
    let f = fixture();
    f.route_host(|project, _| project.shield_mode = "selective".to_string());
    send(
        &f.lb,
        &get("/.well-known/acme-challenge/not-stored"),
        SHIELD_BLOCKED_IP,
    )
    .await
    .assert_answered("shield_block", "403");
}

/// The challenge page's own files are answered before Shield could challenge
/// the request that loads them, and before the wall behind it.
#[tokio::test]
async fn solver_files_answer_before_shield_and_the_password_wall() {
    let f = fixture();
    f.route_host(|project, env| {
        project.shield_mode = "selective".to_string();
        password_walled(env);
    });
    let reply = send(
        &f.lb,
        &get(temps_shield::assets::SOLVER_JS_PATH),
        SHIELD_BLOCKED_IP,
    )
    .await;
    assert!(reply.answered);
    assert!(reply.head.starts_with("HTTP/1.1 200"), "{}", reply.head);
}

#[tokio::test]
async fn shield_answers_before_the_password_wall() {
    let f = fixture();
    f.route_host(|project, env| {
        project.shield_mode = "selective".to_string();
        password_walled(env);
    });
    send(&f.lb, &get("/"), SHIELD_BLOCKED_IP)
        .await
        .assert_answered("shield_block", "403");
    send(&f.lb, &get("/"), CLEAN_IP)
        .await
        .assert_answered("password_wall", "200");
}

/// Guesses are limited before the password is checked: even the right
/// password is refused once the allowance is spent.
#[tokio::test]
async fn a_password_guess_is_counted_before_the_password_is_checked() {
    use argon2::PasswordHasher;
    let hash = argon2::Argon2::default()
        .hash_password(b"right")
        .expect("hash")
        .to_string();
    let f = fixture();
    let walled_hash = hash.clone();
    f.route_host(move |_, env| password_walled_with(env, &walled_hash));
    let client: IpAddr = CLEAN_IP.parse().expect("ip");
    while f
        .lb
        .preview_auth_limiter
        .try_admit_password_wall(client, ENVIRONMENT)
    {}

    send(
        &f.lb,
        &post(PASSWORD_VERIFY_PATH, "password=right"),
        CLEAN_IP,
    )
    .await
    .assert_answered("password_rate_limited", "429");

    f.lb.preview_auth_limiter
        .clear_password_wall(client, ENVIRONMENT);
    send(
        &f.lb,
        &post(PASSWORD_VERIFY_PATH, "password=right"),
        CLEAN_IP,
    )
    .await
    .assert_answered("password_verified", "303");
}

/// The wall decides first; a cookie opens it for the HTTPS redirect after it.
#[tokio::test]
async fn the_password_wall_answers_before_the_https_redirect() {
    let f = fixture();
    f.route_host(|_, env| {
        password_walled(env);
        env.force_https = Some(true);
    });
    send(&f.lb, &get("/"), CLEAN_IP)
        .await
        .assert_answered("password_wall", "200");

    let cookie = crate::handler::password_wall::build_set_cookie_header(
        ENVIRONMENT,
        "not-a-real-hash",
        HOST,
    );
    let pair = cookie.split(';').next().expect("cookie pair");
    let request = format!("GET / HTTP/1.1\r\nHost: {HOST}\r\nCookie: {pair}\r\n\r\n");
    send(&f.lb, &request, CLEAN_IP)
        .await
        .assert_answered("http_to_https_redirect", "301");
}

// ── After the project chain ─────────────────────────────────────────────────

fn redirect_route() -> RouteInfo {
    RouteInfo {
        backend: upstream(),
        redirect_to: Some("https://new.example.test".to_string()),
        status_code: Some(301),
        project: None,
        environment: None,
        deployment: None,
        cert_eligible: false,
    }
}

/// A domain configured as a redirect can still prove ownership.
#[tokio::test]
async fn a_stored_challenge_answers_before_the_domain_redirect() {
    let f = fixture();
    f.routes.insert_route_for_test(HOST, redirect_route());
    let reply = send(&f.lb, &get(ACME_PATH), CLEAN_IP).await;
    reply.assert_answered("acme_challenge", "200");
    send(&f.lb, &get("/"), CLEAN_IP)
        .await
        .assert_answered("redirected", "301");
}

struct NeverIssues;

#[async_trait]
impl OnDemandCertProvisioner for NeverIssues {
    async fn provision(&self, _hostname: &str, _email: &str) -> Result<(), CertProvisionFailure> {
        Ok(())
    }
}

/// A host whose certificate is still provisioning gets the 503, not a
/// redirect to a certificate that does not exist; a challenge still completes.
#[tokio::test]
async fn on_demand_tls_answers_after_the_challenge_and_before_the_https_redirect() {
    let f = fixture();
    f.route_host(|_, env| env.force_https = Some(true));
    let manager = OnDemandCertManager::new(
        OnDemandCertConfig {
            zone: "example.test".to_string(),
            email: "ops@example.test".to_string(),
            max_concurrent: 1,
            hourly_cap: 1,
            console_host: None,
        },
        f.routes.clone(),
        Arc::new(NeverIssues),
    );
    manager.seed_state(vec![(HOST.to_string(), OnDemandCertState::Pending)]);
    let lb = f.lb.with_on_demand_cert_manager(manager, f.routes.clone());

    send(&lb, &get("/"), CLEAN_IP)
        .await
        .assert_answered("on_demand_cert_provisioning", "503");
    send(&lb, &get(ACME_PATH), CLEAN_IP)
        .await
        .assert_answered("acme_challenge", "200");
}

#[tokio::test]
async fn the_https_redirect_answers_before_the_domain_redirect() {
    let f = fixture();
    let mut route = route_for(ENVIRONMENT, |_, env| env.force_https = Some(true));
    route.redirect_to = Some("https://new.example.test".to_string());
    route.status_code = Some(301);
    f.routes.insert_route_for_test(HOST, route);
    send(&f.lb, &get("/"), CLEAN_IP)
        .await
        .assert_answered("http_to_https_redirect", "301");
}

/// The redirect keeps a request from ever reaching the upstream preparation.
#[tokio::test]
async fn a_redirect_answers_before_the_upstream_request_is_prepared() {
    let f = fixture();
    f.routes.insert_route_for_test(HOST, redirect_route());
    let reply = send(&f.lb, &get("/"), CLEAN_IP).await;
    reply.assert_answered("redirected", "301");
    assert!(reply.ctx.request_headers.is_none());
    assert_eq!(reply.upstream_header("x-forwarded-for"), None);
}

fn admin_gated(lb: LoadBalancer) -> LoadBalancer {
    let hosts = ["admin.example.test".to_string()];
    lb.with_admin_gate(AdminGateHandle::new(
        AdminGateConfig::from_parts(&[], &hosts, false, AdminGateSource::Db)
            .expect("admin gate config"),
    ))
}

/// The admin gate decides only what is about to reach the console: whatever
/// the proxy answers first is unaffected by it.
#[tokio::test]
async fn the_admin_gate_runs_after_everything_else_on_a_host_without_a_route() {
    let f = fixture();
    let lb = admin_gated(f.lb);
    send(&lb, &get("/"), CLEAN_IP)
        .await
        .assert_answered("admin_gate_denied", "404");
    send(&lb, &get(ACME_PATH), CLEAN_IP)
        .await
        .assert_answered("acme_challenge", "200");
    let solver = send(&lb, &get(temps_shield::assets::SOLVER_JS_PATH), CLEAN_IP).await;
    assert!(solver.answered);
    assert!(solver.head.starts_with("HTTP/1.1 200"), "{}", solver.head);
}

/// A host with a route never meets the admin gate.
#[tokio::test]
async fn the_admin_gate_does_not_consider_a_host_with_a_route() {
    let f = fixture();
    f.route_host(|_, _| {});
    let lb = admin_gated(f.lb);
    send(&lb, &get("/"), CLEAN_IP)
        .await
        .assert_forwarded("routed");
}

/// A denied client with a public-only listener available is pinned to it.
#[tokio::test]
async fn the_admin_gate_hands_a_denied_client_to_the_public_only_listener() {
    let f = fixture_with(ProxyConfig {
        console_ingress_address: Some("127.0.0.1:1".to_string()),
        ..ProxyConfig::default()
    });
    let lb = admin_gated(f.lb);
    let reply = send(&lb, &get("/"), CLEAN_IP).await;
    reply.assert_forwarded("no_project");
    assert!(reply.ctx.public_surface_only);
}

#[tokio::test]
async fn hidden_platform_identity_preserves_application_headers() {
    for hide in [false, true] {
        let fixture = fixture_with(ProxyConfig::default());
        fixture
            .lb
            .hide_platform_identity
            .store(hide, Ordering::Relaxed);
        let (mut client, server) = tokio::io::duplex(4096);
        client.write_all(get("/").as_bytes()).await.unwrap();
        let mut session = Session::new_h1(Box::new(server) as pingora_core::protocols::Stream);
        assert!(session.read_request().await.unwrap());
        let mut ctx = fixture.lb.new_ctx();
        let mut response = pingora_http::ResponseHeader::build(200, None).unwrap();
        for (name, value) in [
            ("Server", "application-server"),
            ("X-Powered-By", "application"),
            ("X-Upstream-Custom", "app-diagnostic"),
            ("X-Temps-Custom", "app-value"),
        ] {
            response.insert_header(name, value).unwrap();
        }
        fixture
            .lb
            .upstream_response_filter(&mut session, &mut response, &mut ctx)
            .await
            .unwrap();
        for (name, value) in [
            ("Server", "application-server"),
            ("X-Powered-By", "application"),
            ("X-Upstream-Custom", "app-diagnostic"),
            ("X-Temps-Custom", "app-value"),
        ] {
            assert_eq!(response.headers.get(name).unwrap(), value);
        }
        assert_eq!(
            response.headers.contains_key("X-Temps-Proxy-Probe-Capable"),
            !hide
        );
        ctx.upstream_write_pending_time_ms = Some(12);
        fixture.lb.add_response_timing(&mut response, &ctx).unwrap();
        assert_eq!(
            response.headers.contains_key("X-Upstream-Write-Pending"),
            !hide
        );
        assert_eq!(response.headers.contains_key("X-Response-Time"), !hide);
    }
}

#[tokio::test]
async fn hidden_platform_identity_keeps_app_timing_headers() {
    for hide in [false, true] {
        let fixture = fixture_with(ProxyConfig::default());
        fixture
            .lb
            .hide_platform_identity
            .store(hide, Ordering::Relaxed);
        let mut response = pingora_http::ResponseHeader::build(200, None).unwrap();
        response
            .insert_header("X-Upstream-Write-Pending", "app-owned")
            .unwrap();
        response
            .insert_header("X-Response-Time", "app-owned")
            .unwrap();
        fixture
            .lb
            .add_response_timing(&mut response, &fixture.lb.new_ctx())
            .unwrap();
        assert_eq!(
            response.headers.get("X-Upstream-Write-Pending").unwrap(),
            "app-owned"
        );
        assert_eq!(
            response.headers.get("X-Response-Time").unwrap(),
            "app-owned"
        );
    }
}

#[cfg(unix)]
#[tokio::test]
async fn hidden_platform_identity_requires_authentication_even_on_loopback() {
    let fixture = fixture_with(ProxyConfig::default());
    fixture
        .lb
        .hide_platform_identity
        .store(true, Ordering::Relaxed);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let mut client = tokio::net::TcpStream::connect(listener.local_addr().unwrap())
        .await
        .unwrap();
    let (server, _) = listener.accept().await.unwrap();
    client.write_all(get("/").as_bytes()).await.unwrap();
    use pingora_core::protocols::GetSocketDigest;
    use std::os::fd::AsRawFd;
    let digest = pingora_core::protocols::SocketDigest::from_raw_fd(server.as_raw_fd());
    let mut stream = pingora_core::protocols::l4::stream::Stream::from(server);
    stream.set_socket_digest(digest);
    let mut session = Session::new_h1(Box::new(stream));
    assert!(session.read_request().await.unwrap());
    assert!(!fixture.lb.expose_probe_markers(&session));
    session
        .req_header_mut()
        .insert_header("User-Agent", "Temps-Reachability/1.0")
        .unwrap();
    assert!(!fixture.lb.expose_probe_markers(&session));
    session
        .req_header_mut()
        .insert_header(temps_core::probe_auth::HEADER, "invalid")
        .unwrap();
    assert!(!fixture.lb.expose_probe_markers(&session));
    let host = session
        .req_header()
        .headers
        .get("host")
        .unwrap()
        .to_str()
        .unwrap();
    let token = temps_core::probe_auth::sign(
        &fixture.lb.config_service.get_server_config().encryption_key,
        host,
        temps_core::probe_auth::now(),
    );
    session
        .req_header_mut()
        .insert_header(temps_core::probe_auth::HEADER, token)
        .unwrap();
    assert!(fixture.lb.expose_probe_markers(&session));
    let mut ctx = fixture.lb.new_ctx();
    let mut upstream = pingora_http::RequestHeader::build("GET", b"/", None).unwrap();
    upstream
        .insert_header(temps_core::probe_auth::HEADER, "credential")
        .unwrap();
    upstream.insert_header("X-App-Header", "preserved").unwrap();
    fixture
        .lb
        .upstream_request_filter(&mut session, &mut upstream, &mut ctx)
        .await
        .unwrap();
    assert!(!upstream
        .headers
        .contains_key(temps_core::probe_auth::HEADER));
    assert_eq!(upstream.headers.get("X-App-Header").unwrap(), "preserved");
    let mut app_response = pingora_http::ResponseHeader::build(200, None).unwrap();
    app_response.insert_header("Server", "app-owned").unwrap();
    fixture
        .lb
        .upstream_response_filter(&mut session, &mut app_response, &mut ctx)
        .await
        .unwrap();
    assert_eq!(
        app_response
            .headers
            .get("X-Temps-Proxy-Probe-Capable")
            .unwrap(),
        "1"
    );
    assert_eq!(app_response.headers.get("Server").unwrap(), "app-owned");
    let mut response = crate::proxy::https_redirect_response(
        "https://app.example.test/",
        "probe-request",
        fixture.lb.expose_probe_markers(&session),
    )
    .unwrap();
    fixture
        .lb
        .protect_probe_response(&session, &mut response)
        .unwrap();
    assert_eq!(
        response.headers.get("cache-control").unwrap(),
        "private, no-store"
    );
    assert_eq!(
        response.headers.get("vary").unwrap(),
        temps_core::probe_auth::HEADER
    );
    assert_eq!(
        response
            .headers
            .get("X-Temps-Proxy-Https-Redirect")
            .unwrap(),
        "1"
    );
    assert_eq!(
        response.headers.get("X-Temps-Proxy-Probe-Capable").unwrap(),
        "1"
    );
}

#[tokio::test]
async fn platform_identity_controls_error_and_challenge_responses() {
    for hide in [false, true] {
        for outcome in [
            None,
            Some(temps_shield::Outcome::HttpsRequired),
            Some(temps_shield::Outcome::Block),
            Some(temps_shield::Outcome::Unavailable),
            Some(temps_shield::Outcome::Challenge {
                token: "token".to_string(),
                difficulty: 20,
            }),
        ] {
            let fixture = fixture_with(ProxyConfig::default());
            fixture
                .lb
                .hide_platform_identity
                .store(hide, Ordering::Relaxed);
            let (mut client, server) = tokio::io::duplex(65536);
            client.write_all(get("/").as_bytes()).await.unwrap();
            let mut session = Session::new_h1(Box::new(server) as pingora_core::protocols::Stream);
            assert!(session.read_request().await.unwrap());
            let mut ctx = fixture.lb.new_ctx();
            let failed_upstream = outcome.is_none();
            ctx.shield_header = Some("challenge");
            if let Some(outcome) = outcome {
                fixture
                    .lb
                    .answer_shield(&mut session, &mut ctx, outcome, None)
                    .await
                    .unwrap();
            } else {
                fixture
                    .lb
                    .fail_to_proxy(
                        &mut session,
                        &pingora::Error::new(pingora::ErrorType::ConnectRefused),
                        &mut ctx,
                    )
                    .await;
            }
            drop(session);
            let mut raw = Vec::new();
            client.read_to_end(&mut raw).await.unwrap();
            let response = String::from_utf8(raw).unwrap();
            assert!(response
                .to_lowercase()
                .contains(&format!("x-request-id: {}", ctx.request_id)));
            if hide {
                assert!(!response.to_lowercase().contains("temps"), "{response}");
            } else if failed_upstream {
                assert!(response.contains("Temps"));
            }
        }
    }
}

#[tokio::test]
async fn hidden_identity_replaces_cached_upstream_responses_without_changing_app_headers() {
    for hide in [false, true] {
        for method in ["GET", "HEAD", "PUT"] {
            let f = fixture();
            f.lb.hide_platform_identity.store(hide, Ordering::Relaxed);
            let (mut client, server) = tokio::io::duplex(4096);
            client.write_all(get("/").as_bytes()).await.unwrap();
            let mut session = Session::new_h1(Box::new(server) as pingora_core::protocols::Stream);
            assert!(session.read_request().await.unwrap());
            let mut ctx = f.lb.new_ctx();
            ctx.project = Some(Arc::new(project(PROJECT)));
            ctx.upstream_host = Some("127.0.0.1:9000".into());
            let mut request =
                pingora_http::RequestHeader::build(method, b"/api/asset", None).unwrap();
            request.insert_header("If-None-Match", "\"v1\"").unwrap();
            request
                .insert_header("If-Modified-Since", "Wed, 21 Oct 2015 07:28:00 GMT")
                .unwrap();
            request
                .insert_header("If-Match", "\"write-version\"")
                .unwrap();
            request
                .insert_header("If-Unmodified-Since", "Wed, 21 Oct 2015 07:28:00 GMT")
                .unwrap();
            f.lb.upstream_request_filter(&mut session, &mut request, &mut ctx)
                .await
                .unwrap();
            let refresh = hide && method != "PUT";
            assert_eq!(request.headers.contains_key("If-None-Match"), !refresh);
            assert_eq!(request.headers.contains_key("If-Modified-Since"), !refresh);
            assert!(request.headers.contains_key("If-Match"));
            assert!(request.headers.contains_key("If-Unmodified-Since"));
            // Model an unchanged app that returns 304 only for a conditional read.
            if method != "PUT" {
                let status = if request.headers.contains_key("If-None-Match") {
                    304
                } else {
                    200
                };
                let mut response = pingora_http::ResponseHeader::build(status, None).unwrap();
                response.insert_header("ETag", "\"v1\"").unwrap();
                response
                    .insert_header("Last-Modified", "Wed, 21 Oct 2015 07:28:00 GMT")
                    .unwrap();
                response
                    .insert_header("Cache-Control", "public, max-age=0")
                    .unwrap();
                response.insert_header("Server", "application").unwrap();
                f.lb.upstream_response_filter(&mut session, &mut response, &mut ctx)
                    .await
                    .unwrap();
                assert_eq!(response.status.as_u16(), if hide { 200 } else { 304 });
                assert_eq!(response.headers.get("ETag").unwrap(), "\"v1\"");
                assert_eq!(
                    response.headers.get("Cache-Control").unwrap(),
                    "public, max-age=0"
                );
                assert_eq!(response.headers.get("Server").unwrap(), "application");
                assert_eq!(
                    response.headers.contains_key("X-Temps-Proxy-Probe-Capable"),
                    !hide
                );
            }
        }
    }
}

#[tokio::test]
async fn hidden_identity_preserves_console_and_public_api_conditional_reads() {
    for hide in [false, true] {
        for public_surface in [false, true] {
            for method in ["GET", "HEAD"] {
                let f = fixture_with(ProxyConfig {
                    console_ingress_address: Some("127.0.0.1:3001".into()),
                    ..ProxyConfig::default()
                });
                f.lb.hide_platform_identity.store(hide, Ordering::Relaxed);
                let (mut client, server) = tokio::io::duplex(4096);
                client
                    .write_all(get("/api/flags/snapshot").as_bytes())
                    .await
                    .unwrap();
                let mut session =
                    Session::new_h1(Box::new(server) as pingora_core::protocols::Stream);
                assert!(session.read_request().await.unwrap());
                let mut ctx = f.lb.new_ctx();
                ctx.public_surface_only = public_surface;
                // Resolve the real console peer, as Pingora does before filtering
                // its request. Cover both the full and public-only listeners.
                let _peer = f.lb.upstream_peer(&mut session, &mut ctx).await.unwrap();
                assert_eq!(
                    ctx.upstream_host.as_deref(),
                    Some(if public_surface {
                        "127.0.0.1:3001"
                    } else {
                        "127.0.0.1:3000"
                    })
                );
                let mut request =
                    RequestHeader::build(method, b"/api/flags/snapshot", None).unwrap();
                request
                    .insert_header("If-None-Match", "\"snapshot-v1\"")
                    .unwrap();
                request
                    .insert_header("If-Modified-Since", "Wed, 21 Oct 2015 07:28:00 GMT")
                    .unwrap();
                f.lb.upstream_request_filter(&mut session, &mut request, &mut ctx)
                    .await
                    .unwrap();
                assert_eq!(
                    request.headers.get("If-None-Match").unwrap(),
                    "\"snapshot-v1\""
                );
                assert_eq!(
                    request.headers.get("If-Modified-Since").unwrap(),
                    "Wed, 21 Oct 2015 07:28:00 GMT"
                );
                let mut response = pingora_http::ResponseHeader::build(304, None).unwrap();
                response.insert_header("ETag", "\"snapshot-v1\"").unwrap();
                f.lb.upstream_response_filter(&mut session, &mut response, &mut ctx)
                    .await
                    .unwrap();
                assert_eq!(response.status.as_u16(), 304);
                assert_eq!(response.headers.get("ETag").unwrap(), "\"snapshot-v1\"");
                assert_eq!(
                    response.headers.contains_key("X-Temps-Proxy-Probe-Capable"),
                    !hide
                );
            }
        }
    }
}
