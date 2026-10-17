// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Model price catalog and cost estimation for the AI gateway.
//!
//! Prices are USD per 1M tokens. The usage service prices requests that were
//! logged without a cost from this catalog, and the pricing endpoint serves it.

use serde::Serialize;
use utoipa::ToSchema;

/// Pricing for a single model, all values in USD per 1M tokens.
/// Fields are optional because not every provider supports every pricing tier.
#[derive(Debug, Serialize, ToSchema)]
pub struct ModelPricing {
    /// Model identifier (e.g. "gpt-5.4", "claude-sonnet-4-6")
    pub model: String,
    /// Human-readable model name (e.g. "Claude Sonnet 4.6")
    pub display_name: String,
    /// Provider ID (e.g. "openai", "anthropic")
    pub provider: String,
    /// Base input token cost per 1M tokens
    pub input_per_million: f64,
    /// Output token cost per 1M tokens
    pub output_per_million: f64,
    /// 5-minute cache write cost per 1M tokens (Anthropic-style prompt caching)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cache_write_5m_per_million: Option<f64>,
    /// 1-hour cache write cost per 1M tokens
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cache_write_1h_per_million: Option<f64>,
    /// Cache hit / refresh cost per 1M tokens
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cache_hit_per_million: Option<f64>,
    /// Batch API input cost per 1M tokens (if provider offers batch pricing)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub batch_input_per_million: Option<f64>,
    /// Batch API output cost per 1M tokens
    #[serde(skip_serializing_if = "Option::is_none")]
    pub batch_output_per_million: Option<f64>,
    /// Whether the model is deprecated
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub deprecated: bool,
}

// ============================================================================
// Pricing data builder
// ============================================================================

struct PricingBuilder {
    model: String,
    display_name: String,
    provider: String,
    input: f64,
    output: f64,
    cache_write_5m: Option<f64>,
    cache_write_1h: Option<f64>,
    cache_hit: Option<f64>,
    batch_input: Option<f64>,
    batch_output: Option<f64>,
    deprecated: bool,
}

impl PricingBuilder {
    fn new(provider: &str, model: &str, display_name: &str, input: f64, output: f64) -> Self {
        Self {
            model: model.into(),
            display_name: display_name.into(),
            provider: provider.into(),
            input,
            output,
            cache_write_5m: None,
            cache_write_1h: None,
            cache_hit: None,
            batch_input: None,
            batch_output: None,
            deprecated: false,
        }
    }

    fn cache(mut self, write_5m: f64, write_1h: f64, hit: f64) -> Self {
        self.cache_write_5m = Some(write_5m);
        self.cache_write_1h = Some(write_1h);
        self.cache_hit = Some(hit);
        self
    }

    fn batch(mut self, input: f64, output: f64) -> Self {
        self.batch_input = Some(input);
        self.batch_output = Some(output);
        self
    }

    #[allow(dead_code)]
    fn deprecated(mut self) -> Self {
        self.deprecated = true;
        self
    }

    fn build(self) -> ModelPricing {
        ModelPricing {
            model: self.model,
            display_name: self.display_name,
            provider: self.provider,
            input_per_million: self.input,
            output_per_million: self.output,
            cache_write_5m_per_million: self.cache_write_5m,
            cache_write_1h_per_million: self.cache_write_1h,
            cache_hit_per_million: self.cache_hit,
            batch_input_per_million: self.batch_input,
            batch_output_per_million: self.batch_output,
            deprecated: self.deprecated,
        }
    }
}

pub fn build_pricing() -> Vec<ModelPricing> {
    vec![
        // ── Anthropic ───────────────────────────────────────────────────
        PricingBuilder::new("anthropic", "claude-opus-5", "Claude Opus 5", 5.0, 25.0)
            .cache(6.25, 10.0, 0.50)
            .batch(2.50, 12.50)
            .build(),
        // Introductory Sonnet 5 pricing through 2026-08-31.
        PricingBuilder::new("anthropic", "claude-sonnet-5", "Claude Sonnet 5", 2.0, 10.0)
            .cache(2.50, 4.0, 0.20)
            .batch(1.0, 5.0)
            .build(),
        PricingBuilder::new("anthropic", "claude-fable-5", "Claude Fable 5", 10.0, 50.0)
            .cache(12.50, 20.0, 1.0)
            .batch(5.0, 25.0)
            .build(),
        PricingBuilder::new("anthropic", "claude-opus-4-6", "Claude Opus 4.6", 5.0, 25.0)
            .cache(6.25, 10.0, 0.50)
            .batch(2.50, 12.50)
            .build(),
        PricingBuilder::new(
            "anthropic",
            "claude-sonnet-4-6",
            "Claude Sonnet 4.6",
            3.0,
            15.0,
        )
        .cache(3.75, 6.0, 0.30)
        .batch(1.50, 7.50)
        .build(),
        PricingBuilder::new(
            "anthropic",
            "claude-haiku-4-5",
            "Claude Haiku 4.5",
            1.0,
            5.0,
        )
        .cache(1.25, 2.0, 0.10)
        .batch(0.50, 2.50)
        .build(),
        // ── OpenAI ──────────────────────────────────────────────────────
        PricingBuilder::new("openai", "gpt-5.6-sol", "GPT-5.6 Sol", 5.0, 30.0)
            .cache(0.0, 0.0, 0.50)
            .build(),
        PricingBuilder::new("openai", "gpt-5.6", "GPT-5.6", 5.0, 30.0)
            .cache(0.0, 0.0, 0.50)
            .build(),
        PricingBuilder::new("openai", "gpt-5.6-terra", "GPT-5.6 Terra", 2.50, 15.0)
            .cache(0.0, 0.0, 0.25)
            .build(),
        PricingBuilder::new("openai", "gpt-5.6-luna", "GPT-5.6 Luna", 1.0, 6.0)
            .cache(0.0, 0.0, 0.10)
            .build(),
        PricingBuilder::new("openai", "gpt-5.4", "GPT-5.4", 2.50, 10.0)
            .cache(0.0, 0.0, 1.25)
            .batch(1.25, 5.0)
            .build(),
        PricingBuilder::new("openai", "gpt-5.4-pro", "GPT-5.4 Pro", 15.0, 60.0).build(),
        PricingBuilder::new("openai", "gpt-5-mini", "GPT-5 Mini", 0.40, 1.60)
            .cache(0.0, 0.0, 0.20)
            .batch(0.20, 0.80)
            .build(),
        PricingBuilder::new("openai", "gpt-5-nano", "GPT-5 Nano", 0.10, 0.40)
            .cache(0.0, 0.0, 0.05)
            .batch(0.05, 0.20)
            .build(),
        PricingBuilder::new("openai", "gpt-5", "GPT-5", 2.0, 8.0)
            .cache(0.0, 0.0, 1.0)
            .batch(1.0, 4.0)
            .build(),
        PricingBuilder::new("openai", "gpt-4.1", "GPT-4.1", 2.0, 8.0)
            .cache(0.0, 0.0, 0.50)
            .batch(0.50, 2.0)
            .build(),
        PricingBuilder::new("openai", "gpt-4.1-mini", "GPT-4.1 Mini", 0.40, 1.60)
            .cache(0.0, 0.0, 0.10)
            .batch(0.10, 0.40)
            .build(),
        PricingBuilder::new("openai", "gpt-4.1-nano", "GPT-4.1 Nano", 0.10, 0.40)
            .cache(0.0, 0.0, 0.025)
            .batch(0.025, 0.10)
            .build(),
        PricingBuilder::new("openai", "o3", "o3", 10.0, 40.0)
            .cache(0.0, 0.0, 5.0)
            .batch(5.0, 20.0)
            .build(),
        PricingBuilder::new("openai", "o3-pro", "o3 Pro", 20.0, 80.0).build(),
        PricingBuilder::new("openai", "o4-mini", "o4-mini", 1.10, 4.40)
            .cache(0.0, 0.0, 0.55)
            .batch(0.55, 2.20)
            .build(),
        PricingBuilder::new("openai", "o3-mini", "o3 Mini", 1.10, 4.40)
            .cache(0.0, 0.0, 0.55)
            .batch(0.55, 2.20)
            .build(),
        PricingBuilder::new("openai", "gpt-4o", "GPT-4o", 2.50, 10.0)
            .cache(0.0, 0.0, 1.25)
            .batch(1.25, 5.0)
            .build(),
        PricingBuilder::new("openai", "gpt-4o-mini", "GPT-4o Mini", 0.15, 0.60)
            .cache(0.0, 0.0, 0.075)
            .batch(0.075, 0.30)
            .build(),
        // ── xAI ─────────────────────────────────────────────────────────
        PricingBuilder::new("xai", "grok-4.5", "Grok 4.5", 2.0, 6.0)
            .cache(0.0, 0.0, 0.30)
            .build(),
        PricingBuilder::new("xai", "grok-4.20", "Grok 4.20", 1.25, 2.50)
            .cache(0.0, 0.0, 0.20)
            .build(),
        PricingBuilder::new(
            "xai",
            "grok-4.20-0309-reasoning",
            "Grok 4.20 Reasoning",
            1.25,
            2.50,
        )
        .cache(0.0, 0.0, 0.20)
        .build(),
        PricingBuilder::new(
            "xai",
            "grok-4-1-fast-reasoning",
            "Grok 4-1 Fast Reasoning",
            0.20,
            0.50,
        )
        .build(),
        PricingBuilder::new(
            "xai",
            "grok-4-1-fast-non-reasoning",
            "Grok 4-1 Fast Non-Reasoning",
            0.20,
            0.50,
        )
        .build(),
        PricingBuilder::new("xai", "grok-code-fast-1", "Grok Code Fast 1", 0.20, 1.50).build(),
        PricingBuilder::new(
            "xai",
            "grok-4-fast-reasoning",
            "Grok 4 Fast Reasoning",
            0.20,
            0.50,
        )
        .build(),
        PricingBuilder::new(
            "xai",
            "grok-4-fast-non-reasoning",
            "Grok 4 Fast Non-Reasoning",
            0.20,
            0.50,
        )
        .build(),
        PricingBuilder::new("xai", "grok-4-0709", "Grok 4", 3.0, 15.0).build(),
        PricingBuilder::new("xai", "grok-3", "Grok 3", 3.0, 15.0).build(),
        PricingBuilder::new("xai", "grok-3-mini", "Grok 3 Mini", 0.30, 0.50).build(),
        // ── Gemini ──────────────────────────────────────────────────────
        PricingBuilder::new("gemini", "gemini-3.6-flash", "Gemini 3.6 Flash", 1.50, 7.50).build(),
        PricingBuilder::new("gemini", "gemini-3.5-flash", "Gemini 3.5 Flash", 1.50, 9.0)
            .cache(0.0, 0.0, 0.15)
            .build(),
        PricingBuilder::new(
            "gemini",
            "gemini-3.5-flash-lite",
            "Gemini 3.5 Flash-Lite",
            0.30,
            2.50,
        )
        .build(),
        PricingBuilder::new("gemini", "gemini-3.1-pro", "Gemini 3.1 Pro", 1.25, 5.0)
            .cache(0.0, 0.0, 0.315)
            .build(),
        PricingBuilder::new(
            "gemini",
            "gemini-3.1-flash-lite",
            "Gemini 3.1 Flash Lite",
            0.075,
            0.30,
        )
        .cache(0.0, 0.0, 0.01875)
        .build(),
        PricingBuilder::new("gemini", "gemini-3-flash", "Gemini 3 Flash", 0.10, 0.40)
            .cache(0.0, 0.0, 0.025)
            .build(),
        PricingBuilder::new("gemini", "gemini-2.5-pro", "Gemini 2.5 Pro", 1.25, 10.0)
            .cache(0.0, 0.0, 0.315)
            .build(),
        PricingBuilder::new("gemini", "gemini-2.5-flash", "Gemini 2.5 Flash", 0.15, 0.60)
            .cache(0.0, 0.0, 0.0375)
            .build(),
        PricingBuilder::new(
            "gemini",
            "gemini-2.5-flash-lite",
            "Gemini 2.5 Flash Lite",
            0.075,
            0.30,
        )
        .cache(0.0, 0.0, 0.01875)
        .build(),
        PricingBuilder::new("gemini", "gemini-2-flash", "Gemini 2 Flash", 0.10, 0.40)
            .cache(0.0, 0.0, 0.025)
            .build(),
        PricingBuilder::new(
            "gemini",
            "gemini-2-flash-lite",
            "Gemini 2 Flash Lite",
            0.075,
            0.30,
        )
        .cache(0.0, 0.0, 0.01875)
        .build(),
        // ── OpenRouter ──────────────────────────────────────────────────
        // OpenRouter passes through hundreds of upstream models with
        // per-model pricing that changes independently of Temps releases;
        // this is a curated subset of popular routes, not the full catalog.
        // Live per-request cost is billed by OpenRouter directly.
        PricingBuilder::new(
            "openrouter",
            "openai/gpt-4o",
            "GPT-4o (via OpenRouter)",
            2.50,
            10.0,
        )
        .build(),
        PricingBuilder::new(
            "openrouter",
            "openai/gpt-4o-mini",
            "GPT-4o Mini (via OpenRouter)",
            0.15,
            0.60,
        )
        .build(),
        PricingBuilder::new(
            "openrouter",
            "anthropic/claude-sonnet-5",
            "Claude Sonnet 5 (via OpenRouter)",
            2.0,
            10.0,
        )
        .build(),
        PricingBuilder::new(
            "openrouter",
            "meta-llama/llama-3.3-70b-instruct",
            "Llama 3.3 70B (via OpenRouter)",
            0.13,
            0.40,
        )
        .build(),
        PricingBuilder::new(
            "openrouter",
            "deepseek/deepseek-chat",
            "DeepSeek Chat (via OpenRouter)",
            0.28,
            0.42,
        )
        .build(),
    ]
}

// ============================================================================
// Cost estimation
// ============================================================================

/// Per-token rates derived from the price list above. Because the list is in
/// USD per 1M tokens and usage costs are stored in microcents (1e-6 USD, as
/// `estimated_cost_microcents` documents), a rate is directly the cost of one
/// token in that unit: `$2.50 / 1M tokens` is `2.5` microcents per token.
struct RateTable {
    by_model: std::collections::HashMap<String, (f64, f64)>,
    /// Most expensive listed rates, charged for models that are not priced so
    /// an unlisted model can never be a way around a spend cap.
    worst_case: (f64, f64),
}

fn rate_table() -> &'static RateTable {
    static TABLE: std::sync::OnceLock<RateTable> = std::sync::OnceLock::new();
    TABLE.get_or_init(|| {
        let mut by_model = std::collections::HashMap::new();
        let mut worst_case = (0.0_f64, 0.0_f64);
        for model in build_pricing() {
            worst_case.0 = worst_case.0.max(model.input_per_million);
            worst_case.1 = worst_case.1.max(model.output_per_million);
            by_model.insert(
                model.model,
                (model.input_per_million, model.output_per_million),
            );
        }
        RateTable {
            by_model,
            worst_case,
        }
    })
}

/// Estimate the provider cost of a request in microcents (1/10000 of a cent).
///
/// Exact model ids win; otherwise the longest listed id the model extends at a
/// `-`, `:` or `@` boundary applies (so a dated snapshot `gpt-4o-2025-01-01`
/// is priced as `gpt-4o`). Unlisted models are charged at the most expensive
/// listed rate. This is an estimate from list prices: it ignores cache
/// discounts and batch pricing, so it errs on the high side.
pub fn estimate_cost_microcents(model: &str, input_tokens: i64, output_tokens: i64) -> i64 {
    let table = rate_table();
    let (input_rate, output_rate) = table
        .by_model
        .get(model)
        .copied()
        .or_else(|| {
            table
                .by_model
                .iter()
                .filter(|(id, _)| {
                    model.strip_prefix(id.as_str()).is_some_and(|rest| {
                        rest.starts_with('-') || rest.starts_with(':') || rest.starts_with('@')
                    })
                })
                .max_by_key(|(id, _)| id.len())
                .map(|(_, rates)| *rates)
        })
        .unwrap_or(table.worst_case);
    let cost = input_tokens.max(0) as f64 * input_rate + output_tokens.max(0) as f64 * output_rate;
    cost.ceil() as i64
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn estimate_cost_uses_listed_rates_in_microcents() {
        // gpt-4o: $2.50 in / $10 out per 1M tokens => 2.5 and 10 microcents per token.
        assert_eq!(
            estimate_cost_microcents("gpt-4o", 1_000, 100),
            2_500 + 1_000
        );
    }

    #[test]
    fn estimate_cost_prices_dated_snapshots_as_their_base_model() {
        assert_eq!(
            estimate_cost_microcents("gpt-4o-2025-01-01", 1_000, 100),
            estimate_cost_microcents("gpt-4o", 1_000, 100)
        );
        // A longer listed id must win over a shorter prefix of it.
        assert_eq!(
            estimate_cost_microcents("gpt-4o-mini-2025-01-01", 1_000, 0),
            estimate_cost_microcents("gpt-4o-mini", 1_000, 0)
        );
    }

    #[test]
    fn estimate_cost_charges_unlisted_models_at_worst_case() {
        let unlisted = estimate_cost_microcents("totally-unlisted-model", 1_000, 1_000);
        let cheapest = estimate_cost_microcents("gpt-4o-mini", 1_000, 1_000);
        assert!(unlisted > cheapest);
        let table = rate_table();
        assert_eq!(
            unlisted,
            (1_000.0 * table.worst_case.0 + 1_000.0 * table.worst_case.1).ceil() as i64
        );
    }

    #[test]
    fn estimate_cost_is_zero_for_no_tokens_and_ignores_negatives() {
        assert_eq!(estimate_cost_microcents("gpt-4o", 0, 0), 0);
        assert_eq!(estimate_cost_microcents("gpt-4o", -5, -5), 0);
    }

    #[test]
    fn test_build_pricing_has_all_providers() {
        let pricing = build_pricing();
        let providers: Vec<&str> = pricing.iter().map(|p| p.provider.as_str()).collect();
        assert!(providers.contains(&"openai"));
        assert!(providers.contains(&"anthropic"));
        assert!(providers.contains(&"xai"));
        assert!(providers.contains(&"gemini"));
        assert!(providers.contains(&"openrouter"));
    }

    #[test]
    fn test_build_pricing_positive_values() {
        let pricing = build_pricing();
        for model in &pricing {
            assert!(
                model.input_per_million > 0.0,
                "Input price for {} must be positive",
                model.model
            );
            assert!(
                model.output_per_million > 0.0,
                "Output price for {} must be positive",
                model.model
            );
        }
    }

    #[test]
    fn test_anthropic_has_cache_pricing() {
        let pricing = build_pricing();
        let anthropic: Vec<_> = pricing
            .iter()
            .filter(|p| p.provider == "anthropic")
            .collect();
        assert!(!anthropic.is_empty());
        for model in &anthropic {
            assert!(
                model.cache_write_5m_per_million.is_some(),
                "Anthropic {} should have 5m cache write pricing",
                model.model
            );
            assert!(
                model.cache_write_1h_per_million.is_some(),
                "Anthropic {} should have 1h cache write pricing",
                model.model
            );
            assert!(
                model.cache_hit_per_million.is_some(),
                "Anthropic {} should have cache hit pricing",
                model.model
            );
        }
    }

    #[test]
    fn test_anthropic_has_batch_pricing() {
        let pricing = build_pricing();
        let anthropic: Vec<_> = pricing
            .iter()
            .filter(|p| p.provider == "anthropic")
            .collect();
        for model in &anthropic {
            assert!(
                model.batch_input_per_million.is_some(),
                "Anthropic {} should have batch input pricing",
                model.model
            );
            assert!(
                model.batch_output_per_million.is_some(),
                "Anthropic {} should have batch output pricing",
                model.model
            );
        }
    }

    #[test]
    fn test_cache_fields_omitted_when_none() {
        let model = ModelPricing {
            model: "test".into(),
            display_name: "Test".into(),
            provider: "test".into(),
            input_per_million: 1.0,
            output_per_million: 2.0,
            cache_write_5m_per_million: None,
            cache_write_1h_per_million: None,
            cache_hit_per_million: None,
            batch_input_per_million: None,
            batch_output_per_million: None,
            deprecated: false,
        };
        let json = serde_json::to_string(&model).unwrap();
        assert!(!json.contains("cache_write_5m"));
        assert!(!json.contains("cache_write_1h"));
        assert!(!json.contains("cache_hit"));
        assert!(!json.contains("batch_input"));
        assert!(!json.contains("batch_output"));
        assert!(!json.contains("deprecated"));
    }

    #[test]
    fn test_deprecated_field_shown_when_true() {
        let model = PricingBuilder::new("test", "old-model", "Old Model", 1.0, 2.0)
            .deprecated()
            .build();
        let json = serde_json::to_string(&model).unwrap();
        assert!(json.contains("\"deprecated\":true"));
    }

    #[test]
    fn test_pricing_builder_defaults() {
        let model = PricingBuilder::new("test", "m", "M", 1.0, 2.0).build();
        assert!(model.cache_write_5m_per_million.is_none());
        assert!(model.cache_write_1h_per_million.is_none());
        assert!(model.cache_hit_per_million.is_none());
        assert!(model.batch_input_per_million.is_none());
        assert!(model.batch_output_per_million.is_none());
        assert!(!model.deprecated);
    }
}
