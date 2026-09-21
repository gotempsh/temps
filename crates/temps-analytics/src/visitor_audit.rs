// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Audit operation for visitor enrichment writes.
//!
//! A deployment token can now write `custom_data` onto its own project's
//! visitors, so every write that actually changes a visitor is audited. Only
//! the *names* of the keys that were set are recorded, never their values:
//! enrichment routinely carries personal data (a user's name or email), and the
//! audit trail must not become a second copy of it.

use anyhow::Result;
use serde::Serialize;
use std::collections::HashMap;
use std::sync::Mutex;
use std::time::{Duration, Instant};
use temps_core::{AuditContext, AuditOperation};

/// Most key names recorded per audit entry.
pub const MAX_AUDITED_KEYS: usize = 16;
/// Longest key name recorded, in characters. Key names are caller-chosen, so
/// they are truncated rather than trusted to be short.
pub const MAX_AUDITED_KEY_CHARS: usize = 64;

/// Bound caller-chosen key names for the audit payload: at most
/// [`MAX_AUDITED_KEYS`] names of at most [`MAX_AUDITED_KEY_CHARS`] characters,
/// plus the true total so nothing is silently hidden.
pub fn bounded_key_names<'a>(keys: impl Iterator<Item = &'a String>) -> (Vec<String>, usize) {
    let mut total = 0;
    let mut names = Vec::new();
    for key in keys {
        total += 1;
        if names.len() < MAX_AUDITED_KEYS {
            names.push(key.chars().take(MAX_AUDITED_KEY_CHARS).collect());
        }
    }
    (names, total)
}

/// Bounds how many visitor-changing enrichments one deployment token can make.
///
/// Every change is audited, so the audit trail is only as bounded as the writes
/// behind it. A leaked token could otherwise flip a value back and forth and
/// create an audit row per request. Rather than drop audit rows past a budget,
/// the enrichment itself is refused (HTTP 429) once a token has made
/// `max_per_window` changes inside `window`: no write ever goes unaudited.
/// Only writes that change data are counted, so an app that re-sends the same
/// identity on every page load is unaffected. Memory is bounded by the number
/// of tokens seen inside one window.
pub struct EnrichWriteBudget {
    windows: Mutex<HashMap<i32, (Instant, u32)>>,
    max_per_window: u32,
    window: Duration,
}

impl EnrichWriteBudget {
    pub fn new(max_per_window: u32, window: Duration) -> Self {
        Self {
            windows: Mutex::new(HashMap::new()),
            max_per_window,
            window,
        }
    }

    /// Whether `token_id` may still make a visitor-changing enrichment now.
    pub fn has_budget(&self, token_id: i32) -> bool {
        let now = Instant::now();
        // The map only holds counters, so a poisoned lock is still usable.
        let windows = self.windows.lock().unwrap_or_else(|e| e.into_inner());
        match windows.get(&token_id) {
            Some((started, used)) if now.duration_since(*started) < self.window => {
                *used < self.max_per_window
            }
            _ => true,
        }
    }

    /// Count one visitor-changing enrichment made by `token_id`.
    pub fn record(&self, token_id: i32) {
        let now = Instant::now();
        let mut windows = self.windows.lock().unwrap_or_else(|e| e.into_inner());
        if windows.len() > 4096 {
            windows.retain(|_, (started, _)| now.duration_since(*started) < self.window);
        }
        let entry = windows.entry(token_id).or_insert((now, 0));
        if now.duration_since(entry.0) >= self.window {
            *entry = (now, 0);
        }
        entry.1 = entry.1.saturating_add(1);
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct VisitorEnrichedAudit {
    pub context: AuditContext,
    /// `"deployment_token"` or `"user"` (session, API key or CLI token).
    pub actor_kind: &'static str,
    /// Set when the actor is a deployment token. A token has no `users` row,
    /// so its identity lives here rather than in `context.user_id`.
    pub deployment_token_id: Option<i32>,
    pub deployment_token_name: Option<String>,
    /// The token's project; `None` for callers that are not project-scoped.
    pub project_id: Option<i32>,
    pub visitor_row_id: Option<i32>,
    /// Names of the top-level `custom_data` keys the request set, bounded by
    /// [`bounded_key_names`].
    pub custom_data_keys: Vec<String>,
    /// How many keys the request set in total.
    pub custom_data_key_count: usize,
    /// Names of the keys the request removed (sent as `null`), bounded the same way.
    pub removed_keys: Vec<String>,
    /// How many keys the request removed in total.
    pub removed_key_count: usize,
}

impl AuditOperation for VisitorEnrichedAudit {
    fn operation_type(&self) -> String {
        "VISITOR_ENRICHED".to_string()
    }

    /// `None` for deployment tokens: `context.user_id` is `0` for them and no
    /// such user exists, so recording it would violate the audit foreign key.
    fn user_id(&self) -> Option<i32> {
        (self.actor_kind == "user").then_some(self.context.user_id)
    }

    fn ip_address(&self) -> Option<String> {
        self.context.ip_address.clone()
    }

    fn user_agent(&self) -> &str {
        &self.context.user_agent
    }

    fn serialize(&self) -> Result<String> {
        serde_json::to_string(self)
            .map_err(|e| anyhow::anyhow!("Failed to serialize VISITOR_ENRICHED audit: {}", e))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn audit(actor_kind: &'static str) -> VisitorEnrichedAudit {
        VisitorEnrichedAudit {
            context: AuditContext {
                user_id: if actor_kind == "user" { 7 } else { 0 },
                ip_address: Some("203.0.113.9".to_string()),
                user_agent: "test".to_string(),
            },
            actor_kind,
            deployment_token_id: (actor_kind == "deployment_token").then_some(3),
            deployment_token_name: (actor_kind == "deployment_token").then(|| "app".to_string()),
            project_id: Some(5),
            visitor_row_id: Some(11),
            custom_data_keys: vec!["email".to_string()],
            custom_data_key_count: 1,
            removed_keys: Vec::new(),
            removed_key_count: 0,
        }
    }

    #[test]
    fn deployment_token_actor_has_no_user_id() {
        assert_eq!(audit("deployment_token").user_id(), None);
        assert_eq!(audit("user").user_id(), Some(7));
    }

    #[test]
    fn payload_names_keys_but_carries_no_values() {
        let json = AuditOperation::serialize(&audit("deployment_token")).unwrap();
        assert!(json.contains("\"custom_data_keys\":[\"email\"]"));
        assert_eq!(audit("user").operation_type(), "VISITOR_ENRICHED");
    }

    #[test]
    fn key_names_are_truncated_and_counted() {
        let keys: Vec<String> = (0..40)
            .map(|i| format!("{}{}", "k".repeat(500), i))
            .collect();
        let (names, total) = bounded_key_names(keys.iter());
        assert_eq!(total, 40);
        assert_eq!(names.len(), MAX_AUDITED_KEYS);
        assert!(names
            .iter()
            .all(|n| n.chars().count() <= MAX_AUDITED_KEY_CHARS));
    }

    #[test]
    fn budget_allows_up_to_the_limit_then_refuses_per_token() {
        let budget = EnrichWriteBudget::new(2, Duration::from_secs(60));
        assert!(budget.has_budget(1));
        budget.record(1);
        assert!(budget.has_budget(1));
        budget.record(1);
        assert!(
            !budget.has_budget(1),
            "third change in the window is refused"
        );
        // Another token has its own budget.
        assert!(budget.has_budget(2));
    }

    #[test]
    fn budget_resets_after_the_window() {
        let budget = EnrichWriteBudget::new(1, Duration::from_millis(20));
        budget.record(1);
        assert!(!budget.has_budget(1));
        std::thread::sleep(Duration::from_millis(40));
        assert!(budget.has_budget(1));
    }
}
