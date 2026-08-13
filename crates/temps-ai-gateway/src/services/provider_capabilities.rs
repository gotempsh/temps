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
    if provider == "openai" && (model.starts_with("gpt-5") || model.starts_with('o')) {
        return (
            [
                ("low", "Low"),
                ("medium", "Medium"),
                ("high", "High"),
                ("xhigh", "Extra high"),
            ]
            .into_iter()
            .map(|(id, name)| option(id, name, "Provider-specific reasoning depth"))
            .collect(),
            Some("medium".to_string()),
        );
    }
    (Vec::new(), None)
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
                id: model,
                thinking_modes,
                default_thinking_mode_id,
            }
        })
        .collect::<Vec<_>>();
    let default_model_id =
        default_model_id.or_else(|| models.first().map(|model| model.id.clone()));
    ProviderCapabilities {
        id,
        name,
        auth_source: ProviderAuthSource::ConfiguredKey,
        models,
        default_model_id,
        permission_modes: vec![option(
            "confirm-actions",
            "Confirm actions",
            "Require confirmation for write actions",
        )],
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

#[cfg(test)]
mod tests {
    use super::*;

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
}
