// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! ADR-041 §1: per-project telemetry write mode.
//!
//! One additive, defaulted column on `projects`:
//!
//! - `cloud_telemetry_write_mode TEXT NOT NULL DEFAULT 'local'` — whether this
//!   project's spans are stored on this instance at all. `local` reproduces
//!   today's behaviour exactly; `cloud` is a deliberate per-project opt-in that
//!   the service layer refuses unless the project is already at `queryable`
//!   fidelity, the instance is linked, and the Cloud telemetry switch is on.
//!
//! NOT NULL with a default rather than nullable: every project has a write
//! mode, and "unset" would just be `local` spelled ambiguously — which on this
//! particular column would be the difference between "spans are stored" and
//! "spans are not stored".
//!
//! Not a secret (it is a routing decision an operator must be able to read back
//! verbatim), so it does not go through `EncryptionService`. Same `TEXT` +
//! `CHECK` shape as `cloud_telemetry_fidelity` after
//! `m20260901_000003_constrain_cloud_telemetry_fidelity`, and for the same
//! reason: `CloudPolicyCache` decodes this column for every distinct project in
//! an ingest batch through one `FromQueryResult`, so a value outside the enum
//! would fail the decode for the whole result set.
//!
//! The check is added in the same migration as the column here, rather than
//! trailing it, because nothing has applied this one yet.

use sea_orm_migration::prelude::*;

/// Kept in sync with `temps_entities::cloud_telemetry_write_mode`'s
/// `string_value`s.
const CONSTRAINT_NAME: &str = "projects_cloud_telemetry_write_mode_valid";

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
                        ColumnDef::new(Projects::CloudTelemetryWriteMode)
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
                 CHECK (cloud_telemetry_write_mode IN ('local', 'cloud'))"
            ))
            .await?;

        // The §1 gate lives in the service layer, but this closes the worst
        // configuration by construction at the storage layer too: a
        // Cloud-primary project at `metered` fidelity would discard real spans
        // locally and store unreadable placeholders in Cloud — nothing readable
        // anywhere. A direct `UPDATE`, a restored dump, or a future code path
        // that forgets the gate all hit this instead of producing that state.
        manager
            .get_connection()
            .execute_unprepared(
                "ALTER TABLE projects ADD CONSTRAINT projects_cloud_primary_requires_queryable \
                 CHECK (cloud_telemetry_write_mode <> 'cloud' \
                        OR cloud_telemetry_fidelity = 'queryable')",
            )
            .await?;

        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .get_connection()
            .execute_unprepared(
                "ALTER TABLE projects \
                 DROP CONSTRAINT IF EXISTS projects_cloud_primary_requires_queryable",
            )
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
                    .drop_column(Projects::CloudTelemetryWriteMode)
                    .to_owned(),
            )
            .await?;

        Ok(())
    }
}

#[derive(DeriveIden)]
enum Projects {
    Table,
    CloudTelemetryWriteMode,
}
