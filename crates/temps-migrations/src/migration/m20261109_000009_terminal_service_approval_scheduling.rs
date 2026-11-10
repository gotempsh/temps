// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Terminal approvals retain their history but no longer have a scheduled check.
//! Existing rows are drained in bounded batches by the reconciler, not backfilled
//! under a schema lock. The existing timestamp index excludes NULL from due scans.
use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager.get_connection().execute_unprepared(
            "ALTER TABLE sandbox_service_assignments ALTER COLUMN next_reconcile_at DROP NOT NULL",
        ).await?;
        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        // Do not silently reschedule terminal approvals for an older binary.
        manager.get_connection().execute_unprepared(r#"
DO $$ BEGIN
 IF EXISTS (SELECT 1 FROM sandbox_service_assignments WHERE next_reconcile_at IS NULL) THEN
  RAISE EXCEPTION 'Cannot restore service approval scheduling: terminal approvals have no next check; retain the current schema';
 END IF;
END $$;
ALTER TABLE sandbox_service_assignments ALTER COLUMN next_reconcile_at SET NOT NULL;
"#).await?;
        Ok(())
    }
}
