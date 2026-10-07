// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Let a restore run be cancelled while it downloads or provisions.
//!
//! - `cancel_requested_at` / `cancel_requested_by` record a cancellation. The
//!   API sets them only while the run is in a cancellable phase, and the
//!   worker's move out of that phase is conditional on them being NULL, so a
//!   cancellation and the restore's first write can never both succeed.
//! - `download` is the new phase in which an in-place restore stages the
//!   backup before it writes anything to the target.

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
ALTER TABLE restore_runs ADD COLUMN IF NOT EXISTS cancel_requested_at TIMESTAMPTZ NULL;
ALTER TABLE restore_runs ADD COLUMN IF NOT EXISTS cancel_requested_by INTEGER NULL
    REFERENCES users(id) ON DELETE SET NULL;
ALTER TABLE restore_runs DROP CONSTRAINT IF EXISTS restore_runs_phase_check;
ALTER TABLE restore_runs ADD CONSTRAINT restore_runs_phase_check
    CHECK (phase IN ('prepare', 'download', 'provision', 'restore', 'recover', 'verify', 'completed', 'failed'));
"#,
            )
            .await?;
        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        // A run that stopped in `download` never wrote to its target; the
        // older schema's closest phase for that is `prepare`.
        manager
            .get_connection()
            .execute_unprepared(
                r#"
UPDATE restore_runs SET phase = 'prepare' WHERE phase = 'download';
ALTER TABLE restore_runs DROP CONSTRAINT IF EXISTS restore_runs_phase_check;
ALTER TABLE restore_runs ADD CONSTRAINT restore_runs_phase_check
    CHECK (phase IN ('prepare', 'provision', 'restore', 'recover', 'verify', 'completed', 'failed'));
ALTER TABLE restore_runs DROP COLUMN IF EXISTS cancel_requested_by;
ALTER TABLE restore_runs DROP COLUMN IF EXISTS cancel_requested_at;
"#,
            )
            .await?;
        Ok(())
    }
}
