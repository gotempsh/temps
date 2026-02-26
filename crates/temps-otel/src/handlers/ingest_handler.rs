//! OTLP/HTTP ingest handlers.
//!
//! Accept protobuf-encoded payloads with gzip/zstd compression.
//! Authenticate via per-project API key in the `Authorization` header.
//! Return correct OTLP response envelopes.

use axum::body::Bytes;
use axum::extract::State;
use axum::http::{HeaderMap, StatusCode};
use axum::response::IntoResponse;
use prost::Message;
use tracing::debug;

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
                problemdetails::new(StatusCode::UNAUTHORIZED)
                    .with_title("Authentication Failed")
                    .with_detail(error.to_string())
            }
            OtelError::RateLimitExceeded { .. } => {
                problemdetails::new(StatusCode::TOO_MANY_REQUESTS)
                    .with_title("Rate Limit Exceeded")
                    .with_detail(error.to_string())
            }
            OtelError::QuotaExceeded { .. } => problemdetails::new(StatusCode::PAYLOAD_TOO_LARGE)
                .with_title("Storage Quota Exceeded")
                .with_detail(error.to_string()),
            OtelError::ProtobufDecode { .. } | OtelError::Validation { .. } => {
                problemdetails::new(StatusCode::BAD_REQUEST)
                    .with_title("Invalid Payload")
                    .with_detail(error.to_string())
            }
            OtelError::DecompressionFailed { .. } | OtelError::UnsupportedEncoding { .. } => {
                problemdetails::new(StatusCode::BAD_REQUEST)
                    .with_title("Decompression Error")
                    .with_detail(error.to_string())
            }
            OtelError::ProjectNotFound { .. } => problemdetails::new(StatusCode::NOT_FOUND)
                .with_title("Project Not Found")
                .with_detail(error.to_string()),
            OtelError::Storage { .. }
            | OtelError::Database(_)
            | OtelError::S3 { .. }
            | OtelError::Io(_)
            | OtelError::Serialization(_)
            | OtelError::Internal { .. } => problemdetails::new(StatusCode::INTERNAL_SERVER_ERROR)
                .with_title("Internal Server Error")
                .with_detail(error.to_string()),
        }
    }
}

/// Extract API key from headers.
fn extract_api_key(headers: &HeaderMap) -> Option<String> {
    // Check Authorization: Bearer tk_...
    if let Some(auth) = headers.get("authorization") {
        if let Ok(value) = auth.to_str() {
            if let Some(key) = value.strip_prefix("Bearer ") {
                return Some(key.trim().to_string());
            }
        }
    }

    // Check X-Temps-Api-Key: tk_...
    if let Some(key) = headers.get("x-temps-api-key") {
        if let Ok(value) = key.to_str() {
            return Some(value.trim().to_string());
        }
    }

    None
}

/// Extract Content-Encoding from headers.
fn content_encoding(headers: &HeaderMap) -> Option<&str> {
    headers
        .get("content-encoding")
        .and_then(|v| v.to_str().ok())
}

/// Ingest metrics via OTLP/HTTP protobuf.
///
/// Authenticates via API key, decompresses, decodes protobuf,
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
    let api_key = extract_api_key(&headers).ok_or_else(|| OtelError::AuthFailed {
        reason: "Missing API key in Authorization or X-Temps-Api-Key header".into(),
    })?;

    let auth = state.otel_service.authenticate(&api_key).await?;
    state.otel_service.check_rate_limit(auth.project_id)?;
    state.otel_service.check_quota(auth.project_id).await?;

    // Decompress
    let data = decode::decompress(&body, content_encoding(&headers))?;

    // Decode protobuf
    let points = decode::decode_metrics_request(&data, auth.project_id, None)?;
    let count = points.len();

    // Store
    let stored = state.otel_service.ingest_metrics(points).await?;

    debug!(
        project_id = auth.project_id,
        received = count,
        stored,
        "Ingested metrics"
    );

    // Return OTLP response envelope
    let response = proto::collector::metrics::v1::ExportMetricsServiceResponse {
        partial_success: None,
    };
    let encoded = response.encode_to_vec();

    Ok((
        StatusCode::OK,
        [("content-type", "application/x-protobuf")],
        encoded,
    ))
}

/// Ingest trace spans via OTLP/HTTP protobuf.
///
/// Authenticates via API key, decompresses, decodes protobuf,
/// applies tail-based sampling, checks rate limit and storage quota,
/// then stores surviving spans.
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
    let api_key = extract_api_key(&headers).ok_or_else(|| OtelError::AuthFailed {
        reason: "Missing API key in Authorization or X-Temps-Api-Key header".into(),
    })?;

    let auth = state.otel_service.authenticate(&api_key).await?;
    state.otel_service.check_rate_limit(auth.project_id)?;
    state.otel_service.check_quota(auth.project_id).await?;

    let data = decode::decompress(&body, content_encoding(&headers))?;
    let spans = decode::decode_traces_request(&data, auth.project_id, None)?;
    let count = spans.len();

    let stored = state.otel_service.ingest_spans(spans).await?;

    debug!(
        project_id = auth.project_id,
        received = count,
        stored,
        "Ingested traces"
    );

    let response = proto::collector::trace::v1::ExportTraceServiceResponse {
        partial_success: None,
    };
    let encoded = response.encode_to_vec();

    Ok((
        StatusCode::OK,
        [("content-type", "application/x-protobuf")],
        encoded,
    ))
}

/// Ingest log records via OTLP/HTTP protobuf.
///
/// Authenticates via API key, decompresses, decodes protobuf,
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
    let api_key = extract_api_key(&headers).ok_or_else(|| OtelError::AuthFailed {
        reason: "Missing API key in Authorization or X-Temps-Api-Key header".into(),
    })?;

    let auth = state.otel_service.authenticate(&api_key).await?;
    state.otel_service.check_rate_limit(auth.project_id)?;
    state.otel_service.check_quota(auth.project_id).await?;

    let data = decode::decompress(&body, content_encoding(&headers))?;
    let records = decode::decode_logs_request(&data, auth.project_id, None)?;
    let count = records.len();

    let stored = state.otel_service.ingest_logs(records).await?;

    debug!(
        project_id = auth.project_id,
        received = count,
        stored,
        "Ingested logs"
    );

    let response = proto::collector::logs::v1::ExportLogsServiceResponse {
        partial_success: None,
    };
    let encoded = response.encode_to_vec();

    Ok((
        StatusCode::OK,
        [("content-type", "application/x-protobuf")],
        encoded,
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::http::HeaderMap;

    // ── extract_api_key tests ──────────────────────────────────────

    #[test]
    fn test_extract_api_key_bearer() {
        let mut headers = HeaderMap::new();
        headers.insert("authorization", "Bearer tk_abc123".parse().unwrap());
        assert_eq!(extract_api_key(&headers), Some("tk_abc123".to_string()));
    }

    #[test]
    fn test_extract_api_key_custom_header() {
        let mut headers = HeaderMap::new();
        headers.insert("x-temps-api-key", "tk_xyz789".parse().unwrap());
        assert_eq!(extract_api_key(&headers), Some("tk_xyz789".to_string()));
    }

    #[test]
    fn test_extract_api_key_missing() {
        let headers = HeaderMap::new();
        assert_eq!(extract_api_key(&headers), None);
    }

    #[test]
    fn test_extract_api_key_bearer_takes_priority() {
        let mut headers = HeaderMap::new();
        headers.insert("authorization", "Bearer tk_first".parse().unwrap());
        headers.insert("x-temps-api-key", "tk_second".parse().unwrap());
        assert_eq!(extract_api_key(&headers), Some("tk_first".to_string()));
    }

    #[test]
    fn test_extract_api_key_non_bearer_auth_falls_through() {
        let mut headers = HeaderMap::new();
        headers.insert("authorization", "Basic dXNlcjpwYXNz".parse().unwrap());
        headers.insert("x-temps-api-key", "tk_fallback".parse().unwrap());
        assert_eq!(extract_api_key(&headers), Some("tk_fallback".to_string()));
    }

    #[test]
    fn test_extract_api_key_bearer_trimmed() {
        let mut headers = HeaderMap::new();
        headers.insert("authorization", "Bearer  tk_spaces  ".parse().unwrap());
        assert_eq!(extract_api_key(&headers), Some("tk_spaces".to_string()));
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
        let err = OtelError::Io(std::io::Error::new(std::io::ErrorKind::Other, "test"));
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
}
