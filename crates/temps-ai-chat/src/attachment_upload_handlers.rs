// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Chunked attachment upload endpoints (see [`crate::attachment_uploads`]):
//! start, append a multipart chunk, complete (which stores the file exactly as
//! the single-request upload does), and cancel.

use std::sync::Arc;

use axum::{
    extract::{DefaultBodyLimit, Multipart, Path, Query, State},
    http::StatusCode,
    routing::{delete, post, put},
    Extension, Json, Router,
};
use serde::{Deserialize, Serialize};
use temps_auth::{deny_deployment_token, permission_guard, RequireAuth};
use temps_core::{
    problemdetails::{self, Problem, ProblemDetails},
    RequestMetadata,
};
use utoipa::{IntoParams, OpenApi, ToSchema};

use crate::attachment_uploads::{UploadError, MAX_RESERVED_BYTES_PER_USER, UPLOAD_CHUNK_BYTES};
use crate::handlers::{
    authorize_attachment_upload, sanitized_attachment_name, store_attachment_bytes, AppState,
    ChatAttachmentResponse, MAX_CHAT_ATTACHMENT_BYTES,
};

/// Room for the multipart framing around one chunk.
const CHUNK_REQUEST_BODY_LIMIT: usize = UPLOAD_CHUNK_BYTES + 8 * 1024;

// A user's share of the upload budget must still admit one largest-allowed
// file, or that file could never be uploaded at all.
const _: () = assert!(MAX_RESERVED_BYTES_PER_USER >= MAX_CHAT_ATTACHMENT_BYTES);

impl From<UploadError> for Problem {
    fn from(error: UploadError) -> Self {
        match error {
            UploadError::NotFound { .. } => problemdetails::new(StatusCode::NOT_FOUND)
                .with_title("Upload Not Found")
                .with_detail(error.to_string()),
            UploadError::Empty { .. }
            | UploadError::EmptyChunk { .. }
            | UploadError::ChunkTooLarge { .. }
            | UploadError::PastDeclaredSize { .. }
            | UploadError::Incomplete { .. } => problemdetails::new(StatusCode::BAD_REQUEST)
                .with_title("Invalid Attachment Upload")
                .with_detail(error.to_string()),
            UploadError::TooLarge { .. } => problemdetails::new(StatusCode::PAYLOAD_TOO_LARGE)
                .with_title("Attachment Too Large")
                .with_detail(error.to_string()),
            UploadError::OutOfOrder { .. } => problemdetails::new(StatusCode::CONFLICT)
                .with_title("Chunk Out Of Order")
                .with_detail(error.to_string()),
            UploadError::TooManyActive { .. } => problemdetails::new(StatusCode::TOO_MANY_REQUESTS)
                .with_title("Too Many Uploads")
                .with_detail(error.to_string()),
            UploadError::UserOutOfCapacity { .. } => {
                problemdetails::new(StatusCode::TOO_MANY_REQUESTS)
                    .with_title("Too Much Upload In Progress")
                    .with_detail(error.to_string())
            }
            UploadError::OutOfCapacity { .. } => {
                problemdetails::new(StatusCode::SERVICE_UNAVAILABLE)
                    .with_title("Upload Capacity Reached")
                    .with_detail(error.to_string())
            }
        }
    }
}

#[derive(Debug, Deserialize, ToSchema)]
pub struct StartAttachmentUploadRequest {
    /// File name as the user knows it; unsafe characters are replaced.
    pub name: String,
    /// Size of the whole file in bytes.
    pub size_bytes: u64,
    /// Content type the browser reported, if any.
    pub mime_type: Option<String>,
}

#[derive(Debug, Serialize, ToSchema)]
pub struct AttachmentUploadStarted {
    /// Opaque id to send every chunk and the completion to.
    pub upload_id: String,
    /// The most bytes one chunk may carry.
    pub chunk_bytes: usize,
}

#[derive(Debug, Deserialize, IntoParams)]
pub struct AttachmentChunkQuery {
    /// Where this chunk starts in the file: the bytes received so far.
    pub offset: u64,
}

#[derive(Debug, Serialize, ToSchema)]
pub struct AttachmentChunkReceived {
    /// Bytes of the file the server now holds.
    pub received_bytes: u64,
}

/// Request body type for the chunk endpoint's OpenAPI entry.
#[derive(Debug, ToSchema)]
pub struct AttachmentChunkUpload {
    #[schema(value_type = String, format = Binary)]
    pub chunk: String,
}

/// Start uploading a file in chunks, for clients behind a small request-size
/// limit. Send the chunks in order, then complete.
#[utoipa::path(
    post, tag = "AI Chat", path = "/ai/conversations/{public_id}/attachment-uploads",
    params(("public_id" = String, Path, description = "Conversation public id")),
    request_body = StartAttachmentUploadRequest,
    responses(
        (status = 201, description = "Upload opened", body = AttachmentUploadStarted),
        (status = 400, description = "Empty file", body = ProblemDetails),
        (status = 401, description = "Unauthorized", body = ProblemDetails),
        (status = 403, description = "Insufficient permissions", body = ProblemDetails),
        (status = 404, description = "Conversation not found", body = ProblemDetails),
        (status = 413, description = "File too large", body = ProblemDetails),
        (status = 429, description = "Too many uploads in progress", body = ProblemDetails),
        (status = 503, description = "Upload capacity reached", body = ProblemDetails)
    ),
    security(("bearer_auth" = []))
)]
pub async fn start_attachment_upload(
    RequireAuth(auth): RequireAuth,
    State(state): State<Arc<AppState>>,
    Path(public_id): Path<String>,
    Json(request): Json<StartAttachmentUploadRequest>,
) -> Result<(StatusCode, Json<AttachmentUploadStarted>), Problem> {
    let (conversation, _) = authorize_attachment_upload(&state, &auth, &public_id).await?;
    let name = sanitized_attachment_name(Some(&request.name));
    // A size too big for usize cannot be a valid file; saturate so it is refused
    // as too large rather than wrapping.
    let size = usize::try_from(request.size_bytes).unwrap_or(usize::MAX);
    let upload_id = state.attachment_uploads.start(
        auth.user_id(),
        &conversation.public_id,
        &name,
        request.mime_type,
        size,
        MAX_CHAT_ATTACHMENT_BYTES,
    )?;
    Ok((
        StatusCode::CREATED,
        Json(AttachmentUploadStarted {
            upload_id,
            chunk_bytes: UPLOAD_CHUNK_BYTES,
        }),
    ))
}

/// Append the next chunk of an upload.
#[utoipa::path(
    put, tag = "AI Chat",
    path = "/ai/conversations/{public_id}/attachment-uploads/{upload_id}/chunk",
    params(
        ("public_id" = String, Path, description = "Conversation public id"),
        ("upload_id" = String, Path, description = "Upload id from the start request"),
        AttachmentChunkQuery,
    ),
    request_body(content = AttachmentChunkUpload, content_type = "multipart/form-data"),
    responses(
        (status = 200, description = "Chunk stored", body = AttachmentChunkReceived),
        (status = 400, description = "Invalid chunk", body = ProblemDetails),
        (status = 401, description = "Unauthorized", body = ProblemDetails),
        (status = 403, description = "Insufficient permissions", body = ProblemDetails),
        (status = 404, description = "Upload not found or expired", body = ProblemDetails),
        (status = 409, description = "Chunk is not the next one expected", body = ProblemDetails)
    ),
    security(("bearer_auth" = []))
)]
pub async fn append_attachment_chunk(
    RequireAuth(auth): RequireAuth,
    State(state): State<Arc<AppState>>,
    Path((public_id, upload_id)): Path<(String, String)>,
    Query(query): Query<AttachmentChunkQuery>,
    mut multipart: Multipart,
) -> Result<Json<AttachmentChunkReceived>, Problem> {
    // The upload was authorized in full when it started and is bound to this
    // user and chat, so a chunk only needs the cheap checks: re-reading the
    // conversation for every 32 KiB would be hundreds of queries per photo.
    permission_guard!(auth, ProjectsRead);
    deny_deployment_token!(auth);
    let field = multipart
        .next_field()
        .await
        .map_err(|error| {
            problemdetails::new(StatusCode::BAD_REQUEST)
                .with_title("Invalid Attachment Upload")
                .with_detail(format!(
                    "Could not read chunk for upload '{upload_id}': {error}"
                ))
        })?
        .ok_or_else(|| {
            problemdetails::new(StatusCode::BAD_REQUEST)
                .with_title("Missing Chunk")
                .with_detail(format!(
                    "The multipart body for upload '{upload_id}' must contain a 'chunk' field."
                ))
        })?;
    if field.name() != Some("chunk") {
        return Err(problemdetails::new(StatusCode::BAD_REQUEST)
            .with_title("Missing Chunk")
            .with_detail(format!(
                "The multipart field for upload '{upload_id}' must be named 'chunk'."
            )));
    }
    let bytes = field.bytes().await.map_err(|error| {
        problemdetails::new(StatusCode::BAD_REQUEST)
            .with_title("Invalid Attachment Upload")
            .with_detail(format!(
                "Could not read chunk bytes for upload '{upload_id}': {error}"
            ))
    })?;
    let offset = usize::try_from(query.offset).unwrap_or(usize::MAX);
    let received =
        state
            .attachment_uploads
            .append(auth.user_id(), &public_id, &upload_id, offset, &bytes)?;
    Ok(Json(AttachmentChunkReceived {
        received_bytes: received as u64,
    }))
}

/// Finish an upload once every byte has arrived. The file is stored as a
/// single-request upload would store it, and described the same way.
#[utoipa::path(
    post, tag = "AI Chat",
    path = "/ai/conversations/{public_id}/attachment-uploads/{upload_id}/complete",
    params(
        ("public_id" = String, Path, description = "Conversation public id"),
        ("upload_id" = String, Path, description = "Upload id from the start request"),
    ),
    responses(
        (status = 201, description = "Attachment stored", body = ChatAttachmentResponse),
        (status = 400, description = "Not every byte has arrived", body = ProblemDetails),
        (status = 401, description = "Unauthorized", body = ProblemDetails),
        (status = 403, description = "Insufficient permissions", body = ProblemDetails),
        (status = 404, description = "Upload or conversation not found", body = ProblemDetails),
        (status = 409, description = "The chat has no workspace for attachments", body = ProblemDetails)
    ),
    security(("bearer_auth" = []))
)]
pub async fn complete_attachment_upload(
    RequireAuth(auth): RequireAuth,
    State(state): State<Arc<AppState>>,
    Extension(metadata): Extension<RequestMetadata>,
    Path((public_id, upload_id)): Path<(String, String)>,
) -> Result<(StatusCode, Json<ChatAttachmentResponse>), Problem> {
    // Permissions may have changed since the upload started; storing is the
    // write, so it gets the full check again.
    let (conversation, workspace_id) =
        authorize_attachment_upload(&state, &auth, &public_id).await?;
    let finished =
        state
            .attachment_uploads
            .finish(auth.user_id(), &conversation.public_id, &upload_id)?;
    store_attachment_bytes(
        &state,
        &auth,
        &metadata,
        conversation,
        workspace_id,
        finished.name,
        finished.declared_mime,
        finished.bytes,
    )
    .await
}

/// Abandon an upload and free what it reserved.
#[utoipa::path(
    delete, tag = "AI Chat",
    path = "/ai/conversations/{public_id}/attachment-uploads/{upload_id}",
    params(
        ("public_id" = String, Path, description = "Conversation public id"),
        ("upload_id" = String, Path, description = "Upload id from the start request"),
    ),
    responses(
        (status = 204, description = "Upload cancelled (or already gone)"),
        (status = 401, description = "Unauthorized", body = ProblemDetails),
        (status = 403, description = "Insufficient permissions", body = ProblemDetails)
    ),
    security(("bearer_auth" = []))
)]
pub async fn cancel_attachment_upload(
    RequireAuth(auth): RequireAuth,
    State(state): State<Arc<AppState>>,
    Path((public_id, upload_id)): Path<(String, String)>,
) -> Result<StatusCode, Problem> {
    permission_guard!(auth, ProjectsRead);
    deny_deployment_token!(auth);
    state
        .attachment_uploads
        .cancel(auth.user_id(), &public_id, &upload_id);
    Ok(StatusCode::NO_CONTENT)
}

pub fn configure_routes() -> Router<Arc<AppState>> {
    Router::new()
        .route(
            "/ai/conversations/{public_id}/attachment-uploads",
            post(start_attachment_upload),
        )
        .route(
            "/ai/conversations/{public_id}/attachment-uploads/{upload_id}/chunk",
            put(append_attachment_chunk).layer(DefaultBodyLimit::max(CHUNK_REQUEST_BODY_LIMIT)),
        )
        .route(
            "/ai/conversations/{public_id}/attachment-uploads/{upload_id}/complete",
            post(complete_attachment_upload),
        )
        .route(
            "/ai/conversations/{public_id}/attachment-uploads/{upload_id}",
            delete(cancel_attachment_upload),
        )
}

#[derive(OpenApi)]
#[openapi(
    paths(
        start_attachment_upload,
        append_attachment_chunk,
        complete_attachment_upload,
        cancel_attachment_upload
    ),
    components(schemas(
        StartAttachmentUploadRequest,
        AttachmentUploadStarted,
        AttachmentChunkReceived,
        AttachmentChunkUpload
    ))
)]
pub struct AttachmentUploadApiDoc;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn errors_map_to_the_status_a_client_can_act_on() {
        let status = |error: UploadError| Problem::from(error).status_code;
        let id = || "upl_x".to_string();
        assert_eq!(
            status(UploadError::NotFound {
                upload_id: id(),
                conversation_id: "c".into(),
                ttl_secs: 600
            }),
            StatusCode::NOT_FOUND
        );
        assert_eq!(
            status(UploadError::TooLarge {
                name: "a".into(),
                declared: 2,
                limit: 1
            }),
            StatusCode::PAYLOAD_TOO_LARGE
        );
        assert_eq!(
            status(UploadError::OutOfOrder {
                upload_id: id(),
                expected: 1,
                got: 2
            }),
            StatusCode::CONFLICT
        );
        assert_eq!(
            status(UploadError::TooManyActive {
                user_id: 1,
                active: 8,
                limit: 8
            }),
            StatusCode::TOO_MANY_REQUESTS
        );
        assert_eq!(
            status(UploadError::OutOfCapacity {
                reserved: 1,
                limit: 1
            }),
            StatusCode::SERVICE_UNAVAILABLE
        );
        assert_eq!(
            status(UploadError::Incomplete {
                upload_id: id(),
                received: 1,
                declared: 2
            }),
            StatusCode::BAD_REQUEST
        );
        assert_eq!(
            status(UploadError::Empty { name: "a".into() }),
            StatusCode::BAD_REQUEST
        );
        assert_eq!(
            status(UploadError::EmptyChunk {
                upload_id: id(),
                offset: 0
            }),
            StatusCode::BAD_REQUEST
        );
        assert_eq!(
            status(UploadError::UserOutOfCapacity {
                user_id: 1,
                reserved: 20,
                requested: 10,
                limit: 24
            }),
            StatusCode::TOO_MANY_REQUESTS
        );
    }

    #[test]
    fn a_chunk_request_fits_under_a_40_kib_proxy_limit() {
        // The preview proxy that motivated chunking rejects bodies over 40 KiB;
        // the chunk plus its multipart framing must stay well under it.
        const { assert!(UPLOAD_CHUNK_BYTES + 1024 < 40 * 1024) };
        const { assert!(CHUNK_REQUEST_BODY_LIMIT >= UPLOAD_CHUNK_BYTES) };
    }
}
