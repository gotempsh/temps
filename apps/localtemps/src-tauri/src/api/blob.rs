//! Blob API endpoints
//!
//! Implements SDK-compatible Blob API endpoints using the existing BlobService.

use std::sync::Arc;

use axum::{
    body::Body,
    extract::{Path, Query, State},
    http::{header, StatusCode},
    response::IntoResponse,
    Json,
};
use bytes::Bytes;
use chrono::{DateTime, Utc};
use futures::StreamExt;
use serde::{Deserialize, Serialize};
use tracing::debug;

use crate::context::{LocalTempsContext, LOCAL_PROJECT_ID};

/// Upload query parameters
#[derive(Deserialize)]
pub struct UploadQuery {
    pub pathname: String,
    #[serde(default)]
    pub content_type: Option<String>,
    #[serde(default)]
    pub add_random_suffix: Option<bool>,
    #[serde(default)]
    pub project_id: Option<i32>,
}

/// Upload response
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct UploadResponse {
    pub url: String,
    pub pathname: String,
    pub content_type: String,
    pub size: i64,
    pub uploaded_at: DateTime<Utc>,
}

/// List query parameters
#[derive(Deserialize)]
pub struct ListQuery {
    #[serde(default)]
    pub limit: Option<i32>,
    #[serde(default)]
    pub prefix: Option<String>,
    #[serde(default)]
    pub cursor: Option<String>,
    #[serde(default)]
    pub project_id: Option<i32>,
}

/// List response
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ListResponse {
    pub blobs: Vec<BlobInfoResponse>,
    pub cursor: Option<String>,
    pub has_more: bool,
}

/// Blob info response
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct BlobInfoResponse {
    pub url: String,
    pub pathname: String,
    pub content_type: String,
    pub size: i64,
    pub uploaded_at: DateTime<Utc>,
}

/// Delete request
#[derive(Deserialize)]
pub struct DeleteRequest {
    pub pathnames: Vec<String>,
    #[serde(default)]
    pub project_id: Option<i32>,
}

/// Copy request
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CopyRequest {
    pub from_url: String,
    pub to_pathname: String,
    #[serde(default)]
    pub project_id: Option<i32>,
}

/// Error response
#[derive(Serialize)]
pub struct ErrorResponse {
    pub error: ErrorDetails,
}

#[derive(Serialize)]
pub struct ErrorDetails {
    pub message: String,
    pub code: String,
}

fn error_response(status: StatusCode, message: &str, code: &str) -> impl IntoResponse {
    (
        status,
        Json(ErrorResponse {
            error: ErrorDetails {
                message: message.to_string(),
                code: code.to_string(),
            },
        }),
    )
}

/// Upload blob endpoint (POST /api/blob?pathname=...)
pub async fn upload_handler(
    State(ctx): State<Arc<LocalTempsContext>>,
    Query(query): Query<UploadQuery>,
    body: Bytes,
) -> impl IntoResponse {
    let project_id = query.project_id.unwrap_or(LOCAL_PROJECT_ID);
    debug!(
        "BLOB PUT pathname={} size={} project_id={}",
        query.pathname,
        body.len(),
        project_id
    );

    let options = temps_blob::services::PutOptions {
        content_type: query.content_type,
        add_random_suffix: query.add_random_suffix.unwrap_or(false),
    };

    match ctx
        .blob_service()
        .put(project_id, &query.pathname, body, options)
        .await
    {
        Ok(info) => (
            StatusCode::OK,
            Json(UploadResponse {
                url: info.url,
                pathname: info.pathname,
                content_type: info.content_type,
                size: info.size,
                uploaded_at: info.uploaded_at,
            }),
        )
            .into_response(),
        Err(e) => error_response(
            StatusCode::INTERNAL_SERVER_ERROR,
            &e.to_string(),
            "BLOB_ERROR",
        )
        .into_response(),
    }
}

/// List blobs endpoint (GET /api/blob)
pub async fn list_handler(
    State(ctx): State<Arc<LocalTempsContext>>,
    Query(query): Query<ListQuery>,
) -> impl IntoResponse {
    let project_id = query.project_id.unwrap_or(LOCAL_PROJECT_ID);
    debug!(
        "BLOB LIST prefix={:?} limit={:?} project_id={}",
        query.prefix, query.limit, project_id
    );

    let options = temps_blob::services::ListOptions {
        limit: query.limit,
        prefix: query.prefix,
        cursor: query.cursor,
    };

    match ctx.blob_service().list(project_id, options).await {
        Ok(result) => (
            StatusCode::OK,
            Json(ListResponse {
                blobs: result
                    .blobs
                    .into_iter()
                    .map(|b| BlobInfoResponse {
                        url: b.url,
                        pathname: b.pathname,
                        content_type: b.content_type,
                        size: b.size,
                        uploaded_at: b.uploaded_at,
                    })
                    .collect(),
                cursor: result.cursor,
                has_more: result.has_more,
            }),
        )
            .into_response(),
        Err(e) => error_response(
            StatusCode::INTERNAL_SERVER_ERROR,
            &e.to_string(),
            "BLOB_ERROR",
        )
        .into_response(),
    }
}

/// Delete blobs endpoint (DELETE /api/blob)
pub async fn delete_handler(
    State(ctx): State<Arc<LocalTempsContext>>,
    Json(request): Json<DeleteRequest>,
) -> impl IntoResponse {
    let project_id = request.project_id.unwrap_or(LOCAL_PROJECT_ID);
    debug!(
        "BLOB DELETE pathnames={:?} project_id={}",
        request.pathnames, project_id
    );

    match ctx.blob_service().del(project_id, request.pathnames).await {
        Ok(_) => StatusCode::OK.into_response(),
        Err(e) => error_response(
            StatusCode::INTERNAL_SERVER_ERROR,
            &e.to_string(),
            "BLOB_ERROR",
        )
        .into_response(),
    }
}

/// Head blob endpoint (HEAD /api/blob/{project_id}/{path})
pub async fn head_handler(
    State(ctx): State<Arc<LocalTempsContext>>,
    Path((project_id, path)): Path<(i32, String)>,
) -> impl IntoResponse {
    debug!("BLOB HEAD path={} project_id={}", path, project_id);

    match ctx.blob_service().head(project_id, &path).await {
        Ok(info) => {
            let mut response = StatusCode::OK.into_response();
            let headers = response.headers_mut();
            headers.insert(
                header::CONTENT_TYPE,
                info.content_type
                    .parse()
                    .unwrap_or(header::HeaderValue::from_static("application/octet-stream")),
            );
            headers.insert(
                header::CONTENT_LENGTH,
                info.size
                    .to_string()
                    .parse()
                    .unwrap_or(header::HeaderValue::from_static("0")),
            );
            headers.insert(
                header::LAST_MODIFIED,
                info.uploaded_at
                    .to_rfc2822()
                    .parse()
                    .unwrap_or(header::HeaderValue::from_static("")),
            );
            response
        }
        Err(e) => {
            let err_str = e.to_string();
            if err_str.contains("NotFound") || err_str.contains("not found") {
                error_response(StatusCode::NOT_FOUND, "Blob not found", "NOT_FOUND").into_response()
            } else {
                error_response(StatusCode::INTERNAL_SERVER_ERROR, &err_str, "BLOB_ERROR")
                    .into_response()
            }
        }
    }
}

/// Download blob endpoint (GET /api/blob/{project_id}/{path})
pub async fn download_handler(
    State(ctx): State<Arc<LocalTempsContext>>,
    Path((project_id, path)): Path<(i32, String)>,
) -> impl IntoResponse {
    debug!("BLOB GET path={} project_id={}", path, project_id);

    match ctx.blob_service().download(project_id, &path).await {
        Ok((stream, content_type, size)) => {
            let body = Body::from_stream(stream.map(|result| {
                result.map_err(|e| std::io::Error::new(std::io::ErrorKind::Other, e))
            }));

            let mut response = (StatusCode::OK, body).into_response();
            let headers = response.headers_mut();
            headers.insert(
                header::CONTENT_TYPE,
                content_type
                    .parse()
                    .unwrap_or(header::HeaderValue::from_static("application/octet-stream")),
            );
            headers.insert(
                header::CONTENT_LENGTH,
                size.to_string()
                    .parse()
                    .unwrap_or(header::HeaderValue::from_static("0")),
            );
            response
        }
        Err(e) => {
            let err_str = e.to_string();
            if err_str.contains("NotFound") || err_str.contains("not found") {
                error_response(StatusCode::NOT_FOUND, "Blob not found", "NOT_FOUND").into_response()
            } else {
                error_response(StatusCode::INTERNAL_SERVER_ERROR, &err_str, "BLOB_ERROR")
                    .into_response()
            }
        }
    }
}

/// Copy blob endpoint (POST /api/blob/copy)
pub async fn copy_handler(
    State(ctx): State<Arc<LocalTempsContext>>,
    Json(request): Json<CopyRequest>,
) -> impl IntoResponse {
    let project_id = request.project_id.unwrap_or(LOCAL_PROJECT_ID);
    debug!(
        "BLOB COPY from={} to={} project_id={}",
        request.from_url, request.to_pathname, project_id
    );

    // Extract pathname from URL
    let from_pathname = extract_pathname_from_url(&request.from_url, project_id);

    match ctx
        .blob_service()
        .copy(project_id, &from_pathname, &request.to_pathname)
        .await
    {
        Ok(info) => (
            StatusCode::OK,
            Json(UploadResponse {
                url: info.url,
                pathname: info.pathname,
                content_type: info.content_type,
                size: info.size,
                uploaded_at: info.uploaded_at,
            }),
        )
            .into_response(),
        Err(e) => error_response(
            StatusCode::INTERNAL_SERVER_ERROR,
            &e.to_string(),
            "BLOB_ERROR",
        )
        .into_response(),
    }
}

/// Extract pathname from a blob URL
fn extract_pathname_from_url(url: &str, project_id: i32) -> String {
    // Handle both full URLs and relative paths
    let path = if url.starts_with("http://") || url.starts_with("https://") {
        url.split("/api/blob/").last().unwrap_or(url)
    } else if url.starts_with("/api/blob/") {
        &url["/api/blob/".len()..]
    } else {
        url
    };

    // Remove project_id prefix if present
    let prefix = format!("{}/", project_id);
    if path.starts_with(&prefix) {
        path[prefix.len()..].to_string()
    } else {
        path.to_string()
    }
}
