// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Worker nodes: registration and heartbeats, scheduling and placement, SSH
//! enrollment, pairing, the WireGuard mesh, and their HTTP handlers.

#![cfg_attr(
    test,
    allow(
        clippy::disallowed_methods,
        reason = "tests read fixture configuration (database/service URLs) from the environment"
    )
)]
pub mod cluster_ca;
pub mod handlers;
pub mod services;
