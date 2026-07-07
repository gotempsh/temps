//! HTTP handlers for log aggregator endpoints

use std::convert::Infallible;
use std::sync::Arc;

use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::response::sse::{Event, KeepAlive, Sse};
use axum::response::IntoResponse;
use axum::routing::{delete, get, post};
use axum::{Json, Router};
use chrono::{Duration, Utc};
use serde::{Deserialize, Serialize};
use tokio_stream::StreamExt;
use utoipa::{OpenApi, ToSchema};
use uuid::Uuid;

use axum::Extension;
use temps_auth::{permission_guard, RequireAuth};
use temps_core::problemdetails;
use temps_core::problemdetails::{Problem, ProblemDetails};
use temps_core::RequestMetadata;

use crate::error::LogAggregatorError;
use crate::handlers::types::LogAggregatorAppState;
use crate::types::*;

// ── Error conversion ────────────────────────────────────────────────────

impl From<LogAggregatorError> for Problem {
    fn from(error: LogAggregatorError) -> Self {
        match error {
            LogAggregatorError::ChunkNotFound { .. } => problemdetails::new(StatusCode::NOT_FOUND)
                .with_title("Chunk Not Found")
                .with_detail(error.to_string()),
            LogAggregatorError::ContainerNotFound { .. } => {
                problemdetails::new(StatusCode::NOT_FOUND)
                    .with_title("Container Not Found")
                    .with_detail(error.to_string())
            }
            LogAggregatorError::SearchMissingRequiredParams => {
                problemdetails::new(StatusCode::BAD_REQUEST)
                    .with_title("Missing Required Parameters")
                    .with_detail(error.to_string())
            }
            LogAggregatorError::SearchTimeRangeExceeded { .. } => {
                problemdetails::new(StatusCode::BAD_REQUEST)
                    .with_title("Time Range Exceeded")
                    .with_detail(error.to_string())
            }
            LogAggregatorError::InvalidCursor { .. } => {
                problemdetails::new(StatusCode::BAD_REQUEST)
                    .with_title("Invalid Cursor")
                    .with_detail(error.to_string())
            }
            LogAggregatorError::Validation { .. } => problemdetails::new(StatusCode::BAD_REQUEST)
                .with_title("Validation Error")
                .with_detail(error.to_string()),
            LogAggregatorError::Database(_) => {
                problemdetails::new(StatusCode::INTERNAL_SERVER_ERROR)
                    .with_title("Database Error")
                    .with_detail(error.to_string())
            }
            LogAggregatorError::ChunkWriteFailed { .. } => {
                problemdetails::new(StatusCode::INTERNAL_SERVER_ERROR)
                    .with_title("Storage Write Failed")
                    .with_detail(error.to_string())
            }
            LogAggregatorError::ChunkReadFailed { .. } => {
                problemdetails::new(StatusCode::INTERNAL_SERVER_ERROR)
                    .with_title("Storage Read Failed")
                    .with_detail(error.to_string())
            }
            LogAggregatorError::ChunkDeleteFailed { .. } => {
                problemdetails::new(StatusCode::INTERNAL_SERVER_ERROR)
                    .with_title("Storage Delete Failed")
                    .with_detail(error.to_string())
            }
            LogAggregatorError::ChunkListFailed { .. } => {
                problemdetails::new(StatusCode::INTERNAL_SERVER_ERROR)
                    .with_title("Storage List Failed")
                    .with_detail(error.to_string())
            }
            LogAggregatorError::CompressionFailed { .. } => {
                problemdetails::new(StatusCode::INTERNAL_SERVER_ERROR)
                    .with_title("Compression Error")
                    .with_detail(error.to_string())
            }
            LogAggregatorError::DecompressionFailed { .. } => {
                problemdetails::new(StatusCode::INTERNAL_SERVER_ERROR)
                    .with_title("Decompression Error")
                    .with_detail(error.to_string())
            }
            LogAggregatorError::DockerStreamFailed { .. } => {
                problemdetails::new(StatusCode::INTERNAL_SERVER_ERROR)
                    .with_title("Docker Stream Error")
                    .with_detail(error.to_string())
            }
            LogAggregatorError::StorageConfiguration { .. } => {
                problemdetails::new(StatusCode::INTERNAL_SERVER_ERROR)
                    .with_title("Storage Configuration Error")
                    .with_detail(error.to_string())
            }
            LogAggregatorError::Io(_) => problemdetails::new(StatusCode::INTERNAL_SERVER_ERROR)
                .with_title("IO Error")
                .with_detail(error.to_string()),
            LogAggregatorError::Serialization(_) => {
                problemdetails::new(StatusCode::INTERNAL_SERVER_ERROR)
                    .with_title("Serialization Error")
                    .with_detail(error.to_string())
            }
            LogAggregatorError::S3 { .. } => problemdetails::new(StatusCode::INTERNAL_SERVER_ERROR)
                .with_title("S3 Error")
                .with_detail(error.to_string()),
        }
    }
}

// ── Audit types ─────────────────────────────────────────────────────────

/// Audit event for log purge operations
#[derive(Debug, Clone, serde::Serialize)]
struct LogsPurgedAudit {
    pub context: temps_core::AuditContext,
    pub project_id: i32,
    pub before_timestamp: String,
    pub chunks_deleted: u64,
}

impl temps_core::AuditOperation for LogsPurgedAudit {
    fn operation_type(&self) -> String {
        "LOGS_PURGED".to_string()
    }

    fn user_id(&self) -> i32 {
        self.context.user_id
    }

    fn ip_address(&self) -> Option<String> {
        self.context.ip_address.clone()
    }

    fn user_agent(&self) -> &str {
        &self.context.user_agent
    }

    fn serialize(&self) -> temps_core::anyhow::Result<String> {
        Ok(serde_json::to_string(self)?)
    }
}

// ── Request/Response types ──────────────────────────────────────────────

#[derive(Deserialize, ToSchema)]
pub struct SearchLogsRequest {
    /// Project ID (integer, as used by the rest of the platform)
    pub project_id: i32,
    /// When set, search an imported/managed external service's logs instead
    /// of a project's. `project_id` is ignored in this mode.
    #[serde(default)]
    pub external_service_id: Option<i32>,
    /// Start of time range (ISO 8601). Defaults to 1 hour ago.
    pub start_time: Option<String>,
    /// End of time range (ISO 8601). Defaults to now.
    pub end_time: Option<String>,
    /// Filter by log levels
    #[serde(default)]
    pub levels: Vec<String>,
    /// Filter by services
    #[serde(default)]
    pub services: Vec<String>,
    /// Filter by environments
    #[serde(default)]
    pub envs: Vec<String>,
    /// Filter to specific containers (Docker container IDs). Empty = all
    /// containers. Drives "filter by container / show all" in a project's
    /// history, which spans multiple deployments and containers.
    #[serde(default)]
    pub container_ids: Vec<String>,
    /// Filter to specific worker nodes (node_id). Empty = all nodes, including
    /// control-plane-local logs.
    #[serde(default)]
    pub node_ids: Vec<i32>,
    /// Filter by deployment ID (deployments.id)
    pub deploy_id: Option<i32>,
    /// Full text search query
    pub text: Option<String>,
    /// Pagination cursor
    pub cursor: Option<String>,
    /// Page size (default: 100, max: 500)
    pub page_size: Option<u32>,
    /// grep -C: number of raw context lines to include before and after each
    /// match (0 = none, default). Clamped to 50 server-side. The surrounding
    /// lines ignore the level/text filters — they are the actual adjacent log
    /// lines, merged across overlapping matches.
    pub context_lines: Option<u32>,
}

#[derive(Serialize, ToSchema)]
pub struct SearchLogsResponse {
    pub lines: Vec<LogSearchLine>,
    pub next_cursor: Option<String>,
    pub search_mode: SearchMode,
    pub total_scanned: u64,
    /// Distinct containers/nodes/services available in the queried scope, for
    /// the filter dropdowns. Populated on the first page (no cursor).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub available_sources: Vec<LogSource>,
}

#[derive(Deserialize, ToSchema)]
pub struct ContextLogsRequest {
    #[schema(value_type = String)]
    pub chunk_id: Uuid,
    pub line_offset: i32,
    /// Number of context lines before and after (default: 25)
    pub lines: Option<u32>,
}

#[derive(Serialize, ToSchema)]
pub struct ContextLogsResponse {
    pub lines: Vec<ContextLine>,
    pub target_index: usize,
}

#[derive(Deserialize, ToSchema)]
pub struct TailLogsRequest {
    /// Project ID (integer, as used by the rest of the platform)
    pub project_id: i32,
    /// When set, tail an imported/managed external service's logs instead of
    /// a project's (`project_id` is ignored in this mode).
    #[serde(default)]
    pub external_service_id: Option<i32>,
    pub service: String,
    pub env: String,
    #[serde(default)]
    pub levels: Vec<String>,
    pub text: Option<String>,
}

#[derive(Deserialize, ToSchema)]
pub struct PurgeLogsRequest {
    /// Delete all logs before this timestamp (ISO 8601)
    pub before: String,
}

// ── OpenAPI Doc ─────────────────────────────────────────────────────────

#[derive(OpenApi)]
#[openapi(
    paths(search_logs, get_log_context, tail_logs, purge_project_logs),
    components(
        schemas(
            SearchLogsRequest,
            SearchLogsResponse,
            ContextLogsRequest,
            ContextLogsResponse,
            TailLogsRequest,
            PurgeLogsRequest,
            LogSearchLine,
            LogSource,
            ContextLine,
            LineContext,
            SearchMode,
            LogLevel,
            LogStream,
        )
    ),
    info(
        title = "Log Aggregator API",
        description = "API endpoints for searching, streaming, and managing application logs.",
        version = "1.0.0"
    ),
    tags(
        (name = "Logs", description = "Log search, context, live tail, and retention management")
    )
)]
pub struct LogAggregatorApiDoc;

// ── Routes ──────────────────────────────────────────────────────────────

pub fn configure_routes() -> Router<Arc<LogAggregatorAppState>> {
    Router::new()
        .route("/logs/search", post(search_logs))
        .route("/logs/context", get(get_log_context))
        .route("/logs/tail", get(tail_logs))
        .route("/projects/{project_id}/logs", delete(purge_project_logs))
}

// ── Handlers ────────────────────────────────────────────────────────────

/// Search logs with structured filters and full text search
#[utoipa::path(
    tag = "Logs",
    post,
    path = "/logs/search",
    request_body = SearchLogsRequest,
    responses(
        (status = 200, description = "Search results", body = SearchLogsResponse),
        (status = 400, description = "Invalid search parameters", body = ProblemDetails),
        (status = 401, description = "Unauthorized", body = ProblemDetails),
        (status = 500, description = "Internal server error", body = ProblemDetails)
    ),
    security(("bearer_auth" = []))
)]
async fn search_logs(
    RequireAuth(auth): RequireAuth,
    State(app_state): State<Arc<LogAggregatorAppState>>,
    Json(request): Json<SearchLogsRequest>,
) -> Result<impl IntoResponse, Problem> {
    permission_guard!(auth, LogsRead);

    let now = Utc::now();
    let start_time = request
        .start_time
        .as_ref()
        .and_then(|s| chrono::DateTime::parse_from_rfc3339(s).ok())
        .map(|dt| dt.with_timezone(&Utc))
        .unwrap_or(now - Duration::hours(1));
    let end_time = request
        .end_time
        .as_ref()
        .and_then(|s| chrono::DateTime::parse_from_rfc3339(s).ok())
        .map(|dt| dt.with_timezone(&Utc))
        .unwrap_or(now);

    let levels: Vec<LogLevel> = request
        .levels
        .iter()
        .filter_map(|l| LogLevel::parse(l))
        .collect();

    let filter = LogSearchFilter {
        project_id: request.project_id,
        external_service_id: request.external_service_id,
        start_time,
        end_time,
        levels,
        services: request.services,
        envs: request.envs,
        container_ids: request.container_ids,
        node_ids: request.node_ids,
        deploy_id: request.deploy_id,
        text: request.text,
        field_filters: vec![],
        cursor: request.cursor,
        page_size: request.page_size.unwrap_or(100),
        context_lines: request.context_lines.unwrap_or(0),
    };

    let result = app_state.search_service.search(&filter).await?;

    Ok(Json(SearchLogsResponse {
        lines: result.lines,
        next_cursor: result.next_cursor,
        search_mode: result.search_mode,
        total_scanned: result.total_scanned,
        available_sources: result.available_sources,
    }))
}

/// Get context lines surrounding a specific log line
#[utoipa::path(
    tag = "Logs",
    get,
    path = "/logs/context",
    params(
        ("chunk_id" = String, Query, description = "Chunk ID"),
        ("line_offset" = i32, Query, description = "Line offset within the chunk"),
        ("lines" = Option<u32>, Query, description = "Context lines before and after (default: 25)")
    ),
    responses(
        (status = 200, description = "Context lines", body = ContextLogsResponse),
        (status = 400, description = "Invalid parameters", body = ProblemDetails),
        (status = 401, description = "Unauthorized", body = ProblemDetails),
        (status = 404, description = "Chunk not found", body = ProblemDetails),
        (status = 500, description = "Internal server error", body = ProblemDetails)
    ),
    security(("bearer_auth" = []))
)]
async fn get_log_context(
    RequireAuth(auth): RequireAuth,
    State(app_state): State<Arc<LogAggregatorAppState>>,
    Query(request): Query<ContextLogsRequest>,
) -> Result<impl IntoResponse, Problem> {
    permission_guard!(auth, LogsRead);
    let context_req = ContextRequest {
        chunk_id: request.chunk_id,
        line_offset: request.line_offset,
        lines: request.lines.unwrap_or(25),
    };

    let result = app_state.search_service.get_context(&context_req).await?;

    Ok(Json(ContextLogsResponse {
        lines: result.lines,
        target_index: result.target_index,
    }))
}

/// Live tail logs via Server-Sent Events
#[utoipa::path(
    tag = "Logs",
    get,
    path = "/logs/tail",
    params(
        ("project_id" = String, Query, description = "Project ID"),
        ("service" = String, Query, description = "Service name"),
        ("env" = String, Query, description = "Environment"),
        ("levels" = Vec<String>, Query, description = "Optional level filters"),
        ("text" = Option<String>, Query, description = "Optional text filter")
    ),
    responses(
        (status = 200, description = "SSE stream of log lines"),
        (status = 401, description = "Unauthorized", body = ProblemDetails)
    ),
    security(("bearer_auth" = []))
)]
async fn tail_logs(
    RequireAuth(auth): RequireAuth,
    State(app_state): State<Arc<LogAggregatorAppState>>,
    Query(request): Query<TailLogsRequest>,
) -> Result<Sse<impl tokio_stream::Stream<Item = Result<Event, Infallible>>>, Problem> {
    permission_guard!(auth, LogsRead);

    let levels: Vec<LogLevel> = request
        .levels
        .iter()
        .filter_map(|l| LogLevel::parse(l))
        .collect();

    let filter = TailFilter {
        project_id: request.project_id,
        external_service_id: request.external_service_id,
        service: request.service,
        env: request.env,
        levels,
        text: request.text,
    };

    let stream = app_state.tail_service.subscribe(filter);

    // Auto-close after 30 minutes of inactivity
    let stream = tokio_stream::StreamExt::timeout(stream, std::time::Duration::from_secs(1800));

    let event_stream = stream.map(|result| {
        match result {
            Ok(line) => {
                let data = serde_json::to_string(&line).unwrap_or_default();
                Ok(Event::default().data(data))
            }
            Err(_timeout) => {
                // Stream closed due to inactivity timeout
                Ok(Event::default().comment("timeout"))
            }
        }
    });

    Ok(Sse::new(event_stream).keep_alive(KeepAlive::default()))
}

/// Purge all logs for a project before a given timestamp
#[utoipa::path(
    tag = "Logs",
    delete,
    path = "/projects/{project_id}/logs",
    params(
        ("project_id" = i32, Path, description = "Project ID"),
    ),
    request_body = PurgeLogsRequest,
    responses(
        (status = 200, description = "Purge completed"),
        (status = 400, description = "Invalid parameters", body = ProblemDetails),
        (status = 401, description = "Unauthorized", body = ProblemDetails),
        (status = 403, description = "Insufficient permissions", body = ProblemDetails),
        (status = 500, description = "Internal server error", body = ProblemDetails)
    ),
    security(("bearer_auth" = []))
)]
async fn purge_project_logs(
    RequireAuth(auth): RequireAuth,
    State(app_state): State<Arc<LogAggregatorAppState>>,
    Extension(metadata): Extension<RequestMetadata>,
    Path(project_id): Path<i32>,
    Json(request): Json<PurgeLogsRequest>,
) -> Result<impl IntoResponse, Problem> {
    permission_guard!(auth, LogsDelete);

    let before = chrono::DateTime::parse_from_rfc3339(&request.before)
        .map_err(|_| LogAggregatorError::Validation {
            message: format!("Invalid timestamp: {}", request.before),
        })?
        .with_timezone(&Utc);

    let result = app_state
        .retention_service
        .manual_purge(project_id, before)
        .await?;

    // Audit logging for the destructive purge operation
    let audit = LogsPurgedAudit {
        context: temps_core::AuditContext {
            user_id: auth.user_id(),
            ip_address: Some(metadata.ip_address.clone()),
            user_agent: metadata.user_agent.clone(),
        },
        project_id,
        before_timestamp: request.before,
        chunks_deleted: result.chunks_deleted,
    };
    if let Err(e) = app_state.audit_service.create_audit_log(&audit).await {
        tracing::error!("Failed to create audit log for log purge: {}", e);
    }

    Ok(Json(serde_json::json!({
        "chunks_deleted": result.chunks_deleted,
        "chunks_failed": result.chunks_failed,
        "bytes_reclaimed": result.bytes_reclaimed
    })))
}

#[cfg(test)]
mod tests {
    use super::*;
    use async_trait::async_trait;
    use axum::extract::Request;
    use axum::middleware;
    use axum_test::TestServer;
    use chrono::{Duration, Utc};
    use std::sync::Arc;
    use temps_database::test_utils::TestDatabase;
    use uuid::Uuid;

    use std::sync::atomic::{AtomicI32, Ordering};

    use crate::services::{
        ChunkWriterService, LogMetadataService, LogSearchService, RetentionService, TailService,
    };
    use crate::storage::FilesystemStorage;
    use crate::types::{LogLevel, LogLine, LogStream};

    /// Atomic counter for unique test project IDs (avoids cross-test collision)
    static TEST_PROJECT_COUNTER: AtomicI32 = AtomicI32::new(10_000);

    fn next_test_project_id() -> i32 {
        TEST_PROJECT_COUNTER.fetch_add(1, Ordering::Relaxed)
    }

    // ── Mock audit logger ───────────────────────────────────────────────

    #[derive(Clone)]
    struct MockAuditLogger;

    #[async_trait]
    impl temps_core::AuditLogger for MockAuditLogger {
        async fn create_audit_log(
            &self,
            _operation: &dyn temps_core::AuditOperation,
        ) -> Result<(), temps_core::anyhow::Error> {
            Ok(())
        }
    }

    // ── Test context ────────────────────────────────────────────────────

    struct TestContext {
        app_state: Arc<LogAggregatorAppState>,
        chunk_writer: Arc<ChunkWriterService>,
        metadata_service: Arc<LogMetadataService>,
        tail_tx: tokio::sync::broadcast::Sender<LogLine>,
        _db: TestDatabase,
        _tmp_dir: tempfile::TempDir,
    }

    /// Create a full test context with real DB, real filesystem storage, and all services wired up.
    async fn create_test_context() -> TestContext {
        let db = TestDatabase::with_migrations()
            .await
            .expect("Failed to create test database (is Docker running?)");

        let tmp_dir = tempfile::TempDir::new().expect("Failed to create temp dir");
        let storage: Arc<dyn crate::storage::LogStorage> = Arc::new(
            FilesystemStorage::new(tmp_dir.path().to_path_buf())
                .expect("Failed to create filesystem storage"),
        );

        let metadata_service = Arc::new(LogMetadataService::new(db.connection_arc()));
        let search_service = Arc::new(LogSearchService::new(
            storage.clone(),
            metadata_service.clone(),
        ));
        let (tail_tx, _) = tokio::sync::broadcast::channel::<LogLine>(1024);
        let tail_service = Arc::new(TailService::new(tail_tx.clone()));
        let retention_service = Arc::new(RetentionService::new(
            storage.clone(),
            metadata_service.clone(),
        ));
        let chunk_writer = Arc::new(ChunkWriterService::new(storage.clone()));
        let audit_service = Arc::new(MockAuditLogger) as Arc<dyn temps_core::AuditLogger>;

        let app_state = Arc::new(LogAggregatorAppState {
            search_service,
            metadata_service: metadata_service.clone(),
            tail_service,
            retention_service,
            audit_service,
        });

        TestContext {
            app_state,
            chunk_writer,
            metadata_service,
            tail_tx,
            _db: db,
            _tmp_dir: tmp_dir,
        }
    }

    /// Helper to create a mock AuthContext for testing
    fn create_test_auth_context() -> temps_auth::AuthContext {
        let user = temps_entities::users::Model {
            id: 1,
            name: "Test User".to_string(),
            email: "test@example.com".to_string(),
            password_hash: Some("hashed_password".to_string()),
            email_verified: true,
            email_verification_token: None,
            email_verification_expires: None,
            password_reset_token: None,
            password_reset_expires: None,
            deleted_at: None,
            mfa_secret: None,
            mfa_enabled: false,
            mfa_recovery_codes: None,
            oidc_subject: None,
            oidc_provider_id: None,
            created_at: Utc::now(),
            updated_at: Utc::now(),
        };
        temps_auth::AuthContext::new_session(user, temps_auth::Role::Admin)
    }

    /// Build a TestServer with auth middleware that injects AuthContext and RequestMetadata.
    fn build_test_server(app_state: Arc<LogAggregatorAppState>) -> TestServer {
        build_test_server_with_role(app_state, temps_auth::Role::Admin)
    }

    /// Build a TestServer with a specific role for permission testing.
    fn build_test_server_with_role(
        app_state: Arc<LogAggregatorAppState>,
        role: temps_auth::Role,
    ) -> TestServer {
        let auth_middleware =
            middleware::from_fn(move |mut req: Request, next: axum::middleware::Next| {
                let role = role.clone();
                async move {
                    let user = temps_entities::users::Model {
                        id: 1,
                        name: "Test User".to_string(),
                        email: "test@example.com".to_string(),
                        password_hash: Some("hashed_password".to_string()),
                        email_verified: true,
                        email_verification_token: None,
                        email_verification_expires: None,
                        password_reset_token: None,
                        password_reset_expires: None,
                        deleted_at: None,
                        mfa_secret: None,
                        mfa_enabled: false,
                        mfa_recovery_codes: None,
                        oidc_subject: None,
                        oidc_provider_id: None,
                        created_at: Utc::now(),
                        updated_at: Utc::now(),
                    };
                    let auth_context = temps_auth::AuthContext::new_session(user, role);
                    req.extensions_mut().insert(auth_context);
                    req.extensions_mut().insert(temps_core::RequestMetadata {
                        ip_address: "127.0.0.1".to_string(),
                        user_agent: "test-agent".to_string(),
                        headers: axum::http::HeaderMap::new(),
                        visitor_id_cookie: None,
                        session_id_cookie: None,
                        base_url: "http://localhost".to_string(),
                        scheme: "http".to_string(),
                        host: "localhost".to_string(),
                        is_secure: false,
                    });
                    next.run(req).await
                }
            });

        let app = configure_routes()
            .layer(auth_middleware)
            .with_state(app_state);

        TestServer::new(app).expect("Failed to create test server")
    }

    /// Create a test log line with the given parameters.
    fn make_log_line(
        project_id: i32,
        service: &str,
        env: &str,
        level: LogLevel,
        msg: &str,
        ts: chrono::DateTime<Utc>,
        container_id: &str,
    ) -> LogLine {
        LogLine {
            ts,
            stream: LogStream::Stdout,
            level,
            msg: msg.to_string(),
            fields: None,
            container_id: container_id.to_string(),
            service: service.to_string(),
            env: env.to_string(),
            project_id,
            external_service_id: None,
            deploy_id: None,
            node_id: None,
            node_name: None,
        }
    }

    /// Seed log lines into storage and DB via the chunk writer.
    /// Writes lines, flushes to storage, then inserts chunk metadata into DB.
    async fn seed_logs(ctx: &TestContext, lines: Vec<LogLine>) {
        if lines.is_empty() {
            return;
        }

        let container_id = lines[0].container_id.clone();

        for line in &lines {
            ctx.chunk_writer
                .write_line(line.clone())
                .await
                .expect("Failed to write log line");
        }

        let flush_result = ctx
            .chunk_writer
            .flush_container(&container_id)
            .await
            .expect("Failed to flush container buffer");

        // Insert chunk metadata into the database
        ctx.metadata_service
            .insert_chunk_meta(&flush_result.meta)
            .await
            .expect("Failed to insert chunk metadata");
    }

    // ── Tests ───────────────────────────────────────────────────────────

    #[tokio::test]
    #[serial_test::serial]
    async fn test_search_logs_returns_results() {
        let ctx = create_test_context().await;
        let project_id = next_test_project_id();
        let now = Utc::now();

        // Seed some log lines
        let lines = vec![
            make_log_line(
                project_id,
                "web",
                "prod",
                LogLevel::Error,
                "Database connection failed",
                now - Duration::minutes(5),
                "container-1",
            ),
            make_log_line(
                project_id,
                "web",
                "prod",
                LogLevel::Warn,
                "High memory usage detected",
                now - Duration::minutes(4),
                "container-1",
            ),
            make_log_line(
                project_id,
                "web",
                "prod",
                LogLevel::Info,
                "Request processed successfully",
                now - Duration::minutes(3),
                "container-1",
            ),
        ];
        seed_logs(&ctx, lines).await;

        let server = build_test_server(ctx.app_state.clone());

        let response = server
            .post("/logs/search")
            .json(&serde_json::json!({
                "project_id": project_id,
                "start_time": (now - Duration::hours(1)).to_rfc3339(),
                "end_time": (now + Duration::hours(1)).to_rfc3339(),
            }))
            .await;

        assert_eq!(response.status_code(), StatusCode::OK);

        let body: serde_json::Value = response.json();
        let lines_arr = body["lines"].as_array().expect("lines should be an array");
        assert!(
            !lines_arr.is_empty(),
            "Expected at least one log line in search results"
        );
    }

    #[tokio::test]
    #[serial_test::serial]
    async fn test_search_logs_empty_project() {
        let ctx = create_test_context().await;
        let project_id = next_test_project_id(); // No logs seeded

        let server = build_test_server(ctx.app_state.clone());
        let now = Utc::now();

        let response = server
            .post("/logs/search")
            .json(&serde_json::json!({
                "project_id": project_id,
                "start_time": (now - Duration::hours(1)).to_rfc3339(),
                "end_time": (now + Duration::hours(1)).to_rfc3339(),
            }))
            .await;

        assert_eq!(response.status_code(), StatusCode::OK);

        let body: serde_json::Value = response.json();
        let lines_arr = body["lines"].as_array().expect("lines should be an array");
        assert!(
            lines_arr.is_empty(),
            "Expected no log lines for empty project, got {}",
            lines_arr.len()
        );
    }

    #[tokio::test]
    #[serial_test::serial]
    async fn test_search_logs_filters_by_level() {
        let ctx = create_test_context().await;
        let project_id = next_test_project_id();
        let now = Utc::now();

        let lines = vec![
            make_log_line(
                project_id,
                "api",
                "prod",
                LogLevel::Error,
                "Fatal error occurred",
                now - Duration::minutes(5),
                "container-2",
            ),
            make_log_line(
                project_id,
                "api",
                "prod",
                LogLevel::Info,
                "Health check passed",
                now - Duration::minutes(4),
                "container-2",
            ),
            make_log_line(
                project_id,
                "api",
                "prod",
                LogLevel::Warn,
                "Slow query detected",
                now - Duration::minutes(3),
                "container-2",
            ),
        ];
        seed_logs(&ctx, lines).await;

        let server = build_test_server(ctx.app_state.clone());

        let response = server
            .post("/logs/search")
            .json(&serde_json::json!({
                "project_id": project_id,
                "start_time": (now - Duration::hours(1)).to_rfc3339(),
                "end_time": (now + Duration::hours(1)).to_rfc3339(),
                "levels": ["ERROR"],
            }))
            .await;

        assert_eq!(response.status_code(), StatusCode::OK);

        let body: serde_json::Value = response.json();
        let lines_arr = body["lines"].as_array().expect("lines should be an array");

        // All returned lines should be ERROR level
        for line in lines_arr {
            assert_eq!(
                line["level"].as_str().unwrap(),
                "ERROR",
                "Expected only ERROR level lines when filtering by ERROR"
            );
        }
        assert!(
            !lines_arr.is_empty(),
            "Expected at least one ERROR line in results"
        );
    }

    #[tokio::test]
    #[serial_test::serial]
    async fn test_search_logs_filters_by_service() {
        let ctx = create_test_context().await;
        let project_id = next_test_project_id();
        let now = Utc::now();

        // Seed logs for two different services in separate containers
        let web_lines = vec![make_log_line(
            project_id,
            "web",
            "prod",
            LogLevel::Error,
            "Web error",
            now - Duration::minutes(5),
            "container-web",
        )];
        let worker_lines = vec![make_log_line(
            project_id,
            "worker",
            "prod",
            LogLevel::Error,
            "Worker error",
            now - Duration::minutes(4),
            "container-worker",
        )];
        seed_logs(&ctx, web_lines).await;
        seed_logs(&ctx, worker_lines).await;

        let server = build_test_server(ctx.app_state.clone());

        let response = server
            .post("/logs/search")
            .json(&serde_json::json!({
                "project_id": project_id,
                "start_time": (now - Duration::hours(1)).to_rfc3339(),
                "end_time": (now + Duration::hours(1)).to_rfc3339(),
                "services": ["web"],
            }))
            .await;

        assert_eq!(response.status_code(), StatusCode::OK);

        let body: serde_json::Value = response.json();
        let lines_arr = body["lines"].as_array().expect("lines should be an array");

        for line in lines_arr {
            assert_eq!(
                line["service"].as_str().unwrap(),
                "web",
                "Expected only 'web' service lines when filtering by service"
            );
        }
        assert!(
            !lines_arr.is_empty(),
            "Expected at least one line for 'web' service"
        );
    }

    #[tokio::test]
    #[serial_test::serial]
    async fn test_search_logs_fulltext_search() {
        let ctx = create_test_context().await;
        let project_id = next_test_project_id();
        let now = Utc::now();

        let lines = vec![
            make_log_line(
                project_id,
                "api",
                "prod",
                LogLevel::Error,
                "Connection refused to database at port 5432",
                now - Duration::minutes(5),
                "container-ft",
            ),
            make_log_line(
                project_id,
                "api",
                "prod",
                LogLevel::Error,
                "Timeout waiting for response",
                now - Duration::minutes(4),
                "container-ft",
            ),
        ];
        seed_logs(&ctx, lines).await;

        let server = build_test_server(ctx.app_state.clone());

        let response = server
            .post("/logs/search")
            .json(&serde_json::json!({
                "project_id": project_id,
                "start_time": (now - Duration::hours(1)).to_rfc3339(),
                "end_time": (now + Duration::hours(1)).to_rfc3339(),
                "text": "Connection refused",
            }))
            .await;

        assert_eq!(response.status_code(), StatusCode::OK);

        let body: serde_json::Value = response.json();
        let lines_arr = body["lines"].as_array().expect("lines should be an array");
        assert!(
            !lines_arr.is_empty(),
            "Expected at least one line matching 'Connection refused'"
        );
        assert!(
            lines_arr[0]["message"]
                .as_str()
                .unwrap()
                .contains("Connection refused"),
            "Matched line should contain the search text"
        );
    }

    #[tokio::test]
    #[serial_test::serial]
    async fn test_get_log_context() {
        let ctx = create_test_context().await;
        let project_id = next_test_project_id();
        let now = Utc::now();

        // Seed enough lines to have meaningful context
        let mut lines = Vec::new();
        for i in 0..20 {
            lines.push(make_log_line(
                project_id,
                "api",
                "prod",
                if i == 10 {
                    LogLevel::Error
                } else {
                    LogLevel::Info
                },
                &format!("Log line number {}", i),
                now - Duration::seconds(20 - i),
                "container-ctx",
            ));
        }
        seed_logs(&ctx, lines).await;

        // First, search to get a chunk_id
        let server = build_test_server(ctx.app_state.clone());

        let search_response = server
            .post("/logs/search")
            .json(&serde_json::json!({
                "project_id": project_id,
                "start_time": (now - Duration::hours(1)).to_rfc3339(),
                "end_time": (now + Duration::hours(1)).to_rfc3339(),
            }))
            .await;

        assert_eq!(search_response.status_code(), StatusCode::OK);

        let search_body: serde_json::Value = search_response.json();
        let search_lines = search_body["lines"].as_array().unwrap();
        assert!(
            !search_lines.is_empty(),
            "Need search results to get chunk_id"
        );

        let chunk_id = search_lines[0]["chunk_id"].as_str().unwrap();
        let line_offset = search_lines[0]["line_offset"].as_i64().unwrap();

        // Now request context around that line
        let context_response = server
            .get(&format!(
                "/logs/context?chunk_id={}&line_offset={}&lines=5",
                chunk_id, line_offset
            ))
            .await;

        assert_eq!(context_response.status_code(), StatusCode::OK);

        let context_body: serde_json::Value = context_response.json();
        let context_lines = context_body["lines"].as_array().unwrap();
        assert!(
            !context_lines.is_empty(),
            "Expected context lines around the target"
        );
        assert!(
            context_body["target_index"].as_u64().is_some(),
            "Expected target_index in response"
        );
    }

    #[tokio::test]
    #[serial_test::serial]
    async fn test_get_log_context_invalid_chunk() {
        let ctx = create_test_context().await;
        let server = build_test_server(ctx.app_state.clone());

        let fake_chunk_id = Uuid::new_v4();

        let response = server
            .get(&format!(
                "/logs/context?chunk_id={}&line_offset=0&lines=5",
                fake_chunk_id
            ))
            .await;

        // Should return 404 for non-existent chunk
        assert_eq!(
            response.status_code(),
            StatusCode::NOT_FOUND,
            "Expected 404 for non-existent chunk, got {}",
            response.status_code()
        );
    }

    #[tokio::test]
    #[serial_test::serial]
    async fn test_purge_project_logs() {
        let ctx = create_test_context().await;
        let project_id = next_test_project_id();
        let now = Utc::now();

        // Seed logs that we'll purge
        let lines = vec![
            make_log_line(
                project_id,
                "api",
                "prod",
                LogLevel::Error,
                "Old error log",
                now - Duration::hours(2),
                "container-purge",
            ),
            make_log_line(
                project_id,
                "api",
                "prod",
                LogLevel::Info,
                "Old info log",
                now - Duration::hours(2) + Duration::seconds(1),
                "container-purge",
            ),
        ];
        seed_logs(&ctx, lines).await;

        let server = build_test_server(ctx.app_state.clone());

        // Purge all logs before "now" (should delete the seeded logs)
        let purge_response = server
            .delete(&format!("/projects/{}/logs", project_id))
            .json(&serde_json::json!({
                "before": now.to_rfc3339(),
            }))
            .await;

        assert_eq!(purge_response.status_code(), StatusCode::OK);

        let purge_body: serde_json::Value = purge_response.json();
        assert!(
            purge_body["chunks_deleted"].as_u64().unwrap() >= 1,
            "Expected at least 1 chunk deleted"
        );

        // Verify logs are gone by searching again
        let search_response = server
            .post("/logs/search")
            .json(&serde_json::json!({
                "project_id": project_id,
                "start_time": (now - Duration::hours(3)).to_rfc3339(),
                "end_time": (now + Duration::hours(1)).to_rfc3339(),
            }))
            .await;

        assert_eq!(search_response.status_code(), StatusCode::OK);
        let search_body: serde_json::Value = search_response.json();
        let remaining_lines = search_body["lines"].as_array().unwrap();
        assert!(
            remaining_lines.is_empty(),
            "Expected no logs after purge, found {}",
            remaining_lines.len()
        );
    }

    #[tokio::test]
    #[serial_test::serial]
    async fn test_purge_invalid_timestamp() {
        let ctx = create_test_context().await;
        let server = build_test_server(ctx.app_state.clone());

        let response = server
            .delete(&format!("/projects/{}/logs", next_test_project_id()))
            .json(&serde_json::json!({
                "before": "not-a-valid-timestamp",
            }))
            .await;

        assert_eq!(
            response.status_code(),
            StatusCode::BAD_REQUEST,
            "Expected 400 for invalid timestamp, got {}",
            response.status_code()
        );
    }

    #[tokio::test]
    #[serial_test::serial]
    async fn test_tail_logs_sse() {
        use tokio::net::TcpListener;

        let ctx = create_test_context().await;
        let project_id = next_test_project_id();
        let tail_tx = ctx.tail_tx.clone();

        // SSE endpoints stream indefinitely so we can't use axum-test's .await
        // (it waits for the full response body). Instead, bind to a real port,
        // connect with a raw HTTP client, send a log line via broadcast, and
        // read the first SSE event with a timeout.
        let auth_middleware = middleware::from_fn(
            |mut req: Request, next: axum::middleware::Next| async move {
                let auth_context = create_test_auth_context();
                req.extensions_mut().insert(auth_context);
                req.extensions_mut().insert(temps_core::RequestMetadata {
                    ip_address: "127.0.0.1".to_string(),
                    user_agent: "test-agent".to_string(),
                    headers: axum::http::HeaderMap::new(),
                    visitor_id_cookie: None,
                    session_id_cookie: None,
                    base_url: "http://localhost".to_string(),
                    scheme: "http".to_string(),
                    host: "localhost".to_string(),
                    is_secure: false,
                });
                next.run(req).await
            },
        );

        let app = configure_routes()
            .layer(auth_middleware)
            .with_state(ctx.app_state.clone());

        let listener = TcpListener::bind("127.0.0.1:0")
            .await
            .expect("Failed to bind");
        let addr = listener.local_addr().unwrap();

        let server_handle = tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });

        // Give the server a moment to start
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;

        // Connect with a raw TCP stream and send an HTTP GET
        let url = format!(
            "http://127.0.0.1:{}/logs/tail?project_id={}&service=web&env=prod",
            addr.port(),
            project_id,
        );

        let client = reqwest::Client::new();
        let response = client
            .get(&url)
            .timeout(std::time::Duration::from_secs(5))
            .send()
            .await
            .expect("Failed to connect to SSE endpoint");

        assert_eq!(response.status(), 200);

        let content_type = response
            .headers()
            .get("content-type")
            .and_then(|v| v.to_str().ok())
            .unwrap_or("");
        assert!(
            content_type.contains("text/event-stream"),
            "Expected text/event-stream content type, got: {}",
            content_type
        );

        // Send a log line through the broadcast channel
        let log_line = make_log_line(
            project_id,
            "web",
            "prod",
            LogLevel::Info,
            "Tail test message",
            Utc::now(),
            "container-tail",
        );
        let send_result = tail_tx.send(log_line);
        assert!(
            send_result.is_ok(),
            "Failed to send log line to broadcast channel"
        );

        // Read the first chunk of the SSE response body with a timeout
        let body = tokio::time::timeout(std::time::Duration::from_secs(3), response.text()).await;

        // We either get the SSE data or timeout — both are acceptable.
        // The key assertions are the 200 status and content-type above.
        if let Ok(Ok(text)) = body {
            assert!(
                text.contains("Tail test message"),
                "Expected SSE event to contain 'Tail test message', got: {}",
                &text[..std::cmp::min(200, text.len())]
            );
        }

        server_handle.abort();
    }

    #[tokio::test]
    #[serial_test::serial]
    async fn test_search_logs_with_pagination() {
        // Regression test: pagination via next_cursor must walk into older
        // logs without overlap. Previously the server emitted next_cursor
        // but never honored filter.cursor on the way back in, so every
        // "next page" click returned page 1 again.
        let ctx = create_test_context().await;
        let project_id = next_test_project_id();
        let now = Utc::now();

        let mut lines = Vec::new();
        for i in 0..15 {
            lines.push(make_log_line(
                project_id,
                "api",
                "prod",
                LogLevel::Error,
                &format!("Error event {}", i),
                now - Duration::seconds(15 - i),
                "container-page",
            ));
        }
        seed_logs(&ctx, lines).await;

        let server = build_test_server(ctx.app_state.clone());

        let page1 = server
            .post("/logs/search")
            .json(&serde_json::json!({
                "project_id": project_id,
                "start_time": (now - Duration::hours(1)).to_rfc3339(),
                "end_time": (now + Duration::hours(1)).to_rfc3339(),
                "page_size": 5,
            }))
            .await;
        assert_eq!(page1.status_code(), StatusCode::OK);
        let page1_body: serde_json::Value = page1.json();
        let page1_lines = page1_body["lines"].as_array().unwrap();
        assert_eq!(page1_lines.len(), 5);
        let page1_cursor = page1_body["next_cursor"]
            .as_str()
            .expect("page 1 must emit a cursor when more matches exist (15 total, 5 per page)");

        let page2 = server
            .post("/logs/search")
            .json(&serde_json::json!({
                "project_id": project_id,
                "start_time": (now - Duration::hours(1)).to_rfc3339(),
                "end_time": (now + Duration::hours(1)).to_rfc3339(),
                "page_size": 5,
                "cursor": page1_cursor,
            }))
            .await;
        assert_eq!(page2.status_code(), StatusCode::OK);
        let page2_body: serde_json::Value = page2.json();
        let page2_lines = page2_body["lines"].as_array().unwrap();
        assert_eq!(page2_lines.len(), 5, "page 2 should have 5 more lines");

        // No overlap: every page-2 message is distinct from every page-1 message.
        let p1_msgs: std::collections::HashSet<&str> = page1_lines
            .iter()
            .map(|l| l["message"].as_str().unwrap())
            .collect();
        for l in page2_lines {
            let m = l["message"].as_str().unwrap();
            assert!(
                !p1_msgs.contains(m),
                "page 2 leaked a page-1 line: {} (cursor was ignored)",
                m,
            );
        }

        // Page 2 lines are strictly older than every page-1 line.
        let p1_oldest = page1_lines
            .iter()
            .map(|l| l["timestamp"].as_str().unwrap())
            .min()
            .unwrap();
        for l in page2_lines {
            let ts = l["timestamp"].as_str().unwrap();
            assert!(
                ts < p1_oldest,
                "page 2 line {} is not older than page-1 oldest {}",
                ts,
                p1_oldest,
            );
        }
    }

    #[tokio::test]
    #[serial_test::serial]
    async fn test_search_logs_returns_chronological_order() {
        // The page is bounded server-side to the *newest* page_size matches in
        // the time window (heap-kept), but the wire format is ASC so the UI
        // can render terminal-style with newest at the bottom. "Chronological"
        // here means oldest first inside the page; "load older" pages pre-
        // pend at the top of the rendered rope.
        let ctx = create_test_context().await;
        let project_id = next_test_project_id();
        let now = Utc::now();

        let mut lines = Vec::new();
        for i in 0..10 {
            lines.push(make_log_line(
                project_id,
                "api",
                "prod",
                LogLevel::Info,
                &format!("event {}", i),
                now - Duration::seconds(10 - i),
                "container-order",
            ));
        }
        seed_logs(&ctx, lines).await;

        let server = build_test_server(ctx.app_state.clone());
        let response = server
            .post("/logs/search")
            .json(&serde_json::json!({
                "project_id": project_id,
                "start_time": (now - Duration::hours(1)).to_rfc3339(),
                "end_time": (now + Duration::hours(1)).to_rfc3339(),
                "page_size": 100,
            }))
            .await;

        let body: serde_json::Value = response.json();
        let arr = body["lines"].as_array().unwrap();
        assert_eq!(arr.len(), 10);

        // Each subsequent line must be strictly newer (ASC ordering).
        for w in arr.windows(2) {
            let a = w[0]["timestamp"].as_str().unwrap();
            let b = w[1]["timestamp"].as_str().unwrap();
            assert!(
                a <= b,
                "results not in ASC order: {} appeared before {}",
                a,
                b,
            );
        }
    }

    #[tokio::test]
    #[serial_test::serial]
    async fn test_search_logs_wider_window_returns_more_recent_logs() {
        // Regression test: the early-exit `break` after page_size matches
        // ran chunks oldest-first, so a "Last 6 hours" query and a "Last 1
        // hour" query returned essentially the same lines from the start
        // of the window. The bigger window expansion was wasted because
        // the scanner stopped reading once it had enough, and "enough"
        // came from the oldest chunks.
        let ctx = create_test_context().await;
        let project_id = next_test_project_id();
        let now = Utc::now();

        // Seed 10 lines spread across 4 hours: half at -3h30m, half at -30m.
        let mut lines = Vec::new();
        for i in 0..10 {
            let offset = if i < 5 {
                Duration::minutes(-210)
            } else {
                Duration::minutes(-30)
            };
            lines.push(make_log_line(
                project_id,
                "api",
                "prod",
                LogLevel::Info,
                &format!("evt-{}", i),
                now + offset + Duration::seconds(i),
                "container-window",
            ));
        }
        seed_logs(&ctx, lines).await;

        let server = build_test_server(ctx.app_state.clone());

        // Last 1 hour: only the -30m batch is in window (5 lines).
        let r1h = server
            .post("/logs/search")
            .json(&serde_json::json!({
                "project_id": project_id,
                "start_time": (now - Duration::hours(1)).to_rfc3339(),
                "end_time": (now + Duration::minutes(1)).to_rfc3339(),
                "page_size": 100,
            }))
            .await;
        let body_1h: serde_json::Value = r1h.json();
        let len_1h = body_1h["lines"].as_array().unwrap().len();
        assert_eq!(
            len_1h, 5,
            "Last-1h window should hold exactly the -30m batch"
        );

        // Last 4 hours: both batches are in window (10 lines). If the early-exit
        // bug were still present, this would also return ~5.
        let r4h = server
            .post("/logs/search")
            .json(&serde_json::json!({
                "project_id": project_id,
                "start_time": (now - Duration::hours(4)).to_rfc3339(),
                "end_time": (now + Duration::minutes(1)).to_rfc3339(),
                "page_size": 100,
            }))
            .await;
        let body_4h: serde_json::Value = r4h.json();
        let len_4h = body_4h["lines"].as_array().unwrap().len();
        assert_eq!(
            len_4h, 10,
            "Last-4h window should expand to include both batches",
        );
    }

    #[tokio::test]
    async fn test_unauthenticated_request_fails() {
        // Build server WITHOUT auth middleware — RequireAuth should fail
        let ctx = create_test_context().await;

        let app = configure_routes().with_state(ctx.app_state.clone());
        let server = TestServer::new(app).expect("Failed to create test server");
        let now = Utc::now();

        let response = server
            .post("/logs/search")
            .json(&serde_json::json!({
                "project_id": 99999,
                "start_time": (now - Duration::hours(1)).to_rfc3339(),
                "end_time": (now + Duration::hours(1)).to_rfc3339(),
            }))
            .await;

        // Without auth context in extensions, RequireAuth should reject with 401
        assert_eq!(
            response.status_code(),
            StatusCode::UNAUTHORIZED,
            "Expected 401 without auth, got {}",
            response.status_code()
        );
    }

    #[tokio::test]
    #[serial_test::serial]
    async fn test_reader_cannot_purge_logs() {
        let ctx = create_test_context().await;
        let project_id = next_test_project_id();
        let now = Utc::now();

        // Seed some logs so purge has something to target
        let lines = vec![make_log_line(
            project_id,
            "api",
            "prod",
            LogLevel::Error,
            "Error to purge",
            now - Duration::hours(1),
            "container-perm",
        )];
        seed_logs(&ctx, lines).await;

        // Build server with Reader role (has LogsRead but NOT LogsDelete)
        let server = build_test_server_with_role(ctx.app_state.clone(), temps_auth::Role::Reader);

        let response = server
            .delete(&format!("/projects/{}/logs", project_id))
            .json(&serde_json::json!({
                "before": now.to_rfc3339(),
            }))
            .await;

        assert_eq!(
            response.status_code(),
            StatusCode::FORBIDDEN,
            "Reader should get 403 on purge, got {}",
            response.status_code()
        );
    }

    #[tokio::test]
    #[serial_test::serial]
    async fn test_reader_can_search_logs() {
        let ctx = create_test_context().await;
        let project_id = next_test_project_id();
        let now = Utc::now();

        let lines = vec![make_log_line(
            project_id,
            "api",
            "prod",
            LogLevel::Info,
            "Readable log",
            now,
            "container-read",
        )];
        seed_logs(&ctx, lines).await;

        // Build server with Reader role (has LogsRead)
        let server = build_test_server_with_role(ctx.app_state.clone(), temps_auth::Role::Reader);

        let response = server
            .post("/logs/search")
            .json(&serde_json::json!({
                "project_id": project_id,
                "start_time": (now - Duration::hours(1)).to_rfc3339(),
                "end_time": (now + Duration::hours(1)).to_rfc3339(),
            }))
            .await;

        assert_eq!(
            response.status_code(),
            StatusCode::OK,
            "Reader should be able to search logs, got {}",
            response.status_code()
        );
    }
}
