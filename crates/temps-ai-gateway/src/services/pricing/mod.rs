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
//!
//! A running instance can layer a runtime model catalog over this table
//! (ADR 112, [`install_catalog_prices`]): its prices replace a listed model's
//! base rates and price models this release does not know. The bundled table
//! stays the fallback, keeps every long-context tier, and alone decides the
//! worst-case rate charged for models no price covers.

use std::collections::{BTreeMap, HashMap};
use std::sync::{Arc, OnceLock};

use arc_swap::ArcSwap;
use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

use crate::types::PromptTokensDetails;

/// Where a model's price comes from.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum PriceSource {
    /// The price table bundled with this Temps release.
    #[default]
    Bundled,
    /// The runtime model catalog (Settings: AI Gateway > Models).
    Catalog,
}

/// Pricing for a single model, all values in USD per 1M tokens.
/// Fields are optional because not every provider supports every pricing tier.
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct ModelPricing {
    /// Model identifier (e.g. "gpt-6.2-sol", "claude-sonnet-5-5")
    pub model: String,
    /// Human-readable model name (e.g. "Claude Sonnet 5.5")
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
    /// Whether the price is the bundled one or comes from the runtime catalog.
    pub source: PriceSource,
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
const CHECKED_2026_11_23: &str = "2026-11-23";

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
                source: PriceSource::Bundled,
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
/// OpenRouter lists a route's long-context rates as an override from
/// `min_prompt_tokens` on (inclusive): 100,000 for Claude Haiku 5.5,
/// 200,000 for Grok 4.6 and later, and 272,000 for GPT-6.x.
const OPENROUTER_HAIKU_LONG_CONTEXT: i64 = 100_000 - 1;
const OPENROUTER_XAI_LONG_CONTEXT: i64 = 200_000 - 1;
const OPENROUTER_OPENAI_LONG_CONTEXT: i64 = 272_000 - 1;

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
    const D: &str = CHECKED_2026_11_23;
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
        // Claude Mythos models are available to verified organizations only,
        // at the prices of their Fable counterparts.
        PricingBuilder::new(
            "anthropic",
            "claude-mythos-5-2",
            "Claude Mythos 5.2",
            8.0,
            40.0,
        )
        .cache(10.0, 16.0, 0.20)
        .batch(4.0, 20.0)
        .checked(A, D)
        .build(),
        PricingBuilder::new(
            "anthropic",
            "claude-mythos-5-1",
            "Claude Mythos 5.1",
            10.0,
            50.0,
        )
        .cache(12.50, 20.0, 0.25)
        .batch(5.0, 25.0)
        .checked(A, D)
        .build(),
        PricingBuilder::new(
            "anthropic",
            "claude-mythos-5",
            "Claude Mythos 5",
            10.0,
            50.0,
        )
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
        // Retired on the Claude API but still listed for the cloud platforms
        // that serve them; kept so usage recorded on them still prices.
        PricingBuilder::new(
            "anthropic",
            "claude-opus-4-1",
            "Claude Opus 4.1",
            15.0,
            75.0,
        )
        .cache(18.75, 30.0, 1.50)
        .batch(7.50, 37.50)
        .deprecated()
        .checked(A, D)
        .build(),
        PricingBuilder::new("anthropic", "claude-opus-4", "Claude Opus 4", 15.0, 75.0)
            .cache(18.75, 30.0, 1.50)
            .batch(7.50, 37.50)
            .deprecated()
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
        // Retired on the Claude API; dated ids such as
        // `claude-sonnet-4-20250514` price as this row by prefix.
        PricingBuilder::new("anthropic", "claude-sonnet-4", "Claude Sonnet 4", 3.0, 15.0)
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
    const D: &str = CHECKED_2026_11_23;
    vec![
        // Models marked deprecated are "Deprecated" or "Shut down" on their
        // model pages under https://developers.openai.com/api/docs/models.
        PricingBuilder::new("openai", "gpt-6.2-astra", "GPT-6.2 Astra", 8.0, 40.0)
            .cache_read(0.40)
            .batch(4.0, 20.0)
            .long_context(OPENAI_LONG_CONTEXT, 16.0, 60.0)
            .long_cache_read(0.80)
            .long_cache_write(40.0)
            .cache_write_ceiling(20.0)
            .checked(O, D)
            .build(),
        PricingBuilder::new("openai", "gpt-6.2-sol", "GPT-6.2 Sol", 2.0, 10.0)
            .cache_read(0.10)
            .batch(1.0, 5.0)
            .long_context(OPENAI_LONG_CONTEXT, 4.0, 15.0)
            .long_cache_read(0.2)
            .long_cache_write(10.0)
            .cache_write_ceiling(5.0)
            .checked(O, D)
            .build(),
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
    const D: &str = CHECKED_2026_11_23;
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
    const D: &str = CHECKED_2026_11_23;
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
        PricingBuilder::new("gemini", "gemini-4-neon", "Gemini 4 Neon", 3.0, 15.0)
            .cache(0.0, 0.0, 0.15)
            .batch(1.50, 7.50)
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
    const D: &str = CHECKED_2026_11_23;
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
        .cache(5.0, 8.0, 0.20)
        .checked(R, D)
        .build(),
        PricingBuilder::new(
            "openrouter",
            "anthropic/claude-sonnet-5.5",
            "Claude Sonnet 5.5 (via OpenRouter)",
            2.0,
            10.0,
        )
        .cache(2.50, 4.0, 0.10)
        .checked(R, D)
        .build(),
        PricingBuilder::new(
            "openrouter",
            "anthropic/claude-sonnet-5",
            "Claude Sonnet 5 (via OpenRouter)",
            2.0,
            10.0,
        )
        .cache(2.50, 4.0, 0.20)
        .checked(R, D)
        .build(),
        PricingBuilder::new(
            "openrouter",
            "anthropic/claude-fable-5.2",
            "Claude Fable 5.2 (via OpenRouter)",
            8.0,
            40.0,
        )
        .cache(10.0, 16.0, 0.20)
        .checked(R, D)
        .build(),
        PricingBuilder::new(
            "openrouter",
            "anthropic/claude-haiku-5.5",
            "Claude Haiku 5.5 (via OpenRouter)",
            0.10,
            0.50,
        )
        .cache(0.125, 0.20, 0.01)
        .long_context(OPENROUTER_HAIKU_LONG_CONTEXT, 0.50, 2.50)
        .long_cache_read(0.05)
        .long_cache_write(1.0)
        .checked(R, D)
        .build(),
        PricingBuilder::new(
            "openrouter",
            "openai/gpt-6.2-astra",
            "GPT-6.2 Astra (via OpenRouter)",
            8.0,
            40.0,
        )
        .cache_read(0.40)
        .cache_write_ceiling(10.0)
        .long_context(OPENROUTER_OPENAI_LONG_CONTEXT, 16.0, 60.0)
        .long_cache_read(0.80)
        .long_cache_write(20.0)
        .checked(R, D)
        .build(),
        PricingBuilder::new(
            "openrouter",
            "openai/gpt-6.2-sol",
            "GPT-6.2 Sol (via OpenRouter)",
            2.0,
            10.0,
        )
        .cache_read(0.10)
        .cache_write_ceiling(2.50)
        .long_context(OPENROUTER_OPENAI_LONG_CONTEXT, 4.0, 15.0)
        .long_cache_read(0.20)
        .long_cache_write(5.0)
        .checked(R, D)
        .build(),
        PricingBuilder::new(
            "openrouter",
            "openai/gpt-6-luna",
            "GPT-6 Luna (via OpenRouter)",
            0.10,
            0.50,
        )
        .cache_read(0.01)
        .cache_write_ceiling(0.125)
        .long_context(OPENROUTER_OPENAI_LONG_CONTEXT, 0.20, 0.75)
        .long_cache_read(0.02)
        .long_cache_write(0.25)
        .checked(R, D)
        .build(),
        PricingBuilder::new(
            "openrouter",
            "google/gemini-4-neon",
            "Gemini 4 Neon (via OpenRouter)",
            3.0,
            15.0,
        )
        .checked(R, D)
        .build(),
        PricingBuilder::new(
            "openrouter",
            "google/gemini-4-hydrogen",
            "Gemini 4 Hydrogen (via OpenRouter)",
            1.50,
            7.50,
        )
        .cache(0.0, 0.0, 0.075)
        .checked(R, D)
        .build(),
        PricingBuilder::new(
            "openrouter",
            "x-ai/grok-4.7",
            "Grok 4.7 (via OpenRouter)",
            2.0,
            6.0,
        )
        .cache(0.0, 0.0, 0.50)
        .long_context(OPENROUTER_XAI_LONG_CONTEXT, 4.0, 12.0)
        .long_cache_read(1.0)
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
        PricingBuilder::new(
            "openrouter",
            "deepseek/deepseek-v4-pro",
            "DeepSeek V4 Pro (via OpenRouter)",
            0.90,
            1.50,
        )
        .checked(R, D)
        .build(),
        // The undated route is cheaper than its dated `-1017` snapshot, so
        // both are listed: the snapshot must not price by prefix as this one.
        PricingBuilder::new(
            "openrouter",
            "deepseek/deepseek-v4.1-flash",
            "DeepSeek V4.1 Flash (via OpenRouter)",
            0.0498,
            0.1992,
        )
        .checked(R, D)
        .build(),
        PricingBuilder::new(
            "openrouter",
            "deepseek/deepseek-v4.1-flash-1017",
            "DeepSeek V4.1 Flash 1017 (via OpenRouter)",
            0.30,
            1.20,
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
    /// Each rate at the higher of the two, and the long-context tier (when
    /// either has one) from the lower threshold, so the result never bills
    /// below either input.
    fn dearest(self, other: Rates) -> Rates {
        let base = self.base.max(other.base);
        let long_context = match (self.long_context, other.long_context) {
            (Some((a, tier_a)), Some((b, tier_b))) => Some((a.min(b), tier_a.max(tier_b))),
            (one, other) => one.or(other),
        }
        .map(|(above, tier)| (above, tier.max(base)));
        Rates { base, long_context }
    }

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
#[derive(Clone)]
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
        lookup_rates(&self.by_model, model)
    }
}

/// `model`'s rates in `by_model`: its exact id, or else the longest listed id
/// it extends at a `-`, `:` or `@` boundary.
fn lookup_rates(by_model: &HashMap<String, Rates>, model: &str) -> Option<Rates> {
    by_model
        .get(model)
        .copied()
        .or_else(|| prefix_rates(by_model, model).map(|(_, rates)| rates))
}

/// The longest id in `by_model` that `model` extends at a `-`, `:` or `@`
/// boundary, with its length.
fn prefix_rates(by_model: &HashMap<String, Rates>, model: &str) -> Option<(usize, Rates)> {
    by_model
        .iter()
        .filter(|(id, _)| {
            model.strip_prefix(id.as_str()).is_some_and(|rest| {
                rest.starts_with('-') || rest.starts_with(':') || rest.starts_with('@')
            })
        })
        .max_by_key(|(id, _)| id.len())
        .map(|(id, rates)| (id.len(), *rates))
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

/// The bundled table, before any runtime catalog price is applied.
fn rate_table() -> &'static RateTable {
    static TABLE: OnceLock<RateTable> = OnceLock::new();
    TABLE.get_or_init(|| RateTable::from_entries(catalog()))
}

// ============================================================================
// Runtime catalog (ADR 112)
// ============================================================================

/// Upper bound for a catalog price, USD per 1M tokens. The dearest listed
/// model bills well under this; a larger figure is a unit or parsing error,
/// not a price, and would turn every estimate into a spend-cap breach.
pub const MAX_CATALOG_PRICE_PER_MILLION: f64 = 10_000.0;

/// A validated price from the runtime model catalog, USD per 1M tokens.
#[derive(Debug, Clone, PartialEq)]
pub struct CatalogPrice {
    /// Temps provider id.
    pub provider: String,
    pub model: String,
    pub display_name: String,
    pub input_per_million: f64,
    pub output_per_million: f64,
    pub cache_read_per_million: Option<f64>,
    /// The catalog's one cache-write price (the shortest-lived tier).
    pub cache_write_per_million: Option<f64>,
    pub deprecated: bool,
}

/// Whether `price` is a usable per-million price.
pub fn is_valid_catalog_price(price: f64) -> bool {
    price.is_finite() && (0.0..=MAX_CATALOG_PRICE_PER_MILLION).contains(&price)
}

/// The rates estimates use: the bundled table with the runtime catalog's
/// prices applied, and the catalog prices that were applied.
///
/// A catalog may list one model id under several providers at different
/// prices. `by_provider` keeps each provider's own rate, for callers that know
/// which provider served the request; `table`, used when only the model is
/// known, holds the dearest of them, so the result does not depend on catalog
/// order and never bills below any provider's price.
struct EffectivePricing {
    table: RateTable,
    by_provider: HashMap<String, HashMap<String, Rates>>,
    applied: BTreeMap<(String, String), CatalogPrice>,
}

impl EffectivePricing {
    /// Rates for `model` served by `provider`. An exact id always wins over
    /// a prefix, so a bundled `gpt-5.5-pro` row is not replaced by a catalog
    /// price for `gpt-5.5`: the provider's own catalog row for the exact id,
    /// then the table's; otherwise the longest matching prefix in either, the
    /// provider's row on a tie.
    fn rates_for(&self, provider: &str, model: &str) -> Option<Rates> {
        let own = self.by_provider.get(provider);
        if let Some(rates) = own.and_then(|by_model| by_model.get(model)) {
            return Some(*rates);
        }
        if let Some(rates) = self.table.by_model.get(model) {
            return Some(*rates);
        }
        let own_prefix = own.and_then(|by_model| prefix_rates(by_model, model));
        let table_prefix = prefix_rates(&self.table.by_model, model);
        match (own_prefix, table_prefix) {
            (Some((own_len, own)), Some((table_len, table))) => {
                Some(if own_len >= table_len { own } else { table })
            }
            (own, table) => own.or(table).map(|(_, rates)| rates),
        }
    }
}

fn effective() -> &'static ArcSwap<EffectivePricing> {
    static EFFECTIVE: OnceLock<ArcSwap<EffectivePricing>> = OnceLock::new();
    EFFECTIVE.get_or_init(|| {
        ArcSwap::from_pointee(EffectivePricing {
            table: rate_table().clone(),
            by_provider: HashMap::new(),
            applied: BTreeMap::new(),
        })
    })
}

/// Rates for a catalog price, or `None` when it must not replace the
/// bundled row.
///
/// * A catalog never makes a model free that the bundled table bills: a zero
///   price for a paid model is a catalog error, and would lift every spend cap
///   for it.
/// * A catalog publishes one cache-write price (the shortest-lived tier). The
///   bundled row's dearest write tier, scaled to the catalog's input price,
///   still applies, so a 1-hour write is not billed at the 5-minute rate. A
///   catalog without a cache-read price keeps the bundled cache discount,
///   scaled the same way.
/// * The bundled long-context tier is kept, raised to at least the new base
///   rates: a catalog publishes no tiers, and a long prompt must never bill
///   below a short one.
fn catalog_rates(price: &CatalogPrice, bundled: Option<Rates>) -> Option<Rates> {
    let input = price.input_per_million;
    let output = price.output_per_million;
    if !is_valid_catalog_price(input) || !is_valid_catalog_price(output) {
        return None;
    }
    let usable = |rate: Option<f64>| rate.filter(|rate| is_valid_catalog_price(*rate));
    if let Some(bundled) = bundled {
        let bundled_paid = bundled.base.input > 0.0 || bundled.base.output > 0.0;
        if bundled_paid && input <= 0.0 && output <= 0.0 {
            return None;
        }
    }
    // A bundled rate scaled to the catalog's input price, keeping the
    // bundled row's ratio to its input rate.
    let scaled = |rate: fn(&TierRates) -> f64| {
        bundled
            .filter(|bundled| bundled.base.input > 0.0)
            .map(|bundled| input * (rate(&bundled.base) / bundled.base.input))
    };
    let base = TierRates {
        input,
        output,
        cache_read: usable(price.cache_read_per_million)
            .or_else(|| scaled(|tier| tier.cache_read))
            .unwrap_or(input),
        cache_write: usable(price.cache_write_per_million)
            .unwrap_or(input)
            .max(scaled(|tier| tier.cache_write).unwrap_or(0.0))
            .max(input),
    };
    let long_context = bundled
        .and_then(|bundled| bundled.long_context)
        .map(|(above, tier)| (above, tier.max(base)));
    Some(Rates { base, long_context })
}

impl RateTable {
    /// This table with `prices` applied by exact model id. The worst case is
    /// kept: catalog prices are not reviewed like this table, so they never
    /// raise or lower what an unpriced model is charged.
    ///
    /// Each price is checked against the bundled rates the model already
    /// had, by exact id or inherited from the id it extends (a dated
    /// snapshot of a tiered model keeps its parent's long-context tier and
    /// cannot be made free).
    fn with_catalog(&self, prices: &[CatalogPrice]) -> EffectivePricing {
        let mut catalog_by_model: HashMap<String, Rates> = HashMap::new();
        let mut by_provider: HashMap<String, HashMap<String, Rates>> = HashMap::new();
        let mut applied = BTreeMap::new();
        for price in prices {
            let bundled = self.rates_for(&price.model);
            let Some(rates) = catalog_rates(price, bundled) else {
                continue;
            };
            by_provider
                .entry(price.provider.clone())
                .or_default()
                .insert(price.model.clone(), rates);
            catalog_by_model
                .entry(price.model.clone())
                .and_modify(|existing| *existing = existing.dearest(rates))
                .or_insert(rates);
            applied.insert((price.provider.clone(), price.model.clone()), price.clone());
        }
        let mut by_model = self.by_model.clone();
        by_model.extend(catalog_by_model);
        EffectivePricing {
            table: RateTable {
                by_model,
                worst_case: self.worst_case,
            },
            by_provider,
            applied,
        }
    }
}

/// Serialises tests that install catalog prices: the effective table is
/// process-wide, so two such tests must not run at once.
#[cfg(test)]
pub(crate) static CATALOG_TEST_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

/// Replace the runtime catalog prices estimates use. Returns how many were
/// applied; the rest were invalid or would have made a paid model free. An
/// empty slice restores the bundled table.
pub fn install_catalog_prices(prices: &[CatalogPrice]) -> usize {
    let pricing = rate_table().with_catalog(prices);
    let applied = pricing.applied.len();
    effective().store(Arc::new(pricing));
    applied
}

/// How many runtime catalog prices are applied.
pub fn catalog_price_count() -> usize {
    effective().load().applied.len()
}

/// Whether a price covers `model`: the runtime catalog or the bundled table
/// lists it, by exact id or as a dated or tagged variant of a listed id.
/// Every other model is estimated at the worst-case rate, and its usage is
/// recorded as having no known price.
pub fn is_priced(model: &str) -> bool {
    effective().load().table.rates_for(model).is_some()
}

/// Whether a price covers `model` when `provider` serves it: that provider's
/// catalog price, or any price `is_priced` finds.
pub fn is_priced_for(provider: &str, model: &str) -> bool {
    effective().load().rates_for(provider, model).is_some()
}

/// The effective price list: the bundled rows with runtime catalog prices
/// applied, followed by the models only the catalog lists.
pub fn effective_pricing() -> Vec<ModelPricing> {
    let current = effective().load();
    let mut rows = build_pricing();
    let mut seen = std::collections::HashSet::new();
    for row in &mut rows {
        if let Some(price) = current
            .applied
            .get(&(row.provider.clone(), row.model.clone()))
        {
            row.input_per_million = price.input_per_million;
            row.output_per_million = price.output_per_million;
            row.cache_hit_per_million = price.cache_read_per_million;
            if price.cache_write_per_million.is_some() {
                row.cache_write_5m_per_million = price.cache_write_per_million;
            }
            row.batch_input_per_million = None;
            row.batch_output_per_million = None;
            row.deprecated = row.deprecated || price.deprecated;
            row.source = PriceSource::Catalog;
        }
        seen.insert((row.provider.clone(), row.model.clone()));
    }
    // `applied` is ordered by (provider, model), so the extra rows are too.
    let extra: Vec<&CatalogPrice> = current
        .applied
        .iter()
        .filter(|(key, _)| !seen.contains(*key))
        .map(|(_, price)| price)
        .collect();
    rows.extend(extra.into_iter().map(|price| ModelPricing {
        model: price.model.clone(),
        display_name: price.display_name.clone(),
        provider: price.provider.clone(),
        input_per_million: price.input_per_million,
        output_per_million: price.output_per_million,
        cache_write_5m_per_million: price.cache_write_per_million,
        cache_write_1h_per_million: None,
        cache_hit_per_million: price.cache_read_per_million,
        batch_input_per_million: None,
        batch_output_per_million: None,
        deprecated: price.deprecated,
        source: PriceSource::Catalog,
    }));
    rows
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
    let current = effective().load();
    let table = &current.table;
    let rates = table.rates_for(model).unwrap_or(table.worst_case);
    cost_at(
        rates.for_request(input_tokens.max(0)),
        input_tokens,
        output_tokens,
        cache,
    )
}

/// `estimate_cost_microcents` for a request `provider` served: that
/// provider's own catalog price wins over another provider's for the same id.
pub fn estimate_cost_microcents_for(
    provider: &str,
    model: &str,
    input_tokens: i64,
    output_tokens: i64,
    cache: PromptTokensDetails,
) -> i64 {
    let current = effective().load();
    let rates = current
        .rates_for(provider, model)
        .unwrap_or(current.table.worst_case);
    cost_at(
        rates.for_request(input_tokens.max(0)),
        input_tokens,
        output_tokens,
        cache,
    )
}

/// `estimate_batch_total_cost_microcents` for a batch `provider` served.
pub fn estimate_batch_total_cost_microcents_for(
    provider: &str,
    model: &str,
    input_tokens: i64,
    output_tokens: i64,
    cache: PromptTokensDetails,
) -> i64 {
    let current = effective().load();
    let rates = current
        .rates_for(provider, model)
        .unwrap_or(current.table.worst_case);
    cost_at(rates.base, input_tokens, output_tokens, cache)
}

/// Estimate the provider cost of a batch's total usage, in microcents.
///
/// Like `estimate_cost_microcents`, but at the base tier: the total of many
/// requests says nothing about each one's prompt length, and comparing it
/// with a long-context threshold would bill a batch of short prompts at the
/// long-context rate. Batches go to OpenAI, whose batch prices are half its
/// standard ones and whose long-context rates are at most twice the base
/// input and one and a half times the base output rate, so a long prompt's
/// batch price never exceeds the standard base rate this charges.
pub fn estimate_batch_total_cost_microcents(
    model: &str,
    input_tokens: i64,
    output_tokens: i64,
    cache: PromptTokensDetails,
) -> i64 {
    let current = effective().load();
    let table = &current.table;
    let rates = table.rates_for(model).unwrap_or(table.worst_case);
    cost_at(rates.base, input_tokens, output_tokens, cache)
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
    provider: &str,
    model: &str,
    input_tokens: i64,
    output_tokens: i64,
    cache: PromptTokensDetails,
) -> Option<i64> {
    let (above, tier) = effective()
        .load()
        .rates_for(provider, model)?
        .long_context?;
    (input_tokens > above).then(|| cost_at(tier, input_tokens, output_tokens, cache))
}

#[cfg(test)]
mod tests;
