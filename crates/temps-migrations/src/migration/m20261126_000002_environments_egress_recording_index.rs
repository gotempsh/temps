// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! An index on the environments that record their outbound connections.
//!
//! A host serving deployments asks every few seconds whether any live
//! environment records (`egress_recording AND deleted_at IS NULL`, one row
//! at most). With nothing opted in, the default, that lookup had to read
//! every live environment to find none. A partial index holding only the
//! opted-in, live rows answers it from those rows alone: nothing when none
//! records, whatever the number of environments.
//!
//! Additive: the previous binary ignores the index.

use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

pub const UP_SQL: &str = r#"
CREATE INDEX IF NOT EXISTS idx_environments_egress_recording
    ON environments (id)
    WHERE egress_recording AND deleted_at IS NULL;
"#;

pub const DOWN_SQL: &str = r#"
DROP INDEX IF EXISTS idx_environments_egress_recording;
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
    fn the_index_holds_only_live_environments_that_record() {
        assert!(UP_SQL.contains("WHERE egress_recording AND deleted_at IS NULL"));
        assert!(DOWN_SQL.contains("DROP INDEX IF EXISTS idx_environments_egress_recording"));
    }
}
