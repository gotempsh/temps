// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

pub mod service;
pub use service::*;
mod user_agent;

// Shared request enrichment for other telemetry adapters.
pub use user_agent::BrowserInfo;
