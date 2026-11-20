// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Database-backed tests of status page passwords (ADR 108 D2): the public
//! routes behind the gate, the unlock form, and the admin handlers.

use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use axum::body::Body;
use axum::extract::{Extension, Path, State};
use axum::http::{header, Request, StatusCode};
use axum::Json;
use sea_orm::{ActiveModelTrait, EntityTrait, Set};
use temps_authz::permissions::Role;
use temps_authz::{AuthContext, RequireAuth};
use temps_config::ConfigService;
use temps_core::{AuditLogger, AuditOperation, EncryptionService, RequestMetadata};
use temps_entities::{environments, projects, status_pages, upstream_config::UpstreamList, users};
use temps_test_utils::{is_container_runtime_unavailable, TestDatabase};
use tower::ServiceExt;

use super::limiter::MAX_FAILURES;
use super::{PageAccessService, SetPagePasswordRequest};
use crate::routes::page_password::{set_page_password, PagePasswordRoutesState};
use crate::routes::public::{create_public_router, PublicStatusState};
use crate::services::public_pages::{
    CreateStatusPageRequest, PublicStatusPageService, StatusPageMonitorInput, StatusSnapshotCache,
};
use crate::services::{CreateMonitorRequest, StatusPageService};

const PASSWORD: &str = "correct horse battery";

#[derive(Default)]
struct RecordingAudit {
    entries: Mutex<Vec<(String, String)>>,
}

#[async_trait]
impl AuditLogger for RecordingAudit {
    async fn create_audit_log(
        &self,
        operation: &dyn AuditOperation,
    ) -> temps_core::AuditResult<()> {
        self.entries
            .lock()
            .expect("audit lock")
            .push((operation.operation_type(), operation.serialize()?));
        Ok(())
    }
}

struct Fx {
    _db: TestDatabase,
    access: Arc<PageAccessService>,
    audit: Arc<RecordingAudit>,
    admin: Arc<PagePasswordRoutesState>,
    public: axum::Router,
    page_id: i32,
    slug: String,
}

fn auth(role: Role) -> RequireAuth {
    let now = chrono::Utc::now();
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
        ip_address: "192.0.2.42".to_string(),
        user_agent: "page-password-test".to_string(),
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
            println!("Docker not available, skipping status page password test");
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
    let nanos = chrono::Utc::now().timestamp_nanos_opt().unwrap_or(0);
    let slug = format!("pw-{nanos}");
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
    let environment_id = environments::ActiveModel {
        project_id: Set(project_id),
        name: Set(slug.clone()),
        slug: Set(slug.clone()),
        subdomain: Set(slug.clone()),
        host: Set("internal.example.test".to_string()),
        upstreams: Set(UpstreamList::default()),
        ..Default::default()
    }
    .insert(db.as_ref())
    .await
    .expect("environment")
    .id;
    let status = StatusPageService::new(db.clone(), config_service.clone());
    let monitor = status
        .monitor_service()
        .create_monitor(
            project_id,
            CreateMonitorRequest {
                name: "internal-monitor".to_string(),
                monitor_type: "web".to_string(),
                environment_id,
                check_interval_seconds: Some(60),
                check_path: Some("/healthz".to_string()),
            },
        )
        .await
        .expect("monitor");
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
            monitors: vec![StatusPageMonitorInput {
                monitor_id: monitor.id,
                display_name: "API".to_string(),
                group_name: None,
            }],
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
    let access = Arc::new(PageAccessService::new(
        db.clone(),
        cache.clone(),
        &EncryptionService::new_from_password("page-password-tests"),
    ));
    let audit = Arc::new(RecordingAudit::default());
    let admin = Arc::new(PagePasswordRoutesState {
        access: access.clone(),
        pages,
        audit: audit.clone(),
        project_access_checker: None,
    });
    let public = create_public_router(Arc::new(PublicStatusState::new(cache, access.clone())));
    Some(Fx {
        _db: test_db,
        access,
        audit,
        admin,
        public,
        page_id: page.id,
        slug,
    })
}

struct Answer {
    status: StatusCode,
    headers: axum::http::HeaderMap,
    body: String,
}

async fn call(
    fx: &Fx,
    method: &str,
    path: &str,
    cookie: Option<&str>,
    form: Option<&str>,
) -> Answer {
    let mut request = Request::builder()
        .method(method)
        .uri(path)
        .header("x-forwarded-proto", "https");
    if let Some(cookie) = cookie {
        request = request.header(header::COOKIE, cookie);
    }
    if form.is_some() {
        request = request.header(header::CONTENT_TYPE, "application/x-www-form-urlencoded");
    }
    let response = fx
        .public
        .clone()
        .oneshot(
            request
                .body(Body::from(form.unwrap_or_default().to_string()))
                .expect("request"),
        )
        .await
        .expect("router is infallible");
    let status = response.status();
    let headers = response.headers().clone();
    let body = axum::body::to_bytes(response.into_body(), 1 << 20)
        .await
        .expect("body");
    Answer {
        status,
        headers,
        body: String::from_utf8_lossy(&body).into_owned(),
    }
}

async fn protect(fx: &Fx, password: Option<&str>) {
    fx.access
        .set_password(
            fx.page_id,
            SetPagePasswordRequest {
                password: password.map(str::to_string),
            },
        )
        .await
        .expect("set password");
}

/// The `name=value` part of an unlock answer's `Set-Cookie`.
fn cookie_of(answer: &Answer) -> String {
    let set_cookie = answer
        .headers
        .get(header::SET_COOKIE)
        .and_then(|v| v.to_str().ok())
        .expect("unlock sets a cookie")
        .to_string();
    set_cookie
        .split(';')
        .next()
        .expect("cookie pair")
        .to_string()
}

#[tokio::test]
async fn a_protected_page_shows_only_the_password_form_until_unlocked() {
    let Some(fx) = fixture().await else {
        return;
    };
    let page = format!("/status/{}", fx.slug);
    let open = call(&fx, "GET", &page, None, None).await;
    assert_eq!(open.status, StatusCode::OK);
    assert!(
        open.body.contains("feed.atom"),
        "an open page offers its feed"
    );

    protect(&fx, Some(PASSWORD)).await;
    let locked = call(&fx, "GET", &page, None, None).await;
    assert_eq!(locked.status, StatusCode::UNAUTHORIZED);
    assert!(locked.body.contains("type=\"password\""));
    assert!(
        !locked.body.contains("API"),
        "no component leaks: {}",
        locked.body
    );
    let json = call(&fx, "GET", &format!("{page}/summary.json"), None, None).await;
    assert_eq!(json.status, StatusCode::UNAUTHORIZED);
    assert!(json.body.contains("needs a password"));
    assert!(!json.body.contains("API"));
    let feed = call(&fx, "GET", &format!("{page}/feed.atom"), None, None).await;
    assert_eq!(
        feed.status,
        StatusCode::NOT_FOUND,
        "a protected page has no feed"
    );

    let wrong = call(
        &fx,
        "POST",
        &format!("{page}/unlock"),
        None,
        Some("password=nope"),
    )
    .await;
    assert_eq!(wrong.status, StatusCode::UNAUTHORIZED);
    assert!(wrong.body.contains("not right"));
    assert!(wrong.headers.get(header::SET_COOKIE).is_none());

    let form = format!("password={}", PASSWORD.replace(' ', "+"));
    let unlocked = call(
        &fx,
        "POST",
        &format!("/status/{}/unlock", fx.slug),
        None,
        Some(&form),
    )
    .await;
    assert_eq!(unlocked.status, StatusCode::SEE_OTHER);
    assert_eq!(
        unlocked
            .headers
            .get(header::LOCATION)
            .and_then(|v| v.to_str().ok()),
        Some(format!("/api/status/{}", fx.slug).as_str())
    );
    let set_cookie = unlocked
        .headers
        .get(header::SET_COOKIE)
        .and_then(|v| v.to_str().ok())
        .expect("cookie")
        .to_string();
    assert!(
        set_cookie.contains("HttpOnly") && set_cookie.contains("Secure"),
        "{set_cookie}"
    );
    assert!(set_cookie.contains(&format!("Path=/api/status/{}", fx.slug)));
    assert!(!set_cookie.contains(PASSWORD));
    let cookie = cookie_of(&unlocked);

    let page_view = call(&fx, "GET", &page, Some(&cookie), None).await;
    assert_eq!(page_view.status, StatusCode::OK);
    assert!(page_view.body.contains("API"));
    assert!(
        !page_view.body.contains("feed.atom"),
        "no feed link while protected"
    );
    let json = call(
        &fx,
        "GET",
        &format!("{page}/summary.json"),
        Some(&cookie),
        None,
    )
    .await;
    assert_eq!(json.status, StatusCode::OK);
    assert!(
        json.headers
            .get(header::ACCESS_CONTROL_ALLOW_ORIGIN)
            .is_none(),
        "a protected page's data is not shared with other origins"
    );

    // Changing the password signs everyone out; removing it opens the page.
    protect(&fx, Some("another password")).await;
    assert_eq!(
        call(&fx, "GET", &page, Some(&cookie), None).await.status,
        StatusCode::UNAUTHORIZED
    );
    protect(&fx, None).await;
    assert_eq!(
        call(&fx, "GET", &page, None, None).await.status,
        StatusCode::OK
    );
    assert_eq!(
        call(&fx, "GET", &format!("{page}/feed.atom"), None, None)
            .await
            .status,
        StatusCode::OK
    );
}

#[tokio::test]
async fn repeated_wrong_passwords_are_refused_for_a_while() {
    let Some(fx) = fixture().await else {
        return;
    };
    protect(&fx, Some(PASSWORD)).await;
    let unlock = format!("/status/{}/unlock", fx.slug);
    for _ in 0..MAX_FAILURES {
        let answer = call(&fx, "POST", &unlock, None, Some("password=wrong-guess")).await;
        assert_eq!(answer.status, StatusCode::UNAUTHORIZED);
    }
    let form = format!("password={}", PASSWORD.replace(' ', "+"));
    let throttled = call(&fx, "POST", &unlock, None, Some(&form)).await;
    assert_eq!(
        throttled.status,
        StatusCode::TOO_MANY_REQUESTS,
        "even the right password waits"
    );
    assert!(throttled.headers.get(header::RETRY_AFTER).is_some());
    assert!(throttled.body.contains("Too many wrong passwords"));
}

#[tokio::test]
async fn an_unknown_page_cannot_be_unlocked_and_an_open_page_just_redirects() {
    let Some(fx) = fixture().await else {
        return;
    };
    let unknown = call(
        &fx,
        "POST",
        "/status/0123456789abcdef0123456789abcdef/unlock",
        None,
        Some("password=x"),
    )
    .await;
    assert_eq!(unknown.status, StatusCode::NOT_FOUND);
    let open = call(
        &fx,
        "POST",
        &format!("/status/{}/unlock", fx.slug),
        None,
        Some("password=x"),
    )
    .await;
    assert_eq!(open.status, StatusCode::SEE_OTHER);
    assert!(open.headers.get(header::SET_COOKIE).is_none());
}

#[tokio::test]
async fn setting_the_password_is_authorized_validated_and_audited_without_the_password() {
    let Some(fx) = fixture().await else {
        return;
    };
    let forbidden = set_page_password(
        auth(Role::Reader),
        State(fx.admin.clone()),
        metadata(),
        Path(fx.page_id),
        Json(SetPagePasswordRequest {
            password: Some(PASSWORD.to_string()),
        }),
    )
    .await
    .err()
    .expect("a reader cannot set it");
    assert_eq!(forbidden.status_code, StatusCode::FORBIDDEN);

    let invalid = set_page_password(
        auth(Role::Admin),
        State(fx.admin.clone()),
        metadata(),
        Path(fx.page_id),
        Json(SetPagePasswordRequest {
            password: Some("short".to_string()),
        }),
    )
    .await
    .err()
    .expect("too short");
    assert_eq!(invalid.status_code, StatusCode::BAD_REQUEST);

    let missing = set_page_password(
        auth(Role::Admin),
        State(fx.admin.clone()),
        metadata(),
        Path(9_999_999),
        Json(SetPagePasswordRequest { password: None }),
    )
    .await
    .err()
    .expect("unknown page");
    assert_eq!(missing.status_code, StatusCode::NOT_FOUND);

    set_page_password(
        auth(Role::Admin),
        State(fx.admin.clone()),
        metadata(),
        Path(fx.page_id),
        Json(SetPagePasswordRequest {
            password: Some(PASSWORD.to_string()),
        }),
    )
    .await
    .map_err(|p| format!("{:?}", p.body))
    .expect("admin sets it");
    assert!(
        fx.access
            .status(fx.page_id)
            .await
            .expect("status")
            .has_password
    );

    let entries = fx.audit.entries.lock().expect("audit").clone();
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0].0, "STATUS_PAGE_PASSWORD_SET");
    assert!(!entries[0].1.contains(PASSWORD), "{}", entries[0].1);
}

#[tokio::test]
async fn the_admin_routes_answer_401_without_credentials() {
    let Some(fx) = fixture().await else {
        return;
    };
    let router = crate::routes::page_password::create_page_password_router(fx.admin.clone());
    for method in ["GET", "PUT"] {
        let response = router
            .clone()
            .oneshot(
                Request::builder()
                    .method(method)
                    .uri(format!("/status-pages/{}/password", fx.page_id))
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from("{\"password\":null}"))
                    .expect("request"),
            )
            .await
            .expect("router is infallible");
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED, "{method}");
    }
}
