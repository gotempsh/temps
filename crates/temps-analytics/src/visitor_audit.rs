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

/// What to do with the audit entry for one enrichment.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AuditDecision {
    /// Write it.
    Record,
    /// Over the per-token budget: drop it. Log the first drop of a window.
    SuppressFirst,
    /// Over the per-token budget: drop it silently.
    Suppress,
}

/// Bounds how many enrichment audit rows one deployment token can create.
///
/// A leaked token could otherwise flip a value back and forth and write one
/// audit row per request. Beyond the budget the entry is dropped (the
/// enrichment itself still happens) and the first drop of each window is
/// logged, so the gap is visible instead of silent. Memory is bounded by the
/// number of tokens seen inside one window.
pub struct AuditThrottle {
    windows: Mutex<HashMap<i32, (Instant, u32)>>,
    max_per_window: u32,
    window: Duration,
}

impl AuditThrottle {
    pub fn new(max_per_window: u32, window: Duration) -> Self {
        Self {
            windows: Mutex::new(HashMap::new()),
            max_per_window,
            window,
        }
    }

    pub fn decide(&self, token_id: i32) -> AuditDecision {
        let now = Instant::now();
        // The map only holds counters, so a poisoned lock is still usable.
        let mut windows = self.windows.lock().unwrap_or_else(|e| e.into_inner());
        if windows.len() > 4096 {
            windows.retain(|_, (started, _)| now.duration_since(*started) < self.window);
        }
        let entry = windows.entry(token_id).or_insert((now, 0));
        if now.duration_since(entry.0) >= self.window {
            *entry = (now, 0);
        }
        entry.1 = entry.1.saturating_add(1);
        match entry.1 {
            n if n <= self.max_per_window => AuditDecision::Record,
            n if n == self.max_per_window.saturating_add(1) => AuditDecision::SuppressFirst,
            _ => AuditDecision::Suppress,
        }
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
    /// Names of the top-level `custom_data` keys that were written or removed,
    /// bounded by [`bounded_key_names`].
    pub custom_data_keys: Vec<String>,
    /// How many keys the request touched in total.
    pub custom_data_key_count: usize,
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
    fn throttle_records_within_budget_then_suppresses_per_token() {
        let throttle = AuditThrottle::new(2, Duration::from_secs(60));
        assert_eq!(throttle.decide(1), AuditDecision::Record);
        assert_eq!(throttle.decide(1), AuditDecision::Record);
        assert_eq!(throttle.decide(1), AuditDecision::SuppressFirst);
        assert_eq!(throttle.decide(1), AuditDecision::Suppress);
        // Another token has its own budget.
        assert_eq!(throttle.decide(2), AuditDecision::Record);
    }

    #[test]
    fn throttle_budget_resets_after_the_window() {
        let throttle = AuditThrottle::new(1, Duration::from_millis(20));
        assert_eq!(throttle.decide(1), AuditDecision::Record);
        assert_eq!(throttle.decide(1), AuditDecision::SuppressFirst);
        std::thread::sleep(Duration::from_millis(40));
        assert_eq!(throttle.decide(1), AuditDecision::Record);
    }
}
