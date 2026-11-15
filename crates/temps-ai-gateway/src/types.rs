// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

// ============================================================================
// OpenAI-compatible request/response types
// These are the wire format for the /ai/v1/* endpoints.
// All providers translate to/from these types.
// ============================================================================

/// OpenAI-compatible chat completion request.
/// Uses `deny_unknown_fields = false` (serde default) so that SDK-specific
/// fields like `stream_options`, `logprobs`, `top_logprobs`, `logit_bias`,
/// `parallel_tool_calls`, etc. are silently accepted without breaking.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct ChatCompletionRequest {
    pub model: String,
    pub messages: Vec<ChatMessage>,
    #[serde(default)]
    pub stream: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub temperature: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_tokens: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub top_p: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub stop: Option<StopSequence>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub n: Option<i32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tools: Option<Vec<serde_json::Value>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_choice: Option<serde_json::Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub response_format: Option<serde_json::Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub frequency_penalty: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub presence_penalty: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub seed: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub user: Option<String>,
    /// Tolerates extra SDK fields (stream_options, logprobs, etc.)
    #[serde(flatten)]
    pub extra: Option<serde_json::Map<String, serde_json::Value>>,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
#[serde(untagged)]
pub enum StopSequence {
    Single(String),
    Multiple(Vec<String>),
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct ChatMessage {
    pub role: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub content: Option<MessageContent>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_calls: Option<Vec<serde_json::Value>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_call_id: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ToSchema)]
#[serde(untagged)]
pub enum MessageContent {
    Text(String),
    Parts(Vec<ContentPart>),
}

impl MessageContent {
    pub fn as_text(&self) -> Option<&str> {
        match self {
            MessageContent::Text(s) => Some(s),
            MessageContent::Parts(parts) => {
                // Return first text part
                parts.iter().find_map(|p| {
                    if p.r#type == "text" {
                        p.text.as_deref()
                    } else {
                        None
                    }
                })
            }
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ToSchema)]
pub struct ContentPart {
    pub r#type: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub text: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub image_url: Option<serde_json::Value>,
    /// Prompt-cache breakpoint for providers that cache explicitly, e.g.
    /// `{"type": "ephemeral"}`. Everything up to and including this part is
    /// cacheable.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cache_control: Option<serde_json::Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct ChatCompletionResponse {
    pub id: String,
    pub object: String,
    pub created: i64,
    pub model: String,
    pub choices: Vec<ChatCompletionChoice>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub usage: Option<UsageInfo>,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct ChatCompletionChoice {
    pub index: i32,
    pub message: ChatMessage,
    pub finish_reason: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct UsageInfo {
    /// The whole prompt, including any part served from the provider's cache.
    pub prompt_tokens: i64,
    pub completion_tokens: i64,
    pub total_tokens: i64,
    /// How the prompt split between fresh and cached tokens, when the provider says.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub prompt_tokens_details: Option<PromptTokensDetails>,
}

/// The cached share of a prompt. Both counts are parts of `prompt_tokens`.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct PromptTokensDetails {
    /// Tokens read from the provider's prompt cache.
    #[serde(default)]
    pub cached_tokens: i64,
    /// Tokens written to the provider's prompt cache by this request.
    #[serde(default)]
    pub cache_write_tokens: i64,
}

// ============================================================================
// Streaming chunk types (SSE data payload)
// ============================================================================

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChatCompletionChunk {
    pub id: String,
    pub object: String,
    pub created: i64,
    pub model: String,
    pub choices: Vec<ChatCompletionChunkChoice>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub usage: Option<UsageInfo>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChatCompletionChunkChoice {
    pub index: i32,
    pub delta: ChatCompletionDelta,
    pub finish_reason: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChatCompletionDelta {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub role: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub content: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_calls: Option<Vec<serde_json::Value>>,
}

// ============================================================================
// Embeddings
// ============================================================================

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct EmbeddingRequest {
    pub model: String,
    pub input: EmbeddingInput,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub encoding_format: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub dimensions: Option<i32>,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
#[serde(untagged)]
pub enum EmbeddingInput {
    Single(String),
    Multiple(Vec<String>),
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct EmbeddingResponse {
    pub object: String,
    pub data: Vec<EmbeddingData>,
    pub model: String,
    pub usage: EmbeddingUsage,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct EmbeddingData {
    pub object: String,
    pub embedding: Vec<f64>,
    pub index: i32,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct EmbeddingUsage {
    pub prompt_tokens: i64,
    pub total_tokens: i64,
}

// ============================================================================
// Model listing
// ============================================================================

/// What a provider's model catalogue says about one model, kept so a user can
/// choose between models without leaving the console. Every field is optional
/// because catalogues differ: OpenRouter publishes all of them, others few.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, ToSchema)]
pub struct ModelDetails {
    /// Human-readable name, when it differs from the model id.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    /// Maximum context window in tokens.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub context_length: Option<u64>,
    /// What the model accepts: `text`, `image`, `file`, `audio`, ...
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub input_modalities: Vec<String>,
    /// What the model produces: `text`, `image`, ...
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub output_modalities: Vec<String>,
    /// USD per one million input (prompt) tokens.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub input_price_per_million: Option<f64>,
    /// USD per one million output (completion) tokens.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub output_price_per_million: Option<f64>,
    /// USD per one million input tokens read from the provider's prompt cache.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cache_read_price_per_million: Option<f64>,
    /// USD per one million input tokens written to the provider's prompt cache.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cache_write_price_per_million: Option<f64>,
    /// Whether the catalogue lists tool calling. The Temps assistant needs it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub supports_tools: Option<bool>,
}

impl ModelDetails {
    /// Read the fields a catalogue entry publishes. Returns `None` when the
    /// entry carries none of them, so providers that list only ids store
    /// nothing instead of an empty object.
    pub fn from_catalog_entry(entry: &serde_json::Value) -> Option<Self> {
        Self::from_entry(entry, "name")
    }

    /// As [`Self::from_catalog_entry`], for a Gemini `/models` entry. Its
    /// `name` is the resource path (`models/<id>`), not a label; the label is
    /// `displayName`, so a model without one keeps its id as its name.
    pub fn from_gemini_catalog_entry(entry: &serde_json::Value) -> Option<Self> {
        Self::from_entry(entry, "displayName")
    }

    fn from_entry(entry: &serde_json::Value, name_field: &str) -> Option<Self> {
        let strings = |value: Option<&serde_json::Value>| -> Vec<String> {
            value
                .and_then(serde_json::Value::as_array)
                .map(|items| {
                    items
                        .iter()
                        .filter_map(serde_json::Value::as_str)
                        .map(str::to_string)
                        .collect()
                })
                .unwrap_or_default()
        };
        // Prices arrive as USD-per-token decimal strings.
        let per_million = |value: Option<&serde_json::Value>| -> Option<f64> {
            let per_token = match value? {
                serde_json::Value::String(text) => text.trim().parse::<f64>().ok()?,
                serde_json::Value::Number(number) => number.as_f64()?,
                _ => return None,
            };
            // A negative price is OpenRouter's marker for "varies"; not a price.
            (per_token.is_finite() && per_token >= 0.0).then_some(per_token * 1_000_000.0)
        };
        let architecture = entry.get("architecture");
        // OpenRouter nests modalities under `architecture`; OpenAI-compatible
        // servers that publish them at all put them on the entry itself.
        let modalities = |field: &str| {
            let nested = strings(architecture.and_then(|a| a.get(field)));
            if nested.is_empty() {
                strings(entry.get(field))
            } else {
                nested
            }
        };
        let pricing = entry.get("pricing");
        let supported = entry
            .get("supported_parameters")
            .and_then(serde_json::Value::as_array);
        let details = Self {
            // The name doubles as the model row's display name, so it must
            // fit that column and render as one line.
            name: entry
                .get(name_field)
                .and_then(serde_json::Value::as_str)
                .map(str::trim)
                .filter(|name| {
                    !name.is_empty() && name.len() <= 255 && !name.chars().any(char::is_control)
                })
                .map(str::to_string),
            context_length: entry
                .get("context_length")
                .or_else(|| entry.get("context_window"))
                .and_then(serde_json::Value::as_u64),
            input_modalities: modalities("input_modalities"),
            output_modalities: modalities("output_modalities"),
            input_price_per_million: per_million(pricing.and_then(|p| p.get("prompt"))),
            output_price_per_million: per_million(pricing.and_then(|p| p.get("completion"))),
            cache_read_price_per_million: per_million(
                pricing.and_then(|p| p.get("input_cache_read")),
            ),
            cache_write_price_per_million: per_million(
                pricing.and_then(|p| p.get("input_cache_write")),
            ),
            supports_tools: supported.map(|params| {
                params
                    .iter()
                    .any(|param| param.as_str().is_some_and(|name| name == "tools"))
            }),
        };
        (details != Self::default()).then_some(details)
    }
}

/// A catalogue entry together with whatever the provider said about it.
#[derive(Debug, Clone)]
pub struct DiscoveredModel {
    pub info: ModelInfo,
    pub details: Option<ModelDetails>,
}

impl From<ModelInfo> for DiscoveredModel {
    /// A listing entry the provider said nothing more about.
    fn from(info: ModelInfo) -> Self {
        Self {
            info,
            details: None,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct ModelInfo {
    pub id: String,
    pub object: String,
    pub owned_by: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct ModelListResponse {
    pub object: String,
    pub data: Vec<ModelInfo>,
}

// ============================================================================
// OpenAI-compatible error response
// ============================================================================

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct OpenAiErrorResponse {
    pub error: OpenAiError,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct OpenAiError {
    pub message: String,
    pub r#type: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub code: Option<String>,
}

impl OpenAiErrorResponse {
    pub fn new(message: impl Into<String>, error_type: &str, code: Option<&str>) -> Self {
        Self {
            error: OpenAiError {
                message: message.into(),
                r#type: error_type.to_string(),
                code: code.map(|c| c.to_string()),
            },
        }
    }

    pub fn invalid_request(message: impl Into<String>, code: &str) -> Self {
        Self::new(message, "invalid_request_error", Some(code))
    }

    pub fn server_error(message: impl Into<String>, code: &str) -> Self {
        Self::new(message, "server_error", Some(code))
    }

    pub fn rate_limit(message: impl Into<String>) -> Self {
        Self::new(message, "rate_limit_error", Some("rate_limit_exceeded"))
    }
}

#[cfg(test)]
mod model_details_tests {
    use super::ModelDetails;
    use serde_json::json;

    #[test]
    fn reads_modalities_prices_context_and_tool_support() {
        let entry = json!({
            "id": "vendor/model-a",
            "name": "Vendor: Model A",
            "context_length": 200000,
            "architecture": {
                "input_modalities": ["text", "image"],
                "output_modalities": ["text"]
            },
            "pricing": { "prompt": "0.000003", "completion": "0.000015" },
            "supported_parameters": ["temperature", "tools", "max_tokens"]
        });
        let details = ModelDetails::from_catalog_entry(&entry).expect("details");
        assert_eq!(details.name.as_deref(), Some("Vendor: Model A"));
        assert_eq!(details.context_length, Some(200_000));
        assert_eq!(details.input_modalities, vec!["text", "image"]);
        assert_eq!(details.output_modalities, vec!["text"]);
        assert!((details.input_price_per_million.unwrap() - 3.0).abs() < 1e-9);
        assert!((details.output_price_per_million.unwrap() - 15.0).abs() < 1e-9);
        assert_eq!(details.supports_tools, Some(true));
    }

    #[test]
    fn missing_tools_parameter_means_unsupported_but_absent_list_means_unknown() {
        let without_tools = json!({ "supported_parameters": ["temperature"] });
        assert_eq!(
            ModelDetails::from_catalog_entry(&without_tools)
                .unwrap()
                .supports_tools,
            Some(false)
        );
        let no_list = json!({ "name": "Only A Name" });
        assert_eq!(
            ModelDetails::from_catalog_entry(&no_list)
                .unwrap()
                .supports_tools,
            None
        );
    }

    #[test]
    fn variable_or_malformed_prices_are_not_reported() {
        let entry = json!({ "pricing": { "prompt": "-1", "completion": "free" } });
        assert_eq!(ModelDetails::from_catalog_entry(&entry), None);
    }

    #[test]
    fn reads_an_openai_compatible_listing_with_flat_fields() {
        let entry = json!({
            "id": "example-flash",
            "object": "model",
            "owned_by": "example",
            "name": "Example-V1-Flash",
            "context_window": 1048576,
            "input_modalities": ["text", "image"],
            "output_modalities": ["text"]
        });
        let details = ModelDetails::from_catalog_entry(&entry).expect("details");
        assert_eq!(details.name.as_deref(), Some("Example-V1-Flash"));
        assert_eq!(details.context_length, Some(1_048_576));
        assert_eq!(details.input_modalities, vec!["text", "image"]);
        assert_eq!(details.output_modalities, vec!["text"]);
    }

    #[test]
    fn a_gemini_entry_is_named_by_its_display_name_not_its_resource_path() {
        let entry = json!({
            "name": "models/gemini-3.1-pro",
            "displayName": "Gemini 3.1 Pro"
        });
        let details = ModelDetails::from_gemini_catalog_entry(&entry).expect("details");
        assert_eq!(details.name.as_deref(), Some("Gemini 3.1 Pro"));
        // Without a label there is nothing to store; the id stays the name.
        assert_eq!(
            ModelDetails::from_gemini_catalog_entry(&json!({ "name": "models/gemini-3.1-pro" })),
            None
        );
    }

    #[test]
    fn a_name_that_cannot_be_a_display_name_is_dropped() {
        for name in ["   ", "two\nlines", &"x".repeat(256)] {
            assert_eq!(
                ModelDetails::from_catalog_entry(&json!({ "name": name })),
                None,
                "{name:?} must not become a display name"
            );
        }
    }

    #[test]
    fn an_entry_with_only_an_id_has_no_details() {
        assert_eq!(
            ModelDetails::from_catalog_entry(&json!({ "id": "plain" })),
            None
        );
    }
}
