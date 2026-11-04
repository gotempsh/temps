// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

#![cfg_attr(
    test,
    allow(
        clippy::disallowed_methods,
        reason = "tests read fixture configuration (database/service URLs) from the environment"
    )
)]
pub mod handlers;
pub mod plugin;
pub mod preview_gateway;
pub mod services;

pub use temps_agent_core::error;
pub use temps_ai_cli as ai_cli;
pub use temps_sandbox_providers as sandbox;

pub use plugin::AgentsPlugin;
