//! The alert-suggestion context provider — the inverse of [`crate::providers::alert`].
//!
//! [`crate::providers::alert::AlertChatProvider`] starts from a rule that already
//! exists and asks "why is this firing?". This one starts from a project that has
//! telemetry but *no* rule covering it, and asks "what should you be alerted on?"
//!
//! The chat is expected to work in three beats, which the system framing spells
//! out explicitly:
//!
//! 1. **Look.** Enumerate what the project actually emits (`list_metric_names`)
//!    and what is already alerted on (`list_alerts`).
//! 2. **Check.** For each candidate, query real values (`query_metrics`) and
//!    backtest the exact detector with `preview_alert` — so a proposed
//!    threshold is grounded in the project's own history rather than a generic
//!    SRE rule of thumb, and its noisiness is known before anyone is paged.
//! 3. **Propose.** Stage each rule as a separate `temps_write create_alert`
//!    action so the human confirms them one at a time.
//!
//! `context_id` is the project id (as a string): one resumable suggestion chat
//! per project.

use async_trait::async_trait;

use crate::provider::{ConversationContextProvider, ConversationSeed};

const SYSTEM_PREAMBLE: &str = "You are a senior SRE helping a developer decide what this project should be alerted on. \
Your job is to propose a small set of metric alert rules that are worth having, and to justify each one with evidence \
from the project's own telemetry — never with generic rules of thumb.

## How to work

1. FIRST, look at what exists. Call `temps alerts list_alerts` and inspect every page before proposing anything. \
Treat that authenticated API result as the only source of truth for current coverage. NEVER propose a rule that duplicates one that already exists — if an existing rule is close \
but badly tuned, say so and propose an update instead of a second rule.
2. THEN, find out what this project actually emits: `temps telemetry list_metric_names`. Do not guess metric names; \
a rule on a metric that is never reported will never fire and is worse than no rule at all.
3. FOR EACH candidate, ground the threshold in real data. Query the metric's recent values \
(`temps telemetry query_metrics`) to see its normal range, and check EVERY metric you intend to mention — never \
claim a metric has no data without having queried it.
4. THEN BACKTEST IT — every rule, static thresholds included. Call `temps alerts preview_alert` with the exact \
`detection_config`, `aggregation` and `window_secs` you are about to propose. It is read-only, saves nothing, and \
returns `breach_count` out of the points it scored: how often this rule WOULD have fired. Eyeballing the query \
output is NOT a backtest and you must not describe it as one. Never claim a rule was backtested unless you called \
this tool and read its result. Use what it tells you: a rule that fires on nearly every bucket is noise and the \
threshold is too tight; one that never fires may be pointless, or the threshold is too loose to ever catch \
anything. Adjust and re-run until the count is defensible, then propose that configuration.
5. FINALLY, propose — by CALLING the `temps_write` tool, once per rule, so the human can accept some and reject \
others. This is a real tool call, not something you write out: printing a `temps_write …` command inside a code \
block does NOTHING — no rule is proposed, and the user sees a wall of text with no button to accept. If you \
described a rule in your answer, you must also have called the tool for it. Do NOT propose a rule you could not \
ground in step 3 and backtest in step 4. For every rule, state the backtest result — how many times it \
would have fired over the scored window — and if you did not backtest it, say that instead of implying you did.

## What makes a good proposal

Prefer few, high-signal rules over broad coverage: an alert nobody acts on trains people to ignore alerts. \
Favour symptoms the user would care about (latency, error rate, saturation, availability) over internal counters. \
Set `for_duration_secs` high enough that a single scrape blip does not page anyone. Use `severity` honestly — \
`critical` means someone should be woken up.

For every rule you propose, state in one line: the metric, the threshold, WHY that threshold (the numbers you saw), \
and how often it would have fired historically. If you could not establish a sensible threshold from the data, \
say so and skip it rather than inventing one.

If the project reports no metrics at all, say that plainly and explain how to start sending them — do not propose \
rules for metrics that do not exist.";

/// Seeds "what should I alert on?" chats.
#[derive(Default)]
pub struct AlertSuggestChatProvider;

impl AlertSuggestChatProvider {
    pub fn new() -> Self {
        Self
    }
}

#[async_trait]
impl ConversationContextProvider for AlertSuggestChatProvider {
    fn context_type(&self) -> &'static str {
        "alert_suggest"
    }

    async fn seed(&self, project_id: i32, context_id: &str) -> Option<ConversationSeed> {
        // The context id is the project itself; reject a mismatch rather than
        // seeding one project's chat with another's rules.
        let ctx_project: i32 = context_id.parse().ok()?;
        if ctx_project != project_id {
            return None;
        }

        let mut ctx = String::new();
        ctx.push_str(SYSTEM_PREAMBLE);
        ctx.push_str("\n\n");
        ctx.push_str(crate::provider::TOOL_USAGE_GUIDANCE);

        let metadata = serde_json::json!({
            "project_id": project_id,
        });

        Some(ConversationSeed {
            system: ctx,
            first_assistant: None,
            title: Some("Suggest alerts".to_string()),
            metadata: Some(metadata),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A project id in the path that disagrees with the context id must not
    /// seed — otherwise the context id becomes a way to read another project's
    /// rule names through a chat opened on a project you do have access to.
    #[tokio::test]
    async fn seed_rejects_mismatched_project_and_context_id() {
        let provider = AlertSuggestChatProvider::new();

        assert!(provider.seed(7, "9").await.is_none());
        assert!(provider.seed(7, "not-a-number").await.is_none());
    }

    #[tokio::test]
    async fn seed_requires_authenticated_complete_rule_lookup() {
        let provider = AlertSuggestChatProvider::new();

        let seed = provider.seed(7, "7").await.expect("seeds");
        assert!(seed.system.contains("temps alerts list_alerts"));
        assert!(seed.system.contains("every page"));
        assert_eq!(seed.metadata.unwrap()["project_id"], 7);
    }

    /// The backtest step must be unconditional.
    ///
    /// It used to read "for an anomaly detector, backtest it" — and since every
    /// rule the model actually proposes is a static threshold, that let it skip
    /// the step entirely while still writing "backtested" in its answer.
    /// `preview_alert` now accepts static detectors, so there is no rule it
    /// cannot check.
    #[tokio::test]
    async fn seed_requires_backtesting_every_rule_including_static() {
        let provider = AlertSuggestChatProvider::new();
        let seed = provider.seed(7, "7").await.expect("seeds");

        assert!(seed.system.contains("preview_alert"));
        assert!(
            seed.system.contains("static thresholds included"),
            "the instruction must not be scoped to anomaly detectors"
        );
        assert!(
            seed.system.contains("Never claim a rule was backtested"),
            "a model that skipped the call must not be able to imply it didn't"
        );
    }
}
