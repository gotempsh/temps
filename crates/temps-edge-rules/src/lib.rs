// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! # temps-edge-rules
//!
//! The engine behind project edge rules (ADR-089): the rule model, write-time
//! validation, plain-English descriptions, and a compiled evaluator the proxy
//! runs on every request.
//!
//! The crate is pure: no database, no HTTP framework, no async runtime. The
//! same code serves the proxy's hot path, the API's dry run
//! (`POST /projects/{id}/edge-rules/evaluate`), the CLI and the MCP tools, and
//! the conformance fixtures in `tests/conformance/` hold it to the same
//! answers as the console's in-browser tester.

pub mod describe;
pub mod error;
pub mod eval;
pub mod fields;
pub mod ip;
pub mod model;
pub mod normalize;
pub mod validate;

pub use describe::{auto_name, describe_action, describe_expression};
pub use error::EdgeRuleError;
pub use eval::{
    Decision, Evaluation, GeoFacts, MatchRef, ParsedSample, Program, RequestView, RuleOutcome,
    RuleTrace, SampleHeader, SampleRequest, Visit,
};
pub use fields::limits;
pub use model::{
    Action, ActionKind, Combinator, Condition, Expression, Field, Group, Mode, Operator,
    RuleDefinition,
};
pub use validate::{prepare_expression, validate_rule, PatternUsage, RuleIssue};
