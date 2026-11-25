// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Per-environment opt-in to recording outbound connections.
//!
//! Adds `environments.egress_recording`: when true, containers of the
//! environment's deployments are started with the per-node egress proxy as
//! their HTTP(S) proxy, the proxy records every connection they make
//! through it (destination, outcome and refusal reason, bytes and duration;
//! never headers, bodies or query strings) and the control plane stores
//! each record as an OpenTelemetry log record owned by the environment and
//! its project. It is the same recording sandboxes have
//! (`sandboxes.egress_recording`), for another workload kind.
//!
//! It lives on the environment's own row, not on a deployment, so it
//! survives redeploys, is changed per environment through the API and is
//! audit-logged by that write.
//!
//! Off by default and for every existing environment, so the migration
//! changes no behaviour. Additive: the previous binary ignores the column.

use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

pub const UP_SQL: &str = r#"
ALTER TABLE environments
    ADD COLUMN IF NOT EXISTS egress_recording BOOLEAN NOT NULL DEFAULT false;
"#;

pub const DOWN_SQL: &str = r#"
ALTER TABLE environments DROP COLUMN IF EXISTS egress_recording;
"#;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager.get_connection().execute_unprepared(UP_SQL).await?;
        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .get_connection()
            .execute_unprepared(DOWN_SQL)
            .await?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn up_adds_the_column_off_for_every_environment() {
        assert!(UP_SQL
            .contains("ADD COLUMN IF NOT EXISTS egress_recording BOOLEAN NOT NULL DEFAULT false"));
    }

    #[test]
    fn down_drops_the_column() {
        assert!(DOWN_SQL.contains("DROP COLUMN IF EXISTS egress_recording"));
    }

    #[test]
    fn is_registered() {
        let names: Vec<String> = crate::migration::Migrator::migrations()
            .into_iter()
            .map(|m| m.name().to_string())
            .collect();
        assert!(names
            .iter()
            .any(|n| n == "m20261125_000005_environment_egress_recording"));
    }
}
