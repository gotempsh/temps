// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

use thiserror::Error;

/// Errors raised while turning stored rules into a program.
#[derive(Debug, Error, Clone, PartialEq, Eq)]
pub enum EdgeRuleError {
    /// A stored rule failed to compile. Rules are validated on write, so this
    /// means the row bypassed validation (manual edit, downgrade or a bug).
    #[error("Edge rule {rule_id} failed to compile: {reason}")]
    Compile { rule_id: i32, reason: String },

    /// The stored JSON does not match the rule model.
    #[error("Edge rule {rule_id} has an invalid stored {part}: {reason}")]
    Decode {
        rule_id: i32,
        part: &'static str,
        reason: String,
    },
}
