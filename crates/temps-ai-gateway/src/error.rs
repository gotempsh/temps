// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

use thiserror::Error;

#[derive(Error, Debug)]
pub enum AiGatewayError {
    #[error("Provider '{provider}' not found or not configured")]
    ProviderNotConfigured { provider: String },

    #[error("Provider key {key_id} not found")]
    ProviderKeyNotFound { key_id: i32 },

    #[error("Model '{model}' not found: no provider configured that serves this model")]
    ModelNotFound { model: String },

    #[error("Model '{model}' is not allowed in scope '{scope}'")]
    ModelNotAllowed { model: String, scope: String },

    #[error("Upstream provider error for model '{model}': {status} {message}")]
    UpstreamError {
        model: String,
        status: u16,
        message: String,
    },

    #[error("Request translation failed for provider '{provider}': {reason}")]
    TranslationError { provider: String, reason: String },

    #[error("Streaming error for model '{model}': {reason}")]
    StreamError { model: String, reason: String },

    #[error("Validation error: {message}")]
    Validation { message: String },

    #[error("Encryption error: {0}")]
    Encryption(String),

    #[error("HTTP client error: {0}")]
    HttpClient(String),

    #[error("Internal error: {message}")]
    Internal { message: String },

    #[error("Database error: {0}")]
    Database(#[from] sea_orm::DbErr),

    #[error("Invalid provider base URL: {reason}")]
    InvalidProviderUrl { reason: String },

    #[error(
        "{endpoint} is only available for OpenAI models; model '{model}' is served by provider \
         '{provider}'. Use /ai/v1/chat/completions for this model instead."
    )]
    UnsupportedEndpoint {
        endpoint: String,
        model: String,
        provider: String,
    },

    #[error("AI batch input file exceeds the {limit_bytes}-byte upload limit")]
    UploadTooLarge { limit_bytes: u64 },

    #[error("AI batch upload capacity reached; retry when an upload finishes")]
    UploadCapacity,

    #[error("No {kind} with id '{id}' was created through this gateway by the caller")]
    ObjectNotFound { kind: String, id: String },
}

impl From<reqwest::Error> for AiGatewayError {
    fn from(error: reqwest::Error) -> Self {
        // Strip the request URL before converting to string — the URL may contain
        // embedded credentials (BYOK base_url) that must not appear in logs.
        let scrubbed = error.without_url();
        AiGatewayError::HttpClient(scrubbed.to_string())
    }
}
