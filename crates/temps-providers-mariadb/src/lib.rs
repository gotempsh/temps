// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! The MariaDB managed-service engine: container lifecycle, backup and point-in-time restore, data import, binlog health.

#![cfg_attr(
    test,
    allow(
        clippy::disallowed_methods,
        reason = "tests read fixture configuration (database/service URLs) from the environment"
    )
)]
pub mod mariadb;
pub mod mariadb_binlog_health;
pub mod query;

// The engine code addresses the contract it implements through these paths.
pub(crate) use temps_providers_core::externalsvc::*;
#[allow(unused_imports, reason = "not every engine uses all of them")]
pub(crate) use temps_providers_core::{data_import, externalsvc, readiness, utils, workload};
