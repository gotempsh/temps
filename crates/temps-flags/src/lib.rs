// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! temps-flags: feature flags for the Temps platform (ADR 034 Phases 1-2,
//! ADR 110 D3).
//!
//! A flag is defined once per project and its value is overridden per
//! environment. Changing a value takes effect in seconds; changing an
//! environment variable requires a redeploy. That difference is the entire
//! reason this crate exists.
//!
//! Phase 1 shipped the kill switch and fixed the parts that are expensive to
//! change once callers depend on them: the resolution order, the `reason` wire
//! contract, typed values, the `client_visible` security default, and the
//! bucketing algorithm. Targeting (rules, percentage rollouts, named variants
//! and weighted splits) fills the step [`eval::evaluate`] reserved for it, and
//! browsers read `client_visible` flags, evaluated server-side, from the
//! same-origin `/api/_temps/flags` endpoint.

pub mod error;
pub mod eval;
pub mod handlers;
pub mod plugin;
pub mod services;

pub use error::FlagError;
pub use eval::{
    bucket, evaluate, evaluate_compiled, not_found, split_bucket, suffixed_bucket, AttributeValue,
    CompiledPatterns, ConditionOperator, ConditionValue, EvalContext, EvalReason, Evaluation,
    FlagRule, FlagSnapshot, FlagValueType, FlagVariant, RuleCondition, RuleServe, VariantWeight,
};
pub use plugin::FlagsPlugin;
pub use services::FlagService;
