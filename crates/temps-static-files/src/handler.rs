//! File Handler
//!
//! HTTP handlers for serving static files

use axum::{
    extract::{Path, State},
    http::{header, StatusCode},
    response::IntoResponse,
    routing::get,
    Router,
};
use std::sync::Arc;
use tracing::{debug, error};
use utoipa::OpenApi;

use crate::service::FileService;

/// State for file routes
#[derive(Clone)]
pub struct FileState {
    pub file_service: Arc<FileService>,
}

#[derive(OpenApi)]
#[openapi(
    paths(get_file),
    info(
        title = "Static Files API",
        description = "API endpoints for serving static files (screenshots, logs, etc.)",
        version = "1.0.0"
    ),
    tags(
        (name = "Files", description = "Static file serving endpoints")
    )
)]
pub struct FileApiDoc;

#[utoipa::path(
    get,
    path = "/files/{file_path}",
    tag = "Files",
    responses(
        (status = 200, description = "File content retrieved successfully", content_type = "application/octet-stream"),
        (status = 403, description = "Access denied - path outside static directory"),
        (status = 404, description = "File not found"),
        (status = 500, description = "Internal server error")
    ),
    params(
        ("file_path" = String, Path, description = "Relative path to the file from static directory")
    )
)]
async fn get_file(
    Path(file_path): Path<String>,
    State(state): State<Arc<FileState>>,
) -> impl IntoResponse {
    debug!("GET /files/{}", file_path);

    match state.file_service.get_file(&file_path).await {
        Ok(content) => {
            let content_type = infer_content_type(&file_path);
            (
                StatusCode::OK,
                [(header::CONTENT_TYPE, content_type)],
                content,
            )
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            error!("File not found: {}", file_path);
            (
                StatusCode::NOT_FOUND,
                [(header::CONTENT_TYPE, "text/plain")],
                format!("File not found: {}", file_path).into_bytes(),
            )
        }
        Err(e) if e.kind() == std::io::ErrorKind::PermissionDenied => {
            error!("Access denied: {}", file_path);
            (
                StatusCode::FORBIDDEN,
                [(header::CONTENT_TYPE, "text/plain")],
                b"Access denied".to_vec(),
            )
        }
        Err(e) => {
            error!("Error reading file {}: {}", file_path, e);
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                [(header::CONTENT_TYPE, "text/plain")],
                format!("Error reading file: {}", e).into_bytes(),
            )
        }
    }
}

fn infer_content_type(file_path: &str) -> &'static str {
    let extension = std::path::Path::new(file_path)
        .extension()
        .and_then(std::ffi::OsStr::to_str)
        .unwrap_or("");

    match extension.to_lowercase().as_str() {
        "html" => "text/html",
        "css" => "text/css",
        "js" => "application/javascript",
        "json" => "application/json",
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "gif" => "image/gif",
        "svg" => "image/svg+xml",
        "webp" => "image/webp",
        "pdf" => "application/pdf",
        "txt" | "log" => "text/plain",
        "xml" => "application/xml",
        "zip" => "application/zip",
        _ => "application/octet-stream",
    }
}

pub fn configure_routes(file_service: Arc<FileService>) -> Router {
    let state = Arc::new(FileState { file_service });
    Router::new()
        .route("/files/{*file_path}", get(get_file))
        .with_state(state)
}
