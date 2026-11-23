// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

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
        .checked("https://example.com/prices", CHECKED_2026_11_23)
        .build();
    assert!(provenance_problem(&foreign).is_some());
    let good = PricingBuilder::new("openai", "m", "M", 1.0, 2.0)
        .checked(OPENAI_PRICING, CHECKED_2026_11_23)
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
    // it starts at the lowest confirmed threshold (Claude Haiku 5.5's on
    // OpenRouter, which applies from 100,000 prompt tokens on).
    let (above, long) = table.worst_case.long_context.expect("a long tier");
    assert_eq!(above, OPENROUTER_HAIKU_LONG_CONTEXT);
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
fn batch_totals_are_priced_at_the_base_tier() {
    let none = PromptTokensDetails::default();
    // A 300,000-token total is above gpt-5.4's 272,000 threshold, but it
    // is many prompts: the base $2.50 applies.
    assert_eq!(
        estimate_batch_total_cost_microcents("gpt-5.4", 300_000, 0, none),
        300_000 * 5 / 2
    );
    assert_eq!(
        estimate_batch_total_cost_microcents("totally-unlisted-model", 300_000, 1_000, none),
        300_000 * 30 + 1_000 * 180
    );
}

#[test]
fn an_openai_long_prompt_in_a_batch_never_costs_more_than_the_standard_base_rate() {
    // What `estimate_batch_total_cost_microcents` relies on: a batch
    // total billed at the standard base rate never undercounts its long
    // prompts.
    for entry in catalog()
        .into_iter()
        .filter(|entry| entry.pricing.provider == "openai")
    {
        let (Some(long), Some(batch_in), Some(batch_out)) = (
            entry.long_context,
            entry.pricing.batch_input_per_million,
            entry.pricing.batch_output_per_million,
        ) else {
            continue;
        };
        let pricing = &entry.pricing;
        let in_discount = batch_in / pricing.input_per_million;
        let out_discount = batch_out / pricing.output_per_million;
        assert!(
            long.input * in_discount <= pricing.input_per_million + 1e-9
                && long.output * out_discount <= pricing.output_per_million + 1e-9,
            "{}: a long prompt's batch price exceeds the standard base rate",
            pricing.model
        );
    }
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
            .checked(OPENAI_PRICING, CHECKED_2026_11_23)
            .build(),
        PricingBuilder::new("openai", "stale", "Stale", 99.0, 999.0)
            .unconfirmed(OPENAI_PRICING, CHECKED_2026_11_23)
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
                long.input >= model.input_per_million && long.output >= model.output_per_million,
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

#[test]
fn current_generation_models_are_priced_at_their_own_rates() {
    // Rows checked on 2026-11-23 against each provider's pricing page.
    let worst = estimate("totally-unlisted-model", 1_000, 1_000);
    for (model, input, output) in [
        ("claude-fable-5-2", 8.0_f64, 40.0_f64),
        ("claude-mythos-5-2", 8.0, 40.0),
        ("claude-mythos-5-1", 10.0, 50.0),
        ("claude-opus-5-5", 4.0, 20.0),
        ("claude-sonnet-5-5", 2.0, 10.0),
        ("claude-haiku-5-5", 0.10, 0.50),
        ("gpt-6.2-astra", 8.0, 40.0),
        ("gpt-6.2-sol", 2.0, 10.0),
        ("gemini-4-neon", 3.0, 15.0),
        ("anthropic/claude-fable-5.2", 8.0, 40.0),
        ("openai/gpt-6.2-sol", 2.0, 10.0),
        ("deepseek/deepseek-v4-pro", 0.90, 1.50),
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
fn gpt_6_2_bills_long_prompts_and_cache_writes_at_its_listed_tiers() {
    // Over 272K input tokens: $16 / $60 for GPT-6.2 Astra.
    assert_eq!(
        estimate("gpt-6.2-astra", 272_001, 1_000),
        (272_001.0 * 16.0 + 1_000.0 * 60.0_f64).ceil() as i64
    );
    // A written token bills at the 12-hour write rate, $20.
    assert_eq!(with_cache("gpt-6.2-astra", 1_000, 0, 0, 1_000), 1_000 * 20);
}

#[test]
fn retired_claude_ids_price_by_prefix_instead_of_the_worst_case() {
    // Dated ids of retired models used to fall through to the worst case.
    assert_eq!(
        estimate("claude-sonnet-4-20250514", 1_000, 1_000),
        1_000 * 3 + 1_000 * 15
    );
    assert_eq!(
        estimate("claude-opus-4-1-20250805", 1_000, 1_000),
        1_000 * 15 + 1_000 * 75
    );
    // A listed longer id still wins over the shorter retired one.
    assert_eq!(
        estimate("claude-opus-4-6-20260205", 1_000, 1_000),
        estimate("claude-opus-4-6", 1_000, 1_000)
    );
    assert_eq!(
        estimate("claude-sonnet-4-6", 1_000, 1_000),
        1_000 * 3 + 1_000 * 15
    );
    assert!(listed("claude-sonnet-4").deprecated);
}

#[test]
fn a_dearer_dated_snapshot_is_not_priced_as_its_cheaper_alias() {
    assert_eq!(
        estimate("deepseek/deepseek-v4.1-flash-1017", 1_000_000, 0),
        300_000
    );
    assert!(
        estimate("deepseek/deepseek-v4.1-flash", 1_000_000, 0)
            < estimate("deepseek/deepseek-v4.1-flash-1017", 1_000_000, 0)
    );
}

#[test]
fn openrouter_routes_bill_long_prompts_from_their_listed_thresholds() {
    let none = PromptTokensDetails::default();
    // Claude Haiku 5.5 via OpenRouter: $0.10/$0.50, and $0.50/$2.50 from
    // 100,000 prompt tokens on (inclusive).
    assert_eq!(
        estimate_cost_microcents("anthropic/claude-haiku-5.5", 99_999, 1_000, none),
        (99_999.0_f64 * 0.10 + 1_000.0 * 0.50).ceil() as i64
    );
    assert_eq!(
        estimate_cost_microcents("anthropic/claude-haiku-5.5", 100_000, 1_000, none),
        100_000 / 2 + 2_500
    );
    // GPT-6.2 Sol via OpenRouter switches at 272,000 prompt tokens.
    assert_eq!(
        estimate_cost_microcents("openai/gpt-6.2-sol", 271_999, 0, none),
        271_999 * 2
    );
    assert_eq!(
        estimate_cost_microcents("openai/gpt-6.2-sol", 272_000, 0, none),
        272_000 * 4
    );
    assert_eq!(
        estimate_cost_microcents("openai/gpt-6.2-astra", 272_000, 0, none),
        272_000 * 16
    );
}

#[test]
fn openrouter_routes_bill_cache_reads_and_writes_at_their_listed_rates_in_both_tiers() {
    // 50,000 prompt tokens below every threshold and 300,000 above it; a
    // tenth of each read from the cache and a tenth written to it.
    // (model, short (read, write) rates, long (read, write) rates), per
    // OpenRouter's listing; a write bills at the dearest (1-hour) tier.
    for (model, short, long) in [
        ("anthropic/claude-haiku-5.5", (0.01, 0.20), (0.05, 1.0)),
        ("openai/gpt-6.2-astra", (0.40, 10.0), (0.80, 20.0)),
        ("openai/gpt-6.2-sol", (0.10, 2.50), (0.20, 5.0)),
        ("openai/gpt-6-luna", (0.01, 0.125), (0.02, 0.25)),
    ] {
        let rates = rate_table().rates_for(model).expect("listed");
        let (above, long_tier) = rates.long_context.expect("a long tier");
        for (input, (read_rate, write_rate), tier) in
            [(50_000_i64, short, rates.base), (300_000, long, long_tier)]
        {
            assert_eq!(input > above, tier == long_tier, "{model} at {input}");
            let details = PromptTokensDetails {
                cached_tokens: input / 10,
                cache_write_tokens: input / 10,
            };
            let expected = ((input - input / 5) as f64 * tier.input
                + (input / 10) as f64 * read_rate
                + (input / 10) as f64 * write_rate)
                .ceil() as i64;
            assert_eq!(
                estimate_cost_microcents(model, input, 0, details),
                expected,
                "{model} at {input} prompt tokens"
            );
        }
    }
    // The Anthropic routes publish both write tiers and the cache-read rate.
    for (model, write_5m, write_1h, hit) in [
        ("anthropic/claude-opus-5.5", 5.0, 8.0, 0.20),
        ("anthropic/claude-sonnet-5.5", 2.50, 4.0, 0.10),
        ("anthropic/claude-sonnet-5", 2.50, 4.0, 0.20),
        ("anthropic/claude-fable-5.2", 10.0, 16.0, 0.20),
        ("anthropic/claude-haiku-5.5", 0.125, 0.20, 0.01),
    ] {
        let row = listed(model);
        assert_eq!(
            (
                row.cache_write_5m_per_million,
                row.cache_write_1h_per_million,
                row.cache_hit_per_million
            ),
            (Some(write_5m), Some(write_1h), Some(hit)),
            "{model}"
        );
    }
}

#[test]
fn the_openrouter_low_cost_default_is_priced_at_its_own_rate() {
    let default = crate::providers::low_cost_default_model("openrouter").unwrap();
    assert_eq!(default, "openai/gpt-6-luna");
    let row = listed(default);
    assert_eq!(
        (row.input_per_million, row.output_per_million),
        (0.10, 0.50)
    );
    assert_eq!(
        estimate_cost_microcents(default, 100_000, 0, PromptTokensDetails::default()),
        10_000
    );
    // From 272,000 prompt tokens the route bills $0.20 per 1M input tokens.
    assert_eq!(
        estimate_cost_microcents(default, 300_000, 0, PromptTokensDetails::default()),
        60_000
    );
}

#[test]
fn every_bootstrap_chat_model_has_a_built_in_rate() {
    use crate::providers::openai_compat::OpenAiCompatProvider;
    use crate::providers::AiProvider;
    // A new key is seeded with these before discovery supplies anything, so
    // an unpriced one would be charged the worst-case rate against spend
    // caps from its first request.
    for provider in [
        OpenAiCompatProvider::openai(),
        OpenAiCompatProvider::xai(),
        OpenAiCompatProvider::openrouter(),
    ] {
        for model in provider.available_models() {
            if !crate::providers::is_chat_model_id(&model.id) {
                continue;
            }
            assert!(
                rate_table().rates_for(&model.id).is_some(),
                "bootstrap model {} has no built-in rate",
                model.id
            );
        }
    }
}

#[test]
fn openrouter_gemini_and_grok_routes_bill_their_listed_rates() {
    // Microcents are per-token rates times tokens: $1 per 1M tokens bills
    // 1 microcent per token.
    let cost = |model, input| estimate(model, input, 1_000_000);
    assert_eq!(
        cost("google/gemini-4-hydrogen", 1_000_000),
        1_500_000 + 7_500_000
    );
    // Grok 4.7 doubles from 200,000 prompt tokens on (inclusive).
    assert_eq!(cost("x-ai/grok-4.7", 199_999), 399_998 + 6_000_000);
    assert_eq!(cost("x-ai/grok-4.7", 200_000), 800_000 + 12_000_000);
}
