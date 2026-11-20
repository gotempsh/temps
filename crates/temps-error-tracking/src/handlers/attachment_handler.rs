// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Attachments sent with error events and feedback (ADR 111, D4): list them,
//! download one, delete one, and see or change the project's quota.

use std::sync::Arc;

use axum::{
    body::Body,
    extract::{Path, State},
    http::{header, HeaderValue, StatusCode},
    response::Response,
    routing::{delete, get, put},
    Extension, Json, Router,
};
use serde::{Deserialize, Serialize};
use temps_authz::{permission_guard, project_access_guard, project_scope_guard, RequireAuth};
use temps_core::problemdetails::Problem;
use temps_core::RequestMetadata;
use utoipa::{OpenApi, ToSchema};

use super::audit::{AuditContext, ErrorAttachmentDeletedAudit, ErrorAttachmentQuotaAudit};
use super::types::AppState;
use crate::services::attachments::{is_inline_image, AttachmentUsage, AttachmentView};

#[derive(OpenApi)]
#[openapi(
    paths(
        list_error_event_attachments,
        list_feedback_attachments,
        get_error_attachment_content,
        delete_error_attachment,
        get_error_attachment_usage,
        set_error_attachment_quota,
    ),
    components(schemas(
        AttachmentView,
        AttachmentList,
        AttachmentUsage,
        SetAttachmentQuotaRequest
    )),
    tags((name = "error-tracking", description = "Error tracking"))
)]
pub struct AttachmentApiDoc;

pub fn routes() -> Router<Arc<AppState>> {
    Router::new()
        .route(
            "/projects/{project_id}/error-groups/{group_id}/events/{event_id}/attachments",
            get(list_error_event_attachments),
        )
        .route(
            "/projects/{project_id}/feedback/{feedback_id}/attachments",
            get(list_feedback_attachments),
        )
        .route(
            "/projects/{project_id}/error-attachments/usage",
            get(get_error_attachment_usage),
        )
        .route(
            "/projects/{project_id}/error-attachments/quota",
            put(set_error_attachment_quota),
        )
        .route(
            "/projects/{project_id}/error-attachments/{attachment_id}",
            delete(delete_error_attachment),
        )
        .route(
            "/projects/{project_id}/error-attachments/{attachment_id}/content",
            get(get_error_attachment_content),
        )
}

/// The attachments of an event or feedback, in the order the SDK sent them.
#[derive(Debug, Serialize, Deserialize, ToSchema)]
pub struct AttachmentList {
    pub data: Vec<AttachmentView>,
}

#[derive(Debug, Serialize, Deserialize, ToSchema)]
pub struct SetAttachmentQuotaRequest {
    /// Bytes of attachments the project may store, 0 to 100 GiB. Lowering it
    /// deletes nothing: new attachments are dropped until use falls under it.
    pub quota_bytes: i64,
}

/// List the attachments of an error event
#[utoipa::path(
    get,
    path = "/projects/{project_id}/error-groups/{group_id}/events/{event_id}/attachments",
    responses(
        (status = 200, description = "Attachments of the event", body = AttachmentList),
        (status = 401, description = "Not authenticated"),
        (status = 403, description = "No access to the project"),
        (status = 404, description = "No such event in the group"),
        (status = 500, description = "Internal server error")
    ),
    params(
        ("project_id" = i32, Path, description = "Project ID"),
        ("group_id" = i32, Path, description = "Error group ID"),
        ("event_id" = i64, Path, description = "Error event ID")
    ),
    tag = "error-tracking",
    security(("bearer_auth" = []))
)]
pub async fn list_error_event_attachments(
    State(state): State<Arc<AppState>>,
    RequireAuth(auth): RequireAuth,
    Path((project_id, group_id, event_id)): Path<(i32, i32, i64)>,
) -> Result<Json<AttachmentList>, Problem> {
    permission_guard!(auth, ErrorTrackingRead);
    project_scope_guard!(auth, project_id);
    project_access_guard!(auth, project_id, state.project_access_checker);
    let data = state
        .error_tracking_service
        .list_event_attachments(project_id, group_id, event_id)
        .await?;
    Ok(Json(AttachmentList { data }))
}

/// List the attachments of a feedback, such as its screenshot
#[utoipa::path(
    get,
    path = "/projects/{project_id}/feedback/{feedback_id}/attachments",
    responses(
        (status = 200, description = "Attachments of the feedback", body = AttachmentList),
        (status = 401, description = "Not authenticated"),
        (status = 403, description = "No access to the project"),
        (status = 404, description = "No such feedback in the project"),
        (status = 500, description = "Internal server error")
    ),
    params(
        ("project_id" = i32, Path, description = "Project ID"),
        ("feedback_id" = i64, Path, description = "Feedback ID")
    ),
    tag = "error-tracking",
    security(("bearer_auth" = []))
)]
pub async fn list_feedback_attachments(
    State(state): State<Arc<AppState>>,
    RequireAuth(auth): RequireAuth,
    Path((project_id, feedback_id)): Path<(i32, i64)>,
) -> Result<Json<AttachmentList>, Problem> {
    permission_guard!(auth, ErrorTrackingRead);
    project_scope_guard!(auth, project_id);
    project_access_guard!(auth, project_id, state.project_access_checker);
    let data = state
        .error_tracking_service
        .list_feedback_attachments(project_id, feedback_id)
        .await?;
    Ok(Json(AttachmentList { data }))
}

/// Quote a file name for `Content-Disposition`: an ASCII fallback, and the
/// exact name percent-encoded (RFC 6266, RFC 8187).
pub(crate) fn content_disposition(inline: bool, filename: &str) -> String {
    let fallback: String = filename
        .chars()
        .map(|c| {
            if c.is_ascii_graphic() && c != '"' && c != '\\' && c != '%' || c == ' ' {
                c
            } else {
                '_'
            }
        })
        .collect();
    let mut encoded = String::with_capacity(filename.len() * 3);
    for byte in filename.bytes() {
        if byte.is_ascii_alphanumeric() || b"!#$&+-.^_`|~".contains(&byte) {
            encoded.push(byte as char);
        } else {
            encoded.push_str(&format!("%{byte:02X}"));
        }
    }
    format!(
        "{}; filename=\"{fallback}\"; filename*=UTF-8''{encoded}",
        if inline { "inline" } else { "attachment" }
    )
}

/// The response serving a stored attachment. PNG, JPEG, GIF and WebP are
/// shown inline; every other type is a download of opaque bytes, so a stored
/// HTML or SVG file never runs in the console's origin.
pub(crate) fn attachment_response(view: &AttachmentView, bytes: axum::body::Bytes) -> Response {
    let inline = is_inline_image(&view.content_type);
    let content_type = if inline {
        view.content_type.as_str()
    } else {
        "application/octet-stream"
    };
    let mut response = Response::new(Body::from(bytes));
    let headers = response.headers_mut();
    let mut set = |name: header::HeaderName, value: &str| {
        if let Ok(value) = HeaderValue::from_str(value) {
            headers.insert(name, value);
        }
    };
    set(header::CONTENT_TYPE, content_type);
    set(
        header::CONTENT_DISPOSITION,
        &content_disposition(inline, &view.filename),
    );
    set(header::X_CONTENT_TYPE_OPTIONS, "nosniff");
    set(
        header::CONTENT_SECURITY_POLICY,
        "default-src 'none'; img-src 'self'; sandbox",
    );
    set(header::CACHE_CONTROL, "private, no-store");
    response
}

/// Download an attachment
#[utoipa::path(
    get,
    path = "/projects/{project_id}/error-attachments/{attachment_id}/content",
    responses(
        (status = 200, description = "The file. PNG, JPEG, GIF and WebP are served inline; everything else as application/octet-stream to save", content_type = "application/octet-stream"),
        (status = 401, description = "Not authenticated"),
        (status = 403, description = "No access to the project"),
        (status = 404, description = "No such attachment in the project"),
        (status = 503, description = "Blob storage is not set up or not answering"),
        (status = 500, description = "Internal server error")
    ),
    params(
        ("project_id" = i32, Path, description = "Project ID"),
        ("attachment_id" = i64, Path, description = "Attachment ID")
    ),
    tag = "error-tracking",
    security(("bearer_auth" = []))
)]
pub async fn get_error_attachment_content(
    State(state): State<Arc<AppState>>,
    RequireAuth(auth): RequireAuth,
    Path((project_id, attachment_id)): Path<(i32, i64)>,
) -> Result<Response, Problem> {
    permission_guard!(auth, ErrorTrackingRead);
    project_scope_guard!(auth, project_id);
    project_access_guard!(auth, project_id, state.project_access_checker);
    let (view, bytes) = state
        .error_tracking_service
        .attachments
        .content(project_id, attachment_id)
        .await?;
    Ok(attachment_response(&view, bytes))
}

/// Delete an attachment
#[utoipa::path(
    delete,
    path = "/projects/{project_id}/error-attachments/{attachment_id}",
    responses(
        (status = 204, description = "Attachment deleted; its bytes are back in the quota"),
        (status = 401, description = "Not authenticated"),
        (status = 403, description = "Error tracking write permission required"),
        (status = 404, description = "No such attachment in the project"),
        (status = 503, description = "Blob storage is not set up or the file could not be deleted from it"),
        (status = 500, description = "Internal server error")
    ),
    params(
        ("project_id" = i32, Path, description = "Project ID"),
        ("attachment_id" = i64, Path, description = "Attachment ID")
    ),
    tag = "error-tracking",
    security(("bearer_auth" = []))
)]
pub async fn delete_error_attachment(
    State(state): State<Arc<AppState>>,
    RequireAuth(auth): RequireAuth,
    Path((project_id, attachment_id)): Path<(i32, i64)>,
    Extension(metadata): Extension<RequestMetadata>,
) -> Result<StatusCode, Problem> {
    permission_guard!(auth, ErrorTrackingWrite);
    project_scope_guard!(auth, project_id);
    project_access_guard!(auth, project_id, state.project_access_checker);
    let deleted = state
        .error_tracking_service
        .attachments
        .delete(project_id, attachment_id)
        .await?;
    let audit = ErrorAttachmentDeletedAudit {
        context: AuditContext {
            user_id: auth.user_id(),
            ip_address: Some(metadata.ip_address),
            user_agent: metadata.user_agent,
        },
        project_id,
        attachment_id,
        owner_kind: deleted.owner_kind,
        owner_id: deleted.owner_id,
        size_bytes: deleted.size_bytes,
    };
    if let Err(error) = state.audit_service.create_audit_log(&audit).await {
        tracing::error!(
            project_id,
            attachment_id,
            "Failed to create attachment delete audit log: {error}"
        );
    }
    Ok(StatusCode::NO_CONTENT)
}

/// Whether attachments are stored, and how much of the quota they use
#[utoipa::path(
    get,
    path = "/projects/{project_id}/error-attachments/usage",
    responses(
        (status = 200, description = "Storage state, use and limits", body = AttachmentUsage),
        (status = 401, description = "Not authenticated"),
        (status = 403, description = "No access to the project"),
        (status = 500, description = "Internal server error")
    ),
    params(("project_id" = i32, Path, description = "Project ID")),
    tag = "error-tracking",
    security(("bearer_auth" = []))
)]
pub async fn get_error_attachment_usage(
    State(state): State<Arc<AppState>>,
    RequireAuth(auth): RequireAuth,
    Path(project_id): Path<i32>,
) -> Result<Json<AttachmentUsage>, Problem> {
    permission_guard!(auth, ErrorTrackingRead);
    project_scope_guard!(auth, project_id);
    project_access_guard!(auth, project_id, state.project_access_checker);
    Ok(Json(
        state
            .error_tracking_service
            .attachments
            .usage(project_id)
            .await?,
    ))
}

/// Set how many bytes of attachments the project may store
#[utoipa::path(
    put,
    path = "/projects/{project_id}/error-attachments/quota",
    request_body = SetAttachmentQuotaRequest,
    responses(
        (status = 200, description = "Quota set", body = AttachmentUsage),
        (status = 400, description = "Quota out of range"),
        (status = 401, description = "Not authenticated"),
        (status = 403, description = "Error tracking write permission required"),
        (status = 500, description = "Internal server error")
    ),
    params(("project_id" = i32, Path, description = "Project ID")),
    tag = "error-tracking",
    security(("bearer_auth" = []))
)]
pub async fn set_error_attachment_quota(
    State(state): State<Arc<AppState>>,
    RequireAuth(auth): RequireAuth,
    Path(project_id): Path<i32>,
    Extension(metadata): Extension<RequestMetadata>,
    Json(request): Json<SetAttachmentQuotaRequest>,
) -> Result<Json<AttachmentUsage>, Problem> {
    permission_guard!(auth, ErrorTrackingWrite);
    project_scope_guard!(auth, project_id);
    project_access_guard!(auth, project_id, state.project_access_checker);
    let previous_quota_bytes = state
        .error_tracking_service
        .attachments
        .set_quota(project_id, request.quota_bytes)
        .await?;
    let audit = ErrorAttachmentQuotaAudit {
        context: AuditContext {
            user_id: auth.user_id(),
            ip_address: Some(metadata.ip_address),
            user_agent: metadata.user_agent,
        },
        project_id,
        previous_quota_bytes,
        quota_bytes: request.quota_bytes,
    };
    if let Err(error) = state.audit_service.create_audit_log(&audit).await {
        tracing::error!(
            project_id,
            "Failed to create attachment quota audit log: {error}"
        );
    }
    // Read after the audit, so a failed read cannot hide a recorded write.
    let usage = state
        .error_tracking_service
        .attachments
        .usage(project_id)
        .await?;
    Ok(Json(usage))
}

#[cfg(test)]
mod tests;
