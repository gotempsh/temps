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
pub use migration::m20260916_000001_reconcile_otel_trace_summaries::Migration as ReconcileOtelTraceSummariesMigration;
pub use migration::m20260916_000001_reconcile_otel_trace_summaries::{
    trace_summary_rebuild_delta_sql, trace_summary_rebuild_initial_sql,
};
pub use migration::Migrator;

pub use migration::m20260921_000001_http_checks::Migration as HttpChecksMigration;

pub use migration::m20260921_000002_env_check_history::Migration as EnvCheckHistoryMigration;

pub use migration::m20260921_000003_detection_retry::Migration as DetectionRetryMigration;
