// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

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
use temps_auth::{Permission, RequireAuth};
use tracing::{debug, error};
use utoipa::OpenApi;

use super::service::FileService;

/// State for file routes
#[derive(Clone)]
pub(super) struct FileState {
    file_service: Arc<FileService>,
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
pub(super) struct FileApiDoc;

#[utoipa::path(
    get,
    path = "/files/{file_path}",
    tag = "Files",
    responses(
        (status = 200, description = "File content retrieved successfully", content_type = "application/octet-stream"),
        (status = 401, description = "Authentication required"),
        (status = 403, description = "Access denied - path outside static directory or insufficient permissions"),
        (status = 404, description = "File not found"),
        (status = 500, description = "Internal server error")
    ),
    params(
        ("file_path" = String, Path, description = "Relative path to the file from static directory")
    ),
    security(("bearer_auth" = []))
)]
async fn get_file(
    RequireAuth(auth): RequireAuth,
    Path(file_path): Path<String>,
    State(state): State<Arc<FileState>>,
) -> impl IntoResponse {
    debug!("GET /files/{}", file_path);

    if !auth.has_permission(&Permission::FilesRead) {
        error!("Insufficient permissions to read file: {}", file_path);
        return (
            StatusCode::FORBIDDEN,
            [(header::CONTENT_TYPE, "text/plain")],
            b"Insufficient permissions".to_vec(),
        );
    }

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
            debug!("File not found: {}", file_path);
            (
                StatusCode::NOT_FOUND,
                [(header::CONTENT_TYPE, "text/plain")],
                b"Not found".to_vec(),
            )
        }
        Err(e) if e.kind() == std::io::ErrorKind::PermissionDenied => {
            error!("Access denied for file: {}", file_path);
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
                b"Internal server error".to_vec(),
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

pub(super) fn configure_routes(file_service: Arc<FileService>) -> Router {
    let state = Arc::new(FileState { file_service });
    Router::new()
        .route("/files/{*file_path}", get(get_file))
        .with_state(state)
}

#[cfg(test)]
mod tests {
    use axum::body::{to_bytes, Body};
    use axum::http::Request;
    use bytes::Bytes;
    use temps_auth::{AuthContext, Role};
    use temps_file_store::fs_store::FsFileStore;
    use temps_file_store::FileStore;
    use tower::ServiceExt;

    use super::super::service::config_service_for_tests;
    use super::*;

    fn auth(role: Role) -> AuthContext {
        let now = chrono::Utc::now();
        let user = temps_entities::users::Model {
            id: 1,
            name: "Operator".to_string(),
            email: "operator@example.com".to_string(),
            password_hash: None,
            email_verified: true,
            email_verification_token: None,
            email_verification_expires: None,
            password_reset_token: None,
            password_reset_expires: None,
            must_change_password: false,
            deleted_at: None,
            mfa_secret: None,
            mfa_enabled: false,
            mfa_recovery_codes: None,
            oidc_subject: None,
            oidc_provider_id: None,
            created_at: now,
            updated_at: now,
        };
        AuthContext::new_session(user, role)
    }

    async fn router_serving(path: &str, data: &'static [u8]) -> (Router, tempfile::TempDir) {
        let dir = tempfile::tempdir().expect("temp dir");
        let store = FsFileStore::new(dir.path());
        store
            .put(path, Bytes::from_static(data))
            .await
            .expect("seed file");
        let service = Arc::new(FileService::with_store(
            config_service_for_tests(dir.path()),
            Arc::new(store),
        ));
        (configure_routes(service), dir)
    }

    fn get(uri: &str, auth: Option<AuthContext>) -> Request<Body> {
        let mut request = Request::builder()
            .uri(uri)
            .body(Body::empty())
            .expect("valid request");
        if let Some(auth) = auth {
            request.extensions_mut().insert(auth);
        }
        request
    }

    #[tokio::test]
    async fn get_file_requires_authentication() {
        let (router, _dir) = router_serving("screenshots/a.png", b"png").await;

        let response = router
            .oneshot(get("/files/screenshots/a.png", None))
            .await
            .expect("response");

        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn get_file_requires_the_files_read_permission() {
        let (router, _dir) = router_serving("screenshots/a.png", b"png").await;

        let response = router
            .oneshot(get(
                "/files/screenshots/a.png",
                Some(auth(Role::MetricsIngest)),
            ))
            .await
            .expect("response");

        assert_eq!(response.status(), StatusCode::FORBIDDEN);
    }

    #[tokio::test]
    async fn get_file_serves_the_file_with_its_content_type() {
        let (router, _dir) = router_serving("screenshots/a.png", b"png-bytes").await;

        let response = router
            .oneshot(get("/files/screenshots/a.png", Some(auth(Role::Reader))))
            .await
            .expect("response");

        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(response.headers()[header::CONTENT_TYPE], "image/png");
        let body = to_bytes(response.into_body(), 1024).await.expect("body");
        assert_eq!(&body[..], b"png-bytes");
    }

    #[tokio::test]
    async fn get_file_returns_not_found_for_a_missing_file() {
        let (router, _dir) = router_serving("screenshots/a.png", b"png").await;

        let response = router
            .oneshot(get("/files/screenshots/b.png", Some(auth(Role::Admin))))
            .await
            .expect("response");

        assert_eq!(response.status(), StatusCode::NOT_FOUND);
    }

    #[test]
    fn content_type_follows_the_extension_case_insensitively() {
        assert_eq!(infer_content_type("logs/build.LOG"), "text/plain");
        assert_eq!(infer_content_type("shots/a.JPEG"), "image/jpeg");
        assert_eq!(infer_content_type("archive"), "application/octet-stream");
    }
}
