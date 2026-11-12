// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Let a restore run record the two phases of a restore on a worker node.
//!
//! - `waiting_for_node`: the node already runs its maximum of backups and
//!   restores; nothing has run on it yet, and the run can be cancelled.
//! - `outcome_unknown`: the node's restore may still be writing, but its
//!   outcome cannot be established (the node is unreachable, or has no
//!   record of a restore it may have received). The run stays active and
//!   holds its service until the outcome is known or an operator releases
//!   it by cancelling the run.
//!
//! Existing rows are unaffected: both phases are new.

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
ALTER TABLE restore_runs DROP CONSTRAINT IF EXISTS restore_runs_phase_check;
ALTER TABLE restore_runs ADD CONSTRAINT restore_runs_phase_check
    CHECK (phase IN ('prepare', 'download', 'provision', 'waiting_for_node', 'restore', 'recover',
                     'outcome_unknown', 'verify', 'completed', 'failed'));
"#,
            )
            .await?;
        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        // The older schema's closest phases: a run waiting for its node has
        // not started (`prepare`); one whose outcome is unknown was restoring.
        manager
            .get_connection()
            .execute_unprepared(
                r#"
UPDATE restore_runs SET phase = 'prepare' WHERE phase = 'waiting_for_node';
UPDATE restore_runs SET phase = 'restore' WHERE phase = 'outcome_unknown';
ALTER TABLE restore_runs DROP CONSTRAINT IF EXISTS restore_runs_phase_check;
ALTER TABLE restore_runs ADD CONSTRAINT restore_runs_phase_check
    CHECK (phase IN ('prepare', 'download', 'provision', 'restore', 'recover', 'verify', 'completed', 'failed'));
"#,
            )
            .await?;
        Ok(())
    }
}
