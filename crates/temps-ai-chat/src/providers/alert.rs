//! The metric-alert context provider (ADR-023, P2).
//!
//! Seeds a chat to investigate a metric alert and make it actionable — turning
//! "metric X is anomalous" into "here's what it means, why it's firing, and what
//! to do." `context_id` is the alert rule's integer id; one resumable chat per
//! rule.

use std::sync::Arc;

use async_trait::async_trait;
use sea_orm::{DatabaseConnection, EntityTrait};

use temps_entities::metric_alert_rules;

use crate::provider::{ConversationContextProvider, ConversationSeed};

const SYSTEM_PREAMBLE: &str = "You are a senior SRE helping a developer understand and act on a Temps metric alert. \
Use the alert context below. Explain in plain language what the alert means and the likely reasons it is firing, \
then give concrete, prioritized next steps to investigate and resolve it (what to check, where, in what order). \
Ground everything in the provided facts; ask a brief clarifying question only when essential. Be concise and \
practical.";

/// Seeds metric-alert investigation chats.
pub struct AlertChatProvider {
    db: Arc<DatabaseConnection>,
}

impl AlertChatProvider {
    pub fn new(db: Arc<DatabaseConnection>) -> Self {
        Self { db }
    }
}

#[async_trait]
impl ConversationContextProvider for AlertChatProvider {
    fn context_type(&self) -> &'static str {
        "alert"
    }

    async fn seed(&self, project_id: i32, context_id: &str) -> Option<ConversationSeed> {
        let rule_id: i32 = context_id.parse().ok()?;
        let rule = metric_alert_rules::Entity::find_by_id(rule_id)
            .one(self.db.as_ref())
            .await
            .ok()??;
        if rule.project_id != project_id {
            return None;
        }

        let mut ctx = String::new();
        ctx.push_str(SYSTEM_PREAMBLE);
        ctx.push_str("\n\n--- Alert context ---\n");
        ctx.push_str(&format!("Alert: {}\n", rule.name));
        ctx.push_str(&format!(
            "Metric: {} (aggregation: {})\n",
            rule.metric_name, rule.aggregation
        ));
        ctx.push_str(&format!("Detection: {}\n", rule.detection_kind));
        ctx.push_str(&format!("Severity: {}\n", rule.severity));
        ctx.push_str(&format!("Current state: {}\n", rule.last_state));
        if let Some(v) = rule.last_value {
            ctx.push_str(&format!("Latest evaluated value: {v:.3}\n"));
        }
        ctx.push_str(&format!("Evaluation window: {}s\n", rule.window_secs));

        // Detector parameters live in the detection_config JSONB (flat keys for
        // static threshold / anomaly sensitivity). Best-effort extraction.
        let dc = &rule.detection_config;
        if let Some(t) = dc.get("threshold").and_then(|v| v.as_f64()) {
            let cmp = dc.get("comparator").and_then(|v| v.as_str()).unwrap_or(">");
            ctx.push_str(&format!("Static threshold: value {cmp} {t}\n"));
        }
        if let Some(d) = dc.get("deviations").and_then(|v| v.as_f64()) {
            ctx.push_str(&format!(
                "Anomaly band: ±{d} standard deviations from the learned baseline\n"
            ));
        }

        let metadata = serde_json::json!({
            "rule_id": rule_id,
            "metric": rule.metric_name,
            "state": rule.last_state,
        });

        Some(ConversationSeed {
            system: ctx,
            first_assistant: None,
            title: Some(format!("Investigate alert: {}", rule.name)),
            metadata: Some(metadata),
        })
    }
}
