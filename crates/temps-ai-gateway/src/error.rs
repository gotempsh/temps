// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

use thiserror::Error;

/// Render a microcent amount (1 USD = 1,000,000) as dollars for error text.
/// Sub-cent amounts keep four decimals so a small budget never reads `$0.00`.
fn format_usd(microcents: i64) -> String {
    let usd = microcents as f64 / 1_000_000.0;
    if usd != 0.0 && usd.abs() < 0.01 {
        format!("${usd:.4}")
    } else {
        format!("${usd:.2}")
    }
}

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
         '{provider}'. Use /api/ai/v1/chat/completions for this model instead."
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

    #[error(
        "Project {project_id} exceeded its AI gateway limit of {limit} requests per minute; \
         retry in {retry_after_secs}s"
    )]
    RateLimited {
        project_id: i32,
        limit: u32,
        retry_after_secs: u64,
    },

    #[error(
        "Project {project_id} reached its monthly AI gateway budget of {} \
         ({} used); it resets in {retry_after_secs}s",
        format_usd(*limit_microcents),
        format_usd(*used_microcents)
    )]
    BudgetExceeded {
        project_id: i32,
        limit_microcents: i64,
        used_microcents: i64,
        retry_after_secs: u64,
    },

    #[error("Project {project_id} not found; cannot configure AI gateway limits for it")]
    ProjectNotFound { project_id: i32 },

    #[error("No {kind} with id '{id}' was created through this gateway by the caller")]
    ObjectNotFound { kind: String, id: String },

    #[error(
        "Model '{model}' is served by more than one configured provider ({candidates}); refusing \
         to pick one. Send the X-Temps-Provider-Key-Id header with one of these key ids. \
         Disabling the model on the other keys also works, except on a self-hosted key: a \
         self-hosted key that lists a model keeps hosting it while it is disabled there, so \
         requests for it must then pin a key."
    )]
    AmbiguousModel { model: String, candidates: String },

    #[error(
        "Model '{model}' is hosted by self-hosted provider key {key_id}, which cannot serve it \
         right now ({reason}); it is never forwarded to another provider automatically. Make it \
         available on that key again, or send the X-Temps-Provider-Key-Id header to use another \
         key explicitly."
    )]
    SelfHostedModelUnavailable {
        model: String,
        key_id: i32,
        reason: String,
    },

    #[error(
        "Provider key {key_id} does not declare support for {capability}, so model '{model}' \
         cannot be used for it. An administrator can enable it on the provider key if the \
         self-hosted server implements it."
    )]
    CapabilityNotDeclared {
        key_id: i32,
        model: String,
        capability: String,
    },
}

impl AiGatewayError {
    /// A short, human-friendly description. Upstream errors often carry a
    /// raw JSON body; this pulls out just its `error.message` when present.
    pub fn summary(&self) -> String {
        if let AiGatewayError::UpstreamError {
            status, message, ..
        } = self
        {
            if let Ok(parsed) = serde_json::from_str::<serde_json::Value>(message) {
                if let Some(msg) = parsed
                    .get("error")
                    .and_then(|e| e.get("message"))
                    .and_then(|m| m.as_str())
                {
                    return format!("{} — {}", status, msg);
                }
            }
        }
        self.to_string()
    }
}

impl From<reqwest::Error> for AiGatewayError {
    fn from(error: reqwest::Error) -> Self {
        // A connect-time DNS block is a policy decision, not a transport
        // failure: surface it as the same InvalidProviderUrl (with the
        // trusted-networks hint) that save-time validation reports, instead
        // of reqwest's opaque "error sending request" (#1410).
        if let Some(blocked) = find_blocked_address(&error) {
            return AiGatewayError::InvalidProviderUrl {
                reason: blocked.to_string(),
            };
        }
        // Strip the request URL before converting to string — the URL may contain
        // embedded credentials (BYOK base_url) that must not appear in logs.
        let scrubbed = error.without_url();
        AiGatewayError::HttpClient(scrubbed.to_string())
    }
}

fn find_blocked_address(
    error: &reqwest::Error,
) -> Option<&crate::providers::BlockedProviderAddress> {
    let mut source: Option<&(dyn std::error::Error + 'static)> = std::error::Error::source(error);
    while let Some(current) = source {
        if let Some(blocked) = current.downcast_ref::<crate::providers::BlockedProviderAddress>() {
            return Some(blocked);
        }
        source = current.source();
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn budget_exceeded_message_is_in_dollars() {
        let err = AiGatewayError::BudgetExceeded {
            project_id: 7,
            limit_microcents: 25_000_000,
            used_microcents: 26_500_000,
            retry_after_secs: 3600,
        };
        let msg = err.to_string();
        assert!(msg.contains("Project 7"), "{msg}");
        assert!(msg.contains("$25.00"), "{msg}");
        assert!(msg.contains("$26.50 used"), "{msg}");
        assert!(msg.contains("3600s"), "{msg}");
    }

    #[test]
    fn format_usd_keeps_precision_for_sub_cent_amounts() {
        assert_eq!(format_usd(0), "$0.00");
        assert_eq!(format_usd(4_200), "$0.0042");
        assert_eq!(format_usd(1_234_567), "$1.23");
    }
}
