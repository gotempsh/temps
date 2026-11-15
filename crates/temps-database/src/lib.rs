// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Database connection and query utilities

#![cfg_attr(
    test,
    allow(
        clippy::disallowed_methods,
        reason = "tests read fixture configuration (database/service URLs) from the environment"
    )
)]
pub use sea_orm;
pub mod approx_count;
mod connection;

pub use approx_count::{approximate_row_count, count_for_pagination, CountKind};
pub use connection::{
    cancel_migration_backend, connect_for_migrate, connect_options, connect_pooled,
    connect_without_migrations, retry_database_startup, wait_for_database, DbConnection,
    StartupAttemptFailure, DEFAULT_DATABASE_STARTUP_WAIT,
};
