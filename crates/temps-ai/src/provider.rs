//! Provider-neutral discovery and runtime capability contracts.
//!
//! Provider adapters translate their native model, reasoning, permission, and
//! streaming concepts into these types. Chat, HTTP handlers, and frontends
//! consume only this normalized description and never branch on provider ids.

use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RefreshPolicy {
    Cached,
    Refresh,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum ProviderAuthSource {
    ConfiguredKey,
    HostEnvironment,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct SelectOption {
    pub id: String,
    pub name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct ModelCapability {
    pub id: String,
    pub name: String,
    pub thinking_modes: Vec<SelectOption>,
    /// Optional reasoning modes valid when function tools are attached. `None`
    /// inherits `thinking_modes`; `Some` is a model-specific restriction.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_thinking_modes: Option<Vec<SelectOption>>,
    pub default_thinking_mode_id: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct RealtimeCapabilities {
    pub text_streaming: bool,
    pub reasoning_streaming: bool,
    pub tool_events: bool,
    pub user_interactions: bool,
    pub cancellation: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct ProviderCapabilities {
    pub id: String,
    pub name: String,
    pub auth_source: ProviderAuthSource,
    pub models: Vec<ModelCapability>,
    pub default_model_id: Option<String>,
    pub permission_modes: Vec<SelectOption>,
    pub default_permission_mode_id: Option<String>,
    pub realtime: RealtimeCapabilities,
}

impl ProviderCapabilities {
    pub fn model(&self, id: &str) -> Option<&ModelCapability> {
        self.models.iter().find(|model| model.id == id)
    }

    pub fn permission_mode(&self, id: &str) -> Option<&SelectOption> {
        self.permission_modes.iter().find(|mode| mode.id == id)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn provider_capabilities_validate_controls_without_provider_branches() {
        let capabilities = ProviderCapabilities {
            id: "example".to_string(),
            name: "Example".to_string(),
            auth_source: ProviderAuthSource::HostEnvironment,
            models: vec![ModelCapability {
                id: "model-1".to_string(),
                name: "Model 1".to_string(),
                thinking_modes: vec![SelectOption {
                    id: "high".to_string(),
                    name: "High".to_string(),
                    description: None,
                }],
                tool_thinking_modes: None,
                default_thinking_mode_id: Some("high".to_string()),
            }],
            default_model_id: Some("model-1".to_string()),
            permission_modes: vec![SelectOption {
                id: "read_only".to_string(),
                name: "Read only".to_string(),
                description: None,
            }],
            default_permission_mode_id: Some("read_only".to_string()),
            realtime: RealtimeCapabilities {
                text_streaming: true,
                reasoning_streaming: false,
                tool_events: true,
                user_interactions: false,
                cancellation: true,
            },
        };

        assert!(capabilities.model("model-1").is_some());
        assert!(capabilities.permission_mode("read_only").is_some());
        assert!(capabilities.model("unknown").is_none());
    }
}
