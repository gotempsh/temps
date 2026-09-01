// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! End-to-end HTTP integration tests.
//!
//! These tests simulate the full user flow:
//!   1. An OTel SDK/collector sends protobuf-encoded data via HTTP
//!   2. The handler authenticates via API key, decodes, and stores
//!   3. The monitoring UI queries back via HTTP and gets results
//!
//! Uses a real TimescaleDB via Docker. Skips gracefully when Docker is unavailable.

use std::sync::Arc;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use axum::middleware;
use http_body_util::BodyExt;
use prost::Message;
use sea_orm::{ActiveModelTrait, ActiveValue::Set};
use sha2::{Digest, Sha256};
use std::time::Duration;
use tower::ServiceExt;

use temps_otel::handlers::configure_routes;
use temps_otel::ingest::auth::OtelAuthService;
use temps_otel::ingest::rate_limit::RateLimiter;
use temps_otel::services::OtelService;
use temps_otel::storage::timescaledb::TimescaleDbStorage;
use temps_otel::storage::OtelStorage;
use temps_otel::OtelAppState;

/// No-op audit logger so the handler app state can be built without a real
/// audit service (dashboard write endpoints audit-log best-effort).
struct NoOpAuditLogger;

#[async_trait::async_trait]
impl temps_core::AuditLogger for NoOpAuditLogger {
    async fn create_audit_log(
        &self,
        _operation: &dyn temps_core::AuditOperation,
    ) -> anyhow::Result<()> {
        Ok(())
    }
}

/// No-op notification + job-queue stubs so a `MetricAlertEvaluator` (and its two
/// `AlarmService` instances) can be constructed for `OtelAppState`. The evaluator
/// loop is never spawned here and the query endpoints only snapshot its empty
/// in-memory firing map, so these are never actually invoked.
struct NoOpNotificationService;

#[async_trait::async_trait]
impl temps_core::notifications::NotificationService for NoOpNotificationService {
    async fn send_email(
        &self,
        _message: temps_core::notifications::EmailMessage,
    ) -> Result<(), temps_core::notifications::NotificationError> {
        Ok(())
    }
    async fn send_notification(
        &self,
        _notification: temps_core::notifications::NotificationData,
    ) -> Result<(), temps_core::notifications::NotificationError> {
        Ok(())
    }
    async fn is_configured(&self) -> Result<bool, temps_core::notifications::NotificationError> {
        Ok(false)
    }
}

struct NoOpJobQueue;

#[async_trait::async_trait]
impl temps_core::JobQueue for NoOpJobQueue {
    async fn send(&self, _job: temps_core::jobs::Job) -> Result<(), temps_core::jobs::QueueError> {
        Ok(())
    }
    fn subscribe(&self) -> Box<dyn temps_core::jobs::JobReceiver> {
        Box::new(NoOpJobReceiver)
    }
}

struct NoOpJobReceiver;

#[async_trait::async_trait]
impl temps_core::jobs::JobReceiver for NoOpJobReceiver {
    async fn recv(&mut self) -> Result<temps_core::jobs::Job, temps_core::jobs::QueueError> {
        Err(temps_core::jobs::QueueError::InvalidData(
            "no-op receiver".to_string(),
        ))
    }
}

/// Known API key for testing.
const TEST_API_KEY: &str = "tk_test_e2e_integration_key_12345";

/// Set up the full E2E test environment:
/// - TimescaleDB with all migrations
/// - A test user, project, and API key in the database
/// - An axum Router wired with the real OtelService + TimescaleDB storage
/// - Auth middleware that injects an `AuthContext` for query endpoints
///
/// Returns None if Docker is unavailable.
async fn setup_e2e() -> Option<(
    temps_database::test_utils::TestDatabase,
    axum::Router,
    i32, // project_id
)> {
    setup_e2e_as(Some(temps_auth::Role::Admin)).await
}

/// [`setup_e2e`] with control over the caller's identity, for testing the
/// authorization edges of the query endpoints:
///
/// * `Some(Role::Admin)` — the default; has `OtelRead`.
/// * `Some(Role::MetricsIngest)` — authenticated but holds an **empty**
///   permission set, so `permission_guard!(auth, OtelRead)` rejects it. This
///   is what distinguishes a 403 from a 401.
/// * `None` — no `AuthContext` is injected at all, so `RequireAuth` rejects
///   the request before any handler body runs.
///
/// Always builds the app with `metrics_store: None` — see
/// [`setup_e2e_with_metrics_store`] for the variant that wires a real one in.
async fn setup_e2e_as(
    role: Option<temps_auth::Role>,
) -> Option<(
    temps_database::test_utils::TestDatabase,
    axum::Router,
    i32, // project_id
)> {
    setup_e2e_as_full(role, false)
        .await
        .map(|(db, router, project_id, _store)| (db, router, project_id))
}

/// [`setup_e2e`] with a real [`temps_metrics::MetricsStore`] wired into
/// `OtelAppState::metrics_store`, for testing `GET /otel/pipeline-history`'s
/// 200 success path. `setup_e2e`/`setup_e2e_as` always build with
/// `metrics_store: None`, which makes that path structurally unreachable —
/// the handler returns 503 before it ever queries or serializes a series.
///
/// Returns the store handle alongside the router so a test can write points
/// directly, standing in for one tick of the real background pipeline-stats
/// sampler (`plugin.rs`'s 60-second `tokio::spawn` loop, which this manually
/// wired test harness — unlike the full plugin registration path — never
/// starts).
async fn setup_e2e_with_metrics_store() -> Option<(
    temps_database::test_utils::TestDatabase,
    axum::Router,
    i32, // project_id
    std::sync::Arc<dyn temps_metrics::MetricsStore>,
)> {
    let (db, router, project_id, store) =
        setup_e2e_as_full(Some(temps_auth::Role::Admin), true).await?;
    Some((
        db,
        router,
        project_id,
        store.expect("metrics store was requested"),
    ))
}

/// Shared implementation behind [`setup_e2e_as`] and
/// [`setup_e2e_with_metrics_store`]. `with_metrics_store` controls whether
/// `OtelAppState::metrics_store` is `Some(..)` (backed by a real
/// `TimescaleMetricsStore` on the same test database) or `None`.
async fn setup_e2e_as_full(
    role: Option<temps_auth::Role>,
    with_metrics_store: bool,
) -> Option<(
    temps_database::test_utils::TestDatabase,
    axum::Router,
    i32, // project_id
    Option<std::sync::Arc<dyn temps_metrics::MetricsStore>>,
)> {
    let test_db = match temps_database::test_utils::TestDatabase::with_migrations().await {
        Ok(db) => db,
        Err(e) => {
            println!(
                "Docker/TestDatabase not available, skipping E2E test: {}",
                e
            );
            return None;
        }
    };

    let db = test_db.db.clone();

    // Insert a test user (use ActiveModel::insert to trigger before_save for created_at/updated_at)
    let user = temps_entities::users::ActiveModel {
        name: Set("E2E Test User".into()),
        email: Set("e2e@test.local".into()),
        password_hash: Set(Some("not_real".into())),
        email_verified: Set(true),
        mfa_enabled: Set(false),
        ..Default::default()
    };
    let user = user
        .insert(db.as_ref())
        .await
        .expect("Failed to insert test user");
    let user_id = user.id;

    // Insert a test project (use ActiveModel::insert to trigger before_save for created_at/updated_at)
    let project = temps_entities::projects::ActiveModel {
        name: Set("E2E Test Project".into()),
        repo_name: Set("test-repo".into()),
        repo_owner: Set("test-org".into()),
        directory: Set("/".into()),
        main_branch: Set("main".into()),
        preset: Set(temps_entities::preset::Preset::Dockerfile),
        slug: Set("e2e-test-project".into()),
        is_deleted: Set(false),
        is_public_repo: Set(false),
        attack_mode: Set(false),
        error_source_context_enabled: Set(false),
        error_source_root: Set(None),
        enable_preview_environments: Set(false),
        ..Default::default()
    };
    let project = project
        .insert(db.as_ref())
        .await
        .expect("Failed to insert test project");
    let project_id = project.id;

    // Insert an API key with known hash (use ActiveModel::insert to trigger before_save)
    let mut hasher = Sha256::new();
    hasher.update(TEST_API_KEY.as_bytes());
    let key_hash = hex::encode(hasher.finalize());

    let api_key = temps_entities::api_keys::ActiveModel {
        name: Set("E2E Test Key".into()),
        key_hash: Set(key_hash),
        key_prefix: Set(TEST_API_KEY[..8].into()),
        user_id: Set(user_id),
        role_type: Set("admin".into()),
        is_active: Set(true),
        ..Default::default()
    };
    let _api_key = api_key
        .insert(db.as_ref())
        .await
        .expect("Failed to insert test API key");

    // Build the service stack
    let storage = Arc::new(TimescaleDbStorage::new(db.clone(), None));
    let auth_service = Arc::new(OtelAuthService::new(db.clone()));
    let rate_limiter = Arc::new(RateLimiter::new(10000, Duration::from_secs(60)));
    let otel_service = Arc::new(OtelService::new(
        storage,
        auth_service,
        rate_limiter,
        temps_otel::services::otel_service::DEFAULT_MAX_CONCURRENT_INGEST_REQUESTS,
    ));
    let dashboard_service = Arc::new(temps_otel::services::MetricDashboardService::new(
        db.clone(),
    ));
    let metric_alert_service = Arc::new(temps_otel::services::MetricAlertService::new(db.clone()));
    let cross_project_service = Arc::new(temps_otel::services::CrossProjectTraceService::new(
        db.clone(),
        Arc::new(TimescaleDbStorage::new(db.clone(), None)),
    ));
    // Build the shared evaluator so OtelAppState is complete (ADR-026 Phase 3).
    // Both AlarmService instances wrap the same no-op notify/queue stubs.
    let notify: Arc<dyn temps_core::notifications::NotificationService> =
        Arc::new(NoOpNotificationService);
    let queue: Arc<dyn temps_core::JobQueue> = Arc::new(NoOpJobQueue);
    let alarm_service = Arc::new(temps_monitoring::AlarmService::new(
        db.clone(),
        notify.clone(),
        queue.clone(),
    ));
    let alarm_service_dynamic = Arc::new(temps_monitoring::AlarmService::new(
        db.clone(),
        notify,
        queue,
    ));
    let metric_alert_evaluator = Arc::new(temps_otel::services::MetricAlertEvaluator::new(
        metric_alert_service.clone(),
        otel_service.clone(),
        alarm_service,
        alarm_service_dynamic,
        db.clone(),
        None,
    ));
    let facet_cache: temps_otel::services::FacetCache = Arc::new(arc_swap::ArcSwap::from_pointee(
        std::collections::HashMap::new(),
    ));
    let facet_service = Arc::new(temps_otel::services::FacetService::new(
        db.clone(),
        None,
        facet_cache,
    ));
    // Build a MetricsStore pointing at the same TimescaleDB connection only
    // when requested — mirrors how `plugin.rs` builds `TimescaleMetricsStore`
    // for the real app, but this harness never spawns the background
    // pipeline-stats sampler, so tests that need history data write points
    // directly via the returned handle.
    let metrics_store: Option<Arc<dyn temps_metrics::MetricsStore>> = if with_metrics_store {
        Some(Arc::new(temps_metrics::TimescaleMetricsStore::new(
            db.clone(),
        )))
    } else {
        None
    };

    let app_state = OtelAppState {
        otel_service,
        metrics_store: metrics_store.clone(),
        metrics_write_tx: None,
        dashboard_service,
        metric_alert_service,
        metric_alert_evaluator,
        audit_service: Arc::new(NoOpAuditLogger),
        cross_project_service,
        trace_hint_tx: None,
        otel_relay_tx: None,
        project_access_checker: None,
        facet_service,
    };

    // Create auth middleware that injects AuthContext into request extensions.
    // Query handlers use RequireAuth which reads AuthContext from extensions.
    // Ingest handlers use their own API key auth (not RequireAuth), so this doesn't affect them.
    //
    // `role: None` injects nothing, which is how the unauthenticated case is
    // reproduced — RequireAuth then finds no AuthContext and returns 401.
    let auth_context = role.map(|r| temps_auth::AuthContext::new_session(user.clone(), r));
    let auth_middleware = middleware::from_fn(
        move |mut req: axum::extract::Request, next: middleware::Next| {
            let auth_ctx = auth_context.clone();
            async move {
                if let Some(ctx) = auth_ctx {
                    req.extensions_mut().insert(ctx);
                }
                next.run(req).await
            }
        },
    );

    let router = configure_routes()
        .layer(auth_middleware)
        .with_state(app_state);

    Some((test_db, router, project_id, metrics_store))
}

/// Helper: build a protobuf ExportTraceServiceRequest with a trace tree.
fn build_trace_request(
    trace_id: &[u8; 16],
    service_name: &str,
) -> temps_otel::proto::collector::trace::v1::ExportTraceServiceRequest {
    let root_id: [u8; 8] = [0x01, 0x02, 0x03, 0x04, 0x05, 0x06, 0x07, 0x08];
    let child_id: [u8; 8] = [0x11, 0x12, 0x13, 0x14, 0x15, 0x16, 0x17, 0x18];
    let base = 1_700_000_000_000_000_000_u64;

    temps_otel::proto::collector::trace::v1::ExportTraceServiceRequest {
        resource_spans: vec![temps_otel::proto::trace::v1::ResourceSpans {
            resource: Some(temps_otel::proto::resource::v1::Resource {
                attributes: vec![temps_otel::proto::common::v1::KeyValue {
                    key: "service.name".into(),
                    value: Some(temps_otel::proto::common::v1::AnyValue {
                        value: Some(
                            temps_otel::proto::common::v1::any_value::Value::StringValue(
                                service_name.into(),
                            ),
                        ),
                    }),
                }],
                dropped_attributes_count: 0,
            }),
            scope_spans: vec![temps_otel::proto::trace::v1::ScopeSpans {
                scope: None,
                spans: vec![
                    temps_otel::proto::trace::v1::Span {
                        trace_id: trace_id.to_vec(),
                        span_id: root_id.to_vec(),
                        parent_span_id: vec![],
                        name: "GET /api/users".into(),
                        kind: 2, // SERVER
                        start_time_unix_nano: base,
                        end_time_unix_nano: base + 100_000_000,
                        status: Some(temps_otel::proto::trace::v1::Status {
                            code: 1, // OK
                            message: String::new(),
                        }),
                        attributes: vec![],
                        dropped_attributes_count: 0,
                        events: vec![],
                        dropped_events_count: 0,
                        links: vec![],
                        dropped_links_count: 0,
                        trace_state: String::new(),
                        flags: 0,
                    },
                    temps_otel::proto::trace::v1::Span {
                        trace_id: trace_id.to_vec(),
                        span_id: child_id.to_vec(),
                        parent_span_id: root_id.to_vec(),
                        name: "SELECT * FROM users".into(),
                        kind: 3, // CLIENT
                        start_time_unix_nano: base + 5_000_000,
                        end_time_unix_nano: base + 25_000_000,
                        status: Some(temps_otel::proto::trace::v1::Status {
                            code: 1,
                            message: String::new(),
                        }),
                        attributes: vec![],
                        dropped_attributes_count: 0,
                        events: vec![],
                        dropped_events_count: 0,
                        links: vec![],
                        dropped_links_count: 0,
                        trace_state: String::new(),
                        flags: 0,
                    },
                ],
                schema_url: String::new(),
            }],
            schema_url: String::new(),
        }],
    }
}

/// Helper: build a protobuf ExportMetricsServiceRequest.
fn build_metrics_request(
    service_name: &str,
) -> temps_otel::proto::collector::metrics::v1::ExportMetricsServiceRequest {
    temps_otel::proto::collector::metrics::v1::ExportMetricsServiceRequest {
        resource_metrics: vec![temps_otel::proto::metrics::v1::ResourceMetrics {
            resource: Some(temps_otel::proto::resource::v1::Resource {
                attributes: vec![temps_otel::proto::common::v1::KeyValue {
                    key: "service.name".into(),
                    value: Some(temps_otel::proto::common::v1::AnyValue {
                        value: Some(
                            temps_otel::proto::common::v1::any_value::Value::StringValue(
                                service_name.into(),
                            ),
                        ),
                    }),
                }],
                dropped_attributes_count: 0,
            }),
            scope_metrics: vec![temps_otel::proto::metrics::v1::ScopeMetrics {
                scope: None,
                metrics: vec![temps_otel::proto::metrics::v1::Metric {
                    name: "http.request.duration".into(),
                    description: "Request duration".into(),
                    unit: "ms".into(),
                    data: Some(temps_otel::proto::metrics::v1::metric::Data::Gauge(
                        temps_otel::proto::metrics::v1::Gauge {
                            data_points: vec![temps_otel::proto::metrics::v1::NumberDataPoint {
                                time_unix_nano: 1_700_000_000_000_000_000,
                                value: Some(
                                    temps_otel::proto::metrics::v1::number_data_point::Value::AsDouble(42.5),
                                ),
                                attributes: vec![],
                                ..Default::default()
                            }],
                        },
                    )),
                }],
                schema_url: String::new(),
            }],
            schema_url: String::new(),
        }],
    }
}

/// Helper: build a protobuf ExportLogsServiceRequest.
fn build_logs_request(
    service_name: &str,
    body: &str,
    severity: i32,
) -> temps_otel::proto::collector::logs::v1::ExportLogsServiceRequest {
    temps_otel::proto::collector::logs::v1::ExportLogsServiceRequest {
        resource_logs: vec![temps_otel::proto::logs::v1::ResourceLogs {
            resource: Some(temps_otel::proto::resource::v1::Resource {
                attributes: vec![temps_otel::proto::common::v1::KeyValue {
                    key: "service.name".into(),
                    value: Some(temps_otel::proto::common::v1::AnyValue {
                        value: Some(
                            temps_otel::proto::common::v1::any_value::Value::StringValue(
                                service_name.into(),
                            ),
                        ),
                    }),
                }],
                dropped_attributes_count: 0,
            }),
            scope_logs: vec![temps_otel::proto::logs::v1::ScopeLogs {
                scope: None,
                log_records: vec![temps_otel::proto::logs::v1::LogRecord {
                    time_unix_nano: 1_700_000_000_000_000_000,
                    observed_time_unix_nano: 1_700_000_000_000_000_000,
                    severity_number: severity,
                    severity_text: String::new(),
                    body: Some(temps_otel::proto::common::v1::AnyValue {
                        value: Some(
                            temps_otel::proto::common::v1::any_value::Value::StringValue(
                                body.into(),
                            ),
                        ),
                    }),
                    attributes: vec![],
                    dropped_attributes_count: 0,
                    flags: 0,
                    trace_id: vec![],
                    span_id: vec![],
                }],
                schema_url: String::new(),
            }],
            schema_url: String::new(),
        }],
    }
}

// ── Trace E2E tests ─────────────────────────────────────────────────

#[tokio::test]
async fn test_e2e_ingest_traces_and_query_back() {
    let Some((_db, router, project_id)) = setup_e2e().await else {
        return;
    };

    let trace_id: [u8; 16] = [
        0xAA, 0xBB, 0xCC, 0xDD, 0xEE, 0xFF, 0x00, 0x11, 0x22, 0x33, 0x44, 0x55, 0x66, 0x77, 0x88,
        0x99,
    ];
    let trace_id_hex = hex::encode(trace_id);

    // Step 1: POST protobuf traces (like an OTel SDK would)
    let request = build_trace_request(&trace_id, "my-web-app");
    let body = request.encode_to_vec();

    let response = router
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/otel/v1/traces")
                .header("content-type", "application/x-protobuf")
                .header("authorization", format!("Bearer {TEST_API_KEY}"))
                .header("x-temps-project-id", project_id.to_string())
                .body(Body::from(body))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(
        response.status(),
        StatusCode::OK,
        "Trace ingest should return 200"
    );

    // Verify response is valid OTLP protobuf
    let resp_body = response.into_body().collect().await.unwrap().to_bytes();
    let otlp_resp =
        temps_otel::proto::collector::trace::v1::ExportTraceServiceResponse::decode(&resp_body[..])
            .expect("Response should be valid OTLP protobuf");
    // partial_success should be None (all succeeded)
    assert!(otlp_resp.partial_success.is_none());

    // Step 2: GET the trace back (like the monitoring UI would)
    let response = router
        .clone()
        .oneshot(
            Request::builder()
                .method("GET")
                .uri(format!("/otel/traces/{project_id}/{trace_id_hex}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(
        response.status(),
        StatusCode::OK,
        "Trace query should return 200"
    );

    let resp_body = response.into_body().collect().await.unwrap().to_bytes();
    let trace_resp: serde_json::Value =
        serde_json::from_slice(&resp_body).expect("Response should be valid JSON");

    // The trace should have spans (sampler may keep 0-2 depending on sampling)
    // Error spans are always kept; these are OK spans so they go through probabilistic sampling.
    // With default 1% sampling, they likely get sampled out.
    // But the response structure should be correct regardless.
    assert!(trace_resp["data"].is_array(), "data should be an array");
    let count = trace_resp["count"].as_u64().unwrap_or(0);
    // count should match data array length
    assert_eq!(
        count,
        trace_resp["data"].as_array().unwrap().len() as u64,
        "count should match data array length"
    );
}

#[tokio::test]
async fn test_e2e_ingest_error_trace_always_stored() {
    let Some((_db, router, project_id)) = setup_e2e().await else {
        return;
    };

    let trace_id: [u8; 16] = [0xDD; 16];
    let trace_id_hex = hex::encode(trace_id);

    // Build a trace with an ERROR span (always kept by sampler)
    let mut request = build_trace_request(&trace_id, "error-app");
    // Set the root span to ERROR status
    request.resource_spans[0].scope_spans[0].spans[0]
        .status
        .as_mut()
        .unwrap()
        .code = 2; // ERROR

    let body = request.encode_to_vec();

    let response = router
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/otel/v1/traces")
                .header("content-type", "application/x-protobuf")
                .header("authorization", format!("Bearer {TEST_API_KEY}"))
                .header("x-temps-project-id", project_id.to_string())
                .body(Body::from(body))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::OK);

    // Query back — error spans are always kept
    let response = router
        .clone()
        .oneshot(
            Request::builder()
                .method("GET")
                .uri(format!("/otel/traces/{project_id}/{trace_id_hex}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::OK);
    let resp_body = response.into_body().collect().await.unwrap().to_bytes();
    let trace_resp: serde_json::Value = serde_json::from_slice(&resp_body).unwrap();

    let spans = trace_resp["data"].as_array().unwrap();
    // At least the error span should be stored (sampler keeps all error traces)
    assert!(
        !spans.is_empty(),
        "Error traces should always be stored, got 0 spans"
    );

    // Verify the root span has ERROR status
    let root = spans
        .iter()
        .find(|s| s["parent_span_id"].is_null())
        .expect("Should have a root span");
    assert_eq!(root["status_code"], "ERROR");
    assert_eq!(root["name"], "GET /api/users");
    assert_eq!(root["kind"], "SERVER");
}

// ── Metrics E2E test ────────────────────────────────────────────────

#[tokio::test]
async fn test_e2e_ingest_metrics_and_query_back() {
    let Some((_db, router, project_id)) = setup_e2e().await else {
        return;
    };

    // POST metrics
    let request = build_metrics_request("metrics-app");
    let body = request.encode_to_vec();

    let response = router
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/otel/v1/metrics")
                .header("content-type", "application/x-protobuf")
                .header("authorization", format!("Bearer {TEST_API_KEY}"))
                .header("x-temps-project-id", project_id.to_string())
                .body(Body::from(body))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::OK);

    // Verify OTLP response
    let resp_body = response.into_body().collect().await.unwrap().to_bytes();
    temps_otel::proto::collector::metrics::v1::ExportMetricsServiceResponse::decode(&resp_body[..])
        .expect("Should be valid OTLP metrics response");

    // Query metric names
    let response = router
        .clone()
        .oneshot(
            Request::builder()
                .method("GET")
                .uri(format!("/otel/metric-names/{project_id}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::OK);
    let resp_body = response.into_body().collect().await.unwrap().to_bytes();
    let names_resp: serde_json::Value = serde_json::from_slice(&resp_body).unwrap();

    let names = names_resp["names"].as_array().unwrap();
    assert!(
        names.iter().any(|n| n == "http.request.duration"),
        "Should find the ingested metric name, got: {:?}",
        names
    );
}

// ── Logs E2E test ───────────────────────────────────────────────────

#[tokio::test]
async fn test_e2e_ingest_logs_and_query_back() {
    let Some((_db, router, project_id)) = setup_e2e().await else {
        return;
    };

    // POST an ERROR log (severity 17 = ERROR)
    let request = build_logs_request("logging-app", "Database connection timeout", 17);
    let body = request.encode_to_vec();

    let response = router
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/otel/v1/logs")
                .header("content-type", "application/x-protobuf")
                .header("authorization", format!("Bearer {TEST_API_KEY}"))
                .header("x-temps-project-id", project_id.to_string())
                .body(Body::from(body))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::OK);

    // Query logs (ERROR severity goes to DB)
    let response = router
        .clone()
        .oneshot(
            Request::builder()
                .method("GET")
                .uri(format!("/otel/logs?project_id={project_id}&severity=ERROR"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::OK);
    let resp_body = response.into_body().collect().await.unwrap().to_bytes();
    let logs_resp: serde_json::Value = serde_json::from_slice(&resp_body).unwrap();

    let logs = logs_resp["data"].as_array().unwrap();
    assert!(
        !logs.is_empty(),
        "ERROR logs should be stored in DB and queryable"
    );
    assert_eq!(logs[0]["body"], "Database connection timeout");
    assert_eq!(logs[0]["severity"], "ERROR");
}

// ── Auth failure E2E tests ──────────────────────────────────────────

#[tokio::test]
async fn test_e2e_missing_api_key_returns_401() {
    let Some((_db, router, _project_id)) = setup_e2e().await else {
        return;
    };

    let request = build_trace_request(&[0xAA; 16], "no-auth-app");
    let body = request.encode_to_vec();

    // No Authorization header
    let response = router
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/otel/v1/traces")
                .header("content-type", "application/x-protobuf")
                .body(Body::from(body))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(
        response.status(),
        StatusCode::UNAUTHORIZED,
        "Missing API key should return 401"
    );
}

#[tokio::test]
async fn test_e2e_ingest_body_over_limit_returns_413() {
    let Some((_db, router, _project_id)) = setup_e2e().await else {
        return;
    };

    // One byte over the router's `DefaultBodyLimit` (see `handlers::INGEST_BODY_LIMIT`).
    // Axum rejects a body whose declared `Content-Length` exceeds the limit
    // before dispatching to any handler/extractor, so this doesn't need a
    // valid API key — the limit must fire ahead of auth.
    let oversized_body = vec![0_u8; temps_otel::handlers::INGEST_BODY_LIMIT + 1];

    let response = router
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/otel/v1/traces")
                .header("content-type", "application/x-protobuf")
                .body(Body::from(oversized_body))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(
        response.status(),
        StatusCode::PAYLOAD_TOO_LARGE,
        "Body over the ingest route's DefaultBodyLimit should be rejected with 413, \
         not reach the handler/auth layer"
    );
}

#[tokio::test]
async fn test_e2e_invalid_api_key_returns_401() {
    let Some((_db, router, project_id)) = setup_e2e().await else {
        return;
    };

    let request = build_trace_request(&[0xBB; 16], "bad-key-app");
    let body = request.encode_to_vec();

    let response = router
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/otel/v1/traces")
                .header("content-type", "application/x-protobuf")
                .header("authorization", "Bearer tk_this_key_does_not_exist_in_db")
                .header("x-temps-project-id", project_id.to_string())
                .body(Body::from(body))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(
        response.status(),
        StatusCode::UNAUTHORIZED,
        "Invalid API key should return 401"
    );
}

// ── Route tests for the ingest-errors + pipeline-history endpoints ──
//
// Both are read-only, system-scoped (no project parameter) and guarded by
// `RequireAuth` + `permission_guard!(auth, OtelRead)`, so they share one set
// of authorization cases. `setup_e2e` builds the app with
// `metrics_store: None`, which also makes it the natural place to pin
// pipeline-history's "metrics collection not enabled" degradation.

/// Both endpoints must reject an unauthenticated caller before running any
/// handler logic.
#[tokio::test]
async fn test_e2e_otel_reports_require_authentication() {
    let Some((_db, router, _project_id)) = setup_e2e_as(None).await else {
        return;
    };

    for uri in ["/otel/ingest-errors", "/otel/pipeline-history"] {
        let response = router
            .clone()
            .oneshot(
                Request::builder()
                    .method("GET")
                    .uri(uri)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(
            response.status(),
            StatusCode::UNAUTHORIZED,
            "{uri} must return 401 without an AuthContext"
        );
    }
}

/// Authenticated but without `OtelRead` must be 403, not 401 — the two mean
/// different things to a caller ("log in" vs "ask for access").
#[tokio::test]
async fn test_e2e_otel_reports_require_otel_read_permission() {
    let Some((_db, router, _project_id)) =
        setup_e2e_as(Some(temps_auth::Role::MetricsIngest)).await
    else {
        return;
    };

    for uri in ["/otel/ingest-errors", "/otel/pipeline-history"] {
        let response = router
            .clone()
            .oneshot(
                Request::builder()
                    .method("GET")
                    .uri(uri)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(
            response.status(),
            StatusCode::FORBIDDEN,
            "{uri} must return 403 for an authenticated caller lacking OtelRead"
        );
    }
}

/// A healthy pipeline returns 200 with an empty *array*, never `null` and
/// never a 404 — a client mapping over `errors` must not have to null-guard.
#[tokio::test]
async fn test_e2e_ingest_errors_returns_empty_array_when_healthy() {
    let Some((_db, router, _project_id)) = setup_e2e().await else {
        return;
    };

    let response = router
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/otel/ingest-errors")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::OK);
    let body = response.into_body().collect().await.unwrap().to_bytes();
    let json: serde_json::Value = serde_json::from_slice(&body).unwrap();

    assert!(
        json["errors"].is_array(),
        "errors must be an array, got {}",
        json["errors"]
    );
    assert_eq!(json["errors"].as_array().map(Vec::len), Some(0));
}

/// The recorded failure reason must survive a full round trip through the
/// real Postgres table and come back grouped, with the count aggregated.
#[tokio::test]
async fn test_e2e_ingest_errors_returns_recorded_failures() {
    let Some((test_db, router, _project_id)) = setup_e2e().await else {
        return;
    };

    // Record the same (signal, class) twice plus one distinct group, going
    // through the storage layer rather than raw SQL so the upsert under test
    // is the one that actually runs in production.
    let storage = TimescaleDbStorage::new(test_db.db.clone(), None);
    for _ in 0..2 {
        storage
            .record_ingest_error("spans", "clickhouse_network", "connection refused")
            .await
            .expect("recording an ingest error succeeds");
    }
    storage
        .record_ingest_error("logs", "postgres_conn", "connection reset")
        .await
        .expect("recording an ingest error succeeds");

    let response = router
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/otel/ingest-errors?limit=50")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::OK);
    let body = response.into_body().collect().await.unwrap().to_bytes();
    let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
    let errors = json["errors"].as_array().expect("errors array");

    assert_eq!(errors.len(), 2, "two distinct (signal, class) groups");

    let spans_group = errors
        .iter()
        .find(|e| e["signal_type"] == "spans")
        .expect("spans group present");
    assert_eq!(spans_group["error_class"], "clickhouse_network");
    assert_eq!(
        spans_group["count"], 2,
        "repeat failures aggregate into one group rather than appending rows"
    );
    assert!(
        spans_group["sample_message"]
            .as_str()
            .is_some_and(|m| m.contains("connection refused")),
        "the sample message must carry the backend detail"
    );
    // ISO 8601 with Z, per the workspace date convention.
    for field in ["first_seen", "last_seen"] {
        let ts = spans_group[field].as_str().unwrap_or_default();
        assert!(
            ts.ends_with('Z'),
            "{field} must be ISO-8601 UTC, got {ts:?}"
        );
    }
}

/// `metrics_store: None` is a real deployment state (metric collection
/// disabled), and it must produce an explicit, actionable 503 rather than an
/// empty chart that reads as "nothing was dropped".
#[tokio::test]
async fn test_e2e_pipeline_history_reports_metrics_store_unavailable() {
    let Some((_db, router, _project_id)) = setup_e2e().await else {
        return;
    };

    let response = router
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/otel/pipeline-history")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(
        response.status(),
        StatusCode::SERVICE_UNAVAILABLE,
        "history must not silently report zeros when nothing is recording it"
    );

    let body = response.into_body().collect().await.unwrap().to_bytes();
    let problem: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(problem["title"], "Metrics Unavailable");
    let detail = problem["detail"].as_str().unwrap_or_default();
    assert!(
        detail.contains("/otel/pipeline-stats"),
        "the 503 must point at the endpoint that still works, got {detail:?}"
    );
}

/// The window validation runs before the metrics-store lookup, so a bad range
/// is a 400 Problem Details even on a server with collection disabled.
#[tokio::test]
async fn test_e2e_pipeline_history_rejects_invalid_windows() {
    let Some((_db, router, _project_id)) = setup_e2e().await else {
        return;
    };

    // The "too-wide window" case is computed from `Utc::now()` rather than
    // hardcoded, so it keeps exceeding the 7-day cap (`MAX_WINDOW_DAYS`)
    // indefinitely instead of silently becoming a fixed historical range that
    // stops testing anything as time passes.
    //
    // Uses `to_rfc3339_opts(.., true)` (forces a `Z` suffix) rather than the
    // plain `to_rfc3339()`, which renders the UTC offset as `+00:00` — the
    // unescaped `+` is interpreted as a literal space once it lands in a URL
    // query string, corrupting the timestamp before it ever reaches the
    // handler's parser.
    let now = chrono::Utc::now();
    let too_wide_start =
        (now - chrono::Duration::days(80)).to_rfc3339_opts(chrono::SecondsFormat::Secs, true);
    let too_wide_end = now.to_rfc3339_opts(chrono::SecondsFormat::Secs, true);
    let too_wide_uri =
        format!("/otel/pipeline-history?start_time={too_wide_start}&end_time={too_wide_end}");

    let cases = [
        (
            "/otel/pipeline-history?start_time=2026-08-20T00:00:00Z&end_time=2026-08-19T00:00:00Z",
            "Invalid Time Range",
        ),
        (
            "/otel/pipeline-history?start_time=2026-08-20T00:00:00Z",
            "Invalid Time Range",
        ),
        (too_wide_uri.as_str(), "Time Range Too Wide"),
    ];

    for (uri, expected_title) in cases {
        let response = router
            .clone()
            .oneshot(
                Request::builder()
                    .method("GET")
                    .uri(uri)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(
            response.status(),
            StatusCode::BAD_REQUEST,
            "{uri} must be rejected"
        );
        let body = response.into_body().collect().await.unwrap().to_bytes();
        let problem: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(problem["title"], expected_title, "for {uri}");
        assert!(
            problem["detail"].as_str().is_some_and(|d| !d.is_empty()),
            "a Problem Details response must carry an actionable detail for {uri}"
        );
    }
}

/// The 200 success path, structurally unreachable via `setup_e2e` because it
/// hardcodes `metrics_store: None` (see `test_e2e_pipeline_history_reports_metrics_store_unavailable`
/// above) — the handler returns 503 before it ever reaches the
/// query/serialize logic. `setup_e2e_with_metrics_store` wires a real
/// `TimescaleMetricsStore` in instead.
///
/// Ingests one trace (so the pipeline has something to report) and then
/// writes one round of `otel.*` counter points directly to the metrics store
/// to stand in for a single tick of the real background pipeline-stats
/// sampler — `plugin.rs` spawns that sampler on a 60-second interval as part
/// of full plugin registration, which this manually-wired test harness does
/// not build, so a real 60s wait is neither necessary nor practical here.
#[tokio::test]
async fn test_e2e_pipeline_history_returns_series_when_metrics_store_available() {
    let Some((_db, router, project_id, metrics_store)) = setup_e2e_with_metrics_store().await
    else {
        return;
    };

    // 1. At least one ingest.
    let trace_id: [u8; 16] = [0x77; 16];
    let request = build_trace_request(&trace_id, "pipeline-history-e2e");
    let body = request.encode_to_vec();
    let ingest_response = router
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/otel/v1/traces")
                .header("content-type", "application/x-protobuf")
                .header("authorization", format!("Bearer {TEST_API_KEY}"))
                .header("x-temps-project-id", project_id.to_string())
                .body(Body::from(body))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(ingest_response.status(), StatusCode::OK);

    // 2. One simulated sampler tick — same shape the real sampler writes:
    //    one Counter point per name in `OTEL_PIPELINE_METRIC_NAMES`, against
    //    `SourceKind::Node` / `CONTROL_PLANE_NODE_ID`.
    let now = chrono::Utc::now();
    let points: Vec<temps_metrics::MetricPoint> = temps_otel::plugin::OTEL_PIPELINE_METRIC_NAMES
        .iter()
        .map(|name| temps_metrics::MetricPoint {
            time: now,
            source_kind: temps_metrics::SourceKind::Node,
            source_id: temps_otel::plugin::CONTROL_PLANE_NODE_ID,
            name: name.to_string(),
            value: 1.0,
            kind: temps_metrics::MetricKind::Counter,
            engine: Some("otel".to_string()),
            environment: None,
            node_id: Some(temps_otel::plugin::CONTROL_PLANE_NODE_ID),
            labels: std::collections::HashMap::new(),
        })
        .collect();
    metrics_store
        .write_batch(points)
        .await
        .expect("simulated sampler-tick write succeeds");

    // 3. Query the default window and assert the response carries real data.
    let response = router
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/otel/pipeline-history")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::OK);
    let body = response.into_body().collect().await.unwrap().to_bytes();
    let json: serde_json::Value = serde_json::from_slice(&body).unwrap();

    let series = json["series"].as_array().expect("series must be an array");
    assert!(
        !series.is_empty(),
        "expected non-empty series once ingest happened and a sampler tick was recorded, \
         got: {json}"
    );

    for entry in series {
        let name = entry["name"].as_str().unwrap_or_default();
        assert!(
            name.starts_with("otel."),
            "every series name must be an otel.* pipeline counter, got {name:?}"
        );
        assert!(
            entry["points"].is_array(),
            "every series must carry a non-null points array, got {entry}"
        );
    }

    // Proves the query round-trips real data rather than 13 always-present
    // but permanently-empty series shells.
    let has_data_point = series
        .iter()
        .any(|s| s["points"].as_array().is_some_and(|p| !p.is_empty()));
    assert!(
        has_data_point,
        "expected at least one series to contain the point written by the simulated \
         sampler tick, got: {json}"
    );
}

// ── Pipeline stats E2E test ─────────────────────────────────────────

#[tokio::test]
async fn test_e2e_pipeline_stats() {
    let Some((_db, router, project_id)) = setup_e2e().await else {
        return;
    };

    // Ingest some data first
    let request = build_metrics_request("stats-app");
    let body = request.encode_to_vec();

    router
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/otel/v1/metrics")
                .header("content-type", "application/x-protobuf")
                .header("authorization", format!("Bearer {TEST_API_KEY}"))
                .header("x-temps-project-id", project_id.to_string())
                .body(Body::from(body))
                .unwrap(),
        )
        .await
        .unwrap();

    // Check pipeline stats
    let response = router
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/otel/pipeline-stats")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::OK);
    let resp_body = response.into_body().collect().await.unwrap().to_bytes();
    let stats: serde_json::Value = serde_json::from_slice(&resp_body).unwrap();

    assert!(
        stats["stats"]["metrics_received"].as_u64().unwrap() > 0,
        "metrics_received should be > 0 after ingesting"
    );
}

/// `include_total` is the opt-out that lets a caller skip the trace-summaries
/// count query entirely. Its contract has two halves and both matter:
///
///   * default (absent) → `total` is computed and present
///   * `include_total=false` → `total` is **omitted**, not zeroed
///
/// The second half is the one worth a test. `total` is `Option<u64>` with
/// `skip_serializing_if`, and the whole point is that a client reading
/// `total ?? 0` must not silently render "0 traces" over a full page. Because
/// `None` is only ever produced by the branch that skips `count_traces`, an
/// omitted key is also proof that the second query was not issued.
#[tokio::test]
async fn test_e2e_trace_summaries_include_total_contract() {
    let Some((_test_db, router, project_id)) = setup_e2e().await else {
        return;
    };

    // Default: the count is computed, so `total` is present and numeric.
    let response = router
        .clone()
        .oneshot(
            Request::builder()
                .method("GET")
                .uri(format!("/otel/trace-summaries?project_id={project_id}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::OK);
    let resp_body = response.into_body().collect().await.unwrap().to_bytes();
    let body: serde_json::Value = serde_json::from_slice(&resp_body).unwrap();
    assert!(body["data"].is_array(), "data should be an array");
    assert!(
        body.get("total").and_then(|t| t.as_u64()).is_some(),
        "total must be present by default, got: {body}"
    );

    // Opted out: `total` must be ABSENT from the payload — not 0, not null.
    let response = router
        .oneshot(
            Request::builder()
                .method("GET")
                .uri(format!(
                    "/otel/trace-summaries?project_id={project_id}&include_total=false"
                ))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::OK);
    let resp_body = response.into_body().collect().await.unwrap().to_bytes();
    let body: serde_json::Value = serde_json::from_slice(&resp_body).unwrap();
    assert!(
        body["data"].is_array(),
        "data must still be returned when the count is skipped, got: {body}"
    );
    assert!(
        body.get("total").is_none(),
        "include_total=false must omit `total` entirely (a 0 would be read as \
         'no traces' by a client doing `total ?? 0`), got: {body}"
    );
}
