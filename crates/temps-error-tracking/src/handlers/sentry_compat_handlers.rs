// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Sentry CLI-compatible API endpoints
//!
//! Provides endpoints that match the Sentry API format used by `sentry-cli sourcemaps upload`.
//! This allows using the standard sentry-cli tool to upload source maps to Temps.
//!
//! ## Authentication
//!
//! These endpoints accept `Authorization: Bearer <deployment_token>` (a `dt_…`
//! project deployment token), which is how sentry-cli sends `SENTRY_AUTH_TOKEN`.
//! Temps injects a deployment token as `SENTRY_AUTH_TOKEN` into builds, so
//! uploads from a Temps build work without configuration; for uploads from
//! elsewhere (CI, a laptop), create a deployment token for the project and set
//! it as `SENTRY_AUTH_TOKEN`.
//!
//! The DSN public key is **not** accepted here. It is embedded in client-side
//! bundles by design (it only authorises event ingest), so anyone who reads a
//! site's JavaScript has it; accepting it for writes would let them overwrite
//! the project's source maps and releases. Event ingest (`/api/{id}/envelope/`
//! and friends) still authenticates with the DSN key and is unaffected.
//!
//! ## Endpoint mapping
//!
//! | sentry-cli endpoint | Temps handler |
//! |---------------------|---------------|
//! | `POST /api/0/organizations/{org}/releases/` | `create_release` (stub) |
//! | `POST /api/0/projects/{org}/{project}/releases/{version}/files/` | `upload_release_file` |
//! | `GET /api/0/projects/{org}/{project}/releases/{version}/files/` | `list_release_files` |
//! | `GET /api/0/organizations/{org}/chunk-upload/` | `chunk_upload_options` |

use axum::{
    extract::{DefaultBodyLimit, Multipart, Path, State},
    http::{HeaderMap, StatusCode},
    response::IntoResponse,
    routing::{get, post, put},
    Extension, Json, Router,
};
use chrono::Utc;
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use temps_core::RequestMetadata;
use tracing::{debug, error, warn};
use utoipa::{OpenApi, ToSchema};

use crate::handlers::audit::{
    SentryCliAuditContext, SentryCliCredential, SentryCliReleaseAction, SentryCliReleaseAudit,
    SentryCliReleaseFileUploadedAudit,
};
use crate::services::source_map_service::{BuildCredentialError, SourceMapService};

#[derive(OpenApi)]
#[openapi(
    paths(
        create_release,
        create_project_release,
        finalize_project_release,
        upload_release_file,
        list_release_files,
        chunk_upload_options,
        upload_artifact_chunk,
        assemble_artifact_bundle,
    ),
    components(schemas(
        SentryCreateReleaseRequest,
        SentryReleaseResponse,
        SentryReleaseFileResponse,
        SentryChunkUploadResponse,
        ArtifactAssembleRequest,
        crate::services::source_map_service::ArtifactAssembly,
    )),
    tags(
        (name = "sentry-compat", description = "Sentry CLI-compatible API endpoints for source map uploads")
    )
)]
pub struct SentryCompatApiDoc;

#[derive(Clone)]
pub struct SentryCompatAppState {
    pub source_map_service: Arc<SourceMapService>,
    pub audit_service: Arc<dyn temps_core::AuditLogger>,
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

#[derive(Deserialize, ToSchema)]
pub struct ArtifactAssembleRequest {
    pub checksum: String,
    pub chunks: Vec<String>,
    pub projects: Vec<String>,
    pub version: Option<String>,
    pub dist: Option<String>,
}

// --- Route configuration ---

/// Maximum body size for the sentry-compat source map upload routes (50 MiB).
///
/// Uploads require a secret deployment token (see [`authenticate_bearer`]),
/// but the outer body limit still rejects oversized uploads before they reach
/// the multipart reader. A per-field inline check (`MAX_SOURCE_MAP_BYTES`)
/// provides additional defense-in-depth.
pub const SENTRY_UPLOAD_BODY_LIMIT: usize =
    crate::services::source_map_service::MAX_SOURCE_MAP_BYTES;

/// Maximum size for a single source map file field (50 MiB).
///
/// Applied after the compressed body has been received. This is a second line
/// of defense: the outer `DefaultBodyLimit` cap fires first, but if multiple
/// small files add up the per-field check catches any single oversized field.
use crate::services::source_map_service::MAX_SOURCE_MAP_BYTES;

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
    let artifact_routes = Router::new()
        .route(
            "/0/organizations/{org_slug}/chunk-upload/",
            post(upload_artifact_chunk),
        )
        .layer(DefaultBodyLimit::max(
            crate::services::source_map_service::CHUNK_BYTES + 65536,
        ));

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
        .route(
            "/0/organizations/{org_slug}/artifactbundle/assemble/",
            post(assemble_artifact_bundle),
        )
        .merge(artifact_routes)
        .merge(upload_routes)
}

// --- Auth helper ---

/// The project and credential a sentry-cli request authenticated as.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct BearerPrincipal {
    project_id: i32,
    credential: SentryCliCredential,
}

/// The first 12 characters of a release version. Counted in characters because
/// the version is caller-supplied text, and byte 12 can land inside one, which
/// panics a byte slice.
fn short_version(version: &str) -> String {
    version.chars().take(12).collect()
}

fn sentry_cli_audit_context(
    principal: BearerPrincipal,
    metadata: &RequestMetadata,
) -> SentryCliAuditContext {
    SentryCliAuditContext {
        credential: principal.credential,
        ip_address: Some(metadata.ip_address.clone()),
        user_agent: metadata.user_agent.clone(),
    }
}

/// Record a sentry-cli release create/finalize call. An audit failure is
/// logged and does not fail the request (sentry-cli would abort the build).
async fn record_release_audit(
    state: &SentryCompatAppState,
    principal: BearerPrincipal,
    metadata: &RequestMetadata,
    version: &str,
    action: SentryCliReleaseAction,
) {
    let audit = SentryCliReleaseAudit {
        context: sentry_cli_audit_context(principal, metadata),
        project_id: principal.project_id,
        version: version.to_string(),
        action,
    };
    if let Err(e) = state.audit_service.create_audit_log(&audit).await {
        error!(
            project_id = principal.project_id,
            "Failed to create sentry-cli release audit log for release '{}': {}", version, e
        );
    }
}

/// Prefix every Temps deployment token carries.
const DEPLOYMENT_TOKEN_PREFIX: &str = "dt_";

/// Pull the credential out of the `Authorization` header and require it to be
/// a deployment token.
///
/// Pure (no DB) so the credential policy is unit-testable. Anything that is
/// not a `dt_…` token — notably a DSN public key, which ships in client JS and
/// is therefore not a secret — is rejected here, before any lookup.
fn extract_deployment_token(auth_header: &str) -> Result<&str, (StatusCode, String)> {
    let token = if let Some(token) = auth_header.strip_prefix("Bearer ") {
        token.trim()
    } else if auth_header.starts_with("DSN ") {
        return Err((
            StatusCode::UNAUTHORIZED,
            "DSN public keys cannot authorize release or source map uploads: they are \
             embedded in client bundles and are not secret. Use a project deployment token \
             (dt_...) as SENTRY_AUTH_TOKEN, sent as 'Authorization: Bearer <token>'."
                .to_string(),
        ));
    } else {
        return Err((
            StatusCode::UNAUTHORIZED,
            "Invalid Authorization format. Expected 'Bearer <deployment token>'".to_string(),
        ));
    };

    if !token.starts_with(DEPLOYMENT_TOKEN_PREFIX) || token.len() <= DEPLOYMENT_TOKEN_PREFIX.len() {
        return Err((
            StatusCode::UNAUTHORIZED,
            "Release and source map uploads require a project deployment token (dt_...) as \
             SENTRY_AUTH_TOKEN. DSN public keys are not accepted for uploads because they are \
             embedded in client bundles and are not secret."
                .to_string(),
        ));
    }

    Ok(token)
}

fn build_credential_problem(error: BuildCredentialError) -> (StatusCode, String) {
    let status = match &error {
        BuildCredentialError::Unauthorized | BuildCredentialError::InactiveProject { .. } => {
            StatusCode::UNAUTHORIZED
        }
        BuildCredentialError::Forbidden { .. } => StatusCode::FORBIDDEN,
        BuildCredentialError::ProjectNotFound { .. } => StatusCode::NOT_FOUND,
        BuildCredentialError::Storage { .. } => {
            tracing::error!(error = %error, "Build credential service failed");
            return (
                StatusCode::SERVICE_UNAVAILABLE,
                "Build credential lookup temporarily unavailable".into(),
            );
        }
    };
    (status, error.to_string())
}
/// Extract project_id from Bearer token authentication.
///
/// sentry-cli sends `Authorization: Bearer <auth_token>`. The only accepted
/// credential is an active, unexpired project deployment token (`dt_*`),
/// resolved via the `deployment_tokens` table — this is what Temps injects as
/// `SENTRY_AUTH_TOKEN` during builds.
///
/// The DSN public key is deliberately NOT accepted: it is a write-only ingest
/// credential that is public by design (embedded in browser bundles), so it
/// must not authorize source map / release writes.
async fn authenticate_bearer(
    headers: &HeaderMap,
    service: &SourceMapService,
) -> Result<BearerPrincipal, (StatusCode, String)> {
    let header = headers
        .get("authorization")
        .and_then(|value| value.to_str().ok())
        .ok_or((
            StatusCode::UNAUTHORIZED,
            "Missing Authorization header".into(),
        ))?;
    let token = extract_deployment_token(header)?;
    let credential = service
        .authenticate_build_token(token)
        .await
        .map_err(build_credential_problem)?;
    Ok(BearerPrincipal {
        project_id: credential.project_id,
        credential: SentryCliCredential::DeploymentToken(credential.token_id),
    })
}
async fn resolve_project_slug(
    slug: &str,
    project: i32,
    service: &SourceMapService,
) -> Result<i32, (StatusCode, String)> {
    service
        .resolve_build_project(slug, project)
        .await
        .map_err(build_credential_problem)
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
    Extension(metadata): Extension<RequestMetadata>,
    headers: HeaderMap,
    Json(request): Json<SentryCreateReleaseRequest>,
) -> impl IntoResponse {
    // Authenticate
    let principal = match artifact_principal(&headers, &state).await {
        Ok(principal) => principal,
        Err((status, msg)) => return (status, msg).into_response(),
    };

    debug!("sentry-cli: Create release '{}' (stub)", request.version);
    record_release_audit(
        &state,
        principal,
        &metadata,
        &request.version,
        SentryCliReleaseAction::Created,
    )
    .await;

    let now = Utc::now().to_rfc3339();
    let short_version = short_version(&request.version);

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
    Extension(metadata): Extension<RequestMetadata>,
    headers: HeaderMap,
    Json(request): Json<SentryCreateReleaseRequest>,
) -> impl IntoResponse {
    // Authenticate
    let principal = match artifact_principal(&headers, &state).await {
        Ok(principal) => principal,
        Err((status, msg)) => return (status, msg).into_response(),
    };

    // Validate project slug matches the authenticated token
    if let Err((status, msg)) = resolve_project_slug(
        &project_slug,
        principal.project_id,
        state.source_map_service.as_ref(),
    )
    .await
    {
        return (status, msg).into_response();
    }

    debug!(
        "sentry-cli: Create project release '{}' for project '{}' (stub)",
        request.version, project_slug
    );
    record_release_audit(
        &state,
        principal,
        &metadata,
        &request.version,
        SentryCliReleaseAction::Created,
    )
    .await;

    let now = Utc::now().to_rfc3339();
    let short_version = short_version(&request.version);

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
    Extension(metadata): Extension<RequestMetadata>,
    headers: HeaderMap,
) -> impl IntoResponse {
    // Authenticate
    let principal = match artifact_principal(&headers, &state).await {
        Ok(principal) => principal,
        Err((status, msg)) => return (status, msg).into_response(),
    };

    // Validate project slug matches the authenticated token
    if let Err((status, msg)) = resolve_project_slug(
        &project_slug,
        principal.project_id,
        state.source_map_service.as_ref(),
    )
    .await
    {
        return (status, msg).into_response();
    }

    debug!(
        "sentry-cli: Finalize release '{}' for project '{}' (stub)",
        version, project_slug
    );
    record_release_audit(
        &state,
        principal,
        &metadata,
        &version,
        SentryCliReleaseAction::Finalized,
    )
    .await;

    let now = Utc::now().to_rfc3339();
    let short_version = short_version(&version);

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
    Extension(metadata): Extension<RequestMetadata>,
    headers: HeaderMap,
    mut multipart: Multipart,
) -> impl IntoResponse {
    // Authenticate
    let principal = match artifact_principal(&headers, &state).await {
        Ok(principal) => principal,
        Err((status, msg)) => return (status, msg).into_response(),
    };

    // Resolve project slug
    let project_id = match resolve_project_slug(
        &project_slug,
        principal.project_id,
        state.source_map_service.as_ref(),
    )
    .await
    {
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

            let audit = SentryCliReleaseFileUploadedAudit {
                context: sentry_cli_audit_context(principal, &metadata),
                project_id,
                source_map_id: info.id,
                version: version.clone(),
                file_path: info.file_path.clone(),
                dist: dist.clone(),
                size_bytes: info.size_bytes,
            };
            if let Err(e) = state.audit_service.create_audit_log(&audit).await {
                error!(
                    project_id,
                    source_map_id = info.id,
                    "Failed to create sentry-cli source map upload audit log: {}",
                    e
                );
            }

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
    let principal = match artifact_principal(&headers, &state).await {
        Ok(principal) => principal,
        Err((status, msg)) => return (status, msg).into_response(),
    };

    // Resolve project slug
    let project_id = match resolve_project_slug(
        &project_slug,
        principal.project_id,
        state.source_map_service.as_ref(),
    )
    .await
    {
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

/// Negotiates only the artifact capability implemented here. Build credentials
/// are required; browser DSN public keys cannot upload or read source artifacts.
#[utoipa::path(get, path="/0/organizations/{org_slug}/chunk-upload/",tag="sentry-compat",
    params(("org_slug"=String,Path)), responses((status=200,body=SentryChunkUploadResponse),(status=401),(status=403)))]
async fn chunk_upload_options(
    State(state): State<Arc<SentryCompatAppState>>,
    Path(org): Path<String>,
    headers: HeaderMap,
) -> Result<Json<SentryChunkUploadResponse>, (StatusCode, String)> {
    artifact_principal(&headers, &state).await?;
    if org.is_empty()
        || org.len() > 128
        || !org
            .bytes()
            .all(|c| c.is_ascii_alphanumeric() || c == b'-' || c == b'_')
    {
        return Err((StatusCode::BAD_REQUEST, "Invalid organization path".into()));
    }
    Ok(Json(SentryChunkUploadResponse {
        url: format!("/organizations/{org}/chunk-upload/"),
        chunk_size: 8 * 1024 * 1024,
        chunks_per_request: 1,
        max_file_size: 50 * 1024 * 1024,
        max_request_size: 8 * 1024 * 1024 + 65536,
        concurrency: 1,
        hash_algorithm: "sha1".into(),
        compression: vec![],
        accept: vec!["artifact_bundles".into(), "artifact_bundles_v2".into()],
    }))
}
async fn artifact_principal(
    headers: &HeaderMap,
    state: &SentryCompatAppState,
) -> Result<BearerPrincipal, (StatusCode, String)> {
    let principal = authenticate_bearer(headers, &state.source_map_service).await?;
    if !matches!(
        principal.credential,
        SentryCliCredential::DeploymentToken(_)
    ) {
        return Err((
            StatusCode::FORBIDDEN,
            "Artifact uploads require a project deployment token; public DSNs are ingestion-only"
                .into(),
        ));
    }
    Ok(principal)
}
fn artifact_error(
    error: crate::services::source_map_service::ArtifactUploadError,
) -> (StatusCode, String) {
    use crate::services::source_map_service::ArtifactUploadError;
    match error {
        ArtifactUploadError::Invalid { .. } => (StatusCode::BAD_REQUEST, error.to_string()),
        ArtifactUploadError::Quota(_) => (StatusCode::TOO_MANY_REQUESTS, error.to_string()),
        _ => {
            error!(error_class = "artifact_storage", "Artifact upload failed");
            (
                StatusCode::SERVICE_UNAVAILABLE,
                "Artifact storage temporarily unavailable".into(),
            )
        }
    }
}
#[utoipa::path(post,path="/0/organizations/{org_slug}/chunk-upload/",tag="sentry-compat",
    params(("org_slug"=String,Path)),responses((status=200),(status=400),(status=401),(status=403),(status=429)))]
async fn upload_artifact_chunk(
    State(state): State<Arc<SentryCompatAppState>>,
    Path(_org): Path<String>,
    headers: HeaderMap,
    mut multipart: Multipart,
) -> Result<StatusCode, (StatusCode, String)> {
    use crate::services::source_map_service::{ARTIFACT_WORKERS, CHUNK_BYTES};
    let principal = artifact_principal(&headers, &state).await?;
    let _permit = ARTIFACT_WORKERS.try_acquire().map_err(|_| {
        (
            StatusCode::TOO_MANY_REQUESTS,
            "Artifact upload busy; retry shortly".into(),
        )
    })?;
    let mut chunks = Vec::new();
    while let Some(mut field) = multipart.next_field().await.map_err(|_| {
        (
            StatusCode::BAD_REQUEST,
            "Invalid chunk multipart request".into(),
        )
    })? {
        if !chunks.is_empty() || field.name() != Some("file") {
            return Err((
                StatusCode::BAD_REQUEST,
                "Expected one uncompressed file chunk".into(),
            ));
        }
        let checksum = field
            .file_name()
            .ok_or((StatusCode::BAD_REQUEST, "Missing chunk checksum".into()))?
            .to_owned();
        let mut bytes = Vec::new();
        while let Some(part) = field
            .chunk()
            .await
            .map_err(|_| (StatusCode::BAD_REQUEST, "Invalid chunk body".into()))?
        {
            if bytes.len() + part.len() > CHUNK_BYTES {
                return Err((StatusCode::PAYLOAD_TOO_LARGE, "Chunk exceeds 8 MiB".into()));
            }
            bytes.extend_from_slice(&part);
        }
        chunks.push((checksum, bytes));
    }
    state
        .source_map_service
        .stage_artifact_chunks(principal.project_id, chunks)
        .await
        .map_err(artifact_error)?;
    Ok(StatusCode::OK)
}
#[utoipa::path(post,path="/0/organizations/{org_slug}/artifactbundle/assemble/",tag="sentry-compat",
    params(("org_slug"=String,Path)),request_body=ArtifactAssembleRequest,
    responses((status=200,body=crate::services::source_map_service::ArtifactAssembly),(status=400),(status=401),(status=403),(status=429)))]
async fn assemble_artifact_bundle(
    State(state): State<Arc<SentryCompatAppState>>,
    Path(_org): Path<String>,
    headers: HeaderMap,
    Extension(metadata): Extension<RequestMetadata>,
    Json(request): Json<ArtifactAssembleRequest>,
) -> Result<Json<crate::services::source_map_service::ArtifactAssembly>, (StatusCode, String)> {
    let principal = artifact_principal(&headers, &state).await?;
    if request.projects.len() != 1 {
        return Err((
            StatusCode::BAD_REQUEST,
            "Exactly one scoped project is required".into(),
        ));
    }
    resolve_project_slug(
        &request.projects[0],
        principal.project_id,
        &state.source_map_service,
    )
    .await?;
    let _permit = crate::services::source_map_service::ARTIFACT_WORKERS
        .try_acquire()
        .map_err(|_| {
            (
                StatusCode::TOO_MANY_REQUESTS,
                "Artifact assembly busy; retry shortly".into(),
            )
        })?;
    let checksum = request.checksum.clone();
    let result = state
        .source_map_service
        .assemble_artifact(
            principal.project_id,
            request.checksum,
            request.chunks,
            request.version,
            request.dist,
        )
        .await
        .map_err(artifact_error)?;
    if result.state == "ok" {
        let audit = crate::handlers::audit::SentryArtifactBundleUploadedAudit {
            context: sentry_cli_audit_context(principal, &metadata),
            project_id: principal.project_id,
            checksum,
        };
        if let Err(error) = state.audit_service.create_audit_log(&audit).await {
            error!(project_id=principal.project_id,%error,"Artifact upload audit failed");
        }
    }
    Ok(Json(result))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_bearer_deployment_token_is_accepted() {
        assert_eq!(
            extract_deployment_token("Bearer dt_abc123def456"),
            Ok("dt_abc123def456")
        );
        assert_eq!(
            extract_deployment_token("Bearer   dt_abc123  "),
            Ok("dt_abc123")
        );
    }

    /// F-010: a DSN public key (any non-`dt_` bearer) must never authorize an
    /// upload — it is embedded in client bundles.
    #[test]
    fn test_dsn_public_key_is_rejected_as_bearer() {
        let (status, detail) = extract_deployment_token("Bearer 0123456789abcdef0123456789abcdef")
            .expect_err("a DSN public key must not authenticate uploads");
        assert_eq!(status, StatusCode::UNAUTHORIZED);
        assert!(detail.contains("deployment token"), "{detail}");
    }

    #[test]
    fn test_dsn_scheme_is_rejected() {
        let (status, detail) = extract_deployment_token("DSN abc123def456")
            .expect_err("the DSN auth scheme must not authenticate uploads");
        assert_eq!(status, StatusCode::UNAUTHORIZED);
        assert!(detail.contains("DSN public keys"), "{detail}");
        // Even if it carries something token-shaped.
        assert!(extract_deployment_token("DSN dt_abc").is_err());
    }

    #[test]
    fn test_malformed_authorization_is_rejected() {
        for header in [
            "",
            "Bearer ",
            "Bearer dt_",
            "Basic dt_abc",
            "dt_abc",
            "bearer dt_abc",
        ] {
            let (status, _) = extract_deployment_token(header)
                .expect_err("malformed Authorization must be rejected");
            assert_eq!(status, StatusCode::UNAUTHORIZED, "{header:?}");
        }
    }

    #[test]
    fn test_short_version() {
        assert_eq!(short_version("abc123def456789"), "abc123def456");
        assert_eq!(short_version("1.0.0"), "1.0.0");
        assert_eq!(short_version(""), "");
    }

    #[test]
    fn short_version_does_not_split_a_multibyte_character() {
        // Byte 12 falls inside the euro sign, which a byte slice panics on.
        assert_eq!(short_version("aaaaaaaaaaa€bbb"), "aaaaaaaaaaa€");
        assert_eq!(
            short_version("日本語のリリース名前です長い"),
            "日本語のリリース名前です"
        );
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
