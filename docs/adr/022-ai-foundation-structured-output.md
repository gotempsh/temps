---
title: "ADR-022: A general AI foundation for typed/structured output (`AiService`)"
status: Proposed
date: 2026-06-27
author: David Viejo
---

# ADR-022: A general AI foundation for typed/structured output (`AiService`)

**Status:** Proposed
**Date:** 2026-06-27
**Author:** David Viejo

## Context

ADR-021 added an AI enricher for alert notifications. While wiring it, the real
requirement surfaced: **any part of Temps should be able to ask the configured
model for structured, typed data** — not just free text, and not just for
alerts. Alert summaries are merely the first consumer. We want one governed,
provider-agnostic way to say "given these facts, return *this Rust type*," and
get back a validated value.

Building this per-feature (a bespoke trait like ADR-021's `AlertSummarizer`, a
hand-rolled prompt, ad-hoc JSON parsing) would scatter prompt-wrangling, JSON
repair, model selection, and governance across every crate that wants AI. That
is the wrong foundation.

The pieces already exist:

- **`temps-ai-gateway`** routes to providers (OpenAI, Anthropic, xAI, Gemini),
  decrypts BYO/system keys, and enforces per-scope rate/cost governance via
  `ai_gateway_config` (`allowed_models`, `max_requests_per_minute`,
  `max_cost_per_month_microcents`). Entry point:
  `GatewayService::chat_completion` (`crates/temps-ai-gateway/src/services/gateway_service.rs:130`).
- **Structured output is already plumbed.** `ChatCompletionRequest.response_format`
  exists (`crates/temps-ai-gateway/src/types.rs:35`), and the Gemini provider
  already translates it to JSON mode
  (`crates/temps-ai-gateway/src/providers/gemini.rs:266`); OpenAI-compatible
  providers accept `{"type":"json_schema", ...}` natively.
- **`schemars` 0.8** is a workspace dependency (`Cargo.toml:182`), so a JSON
  Schema can be *derived* from any Rust type.

What's missing is a small, reusable seam on top of the gateway that turns those
into "call AI, get a `T`."

### Rejected alternative: feature-specific AI traits

Continuing the ADR-021 pattern — a new `XxxSummarizer`/`XxxAnalyzer` trait per
feature — is rejected. It duplicates model resolution, prompt scaffolding, JSON
extraction/repair, governance, and the best-effort/timeout discipline in every
consumer. The `AlertSummarizer` introduced in ADR-021 is therefore **demoted to
a thin consumer** of the foundation defined here (see §5).

### Rejected alternative: a generic method on the trait object

The natural API — `async fn complete<T: DeserializeOwned>(...) -> T` — is **not
object-safe**, so it can't live on an `Arc<dyn AiService>` resolved through the
plugin DI. The decision below splits it: an object-safe core method that speaks
`serde_json::Value`, plus a generic *free function* that adds the typed sugar.

## Decision

**Add `temps_core::ai::AiService`: an object-safe, governed, provider-agnostic AI
seam, plus a generic `complete_typed::<T>()` helper that derives a JSON Schema
from `T`, asks the provider for matching JSON, and deserializes. Implement it in
`temps-ai-gateway` over `GatewayService`. Every AI use in Temps — alerts and
beyond — goes through this.**

### 1. The seam (`temps-core`, no AI dependency)

```rust
// crates/temps-core/src/ai/mod.rs
use async_trait::async_trait;

#[derive(Debug, Clone, Default)]
pub struct AiRequest {
    /// Short tag for logging / usage attribution, e.g. "alert.summary".
    pub purpose: String,
    /// Optional governance + usage scope (per-project budgets).
    pub project_id: Option<i32>,
    pub system: Option<String>,
    pub prompt: String,
    /// Override the configured default model for this call.
    pub model: Option<String>,
    pub max_tokens: Option<u32>,
    pub temperature: Option<f32>,
    /// When set, the provider is asked to return JSON matching this schema.
    pub response_schema: Option<serde_json::Value>,
}

#[derive(Debug, Clone)]
pub struct AiResponse {
    pub text: String,
    /// Parsed JSON when a schema was requested and the reply parsed.
    pub json: Option<serde_json::Value>,
    pub model: String,
}

/// The governed AI capability. Object-safe: registered + resolved as
/// `Arc<dyn AiService>` via the plugin DI, like `NotificationService`.
#[async_trait]
pub trait AiService: Send + Sync {
    /// Cheap gate: is a provider key + usable model actually configured? Lets a
    /// caller skip building a prompt when AI is unavailable.
    async fn is_available(&self) -> bool;

    /// Low-level completion. Best-effort: returns `AiError` rather than
    /// panicking; callers wrap in a timeout.
    async fn complete(&self, request: AiRequest) -> Result<AiResponse, AiError>;
}
```

### 2. The typed sugar (generic free functions)

Object-safety lives on the trait; ergonomics live beside it:

```rust
// crates/temps-core/src/ai/typed.rs

/// Plain text. `None` if AI is unavailable or the call fails.
pub async fn complete_text(ai: &dyn AiService, req: AiRequest) -> Option<String> {
    ai.complete(req).await.ok().map(|r| r.text)
}

/// Typed structured output. Derives `T`'s JSON Schema, requests matching JSON,
/// and deserializes. `None` on any failure (unavailable, provider error,
/// non-conforming JSON). One repair retry on parse failure.
pub async fn complete_typed<T>(ai: &dyn AiService, mut req: AiRequest) -> Option<T>
where
    T: schemars::JsonSchema + serde::de::DeserializeOwned,
{
    let schema = serde_json::to_value(schemars::schema_for!(T)).ok()?;
    req.response_schema = Some(schema);
    let resp = ai.complete(req).await.ok()?;
    let value = resp.json.or_else(|| extract_json_block(&resp.text))?;
    serde_json::from_value(value).ok()
}
```

Caller experience — "structured data from AI anywhere":

```rust
#[derive(serde::Deserialize, schemars::JsonSchema)]
struct DeployRiskAssessment { risk: String, reasons: Vec<String> }

let assessment: Option<DeployRiskAssessment> = complete_typed(
    ai.as_ref(),
    AiRequest { purpose: "deploy.risk".into(), prompt, ..Default::default() },
).await;
```

### 3. The implementation (`temps-ai-gateway`)

`GatewayAiService { gateway: Arc<GatewayService>, db: Arc<DatabaseConnection> }`,
registered in the AI-gateway plugin as `Arc<dyn temps_core::ai::AiService>`:

- `is_available()` — a model resolves (from `ai_gateway_config.allowed_models`,
  or a configured default) **and** a provider key exists for it.
- `complete(req)`:
  1. resolve the model (`req.model` → project-scope config → instance config);
  2. build a `ChatCompletionRequest`; when `req.response_schema` is set, attach
     `response_format = {"type":"json_schema","json_schema":{"name":"temps_response","schema":<schema>,"strict":true}}`
     (OpenAI-native; Gemini already maps it to JSON mode);
  3. `gateway.chat_completion(&req, &ByokOverride::default())` — inherits key
     decryption, provider routing, rate/cost governance, usage tracking;
  4. extract assistant text; if a schema was requested, parse JSON (stripping
     markdown fences) into `AiResponse.json`.

Provider JSON-mode support is uneven, so the helper also extracts a JSON block
from plain text — structured output degrades to "parse what came back," never to
a hard failure.

### 4. Governance, safety, config

- **One throttle/budget path:** every call goes through `GatewayService`, so the
  existing `ai_gateway_config` rate/cost limits and usage accounting apply to
  internal AI exactly as to the public gateway. `purpose` + `project_id` attribute
  the spend.
- **Best-effort everywhere:** `complete_typed`/`complete_text` return `Option`;
  the trait returns `Result`. AI is never on a path that can block or fail a core
  operation — callers add a timeout (e.g. alerts' `AI_SUMMARY_TIMEOUT`).
- **Availability is a capability, opt-in is a feature concern:** `is_available()`
  answers "is AI configured at all." Whether a *feature* uses it (e.g. the
  per-project `ai_alert_summaries_enabled` toggle from ADR-021) stays with the
  feature, not the foundation.
- **No new secrets as env vars:** keys stay in `ai_provider_keys`; model/limits
  stay in `ai_gateway_config` (entity rows, per CLAUDE.md).

### 5. Alerts become consumer #1 (refactor ADR-021)

The bespoke `temps_core::alert_summary::AlertSummarizer` + `AiGatewaySummarizer`
introduced in ADR-021 are refactored onto this foundation:

- The metric alert evaluator depends on `Option<Arc<dyn AiService>>`.
- On fire (when the project toggle is on and `ai.is_available()`), it builds an
  `AiRequest` from the alert facts and calls `complete_text` (a richer
  `complete_typed::<AlertNote>` once we want a structured headline + reason),
  inside the existing timeout, falling back to the deterministic Tier-1 text.

This deletes the alert-specific trait in favour of the general one — less code,
and the same call shape every future feature will use.

## Consequences

### Positive

- One governed, provider-agnostic way to get **typed** data from AI, callable
  from any crate via a `temps-core` seam — the foundation the product needs.
- Schema-derived structured output (`schemars`) means call sites declare a Rust
  type and get it back; no hand-written schemas, no brittle string parsing.
- Reuses all existing gateway machinery (keys, routing, rate/cost, usage) — no
  parallel AI stack, no new secret env vars.
- Best-effort + object-safe by construction; AI can never block or fail a core
  path, and the capability flows through normal plugin DI.
- Collapses ADR-021's alert-specific trait into a general one (net less code).

### Negative / risks

- **Provider JSON-mode variance:** not every provider/model honours
  `response_format` identically; the text-extraction fallback + one repair retry
  mitigate but don't eliminate non-conforming replies. `complete_typed` returns
  `None` rather than a wrong value.
- **Cost/latency** are real when used on hot paths; mitigated by `is_available()`
  gating, per-scope budgets, and mandatory caller-side timeouts. Internal AI
  should stay on cold/triggered paths (alerts, on-demand analysis), not request
  hot paths.
- **Prompt-injection / data exposure:** callers must pass only the data they
  intend; the foundation does not sanitize. Document that user-controlled text in
  a prompt is a trust boundary.
- **Schema drift:** `schemars` output for complex types can be large; keep
  request/response DTOs small and flat for reliable structured replies.

### Neutral

- No behaviour change when no provider key is configured: `is_available()` is
  false and every helper returns `None`.

## Phased plan

1. **Foundation:** `temps_core::ai` (trait, `AiRequest`/`AiResponse`/`AiError`,
   `complete_text`/`complete_typed`, `extract_json_block`); add `schemars` +
   `async-trait` to `temps-core`. Unit-test `extract_json_block` and the typed
   helper against a mock `AiService`.
2. **Impl + DI:** `GatewayAiService` in `temps-ai-gateway`; register as
   `Arc<dyn AiService>` in the gateway plugin (replacing ADR-021's summarizer
   registration). Model resolution + `is_available()` + `response_format` wiring.
3. **Refactor alerts onto it:** evaluator depends on `AiService`; delete
   `AlertSummarizer`/`AiGatewaySummarizer`; keep the `ai_alert_summaries_enabled`
   toggle and the timeout/fallback.
4. **Second consumer (proof of generality):** pick one structured use —
   e.g. a `complete_typed::<DeployFailureDiagnosis>` over a failed build log tail,
   or error-group titling — to validate the typed path end-to-end.

## Open questions

1. **Default internal model:** reuse `allowed_models[0]`, or add an explicit
   `ai_gateway_config.default_model` for server-internal features so summaries
   don't depend on the public allow-list?
2. **Repair policy:** how many JSON-repair retries (0 or 1) before giving up, and
   do we log non-conforming replies for tuning?
3. **Sync vs streaming:** the foundation is request/response; streaming
   (`chat_completion_stream`) stays gateway-only until a consumer needs it.
4. **Per-purpose budgets:** do we want `ai_gateway_config` scopes keyed by
   `purpose` (e.g. cap "alert.summary" spend separately), or is project/instance
   scope enough for v1?

## References

- ADR-021 (humanized alert notifications) — the first consumer; its
  `AlertSummarizer` is refactored onto this foundation (§5).
- `crates/temps-ai-gateway/src/services/gateway_service.rs:130` —
  `GatewayService::chat_completion`, the governed entry point this wraps.
- `crates/temps-ai-gateway/src/types.rs:35` — existing
  `ChatCompletionRequest.response_format` (structured-output input).
- `crates/temps-ai-gateway/src/providers/gemini.rs:266` — provider translation of
  `response_format` to JSON mode (`response_mime_type`).
- `crates/temps-entities/src/ai_gateway_config.rs` — `allowed_models` +
  rate/cost governance inherited by internal AI calls.
- `Cargo.toml:182` — `schemars` 0.8, used to derive JSON Schemas from Rust types.
