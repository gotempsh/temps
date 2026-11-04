// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Docker housekeeping: scheduled cleanup of images, build cache and assets, and the Docker disk usage report with its HTTP handler.

#![cfg_attr(
    test,
    allow(
        clippy::disallowed_methods,
        reason = "tests read fixture configuration (database/service URLs) from the environment"
    )
)]
pub mod handlers;
pub mod services;
