// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Normalized capability contracts for configured gateway keys.
//!
//! This is the single translation boundary between gateway-native model
//! metadata and the provider-neutral controls consumed by chat and the UI.

use temps_ai::{
    ModelCapability, ProviderAuthSource, ProviderCapabilities, RealtimeCapabilities, SelectOption,
};

fn option(id: &str, name: &str, description: &str) -> SelectOption {
    SelectOption {
        id: id.to_string(),
        name: name.to_string(),
        description: Some(description.to_string()),
    }
}

fn thinking_modes(provider: &str, model: &str) -> (Vec<SelectOption>, Option<String>) {
    if provider == "openai" && model.starts_with("gpt-5.6") {
        return thinking_options(&["none", "low", "medium", "high", "xhigh", "max"], "medium");
    }
    if provider == "openai" && (model.starts_with("gpt-5") || model.starts_with('o')) {
        return thinking_options(&["low", "medium", "high", "xhigh"], "medium");
    }
    if provider == "xai" && model.starts_with("grok-4.5") {
        return thinking_options(&["low", "medium", "high"], "high");
    }
    if provider == "anthropic"
        && (model.starts_with("claude-sonnet-5")
            || model.starts_with("claude-opus-5")
            || model.starts_with("claude-fable-5"))
    {
        // Adaptive thinking effort values differ between Claude SKUs and can
        // change independently. Advertise the portable intersection here;
        // live model discovery remains the source of truth for model ids.
        return thinking_options(&["low", "medium", "high"], "high");
    }
    if provider == "gemini"
        && model.starts_with("gemini-3")
        && (model.contains("flash") || model.contains("lite"))
    {
        return thinking_options(&["minimal", "low", "medium", "high"], "medium");
    }
    if provider == "gemini" && model.starts_with("gemini-3") {
        return thinking_options(&["low", "medium", "high"], "medium");
    }
    (Vec::new(), None)
}

/// Select the OpenAI wire API for a discovered model. OpenAI's model-list
/// endpoint only returns identifiers, so transport support cannot be inferred
/// from discovery metadata. GPT 5.4 and newer use the Responses API, which is
/// required for reasoning plus function tools and is OpenAI's recommended API
/// for new integrations.
pub(crate) fn uses_openai_responses_api(provider: &str, model: &str) -> bool {
    if provider != "openai" {
        return false;
    }
    let normalized_model = model.to_ascii_lowercase();
    let Some(version) = normalized_model.strip_prefix("gpt-") else {
        return false;
    };
    let mut components = version.split(['.', '-']);
    let major = components
        .next()
        .and_then(|value| value.parse::<u16>().ok());
    let minor = components
        .next()
        .and_then(|value| value.parse::<u16>().ok());
    matches!(
        (major, minor),
        (Some(major), _) if major > 5
    ) || matches!((major, minor), (Some(5), Some(minor)) if minor >= 4)
}

fn thinking_options(ids: &[&str], default: &str) -> (Vec<SelectOption>, Option<String>) {
    let names = |id: &str| match id {
        "none" => "None",
        "minimal" => "Minimal",
        "low" => "Low",
        "medium" => "Medium",
        "high" => "High",
        "xhigh" => "Extra high",
        "max" => "Maximum",
        _ => "Custom",
    };
    (
        ids.iter()
            .map(|id| option(id, names(id), "Provider-specific reasoning depth"))
            .collect(),
        Some(default.to_string()),
    )
}

pub fn gateway_provider_capabilities(
    id: String,
    name: String,
    provider: &str,
    default_model_id: Option<String>,
    mut model_ids: Vec<String>,
) -> ProviderCapabilities {
    if let Some(default) = default_model_id
        .as_deref()
        .filter(|model| !model.is_empty())
    {
        if !model_ids.iter().any(|model| model == default) {
            model_ids.insert(0, default.to_string());
        }
    }
    model_ids.sort();
    model_ids.dedup();
    let models = model_ids
        .into_iter()
        .map(|model| {
            let (thinking_modes, default_thinking_mode_id) = thinking_modes(provider, &model);
            ModelCapability {
                name: model.clone(),
                tool_thinking_modes: None,
                id: model,
                thinking_modes,
                default_thinking_mode_id,
            }
        })
        .collect::<Vec<_>>();
    let default_model_id = default_model_id
        .or_else(|| preferred_default_model(provider, &models))
        .or_else(|| models.first().map(|model| model.id.clone()));
    ProviderCapabilities {
        id,
        name,
        auth_source: ProviderAuthSource::ConfiguredKey,
        models,
        default_model_id,
        permission_modes: vec![
            option(
                "confirm-actions",
                "Ask before changes",
                "Require confirmation for write actions",
            ),
            option(
                "auto",
                "Run changes automatically",
                "Apply changes without asking; deletions still ask",
            ),
        ],
        default_permission_mode_id: Some("confirm-actions".to_string()),
        realtime: RealtimeCapabilities {
            text_streaming: true,
            reasoning_streaming: false,
            tool_events: true,
            user_interactions: true,
            cancellation: true,
        },
    }
}

/// Models worth starting a new chat on when nobody has pinned one, best first.
///
/// Without this the fallback was the first id alphabetically, which on a
/// catalogue of hundreds (OpenRouter) is an arbitrary model nobody chose. Only
/// providers with such a catalogue need an entry; the rest keep the first id.
fn preferred_default_model(provider: &str, models: &[ModelCapability]) -> Option<String> {
    let preferred: &[&str] = match provider {
        "openrouter" => &[
            "anthropic/claude-sonnet-5",
            "openai/gpt-4o",
            "google/gemini-3.5-flash",
            "openai/gpt-4o-mini",
        ],
        _ => &[],
    };
    preferred
        .iter()
        .find(|candidate| models.iter().any(|model| model.id == **candidate))
        .map(|candidate| candidate.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unpinned_openrouter_key_starts_on_a_known_good_model_not_the_first_id() {
        let capabilities = gateway_provider_capabilities(
            "gateway_key:7".to_string(),
            "OpenRouter".to_string(),
            "openrouter",
            None,
            vec![
                "aion-labs/aion-2.0".to_string(),
                "openai/gpt-4o".to_string(),
                "zeta/last".to_string(),
            ],
        );
        assert_eq!(
            capabilities.default_model_id.as_deref(),
            Some("openai/gpt-4o")
        );
    }

    #[test]
    fn unpinned_default_falls_back_to_first_id_when_no_preferred_model_exists() {
        let capabilities = gateway_provider_capabilities(
            "gateway_key:7".to_string(),
            "OpenRouter".to_string(),
            "openrouter",
            None,
            vec!["b/two".to_string(), "a/one".to_string()],
        );
        assert_eq!(capabilities.default_model_id.as_deref(), Some("a/one"));
    }

    #[test]
    fn a_pinned_default_always_wins_over_the_preferred_list() {
        let capabilities = gateway_provider_capabilities(
            "gateway_key:7".to_string(),
            "OpenRouter".to_string(),
            "openrouter",
            Some("aion-labs/aion-2.0".to_string()),
            vec![
                "aion-labs/aion-2.0".to_string(),
                "openai/gpt-4o".to_string(),
            ],
        );
        assert_eq!(
            capabilities.default_model_id.as_deref(),
            Some("aion-labs/aion-2.0")
        );
    }

    #[test]
    fn custom_default_model_is_part_of_the_normalized_catalog() {
        let capabilities = gateway_provider_capabilities(
            "gateway_key:7".to_string(),
            "Custom".to_string(),
            "openai",
            Some("custom-model".to_string()),
            vec!["gpt-5".to_string()],
        );
        assert!(capabilities.model("custom-model").is_some());
        assert_eq!(
            capabilities.default_model_id.as_deref(),
            Some("custom-model")
        );
    }

    #[test]
    fn gateway_providers_offer_both_confirming_and_automatic_writes() {
        let capabilities = gateway_provider_capabilities(
            "gateway_key:7".to_string(),
            "OpenRouter".to_string(),
            "openrouter",
            None,
            Vec::new(),
        );
        assert!(capabilities.permission_mode("confirm-actions").is_some());
        assert!(capabilities.permission_mode("auto").is_some());
        // Existing conversations and non-assistant chats keep asking by default.
        assert_eq!(
            capabilities.default_permission_mode_id.as_deref(),
            Some("confirm-actions")
        );
    }

    #[test]
    fn gemini_pro_does_not_advertise_unsupported_minimal_thinking() {
        let (modes, _) = thinking_modes("gemini", "gemini-3.1-pro");
        assert!(!modes.iter().any(|mode| mode.id == "minimal"));
    }

    #[test]
    fn gemini_flash_advertises_minimal_thinking() {
        let (modes, _) = thinking_modes("gemini", "gemini-3.6-flash");
        assert!(modes.iter().any(|mode| mode.id == "minimal"));
    }

    #[test]
    fn gpt_5_6_luna_uses_responses_and_keeps_tool_reasoning() {
        let capabilities = gateway_provider_capabilities(
            "gateway_key:7".to_string(),
            "OpenAI".to_string(),
            "openai",
            Some("gpt-5.6-luna".to_string()),
            vec!["gpt-5.6-luna".to_string()],
        );
        let model = capabilities
            .model("gpt-5.6-luna")
            .expect("discovered model should be normalized");
        assert!(model.thinking_modes.iter().any(|mode| mode.id == "medium"));
        assert!(model.tool_thinking_modes.is_none());
        assert!(uses_openai_responses_api("openai", "gpt-5.6-luna"));
        assert!(uses_openai_responses_api("openai", "gpt-5.4"));
        assert!(!uses_openai_responses_api("openai", "gpt-5"));
        assert!(uses_openai_responses_api("openai", "gpt-6"));
        assert!(!uses_openai_responses_api("xai", "gpt-5.6-luna"));
    }
}
