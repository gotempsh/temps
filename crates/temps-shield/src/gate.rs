// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::Arc;
use std::time::Duration;

use moka::sync::Cache;

use crate::identity::VisitorIdentity;
use crate::keys::ShieldKeys;
use crate::token::{
    check_work, is_fresh, open_challenge, open_clearance, seal_challenge, seal_clearance,
    ChallengeClaims, ChallengeError, ClearanceClaims, CHALLENGE_TTL_SECS, CLEARANCE_TTL_SECS,
    CLOCK_SKEW_SECS,
};

/// The clearance cookie. The `__Host-` prefix makes the browser require
/// `Secure`, `Path=/` and no `Domain`, so a sibling subdomain can neither
/// set nor shadow it.
pub const CLEARANCE_COOKIE_NAME: &str = "__Host-temps_clr";

/// Requests one clearance may make per hour on one ingress. Every request
/// counts, page assets included, so this is well above what one person
/// browsing produces.
pub const DEFAULT_BUDGET_PER_HOUR: u32 = 3_000;

/// Upper bound on entries in each in-memory set, so a flood of distinct
/// tokens or clearances cannot grow memory without limit.
const MAX_TRACKED: u64 = 200_000;
const BUDGET_WINDOW: Duration = Duration::from_secs(60 * 60);

/// What a request's clearance cookie allows.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Admission {
    /// A valid clearance for this environment and visitor, within budget.
    Admitted,
    /// No cookie, or one that is invalid, expired or for someone else.
    NotCleared,
    /// A valid clearance that has used up its requests for this hour.
    BudgetSpent,
}

/// Issues and verifies challenges and clearances for one ingress.
///
/// Verification needs no database: tokens are signed, and the only state is
/// two bounded in-memory sets, the tokens already redeemed (so one solution
/// mints one clearance per ingress) and the per-clearance request counters.
pub struct ChallengeGate {
    keys: ShieldKeys,
    redeemed: Cache<[u8; 16], ()>,
    budgets: Cache<[u8; 8], Arc<AtomicU32>>,
    budget_per_hour: u32,
}

impl ChallengeGate {
    pub fn new(keys: ShieldKeys) -> Self {
        Self::with_budget(keys, DEFAULT_BUDGET_PER_HOUR)
    }

    pub fn with_budget(keys: ShieldKeys, budget_per_hour: u32) -> Self {
        Self {
            keys,
            redeemed: Cache::builder()
                .max_capacity(MAX_TRACKED)
                .time_to_live(Duration::from_secs(CHALLENGE_TTL_SECS + 60))
                .build(),
            budgets: Cache::builder()
                .max_capacity(MAX_TRACKED)
                .time_to_live(BUDGET_WINDOW)
                .build(),
            budget_per_hour,
        }
    }

    /// A new challenge token for this visitor and environment.
    pub fn issue(
        &self,
        env_id: i32,
        identity: &VisitorIdentity,
        difficulty: u8,
        now: u64,
    ) -> Result<String, ChallengeError> {
        seal_challenge(
            &self.keys,
            &ChallengeClaims {
                env_id,
                identity: *identity,
                difficulty,
                issued_at: now,
                salt: rand::random(),
            },
        )
    }

    /// Verifies a solved challenge and returns a clearance cookie value.
    ///
    /// `env_id` and `identity` are what the ingress computed for this
    /// request; nothing in the request body is trusted. The checks run
    /// cheapest first: signature, expiry, environment, visitor, proof of
    /// work, and last whether the token was already redeemed.
    pub fn redeem(
        &self,
        token: &str,
        nonce: &str,
        env_id: i32,
        identity: &VisitorIdentity,
        now: u64,
    ) -> Result<String, ChallengeError> {
        let claims = open_challenge(&self.keys, token)?;
        if !is_fresh(claims.issued_at, CHALLENGE_TTL_SECS, now) {
            return Err(ChallengeError::Expired);
        }
        if claims.env_id != env_id {
            return Err(ChallengeError::WrongEnvironment);
        }
        if claims.identity != *identity {
            return Err(ChallengeError::WrongVisitor);
        }
        check_work(token, nonce, claims.difficulty)?;
        // `or_insert_with` coalesces concurrent callers on one key, so exactly
        // one sees a fresh entry. `or_insert` does not: racing submissions of
        // one solution would each mint a clearance.
        if !self
            .redeemed
            .entry(claims.salt)
            .or_insert_with(|| ())
            .is_fresh()
        {
            return Err(ChallengeError::Replayed);
        }
        seal_clearance(
            &self.keys,
            &ClearanceClaims {
                env_id,
                identity: *identity,
                issued_at: now,
                expires_at: now.saturating_add(CLEARANCE_TTL_SECS),
                budget_id: rand::random(),
            },
        )
    }

    /// Checks a clearance cookie value and, if valid, counts this request
    /// against its budget.
    pub fn admit(
        &self,
        cookie: &str,
        env_id: i32,
        identity: &VisitorIdentity,
        now: u64,
    ) -> Admission {
        let Ok(claims) = open_clearance(&self.keys, cookie) else {
            return Admission::NotCleared;
        };
        let valid = claims.env_id == env_id
            && claims.identity == *identity
            && claims.issued_at <= now.saturating_add(CLOCK_SKEW_SECS)
            && now < claims.expires_at;
        if !valid {
            return Admission::NotCleared;
        }
        let used = self
            .budgets
            .get_with(claims.budget_id, || Arc::new(AtomicU32::new(0)))
            .fetch_add(1, Ordering::Relaxed);
        if used < self.budget_per_hour {
            Admission::Admitted
        } else {
            Admission::BudgetSpent
        }
    }
}

/// The `Set-Cookie` value that stores a clearance.
pub fn clearance_set_cookie(value: &str) -> String {
    format!(
        "{CLEARANCE_COOKIE_NAME}={value}; Path=/; Max-Age={CLEARANCE_TTL_SECS}; Secure; HttpOnly; SameSite=Lax"
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::token::has_leading_zero_bits;
    use sha2::{Digest, Sha256};

    const NOW: u64 = 1_800_000_000;
    const DIFFICULTY: u8 = 8;

    fn gate_with_budget(budget: u32) -> ChallengeGate {
        ChallengeGate::with_budget(
            ShieldKeys::derive(|label| Sha256::digest(label.as_bytes()).into()),
            budget,
        )
    }

    fn gate() -> ChallengeGate {
        gate_with_budget(DEFAULT_BUDGET_PER_HOUR)
    }

    fn visitor(n: u8) -> VisitorIdentity {
        VisitorIdentity::compute(None, "Mozilla/5.0", &format!("tls:{n}"))
    }

    /// The same search the WASM solver does.
    fn solve(token: &str, difficulty: u8) -> String {
        (0u64..)
            .map(|nonce| nonce.to_string())
            .find(|nonce| {
                let hash = Sha256::digest(format!("{token}{nonce}").as_bytes());
                has_leading_zero_bits(&hash, u32::from(difficulty))
            })
            .unwrap_or_default()
    }

    fn clearance(gate: &ChallengeGate, env_id: i32, who: &VisitorIdentity) -> String {
        let token = gate.issue(env_id, who, DIFFICULTY, NOW).unwrap();
        let nonce = solve(&token, DIFFICULTY);
        gate.redeem(&token, &nonce, env_id, who, NOW + 1).unwrap()
    }

    #[test]
    fn a_solved_challenge_earns_a_clearance() {
        let gate = gate();
        let cookie = clearance(&gate, 1, &visitor(1));
        assert_eq!(
            gate.admit(&cookie, 1, &visitor(1), NOW + 2),
            Admission::Admitted
        );
    }

    /// Regression for the attack-mode bypass: the old endpoint accepted a
    /// challenge, nonce, environment and identifier all chosen by the client.
    /// A token the server did not sign, or an unsolved one, earns nothing.
    #[test]
    fn client_made_or_unsolved_tokens_earn_nothing() {
        let gate = gate();
        let who = visitor(1);
        assert_eq!(
            gate.redeem("anything", "0", 1, &who, NOW),
            Err(ChallengeError::Malformed)
        );
        let forged = "A".repeat(80);
        let nonce = solve(&forged, DIFFICULTY);
        assert!(gate.redeem(&forged, &nonce, 1, &who, NOW).is_err());

        let token = gate.issue(1, &who, 20, NOW).unwrap();
        let unsolved = (0u64..)
            .map(|n| n.to_string())
            .find(|nonce| {
                let hash = Sha256::digest(format!("{token}{nonce}").as_bytes());
                !has_leading_zero_bits(&hash, 20)
            })
            .unwrap();
        assert_eq!(
            gate.redeem(&token, &unsolved, 1, &who, NOW),
            Err(ChallengeError::InsufficientWork)
        );
    }

    #[test]
    fn a_token_is_bound_to_its_environment_and_visitor() {
        let gate = gate();
        let token = gate.issue(1, &visitor(1), DIFFICULTY, NOW).unwrap();
        let nonce = solve(&token, DIFFICULTY);
        assert_eq!(
            gate.redeem(&token, &nonce, 2, &visitor(1), NOW),
            Err(ChallengeError::WrongEnvironment)
        );
        assert_eq!(
            gate.redeem(&token, &nonce, 1, &visitor(2), NOW),
            Err(ChallengeError::WrongVisitor)
        );
    }

    #[test]
    fn a_token_expires_after_five_minutes() {
        let gate = gate();
        let token = gate.issue(1, &visitor(1), DIFFICULTY, NOW).unwrap();
        let nonce = solve(&token, DIFFICULTY);
        assert_eq!(
            gate.redeem(&token, &nonce, 1, &visitor(1), NOW + CHALLENGE_TTL_SECS),
            Err(ChallengeError::Expired)
        );
    }

    #[test]
    fn a_token_is_redeemed_once() {
        let gate = gate();
        let token = gate.issue(1, &visitor(1), DIFFICULTY, NOW).unwrap();
        let nonce = solve(&token, DIFFICULTY);
        assert!(gate.redeem(&token, &nonce, 1, &visitor(1), NOW).is_ok());
        assert_eq!(
            gate.redeem(&token, &nonce, 1, &visitor(1), NOW),
            Err(ChallengeError::Replayed)
        );
    }

    /// Redemption is one atomic insert into the redeemed set, so racing
    /// submissions of one solution mint exactly one clearance.
    #[test]
    fn concurrent_redemptions_of_one_token_succeed_once() {
        const THREADS: usize = 8;
        let gate = gate();
        let token = gate.issue(1, &visitor(1), DIFFICULTY, NOW).unwrap();
        let nonce = solve(&token, DIFFICULTY);
        let barrier = std::sync::Barrier::new(THREADS);
        let successes = std::thread::scope(|scope| {
            let handles: Vec<_> = (0..THREADS)
                .map(|_| {
                    scope.spawn(|| {
                        barrier.wait();
                        gate.redeem(&token, &nonce, 1, &visitor(1), NOW).is_ok()
                    })
                })
                .collect();
            handles
                .into_iter()
                .filter_map(|handle| handle.join().ok())
                .filter(|ok| *ok)
                .count()
        });
        assert_eq!(successes, 1);
    }

    #[test]
    fn a_clearance_is_bound_and_expires() {
        let gate = gate();
        let cookie = clearance(&gate, 1, &visitor(1));
        assert_eq!(
            gate.admit(&cookie, 2, &visitor(1), NOW),
            Admission::NotCleared
        );
        assert_eq!(
            gate.admit(&cookie, 1, &visitor(2), NOW),
            Admission::NotCleared
        );
        assert_eq!(
            gate.admit(&cookie, 1, &visitor(1), NOW + 1 + CLEARANCE_TTL_SECS),
            Admission::NotCleared
        );
        assert_eq!(gate.admit("", 1, &visitor(1), NOW), Admission::NotCleared);
    }

    #[test]
    fn another_ingress_with_the_same_keys_accepts_the_clearance() {
        let cookie = clearance(&gate(), 1, &visitor(1));
        assert_eq!(
            gate().admit(&cookie, 1, &visitor(1), NOW),
            Admission::Admitted
        );
    }

    #[test]
    fn the_budget_runs_out() {
        let gate = gate_with_budget(3);
        let cookie = clearance(&gate, 1, &visitor(1));
        for _ in 0..3 {
            assert_eq!(
                gate.admit(&cookie, 1, &visitor(1), NOW),
                Admission::Admitted
            );
        }
        assert_eq!(
            gate.admit(&cookie, 1, &visitor(1), NOW),
            Admission::BudgetSpent
        );
        // A fresh clearance has its own budget.
        let next = clearance(&gate, 1, &visitor(1));
        assert_eq!(gate.admit(&next, 1, &visitor(1), NOW), Admission::Admitted);
    }

    #[test]
    fn set_cookie_has_the_host_prefix_attributes() {
        let header = clearance_set_cookie("v");
        assert!(header.starts_with("__Host-temps_clr=v; Path=/;"));
        assert!(header.contains("; Secure"));
        assert!(header.contains("; HttpOnly"));
        assert!(!header.contains("Domain"));
    }
}
