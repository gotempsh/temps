//! Sentry CLI-compatible API endpoints
//!
//! Provides endpoints that match the Sentry API format used by `sentry-cli sourcemaps upload`.
//! This allows using the standard sentry-cli tool to upload source maps to Temps.
//!
//! ## Authentication
//!
//! These endpoints accept `Authorization: Bearer <dsn_public_key>` where the Bearer token
//! is the DSN public key from a project DSN. This matches how sentry-cli sends auth tokens.
//!
//! ## Endpoint mapping
//!
//! | sentry-cli endpoint | Temps handler |
//! |---------------------|---------------|
//! | `POST /api/0/organizations/{org}/releases/` | `create_release` (stub) |
//! | `POST /api/0/projects/{org}/{project}/releases/{version}/files/` | `upload_release_file` |
//! | `GET /api/0/projects/{org}/{project}/releases/{version}/files/` | `list_release_files` |
//! | `GET /api/0/organizations/{org}/chunk-upload/` | `chunk_upload_options` (stub) |

use axum::{
    extract::{DefaultBodyLimit, Multipart, Path, State},
    http::{HeaderMap, StatusCode},
    response::IntoResponse,
    routing::{get, post, put},
    Json, Router,
};
use chrono::Utc;
use sea_orm::{ColumnTrait, DatabaseConnection, EntityTrait, QueryFilter};
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use temps_entities::{deployment_tokens, project_dsns, projects};
use tracing::{debug, warn};
use utoipa::{OpenApi, ToSchema};

use crate::services::source_map_service::SourceMapService;

#[derive(OpenApi)]
#[openapi(
    paths(
        create_release,
        create_project_release,
        finalize_project_release,
        upload_release_file,
        list_release_files,
        chunk_upload_options,
    ),
    components(schemas(
        SentryCreateReleaseRequest,
        SentryReleaseResponse,
        SentryReleaseFileResponse,
        SentryChunkUploadResponse,
    )),
    tags(
        (name = "sentry-compat", description = "Sentry CLI-compatible API endpoints for source map uploads")
    )
)]
pub struct SentryCompatApiDoc;

#[derive(Clone)]
pub struct SentryCompatAppState {
    pub source_map_service: Arc<SourceMapService>,
    pub db: Arc<DatabaseConnection>,
}

// --- Request/Response types ---

#[derive(Deserialize, ToSchema)]
pub struct SentryCreateReleaseRequest {
    /// Release version identifier
    pub version: String,
    /// Project slugs this release belongs to
    #[serde(default)]
    pub projects: Vec<String>,
}

#[derive(Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct SentryReleaseResponse {
    pub version: String,
    pub date_created: String,
    pub date_released: Option<String>,
    pub short_version: String,
    pub projects: Vec<SentryReleaseProjectRef>,
}

#[derive(Serialize, ToSchema)]
pub struct SentryReleaseProjectRef {
    pub name: String,
    pub slug: String,
}

#[derive(Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct SentryReleaseFileResponse {
    pub id: String,
    pub name: String,
    pub dist: Option<String>,
    pub headers: serde_json::Value,
    pub size: i64,
    pub sha1: String,
    pub date_created: String,
}

#[derive(Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct SentryChunkUploadResponse {
    pub url: String,
    pub chunk_size: u64,
    pub chunks_per_request: u32,
    pub max_file_size: u64,
    pub max_request_size: u64,
    pub concurrency: u32,
    pub hash_algorithm: String,
    pub compression: Vec<String>,
    pub accept: Vec<String>,
}

// --- Route configuration ---

/// Maximum body size for the sentry-compat source map upload routes (50 MiB).
///
/// DSN public keys are semi-public (embedded in browser bundles), so we cannot
/// rely on authentication alone to prevent abuse. The outer body limit rejects
/// oversized uploads before they reach the multipart reader. A per-field inline
/// check (`MAX_SOURCE_MAP_BYTES`) provides additional defense-in-depth.
pub const SENTRY_UPLOAD_BODY_LIMIT: usize = 50 * 1024 * 1024;

/// Maximum size for a single source map file field (50 MiB).
///
/// Applied after the compressed body has been received. This is a second line
/// of defense: the outer `DefaultBodyLimit` cap fires first, but if multiple
/// small files add up the per-field check catches any single oversized field.
const MAX_SOURCE_MAP_BYTES: usize = 50 * 1024 * 1024;

pub fn configure_sentry_compat_routes() -> Router<Arc<SentryCompatAppState>> {
    // Fix #4: the upload route gets its own sub-router with a 50 MiB body limit.
    // The other routes (create_release, finalize, list) use the default Axum
    // body limit (2 MiB) which is fine for small JSON payloads.
    let upload_routes = Router::new()
        .route(
            "/0/projects/{org_slug}/{project_slug}/releases/{version}/files/",
            post(upload_release_file).get(list_release_files),
        )
        .layer(DefaultBodyLimit::max(SENTRY_UPLOAD_BODY_LIMIT));

    Router::new()
        .route(
            "/0/organizations/{org_slug}/releases/",
            post(create_release),
        )
        // sentry-cli uses this endpoint when both SENTRY_ORG and SENTRY_PROJECT are set
        .route(
            "/0/projects/{org_slug}/{project_slug}/releases/",
            post(create_project_release),
        )
        // sentry-cli `releases finalize` hits PUT /api/0/projects/{org}/{proj}/releases/{version}/
        .route(
            "/0/projects/{org_slug}/{project_slug}/releases/{version}/",
            put(finalize_project_release),
        )
        .route(
            "/0/organizations/{org_slug}/chunk-upload/",
            get(chunk_upload_options),
        )
        .merge(upload_routes)
}

// --- Auth helper ---

/// Extract project_id from Bearer token authentication.
///
/// sentry-cli sends `Authorization: Bearer <auth_token>`.
/// We accept:
///   1. DSN public key → resolves to project via `project_dsns` table
///   2. Deployment token (`dt_*`) → resolves to project via `deployment_tokens` table
///      (used when Temps injects SENTRY_AUTH_TOKEN during builds)
async fn authenticate_bearer(
    headers: &HeaderMap,
    db: &DatabaseConnection,
) -> Result<i32, (StatusCode, String)> {
    let auth_header = headers
        .get("authorization")
        .and_then(|v| v.to_str().ok())
        .ok_or((
            StatusCode::UNAUTHORIZED,
            "Missing Authorization header".to_string(),
        ))?;

    let token = if let Some(token) = auth_header.strip_prefix("Bearer ") {
        token.trim()
    } else if let Some(token) = auth_header.strip_prefix("DSN ") {
        token.trim()
    } else {
        return Err((
            StatusCode::UNAUTHORIZED,
            "Invalid Authorization format. Expected 'Bearer <token>'".to_string(),
        ));
    };

    // Deployment tokens start with "dt_" — validate via hash lookup
    if token.starts_with("dt_") {
        use sha2::{Digest, Sha256};
        let token_hash = hex::encode(Sha256::digest(token.as_bytes()));
        let token_prefix: String = token.chars().take(8).collect();

        let dt = deployment_tokens::Entity::find()
            .filter(deployment_tokens::Column::TokenHash.eq(&token_hash))
            .filter(deployment_tokens::Column::TokenPrefix.eq(&token_prefix))
            .filter(deployment_tokens::Column::IsActive.eq(true))
            .one(db)
            .await
            .map_err(|e| {
                (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    format!("Database error: {}", e),
                )
            })?
            .ok_or_else(|| {
                (
                    StatusCode::UNAUTHORIZED,
                    "Invalid deployment token".to_string(),
                )
            })?;

        return Ok(dt.project_id);
    }

    // Otherwise try DSN public key lookup
    let dsn = project_dsns::Entity::find()
        .filter(project_dsns::Column::PublicKey.eq(token))
        .filter(project_dsns::Column::IsActive.eq(true))
        .one(db)
        .await
        .map_err(|e| {
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("Database error: {}", e),
            )
        })?
        .ok_or_else(|| (StatusCode::UNAUTHORIZED, "Invalid auth token".to_string()))?;

    Ok(dsn.project_id)
}

/// Resolve a project slug (or numeric ID) to a project_id.
/// Also validates it matches the authenticated project.
async fn resolve_project_slug(
    project_slug: &str,
    expected_project_id: i32,
    db: &DatabaseConnection,
) -> Result<i32, (StatusCode, String)> {
    // Try numeric ID first
    if let Ok(id) = project_slug.parse::<i32>() {
        if id == expected_project_id {
            return Ok(id);
        }
    }

    // Try slug lookup
    let project = projects::Entity::find()
        .filter(projects::Column::Slug.eq(project_slug))
        .one(db)
        .await
        .map_err(|e| {
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("Database error: {}", e),
            )
        })?
        .ok_or_else(|| {
            (
                StatusCode::NOT_FOUND,
                format!("Project '{}' not found", project_slug),
            )
        })?;

    if project.id != expected_project_id {
        return Err((
            StatusCode::FORBIDDEN,
            "Token does not have access to this project".to_string(),
        ));
    }

    Ok(project.id)
}

// --- Handlers ---

/// Create a release (stub for sentry-cli compatibility).
///
/// sentry-cli calls this before uploading files. Since Temps implicitly creates
/// releases when source maps are uploaded, this is a no-op that returns the
/// expected response format.
#[utoipa::path(
    tag = "sentry-compat",
    post,
    path = "/0/organizations/{org_slug}/releases/",
    params(
        ("org_slug" = String, Path, description = "Organization slug (ignored in single-tenant mode)")
    ),
    request_body = SentryCreateReleaseRequest,
    responses(
        (status = 201, description = "Release created", body = SentryReleaseResponse),
        (status = 401, description = "Unauthorized"),
    ),
)]
async fn create_release(
    State(state): State<Arc<SentryCompatAppState>>,
    Path(_org_slug): Path<String>,
    headers: HeaderMap,
    Json(request): Json<SentryCreateReleaseRequest>,
) -> impl IntoResponse {
    // Authenticate
    if let Err((status, msg)) = authenticate_bearer(&headers, state.db.as_ref()).await {
        return (status, msg).into_response();
    }

    debug!("sentry-cli: Create release '{}' (stub)", request.version);

    let now = Utc::now().to_rfc3339();
    let short_version = if request.version.len() > 12 {
        request.version[..12].to_string()
    } else {
        request.version.clone()
    };

    let project_refs: Vec<SentryReleaseProjectRef> = request
        .projects
        .iter()
        .map(|p| SentryReleaseProjectRef {
            name: p.clone(),
            slug: p.clone(),
        })
        .collect();

    let response = SentryReleaseResponse {
        version: request.version,
        date_created: now,
        date_released: None,
        short_version,
        projects: project_refs,
    };

    (StatusCode::CREATED, Json(response)).into_response()
}

/// Create a release for a specific project (stub for sentry-cli compatibility).
///
/// sentry-cli calls this endpoint (instead of /organizations/.../releases/) when
/// both SENTRY_ORG and SENTRY_PROJECT env vars are set. Behaves identically to
/// the organizations endpoint but validates the project slug.
#[utoipa::path(
    tag = "sentry-compat",
    post,
    path = "/0/projects/{org_slug}/{project_slug}/releases/",
    params(
        ("org_slug" = String, Path, description = "Organization slug (ignored in single-tenant mode)"),
        ("project_slug" = String, Path, description = "Project slug or numeric ID"),
    ),
    request_body = SentryCreateReleaseRequest,
    responses(
        (status = 201, description = "Release created", body = SentryReleaseResponse),
        (status = 401, description = "Unauthorized"),
        (status = 404, description = "Project not found"),
    ),
)]
async fn create_project_release(
    State(state): State<Arc<SentryCompatAppState>>,
    Path((_org_slug, project_slug)): Path<(String, String)>,
    headers: HeaderMap,
    Json(request): Json<SentryCreateReleaseRequest>,
) -> impl IntoResponse {
    // Authenticate
    let auth_project_id = match authenticate_bearer(&headers, state.db.as_ref()).await {
        Ok(id) => id,
        Err((status, msg)) => return (status, msg).into_response(),
    };

    // Validate project slug matches the authenticated token
    if let Err((status, msg)) =
        resolve_project_slug(&project_slug, auth_project_id, state.db.as_ref()).await
    {
        return (status, msg).into_response();
    }

    debug!(
        "sentry-cli: Create project release '{}' for project '{}' (stub)",
        request.version, project_slug
    );

    let now = Utc::now().to_rfc3339();
    let short_version = if request.version.len() > 12 {
        request.version[..12].to_string()
    } else {
        request.version.clone()
    };

    let project_refs = vec![SentryReleaseProjectRef {
        name: project_slug.clone(),
        slug: project_slug,
    }];

    let response = SentryReleaseResponse {
        version: request.version,
        date_created: now,
        date_released: None,
        short_version,
        projects: project_refs,
    };

    (StatusCode::CREATED, Json(response)).into_response()
}

/// Finalize a release (stub for sentry-cli compatibility).
///
/// sentry-cli calls `releases finalize` after uploading source maps. This sets
/// the dateReleased on the release. Since Temps stores source maps independently
/// of releases, this is a no-op that returns the expected response.
#[utoipa::path(
    tag = "sentry-compat",
    put,
    path = "/0/projects/{org_slug}/{project_slug}/releases/{version}/",
    params(
        ("org_slug" = String, Path, description = "Organization slug (ignored)"),
        ("project_slug" = String, Path, description = "Project slug or numeric ID"),
        ("version" = String, Path, description = "Release version to finalize"),
    ),
    responses(
        (status = 200, description = "Release finalized", body = SentryReleaseResponse),
        (status = 401, description = "Unauthorized"),
        (status = 404, description = "Project not found"),
    ),
)]
async fn finalize_project_release(
    State(state): State<Arc<SentryCompatAppState>>,
    Path((_org_slug, project_slug, version)): Path<(String, String, String)>,
    headers: HeaderMap,
) -> impl IntoResponse {
    // Authenticate
    let auth_project_id = match authenticate_bearer(&headers, state.db.as_ref()).await {
        Ok(id) => id,
        Err((status, msg)) => return (status, msg).into_response(),
    };

    // Validate project slug matches the authenticated token
    if let Err((status, msg)) =
        resolve_project_slug(&project_slug, auth_project_id, state.db.as_ref()).await
    {
        return (status, msg).into_response();
    }

    debug!(
        "sentry-cli: Finalize release '{}' for project '{}' (stub)",
        version, project_slug
    );

    let now = Utc::now().to_rfc3339();
    let short_version = if version.len() > 12 {
        version[..12].to_string()
    } else {
        version.clone()
    };

    let response = SentryReleaseResponse {
        version: version.clone(),
        date_created: now.clone(),
        date_released: Some(now),
        short_version,
        projects: vec![SentryReleaseProjectRef {
            name: project_slug.clone(),
            slug: project_slug,
        }],
    };

    (StatusCode::OK, Json(response)).into_response()
}

/// Upload a source map file for a release.
///
/// Accepts the same multipart format as the Sentry release files API.
/// The `name` field should be the URL path of the file (e.g., `~/dist/bundle.js.map`).
///
/// The route has a 50 MiB body limit applied at the router level (Fix #4).
/// A per-field size check provides an additional defense-in-depth layer.
#[utoipa::path(
    tag = "sentry-compat",
    post,
    path = "/0/projects/{org_slug}/{project_slug}/releases/{version}/files/",
    params(
        ("org_slug" = String, Path, description = "Organization slug (ignored)"),
        ("project_slug" = String, Path, description = "Project slug or numeric ID"),
        ("version" = String, Path, description = "Release version"),
    ),
    responses(
        (status = 201, description = "File uploaded", body = SentryReleaseFileResponse),
        (status = 400, description = "Bad request"),
        (status = 401, description = "Unauthorized"),
        (status = 404, description = "Project not found"),
        (status = 413, description = "Source map file exceeds the 50 MiB per-field limit"),
    ),
)]
async fn upload_release_file(
    State(state): State<Arc<SentryCompatAppState>>,
    Path((_org_slug, project_slug, version)): Path<(String, String, String)>,
    headers: HeaderMap,
    mut multipart: Multipart,
) -> impl IntoResponse {
    // Authenticate
    let auth_project_id = match authenticate_bearer(&headers, state.db.as_ref()).await {
        Ok(id) => id,
        Err((status, msg)) => return (status, msg).into_response(),
    };

    // Resolve project slug
    let project_id =
        match resolve_project_slug(&project_slug, auth_project_id, state.db.as_ref()).await {
            Ok(id) => id,
            Err((status, msg)) => return (status, msg).into_response(),
        };

    // Parse multipart form
    let mut file_data: Option<Vec<u8>> = None;
    let mut file_name: Option<String> = None;
    let mut dist: Option<String> = None;

    while let Ok(Some(field)) = multipart.next_field().await {
        let name = field.name().unwrap_or_default().to_string();

        match name.as_str() {
            "file" => {
                // Use the upload filename if no explicit name is set
                if file_name.is_none() {
                    if let Some(filename) = field.file_name() {
                        file_name = Some(filename.to_string());
                    }
                }
                match field.bytes().await {
                    Ok(data) => {
                        // Fix #4 — defense-in-depth per-field size cap.
                        //
                        // The route-level DefaultBodyLimit fires first (50 MiB for the whole
                        // request). This per-field check is a second layer that catches a single
                        // oversized field even if the overall request body somehow slips through
                        // (e.g. when a future middleware changes the outer cap).
                        if data.len() > MAX_SOURCE_MAP_BYTES {
                            let field_name = file_name.as_deref().unwrap_or("<unknown>");
                            warn!(
                                field_name = field_name,
                                size_bytes = data.len(),
                                limit_bytes = MAX_SOURCE_MAP_BYTES,
                                "source map upload rejected: field exceeds size limit"
                            );
                            return (
                                StatusCode::PAYLOAD_TOO_LARGE,
                                format!(
                                    "source map exceeds {} bytes (got {})",
                                    MAX_SOURCE_MAP_BYTES,
                                    data.len()
                                ),
                            )
                                .into_response();
                        }
                        file_data = Some(data.to_vec());
                    }
                    Err(e) => {
                        return (
                            StatusCode::BAD_REQUEST,
                            format!("Failed to read file: {}", e),
                        )
                            .into_response();
                    }
                }
            }
            "name" => {
                if let Ok(text) = field.text().await {
                    file_name = Some(text);
                }
            }
            "dist" => {
                if let Ok(text) = field.text().await {
                    if !text.is_empty() {
                        dist = Some(text);
                    }
                }
            }
            "header" => {
                // sentry-cli sends "Sourcemap: <url>" headers — we don't need these
                // since we track source maps by file path
                let _ = field.bytes().await;
            }
            _ => {
                let _ = field.bytes().await;
            }
        }
    }

    let source_map_data = match file_data {
        Some(data) if !data.is_empty() => data,
        _ => {
            return (StatusCode::BAD_REQUEST, "No file data provided".to_string()).into_response();
        }
    };

    let name = match file_name {
        Some(n) if !n.is_empty() => n,
        _ => {
            return (StatusCode::BAD_REQUEST, "No file name provided".to_string()).into_response();
        }
    };

    // Only store actual source map files (.map extension).
    // sentry-cli also uploads the JS bundles alongside the maps — skip those.
    // Storing JS bundles would overwrite the correct source maps stored by capture_source_maps job.
    if !name.ends_with(".map") && !name.ends_with(".js.map") {
        debug!(
            "sentry-cli: Skipping non-source-map file '{}' for release '{}'",
            name, version
        );
        let now = Utc::now().to_rfc3339();
        let response = SentryReleaseFileResponse {
            id: "0".to_string(),
            name: name.clone(),
            dist,
            headers: serde_json::json!({}),
            size: 0,
            sha1: String::new(),
            date_created: now,
        };
        return (StatusCode::CREATED, Json(response)).into_response();
    }

    // Strip .map suffix so the stored path matches browser stack trace filenames.
    // e.g. "~/_next/server/app/api/route.js.map" → "~/_next/server/app/api/route.js"
    // This matches the convention used by the capture_source_maps job.
    let file_path = name.strip_suffix(".map").unwrap_or(&name).to_string();

    match state
        .source_map_service
        .upload(
            project_id,
            &version,
            &file_path,
            source_map_data,
            dist.clone(),
        )
        .await
    {
        Ok(info) => {
            debug!(
                "sentry-cli: Uploaded file '{}' for release '{}' (project {})",
                info.file_path, version, project_id
            );

            let response = SentryReleaseFileResponse {
                id: info.id.to_string(),
                name: info.file_path,
                dist,
                headers: serde_json::json!({
                    "Content-Type": "application/json"
                }),
                size: info.size_bytes,
                sha1: info.checksum.unwrap_or_default(),
                date_created: info.created_at.to_rfc3339(),
            };

            (StatusCode::CREATED, Json(response)).into_response()
        }
        Err(e) => {
            // sentry-cli uploads both JS bundles and .map files.
            // We only store actual source maps — JS bundles are accepted but not stored.
            // Return 201 so sentry-cli doesn't fail the build.
            warn!(
                "sentry-cli: Skipping non-source-map file '{}' for release '{}': {}",
                name, version, e
            );

            let now = Utc::now().to_rfc3339();
            let response = SentryReleaseFileResponse {
                id: "0".to_string(),
                name: name.clone(),
                dist,
                headers: serde_json::json!({}),
                size: 0,
                sha1: String::new(),
                date_created: now,
            };
            (StatusCode::CREATED, Json(response)).into_response()
        }
    }
}

/// List files for a release.
///
/// Returns all source maps stored for a specific release in sentry-cli compatible format.
#[utoipa::path(
    tag = "sentry-compat",
    get,
    path = "/0/projects/{org_slug}/{project_slug}/releases/{version}/files/",
    params(
        ("org_slug" = String, Path, description = "Organization slug (ignored)"),
        ("project_slug" = String, Path, description = "Project slug or numeric ID"),
        ("version" = String, Path, description = "Release version"),
    ),
    responses(
        (status = 200, description = "List of release files", body = Vec<SentryReleaseFileResponse>),
        (status = 401, description = "Unauthorized"),
        (status = 404, description = "Project not found"),
    ),
)]
async fn list_release_files(
    State(state): State<Arc<SentryCompatAppState>>,
    Path((_org_slug, project_slug, version)): Path<(String, String, String)>,
    headers: HeaderMap,
) -> impl IntoResponse {
    // Authenticate
    let auth_project_id = match authenticate_bearer(&headers, state.db.as_ref()).await {
        Ok(id) => id,
        Err((status, msg)) => return (status, msg).into_response(),
    };

    // Resolve project slug
    let project_id =
        match resolve_project_slug(&project_slug, auth_project_id, state.db.as_ref()).await {
            Ok(id) => id,
            Err((status, msg)) => return (status, msg).into_response(),
        };

    match state
        .source_map_service
        .list_for_release(project_id, &version)
        .await
    {
        Ok(maps) => {
            let files: Vec<SentryReleaseFileResponse> = maps
                .into_iter()
                .map(|info| SentryReleaseFileResponse {
                    id: info.id.to_string(),
                    name: info.file_path,
                    dist: info.dist,
                    headers: serde_json::json!({
                        "Content-Type": "application/json"
                    }),
                    size: info.size_bytes,
                    sha1: info.checksum.unwrap_or_default(),
                    date_created: info.created_at.to_rfc3339(),
                })
                .collect();

            (StatusCode::OK, Json(files)).into_response()
        }
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("Failed to list files: {}", e),
        )
            .into_response(),
    }
}

/// Chunk upload options (stub for sentry-cli compatibility).
///
/// sentry-cli checks this endpoint to determine if chunk-based upload is supported.
/// We return a response indicating that chunk upload is NOT supported, which forces
/// sentry-cli to fall back to the standard file-by-file upload.
#[utoipa::path(
    tag = "sentry-compat",
    get,
    path = "/0/organizations/{org_slug}/chunk-upload/",
    params(
        ("org_slug" = String, Path, description = "Organization slug (ignored)")
    ),
    responses(
        (status = 200, description = "Chunk upload options", body = SentryChunkUploadResponse),
    ),
)]
async fn chunk_upload_options(Path(_org_slug): Path<String>) -> impl IntoResponse {
    // Return options that effectively disable chunk upload
    // sentry-cli will fall back to file-by-file upload
    let response = SentryChunkUploadResponse {
        url: String::new(),
        chunk_size: 8 * 1024 * 1024, // 8MB
        chunks_per_request: 64,
        max_file_size: 50 * 1024 * 1024, // 50MB
        max_request_size: 50 * 1024 * 1024,
        concurrency: 1,
        hash_algorithm: "sha1".to_string(),
        compression: vec!["gzip".to_string()],
        accept: vec![], // Empty accept = no chunk upload support
    };

    Json(response)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_bearer_token_extraction() {
        // Test that auth header parsing logic is correct
        let auth = "Bearer abc123def456";
        let token = auth.strip_prefix("Bearer ").unwrap();
        assert_eq!(token, "abc123def456");
    }

    #[test]
    fn test_dsn_token_extraction() {
        let auth = "DSN abc123def456";
        let token = auth.strip_prefix("DSN ").unwrap();
        assert_eq!(token, "abc123def456");
    }

    #[test]
    fn test_short_version() {
        let version = "abc123def456789";
        let short = if version.len() > 12 {
            version[..12].to_string()
        } else {
            version.to_string()
        };
        assert_eq!(short, "abc123def456");

        let short_version = "1.0.0";
        let short = if short_version.len() > 12 {
            short_version[..12].to_string()
        } else {
            short_version.to_string()
        };
        assert_eq!(short, "1.0.0");
    }

    // === Fix #4 — body-limit constant sanity checks ===

    /// The upload body-limit constant must be exactly 50 MiB.
    ///
    /// Source maps for large production apps can be several MiB each. 50 MiB
    /// is a reasonable ceiling for a full release upload while still bounding
    /// abuse from unauthenticated or semi-authenticated callers.
    #[test]
    fn test_sentry_upload_body_limit_is_50mib() {
        assert_eq!(
            SENTRY_UPLOAD_BODY_LIMIT,
            50 * 1024 * 1024,
            "Sentry upload body limit must be 50 MiB"
        );
    }

    /// The per-field cap must match the route-level cap.
    ///
    /// If MAX_SOURCE_MAP_BYTES were larger than SENTRY_UPLOAD_BODY_LIMIT, the
    /// per-field check would never fire (the outer cap fires first). Keep them
    /// equal so both layers are meaningful.
    #[test]
    fn test_per_field_cap_matches_route_cap() {
        assert_eq!(
            MAX_SOURCE_MAP_BYTES, SENTRY_UPLOAD_BODY_LIMIT,
            "Per-field cap and route-level cap must be equal"
        );
    }
}
