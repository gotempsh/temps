// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Database-backed tests of monitors for targets outside Temps
//! (ADR 108 D3, D4): the service, the scheduler and the handlers.

use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use axum::body::Body;
use axum::extract::{Extension, Path, State};
use axum::http::{header, Request, StatusCode};
use axum::Json;
use chrono::Utc;
use sea_orm::{ActiveModelTrait, ColumnTrait, EntityTrait, QueryFilter, Set};
use temps_authz::permissions::Role;
use temps_authz::{AuthContext, RequireAuth};
use temps_config::ConfigService;
use temps_core::{AuditLogger, AuditOperation, EncryptionService, RequestMetadata};
use temps_entities::{projects, status_checks, status_monitors, users};
use temps_test_utils::{is_container_runtime_unavailable, TestDatabase};
use tower::ServiceExt;

use super::locations::{NodeLocation, NodeProbeError};
use super::probe::{ProbeOutcome, ProbeStatus, TargetProber};
use super::spec::{CheckRequest, CheckRequestHeader, CheckSpec, HttpCheck, Target, TcpCheck};
use super::{
    CreateTargetMonitorRequest, MonitorCheckKind, MonitorLocationResults, NoNodeLocations,
    ProbeLocations, ProbeLocationsResponse, TargetChecks, TargetMonitorError, TargetMonitorService,
    UpdateTargetMonitorRequest, MAX_TARGET_MONITORS_PER_PROJECT,
};
use crate::routes::target_monitors::{
    create_target_monitor, create_target_monitor_router, get_monitor_probe_locations,
    update_target_monitor, TargetMonitorRoutesState,
};
use crate::services::{HealthCheckService, MonitorResponse};
use temps_uptime_probe::RemoteProbeRequest;

const SECRET: &str = "Bearer tk-secret-value-1234";

#[derive(Default)]
struct RecordingAudit {
    entries: Mutex<Vec<(String, serde_json::Value)>>,
}

#[async_trait]
impl AuditLogger for RecordingAudit {
    async fn create_audit_log(
        &self,
        operation: &dyn AuditOperation,
    ) -> temps_core::AuditResult<()> {
        let payload = serde_json::from_str(&operation.serialize()?).expect("audit json");
        self.entries
            .lock()
            .expect("audit lock")
            .push((operation.operation_type(), payload));
        Ok(())
    }
}

/// Answers with a fixed outcome, records what it was asked to probe, and
/// can rename the monitor mid-probe to simulate an edit racing a check.
struct FakeProber {
    outcome: ProbeOutcome,
    seen: Mutex<Vec<(CheckSpec, Target, CheckRequest)>>,
    rename_during_probe: Mutex<Option<(Arc<sea_orm::DatabaseConnection>, i32)>>,
}

impl FakeProber {
    fn new(outcome: ProbeOutcome) -> Arc<Self> {
        Arc::new(Self {
            outcome,
            seen: Mutex::new(Vec::new()),
            rename_during_probe: Mutex::new(None),
        })
    }
}

#[async_trait]
impl TargetProber for FakeProber {
    async fn probe(
        &self,
        spec: &CheckSpec,
        target: &Target,
        request: &CheckRequest,
    ) -> ProbeOutcome {
        self.seen
            .lock()
            .expect("seen lock")
            .push((spec.clone(), target.clone(), request.clone()));
        let rename = self.rename_during_probe.lock().expect("rename lock").take();
        if let Some((db, monitor_id)) = rename {
            let model = status_monitors::Entity::find_by_id(monitor_id)
                .one(db.as_ref())
                .await
                .expect("load")
                .expect("monitor");
            let mut active: status_monitors::ActiveModel = model.into();
            active.name = Set("renamed while probing".to_string());
            active.update(db.as_ref()).await.expect("rename");
        }
        self.outcome.clone()
    }
}

struct Fx {
    _db: TestDatabase,
    db: Arc<sea_orm::DatabaseConnection>,
    encryption: Arc<EncryptionService>,
    project_id: i32,
    other_project_id: i32,
}

async fn project(db: &sea_orm::DatabaseConnection, slug: &str) -> i32 {
    projects::ActiveModel {
        name: Set(format!("Project {slug}")),
        slug: Set(slug.to_string()),
        directory: Set(slug.to_string()),
        main_branch: Set("main".to_string()),
        preset: Set(temps_entities::preset::Preset::Nixpacks),
        repo_name: Set("repo".to_string()),
        repo_owner: Set("owner".to_string()),
        ..Default::default()
    }
    .insert(db)
    .await
    .expect("project")
    .id
}

async fn fixture() -> Option<Fx> {
    let test_db = match TestDatabase::with_migrations().await {
        Ok(db) => db,
        Err(e) if is_container_runtime_unavailable(&e.to_string()) => {
            println!("Docker not available, skipping target monitor test");
            return None;
        }
        Err(e) => panic!("test database setup failed: {e}"),
    };
    let db = test_db.connection_arc();
    let nanos = Utc::now().timestamp_nanos_opt().unwrap_or(0);
    let project_id = project(db.as_ref(), &format!("targets-{nanos}")).await;
    let other_project_id = project(db.as_ref(), &format!("targets-other-{nanos}")).await;
    Some(Fx {
        _db: test_db,
        db,
        encryption: Arc::new(EncryptionService::new_from_password("target-monitor-tests")),
        project_id,
        other_project_id,
    })
}

impl Fx {
    fn checks(&self, prober: Arc<dyn TargetProber>) -> Arc<TargetChecks> {
        self.checks_from(prober, Arc::new(NoNodeLocations))
    }

    fn checks_from(
        &self,
        prober: Arc<dyn TargetProber>,
        locations: Arc<dyn ProbeLocations>,
    ) -> Arc<TargetChecks> {
        Arc::new(TargetChecks::new(
            self.encryption.clone(),
            prober,
            locations,
        ))
    }

    fn service(&self) -> TargetMonitorService {
        TargetMonitorService::new(
            self.db.clone(),
            self.checks(FakeProber::new(ProbeOutcome {
                status: ProbeStatus::Operational,
                response_time_ms: Some(1),
                message: None,
            })),
        )
        .without_address_checks()
    }

    fn routes(&self, audit: Arc<RecordingAudit>) -> Arc<TargetMonitorRoutesState> {
        Arc::new(TargetMonitorRoutesState {
            monitors: Arc::new(self.service()),
            audit,
            project_access_checker: None,
        })
    }
}

fn http_request(target: &str) -> CreateTargetMonitorRequest {
    CreateTargetMonitorRequest {
        name: "Payments API".to_string(),
        target: target.to_string(),
        check: CheckSpec::Http(HttpCheck {
            expected_status: vec!["200".to_string()],
            ..Default::default()
        }),
        request: Some(CheckRequest {
            headers: vec![CheckRequestHeader {
                name: "Authorization".to_string(),
                value: SECRET.to_string(),
            }],
            body: None,
        }),
        check_interval_seconds: Some(10),
        probe_from_nodes: false,
    }
}

fn auth(role: Role) -> RequireAuth {
    let now = Utc::now();
    RequireAuth(AuthContext::new_session(
        users::Model {
            id: 1,
            name: "Test User".to_string(),
            email: "admin@example.test".to_string(),
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
        },
        role,
    ))
}

/// An admin's API key bound to `project_id`.
fn scoped_key(project_id: i32) -> RequireAuth {
    let RequireAuth(session) = auth(Role::Admin);
    let user = session.user.clone().expect("session user");
    RequireAuth(
        AuthContext::new_api_key(user, Some(Role::Admin), None, "scoped".to_string(), 77)
            .with_api_key_scope(Some(temps_authz::ApiKeyScope {
                project_id,
                environment_id: None,
            })),
    )
}

fn metadata() -> Extension<RequestMetadata> {
    Extension(RequestMetadata {
        ip_address: "192.0.2.41".to_string(),
        user_agent: "target-monitor-test".to_string(),
        headers: axum::http::HeaderMap::new(),
        visitor_id_cookie: None,
        session_id_cookie: None,
        base_url: "http://localhost".to_string(),
        scheme: "http".to_string(),
        host: "localhost".to_string(),
        is_secure: false,
    })
}

#[tokio::test]
async fn http_monitor_stores_its_request_encrypted_and_never_returns_it() {
    let Some(fx) = fixture().await else { return };
    let model = fx
        .service()
        .create(
            fx.project_id,
            http_request("https://api.example.test/health?token=abc"),
        )
        .await
        .expect("create");

    assert_eq!(model.check_kind, "http");
    assert_eq!(model.environment_id, None);
    assert_eq!(model.check_interval_seconds, 30, "clamped to the floor");
    assert_eq!(model.next_check_at, None, "due on the next sweep");
    let encrypted = model.request_encrypted.clone().expect("encrypted request");
    assert!(!encrypted.contains(SECRET));
    let stored_spec = model.check_spec.clone().expect("spec").to_string();
    assert!(!stored_spec.contains(SECRET), "{stored_spec}");

    let response = MonitorResponse::from(model.clone());
    assert_eq!(response.check_kind, MonitorCheckKind::Http);
    assert_eq!(
        response.monitor_url,
        "https://api.example.test/health?token=abc"
    );
    let view = response.check.as_ref().expect("view");
    assert_eq!(view.header_names, vec!["Authorization".to_string()]);
    assert!(!view.has_body);
    let json = serde_json::to_string(&response).expect("json");
    assert!(!json.contains(SECRET), "{json}");

    let decoded = fx
        .checks(FakeProber::new(ProbeOutcome {
            status: ProbeStatus::Operational,
            response_time_ms: None,
            message: None,
        }))
        .decode(&model)
        .expect("decode");
    assert_eq!(decoded.request.headers[0].value, SECRET);
}

#[tokio::test]
async fn invalid_and_non_public_targets_are_refused() {
    let Some(fx) = fixture().await else { return };
    let service = fx.service();
    let mut bad = http_request("ftp://files.example.test/");
    let err = service
        .create(fx.project_id, bad.clone())
        .await
        .expect_err("scheme");
    assert!(
        matches!(err, TargetMonitorError::Config { project_id, .. } if project_id == fx.project_id)
    );
    assert!(err.to_string().contains("http://"), "{err}");

    bad.target = "https://api.example.test/".to_string();
    bad.name = " ".to_string();
    let err = service.create(fx.project_id, bad).await.expect_err("name");
    assert!(err.to_string().contains("name"), "{err}");

    // With address checks on, a loopback or metadata address is refused
    // before anything is stored. IP literals need no DNS.
    let checked =
        TargetMonitorService::new(fx.db.clone(), fx.checks(Arc::new(super::NetworkProber)));
    for target in [
        "http://127.0.0.1:8080/",
        "http://169.254.169.254/latest/meta-data",
    ] {
        let err = checked
            .create(fx.project_id, http_request(target))
            .await
            .expect_err(target);
        assert!(
            matches!(err, TargetMonitorError::NonPublicTarget { .. }),
            "{target}: {err}"
        );
    }
    let tcp = CreateTargetMonitorRequest {
        name: "Database".to_string(),
        target: "10.0.0.5:5432".to_string(),
        check: CheckSpec::Tcp(TcpCheck::default()),
        request: None,
        check_interval_seconds: None,
        probe_from_nodes: false,
    };
    assert!(matches!(
        checked.create(fx.project_id, tcp).await,
        Err(TargetMonitorError::NonPublicTarget { .. })
    ));
    let stored = status_monitors::Entity::find()
        .filter(status_monitors::Column::ProjectId.eq(fx.project_id))
        .all(fx.db.as_ref())
        .await
        .expect("list");
    assert!(stored.is_empty(), "nothing is stored for a refused monitor");

    let err = service
        .create(9_999_999, http_request("https://api.example.test/"))
        .await
        .expect_err("no project");
    assert!(matches!(
        err,
        TargetMonitorError::ProjectNotFound {
            project_id: 9_999_999
        }
    ));
}

#[tokio::test]
async fn a_project_has_a_bounded_number_of_target_monitors() {
    let Some(fx) = fixture().await else { return };
    let service = fx.service();
    for index in 0..MAX_TARGET_MONITORS_PER_PROJECT {
        service
            .create(
                fx.project_id,
                http_request(&format!("https://api{index}.example.test/")),
            )
            .await
            .expect("under the limit");
    }
    let err = service
        .create(
            fx.project_id,
            http_request("https://one-more.example.test/"),
        )
        .await
        .expect_err("over the limit");
    assert!(
        matches!(err, TargetMonitorError::TooMany { limit, .. } if limit == MAX_TARGET_MONITORS_PER_PROJECT)
    );
    // The limit is per project.
    service
        .create(
            fx.other_project_id,
            http_request("https://api.example.test/"),
        )
        .await
        .expect("other project");
}

#[tokio::test]
async fn updates_can_change_the_kind_and_drop_the_request() {
    let Some(fx) = fixture().await else { return };
    let service = fx.service();
    let created = service
        .create(fx.project_id, http_request("https://api.example.test/"))
        .await
        .expect("create");

    // Renaming keeps the encrypted request as it was.
    let renamed = service
        .update(
            fx.project_id,
            created.id,
            UpdateTargetMonitorRequest {
                name: Some("Payments".to_string()),
                ..Default::default()
            },
        )
        .await
        .expect("rename");
    assert_eq!(renamed.request_encrypted, created.request_encrypted);
    assert!(renamed.updated_at > created.updated_at);

    // A target that does not fit the new kind is refused.
    let err = service
        .update(
            fx.project_id,
            created.id,
            UpdateTargetMonitorRequest {
                check: Some(CheckSpec::Tcp(TcpCheck::default())),
                ..Default::default()
            },
        )
        .await
        .expect_err("a URL is not host:port");
    assert!(err.to_string().contains("target"), "{err}");

    let tcp = service
        .update(
            fx.project_id,
            created.id,
            UpdateTargetMonitorRequest {
                target: Some("db.example.test:5432".to_string()),
                check: Some(CheckSpec::Tcp(TcpCheck::default())),
                is_active: Some(false),
                ..Default::default()
            },
        )
        .await
        .expect("to tcp");
    assert_eq!(tcp.check_kind, "tcp");
    assert_eq!(tcp.target.as_deref(), Some("db.example.test:5432"));
    assert_eq!(tcp.request_encrypted, None, "a TCP check sends no request");
    assert!(!tcp.is_active);

    let err = service
        .update(
            fx.other_project_id,
            created.id,
            UpdateTargetMonitorRequest::default(),
        )
        .await
        .expect_err("other project");
    assert!(matches!(err, TargetMonitorError::NotFound { monitor_id } if monitor_id == created.id));
}

#[tokio::test]
async fn environment_monitors_are_not_changed_here_and_the_shapes_stay_apart() {
    let Some(fx) = fixture().await else { return };
    let environment_monitor = status_monitors::ActiveModel {
        project_id: Set(fx.project_id),
        environment_id: Set(None),
        name: Set("legacy".to_string()),
        monitor_type: Set("web".to_string()),
        check_interval_seconds: Set(60),
        is_active: Set(true),
        ..Default::default()
    }
    .insert(fx.db.as_ref())
    .await
    .expect("environment monitor");
    assert_eq!(
        environment_monitor.check_kind, "environment",
        "the column default"
    );
    let err = fx
        .service()
        .update(
            fx.project_id,
            environment_monitor.id,
            UpdateTargetMonitorRequest::default(),
        )
        .await
        .expect_err("environment monitor");
    assert!(matches!(err, TargetMonitorError::NotTargetMonitor { .. }));

    // The database refuses a target monitor without a spec, and an
    // environment monitor with a target.
    for (kind, target, spec) in [
        ("http", Some("https://api.example.test/"), None),
        ("environment", Some("https://api.example.test/"), None),
    ] {
        let result = status_monitors::ActiveModel {
            project_id: Set(fx.project_id),
            name: Set("bad".to_string()),
            monitor_type: Set("web".to_string()),
            check_interval_seconds: Set(60),
            is_active: Set(true),
            check_kind: Set(kind.to_string()),
            target: Set(target.map(str::to_string)),
            check_spec: Set(spec),
            ..Default::default()
        }
        .insert(fx.db.as_ref())
        .await;
        assert!(
            result.is_err(),
            "{kind} with target {target:?} must be refused"
        );
    }
}

async fn health_service(fx: &Fx, prober: Arc<FakeProber>) -> HealthCheckService {
    health_service_with(fx, fx.checks(prober)).await
}

async fn health_service_with(fx: &Fx, checks: Arc<TargetChecks>) -> HealthCheckService {
    let config = temps_config::ServerConfig::new(
        "127.0.0.1:3000".to_string(),
        "postgres://test:test@localhost/test".to_string(),
        None,
        None,
    )
    .expect("server config");
    let config_service = Arc::new(ConfigService::new(Arc::new(config), fx.db.clone()));
    HealthCheckService::new(
        fx.db.clone(),
        config_service,
        Arc::new(RecordingQueue::default()),
        checks,
    )
    .expect("service")
}

#[derive(Default)]
struct RecordingQueue {
    jobs: Mutex<Vec<temps_core::Job>>,
}

#[async_trait]
impl temps_core::JobQueue for RecordingQueue {
    async fn send(&self, job: temps_core::Job) -> Result<(), temps_core::QueueError> {
        self.jobs.lock().expect("jobs lock").push(job);
        Ok(())
    }
    fn subscribe(&self) -> Box<dyn temps_core::JobReceiver> {
        struct Never;
        #[async_trait]
        impl temps_core::JobReceiver for Never {
            async fn recv(&mut self) -> Result<temps_core::Job, temps_core::QueueError> {
                std::future::pending().await
            }
        }
        Box::new(Never)
    }
}

#[tokio::test]
async fn the_scheduler_probes_target_monitors_and_records_the_outcome() {
    let Some(fx) = fixture().await else { return };
    let monitor = fx
        .service()
        .create(fx.project_id, http_request("https://api.example.test/"))
        .await
        .expect("create");
    let prober = FakeProber::new(ProbeOutcome {
        status: ProbeStatus::Down,
        response_time_ms: Some(42),
        message: Some("HTTP 503, expected 200".to_string()),
    });
    let health = health_service(&fx, prober.clone()).await;
    health.run_all_checks().await.expect("sweep");

    let seen = prober.seen.lock().expect("seen").clone();
    assert_eq!(seen.len(), 1);
    assert_eq!(
        seen[0].2.headers[0].value, SECRET,
        "the prober gets the decrypted request"
    );
    let checks = status_checks::Entity::find()
        .filter(status_checks::Column::MonitorId.eq(monitor.id))
        .all(fx.db.as_ref())
        .await
        .expect("checks");
    assert_eq!(checks.len(), 1);
    assert_eq!(checks[0].status, "major_outage");
    assert_eq!(checks[0].response_time_ms, Some(42));
    assert_eq!(
        checks[0].error_message.as_deref(),
        Some("HTTP 503, expected 200")
    );
    let after = status_monitors::Entity::find_by_id(monitor.id)
        .one(fx.db.as_ref())
        .await
        .expect("load")
        .expect("monitor");
    assert!(after.next_check_at.is_some(), "the sweep claimed the slot");
}

#[tokio::test]
async fn a_check_of_settings_changed_mid_probe_is_discarded() {
    let Some(fx) = fixture().await else { return };
    let monitor = fx
        .service()
        .create(fx.project_id, http_request("https://api.example.test/"))
        .await
        .expect("create");
    let prober = FakeProber::new(ProbeOutcome {
        status: ProbeStatus::Operational,
        response_time_ms: Some(5),
        message: None,
    });
    *prober.rename_during_probe.lock().expect("rename") = Some((fx.db.clone(), monitor.id));
    let health = health_service(&fx, prober.clone()).await;
    health.run_all_checks().await.expect("sweep");

    assert_eq!(prober.seen.lock().expect("seen").len(), 1);
    let checks = status_checks::Entity::find()
        .filter(status_checks::Column::MonitorId.eq(monitor.id))
        .all(fx.db.as_ref())
        .await
        .expect("checks");
    assert!(checks.is_empty(), "the stale result was not written");
}

#[tokio::test]
async fn handlers_check_permissions_and_audit_without_secrets() {
    let Some(fx) = fixture().await else { return };
    let audit = Arc::new(RecordingAudit::default());
    let state = fx.routes(audit.clone());

    let denied = create_target_monitor(
        auth(Role::Reader),
        State(state.clone()),
        metadata(),
        Path(fx.project_id),
        Json(http_request("https://api.example.test/v1?token=abc")),
    )
    .await;
    let Err(problem) = denied else {
        panic!("a reader cannot create monitors")
    };
    assert_eq!(problem.status_code, StatusCode::FORBIDDEN);

    let created = create_target_monitor(
        auth(Role::Admin),
        State(state.clone()),
        metadata(),
        Path(fx.project_id),
        Json(http_request("https://api.example.test/v1?token=abc")),
    )
    .await
    .unwrap_or_else(|_| panic!("admin creates"));
    let response = axum::response::IntoResponse::into_response(created);
    assert_eq!(response.status(), StatusCode::CREATED);
    let body = axum::body::to_bytes(response.into_body(), 1 << 20)
        .await
        .expect("body");
    let monitor: serde_json::Value = serde_json::from_slice(&body).expect("json");
    assert_eq!(monitor["check_kind"], "http");
    assert_eq!(monitor["check"]["header_names"][0], "Authorization");
    assert!(!String::from_utf8_lossy(&body).contains(SECRET));
    let monitor_id = i32::try_from(monitor["id"].as_i64().expect("id")).expect("i32");

    let invalid = update_target_monitor(
        auth(Role::Admin),
        State(state.clone()),
        metadata(),
        Path((fx.project_id, monitor_id)),
        Json(UpdateTargetMonitorRequest {
            target: Some("not a url".to_string()),
            ..Default::default()
        }),
    )
    .await;
    let Err(problem) = invalid else {
        panic!("invalid target")
    };
    assert_eq!(problem.status_code, StatusCode::BAD_REQUEST);

    let missing = update_target_monitor(
        auth(Role::Admin),
        State(state.clone()),
        metadata(),
        Path((fx.other_project_id, monitor_id)),
        Json(UpdateTargetMonitorRequest::default()),
    )
    .await;
    let Err(problem) = missing else {
        panic!("other project")
    };
    assert_eq!(problem.status_code, StatusCode::NOT_FOUND);

    let entries = audit.entries.lock().expect("audit").clone();
    assert_eq!(entries.len(), 1, "only the successful write is audited");
    assert_eq!(entries[0].0, "STATUS_PAGE_MONITOR_CREATED");
    assert_eq!(entries[0].1["target_host"], "api.example.test");
    assert_eq!(entries[0].1["check_kind"], "http");
    let recorded = entries[0].1.to_string();
    assert!(
        !recorded.contains(SECRET) && !recorded.contains("token=abc"),
        "{recorded}"
    );
}

#[tokio::test]
async fn a_key_scoped_to_another_project_cannot_create_or_change_monitors() {
    let Some(fx) = fixture().await else { return };
    let audit = Arc::new(RecordingAudit::default());
    let state = fx.routes(audit.clone());
    let created = fx
        .service()
        .create(fx.project_id, http_request("https://api.example.test/"))
        .await
        .expect("create");

    let create = create_target_monitor(
        scoped_key(fx.other_project_id),
        State(state.clone()),
        metadata(),
        Path(fx.project_id),
        Json(http_request("https://api.example.test/other")),
    )
    .await;
    let Err(problem) = create else {
        panic!("a key scoped to another project must not create here")
    };
    assert_eq!(problem.status_code, StatusCode::FORBIDDEN);

    let update = update_target_monitor(
        scoped_key(fx.other_project_id),
        State(state.clone()),
        metadata(),
        Path((fx.project_id, created.id)),
        Json(UpdateTargetMonitorRequest {
            name: Some("renamed".to_string()),
            ..Default::default()
        }),
    )
    .await;
    let Err(problem) = update else {
        panic!("a key scoped to another project must not change monitors here")
    };
    assert_eq!(problem.status_code, StatusCode::FORBIDDEN);

    // The key's own project is allowed.
    let own = create_target_monitor(
        scoped_key(fx.project_id),
        State(state),
        metadata(),
        Path(fx.project_id),
        Json(http_request("https://api.example.test/own")),
    )
    .await;
    assert!(own.is_ok(), "a key may manage its own project's monitors");
    assert_eq!(audit.entries.lock().expect("audit").len(), 1);
}

#[tokio::test]
async fn a_rename_computed_before_another_save_does_not_restore_old_settings() {
    let Some(fx) = fixture().await else { return };
    let service = fx.service();
    let created = service
        .create(fx.project_id, http_request("https://api.example.test/old"))
        .await
        .expect("create");
    let stale = status_monitors::Entity::find_by_id(created.id)
        .one(fx.db.as_ref())
        .await
        .expect("read")
        .expect("row");

    // Another request saves a new target after `stale` was read.
    service
        .update(
            fx.project_id,
            created.id,
            UpdateTargetMonitorRequest {
                target: Some("https://api.example.test/new".to_string()),
                ..Default::default()
            },
        )
        .await
        .expect("save the new target");

    let rename = UpdateTargetMonitorRequest {
        name: Some("renamed".to_string()),
        ..Default::default()
    };
    let written = service
        .update_from(fx.project_id, created.id, &rename, stale)
        .await
        .expect("no error");
    assert!(
        written.is_none(),
        "a save computed from a stale row writes nothing"
    );

    let renamed = service
        .update(fx.project_id, created.id, rename)
        .await
        .expect("rename");
    assert_eq!(renamed.name, "renamed");
    assert_eq!(
        renamed.target.as_deref(),
        Some("https://api.example.test/new"),
        "the rename keeps the target saved meanwhile"
    );
}

#[tokio::test]
async fn routes_require_authentication() {
    let Some(fx) = fixture().await else { return };
    let router = create_target_monitor_router(fx.routes(Arc::new(RecordingAudit::default())));
    for (method, path) in [
        (
            "POST",
            format!("/projects/{}/monitors/targets", fx.project_id),
        ),
        (
            "PATCH",
            format!("/projects/{}/monitors/1/target", fx.project_id),
        ),
        (
            "GET",
            format!("/projects/{}/monitors/probe-locations", fx.project_id),
        ),
    ] {
        let response = router
            .clone()
            .oneshot(
                Request::builder()
                    .method(method)
                    .uri(&path)
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from("{}"))
                    .expect("request"),
            )
            .await
            .expect("router is infallible");
        assert_eq!(
            response.status(),
            StatusCode::UNAUTHORIZED,
            "{method} {path}"
        );
    }
}

/// Worker nodes as probe locations, answering from a script.
struct FakeLocations {
    nodes: Vec<NodeLocation>,
    answers: Vec<(i32, Result<ProbeOutcome, String>)>,
    asked: Mutex<Vec<(i32, RemoteProbeRequest)>>,
}

impl FakeLocations {
    fn new(answers: Vec<(NodeLocation, Result<ProbeOutcome, String>)>) -> Arc<Self> {
        Arc::new(Self {
            nodes: answers.iter().map(|(node, _)| node.clone()).collect(),
            answers: answers
                .into_iter()
                .map(|(node, answer)| (node.node_id, answer))
                .collect(),
            asked: Mutex::new(Vec::new()),
        })
    }
}

#[async_trait]
impl ProbeLocations for FakeLocations {
    async fn nodes(&self) -> Result<Vec<NodeLocation>, sea_orm::DbErr> {
        Ok(self.nodes.clone())
    }

    async fn probe(
        &self,
        node: &NodeLocation,
        request: &RemoteProbeRequest,
    ) -> Result<ProbeOutcome, NodeProbeError> {
        self.asked
            .lock()
            .expect("asked lock")
            .push((node.node_id, request.clone()));
        self.answers
            .iter()
            .find(|(id, _)| *id == node.node_id)
            .map(|(_, answer)| answer.clone())
            .unwrap_or_else(|| Err("not scripted".to_string()))
            .map_err(|reason| NodeProbeError::Refused {
                node: node.name.clone(),
                status: 503,
                reason,
            })
    }
}

fn worker(node_id: i32, region: Option<&str>) -> NodeLocation {
    NodeLocation {
        node_id,
        name: format!("worker-{node_id}"),
        region: region.map(str::to_string),
    }
}

fn down(message: &str) -> Result<ProbeOutcome, String> {
    Ok(ProbeOutcome {
        status: ProbeStatus::Down,
        response_time_ms: None,
        message: Some(message.to_string()),
    })
}

#[tokio::test]
async fn checked_from_nodes_the_majority_decides_and_every_answer_is_kept() {
    let Some(fx) = fixture().await else { return };
    let monitor = fx
        .service()
        .create(
            fx.project_id,
            CreateTargetMonitorRequest {
                probe_from_nodes: true,
                ..http_request("https://api.example.test/")
            },
        )
        .await
        .expect("create");
    assert!(monitor.probe_from_nodes);
    let locations = FakeLocations::new(vec![
        (
            worker(7, Some("eu")),
            down("could not connect: connection refused"),
        ),
        (worker(8, None), down("no answer within 10.0 s")),
        (
            worker(9, None),
            Err("could not reach its agent".to_string()),
        ),
    ]);
    let prober = FakeProber::new(ProbeOutcome {
        status: ProbeStatus::Operational,
        response_time_ms: Some(40),
        message: None,
    });
    let health = health_service_with(&fx, fx.checks_from(prober.clone(), locations.clone())).await;
    health.run_all_checks().await.expect("sweep");

    assert_eq!(
        prober.seen.lock().expect("seen").len(),
        1,
        "checked here too"
    );
    let asked = locations.asked.lock().expect("asked").clone();
    assert_eq!(asked.len(), 3, "every node was asked");
    assert_eq!(asked[0].1.target, "https://api.example.test/");
    assert_eq!(
        asked[0].1.request.headers[0].value, SECRET,
        "nodes get the decrypted request, which the real client only sends sealed"
    );

    let checks = status_checks::Entity::find()
        .filter(status_checks::Column::MonitorId.eq(monitor.id))
        .all(fx.db.as_ref())
        .await
        .expect("checks");
    assert_eq!(checks.len(), 1);
    assert_eq!(checks[0].status, "major_outage", "2 of the 3 that answered");
    let message = checks[0].error_message.clone().expect("message");
    assert!(
        message.starts_with("down from 2 of 3 locations"),
        "{message}"
    );
    assert!(message.contains("worker-7: could not connect"), "{message}");

    let after = status_monitors::Entity::find_by_id(monitor.id)
        .one(fx.db.as_ref())
        .await
        .expect("load")
        .expect("monitor");
    assert_eq!(
        after.updated_at, monitor.updated_at,
        "recording answers is not an edit"
    );
    let response = MonitorResponse::from(after);
    let results = response.location_results.expect("answers kept").results;
    let summary: Vec<(String, Option<&str>)> = results
        .iter()
        .map(|result| {
            (
                result.location.clone(),
                result.status.map(|status| status.stored()),
            )
        })
        .collect();
    assert_eq!(
        summary,
        vec![
            ("control plane".to_string(), Some("operational")),
            ("worker-7".to_string(), Some("major_outage")),
            ("worker-8".to_string(), Some("major_outage")),
            ("worker-9".to_string(), None),
        ]
    );
    assert_eq!(results[1].region.as_deref(), Some("eu"));
    assert_eq!(
        results[3].message.as_deref(),
        Some("no answer: its agent refused the check (HTTP 503): could not reach its agent")
    );
}

#[tokio::test]
async fn one_failing_location_is_not_an_outage_and_others_are_not_asked() {
    let Some(fx) = fixture().await else { return };
    let service = fx.service();
    let spread = service
        .create(
            fx.project_id,
            CreateTargetMonitorRequest {
                probe_from_nodes: true,
                ..http_request("https://api.example.test/a")
            },
        )
        .await
        .expect("create");
    let here_only = service
        .create(fx.project_id, http_request("https://api.example.test/b"))
        .await
        .expect("create");
    let locations = FakeLocations::new(vec![
        (worker(7, None), down("could not connect: no route to host")),
        (
            worker(8, None),
            Ok(ProbeOutcome {
                status: ProbeStatus::Operational,
                response_time_ms: Some(80),
                message: None,
            }),
        ),
    ]);
    let prober = FakeProber::new(ProbeOutcome {
        status: ProbeStatus::Operational,
        response_time_ms: Some(20),
        message: None,
    });
    let health = health_service_with(&fx, fx.checks_from(prober, locations.clone())).await;
    health.run_all_checks().await.expect("sweep");

    assert_eq!(
        locations.asked.lock().expect("asked").len(),
        2,
        "only the monitor checked from nodes asked them"
    );
    for (monitor, kept) in [(&spread, true), (&here_only, false)] {
        let check = status_checks::Entity::find()
            .filter(status_checks::Column::MonitorId.eq(monitor.id))
            .one(fx.db.as_ref())
            .await
            .expect("check")
            .expect("one check");
        assert_eq!(check.status, "operational");
        assert_eq!(check.error_message, None);
        let row = status_monitors::Entity::find_by_id(monitor.id)
            .one(fx.db.as_ref())
            .await
            .expect("load")
            .expect("monitor");
        assert_eq!(row.location_results.is_some(), kept);
    }
    let check = status_checks::Entity::find()
        .filter(status_checks::Column::MonitorId.eq(spread.id))
        .one(fx.db.as_ref())
        .await
        .expect("check")
        .expect("one check");
    assert_eq!(
        check.response_time_ms,
        Some(80),
        "the median of 20, 80 and none"
    );

    // Turning it off forgets the answers, which were for other settings.
    let updated = service
        .update(
            fx.project_id,
            spread.id,
            UpdateTargetMonitorRequest {
                probe_from_nodes: Some(false),
                ..Default::default()
            },
        )
        .await
        .expect("update");
    assert!(!updated.probe_from_nodes);
    assert_eq!(updated.location_results, None);
}

#[tokio::test]
async fn only_target_monitors_can_be_checked_from_nodes() {
    let Some(fx) = fixture().await else { return };
    let environment_monitor = status_monitors::ActiveModel {
        project_id: Set(fx.project_id),
        environment_id: Set(None),
        name: Set("environment".to_string()),
        monitor_type: Set("web".to_string()),
        check_interval_seconds: Set(60),
        is_active: Set(true),
        is_managed: Set(false),
        check_kind: Set("environment".to_string()),
        probe_from_nodes: Set(true),
        ..Default::default()
    }
    .insert(fx.db.as_ref())
    .await;
    let error = environment_monitor.expect_err("the constraint refuses it");
    assert!(
        error
            .to_string()
            .contains("status_monitors_probe_from_nodes_target"),
        "{error}"
    );
}

async fn locations_body(
    state: Arc<TargetMonitorRoutesState>,
    project_id: i32,
) -> ProbeLocationsResponse {
    let response = get_monitor_probe_locations(auth(Role::Reader), State(state), Path(project_id))
        .await
        .unwrap_or_else(|_| panic!("a reader sees the locations"));
    let response = axum::response::IntoResponse::into_response(response);
    assert_eq!(response.status(), StatusCode::OK);
    let body = axum::body::to_bytes(response.into_body(), 1 << 20)
        .await
        .expect("body");
    serde_json::from_slice(&body).expect("json")
}

#[tokio::test]
async fn probe_locations_say_what_is_missing_without_worker_nodes() {
    let Some(fx) = fixture().await else { return };
    let state = fx.routes(Arc::new(RecordingAudit::default()));
    let denied = get_monitor_probe_locations(
        auth(Role::MetricsIngest),
        State(state.clone()),
        Path(fx.project_id),
    )
    .await;
    let Err(problem) = denied else {
        panic!("no status page permission")
    };
    assert_eq!(problem.status_code, StatusCode::FORBIDDEN);

    let alone = locations_body(state, fx.project_id).await;
    assert!(!alone.configured);
    assert_eq!(alone.locations.len(), 1);
    assert_eq!(alone.locations[0].name, "control plane");
    assert!(alone
        .reason
        .expect("reason")
        .contains("No worker node is online"));
    assert_eq!(alone.setup_path.as_deref(), Some("/settings/nodes"));

    let with_nodes = Arc::new(TargetMonitorRoutesState {
        monitors: Arc::new(
            TargetMonitorService::new(
                fx.db.clone(),
                fx.checks_from(
                    FakeProber::new(ProbeOutcome {
                        status: ProbeStatus::Operational,
                        response_time_ms: Some(1),
                        message: None,
                    }),
                    FakeLocations::new(vec![(worker(7, Some("eu")), down("unused"))]),
                ),
            )
            .without_address_checks(),
        ),
        audit: Arc::new(RecordingAudit::default()),
        project_access_checker: None,
    });
    let spread = locations_body(with_nodes, fx.project_id).await;
    assert!(spread.configured);
    assert_eq!(spread.reason, None);
    assert_eq!(spread.locations[1].name, "worker-7");
    assert_eq!(spread.locations[1].region.as_deref(), Some("eu"));
}

#[test]
fn location_results_round_trip_through_the_column() {
    let results = MonitorLocationResults {
        checked_at: Utc::now(),
        results: vec![super::MonitorLocationResult::control_plane(ProbeOutcome {
            status: ProbeStatus::Degraded,
            response_time_ms: Some(900),
            message: Some("HTTP 200 in 900 ms, slower than 500 ms".to_string()),
        })],
    };
    let json = serde_json::to_value(&results).expect("json");
    assert_eq!(json["results"][0]["status"], "degraded");
    assert_eq!(
        serde_json::from_value::<MonitorLocationResults>(json).expect("back"),
        results
    );
}
