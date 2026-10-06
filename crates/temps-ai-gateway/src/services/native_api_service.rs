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
//! that creator. BYOK uploads and batches also retain accounting metadata
//! and encrypted credentials so completed work is reconciled without client polling.

use std::collections::HashSet;
use std::pin::Pin;
use std::sync::Arc;

use bytes::Bytes;
use futures_util::StreamExt;
use reqwest::Method;
use sea_orm::sea_query::{Expr, OnConflict};
use sea_orm::{
    ActiveValue::Set, ColumnTrait, DatabaseConnection, EntityTrait, QueryFilter, QueryOrder,
    QuerySelect, TransactionTrait,
};
use serde::Deserialize;
use sha2::{Digest, Sha256};
use temps_entities::ai_gateway_objects::{self, KIND_BATCH, KIND_FILE};
use tokio::io::AsyncWriteExt;
use tokio_stream::Stream;
use tracing::{error, info, warn};

use crate::error::AiGatewayError;
use crate::native_types::{
    BatchObject, BatchUsage, CreateBatchRequest, FileObject, ResponseObject, ResponseUsage,
    ResponsesRequest,
};
use crate::providers::openai_native::{
    read_reply, validate_upstream_id, OpenAiNativeClient, UpstreamReply, FILE_TRANSFER_TIMEOUT,
    NATIVE_REQUEST_TIMEOUT,
};
use crate::services::gateway_service::{
    ByokOverride, CredentialType, GatewayService, ResolvedCredentials,
};
use crate::services::usage_service::{AiRequestContext, UsageService};

/// Provider that serves the native endpoints.
const OPENAI: &str = "openai";

// Allow metadata retrieval, a full result transfer and a minute for accounting.
// Keep the durable lease longer than the entire job so another worker cannot
// restart the same download while it is still in progress.
const BATCH_RECONCILIATION_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(
    NATIVE_REQUEST_TIMEOUT.as_secs() + FILE_TRANSFER_TIMEOUT.as_secs() + 60,
);
const BATCH_RETRY_LEASE: chrono::Duration =
    chrono::Duration::seconds(BATCH_RECONCILIATION_TIMEOUT.as_secs() as i64 + 60);

/// Largest batch input file accepted, matching OpenAI's limit.
pub const MAX_BATCH_FILE_BYTES: u64 = 200 * 1024 * 1024;
/// Most requests one batch input file may contain, matching OpenAI's limit.
pub const MAX_BATCH_REQUESTS: usize = 50_000;
/// Longest single request line held in memory while validating.
pub const MAX_BATCH_LINE_BYTES: usize = 32 * 1024 * 1024;
/// Longest `custom_id`, bounding the duplicate-detection set.
const MAX_CUSTOM_ID_LEN: usize = 512;
const MAX_MODEL_ID_BYTES: usize = 256;
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
                parsed.method.chars().take(16).collect::<String>()
            )));
        }
        if !BATCH_ENDPOINTS.contains(&parsed.url.as_str()) {
            return Err(invalid_file(format!(
                "line {number}: url '{}' is not supported, use one of {}",
                parsed.url.chars().take(128).collect::<String>(),
                BATCH_ENDPOINTS.join(", ")
            )));
        }
        reject_shared_state(&serde_json::Value::Object(parsed.body.extra.clone()))?;
        validate_model_id(&parsed.body.model).map_err(|_| invalid_file(format!("line {number}: body.model must be a nonempty name of at most {MAX_MODEL_ID_BYTES} bytes")))?;
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

pub(crate) fn validate_model_id(model: &str) -> Result<(), AiGatewayError> {
    if model.trim().is_empty() || model.len() > MAX_MODEL_ID_BYTES {
        return Err(AiGatewayError::Validation {
            message: format!("model must be a nonempty name of at most {MAX_MODEL_ID_BYTES} bytes"),
        });
    }
    Ok(())
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

#[derive(Clone)]
pub struct NativeApiService {
    db: Arc<DatabaseConnection>,
    gateway_service: Arc<GatewayService>,
    usage_service: Arc<UsageService>,
    client: OpenAiNativeClient,
    upload_slots: Arc<tokio::sync::Semaphore>,
    reconciliation_slots: Arc<tokio::sync::Semaphore>,
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
            reconciliation_slots: Arc::new(tokio::sync::Semaphore::new(2)),
        }
    }

    /// One worker per app state; a weak reference lets it stop when the plugin is dropped.
    pub fn start_reconciler(service: &Arc<Self>) {
        let service = Arc::downgrade(service);
        tokio::spawn(async move {
            let mut tick = tokio::time::interval(std::time::Duration::from_secs(30));
            tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
            loop {
                tick.tick().await;
                let Some(service) = service.upgrade() else {
                    break;
                };
                if let Err(error) = service.reconcile_due_batches().await {
                    warn!(error = %error, "Batch reconciliation failed; due jobs will retry");
                }
            }
        });
    }

    /// Schedule only work that can start now. Downloads never hold up the next tick.
    /// Two workers bound accounting buffers to two lines (64 MiB) plus HTTP chunks;
    /// at saturation, additional jobs remain durable due rows rather than queued tasks.
    async fn reconcile_due_batches(
        &self,
    ) -> Result<Vec<tokio::task::JoinHandle<()>>, AiGatewayError> {
        let capacity = self.reconciliation_slots.available_permits();
        if capacity == 0 {
            return Ok(Vec::new());
        }
        let now = chrono::Utc::now();
        let due = ai_gateway_objects::Entity::find()
            .filter(ai_gateway_objects::Column::NextPollAt.lte(now))
            .order_by_asc(ai_gateway_objects::Column::NextPollAt)
            .order_by_asc(ai_gateway_objects::Column::Id)
            .limit(capacity as u64)
            .all(self.db.as_ref())
            .await?;
        let mut jobs = Vec::with_capacity(due.len());
        for record in due {
            let Ok(permit) = self.reconciliation_slots.clone().try_acquire_owned() else {
                break;
            };
            let lease_until = chrono::Utc::now() + BATCH_RETRY_LEASE;
            let claimed = ai_gateway_objects::Entity::update_many()
                .col_expr(
                    ai_gateway_objects::Column::NextPollAt,
                    Expr::value(lease_until),
                )
                .filter(ai_gateway_objects::Column::Id.eq(record.id))
                .filter(ai_gateway_objects::Column::NextPollAt.lte(now))
                .exec(self.db.as_ref())
                .await?;
            if claimed.rows_affected != 1 {
                continue;
            }
            // Clone the dependencies, not the owning Arc: active jobs must not
            // keep the weak-reference polling loop alive after plugin shutdown.
            let service = Self::clone(self);
            jobs.push(tokio::spawn(async move {
                let _permit = permit;
                let result = tokio::time::timeout(
                    BATCH_RECONCILIATION_TIMEOUT,
                    service.reconcile_batch(&record),
                )
                .await;
                if !matches!(result, Ok(Ok(()))) {
                    warn!(object_id = record.id, error = ?result, "Batch reconciliation incomplete");
                }
                // Completed accounting clears the lease transactionally. For
                // errors, running batches or missing usage evidence, retry soon.
                // Compare the exact lease so this cannot overwrite a new claim
                // or a completion performed concurrently by a client poll.
                if let Err(error) = ai_gateway_objects::Entity::update_many()
                    .col_expr(
                        ai_gateway_objects::Column::NextPollAt,
                        Expr::value(chrono::Utc::now() + chrono::Duration::minutes(5)),
                    )
                    .filter(ai_gateway_objects::Column::Id.eq(record.id))
                    .filter(ai_gateway_objects::Column::NextPollAt.eq(lease_until))
                    .filter(ai_gateway_objects::Column::UsageRecordedAt.is_null())
                    .exec(service.db.as_ref())
                    .await
                {
                    warn!(object_id = record.id, error = %error, "Batch retry scheduling failed; durable lease remains");
                }
            }));
        }
        Ok(jobs)
    }

    async fn reconcile_batch(
        &self,
        record: &ai_gateway_objects::Model,
    ) -> Result<(), AiGatewayError> {
        let credentials = self.record_credentials(record).await?;
        let reply = self
            .client
            .send_buffered(
                Method::GET,
                &self.base_url(&credentials)?,
                &credentials.api_key,
                &format!("batches/{}", record.upstream_id),
                None,
            )
            .await?;
        if !reply.is_success() {
            return Err(AiGatewayError::Validation {
                message: "Provider could not retrieve tracked batch".into(),
            });
        }
        let batch = parse_reply(&reply, "batches")?;
        self.observe_batch(record, &batch).await
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
        let base_url = match credentials.base_url.as_deref() {
            Some(base_url) => base_url.to_string(),
            None => self
                .gateway_service
                .default_base_url(credentials.provider_id)
                .map(str::to_string)
                .ok_or_else(|| AiGatewayError::ProviderNotConfigured {
                    provider: credentials.provider_id.into(),
                })?,
        };
        let parsed =
            reqwest::Url::parse(&base_url).map_err(|_| AiGatewayError::InvalidProviderUrl {
                reason: "Native provider base URL is malformed".into(),
            })?;
        // Credentials belong in X-Provider-Api-Key. The base URL is persisted
        // as routing metadata and must never contain plaintext credentials.
        if !parsed.username().is_empty()
            || parsed.password().is_some()
            || parsed.query().is_some()
            || parsed.fragment().is_some()
        {
            return Err(AiGatewayError::InvalidProviderUrl { reason: "Native provider base URL must not include user info, query parameters, or a fragment; provide credentials in X-Provider-Api-Key".into() });
        }
        Ok(base_url)
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
        validate_model_id(&request.model)?;
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

        {
            let uploaded: FileObject = parse_reply(&reply, "files")?;
            validate_upstream_id(KIND_FILE, &uploaded.id)?;
            if let Err(record_error) = self
                .record(
                    owner,
                    KIND_FILE,
                    &uploaded.id,
                    &credentials,
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
                    request.endpoint.chars().take(128).collect::<String>(),
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

        let (input_credentials, input) = self
            .object_credentials(owner, byok, KIND_FILE, &request.input_file_id)
            .await?;
        let input = input.ok_or_else(|| AiGatewayError::Validation { message: "Upload batch input through the gateway so model and accounting metadata are available".into() })?;
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
        let credentials = if input.provider_key_id.is_some() {
            self.gateway_service
                .resolve_credentials(
                    &model,
                    &ByokOverride {
                        api_key: None,
                        base_url: None,
                        system_key_id: input.provider_key_id,
                    },
                )
                .await?
        } else {
            input_credentials
        };
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
                &credentials,
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
        if batch.id != record.upstream_id {
            return Err(AiGatewayError::Validation {
                message: "Provider returned a different batch id".into(),
            });
        }
        if record.usage_recorded_at.is_some() {
            return Ok(());
        }
        let owner = Owner {
            user_id: record.owner_user_id,
            project_id: record.owner_project_id,
        };
        for file_id in [&batch.output_file_id, &batch.error_file_id]
            .into_iter()
            .flatten()
        {
            if record.provider_key_id.is_some() && validate_upstream_id("file", file_id).is_ok() {
                self.record(
                    owner,
                    KIND_FILE,
                    file_id,
                    &self.record_credentials(record).await?,
                    None,
                    None,
                )
                .await?;
            }
        }

        if !batch.is_terminal() {
            return Ok(());
        }
        let Some(usage) = self.terminal_usage(record, batch).await? else {
            return Ok(());
        };
        if usage.input_tokens < 0 || usage.output_tokens < 0 {
            return Err(AiGatewayError::Validation {
                message: "Provider returned negative batch usage".into(),
            });
        }
        let transaction = self.db.begin().await?;
        let claimed = ai_gateway_objects::Entity::update_many()
            .col_expr(
                ai_gateway_objects::Column::UsageRecordedAt,
                Expr::value(chrono::Utc::now()),
            )
            .col_expr(
                ai_gateway_objects::Column::NextPollAt,
                Expr::value(Option::<chrono::DateTime<chrono::Utc>>::None),
            )
            .col_expr(
                ai_gateway_objects::Column::ByokKeyEncrypted,
                Expr::value(Option::<String>::None),
            )
            .filter(ai_gateway_objects::Column::Id.eq(record.id))
            .filter(ai_gateway_objects::Column::UsageRecordedAt.is_null())
            .exec(&transaction)
            .await?;
        if claimed.rows_affected != 1 {
            transaction.commit().await?;
            return Ok(()); // another poll recorded it first
        }
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
                record.provider_key_id.is_none(),
                &context,
            )
            .await?;
        transaction.commit().await?;
        Ok(())
    }

    /// Some providers omit aggregate usage. Sum the streamed JSONL output instead,
    /// including partial results from cancelled/expired batches, without buffering the file.
    async fn terminal_usage(
        &self,
        record: &ai_gateway_objects::Model,
        batch: &BatchObject,
    ) -> Result<Option<BatchUsage>, AiGatewayError> {
        if let Some(usage) = &batch.usage {
            return Ok(Some(usage.clone()));
        }
        let Some(file_id) = &batch.output_file_id else {
            if batch
                .request_counts
                .as_ref()
                .is_some_and(|counts| counts.completed == 0)
            {
                return Ok(Some(BatchUsage::default()));
            }
            // Missing evidence is not zero consumption. Keep the durable retry pending.
            return Ok(None);
        };
        validate_upstream_id(KIND_FILE, file_id)?;
        let credentials = self.record_credentials(record).await?;
        let response = self
            .client
            .send(
                Method::GET,
                &self.base_url(&credentials)?,
                &credentials.api_key,
                &format!("files/{file_id}/content"),
                None,
                true,
            )
            .await?;
        if !response.status().is_success() {
            return Err(AiGatewayError::Validation {
                message: "Batch output is not yet available for accounting".into(),
            });
        }
        let mut chunks = response.bytes_stream();
        let mut line = Vec::new();
        let mut usage = BatchUsage::default();
        let mut lines = 0;
        while let Some(chunk) = chunks.next().await {
            let chunk = chunk.map_err(|error| AiGatewayError::Internal {
                message: error.without_url().to_string(),
            })?;
            for part in chunk.split_inclusive(|byte| *byte == b'\n') {
                if line.len() + part.len() > MAX_BATCH_LINE_BYTES {
                    return Err(AiGatewayError::Validation {
                        message: "Batch output line exceeds accounting limit".into(),
                    });
                }
                line.extend_from_slice(part);
                if part.last() == Some(&b'\n') {
                    add_output_usage(&line, &mut usage)?;
                    line.clear();
                    lines += 1;
                    if lines > MAX_BATCH_REQUESTS {
                        return Err(AiGatewayError::Validation {
                            message: "Batch output exceeds request limit".into(),
                        });
                    }
                }
            }
        }
        if !line.is_empty() {
            add_output_usage(&line, &mut usage)?;
        }
        Ok(Some(usage))
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
            let record = self
                .find_owned_object(
                    owner,
                    kind,
                    upstream_id,
                    Some(&self.credential_scope(&credentials)?),
                )
                .await?;
            return Ok((credentials, record));
        }
        let record = self.owned_object(owner, kind, upstream_id).await?;
        // BYOK objects always require the caller to provide their own key.
        if record.provider_key_id.is_none() {
            return Err(AiGatewayError::ObjectNotFound {
                kind: kind.into(),
                id: upstream_id.into(),
            });
        }
        let credentials = self.record_credentials(&record).await?;
        Ok((credentials, Some(record)))
    }

    fn credential_scope(
        &self,
        credentials: &ResolvedCredentials,
    ) -> Result<String, AiGatewayError> {
        if let Some(id) = credentials.system_key_id {
            return Ok(format!("system:{id}"));
        }
        let mut hash = Sha256::new();
        hash.update(credentials.api_key.as_bytes());
        hash.update([0]);
        hash.update(self.base_url(credentials)?.trim_end_matches('/').as_bytes());
        Ok(format!("byok:{}", hex::encode(hash.finalize())))
    }

    async fn record_credentials(
        &self,
        record: &ai_gateway_objects::Model,
    ) -> Result<ResolvedCredentials, AiGatewayError> {
        if let Some(id) = record.provider_key_id {
            return self.gateway_service.system_key_credentials(id).await;
        }
        let encrypted =
            record
                .byok_key_encrypted
                .as_deref()
                .ok_or_else(|| AiGatewayError::Validation {
                    message: "BYOK reconciliation credential has already been released".into(),
                })?;
        Ok(ResolvedCredentials {
            provider_id: OPENAI,
            api_key: self.gateway_service.decrypt_native_key(encrypted)?,
            base_url: record.byok_base_url.clone(),
            credential_type: CredentialType::Byok,
            system_key_id: None,
        })
    }

    /// The caller's record of an object. Objects owned by someone else are
    /// reported as not found, so ids cannot be probed.
    async fn owned_object(
        &self,
        owner: Owner,
        kind: &str,
        upstream_id: &str,
    ) -> Result<ai_gateway_objects::Model, AiGatewayError> {
        self.find_owned_object(owner, kind, upstream_id, None)
            .await?
            .ok_or_else(|| AiGatewayError::ObjectNotFound {
                kind: kind.into(),
                id: upstream_id.into(),
            })
    }

    async fn find_owned_object(
        &self,
        owner: Owner,
        kind: &str,
        upstream_id: &str,
        scope: Option<&str>,
    ) -> Result<Option<ai_gateway_objects::Model>, AiGatewayError> {
        let mut query = ai_gateway_objects::Entity::find()
            .filter(ai_gateway_objects::Column::Kind.eq(kind))
            .filter(ai_gateway_objects::Column::UpstreamId.eq(upstream_id));
        query = if let Some(scope) = scope {
            query.filter(ai_gateway_objects::Column::CredentialScope.eq(scope))
        } else {
            query.filter(ai_gateway_objects::Column::ProviderKeyId.is_not_null())
        };
        query = match (owner.user_id, owner.project_id) {
            (Some(id), _) => query.filter(ai_gateway_objects::Column::OwnerUserId.eq(id)),
            (None, Some(id)) => query.filter(ai_gateway_objects::Column::OwnerProjectId.eq(id)),
            _ => {
                return Err(AiGatewayError::ObjectNotFound {
                    kind: kind.into(),
                    id: upstream_id.into(),
                })
            }
        };
        Ok(query.one(self.db.as_ref()).await?)
    }

    async fn record(
        &self,
        owner: Owner,
        kind: &str,
        upstream_id: &str,
        credentials: &ResolvedCredentials,
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
            provider_key_id: Set(credentials.system_key_id),
            byok_key_encrypted: Set(
                if credentials.system_key_id.is_none() && kind == KIND_BATCH {
                    Some(
                        self.gateway_service
                            .encrypt_native_key(&credentials.api_key)?,
                    )
                } else {
                    None
                },
            ),
            byok_base_url: Set(credentials.base_url.clone()),
            credential_scope: Set(self.credential_scope(credentials)?),
            next_poll_at: Set(if kind == KIND_BATCH {
                Some(chrono::Utc::now())
            } else {
                None
            }),
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
                    ai_gateway_objects::Column::CredentialScope,
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

/// Require exact token evidence for successful results; never silently count malformed usage as zero.
fn add_output_usage(line: &[u8], total: &mut BatchUsage) -> Result<(), AiGatewayError> {
    if line.iter().all(u8::is_ascii_whitespace) {
        return Ok(());
    }
    let value: serde_json::Value =
        serde_json::from_slice(line).map_err(|_| AiGatewayError::Validation {
            message: "Invalid JSONL batch result".into(),
        })?;
    let response = &value["response"];
    if response.is_null() && !value["error"].is_null() {
        return Ok(());
    }
    if response["status_code"].as_u64().is_none() {
        return Err(AiGatewayError::Validation {
            message: "Batch result is missing response status".into(),
        });
    }
    if !response["status_code"]
        .as_u64()
        .is_some_and(|status| (200..300).contains(&status))
    {
        return Ok(());
    }
    let usage = &response["body"]["usage"];
    let input = usage["input_tokens"]
        .as_i64()
        .or_else(|| usage["prompt_tokens"].as_i64());
    let output = usage["output_tokens"]
        .as_i64()
        .or_else(|| usage["completion_tokens"].as_i64())
        .or_else(|| {
            if response["body"]["object"] == "list" {
                Some(0)
            } else {
                None
            }
        });
    let (Some(input), Some(output)) = (input, output) else {
        return Err(AiGatewayError::Validation {
            message: "Successful batch result is missing token usage".into(),
        });
    };
    if input < 0 || output < 0 {
        return Err(AiGatewayError::Validation {
            message: "Negative token usage in batch output".into(),
        });
    }
    total.input_tokens =
        total
            .input_tokens
            .checked_add(input)
            .ok_or_else(|| AiGatewayError::Validation {
                message: "Batch input token count overflow".into(),
            })?;
    total.output_tokens =
        total
            .output_tokens
            .checked_add(output)
            .ok_or_else(|| AiGatewayError::Validation {
                message: "Batch output token count overflow".into(),
            })?;
    Ok(())
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

    #[test]
    fn identifiers_cannot_amplify_validation_errors() {
        assert!(validate_model_id(&"gpt-".repeat(1000)).is_err());
        let request = serde_json::json!({"custom_id":"one","method":"x".repeat(100_000),"url":"/v1/responses","body":{"model":"gpt-4o"}});
        let error = validate(&serde_json::to_string(&request).unwrap()).unwrap_err();
        assert!(error.to_string().len() < 512);
        let request = serde_json::json!({"custom_id":"one","method":"POST","url":"x".repeat(100_000),"body":{"model":"gpt-4o"}});
        let error = validate(&serde_json::to_string(&request).unwrap()).unwrap_err();
        assert!(error.to_string().len() < 512);
    }

    #[test]
    fn native_urls_cannot_persist_or_log_embedded_credentials() {
        let svc = service(Arc::new(
            sea_orm::MockDatabase::new(sea_orm::DatabaseBackend::Postgres).into_connection(),
        ));
        for url in [
            "https://user:example-secret@provider.example/v1",
            "https://provider.example/v1?key=example-secret",
            "https://provider.example/v1#example-secret",
        ] {
            let credentials = ResolvedCredentials {
                provider_id: OPENAI,
                api_key: "example-key".into(),
                base_url: Some(url.into()),
                credential_type: CredentialType::Byok,
                system_key_id: None,
            };
            let error = svc.base_url(&credentials).unwrap_err();
            assert!(matches!(error, AiGatewayError::InvalidProviderUrl { .. }));
            assert!(!error.to_string().contains("example-secret"));
        }
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
            provider_key_id: Some(2),
            byok_key_encrypted: None,
            byok_base_url: None,
            credential_scope: "system:2".into(),
            next_poll_at: Some(chrono::Utc::now()),
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
    async fn byok_batch_is_reconciled_without_a_client_poll() {
        assert_byok_batch_reconciled(None).await;
    }

    #[tokio::test]
    async fn batch_result_transfer_longer_than_a_minute_records_usage() {
        assert_byok_batch_reconciled(Some(std::time::Duration::from_secs(65))).await;
    }

    async fn assert_byok_batch_reconciled(output_delay: Option<std::time::Duration>) {
        use sea_orm::{DatabaseBackend, MockDatabase, MockExecResult};
        use wiremock::{
            matchers::{header, method, path},
            Mock, MockServer, ResponseTemplate,
        };
        let server = MockServer::start().await;
        let batch = if let Some(delay) = output_delay {
            Mock::given(method("GET"))
                .and(path("/v1/files/file-result/content"))
                .and(header("authorization", "Bearer test-reconciliation-key"))
                .respond_with(ResponseTemplate::new(200).set_delay(delay).set_body_string(
                    "{\"response\":{\"status_code\":200,\"body\":{\"usage\":{\"input_tokens\":10,\"output_tokens\":5}}}}\n",
                ))
                .expect(1)
                .mount(&server)
                .await;
            serde_json::json!({
                "id":"batch_test", "object":"batch", "status":"completed",
                "output_file_id":"file-result"
            })
        } else {
            serde_json::json!({
                "id":"batch_test", "object":"batch", "status":"cancelled",
                "usage":{"input_tokens":10,"output_tokens":5}
            })
        };
        Mock::given(method("GET"))
            .and(path("/v1/batches/batch_test"))
            .and(header("authorization", "Bearer test-reconciliation-key"))
            .respond_with(ResponseTemplate::new(200).set_body_json(batch))
            .expect(1)
            .mount(&server)
            .await;
        let mut record = batch_record();
        record.provider_key_id = None;
        record.credential_scope = "byok:test".into();
        record.byok_base_url = Some(format!("{}/v1", server.uri()));
        record.byok_key_encrypted = Some(
            temps_core::EncryptionService::new("01234567890123456789012345678901")
                .unwrap()
                .encrypt_string("test-reconciliation-key")
                .unwrap(),
        );
        let log = temps_entities::ai_usage_logs::Model {
            id: 1,
            timestamp: chrono::Utc::now(),
            user_id: Some(4),
            provider: OPENAI.into(),
            model: "gpt-4o".into(),
            input_tokens: 10,
            output_tokens: 5,
            latency_ms: 0,
            estimated_cost_microcents: 0,
            status: 200,
            is_streaming: false,
            is_byok: true,
            conversation_id: None,
            tags: vec!["batch".into()],
            request_id: Some("batch_test".into()),
            trace_id: None,
        };
        let db = Arc::new(
            MockDatabase::new(DatabaseBackend::Postgres)
                .append_query_results([vec![record]])
                .append_query_results([vec![log]])
                .append_exec_results([
                    MockExecResult {
                        last_insert_id: 0,
                        rows_affected: 1,
                    },
                    MockExecResult {
                        last_insert_id: 0,
                        rows_affected: 1,
                    },
                    MockExecResult {
                        last_insert_id: 0,
                        rows_affected: 0,
                    },
                ])
                .into_connection(),
        );
        let mut svc = service(db.clone());
        svc.client = OpenAiNativeClient::for_test();
        let started = chrono::Utc::now();
        for job in svc.reconcile_due_batches().await.unwrap() {
            job.await.unwrap();
        }
        drop(svc);
        let transactions = Arc::try_unwrap(db).unwrap().into_transaction_log();
        let claim = transactions
            .iter()
            .flat_map(|transaction| transaction.statements())
            .find(|statement| {
                statement.sql.starts_with("UPDATE") && statement.sql.contains("next_poll_at")
            })
            .expect("batch retry lease was persisted");
        let sea_orm::sea_query::Value::ChronoDateTimeUtc(Some(retry_at)) =
            &claim.values.as_ref().unwrap().0[0]
        else {
            panic!("batch lease must be a timestamp: {claim:?}");
        };
        assert!(
            **retry_at
                > started
                    + chrono::Duration::seconds(BATCH_RECONCILIATION_TIMEOUT.as_secs() as i64),
            "another worker must not reclaim the batch before accounting finishes"
        );
        let sql = format!("{transactions:?}");
        assert!(
            sql.contains("ai_usage_logs") && sql.contains("COMMIT"),
            "{sql}"
        );
        assert!(
            sql.contains("byok_key_encrypted") && sql.contains("next_poll_at"),
            "{sql}"
        );
        assert!(
            sql.contains("Bool(Some(true))"),
            "BYOK flag was not logged: {sql}"
        );
    }

    fn reconciliation_record(server: &wiremock::MockServer) -> ai_gateway_objects::Model {
        let mut record = batch_record();
        record.provider_key_id = None;
        record.credential_scope = "byok:test".into();
        record.byok_base_url = Some(format!("{}/v1", server.uri()));
        record.byok_key_encrypted = Some(
            temps_core::EncryptionService::new("01234567890123456789012345678901")
                .unwrap()
                .encrypt_string("test-reconciliation-key")
                .unwrap(),
        );
        record
    }

    #[tokio::test]
    async fn unfinished_batch_jobs_retry_in_five_minutes() {
        use sea_orm::{DatabaseBackend, MockDatabase, MockExecResult};
        use wiremock::{
            matchers::{method, path},
            Mock, MockServer, ResponseTemplate,
        };
        // A provider error, an in-progress batch and missing terminal evidence
        // all need a short retry after the active job has released its lease.
        for (status, batch_status) in [(503, "failed"), (200, "in_progress"), (200, "completed")] {
            let server = MockServer::start().await;
            Mock::given(method("GET"))
                .and(path("/v1/batches/batch_test"))
                .respond_with(
                    ResponseTemplate::new(status).set_body_json(serde_json::json!({
                        "id":"batch_test", "object":"batch", "status":batch_status
                    })),
                )
                .expect(1)
                .mount(&server)
                .await;
            let db = Arc::new(
                MockDatabase::new(DatabaseBackend::Postgres)
                    .append_query_results([vec![reconciliation_record(&server)]])
                    .append_exec_results((0..2).map(|_| MockExecResult {
                        last_insert_id: 0,
                        rows_affected: 1,
                    }))
                    .into_connection(),
            );
            let mut svc = service(db.clone());
            svc.client = OpenAiNativeClient::for_test();
            let started = chrono::Utc::now();
            for job in svc.reconcile_due_batches().await.unwrap() {
                job.await.unwrap();
            }
            drop(svc);
            let transactions = Arc::try_unwrap(db).unwrap().into_transaction_log();
            let updates: Vec<_> = transactions
                .iter()
                .flat_map(|t| t.statements())
                .filter(|s| s.sql.starts_with("UPDATE"))
                .collect();
            assert_eq!(updates.len(), 2);
            let retry = updates[1];
            let sea_orm::sea_query::Value::ChronoDateTimeUtc(Some(retry_at)) =
                &retry.values.as_ref().unwrap().0[0]
            else {
                panic!("{retry:?}")
            };
            assert!(**retry_at >= started + chrono::Duration::minutes(5));
            assert!(**retry_at < started + chrono::Duration::minutes(6));
            assert!(
                retry.sql.contains("usage_recorded_at\" IS NULL"),
                "{retry:?}"
            );
            // The old lease is compared, not just the id: stale jobs cannot
            // reschedule another claim or resurrect a client-poll completion.
            assert!(retry.sql.matches("next_poll_at").count() >= 2, "{retry:?}");
        }
    }

    #[tokio::test]
    async fn slow_batch_does_not_block_another_job_or_the_next_tick() {
        use sea_orm::{DatabaseBackend, MockDatabase, MockExecResult};
        use wiremock::{
            matchers::{method, path},
            Mock, MockServer, ResponseTemplate,
        };
        let server = MockServer::start().await;
        for (id, delay) in [("batch_test", 3), ("batch_fast", 0)] {
            Mock::given(method("GET"))
                .and(path(format!("/v1/batches/{id}")))
                .respond_with(
                    ResponseTemplate::new(503).set_delay(std::time::Duration::from_secs(delay)),
                )
                .expect(1)
                .mount(&server)
                .await;
        }
        let slow = reconciliation_record(&server);
        let mut fast = slow.clone();
        fast.id = 2;
        fast.upstream_id = "batch_fast".into();
        let db = Arc::new(
            MockDatabase::new(DatabaseBackend::Postgres)
                .append_query_results([vec![slow, fast], Vec::<ai_gateway_objects::Model>::new()])
                .append_exec_results((0..4).map(|_| MockExecResult {
                    last_insert_id: 0,
                    rows_affected: 1,
                }))
                .into_connection(),
        );
        let mut svc = service(db.clone());
        svc.client = OpenAiNativeClient::for_test();
        let deadline = std::time::Duration::from_secs(2);
        let mut jobs = tokio::time::timeout(deadline, svc.reconcile_due_batches())
            .await
            .expect("scheduling must not await provider requests")
            .unwrap();
        assert_eq!(jobs.len(), 2);
        tokio::time::timeout(deadline, jobs.remove(1))
            .await
            .expect("the second batch must finish while the first is slow")
            .unwrap();
        assert!(!jobs[0].is_finished());
        assert!(tokio::time::timeout(deadline, svc.reconcile_due_batches())
            .await
            .expect("the next tick must not wait for an active transfer")
            .unwrap()
            .is_empty());
        jobs.remove(0).await.unwrap();
        assert_eq!(svc.reconciliation_slots.available_permits(), 2);
    }

    #[tokio::test]
    async fn saturated_batch_workers_leave_due_work_in_the_database() {
        use sea_orm::{DatabaseBackend, MockDatabase};
        let db = Arc::new(MockDatabase::new(DatabaseBackend::Postgres).into_connection());
        let svc = service(db.clone());
        let _permits = svc
            .reconciliation_slots
            .clone()
            .try_acquire_many_owned(2)
            .unwrap();
        assert!(svc.reconcile_due_batches().await.unwrap().is_empty());
        drop(svc);
        assert!(Arc::try_unwrap(db)
            .unwrap()
            .into_transaction_log()
            .is_empty());
    }

    #[tokio::test]
    async fn byok_lookup_binds_owner_and_credential_account() {
        use sea_orm::{DatabaseBackend, MockDatabase};
        let db = Arc::new(
            MockDatabase::new(DatabaseBackend::Postgres)
                .append_query_results([Vec::<ai_gateway_objects::Model>::new()])
                .into_connection(),
        );
        let svc = service(db.clone());
        let key = ByokOverride {
            api_key: Some("test-key".into()),
            ..Default::default()
        };
        let (_, record) = svc
            .object_credentials(
                Owner {
                    user_id: Some(4),
                    project_id: None,
                },
                &key,
                KIND_BATCH,
                "batch_test",
            )
            .await
            .unwrap();
        assert!(record.is_none());
        let first = svc
            .credential_scope(&ResolvedCredentials {
                provider_id: OPENAI,
                api_key: "first".into(),
                base_url: None,
                credential_type: CredentialType::Byok,
                system_key_id: None,
            })
            .unwrap();
        let second = svc
            .credential_scope(&ResolvedCredentials {
                provider_id: OPENAI,
                api_key: "second".into(),
                base_url: None,
                credential_type: CredentialType::Byok,
                system_key_id: None,
            })
            .unwrap();
        assert_ne!(first, second);
        drop(svc);
        let sql = format!("{:?}", Arc::try_unwrap(db).unwrap().into_transaction_log());
        assert!(
            sql.contains("owner_user_id") && sql.contains("credential_scope"),
            "{sql}"
        );
        assert!(!sql.contains("test-key"), "credential leaked to SQL");
    }

    #[tokio::test]
    async fn cancelled_batch_without_aggregate_usage_reads_partial_results() {
        use wiremock::{matchers::path, Mock, MockServer, ResponseTemplate};
        let server = MockServer::start().await;
        let output = concat!(
            "{\"response\":{\"status_code\":200,\"body\":{\"usage\":{\"prompt_tokens\":7,\"completion_tokens\":3}}}}\n",
            "{\"response\":{\"status_code\":200,\"body\":{\"usage\":{\"input_tokens\":2,\"output_tokens\":1}}}}"
        );
        Mock::given(path("/v1/files/file-output/content"))
            .respond_with(ResponseTemplate::new(200).set_body_string(output))
            .expect(1)
            .mount(&server)
            .await;
        let mut svc = service(Arc::new(
            sea_orm::MockDatabase::new(sea_orm::DatabaseBackend::Postgres).into_connection(),
        ));
        svc.client = OpenAiNativeClient::for_test();
        let mut record = batch_record();
        record.provider_key_id = None;
        record.byok_key_encrypted =
            Some(svc.gateway_service.encrypt_native_key("test-key").unwrap());
        record.byok_base_url = Some(format!("{}/v1", server.uri()));
        let mut batch = finished_batch();
        batch.status = "cancelled".into();
        batch.usage = None;
        batch.output_file_id = Some("file-output".into());
        let usage = svc.terminal_usage(&record, &batch).await.unwrap().unwrap();
        assert_eq!((usage.input_tokens, usage.output_tokens), (9, 4));
    }

    #[test]
    fn malformed_successful_result_is_not_silently_counted_as_zero() {
        let mut usage = BatchUsage::default();
        assert!(
            add_output_usage(br#"{"response":{"status_code":200,"body":{}}}"#, &mut usage).is_err()
        );
        assert!(add_output_usage(br#"{"response":{"status_code":200,"body":{"usage":{"input_tokens":-1,"output_tokens":0}}}}"#, &mut usage).is_err());
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
