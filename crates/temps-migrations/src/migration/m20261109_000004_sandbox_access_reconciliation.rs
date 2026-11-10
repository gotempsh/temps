// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Persist the next short-lease recheck, including across control-plane restarts.
//! Inactive authorities default to NULL; consent changes wake them transactionally.
//! Existing active incarnations are scheduled, never granted additional access.
//! Downgrade refuses active incarnations because it would stop lease maintenance.
use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager.get_connection().execute_unprepared(r#"
ALTER TABLE sandbox_access_authorities ADD COLUMN IF NOT EXISTS next_reconcile_at TIMESTAMPTZ;
CREATE INDEX IF NOT EXISTS sandbox_access_authority_due ON sandbox_access_authorities (next_reconcile_at,id)
    WHERE closed_at IS NULL AND next_reconcile_at IS NOT NULL;
CREATE INDEX IF NOT EXISTS sandbox_access_delivery_priority ON sandbox_access_delivery ((operation='retire') DESC,available_at,id)
    WHERE completed_at IS NULL;
CREATE INDEX IF NOT EXISTS sandbox_access_delivery_completed ON sandbox_access_delivery (completed_at,id)
    WHERE completed_at IS NOT NULL;
CREATE OR REPLACE FUNCTION sandbox_access_wake_authority() RETURNS trigger AS $$
BEGIN
    IF NEW.desired_revision IS DISTINCT FROM OLD.desired_revision THEN
        NEW.next_reconcile_at=LEAST(OLD.next_reconcile_at,now());
    END IF;
    RETURN NEW;
END;
$$ LANGUAGE plpgsql;
DROP TRIGGER IF EXISTS sandbox_access_authority_wakeup ON sandbox_access_authorities;
CREATE TRIGGER sandbox_access_authority_wakeup BEFORE UPDATE ON sandbox_access_authorities
    FOR EACH ROW EXECUTE FUNCTION sandbox_access_wake_authority();
UPDATE sandbox_access_authorities a SET next_reconcile_at=now()
    WHERE a.closed_at IS NULL AND a.next_reconcile_at IS NULL
      AND EXISTS (SELECT 1 FROM sandbox_access_runtimes r WHERE r.authority_id=a.id AND r.closed_at IS NULL);
"#).await?;
        Ok(())
    }
    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager.get_connection().execute_unprepared(r#"
DO $$ BEGIN
    IF to_regclass('sandbox_access_runtimes') IS NOT NULL THEN
        IF EXISTS (SELECT 1 FROM sandbox_access_runtimes WHERE authority_id IS NOT NULL AND closed_at IS NULL) THEN
            RAISE EXCEPTION 'Cannot remove service-access reconciliation while active incarnations exist';
        END IF;
    END IF;
END $$;
DROP TRIGGER IF EXISTS sandbox_access_authority_wakeup ON sandbox_access_authorities;
DROP FUNCTION IF EXISTS sandbox_access_wake_authority();
DROP INDEX IF EXISTS sandbox_access_authority_due;
DROP INDEX IF EXISTS sandbox_access_delivery_priority;
DROP INDEX IF EXISTS sandbox_access_delivery_completed;
ALTER TABLE IF EXISTS sandbox_access_authorities DROP COLUMN IF EXISTS next_reconcile_at;
"#).await?;
        Ok(())
    }
}
