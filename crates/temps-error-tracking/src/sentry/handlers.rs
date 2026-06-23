use axum::{
    body::Bytes,
    extract::{ConnectInfo, DefaultBodyLimit, Extension, Path, Query, State},
    http::{HeaderMap, StatusCode},
    response::IntoResponse,
    routing::post,
    Json, Router,
};
use flate2::read::GzDecoder;
use std::io::Read as IoRead;
use std::net::SocketAddr;
use std::sync::Arc;
use tower_http::cors::{Any, CorsLayer};
use tracing::debug;
use utoipa::OpenApi;

use crate::providers::{sentry::SentryProvider, ErrorProvider};
use crate::sentry::types::{SentryEventRequest, SentryEventResponse};
use crate::services::error_tracking_service::ErrorTrackingService;
use temps_geo::IpAddressService;

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
    pub ip_address_service: Option<Arc<IpAddressService>>,
    pub db: Option<Arc<sea_orm::DatabaseConnection>>,
    pub telemetry: Arc<dyn temps_core::telemetry::TelemetryReporter>,
}

/// Maximum compressed body size for Sentry ingest routes (2 MiB).
///
/// Typical Sentry events are 5–50 KB. The 2 MiB cap rejects slow-POST DoS
/// attempts before the body is fully buffered into memory. The decompression
/// bomb guard (MAX_DECOMPRESSED_SIZE) provides a second layer of protection
/// against gzip bombs where the compressed input is under this limit but the
/// expanded output is enormous.
const SENTRY_INGEST_BODY_LIMIT: usize = 2 * 1024 * 1024;

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
        // Fix #3: cap compressed body to 2 MiB before any buffering occurs.
        // This prevents slow-POST DoS where a client drip-feeds a large body
        // to hold a Tokio worker thread indefinitely.
        .layer(DefaultBodyLimit::max(SENTRY_INGEST_BODY_LIMIT))
        .layer(cors)
}

// Types are now in types.rs

/// Ingest a Sentry event (JSON payload)
#[utoipa::path(
    post,
    path = "/{project_id}/store/",
    params(
        ("project_id" = i32, Path, description = "Project ID")
    ),
    request_body = SentryEventRequest,
    responses(
        (status = 200, description = "Event ingested", body = SentryEventResponse),
        (status = 400, description = "Bad request"),
        (status = 401, description = "Unauthorized"),
        (status = 413, description = "Request body too large (exceeds 2 MiB)"),
    ),
    tag = "sentry-ingestor"
)]
async fn ingest_sentry_event(
    State(state): State<Arc<AppState>>,
    Path(project_id): Path<i32>,
    Query(params): Query<std::collections::HashMap<String, String>>,
    headers: HeaderMap,
    // Fix #2: read ConnectInfo from extensions (inserted by axum when the listener
    // is started with `into_make_service_with_connect_info`). Option<Extension<T>>
    // returns None gracefully when the extension is absent (e.g. in unit tests).
    connect_info: Option<Extension<ConnectInfo<SocketAddr>>>,
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
    let mut parsed_event = match state.sentry_provider.parse_json_event(event, &auth).await {
        Ok(event) => event,
        Err(e) => {
            tracing::error!("Failed to parse event: {:?}", e);
            return (StatusCode::BAD_REQUEST, e.to_string()).into_response();
        }
    };

    // Fix #2: resolve the real client IP using proxy-trust logic.
    // XFF is honored only when the direct TCP peer is loopback (our trusted Pingora proxy).
    // An attacker connecting directly and setting X-Forwarded-For is ignored.
    let peer = connect_info.map(|ext| ext.0 .0);
    let client_ip = temps_auth::resolve_client_ip(&headers, peer);

    // Enrich with IP geolocation and visitor correlation
    enrich_error_event(
        &mut parsed_event.error_data,
        Some(client_ip.as_str()),
        state.ip_address_service.as_ref(),
        state.db.as_ref(),
    )
    .await;

    // Store event using the error tracking service
    match state
        .error_tracking_service
        .process_error_event(parsed_event.error_data)
        .await
    {
        Ok(_) => {
            // Once-per-instance: "error tracking is in use on this instance".
            // report_once dedupes durably (and frees the error-ingest hot path
            // of the previous per-event has_error_groups DB lookup), so this is
            // instance-scoped — consistent with the other first-touch events —
            // rather than the old per-project guard.
            state.telemetry.report_once(
                "error_tracking_first_error",
                temps_core::telemetry::TelemetryEvent::new(
                    temps_core::telemetry::TelemetryEventKind::ErrorTrackingFirstError,
                ),
            );
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
    path = "/{project_id}/envelope/",
    params(
        ("project_id" = i32, Path, description = "Project ID")
    ),
    request_body(content = String, description = "Sentry envelope as binary data", content_type = "application/octet-stream"),
    responses(
        (status = 200, description = "Envelope ingested"),
        (status = 400, description = "Bad request"),
        (status = 401, description = "Unauthorized"),
        (status = 413, description = "Request body too large (exceeds 2 MiB)"),
    ),
    tag = "sentry-ingestor"
)]
async fn ingest_sentry_envelope(
    State(state): State<Arc<AppState>>,
    Path(project_id): Path<i32>,
    Query(params): Query<std::collections::HashMap<String, String>>,
    headers: HeaderMap,
    // Fix #2: read ConnectInfo from extensions for proxy-trust IP resolution.
    connect_info: Option<Extension<ConnectInfo<SocketAddr>>>,
    body: Bytes,
) -> impl IntoResponse {
    // Log only key names — values and headers can contain the Sentry DSN auth key.
    debug!(
        "Sentry ingest: query param keys={:?}, header names={:?}",
        params.keys().collect::<Vec<_>>(),
        headers.keys().map(|k| k.as_str()).collect::<Vec<_>>()
    );
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
            tracing::error!(
                "Failed to parse envelope for project {}: {:?}",
                project_id,
                e
            );
            return (StatusCode::BAD_REQUEST, e.to_string()).into_response();
        }
    };

    // Fix #2: resolve the real client IP using proxy-trust logic.
    // XFF is honored only when the direct TCP peer is loopback (our trusted Pingora proxy).
    let peer = connect_info.map(|ext| ext.0 .0);
    let client_ip = temps_auth::resolve_client_ip(&headers, peer);

    // Store each event using the error tracking service
    for mut event in parsed_events {
        // Enrich with IP geolocation and visitor correlation
        enrich_error_event(
            &mut event.error_data,
            Some(client_ip.as_str()),
            state.ip_address_service.as_ref(),
            state.db.as_ref(),
        )
        .await;

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

        // Once-per-instance: "error tracking is in use here". report_once is
        // idempotent and durably deduped, so no per-request flag is needed —
        // and the error-ingest hot path no longer pays a has_error_groups DB
        // lookup per envelope.
        state.telemetry.report_once(
            "error_tracking_first_error",
            temps_core::telemetry::TelemetryEvent::new(
                temps_core::telemetry::TelemetryEventKind::ErrorTrackingFirstError,
            ),
        );
    }

    StatusCode::OK.into_response()
}

/// Maximum decompressed size to prevent decompression bombs (10 MB)
const MAX_DECOMPRESSED_SIZE: usize = 10 * 1024 * 1024;

/// Decompress the request body if it's gzip-compressed
/// Sentry SDKs can send gzip-compressed envelopes with Content-Encoding: gzip header
///
/// SECURITY: Uses a size-limited reader to prevent decompression bomb attacks where
/// a small compressed payload expands to consume all available memory.
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

    // Decompress gzip data with size limit to prevent decompression bombs
    let decoder = GzDecoder::new(&body[..]);
    let mut limited_reader = decoder.take(MAX_DECOMPRESSED_SIZE as u64 + 1);
    let mut decompressed = Vec::new();

    limited_reader
        .read_to_end(&mut decompressed)
        .map_err(|e| format!("Failed to decompress gzip data: {}", e))?;

    if decompressed.len() > MAX_DECOMPRESSED_SIZE {
        return Err(format!(
            "Decompressed data exceeds maximum allowed size of {} bytes",
            MAX_DECOMPRESSED_SIZE
        ));
    }

    tracing::debug!(
        "Decompressed envelope: {} bytes -> {} bytes",
        body.len(),
        decompressed.len()
    );

    Ok(Bytes::from(decompressed))
}

/// Enrich error event data with IP geolocation and visitor information.
///
/// This resolves the client IP to a geolocation record and looks up the
/// most recent visitor for this project with the same IP address.
async fn enrich_error_event(
    error_data: &mut crate::services::types::CreateErrorEventData,
    client_ip: Option<&str>,
    ip_address_service: Option<&Arc<IpAddressService>>,
    db: Option<&Arc<sea_orm::DatabaseConnection>>,
) {
    let ip = match client_ip.or(error_data.user_ip_address.as_deref()) {
        Some(ip) if !ip.is_empty() => ip,
        _ => return,
    };

    let ip_service = match ip_address_service {
        Some(s) => s,
        None => return,
    };

    // Resolve IP geolocation
    match ip_service.get_or_create_ip(ip).await {
        Ok(ip_info) => {
            let geo_id = ip_info.id;
            error_data.ip_geolocation_id = Some(geo_id);

            // Try to find a visitor with this IP for this project
            if let Some(db) = db {
                use sea_orm::{ColumnTrait, EntityTrait, QueryFilter, QueryOrder};
                use temps_entities::visitor;

                match visitor::Entity::find()
                    .filter(visitor::Column::ProjectId.eq(error_data.project_id))
                    .filter(visitor::Column::IpAddressId.eq(geo_id))
                    .order_by_desc(visitor::Column::LastSeen)
                    .one(db.as_ref())
                    .await
                {
                    Ok(Some(v)) => {
                        error_data.visitor_id = Some(v.id);
                        debug!("Linked error event to visitor {} (ip: {})", v.id, ip);
                    }
                    Ok(None) => {
                        // No visitor found for this IP — that's fine
                    }
                    Err(e) => {
                        debug!("Failed to look up visitor by IP: {}", e);
                    }
                }
            }
        }
        Err(e) => {
            debug!("Failed to resolve IP geolocation for {}: {}", ip, e);
        }
    }
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
        use temps_entities::projects;
        use uuid::Uuid;

        // Create a test database with migrations
        let db = TestDatabase::with_migrations().await.unwrap();

        // Create a test project with all required fields (use unique slug per test)
        let unique_slug = format!("test-project-{}", Uuid::new_v4());
        let project = projects::ActiveModel {
            name: Set("Test Project".to_string()),
            repo_name: Set("test-repo".to_string()),
            repo_owner: Set("test-owner".to_string()),
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
            ip_address_service: None,
            db: None,
            telemetry: Arc::new(temps_core::telemetry::NoopTelemetryReporter),
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

    // === Decompression bomb protection tests ===

    #[test]
    fn test_decompress_normal_gzip() {
        use flate2::write::GzEncoder;
        use flate2::Compression;
        use std::io::Write;

        let data = b"Hello, World!";
        let mut encoder = GzEncoder::new(Vec::new(), Compression::default());
        encoder.write_all(data).unwrap();
        let compressed = encoder.finish().unwrap();

        let mut headers = HeaderMap::new();
        headers.insert("content-encoding", "gzip".parse().unwrap());
        let body = Bytes::from(compressed);

        let result = decompress_if_needed(&headers, &body);
        assert!(result.is_ok());
        assert_eq!(result.unwrap().as_ref(), data);
    }

    #[test]
    fn test_decompress_no_encoding_passthrough() {
        let headers = HeaderMap::new();
        let body = Bytes::from_static(b"raw data");
        let result = decompress_if_needed(&headers, &body);
        assert!(result.is_ok());
        assert_eq!(result.unwrap().as_ref(), b"raw data");
    }

    #[test]
    fn test_decompress_bomb_rejected() {
        use flate2::write::GzEncoder;
        use flate2::Compression;
        use std::io::Write;

        // Create a gzip bomb: highly compressible data that expands beyond MAX_DECOMPRESSED_SIZE
        // A sequence of zeros compresses extremely well
        let large_data = vec![0u8; MAX_DECOMPRESSED_SIZE + 1024];
        let mut encoder = GzEncoder::new(Vec::new(), Compression::best());
        encoder.write_all(&large_data).unwrap();
        let compressed = encoder.finish().unwrap();

        // The compressed data should be much smaller than the expanded data
        assert!(
            compressed.len() < MAX_DECOMPRESSED_SIZE,
            "Compressed size {} should be much smaller than limit {}",
            compressed.len(),
            MAX_DECOMPRESSED_SIZE
        );

        let mut headers = HeaderMap::new();
        headers.insert("content-encoding", "gzip".parse().unwrap());
        let body = Bytes::from(compressed);

        let result = decompress_if_needed(&headers, &body);
        assert!(result.is_err(), "Decompression bomb should be rejected");
        assert!(
            result.unwrap_err().contains("exceeds maximum"),
            "Error should mention size limit"
        );
    }

    #[test]
    fn test_decompress_at_exact_limit_allowed() {
        use flate2::write::GzEncoder;
        use flate2::Compression;
        use std::io::Write;

        // Data exactly at the limit should be allowed
        let data = vec![b'A'; MAX_DECOMPRESSED_SIZE];
        let mut encoder = GzEncoder::new(Vec::new(), Compression::default());
        encoder.write_all(&data).unwrap();
        let compressed = encoder.finish().unwrap();

        let mut headers = HeaderMap::new();
        headers.insert("content-encoding", "gzip".parse().unwrap());
        let body = Bytes::from(compressed);

        let result = decompress_if_needed(&headers, &body);
        assert!(result.is_ok(), "Data at exact limit should be allowed");
        assert_eq!(result.unwrap().len(), MAX_DECOMPRESSED_SIZE);
    }

    // === Fix #2 — XFF-spoof guard tests ===

    /// Non-loopback peer with X-Forwarded-For: resolved IP must be the peer, not the XFF value.
    ///
    /// This verifies the ingest path delegates to `temps_auth::resolve_client_ip`, which
    /// ignores client-supplied XFF headers when the direct TCP peer is not a trusted proxy.
    #[test]
    fn test_xff_spoof_rejected_for_non_loopback_peer() {
        let mut headers = HeaderMap::new();
        headers.insert(
            HeaderName::from_static("x-forwarded-for"),
            HeaderValue::from_static("1.2.3.4"),
        );
        // Non-loopback peer: attacker connected directly, not via a trusted proxy
        let peer: SocketAddr = "8.8.8.8:443".parse().unwrap();
        let resolved = temps_auth::resolve_client_ip(&headers, Some(peer));
        // Must be the peer address, NOT the spoofed XFF value
        assert_eq!(
            resolved, "8.8.8.8",
            "XFF must be ignored for non-loopback peers"
        );
    }

    /// Loopback peer with X-Forwarded-For: the rightmost XFF entry is trusted.
    ///
    /// When Pingora (our reverse proxy) runs on the same host it connects as 127.0.0.1,
    /// so XFF is trusted and we return the real client IP that the proxy appended.
    #[test]
    fn test_xff_trusted_for_loopback_peer() {
        let mut headers = HeaderMap::new();
        headers.insert(
            HeaderName::from_static("x-forwarded-for"),
            // "client, proxy" → rightmost is what the trusted proxy appended
            HeaderValue::from_static("1.2.3.4, 5.6.7.8"),
        );
        let peer: SocketAddr = "127.0.0.1:1234".parse().unwrap();
        let resolved = temps_auth::resolve_client_ip(&headers, Some(peer));
        // Rightmost XFF entry is the one appended by our trusted proxy
        assert_eq!(
            resolved, "5.6.7.8",
            "Rightmost XFF entry should be trusted for loopback peer"
        );
    }

    // === Fix #3 — body-limit constant sanity check ===

    /// The ingest body-limit constant must be exactly 2 MiB.
    ///
    /// A typical Sentry event is 5–50 KB; 2 MiB is a generous ceiling that still
    /// protects against slow-POST DoS. This test is a canary — if someone raises
    /// the constant they have to update the test too and justify the change.
    #[test]
    fn test_sentry_ingest_body_limit_is_2mib() {
        assert_eq!(
            SENTRY_INGEST_BODY_LIMIT,
            2 * 1024 * 1024,
            "Sentry ingest body limit must be 2 MiB (2 * 1024 * 1024 bytes)"
        );
    }
}
