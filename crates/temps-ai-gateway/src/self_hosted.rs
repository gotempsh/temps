// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Self-hosted OpenAI-compatible provider keys (MLX, vLLM, llama.cpp,
//! LM Studio, Ollama's OpenAI API, ...).
//!
//! Unlike the vendor providers, nothing about such a server can be assumed
//! from a vendor name: its model IDs are arbitrary (often Hugging Face style
//! `org/model`), an API key is optional, and which OpenAI endpoints it
//! implements varies. So model IDs come only from its own `/models` listing,
//! and its capabilities are declared per key by the administrator.

use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

/// Provider identifier stored on `ai_provider_keys.provider`.
pub const OPENAI_COMPATIBLE: &str = "openai_compatible";

/// Whether `provider` is the self-hosted provider type.
pub fn is_self_hosted(provider: &str) -> bool {
    provider == OPENAI_COMPATIBLE
}

/// Capabilities an administrator declared for a self-hosted key. Requests
/// needing an undeclared capability are refused before anything is sent.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct SelfHostedCapabilities {
    /// Streaming chat completions (`"stream": true`). Required by AI chat.
    #[serde(default = "enabled")]
    pub streaming: bool,
    /// Function/tool calling in chat completions.
    #[serde(default = "enabled")]
    pub tools: bool,
    /// The `/embeddings` endpoint.
    #[serde(default)]
    pub embeddings: bool,
    /// The OpenAI Responses API (`/responses`).
    #[serde(default)]
    pub responses_api: bool,
}

fn enabled() -> bool {
    true
}

impl Default for SelfHostedCapabilities {
    fn default() -> Self {
        Self {
            streaming: true,
            tools: true,
            embeddings: false,
            responses_api: false,
        }
    }
}

impl SelfHostedCapabilities {
    /// Read the declaration stored on a key. A missing or unreadable value
    /// falls back to the defaults (chat, streaming and tools only).
    pub fn from_stored(value: Option<&serde_json::Value>) -> Self {
        value
            .and_then(|value| serde_json::from_value(value.clone()).ok())
            .unwrap_or_default()
    }

    pub fn to_stored(self) -> serde_json::Value {
        serde_json::json!({
            "streaming": self.streaming,
            "tools": self.tools,
            "embeddings": self.embeddings,
            "responses_api": self.responses_api,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_cover_chat_only() {
        let defaults = SelfHostedCapabilities::from_stored(None);
        assert!(defaults.streaming && defaults.tools);
        assert!(!defaults.embeddings && !defaults.responses_api);
    }

    #[test]
    fn stored_declaration_round_trips_and_missing_fields_default() {
        let declared = SelfHostedCapabilities {
            streaming: false,
            tools: false,
            embeddings: true,
            responses_api: true,
        };
        assert_eq!(
            SelfHostedCapabilities::from_stored(Some(&declared.to_stored())),
            declared
        );
        let partial = serde_json::json!({ "embeddings": true });
        let parsed = SelfHostedCapabilities::from_stored(Some(&partial));
        assert!(parsed.embeddings && parsed.streaming && parsed.tools);
        assert!(!parsed.responses_api);
        let garbage = serde_json::json!("nope");
        assert_eq!(
            SelfHostedCapabilities::from_stored(Some(&garbage)),
            SelfHostedCapabilities::default()
        );
    }
}
