// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Standalone sandbox API backed by the same `SandboxProvider` primitive
//! used by agent runs and workspace sessions.
//!
//! This crate owns the HTTP surface at `/v1/sandbox/*`. The request/response
//! shapes are compatible with the `@vercel/sandbox` npm SDK so drop-in
//! clients work without modification — see `tests/vercel_compat.rs` for the
//! pinned contract. Unlike the workspace API, it has no chat concepts
//! (no messages, no AI provider config, no skills); unlike the agent-runs
//! API, it has no multi-phase workflow or PR creation.
//!
//! Architecture:
//! - One shared [`SandboxProvider`](temps_agents::sandbox::SandboxProvider)
//!   instance is registered by the `temps-agents` plugin and consumed here.
//! - Sandboxes are persisted in the `sandboxes` table so that the API can
//!   enumerate them and recover container handles across restarts.
//! - Public IDs are opaque strings (e.g. `sbx_a1b2c3d4`) so callers never
//!   see internal numeric IDs; an internal `i32` keys into the provider's
//!   `run_id`-addressed handle map for compatibility.

pub mod error;
pub mod handlers;
pub mod plugin;
pub mod services;
