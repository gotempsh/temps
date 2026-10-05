// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! OpenAI-native Responses, Files and Batch endpoints.
//!
//! Requests are forwarded to OpenAI unchanged so official SDKs work against
//! the gateway, with the gateway's usual model routing, catalog allowlist,
//! BYOK and usage logging applied.
//!
//! Files and batches created with an administrator-configured key live in
//! the operator's OpenAI account, a namespace every gateway caller shares.
//! Each one is recorded in `ai_gateway_objects` with its creator, and every
//! later read, download, cancel or delete is refused unless the caller is
//! that creator. Objects created with the caller's own key are not recorded:
//! OpenAI already scopes them to the caller's account.

use std::collections::HashSet;
use std::pin::Pin;
use std::sync::Arc;

use bytes::Bytes;
use futures_util::StreamExt;
use reqwest::Method;
use sea_orm::sea_query::{Expr, OnConflict};
use sea_orm::{
    ActiveValue::Set, ColumnTrait, DatabaseConnection, EntityTrait, QueryFilter, TransactionTrait,
};
use serde::Deserialize;
use temps_entities::ai_gateway_objects::{self, KIND_BATCH, KIND_FILE};
use tokio::io::AsyncWriteExt;
use tokio_stream::Stream;
use tracing::{error, info, warn};

use crate::error::AiGatewayError;
use crate::native_types::{
    BatchObject, CreateBatchRequest, FileObject, ResponseObject, ResponseUsage, ResponsesRequest,
};
use crate::providers::openai_native::{
    read_reply, validate_upstream_id, OpenAiNativeClient, UpstreamReply,
};
use crate::services::gateway_service::{
    ByokOverride, CredentialType, GatewayService, ResolvedCredentials,
};
use crate::services::usage_service::{AiRequestContext, UsageService};

/// Provider that serves the native endpoints.
const OPENAI: &str = "openai";

/// Largest batch input file accepted, matching OpenAI's limit.
pub const MAX_BATCH_FILE_BYTES: u64 = 200 * 1024 * 1024;
/// Most requests one batch input file may contain, matching OpenAI's limit.
pub const MAX_BATCH_REQUESTS: usize = 50_000;
/// Longest single request line held in memory while validating.
pub const MAX_BATCH_LINE_BYTES: usize = 32 * 1024 * 1024;
/// Longest `custom_id`, bounding the duplicate-detection set.
const MAX_CUSTOM_ID_LEN: usize = 512;
/// Endpoints a batch may target through the gateway.
pub const BATCH_ENDPOINTS: [&str; 3] = ["/v1/responses", "/v1/chat/completions", "/v1/embeddings"];

pub type ByteStream = Pin<Box<dyn Stream<Item = Result<Bytes, AiGatewayError>> + Send>>;

/// Who is calling: a user, or a project for deployment tokens.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Owner {
    pub user_id: Option<i32>,
    pub project_id: Option<i32>,
}

impl Owner {
    fn describe(&self) -> String {
        match (self.user_id, self.project_id) {
            (Some(user_id), _) => format!("user {user_id}"),
            (None, Some(project_id)) => format!("project {project_id}"),
            (None, None) => "an unidentified caller".to_string(),
        }
    }
}

// ============================================================================
// Batch input file validation
// ============================================================================

/// What a validated batch input file contains.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BatchFileSummary {
    pub model: String,
    pub endpoint: String,
    pub request_count: usize,
}

#[derive(Deserialize)]
struct BatchLine {
    custom_id: String,
    method: String,
    url: String,
    body: BatchLineBody,
}

#[derive(Deserialize)]
struct BatchLineBody {
    model: String,
    #[serde(flatten)]
    extra: serde_json::Map<String, serde_json::Value>,
}

/// Incremental validator for a JSONL batch input file.
///
/// Fed the upload chunk by chunk, it holds at most one request line in
/// memory. Every line must be a JSON object with a unique `custom_id`,
/// `method: "POST"`, a supported `url` and a `body.model`; all lines must
/// share one model and one endpoint, because the model decides which
/// provider key, and therefore which account, runs the batch.
#[derive(Default)]
pub struct BatchFileValidator {
    line: Vec<u8>,
    line_number: usize,
    model: Option<String>,
    endpoint: Option<String>,
    custom_ids: HashSet<String>,
}

impl BatchFileValidator {
    pub fn feed(&mut self, mut chunk: &[u8]) -> Result<(), AiGatewayError> {
        while let Some(newline) = chunk.iter().position(|byte| *byte == b'\n') {
            self.extend_line(&chunk[..newline])?;
            self.finish_line()?;
            chunk = &chunk[newline + 1..];
        }
        self.extend_line(chunk)
    }

    pub fn finish(mut self) -> Result<BatchFileSummary, AiGatewayError> {
        if !self.line.is_empty() {
            self.finish_line()?;
        }
        match (self.model, self.endpoint) {
            (Some(model), Some(endpoint)) => Ok(BatchFileSummary {
                model,
                endpoint,
                request_count: self.custom_ids.len(),
            }),
            _ => Err(invalid_file("the file contains no requests".to_string())),
        }
    }

    fn extend_line(&mut self, bytes: &[u8]) -> Result<(), AiGatewayError> {
        if self.line.len().saturating_add(bytes.len()) > MAX_BATCH_LINE_BYTES {
            return Err(invalid_file(format!(
                "line {} is longer than {} MiB",
                self.line_number + 1,
                MAX_BATCH_LINE_BYTES / (1024 * 1024)
            )));
        }
        self.line.extend_from_slice(bytes);
        Ok(())
    }

    fn finish_line(&mut self) -> Result<(), AiGatewayError> {
        self.line_number += 1;
        let number = self.line_number;
        let mut line = std::mem::take(&mut self.line);
        if line.last() == Some(&b'\r') {
            line.pop();
        }
        if line.iter().all(u8::is_ascii_whitespace) {
            return Err(invalid_file(format!("line {number} is empty")));
        }
        if self.custom_ids.len() >= MAX_BATCH_REQUESTS {
            return Err(invalid_file(format!(
                "more than {MAX_BATCH_REQUESTS} requests (line {number})"
            )));
        }
        let parsed: BatchLine = serde_json::from_slice(&line).map_err(|error| {
            invalid_file(format!(
                "line {number} is not a batch request (expected custom_id, method, url and body.model): {error}"
            ))
        })?;

        if parsed.custom_id.is_empty() || parsed.custom_id.len() > MAX_CUSTOM_ID_LEN {
            return Err(invalid_file(format!(
                "line {number}: custom_id must be 1-{MAX_CUSTOM_ID_LEN} characters"
            )));
        }
        if parsed.method != "POST" {
            return Err(invalid_file(format!(
                "line {number}: method '{}' is not supported, use POST",
                parsed.method
            )));
        }
        if !BATCH_ENDPOINTS.contains(&parsed.url.as_str()) {
            return Err(invalid_file(format!(
                "line {number}: url '{}' is not supported, use one of {}",
                parsed.url,
                BATCH_ENDPOINTS.join(", ")
            )));
        }
        reject_shared_state(&serde_json::Value::Object(parsed.body.extra.clone()))?;
        if parsed.body.model.trim().is_empty() {
            return Err(invalid_file(format!("line {number}: body.model is empty")));
        }
        match &self.model {
            None => self.model = Some(parsed.body.model),
            Some(model) if *model != parsed.body.model => {
                return Err(invalid_file(format!(
                    "line {number}: model '{}' differs from '{model}' on line 1; a batch runs one model",
                    parsed.body.model
                )));
            }
            Some(_) => {}
        }
        match &self.endpoint {
            None => self.endpoint = Some(parsed.url),
            Some(endpoint) if *endpoint != parsed.url => {
                return Err(invalid_file(format!(
                    "line {number}: url '{}' differs from '{endpoint}' on line 1; a batch targets one endpoint",
                    parsed.url
                )));
            }
            Some(_) => {}
        }
        if !self.custom_ids.insert(parsed.custom_id.clone()) {
            return Err(invalid_file(format!(
                "line {number}: custom_id '{}' is used more than once",
                parsed.custom_id
            )));
        }
        Ok(())
    }
}

fn invalid_file(reason: String) -> AiGatewayError {
    AiGatewayError::Validation {
        message: format!("Invalid batch input file: {reason}"),
    }
}

/// A validated batch input file waiting on local disk to be uploaded. The
/// file is deleted when this value is dropped.
pub struct SpooledBatchFile {
    file: tempfile::NamedTempFile,
    pub length: u64,
    pub summary: BatchFileSummary,
}

/// Write an uploaded batch file to a temporary file while validating it, so
/// memory use stays constant however large the upload is.
pub async fn spool_batch_file<S, E>(chunks: S) -> Result<SpooledBatchFile, AiGatewayError>
where
    S: Stream<Item = Result<Bytes, E>> + Send,
    E: std::fmt::Display,
{
    let temp = tempfile::Builder::new()
        .prefix("temps-ai-batch-")
        .suffix(".jsonl")
        .tempfile()
        .map_err(|error| AiGatewayError::Internal {
            message: format!("Failed to create a temporary file for the batch upload: {error}"),
        })?;
    let writer = temp.reopen().map_err(|error| AiGatewayError::Internal {
        message: format!(
            "Failed to open temporary batch file {}: {error}",
            temp.path().display()
        ),
    })?;
    let mut writer = tokio::fs::File::from_std(writer);
    let mut validator = BatchFileValidator::default();
    let mut length: u64 = 0;
    let mut chunks = std::pin::pin!(chunks);
    while let Some(chunk) = chunks.next().await {
        let chunk = chunk.map_err(|error| AiGatewayError::Validation {
            message: format!("Failed to read the uploaded batch file: {error}"),
        })?;
        length = length.saturating_add(chunk.len() as u64);
        if length > MAX_BATCH_FILE_BYTES {
            return Err(AiGatewayError::UploadTooLarge {
                limit_bytes: MAX_BATCH_FILE_BYTES,
            });
        }
        validator.feed(&chunk)?;
        writer
            .write_all(&chunk)
            .await
            .map_err(|error| AiGatewayError::Internal {
                message: format!(
                    "Failed to write temporary batch file {}: {error}",
                    temp.path().display()
                ),
            })?;
    }
    writer
        .flush()
        .await
        .map_err(|error| AiGatewayError::Internal {
            message: format!(
                "Failed to flush temporary batch file {}: {error}",
                temp.path().display()
            ),
        })?;
    let summary = validator.finish()?;
    Ok(SpooledBatchFile {
        file: temp,
        length,
        summary,
    })
}

// ============================================================================
// Service
// ============================================================================

/// Result of `POST /ai/v1/responses`.
pub enum ResponsesOutcome {
    /// A complete reply (success or provider error), forwarded as-is.
    Reply {
        reply: UpstreamReply,
        usage: Option<ResponseUsage>,
    },
    /// A successful streaming reply: the provider's server-sent events.
    Stream(ByteStream),
}

pub struct NativeApiService {
    db: Arc<DatabaseConnection>,
    gateway_service: Arc<GatewayService>,
    usage_service: Arc<UsageService>,
    client: OpenAiNativeClient,
    upload_slots: Arc<tokio::sync::Semaphore>,
}

impl NativeApiService {
    pub fn new(
        db: Arc<DatabaseConnection>,
        gateway_service: Arc<GatewayService>,
        usage_service: Arc<UsageService>,
    ) -> Self {
        Self {
            db,
            gateway_service,
            usage_service,
            client: OpenAiNativeClient::new(),
            upload_slots: Arc::new(tokio::sync::Semaphore::new(2)),
        }
    }

    /// Bound simultaneous spool files and validator memory; reject overload
    /// immediately instead of retaining queued request bodies.
    pub fn acquire_upload_slot(&self) -> Result<tokio::sync::OwnedSemaphorePermit, AiGatewayError> {
        self.upload_slots
            .clone()
            .try_acquire_owned()
            .map_err(|_| AiGatewayError::UploadCapacity)
    }

    fn base_url(&self, credentials: &ResolvedCredentials) -> Result<String, AiGatewayError> {
        match credentials.base_url.as_deref() {
            Some(base_url) => Ok(base_url.to_string()),
            None => self
                .gateway_service
                .default_base_url(credentials.provider_id)
                .map(str::to_string)
                .ok_or_else(|| AiGatewayError::ProviderNotConfigured {
                    provider: credentials.provider_id.to_string(),
                }),
        }
    }

    fn require_openai(
        credentials: &ResolvedCredentials,
        endpoint: &str,
        model: &str,
    ) -> Result<(), AiGatewayError> {
        if credentials.provider_id == OPENAI {
            Ok(())
        } else {
            Err(AiGatewayError::UnsupportedEndpoint {
                endpoint: endpoint.to_string(),
                model: model.to_string(),
                provider: credentials.provider_id.to_string(),
            })
        }
    }

    /// Credentials for a file or batch call made with the caller's own key,
    /// where there is no model to route on: the native endpoints only exist
    /// on OpenAI.
    fn byok_credentials(
        byok: &ByokOverride,
    ) -> Result<Option<ResolvedCredentials>, AiGatewayError> {
        let Some(api_key) = byok.api_key.clone() else {
            return Ok(None);
        };
        if let Some(base_url) = byok.base_url.as_deref() {
            temps_core::url_validation::validate_external_url(base_url).map_err(|error| {
                AiGatewayError::InvalidProviderUrl {
                    reason: error.to_string(),
                }
            })?;
        }
        Ok(Some(ResolvedCredentials {
            provider_id: OPENAI,
            api_key,
            base_url: byok.base_url.clone(),
            credential_type: CredentialType::Byok,
            system_key_id: None,
        }))
    }

    // ------------------------------------------------------------------------
    // Responses
    // ------------------------------------------------------------------------

    /// Forward a Responses API request to OpenAI.
    pub async fn create_response(
        &self,
        request: &ResponsesRequest,
        byok: &ByokOverride,
    ) -> Result<(ResponsesOutcome, CredentialType), AiGatewayError> {
        let credentials = self
            .gateway_service
            .resolve_credentials(&request.model, byok)
            .await?;
        Self::require_openai(&credentials, "/ai/v1/responses", &request.model)?;
        if credentials.credential_type == CredentialType::System {
            reject_stateful_request(request)?;
        }

        let body = Bytes::from(serde_json::to_vec(request).map_err(|error| {
            AiGatewayError::TranslationError {
                provider: OPENAI.to_string(),
                reason: format!(
                    "Failed to encode Responses request for model '{}': {error}",
                    request.model
                ),
            }
        })?);
        let base_url = self.base_url(&credentials)?;

        if !request.stream {
            let reply = self
                .client
                .send_buffered(
                    Method::POST,
                    &base_url,
                    &credentials.api_key,
                    "responses",
                    Some(body),
                )
                .await?;
            let usage = if reply.is_success() {
                serde_json::from_slice::<ResponseObject>(&reply.body)
                    .ok()
                    .and_then(|response| response.usage)
            } else {
                None
            };
            return Ok((
                ResponsesOutcome::Reply { reply, usage },
                credentials.credential_type,
            ));
        }

        let response = self
            .client
            .send(
                Method::POST,
                &base_url,
                &credentials.api_key,
                "responses",
                Some(body),
                false,
            )
            .await?;
        if !response.status().is_success() {
            let reply = read_reply(response, "responses").await?;
            return Ok((
                ResponsesOutcome::Reply { reply, usage: None },
                credentials.credential_type,
            ));
        }
        let model = request.model.clone();
        let stream = response.bytes_stream().map(move |chunk| {
            chunk.map_err(|error| AiGatewayError::StreamError {
                model: model.clone(),
                reason: error.without_url().to_string(),
            })
        });
        Ok((
            ResponsesOutcome::Stream(Box::pin(stream)),
            credentials.credential_type,
        ))
    }

    // ------------------------------------------------------------------------
    // Files
    // ------------------------------------------------------------------------

    /// Upload a validated batch input file and record its owner.
    pub async fn upload_batch_file(
        &self,
        owner: Owner,
        byok: &ByokOverride,
        spooled: SpooledBatchFile,
        filename: &str,
        text_fields: &[(String, String)],
    ) -> Result<UpstreamReply, AiGatewayError> {
        let summary = spooled.summary.clone();
        let credentials = self
            .gateway_service
            .resolve_credentials(&summary.model, byok)
            .await?;
        Self::require_openai(&credentials, "/ai/v1/files", &summary.model)?;
        let base_url = self.base_url(&credentials)?;

        let file = tokio::fs::File::open(spooled.file.path())
            .await
            .map_err(|error| AiGatewayError::Internal {
                message: format!(
                    "Failed to reopen temporary batch file {}: {error}",
                    spooled.file.path().display()
                ),
            })?;
        let reply = self
            .client
            .upload_file(
                &base_url,
                &credentials.api_key,
                "batch",
                filename,
                file,
                spooled.length,
                text_fields,
            )
            .await?;
        drop(spooled);
        if !reply.is_success() {
            return Ok(reply);
        }

        if let Some(key_id) = credentials.system_key_id {
            let uploaded: FileObject = parse_reply(&reply, "files")?;
            validate_upstream_id(KIND_FILE, &uploaded.id)?;
            if let Err(record_error) = self
                .record(
                    owner,
                    KIND_FILE,
                    &uploaded.id,
                    key_id,
                    Some(summary.model.clone()),
                    Some(summary.endpoint.clone()),
                )
                .await
            {
                self.cleanup_unrecorded_object(&credentials, KIND_FILE, &uploaded.id)
                    .await;
                return Err(record_error);
            }
            info!(
                file_id = uploaded.id,
                model = summary.model,
                endpoint = summary.endpoint,
                requests = summary.request_count,
                owner = owner.describe(),
                "AI gateway batch input file uploaded"
            );
        }
        Ok(reply)
    }

    pub async fn retrieve_file(
        &self,
        owner: Owner,
        byok: &ByokOverride,
        file_id: &str,
    ) -> Result<UpstreamReply, AiGatewayError> {
        validate_upstream_id("file", file_id)?;
        let (credentials, _) = self
            .object_credentials(owner, byok, KIND_FILE, file_id)
            .await?;
        let base_url = self.base_url(&credentials)?;
        self.client
            .send_buffered(
                Method::GET,
                &base_url,
                &credentials.api_key,
                &format!("files/{file_id}"),
                None,
            )
            .await
    }

    /// Download a file's content. Successful downloads are streamed, never
    /// buffered: batch result files can be hundreds of megabytes.
    pub async fn file_content(
        &self,
        owner: Owner,
        byok: &ByokOverride,
        file_id: &str,
    ) -> Result<Result<(Option<String>, ByteStream), UpstreamReply>, AiGatewayError> {
        validate_upstream_id("file", file_id)?;
        let (credentials, _) = self
            .object_credentials(owner, byok, KIND_FILE, file_id)
            .await?;
        let base_url = self.base_url(&credentials)?;
        let path = format!("files/{file_id}/content");
        let response = self
            .client
            .send(
                Method::GET,
                &base_url,
                &credentials.api_key,
                &path,
                None,
                true,
            )
            .await?;
        if !response.status().is_success() {
            return Ok(Err(read_reply(response, &path).await?));
        }
        let content_type = response
            .headers()
            .get(reqwest::header::CONTENT_TYPE)
            .and_then(|value| value.to_str().ok())
            .map(str::to_string);
        let file_id = file_id.to_string();
        let stream = response.bytes_stream().map(move |chunk| {
            chunk.map_err(|error| AiGatewayError::StreamError {
                model: format!("file {file_id}"),
                reason: error.without_url().to_string(),
            })
        });
        Ok(Ok((content_type, Box::pin(stream))))
    }

    pub async fn delete_file(
        &self,
        owner: Owner,
        byok: &ByokOverride,
        file_id: &str,
    ) -> Result<UpstreamReply, AiGatewayError> {
        validate_upstream_id("file", file_id)?;
        let (credentials, record) = self
            .object_credentials(owner, byok, KIND_FILE, file_id)
            .await?;
        let base_url = self.base_url(&credentials)?;
        let reply = self
            .client
            .send_buffered(
                Method::DELETE,
                &base_url,
                &credentials.api_key,
                &format!("files/{file_id}"),
                None,
            )
            .await?;
        if reply.is_success() {
            if let Some(record) = record {
                ai_gateway_objects::Entity::delete_by_id(record.id)
                    .exec(self.db.as_ref())
                    .await?;
            }
        }
        Ok(reply)
    }

    // ------------------------------------------------------------------------
    // Batches
    // ------------------------------------------------------------------------

    pub async fn create_batch(
        &self,
        owner: Owner,
        byok: &ByokOverride,
        request: &CreateBatchRequest,
    ) -> Result<UpstreamReply, AiGatewayError> {
        validate_upstream_id("file", &request.input_file_id)?;
        if !BATCH_ENDPOINTS.contains(&request.endpoint.as_str()) {
            return Err(AiGatewayError::Validation {
                message: format!(
                    "endpoint '{}' is not supported for batches, use one of {}",
                    request.endpoint,
                    BATCH_ENDPOINTS.join(", ")
                ),
            });
        }
        let body = Bytes::from(serde_json::to_vec(request).map_err(|error| {
            AiGatewayError::TranslationError {
                provider: OPENAI.to_string(),
                reason: format!(
                    "Failed to encode batch request for file '{}': {error}",
                    request.input_file_id
                ),
            }
        })?);

        if let Some(credentials) = Self::byok_credentials(byok)? {
            let base_url = self.base_url(&credentials)?;
            return self
                .client
                .send_buffered(
                    Method::POST,
                    &base_url,
                    &credentials.api_key,
                    "batches",
                    Some(body),
                )
                .await;
        }

        let input = self
            .owned_object(owner, KIND_FILE, &request.input_file_id)
            .await?;
        let (Some(model), Some(endpoint)) = (input.model.clone(), input.endpoint.clone()) else {
            return Err(AiGatewayError::Validation {
                message: format!(
                    "File '{}' is a batch result, not a batch input file",
                    request.input_file_id
                ),
            });
        };
        if endpoint != request.endpoint {
            return Err(AiGatewayError::Validation {
                message: format!(
                    "File '{}' contains {endpoint} requests but the batch targets {}",
                    request.input_file_id, request.endpoint
                ),
            });
        }
        // Re-check the catalog: the model may have been disabled since the
        // file was uploaded, and the batch must run on the key holding it.
        let credentials = self
            .gateway_service
            .resolve_credentials(
                &model,
                &ByokOverride {
                    api_key: None,
                    base_url: None,
                    system_key_id: Some(input.provider_key_id),
                },
            )
            .await?;
        let base_url = self.base_url(&credentials)?;
        let reply = self
            .client
            .send_buffered(
                Method::POST,
                &base_url,
                &credentials.api_key,
                "batches",
                Some(body),
            )
            .await?;
        if !reply.is_success() {
            return Ok(reply);
        }
        let batch: BatchObject = parse_reply(&reply, "batches")?;
        validate_upstream_id(KIND_BATCH, &batch.id)?;
        if let Err(record_error) = self
            .record(
                owner,
                KIND_BATCH,
                &batch.id,
                input.provider_key_id,
                Some(model.clone()),
                Some(endpoint),
            )
            .await
        {
            self.cleanup_unrecorded_object(&credentials, KIND_BATCH, &batch.id)
                .await;
            return Err(record_error);
        }
        info!(
            batch_id = batch.id,
            input_file_id = request.input_file_id,
            model = model,
            owner = owner.describe(),
            "AI gateway batch created"
        );
        Ok(reply)
    }

    pub async fn retrieve_batch(
        &self,
        owner: Owner,
        byok: &ByokOverride,
        batch_id: &str,
    ) -> Result<UpstreamReply, AiGatewayError> {
        self.batch_call(
            owner,
            byok,
            batch_id,
            Method::GET,
            format!("batches/{batch_id}"),
        )
        .await
    }

    pub async fn cancel_batch(
        &self,
        owner: Owner,
        byok: &ByokOverride,
        batch_id: &str,
    ) -> Result<UpstreamReply, AiGatewayError> {
        self.batch_call(
            owner,
            byok,
            batch_id,
            Method::POST,
            format!("batches/{batch_id}/cancel"),
        )
        .await
    }

    async fn batch_call(
        &self,
        owner: Owner,
        byok: &ByokOverride,
        batch_id: &str,
        method: Method,
        path: String,
    ) -> Result<UpstreamReply, AiGatewayError> {
        validate_upstream_id("batch", batch_id)?;
        let (credentials, record) = self
            .object_credentials(owner, byok, KIND_BATCH, batch_id)
            .await?;
        let base_url = self.base_url(&credentials)?;
        let reply = self
            .client
            .send_buffered(method, &base_url, &credentials.api_key, &path, None)
            .await?;
        if let (true, Some(record)) = (reply.is_success(), record) {
            match serde_json::from_slice::<BatchObject>(&reply.body) {
                Ok(batch) => self.observe_batch(&record, &batch).await?,
                Err(parse_error) => warn!(
                    batch_id,
                    error = %parse_error,
                    "Could not read batch object; result files and usage not recorded"
                ),
            }
        }
        Ok(reply)
    }

    /// Record what a batch reply reveals: its result files belong to the
    /// batch's owner, and once it is finished its token usage is logged,
    /// exactly once however many times the batch is polled.
    async fn observe_batch(
        &self,
        record: &ai_gateway_objects::Model,
        batch: &BatchObject,
    ) -> Result<(), AiGatewayError> {
        let owner = Owner {
            user_id: record.owner_user_id,
            project_id: record.owner_project_id,
        };
        for file_id in [&batch.output_file_id, &batch.error_file_id]
            .into_iter()
            .flatten()
        {
            if validate_upstream_id("file", file_id).is_ok() {
                self.record(
                    owner,
                    KIND_FILE,
                    file_id,
                    record.provider_key_id,
                    None,
                    None,
                )
                .await?;
            }
        }

        if !batch.is_terminal() || record.usage_recorded_at.is_some() || batch.usage.is_none() {
            return Ok(());
        }
        let transaction = self.db.begin().await?;
        let claimed = ai_gateway_objects::Entity::update_many()
            .col_expr(
                ai_gateway_objects::Column::UsageRecordedAt,
                Expr::value(chrono::Utc::now()),
            )
            .filter(ai_gateway_objects::Column::Id.eq(record.id))
            .filter(ai_gateway_objects::Column::UsageRecordedAt.is_null())
            .exec(&transaction)
            .await?;
        if claimed.rows_affected != 1 {
            transaction.commit().await?;
            return Ok(()); // another poll recorded it first
        }
        let usage = batch.usage.clone().unwrap_or_default();
        if usage.input_tokens == 0 && usage.output_tokens == 0 {
            transaction.commit().await?;
            return Ok(());
        }
        let model = batch
            .model
            .clone()
            .or_else(|| record.model.clone())
            .unwrap_or_else(|| "unknown".to_string());
        let context = AiRequestContext {
            conversation_id: None,
            tags: vec!["batch".to_string()],
            request_id: Some(batch.id.clone()),
            trace_id: None,
        };
        // Latency is not meaningful for a job that ran for hours; 0 keeps it
        // out of latency percentiles.
        self.usage_service
            .log_usage_on_connection(
                &transaction,
                owner.user_id,
                &record.provider,
                &model,
                usage.input_tokens,
                usage.output_tokens,
                0,
                0,
                200,
                false,
                false,
                &context,
            )
            .await?;
        transaction.commit().await?;
        Ok(())
    }

    /// Undo a provider operation when its ownership row could not be saved.
    /// Never leave an untracked batch running on the shared operator key.
    async fn cleanup_unrecorded_object(
        &self,
        credentials: &ResolvedCredentials,
        kind: &str,
        id: &str,
    ) {
        let Ok(base_url) = self.base_url(credentials) else {
            return;
        };
        let (method, path) = if kind == KIND_BATCH {
            (Method::POST, format!("batches/{id}/cancel"))
        } else {
            (Method::DELETE, format!("files/{id}"))
        };
        match self
            .client
            .send_buffered(method, &base_url, &credentials.api_key, &path, None)
            .await
        {
            Ok(reply) if reply.is_success() => {}
            Ok(reply) => error!(
                kind,
                id,
                status = reply.status.as_u16(),
                "Failed to clean up unrecorded AI gateway object"
            ),
            Err(error) => {
                error!(kind, id, error = %error, "Failed to clean up unrecorded AI gateway object")
            }
        }
    }

    // ------------------------------------------------------------------------
    // Ownership
    // ------------------------------------------------------------------------

    /// Credentials for an existing file or batch: the caller's own key when
    /// they supplied one, otherwise the administrator key that created the
    /// object, after checking the caller created it.
    async fn object_credentials(
        &self,
        owner: Owner,
        byok: &ByokOverride,
        kind: &str,
        upstream_id: &str,
    ) -> Result<(ResolvedCredentials, Option<ai_gateway_objects::Model>), AiGatewayError> {
        if let Some(credentials) = Self::byok_credentials(byok)? {
            return Ok((credentials, None));
        }
        let record = self.owned_object(owner, kind, upstream_id).await?;
        let credentials = self
            .gateway_service
            .system_key_credentials(record.provider_key_id)
            .await?;
        Ok((credentials, Some(record)))
    }

    /// The caller's record of an object. Objects owned by someone else are
    /// reported as not found, so ids cannot be probed.
    async fn owned_object(
        &self,
        owner: Owner,
        kind: &str,
        upstream_id: &str,
    ) -> Result<ai_gateway_objects::Model, AiGatewayError> {
        let mut query = ai_gateway_objects::Entity::find()
            .filter(ai_gateway_objects::Column::Kind.eq(kind))
            .filter(ai_gateway_objects::Column::UpstreamId.eq(upstream_id));
        query = match (owner.user_id, owner.project_id) {
            (Some(user_id), _) => query.filter(ai_gateway_objects::Column::OwnerUserId.eq(user_id)),
            (None, Some(project_id)) => {
                query.filter(ai_gateway_objects::Column::OwnerProjectId.eq(project_id))
            }
            (None, None) => {
                return Err(AiGatewayError::ObjectNotFound {
                    kind: kind.to_string(),
                    id: upstream_id.to_string(),
                })
            }
        };
        query
            .one(self.db.as_ref())
            .await?
            .ok_or_else(|| AiGatewayError::ObjectNotFound {
                kind: kind.to_string(),
                id: upstream_id.to_string(),
            })
    }

    async fn record(
        &self,
        owner: Owner,
        kind: &str,
        upstream_id: &str,
        provider_key_id: i32,
        model: Option<String>,
        endpoint: Option<String>,
    ) -> Result<(), AiGatewayError> {
        if owner.user_id.is_none() && owner.project_id.is_none() {
            return Err(AiGatewayError::Validation {
                message: format!(
                    "Cannot record {kind} '{upstream_id}': the caller is neither a user nor a deployment token"
                ),
            });
        }
        let row = ai_gateway_objects::ActiveModel {
            kind: Set(kind.to_string()),
            upstream_id: Set(upstream_id.to_string()),
            provider: Set(OPENAI.to_string()),
            provider_key_id: Set(provider_key_id),
            owner_user_id: Set(owner.user_id),
            owner_project_id: Set(if owner.user_id.is_some() {
                None
            } else {
                owner.project_id
            }),
            model: Set(model),
            endpoint: Set(endpoint),
            usage_recorded_at: Set(None),
            created_at: Set(chrono::Utc::now()),
            ..Default::default()
        };
        ai_gateway_objects::Entity::insert(row)
            .on_conflict(
                OnConflict::columns([
                    ai_gateway_objects::Column::ProviderKeyId,
                    ai_gateway_objects::Column::Kind,
                    ai_gateway_objects::Column::UpstreamId,
                ])
                .do_nothing()
                .to_owned(),
            )
            .exec_without_returning(self.db.as_ref())
            .await?;
        Ok(())
    }
}

/// Requests that read state stored in the provider account. With the shared
/// administrator key that account belongs to every gateway caller, so these
/// need the caller's own key.
fn reject_stateful_request(request: &ResponsesRequest) -> Result<(), AiGatewayError> {
    let field = if request.previous_response_id.is_some() {
        Some("previous_response_id")
    } else if request.conversation.is_some() {
        Some("conversation")
    } else if request.background == Some(true) {
        Some("background")
    } else {
        None
    };
    match field {
        None => reject_shared_state(&serde_json::Value::Object(request.extra.clone())),
        Some(field) => Err(AiGatewayError::Validation {
            message: format!(
                "'{field}' reads state stored in the provider account, which the gateway's shared \
                 key cannot scope to you. Send your own key in X-Provider-Api-Key to use it, or \
                 pass the earlier output in 'input' instead."
            ),
        }),
    }
}

/// Provider objects referenced anywhere in input or tool configuration live
/// in a shared account. Until those objects have ownership checks, fail
/// closed rather than allowing a caller to resolve an arbitrary provider ID.
fn reject_shared_state(value: &serde_json::Value) -> Result<(), AiGatewayError> {
    match value {
        serde_json::Value::Object(object) => {
            for (field, value) in object {
                // These are user schemas or metadata, never references that
                // the provider resolves in its account. A schema property
                // named file_id must remain valid for structured outputs.
                if matches!(
                    field.as_str(),
                    "text" | "response_format" | "parameters" | "metadata" | "reasoning"
                ) {
                    continue;
                }
                if matches!(
                    field.as_str(),
                    "previous_response_id"
                        | "conversation"
                        | "file_id"
                        | "file_ids"
                        | "vector_store_ids"
                        | "container_id"
                        | "container"
                        | "prompt"
                ) && !value.is_null()
                    || field == "background" && value == &serde_json::Value::Bool(true)
                    || field == "type" && value == "item_reference"
                {
                    return Err(AiGatewayError::Validation {
                        message: format!("'{field}' references provider account state; use X-Provider-Api-Key for a Responses request with your own key. Batch inputs must be stateless."),
                    });
                }
                reject_shared_state(value)?;
            }
        }
        serde_json::Value::Array(items) => {
            for item in items {
                reject_shared_state(item)?;
            }
        }
        _ => {}
    }
    Ok(())
}

fn parse_reply<T: serde::de::DeserializeOwned>(
    reply: &UpstreamReply,
    path: &str,
) -> Result<T, AiGatewayError> {
    serde_json::from_slice(&reply.body).map_err(|error| AiGatewayError::TranslationError {
        provider: OPENAI.to_string(),
        reason: format!("Failed to parse the reply from /{path}: {error}"),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn service(db: Arc<DatabaseConnection>) -> NativeApiService {
        let encryption = Arc::new(
            temps_core::EncryptionService::new("01234567890123456789012345678901").unwrap(),
        );
        let keys = Arc::new(crate::services::ProviderKeyService::new(
            db.clone(),
            encryption,
        ));
        NativeApiService::new(
            db.clone(),
            Arc::new(GatewayService::new(keys)),
            Arc::new(UsageService::new(db)),
        )
    }

    #[tokio::test]
    async fn ownership_lookup_is_scoped_to_the_user_or_project() {
        use sea_orm::{DatabaseBackend, MockDatabase};
        for (owner, column) in [
            (
                Owner {
                    user_id: Some(4),
                    project_id: Some(9),
                },
                "owner_user_id",
            ),
            (
                Owner {
                    user_id: None,
                    project_id: Some(9),
                },
                "owner_project_id",
            ),
        ] {
            let db = Arc::new(
                MockDatabase::new(DatabaseBackend::Postgres)
                    .append_query_results([Vec::<ai_gateway_objects::Model>::new()])
                    .into_connection(),
            );
            let svc = service(db.clone());
            let result = svc.owned_object(owner, KIND_FILE, "file-private").await;
            assert!(matches!(result, Err(AiGatewayError::ObjectNotFound { .. })));
            drop(svc);
            let log = Arc::try_unwrap(db).unwrap().into_transaction_log();
            let sql = format!("{log:?}");
            assert!(sql.contains(column), "{sql}");
            assert!(sql.contains("file-private"), "{sql}");
        }
    }

    #[tokio::test]
    async fn unidentified_callers_fail_before_database_or_upstream_access() {
        let db = Arc::new(
            sea_orm::MockDatabase::new(sea_orm::DatabaseBackend::Postgres).into_connection(),
        );
        let svc = service(db);
        assert!(matches!(
            svc.owned_object(
                Owner {
                    user_id: None,
                    project_id: None
                },
                KIND_BATCH,
                "batch_private"
            )
            .await,
            Err(AiGatewayError::ObjectNotFound { .. })
        ));
    }

    #[tokio::test]
    async fn upload_capacity_is_released_when_a_request_finishes() {
        let db = Arc::new(
            sea_orm::MockDatabase::new(sea_orm::DatabaseBackend::Postgres).into_connection(),
        );
        let svc = service(db);
        let first = svc.acquire_upload_slot().unwrap();
        let _second = svc.acquire_upload_slot().unwrap();
        assert!(matches!(
            svc.acquire_upload_slot(),
            Err(AiGatewayError::UploadCapacity)
        ));
        drop(first);
        assert!(svc.acquire_upload_slot().is_ok());
    }

    fn batch_record() -> ai_gateway_objects::Model {
        ai_gateway_objects::Model {
            id: 1,
            kind: KIND_BATCH.into(),
            upstream_id: "batch_test".into(),
            provider: OPENAI.into(),
            provider_key_id: 2,
            owner_user_id: Some(4),
            owner_project_id: None,
            model: Some("gpt-4o".into()),
            endpoint: Some("/v1/responses".into()),
            usage_recorded_at: None,
            created_at: chrono::Utc::now(),
        }
    }

    fn finished_batch() -> BatchObject {
        serde_json::from_value(
            serde_json::json!({"id":"batch_test", "object":"batch", "status":"completed",
            "usage":{"input_tokens":10,"output_tokens":5,"total_tokens":15}}),
        )
        .unwrap()
    }

    #[tokio::test]
    async fn failed_usage_insert_rolls_back_the_batch_marker() {
        use sea_orm::{DatabaseBackend, DbErr, MockDatabase, MockExecResult};
        let db = Arc::new(
            MockDatabase::new(DatabaseBackend::Postgres)
                .append_exec_results([MockExecResult {
                    last_insert_id: 0,
                    rows_affected: 1,
                }])
                .append_query_errors([DbErr::Custom("usage insert failed".into())])
                .into_connection(),
        );
        let svc = service(db.clone());
        assert!(svc
            .observe_batch(&batch_record(), &finished_batch())
            .await
            .is_err());
        drop(svc);
        let sql = format!("{:?}", Arc::try_unwrap(db).unwrap().into_transaction_log());
        assert!(sql.contains("ROLLBACK"), "{sql}");
        assert!(sql.contains("usage_recorded_at"), "{sql}");
    }

    #[tokio::test]
    async fn concurrent_poll_that_loses_the_marker_does_not_insert_usage() {
        use sea_orm::{DatabaseBackend, MockDatabase, MockExecResult};
        let db = Arc::new(
            MockDatabase::new(DatabaseBackend::Postgres)
                .append_exec_results([MockExecResult {
                    last_insert_id: 0,
                    rows_affected: 0,
                }])
                .into_connection(),
        );
        let svc = service(db.clone());
        svc.observe_batch(&batch_record(), &finished_batch())
            .await
            .unwrap();
        drop(svc);
        let sql = format!("{:?}", Arc::try_unwrap(db).unwrap().into_transaction_log());
        assert!(sql.contains("COMMIT"), "{sql}");
        assert!(!sql.contains("ai_usage_logs"), "{sql}");
    }

    #[tokio::test]
    async fn unrecorded_provider_objects_are_deleted_or_cancelled() {
        use wiremock::{
            matchers::{header, method, path},
            Mock, MockServer, ResponseTemplate,
        };
        let server = MockServer::start().await;
        for (kind, id, verb, route) in [
            (KIND_FILE, "file-orphan", "DELETE", "/v1/files/file-orphan"),
            (
                KIND_BATCH,
                "batch_orphan",
                "POST",
                "/v1/batches/batch_orphan/cancel",
            ),
        ] {
            Mock::given(method(verb))
                .and(path(route))
                .and(header("authorization", "Bearer sk-test"))
                .respond_with(
                    ResponseTemplate::new(200).set_body_json(serde_json::json!({"id":id})),
                )
                .expect(1)
                .mount(&server)
                .await;
            let db = Arc::new(
                sea_orm::MockDatabase::new(sea_orm::DatabaseBackend::Postgres).into_connection(),
            );
            let mut svc = service(db);
            svc.client = OpenAiNativeClient::for_test();
            svc.cleanup_unrecorded_object(
                &ResolvedCredentials {
                    provider_id: OPENAI,
                    api_key: "sk-test".into(),
                    base_url: Some(format!("{}/v1", server.uri())),
                    credential_type: CredentialType::System,
                    system_key_id: Some(2),
                },
                kind,
                id,
            )
            .await;
        }
    }

    fn line(custom_id: &str, url: &str, model: &str) -> String {
        format!(
            r#"{{"custom_id":"{custom_id}","method":"POST","url":"{url}","body":{{"model":"{model}","input":"check this claim"}}}}"#
        )
    }

    fn validate(content: &str) -> Result<BatchFileSummary, AiGatewayError> {
        let mut validator = BatchFileValidator::default();
        validator.feed(content.as_bytes())?;
        validator.finish()
    }

    fn validation_message(result: Result<BatchFileSummary, AiGatewayError>) -> String {
        match result {
            Err(AiGatewayError::Validation { message }) => message,
            other => panic!("expected a validation error, got {other:?}"),
        }
    }

    #[test]
    fn valid_file_reports_model_endpoint_and_count() {
        let content = format!(
            "{}\n{}\n{}\n",
            line("page-1", "/v1/responses", "gpt-6-luna"),
            line("page-2", "/v1/responses", "gpt-6-luna"),
            line("page-3", "/v1/responses", "gpt-6-luna"),
        );
        let summary = validate(&content).expect("valid");
        assert_eq!(
            summary,
            BatchFileSummary {
                model: "gpt-6-luna".into(),
                endpoint: "/v1/responses".into(),
                request_count: 3,
            }
        );
    }

    #[test]
    fn last_line_without_newline_and_crlf_are_accepted() {
        let content = format!(
            "{}\r\n{}",
            line("a", "/v1/chat/completions", "gpt-6-luna"),
            line("b", "/v1/chat/completions", "gpt-6-luna"),
        );
        assert_eq!(validate(&content).expect("valid").request_count, 2);
    }

    #[test]
    fn lines_split_across_chunks_are_reassembled() {
        let content = format!(
            "{}\n{}\n",
            line("a", "/v1/responses", "gpt-6-luna"),
            line("b", "/v1/responses", "gpt-6-luna"),
        );
        let mut validator = BatchFileValidator::default();
        for chunk in content.as_bytes().chunks(7) {
            validator.feed(chunk).expect("chunk");
        }
        assert_eq!(validator.finish().expect("valid").request_count, 2);
    }

    #[test]
    fn empty_file_is_rejected() {
        assert!(validation_message(validate("")).contains("no requests"));
    }

    #[test]
    fn blank_line_is_rejected_with_its_number() {
        let content = format!(
            "{}\n\n{}\n",
            line("a", "/v1/responses", "m"),
            line("b", "/v1/responses", "m")
        );
        assert!(validation_message(validate(&content)).contains("line 2 is empty"));
    }

    #[test]
    fn mixed_models_are_rejected() {
        let content = format!(
            "{}\n{}\n",
            line("a", "/v1/responses", "gpt-6-luna"),
            line("b", "/v1/responses", "gpt-6-sol"),
        );
        let message = validation_message(validate(&content));
        assert!(message.contains("line 2"), "{message}");
        assert!(message.contains("one model"), "{message}");
    }

    #[test]
    fn mixed_endpoints_are_rejected() {
        let content = format!(
            "{}\n{}\n",
            line("a", "/v1/responses", "gpt-6-luna"),
            line("b", "/v1/chat/completions", "gpt-6-luna"),
        );
        assert!(validation_message(validate(&content)).contains("one endpoint"));
    }

    #[test]
    fn unsupported_url_is_rejected() {
        let content = line("a", "/v1/images/generations", "gpt-image-2");
        assert!(validation_message(validate(&content)).contains("not supported"));
    }

    #[test]
    fn non_post_method_is_rejected() {
        let content =
            r#"{"custom_id":"a","method":"GET","url":"/v1/responses","body":{"model":"m"}}"#;
        assert!(validation_message(validate(content)).contains("use POST"));
    }

    #[test]
    fn duplicate_custom_id_is_rejected() {
        let content = format!(
            "{}\n{}\n",
            line("same", "/v1/responses", "m"),
            line("same", "/v1/responses", "m"),
        );
        assert!(validation_message(validate(&content)).contains("more than once"));
    }

    #[test]
    fn malformed_json_names_the_line() {
        let content = format!("{}\n{{not json\n", line("a", "/v1/responses", "m"));
        assert!(validation_message(validate(&content)).contains("line 2 is not a batch request"));
    }

    #[test]
    fn missing_model_is_rejected() {
        let content =
            r#"{"custom_id":"a","method":"POST","url":"/v1/responses","body":{"input":"x"}}"#;
        assert!(validation_message(validate(content)).contains("line 1 is not a batch request"));
    }

    #[test]
    fn oversized_line_is_rejected_without_buffering_it() {
        let mut validator = BatchFileValidator::default();
        let chunk = vec![b'x'; 1024 * 1024];
        let mut result = Ok(());
        for _ in 0..=(MAX_BATCH_LINE_BYTES / chunk.len()) {
            result = validator.feed(&chunk);
            if result.is_err() {
                break;
            }
        }
        match result {
            Err(AiGatewayError::Validation { message }) => assert!(message.contains("longer than")),
            other => panic!("expected oversized-line error, got {other:?}"),
        }
        assert!(validator.line.len() <= MAX_BATCH_LINE_BYTES);
    }

    #[tokio::test]
    async fn spooling_writes_the_upload_and_validates_it() {
        let content = format!(
            "{}\n{}\n",
            line("a", "/v1/responses", "gpt-6-luna"),
            line("b", "/v1/responses", "gpt-6-luna"),
        );
        let chunks: Vec<Result<Bytes, std::io::Error>> = content
            .as_bytes()
            .chunks(10)
            .map(|chunk| Ok(Bytes::copy_from_slice(chunk)))
            .collect();
        let spooled = spool_batch_file(tokio_stream::iter(chunks))
            .await
            .expect("spooled");
        assert_eq!(spooled.length, content.len() as u64);
        assert_eq!(spooled.summary.request_count, 2);
        let written = tokio::fs::read_to_string(spooled.file.path())
            .await
            .expect("read spool");
        assert_eq!(written, content);
        let path = spooled.file.path().to_path_buf();
        drop(spooled);
        assert!(!path.exists(), "the spool file is removed when dropped");
    }

    #[tokio::test]
    async fn spooling_rejects_invalid_content() {
        let chunks: Vec<Result<Bytes, std::io::Error>> = vec![Ok(Bytes::from_static(b"nope\n"))];
        let result = spool_batch_file(tokio_stream::iter(chunks)).await;
        assert!(matches!(result, Err(AiGatewayError::Validation { .. })));
    }

    #[tokio::test]
    async fn spooling_surfaces_read_errors() {
        let chunks: Vec<Result<Bytes, std::io::Error>> =
            vec![Err(std::io::Error::other("client disconnected"))];
        match spool_batch_file(tokio_stream::iter(chunks)).await {
            Err(AiGatewayError::Validation { message }) => {
                assert!(message.contains("client disconnected"))
            }
            other => panic!("expected read error, got {:?}", other.err()),
        }
    }

    fn responses_request(json: serde_json::Value) -> ResponsesRequest {
        serde_json::from_value(json).expect("request")
    }

    #[test]
    fn stateless_responses_requests_are_allowed_on_the_shared_key() {
        let request = responses_request(serde_json::json!({"model": "gpt-6-luna", "input": "hi"}));
        assert!(reject_stateful_request(&request).is_ok());
        let request = responses_request(
            serde_json::json!({"model": "gpt-6-luna", "input": "hi", "background": false}),
        );
        assert!(reject_stateful_request(&request).is_ok());
    }

    #[test]
    fn stateful_responses_requests_need_the_callers_key() {
        for (field, value) in [
            ("previous_response_id", serde_json::json!("resp_123")),
            ("conversation", serde_json::json!("conv_123")),
            ("background", serde_json::json!(true)),
        ] {
            let mut body = serde_json::json!({"model": "gpt-6-luna", "input": "hi"});
            body[field] = value;
            match reject_stateful_request(&responses_request(body)) {
                Err(AiGatewayError::Validation { message }) => {
                    assert!(message.contains(field), "{message}");
                    assert!(message.contains("X-Provider-Api-Key"), "{message}");
                }
                other => panic!("{field} must be rejected, got {other:?}"),
            }
        }
    }

    #[test]
    fn shared_key_rejects_nested_provider_object_references() {
        for extra in [
            serde_json::json!({"input":[{"type":"item_reference","id":"resp_other"}]}),
            serde_json::json!({"input":[{"content":[{"type":"input_file","file_id":"file-other"}]}]}),
            serde_json::json!({"tools":[{"type":"file_search","vector_store_ids":["vs_other"]}]}),
            serde_json::json!({"tools":[{"type":"code_interpreter","container":"cntr_other"}]}),
            serde_json::json!({"prompt":{"id":"pmpt_other"}}),
        ] {
            let mut request = responses_request(serde_json::json!({"model":"gpt-4o","input":"hi"}));
            request.extra = extra.as_object().unwrap().clone();
            assert!(reject_stateful_request(&request).is_err(), "{extra}");
        }
    }

    #[test]
    fn structured_output_schemas_can_name_provider_reference_fields() {
        let request = responses_request(serde_json::json!({"model":"gpt-4o","input":"hi",
            "text":{"format":{"type":"json_schema","schema":{"properties":{
                "file_id":{"type":"string"},"prompt":{"type":"string"}}}}}}));
        assert!(reject_stateful_request(&request).is_ok());
    }

    #[test]
    fn batch_inputs_cannot_bypass_shared_account_isolation() {
        let content = r#"{"custom_id":"a","method":"POST","url":"/v1/responses","body":{"model":"gpt-4o","previous_response_id":"resp_other"}}"#;
        assert!(validation_message(validate(content)).contains("provider account state"));
    }

    #[test]
    fn owner_description_names_the_caller() {
        assert_eq!(
            Owner {
                user_id: Some(4),
                project_id: None
            }
            .describe(),
            "user 4"
        );
        assert_eq!(
            Owner {
                user_id: None,
                project_id: Some(9)
            }
            .describe(),
            "project 9"
        );
    }
}
