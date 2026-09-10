// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

use std::collections::HashMap;
use std::sync::Mutex;
use std::time::{Duration, Instant};
use uuid::Uuid;

/// How long a proposal token remains valid before it expires.
pub const PROPOSAL_TTL: Duration = Duration::from_secs(5 * 60);

/// A pending write action awaiting human confirmation.
///
/// Created by a write tool (e.g. `trigger_deployment`); consumed exactly once
/// by `confirm_action`.  Expired tokens are rejected.
#[derive(Debug)]
pub struct Proposal {
    pub token: String,
    pub tool_name: String,
    pub arguments: serde_json::Value,
    pub created_at: Instant,
}

/// In-memory store for pending proposals.  Shared across all MCP connections
/// via `Arc<ProposalStore>` inside `McpHandlerState`.
///
/// Uses a `Mutex<HashMap>` because:
/// - Proposal operations are infrequent (human-facing flow, not hot path).
/// - `Mutex` avoids the need for an additional async dependency just for a
///   simple map.
pub struct ProposalStore {
    proposals: Mutex<HashMap<String, Proposal>>,
}

impl ProposalStore {
    pub fn new() -> Self {
        Self {
            proposals: Mutex::new(HashMap::new()),
        }
    }

    /// Acquire the inner mutex, recovering gracefully from lock poisoning.
    fn lock_store(&self) -> std::sync::MutexGuard<'_, HashMap<String, Proposal>> {
        match self.proposals.lock() {
            Ok(guard) => guard,
            // If a thread panicked while holding the lock we still get the
            // data — the map itself is consistent; only the thread is gone.
            Err(poisoned) => poisoned.into_inner(),
        }
    }

    /// Store a new proposal and return its token.
    ///
    /// Opportunistically sweeps expired proposals on every `create()` call.
    /// This is a human-facing, low-volume flow (propose-then-confirm), so an
    /// O(n) scan per call is acceptable and avoids unbounded memory growth
    /// from proposals that were created but never confirmed.
    pub fn create(&self, tool_name: String, arguments: serde_json::Value) -> String {
        let token = Uuid::new_v4().to_string();
        let proposal = Proposal {
            token: token.clone(),
            tool_name,
            arguments,
            created_at: Instant::now(),
        };
        let mut store = self.lock_store();
        // Sweep expired entries before inserting so stale proposals don't
        // accumulate indefinitely.
        store.retain(|_, p| p.created_at.elapsed() <= PROPOSAL_TTL);
        store.insert(token.clone(), proposal);
        token
    }

    /// Peek at the proposal identified by `token` without consuming it.
    ///
    /// Returns the same data as [`take`] (tool name and arguments) after the
    /// same validity checks, but does NOT remove the entry from the map.
    ///
    /// An expired proposal is still removed opportunistically and
    /// `Err(ProposalTakeError::Expired)` is returned — expiry is not an
    /// "awaiting authorisation" state; the proposal is simply gone.
    ///
    /// Use this to extract arguments for an access check before committing
    /// to consuming the token via [`take`].  A denied access check that calls
    /// only `peek` leaves the token intact so the legitimate confirmer can retry.
    pub fn peek(&self, token: &str) -> Result<TakenProposal, ProposalTakeError> {
        let mut store = self.lock_store();

        let proposal = store.get(token).ok_or(ProposalTakeError::NotFound)?;

        if proposal.created_at.elapsed() > PROPOSAL_TTL {
            // Remove expired entries opportunistically, same as take().
            store.remove(token);
            return Err(ProposalTakeError::Expired);
        }

        // Clone the data out — we hold the lock throughout and do NOT remove
        // the entry.  The proposal remains available for a subsequent take().
        Ok(TakenProposal {
            tool_name: proposal.tool_name.clone(),
            arguments: proposal.arguments.clone(),
        })
    }

    /// Consume the proposal identified by `token`.
    ///
    /// Returns `Err(ProposalTakeError::NotFound)` when the token is unknown,
    /// and `Err(ProposalTakeError::Expired)` when the TTL has elapsed.
    /// On success the proposal is removed from the store (single-use).
    pub fn take(&self, token: &str) -> Result<TakenProposal, ProposalTakeError> {
        let mut store = self.lock_store();

        let proposal = store.get(token).ok_or(ProposalTakeError::NotFound)?;

        if proposal.created_at.elapsed() > PROPOSAL_TTL {
            store.remove(token);
            return Err(ProposalTakeError::Expired);
        }

        // Remove on success — proposals are single-use.  A subsequent call
        // with the same token returns NotFound, identical to an unknown token,
        // which is correct: after consumption the token has no further meaning.
        //
        // We hold the mutex throughout (no TOCTOU), so the remove() cannot
        // fail in practice — but we handle it gracefully rather than panicking.
        if let Some(proposal) = store.remove(token) {
            Ok(TakenProposal {
                tool_name: proposal.tool_name,
                arguments: proposal.arguments,
            })
        } else {
            Err(ProposalTakeError::NotFound)
        }
    }
}

impl Default for ProposalStore {
    fn default() -> Self {
        Self::new()
    }
}

/// The data extracted from a proposal on successful consumption.
#[derive(Debug)]
pub struct TakenProposal {
    pub tool_name: String,
    pub arguments: serde_json::Value,
}

/// Error returned by [`ProposalStore::take`].
#[derive(Debug)]
pub enum ProposalTakeError {
    NotFound,
    Expired,
}

impl From<ProposalTakeError> for crate::error::McpError {
    fn from(e: ProposalTakeError) -> Self {
        match e {
            ProposalTakeError::NotFound => crate::error::McpError::ProposalNotFound,
            ProposalTakeError::Expired => crate::error::McpError::ProposalExpired,
        }
    }
}

#[cfg(test)]
impl ProposalStore {
    /// Backdates the `created_at` field of the named proposal by `age`.
    ///
    /// This is test scaffolding only: it allows tests to simulate TTL
    /// expiry without actually sleeping.  The `created_at` field is set
    /// with `Instant::now()` in production and has no injectable clock —
    /// this helper is the ONLY way a test can reach the `Expired` branch.
    pub(crate) fn backdate(&self, token: &str, age: Duration) {
        let mut store = self.lock_store();
        if let Some(p) = store.get_mut(token) {
            // `Instant` arithmetic: subtract `age` from `created_at` by
            // computing a new `Instant` that is `age` before `now()` and
            // then adjusting for the elapsed time since creation.
            let elapsed = p.created_at.elapsed();
            // We want the new created_at to be `now - age`, i.e. elapsed
            // will read as `age` when take() calls `.elapsed()`.  Since we
            // can't set an Instant directly, subtract from now minus the
            // requested age plus any time already on the clock.
            if let Some(new_created_at) = Instant::now().checked_sub(age + elapsed) {
                p.created_at = new_created_at;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── peek() tests ─────────────────────────────────────────────────────────

    #[test]
    fn peek_returns_data_without_consuming() {
        let store = ProposalStore::new();
        let args = serde_json::json!({ "project_id": 1 });
        let token = store.create("trigger_deployment".to_string(), args.clone());

        // peek() must return the data ...
        let peeked = store.peek(&token).expect("peek must succeed");
        assert_eq!(peeked.tool_name, "trigger_deployment");
        assert_eq!(peeked.arguments, args);

        // ... without removing the entry — take() must still succeed.
        let taken = store.take(&token).expect("take after peek must succeed");
        assert_eq!(taken.tool_name, "trigger_deployment");

        // Now it's consumed — a second take must fail.
        store.take(&token).expect_err("second take must fail");
    }

    #[test]
    fn peek_expired_proposal_returns_expired_and_cleans_up() {
        let store = ProposalStore::new();
        let token = store.create("trigger_deployment".to_string(), serde_json::json!({}));

        // Backdate past the TTL.
        store.backdate(&token, PROPOSAL_TTL + Duration::from_secs(1));

        // peek() on an expired proposal must reject it (Expired) ...
        let err = store
            .peek(&token)
            .expect_err("peek on expired token must fail");
        assert!(matches!(err, ProposalTakeError::Expired));

        // ... and must clean it up so a subsequent take() sees NotFound, not Expired.
        let err2 = store
            .take(&token)
            .expect_err("take after expired peek must fail");
        assert!(matches!(err2, ProposalTakeError::NotFound));
    }

    #[test]
    fn peek_unknown_token_returns_not_found() {
        let store = ProposalStore::new();
        let err = store
            .peek("no-such-token")
            .expect_err("unknown token must fail");
        assert!(matches!(err, ProposalTakeError::NotFound));
    }

    // ── take() / create() / general tests ────────────────────────────────────

    #[test]
    fn create_and_take_proposal() {
        let store = ProposalStore::new();
        let args = serde_json::json!({ "project_id": 1 });
        let token = store.create("trigger_deployment".to_string(), args.clone());

        let taken = store.take(&token).expect("should consume proposal");
        assert_eq!(taken.tool_name, "trigger_deployment");
        assert_eq!(taken.arguments, args);
    }

    #[test]
    fn take_consumed_proposal_returns_not_found() {
        let store = ProposalStore::new();
        let token = store.create("trigger_deployment".to_string(), serde_json::json!({}));

        let _ = store.take(&token).expect("first take must succeed");
        let err = store.take(&token).expect_err("second take must fail");
        assert!(matches!(err, ProposalTakeError::NotFound));
    }

    #[test]
    fn take_unknown_token_returns_not_found() {
        let store = ProposalStore::new();
        let err = store
            .take("no-such-token")
            .expect_err("unknown token must fail");
        assert!(matches!(err, ProposalTakeError::NotFound));
    }

    #[test]
    fn take_expired_proposal_returns_expired() {
        let store = ProposalStore::new();
        let token = store.create("trigger_deployment".to_string(), serde_json::json!({}));

        // Backdate by more than the TTL so take() sees it as expired.
        store.backdate(&token, PROPOSAL_TTL + Duration::from_secs(1));

        let err = store.take(&token).expect_err("expired token must fail");
        assert!(matches!(err, ProposalTakeError::Expired));
    }

    #[test]
    fn create_sweeps_expired_proposals() {
        let store = ProposalStore::new();

        // Create a proposal and expire it.
        let old_token = store.create("trigger_deployment".to_string(), serde_json::json!({}));
        store.backdate(&old_token, PROPOSAL_TTL + Duration::from_secs(1));

        // A fresh create() must sweep the expired entry.
        let _new_token = store.create("trigger_deployment".to_string(), serde_json::json!({}));

        // The expired token is no longer in the store (swept by the second
        // create) — so take() returns NotFound, not Expired.
        let err = store.take(&old_token).expect_err("swept token must fail");
        assert!(matches!(err, ProposalTakeError::NotFound));
    }

    #[test]
    fn consumed_proposal_is_removed_from_store() {
        let store = ProposalStore::new();
        let token = store.create("trigger_deployment".to_string(), serde_json::json!({}));

        // Consume it.
        let _ = store.take(&token).expect("first take must succeed");

        // Trigger a create() to flush any retained entries.
        let _other = store.create("trigger_deployment".to_string(), serde_json::json!({}));

        // The original token is gone (removed on consumption, not just flagged).
        let err = store
            .take(&token)
            .expect_err("consumed token must not be found");
        assert!(matches!(err, ProposalTakeError::NotFound));
    }
}
