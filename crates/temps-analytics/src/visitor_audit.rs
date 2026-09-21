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
use std::sync::{Arc, Mutex};
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
/// the enrichment itself is refused (HTTP 429) once a token has `max_per_window`
/// enrichments in flight or completed inside `window`, so the number of audit
/// rows one token can create is bounded. (The audit write itself stays
/// best-effort: a failing audit insert is logged and does not undo the change.)
///
/// A request reserves a slot atomically *before* it writes ([`Self::try_reserve`]),
/// so concurrent requests cannot all pass a check and then all be counted. The
/// reservation is kept only if the request changed a visitor
/// ([`BudgetReservation::commit`]); dropping it releases the slot. That way an
/// app that re-sends the same identity on every page load (no change) is
/// unaffected. Once the budget is spent every enrich call from that token is
/// refused until the window rolls over, including calls that would have changed
/// nothing. It is a per-process, fixed-window counter meant to bound audit
/// volume, not a security-grade rate limiter. Memory is bounded by the number of
/// tokens seen inside one window.
pub struct EnrichWriteBudget {
    windows: Mutex<HashMap<i32, WindowState>>,
    max_per_window: u32,
    window: Duration,
}

/// One token's counters for the current window.
struct WindowState {
    started: Instant,
    used: u32,
    /// Whether a refusal in this window has already been reported.
    refusal_reported: bool,
}

/// Why a reservation was refused.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BudgetRefused {
    /// The first refusal of this window: the caller should log it. Later
    /// refusals in the same window are silent so a flood cannot flood the log.
    pub first_in_window: bool,
}

/// A slot in a token's budget, held while its request runs.
#[must_use = "dropping a reservation releases its slot; call commit() when the request changed a visitor"]
pub struct BudgetReservation {
    budget: Arc<EnrichWriteBudget>,
    token_id: i32,
    window_start: Instant,
    committed: bool,
}

impl BudgetReservation {
    /// Keep the slot: the request changed a visitor.
    pub fn commit(mut self) {
        self.committed = true;
    }
}

impl Drop for BudgetReservation {
    fn drop(&mut self) {
        if !self.committed {
            self.budget.release(self.token_id, self.window_start);
        }
    }
}

impl EnrichWriteBudget {
    pub fn new(max_per_window: u32, window: Duration) -> Self {
        Self {
            windows: Mutex::new(HashMap::new()),
            max_per_window,
            window,
        }
    }

    /// Atomically take a slot for `token_id`, or refuse when its budget for the
    /// current window is used up (by completed changes or requests in flight).
    pub fn try_reserve(
        self: &Arc<Self>,
        token_id: i32,
    ) -> Result<BudgetReservation, BudgetRefused> {
        let now = Instant::now();
        // The map only holds counters, so a poisoned lock is still usable.
        let mut windows = self.windows.lock().unwrap_or_else(|e| e.into_inner());
        if windows.len() > 4096 {
            windows.retain(|_, state| now.duration_since(state.started) < self.window);
        }
        let state = windows.entry(token_id).or_insert(WindowState {
            started: now,
            used: 0,
            refusal_reported: false,
        });
        if now.duration_since(state.started) >= self.window {
            *state = WindowState {
                started: now,
                used: 0,
                refusal_reported: false,
            };
        }
        if state.used >= self.max_per_window {
            let first_in_window = !state.refusal_reported;
            state.refusal_reported = true;
            return Err(BudgetRefused { first_in_window });
        }
        state.used += 1;
        Ok(BudgetReservation {
            budget: Arc::clone(self),
            token_id,
            window_start: state.started,
            committed: false,
        })
    }

    /// Give back a slot taken in the window that started at `window_start`. A
    /// window that has since rolled over already forgot the reservation.
    fn release(&self, token_id: i32, window_start: Instant) {
        let mut windows = self.windows.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(state) = windows.get_mut(&token_id) {
            if state.started == window_start {
                state.used = state.used.saturating_sub(1);
            }
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
    /// Names of the top-level `custom_data` keys the request set, bounded by
    /// [`bounded_key_names`].
    pub custom_data_keys: Vec<String>,
    /// How many keys the request set in total.
    pub custom_data_key_count: usize,
    /// Names of the keys the request removed (sent as `null`), bounded the same way.
    /// This records the request, not the effect: a key that was never present is
    /// still listed, so it is not proof the key existed.
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
        let budget = Arc::new(EnrichWriteBudget::new(2, Duration::from_secs(60)));
        let first = budget.try_reserve(1).expect("first slot");
        let second = budget.try_reserve(1).expect("second slot");
        first.commit();
        second.commit();
        assert!(
            budget.try_reserve(1).is_err(),
            "third change in the window is refused"
        );
        // Another token has its own budget.
        assert!(budget.try_reserve(2).is_ok());
    }

    #[test]
    fn dropping_a_reservation_releases_its_slot() {
        let budget = Arc::new(EnrichWriteBudget::new(1, Duration::from_secs(60)));
        // A request that changed nothing gives its slot back.
        drop(budget.try_reserve(1).expect("slot"));
        let kept = budget.try_reserve(1).expect("slot is free again");
        // While one is in flight the budget is spoken for.
        assert!(budget.try_reserve(1).is_err());
        kept.commit();
        assert!(budget.try_reserve(1).is_err());
    }

    #[test]
    fn concurrent_reservations_cannot_exceed_the_limit() {
        let budget = Arc::new(EnrichWriteBudget::new(10, Duration::from_secs(60)));
        let handles: Vec<_> = (0..64)
            .map(|_| {
                let budget = Arc::clone(&budget);
                std::thread::spawn(move || budget.try_reserve(1))
            })
            .collect();
        let granted: Vec<_> = handles
            .into_iter()
            .filter_map(|h| h.join().expect("thread").ok())
            .collect();
        assert_eq!(
            granted.len(),
            10,
            "exactly the limit is granted, never more"
        );
        drop(granted);
    }

    #[test]
    fn only_the_first_refusal_of_a_window_is_reported() {
        let budget = Arc::new(EnrichWriteBudget::new(1, Duration::from_secs(60)));
        budget.try_reserve(1).expect("slot").commit();
        assert_eq!(
            budget.try_reserve(1).err(),
            Some(BudgetRefused {
                first_in_window: true
            })
        );
        assert_eq!(
            budget.try_reserve(1).err(),
            Some(BudgetRefused {
                first_in_window: false
            })
        );
    }

    #[test]
    fn budget_resets_after_the_window() {
        let budget = Arc::new(EnrichWriteBudget::new(1, Duration::from_millis(20)));
        budget.try_reserve(1).expect("slot").commit();
        assert!(budget.try_reserve(1).is_err());
        std::thread::sleep(Duration::from_millis(40));
        assert!(budget.try_reserve(1).is_ok());
    }

    #[test]
    fn releasing_after_the_window_rolled_does_not_free_the_new_windows_slot() {
        let budget = Arc::new(EnrichWriteBudget::new(1, Duration::from_millis(20)));
        let stale = budget.try_reserve(1).expect("slot");
        std::thread::sleep(Duration::from_millis(40));
        let current = budget.try_reserve(1).expect("new window");
        drop(stale); // belongs to the old window: must not touch the new count
        assert!(budget.try_reserve(1).is_err());
        current.commit();
    }
}
