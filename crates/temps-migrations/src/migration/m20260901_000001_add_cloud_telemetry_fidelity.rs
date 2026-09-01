// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! ADR-040 §1: per-project fidelity gate for the Temps Cloud telemetry mirror.
//!
//! Adds two additive, defaulted columns to `projects`:
//!
//! - `cloud_telemetry_fidelity TEXT NOT NULL DEFAULT 'metered'` — how much of a
//!   span may leave this instance. `metered` reproduces today's behaviour
//!   exactly (pseudonymised identifiers, constant span name, no attributes);
//!   `queryable` is a deliberate per-project opt-in.
//! - `cloud_telemetry_attribute_allowlist TEXT[] NOT NULL DEFAULT '{}'` — the
//!   exact-match keys whose span attributes may be mirrored at `queryable`
//!   fidelity. Empty means none, which is the safe default even after opting
//!   in.
//!
//! Both are NOT NULL with a default rather than nullable, because the absence
//! of a value here is not a meaningful third state: every project has a
//! fidelity, and "unset" would just be `metered` spelled ambiguously.
//!
//! Neither column is a secret — a consent flag an operator cannot read back
//! verbatim is not consent — so neither goes through `EncryptionService`.
//!
//! The fidelity column is a `TEXT` check-free enum matching the `SourceType`
//! precedent on this same table: Sea-ORM's `DeriveActiveEnum` maps the strings,
//! and a Postgres `ENUM` type would make adding a future tier a type mutation
//! rather than a code change.
//!
//! The allowlist column is created with raw SQL because the Sea-ORM schema
//! builder has no Postgres array type — the same reason
//! `m20260310_000005_add_agent_tracking_to_ai_usage_logs` does it that way for
//! `ai_usage_logs.tags`.

use sea_orm_migration::prelude::*;

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
                        ColumnDef::new(Projects::CloudTelemetryFidelity)
                            .text()
                            .not_null()
                            .default("metered"),
                    )
                    .to_owned(),
            )
            .await?;

        manager
            .get_connection()
            .execute_unprepared(
                "ALTER TABLE projects \
                 ADD COLUMN cloud_telemetry_attribute_allowlist TEXT[] NOT NULL DEFAULT '{}'",
            )
            .await?;

        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .alter_table(
                Table::alter()
                    .table(Projects::Table)
                    .drop_column(Projects::CloudTelemetryAttributeAllowlist)
                    .to_owned(),
            )
            .await?;

        manager
            .alter_table(
                Table::alter()
                    .table(Projects::Table)
                    .drop_column(Projects::CloudTelemetryFidelity)
                    .to_owned(),
            )
            .await?;

        Ok(())
    }
}

#[derive(DeriveIden)]
enum Projects {
    Table,
    CloudTelemetryFidelity,
    CloudTelemetryAttributeAllowlist,
}
