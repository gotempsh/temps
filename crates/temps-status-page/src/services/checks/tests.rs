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

use super::probe::{ProbeOutcome, ProbeStatus, TargetProber};
use super::spec::{CheckRequest, CheckRequestHeader, CheckSpec, HttpCheck, Target, TcpCheck};
use super::{
    CreateTargetMonitorRequest, MonitorCheckKind, TargetChecks, TargetMonitorError,
    TargetMonitorService, UpdateTargetMonitorRequest, MAX_TARGET_MONITORS_PER_PROJECT,
};
use crate::routes::target_monitors::{
    create_target_monitor, create_target_monitor_router, update_target_monitor,
    TargetMonitorRoutesState,
};
use crate::services::{HealthCheckService, MonitorResponse};

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
        Arc::new(TargetChecks::new(self.encryption.clone(), prober))
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
        fx.checks(prober),
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
