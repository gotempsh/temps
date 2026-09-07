// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! ADR-043 §1: per-project write mode for non-span analytics signals.
//!
//! Mirrors `m20260901_000005_add_cloud_telemetry_write_mode` exactly, for the
//! second write-mode switch described in ADR-043. Where that migration controls
//! span writes, this one controls the group of signals that share a single Cloud
//! switch: `otel_metrics`, `service_metrics`, `analytics_events`,
//! `analytics_sessions`, and `proxy_logs`.
//!
//! # Column
//!
//! `cloud_analytics_write_mode TEXT NOT NULL DEFAULT 'local'` with a
//! `CHECK (cloud_analytics_write_mode IN ('local', 'cloud'))` constraint.
//!
//! `local` is today's behaviour for every signal in this group (write to the
//! local store — TimescaleDB hypertable or Postgres system-of-record — and do
//! not queue anything for Cloud). `cloud` is a per-project opt-in that the
//! service layer refuses unless `cloud_telemetry_fidelity = 'queryable'`, the
//! instance is linked, and the Cloud telemetry feature switch is on — the same
//! gate as `cloud_telemetry_write_mode`.
//!
//! # Why this migration does NOT add a cross-column CHECK
//!
//! `m20260901_000005` added a `CHECK` enforcing that a Cloud-primary project
//! must be at `queryable` fidelity, closing a configuration that would produce
//! data stored nowhere readable. The same constraint applies to analytics — a
//! Cloud-primary analytics project at `metered` fidelity is equally broken.
//! That CHECK is added here for the same reason: belt-and-suspenders, because
//! a direct SQL `UPDATE`, a restored dump, or a future migration that forgets
//! the gate all need to hit the constraint rather than produce a silently broken
//! configuration.

use sea_orm_migration::prelude::*;

/// Kept in sync with `temps_entities::cloud_analytics_write_mode`'s
/// `string_value`s.
const CONSTRAINT_NAME: &str = "projects_cloud_analytics_write_mode_valid";

/// Mirrors `projects_cloud_primary_requires_queryable` from the span migration.
const FIDELITY_CONSTRAINT_NAME: &str = "projects_cloud_analytics_primary_requires_queryable";

#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .alter_table(
                Table::alter()
                    .table(Projects::Table)
                    .add_column(
                        ColumnDef::new(Projects::CloudAnalyticsWriteMode)
                            .text()
                            .not_null()
                            .default("local"),
                    )
                    .to_owned(),
            )
            .await?;

        manager
            .get_connection()
            .execute_unprepared(&format!(
                "ALTER TABLE projects ADD CONSTRAINT {CONSTRAINT_NAME} \
                 CHECK (cloud_analytics_write_mode IN ('local', 'cloud'))"
            ))
            .await?;

        manager
            .get_connection()
            .execute_unprepared(&format!(
                "ALTER TABLE projects ADD CONSTRAINT {FIDELITY_CONSTRAINT_NAME} \
                 CHECK (cloud_analytics_write_mode <> 'cloud' \
                        OR cloud_telemetry_fidelity = 'queryable')"
            ))
            .await?;

        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .get_connection()
            .execute_unprepared(&format!(
                "ALTER TABLE projects DROP CONSTRAINT IF EXISTS {FIDELITY_CONSTRAINT_NAME}"
            ))
            .await?;

        manager
            .get_connection()
            .execute_unprepared(&format!(
                "ALTER TABLE projects DROP CONSTRAINT IF EXISTS {CONSTRAINT_NAME}"
            ))
            .await?;

        manager
            .alter_table(
                Table::alter()
                    .table(Projects::Table)
                    .drop_column(Projects::CloudAnalyticsWriteMode)
                    .to_owned(),
            )
            .await?;

        Ok(())
    }
}

#[derive(DeriveIden)]
enum Projects {
    Table,
    CloudAnalyticsWriteMode,
}
