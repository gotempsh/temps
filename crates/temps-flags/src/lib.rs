// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! temps-flags: feature flags for the Temps platform (ADR-034, Phase 1).
//!
//! A flag is defined once per project and its value is overridden per
//! environment. Changing a value takes effect in seconds; changing an
//! environment variable requires a redeploy. That difference is the entire
//! reason this crate exists.
//!
//! Phase 1 deliberately ships only the kill switch — set a value per
//! environment, flip it without a redeploy. What it also does is fix the parts
//! that are expensive to change once callers depend on them: the resolution
//! order, the `reason` wire contract, typed values, the `client_visible`
//! security default, and the bucketing algorithm. Targeting rules slot into a
//! reserved step in [`eval::evaluate`] without disturbing any of it.

pub mod error;
pub mod eval;
pub mod handlers;
pub mod plugin;
pub mod services;

pub use error::FlagError;
pub use eval::{
    bucket, evaluate, not_found, AttributeValue, EvalContext, EvalReason, Evaluation, FlagSnapshot,
    FlagValueType,
};
pub use plugin::FlagsPlugin;
pub use services::FlagService;
