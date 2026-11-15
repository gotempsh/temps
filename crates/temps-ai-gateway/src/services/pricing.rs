// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Model price catalog and cost estimation for the AI gateway.
//!
//! Prices are USD per 1M tokens. The usage service prices requests that were
//! logged without a cost from this catalog, and the pricing endpoint serves it.
//!
//! The table is maintained by hand from each provider's own pricing page, and
//! every row records the page it was checked against and when. It lives in
//! this repository, not in a third-party package, because it drives spend caps
//! and budgets: a price change is reviewed like any other code change.
//!
//! When a provider announces a dated price change (a promotion that ends, or
//! a scheduled increase), the row carries the price that applies after the
//! change, so a release that outlives the date never undercounts spend.

use serde::Serialize;
use utoipa::ToSchema;

use crate::types::PromptTokensDetails;

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
// Provenance
// ============================================================================

const ANTHROPIC_PRICING: &str = "https://platform.claude.com/docs/en/about-claude/pricing";
const OPENAI_PRICING: &str = "https://developers.openai.com/api/docs/pricing";
const OPENAI_GPT_5_6_SOL: &str = "https://developers.openai.com/api/docs/models/gpt-5.6-sol";
const XAI_PRICING: &str = "https://docs.x.ai/developers/pricing";
const GEMINI_PRICING: &str = "https://aistudio.google.com/docs/pricing";
const OPENROUTER_MODELS: &str = "https://openrouter.ai/api/v1/models";

/// Date (UTC) of the last full check of the table against every source above.
const CHECKED_2026_11_15: &str = "2026-11-15";

/// Where a row's prices come from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Provenance {
    /// The provider's own pricing page the row was checked against.
    source: &'static str,
    /// Day (`YYYY-MM-DD`, UTC) the row was last checked against `source`.
    checked_on: &'static str,
    /// `false` when `source` did not list this model at this price on
    /// `checked_on` (the model is no longer listed, or only a promotional
    /// price is). The row then keeps its earlier prices, unconfirmed, and is
    /// left out of the worst-case rate.
    confirmed: bool,
}

/// Higher rates a provider bills for the whole request once its prompt is
/// longer than a threshold.
#[derive(Debug, Clone, Copy, PartialEq)]
struct LongContext {
    /// The long-context rates apply when the request has more input tokens
    /// than this.
    above_input_tokens: i64,
    input: f64,
    output: f64,
    /// Long-context cache-read rate; `None` bills cache reads as long-context
    /// input.
    cache_read: Option<f64>,
    /// Long-context rate of the dearest cache-write tier; `None` bills cache
    /// writes as long-context input.
    cache_write: Option<f64>,
}

// ============================================================================
// Pricing data builder
// ============================================================================

/// One row of the price table: the published pricing plus the facts only
/// the estimator and the table's tests use.
struct CatalogEntry {
    pricing: ModelPricing,
    provenance: Option<Provenance>,
    long_context: Option<LongContext>,
    /// Rate of the dearest cache-write tier when the provider's tiers are not
    /// the 5-minute / 1-hour ones `ModelPricing` publishes (OpenAI's 12-hour
    /// write), so a written token is never estimated below what it bills.
    cache_write_ceiling: Option<f64>,
}

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
    provenance: Option<Provenance>,
    long_context: Option<LongContext>,
    cache_write_ceiling: Option<f64>,
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
            provenance: None,
            long_context: None,
            cache_write_ceiling: None,
        }
    }

    fn cache(mut self, write_5m: f64, write_1h: f64, hit: f64) -> Self {
        self.cache_write_5m = Some(write_5m);
        self.cache_write_1h = Some(write_1h);
        self.cache_hit = Some(hit);
        self
    }

    /// Cache-read price only, for a provider whose cache writes are not the
    /// 5-minute / 1-hour tiers this table models (OpenAI's 30-minute and
    /// 12-hour writes): the write fields stay unset rather than claim zero.
    fn cache_read(mut self, hit: f64) -> Self {
        self.cache_hit = Some(hit);
        self
    }

    fn batch(mut self, input: f64, output: f64) -> Self {
        self.batch_input = Some(input);
        self.batch_output = Some(output);
        self
    }

    fn long_context(mut self, above_input_tokens: i64, input: f64, output: f64) -> Self {
        self.long_context = Some(LongContext {
            above_input_tokens,
            input,
            output,
            cache_read: None,
            cache_write: None,
        });
        self
    }

    /// Long-context cache-read rate. Call after `long_context`.
    fn long_cache_read(mut self, rate: f64) -> Self {
        if let Some(long) = self.long_context.as_mut() {
            long.cache_read = Some(rate);
        }
        self
    }

    /// Long-context rate of the dearest cache-write tier. Call after
    /// `long_context`.
    fn long_cache_write(mut self, rate: f64) -> Self {
        if let Some(long) = self.long_context.as_mut() {
            long.cache_write = Some(rate);
        }
        self
    }

    /// Rate of the dearest cache-write tier, for a provider whose write tiers
    /// are not the 5-minute / 1-hour ones (OpenAI's 12-hour write).
    fn cache_write_ceiling(mut self, rate: f64) -> Self {
        self.cache_write_ceiling = Some(rate);
        self
    }

    fn deprecated(mut self) -> Self {
        self.deprecated = true;
        self
    }

    /// The row's prices match `source` as checked on `checked_on`.
    fn checked(mut self, source: &'static str, checked_on: &'static str) -> Self {
        self.provenance = Some(Provenance {
            source,
            checked_on,
            confirmed: true,
        });
        self
    }

    /// `source` was checked on `checked_on` and does not list this model at
    /// this price; the row keeps its earlier, unconfirmed prices.
    fn unconfirmed(mut self, source: &'static str, checked_on: &'static str) -> Self {
        self.provenance = Some(Provenance {
            source,
            checked_on,
            confirmed: false,
        });
        self
    }

    fn build(self) -> CatalogEntry {
        CatalogEntry {
            pricing: ModelPricing {
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
            },
            provenance: self.provenance,
            long_context: self.long_context,
            cache_write_ceiling: self.cache_write_ceiling,
        }
    }
}

/// Provider thresholds for long-context pricing, as each pricing page words
/// them: Anthropic bills Claude Haiku 5.5 prompts "over 100,000 tokens",
/// OpenAI ">272K input tokens", Gemini "prompts > 200k tokens", and xAI
/// prompts that reach "≥ 200k prompt tokens".
const ANTHROPIC_HAIKU_LONG_CONTEXT: i64 = 100_000;
const OPENAI_LONG_CONTEXT: i64 = 272_000;
const GEMINI_LONG_CONTEXT: i64 = 200_000;
const XAI_LONG_CONTEXT: i64 = 200_000 - 1;

pub fn build_pricing() -> Vec<ModelPricing> {
    catalog().into_iter().map(|entry| entry.pricing).collect()
}

fn catalog() -> Vec<CatalogEntry> {
    [
        anthropic_rows(),
        openai_rows(),
        xai_rows(),
        gemini_rows(),
        openrouter_rows(),
    ]
    .into_iter()
    .flatten()
    .collect()
}

fn anthropic_rows() -> Vec<CatalogEntry> {
    const A: &str = ANTHROPIC_PRICING;
    const D: &str = CHECKED_2026_11_15;
    vec![
        PricingBuilder::new(
            "anthropic",
            "claude-fable-5-2",
            "Claude Fable 5.2",
            8.0,
            40.0,
        )
        .cache(10.0, 16.0, 0.20)
        .batch(4.0, 20.0)
        .checked(A, D)
        .build(),
        PricingBuilder::new(
            "anthropic",
            "claude-fable-5-1",
            "Claude Fable 5.1",
            10.0,
            50.0,
        )
        .cache(12.50, 20.0, 0.25)
        .batch(5.0, 25.0)
        .checked(A, D)
        .build(),
        PricingBuilder::new("anthropic", "claude-fable-5", "Claude Fable 5", 10.0, 50.0)
            .cache(12.50, 20.0, 1.0)
            .batch(5.0, 25.0)
            .checked(A, D)
            .build(),
        PricingBuilder::new("anthropic", "claude-opus-5-5", "Claude Opus 5.5", 4.0, 20.0)
            .cache(5.0, 8.0, 0.20)
            .batch(2.0, 10.0)
            .checked(A, D)
            .build(),
        PricingBuilder::new("anthropic", "claude-opus-5", "Claude Opus 5", 5.0, 25.0)
            .cache(6.25, 10.0, 0.50)
            .batch(2.50, 12.50)
            .checked(A, D)
            .build(),
        PricingBuilder::new("anthropic", "claude-opus-4-8", "Claude Opus 4.8", 5.0, 25.0)
            .cache(6.25, 10.0, 0.50)
            .batch(2.50, 12.50)
            .checked(A, D)
            .build(),
        PricingBuilder::new("anthropic", "claude-opus-4-7", "Claude Opus 4.7", 5.0, 25.0)
            .cache(6.25, 10.0, 0.50)
            .batch(2.50, 12.50)
            .checked(A, D)
            .build(),
        PricingBuilder::new("anthropic", "claude-opus-4-6", "Claude Opus 4.6", 5.0, 25.0)
            .cache(6.25, 10.0, 0.50)
            .batch(2.50, 12.50)
            .checked(A, D)
            .build(),
        PricingBuilder::new("anthropic", "claude-opus-4-5", "Claude Opus 4.5", 5.0, 25.0)
            .cache(6.25, 10.0, 0.50)
            .batch(2.50, 12.50)
            .checked(A, D)
            .build(),
        PricingBuilder::new(
            "anthropic",
            "claude-sonnet-5-5",
            "Claude Sonnet 5.5",
            2.0,
            10.0,
        )
        .cache(2.50, 4.0, 0.10)
        .batch(1.0, 5.0)
        .checked(A, D)
        .build(),
        // The launch price, first announced as introductory, is now standard.
        PricingBuilder::new("anthropic", "claude-sonnet-5", "Claude Sonnet 5", 2.0, 10.0)
            .cache(2.50, 4.0, 0.20)
            .batch(1.0, 5.0)
            .checked(A, D)
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
        .checked(A, D)
        .build(),
        PricingBuilder::new(
            "anthropic",
            "claude-sonnet-4-5",
            "Claude Sonnet 4.5",
            3.0,
            15.0,
        )
        .cache(3.75, 6.0, 0.30)
        .batch(1.50, 7.50)
        .deprecated()
        .checked(A, D)
        .build(),
        PricingBuilder::new(
            "anthropic",
            "claude-haiku-5-5",
            "Claude Haiku 5.5",
            0.10,
            0.50,
        )
        .cache(0.125, 0.20, 0.01)
        .batch(0.05, 0.25)
        .long_context(ANTHROPIC_HAIKU_LONG_CONTEXT, 0.50, 2.50)
        .long_cache_read(0.05)
        .long_cache_write(1.0)
        .checked(A, D)
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
        .checked(A, D)
        .build(),
    ]
}

fn openai_rows() -> Vec<CatalogEntry> {
    const O: &str = OPENAI_PRICING;
    const D: &str = CHECKED_2026_11_15;
    vec![
        // Models marked deprecated are "Deprecated" or "Shut down" on their
        // model pages under https://developers.openai.com/api/docs/models.
        PricingBuilder::new("openai", "gpt-6-astra", "GPT-6 Astra", 10.0, 50.0)
            .cache_read(1.0)
            .batch(5.0, 25.0)
            .long_context(OPENAI_LONG_CONTEXT, 20.0, 75.0)
            .long_cache_read(2.0)
            .long_cache_write(50.0)
            .cache_write_ceiling(25.0)
            .checked(O, D)
            .build(),
        PricingBuilder::new("openai", "gpt-6.1-sol", "GPT-6.1 Sol", 2.0, 10.0)
            .cache_read(0.10)
            .batch(1.0, 5.0)
            .long_context(OPENAI_LONG_CONTEXT, 4.0, 15.0)
            .long_cache_read(0.2)
            .long_cache_write(10.0)
            .cache_write_ceiling(5.0)
            .checked(O, D)
            .build(),
        PricingBuilder::new("openai", "gpt-6.1-terra", "GPT-6.1 Terra", 1.0, 5.0)
            .cache_read(0.05)
            .batch(0.50, 2.50)
            .long_context(OPENAI_LONG_CONTEXT, 2.0, 7.50)
            .long_cache_read(0.1)
            .long_cache_write(5.0)
            .cache_write_ceiling(2.5)
            .checked(O, D)
            .build(),
        PricingBuilder::new("openai", "gpt-6-sol", "GPT-6 Sol", 2.0, 10.0)
            .cache_read(0.20)
            .batch(1.0, 5.0)
            .long_context(OPENAI_LONG_CONTEXT, 4.0, 15.0)
            .long_cache_read(0.4)
            .long_cache_write(10.0)
            .cache_write_ceiling(5.0)
            .checked(O, D)
            .build(),
        PricingBuilder::new("openai", "gpt-6-luna", "GPT-6 Luna", 0.10, 0.50)
            .cache_read(0.01)
            .batch(0.05, 0.25)
            .long_context(OPENAI_LONG_CONTEXT, 0.20, 0.75)
            .long_cache_read(0.02)
            .long_cache_write(0.5)
            .cache_write_ceiling(0.25)
            .checked(O, D)
            .build(),
        // GPT-5.6 Sol is listed at a promotional $4 / $20 until 2026-11-30.
        // Its model page states that is a 20% input and 33% output reduction
        // from the standard price this row keeps, unconfirmed, and that
        // prompts over 272K tokens bill 2x input and 1.5x output. Its cache
        // rates keep the ratios the pricing page lists for the promotional
        // price (read 0.1x input, 12-hour write 2.5x input).
        PricingBuilder::new("openai", "gpt-5.6-sol", "GPT-5.6 Sol", 5.0, 30.0)
            .cache_read(0.50)
            .long_context(OPENAI_LONG_CONTEXT, 10.0, 45.0)
            .long_cache_read(1.0)
            .long_cache_write(25.0)
            .cache_write_ceiling(12.5)
            .unconfirmed(OPENAI_GPT_5_6_SOL, D)
            .build(),
        // The `gpt-5.6` alias routes to GPT-5.6 Sol.
        PricingBuilder::new("openai", "gpt-5.6", "GPT-5.6", 5.0, 30.0)
            .cache_read(0.50)
            .long_context(OPENAI_LONG_CONTEXT, 10.0, 45.0)
            .long_cache_read(1.0)
            .long_cache_write(25.0)
            .cache_write_ceiling(12.5)
            .unconfirmed(OPENAI_GPT_5_6_SOL, D)
            .build(),
        PricingBuilder::new("openai", "gpt-5.6-terra", "GPT-5.6 Terra", 2.0, 12.0)
            .cache_read(0.20)
            .batch(1.0, 6.0)
            .long_context(OPENAI_LONG_CONTEXT, 4.0, 18.0)
            .long_cache_read(0.4)
            .long_cache_write(10.0)
            .cache_write_ceiling(5.0)
            .checked(O, D)
            .build(),
        PricingBuilder::new("openai", "gpt-5.6-luna", "GPT-5.6 Luna", 0.20, 1.20)
            .cache_read(0.02)
            .batch(0.10, 0.60)
            .long_context(OPENAI_LONG_CONTEXT, 0.40, 1.80)
            .long_cache_read(0.04)
            .long_cache_write(1.0)
            .cache_write_ceiling(0.5)
            .checked(O, D)
            .build(),
        PricingBuilder::new("openai", "gpt-5.5", "GPT-5.5", 5.0, 30.0)
            .cache(0.0, 0.0, 0.50)
            .batch(2.50, 15.0)
            .long_context(OPENAI_LONG_CONTEXT, 10.0, 45.0)
            .long_cache_read(1.0)
            .checked(O, D)
            .build(),
        PricingBuilder::new("openai", "gpt-5.5-pro", "GPT-5.5 Pro", 30.0, 180.0)
            .batch(15.0, 90.0)
            .long_context(OPENAI_LONG_CONTEXT, 60.0, 270.0)
            .checked(O, D)
            .build(),
        PricingBuilder::new("openai", "gpt-5.4", "GPT-5.4", 2.50, 15.0)
            .cache(0.0, 0.0, 0.25)
            .batch(1.25, 7.50)
            .long_context(OPENAI_LONG_CONTEXT, 5.0, 22.50)
            .long_cache_read(0.5)
            .checked(O, D)
            .build(),
        PricingBuilder::new("openai", "gpt-5.4-pro", "GPT-5.4 Pro", 30.0, 180.0)
            .batch(15.0, 90.0)
            .long_context(OPENAI_LONG_CONTEXT, 60.0, 270.0)
            .checked(O, D)
            .build(),
        PricingBuilder::new("openai", "gpt-5.4-mini", "GPT-5.4 Mini", 0.75, 4.50)
            .cache(0.0, 0.0, 0.075)
            .batch(0.375, 2.25)
            .checked(O, D)
            .build(),
        PricingBuilder::new("openai", "gpt-5.4-nano", "GPT-5.4 Nano", 0.20, 1.25)
            .cache(0.0, 0.0, 0.02)
            .batch(0.10, 0.625)
            .deprecated()
            .checked(O, D)
            .build(),
        PricingBuilder::new("openai", "gpt-5.2", "GPT-5.2", 1.75, 14.0)
            .cache(0.0, 0.0, 0.175)
            .batch(0.875, 7.0)
            .checked(O, D)
            .build(),
        PricingBuilder::new("openai", "gpt-5.2-pro", "GPT-5.2 Pro", 21.0, 168.0)
            .batch(10.50, 84.0)
            .checked(O, D)
            .build(),
        PricingBuilder::new("openai", "gpt-5.1", "GPT-5.1", 1.25, 10.0)
            .cache(0.0, 0.0, 0.125)
            .batch(0.625, 5.0)
            .deprecated()
            .checked(O, D)
            .build(),
        PricingBuilder::new("openai", "gpt-5-pro", "GPT-5 Pro", 15.0, 120.0)
            .batch(7.50, 60.0)
            .deprecated()
            .checked(O, D)
            .build(),
        PricingBuilder::new("openai", "gpt-5-mini", "GPT-5 Mini", 0.25, 2.0)
            .cache(0.0, 0.0, 0.025)
            .batch(0.125, 1.0)
            .deprecated()
            .checked(O, D)
            .build(),
        PricingBuilder::new("openai", "gpt-5-nano", "GPT-5 Nano", 0.05, 0.40)
            .cache(0.0, 0.0, 0.005)
            .batch(0.025, 0.20)
            .deprecated()
            .checked(O, D)
            .build(),
        PricingBuilder::new("openai", "gpt-5", "GPT-5", 1.25, 10.0)
            .cache(0.0, 0.0, 0.125)
            .batch(0.625, 5.0)
            .deprecated()
            .checked(O, D)
            .build(),
        PricingBuilder::new("openai", "gpt-4.1", "GPT-4.1", 2.0, 8.0)
            .cache(0.0, 0.0, 0.50)
            .batch(1.0, 4.0)
            .deprecated()
            .checked(O, D)
            .build(),
        PricingBuilder::new("openai", "gpt-4.1-mini", "GPT-4.1 Mini", 0.40, 1.60)
            .cache(0.0, 0.0, 0.10)
            .batch(0.20, 0.80)
            .deprecated()
            .checked(O, D)
            .build(),
        PricingBuilder::new("openai", "gpt-4.1-nano", "GPT-4.1 Nano", 0.10, 0.40)
            .cache(0.0, 0.0, 0.025)
            .batch(0.025, 0.10)
            .deprecated()
            .unconfirmed(O, D)
            .build(),
        PricingBuilder::new("openai", "o3", "o3", 2.0, 8.0)
            .cache(0.0, 0.0, 0.50)
            .batch(1.0, 4.0)
            .deprecated()
            .checked(O, D)
            .build(),
        PricingBuilder::new("openai", "o3-pro", "o3 Pro", 20.0, 80.0)
            .batch(10.0, 40.0)
            .deprecated()
            .checked(O, D)
            .build(),
        PricingBuilder::new("openai", "o4-mini", "o4-mini", 1.10, 4.40)
            .cache(0.0, 0.0, 0.55)
            .batch(0.55, 2.20)
            .deprecated()
            .unconfirmed(O, D)
            .build(),
        PricingBuilder::new("openai", "o3-mini", "o3 Mini", 1.10, 4.40)
            .cache(0.0, 0.0, 0.55)
            .batch(0.55, 2.20)
            .deprecated()
            .unconfirmed(O, D)
            .build(),
        PricingBuilder::new("openai", "gpt-4o", "GPT-4o", 2.50, 10.0)
            .cache(0.0, 0.0, 1.25)
            .batch(1.25, 5.0)
            .deprecated()
            .checked(O, D)
            .build(),
        PricingBuilder::new("openai", "gpt-4o-mini", "GPT-4o Mini", 0.15, 0.60)
            .cache(0.0, 0.0, 0.075)
            .batch(0.075, 0.30)
            .deprecated()
            .checked(O, D)
            .build(),
    ]
}

fn xai_rows() -> Vec<CatalogEntry> {
    const X: &str = XAI_PRICING;
    const D: &str = CHECKED_2026_11_15;
    vec![
        PricingBuilder::new("xai", "grok-4.7", "Grok 4.7", 2.0, 6.0)
            .cache(0.0, 0.0, 0.50)
            .long_context(XAI_LONG_CONTEXT, 4.0, 12.0)
            .long_cache_read(1.0)
            .checked(X, D)
            .build(),
        PricingBuilder::new("xai", "grok-4.6", "Grok 4.6", 2.0, 6.0)
            .cache(0.0, 0.0, 0.50)
            .long_context(XAI_LONG_CONTEXT, 4.0, 12.0)
            .long_cache_read(1.0)
            .checked(X, D)
            .build(),
        PricingBuilder::new("xai", "grok-4.5", "Grok 4.5", 2.0, 6.0)
            .cache(0.0, 0.0, 0.30)
            .long_context(XAI_LONG_CONTEXT, 4.0, 12.0)
            .long_cache_read(0.6)
            .checked(X, D)
            .build(),
        PricingBuilder::new("xai", "grok-4.3", "Grok 4.3", 1.25, 2.50)
            .cache(0.0, 0.0, 0.20)
            .long_context(XAI_LONG_CONTEXT, 2.50, 5.0)
            .long_cache_read(0.4)
            .checked(X, D)
            .build(),
        // The pricing page lists only the dated 0309 variants of Grok 4.20.
        PricingBuilder::new("xai", "grok-4.20", "Grok 4.20", 1.25, 2.50)
            .cache(0.0, 0.0, 0.20)
            .unconfirmed(X, D)
            .build(),
        PricingBuilder::new(
            "xai",
            "grok-4.20-0309-reasoning",
            "Grok 4.20 Reasoning",
            1.25,
            2.50,
        )
        .cache(0.0, 0.0, 0.20)
        .long_context(XAI_LONG_CONTEXT, 2.50, 5.0)
        .long_cache_read(0.4)
        .checked(X, D)
        .build(),
        PricingBuilder::new(
            "xai",
            "grok-4.20-0309-non-reasoning",
            "Grok 4.20 Non-Reasoning",
            1.25,
            2.50,
        )
        .cache(0.0, 0.0, 0.20)
        .long_context(XAI_LONG_CONTEXT, 2.50, 5.0)
        .long_cache_read(0.4)
        .checked(X, D)
        .build(),
        PricingBuilder::new(
            "xai",
            "grok-4.20-multi-agent-0309",
            "Grok 4.20 Multi-Agent",
            1.25,
            2.50,
        )
        .cache(0.0, 0.0, 0.20)
        .long_context(XAI_LONG_CONTEXT, 2.50, 5.0)
        .long_cache_read(0.4)
        .checked(X, D)
        .build(),
        PricingBuilder::new("xai", "grok-build-0.1", "Grok Build 0.1", 1.0, 2.0)
            .cache(0.0, 0.0, 0.20)
            .long_context(XAI_LONG_CONTEXT, 2.0, 4.0)
            .long_cache_read(0.4)
            .checked(X, D)
            .build(),
        PricingBuilder::new(
            "xai",
            "grok-4-1-fast-reasoning",
            "Grok 4-1 Fast Reasoning",
            0.20,
            0.50,
        )
        .unconfirmed(X, D)
        .build(),
        PricingBuilder::new(
            "xai",
            "grok-4-1-fast-non-reasoning",
            "Grok 4-1 Fast Non-Reasoning",
            0.20,
            0.50,
        )
        .unconfirmed(X, D)
        .build(),
        PricingBuilder::new("xai", "grok-code-fast-1", "Grok Code Fast 1", 0.20, 1.50)
            .unconfirmed(X, D)
            .build(),
        PricingBuilder::new(
            "xai",
            "grok-4-fast-reasoning",
            "Grok 4 Fast Reasoning",
            0.20,
            0.50,
        )
        .unconfirmed(X, D)
        .build(),
        PricingBuilder::new(
            "xai",
            "grok-4-fast-non-reasoning",
            "Grok 4 Fast Non-Reasoning",
            0.20,
            0.50,
        )
        .unconfirmed(X, D)
        .build(),
        PricingBuilder::new("xai", "grok-4-0709", "Grok 4", 3.0, 15.0)
            .unconfirmed(X, D)
            .build(),
        PricingBuilder::new("xai", "grok-3", "Grok 3", 3.0, 15.0)
            .unconfirmed(X, D)
            .build(),
        PricingBuilder::new("xai", "grok-3-mini", "Grok 3 Mini", 0.30, 0.50)
            .unconfirmed(X, D)
            .build(),
    ]
}

fn gemini_rows() -> Vec<CatalogEntry> {
    const G: &str = GEMINI_PRICING;
    const D: &str = CHECKED_2026_11_15;
    vec![
        // Text input prices; audio input is billed higher on several models.
        // Gemini 4 Carbon and Argon rise to these prices on 2026-12-01, and
        // Gemini 3.8 Flash and 3.6 Flash on 2027-01-01.
        PricingBuilder::new("gemini", "gemini-4-carbon", "Gemini 4 Carbon", 4.0, 20.0)
            .cache(0.0, 0.0, 0.20)
            .batch(2.0, 10.0)
            .checked(G, D)
            .build(),
        PricingBuilder::new("gemini", "gemini-4-argon", "Gemini 4 Argon", 4.0, 20.0)
            .cache(0.0, 0.0, 0.20)
            .batch(2.0, 10.0)
            .checked(G, D)
            .build(),
        PricingBuilder::new(
            "gemini",
            "gemini-4-hydrogen",
            "Gemini 4 Hydrogen",
            1.50,
            7.50,
        )
        .cache(0.0, 0.0, 0.075)
        .batch(0.75, 3.75)
        .checked(G, D)
        .build(),
        PricingBuilder::new("gemini", "gemini-3.8-flash", "Gemini 3.8 Flash", 1.50, 7.50)
            .cache(0.0, 0.0, 0.15)
            .batch(0.75, 3.75)
            .checked(G, D)
            .build(),
        PricingBuilder::new("gemini", "gemini-3.6-flash", "Gemini 3.6 Flash", 1.50, 7.50)
            .cache(0.0, 0.0, 0.15)
            .batch(0.75, 3.75)
            .checked(G, D)
            .build(),
        PricingBuilder::new("gemini", "gemini-3.5-flash", "Gemini 3.5 Flash", 1.50, 9.0)
            .cache(0.0, 0.0, 0.15)
            .unconfirmed(G, D)
            .build(),
        PricingBuilder::new(
            "gemini",
            "gemini-3.5-flash-lite",
            "Gemini 3.5 Flash-Lite",
            0.30,
            2.50,
        )
        .cache(0.0, 0.0, 0.03)
        .batch(0.15, 1.25)
        .checked(G, D)
        .build(),
        // Listed as `gemini-3.1-pro-preview`, which this id prices by prefix.
        PricingBuilder::new("gemini", "gemini-3.1-pro", "Gemini 3.1 Pro", 2.0, 12.0)
            .cache(0.0, 0.0, 0.20)
            .batch(1.0, 6.0)
            .long_context(GEMINI_LONG_CONTEXT, 4.0, 18.0)
            .long_cache_read(0.4)
            .checked(G, D)
            .build(),
        PricingBuilder::new(
            "gemini",
            "gemini-3.1-flash-lite",
            "Gemini 3.1 Flash Lite",
            0.25,
            1.50,
        )
        .cache(0.0, 0.0, 0.025)
        .batch(0.125, 0.75)
        .checked(G, D)
        .build(),
        // Listed as `gemini-3-flash-preview`, which this id prices by prefix.
        PricingBuilder::new("gemini", "gemini-3-flash", "Gemini 3 Flash", 0.50, 3.0)
            .cache(0.0, 0.0, 0.05)
            .batch(0.25, 1.50)
            .checked(G, D)
            .build(),
        // Deprecated: from 2026-12-01 requests route to, and bill as,
        // Gemini 3.8 Flash, whose prices rise on 2027-01-01. Each price is the
        // dearer of the model's own ($1.25 in, $10 out, $0.125 cache hit,
        // $0.625 / $5 batch) and Gemini 3.8 Flash's from 2027-01-01 ($1.50,
        // $7.50, $0.15, $0.75 / $3.75), so no period is undercounted. Gemini
        // 3.8 Flash lists no long-context tier.
        PricingBuilder::new("gemini", "gemini-2.5-pro", "Gemini 2.5 Pro", 1.50, 10.0)
            .cache(0.0, 0.0, 0.15)
            .batch(0.75, 5.0)
            .long_context(GEMINI_LONG_CONTEXT, 2.50, 15.0)
            .long_cache_read(0.25)
            .deprecated()
            .checked(G, D)
            .build(),
        // Deprecated: from 2026-12-01 requests route to, and bill as,
        // Gemini 3.5 Flash-Lite.
        PricingBuilder::new("gemini", "gemini-2.5-flash", "Gemini 2.5 Flash", 0.30, 2.50)
            .cache(0.0, 0.0, 0.03)
            .batch(0.15, 1.25)
            .deprecated()
            .checked(G, D)
            .build(),
        PricingBuilder::new(
            "gemini",
            "gemini-2.5-flash-lite",
            "Gemini 2.5 Flash Lite",
            0.10,
            0.40,
        )
        .cache(0.0, 0.0, 0.01)
        .batch(0.05, 0.20)
        .checked(G, D)
        .build(),
        PricingBuilder::new("gemini", "gemini-2-flash", "Gemini 2 Flash", 0.10, 0.40)
            .cache(0.0, 0.0, 0.025)
            .unconfirmed(G, D)
            .build(),
        PricingBuilder::new(
            "gemini",
            "gemini-2-flash-lite",
            "Gemini 2 Flash Lite",
            0.075,
            0.30,
        )
        .cache(0.0, 0.0, 0.01875)
        .unconfirmed(G, D)
        .build(),
    ]
}

fn openrouter_rows() -> Vec<CatalogEntry> {
    const R: &str = OPENROUTER_MODELS;
    const D: &str = CHECKED_2026_11_15;
    vec![
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
        .checked(R, D)
        .build(),
        PricingBuilder::new(
            "openrouter",
            "openai/gpt-4o-mini",
            "GPT-4o Mini (via OpenRouter)",
            0.15,
            0.60,
        )
        .checked(R, D)
        .build(),
        PricingBuilder::new(
            "openrouter",
            "anthropic/claude-opus-5.5",
            "Claude Opus 5.5 (via OpenRouter)",
            4.0,
            20.0,
        )
        .checked(R, D)
        .build(),
        PricingBuilder::new(
            "openrouter",
            "anthropic/claude-sonnet-5.5",
            "Claude Sonnet 5.5 (via OpenRouter)",
            2.0,
            10.0,
        )
        .checked(R, D)
        .build(),
        PricingBuilder::new(
            "openrouter",
            "anthropic/claude-sonnet-5",
            "Claude Sonnet 5 (via OpenRouter)",
            2.0,
            10.0,
        )
        .checked(R, D)
        .build(),
        PricingBuilder::new(
            "openrouter",
            "meta-llama/llama-3.3-70b-instruct",
            "Llama 3.3 70B (via OpenRouter)",
            0.71,
            0.71,
        )
        .checked(R, D)
        .build(),
        PricingBuilder::new(
            "openrouter",
            "deepseek/deepseek-chat",
            "DeepSeek Chat (via OpenRouter)",
            0.2574,
            1.0287,
        )
        .checked(R, D)
        .build(),
    ]
}

// ============================================================================
// Cost estimation
// ============================================================================

/// Per-token rates one request is billed at.
#[derive(Debug, Clone, Copy, PartialEq)]
struct TierRates {
    input: f64,
    output: f64,
    cache_read: f64,
    cache_write: f64,
}

impl TierRates {
    /// The dearer of `self` and `other` for every kind of token.
    fn max(self, other: TierRates) -> TierRates {
        TierRates {
            input: self.input.max(other.input),
            output: self.output.max(other.output),
            cache_read: self.cache_read.max(other.cache_read),
            cache_write: self.cache_write.max(other.cache_write),
        }
    }

    /// No input token, cached or written, bills below ordinary input.
    fn at_least_input(self) -> TierRates {
        TierRates {
            cache_read: self.cache_read.max(self.input),
            cache_write: self.cache_write.max(self.input),
            ..self
        }
    }
}

/// Per-token rates for one model: its base tier and, when the provider bills
/// long prompts higher, the threshold and long-context tier.
#[derive(Debug, Clone, Copy, PartialEq)]
struct Rates {
    base: TierRates,
    long_context: Option<(i64, TierRates)>,
}

impl Rates {
    fn from_entry(entry: &CatalogEntry) -> Self {
        let pricing = &entry.pricing;
        let input = pricing.input_per_million;
        // A written token always bills at least as ordinary input; the
        // dearest listed write tier applies because a request's usage does
        // not say which tier it wrote to.
        let cache_write = [
            pricing.cache_write_5m_per_million,
            pricing.cache_write_1h_per_million,
            entry.cache_write_ceiling,
        ]
        .into_iter()
        .flatten()
        .fold(input, f64::max);
        let base = TierRates {
            input,
            output: pricing.output_per_million,
            cache_read: pricing.cache_hit_per_million.unwrap_or(input),
            cache_write,
        };
        let long_context = entry.long_context.map(|long| {
            (
                long.above_input_tokens,
                TierRates {
                    input: long.input,
                    output: long.output,
                    cache_read: long.cache_read.unwrap_or(long.input),
                    cache_write: long.cache_write.unwrap_or(long.input).max(long.input),
                },
            )
        });
        Rates { base, long_context }
    }

    /// The rates a request with `input_tokens` is billed at.
    fn for_request(&self, input_tokens: i64) -> TierRates {
        match self.long_context {
            Some((above, long)) if input_tokens > above => long,
            _ => self.base,
        }
    }
}

/// Per-token rates derived from the price list above. Because the list is in
/// USD per 1M tokens and usage costs are stored in microcents (1e-6 USD, as
/// `estimated_cost_microcents` documents), a rate is directly the cost of one
/// token in that unit: `$2.50 / 1M tokens` is `2.5` microcents per token.
struct RateTable {
    by_model: std::collections::HashMap<String, Rates>,
    /// Rates charged for models that are not priced, so an unlisted model can
    /// never be a way around a spend cap: for any prompt length, no confirmed
    /// row bills a token above them. The base tier is the dearest confirmed
    /// base rate of each kind; above the lowest confirmed long-context
    /// threshold, the long tier is the dearest confirmed rate of each kind
    /// either tier lists. Every input token, cached or written, bills at no
    /// less than the input rate. Rows whose prices are unconfirmed do not set
    /// it.
    worst_case: Rates,
}

impl RateTable {
    fn from_entries(entries: Vec<CatalogEntry>) -> Self {
        let zero = TierRates {
            input: 0.0,
            output: 0.0,
            cache_read: 0.0,
            cache_write: 0.0,
        };
        let mut by_model = std::collections::HashMap::new();
        let mut base = zero;
        let mut long: Option<(i64, TierRates)> = None;
        for entry in entries {
            let rates = Rates::from_entry(&entry);
            if entry.provenance.is_some_and(|p| p.confirmed) {
                base = base.max(rates.base);
                if let Some((above, tier)) = rates.long_context {
                    long = Some(match long {
                        Some((lowest, worst)) => (lowest.min(above), worst.max(tier)),
                        None => (above, tier),
                    });
                }
            }
            by_model.insert(entry.pricing.model.clone(), rates);
        }
        let base = base.at_least_input();
        let worst_case = Rates {
            base,
            long_context: long.map(|(above, tier)| (above, tier.max(base).at_least_input())),
        };
        RateTable {
            by_model,
            worst_case,
        }
    }

    /// Rates for `model`: its exact id, or else the longest listed id it
    /// extends at a `-`, `:` or `@` boundary. `None` when it is not listed.
    fn rates_for(&self, model: &str) -> Option<Rates> {
        self.by_model.get(model).copied().or_else(|| {
            self.by_model
                .iter()
                .filter(|(id, _)| {
                    model.strip_prefix(id.as_str()).is_some_and(|rest| {
                        rest.starts_with('-') || rest.starts_with(':') || rest.starts_with('@')
                    })
                })
                .max_by_key(|(id, _)| id.len())
                .map(|(_, rates)| *rates)
        })
    }
}

/// Cost of one request at `tier`, in microcents, rounded up.
fn cost_at(
    tier: TierRates,
    input_tokens: i64,
    output_tokens: i64,
    cache: PromptTokensDetails,
) -> i64 {
    let input_tokens = input_tokens.max(0);
    let read = cache.cached_tokens.clamp(0, input_tokens);
    let write = cache.cache_write_tokens.clamp(0, input_tokens - read);
    let fresh = input_tokens - read - write;
    let cost = fresh as f64 * tier.input
        + read as f64 * tier.cache_read
        + write as f64 * tier.cache_write
        + output_tokens.max(0) as f64 * tier.output;
    // Saturate rather than wrap on an absurd product.
    cost.ceil().clamp(0.0, i64::MAX as f64) as i64
}

fn rate_table() -> &'static RateTable {
    static TABLE: std::sync::OnceLock<RateTable> = std::sync::OnceLock::new();
    TABLE.get_or_init(|| RateTable::from_entries(catalog()))
}

/// Estimate the provider cost of a request in microcents (1e-6 USD).
///
/// Exact model ids win; otherwise the longest listed id the model extends at a
/// `-`, `:` or `@` boundary applies (so a dated snapshot `gpt-4o-2025-01-01`
/// is priced as `gpt-4o`). A model whose provider bills long prompts at a
/// higher rate is charged that tier for the whole request once its input
/// crosses the threshold. `input_tokens` is the whole prompt; the parts read
/// from and written to the prompt cache (`cache`) are carved out of it and
/// billed at the cache-read rate and at the dearest cache-write tier. Unlisted
/// models are charged the worst case (see `RateTable::worst_case`), including
/// its long-context tier. This is an estimate from list prices that ignores batch
/// discounts, so it errs on the high side.
pub fn estimate_cost_microcents(
    model: &str,
    input_tokens: i64,
    output_tokens: i64,
    cache: PromptTokensDetails,
) -> i64 {
    let table = rate_table();
    let rates = table.rates_for(model).unwrap_or(table.worst_case);
    cost_at(
        rates.for_request(input_tokens.max(0)),
        input_tokens,
        output_tokens,
        cache,
    )
}

/// The listed long-context cost of a request whose prompt crosses its model's
/// long-context threshold; `None` when the model is not listed, has no long
/// tier, or the prompt is below it.
///
/// A synced model catalogue publishes one input and one output price and no
/// tier, so a cost computed from it bills a long prompt at the base rate. The
/// usage service records the higher of that figure and this one, so a long
/// prompt to a tiered model is never recorded at the base rate.
pub fn long_context_cost_microcents(
    model: &str,
    input_tokens: i64,
    output_tokens: i64,
    cache: PromptTokensDetails,
) -> Option<i64> {
    let (above, tier) = rate_table().rates_for(model)?.long_context?;
    (input_tokens > above).then(|| cost_at(tier, input_tokens, output_tokens, cache))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A request with no prompt-cache activity.
    fn estimate(model: &str, input_tokens: i64, output_tokens: i64) -> i64 {
        estimate_cost_microcents(
            model,
            input_tokens,
            output_tokens,
            PromptTokensDetails::default(),
        )
    }

    #[test]
    fn estimate_cost_uses_listed_rates_in_microcents() {
        // gpt-4o: $2.50 in / $10 out per 1M tokens => 2.5 and 10 microcents per token.
        assert_eq!(estimate("gpt-4o", 1_000, 100), 2_500 + 1_000);
    }

    #[test]
    fn estimate_cost_prices_dated_snapshots_as_their_base_model() {
        assert_eq!(
            estimate("gpt-4o-2025-01-01", 1_000, 100),
            estimate("gpt-4o", 1_000, 100)
        );
        // A longer listed id must win over a shorter prefix of it.
        assert_eq!(
            estimate("gpt-4o-mini-2025-01-01", 1_000, 0),
            estimate("gpt-4o-mini", 1_000, 0)
        );
    }

    #[test]
    fn estimate_cost_charges_unlisted_models_at_worst_case() {
        let unlisted = estimate("totally-unlisted-model", 1_000, 1_000);
        let cheapest = estimate("gpt-4o-mini", 1_000, 1_000);
        assert!(unlisted > cheapest);
        let worst = rate_table().worst_case.base;
        assert_eq!(
            unlisted,
            (1_000.0 * worst.input + 1_000.0 * worst.output).ceil() as i64
        );
    }

    #[test]
    fn estimate_cost_is_zero_for_no_tokens_and_ignores_negatives() {
        assert_eq!(estimate("gpt-4o", 0, 0), 0);
        assert_eq!(estimate("gpt-4o", -5, -5), 0);
    }

    fn listed(model: &str) -> ModelPricing {
        build_pricing()
            .into_iter()
            .find(|row| row.model == model)
            .unwrap_or_else(|| panic!("{model} is listed"))
    }

    /// Why `entry` fails the provenance rule, or `None` when it passes: it
    /// must name a page on its provider's own site and a `YYYY-MM-DD` date.
    fn provenance_problem(entry: &CatalogEntry) -> Option<String> {
        let official_hosts = [
            ("anthropic", "https://platform.claude.com/"),
            ("openai", "https://developers.openai.com/"),
            ("xai", "https://docs.x.ai/"),
            ("gemini", "https://aistudio.google.com/"),
            ("openrouter", "https://openrouter.ai/"),
        ];
        let model = &entry.pricing.model;
        let Some(provenance) = entry.provenance else {
            return Some(format!("{model} has no source or checked date"));
        };
        let Some((_, host)) = official_hosts
            .iter()
            .find(|(provider, _)| *provider == entry.pricing.provider)
        else {
            return Some(format!("{model}: no official host for its provider"));
        };
        if !provenance.source.starts_with(host) {
            return Some(format!(
                "{model}: source {} is not on the provider's own site {host}",
                provenance.source
            ));
        }
        if chrono::NaiveDate::parse_from_str(provenance.checked_on, "%Y-%m-%d").is_err() {
            return Some(format!(
                "{model}: checked date {:?} is not YYYY-MM-DD",
                provenance.checked_on
            ));
        }
        None
    }

    #[test]
    fn every_row_records_its_source_and_the_date_it_was_checked() {
        let problems: Vec<String> = catalog().iter().filter_map(provenance_problem).collect();
        assert!(problems.is_empty(), "{problems:#?}");
    }

    #[test]
    fn the_provenance_check_rejects_a_missing_source_date_or_foreign_host() {
        let missing = PricingBuilder::new("openai", "m", "M", 1.0, 2.0).build();
        assert!(provenance_problem(&missing).is_some());
        let bad_date = PricingBuilder::new("openai", "m", "M", 1.0, 2.0)
            .checked(OPENAI_PRICING, "15 Nov 2026")
            .build();
        assert!(provenance_problem(&bad_date).is_some());
        let foreign = PricingBuilder::new("openai", "m", "M", 1.0, 2.0)
            .checked("https://example.com/prices", CHECKED_2026_11_15)
            .build();
        assert!(provenance_problem(&foreign).is_some());
        let good = PricingBuilder::new("openai", "m", "M", 1.0, 2.0)
            .checked(OPENAI_PRICING, CHECKED_2026_11_15)
            .build();
        assert_eq!(provenance_problem(&good), None);
    }

    #[test]
    fn model_ids_are_unique() {
        let mut seen = std::collections::HashSet::new();
        for row in build_pricing() {
            assert!(
                seen.insert(row.model.clone()),
                "{} is listed twice",
                row.model
            );
        }
    }

    #[test]
    fn refreshed_rows_carry_the_provider_list_prices() {
        // (model, input, output, cache hit, batch input, batch output)
        type Expected = (
            &'static str,
            f64,
            f64,
            Option<f64>,
            Option<f64>,
            Option<f64>,
        );
        let expected: [Expected; 8] = [
            ("o3", 2.0, 8.0, Some(0.50), Some(1.0), Some(4.0)),
            ("gpt-5.4", 2.50, 15.0, Some(0.25), Some(1.25), Some(7.50)),
            ("gpt-5.4-pro", 30.0, 180.0, None, Some(15.0), Some(90.0)),
            (
                "gpt-5.6-luna",
                0.20,
                1.20,
                Some(0.02),
                Some(0.10),
                Some(0.60),
            ),
            (
                "gemini-2.5-flash",
                0.30,
                2.50,
                Some(0.03),
                Some(0.15),
                Some(1.25),
            ),
            (
                "gemini-3.1-pro",
                2.0,
                12.0,
                Some(0.20),
                Some(1.0),
                Some(6.0),
            ),
            (
                "claude-opus-5-5",
                4.0,
                20.0,
                Some(0.20),
                Some(2.0),
                Some(10.0),
            ),
            ("gpt-6-luna", 0.10, 0.50, Some(0.01), Some(0.05), Some(0.25)),
        ];
        for (model, input, output, hit, batch_in, batch_out) in expected {
            let row = listed(model);
            assert_eq!(row.input_per_million, input, "{model} input");
            assert_eq!(row.output_per_million, output, "{model} output");
            assert_eq!(row.cache_hit_per_million, hit, "{model} cache hit");
            assert_eq!(row.batch_input_per_million, batch_in, "{model} batch in");
            assert_eq!(row.batch_output_per_million, batch_out, "{model} batch out");
        }
    }

    #[test]
    fn claude_5_5_and_gpt_6_models_are_priced_at_their_own_rates() {
        // Before these rows existed, Claude Haiku 5.5 and the GPT-6 family
        // fell through to the worst-case rate, and Claude Opus 5.5 and
        // Sonnet 5.5 were priced as Opus 5 and Sonnet 5 by prefix.
        let worst = estimate("totally-unlisted-model", 1_000, 1_000);
        for (model, input, output) in [
            ("claude-haiku-5-5", 0.10_f64, 0.50_f64),
            ("claude-opus-5-5", 4.0, 20.0),
            ("claude-sonnet-5-5", 2.0, 10.0),
            ("gpt-6-astra", 10.0, 50.0),
            ("gpt-6-luna", 0.10, 0.50),
            ("gpt-6.1-sol", 2.0, 10.0),
        ] {
            let cost = estimate(model, 1_000, 1_000);
            assert_eq!(
                cost,
                (1_000.0 * input + 1_000.0 * output).ceil() as i64,
                "{model}"
            );
            assert!(cost < worst, "{model} is charged below the worst case");
        }
    }

    #[test]
    fn long_prompts_are_billed_at_the_long_context_rate_for_the_whole_request() {
        // Claude Haiku 5.5: $0.10 / $0.50 up to 100,000 prompt tokens,
        // $0.50 / $2.50 over.
        assert_eq!(
            estimate("claude-haiku-5-5", 100_000, 1_000),
            (100_000.0 * 0.10 + 1_000.0 * 0.50_f64).ceil() as i64
        );
        assert_eq!(
            estimate("claude-haiku-5-5", 100_001, 1_000),
            (100_001.0 * 0.50 + 1_000.0 * 2.50_f64).ceil() as i64
        );
        // OpenAI: over 272K input tokens.
        assert_eq!(
            estimate("gpt-5.4", 272_000, 0),
            (272_000.0 * 2.50_f64).ceil() as i64
        );
        assert_eq!(
            estimate("gpt-5.4", 272_001, 10),
            (272_001.0 * 5.0 + 10.0 * 22.50_f64).ceil() as i64
        );
        // xAI: at 200K prompt tokens and above.
        assert_eq!(
            estimate("grok-4.5", 199_999, 0),
            (199_999.0 * 2.0_f64).ceil() as i64
        );
        assert_eq!(
            estimate("grok-4.5", 200_000, 0),
            (200_000.0 * 4.0_f64).ceil() as i64
        );
        // Gemini: over 200K prompt tokens.
        assert_eq!(
            estimate("gemini-3.1-pro-preview", 200_001, 0),
            (200_001.0 * 4.0_f64).ceil() as i64
        );
    }

    #[test]
    fn a_dated_snapshot_keeps_its_base_model_long_context_rate() {
        assert_eq!(
            estimate("gpt-5.4-2026-03-05", 300_000, 0),
            estimate("gpt-5.4", 300_000, 0)
        );
    }

    #[test]
    fn worst_case_is_the_most_expensive_confirmed_row() {
        let table = rate_table();
        let confirmed: Vec<_> = catalog()
            .into_iter()
            .filter(|entry| entry.provenance.is_some_and(|p| p.confirmed))
            .map(|entry| entry.pricing)
            .collect();
        let max_input = confirmed
            .iter()
            .map(|row| row.input_per_million)
            .fold(0.0, f64::max);
        let max_output = confirmed
            .iter()
            .map(|row| row.output_per_million)
            .fold(0.0, f64::max);
        let base = table.worst_case.base;
        assert_eq!((base.input, base.output), (max_input, max_output));
        // GPT-5.4 Pro and GPT-5.5 Pro, at $30 / $180, are the dearest today.
        assert_eq!((base.input, base.output), (30.0, 180.0));
        // Their long-context tier, $60 / $270, is the dearest long tier, and
        // it starts at the lowest confirmed threshold (Claude Haiku 5.5's).
        let (above, long) = table.worst_case.long_context.expect("a long tier");
        assert_eq!(above, ANTHROPIC_HAIKU_LONG_CONTEXT);
        assert_eq!((long.input, long.output), (60.0, 270.0));
    }

    #[test]
    fn no_confirmed_row_bills_a_request_above_the_worst_case() {
        let table = rate_table();
        let cache = |read, write| PromptTokensDetails {
            cached_tokens: read,
            cache_write_tokens: write,
        };
        for entry in catalog()
            .into_iter()
            .filter(|entry| entry.provenance.is_some_and(|p| p.confirmed))
        {
            let rates = Rates::from_entry(&entry);
            for input in [1_000, 150_000, 250_000, 300_000, 2_000_000] {
                for details in [cache(0, 0), cache(input / 2, 0), cache(0, input / 2)] {
                    let own = cost_at(rates.for_request(input), input, 1_000, details);
                    let worst = cost_at(table.worst_case.for_request(input), input, 1_000, details);
                    assert!(
                        own <= worst,
                        "{} bills {own} for {input} input tokens, above the worst case {worst}",
                        entry.pricing.model
                    );
                }
            }
        }
    }

    #[test]
    fn unlisted_models_bill_long_prompts_at_the_worst_long_context_rate() {
        // 300,000 input and 1,000 output tokens at $60 / $270 per 1M, the
        // dearest confirmed long tier (GPT-5.4 Pro's), not the $30 / $180 base.
        assert_eq!(
            estimate("totally-unlisted-model", 300_000, 1_000),
            300_000 * 60 + 1_000 * 270
        );
        assert!(
            estimate("totally-unlisted-model", 300_000, 1_000)
                >= estimate("gpt-5.4-pro", 300_000, 1_000)
        );
    }

    #[test]
    fn gemini_2_5_pro_never_bills_below_the_model_it_is_routed_to() {
        // From 2026-12-01 its requests bill as Gemini 3.8 Flash, which rises to
        // $1.50 input on 2027-01-01.
        let own = Rates::from_entry(
            &catalog()
                .into_iter()
                .find(|entry| entry.pricing.model == "gemini-2.5-pro")
                .expect("gemini-2.5-pro is listed"),
        );
        let successor = Rates::from_entry(
            &catalog()
                .into_iter()
                .find(|entry| entry.pricing.model == "gemini-3.8-flash")
                .expect("gemini-3.8-flash is listed"),
        );
        assert_eq!(own.base.input, 1.50);
        assert_eq!(own.base.max(successor.base), own.base);
    }

    #[test]
    fn long_context_cost_is_given_only_for_a_listed_long_tier_the_prompt_crosses() {
        let none = PromptTokensDetails::default();
        // gpt-5.4 bills $5 / $22.50 per 1M above 272,000 input tokens.
        assert_eq!(
            long_context_cost_microcents("gpt-5.4", 300_000, 0, none),
            Some(300_000 * 5)
        );
        assert_eq!(
            long_context_cost_microcents("gpt-5.4-2026-03-05", 300_000, 0, none),
            Some(300_000 * 5)
        );
        assert_eq!(
            long_context_cost_microcents("gpt-5.4", 272_000, 0, none),
            None
        );
        // gpt-4o has no long tier; an unlisted model has no listed tier.
        assert_eq!(
            long_context_cost_microcents("gpt-4o", 2_000_000, 0, none),
            None
        );
        assert_eq!(
            long_context_cost_microcents("totally-unlisted-model", 2_000_000, 0, none),
            None
        );
    }

    #[test]
    fn unconfirmed_rows_do_not_raise_the_worst_case() {
        let table = RateTable::from_entries(vec![
            PricingBuilder::new("openai", "cheap", "Cheap", 1.0, 2.0)
                .checked(OPENAI_PRICING, CHECKED_2026_11_15)
                .build(),
            PricingBuilder::new("openai", "stale", "Stale", 99.0, 999.0)
                .unconfirmed(OPENAI_PRICING, CHECKED_2026_11_15)
                .build(),
        ]);
        let base = table.worst_case.base;
        assert_eq!((base.input, base.output), (1.0, 2.0));
        // The unconfirmed row still prices its own model.
        assert_eq!(table.by_model["stale"].for_request(1).input, 99.0);
    }

    fn with_cache(model: &str, input: i64, output: i64, read: i64, write: i64) -> i64 {
        estimate_cost_microcents(
            model,
            input,
            output,
            PromptTokensDetails {
                cached_tokens: read,
                cache_write_tokens: write,
            },
        )
    }

    #[test]
    fn cache_writes_bill_at_the_dearest_write_tier() {
        // Claude Haiku 5.5: input $0.10, 5-minute write $0.125, 1-hour
        // write $0.20. Usage does not say which tier was written, so 1-hour.
        assert_eq!(with_cache("claude-haiku-5-5", 50_000, 0, 0, 50_000), 10_000);
        // Claude Opus 5.5: 10,000 fresh at $4 + 40,000 written at $8.
        assert_eq!(
            with_cache("claude-opus-5-5", 50_000, 0, 0, 40_000),
            10_000 * 4 + 40_000 * 8
        );
        // GPT-6 Astra: 12-hour write $25, above its $10 input.
        assert_eq!(with_cache("gpt-6-astra", 1_000, 0, 0, 1_000), 25_000);
    }

    #[test]
    fn a_free_write_tier_still_bills_the_token_as_input() {
        // GPT-4o lists no write surcharge ($0), so a written token is input.
        assert_eq!(
            with_cache("gpt-4o", 1_000, 0, 0, 1_000),
            estimate("gpt-4o", 1_000, 0)
        );
    }

    #[test]
    fn cache_reads_bill_at_the_cache_hit_rate() {
        // Claude Sonnet 5.5: 1,000 fresh at $2 + 9,000 read at $0.10.
        assert_eq!(
            with_cache("claude-sonnet-5-5", 10_000, 0, 9_000, 0),
            1_000 * 2 + 900
        );
        // A row with no cache-hit price bills reads as input.
        assert_eq!(
            with_cache("gpt-5.4-pro", 1_000, 0, 1_000, 0),
            estimate("gpt-5.4-pro", 1_000, 0)
        );
    }

    #[test]
    fn long_prompts_bill_cache_activity_at_long_context_rates() {
        // Claude Haiku 5.5 over 100,000 tokens: input $0.50, read $0.05,
        // 1-hour write $1.
        assert_eq!(
            with_cache("claude-haiku-5-5", 200_000, 0, 100_000, 50_000),
            (50_000.0 * 0.50 + 100_000.0 * 0.05 + 50_000.0 * 1.0_f64).ceil() as i64
        );
        // GPT-5.4 over 272K: read $0.50; no write surcharge, so input ($5).
        assert_eq!(
            with_cache("gpt-5.4", 300_000, 0, 200_000, 100_000),
            (200_000.0 * 0.50 + 100_000.0 * 5.0_f64).ceil() as i64
        );
    }

    #[test]
    fn unlisted_models_bill_cached_input_at_the_worst_case_input_rate() {
        let input = rate_table().worst_case.base.input;
        assert_eq!(
            with_cache("totally-unlisted-model", 1_000, 0, 600, 400),
            (1_000.0 * input).ceil() as i64
        );
    }

    #[test]
    fn cache_counts_are_clamped_to_the_prompt() {
        // More cached and written tokens than the prompt holds, or negative
        // counts, never bill beyond the prompt or below zero.
        assert_eq!(
            with_cache("claude-haiku-5-5", 1_000, 0, 5_000, 5_000),
            with_cache("claude-haiku-5-5", 1_000, 0, 1_000, 0)
        );
        assert_eq!(
            with_cache("claude-haiku-5-5", 1_000, 0, -5, -5),
            estimate("claude-haiku-5-5", 1_000, 0)
        );
    }

    #[test]
    fn long_cache_rates_are_set_only_on_rows_with_a_long_context_tier() {
        for entry in catalog() {
            if let Some(long) = entry.long_context {
                for rate in [long.cache_read, long.cache_write].into_iter().flatten() {
                    assert!(rate > 0.0, "{}", entry.pricing.model);
                }
            }
        }
        // `long_cache_read` before `long_context` has nothing to attach to.
        let entry = PricingBuilder::new("openai", "m", "M", 1.0, 2.0)
            .long_cache_read(0.5)
            .build();
        assert!(entry.long_context.is_none());
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
        for entry in catalog() {
            let model = &entry.pricing;
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
            if let Some(long) = entry.long_context {
                assert!(
                    long.input >= model.input_per_million
                        && long.output >= model.output_per_million,
                    "Long-context rates for {} must not be below its base rates",
                    model.model
                );
                assert!(long.above_input_tokens > 0, "{} threshold", model.model);
            }
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
    fn cache_read_only_rows_leave_cache_writes_unset() {
        let row = listed("gpt-6-astra");
        assert_eq!(row.cache_hit_per_million, Some(1.0));
        assert!(row.cache_write_5m_per_million.is_none());
        assert!(row.cache_write_1h_per_million.is_none());
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
            .build()
            .pricing;
        let json = serde_json::to_string(&model).unwrap();
        assert!(json.contains("\"deprecated\":true"));
    }

    #[test]
    fn test_pricing_builder_defaults() {
        let entry = PricingBuilder::new("test", "m", "M", 1.0, 2.0).build();
        let model = entry.pricing;
        assert!(model.cache_write_5m_per_million.is_none());
        assert!(model.cache_write_1h_per_million.is_none());
        assert!(model.cache_hit_per_million.is_none());
        assert!(model.batch_input_per_million.is_none());
        assert!(model.batch_output_per_million.is_none());
        assert!(!model.deprecated);
        assert!(entry.provenance.is_none());
        assert!(entry.long_context.is_none());
    }
}
