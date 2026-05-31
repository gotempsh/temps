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
use temps_otel::OtelAppState;

/// Known API key for testing.
const TEST_API_KEY: &str = "tk_test_e2e_integration_key_12345";

/// Create a test AuthContext that satisfies RequireAuth + permission_guard!(auth, OtelRead).
/// Uses Role::Admin which has all permissions including OtelRead.
fn create_test_auth_context(user: &temps_entities::users::Model) -> temps_auth::AuthContext {
    temps_auth::AuthContext::new_session(user.clone(), temps_auth::Role::Admin)
}

/// Set up the full E2E test environment:
/// - TimescaleDB with all migrations
/// - A test user, project, and API key in the database
/// - An axum Router wired with the real OtelService + TimescaleDB storage
/// - Auth middleware that injects AuthContext for query endpoints
///
/// Returns None if Docker is unavailable.
async fn setup_e2e() -> Option<(
    temps_database::test_utils::TestDatabase,
    axum::Router,
    i32, // project_id
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
    let otel_service = Arc::new(OtelService::new(storage, auth_service, rate_limiter));
    let app_state = OtelAppState {
        otel_service,
        metrics_store: None,
        metrics_write_tx: None,
    };

    // Create auth middleware that injects AuthContext into request extensions.
    // Query handlers use RequireAuth which reads AuthContext from extensions.
    // Ingest handlers use their own API key auth (not RequireAuth), so this doesn't affect them.
    let auth_context = create_test_auth_context(&user);
    let auth_middleware = middleware::from_fn(
        move |mut req: axum::extract::Request, next: middleware::Next| {
            let auth_ctx = auth_context.clone();
            async move {
                req.extensions_mut().insert(auth_ctx);
                next.run(req).await
            }
        },
    );

    let router = configure_routes()
        .layer(auth_middleware)
        .with_state(app_state);

    Some((test_db, router, project_id))
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
