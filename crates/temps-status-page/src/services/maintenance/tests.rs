// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Database-backed tests of maintenance windows (ADR 108 D2): the service,
//! what the published page, feed and uptime show, and the admin handlers.

use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use axum::body::Body;
use axum::extract::{Extension, Path, State};
use axum::http::{header, Request, StatusCode};
use axum::Json;
use chrono::{Duration, Utc};
use sea_orm::{ActiveModelTrait, EntityTrait, Set};
use temps_authz::permissions::Role;
use temps_authz::{AuthContext, RequireAuth};
use temps_config::ConfigService;
use temps_core::{AuditLogger, AuditOperation, RequestMetadata};
use temps_entities::{
    environments, projects, status_checks, status_page_maintenance_monitors,
    status_page_maintenances, status_pages, upstream_config::UpstreamList, users,
};
use temps_test_utils::{is_container_runtime_unavailable, TestDatabase};
use tower::ServiceExt;

use super::{
    CreateMaintenanceRequest, MaintenanceError, MaintenanceState, StatusPageMaintenanceService,
    UpdateMaintenanceRequest,
};
use crate::routes::maintenance::{
    cancel_maintenance, create_maintenance, create_maintenance_router, MaintenanceRoutesState,
};
use crate::services::public_pages::{
    CreateStatusPageRequest, PublicComponentStatus, PublicMaintenanceStatus,
    PublicStatusPageResponse, PublicStatusPageService, StatusPageMonitorInput, StatusSnapshotCache,
    UpdateStatusPageRequest,
};
use crate::services::{CreateMonitorRequest, StatusPageService};

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

struct Fx {
    _db: TestDatabase,
    db: Arc<sea_orm::DatabaseConnection>,
    cache: Arc<StatusSnapshotCache>,
    maintenance: Arc<StatusPageMaintenanceService>,
    audit: Arc<RecordingAudit>,
    state: Arc<MaintenanceRoutesState>,
    page_id: i32,
    slug: String,
    /// On the page as "API".
    api_monitor: i32,
    /// On the page as "Web".
    web_monitor: i32,
    /// Not on the page.
    other_monitor: i32,
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

fn metadata() -> Extension<RequestMetadata> {
    Extension(RequestMetadata {
        ip_address: "192.0.2.41".to_string(),
        user_agent: "maintenance-test".to_string(),
        headers: axum::http::HeaderMap::new(),
        visitor_id_cookie: None,
        session_id_cookie: None,
        base_url: "http://localhost".to_string(),
        scheme: "http".to_string(),
        host: "localhost".to_string(),
        is_secure: false,
    })
}

async fn fixture() -> Option<Fx> {
    let test_db = match TestDatabase::with_migrations().await {
        Ok(db) => db,
        Err(e) if is_container_runtime_unavailable(&e.to_string()) => {
            println!("Docker not available, skipping status page maintenance test");
            return None;
        }
        Err(e) => panic!("test database setup failed: {e}"),
    };
    let db = test_db.connection_arc();
    let config = temps_config::ServerConfig::new(
        "127.0.0.1:3000".to_string(),
        "postgres://test:test@localhost/test".to_string(),
        None,
        None,
    )
    .expect("server config");
    let config_service = Arc::new(ConfigService::new(Arc::new(config), db.clone()));

    let nanos = Utc::now().timestamp_nanos_opt().unwrap_or(0);
    let slug = format!("maint-{nanos}");
    let project_id = projects::ActiveModel {
        name: Set("Internal Project Name".to_string()),
        slug: Set(slug.clone()),
        directory: Set(slug.clone()),
        main_branch: Set("main".to_string()),
        preset: Set(temps_entities::preset::Preset::Nixpacks),
        repo_name: Set("repo".to_string()),
        repo_owner: Set("owner".to_string()),
        ..Default::default()
    }
    .insert(db.as_ref())
    .await
    .expect("project")
    .id;
    let mut environment_ids = Vec::new();
    for name in ["api", "web", "other"] {
        environment_ids.push(
            environments::ActiveModel {
                project_id: Set(project_id),
                name: Set(format!("{slug}-{name}")),
                slug: Set(format!("{slug}-{name}")),
                subdomain: Set(format!("{slug}-{name}")),
                host: Set(format!("{name}.internal.example.test")),
                upstreams: Set(UpstreamList::default()),
                ..Default::default()
            }
            .insert(db.as_ref())
            .await
            .expect("environment")
            .id,
        );
    }

    let status = StatusPageService::new(db.clone(), config_service.clone());
    let mut monitors = Vec::new();
    for (index, environment_id) in environment_ids.iter().enumerate() {
        monitors.push(
            status
                .monitor_service()
                .create_monitor(
                    project_id,
                    CreateMonitorRequest {
                        name: format!("internal-monitor-{index}"),
                        monitor_type: "web".to_string(),
                        environment_id: *environment_id,
                        check_interval_seconds: Some(60),
                        check_path: Some("/healthz".to_string()),
                    },
                )
                .await
                .expect("monitor")
                .id,
        );
    }

    let cache = Arc::new(StatusSnapshotCache::new(db.clone()));
    let pages = Arc::new(PublicStatusPageService::new(
        db.clone(),
        config_service,
        cache.clone(),
    ));
    let page = pages
        .create(CreateStatusPageRequest {
            title: "Example status".to_string(),
            description: None,
            monitors: vec![
                StatusPageMonitorInput {
                    monitor_id: monitors[0],
                    display_name: "API".to_string(),
                    group_name: None,
                },
                StatusPageMonitorInput {
                    monitor_id: monitors[1],
                    display_name: "Web".to_string(),
                    group_name: None,
                },
            ],
            appearance: None,
        })
        .await
        .expect("page");
    pages.publish(page.id).await.expect("publish");
    let slug = status_pages::Entity::find_by_id(page.id)
        .one(db.as_ref())
        .await
        .expect("page row")
        .expect("page exists")
        .slug;

    let maintenance = Arc::new(StatusPageMaintenanceService::new(db.clone(), cache.clone()));
    let audit = Arc::new(RecordingAudit::default());
    let state = Arc::new(MaintenanceRoutesState {
        maintenance: maintenance.clone(),
        pages,
        audit: audit.clone(),
        project_access_checker: None,
    });
    Some(Fx {
        _db: test_db,
        db,
        cache,
        maintenance,
        audit,
        state,
        page_id: page.id,
        slug,
        api_monitor: monitors[0],
        web_monitor: monitors[1],
        other_monitor: monitors[2],
    })
}

fn request(
    monitor_ids: Vec<i32>,
    starts_in: Duration,
    lasts: Duration,
) -> CreateMaintenanceRequest {
    let starts_at = Utc::now() + starts_in;
    CreateMaintenanceRequest {
        title: "Database upgrade".to_string(),
        description: Some("We are upgrading the primary database.".to_string()),
        starts_at,
        ends_at: starts_at + lasts,
        monitor_ids,
    }
}

fn published(fx: &Fx) -> PublicStatusPageResponse {
    let snapshot = fx.cache.get(&fx.slug).expect("page is published");
    serde_json::from_slice(&snapshot.json).expect("snapshot json")
}

async fn check(fx: &Fx, monitor_id: i32, status: &str, ago: Duration) {
    let at = Utc::now() - ago;
    status_checks::ActiveModel {
        monitor_id: Set(monitor_id),
        status: Set(status.to_string()),
        response_time_ms: Set(Some(100)),
        checked_at: Set(at),
        created_at: Set(at),
        ..Default::default()
    }
    .insert(fx.db.as_ref())
    .await
    .expect("check");
}

#[tokio::test]
async fn a_scheduled_window_is_shown_announced_in_the_feed_and_leaves_status_alone() {
    let Some(fx) = fixture().await else {
        return;
    };
    let window = fx
        .maintenance
        .create(
            fx.page_id,
            request(vec![fx.api_monitor], Duration::hours(2), Duration::hours(1)),
        )
        .await
        .expect("schedule");
    assert_eq!(window.state, MaintenanceState::Scheduled);
    assert_eq!(window.monitor_ids, vec![fx.api_monitor]);

    let page = published(&fx);
    assert_eq!(page.maintenances.len(), 1);
    assert_eq!(
        page.maintenances[0].status,
        PublicMaintenanceStatus::Scheduled
    );
    assert_eq!(
        page.maintenances[0].affected_components,
        vec!["API".to_string()]
    );
    assert!(page
        .components
        .iter()
        .all(|c| c.status != PublicComponentStatus::Maintenance));

    let snapshot = fx.cache.get(&fx.slug).expect("published");
    let html = String::from_utf8(snapshot.html.to_vec()).expect("utf-8");
    assert!(html.contains("Scheduled maintenance"));
    assert!(html.contains("Database upgrade"));
    let feed = String::from_utf8(snapshot.feed.to_vec()).expect("utf-8");
    assert!(
        feed.contains("<title>Database upgrade: Scheduled maintenance</title>"),
        "{feed}"
    );
    let json = String::from_utf8(snapshot.json.to_vec()).expect("utf-8");
    assert!(
        !json.contains("internal-monitor"),
        "no internal names: {json}"
    );
}

#[tokio::test]
async fn a_window_in_progress_marks_its_components_and_drops_its_checks_from_uptime() {
    let Some(fx) = fixture().await else {
        return;
    };
    // Before the window: one operational and one down check on each monitor.
    for monitor in [fx.api_monitor, fx.web_monitor] {
        check(&fx, monitor, "operational", Duration::minutes(10)).await;
        check(&fx, monitor, "down", Duration::minutes(8)).await;
    }
    fx.maintenance
        .create(
            fx.page_id,
            request(
                vec![fx.api_monitor],
                -Duration::minutes(4),
                Duration::hours(1),
            ),
        )
        .await
        .expect("start now");
    // During the window, API is down: those checks must not count.
    check(&fx, fx.api_monitor, "down", Duration::minutes(2)).await;
    check(&fx, fx.api_monitor, "down", Duration::minutes(1)).await;
    check(&fx, fx.web_monitor, "down", Duration::minutes(1)).await;
    fx.cache.refresh_page(fx.page_id).await.expect("refresh");

    let page = published(&fx);
    let api = page
        .components
        .iter()
        .find(|c| c.name == "API")
        .expect("API");
    let web = page
        .components
        .iter()
        .find(|c| c.name == "Web")
        .expect("Web");
    assert_eq!(api.status, PublicComponentStatus::Maintenance);
    assert_eq!(
        page.maintenances[0].status,
        PublicMaintenanceStatus::InProgress
    );
    let api_today = api.days.last().expect("today");
    assert_eq!(
        api_today.total_checks,
        Some(2),
        "checks in the window are left out"
    );
    assert_eq!(api_today.failed_checks, Some(1));
    let web_today = web.days.last().expect("today");
    assert_eq!(
        web_today.total_checks,
        Some(3),
        "an uncovered monitor counts everything"
    );
    assert_ne!(web.status, PublicComponentStatus::Maintenance);
}

#[tokio::test]
async fn completed_days_follow_a_page_dropping_the_monitor_its_window_covered() {
    let Some(fx) = fixture().await else {
        return;
    };
    // Yesterday, 11:30-12:30 UTC, the page held a window over API, which
    // was down during it and up afterwards.
    let yesterday = (Utc::now() - Duration::days(1)).date_naive();
    let at = |hour: u32, minute: u32| {
        yesterday
            .and_hms_opt(hour, minute, 0)
            .expect("valid time")
            .and_utc()
    };
    let window = status_page_maintenances::ActiveModel {
        status_page_id: Set(fx.page_id),
        title: Set("Database upgrade".to_string()),
        starts_at: Set(at(11, 30)),
        ends_at: Set(at(12, 30)),
        created_at: Set(at(10, 0)),
        updated_at: Set(at(10, 0)),
        ..Default::default()
    }
    .insert(fx.db.as_ref())
    .await
    .expect("window");
    status_page_maintenance_monitors::ActiveModel {
        maintenance_id: Set(window.id),
        monitor_id: Set(fx.api_monitor),
    }
    .insert(fx.db.as_ref())
    .await
    .expect("window monitor");
    for (status, hour, minute) in [("down", 12, 0), ("down", 12, 10), ("operational", 13, 0)] {
        let checked_at = at(hour, minute);
        status_checks::ActiveModel {
            monitor_id: Set(fx.api_monitor),
            status: Set(status.to_string()),
            response_time_ms: Set(Some(100)),
            checked_at: Set(checked_at),
            created_at: Set(checked_at),
            ..Default::default()
        }
        .insert(fx.db.as_ref())
        .await
        .expect("check");
    }

    // The fixture's page cached API's history before these backdated rows
    // existed; start from a cold history, as on the next UTC day.
    fx.cache.forget_history(&[fx.api_monitor]).await;

    // A second page shows API too, without a window of its own.
    let other = fx
        .state
        .pages
        .create(CreateStatusPageRequest {
            title: "Partner status".to_string(),
            description: None,
            monitors: vec![StatusPageMonitorInput {
                monitor_id: fx.api_monitor,
                display_name: "API".to_string(),
                group_name: None,
            }],
            appearance: None,
        })
        .await
        .expect("second page");
    fx.state.pages.publish(other.id).await.expect("publish");
    let other_slug = status_pages::Entity::find_by_id(other.id)
        .one(fx.db.as_ref())
        .await
        .expect("page row")
        .expect("page exists")
        .slug;
    let api_yesterday = || {
        let snapshot = fx.cache.get(&other_slug).expect("published");
        let page: PublicStatusPageResponse =
            serde_json::from_slice(&snapshot.json).expect("snapshot json");
        let date = yesterday.format("%Y-%m-%d").to_string();
        page.components
            .into_iter()
            .find(|c| c.name == "API")
            .expect("API")
            .days
            .into_iter()
            .find(|day| day.date == date)
            .expect("yesterday")
            .total_checks
    };
    fx.cache.refresh_page(other.id).await.expect("refresh");
    assert_eq!(
        api_yesterday(),
        Some(1),
        "the first page still shows API, so its window's checks are left out"
    );

    // The first page stops showing API: its window no longer excludes
    // anything, and the completed day is recomputed rather than served from
    // the cached history.
    fx.state
        .pages
        .update(
            fx.page_id,
            UpdateStatusPageRequest {
                monitors: Some(vec![StatusPageMonitorInput {
                    monitor_id: fx.web_monitor,
                    display_name: "Web".to_string(),
                    group_name: None,
                }]),
                ..Default::default()
            },
        )
        .await
        .expect("drop API");
    fx.cache.refresh_page(other.id).await.expect("refresh");
    assert_eq!(
        api_yesterday(),
        Some(3),
        "every check of the day counts again"
    );
}

#[tokio::test]
async fn windows_are_validated_against_the_page_and_its_limit() {
    let Some(fx) = fixture().await else {
        return;
    };
    let err = fx
        .maintenance
        .create(
            fx.page_id,
            request(
                vec![fx.other_monitor],
                Duration::hours(1),
                Duration::hours(1),
            ),
        )
        .await
        .expect_err("foreign monitor");
    assert!(
        matches!(err, MaintenanceError::Validation { status_page_id, .. } if status_page_id == fx.page_id)
    );
    assert!(
        err.to_string().contains(&fx.other_monitor.to_string()),
        "{err}"
    );

    let err = fx
        .maintenance
        .create(
            fx.page_id,
            request(
                vec![fx.api_monitor],
                -Duration::hours(2),
                Duration::hours(3),
            ),
        )
        .await
        .expect_err("backdated");
    assert!(matches!(err, MaintenanceError::Validation { .. }));

    let err = fx
        .maintenance
        .create(
            9_999_999,
            request(vec![fx.api_monitor], Duration::hours(1), Duration::hours(1)),
        )
        .await
        .expect_err("unknown page");
    assert!(matches!(
        err,
        MaintenanceError::StatusPageNotFound {
            status_page_id: 9_999_999
        }
    ));

    for hour in 0..super::service::MAX_OPEN_PER_PAGE as i64 {
        fx.maintenance
            .create(
                fx.page_id,
                request(
                    vec![fx.api_monitor],
                    Duration::hours(hour + 1),
                    Duration::minutes(30),
                ),
            )
            .await
            .expect("within the limit");
    }
    let err = fx
        .maintenance
        .create(
            fx.page_id,
            request(
                vec![fx.api_monitor],
                Duration::days(2),
                Duration::minutes(30),
            ),
        )
        .await
        .expect_err("over the limit");
    assert!(matches!(
        err,
        MaintenanceError::TooManyScheduled { limit: 20, .. }
    ));

    let listed = fx.maintenance.list(fx.page_id, 1, 500).await.expect("list");
    assert_eq!(listed.total, 20);
    assert_eq!(listed.page_size, 100, "page size is capped");
}

#[tokio::test]
async fn a_started_window_keeps_its_start_and_monitors_and_cancelling_ends_it() {
    let Some(fx) = fixture().await else {
        return;
    };
    let window = fx
        .maintenance
        .create(
            fx.page_id,
            request(
                vec![fx.api_monitor],
                -Duration::minutes(1),
                Duration::hours(1),
            ),
        )
        .await
        .expect("start now");
    let err = fx
        .maintenance
        .update(
            fx.page_id,
            window.id,
            UpdateMaintenanceRequest {
                monitor_ids: Some(vec![fx.api_monitor, fx.web_monitor]),
                ..Default::default()
            },
        )
        .await
        .expect_err("monitors are fixed");
    assert!(
        matches!(err, MaintenanceError::Conflict { maintenance_id, .. } if maintenance_id == window.id)
    );

    let extended = fx
        .maintenance
        .update(
            fx.page_id,
            window.id,
            UpdateMaintenanceRequest {
                title: Some("Longer upgrade".to_string()),
                ends_at: Some(window.ends_at + Duration::hours(1)),
                ..Default::default()
            },
        )
        .await
        .expect("extend");
    assert_eq!(extended.title, "Longer upgrade");
    assert_eq!(published(&fx).maintenances[0].title, "Longer upgrade");

    let cancelled = fx
        .maintenance
        .cancel(fx.page_id, window.id)
        .await
        .expect("cancel");
    assert_eq!(cancelled.state, MaintenanceState::Cancelled);
    let page = published(&fx);
    assert!(page.maintenances.is_empty());
    assert!(page
        .components
        .iter()
        .all(|c| c.status != PublicComponentStatus::Maintenance));
    assert!(matches!(
        fx.maintenance.cancel(fx.page_id, window.id).await,
        Err(MaintenanceError::Conflict { .. })
    ));
    assert!(matches!(
        fx.maintenance.get(fx.page_id + 1, window.id).await,
        Err(MaintenanceError::NotFound { .. })
    ));
}

#[tokio::test]
async fn the_handlers_authorize_and_audit_every_write() {
    let Some(fx) = fixture().await else {
        return;
    };
    let forbidden = create_maintenance(
        auth(Role::Reader),
        State(fx.state.clone()),
        metadata(),
        Path(fx.page_id),
        Json(request(
            vec![fx.api_monitor],
            Duration::hours(1),
            Duration::hours(1),
        )),
    )
    .await
    .err()
    .expect("a reader cannot schedule");
    assert_eq!(forbidden.status_code, StatusCode::FORBIDDEN);

    let response = create_maintenance(
        auth(Role::Admin),
        State(fx.state.clone()),
        metadata(),
        Path(fx.page_id),
        Json(request(
            vec![fx.api_monitor],
            Duration::hours(1),
            Duration::hours(1),
        )),
    )
    .await
    .map_err(|p| format!("{:?}", p.body))
    .expect("admin schedules");
    let response = axum::response::IntoResponse::into_response(response);
    assert_eq!(response.status(), StatusCode::CREATED);
    let body = axum::body::to_bytes(response.into_body(), 1 << 20)
        .await
        .expect("body");
    let created: super::MaintenanceResponse = serde_json::from_slice(&body).expect("json");

    let invalid = create_maintenance(
        auth(Role::Admin),
        State(fx.state.clone()),
        metadata(),
        Path(fx.page_id),
        Json(request(vec![], Duration::hours(1), Duration::hours(1))),
    )
    .await
    .err()
    .expect("no monitors");
    assert_eq!(invalid.status_code, StatusCode::BAD_REQUEST);

    cancel_maintenance(
        auth(Role::Admin),
        State(fx.state.clone()),
        metadata(),
        Path((fx.page_id, created.id)),
    )
    .await
    .map_err(|p| format!("{:?}", p.body))
    .expect("cancel");

    let entries = fx.audit.entries.lock().expect("audit").clone();
    let kinds: Vec<&str> = entries.iter().map(|(kind, _)| kind.as_str()).collect();
    assert_eq!(
        kinds,
        [
            "STATUS_PAGE_MAINTENANCE_SCHEDULED",
            "STATUS_PAGE_MAINTENANCE_CANCELLED"
        ]
    );
    assert_eq!(entries[0].1["maintenance_id"], created.id);
    assert_eq!(entries[0].1["monitor_ids"][0], fx.api_monitor);
}

#[tokio::test]
async fn the_admin_routes_answer_401_without_credentials() {
    let Some(fx) = fixture().await else {
        return;
    };
    let router = create_maintenance_router(fx.state.clone());
    let base = format!("/status-pages/{}/maintenances", fx.page_id);
    for (method, path) in [
        ("GET", base.clone()),
        ("POST", base.clone()),
        ("GET", format!("{base}/1")),
        ("PATCH", format!("{base}/1")),
        ("DELETE", format!("{base}/1")),
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
