//! OTLP/HTTP ingest handlers.
//!
//! Accept protobuf-encoded payloads with gzip/zstd compression.
//! Authenticate via per-project API key in the `Authorization` header.
//! Return correct OTLP response envelopes.

use axum::body::Bytes;
use axum::extract::{Path, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::IntoResponse;
use prost::Message;
use tracing::{debug, error, warn};

use crate::error::OtelError;
use crate::ingest::decode;
use crate::proto;
use crate::OtelAppState;
use temps_core::problemdetails::{self, Problem};
use temps_core::ProblemDetails;

impl From<OtelError> for Problem {
    fn from(error: OtelError) -> Self {
        match error {
            OtelError::AuthFailed { .. } | OtelError::InvalidApiKey => {
                warn!(error = %error, "OTel ingest auth failed");
                problemdetails::new(StatusCode::UNAUTHORIZED)
                    .with_title("Authentication Failed")
                    .with_detail(error.to_string())
            }
            OtelError::RateLimitExceeded { .. } => {
                warn!(error = %error, "OTel ingest rate limited");
                problemdetails::new(StatusCode::TOO_MANY_REQUESTS)
                    .with_title("Rate Limit Exceeded")
                    .with_detail(error.to_string())
            }
            OtelError::QuotaExceeded { .. } => {
                warn!(error = %error, "OTel ingest quota exceeded");
                problemdetails::new(StatusCode::PAYLOAD_TOO_LARGE)
                    .with_title("Storage Quota Exceeded")
                    .with_detail(error.to_string())
            }
            OtelError::ProtobufDecode { .. } | OtelError::Validation { .. } => {
                warn!(error = %error, "OTel ingest bad payload");
                problemdetails::new(StatusCode::BAD_REQUEST)
                    .with_title("Invalid Payload")
                    .with_detail(error.to_string())
            }
            OtelError::DecompressionFailed { .. } | OtelError::UnsupportedEncoding { .. } => {
                warn!(error = %error, "OTel ingest decompression error");
                problemdetails::new(StatusCode::BAD_REQUEST)
                    .with_title("Decompression Error")
                    .with_detail(error.to_string())
            }
            OtelError::ProjectNotFound { .. } => {
                warn!(error = %error, "OTel ingest project not found");
                problemdetails::new(StatusCode::NOT_FOUND)
                    .with_title("Project Not Found")
                    .with_detail(error.to_string())
            }
            OtelError::Storage { .. }
            | OtelError::Database(_)
            | OtelError::S3 { .. }
            | OtelError::Io(_)
            | OtelError::Serialization(_)
            | OtelError::Internal { .. } => {
                error!(error = %error, "OTel ingest internal error");
                problemdetails::new(StatusCode::INTERNAL_SERVER_ERROR)
                    .with_title("Internal Server Error")
                    .with_detail(error.to_string())
            }
        }
    }
}

/// Extract authentication token from headers.
///
/// Checks `Authorization: Bearer <token>` and `X-Temps-Api-Key: <token>`.
/// Works for both `tk_` (API key) and `dt_` (deployment token) prefixes.
fn extract_token(headers: &HeaderMap) -> Option<String> {
    // Check Authorization: Bearer <token>
    if let Some(auth) = headers.get("authorization") {
        if let Ok(value) = auth.to_str() {
            if let Some(key) = value.strip_prefix("Bearer ") {
                return Some(key.trim().to_string());
            }
        }
    }

    // Check X-Temps-Api-Key: <token>
    if let Some(key) = headers.get("x-temps-api-key") {
        if let Ok(value) = key.to_str() {
            return Some(value.trim().to_string());
        }
    }

    None
}

/// Extract optional project ID from `X-Temps-Project-Id` header.
///
/// Required for `tk_` (API key) tokens; ignored for `dt_` tokens.
fn extract_project_id_header(headers: &HeaderMap) -> Option<i32> {
    headers
        .get("x-temps-project-id")
        .and_then(|v| v.to_str().ok())
        .and_then(|s| s.parse::<i32>().ok())
}

/// Extract Content-Encoding from headers.
fn content_encoding(headers: &HeaderMap) -> Option<&str> {
    headers
        .get("content-encoding")
        .and_then(|v| v.to_str().ok())
}

// ── Shared ingest logic ─────────────────────────────────────────────

/// Resolved context for an ingest request: who authenticated and which
/// project/environment/deployment the telemetry belongs to.
struct IngestContext {
    project_id: i32,
    environment_id: Option<i32>,
    deployment_id: Option<i32>,
}

/// Authenticate the request and resolve the ingest context.
///
/// For header-only requests the IDs come from the `ProjectAuth` returned
/// by `authenticate()`.  For path-based requests the path IDs take
/// precedence — if a `dt_` token is used, we validate that the path IDs
/// match the token record.
async fn resolve_ingest_context(
    state: &OtelAppState,
    token: &str,
    path_ids: Option<(i32, i32, i32)>,
    headers: &HeaderMap,
) -> Result<IngestContext, OtelError> {
    let header_project_id = extract_project_id_header(headers);

    // When path IDs are provided, use path project_id as the header_project_id
    // so `authenticate_api_key` can verify access.
    let effective_project_id = path_ids.map(|(pid, _, _)| pid).or(header_project_id);

    let auth = state
        .otel_service
        .authenticate(token, effective_project_id)
        .await?;

    match path_ids {
        Some((path_project_id, path_environment_id, path_deployment_id)) => {
            // Validate: if the token already binds to a project (dt_ tokens),
            // the path project_id must match.
            if auth.project_id != path_project_id {
                return Err(OtelError::AuthFailed {
                    reason: format!(
                        "Token is bound to project {} but path specifies project {}",
                        auth.project_id, path_project_id
                    ),
                });
            }

            // For dt_ tokens, also validate environment_id if the token has one.
            if let Some(token_env) = auth.environment_id {
                if token_env != path_environment_id {
                    return Err(OtelError::AuthFailed {
                        reason: format!(
                            "Token is bound to environment {} but path specifies environment {}",
                            token_env, path_environment_id
                        ),
                    });
                }
            }

            Ok(IngestContext {
                project_id: path_project_id,
                environment_id: Some(path_environment_id),
                deployment_id: Some(path_deployment_id),
            })
        }
        None => Ok(IngestContext {
            project_id: auth.project_id,
            environment_id: auth.environment_id,
            deployment_id: auth.deployment_id,
        }),
    }
}

/// Core metrics ingest: authenticate, decompress, decode, store.
async fn do_ingest_metrics(
    state: &OtelAppState,
    token: &str,
    path_ids: Option<(i32, i32, i32)>,
    headers: &HeaderMap,
    body: &Bytes,
) -> Result<(StatusCode, [(&'static str, &'static str); 1], Vec<u8>), Problem> {
    let ctx = resolve_ingest_context(state, token, path_ids, headers).await?;
    state.otel_service.check_rate_limit(ctx.project_id)?;
    state.otel_service.check_quota(ctx.project_id).await?;

    let data = decode::decompress(body, content_encoding(headers))?;
    let points = decode::decode_metrics_request(&data, ctx.project_id, ctx.deployment_id)?;
    let count = points.len();

    for point in &points {
        debug!(
            project_id = ctx.project_id,
            metric_name = %point.metric_name,
            metric_type = %point.metric_type,
            value = point.value,
            service = %point.resource.service_name,
            "New metric received"
        );
    }

    let stored = state.otel_service.ingest_metrics(points).await?;

    debug!(
        project_id = ctx.project_id,
        environment_id = ?ctx.environment_id,
        deployment_id = ?ctx.deployment_id,
        received = count,
        stored,
        "Ingested metrics batch"
    );

    let response = proto::collector::metrics::v1::ExportMetricsServiceResponse {
        partial_success: None,
    };
    Ok((
        StatusCode::OK,
        [("content-type", "application/x-protobuf")],
        response.encode_to_vec(),
    ))
}

/// Core traces ingest: authenticate, decompress, decode, store.
async fn do_ingest_traces(
    state: &OtelAppState,
    token: &str,
    path_ids: Option<(i32, i32, i32)>,
    headers: &HeaderMap,
    body: &Bytes,
) -> Result<(StatusCode, [(&'static str, &'static str); 1], Vec<u8>), Problem> {
    let ctx = resolve_ingest_context(state, token, path_ids, headers).await?;
    state.otel_service.check_rate_limit(ctx.project_id)?;
    state.otel_service.check_quota(ctx.project_id).await?;

    let data = decode::decompress(body, content_encoding(headers))?;
    let spans = decode::decode_traces_request(&data, ctx.project_id, ctx.deployment_id)?;
    let count = spans.len();

    // Log each span at debug level so operators can see what arrived
    for span in &spans {
        debug!(
            project_id = ctx.project_id,
            trace_id = %span.trace_id,
            span_id = %span.span_id,
            parent_span_id = ?span.parent_span_id,
            name = %span.name,
            kind = %span.kind,
            status = %span.status_code,
            duration_ms = span.duration_ms,
            service = %span.resource.service_name,
            "New span received"
        );
    }

    let stored = state.otel_service.ingest_spans(spans).await?;

    debug!(
        project_id = ctx.project_id,
        environment_id = ?ctx.environment_id,
        deployment_id = ?ctx.deployment_id,
        received = count,
        stored,
        "Ingested traces batch"
    );

    let response = proto::collector::trace::v1::ExportTraceServiceResponse {
        partial_success: None,
    };
    Ok((
        StatusCode::OK,
        [("content-type", "application/x-protobuf")],
        response.encode_to_vec(),
    ))
}

/// Core logs ingest: authenticate, decompress, decode, store.
async fn do_ingest_logs(
    state: &OtelAppState,
    token: &str,
    path_ids: Option<(i32, i32, i32)>,
    headers: &HeaderMap,
    body: &Bytes,
) -> Result<(StatusCode, [(&'static str, &'static str); 1], Vec<u8>), Problem> {
    let ctx = resolve_ingest_context(state, token, path_ids, headers).await?;
    state.otel_service.check_rate_limit(ctx.project_id)?;
    state.otel_service.check_quota(ctx.project_id).await?;

    let data = decode::decompress(body, content_encoding(headers))?;
    let records = decode::decode_logs_request(&data, ctx.project_id, ctx.deployment_id)?;
    let count = records.len();

    for record in &records {
        debug!(
            project_id = ctx.project_id,
            severity = %record.severity,
            body = %record.body,
            service = %record.resource.service_name,
            "New log received"
        );
    }

    let stored = state.otel_service.ingest_logs(records).await?;

    debug!(
        project_id = ctx.project_id,
        environment_id = ?ctx.environment_id,
        deployment_id = ?ctx.deployment_id,
        received = count,
        stored,
        "Ingested logs"
    );

    let response = proto::collector::logs::v1::ExportLogsServiceResponse {
        partial_success: None,
    };
    Ok((
        StatusCode::OK,
        [("content-type", "application/x-protobuf")],
        response.encode_to_vec(),
    ))
}

// ── Header-based ingest handlers ────────────────────────────────────

/// Ingest metrics via OTLP/HTTP protobuf.
///
/// Authenticates via API key in header, decompresses, decodes protobuf,
/// checks rate limit and storage quota, then stores.
#[utoipa::path(
    tag = "OTel Ingest",
    post,
    path = "/otel/v1/metrics",
    request_body(content = String, content_type = "application/x-protobuf", description = "OTLP ExportMetricsServiceRequest (protobuf, optionally gzip/zstd compressed)"),
    responses(
        (status = 200, description = "Metrics accepted (OTLP protobuf response)"),
        (status = 400, description = "Invalid payload", body = ProblemDetails),
        (status = 401, description = "Missing or invalid API key", body = ProblemDetails),
        (status = 413, description = "Storage quota exceeded", body = ProblemDetails),
        (status = 429, description = "Rate limit exceeded", body = ProblemDetails),
        (status = 500, description = "Internal server error", body = ProblemDetails),
    ),
    security(("api_key" = []))
)]
pub async fn ingest_metrics(
    State(state): State<OtelAppState>,
    headers: HeaderMap,
    body: Bytes,
) -> Result<impl IntoResponse, Problem> {
    let token = extract_token(&headers).ok_or_else(|| OtelError::AuthFailed {
        reason: "Missing token in Authorization or X-Temps-Api-Key header".into(),
    })?;
    let result = do_ingest_metrics(&state, &token, None, &headers, &body).await;
    if let Err(ref e) = result {
        error!(error = ?e, "Failed to ingest metrics (header auth)");
    }
    result
}

/// Ingest trace spans via OTLP/HTTP protobuf.
///
/// Authenticates via API key in header, decompresses, decodes protobuf,
/// checks rate limit and storage quota, then stores spans.
#[utoipa::path(
    tag = "OTel Ingest",
    post,
    path = "/otel/v1/traces",
    request_body(content = String, content_type = "application/x-protobuf", description = "OTLP ExportTraceServiceRequest (protobuf, optionally gzip/zstd compressed)"),
    responses(
        (status = 200, description = "Traces accepted (OTLP protobuf response)"),
        (status = 400, description = "Invalid payload", body = ProblemDetails),
        (status = 401, description = "Missing or invalid API key", body = ProblemDetails),
        (status = 413, description = "Storage quota exceeded", body = ProblemDetails),
        (status = 429, description = "Rate limit exceeded", body = ProblemDetails),
        (status = 500, description = "Internal server error", body = ProblemDetails),
    ),
    security(("api_key" = []))
)]
pub async fn ingest_traces(
    State(state): State<OtelAppState>,
    headers: HeaderMap,
    body: Bytes,
) -> Result<impl IntoResponse, Problem> {
    let token = extract_token(&headers).ok_or_else(|| OtelError::AuthFailed {
        reason: "Missing token in Authorization or X-Temps-Api-Key header".into(),
    })?;
    let result = do_ingest_traces(&state, &token, None, &headers, &body).await;
    if let Err(ref e) = result {
        error!(error = ?e, "Failed to ingest traces (header auth)");
    }
    result
}

/// Ingest log records via OTLP/HTTP protobuf.
///
/// Authenticates via API key in header, decompresses, decodes protobuf,
/// checks rate limit and storage quota, routes high-severity logs
/// to DB and all logs to S3.
#[utoipa::path(
    tag = "OTel Ingest",
    post,
    path = "/otel/v1/logs",
    request_body(content = String, content_type = "application/x-protobuf", description = "OTLP ExportLogsServiceRequest (protobuf, optionally gzip/zstd compressed)"),
    responses(
        (status = 200, description = "Logs accepted (OTLP protobuf response)"),
        (status = 400, description = "Invalid payload", body = ProblemDetails),
        (status = 401, description = "Missing or invalid API key", body = ProblemDetails),
        (status = 413, description = "Storage quota exceeded", body = ProblemDetails),
        (status = 429, description = "Rate limit exceeded", body = ProblemDetails),
        (status = 500, description = "Internal server error", body = ProblemDetails),
    ),
    security(("api_key" = []))
)]
pub async fn ingest_logs(
    State(state): State<OtelAppState>,
    headers: HeaderMap,
    body: Bytes,
) -> Result<impl IntoResponse, Problem> {
    let token = extract_token(&headers).ok_or_else(|| OtelError::AuthFailed {
        reason: "Missing token in Authorization or X-Temps-Api-Key header".into(),
    })?;
    let result = do_ingest_logs(&state, &token, None, &headers, &body).await;
    if let Err(ref e) = result {
        error!(error = ?e, "Failed to ingest logs (header auth)");
    }
    result
}

// ── Path-based ingest handlers ──────────────────────────────────────
//
// These accept project_id, environment_id, and deployment_id as path
// parameters, so the OTLP exporter endpoint becomes:
//
//   https://host/api/otel/v1/{project_id}/{environment_id}/{deployment_id}
//
// The SDK automatically appends /traces, /metrics, /logs.
// Authentication still uses the Authorization header (Bearer tk_/dt_).

/// Path parameters for path-based ingest: (project_id, environment_id, deployment_id).
type IngestPathParams = (i32, i32, i32);

/// Ingest metrics with project/environment/deployment in the URL path.
#[utoipa::path(
    tag = "OTel Ingest",
    post,
    path = "/otel/v1/{project_id}/{environment_id}/{deployment_id}/metrics",
    params(
        ("project_id" = i32, Path, description = "Project ID"),
        ("environment_id" = i32, Path, description = "Environment ID"),
        ("deployment_id" = i32, Path, description = "Deployment ID"),
    ),
    request_body(content = String, content_type = "application/x-protobuf", description = "OTLP ExportMetricsServiceRequest (protobuf, optionally gzip/zstd compressed)"),
    responses(
        (status = 200, description = "Metrics accepted (OTLP protobuf response)"),
        (status = 400, description = "Invalid payload", body = ProblemDetails),
        (status = 401, description = "Missing or invalid API key", body = ProblemDetails),
        (status = 413, description = "Storage quota exceeded", body = ProblemDetails),
        (status = 429, description = "Rate limit exceeded", body = ProblemDetails),
        (status = 500, description = "Internal server error", body = ProblemDetails),
    ),
    security(("api_key" = []))
)]
pub async fn ingest_metrics_by_path(
    State(state): State<OtelAppState>,
    Path(path_ids): Path<IngestPathParams>,
    headers: HeaderMap,
    body: Bytes,
) -> Result<impl IntoResponse, Problem> {
    let token = extract_token(&headers).ok_or_else(|| OtelError::AuthFailed {
        reason: "Missing token in Authorization or X-Temps-Api-Key header".into(),
    })?;
    let result = do_ingest_metrics(&state, &token, Some(path_ids), &headers, &body).await;
    if let Err(ref e) = result {
        error!(error = ?e, "Failed to ingest metrics (path auth)");
    }
    result
}

/// Ingest trace spans with project/environment/deployment in the URL path.
#[utoipa::path(
    tag = "OTel Ingest",
    post,
    path = "/otel/v1/{project_id}/{environment_id}/{deployment_id}/traces",
    params(
        ("project_id" = i32, Path, description = "Project ID"),
        ("environment_id" = i32, Path, description = "Environment ID"),
        ("deployment_id" = i32, Path, description = "Deployment ID"),
    ),
    request_body(content = String, content_type = "application/x-protobuf", description = "OTLP ExportTraceServiceRequest (protobuf, optionally gzip/zstd compressed)"),
    responses(
        (status = 200, description = "Traces accepted (OTLP protobuf response)"),
        (status = 400, description = "Invalid payload", body = ProblemDetails),
        (status = 401, description = "Missing or invalid API key", body = ProblemDetails),
        (status = 413, description = "Storage quota exceeded", body = ProblemDetails),
        (status = 429, description = "Rate limit exceeded", body = ProblemDetails),
        (status = 500, description = "Internal server error", body = ProblemDetails),
    ),
    security(("api_key" = []))
)]
pub async fn ingest_traces_by_path(
    State(state): State<OtelAppState>,
    Path(path_ids): Path<IngestPathParams>,
    headers: HeaderMap,
    body: Bytes,
) -> Result<impl IntoResponse, Problem> {
    let token = extract_token(&headers).ok_or_else(|| OtelError::AuthFailed {
        reason: "Missing token in Authorization or X-Temps-Api-Key header".into(),
    })?;
    let result = do_ingest_traces(&state, &token, Some(path_ids), &headers, &body).await;
    if let Err(ref e) = result {
        error!(error = ?e, "Failed to ingest traces (path auth)");
    }
    result
}

/// Ingest log records with project/environment/deployment in the URL path.
#[utoipa::path(
    tag = "OTel Ingest",
    post,
    path = "/otel/v1/{project_id}/{environment_id}/{deployment_id}/logs",
    params(
        ("project_id" = i32, Path, description = "Project ID"),
        ("environment_id" = i32, Path, description = "Environment ID"),
        ("deployment_id" = i32, Path, description = "Deployment ID"),
    ),
    request_body(content = String, content_type = "application/x-protobuf", description = "OTLP ExportLogsServiceRequest (protobuf, optionally gzip/zstd compressed)"),
    responses(
        (status = 200, description = "Logs accepted (OTLP protobuf response)"),
        (status = 400, description = "Invalid payload", body = ProblemDetails),
        (status = 401, description = "Missing or invalid API key", body = ProblemDetails),
        (status = 413, description = "Storage quota exceeded", body = ProblemDetails),
        (status = 429, description = "Rate limit exceeded", body = ProblemDetails),
        (status = 500, description = "Internal server error", body = ProblemDetails),
    ),
    security(("api_key" = []))
)]
pub async fn ingest_logs_by_path(
    State(state): State<OtelAppState>,
    Path(path_ids): Path<IngestPathParams>,
    headers: HeaderMap,
    body: Bytes,
) -> Result<impl IntoResponse, Problem> {
    let token = extract_token(&headers).ok_or_else(|| OtelError::AuthFailed {
        reason: "Missing token in Authorization or X-Temps-Api-Key header".into(),
    })?;
    let result = do_ingest_logs(&state, &token, Some(path_ids), &headers, &body).await;
    if let Err(ref e) = result {
        error!(error = ?e, "Failed to ingest logs (path auth)");
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::http::HeaderMap;

    // ── extract_token tests ──────────────────────────────────────

    #[test]
    fn test_extract_token_bearer() {
        let mut headers = HeaderMap::new();
        headers.insert("authorization", "Bearer tk_abc123".parse().unwrap());
        assert_eq!(extract_token(&headers), Some("tk_abc123".to_string()));
    }

    #[test]
    fn test_extract_token_deployment_token_bearer() {
        let mut headers = HeaderMap::new();
        headers.insert("authorization", "Bearer dt_abc123".parse().unwrap());
        assert_eq!(extract_token(&headers), Some("dt_abc123".to_string()));
    }

    #[test]
    fn test_extract_token_custom_header() {
        let mut headers = HeaderMap::new();
        headers.insert("x-temps-api-key", "tk_xyz789".parse().unwrap());
        assert_eq!(extract_token(&headers), Some("tk_xyz789".to_string()));
    }

    #[test]
    fn test_extract_token_missing() {
        let headers = HeaderMap::new();
        assert_eq!(extract_token(&headers), None);
    }

    #[test]
    fn test_extract_token_bearer_takes_priority() {
        let mut headers = HeaderMap::new();
        headers.insert("authorization", "Bearer tk_first".parse().unwrap());
        headers.insert("x-temps-api-key", "tk_second".parse().unwrap());
        assert_eq!(extract_token(&headers), Some("tk_first".to_string()));
    }

    #[test]
    fn test_extract_token_non_bearer_auth_falls_through() {
        let mut headers = HeaderMap::new();
        headers.insert("authorization", "Basic dXNlcjpwYXNz".parse().unwrap());
        headers.insert("x-temps-api-key", "tk_fallback".parse().unwrap());
        assert_eq!(extract_token(&headers), Some("tk_fallback".to_string()));
    }

    #[test]
    fn test_extract_token_bearer_trimmed() {
        let mut headers = HeaderMap::new();
        headers.insert("authorization", "Bearer  tk_spaces  ".parse().unwrap());
        assert_eq!(extract_token(&headers), Some("tk_spaces".to_string()));
    }

    // ── extract_project_id_header tests ────────────────────────────

    #[test]
    fn test_extract_project_id_header_present() {
        let mut headers = HeaderMap::new();
        headers.insert("x-temps-project-id", "42".parse().unwrap());
        assert_eq!(extract_project_id_header(&headers), Some(42));
    }

    #[test]
    fn test_extract_project_id_header_missing() {
        let headers = HeaderMap::new();
        assert_eq!(extract_project_id_header(&headers), None);
    }

    #[test]
    fn test_extract_project_id_header_invalid() {
        let mut headers = HeaderMap::new();
        headers.insert("x-temps-project-id", "not-a-number".parse().unwrap());
        assert_eq!(extract_project_id_header(&headers), None);
    }

    // ── content_encoding tests ─────────────────────────────────────

    #[test]
    fn test_content_encoding_present() {
        let mut headers = HeaderMap::new();
        headers.insert("content-encoding", "gzip".parse().unwrap());
        assert_eq!(content_encoding(&headers), Some("gzip"));
    }

    #[test]
    fn test_content_encoding_absent() {
        let headers = HeaderMap::new();
        assert_eq!(content_encoding(&headers), None);
    }

    // ── From<OtelError> for Problem tests ──────────────────────────

    #[test]
    fn test_error_auth_failed_maps_to_401() {
        let err = OtelError::AuthFailed {
            reason: "bad key".into(),
        };
        let problem: Problem = err.into();
        assert_eq!(problem.status_code, StatusCode::UNAUTHORIZED);
    }

    #[test]
    fn test_error_invalid_api_key_maps_to_401() {
        let err = OtelError::InvalidApiKey;
        let problem: Problem = err.into();
        assert_eq!(problem.status_code, StatusCode::UNAUTHORIZED);
    }

    #[test]
    fn test_error_rate_limit_maps_to_429() {
        let err = OtelError::RateLimitExceeded {
            project_id: 1,
            limit: 1000,
        };
        let problem: Problem = err.into();
        assert_eq!(problem.status_code, StatusCode::TOO_MANY_REQUESTS);
    }

    #[test]
    fn test_error_quota_exceeded_maps_to_413() {
        let err = OtelError::QuotaExceeded {
            project_id: 1,
            used_bytes: 100,
            limit_bytes: 50,
        };
        let problem: Problem = err.into();
        assert_eq!(problem.status_code, StatusCode::PAYLOAD_TOO_LARGE);
    }

    #[test]
    fn test_error_protobuf_decode_maps_to_400() {
        let err = OtelError::ProtobufDecode {
            reason: "bad data".into(),
        };
        let problem: Problem = err.into();
        assert_eq!(problem.status_code, StatusCode::BAD_REQUEST);
    }

    #[test]
    fn test_error_validation_maps_to_400() {
        let err = OtelError::Validation {
            message: "missing field".into(),
        };
        let problem: Problem = err.into();
        assert_eq!(problem.status_code, StatusCode::BAD_REQUEST);
    }

    #[test]
    fn test_error_decompression_maps_to_400() {
        let err = OtelError::DecompressionFailed {
            encoding: "gzip".into(),
            reason: "corrupt".into(),
        };
        let problem: Problem = err.into();
        assert_eq!(problem.status_code, StatusCode::BAD_REQUEST);
    }

    #[test]
    fn test_error_unsupported_encoding_maps_to_400() {
        let err = OtelError::UnsupportedEncoding {
            encoding: "brotli".into(),
        };
        let problem: Problem = err.into();
        assert_eq!(problem.status_code, StatusCode::BAD_REQUEST);
    }

    #[test]
    fn test_error_project_not_found_maps_to_404() {
        let err = OtelError::ProjectNotFound { project_id: 42 };
        let problem: Problem = err.into();
        assert_eq!(problem.status_code, StatusCode::NOT_FOUND);
    }

    #[test]
    fn test_error_storage_maps_to_500() {
        let err = OtelError::Storage {
            message: "disk full".into(),
        };
        let problem: Problem = err.into();
        assert_eq!(problem.status_code, StatusCode::INTERNAL_SERVER_ERROR);
    }

    #[test]
    fn test_error_database_maps_to_500() {
        let err = OtelError::Database(sea_orm::DbErr::Custom("test".into()));
        let problem: Problem = err.into();
        assert_eq!(problem.status_code, StatusCode::INTERNAL_SERVER_ERROR);
    }

    #[test]
    fn test_error_s3_maps_to_500() {
        let err = OtelError::S3 {
            project_id: 1,
            reason: "timeout".into(),
        };
        let problem: Problem = err.into();
        assert_eq!(problem.status_code, StatusCode::INTERNAL_SERVER_ERROR);
    }

    #[test]
    fn test_error_io_maps_to_500() {
        let err = OtelError::Io(std::io::Error::other("test"));
        let problem: Problem = err.into();
        assert_eq!(problem.status_code, StatusCode::INTERNAL_SERVER_ERROR);
    }

    #[test]
    fn test_error_internal_maps_to_500() {
        let err = OtelError::Internal {
            message: "unexpected".into(),
        };
        let problem: Problem = err.into();
        assert_eq!(problem.status_code, StatusCode::INTERNAL_SERVER_ERROR);
    }

    #[test]
    fn test_error_problem_detail_contains_message() {
        let err = OtelError::RateLimitExceeded {
            project_id: 7,
            limit: 500,
        };
        let problem: Problem = err.into();
        let detail = problem
            .body
            .get("detail")
            .and_then(|v| v.as_str())
            .unwrap_or_default();
        assert!(detail.contains("project 7"), "detail: {detail}");
        assert!(detail.contains("500"), "detail: {detail}");
    }

    // ── IngestContext tests ────────────────────────────────────────

    #[test]
    fn test_ingest_context_from_path_ids() {
        let ctx = IngestContext {
            project_id: 10,
            environment_id: Some(20),
            deployment_id: Some(30),
        };
        assert_eq!(ctx.project_id, 10);
        assert_eq!(ctx.environment_id, Some(20));
        assert_eq!(ctx.deployment_id, Some(30));
    }

    #[test]
    fn test_ingest_path_params_type_alias() {
        let params: IngestPathParams = (1, 2, 3);
        assert_eq!(params.0, 1);
        assert_eq!(params.1, 2);
        assert_eq!(params.2, 3);
    }
}
