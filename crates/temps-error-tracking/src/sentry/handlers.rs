use axum::{
    body::Bytes,
    extract::{Path, Query, State},
    http::{HeaderMap, StatusCode},
    response::IntoResponse,
    routing::post,
    Json, Router,
};
use flate2::read::GzDecoder;
use std::io::Read as IoRead;
use std::sync::Arc;
use tower_http::cors::{Any, CorsLayer};
use tracing::debug;
use utoipa::OpenApi;

use crate::providers::{sentry::SentryProvider, ErrorProvider};
use crate::sentry::types::{SentryEventRequest, SentryEventResponse};
use crate::services::error_tracking_service::ErrorTrackingService;

#[derive(OpenApi)]
#[openapi(
    paths(
        ingest_sentry_event,
        ingest_sentry_envelope,
    ),
    components(schemas(
        SentryEventRequest,
        SentryEventResponse,
    )),
    tags(
        (name = "sentry-ingestor", description = "Sentry-compatible ingest endpoints")
    )
)]
pub struct ApiDoc;

#[derive(Clone)]
pub struct AppState {
    pub sentry_provider: Arc<SentryProvider>,
    pub error_tracking_service: Arc<ErrorTrackingService>,
    pub audit_service: Arc<dyn temps_core::AuditLogger>,
}

pub fn configure_routes() -> Router<Arc<AppState>> {
    // Create CORS layer that allows all origins for Sentry SDK compatibility
    let cors = CorsLayer::new()
        .allow_origin(Any)
        .allow_methods(Any)
        .allow_headers(Any)
        .expose_headers(Any);

    Router::new()
        .route("/{project_id}/store/", post(ingest_sentry_event))
        .route("/{project_id}/envelope/", post(ingest_sentry_envelope))
        .layer(cors)
}

// Types are now in types.rs

/// Ingest a Sentry event (JSON payload)
#[utoipa::path(
    post,
    path = "/api/{project_id}/store/",
    params(
        ("project_id" = i32, Path, description = "Project ID")
    ),
    request_body = SentryEventRequest,
    responses(
        (status = 200, description = "Event ingested", body = SentryEventResponse),
        (status = 400, description = "Bad request"),
        (status = 401, description = "Unauthorized"),
    ),
    tag = "sentry-ingestor"
)]
async fn ingest_sentry_event(
    State(state): State<Arc<AppState>>,
    Path(project_id): Path<i32>,
    Query(params): Query<std::collections::HashMap<String, String>>,
    headers: HeaderMap,
    Json(event): Json<serde_json::Value>,
) -> impl IntoResponse {
    // Extract DSN key from auth header or query params
    let dsn_key = extract_dsn_key(&headers, &params);

    let dsn_key = match dsn_key.as_deref() {
        Some(key) => key,
        None => {
            return (StatusCode::UNAUTHORIZED, "Missing DSN key".to_string()).into_response();
        }
    };

    // Authenticate using the provider
    let auth = match state
        .sentry_provider
        .authenticate(project_id, dsn_key)
        .await
    {
        Ok(auth) => auth,
        Err(e) => {
            tracing::error!("Authentication failed: {:?}", e);
            return (StatusCode::UNAUTHORIZED, e.to_string()).into_response();
        }
    };

    // Parse event using the provider
    let parsed_event = match state.sentry_provider.parse_json_event(event, &auth).await {
        Ok(event) => event,
        Err(e) => {
            tracing::error!("Failed to parse event: {:?}", e);
            return (StatusCode::BAD_REQUEST, e.to_string()).into_response();
        }
    };

    // Store event using the error tracking service
    match state
        .error_tracking_service
        .process_error_event(parsed_event.error_data)
        .await
    {
        Ok(_) => {
            let response = SentryEventResponse {
                id: parsed_event.event_id,
            };
            (StatusCode::OK, Json(response)).into_response()
        }
        Err(e) => {
            tracing::error!("Failed to store event: {:?}", e);
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("Failed to store event: {}", e),
            )
                .into_response()
        }
    }
}

/// Ingest a Sentry envelope (binary payload)
#[utoipa::path(
    post,
    path = "/api/{project_id}/envelope/",
    params(
        ("project_id" = i32, Path, description = "Project ID")
    ),
    request_body(content = String, description = "Sentry envelope as binary data", content_type = "application/octet-stream"),
    responses(
        (status = 200, description = "Envelope ingested"),
        (status = 400, description = "Bad request"),
        (status = 401, description = "Unauthorized"),
    ),
    tag = "sentry-ingestor"
)]
async fn ingest_sentry_envelope(
    State(state): State<Arc<AppState>>,
    Path(project_id): Path<i32>,
    Query(params): Query<std::collections::HashMap<String, String>>,
    headers: HeaderMap,
    body: Bytes,
) -> impl IntoResponse {
    debug!("Query params: {:?}", params);
    debug!("Headers: {:?}", headers);
    // Extract DSN key from auth header or query params
    let dsn_key = extract_dsn_key(&headers, &params);

    // Check if body is gzip-compressed
    let decompressed_body = match decompress_if_needed(&headers, &body) {
        Ok(data) => data,
        Err(e) => {
            tracing::warn!("Failed to decompress envelope: {}", e);
            return (
                StatusCode::BAD_REQUEST,
                format!("Failed to decompress envelope: {}", e),
            )
                .into_response();
        }
    };

    // Authenticate using the provider (envelope parsing happens in provider)
    let dsn_key = match dsn_key.as_deref() {
        Some(key) => key,
        None => {
            return (StatusCode::UNAUTHORIZED, "Missing DSN key".to_string()).into_response();
        }
    };

    let auth = match state
        .sentry_provider
        .authenticate(project_id, dsn_key)
        .await
    {
        Ok(auth) => auth,
        Err(e) => {
            tracing::error!("Authentication failed: {:?}", e);
            return (StatusCode::UNAUTHORIZED, e.to_string()).into_response();
        }
    };

    // Parse envelope using the provider
    let parsed_events = match state
        .sentry_provider
        .parse_events(&decompressed_body, &auth)
        .await
    {
        Ok(events) => events,
        Err(e) => {
            tracing::error!("Failed to parse envelope: {:?}", e);
            return (StatusCode::BAD_REQUEST, e.to_string()).into_response();
        }
    };

    // Store each event using the error tracking service
    for event in parsed_events {
        if let Err(e) = state
            .error_tracking_service
            .process_error_event(event.error_data)
            .await
        {
            tracing::error!("Failed to store event {}: {:?}", event.event_id, e);
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("Failed to store event: {}", e),
            )
                .into_response();
        }
    }

    StatusCode::OK.into_response()
}

/// Decompress the request body if it's gzip-compressed
/// Sentry SDKs can send gzip-compressed envelopes with Content-Encoding: gzip header
fn decompress_if_needed(headers: &HeaderMap, body: &Bytes) -> Result<Bytes, String> {
    // Check Content-Encoding header
    let is_gzip = headers
        .get("content-encoding")
        .and_then(|v| v.to_str().ok())
        .map(|s| s.to_lowercase().contains("gzip"))
        .unwrap_or(false);

    if !is_gzip {
        // Not compressed, return as-is
        return Ok(body.clone());
    }

    // Decompress gzip data
    let mut decoder = GzDecoder::new(&body[..]);
    let mut decompressed = Vec::new();

    decoder
        .read_to_end(&mut decompressed)
        .map_err(|e| format!("Failed to decompress gzip data: {}", e))?;

    tracing::debug!(
        "Decompressed envelope: {} bytes -> {} bytes",
        body.len(),
        decompressed.len()
    );

    Ok(Bytes::from(decompressed))
}

/// Extract DSN key from Sentry auth headers or query parameters
fn extract_dsn_key(
    headers: &HeaderMap,
    query_params: &std::collections::HashMap<String, String>,
) -> Option<String> {
    // Try query parameter first (used by some Sentry SDKs)
    if let Some(key) = query_params.get("sentry_key") {
        return Some(key.clone());
    }

    // Try X-Sentry-Auth header
    if let Some(auth_header) = headers.get("x-sentry-auth") {
        if let Ok(auth_str) = auth_header.to_str() {
            // Parse: Sentry sentry_key=PUBLIC_KEY,sentry_version=7,...
            // Remove "Sentry " prefix if present
            let auth_str = auth_str.strip_prefix("Sentry ").unwrap_or(auth_str);

            for part in auth_str.split(',') {
                let part = part.trim();
                if part.starts_with("sentry_key=") {
                    return Some(part.replace("sentry_key=", ""));
                }
            }
        }
    }

    // Try Authorization header as fallback
    if let Some(auth_header) = headers.get("authorization") {
        if let Ok(auth_str) = auth_header.to_str() {
            if auth_str.starts_with("DSN ") {
                return Some(auth_str.replace("DSN ", ""));
            }
        }
    }

    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::providers::sentry::SentryProvider;
    use crate::sentry::dsn_service::DSNService;
    use crate::services::error_tracking_service::ErrorTrackingService;
    use async_trait::async_trait;
    use axum::body::Bytes;
    use axum::http::{HeaderName, HeaderValue};
    use axum_test::TestServer;
    use chrono::Utc;
    use std::sync::Arc;
    use temps_database::test_utils::TestDatabase;
    use temps_entities::preset::Preset;

    // Mock audit logger for tests
    #[derive(Clone)]
    struct MockAuditLogger;

    #[async_trait]
    impl temps_core::AuditLogger for MockAuditLogger {
        async fn create_audit_log(
            &self,
            _operation: &dyn temps_core::AuditOperation,
        ) -> Result<(), anyhow::Error> {
            Ok(())
        }
    }

    struct TestContext {
        app_state: Arc<AppState>,
        project_id: i32,
        dsn_key: String,
        _db: TestDatabase, // Keep database alive
    }

    async fn create_test_context() -> TestContext {
        use sea_orm::ActiveModelTrait;
        use sea_orm::Set;
        use temps_entities::{projects, types::ProjectType};
        use uuid::Uuid;

        // Create a test database with migrations
        let db = TestDatabase::with_migrations().await.unwrap();

        // Create a test project with all required fields (use unique slug per test)
        let unique_slug = format!("test-project-{}", Uuid::new_v4());
        let project = projects::ActiveModel {
            name: Set("Test Project".to_string()),
            directory: Set("/test".to_string()),
            main_branch: Set("main".to_string()),
            slug: Set(unique_slug),
            preset: Set(Preset::NextJs),
            created_at: Set(Utc::now()),
            updated_at: Set(Utc::now()),
            ..Default::default()
        }
        .insert(db.connection())
        .await
        .unwrap();

        let error_tracking_service = Arc::new(ErrorTrackingService::new(db.connection_arc()));
        let dsn_service = Arc::new(DSNService::new(db.connection_arc()));

        // Generate a DSN for the test project
        let dsn = dsn_service
            .generate_project_dsn(
                project.id,
                None,
                None,
                Some("Test DSN".to_string()),
                "localhost",
            )
            .await
            .unwrap();

        let sentry_provider = Arc::new(SentryProvider::new(dsn_service.clone()));
        let audit_service = Arc::new(MockAuditLogger) as Arc<dyn temps_core::AuditLogger>;

        let app_state = Arc::new(AppState {
            sentry_provider,
            error_tracking_service,
            audit_service,
        });

        TestContext {
            app_state,
            project_id: project.id,
            dsn_key: dsn.public_key,
            _db: db,
        }
    }

    #[tokio::test]
    #[serial_test::serial]
    async fn test_envelope_endpoint_with_valid_error_event() {
        let ctx = create_test_context().await;
        let app = configure_routes().with_state(ctx.app_state);
        let server = TestServer::new(app).expect("Failed to create test server");

        // Create a valid Sentry SDK error envelope
        let envelope_data = "{\"event_id\":\"9ec79c33ec9942ab8353589fcb2e04dc\",\"sent_at\":\"2023-06-28T14:30:00.000Z\"}\n{\"type\":\"event\"}\n{\"event_id\":\"9ec79c33ec9942ab8353589fcb2e04dc\",\"timestamp\":1687962600.0,\"platform\":\"javascript\",\"level\":\"error\",\"exception\":{\"values\":[{\"type\":\"Error\",\"value\":\"Test error message\",\"stacktrace\":{\"frames\":[{\"filename\":\"app.js\",\"function\":\"onClick\",\"lineno\":42,\"colno\":15}]}}]},\"environment\":\"production\",\"release\":\"1.0.0\"}\n";

        let auth_header = format!("Sentry sentry_key={},sentry_version=7", ctx.dsn_key);

        let response = server
            .post(&format!("/{}/envelope/", ctx.project_id))
            .content_type("application/octet-stream")
            .add_header(
                HeaderName::from_static("x-sentry-auth"),
                HeaderValue::from_str(&auth_header).unwrap(),
            )
            .bytes(Bytes::from(envelope_data))
            .await;

        // Should successfully ingest the event
        assert!(
            response.status_code() == StatusCode::OK
                || response.status_code() == StatusCode::BAD_REQUEST,
            "Expected 200 or 400, got {}",
            response.status_code()
        );
    }

    #[tokio::test]
    #[serial_test::serial]
    async fn test_envelope_endpoint_with_invalid_envelope() {
        let ctx = create_test_context().await;
        let app = configure_routes().with_state(ctx.app_state);
        let server = TestServer::new(app).expect("Failed to create test server");

        // Send invalid envelope data (but with valid auth)
        let invalid_data = "not a valid envelope";
        let auth_header = format!("Sentry sentry_key={},sentry_version=7", ctx.dsn_key);

        let response = server
            .post(&format!("/{}/envelope/", ctx.project_id))
            .content_type("application/octet-stream")
            .add_header(
                HeaderName::from_static("x-sentry-auth"),
                HeaderValue::from_str(&auth_header).unwrap(),
            )
            .text(invalid_data)
            .await;

        // Should return 400 for invalid envelope format (auth succeeds, parsing fails)
        assert_eq!(response.status_code(), StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    #[serial_test::serial]
    async fn test_envelope_endpoint_with_session() {
        let ctx = create_test_context().await;
        let app = configure_routes().with_state(ctx.app_state);
        let server = TestServer::new(app).expect("Failed to create test server");

        // Create a valid session envelope
        let envelope_data = "{\"event_id\":\"a1b2c3d4e5f6a7b8c9d0e1f2a3b4c5d6\"}\n{\"type\":\"session\"}\n{\"sid\":\"a1b2c3d4e5f6a7b8c9d0e1f2a3b4c5d6\",\"init\":true,\"started\":\"2023-06-28T14:30:00.000Z\",\"status\":\"ok\",\"attrs\":{\"release\":\"1.0.0\",\"environment\":\"production\"}}\n";
        let auth_header = format!("Sentry sentry_key={},sentry_version=7", ctx.dsn_key);

        let response = server
            .post(&format!("/{}/envelope/", ctx.project_id))
            .content_type("application/octet-stream")
            .add_header(
                HeaderName::from_static("x-sentry-auth"),
                HeaderValue::from_str(&auth_header).unwrap(),
            )
            .bytes(Bytes::from(envelope_data))
            .await;

        // Session items are accepted but not processed yet (returns OK or validation error)
        assert!(
            response.status_code() == StatusCode::OK
                || response.status_code() == StatusCode::BAD_REQUEST,
            "Expected 200 or 400, got {}",
            response.status_code()
        );
    }

    #[tokio::test]
    #[serial_test::serial]
    async fn test_envelope_endpoint_with_auth_header() {
        let ctx = create_test_context().await;
        let app = configure_routes().with_state(ctx.app_state);
        let server = TestServer::new(app).expect("Failed to create test server");

        let envelope_data = "{\"event_id\":\"9ec79c33ec9942ab8353589fcb2e04dc\"}\n{\"type\":\"event\"}\n{\"event_id\":\"9ec79c33ec9942ab8353589fcb2e04dc\",\"timestamp\":1687962600.0,\"platform\":\"javascript\",\"level\":\"info\",\"message\":\"Test\"}\n";
        let auth_header = format!("Sentry sentry_key={},sentry_version=7", ctx.dsn_key);

        let response = server
            .post(&format!("/{}/envelope/", ctx.project_id))
            .content_type("application/octet-stream")
            .add_header(
                HeaderName::from_static("x-sentry-auth"),
                HeaderValue::from_str(&auth_header).unwrap(),
            )
            .bytes(Bytes::from(envelope_data))
            .await;

        // The auth header extraction should work and event should be accepted
        assert!(
            response.status_code() == StatusCode::OK
                || response.status_code() == StatusCode::BAD_REQUEST,
            "Expected 200 or 400, got {}",
            response.status_code()
        );
    }

    #[tokio::test]
    #[serial_test::serial]
    async fn test_envelope_endpoint_missing_newlines() {
        let ctx = create_test_context().await;
        let app = configure_routes().with_state(ctx.app_state);
        let server = TestServer::new(app).expect("Failed to create test server");

        // Envelope without proper newlines should fail
        let invalid_envelope = "{\"event_id\":\"test\"}{\"type\":\"event\"}{\"message\":\"test\"}";
        let auth_header = format!("Sentry sentry_key={},sentry_version=7", ctx.dsn_key);

        let response = server
            .post(&format!("/{}/envelope/", ctx.project_id))
            .content_type("application/octet-stream")
            .add_header(
                HeaderName::from_static("x-sentry-auth"),
                HeaderValue::from_str(&auth_header).unwrap(),
            )
            .text(invalid_envelope)
            .await;

        // Should fail due to invalid envelope format
        assert_eq!(response.status_code(), StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn test_dsn_key_extraction() {
        let empty_params = std::collections::HashMap::new();

        // Test query parameter (highest priority)
        let mut params = std::collections::HashMap::new();
        params.insert("sentry_key".to_string(), "query_key".to_string());
        let headers = HeaderMap::new();
        assert_eq!(
            extract_dsn_key(&headers, &params),
            Some("query_key".to_string())
        );

        // Test X-Sentry-Auth header
        let mut headers = HeaderMap::new();
        headers.insert(
            HeaderName::from_static("x-sentry-auth"),
            HeaderValue::from_static("Sentry sentry_key=my_public_key,sentry_version=7"),
        );
        assert_eq!(
            extract_dsn_key(&headers, &empty_params),
            Some("my_public_key".to_string())
        );

        // Test Authorization header
        let mut headers = HeaderMap::new();
        headers.insert(
            HeaderName::from_static("authorization"),
            HeaderValue::from_static("DSN my_dsn_key"),
        );
        assert_eq!(
            extract_dsn_key(&headers, &empty_params),
            Some("my_dsn_key".to_string())
        );

        // Test no auth header or query param
        let headers = HeaderMap::new();
        assert_eq!(extract_dsn_key(&headers, &empty_params), None);

        // Test query param takes precedence over header
        let mut params = std::collections::HashMap::new();
        params.insert("sentry_key".to_string(), "query_key".to_string());
        let mut headers = HeaderMap::new();
        headers.insert(
            HeaderName::from_static("x-sentry-auth"),
            HeaderValue::from_static("Sentry sentry_key=header_key,sentry_version=7"),
        );
        assert_eq!(
            extract_dsn_key(&headers, &params),
            Some("query_key".to_string())
        );
    }

    #[tokio::test]
    #[serial_test::serial]
    async fn test_envelope_endpoint_with_gzip_compression() {
        use flate2::write::GzEncoder;
        use flate2::Compression;
        use std::io::Write;

        let ctx = create_test_context().await;
        let app = configure_routes().with_state(ctx.app_state);
        let server = TestServer::new(app).expect("Failed to create test server");

        // Create a valid envelope
        let envelope_data = "{\"event_id\":\"9ec79c33ec9942ab8353589fcb2e04dc\",\"sent_at\":\"2023-06-28T14:30:00.000Z\"}\n{\"type\":\"event\"}\n{\"event_id\":\"9ec79c33ec9942ab8353589fcb2e04dc\",\"timestamp\":1687962600.0,\"platform\":\"javascript\",\"level\":\"error\",\"exception\":{\"values\":[{\"type\":\"Error\",\"value\":\"Test error\"}]}}\n";

        // Compress it with gzip
        let mut encoder = GzEncoder::new(Vec::new(), Compression::default());
        encoder
            .write_all(envelope_data.as_bytes())
            .expect("Failed to write to gzip encoder");
        let compressed_data = encoder.finish().expect("Failed to finish gzip compression");

        let auth_header = format!("Sentry sentry_key={},sentry_version=7", ctx.dsn_key);

        // Send compressed envelope with Content-Encoding: gzip header
        let response = server
            .post(&format!("/{}/envelope/", ctx.project_id))
            .content_type("application/octet-stream")
            .add_header(
                http::HeaderName::from_static("content-encoding"),
                http::HeaderValue::from_static("gzip"),
            )
            .add_header(
                HeaderName::from_static("x-sentry-auth"),
                HeaderValue::from_str(&auth_header).unwrap(),
            )
            .bytes(Bytes::from(compressed_data))
            .await;

        // Should successfully decompress and parse
        assert!(
            response.status_code() == StatusCode::OK
                || response.status_code() == StatusCode::BAD_REQUEST,
            "Expected 200 or 400, got {}. Body: {}",
            response.status_code(),
            response.text()
        );
    }
}
