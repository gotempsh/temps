//! Flag evaluation: a pure, synchronous, total function.
//!
//! No database, no I/O, no `async`. That is what lets the same code run in an
//! HTTP handler, inside the SDK, and (later) on the proxy hot path.
//!
//! # Resolution order
//!
//! ```text
//! 1. flag missing or archived   -> caller's fallback   FLAG_NOT_FOUND
//! 2. enabled == false           -> default_value       DISABLED
//! 3. -- reserved for rules --                          RULE_MATCH{i}
//!                                                    | PERCENTAGE_ROLLOUT{i}
//! 4. environment value present  -> that value          ENVIRONMENT_VALUE
//! 5. otherwise                  -> default_value       DEFAULT
//! ```
//!
//! Step 3 is unreachable in Phase 1 because no rules are ever stored. The order
//! is fixed now on purpose: deciding later whether a rule outranks a blanket
//! per-environment value would silently change what existing flags serve.
//!
//! Two consequences worth keeping:
//!
//! - Rules beat the environment value, because a rule is the more specific
//!   statement.
//! - The kill switch outranks everything. `enabled = false` short-circuits
//!   before targeting is consulted, so it keeps meaning exactly "ignore all
//!   targeting, everyone gets the default" once rules exist.

use std::collections::HashMap;

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use utoipa::ToSchema;

/// The declared type of a flag's value. Fixed at create time.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "lowercase")]
pub enum FlagValueType {
    Bool,
    String,
    Number,
    Json,
}

impl FlagValueType {
    pub fn as_str(&self) -> &'static str {
        match self {
            FlagValueType::Bool => "bool",
            FlagValueType::String => "string",
            FlagValueType::Number => "number",
            FlagValueType::Json => "json",
        }
    }

    pub fn parse(raw: &str) -> Option<Self> {
        match raw {
            "bool" => Some(FlagValueType::Bool),
            "string" => Some(FlagValueType::String),
            "number" => Some(FlagValueType::Number),
            "json" => Some(FlagValueType::Json),
            _ => None,
        }
    }

    /// Whether a concrete JSON value is admissible for this declared type.
    ///
    /// `Json` accepts anything except `null`: a flag must always resolve to a
    /// usable value, and `null` is indistinguishable from "unset" downstream.
    pub fn matches(&self, value: &serde_json::Value) -> bool {
        match self {
            FlagValueType::Bool => value.is_boolean(),
            FlagValueType::String => value.is_string(),
            FlagValueType::Number => value.is_number(),
            FlagValueType::Json => !value.is_null(),
        }
    }
}

/// Why an evaluation produced the value it did.
///
/// Part of the wire contract, and the only thing that makes "why is this user
/// seeing the old checkout?" answerable. `RuleMatch` and `PercentageRollout`
/// are declared now but never produced until targeting ships.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(tag = "kind", rename_all = "SCREAMING_SNAKE_CASE")]
pub enum EvalReason {
    /// No such flag, or it has been archived. The caller's fallback is served.
    FlagNotFound,
    /// Kill switch engaged for this environment.
    Disabled,
    /// A targeting rule matched. Reserved; not produced in Phase 1.
    RuleMatch { index: usize },
    /// A percentage rollout included this subject. Reserved; not produced in
    /// Phase 1.
    PercentageRollout { index: usize },
    /// An environment-specific value was set.
    EnvironmentValue,
    /// Nothing more specific applied; the flag default was served.
    Default,
    /// Evaluation degraded (malformed stored value). The default is served and
    /// the caller is told the value is not trustworthy. Never panics.
    Error,
}

/// A targeting attribute supplied by the caller.
///
/// Accepted and carried through in Phase 1, but not yet read by [`evaluate`].
/// The parameter is the expensive part of the contract; the code that consumes
/// it is cheap to add.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum AttributeValue {
    Bool(bool),
    Number(f64),
    String(String),
}

/// Everything the caller knows about the subject being evaluated.
#[derive(Debug, Clone, Default)]
pub struct EvalContext {
    /// Stable bucketing subject (user id, account id, device id). Unused in
    /// Phase 1; required for stable percentage rollout later.
    pub key: Option<String>,
    /// Targeting attributes. Unused in Phase 1.
    pub attributes: HashMap<String, AttributeValue>,
}

/// A single flag, already resolved down to one environment. This is what the
/// evaluator sees and what the SDK caches in memory.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ToSchema)]
pub struct FlagSnapshot {
    pub key: String,
    pub value_type: FlagValueType,
    /// Served whenever evaluation cannot do better. Genuinely polymorphic by
    /// design — the surrounding struct carries the type.
    pub default_value: serde_json::Value,
    /// False means the kill switch is engaged for this environment.
    pub enabled: bool,
    /// `None` means "inherit `default_value`".
    pub environment_value: Option<serde_json::Value>,
}

/// The outcome of evaluating one flag. Always carries a usable value.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ToSchema)]
pub struct Evaluation {
    pub value: serde_json::Value,
    /// Named variant, once variants exist. Always `None` in Phase 1.
    pub variant: Option<String>,
    pub reason: EvalReason,
}

impl Evaluation {
    fn new(value: &serde_json::Value, reason: EvalReason) -> Self {
        Self {
            value: value.clone(),
            variant: None,
            reason,
        }
    }
}

/// Evaluate one flag against a context.
///
/// Total: returns a usable value for every input, never panics, never errors.
/// A malformed stored value degrades to the declared default with
/// [`EvalReason::Error`] rather than failing the caller's request — an app must
/// not break because someone typed the wrong thing into the flag UI.
pub fn evaluate(flag: &FlagSnapshot, _ctx: &EvalContext) -> Evaluation {
    if !flag.enabled {
        return sanitized(flag, &flag.default_value, EvalReason::Disabled);
    }

    // ---------------------------------------------------------------------
    // Step 3: targeting rules are evaluated HERE, ahead of the environment
    // value and behind the kill switch. Nothing above or below this point
    // changes when they land.
    // ---------------------------------------------------------------------

    match &flag.environment_value {
        Some(value) => sanitized(flag, value, EvalReason::EnvironmentValue),
        None => sanitized(flag, &flag.default_value, EvalReason::Default),
    }
}

/// Serve `value` if it matches the flag's declared type, otherwise fall back to
/// the default (and if the default is itself invalid, report `Error`).
fn sanitized(flag: &FlagSnapshot, value: &serde_json::Value, reason: EvalReason) -> Evaluation {
    if flag.value_type.matches(value) {
        return Evaluation::new(value, reason);
    }
    if flag.value_type.matches(&flag.default_value) {
        return Evaluation::new(&flag.default_value, EvalReason::Error);
    }
    Evaluation::new(&serde_json::Value::Null, EvalReason::Error)
}

/// Evaluate a flag that could not be found, using the caller's fallback.
pub fn not_found(fallback: serde_json::Value) -> Evaluation {
    Evaluation {
        value: fallback,
        variant: None,
        reason: EvalReason::FlagNotFound,
    }
}

/// Stable percentage bucket in `[0, 100)`.
///
/// **This function is not called in Phase 1.** It ships early, tested, so the
/// algorithm is pinned before anything can depend on it: once a single user is
/// inside a percentage rollout, changing this silently re-randomizes live
/// experiments.
///
/// Properties the design relies on, asserted in the tests below:
///
/// - Widening a rollout never re-rolls anyone already included.
/// - Rotating the salt reshuffles the cohort.
/// - Different flags bucket independently, so the same unlucky subjects are not
///   the guinea pigs for every experiment.
pub fn bucket(flag_key: &str, salt: &str, subject: &str) -> f64 {
    let mut hasher = Sha256::new();
    hasher.update(flag_key.as_bytes());
    hasher.update(b":");
    hasher.update(salt.as_bytes());
    hasher.update(b":");
    hasher.update(subject.as_bytes());
    let digest = hasher.finalize();

    let n = u32::from_be_bytes([digest[0], digest[1], digest[2], digest[3]]);
    f64::from(n) / f64::from(u32::MAX) * 100.0
}

#[cfg(test)]
mod tests {
    use super::*;

    fn snapshot(enabled: bool, env_value: Option<serde_json::Value>) -> FlagSnapshot {
        FlagSnapshot {
            key: "checkout.v2".to_string(),
            value_type: FlagValueType::Bool,
            default_value: serde_json::json!(false),
            enabled,
            environment_value: env_value,
        }
    }

    #[test]
    fn serves_environment_value_when_set() {
        let flag = snapshot(true, Some(serde_json::json!(true)));
        let result = evaluate(&flag, &EvalContext::default());

        assert_eq!(result.value, serde_json::json!(true));
        assert_eq!(result.reason, EvalReason::EnvironmentValue);
        assert_eq!(result.variant, None);
    }

    #[test]
    fn falls_back_to_default_when_no_environment_value() {
        let flag = snapshot(true, None);
        let result = evaluate(&flag, &EvalContext::default());

        assert_eq!(result.value, serde_json::json!(false));
        assert_eq!(result.reason, EvalReason::Default);
    }

    /// The kill switch must outrank the environment value, or "disable this
    /// flag" would not actually disable a flag that has an override set.
    #[test]
    fn kill_switch_beats_environment_value() {
        let flag = snapshot(false, Some(serde_json::json!(true)));
        let result = evaluate(&flag, &EvalContext::default());

        assert_eq!(result.value, serde_json::json!(false));
        assert_eq!(result.reason, EvalReason::Disabled);
    }

    /// Context is accepted and ignored in Phase 1. Asserting it explicitly so
    /// the day targeting lands, this test fails and forces a conscious update.
    #[test]
    fn context_does_not_affect_evaluation_in_phase_one() {
        let flag = snapshot(true, Some(serde_json::json!(true)));

        let mut attributes = HashMap::new();
        attributes.insert("plan".to_string(), AttributeValue::String("free".into()));
        let ctx = EvalContext {
            key: Some("user_1001".to_string()),
            attributes,
        };

        assert_eq!(
            evaluate(&flag, &ctx),
            evaluate(&flag, &EvalContext::default())
        );
    }

    /// A value that does not match the declared type must not reach the app.
    #[test]
    fn type_mismatch_degrades_to_default_with_error_reason() {
        let mut flag = snapshot(true, Some(serde_json::json!("yes")));
        flag.value_type = FlagValueType::Bool;

        let result = evaluate(&flag, &EvalContext::default());

        assert_eq!(result.value, serde_json::json!(false));
        assert_eq!(result.reason, EvalReason::Error);
    }

    #[test]
    fn evaluation_is_total_even_when_default_is_also_invalid() {
        let flag = FlagSnapshot {
            key: "broken".to_string(),
            value_type: FlagValueType::Number,
            default_value: serde_json::json!("not a number"),
            enabled: true,
            environment_value: Some(serde_json::json!("also not a number")),
        };

        let result = evaluate(&flag, &EvalContext::default());
        assert_eq!(result.reason, EvalReason::Error);
    }

    #[test]
    fn value_type_matching() {
        assert!(FlagValueType::Bool.matches(&serde_json::json!(true)));
        assert!(!FlagValueType::Bool.matches(&serde_json::json!(1)));
        assert!(FlagValueType::Number.matches(&serde_json::json!(1.5)));
        assert!(FlagValueType::String.matches(&serde_json::json!("x")));
        assert!(FlagValueType::Json.matches(&serde_json::json!({"a": 1})));
        // null is never a valid flag value: indistinguishable from "unset".
        assert!(!FlagValueType::Json.matches(&serde_json::Value::Null));
    }

    // =====================================================================
    // Locked bucketing vectors.
    //
    // These values are the algorithm's public contract. If a change to
    // `bucket()` makes these fail, that change is silently re-randomizing
    // every live experiment — fix the change, not the test.
    // =====================================================================

    fn assert_bucket(flag: &str, salt: &str, subject: &str, expected: f64) {
        let actual = bucket(flag, salt, subject);
        assert!(
            (actual - expected).abs() < 0.005,
            "bucket({flag}, {salt}, {subject}) = {actual}, expected {expected}"
        );
    }

    #[test]
    fn bucket_locked_vectors() {
        assert_bucket("checkout.v2", "a1b2c3", "user_1001", 36.47);
        assert_bucket("checkout.v2", "a1b2c3", "user_1002", 21.03);
        assert_bucket("checkout.v2", "a1b2c3", "user_1003", 92.11);
        assert_bucket("checkout.v2", "z9y8x7", "user_1003", 8.39);
        assert_bucket("new.search", "a1b2c3", "user_1001", 0.33);
    }

    #[test]
    fn bucket_is_deterministic() {
        for _ in 0..100 {
            assert_eq!(
                bucket("checkout.v2", "a1b2c3", "user_1001"),
                bucket("checkout.v2", "a1b2c3", "user_1001")
            );
        }
    }

    #[test]
    fn bucket_is_in_range() {
        for i in 0..10_000 {
            let b = bucket("checkout.v2", "a1b2c3", &format!("user_{i}"));
            assert!((0.0..100.0).contains(&b), "bucket out of range: {b}");
        }
    }

    /// Widening a rollout must never remove anyone who was already included.
    #[test]
    fn ramping_never_re_rolls_an_included_subject() {
        for i in 0..2_000 {
            let subject = format!("user_{i}");
            let b = bucket("checkout.v2", "a1b2c3", &subject);

            for (lower, upper) in [(10.0, 25.0), (25.0, 50.0), (50.0, 100.0)] {
                if b < lower {
                    assert!(
                        b < upper,
                        "subject {subject} was in at {lower}% but out at {upper}%"
                    );
                }
            }
        }
    }

    #[test]
    fn rotating_the_salt_reshuffles_the_cohort() {
        let before: Vec<bool> = (0..1_000)
            .map(|i| bucket("checkout.v2", "a1b2c3", &format!("user_{i}")) < 10.0)
            .collect();
        let after: Vec<bool> = (0..1_000)
            .map(|i| bucket("checkout.v2", "z9y8x7", &format!("user_{i}")) < 10.0)
            .collect();

        let moved = before
            .iter()
            .zip(after.iter())
            .filter(|(a, b)| a != b)
            .count();

        assert!(
            moved > 50,
            "salt rotation barely changed the cohort: {moved}"
        );
    }

    /// Two flags must not select the same subjects, or the same unlucky users
    /// are the guinea pigs for every experiment.
    #[test]
    fn different_flags_bucket_independently() {
        let a: Vec<bool> = (0..1_000)
            .map(|i| bucket("checkout.v2", "a1b2c3", &format!("user_{i}")) < 20.0)
            .collect();
        let b: Vec<bool> = (0..1_000)
            .map(|i| bucket("new.search", "a1b2c3", &format!("user_{i}")) < 20.0)
            .collect();

        let both = a.iter().zip(b.iter()).filter(|(x, y)| **x && **y).count();

        // Independent 20% selections overlap ~4% of the time (40 of 1000).
        // A correlated implementation would land near 200.
        assert!(
            (10..=90).contains(&both),
            "overlap {both} suggests the two flags are not independent"
        );
    }

    /// Distribution sanity: a 25% rollout should land near 25% in aggregate.
    #[test]
    fn bucket_distributes_evenly() {
        let included = (0..20_000)
            .filter(|i| bucket("checkout.v2", "a1b2c3", &format!("user_{i}")) < 25.0)
            .count();

        let pct = included as f64 / 20_000.0 * 100.0;
        assert!(
            (23.0..27.0).contains(&pct),
            "25% rollout produced {pct}% inclusion"
        );
    }
}
