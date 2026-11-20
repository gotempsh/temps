// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Flag evaluation: a pure, synchronous, total function.
//!
//! No database, no I/O, no `async`. That is what lets the same code run in an
//! HTTP handler, inside the SDK, and on the public same-origin endpoint.
//!
//! # Resolution order
//!
//! ```text
//! 1. flag missing or archived   -> caller's fallback   FLAG_NOT_FOUND
//! 2. enabled == false           -> default_value       DISABLED
//! 3. first matching rule        -> its serve           RULE_MATCH{i}
//!                                                    | PERCENTAGE_ROLLOUT{i}
//! 4. environment value present  -> that value          ENVIRONMENT_VALUE
//! 5. otherwise                  -> default_value       DEFAULT
//! ```
//!
//! The order was fixed in Phase 1 (ADR 034) and step 3 filled in by ADR 110 D3
//! without moving anything around it:
//!
//! - Rules beat the environment value, because a rule is the more specific
//!   statement.
//! - The kill switch outranks everything. `enabled = false` short-circuits
//!   before targeting is consulted, so it keeps meaning exactly "ignore all
//!   targeting, everyone gets the default".
//!
//! # Rules
//!
//! A rule is `when <all conditions> [and the subject is inside rollout_percentage]
//! serve <value | variant | weighted split of variants>`. First match wins.
//!
//! - A condition on an attribute the context does not carry is false, except
//!   `not_exists`. Targeting fails closed: a subject we know nothing about
//!   gets the untargeted value, never the targeted one.
//! - A rule that needs a bucketing subject (a rollout percentage or a split)
//!   is skipped when the context has no `key`. It does not match, it does not
//!   error: the next rule, then the environment value, apply.
//! - A rule that cannot be evaluated at all (a pattern that does not compile,
//!   a split naming a variant the flag does not declare) degrades the whole
//!   evaluation to the default with reason `ERROR` (ADR 034 R2.6). Validation
//!   on save makes this unreachable through the API; it exists so a row
//!   edited by hand cannot break an app.

use std::collections::HashMap;

use regex::Regex;
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
/// seeing the old checkout?" answerable.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(tag = "kind", rename_all = "SCREAMING_SNAKE_CASE")]
pub enum EvalReason {
    /// No such flag, or it has been archived. The caller's fallback is served.
    FlagNotFound,
    /// Kill switch engaged for this environment.
    Disabled,
    /// Targeting rule `index` (0-based) matched on its conditions alone.
    RuleMatch { index: usize },
    /// Targeting rule `index` matched and the subject's bucket decided the
    /// outcome: it was inside the rule's rollout percentage, or the rule split
    /// subjects across variants.
    PercentageRollout { index: usize },
    /// An environment-specific value was set.
    EnvironmentValue,
    /// Nothing more specific applied; the flag default was served.
    Default,
    /// Evaluation degraded (malformed stored value or rule). The default is
    /// served and the caller is told the value is not trustworthy. Never
    /// panics.
    Error,
}

/// A targeting attribute: what the caller (or the server, implicitly) knows
/// about the subject.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ToSchema)]
#[serde(untagged)]
pub enum AttributeValue {
    Bool(bool),
    Number(f64),
    String(String),
}

impl AttributeValue {
    /// Numeric view used by the ordering operators. A string that parses as a
    /// number compares as one, because implicit attributes and many SDK
    /// contexts carry numbers as strings (`"42"`).
    fn as_number(&self) -> Option<f64> {
        match self {
            AttributeValue::Number(n) => Some(*n),
            AttributeValue::String(s) => s.trim().parse::<f64>().ok().filter(|n| n.is_finite()),
            AttributeValue::Bool(_) => None,
        }
    }

    /// Equality with one coercion: a number and a string holding the same
    /// number are equal. `true` and `"true"` are not, on purpose: booleans are
    /// rarely carried as strings, and silently equating them would make
    /// `plan == "true"` match a boolean attribute.
    fn loosely_equals(&self, other: &AttributeValue) -> bool {
        match (self, other) {
            (AttributeValue::Bool(a), AttributeValue::Bool(b)) => a == b,
            (AttributeValue::String(a), AttributeValue::String(b)) => a == b,
            (AttributeValue::Number(a), AttributeValue::Number(b)) => a == b,
            (AttributeValue::Number(_), AttributeValue::String(_))
            | (AttributeValue::String(_), AttributeValue::Number(_)) => {
                match (self.as_number(), other.as_number()) {
                    (Some(a), Some(b)) => a == b,
                    _ => false,
                }
            }
            (AttributeValue::Bool(_), _) | (_, AttributeValue::Bool(_)) => false,
        }
    }
}

/// Everything the caller knows about the subject being evaluated.
#[derive(Debug, Clone, Default)]
pub struct EvalContext {
    /// Stable bucketing subject (user id, account id, device id). Required by
    /// rules with a rollout percentage or a split; without it those rules are
    /// skipped.
    pub key: Option<String>,
    /// Targeting attributes.
    pub attributes: HashMap<String, AttributeValue>,
}

/// A named value a flag can serve, so a rollout or an experiment keys off a
/// stable name (`control`, `test`) rather than a serialized value (ADR 034
/// R1.4, ADR 110 D3).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ToSchema)]
pub struct FlagVariant {
    /// `[a-z0-9][a-z0-9._-]*`, unique within the flag.
    #[schema(example = "test")]
    pub key: String,
    /// Must match the flag's `value_type`.
    pub value: serde_json::Value,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
}

/// Comparison applied by one rule condition.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum ConditionOperator {
    /// Equal. A number and a numeric string compare as numbers.
    Eq,
    /// Not equal. False when the attribute is absent.
    Neq,
    /// Equal to one of the listed values.
    In,
    /// Equal to none of the listed values. False when the attribute is absent.
    NotIn,
    /// String attribute contains the value as a substring.
    Contains,
    /// String attribute does not contain the value.
    NotContains,
    /// String attribute matches the pattern in full (anchored at both ends).
    Matches,
    Gt,
    Gte,
    Lt,
    Lte,
    /// The attribute is present, whatever its value.
    Exists,
    /// The attribute is absent. The only operator true for a missing attribute.
    NotExists,
}

impl ConditionOperator {
    pub fn as_str(&self) -> &'static str {
        match self {
            ConditionOperator::Eq => "eq",
            ConditionOperator::Neq => "neq",
            ConditionOperator::In => "in",
            ConditionOperator::NotIn => "not_in",
            ConditionOperator::Contains => "contains",
            ConditionOperator::NotContains => "not_contains",
            ConditionOperator::Matches => "matches",
            ConditionOperator::Gt => "gt",
            ConditionOperator::Gte => "gte",
            ConditionOperator::Lt => "lt",
            ConditionOperator::Lte => "lte",
            ConditionOperator::Exists => "exists",
            ConditionOperator::NotExists => "not_exists",
        }
    }

    /// What shape of `value` the operator takes.
    pub fn operand(&self) -> OperandShape {
        match self {
            ConditionOperator::Exists | ConditionOperator::NotExists => OperandShape::None,
            ConditionOperator::In | ConditionOperator::NotIn => OperandShape::List,
            ConditionOperator::Contains
            | ConditionOperator::NotContains
            | ConditionOperator::Matches => OperandShape::String,
            ConditionOperator::Gt
            | ConditionOperator::Gte
            | ConditionOperator::Lt
            | ConditionOperator::Lte => OperandShape::Number,
            ConditionOperator::Eq | ConditionOperator::Neq => OperandShape::Scalar,
        }
    }
}

/// The operand an operator expects. Used by validation on save.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OperandShape {
    None,
    Scalar,
    String,
    Number,
    List,
}

/// The right-hand side of a condition: one value, or a list for `in`/`not_in`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ToSchema)]
#[serde(untagged)]
pub enum ConditionValue {
    List(Vec<AttributeValue>),
    Single(AttributeValue),
}

/// `attribute <operator> value`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ToSchema)]
pub struct RuleCondition {
    /// Context attribute name, e.g. `plan`, `country`, `email`.
    #[schema(example = "plan")]
    pub attribute: String,
    pub operator: ConditionOperator,
    /// Absent for `exists` / `not_exists`; a list for `in` / `not_in`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub value: Option<ConditionValue>,
}

/// One arm of a weighted split.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct VariantWeight {
    /// A variant key declared on the flag.
    pub variant: String,
    /// Share of the rule's subjects, in whole percent. A split's weights sum
    /// to 100.
    pub weight: u32,
}

/// What a matching rule serves.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ToSchema)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum RuleServe {
    /// A literal value of the flag's type.
    Value { value: serde_json::Value },
    /// One declared variant.
    Variant { variant: String },
    /// Subjects split across declared variants by weight, stably per subject.
    Split { split: Vec<VariantWeight> },
}

/// `when <conditions> [within rollout_percentage] serve <serve>`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ToSchema)]
pub struct FlagRule {
    /// Free text shown in the console, e.g. "Beta customers".
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    /// All must hold. Empty means every subject.
    #[serde(default)]
    pub conditions: Vec<RuleCondition>,
    /// Share of matching subjects the rule applies to, `0..=100`. `None` means
    /// all of them. Ramping it up never removes anyone already included.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rollout_percentage: Option<f64>,
    pub serve: RuleServe,
}

impl FlagRule {
    /// Whether evaluating this rule needs a bucketing subject.
    pub fn needs_subject(&self) -> bool {
        self.rollout_percentage.is_some_and(|p| p < 100.0)
            || matches!(self.serve, RuleServe::Split { .. })
    }
}

/// A single flag, already resolved down to one environment. This is what the
/// evaluator sees and what the server-side SDK caches in memory.
///
/// Carries the rules and the salt, so it is **server-side only**: it is served
/// to deployment tokens (`/flags/snapshot`) and never to a browser, which only
/// ever receives evaluated results (ADR 034 R5.1).
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
    /// Bucketing salt. Needed by an SDK that evaluates rollouts locally.
    #[serde(default)]
    pub salt: String,
    /// Named values rules may serve.
    #[serde(default)]
    pub variants: Vec<FlagVariant>,
    /// Ordered targeting rules for this environment. First match wins.
    #[serde(default)]
    pub rules: Vec<FlagRule>,
}

/// The outcome of evaluating one flag. Always carries a usable value.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ToSchema)]
pub struct Evaluation {
    pub value: serde_json::Value,
    /// The variant served, when a rule served one. `None` for plain values.
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

/// Patterns for `matches` conditions, compiled once per cached snapshot so the
/// public endpoint never compiles a regex per request. Keyed by the pattern
/// source.
#[derive(Debug, Clone, Default)]
pub struct CompiledPatterns {
    patterns: HashMap<String, Regex>,
}

/// Longest pattern accepted by a `matches` condition.
pub const MAX_PATTERN_LEN: usize = 256;
/// Compiled-program ceiling for one pattern. The `regex` crate runs in linear
/// time, so backtracking is not the risk; a pattern like `a{1000}{1000}`
/// compiling into a huge program is, and this bounds it.
const PATTERN_SIZE_LIMIT: usize = 256 * 1024;

/// Compile a `matches` pattern the way the evaluator uses it: anchored at both
/// ends, size-limited. Shared with save-time validation so both reject the
/// same inputs.
pub fn compile_pattern(pattern: &str) -> Result<Regex, String> {
    if pattern.len() > MAX_PATTERN_LEN {
        return Err(format!(
            "pattern is {} characters, maximum is {MAX_PATTERN_LEN}",
            pattern.len()
        ));
    }
    regex::RegexBuilder::new(&format!("^(?:{pattern})$"))
        .size_limit(PATTERN_SIZE_LIMIT)
        .dfa_size_limit(PATTERN_SIZE_LIMIT)
        .build()
        .map_err(|e| e.to_string())
}

impl CompiledPatterns {
    /// Compile every `matches` pattern a flag's rules use. A pattern that does
    /// not compile is left out; evaluation then reports `ERROR` for the flag.
    pub fn for_flag(flag: &FlagSnapshot) -> Self {
        let mut patterns = HashMap::new();
        for rule in &flag.rules {
            for condition in &rule.conditions {
                if condition.operator != ConditionOperator::Matches {
                    continue;
                }
                if let Some(ConditionValue::Single(AttributeValue::String(source))) =
                    &condition.value
                {
                    if patterns.contains_key(source) {
                        continue;
                    }
                    if let Ok(regex) = compile_pattern(source) {
                        patterns.insert(source.clone(), regex);
                    }
                }
            }
        }
        Self { patterns }
    }
}

/// A rule could not be evaluated. Internal: surfaces as `EvalReason::Error`.
struct Malformed;

/// Evaluate one flag against a context.
///
/// Total: returns a usable value for every input, never panics, never errors.
/// Compiles `matches` patterns on the fly; callers evaluating the same flag
/// repeatedly should hold a [`CompiledPatterns`] and use [`evaluate_compiled`].
pub fn evaluate(flag: &FlagSnapshot, ctx: &EvalContext) -> Evaluation {
    let patterns = if flag
        .rules
        .iter()
        .flat_map(|rule| &rule.conditions)
        .any(|condition| condition.operator == ConditionOperator::Matches)
    {
        CompiledPatterns::for_flag(flag)
    } else {
        CompiledPatterns::default()
    };
    evaluate_compiled(flag, &patterns, ctx)
}

/// [`evaluate`] with patterns compiled ahead of time.
pub fn evaluate_compiled(
    flag: &FlagSnapshot,
    patterns: &CompiledPatterns,
    ctx: &EvalContext,
) -> Evaluation {
    if !flag.enabled {
        return sanitized(flag, &flag.default_value, EvalReason::Disabled);
    }

    // Step 3: targeting rules, ahead of the environment value and behind the
    // kill switch.
    for (index, rule) in flag.rules.iter().enumerate() {
        match evaluate_rule(flag, patterns, ctx, index, rule) {
            Ok(Some(evaluation)) => return evaluation,
            Ok(None) => continue,
            Err(Malformed) => {
                tracing::warn!(
                    flag_key = %flag.key,
                    rule_index = index,
                    "Feature flag rule could not be evaluated; serving the default"
                );
                return degraded(flag);
            }
        }
    }

    match &flag.environment_value {
        Some(value) => sanitized(flag, value, EvalReason::EnvironmentValue),
        None => sanitized(flag, &flag.default_value, EvalReason::Default),
    }
}

/// `Ok(Some)` when the rule matched, `Ok(None)` when it did not, `Err` when it
/// cannot be evaluated.
fn evaluate_rule(
    flag: &FlagSnapshot,
    patterns: &CompiledPatterns,
    ctx: &EvalContext,
    index: usize,
    rule: &FlagRule,
) -> Result<Option<Evaluation>, Malformed> {
    for condition in &rule.conditions {
        if !condition_holds(condition, patterns, ctx)? {
            return Ok(None);
        }
    }

    let subject = ctx.key.as_deref().filter(|key| !key.is_empty());
    let mut bucketed = false;

    if let Some(percentage) = rule.rollout_percentage {
        if !percentage.is_finite() || !(0.0..=100.0).contains(&percentage) {
            return Err(Malformed);
        }
        if percentage < 100.0 {
            let Some(subject) = subject else {
                return Ok(None);
            };
            if bucket(&flag.key, &flag.salt, subject) >= percentage {
                return Ok(None);
            }
            bucketed = true;
        }
    }

    let reason = |bucketed: bool| {
        if bucketed {
            EvalReason::PercentageRollout { index }
        } else {
            EvalReason::RuleMatch { index }
        }
    };

    match &rule.serve {
        RuleServe::Value { value } => Ok(Some(sanitized_rule(flag, value, None, reason(bucketed)))),
        RuleServe::Variant { variant } => {
            let declared = find_variant(flag, variant)?;
            Ok(Some(sanitized_rule(
                flag,
                &declared.value,
                Some(&declared.key),
                reason(bucketed),
            )))
        }
        RuleServe::Split { split } => {
            // Shape first, subject second: a malformed split is an `ERROR`
            // whether or not this caller sent a key, so whether a broken rule
            // is noticed never depends on who happens to be asking.
            check_split(flag, split)?;
            let Some(subject) = subject else {
                return Ok(None);
            };
            let chosen = pick_split(flag, split, subject)?;
            let declared = find_variant(flag, chosen)?;
            Ok(Some(sanitized_rule(
                flag,
                &declared.value,
                Some(&declared.key),
                reason(true),
            )))
        }
    }
}

fn find_variant<'a>(flag: &'a FlagSnapshot, key: &str) -> Result<&'a FlagVariant, Malformed> {
    flag.variants
        .iter()
        .find(|variant| variant.key == key)
        .ok_or(Malformed)
}

/// Choose a split arm for a subject.
///
/// Uses a bucket independent of the rollout bucket (the flag key is suffixed
/// with `#split`, a character flag keys cannot contain), so widening a rule's
/// rollout never moves an already-included subject to another variant.
/// A split is evaluable when it is non-empty, its weights sum to exactly 100
/// and every arm names a declared variant.
fn check_split(flag: &FlagSnapshot, split: &[VariantWeight]) -> Result<(), Malformed> {
    let total = split
        .iter()
        .fold(0u32, |sum, arm| sum.saturating_add(arm.weight));
    if split.is_empty() || total != 100 {
        return Err(Malformed);
    }
    for arm in split {
        find_variant(flag, &arm.variant)?;
    }
    Ok(())
}

/// Called after [`check_split`].
fn pick_split<'a>(
    flag: &FlagSnapshot,
    split: &'a [VariantWeight],
    subject: &str,
) -> Result<&'a str, Malformed> {
    let position = split_bucket(&flag.key, &flag.salt, subject);
    let mut cumulative = 0.0;
    for arm in split {
        cumulative += f64::from(arm.weight);
        if position < cumulative {
            return Ok(&arm.variant);
        }
    }
    // position is < 100 and the weights sum to 100, so the loop returned —
    // unless every remaining arm has weight 0, in which case the last
    // non-empty arm owns the top of the range.
    split
        .iter()
        .rev()
        .find(|arm| arm.weight > 0)
        .map(|arm| arm.variant.as_str())
        .ok_or(Malformed)
}

/// The bucket a subject's split position is drawn from. See [`pick_split`].
pub fn split_bucket(flag_key: &str, salt: &str, subject: &str) -> f64 {
    suffixed_bucket(flag_key, "#split", salt, subject)
}

/// `bucket(format!("{flag_key}{suffix}"), salt, subject)` without building the
/// string: SHA-256 over the same bytes, so it is the same number (asserted by
/// `suffixed_bucket_equals_bucket_of_the_joined_key`). Used on the public
/// endpoints' per-request path. Kept separate from [`bucket`] so the locked
/// function's body never changes.
pub fn suffixed_bucket(flag_key: &str, suffix: &str, salt: &str, subject: &str) -> f64 {
    let mut hasher = Sha256::new();
    hasher.update(flag_key.as_bytes());
    hasher.update(suffix.as_bytes());
    hasher.update(b":");
    hasher.update(salt.as_bytes());
    hasher.update(b":");
    hasher.update(subject.as_bytes());
    let digest = hasher.finalize();

    let n = u32::from_be_bytes([digest[0], digest[1], digest[2], digest[3]]);
    f64::from(n) / f64::from(u32::MAX) * 100.0
}

fn condition_holds(
    condition: &RuleCondition,
    patterns: &CompiledPatterns,
    ctx: &EvalContext,
) -> Result<bool, Malformed> {
    let actual = ctx.attributes.get(&condition.attribute);

    let Some(actual) = actual else {
        return Ok(condition.operator == ConditionOperator::NotExists);
    };

    let single = || match &condition.value {
        Some(ConditionValue::Single(value)) => Ok(value),
        _ => Err(Malformed),
    };
    let list = || match &condition.value {
        Some(ConditionValue::List(values)) => Ok(values),
        _ => Err(Malformed),
    };
    let string_operand = || match single()? {
        AttributeValue::String(s) => Ok(s.as_str()),
        _ => Err(Malformed),
    };
    let number_operand = || single()?.as_number().ok_or(Malformed);
    let actual_string = match actual {
        AttributeValue::String(s) => Some(s.as_str()),
        _ => None,
    };

    let holds = match condition.operator {
        ConditionOperator::Exists => true,
        ConditionOperator::NotExists => false,
        ConditionOperator::Eq => actual.loosely_equals(single()?),
        ConditionOperator::Neq => !actual.loosely_equals(single()?),
        ConditionOperator::In => list()?.iter().any(|v| actual.loosely_equals(v)),
        ConditionOperator::NotIn => !list()?.iter().any(|v| actual.loosely_equals(v)),
        ConditionOperator::Contains => {
            let needle = string_operand()?;
            actual_string.is_some_and(|s| s.contains(needle))
        }
        ConditionOperator::NotContains => {
            let needle = string_operand()?;
            actual_string.is_some_and(|s| !s.contains(needle))
        }
        ConditionOperator::Matches => {
            let source = string_operand()?;
            let regex = patterns.patterns.get(source).ok_or(Malformed)?;
            actual_string.is_some_and(|s| regex.is_match(s))
        }
        ConditionOperator::Gt
        | ConditionOperator::Gte
        | ConditionOperator::Lt
        | ConditionOperator::Lte => {
            let operand = number_operand()?;
            match actual.as_number() {
                Some(value) => match condition.operator {
                    ConditionOperator::Gt => value > operand,
                    ConditionOperator::Gte => value >= operand,
                    ConditionOperator::Lt => value < operand,
                    ConditionOperator::Lte => value <= operand,
                    _ => false,
                },
                None => false,
            }
        }
    };
    Ok(holds)
}

/// Serve `value` if it matches the flag's declared type, otherwise fall back to
/// the default (and if the default is itself invalid, report `Error`).
fn sanitized(flag: &FlagSnapshot, value: &serde_json::Value, reason: EvalReason) -> Evaluation {
    if flag.value_type.matches(value) {
        return Evaluation::new(value, reason);
    }
    degraded(flag)
}

fn sanitized_rule(
    flag: &FlagSnapshot,
    value: &serde_json::Value,
    variant: Option<&str>,
    reason: EvalReason,
) -> Evaluation {
    let mut evaluation = sanitized(flag, value, reason);
    if evaluation.reason != EvalReason::Error {
        evaluation.variant = variant.map(str::to_string);
    }
    evaluation
}

/// The default with reason `Error`, or `null` if even the default is invalid.
fn degraded(flag: &FlagSnapshot) -> Evaluation {
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
/// Pinned in Phase 1 before anything depended on it: once a single user is
/// inside a percentage rollout, changing this silently re-randomizes live
/// experiments. The locked vectors in the tests below are its contract.
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

    #[test]
    fn suffixed_bucket_equals_bucket_of_the_joined_key() {
        for (key, suffix, salt, subject) in [
            ("checkout.v2", "#split", "a1b2c3", "user_1001"),
            ("checkout.v2", "#exposure", "z9y8x7", "user_1003"),
            ("new.search", "", "a1b2c3", "user_1001"),
        ] {
            assert_eq!(
                suffixed_bucket(key, suffix, salt, subject),
                bucket(&format!("{key}{suffix}"), salt, subject)
            );
        }
    }

    fn snapshot(enabled: bool, env_value: Option<serde_json::Value>) -> FlagSnapshot {
        FlagSnapshot {
            key: "checkout.v2".to_string(),
            value_type: FlagValueType::Bool,
            default_value: serde_json::json!(false),
            enabled,
            environment_value: env_value,
            salt: "a1b2c3".to_string(),
            variants: Vec::new(),
            rules: Vec::new(),
        }
    }

    fn ctx(key: Option<&str>, attributes: &[(&str, AttributeValue)]) -> EvalContext {
        EvalContext {
            key: key.map(str::to_string),
            attributes: attributes
                .iter()
                .map(|(name, value)| (name.to_string(), value.clone()))
                .collect(),
        }
    }

    fn s(value: &str) -> AttributeValue {
        AttributeValue::String(value.to_string())
    }

    fn condition(
        attribute: &str,
        operator: ConditionOperator,
        value: Option<ConditionValue>,
    ) -> RuleCondition {
        RuleCondition {
            attribute: attribute.to_string(),
            operator,
            value,
        }
    }

    fn serve_true_when(conditions: Vec<RuleCondition>) -> FlagRule {
        FlagRule {
            description: None,
            conditions,
            rollout_percentage: None,
            serve: RuleServe::Value {
                value: serde_json::json!(true),
            },
        }
    }

    fn string_flag_with_variants() -> FlagSnapshot {
        FlagSnapshot {
            key: "checkout.v2".to_string(),
            value_type: FlagValueType::String,
            default_value: serde_json::json!("off"),
            enabled: true,
            environment_value: None,
            salt: "a1b2c3".to_string(),
            variants: vec![
                FlagVariant {
                    key: "control".to_string(),
                    value: serde_json::json!("old"),
                    description: None,
                },
                FlagVariant {
                    key: "test".to_string(),
                    value: serde_json::json!("new"),
                    description: None,
                },
            ],
            rules: Vec::new(),
        }
    }

    fn fifty_fifty() -> FlagRule {
        FlagRule {
            description: None,
            conditions: Vec::new(),
            rollout_percentage: None,
            serve: RuleServe::Split {
                split: vec![
                    VariantWeight {
                        variant: "control".to_string(),
                        weight: 50,
                    },
                    VariantWeight {
                        variant: "test".to_string(),
                        weight: 50,
                    },
                ],
            },
        }
    }

    // =====================================================================
    // Resolution order (unchanged since Phase 1)
    // =====================================================================

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

    /// Phase 1 pinned "context is ignored" with a test meant to fail the day
    /// targeting landed. Targeting has landed; what still holds is that a flag
    /// with no rules ignores the context, so existing flags serve exactly what
    /// they served before the upgrade.
    #[test]
    fn a_flag_without_rules_ignores_the_context() {
        let flag = snapshot(true, Some(serde_json::json!(true)));
        let ctx = ctx(Some("user_1001"), &[("plan", s("free"))]);

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
        let mut flag = snapshot(true, Some(serde_json::json!("also not a number")));
        flag.value_type = FlagValueType::Number;
        flag.default_value = serde_json::json!("not a number");

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
    // Rules (step 3)
    // =====================================================================

    #[test]
    fn a_matching_rule_beats_the_environment_value() {
        let mut flag = snapshot(true, Some(serde_json::json!(false)));
        flag.rules = vec![serve_true_when(vec![condition(
            "plan",
            ConditionOperator::Eq,
            Some(ConditionValue::Single(s("pro"))),
        )])];

        let pro = evaluate(&flag, &ctx(None, &[("plan", s("pro"))]));
        assert_eq!(pro.value, serde_json::json!(true));
        assert_eq!(pro.reason, EvalReason::RuleMatch { index: 0 });

        let free = evaluate(&flag, &ctx(None, &[("plan", s("free"))]));
        assert_eq!(free.value, serde_json::json!(false));
        assert_eq!(free.reason, EvalReason::EnvironmentValue);
    }

    #[test]
    fn the_kill_switch_beats_every_rule() {
        let mut flag = snapshot(false, None);
        flag.rules = vec![serve_true_when(Vec::new())];

        let result = evaluate(&flag, &ctx(Some("user_1"), &[]));
        assert_eq!(result.value, serde_json::json!(false));
        assert_eq!(result.reason, EvalReason::Disabled);
    }

    #[test]
    fn the_first_matching_rule_wins() {
        let mut flag = string_flag_with_variants();
        flag.rules = vec![
            FlagRule {
                description: None,
                conditions: vec![condition(
                    "country",
                    ConditionOperator::In,
                    Some(ConditionValue::List(vec![s("DE"), s("FR")])),
                )],
                rollout_percentage: None,
                serve: RuleServe::Variant {
                    variant: "test".to_string(),
                },
            },
            FlagRule {
                description: None,
                conditions: Vec::new(),
                rollout_percentage: None,
                serve: RuleServe::Variant {
                    variant: "control".to_string(),
                },
            },
        ];

        let de = evaluate(&flag, &ctx(None, &[("country", s("DE"))]));
        assert_eq!(de.value, serde_json::json!("new"));
        assert_eq!(de.variant.as_deref(), Some("test"));
        assert_eq!(de.reason, EvalReason::RuleMatch { index: 0 });

        let us = evaluate(&flag, &ctx(None, &[("country", s("US"))]));
        assert_eq!(us.value, serde_json::json!("old"));
        assert_eq!(us.variant.as_deref(), Some("control"));
        assert_eq!(us.reason, EvalReason::RuleMatch { index: 1 });
    }

    /// Targeting fails closed: an attribute the caller did not send never
    /// selects the targeted value, even for the negative operators.
    #[test]
    fn a_missing_attribute_matches_only_not_exists() {
        let cases = [
            (
                ConditionOperator::Eq,
                Some(ConditionValue::Single(s("x"))),
                false,
            ),
            (
                ConditionOperator::Neq,
                Some(ConditionValue::Single(s("x"))),
                false,
            ),
            (
                ConditionOperator::In,
                Some(ConditionValue::List(vec![s("x")])),
                false,
            ),
            (
                ConditionOperator::NotIn,
                Some(ConditionValue::List(vec![s("x")])),
                false,
            ),
            (
                ConditionOperator::Contains,
                Some(ConditionValue::Single(s("x"))),
                false,
            ),
            (
                ConditionOperator::NotContains,
                Some(ConditionValue::Single(s("x"))),
                false,
            ),
            (
                ConditionOperator::Matches,
                Some(ConditionValue::Single(s("x"))),
                false,
            ),
            (
                ConditionOperator::Gt,
                Some(ConditionValue::Single(AttributeValue::Number(1.0))),
                false,
            ),
            (ConditionOperator::Exists, None, false),
            (ConditionOperator::NotExists, None, true),
        ];
        for (operator, value, expected) in cases {
            let mut flag = snapshot(true, None);
            flag.rules = vec![serve_true_when(vec![condition("plan", operator, value)])];
            let result = evaluate(&flag, &EvalContext::default());
            assert_eq!(
                result.value,
                serde_json::json!(expected),
                "operator {} on a missing attribute",
                operator.as_str()
            );
        }
    }

    #[test]
    fn operators_compare_as_documented() {
        let check =
            |operator: ConditionOperator, value: Option<ConditionValue>, actual: AttributeValue| {
                let mut flag = snapshot(true, None);
                flag.rules = vec![serve_true_when(vec![condition("a", operator, value)])];
                evaluate(&flag, &ctx(None, &[("a", actual)])).value == serde_json::json!(true)
            };
        let one = |v: AttributeValue| Some(ConditionValue::Single(v));
        let n = AttributeValue::Number;

        assert!(check(ConditionOperator::Eq, one(s("pro")), s("pro")));
        assert!(
            !check(ConditionOperator::Eq, one(s("pro")), s("Pro")),
            "case-sensitive"
        );
        assert!(
            check(ConditionOperator::Eq, one(n(42.0)), s("42")),
            "numeric string equals number"
        );
        assert!(!check(
            ConditionOperator::Eq,
            one(s("true")),
            AttributeValue::Bool(true)
        ));
        assert!(check(ConditionOperator::Neq, one(s("pro")), s("free")));
        assert!(check(
            ConditionOperator::NotIn,
            Some(ConditionValue::List(vec![s("a"), s("b")])),
            s("c")
        ));
        assert!(check(
            ConditionOperator::Contains,
            one(s("@acme.")),
            s("jo@acme.test")
        ));
        assert!(
            !check(ConditionOperator::Contains, one(s("1")), n(10.0)),
            "contains needs a string attribute"
        );
        assert!(check(
            ConditionOperator::NotContains,
            one(s("@acme.")),
            s("jo@other.test")
        ));
        assert!(check(
            ConditionOperator::Matches,
            one(s(r"[a-z]+@acme\.test")),
            s("jo@acme.test")
        ));
        assert!(
            !check(
                ConditionOperator::Matches,
                one(s(r"acme")),
                s("jo@acme.test")
            ),
            "patterns are anchored at both ends"
        );
        assert!(check(ConditionOperator::Gt, one(n(10.0)), n(11.0)));
        assert!(!check(ConditionOperator::Gt, one(n(10.0)), n(10.0)));
        assert!(check(ConditionOperator::Gte, one(n(10.0)), s("10")));
        assert!(check(ConditionOperator::Lt, one(n(10.0)), n(9.5)));
        assert!(check(ConditionOperator::Lte, one(n(10.0)), n(10.0)));
        assert!(
            !check(ConditionOperator::Lt, one(n(10.0)), s("ten")),
            "non-numeric never compares"
        );
        assert!(check(
            ConditionOperator::Exists,
            None,
            AttributeValue::Bool(false)
        ));
    }

    #[test]
    fn every_condition_of_a_rule_must_hold() {
        let mut flag = snapshot(true, None);
        flag.rules = vec![serve_true_when(vec![
            condition(
                "plan",
                ConditionOperator::Eq,
                Some(ConditionValue::Single(s("pro"))),
            ),
            condition(
                "country",
                ConditionOperator::Eq,
                Some(ConditionValue::Single(s("DE"))),
            ),
        ])];

        let both = ctx(None, &[("plan", s("pro")), ("country", s("DE"))]);
        let one = ctx(None, &[("plan", s("pro")), ("country", s("US"))]);
        assert_eq!(evaluate(&flag, &both).value, serde_json::json!(true));
        assert_eq!(evaluate(&flag, &one).value, serde_json::json!(false));
    }

    #[test]
    fn rollout_includes_exactly_the_subjects_below_the_percentage() {
        let mut flag = snapshot(true, None);
        let mut rule = serve_true_when(Vec::new());
        rule.rollout_percentage = Some(25.0);
        flag.rules = vec![rule];

        for i in 0..2_000 {
            let subject = format!("user_{i}");
            let expected = bucket("checkout.v2", "a1b2c3", &subject) < 25.0;
            let result = evaluate(&flag, &ctx(Some(&subject), &[]));
            assert_eq!(
                result.value,
                serde_json::json!(expected),
                "subject {subject}"
            );
            if expected {
                assert_eq!(result.reason, EvalReason::PercentageRollout { index: 0 });
            } else {
                assert_eq!(result.reason, EvalReason::Default);
            }
        }
    }

    /// Without a subject there is nothing to bucket, so a partial rollout is
    /// skipped rather than guessed. The next step applies.
    #[test]
    fn a_partial_rollout_without_a_subject_is_skipped() {
        let mut flag = snapshot(true, Some(serde_json::json!(false)));
        let mut rule = serve_true_when(Vec::new());
        rule.rollout_percentage = Some(50.0);
        flag.rules = vec![rule];

        let result = evaluate(&flag, &EvalContext::default());
        assert_eq!(result.reason, EvalReason::EnvironmentValue);
    }

    #[test]
    fn a_full_rollout_needs_no_subject() {
        let mut flag = snapshot(true, None);
        let mut rule = serve_true_when(Vec::new());
        rule.rollout_percentage = Some(100.0);
        flag.rules = vec![rule];

        let result = evaluate(&flag, &EvalContext::default());
        assert_eq!(result.value, serde_json::json!(true));
        assert_eq!(result.reason, EvalReason::RuleMatch { index: 0 });
    }

    #[test]
    fn a_split_serves_variants_in_proportion() {
        let mut flag = string_flag_with_variants();
        flag.rules = vec![fifty_fifty()];

        let tests = (0..20_000)
            .filter(|i| {
                evaluate(&flag, &ctx(Some(&format!("user_{i}")), &[]))
                    .variant
                    .as_deref()
                    == Some("test")
            })
            .count();
        let pct = tests as f64 / 20_000.0 * 100.0;
        assert!(
            (48.0..52.0).contains(&pct),
            "50/50 split produced {pct}% test"
        );
    }

    #[test]
    fn a_split_reports_percentage_rollout_and_the_variant() {
        let mut flag = string_flag_with_variants();
        flag.rules = vec![fifty_fifty()];

        let result = evaluate(&flag, &ctx(Some("user_1001"), &[]));
        assert_eq!(result.reason, EvalReason::PercentageRollout { index: 0 });
        let variant = result.variant.expect("a split always names its variant");
        let expected = if variant == "test" { "new" } else { "old" };
        assert_eq!(result.value, serde_json::json!(expected));
    }

    /// Experiments depend on this: widening the rollout of a split rule must
    /// not move anyone already in it to the other variant.
    #[test]
    fn widening_a_split_rollout_never_changes_an_included_subjects_variant() {
        let mut narrow = string_flag_with_variants();
        let mut rule = fifty_fifty();
        rule.rollout_percentage = Some(20.0);
        narrow.rules = vec![rule.clone()];

        let mut wide = narrow.clone();
        rule.rollout_percentage = Some(60.0);
        wide.rules = vec![rule];

        for i in 0..3_000 {
            let subject = ctx(Some(&format!("user_{i}")), &[]);
            let before = evaluate(&narrow, &subject);
            if before.variant.is_some() {
                assert_eq!(evaluate(&wide, &subject).variant, before.variant);
            }
        }
    }

    #[test]
    fn a_rule_naming_an_undeclared_variant_degrades_to_error() {
        let mut flag = string_flag_with_variants();
        flag.rules = vec![FlagRule {
            description: None,
            conditions: Vec::new(),
            rollout_percentage: None,
            serve: RuleServe::Variant {
                variant: "ghost".to_string(),
            },
        }];

        let result = evaluate(&flag, &ctx(None, &[]));
        assert_eq!(result.value, serde_json::json!("off"));
        assert_eq!(result.reason, EvalReason::Error);
        assert_eq!(result.variant, None);
    }

    #[test]
    fn a_split_whose_weights_do_not_sum_to_100_degrades_to_error() {
        let mut flag = string_flag_with_variants();
        let mut rule = fifty_fifty();
        if let RuleServe::Split { split } = &mut rule.serve {
            split[1].weight = 40;
        }
        flag.rules = vec![rule];

        assert_eq!(
            evaluate(&flag, &ctx(Some("u"), &[])).reason,
            EvalReason::Error
        );
    }

    /// A broken split is reported whether or not the caller sent a key.
    #[test]
    fn a_malformed_split_degrades_to_error_even_without_a_subject() {
        let mut flag = string_flag_with_variants();
        flag.rules = vec![FlagRule {
            description: None,
            conditions: Vec::new(),
            rollout_percentage: None,
            serve: RuleServe::Split { split: Vec::new() },
        }];
        assert_eq!(
            evaluate(&flag, &EvalContext::default()).reason,
            EvalReason::Error
        );

        let mut ghost = fifty_fifty();
        if let RuleServe::Split { split } = &mut ghost.serve {
            split[1].variant = "ghost".to_string();
        }
        flag.rules = vec![ghost];
        assert_eq!(
            evaluate(&flag, &EvalContext::default()).reason,
            EvalReason::Error
        );
        assert_eq!(
            evaluate(&flag, &ctx(Some("u"), &[])).reason,
            EvalReason::Error
        );
    }

    #[test]
    fn an_uncompilable_pattern_degrades_to_error() {
        let mut flag = snapshot(true, None);
        flag.rules = vec![serve_true_when(vec![condition(
            "email",
            ConditionOperator::Matches,
            Some(ConditionValue::Single(s("(unclosed"))),
        )])];

        let result = evaluate(&flag, &ctx(None, &[("email", s("x"))]));
        assert_eq!(result.value, serde_json::json!(false));
        assert_eq!(result.reason, EvalReason::Error);
    }

    #[test]
    fn an_operand_of_the_wrong_shape_degrades_to_error() {
        let mut flag = snapshot(true, None);
        flag.rules = vec![serve_true_when(vec![condition(
            "plan",
            ConditionOperator::In,
            Some(ConditionValue::Single(s("pro"))),
        )])];

        assert_eq!(
            evaluate(&flag, &ctx(None, &[("plan", s("pro"))])).reason,
            EvalReason::Error
        );
    }

    #[test]
    fn a_rule_serving_a_wrongly_typed_value_degrades_to_error() {
        let mut flag = snapshot(true, None);
        flag.rules = vec![FlagRule {
            description: None,
            conditions: Vec::new(),
            rollout_percentage: None,
            serve: RuleServe::Value {
                value: serde_json::json!("yes"),
            },
        }];

        let result = evaluate(&flag, &EvalContext::default());
        assert_eq!(result.value, serde_json::json!(false));
        assert_eq!(result.reason, EvalReason::Error);
    }

    #[test]
    fn precompiled_and_on_the_fly_evaluation_agree() {
        let mut flag = snapshot(true, None);
        flag.rules = vec![serve_true_when(vec![condition(
            "email",
            ConditionOperator::Matches,
            Some(ConditionValue::Single(s(r".+@acme\.test"))),
        )])];
        let patterns = CompiledPatterns::for_flag(&flag);

        for email in ["a@acme.test", "a@other.test", ""] {
            let context = ctx(None, &[("email", s(email))]);
            assert_eq!(
                evaluate_compiled(&flag, &patterns, &context),
                evaluate(&flag, &context)
            );
        }
    }

    #[test]
    fn oversized_patterns_are_rejected() {
        assert!(compile_pattern(&"a".repeat(MAX_PATTERN_LEN + 1)).is_err());
        assert!(
            compile_pattern("a{1000}{1000}").is_err(),
            "program size is bounded"
        );
        assert!(compile_pattern(r"[a-z]+").is_ok());
    }

    #[test]
    fn rules_round_trip_through_their_stored_json() {
        let stored = serde_json::json!([
            {
                "description": "Beta",
                "conditions": [
                    {"attribute": "plan", "operator": "in", "value": ["pro", "team"]},
                    {"attribute": "seats", "operator": "gte", "value": 5},
                    {"attribute": "email", "operator": "exists"}
                ],
                "rollout_percentage": 20.0,
                "serve": {"kind": "split", "split": [
                    {"variant": "control", "weight": 50},
                    {"variant": "test", "weight": 50}
                ]}
            },
            {"conditions": [], "serve": {"kind": "value", "value": true}},
            {"serve": {"kind": "variant", "variant": "test"}}
        ]);
        let rules: Vec<FlagRule> = serde_json::from_value(stored.clone()).unwrap();
        assert_eq!(rules.len(), 3);
        assert_eq!(
            rules[0].conditions[0].value,
            Some(ConditionValue::List(vec![s("pro"), s("team")]))
        );
        assert_eq!(
            rules[0].conditions[1].value,
            Some(ConditionValue::Single(AttributeValue::Number(5.0)))
        );
        let back: Vec<FlagRule> =
            serde_json::from_value(serde_json::to_value(&rules).unwrap()).unwrap();
        assert_eq!(back, rules);
    }

    /// The split bucket is part of the assignment contract exactly like
    /// `bucket()`: changing it re-assigns every subject of every running
    /// experiment. Locked the same way.
    #[test]
    fn split_bucket_locked_vectors() {
        let cases = [
            ("checkout.v2", "a1b2c3", "user_1001", SPLIT_1001),
            ("checkout.v2", "a1b2c3", "user_1002", SPLIT_1002),
            ("checkout.v2", "z9y8x7", "user_1001", SPLIT_1001_ROTATED),
        ];
        for (flag, salt, subject, expected) in cases {
            let actual = split_bucket(flag, salt, subject);
            assert!(
                (actual - expected).abs() < 0.005,
                "split_bucket({flag}, {salt}, {subject}) = {actual}, expected {expected}"
            );
        }
        assert!(
            (split_bucket("checkout.v2", "a1b2c3", "user_1001")
                - bucket("checkout.v2", "a1b2c3", "user_1001"))
            .abs()
                > 0.5,
            "the split position must be independent of the rollout position"
        );
    }

    const SPLIT_1001: f64 = 94.69;
    const SPLIT_1002: f64 = 14.38;
    const SPLIT_1001_ROTATED: f64 = 77.71;

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
