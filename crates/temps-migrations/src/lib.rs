// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Database migrations for the Temps application
//!
//! This crate contains all database migration files that will be
//! moved from src/migration/

pub use sea_orm_migration::prelude::*;

// Module removed for initial build
mod migration;
// Re-export for convenience
// Re-export removed
pub use migration::m20260805_000001_index_normalized_managed_domains::Migration as NormalizedManagedDomainIndexMigration;
pub use migration::m20260806_000001_sandbox_workspace_lifecycle::Migration as SandboxWorkspaceLifecycleMigration;
pub use migration::Migrator;
