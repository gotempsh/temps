// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! The PostgreSQL managed-service engines: standalone and HA cluster lifecycle, WAL-G backup and point-in-time restore, major upgrades, data import.

#![cfg_attr(
    test,
    allow(
        clippy::disallowed_methods,
        reason = "tests read fixture configuration (database/service URLs) from the environment"
    )
)]
pub mod dump_filter;
pub mod postgres;
pub mod postgres_cluster;
pub mod postgres_cluster_tls;
pub mod postgres_upgrade;
pub mod postgres_wal_health;
pub mod query;

// The engine code addresses the contract it implements through these paths.
pub(crate) use temps_providers_core::externalsvc::*;
#[allow(unused_imports, reason = "not every engine uses all of them")]
pub(crate) use temps_providers_core::{data_import, externalsvc, readiness, utils, workload};
