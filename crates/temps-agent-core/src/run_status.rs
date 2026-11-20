// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! The states an agent run can be in, as stored in `agent_runs.status`.

/// Non-terminal run statuses: a run in one of these states is in-flight.
/// Used for concurrency counting and restart recovery.
pub const ACTIVE_RUN_STATUSES: &[&str] = &[
    "pending",
    "cloning",
    "analyzing",
    "fixing",
    "pushing",
    "creating_pr",
    "deploying",
];

/// Terminal run statuses: once a run reaches one of these it never
/// transitions again. Consumers outside this crate (e.g. the sandbox
/// plugin's orphaned agent-run sandbox cleanup) rely on this to decide
/// when resources attributed to a run can safely be released.
pub const TERMINAL_RUN_STATUSES: &[&str] = &["completed", "failed", "no_fix", "cancelled"];
