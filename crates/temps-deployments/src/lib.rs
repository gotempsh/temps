// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! deployments services and utilities

#![cfg_attr(
    test,
    allow(
        clippy::disallowed_methods,
        reason = "tests read fixture configuration (database/service URLs) from the environment"
    )
)]
pub mod cluster_ca;
pub mod handlers;
pub mod jobs;
pub mod plugin;
pub mod services;
pub mod test_utils;
pub mod utils;

#[allow(ambiguous_glob_reexports)]
pub use handlers::*;
pub use jobs::*;
pub use plugin::*;
#[allow(ambiguous_glob_reexports)]
pub use services::*;
