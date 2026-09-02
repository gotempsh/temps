// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! ADR-040 §1: constrain `projects.cloud_telemetry_fidelity` to the two tiers
//! the code knows how to read.
//!
//! `m20260901_000001_add_cloud_telemetry_fidelity` added the column as plain
//! `TEXT`, matching the `SourceType` precedent on the same table. Its sibling
//! `m20260901_000002_create_cloud_telemetry_backfills` did add a `CHECK` to its
//! own status column; this closes the gap between them.
//!
//! # Why it matters more than a normal enum-column check
//!
//! `CloudPolicyCache` resolves fidelity for **every distinct project in an
//! ingest batch with one query**, decoding the rows through a single
//! `FromQueryResult`. A value outside the enum fails that decode for the whole
//! result set, so one bad row silently drops every co-resolved project in that
//! batch to `metered` behind a single `warn!`. That is the safe direction —
//! nothing over-shares — but it is close to undiagnosable from the outside: the
//! symptom is "the project I opted in stopped being queryable", with no row
//! visibly wrong. Refusing the write is how the bad value never gets there.
//!
//! # Why a separate migration rather than an edit to 000001
//!
//! Editing an already-applied migration in place is a no-op on any database
//! that has run it, which would leave exactly the skew this constraint exists
//! to prevent — and this changeset's own development databases have already
//! applied 000001. A new migration reaches both fresh and existing databases.
//!
//! Both existing values are written only by `CloudTelemetryFidelity`'s
//! `DeriveActiveEnum` mapping (`metered` / `queryable`) and the column's own
//! `DEFAULT 'metered'`, so validating existing rows cannot fail on a database
//! this instance wrote.

use sea_orm_migration::prelude::*;

/// Kept in sync with `temps_entities::cloud_telemetry_fidelity`'s
/// `string_value`s. There are deliberately only two: adding a tier is a code
/// change plus a migration, not a value someone can type into the column.
const CONSTRAINT_NAME: &str = "projects_cloud_telemetry_fidelity_valid";

#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .get_connection()
            .execute_unprepared(&format!(
                "ALTER TABLE projects ADD CONSTRAINT {CONSTRAINT_NAME} \
                 CHECK (cloud_telemetry_fidelity IN ('metered', 'queryable'))"
            ))
            .await?;

        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .get_connection()
            .execute_unprepared(&format!(
                "ALTER TABLE projects DROP CONSTRAINT IF EXISTS {CONSTRAINT_NAME}"
            ))
            .await?;

        Ok(())
    }
}
