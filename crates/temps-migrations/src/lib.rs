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

pub use migration::m20261022_000001_dns_name_claims::{
    backfill_sql as dns_name_claims_backfill_sql, Migration as DnsNameClaimsMigration,
};

pub use migration::m20261022_000002_workloads::{
    Migration as WorkloadsMigration, WORKLOAD_TABLES_SQL,
};

pub use migration::m20260921_000001_http_checks::Migration as HttpChecksMigration;

pub use migration::m20260921_000002_env_check_history::Migration as EnvCheckHistoryMigration;

pub use migration::m20260921_000003_detection_retry::Migration as DetectionRetryMigration;

pub use migration::m20260921_000004_credential_catalog::Migration as CredentialCatalogMigration;
pub use migration::m20260922_000001_stateless_control_plane_jobs::Migration as StatelessControlPlaneJobsMigration;
pub use migration::m20261002_000001_secret_checks_and_history::Migration as SecretChecksAndHistoryMigration;

pub use migration::m20261016_000002_ci_build_dispatch::Migration as CiBuildDispatchMigration;

pub use migration::m20261017_000001_retain_ci_cancellation::Migration as RetainCiCancellationMigration;

pub use migration::m20261017_000002_runtime_health::Migration as RuntimeHealthMigration;

pub use migration::m20261019_000002_reconcile_error_group_counts::Migration as ReconcileErrorGroupCountsMigration;

pub use migration::m20261022_000007_network_policy::{
    backfill_sql as network_policy_backfill_sql, Migration as NetworkPolicyMigration,
};
pub use migration::m20261022_000008_ingress_endpoints::Migration as IngressEndpointsMigration;
