// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Allow `restore_runs.status = 'interrupted'`.
//!
//! A restore whose owning worker died with the Temps process (restart, crash,
//! OOM kill) is reconciled at the next boot into this terminal status. It is
//! deliberately distinct from `failed`: a failed restore ran to an error the
//! engine reported, while an interrupted one stopped at an unknown point
//! inside its last recorded phase and may have left the target partially
//! restored.

use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .get_connection()
            .execute_unprepared(
                r#"
ALTER TABLE restore_runs DROP CONSTRAINT IF EXISTS restore_runs_status_check;
ALTER TABLE restore_runs ADD CONSTRAINT restore_runs_status_check
    CHECK (status IN ('pending', 'running', 'completed', 'failed', 'cancelled', 'interrupted'));
"#,
            )
            .await?;
        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        // Interrupted runs become plain failures: the closest outcome the
        // older schema can represent. Their error_message still says why.
        manager
            .get_connection()
            .execute_unprepared(
                r#"
UPDATE restore_runs SET status = 'failed' WHERE status = 'interrupted';
ALTER TABLE restore_runs DROP CONSTRAINT IF EXISTS restore_runs_status_check;
ALTER TABLE restore_runs ADD CONSTRAINT restore_runs_status_check
    CHECK (status IN ('pending', 'running', 'completed', 'failed', 'cancelled'));
"#,
            )
            .await?;
        Ok(())
    }
}
