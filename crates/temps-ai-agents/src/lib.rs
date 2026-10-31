// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

#![cfg_attr(
    test,
    allow(
        clippy::disallowed_methods,
        reason = "tests read fixture configuration (database/service URLs) from the environment"
    )
)]
pub mod ai_cli;
mod docker_network_isolation;
pub mod error;
pub mod handlers;
pub mod plugin;
pub mod preview_gateway;
pub mod sandbox;
pub mod services;

pub use plugin::AgentsPlugin;
