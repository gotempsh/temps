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
//!    and what is already alerted on (seeded below, plus `list_alerts`).
//! 2. **Check.** For each candidate, query real values (`query_metrics`) and,
//!    for anomaly detectors, backtest with `preview_alert` — so a proposed
//!    threshold is grounded in the project's own history rather than a generic
//!    SRE rule of thumb.
//! 3. **Propose.** Stage each rule as a separate `temps_write create_alert`
//!    action so the human confirms them one at a time.
//!
//! `context_id` is the project id (as a string): one resumable suggestion chat
//! per project.

use std::sync::Arc;

use async_trait::async_trait;
use sea_orm::{ColumnTrait, DatabaseConnection, EntityTrait, QueryFilter, QueryOrder, QuerySelect};

use temps_entities::metric_alert_rules;

use crate::provider::{ConversationContextProvider, ConversationSeed};

/// Cap on how many existing rules are listed in the seed. A project with more
/// alerts than this does not need suggestions, and the list is framing, not
/// data — the model can always page through `list_alerts` itself.
const MAX_SEEDED_RULES: u64 = 50;

const SYSTEM_PREAMBLE: &str = "You are a senior SRE helping a developer decide what this project should be alerted on. \
Your job is to propose a small set of metric alert rules that are worth having, and to justify each one with evidence \
from the project's own telemetry — never with generic rules of thumb.

## How to work

1. FIRST, look at what exists. The rules already configured are listed below. Use `temps alerts list_alerts` if you \
need their full detail. NEVER propose a rule that duplicates one that already exists — if an existing rule is close \
but badly tuned, say so and propose an update instead of a second rule.
2. THEN, find out what this project actually emits: `temps telemetry list_metric_names`. Do not guess metric names; \
a rule on a metric that is never reported will never fire and is worse than no rule at all.
3. FOR EACH candidate, ground the threshold in real data. Query the metric's recent values \
(`temps telemetry query_metrics`) to see its normal range, and check EVERY metric you intend to mention — never \
claim a metric has no data without having queried it. For an anomaly detector, backtest it by calling \
`temps alerts preview_alert`, which is read-only and reports how often the rule WOULD have fired over the last \
week; eyeballing the query output is not a backtest. A rule that would have fired constantly is noise; a rule that \
would never have fired may be pointless.
4. FINALLY, propose — by CALLING the `temps_write` tool, once per rule, so the human can accept some and reject \
others. This is a real tool call, not something you write out: printing a `temps_write …` command inside a code \
block does NOTHING — no rule is proposed, and the user sees a wall of text with no button to accept. If you \
described a rule in your answer, you must also have called the tool for it. Do NOT propose a rule you could not \
ground in step 3.

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
pub struct AlertSuggestChatProvider {
    db: Arc<DatabaseConnection>,
}

impl AlertSuggestChatProvider {
    pub fn new(db: Arc<DatabaseConnection>) -> Self {
        Self { db }
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

        let existing = metric_alert_rules::Entity::find()
            .filter(metric_alert_rules::Column::ProjectId.eq(project_id))
            .order_by_asc(metric_alert_rules::Column::Id)
            .limit(MAX_SEEDED_RULES)
            .all(self.db.as_ref())
            .await
            .ok()?;

        let mut ctx = String::new();
        ctx.push_str(SYSTEM_PREAMBLE);
        ctx.push_str("\n\n");
        ctx.push_str(crate::provider::TOOL_USAGE_GUIDANCE);

        ctx.push_str("\n\n--- Existing metric alert rules ---\n");
        if existing.is_empty() {
            ctx.push_str(
                "None. This project has no metric alert rules at all, so nothing about it is \
                 currently being watched.\n",
            );
        } else {
            for rule in &existing {
                ctx.push_str(&format!(
                    "- {} — metric `{}` ({}), detector: {}, severity: {}, window {}s{}\n",
                    rule.name,
                    rule.metric_name,
                    rule.aggregation,
                    rule.detection_kind,
                    rule.severity,
                    rule.window_secs,
                    if rule.enabled { "" } else { " [DISABLED]" },
                ));
            }
            ctx.push_str(
                "\nThese are already covered. Propose only rules that add something these do not.\n",
            );
        }

        let metadata = serde_json::json!({
            "project_id": project_id,
            "existing_rule_count": existing.len(),
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
    use sea_orm::{DatabaseBackend, MockDatabase};

    fn rule(id: i32, name: &str, metric: &str, enabled: bool) -> metric_alert_rules::Model {
        metric_alert_rules::Model {
            id,
            project_id: 7,
            environment_id: None,
            name: name.to_string(),
            metric_name: metric.to_string(),
            aggregation: "avg".to_string(),
            detection_kind: "static".to_string(),
            detection_config: serde_json::json!({"kind": "static"}),
            label_filters: serde_json::json!([]),
            group_by: serde_json::json!([]),
            dynamic_alerts: false,
            max_series: 20,
            grouped_notification_threshold: 5,
            window_secs: 300,
            for_duration_secs: 60,
            severity: "warning".to_string(),
            enabled,
            last_state: "ok".to_string(),
            last_value: None,
            series_states: serde_json::json!({}),
            last_dropped_series_count: 0,
            last_evaluated_at: None,
            created_at: chrono::Utc::now(),
            updated_at: chrono::Utc::now(),
        }
    }

    /// A project id in the path that disagrees with the context id must not
    /// seed — otherwise the context id becomes a way to read another project's
    /// rule names through a chat opened on a project you do have access to.
    #[tokio::test]
    async fn seed_rejects_mismatched_project_and_context_id() {
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results([Vec::<metric_alert_rules::Model>::new()])
            .into_connection();
        let provider = AlertSuggestChatProvider::new(Arc::new(db));

        assert!(provider.seed(7, "9").await.is_none());
        assert!(provider.seed(7, "not-a-number").await.is_none());
    }

    /// The empty case has to be stated explicitly rather than left blank: an
    /// empty section reads as "unknown" to the model, and it would go on to
    /// re-derive what is already covered.
    #[tokio::test]
    async fn seed_states_plainly_when_no_rules_exist() {
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results([Vec::<metric_alert_rules::Model>::new()])
            .into_connection();
        let provider = AlertSuggestChatProvider::new(Arc::new(db));

        let seed = provider.seed(7, "7").await.expect("seeds");
        assert!(seed.system.contains("no metric alert rules at all"));
        assert_eq!(seed.metadata.unwrap()["existing_rule_count"], 0);
    }

    #[tokio::test]
    async fn seed_lists_existing_rules_and_flags_disabled_ones() {
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results([vec![
                rule(1, "API p95 latency", "http.server.duration", true),
                rule(2, "Old CPU rule", "system.cpu.utilization", false),
            ]])
            .into_connection();
        let provider = AlertSuggestChatProvider::new(Arc::new(db));

        let seed = provider.seed(7, "7").await.expect("seeds");
        assert!(seed.system.contains("API p95 latency"));
        assert!(seed.system.contains("http.server.duration"));
        assert!(
            seed.system.contains("[DISABLED]"),
            "a disabled rule must be marked — otherwise the model treats a \
             switched-off rule as active coverage and skips proposing one"
        );
        assert_eq!(seed.metadata.unwrap()["existing_rule_count"], 2);
    }
}
