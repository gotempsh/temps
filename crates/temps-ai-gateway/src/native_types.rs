// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Wire types for the OpenAI-native endpoints (`/ai/v1/responses`,
//! `/ai/v1/files`, `/ai/v1/batches`).
//!
//! The gateway forwards these requests and replies without translating them,
//! so each type names only the fields the gateway reads or documents; every
//! other field is carried through `extra` untouched. That keeps new provider
//! options (reasoning settings, tools, output formats) working without a
//! gateway release.

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use std::collections::HashMap;
use utoipa::ToSchema;

// ============================================================================
// Responses
// ============================================================================

/// Request body of `POST /ai/v1/responses`, OpenAI's Responses API.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct ResponsesRequest {
    /// Model to run, e.g. `gpt-6-luna`. Must be served by OpenAI.
    pub model: String,
    /// Stream the reply as server-sent events.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub stream: bool,
    /// Continue from a stored response. Requires your own provider key
    /// (`X-Provider-Api-Key`), because stored responses live in the
    /// provider account that created them.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub previous_response_id: Option<String>,
    /// Attach the response to a stored conversation. Requires your own
    /// provider key, for the same reason as `previous_response_id`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schema(value_type = Option<Object>)]
    pub conversation: Option<Value>,
    /// Run asynchronously and poll for the result. Requires your own
    /// provider key: the result is fetched from the provider account.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub background: Option<bool>,
    /// Every other Responses API field (`input`, `instructions`, `tools`,
    /// `text`, `reasoning`, …), forwarded unchanged.
    #[serde(flatten)]
    #[schema(value_type = Object)]
    pub extra: Map<String, Value>,
}

/// Token usage of a response.
#[derive(Debug, Clone, Default, Serialize, Deserialize, ToSchema)]
pub struct ResponseUsage {
    #[serde(default)]
    pub input_tokens: i64,
    #[serde(default)]
    pub output_tokens: i64,
    #[serde(default)]
    pub total_tokens: i64,
}

/// A response object as returned by the provider.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct ResponseObject {
    pub id: String,
    /// Always `response`
    pub object: String,
    pub model: String,
    /// `completed`, `incomplete`, `failed`, `in_progress`, …
    #[serde(default)]
    pub status: Option<String>,
    #[serde(default)]
    pub usage: Option<ResponseUsage>,
    /// `output`, `error`, `incomplete_details` and every other field.
    #[serde(flatten)]
    #[schema(value_type = Object)]
    pub extra: Map<String, Value>,
}

// ============================================================================
// Files
// ============================================================================

/// `multipart/form-data` body of `POST /ai/v1/files`.
#[derive(Debug, ToSchema)]
#[allow(dead_code)] // documents the multipart form; parsed field by field
pub struct UploadFileForm {
    /// A JSONL batch input file: one request per line, every line targeting
    /// the same model and endpoint. At most 200 MB and 50,000 requests.
    #[schema(value_type = String, format = Binary)]
    pub file: Vec<u8>,
    /// Must be `batch`.
    pub purpose: String,
}

/// A file object as returned by the provider.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct FileObject {
    pub id: String,
    /// Always `file`
    pub object: String,
    #[serde(default)]
    pub bytes: i64,
    #[serde(default)]
    pub created_at: i64,
    #[serde(default)]
    pub filename: String,
    /// `batch` for uploads, `batch_output` for batch results
    #[serde(default)]
    pub purpose: String,
    #[serde(flatten)]
    #[schema(value_type = Object)]
    pub extra: Map<String, Value>,
}

/// Reply of `DELETE /ai/v1/files/{file_id}`.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct FileDeletedResponse {
    pub id: String,
    /// Always `file`
    pub object: String,
    pub deleted: bool,
}

// ============================================================================
// Batches
// ============================================================================

/// Request body of `POST /ai/v1/batches`.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct CreateBatchRequest {
    /// A file uploaded through `POST /ai/v1/files` by the same caller.
    pub input_file_id: String,
    /// Endpoint every request in the input file targets: `/v1/responses`,
    /// `/v1/chat/completions` or `/v1/embeddings`.
    pub endpoint: String,
    /// Currently only `24h`.
    pub completion_window: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub metadata: Option<HashMap<String, String>>,
    /// `output_expires_after` and any other field, forwarded unchanged.
    #[serde(flatten)]
    #[schema(value_type = Object)]
    pub extra: Map<String, Value>,
}

/// Progress counters of a batch.
#[derive(Debug, Clone, Default, Serialize, Deserialize, ToSchema)]
pub struct BatchRequestCounts {
    #[serde(default)]
    pub total: i64,
    #[serde(default)]
    pub completed: i64,
    #[serde(default)]
    pub failed: i64,
}

/// Token usage of a finished batch, summed over its requests.
#[derive(Debug, Clone, Default, Serialize, Deserialize, ToSchema)]
pub struct BatchUsage {
    #[serde(default)]
    pub input_tokens: i64,
    #[serde(default)]
    pub output_tokens: i64,
    #[serde(default)]
    pub total_tokens: i64,
}

/// A batch object as returned by the provider.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct BatchObject {
    pub id: String,
    /// Always `batch`
    pub object: String,
    #[serde(default)]
    pub endpoint: String,
    #[serde(default)]
    pub model: Option<String>,
    #[serde(default)]
    pub input_file_id: String,
    #[serde(default)]
    pub completion_window: String,
    /// `validating`, `in_progress`, `finalizing`, `completed`, `failed`,
    /// `expired`, `cancelling` or `cancelled`
    pub status: String,
    /// Results of the successful requests; download with
    /// `GET /ai/v1/files/{file_id}/content`.
    #[serde(default)]
    pub output_file_id: Option<String>,
    /// Results of the failed requests.
    #[serde(default)]
    pub error_file_id: Option<String>,
    #[serde(default)]
    pub created_at: i64,
    #[serde(default)]
    pub request_counts: Option<BatchRequestCounts>,
    #[serde(default)]
    pub usage: Option<BatchUsage>,
    #[serde(flatten)]
    #[schema(value_type = Object)]
    pub extra: Map<String, Value>,
}

impl BatchObject {
    /// Whether the batch can no longer change: its usage is final and any
    /// result files exist.
    pub fn is_terminal(&self) -> bool {
        matches!(
            self.status.as_str(),
            "completed" | "failed" | "expired" | "cancelled"
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn responses_request_forwards_unknown_fields_unchanged() {
        let json = r#"{
            "model": "gpt-6-luna",
            "input": [{"role": "user", "content": "hi"}],
            "text": {"format": {"type": "json_schema", "name": "verdict", "schema": {"type": "object"}}},
            "reasoning": {"effort": "low"}
        }"#;
        let request: ResponsesRequest = serde_json::from_str(json).expect("parse");
        assert_eq!(request.model, "gpt-6-luna");
        assert!(!request.stream);
        let forwarded: Value = serde_json::to_value(&request).expect("serialize");
        let original: Value = serde_json::from_str(json).expect("parse original");
        assert_eq!(forwarded, original);
    }

    #[test]
    fn responses_request_keeps_stream_flag() {
        let request: ResponsesRequest =
            serde_json::from_str(r#"{"model":"gpt-6-luna","input":"hi","stream":true}"#)
                .expect("parse");
        assert!(request.stream);
        let forwarded = serde_json::to_value(&request).expect("serialize");
        assert_eq!(forwarded["stream"], Value::Bool(true));
    }

    #[test]
    fn batch_object_parses_usage_and_result_files() {
        let json = r#"{
            "id": "batch_abc123", "object": "batch", "endpoint": "/v1/responses",
            "model": "gpt-6-luna", "input_file_id": "file-in", "completion_window": "24h",
            "status": "completed", "output_file_id": "file-out", "error_file_id": null,
            "created_at": 1759600000,
            "request_counts": {"total": 3, "completed": 3, "failed": 0},
            "usage": {"input_tokens": 1200, "input_tokens_details": {"cached_tokens": 800},
                      "output_tokens": 90, "output_tokens_details": {"reasoning_tokens": 0},
                      "total_tokens": 1290},
            "metadata": {"run": "nightly"}
        }"#;
        let batch: BatchObject = serde_json::from_str(json).expect("parse");
        assert!(batch.is_terminal());
        assert_eq!(batch.output_file_id.as_deref(), Some("file-out"));
        assert_eq!(batch.error_file_id, None);
        let usage = batch.usage.expect("usage");
        assert_eq!((usage.input_tokens, usage.output_tokens), (1200, 90));
        assert!(batch.extra.contains_key("metadata"));
    }

    #[test]
    fn batch_in_progress_is_not_terminal() {
        for status in ["validating", "in_progress", "finalizing", "cancelling"] {
            let batch: BatchObject = serde_json::from_value(serde_json::json!({
                "id": "batch_1", "object": "batch", "status": status
            }))
            .expect("parse");
            assert!(!batch.is_terminal(), "{status} must not be terminal");
        }
    }
}
