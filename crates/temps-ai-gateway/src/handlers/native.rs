// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! OpenAI-native endpoints: `/ai/v1/responses`, `/ai/v1/files` and
//! `/ai/v1/batches`.
//!
//! Point an official OpenAI SDK at `{temps}/api/ai/v1` and these behave like
//! OpenAI's own endpoints. Provider replies, including provider errors, are
//! forwarded unchanged; errors raised by the gateway itself use the same
//! OpenAI error shape as `/ai/v1/chat/completions`.

use std::sync::Arc;
use std::time::Instant;

use anyhow::Result as AnyhowResult;
use axum::{
    body::Body,
    extract::{DefaultBodyLimit, Multipart, Path, State},
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Response},
    routing::{get, post},
    Json, Router,
};
use serde::Serialize;
use temps_auth::{permission_guard, AuthContext, RequireAuth};
use temps_core::problemdetails::Problem;
use temps_core::{AuditOperation, RequestMetadata};
use tracing::{error, info};
use utoipa::OpenApi;

use crate::error::AiGatewayError;
use crate::handlers::gateway::{
    credential_type_str, error_to_response, extract_ai_context, extract_byok,
    extract_responses_usage_from_sse_line, reject_deployment_token_base_url,
    wrap_stream_with_usage_tracking,
};
use crate::handlers::types::AiGatewayAppState;
use crate::native_types::*;
use crate::providers::openai_native::UpstreamReply;
use crate::services::gateway_service::CredentialType;
use crate::services::native_api_service::{
    spool_batch_file, ResponsesOutcome, MAX_BATCH_FILE_BYTES,
};
use crate::services::Owner;
use crate::types::OpenAiErrorResponse;

/// Multipart framing (boundaries, headers, the `purpose` field) on top of
/// the largest accepted file.
const UPLOAD_BODY_LIMIT: usize = MAX_BATCH_FILE_BYTES as usize + 1024 * 1024;
/// Longest file name forwarded to the provider.
const MAX_FILENAME_LEN: usize = 255;
/// Optional multipart fields forwarded to the provider with an upload.
const FORWARDED_UPLOAD_FIELDS: [&str; 2] = ["expires_after[anchor]", "expires_after[seconds]"];

#[derive(OpenApi)]
#[openapi(
    paths(
        create_response,
        create_response_json,
        create_response_stream,
        upload_file,
        retrieve_file,
        delete_file,
        file_content,
        create_batch,
        retrieve_batch,
        cancel_batch
    ),
    components(schemas(
        ResponsesRequest,
        ResponseObject,
        ResponseUsage,
        ResponseStreamEvent,
        UploadFileForm,
        FileObject,
        FileDeletedResponse,
        CreateBatchRequest,
        BatchObject,
        BatchRequestCounts,
        BatchUsage,
    )),
    tags(
        (name = "AI Gateway", description = "OpenAI-compatible chat, embeddings, and model endpoints")
    )
)]
pub struct AiGatewayNativeApiDoc;

pub fn configure_native_routes() -> Router<Arc<AiGatewayAppState>> {
    Router::new()
        .route("/ai/v1/responses", post(create_response))
        .route("/ai/v1/responses/json", post(create_response_json))
        .route("/ai/v1/responses/stream", post(create_response_stream))
        .route(
            "/ai/v1/files",
            post(upload_file).layer(DefaultBodyLimit::max(UPLOAD_BODY_LIMIT)),
        )
        .route(
            "/ai/v1/files/{file_id}",
            get(retrieve_file).delete(delete_file),
        )
        .route("/ai/v1/files/{file_id}/content", get(file_content))
        .route("/ai/v1/batches", post(create_batch))
        .route("/ai/v1/batches/{batch_id}", get(retrieve_batch))
        .route("/ai/v1/batches/{batch_id}/cancel", post(cancel_batch))
}

// ============================================================================
// Helpers
// ============================================================================

fn owner_of(auth: &AuthContext) -> Owner {
    Owner {
        user_id: auth.user_id_opt(),
        project_id: auth.project_id(),
    }
}

/// Forward a provider reply unchanged.
fn forward(reply: UpstreamReply, credential_type: Option<CredentialType>) -> Response {
    let status = StatusCode::from_u16(reply.status.as_u16()).unwrap_or(StatusCode::BAD_GATEWAY);
    let mut builder = Response::builder().status(status).header(
        "content-type",
        reply
            .content_type
            .unwrap_or_else(|| "application/json".to_string()),
    );
    if let Some(credential_type) = credential_type {
        builder = builder.header(
            "x-temps-credential-type",
            credential_type_str(credential_type),
        );
    }
    match builder.body(Body::from(reply.body)) {
        Ok(response) => response,
        Err(build_error) => {
            error!(error = %build_error, "Failed to build forwarded provider response");
            error_to_response(AiGatewayError::Internal {
                message: "Failed to build forwarded provider response".to_string(),
            })
            .into_response()
        }
    }
}

fn credential_kind(headers: &HeaderMap) -> CredentialType {
    if extract_byok(headers).api_key.is_some() {
        CredentialType::Byok
    } else {
        CredentialType::System
    }
}

fn sanitize_filename(name: Option<&str>) -> String {
    let base = name
        .and_then(|name| name.rsplit(['/', '\\']).next())
        .map(|name| {
            name.chars()
                .filter(|c| !c.is_control())
                .take(MAX_FILENAME_LEN)
                .collect::<String>()
        })
        .unwrap_or_default();
    if base.trim().is_empty() {
        "batch.jsonl".to_string()
    } else {
        base
    }
}

/// Text multipart fields are tiny; never buffer arbitrary framing data.
async fn read_upload_text(
    mut field: axum::extract::multipart::Field<'_>,
    deadline: tokio::time::Instant,
) -> Result<String, String> {
    let mut bytes = Vec::new();
    while let Some(chunk) = tokio::time::timeout_at(deadline, field.chunk())
        .await
        .map_err(|_| "batch upload timed out after 30 minutes".to_string())?
        .map_err(|error| error.to_string())?
    {
        if bytes.len().saturating_add(chunk.len()) > 1024 {
            return Err("text field exceeds 1024 bytes".to_string());
        }
        bytes.extend_from_slice(&chunk);
    }
    String::from_utf8(bytes).map_err(|error| error.to_string())
}

fn upload_timeout() -> AiGatewayError {
    AiGatewayError::Validation {
        message: "Batch upload timed out after 30 minutes".to_string(),
    }
}

/// Audit record for a file or batch the caller created or removed.
#[derive(Debug, Clone, Serialize)]
struct AiGatewayObjectAudit {
    #[serde(skip)]
    operation: &'static str,
    user_id: Option<i32>,
    project_id: Option<i32>,
    #[serde(skip)]
    ip_address: Option<String>,
    #[serde(skip)]
    user_agent: String,
    /// Provider id of the file or batch, e.g. `batch_abc123`
    object_id: String,
    credential_type: &'static str,
}

impl AuditOperation for AiGatewayObjectAudit {
    fn operation_type(&self) -> String {
        self.operation.to_string()
    }

    fn user_id(&self) -> Option<i32> {
        self.user_id
    }

    fn ip_address(&self) -> Option<String> {
        self.ip_address.clone()
    }

    fn user_agent(&self) -> &str {
        &self.user_agent
    }

    fn serialize(&self) -> AnyhowResult<String> {
        serde_json::to_string(self)
            .map_err(|e| anyhow::anyhow!("Failed to serialize audit operation: {e}"))
    }
}

/// Audit a successful create/delete/cancel. Failures are logged, never
/// surfaced: the provider operation already happened.
async fn audit_object(
    app_state: &AiGatewayAppState,
    auth: &AuthContext,
    metadata: &RequestMetadata,
    operation: &'static str,
    reply: &UpstreamReply,
    fallback_id: Option<&str>,
    credential_type: CredentialType,
) {
    if !reply.is_success() {
        return;
    }
    let object_id =
        serde_json::from_slice::<serde_json::Map<String, serde_json::Value>>(&reply.body)
            .ok()
            .and_then(|object| {
                object
                    .get("id")
                    .and_then(|id| id.as_str())
                    .map(str::to_string)
            })
            .or_else(|| fallback_id.map(str::to_string))
            .unwrap_or_default();
    let audit = AiGatewayObjectAudit {
        operation,
        user_id: auth.user_id_opt(),
        project_id: auth.project_id(),
        ip_address: Some(metadata.ip_address.clone()),
        user_agent: metadata.user_agent.clone(),
        object_id,
        credential_type: credential_type_str(credential_type),
    };
    if let Err(audit_error) = app_state.audit_service.create_audit_log(&audit).await {
        error!(operation, error = %audit_error, "Failed to create audit log for AI gateway object");
    }
}

/// Shared prelude: BYOK headers, refusing provider URL overrides for
/// deployment tokens.
fn byok_for(
    auth: &AuthContext,
    headers: &HeaderMap,
) -> Result<crate::services::ByokOverride, AiGatewayError> {
    let byok = extract_byok(headers);
    match reject_deployment_token_base_url(auth, &byok) {
        Some(error) => Err(error),
        None => Ok(byok),
    }
}

// ============================================================================
// Responses
// ============================================================================

/// Explicit JSON contract for generated clients; the standard OpenAI route remains polymorphic.
#[utoipa::path(tag = "AI Gateway", post, path = "/ai/v1/responses/json",
    request_body = ResponsesRequest,
    responses((status = 200, description = "Complete response object; forces stream=false", body = ResponseObject),
        (status = 400, description = "Invalid request", body = OpenAiErrorResponse)),
    security(("bearer_auth" = [])))]
async fn create_response_json(
    auth: RequireAuth,
    state: State<Arc<AiGatewayAppState>>,
    headers: HeaderMap,
    Json(mut request): Json<ResponsesRequest>,
) -> Result<Response, Problem> {
    request.stream = false;
    create_response(auth, state, headers, Json(request)).await
}

/// Explicit SSE contract for generated clients; never buffers a successful event stream.
#[utoipa::path(tag = "AI Gateway", post, path = "/ai/v1/responses/stream",
    request_body = ResponsesRequest,
    responses((status = 200, description = "Incremental Responses events; forces stream=true", body = ResponseStreamEvent, content_type = "text/event-stream"),
        (status = 400, description = "Invalid request", body = OpenAiErrorResponse)),
    security(("bearer_auth" = [])))]
async fn create_response_stream(
    auth: RequireAuth,
    state: State<Arc<AiGatewayAppState>>,
    headers: HeaderMap,
    Json(mut request): Json<ResponsesRequest>,
) -> Result<Response, Problem> {
    request.stream = true;
    create_response(auth, state, headers, Json(request)).await
}

#[utoipa::path(
    tag = "AI Gateway",
    post,
    path = "/ai/v1/responses",
    request_body = ResponsesRequest,
    responses(
        (status = 200, description = "JSON when stream=false, SSE when stream=true. Generated SDKs use the explicit /json and /stream operations.", content((ResponseObject = "application/json"), (ResponseStreamEvent = "text/event-stream"))),
        (status = 400, description = "Invalid request, or the model is not served by OpenAI", body = OpenAiErrorResponse),
        (status = 401, description = "Unauthorized", body = OpenAiErrorResponse),
        (status = 403, description = "Model not allowed", body = OpenAiErrorResponse),
        (status = 404, description = "Model or provider not configured", body = OpenAiErrorResponse),
        (status = 500, description = "Internal error", body = OpenAiErrorResponse)
    ),
    security(("bearer_auth" = []))
)]
async fn create_response(
    RequireAuth(auth): RequireAuth,
    State(app_state): State<Arc<AiGatewayAppState>>,
    headers: HeaderMap,
    Json(request): Json<ResponsesRequest>,
) -> Result<Response, Problem> {
    permission_guard!(auth, AiGatewayExecute);

    if request.model.trim().is_empty() {
        return Ok(error_to_response(AiGatewayError::Validation {
            message: "model field is required".to_string(),
        })
        .into_response());
    }
    let byok = match byok_for(&auth, &headers) {
        Ok(byok) => byok,
        Err(error) => return Ok(error_to_response(error).into_response()),
    };
    let ai_context = extract_ai_context(&headers);
    let start = Instant::now();
    let user_id = auth.user_id_opt();
    let model = request.model.clone();

    let (outcome, credential_type) = match app_state
        .native_api_service
        .create_response(&request, &byok)
        .await
    {
        Ok(result) => result,
        Err(call_error) => {
            error!(model, error = %call_error, "AI gateway Responses request failed");
            return Ok(error_to_response(call_error).into_response());
        }
    };
    let is_byok = credential_type == CredentialType::Byok;

    app_state.telemetry.report_once(
        "ai_gateway_first_request",
        temps_core::telemetry::TelemetryEvent::new(
            temps_core::telemetry::TelemetryEventKind::AiGatewayFirstRequest,
        )
        .with("provider", "openai"),
    );

    match outcome {
        ResponsesOutcome::Reply { reply, usage } => {
            let latency_ms = start.elapsed().as_millis() as i32;
            if let Some(usage) = usage {
                let usage_service = app_state.usage_service.clone();
                let model = model.clone();
                tokio::spawn(async move {
                    if let Err(log_error) = usage_service
                        .log_usage_with_context(
                            user_id,
                            "openai",
                            &model,
                            usage.input_tokens,
                            usage.output_tokens,
                            latency_ms,
                            0,
                            200,
                            false,
                            is_byok,
                            &ai_context,
                        )
                        .await
                    {
                        error!(error = %log_error, "Failed to log AI Responses usage");
                    }
                });
            }
            info!(
                model,
                user_id = ?user_id,
                status = reply.status.as_u16(),
                latency_ms,
                credential_type = credential_type_str(credential_type),
                "AI gateway Responses request completed"
            );
            Ok(forward(reply, Some(credential_type)))
        }
        ResponsesOutcome::Stream(stream) => {
            info!(
                model,
                user_id = ?user_id,
                streaming = true,
                credential_type = credential_type_str(credential_type),
                "AI gateway Responses streaming request started"
            );
            let wrapped = wrap_stream_with_usage_tracking(
                stream,
                app_state.usage_service.clone(),
                user_id,
                "openai".to_string(),
                model,
                start,
                is_byok,
                ai_context,
                extract_responses_usage_from_sse_line,
            );
            let response = Response::builder()
                .status(StatusCode::OK)
                .header("content-type", "text/event-stream")
                .header("cache-control", "no-cache")
                .header("x-temps-provider", "openai")
                .header(
                    "x-temps-credential-type",
                    credential_type_str(credential_type),
                )
                .body(Body::from_stream(wrapped));
            match response {
                Ok(response) => Ok(response),
                Err(build_error) => {
                    error!(error = %build_error, "Failed to build Responses streaming response");
                    Ok(error_to_response(AiGatewayError::Internal {
                        message: "Failed to build streaming response".to_string(),
                    })
                    .into_response())
                }
            }
        }
    }
}

// ============================================================================
// Files
// ============================================================================

#[utoipa::path(
    tag = "AI Gateway",
    post,
    path = "/ai/v1/files",
    request_body(content = UploadFileForm, content_type = "multipart/form-data"),
    responses(
        (status = 200, description = "Uploaded file", body = FileObject),
        (status = 400, description = "Invalid batch input file", body = OpenAiErrorResponse),
        (status = 401, description = "Unauthorized", body = OpenAiErrorResponse),
        (status = 403, description = "Model not allowed", body = OpenAiErrorResponse),
        (status = 404, description = "Model or provider not configured", body = OpenAiErrorResponse),
        (status = 413, description = "File larger than 200 MB", body = OpenAiErrorResponse),
        (status = 429, description = "Concurrent upload capacity reached; retry later", body = OpenAiErrorResponse)
    ),
    security(("bearer_auth" = []))
)]
async fn upload_file(
    RequireAuth(auth): RequireAuth,
    State(app_state): State<Arc<AiGatewayAppState>>,
    axum::extract::Extension(metadata): axum::extract::Extension<RequestMetadata>,
    headers: HeaderMap,
    mut multipart: Multipart,
) -> Result<Response, Problem> {
    permission_guard!(auth, AiGatewayExecute);
    let byok = match byok_for(&auth, &headers) {
        Ok(byok) => byok,
        Err(error) => return Ok(error_to_response(error).into_response()),
    };

    let _upload_slot = match app_state.native_api_service.acquire_upload_slot() {
        Ok(permit) => permit,
        Err(error) => return Ok(error_to_response(error).into_response()),
    };
    let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(30 * 60);
    let mut purpose: Option<String> = None;
    let mut spooled = None;
    let mut filename = None;
    let mut text_fields: Vec<(String, String)> = Vec::new();
    loop {
        let next_field = match tokio::time::timeout_at(deadline, multipart.next_field()).await {
            Ok(result) => result,
            Err(_) => return Ok(error_to_response(upload_timeout()).into_response()),
        };
        let field = match next_field {
            Ok(Some(field)) => field,
            Ok(None) => break,
            Err(read_error) => {
                return Ok(error_to_response(AiGatewayError::Validation {
                    message: format!("Invalid multipart upload: {read_error}"),
                })
                .into_response())
            }
        };
        let name = field.name().unwrap_or_default().to_string();
        if (name == "purpose" && purpose.is_some())
            || text_fields.iter().any(|(existing, _)| existing == &name)
        {
            return Ok(error_to_response(AiGatewayError::Validation {
                message: format!("Upload field '{name}' must occur only once"),
            })
            .into_response());
        }
        match name.as_str() {
            "file" => {
                if spooled.is_some() {
                    return Ok(error_to_response(AiGatewayError::Validation {
                        message: "Upload exactly one 'file' field".to_string(),
                    })
                    .into_response());
                }
                filename = Some(sanitize_filename(field.file_name()));
                match tokio::time::timeout_at(deadline, spool_batch_file(field))
                    .await
                    .unwrap_or_else(|_| Err(upload_timeout()))
                {
                    Ok(file) => spooled = Some(file),
                    Err(spool_error) => return Ok(error_to_response(spool_error).into_response()),
                }
            }
            "purpose" => match read_upload_text(field, deadline).await {
                Ok(value) => purpose = Some(value),
                Err(read_error) => {
                    return Ok(error_to_response(AiGatewayError::Validation {
                        message: format!("Failed to read the 'purpose' field: {read_error}"),
                    })
                    .into_response())
                }
            },
            other if FORWARDED_UPLOAD_FIELDS.contains(&other) => {
                match read_upload_text(field, deadline).await {
                    Ok(value) => text_fields.push((other.to_string(), value)),
                    Err(read_error) => {
                        return Ok(error_to_response(AiGatewayError::Validation {
                            message: format!("Failed to read the '{other}' field: {read_error}"),
                        })
                        .into_response())
                    }
                }
            }
            other => {
                return Ok(error_to_response(AiGatewayError::Validation {
                    message: format!(
                    "Unexpected form field '{other}'; expected 'file', 'purpose' and optionally {}",
                    FORWARDED_UPLOAD_FIELDS.join(", ")
                ),
                })
                .into_response())
            }
        }
    }

    if purpose.as_deref() != Some("batch") {
        return Ok(error_to_response(AiGatewayError::Validation {
            message: format!(
                "purpose '{}' is not supported; the gateway accepts batch input files only (purpose=batch)",
                purpose.unwrap_or_default()
            ),
        })
        .into_response());
    }
    let Some(spooled) = spooled else {
        return Ok(error_to_response(AiGatewayError::Validation {
            message: "The 'file' field is required".to_string(),
        })
        .into_response());
    };

    let credential_type = credential_kind(&headers);
    let filename = filename.unwrap_or_else(|| "batch.jsonl".to_string());
    match app_state
        .native_api_service
        .upload_batch_file(owner_of(&auth), &byok, spooled, &filename, &text_fields)
        .await
    {
        Ok(reply) => {
            audit_object(
                &app_state,
                &auth,
                &metadata,
                "ai_gateway.file.uploaded",
                &reply,
                None,
                credential_type,
            )
            .await;
            Ok(forward(reply, Some(credential_type)))
        }
        Err(upload_error) => Ok(error_to_response(upload_error).into_response()),
    }
}

#[utoipa::path(
    tag = "AI Gateway",
    get,
    path = "/ai/v1/files/{file_id}",
    params(("file_id" = String, Path, description = "Provider file id, e.g. file-abc123")),
    responses(
        (status = 200, description = "File object", body = FileObject),
        (status = 401, description = "Unauthorized", body = OpenAiErrorResponse),
        (status = 404, description = "No such file created by the caller", body = OpenAiErrorResponse)
    ),
    security(("bearer_auth" = []))
)]
async fn retrieve_file(
    RequireAuth(auth): RequireAuth,
    State(app_state): State<Arc<AiGatewayAppState>>,
    headers: HeaderMap,
    Path(file_id): Path<String>,
) -> Result<Response, Problem> {
    permission_guard!(auth, AiGatewayExecute);
    let byok = match byok_for(&auth, &headers) {
        Ok(byok) => byok,
        Err(error) => return Ok(error_to_response(error).into_response()),
    };
    match app_state
        .native_api_service
        .retrieve_file(owner_of(&auth), &byok, &file_id)
        .await
    {
        Ok(reply) => Ok(forward(reply, Some(credential_kind(&headers)))),
        Err(call_error) => Ok(error_to_response(call_error).into_response()),
    }
}

#[utoipa::path(
    tag = "AI Gateway",
    delete,
    path = "/ai/v1/files/{file_id}",
    params(("file_id" = String, Path, description = "Provider file id, e.g. file-abc123")),
    responses(
        (status = 200, description = "File deleted", body = FileDeletedResponse),
        (status = 401, description = "Unauthorized", body = OpenAiErrorResponse),
        (status = 404, description = "No such file created by the caller", body = OpenAiErrorResponse)
    ),
    security(("bearer_auth" = []))
)]
async fn delete_file(
    RequireAuth(auth): RequireAuth,
    State(app_state): State<Arc<AiGatewayAppState>>,
    axum::extract::Extension(metadata): axum::extract::Extension<RequestMetadata>,
    headers: HeaderMap,
    Path(file_id): Path<String>,
) -> Result<Response, Problem> {
    permission_guard!(auth, AiGatewayExecute);
    let byok = match byok_for(&auth, &headers) {
        Ok(byok) => byok,
        Err(error) => return Ok(error_to_response(error).into_response()),
    };
    let credential_type = credential_kind(&headers);
    match app_state
        .native_api_service
        .delete_file(owner_of(&auth), &byok, &file_id)
        .await
    {
        Ok(reply) => {
            audit_object(
                &app_state,
                &auth,
                &metadata,
                "ai_gateway.file.deleted",
                &reply,
                Some(&file_id),
                credential_type,
            )
            .await;
            Ok(forward(reply, Some(credential_type)))
        }
        Err(call_error) => Ok(error_to_response(call_error).into_response()),
    }
}

#[utoipa::path(
    tag = "AI Gateway",
    get,
    path = "/ai/v1/files/{file_id}/content",
    params(("file_id" = String, Path, description = "Provider file id, e.g. a batch's output_file_id")),
    responses(
        (status = 200, description = "File content (JSONL for batch files), streamed", content_type = "application/octet-stream"),
        (status = 401, description = "Unauthorized", body = OpenAiErrorResponse),
        (status = 404, description = "No such file created by the caller", body = OpenAiErrorResponse)
    ),
    security(("bearer_auth" = []))
)]
async fn file_content(
    RequireAuth(auth): RequireAuth,
    State(app_state): State<Arc<AiGatewayAppState>>,
    headers: HeaderMap,
    Path(file_id): Path<String>,
) -> Result<Response, Problem> {
    permission_guard!(auth, AiGatewayExecute);
    let byok = match byok_for(&auth, &headers) {
        Ok(byok) => byok,
        Err(error) => return Ok(error_to_response(error).into_response()),
    };
    let credential_type = credential_kind(&headers);
    match app_state
        .native_api_service
        .file_content(owner_of(&auth), &byok, &file_id)
        .await
    {
        Ok(Ok((content_type, stream))) => {
            let response = Response::builder()
                .status(StatusCode::OK)
                .header(
                    "content-type",
                    content_type.unwrap_or_else(|| "application/octet-stream".to_string()),
                )
                .header(
                    "x-temps-credential-type",
                    credential_type_str(credential_type),
                )
                .body(Body::from_stream(stream));
            match response {
                Ok(response) => Ok(response),
                Err(build_error) => {
                    error!(file_id, error = %build_error, "Failed to build file content response");
                    Ok(error_to_response(AiGatewayError::Internal {
                        message: format!(
                            "Failed to build the content response for file '{file_id}'"
                        ),
                    })
                    .into_response())
                }
            }
        }
        Ok(Err(reply)) => Ok(forward(reply, Some(credential_type))),
        Err(call_error) => Ok(error_to_response(call_error).into_response()),
    }
}

// ============================================================================
// Batches
// ============================================================================

#[utoipa::path(
    tag = "AI Gateway",
    post,
    path = "/ai/v1/batches",
    request_body = CreateBatchRequest,
    responses(
        (status = 200, description = "Batch created", body = BatchObject),
        (status = 400, description = "Invalid request", body = OpenAiErrorResponse),
        (status = 401, description = "Unauthorized", body = OpenAiErrorResponse),
        (status = 404, description = "Input file not found for the caller", body = OpenAiErrorResponse)
    ),
    security(("bearer_auth" = []))
)]
async fn create_batch(
    RequireAuth(auth): RequireAuth,
    State(app_state): State<Arc<AiGatewayAppState>>,
    axum::extract::Extension(metadata): axum::extract::Extension<RequestMetadata>,
    headers: HeaderMap,
    Json(request): Json<CreateBatchRequest>,
) -> Result<Response, Problem> {
    permission_guard!(auth, AiGatewayExecute);
    let byok = match byok_for(&auth, &headers) {
        Ok(byok) => byok,
        Err(error) => return Ok(error_to_response(error).into_response()),
    };
    let credential_type = credential_kind(&headers);
    match app_state
        .native_api_service
        .create_batch(owner_of(&auth), &byok, &request)
        .await
    {
        Ok(reply) => {
            audit_object(
                &app_state,
                &auth,
                &metadata,
                "ai_gateway.batch.created",
                &reply,
                None,
                credential_type,
            )
            .await;
            Ok(forward(reply, Some(credential_type)))
        }
        Err(call_error) => Ok(error_to_response(call_error).into_response()),
    }
}

#[utoipa::path(
    tag = "AI Gateway",
    get,
    path = "/ai/v1/batches/{batch_id}",
    params(("batch_id" = String, Path, description = "Provider batch id, e.g. batch_abc123")),
    responses(
        (status = 200, description = "Batch object", body = BatchObject),
        (status = 401, description = "Unauthorized", body = OpenAiErrorResponse),
        (status = 404, description = "No such batch created by the caller", body = OpenAiErrorResponse)
    ),
    security(("bearer_auth" = []))
)]
async fn retrieve_batch(
    RequireAuth(auth): RequireAuth,
    State(app_state): State<Arc<AiGatewayAppState>>,
    headers: HeaderMap,
    Path(batch_id): Path<String>,
) -> Result<Response, Problem> {
    permission_guard!(auth, AiGatewayExecute);
    let byok = match byok_for(&auth, &headers) {
        Ok(byok) => byok,
        Err(error) => return Ok(error_to_response(error).into_response()),
    };
    match app_state
        .native_api_service
        .retrieve_batch(owner_of(&auth), &byok, &batch_id)
        .await
    {
        Ok(reply) => Ok(forward(reply, Some(credential_kind(&headers)))),
        Err(call_error) => Ok(error_to_response(call_error).into_response()),
    }
}

#[utoipa::path(
    tag = "AI Gateway",
    post,
    path = "/ai/v1/batches/{batch_id}/cancel",
    params(("batch_id" = String, Path, description = "Provider batch id, e.g. batch_abc123")),
    responses(
        (status = 200, description = "Batch is cancelling", body = BatchObject),
        (status = 401, description = "Unauthorized", body = OpenAiErrorResponse),
        (status = 404, description = "No such batch created by the caller", body = OpenAiErrorResponse)
    ),
    security(("bearer_auth" = []))
)]
async fn cancel_batch(
    RequireAuth(auth): RequireAuth,
    State(app_state): State<Arc<AiGatewayAppState>>,
    axum::extract::Extension(metadata): axum::extract::Extension<RequestMetadata>,
    headers: HeaderMap,
    Path(batch_id): Path<String>,
) -> Result<Response, Problem> {
    permission_guard!(auth, AiGatewayExecute);
    let byok = match byok_for(&auth, &headers) {
        Ok(byok) => byok,
        Err(error) => return Ok(error_to_response(error).into_response()),
    };
    let credential_type = credential_kind(&headers);
    match app_state
        .native_api_service
        .cancel_batch(owner_of(&auth), &byok, &batch_id)
        .await
    {
        Ok(reply) => {
            audit_object(
                &app_state,
                &auth,
                &metadata,
                "ai_gateway.batch.cancelled",
                &reply,
                Some(&batch_id),
                credential_type,
            )
            .await;
            Ok(forward(reply, Some(credential_type)))
        }
        Err(call_error) => Ok(error_to_response(call_error).into_response()),
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn responses_media_contracts_distinguish_json_and_incremental_events() {
        use utoipa::OpenApi;
        let document = serde_json::to_value(super::AiGatewayNativeApiDoc::openapi()).unwrap();
        let responses =
            &document["paths"]["/ai/v1/responses"]["post"]["responses"]["200"]["content"];
        assert!(responses.get("application/json").is_some());
        assert!(responses.get("text/event-stream").is_some());
        let stream =
            &document["paths"]["/ai/v1/responses/stream"]["post"]["responses"]["200"]["content"];
        assert!(stream.get("text/event-stream").is_some());
        assert!(stream.get("application/json").is_none());
        let json =
            &document["paths"]["/ai/v1/responses/json"]["post"]["responses"]["200"]["content"];
        assert!(json.get("application/json").is_some());
        assert!(json.get("text/event-stream").is_none());
    }

    use super::*;

    #[test]
    fn openapi_registers_every_native_route_and_wire_schema() {
        let doc = AiGatewayNativeApiDoc::openapi();
        for path in [
            "/ai/v1/responses",
            "/ai/v1/files",
            "/ai/v1/files/{file_id}",
            "/ai/v1/files/{file_id}/content",
            "/ai/v1/batches",
            "/ai/v1/batches/{batch_id}",
            "/ai/v1/batches/{batch_id}/cancel",
        ] {
            assert!(doc.paths.paths.contains_key(path), "missing {path}");
        }
        let components = doc.components.unwrap();
        for schema in [
            "ResponsesRequest",
            "ResponseObject",
            "CreateBatchRequest",
            "BatchObject",
            "UploadFileForm",
        ] {
            assert!(components.schemas.contains_key(schema), "missing {schema}");
        }
    }

    #[test]
    fn filenames_are_reduced_to_a_safe_base_name() {
        assert_eq!(sanitize_filename(Some("claims.jsonl")), "claims.jsonl");
        assert_eq!(sanitize_filename(Some("../../etc/passwd")), "passwd");
        assert_eq!(sanitize_filename(Some("C:\\work\\run.jsonl")), "run.jsonl");
        assert_eq!(sanitize_filename(Some("bad\nname.jsonl")), "badname.jsonl");
        assert_eq!(sanitize_filename(Some("")), "batch.jsonl");
        assert_eq!(sanitize_filename(None), "batch.jsonl");
        assert_eq!(
            sanitize_filename(Some(&"a".repeat(400))).len(),
            MAX_FILENAME_LEN
        );
    }

    #[test]
    fn credential_kind_follows_the_byok_header() {
        let mut headers = HeaderMap::new();
        assert_eq!(credential_kind(&headers), CredentialType::System);
        headers.insert("x-provider-api-key", "sk-test".parse().unwrap());
        assert_eq!(credential_kind(&headers), CredentialType::Byok);
        headers.insert(
            "x-provider-api-key",
            axum::http::HeaderValue::from_bytes(b"\xff").unwrap(),
        );
        assert_eq!(credential_kind(&headers), CredentialType::System);
    }

    #[test]
    fn owner_is_the_user_or_the_deployment_tokens_project() {
        let token = AuthContext::new_deployment_token(
            12,
            None,
            None,
            3,
            "app-token".to_string(),
            vec![temps_entities::deployment_tokens::DeploymentTokenPermission::AiGatewayExecute],
        );
        assert_eq!(
            owner_of(&token),
            Owner {
                user_id: None,
                project_id: Some(12)
            }
        );
    }

    #[tokio::test]
    async fn forwarded_replies_keep_status_body_and_content_type() {
        let reply = UpstreamReply {
            status: reqwest::StatusCode::TOO_MANY_REQUESTS,
            content_type: Some("application/json".to_string()),
            body: bytes::Bytes::from_static(
                br#"{"error":{"message":"slow down","type":"rate_limit_error"}}"#,
            ),
        };
        let response = forward(reply, Some(CredentialType::System));
        assert_eq!(response.status(), StatusCode::TOO_MANY_REQUESTS);
        assert_eq!(
            response.headers().get("x-temps-credential-type").unwrap(),
            "system"
        );
        let body = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .expect("body");
        assert_eq!(
            &body[..],
            br#"{"error":{"message":"slow down","type":"rate_limit_error"}}"#
        );
    }
}
