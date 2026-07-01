---
title: "ADR-021: Humanized alert notification text (deterministic default, optional AI enrichment)"
status: Proposed
date: 2026-06-27
author: David Viejo
---

# ADR-021: Humanized alert notification text (deterministic default, optional AI enrichment)

**Status:** Proposed
**Date:** 2026-06-27
**Author:** David Viejo

## Context

The metric alert notification (email/Slack/webhook) currently leads with a
statistician's sentence. For an anomaly it reads:

> `guestbook.activity.level avg is 76.223 — 34.6σ from the baseline 18.467 ± 1.672 (band ±3σ)`

and for a static threshold:

> `guestbook.list.requests avg is 5.000 (threshold ≥ 0.000)`

These strings are built by a single `format!` each, in `FireDetails::anomaly_breach`
(`crates/temps-otel/src/services/metric_alert_evaluator.rs:409`) and
`FireDetails::static_breach` (`:382`). They are precise and grep-able, but for an
on-call engineer glancing at a phone notification they bury the lede: *what
happened, how bad, and which direction.* "34.6σ" is not how a human reads "this
metric is ~4× its normal level."

We want the notification to read like a human wrote it. The question raised was
whether to do this with a **configured AI/model in OSS** (the `temps-ai-gateway`
crate already exists with BYO-key providers). The honest answer is: humanization
and AI are two different levers, and conflating them puts an LLM on a path that
must never flake.

### The reliability constraint

An alert is the thing you depend on when something is on fire. Its baseline
quality cannot depend on an LLM being configured, reachable, fast, and within
budget. The notification text is produced **inside the alarm fire path**: the
evaluator builds `FireDetails`, then `AlarmService::fire_alarm` synchronously
fans the message out to email/Slack/webhook (the email is rendered and dispatched
as part of firing — once sent it cannot be edited). Any text that must appear in
the email therefore has to be ready *before* dispatch. Routing the **baseline**
message through a network LLM call would add that call's latency and failure risk
to every fire.

### Rejected alternative: AI as the humanizer

Generating the primary, always-present message via the model is rejected. It is
over-engineering a reliability-critical path: it adds per-alert latency and cost,
introduces a hard dependency (no provider key configured ⇒ no readable alert), and
risks hallucinated causes in the one place users must trust literally. Rephrasing
a number we already computed is not where an LLM earns its keep — we have the value,
baseline, scale, σ, band, and direction in hand already.

### Where AI genuinely adds value

An LLM beats a template at **synthesis across signals** — "errors on `/checkout`
co-spiked and a deploy landed 3 minutes earlier" — not at restating a single
number. That is also the boundary with the EE **AI SRE companion**
(`Feature::AiSre`, interactive multi-step investigation over logs/traces/errors).
A one-shot *summary* enricher is a fair OSS feature; deep *investigation* stays EE.

## Decision

**Humanize deterministically by default; offer an optional, best-effort AI
enricher that only ever augments — never gates or delays — the deterministic text.**

Two tiers:

1. **Tier 1 — deterministic humanizer (OSS default, no AI).** A pure
   `humanize` module in `temps-otel` rewrites the `FireDetails.message` into plain
   language from the facts already computed. Zero latency, zero cost, no key
   required, works for 100% of self-hosters. Exact figures continue to live in
   `metadata` and render in the email's DETAILS table for the math-minded.

2. **Tier 2 — optional AI enricher (OSS, opt-in, off by default).** When a
   provider key is configured *and* a per-project toggle is on, the evaluator
   calls `temps-ai-gateway`'s `GatewayService::chat_completion`
   (`crates/temps-ai-gateway/src/services/gateway_service.rs:130`) with a compact
   set of **structured facts** to produce a 1–2 sentence summary. It is wrapped in
   a hard timeout and falls back to the Tier-1 text on any failure, missing key,
   or budget exhaustion. It enriches the message; it never blocks the fire beyond
   its bounded timeout.

### Tier 1 — the deterministic humanizer

New module `crates/temps-otel/src/services/humanize.rs`, pure and unit-tested. It
takes the same inputs the `FireDetails` constructors already have:

```rust
pub struct AnomalyFacts {
    pub metric: String,        // rule.metric_name
    pub agg: String,           // rule.aggregation
    pub value: f64,
    pub center: f64,           // baseline center
    pub scale: f64,            // baseline scale (≈ σ)
    pub z: f64,                // (value - center) / scale
    pub band_lo: f64,          // center - deviations*scale
    pub band_hi: f64,          // center + deviations*scale
    pub window_secs: i64,
}

/// e.g. "Activity level (guestbook.activity.level) is unusually high — averaging
/// ~76 over the last minute, about 4× the normal ~18 (usual range 13–23), far
/// outside the expected band."
pub fn humanize_anomaly(f: &AnomalyFacts) -> String { /* … */ }
pub fn humanize_static(f: &StaticFacts) -> String { /* … */ }
```

Phrasing rules (deterministic, no inference of cause):

- **Direction** from `value` vs `center`: above ⇒ "high / elevated", below ⇒
  "low / depressed".
- **Magnitude** preferring a ratio when `center` is meaningfully non-zero:
  `value/center` ⇒ "about 4× the normal ~18"; otherwise fall back to an absolute
  delta. Numbers rounded via the existing `fmt_compact` helper (`:136`).
- **Severity adverb** banded off `|z|` relative to the rule's `deviations` (e.g.
  "just outside" / "well outside" / "far outside" the expected band).
- **Window** rendered in human units ("over the last minute") from `window_secs`,
  reusing the `window_label` logic already in `chart_svg_for` (`:815`).
- Keep the canonical metric name in parentheses so it stays searchable.

This replaces only the `message` field; `title` and `metadata` are unchanged, so
the email subject, the DETAILS table, the chart, Slack blocks, and the webhook
payload all keep working. The exact `value/center/scale/z/deviations` stay in
`metadata` (already populated at `:419-434`).

### Tier 2 — the optional AI enricher

A trait carried by the evaluator, `None` by default:

```rust
// temps-otel
pub trait AlertSummarizer: Send + Sync {
    /// Best-effort 1–2 sentence summary. Returns None on any failure so the
    /// caller keeps the deterministic text. MUST NOT outlive its own timeout.
    async fn summarize(&self, facts: &AlertFacts) -> Option<String>;
}
```

`MetricAlertEvaluator` holds `Option<Arc<dyn AlertSummarizer>>` (wired like the
other optional deps). The OSS implementation lives behind the existing
`temps-ai-gateway` and is registered only when a provider key exists. Flow inside
`fire`, after `FireDetails` is built with the Tier-1 message:

```rust
let mut details = FireDetails::anomaly_breach(rule, value, params, &eval);
if let Some(sum) = &self.summarizer {
    if project_ai_summaries_enabled(rule.project_id) {
        if let Ok(Some(text)) =
            tokio::time::timeout(AI_SUMMARY_TIMEOUT, sum.summarize(&facts)).await
        {
            details.message = text;                 // enrich
            details.metadata["ai_summary"] = json!(true);
        }
    }
}
// any timeout/None/error path leaves the deterministic message intact
```

Hard rules:

- **`AI_SUMMARY_TIMEOUT` ≈ 4s**, wrapped in `tokio::time::timeout`. Bounds the
  added fire latency; only paid when the feature is enabled and a key is set.
- **Always fall back** to the Tier-1 text on timeout, error, no key, disabled, or
  over-budget. The deterministic message is the floor.
- **Only on an actual fire** (not every 30s tick) — alarms are already
  cooldown-throttled by `AlarmService`, so call volume is naturally low. Optionally
  cache by `(rule_id, direction)` to skip repeated fires within a window.
- **Structured facts only** in the prompt — the numbers above plus an optional
  one-line shape descriptor — never raw series, labels with PII, or request bodies.
  The system prompt forbids inventing causes: "summarize these facts for an on-call
  engineer in 1–2 sentences; do not speculate about root cause." This keeps the
  output grounded; root-cause reasoning is the EE companion's job.
- **Cross-signal context (Phase 2):** when cheap to gather, pass already-computed
  facts like "a deploy completed N minutes before the breach" or "error events on
  this project rose K× in the same window." This is the only place the LLM does
  something a template can't, and it stays factual because the facts are precomputed.

### Configuration (per the entity-column rule)

Following CLAUDE.md (runtime config is an entity column, not an env var) and the
`attack_mode` tri-state precedent (`crates/temps-entities/src/environments.rs:45`):

| Layer | Column | Meaning |
|---|---|---|
| projects | `ai_alert_summaries_enabled BOOLEAN NULL` | NULL = inherit global default (off) |
| environments | `ai_alert_summaries_enabled BOOLEAN NULL` | tri-state override (`Option<bool>`) |

The **provider key and budget** already live in `ai_provider_keys`
(migration `m20260310_000001`) and `ai_gateway_config`
(`crates/temps-entities/src/ai_gateway_config.rs`: `allowed_models`,
`max_requests_per_minute`, `max_cost_per_month_microcents`). The enricher routes
through `GatewayService`, so it inherits that rate-limit and monthly-cost
governance for free, and the summarization model is chosen from `allowed_models`.
No new secret env var is introduced.

### OSS ↔ EE boundary

- **OSS:** Tier 1 (always) + Tier 2 one-shot summary (opt-in, BYO key). A nicer
  sentence and, later, a factual cross-signal note.
- **EE:** the AI SRE companion (`Feature::AiSre`) — interactive, multi-step
  investigation across logs/traces/errors. Distinct value; not cannibalized by a
  one-shot summary.

## Consequences

### Positive

- Every self-hoster gets a readable alert with no AI, no latency, no key — the
  ~90% readability win at zero risk.
- The fire path's reliability is unchanged: the deterministic message is always
  the floor; the LLM can only add, and only within a bounded timeout.
- Reuses existing infrastructure: `temps-ai-gateway`, `ai_provider_keys`, and the
  `ai_gateway_config` rate/cost governance. No new provider abstraction, no new
  secret env var.
- Clean product ladder OSS → EE, with the EE AI SRE companion's value protected.
- Localized blast radius for Tier 1: one new pure module + the two `FireDetails`
  message lines; `title`/`metadata`/chart/Slack/webhook untouched.

### Negative / risks

- **Hallucinated causes** are the headline risk for Tier 2. Mitigated by feeding
  only precomputed facts and a prompt that forbids speculation; root-cause
  reasoning is explicitly out of scope for the OSS summary.
- **Bounded added latency on fire** when Tier 2 is enabled (≤ `AI_SUMMARY_TIMEOUT`).
  Acceptable for a 30s evaluator cadence, but it is real and only paid when enabled.
- **Cost**, governed by `ai_gateway_config` budgets; an operator who sets generous
  limits and noisy rules will spend more. Throttling + fire-only generation keep it
  bounded.
- **Provider downtime / rate limits** degrade to the Tier-1 text (by design), but
  the operator may not notice the AI summary silently stopped — surface an
  `ai_summary` success counter.
- **Translation/locale:** Tier 1 ships English only; a future locale pass or the
  LLM (which can be prompted for a target language) would address non-English teams.

### Neutral

- No behavior change for operators who leave `ai_alert_summaries_enabled` off /
  NULL and configure no provider key: the summarizer slot is `None`; the path is
  the deterministic humanizer only.
- The DETAILS table keeps the precise statistics, so nothing is lost for users who
  want the exact σ/baseline numbers.

## Phased plan

### Phase 1 — Tier 1 deterministic humanizer (ship first)

- Add `crates/temps-otel/src/services/humanize.rs` (`humanize_anomaly`,
  `humanize_static`) with unit tests for direction, ratio vs delta, rounding, and
  severity banding.
- Swap the two `format!` calls in `FireDetails` (`:382`, `:409`) to call it; keep
  `title`/`metadata` unchanged.
- Verify live: trigger an anomaly, confirm the email/Slack/webhook lead sentence
  is humanized and the DETAILS table still carries exact figures.

### Phase 2 — Tier 2 opt-in AI enricher

- Add the `AlertSummarizer` trait + an OSS impl over `GatewayService::chat_completion`.
- Add `ai_alert_summaries_enabled` columns (projects + environments, tri-state) and
  a global default (off).
- Wire `Option<Arc<dyn AlertSummarizer>>` into the evaluator; gate on toggle + key;
  `tokio::time::timeout`; fallback; fire-only; optional `(rule_id, direction)` cache.
- Structured-facts prompt; system prompt forbids cause speculation; cap output
  length; record an `ai_summary` success/failure counter.

### Phase 3 — cross-signal facts in the prompt

- Pass precomputed context (recent deploy within N minutes; co-spiking error count
  in the same window) so the summary can note correlated changes factually.
- This is the genuine AI-shaped value and the natural hand-off point to consider
  surfacing "open in AI SRE" (EE) for deeper investigation.

## Open questions

1. **Default model for summarization?** A small/cheap model is plenty for a 1–2
   sentence summary. Pick a sensible default from `allowed_models`, or require the
   operator to designate a "summary model" in `ai_gateway_config`?
2. **Global toggle home:** a global default for `ai_alert_summaries_enabled` —
   server config row, or a settings table? (Per-project/env columns are settled;
   the global default's home is open.)
3. **Generalize `humanize`?** Start narrow in `temps-otel`. If monitoring alarms,
   deploy failures, and error-group notifications want the same treatment, promote
   a generic `humanize(context)` to `temps-core` later — not up front.
4. **Slack/webhook vs email parity:** the AI summary replaces `message`, so all
   channels get it. Confirm we want the same enriched text everywhere (likely yes)
   vs email-only.
5. **Cache TTL / invalidation** for `(rule_id, direction)` summaries on repeated
   fires — fixed short TTL, or skip caching and rely on fire-throttling alone?

## References

- `crates/temps-otel/src/services/metric_alert_evaluator.rs:372-437` —
  `FireDetails` and the two message `format!`s (`:382` static, `:409` anomaly) this
  ADR humanizes; `metadata` already carries the exact stats (`:419-434`).
- `crates/temps-otel/src/services/metric_alert_evaluator.rs:136` — `fmt_compact`
  (reused for rounded numbers); `:815` — `window_label` logic (reused for "last 1m").
- `crates/temps-ai-gateway/src/services/gateway_service.rs:130` —
  `GatewayService::chat_completion(&ChatCompletionRequest, &ByokOverride)`, the
  Tier-2 entrypoint (resolves system or BYO key, routes to provider).
- `crates/temps-entities/src/ai_gateway_config.rs` — `allowed_models`,
  `max_requests_per_minute`, `max_cost_per_month_microcents` (rate/cost governance
  the enricher inherits).
- `crates/temps-migrations/src/migration/m20260310_000001_create_ai_provider_keys.rs`
  — encrypted provider keys (the "is a model configured?" gate).
- `crates/temps-entities/src/environments.rs:45` — `attack_mode: Option<bool>`
  tri-state per-environment precedent for `ai_alert_summaries_enabled`.
- `temps-monitoring` `AlarmService::fire_alarm` / `send_alarm_notification` — the
  synchronous fan-out that renders and dispatches the email during firing (why the
  AI summary must be ready before dispatch, not after).
- AI SRE companion (`Feature::AiSre`) — the EE interactive investigator; the OSS↔EE
  boundary this ADR draws (one-shot summary in OSS, deep investigation in EE).
