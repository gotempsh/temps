// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Database-backed tests of status page subscriptions (ADR 108 D2): the public
//! routes, the dispatcher and the admin handlers together.

use std::collections::HashSet;
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use axum::body::Body;
use axum::extract::{Extension, Path, Query, State};
use axum::http::{header, Request, StatusCode};
use axum::response::IntoResponse;
use axum::Json;
use sea_orm::{ActiveModelTrait, ColumnTrait, EntityTrait, PaginatorTrait, QueryFilter, Set};
use temps_authz::permissions::Role;
use temps_authz::{AuthContext, RequireAuth};
use temps_config::ConfigService;
use temps_core::{AuditLogger, AuditOperation, EncryptionService, RequestMetadata};
use temps_entities::{
    environments, projects, status_incident_updates, status_page_notices, status_page_subscribers,
    status_pages, upstream_config::UpstreamList, users,
};
use temps_test_utils::{is_container_runtime_unavailable, TestDatabase};
use tower::ServiceExt;

use super::dispatcher::DispatcherState;
use super::mailer::{judge_sender, MailReadiness, OutgoingMail, SubscriberMailer};
use super::{
    StatusPageSubscriberService, SubscriberListResponse, SubscriptionError,
    SubscriptionSettingsResponse, UpdateSubscriptionSettingsRequest,
};
use crate::routes::public_subscriptions::{
    create_public_subscription_router, PublicSubscriptionState,
};
use crate::routes::subscribers::{
    delete_subscriber, export_subscribers, get_subscription_settings, list_subscribers,
    update_subscription_settings, SubscriberListQuery, SubscriberRoutesState,
};
use crate::services::public_pages::{
    CreateStatusPageRequest, PublicStatusPageService, StatusPageMonitorInput, StatusSnapshotCache,
};
use crate::services::{
    CreateIncidentRequest, CreateMonitorRequest, StatusPageService, UpdateIncidentStatusRequest,
};

const PUBLIC_UPDATE: &str = "We are investigating elevated errors.";
const PRIVATE_UPDATE: &str = "PRIVATE-UPDATE-TEXT";
const INTERNAL_TITLE: &str = "Internal incident title";
const INTERNAL_MONITOR: &str = "internal-monitor-name";
const SENDER: &str = "status@example.test";

#[derive(Default)]
struct FakeMailer {
    not_ready: std::sync::atomic::AtomicBool,
    sent: Mutex<Vec<OutgoingMail>>,
}

impl FakeMailer {
    fn sent(&self) -> Vec<OutgoingMail> {
        self.sent.lock().expect("mailer lock").clone()
    }
}

#[async_trait]
impl SubscriberMailer for FakeMailer {
    async fn readiness(&self, sender: Option<&str>) -> Result<MailReadiness, SubscriptionError> {
        let ready = !self.not_ready.load(std::sync::atomic::Ordering::SeqCst);
        Ok(judge_sender(
            ready,
            if ready {
                vec!["example.test".to_string()]
            } else {
                Vec::new()
            },
            sender,
        ))
    }

    async fn send(&self, mail: OutgoingMail) -> Result<(), SubscriptionError> {
        self.sent.lock().expect("mailer lock").push(mail);
        Ok(())
    }
}

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
    status: StatusPageService,
    subscribers: Arc<StatusPageSubscriberService>,
    mailer: Arc<FakeMailer>,
    audit: Arc<RecordingAudit>,
    admin_state: Arc<SubscriberRoutesState>,
    public: axum::Router,
    page_id: i32,
    slug: String,
    incident_id: i32,
}

fn admin() -> RequireAuth {
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
        Role::Admin,
    ))
}

fn metadata() -> Extension<RequestMetadata> {
    Extension(RequestMetadata {
        ip_address: "192.0.2.40".to_string(),
        user_agent: "subscriber-test".to_string(),
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
            println!("Docker not available, skipping status page subscription test");
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
    config_service
        .update_setting_field(|settings| {
            settings.external_url = Some("https://status-host.example.test".to_string())
        })
        .await
        .expect("set external url");

    let nanos = chrono::Utc::now().timestamp_nanos_opt().unwrap_or(0);
    let slug = format!("subs-{nanos}");
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
                name: INTERNAL_MONITOR.to_string(),
                monitor_type: "web".to_string(),
                environment_id,
                check_interval_seconds: Some(60),
                check_path: Some("/healthz".to_string()),
            },
        )
        .await
        .expect("monitor");
    let incident_id = status
        .incident_service()
        .create_incident(
            project_id,
            CreateIncidentRequest {
                title: INTERNAL_TITLE.to_string(),
                description: None,
                severity: "major".to_string(),
                environment_id: Some(environment_id),
                monitor_id: Some(monitor.id),
                ..Default::default()
            },
        )
        .await
        .expect("incident")
        .id;

    let cache = Arc::new(StatusSnapshotCache::new(db.clone()));
    let pages = Arc::new(PublicStatusPageService::new(
        db.clone(),
        config_service.clone(),
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

    let mailer = Arc::new(FakeMailer::default());
    let subscribers = Arc::new(StatusPageSubscriberService::new(
        db.clone(),
        cache.clone(),
        Arc::new(EncryptionService::new_from_password("subscriber-tests")),
        mailer.clone(),
        config_service,
    ));
    let audit = Arc::new(RecordingAudit::default());
    let admin_state = Arc::new(SubscriberRoutesState {
        subscribers: subscribers.clone(),
        pages,
        audit: audit.clone(),
        project_access_checker: None,
    });
    let access = Arc::new(crate::services::page_access::PageAccessService::new(
        db.clone(),
        cache.clone(),
        &EncryptionService::new_from_password("subscriber-tests"),
    ));
    let public = create_public_subscription_router(Arc::new(PublicSubscriptionState::new(
        cache.clone(),
        subscribers.clone(),
        access,
    )));
    let slug = status_pages::Entity::find_by_id(page.id)
        .one(db.as_ref())
        .await
        .expect("page row")
        .expect("page exists")
        .slug;
    Some(Fx {
        _db: test_db,
        db,
        cache,
        status,
        subscribers,
        mailer,
        audit,
        admin_state,
        public,
        page_id: page.id,
        slug,
        incident_id,
    })
}

async fn enable(fx: &Fx) -> SubscriptionSettingsResponse {
    decode(
        update_subscription_settings(
            admin(),
            State(fx.admin_state.clone()),
            metadata(),
            Path(fx.page_id),
            Json(UpdateSubscriptionSettingsRequest {
                enabled: Some(true),
                sender: Some(SENDER.to_string()),
            }),
        )
        .await
        .map_err(|p| format!("{:?}", p.body))
        .expect("enable subscriptions"),
    )
    .await
}

async fn decode<T: serde::de::DeserializeOwned>(response: impl IntoResponse) -> T {
    let body = axum::body::to_bytes(response.into_response().into_body(), 1 << 20)
        .await
        .expect("body");
    serde_json::from_slice(&body).expect("json")
}

async fn call(
    router: &axum::Router,
    method: &str,
    path: &str,
    form: Option<&str>,
) -> (StatusCode, String) {
    let mut request = Request::builder().method(method).uri(path);
    if form.is_some() {
        request = request.header(header::CONTENT_TYPE, "application/x-www-form-urlencoded");
    }
    let response = router
        .clone()
        .oneshot(
            request
                .body(Body::from(form.unwrap_or_default().to_string()))
                .expect("request"),
        )
        .await
        .expect("router is infallible");
    let status = response.status();
    let body = axum::body::to_bytes(response.into_body(), 1 << 20)
        .await
        .expect("body");
    (status, String::from_utf8_lossy(&body).into_owned())
}

fn token_after(text: &str, marker: &str) -> String {
    let start = text.find(marker).expect("link in mail") + marker.len();
    text[start..start + 64].to_string()
}

async fn post_update(fx: &Fx, message: &str, is_public: bool) {
    fx.status
        .incident_service()
        .update_incident_status(
            fx.incident_id,
            UpdateIncidentStatusRequest {
                status: "identified".to_string(),
                message: message.to_string(),
                is_public,
            },
        )
        .await
        .expect("incident update");
    fx.cache.refresh_page(fx.page_id).await.expect("refresh");
}

async fn subscriber_rows(fx: &Fx) -> Vec<status_page_subscribers::Model> {
    status_page_subscribers::Entity::find()
        .filter(status_page_subscribers::Column::StatusPageId.eq(fx.page_id))
        .all(fx.db.as_ref())
        .await
        .expect("subscribers")
}

#[tokio::test]
async fn a_visitor_subscribes_confirms_gets_public_updates_only_and_unsubscribes() {
    let Some(fx) = fixture().await else {
        return;
    };
    let subscribe = format!("/status/{}/subscribe", fx.slug);

    // Off by default: the route is the standard 404 and the page has no form.
    let (status, _) = call(
        &fx.public,
        "POST",
        &subscribe,
        Some("email=a%40example.test"),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    let snapshot = fx.cache.get(&fx.slug).expect("published");
    assert!(!String::from_utf8_lossy(&snapshot.html).contains("name=\"email\""));

    let settings = enable(&fx).await;
    assert!(settings.enabled && settings.email.configured && settings.links.configured);
    let snapshot = fx.cache.get(&fx.slug).expect("published");
    let html = String::from_utf8_lossy(&snapshot.html).into_owned();
    assert!(html.contains(&format!("action=\"/api/status/{}/subscribe\"", fx.slug)));
    assert!(!html.contains("<script"), "the page stays script-free");
    assert!(String::from_utf8_lossy(&snapshot.html_custom_host).contains("action=\"/subscribe\""));

    let (status, body) = call(&fx.public, "POST", &subscribe, Some("email=nope")).await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    for _ in 0..2 {
        let (status, body) = call(
            &fx.public,
            "POST",
            &subscribe,
            Some("email=Visitor%40Example.TEST"),
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        assert!(body.contains("Check your inbox"));
    }
    let rows = subscriber_rows(&fx).await;
    assert_eq!(rows.len(), 1, "a repeated request does not add a row");
    assert_eq!(rows[0].email, "visitor@example.test");
    assert_eq!(rows[0].state, "pending");

    let mut state = DispatcherState::default();
    let report = fx
        .subscribers
        .dispatch_tick(&mut state, chrono::Utc::now())
        .await
        .expect("tick");
    assert_eq!(report.confirmations_sent, 1);
    let mail = fx.mailer.sent().pop().expect("confirmation mail");
    assert_eq!(mail.to, "visitor@example.test");
    assert_eq!(mail.from, SENDER);
    let token = token_after(&mail.text, "/confirm/");
    assert!(mail.text.contains(&format!(
        "https://status-host.example.test/api/status/{}/confirm/",
        fx.slug
    )));
    let stored = subscriber_rows(&fx).await.remove(0);
    assert_ne!(stored.confirm_token_hash.as_deref(), Some(token.as_str()));

    // Opening the link only shows a button: nothing changes until it posts.
    let confirm = format!("/status/{}/confirm/{token}", fx.slug);
    let (status, body) = call(&fx.public, "GET", &confirm, None).await;
    assert_eq!(status, StatusCode::OK);
    assert!(body.contains("method=\"post\""));
    assert_eq!(subscriber_rows(&fx).await[0].state, "pending");
    let wrong = format!("/status/{}/confirm/{}", fx.slug, "0".repeat(64));
    assert_eq!(
        call(&fx.public, "POST", &wrong, None).await.0,
        StatusCode::NOT_FOUND
    );
    assert_eq!(
        call(&fx.public, "POST", &confirm, None).await.0,
        StatusCode::OK
    );
    assert_eq!(subscriber_rows(&fx).await[0].state, "confirmed");
    assert_eq!(
        call(&fx.public, "POST", &confirm, None).await.0,
        StatusCode::NOT_FOUND,
        "a confirmation link works once"
    );

    // A private update is never announced; a public one is, once.
    post_update(&fx, PRIVATE_UPDATE, false).await;
    post_update(&fx, PUBLIC_UPDATE, true).await;
    let before = fx.mailer.sent().len();
    let report = fx
        .subscribers
        .dispatch_tick(
            &mut state,
            chrono::Utc::now() + chrono::Duration::seconds(1),
        )
        .await
        .expect("tick");
    assert_eq!(report.queued, 1, "{report:?}");
    assert_eq!(report.notices_sent, 1, "{report:?}");
    let sent = fx.mailer.sent();
    assert_eq!(sent.len(), before + 1);
    let notice = sent.last().expect("notice");
    for body in [&notice.text, &notice.html, &notice.subject] {
        assert!(!body.contains(PRIVATE_UPDATE), "{body}");
        assert!(!body.contains(INTERNAL_TITLE), "{body}");
        assert!(!body.contains(INTERNAL_MONITOR), "{body}");
    }
    assert!(notice.text.contains(PUBLIC_UPDATE));
    assert!(notice.text.contains("Affected: API"));
    let unsubscribe_token = token_after(&notice.text, "/unsubscribe/");

    let report = fx
        .subscribers
        .dispatch_tick(
            &mut state,
            chrono::Utc::now() + chrono::Duration::seconds(2),
        )
        .await
        .expect("tick");
    assert_eq!(report.notices_sent, 0, "delivered once: {report:?}");
    let mut fresh = DispatcherState::default();
    let report = fx
        .subscribers
        .dispatch_tick(
            &mut fresh,
            chrono::Utc::now() + chrono::Duration::seconds(3),
        )
        .await
        .expect("tick after a restart");
    assert_eq!(
        report.notices_sent, 0,
        "a restart never resends: {report:?}"
    );

    let unsubscribe = format!("/status/{}/unsubscribe/{unsubscribe_token}", fx.slug);
    let (status, body) = call(&fx.public, "GET", &unsubscribe, None).await;
    assert_eq!(status, StatusCode::OK);
    assert!(body.contains("Unsubscribe"));
    assert_eq!(subscriber_rows(&fx).await.len(), 1, "GET changes nothing");
    assert_eq!(
        call(&fx.public, "POST", &unsubscribe, None).await.0,
        StatusCode::OK
    );
    assert!(
        subscriber_rows(&fx).await.is_empty(),
        "the address is deleted"
    );
}

#[tokio::test]
async fn subscriptions_cannot_be_switched_on_without_a_working_sender() {
    let Some(fx) = fixture().await else {
        return;
    };
    fx.mailer
        .not_ready
        .store(true, std::sync::atomic::Ordering::SeqCst);
    let settings: SubscriptionSettingsResponse = decode(
        get_subscription_settings(admin(), State(fx.admin_state.clone()), Path(fx.page_id))
            .await
            .map_err(|p| format!("{:?}", p.body))
            .expect("settings"),
    )
    .await;
    assert!(!settings.email.configured);
    assert_eq!(
        settings.email.reason.as_deref(),
        Some("No email provider is configured")
    );
    assert_eq!(settings.email.setup_path.as_deref(), Some("/email"));

    let error = update_subscription_settings(
        admin(),
        State(fx.admin_state.clone()),
        metadata(),
        Path(fx.page_id),
        Json(UpdateSubscriptionSettingsRequest {
            enabled: Some(true),
            sender: Some(SENDER.to_string()),
        }),
    )
    .await
    .err()
    .expect("refused");
    assert_eq!(error.status_code, StatusCode::CONFLICT);

    fx.mailer
        .not_ready
        .store(false, std::sync::atomic::Ordering::SeqCst);
    let error = update_subscription_settings(
        admin(),
        State(fx.admin_state.clone()),
        metadata(),
        Path(fx.page_id),
        Json(UpdateSubscriptionSettingsRequest {
            enabled: None,
            sender: Some("status@unverified.test".to_string()),
        }),
    )
    .await
    .err()
    .expect("unverified sender refused");
    assert_eq!(error.status_code, StatusCode::BAD_REQUEST);

    let settings = enable(&fx).await;
    assert!(settings.enabled);
    let audit = fx.audit.entries.lock().expect("audit").clone();
    let (operation, payload) = audit.last().expect("audited");
    assert_eq!(operation, "STATUS_PAGE_SUBSCRIPTIONS_UPDATED");
    assert_eq!(payload["enabled"], true);
    assert_eq!(payload["status_page_id"], fx.page_id);
}

#[tokio::test]
async fn admins_list_export_and_delete_and_the_page_takes_its_subscribers_with_it() {
    let Some(fx) = fixture().await else {
        return;
    };
    enable(&fx).await;
    let snapshot = fx.cache.get(&fx.slug).expect("published");
    for email in [
        "one@example.test",
        "=two@example.test",
        "three@example.test",
    ] {
        fx.subscribers
            .subscribe(&snapshot, email)
            .await
            .expect("subscribe");
    }

    let list: SubscriberListResponse = decode(
        list_subscribers(
            admin(),
            State(fx.admin_state.clone()),
            Path(fx.page_id),
            Query(SubscriberListQuery {
                page: Some(1),
                page_size: Some(2),
            }),
        )
        .await
        .map_err(|p| format!("{:?}", p.body))
        .expect("list"),
    )
    .await;
    assert_eq!(list.total, 3);
    assert_eq!(list.items.len(), 2);
    assert_eq!(list.items[0].email, "three@example.test", "newest first");

    let response = export_subscribers(
        admin(),
        State(fx.admin_state.clone()),
        metadata(),
        Path(fx.page_id),
    )
    .await
    .map_err(|p| format!("{:?}", p.body))
    .expect("export")
    .into_response();
    assert_eq!(
        response
            .headers()
            .get(header::CACHE_CONTROL)
            .map(|v| v.as_bytes()),
        Some(&b"no-store"[..])
    );
    let csv = String::from_utf8(
        axum::body::to_bytes(response.into_body(), 1 << 20)
            .await
            .expect("csv")
            .to_vec(),
    )
    .expect("utf8");
    assert!(csv.starts_with("email,state,subscribed_at,confirmed_at\n"));
    assert!(csv.contains("\"'=two@example.test\""), "{csv}");
    assert_eq!(csv.lines().count(), 4);

    let id = list.items[0].id;
    delete_subscriber(
        admin(),
        State(fx.admin_state.clone()),
        metadata(),
        Path((fx.page_id, id)),
    )
    .await
    .map_err(|p| format!("{:?}", p.body))
    .expect("delete");
    let error = delete_subscriber(
        admin(),
        State(fx.admin_state.clone()),
        metadata(),
        Path((fx.page_id, id)),
    )
    .await
    .err()
    .expect("already gone");
    assert_eq!(error.status_code, StatusCode::NOT_FOUND);

    let audit = fx.audit.entries.lock().expect("audit").clone();
    let operations: Vec<&str> = audit.iter().map(|(op, _)| op.as_str()).collect();
    assert!(operations.contains(&"STATUS_PAGE_SUBSCRIBERS_EXPORTED"));
    assert!(operations.contains(&"STATUS_PAGE_SUBSCRIBER_DELETED"));
    let serialized = serde_json::to_string(&audit).expect("json");
    for address in ["one@", "two@", "three@"] {
        assert!(
            !serialized.contains(address),
            "no subscriber address in the audit log: {serialized}"
        );
    }

    status_pages::Entity::delete_by_id(fx.page_id)
        .exec(fx.db.as_ref())
        .await
        .expect("delete page");
    assert_eq!(
        status_page_subscribers::Entity::find()
            .filter(status_page_subscribers::Column::StatusPageId.eq(fx.page_id))
            .count(fx.db.as_ref())
            .await
            .expect("count"),
        0
    );
}

#[tokio::test]
async fn a_notice_is_cancelled_when_its_update_stops_being_public() {
    let Some(fx) = fixture().await else {
        return;
    };
    enable(&fx).await;
    let snapshot = fx.cache.get(&fx.slug).expect("published");
    fx.subscribers
        .subscribe(&snapshot, "reader@example.test")
        .await
        .expect("subscribe");
    status_page_subscribers::Entity::update_many()
        .col_expr(
            status_page_subscribers::Column::State,
            sea_orm::sea_query::Expr::value("confirmed"),
        )
        .col_expr(
            status_page_subscribers::Column::ConfirmedAt,
            sea_orm::sea_query::Expr::value(chrono::Utc::now() - chrono::Duration::minutes(1)),
        )
        .exec(fx.db.as_ref())
        .await
        .expect("confirm");

    // Queued while the sender cannot send, so nothing goes out yet.
    post_update(&fx, PUBLIC_UPDATE, true).await;
    fx.mailer
        .not_ready
        .store(true, std::sync::atomic::Ordering::SeqCst);
    let mut state = DispatcherState::default();
    let report = fx
        .subscribers
        .dispatch_tick(&mut state, chrono::Utc::now())
        .await
        .expect("tick");
    assert_eq!(report.queued, 1);
    assert_eq!(report.notices_sent, 0);

    // The admin takes the update back before mail can go out.
    status_incident_updates::Entity::update_many()
        .col_expr(
            status_incident_updates::Column::IsPublic,
            sea_orm::sea_query::Expr::value(false),
        )
        .filter(status_incident_updates::Column::IncidentId.eq(fx.incident_id))
        .exec(fx.db.as_ref())
        .await
        .expect("make private");
    fx.cache.refresh_page(fx.page_id).await.expect("refresh");
    fx.mailer
        .not_ready
        .store(false, std::sync::atomic::Ordering::SeqCst);
    let report = fx
        .subscribers
        .dispatch_tick(&mut state, chrono::Utc::now())
        .await
        .expect("tick");
    assert_eq!(report.notices_cancelled, 1, "{report:?}");
    assert_eq!(report.notices_sent, 0);
    assert!(fx
        .mailer
        .sent()
        .iter()
        .all(|m| !m.text.contains(PUBLIC_UPDATE)));
    let open = status_page_notices::Entity::find()
        .filter(status_page_notices::Column::CompletedAt.is_null())
        .count(fx.db.as_ref())
        .await
        .expect("count");
    assert_eq!(open, 0);
}

#[tokio::test]
async fn unconfirmed_requests_expire_and_their_addresses_are_deleted() {
    let Some(fx) = fixture().await else {
        return;
    };
    enable(&fx).await;
    let snapshot = fx.cache.get(&fx.slug).expect("published");
    for email in ["old@example.test", "new@example.test"] {
        fx.subscribers
            .subscribe(&snapshot, email)
            .await
            .expect("subscribe");
    }
    status_page_subscribers::Entity::update_many()
        .col_expr(
            status_page_subscribers::Column::RequestedAt,
            sea_orm::sea_query::Expr::value(chrono::Utc::now() - chrono::Duration::hours(49)),
        )
        .filter(status_page_subscribers::Column::Email.eq("old@example.test"))
        .exec(fx.db.as_ref())
        .await
        .expect("age");
    let mut state = DispatcherState::default();
    fx.subscribers
        .dispatch_tick(&mut state, chrono::Utc::now())
        .await
        .expect("tick");
    let emails: HashSet<String> = subscriber_rows(&fx)
        .await
        .into_iter()
        .map(|row| row.email)
        .collect();
    assert_eq!(emails, HashSet::from(["new@example.test".to_string()]));
}

#[tokio::test]
async fn the_admin_routes_answer_401_without_credentials() {
    let Some(fx) = fixture().await else {
        return;
    };
    let router = crate::routes::subscribers::create_subscriber_router(fx.admin_state.clone());
    let base = format!("/status-pages/{}", fx.page_id);
    for (method, path) in [
        ("GET", format!("{base}/subscriptions")),
        ("PUT", format!("{base}/subscriptions")),
        ("GET", format!("{base}/subscribers")),
        ("GET", format!("{base}/subscribers/export")),
        ("DELETE", format!("{base}/subscribers/1")),
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

#[tokio::test]
async fn a_protected_page_takes_subscriptions_only_from_visitors_who_unlocked_it() {
    let Some(fx) = fixture().await else {
        return;
    };
    enable(&fx).await;
    // Same master key as the fixture's public router, so its cookies verify.
    let access = crate::services::page_access::PageAccessService::new(
        fx.db.clone(),
        fx.cache.clone(),
        &EncryptionService::new_from_password("subscriber-tests"),
    );
    access
        .set_password(
            fx.page_id,
            crate::services::page_access::SetPagePasswordRequest {
                password: Some("page password".to_string()),
            },
        )
        .await
        .expect("protect");
    let subscribe = format!("/status/{}/subscribe", fx.slug);
    let (status, body) = call(
        &fx.public,
        "POST",
        &subscribe,
        Some("email=a%40example.test"),
    )
    .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    assert!(body.contains("type=\"password\""), "{body}");
    assert!(subscriber_rows(&fx).await.is_empty(), "nothing is stored");
    assert!(fx.mailer.sent().is_empty());

    let snapshot = fx.cache.get(&fx.slug).expect("published");
    let crate::services::page_access::UnlockOutcome::Unlocked(cookie) = access
        .unlock(&snapshot, "page password", "192.0.2.50")
        .await
    else {
        panic!("the right password unlocks");
    };
    let response = fx
        .public
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(&subscribe)
                .header(header::CONTENT_TYPE, "application/x-www-form-urlencoded")
                .header(header::COOKIE, format!("temps_status_access={cookie}"))
                .body(Body::from("email=a%40example.test"))
                .expect("request"),
        )
        .await
        .expect("router is infallible");
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(subscriber_rows(&fx).await.len(), 1);
}
