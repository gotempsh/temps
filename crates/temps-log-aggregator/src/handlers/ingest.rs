// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! HTTP log ingest and its keys (ADR 108 D1).
//!
//! `POST /logs/ingest` is authenticated by a log ingest key, not by a user
//! session: it is called by collectors. The key admin routes are ordinary
//! console routes: listing needs `logs:read`, creating and revoking need
//! `logs:write`, and deployment tokens are refused outright (a `dt_` token
//! lives inside a deployed app and must not mint itself a credential that
//! writes into the project's logs). Every key write is audited, without the
//! plaintext.

use std::sync::Arc;

use axum::body::Bytes;
use axum::extract::{DefaultBodyLimit, FromRequest, Path, Query, Request, State};
use axum::http::{header, HeaderMap, HeaderValue, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Extension, Json, Router};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use temps_authz::{
    deny_deployment_token, permission_guard, project_access_guard, project_scope_guard, RequireAuth,
};
use temps_core::problemdetails::Problem;
use temps_core::ProblemDetails;
use temps_core::RequestMetadata;
use utoipa::{IntoParams, ToSchema};

use temps_entities::log_ingest_keys::Model as KeyModel;

use crate::error::LogAggregatorError;
use crate::handlers::types::LogAggregatorAppState;
use crate::ingest::ndjson::MAX_BODY_BYTES;
use crate::ingest::service::MAX_CONCURRENT_INGEST_REQUESTS;
use crate::ingest::{LogIngestService, NewLogIngestKey};

/// Longest an ingest request may take to deliver its body once it holds a
/// slot. A 5 MiB batch arrives in well under a second on any link a
/// collector ships logs over; a stalled upload must not keep one of the few
/// slots every collector shares.
pub const BODY_READ_DEADLINE: std::time::Duration = std::time::Duration::from_secs(20);

/// Header a collector may use instead of `Authorization: Bearer`.
pub const LOG_KEY_HEADER: &str = "x-temps-log-key";

// ── DTOs ────────────────────────────────────────────────────────────────

#[derive(Debug, Serialize, ToSchema)]
pub struct LogIngestResponse {
    /// Lines stored from this request.
    pub accepted: usize,
}

#[derive(Debug, Deserialize, IntoParams)]
pub struct ListLogIngestKeysParams {
    /// 1-based page. Default 1.
    pub page: Option<u64>,
    /// Default 20, maximum 100.
    pub page_size: Option<u64>,
}

#[derive(Debug, Deserialize, ToSchema)]
pub struct CreateLogIngestKeyRequest {
    /// A label for the collector or host using the key.
    pub name: String,
    /// Pin the key to one environment of the project. Absent: lines are
    /// labelled with the environment `external`.
    pub environment_id: Option<i32>,
    /// Service label for lines that do not name one. Default `external`.
    pub default_service: Option<String>,
}

#[derive(Debug, Serialize, ToSchema)]
pub struct LogIngestKeyResponse {
    pub id: i32,
    pub project_id: i32,
    pub environment_id: Option<i32>,
    pub name: String,
    /// The first characters of the key, for recognising it.
    pub key_prefix: String,
    pub default_service: String,
    pub is_active: bool,
    #[schema(value_type = Option<String>, format = DateTime)]
    pub revoked_at: Option<DateTime<Utc>>,
    /// Lines ingested with this key. Updated every 30 seconds.
    pub line_count: i64,
    #[schema(value_type = Option<String>, format = DateTime)]
    pub last_used_at: Option<DateTime<Utc>>,
    pub created_by_user_id: Option<i32>,
    #[schema(value_type = String, format = DateTime)]
    pub created_at: DateTime<Utc>,
}

impl From<KeyModel> for LogIngestKeyResponse {
    fn from(model: KeyModel) -> Self {
        Self {
            id: model.id,
            project_id: model.project_id,
            environment_id: model.environment_id,
            name: model.name,
            key_prefix: model.key_prefix,
            default_service: model.default_service,
            is_active: model.is_active,
            revoked_at: model.revoked_at,
            line_count: model.line_count,
            last_used_at: model.last_used_at,
            created_by_user_id: model.created_by_user_id,
            created_at: model.created_at,
        }
    }
}

/// A key just created: the only response that carries the plaintext.
#[derive(Debug, Serialize, ToSchema)]
pub struct CreatedLogIngestKeyResponse {
    #[serde(flatten)]
    pub ingest_key: LogIngestKeyResponse,
    /// The full key. Shown once; only its hash is stored.
    pub key: String,
}

#[derive(Debug, Serialize, ToSchema)]
pub struct LogIngestKeysResponse {
    pub data: Vec<LogIngestKeyResponse>,
    pub total: u64,
    pub page: u64,
    pub page_size: u64,
}

// ── Audit events ────────────────────────────────────────────────────────

macro_rules! key_audit {
    ($name:ident, $operation:literal) => {
        #[derive(Debug, Clone, serde::Serialize)]
        struct $name {
            context: temps_core::AuditContext,
            project_id: i32,
            key_id: i32,
            name: String,
            key_prefix: String,
        }

        impl temps_core::AuditOperation for $name {
            fn operation_type(&self) -> String {
                $operation.to_string()
            }
            fn user_id(&self) -> Option<i32> {
                Some(self.context.user_id)
            }
            fn ip_address(&self) -> Option<String> {
                self.context.ip_address.clone()
            }
            fn user_agent(&self) -> &str {
                &self.context.user_agent
            }
            fn serialize(&self) -> temps_core::AuditResult<String> {
                temps_core::serialize_audit_operation(self)
            }
        }
    };
}

key_audit!(LogIngestKeyCreatedAudit, "LOG_INGEST_KEY_CREATED");
key_audit!(LogIngestKeyRevokedAudit, "LOG_INGEST_KEY_REVOKED");

fn audit_context(
    auth: &temps_authz::AuthContext,
    metadata: &RequestMetadata,
) -> temps_core::AuditContext {
    temps_core::AuditContext {
        user_id: auth.user_id(),
        ip_address: Some(metadata.ip_address.clone()),
        user_agent: metadata.user_agent.clone(),
    }
}

// ── Routes ──────────────────────────────────────────────────────────────

/// The ingest receiver, mounted on the public router (ADR 051 D3).
///
/// A collector outside Temps posts here with its key, so the route must be
/// reachable where the admin console is not: behind the admin gate, or with
/// a separate public ingress listener. It authenticates the request by its
/// key and never reads a session. Key management stays on the admin router
/// ([`configure_routes`]).
pub fn configure_public_routes() -> Router<Arc<LogIngestService>> {
    Router::new().route(
        "/logs/ingest",
        // The body limit is the batch limit plus room for the framing;
        // the parser enforces the exact limit with a typed error.
        post(ingest_logs).layer(DefaultBodyLimit::max(MAX_BODY_BYTES + 1024)),
    )
}

/// Key management, on the admin router.
pub fn configure_routes() -> Router<Arc<LogAggregatorAppState>> {
    Router::new()
        .route(
            "/projects/{project_id}/logs/ingest-keys",
            get(list_log_ingest_keys).post(create_log_ingest_key),
        )
        .route(
            "/projects/{project_id}/logs/ingest-keys/{key_id}/revoke",
            post(revoke_log_ingest_key),
        )
}

/// The key from `Authorization: Bearer <key>` or `X-Temps-Log-Key`.
fn presented_key(headers: &HeaderMap) -> Option<&str> {
    if let Some(value) = headers
        .get(header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
    {
        let value = value.trim();
        if let Some(token) = value
            .strip_prefix("Bearer ")
            .or_else(|| value.strip_prefix("bearer "))
        {
            return Some(token.trim());
        }
    }
    headers
        .get(LOG_KEY_HEADER)
        .and_then(|v| v.to_str().ok())
        .map(str::trim)
}

// ── Handlers ────────────────────────────────────────────────────────────

/// Ingest newline-delimited JSON log lines with a log ingest key.
///
/// One JSON object per line; see the logs documentation for the keys read.
/// A batch is parsed and charged to the budget whole before anything is
/// stored, so a malformed or over-budget batch stores nothing. Storage is
/// at least once: if storing fails part-way (a full disk), the lines before
/// the failure are kept and counted against the key, the request fails, and
/// a collector that resends the batch stores those lines again.
#[utoipa::path(
    tag = "Logs",
    post,
    path = "/logs/ingest",
    request_body(content = String, content_type = "application/x-ndjson", description = "One JSON object per line"),
    responses(
        (status = 202, description = "Lines stored", body = LogIngestResponse),
        (status = 400, description = "A line is not a JSON object", body = ProblemDetails),
        (status = 401, description = "Missing or unknown key", body = ProblemDetails),
        (status = 408, description = "The body did not arrive in time", body = ProblemDetails),
        (status = 413, description = "Body too large", body = ProblemDetails),
        (status = 415, description = "Compressed body", body = ProblemDetails),
        (status = 429, description = "The project's log ingestion budget is exhausted", body = ProblemDetails),
        (status = 503, description = "Too many ingest requests in flight; retry after the Retry-After delay", body = ProblemDetails),
        (status = 500, description = "Internal server error", body = ProblemDetails)
    ),
    security(("bearer_auth" = []))
)]
pub async fn ingest_logs(
    State(ingest_service): State<Arc<LogIngestService>>,
    request: Request,
) -> Response {
    let headers = request.headers().clone();
    let Some(key) = presented_key(&headers) else {
        return Problem::from(LogAggregatorError::IngestKeyRejected).into_response();
    };
    if let Some(encoding) = headers
        .get(header::CONTENT_ENCODING)
        .and_then(|v| v.to_str().ok())
    {
        if !encoding.trim().eq_ignore_ascii_case("identity") {
            return temps_core::problemdetails::new(StatusCode::UNSUPPORTED_MEDIA_TYPE)
                .with_title("Compressed Log Ingest Body")
                .with_detail(format!(
                    "Content-Encoding '{encoding}' is not accepted; send the body uncompressed \
                     (in Vector set compression = \"none\")"
                ))
                .into_response();
        }
    }
    // The key is checked and a slot taken before the body is read, so a
    // caller without a key, or a burst of uploads, cannot make the receiver
    // buffer bodies without bound.
    if let Err(error) = ingest_service.check_key(key).await {
        return Problem::from(error).into_response();
    }
    let Some(_slot) = ingest_service.try_admit_request() else {
        let mut response = temps_core::problemdetails::new(StatusCode::SERVICE_UNAVAILABLE)
            .with_title("Log Ingest Busy")
            .with_detail(format!(
                "{MAX_CONCURRENT_INGEST_REQUESTS} log ingest requests are already being stored; \
                 retry the batch shortly"
            ))
            .into_response();
        response
            .headers_mut()
            .insert(header::RETRY_AFTER, HeaderValue::from_static("1"));
        return response;
    };
    // Reads the body under the route's `DefaultBodyLimit`, answering 413
    // past it exactly as the `Bytes` extractor would.
    // A deadline on the read, so an upload that stalls gives its slot back.
    let body =
        match tokio::time::timeout(BODY_READ_DEADLINE, Bytes::from_request(request, &())).await {
            Ok(Ok(body)) => body,
            Ok(Err(rejection)) => return rejection.into_response(),
            Err(_) => {
                return temps_core::problemdetails::new(StatusCode::REQUEST_TIMEOUT)
                    .with_title("Log Ingest Body Too Slow")
                    .with_detail(format!(
                        "The request body did not arrive within {}s; resend the batch",
                        BODY_READ_DEADLINE.as_secs()
                    ))
                    .into_response();
            }
        };
    match ingest_service.ingest(key, &body).await {
        Ok(outcome) => (
            StatusCode::ACCEPTED,
            Json(LogIngestResponse {
                accepted: outcome.accepted,
            }),
        )
            .into_response(),
        Err(error) => {
            let over_budget = matches!(error, LogAggregatorError::IngestBudgetExceeded { .. });
            let mut response = Problem::from(error).into_response();
            if over_budget {
                response
                    .headers_mut()
                    .insert(header::RETRY_AFTER, HeaderValue::from_static("5"));
            }
            response
        }
    }
}

/// List a project's log ingest keys. Never returns a key's plaintext.
#[utoipa::path(
    tag = "Logs",
    get,
    path = "/projects/{project_id}/logs/ingest-keys",
    params(("project_id" = i32, Path, description = "Project ID"), ListLogIngestKeysParams),
    responses(
        (status = 200, description = "Keys", body = LogIngestKeysResponse),
        (status = 401, description = "Unauthorized", body = ProblemDetails),
        (status = 403, description = "Insufficient permissions", body = ProblemDetails),
        (status = 500, description = "Internal server error", body = ProblemDetails)
    ),
    security(("bearer_auth" = []))
)]
pub async fn list_log_ingest_keys(
    RequireAuth(auth): RequireAuth,
    State(state): State<Arc<LogAggregatorAppState>>,
    Path(project_id): Path<i32>,
    Query(params): Query<ListLogIngestKeysParams>,
) -> Result<impl IntoResponse, Problem> {
    permission_guard!(auth, LogsRead);
    deny_deployment_token!(auth);
    project_scope_guard!(auth, project_id);
    project_access_guard!(auth, project_id, state.project_access_checker);

    let page = params.page.unwrap_or(1).max(1);
    let page_size = params.page_size.unwrap_or(20).clamp(1, 100);
    let (items, total) = state
        .ingest_key_service
        .list(project_id, page, page_size)
        .await?;
    Ok(Json(LogIngestKeysResponse {
        data: items.into_iter().map(LogIngestKeyResponse::from).collect(),
        total,
        page,
        page_size,
    }))
}

/// Create a log ingest key. The response is the only time the key is shown.
#[utoipa::path(
    tag = "Logs",
    post,
    path = "/projects/{project_id}/logs/ingest-keys",
    params(("project_id" = i32, Path, description = "Project ID")),
    request_body = CreateLogIngestKeyRequest,
    responses(
        (status = 201, description = "Key created; the plaintext is in `key`", body = CreatedLogIngestKeyResponse),
        (status = 400, description = "Validation error", body = ProblemDetails),
        (status = 401, description = "Unauthorized", body = ProblemDetails),
        (status = 403, description = "Insufficient permissions", body = ProblemDetails),
        (status = 404, description = "Project or environment not found", body = ProblemDetails),
        (status = 500, description = "Internal server error", body = ProblemDetails)
    ),
    security(("bearer_auth" = []))
)]
pub async fn create_log_ingest_key(
    RequireAuth(auth): RequireAuth,
    State(state): State<Arc<LogAggregatorAppState>>,
    Extension(metadata): Extension<RequestMetadata>,
    Path(project_id): Path<i32>,
    Json(request): Json<CreateLogIngestKeyRequest>,
) -> Result<impl IntoResponse, Problem> {
    permission_guard!(auth, LogsWrite);
    deny_deployment_token!(auth);
    project_scope_guard!(auth, project_id);
    project_access_guard!(auth, project_id, state.project_access_checker);

    let created = state
        .ingest_key_service
        .create(
            project_id,
            NewLogIngestKey {
                name: request.name,
                environment_id: request.environment_id,
                default_service: request.default_service,
                created_by: auth.user_id_opt(),
            },
        )
        .await?;

    let audit = LogIngestKeyCreatedAudit {
        context: audit_context(&auth, &metadata),
        project_id,
        key_id: created.key.id,
        name: created.key.name.clone(),
        key_prefix: created.key.key_prefix.clone(),
    };
    if let Err(e) = state.audit_service.create_audit_log(&audit).await {
        tracing::error!(error = %e, "Failed to audit a log ingest key creation");
    }

    let mut response = (
        StatusCode::CREATED,
        Json(CreatedLogIngestKeyResponse {
            key: created.plaintext,
            ingest_key: LogIngestKeyResponse::from(created.key),
        }),
    )
        .into_response();
    response
        .headers_mut()
        .insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    Ok(response)
}

/// Revoke a log ingest key. Lines it already sent stay.
#[utoipa::path(
    tag = "Logs",
    post,
    path = "/projects/{project_id}/logs/ingest-keys/{key_id}/revoke",
    params(
        ("project_id" = i32, Path, description = "Project ID"),
        ("key_id" = i32, Path, description = "Log ingest key ID")
    ),
    responses(
        (status = 200, description = "Key revoked", body = LogIngestKeyResponse),
        (status = 401, description = "Unauthorized", body = ProblemDetails),
        (status = 403, description = "Insufficient permissions", body = ProblemDetails),
        (status = 404, description = "Key not found", body = ProblemDetails),
        (status = 500, description = "Internal server error", body = ProblemDetails)
    ),
    security(("bearer_auth" = []))
)]
pub async fn revoke_log_ingest_key(
    RequireAuth(auth): RequireAuth,
    State(state): State<Arc<LogAggregatorAppState>>,
    Extension(metadata): Extension<RequestMetadata>,
    Path((project_id, key_id)): Path<(i32, i32)>,
) -> Result<impl IntoResponse, Problem> {
    permission_guard!(auth, LogsWrite);
    deny_deployment_token!(auth);
    project_scope_guard!(auth, project_id);
    project_access_guard!(auth, project_id, state.project_access_checker);

    let model = state.ingest_key_service.revoke(project_id, key_id).await?;
    // The key is already refused; closing its streams frees the head
    // buffers and WAL files it held. A failure here leaves them to be
    // sealed by age and does not undo the revocation.
    match state.ingest_service.close_key(key_id).await {
        Ok(streams) => tracing::debug!(
            project_id,
            key_id,
            streams,
            "Closed a revoked log ingest key's streams"
        ),
        Err(e) => {
            tracing::warn!(project_id, key_id, error = %e, "Failed to close a revoked log ingest key's streams")
        }
    }

    let audit = LogIngestKeyRevokedAudit {
        context: audit_context(&auth, &metadata),
        project_id,
        key_id,
        name: model.name.clone(),
        key_prefix: model.key_prefix.clone(),
    };
    if let Err(e) = state.audit_service.create_audit_log(&audit).await {
        tracing::error!(error = %e, "Failed to audit a log ingest key revocation");
    }
    Ok(Json(LogIngestKeyResponse::from(model)))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ingest::keys::AuthenticatedKey;
    use crate::ingest::service::{IngestKeyResolver, IngestLineWriter};
    use crate::services::{IngestBudget, IngestLimits};

    struct OneKey;

    #[async_trait::async_trait]
    impl IngestKeyResolver for OneKey {
        async fn resolve(&self, presented: &str) -> Result<AuthenticatedKey, LogAggregatorError> {
            if presented == "good" {
                Ok(AuthenticatedKey {
                    key_id: 1,
                    project_id: 2,
                    environment_id: None,
                    default_service: "edge".to_string(),
                })
            } else {
                Err(LogAggregatorError::IngestKeyRejected)
            }
        }
        fn record_usage(&self, _key_id: i32, _lines: u64, _at: DateTime<Utc>) {}
    }

    struct Discard;

    #[async_trait::async_trait]
    impl IngestLineWriter for Discard {
        async fn write(&self, _line: crate::types::LogLine) -> Result<(), LogAggregatorError> {
            Ok(())
        }
        async fn close_streams(&self, _prefix: &str) -> Result<usize, LogAggregatorError> {
            Ok(0)
        }
    }

    fn service() -> Arc<LogIngestService> {
        Arc::new(LogIngestService::new(
            Arc::new(OneKey),
            Arc::new(Discard),
            Arc::new(IngestBudget::new(IngestLimits::UNLIMITED)),
        ))
    }

    fn stalled_upload(key: &str) -> Request {
        let body = axum::body::Body::from_stream(futures::stream::pending::<
            Result<Bytes, std::io::Error>,
        >());
        axum::http::Request::builder()
            .method("POST")
            .uri("/logs/ingest")
            .header(header::AUTHORIZATION, format!("Bearer {key}"))
            .body(body)
            .expect("request")
    }

    #[tokio::test(start_paused = true)]
    async fn a_stalled_upload_times_out_and_gives_its_slot_back() {
        let service = service();
        let response = ingest_logs(State(service.clone()), stalled_upload("good")).await;
        assert_eq!(response.status(), StatusCode::REQUEST_TIMEOUT);
        let free: Vec<_> = (0..MAX_CONCURRENT_INGEST_REQUESTS)
            .map(|_| service.try_admit_request())
            .collect();
        assert!(free.iter().all(Option::is_some), "every slot is free again");
    }

    #[tokio::test(start_paused = true)]
    async fn an_unknown_key_is_refused_without_reading_the_body() {
        // The body never arrives; only a refusal that skips the read returns.
        let response = ingest_logs(State(service()), stalled_upload("bad")).await;
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    }
}
