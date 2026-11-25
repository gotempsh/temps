// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Database-backed tests of status page webhooks (ADR 108 D2): registration,
//! delivery, retries and cancellation, and the admin handlers.

use std::collections::VecDeque;
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use axum::body::Body;
use axum::extract::{Extension, Path, State};
use axum::http::{header, Request, StatusCode};
use axum::response::IntoResponse;
use axum::Json;
use chrono::{Duration, Utc};
use sea_orm::{ActiveModelTrait, ColumnTrait, EntityTrait, QueryFilter, Set};
use temps_authz::permissions::Role;
use temps_authz::{AuthContext, RequireAuth};
use temps_config::ConfigService;
use temps_core::{AuditLogger, AuditOperation, EncryptionService, RequestMetadata};
use temps_entities::{
    environments, projects, status_page_webhook_deliveries, status_page_webhooks,
    upstream_config::UpstreamList, users,
};
use temps_test_utils::{is_container_runtime_unavailable, TestDatabase};
use tower::ServiceExt;

use super::dispatcher::{WebhookDispatcherState, MAX_ATTEMPTS};
use super::payload::{signature, WebhookPayload, SIGNATURE_HEADER};
use super::service::MAX_WEBHOOKS_PER_PAGE;
use super::{
    CreateStatusPageWebhookRequest, StatusPageWebhookService, TransportError,
    UpdateStatusPageWebhookRequest, WebhookError, WebhookTransport,
};
use crate::routes::webhooks::{
    create_webhook, create_webhook_router, get_webhook, WebhookRoutesState,
};
use crate::services::maintenance::{
    CreateMaintenanceRequest, StatusPageMaintenanceService, UpdateMaintenanceRequest,
};
use crate::services::public_pages::{
    CreateStatusPageRequest, PublicStatusPageService, StatusPageMonitorInput, StatusSnapshotCache,
};
use crate::services::{CreateMonitorRequest, StatusPageService};

const URL: &str = "https://hooks.example.test/status";

#[derive(Clone, Debug)]
struct Sent {
    url: String,
    headers: Vec<(&'static str, String)>,
    body: String,
}

/// A one-shot async action run from inside the transport, to change the
/// world while a delivery is in flight.
type Hook =
    Box<dyn FnOnce() -> std::pin::Pin<Box<dyn std::future::Future<Output = ()> + Send>> + Send>;

#[derive(Default)]
struct FakeTransport {
    /// Statuses to answer with, in order; 204 once empty.
    answers: Mutex<VecDeque<Result<u16, TransportError>>>,
    refuse_check: std::sync::atomic::AtomicBool,
    sent: Mutex<Vec<Sent>>,
    /// Run once, during the next post.
    during_post: Mutex<Option<Hook>>,
    /// Pause in every post, so concurrent dispatchers interleave.
    delay: Mutex<Option<std::time::Duration>>,
}

fn hook<F>(future: F) -> Option<Hook>
where
    F: std::future::Future<Output = ()> + Send + 'static,
{
    Some(Box::new(move || Box::pin(future)))
}

impl FakeTransport {
    fn sent(&self) -> Vec<Sent> {
        self.sent.lock().expect("lock").clone()
    }

    fn answer(&self, answer: Result<u16, TransportError>) {
        self.answers.lock().expect("lock").push_back(answer);
    }
}

#[async_trait]
impl WebhookTransport for FakeTransport {
    async fn check(&self, _url: &str) -> Result<(), TransportError> {
        if self.refuse_check.load(std::sync::atomic::Ordering::SeqCst) {
            Err(TransportError::NonPublic {
                reason: "10.0.0.5".to_string(),
            })
        } else {
            Ok(())
        }
    }

    async fn post(
        &self,
        url: &str,
        headers: Vec<(&'static str, String)>,
        body: String,
    ) -> Result<u16, TransportError> {
        let during = self.during_post.lock().expect("lock").take();
        if let Some(during) = during {
            during().await;
        }
        let delay = *self.delay.lock().expect("lock");
        if let Some(delay) = delay {
            tokio::time::sleep(delay).await;
        }
        self.sent.lock().expect("lock").push(Sent {
            url: url.to_string(),
            headers,
            body,
        });
        self.answers
            .lock()
            .expect("lock")
            .pop_front()
            .unwrap_or(Ok(204))
    }
}

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
    db: Arc<sea_orm::DatabaseConnection>,
    webhooks: Arc<StatusPageWebhookService>,
    maintenance: StatusPageMaintenanceService,
    transport: Arc<FakeTransport>,
    audit: Arc<RecordingAudit>,
    state: Arc<WebhookRoutesState>,
    page_id: i32,
    monitor_id: i32,
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
        ip_address: "192.0.2.43".to_string(),
        user_agent: "webhook-test".to_string(),
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
            println!("Docker not available, skipping status page webhook test");
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
    let slug = format!("hook-{nanos}");
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
    let transport = Arc::new(FakeTransport::default());
    let webhooks = Arc::new(StatusPageWebhookService::new(
        db.clone(),
        cache.clone(),
        Arc::new(EncryptionService::new_from_password("webhook-tests")),
        transport.clone(),
    ));
    let audit = Arc::new(RecordingAudit::default());
    let state = Arc::new(WebhookRoutesState {
        webhooks: webhooks.clone(),
        pages,
        audit: audit.clone(),
        project_access_checker: None,
    });
    Some(Fx {
        _db: test_db,
        maintenance: StatusPageMaintenanceService::new(db.clone(), cache),
        db,
        webhooks,
        transport,
        audit,
        state,
        page_id: page.id,
        monitor_id: monitor.id,
    })
}

async fn schedule(fx: &Fx, title: &str) {
    let starts_at = Utc::now() + Duration::hours(2);
    fx.maintenance
        .create(
            fx.page_id,
            CreateMaintenanceRequest {
                title: title.to_string(),
                description: None,
                starts_at,
                ends_at: starts_at + Duration::hours(1),
                monitor_ids: vec![fx.monitor_id],
            },
        )
        .await
        .expect("schedule");
}

async fn deliveries(fx: &Fx) -> Vec<status_page_webhook_deliveries::Model> {
    status_page_webhook_deliveries::Entity::find()
        .all(fx.db.as_ref())
        .await
        .expect("deliveries")
}

#[tokio::test]
async fn a_new_event_is_posted_once_signed_with_the_secret_shown_at_creation() {
    let Some(fx) = fixture().await else {
        return;
    };
    // An event from before the webhook existed is never sent to it.
    schedule(&fx, "Earlier work").await;
    let created = fx
        .webhooks
        .create(
            fx.page_id,
            CreateStatusPageWebhookRequest {
                url: URL.to_string(),
                enabled: true,
            },
        )
        .await
        .expect("create");
    let stored = status_page_webhooks::Entity::find_by_id(created.webhook.id)
        .one(fx.db.as_ref())
        .await
        .expect("row")
        .expect("exists");
    assert_ne!(stored.secret_encrypted, created.secret, "encrypted at rest");
    assert!(!stored.secret_encrypted.contains(&created.secret));

    schedule(&fx, "Database upgrade").await;
    let mut state = WebhookDispatcherState::default();
    let report = fx
        .webhooks
        .dispatch_tick(&mut state, Utc::now())
        .await
        .expect("tick");
    assert_eq!(report.queued, 1, "{report:?}");
    assert_eq!(report.delivered, 1, "{report:?}");
    let sent = fx.transport.sent();
    assert_eq!(sent.len(), 1);
    assert_eq!(sent[0].url, URL);
    let payload: WebhookPayload = serde_json::from_str(&sent[0].body).expect("typed body");
    assert_eq!(payload.kind, "maintenance_scheduled");
    assert_eq!(payload.event.title, "Database upgrade");
    assert_eq!(payload.event.affected_components, vec!["API".to_string()]);
    assert!(!sent[0].body.contains("internal-monitor"));
    let header = sent[0]
        .headers
        .iter()
        .find(|(name, _)| *name == SIGNATURE_HEADER)
        .map(|(_, value)| value.clone())
        .expect("signed");
    let timestamp: i64 = header
        .trim_start_matches("t=")
        .split(',')
        .next()
        .and_then(|t| t.parse().ok())
        .expect("timestamp");
    assert_eq!(header, signature(&created.secret, timestamp, &sent[0].body));

    // Later ticks send nothing more.
    let again = fx
        .webhooks
        .dispatch_tick(&mut state, Utc::now())
        .await
        .expect("tick");
    assert_eq!(again.delivered, 0);
    let mut fresh = WebhookDispatcherState::default();
    fx.webhooks
        .dispatch_tick(&mut fresh, Utc::now())
        .await
        .expect("tick after a restart");
    assert_eq!(fx.transport.sent().len(), 1, "a restart re-sends nothing");
    let hook = fx
        .webhooks
        .list(fx.page_id, 1, 20)
        .await
        .expect("list")
        .items
        .remove(0);
    assert!(hook.last_success_at.is_some() && hook.last_failure_reason.is_none());
}

#[tokio::test]
async fn failures_are_retried_then_marked_failed_with_the_reason_shown() {
    let Some(fx) = fixture().await else {
        return;
    };
    fx.webhooks
        .create(
            fx.page_id,
            CreateStatusPageWebhookRequest {
                url: URL.to_string(),
                enabled: true,
            },
        )
        .await
        .expect("create");
    schedule(&fx, "Database upgrade").await;
    for _ in 0..MAX_ATTEMPTS {
        fx.transport.answer(Ok(500));
    }
    let mut state = WebhookDispatcherState::default();
    let mut now = Utc::now();
    let first = fx
        .webhooks
        .dispatch_tick(&mut state, now)
        .await
        .expect("tick");
    assert_eq!(first.retried, 1, "{first:?}");
    let not_yet = fx
        .webhooks
        .dispatch_tick(&mut state, now)
        .await
        .expect("tick");
    assert_eq!(
        fx.transport.sent().len(),
        1,
        "waits for the retry delay: {not_yet:?}"
    );
    now += Duration::minutes(2);
    fx.webhooks
        .dispatch_tick(&mut state, now)
        .await
        .expect("tick");
    now += Duration::minutes(11);
    let last = fx
        .webhooks
        .dispatch_tick(&mut state, now)
        .await
        .expect("tick");
    assert_eq!(last.failed, 1, "{last:?}");
    assert_eq!(fx.transport.sent().len(), MAX_ATTEMPTS as usize);
    let rows = deliveries(&fx).await;
    assert_eq!(rows[0].attempts, MAX_ATTEMPTS);
    assert!(rows[0].failed_at.is_some());
    let hook = fx
        .webhooks
        .list(fx.page_id, 1, 20)
        .await
        .expect("list")
        .items
        .remove(0);
    assert_eq!(
        hook.last_failure_reason.as_deref(),
        Some("the endpoint answered HTTP 500")
    );
    now += Duration::hours(1);
    fx.webhooks
        .dispatch_tick(&mut state, now)
        .await
        .expect("tick");
    assert_eq!(
        fx.transport.sent().len(),
        MAX_ATTEMPTS as usize,
        "no more attempts"
    );
}

#[tokio::test]
async fn a_backlog_larger_than_one_insert_is_queued_and_sent_in_full() {
    let Some(fx) = fixture().await else {
        return;
    };
    fx.webhooks
        .create(
            fx.page_id,
            CreateStatusPageWebhookRequest {
                url: URL.to_string(),
                enabled: true,
            },
        )
        .await
        .expect("create");
    let windows = super::dispatcher::INSERT_CHUNK * 2 + 1;
    for index in 0..windows {
        schedule(&fx, &format!("Upgrade {index}")).await;
    }
    let mut state = WebhookDispatcherState::default();
    let report = fx
        .webhooks
        .dispatch_tick(&mut state, Utc::now())
        .await
        .expect("tick");
    assert_eq!(report.queued, windows, "{report:?}");
    assert_eq!(deliveries(&fx).await.len(), windows);
    assert_eq!(fx.transport.sent().len(), windows);
}

#[tokio::test]
async fn a_retry_sends_the_event_as_the_page_shows_it_now() {
    let Some(fx) = fixture().await else {
        return;
    };
    fx.webhooks
        .create(
            fx.page_id,
            CreateStatusPageWebhookRequest {
                url: URL.to_string(),
                enabled: true,
            },
        )
        .await
        .expect("create");
    let starts_at = Utc::now() + Duration::hours(2);
    let window = fx
        .maintenance
        .create(
            fx.page_id,
            CreateMaintenanceRequest {
                title: "Database upgrade".to_string(),
                description: None,
                starts_at,
                ends_at: starts_at + Duration::hours(1),
                monitor_ids: vec![fx.monitor_id],
            },
        )
        .await
        .expect("schedule");
    fx.transport.answer(Ok(500));
    let mut state = WebhookDispatcherState::default();
    let mut now = Utc::now();
    fx.webhooks
        .dispatch_tick(&mut state, now)
        .await
        .expect("first attempt");

    fx.maintenance
        .update(
            fx.page_id,
            window.id,
            UpdateMaintenanceRequest {
                title: Some("Database upgrade, moved to Sunday".to_string()),
                ..Default::default()
            },
        )
        .await
        .expect("edit");
    now += Duration::minutes(2);
    fx.webhooks
        .dispatch_tick(&mut state, now)
        .await
        .expect("retry");

    let sent = fx.transport.sent();
    assert_eq!(sent.len(), 2);
    assert!(
        sent[0].body.contains("\"Database upgrade\""),
        "{}",
        sent[0].body
    );
    assert!(
        sent[1].body.contains("Database upgrade, moved to Sunday"),
        "the retry carries the edited title: {}",
        sent[1].body
    );
}

#[tokio::test]
async fn a_disabled_webhook_gets_nothing_and_its_pending_events_are_cancelled() {
    let Some(fx) = fixture().await else {
        return;
    };
    let created = fx
        .webhooks
        .create(
            fx.page_id,
            CreateStatusPageWebhookRequest {
                url: URL.to_string(),
                enabled: true,
            },
        )
        .await
        .expect("create");
    schedule(&fx, "Database upgrade").await;
    fx.transport.answer(Err(TransportError::Connect));
    let mut state = WebhookDispatcherState::default();
    let now = Utc::now();
    fx.webhooks
        .dispatch_tick(&mut state, now)
        .await
        .expect("tick");
    fx.webhooks
        .update(
            fx.page_id,
            created.webhook.id,
            UpdateStatusPageWebhookRequest {
                enabled: Some(false),
                ..Default::default()
            },
        )
        .await
        .expect("disable");
    let report = fx
        .webhooks
        .dispatch_tick(&mut state, now + Duration::minutes(5))
        .await
        .expect("tick");
    assert_eq!(report.cancelled, 1, "{report:?}");
    assert_eq!(fx.transport.sent().len(), 1);
    let rows = deliveries(&fx).await;
    assert_eq!(
        rows[0].last_error.as_deref(),
        Some("the webhook was disabled")
    );
}

#[tokio::test]
async fn registration_is_validated_bounded_and_scoped_to_the_page() {
    let Some(fx) = fixture().await else {
        return;
    };
    let err = fx
        .webhooks
        .create(
            fx.page_id,
            CreateStatusPageWebhookRequest {
                url: "http://hooks.example.test/".to_string(),
                enabled: true,
            },
        )
        .await
        .expect_err("plain http");
    assert!(matches!(err, WebhookError::Validation { .. }));
    fx.transport
        .refuse_check
        .store(true, std::sync::atomic::Ordering::SeqCst);
    let err = fx
        .webhooks
        .create(
            fx.page_id,
            CreateStatusPageWebhookRequest {
                url: URL.to_string(),
                enabled: true,
            },
        )
        .await
        .expect_err("non-public address");
    assert!(err.to_string().contains("not public"), "{err}");
    fx.transport
        .refuse_check
        .store(false, std::sync::atomic::Ordering::SeqCst);
    assert!(matches!(
        fx.webhooks
            .create(
                9_999_999,
                CreateStatusPageWebhookRequest {
                    url: URL.to_string(),
                    enabled: true,
                }
            )
            .await,
        Err(WebhookError::StatusPageNotFound {
            status_page_id: 9_999_999
        })
    ));

    let mut first = None;
    for _ in 0..MAX_WEBHOOKS_PER_PAGE {
        let created = fx
            .webhooks
            .create(
                fx.page_id,
                CreateStatusPageWebhookRequest {
                    url: URL.to_string(),
                    enabled: true,
                },
            )
            .await
            .expect("within the limit");
        first.get_or_insert(created.webhook.id);
    }
    assert!(matches!(
        fx.webhooks
            .create(
                fx.page_id,
                CreateStatusPageWebhookRequest {
                    url: URL.to_string(),
                    enabled: true,
                }
            )
            .await,
        Err(WebhookError::TooMany { limit: 10, .. })
    ));
    let first = first.expect("created");
    assert!(matches!(
        fx.webhooks.delete(fx.page_id + 1, first).await,
        Err(WebhookError::NotFound { .. })
    ));
    fx.webhooks.delete(fx.page_id, first).await.expect("delete");
    let remaining = status_page_webhooks::Entity::find()
        .filter(status_page_webhooks::Column::StatusPageId.eq(fx.page_id))
        .all(fx.db.as_ref())
        .await
        .expect("rows");
    assert_eq!(remaining.len() as u64, MAX_WEBHOOKS_PER_PAGE - 1);
}

#[tokio::test]
async fn the_handlers_authorize_return_the_secret_once_and_audit_without_it() {
    let Some(fx) = fixture().await else {
        return;
    };
    let forbidden = create_webhook(
        auth(Role::Reader),
        State(fx.state.clone()),
        metadata(),
        Path(fx.page_id),
        Json(CreateStatusPageWebhookRequest {
            url: URL.to_string(),
            enabled: true,
        }),
    )
    .await
    .err()
    .expect("a reader cannot register");
    assert_eq!(forbidden.status_code, StatusCode::FORBIDDEN);

    let response = create_webhook(
        auth(Role::Admin),
        State(fx.state.clone()),
        metadata(),
        Path(fx.page_id),
        Json(CreateStatusPageWebhookRequest {
            url: URL.to_string(),
            enabled: true,
        }),
    )
    .await
    .map_err(|p| format!("{:?}", p.body))
    .expect("admin registers")
    .into_response();
    assert_eq!(response.status(), StatusCode::CREATED);
    assert_eq!(
        response
            .headers()
            .get(header::CACHE_CONTROL)
            .and_then(|v| v.to_str().ok()),
        Some("no-store")
    );
    let body = axum::body::to_bytes(response.into_body(), 1 << 20)
        .await
        .expect("body");
    let created: super::StatusPageWebhookCreatedResponse =
        serde_json::from_slice(&body).expect("json");
    assert!(created.secret.starts_with("whsec_"));

    let listed = serde_json::to_string(&fx.webhooks.list(fx.page_id, 1, 20).await.expect("list"))
        .expect("json");
    assert!(!listed.contains(&created.secret), "never listed again");
    let entries = fx.audit.entries.lock().expect("audit").clone();
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0].0, "STATUS_PAGE_WEBHOOK_CREATED");
    assert!(!entries[0].1.contains(&created.secret));
    assert!(entries[0].1.contains(URL));

    let router = create_webhook_router(fx.state.clone());
    let base = format!("/status-pages/{}/webhooks", fx.page_id);
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

async fn one_webhook(fx: &Fx) -> i32 {
    fx.webhooks
        .create(
            fx.page_id,
            CreateStatusPageWebhookRequest {
                url: URL.to_string(),
                enabled: true,
            },
        )
        .await
        .expect("create")
        .webhook
        .id
}

#[tokio::test]
async fn two_dispatchers_on_one_database_post_each_event_once() {
    let Some(fx) = fixture().await else {
        return;
    };
    one_webhook(&fx).await;
    for index in 0..5 {
        schedule(&fx, &format!("Upgrade {index}")).await;
    }
    *fx.transport.delay.lock().expect("lock") = Some(std::time::Duration::from_millis(30));
    let other = StatusPageWebhookService::new(
        fx.db.clone(),
        fx.webhooks.cache.clone(),
        Arc::new(EncryptionService::new_from_password("webhook-tests")),
        fx.transport.clone(),
    );
    let (mut one, mut two) = (
        WebhookDispatcherState::default(),
        WebhookDispatcherState::default(),
    );
    let now = Utc::now();
    let (a, b) = tokio::join!(
        fx.webhooks.dispatch_tick(&mut one, now),
        other.dispatch_tick(&mut two, now)
    );
    let (a, b) = (a.expect("first tick"), b.expect("second tick"));
    assert_eq!(a.delivered + b.delivered, 5, "{a:?} {b:?}");
    assert_eq!(fx.transport.sent().len(), 5, "every event is posted once");
    for row in deliveries(&fx).await {
        assert_eq!(row.attempts, 1, "{row:?}");
        assert!(row.delivered_at.is_some(), "{row:?}");
    }
}

#[tokio::test]
async fn a_webhook_disabled_while_a_group_is_sending_gets_no_more_requests() {
    let Some(fx) = fixture().await else {
        return;
    };
    let id = one_webhook(&fx).await;
    let events = super::dispatcher::CONCURRENCY + 2;
    for index in 0..events {
        schedule(&fx, &format!("Upgrade {index}")).await;
    }
    let db = fx.db.clone();
    *fx.transport.during_post.lock().expect("lock") = hook(async move {
        status_page_webhooks::Entity::update_many()
            .col_expr(
                status_page_webhooks::Column::Enabled,
                sea_orm::sea_query::Expr::value(false),
            )
            .filter(status_page_webhooks::Column::Id.eq(id))
            .exec(db.as_ref())
            .await
            .expect("disable");
    });
    let report = fx
        .webhooks
        .dispatch_tick(&mut WebhookDispatcherState::default(), Utc::now())
        .await
        .expect("tick");
    assert_eq!(
        fx.transport.sent().len(),
        super::dispatcher::CONCURRENCY,
        "only the group already sending goes out"
    );
    assert_eq!(report.cancelled, 2, "{report:?}");
}

#[tokio::test]
async fn a_url_edited_while_a_group_is_sending_is_used_for_the_next_group() {
    let Some(fx) = fixture().await else {
        return;
    };
    let id = one_webhook(&fx).await;
    let events = super::dispatcher::CONCURRENCY + 1;
    for index in 0..events {
        schedule(&fx, &format!("Upgrade {index}")).await;
    }
    let db = fx.db.clone();
    *fx.transport.during_post.lock().expect("lock") = hook(async move {
        status_page_webhooks::Entity::update_many()
            .col_expr(
                status_page_webhooks::Column::Url,
                sea_orm::sea_query::Expr::value("https://hooks.example.test/moved"),
            )
            .filter(status_page_webhooks::Column::Id.eq(id))
            .exec(db.as_ref())
            .await
            .expect("edit url");
    });
    fx.webhooks
        .dispatch_tick(&mut WebhookDispatcherState::default(), Utc::now())
        .await
        .expect("tick");
    let sent = fx.transport.sent();
    assert_eq!(sent.len(), events);
    assert_eq!(
        sent.last().expect("last").url,
        "https://hooks.example.test/moved"
    );
}

#[tokio::test]
async fn queueing_a_backlog_holds_one_insert_batch_at_a_time() {
    let Some(fx) = fixture().await else {
        return;
    };
    let webhooks = super::dispatcher::WEBHOOKS_PER_QUERY as usize * 2 + 1;
    for index in 0..webhooks {
        fx.webhooks
            .create(
                fx.page_id,
                CreateStatusPageWebhookRequest {
                    url: format!("https://hooks.example.test/{index}"),
                    enabled: true,
                },
            )
            .await
            .expect("create");
    }
    for index in 0..3 {
        schedule(&fx, &format!("Upgrade {index}")).await;
    }
    let mut state = WebhookDispatcherState::default();
    let report = fx
        .webhooks
        .dispatch_tick(&mut state, Utc::now())
        .await
        .expect("tick");
    assert_eq!(report.queued, webhooks * 3, "{report:?}");
    assert_eq!(deliveries(&fx).await.len(), webhooks * 3);
    assert!(
        state.peak_buffered() <= super::dispatcher::INSERT_CHUNK,
        "held {} payloads at once",
        state.peak_buffered()
    );
}

#[tokio::test]
async fn an_attempt_interrupted_after_its_claim_is_retried_and_then_given_up() {
    let Some(fx) = fixture().await else {
        return;
    };
    one_webhook(&fx).await;
    schedule(&fx, "Database upgrade").await;
    let now = Utc::now();
    let mut state = WebhookDispatcherState::default();
    // The first attempt fails and is due again a minute later.
    fx.transport.answer(Ok(500));
    fx.webhooks
        .dispatch_tick(&mut state, now)
        .await
        .expect("first attempt");
    // A dispatcher claims the last attempt and stops before recording it.
    status_page_webhook_deliveries::Entity::update_many()
        .col_expr(
            status_page_webhook_deliveries::Column::Attempts,
            sea_orm::sea_query::Expr::value(MAX_ATTEMPTS),
        )
        .col_expr(
            status_page_webhook_deliveries::Column::NextAttemptAt,
            sea_orm::sea_query::Expr::value(now + super::dispatcher::CLAIM_LEASE),
        )
        .exec(fx.db.as_ref())
        .await
        .expect("interrupted claim");
    let report = fx
        .webhooks
        .dispatch_tick(&mut state, now + Duration::minutes(2))
        .await
        .expect("inside the lease");
    assert_eq!(report, Default::default(), "the lease holds the attempt");
    let report = fx
        .webhooks
        .dispatch_tick(&mut state, now + Duration::minutes(6))
        .await
        .expect("after the lease");
    assert_eq!(report.cancelled, 1, "{report:?}");
    assert_eq!(fx.transport.sent().len(), 1);
    let row = deliveries(&fx).await.remove(0);
    assert!(row.failed_at.is_some());
    assert!(row
        .last_error
        .as_deref()
        .is_some_and(|error| error.contains("interrupted")));
}

#[test]
fn a_webhook_is_created_enabled_unless_told_otherwise() {
    let request: CreateStatusPageWebhookRequest =
        serde_json::from_str(r#"{"url":"https://hooks.example.test/status"}"#).expect("json");
    assert!(request.enabled);
    let request: CreateStatusPageWebhookRequest =
        serde_json::from_str(r#"{"url":"https://hooks.example.test/status","enabled":false}"#)
            .expect("json");
    assert!(!request.enabled);
}

/// A webhook registered switched off is stored off, reads back as listed,
/// and is sent nothing; reading one needs `status_page:read` and a webhook
/// of another page is a 404 naming it.
#[tokio::test]
async fn a_webhook_created_switched_off_reads_back_and_receives_nothing() {
    let Some(fx) = fixture().await else {
        return;
    };
    let response = create_webhook(
        auth(Role::Admin),
        State(fx.state.clone()),
        metadata(),
        Path(fx.page_id),
        Json(CreateStatusPageWebhookRequest {
            url: URL.to_string(),
            enabled: false,
        }),
    )
    .await
    .map_err(|p| format!("{:?}", p.body))
    .expect("admin registers")
    .into_response();
    assert_eq!(response.status(), StatusCode::CREATED);
    let body = axum::body::to_bytes(response.into_body(), 1 << 20)
        .await
        .expect("body");
    let created: super::StatusPageWebhookCreatedResponse =
        serde_json::from_slice(&body).expect("json");
    assert!(!created.webhook.enabled);
    let entries = fx.audit.entries.lock().expect("audit").clone();
    assert!(
        entries[0].1.contains("\"enabled\":false"),
        "{}",
        entries[0].1
    );

    let Json(read) = get_webhook(
        auth(Role::Reader),
        State(fx.state.clone()),
        Path((fx.page_id, created.webhook.id)),
    )
    .await
    .map_err(|p| format!("{:?}", p.body))
    .expect("a reader reads one webhook");
    assert_eq!(read.id, created.webhook.id);
    assert!(!read.enabled);
    assert!(read.has_secret);
    assert!(!serde_json::to_string(&read)
        .expect("json")
        .contains(&created.secret));

    let missing = created.webhook.id + 1000;
    let not_found = get_webhook(
        auth(Role::Admin),
        State(fx.state.clone()),
        Path((fx.page_id, missing)),
    )
    .await
    .err()
    .expect("missing webhook");
    assert_eq!(not_found.status_code, StatusCode::NOT_FOUND);
    assert!(format!("{:?}", not_found.body).contains(&missing.to_string()));

    let RequireAuth(session) = auth(Role::Admin);
    let user = session.user.clone().expect("session user");
    let key_without_read = RequireAuth(AuthContext::new_api_key(
        user,
        None,
        Some(vec![temps_authz::Permission::ProjectsRead]),
        "ci".to_string(),
        7,
    ));
    let forbidden = get_webhook(
        key_without_read,
        State(fx.state.clone()),
        Path((fx.page_id, created.webhook.id)),
    )
    .await
    .err()
    .expect("a key without status_page:read");
    assert_eq!(forbidden.status_code, StatusCode::FORBIDDEN);

    schedule(&fx, "Database upgrade").await;
    fx.webhooks
        .dispatch_tick(&mut WebhookDispatcherState::default(), Utc::now())
        .await
        .expect("tick");
    assert!(
        fx.transport.sent().is_empty(),
        "a disabled webhook gets nothing"
    );
}
