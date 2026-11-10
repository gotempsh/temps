// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Durable, explicitly approved workload access. Existing sessions and grants
//! keep their original deadlines; no existing authorization is promoted.
use sea_orm_migration::prelude::*;
#[derive(DeriveMigrationName)]
pub struct Migration;
#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager.get_connection().execute_unprepared(r#"
CREATE TABLE IF NOT EXISTS sandbox_service_assignments (
 id text PRIMARY KEY,
 project_id integer NOT NULL REFERENCES projects(id) ON DELETE CASCADE,
 sandbox_id integer NOT NULL REFERENCES sandboxes(id) ON DELETE CASCADE,
 service_id text NOT NULL REFERENCES sandbox_access_services(id) ON DELETE CASCADE,
 service_version integer NOT NULL,
 requested_by integer REFERENCES users(id) ON DELETE SET NULL,
 approved_by integer REFERENCES users(id) ON DELETE SET NULL,
 approved_at timestamptz,
 expires_at timestamptz,
 revoked_at timestamptz,
 version integer NOT NULL DEFAULT 1,
 grant_id text,
 next_reconcile_at timestamptz NOT NULL DEFAULT now(),
 created_at timestamptz NOT NULL DEFAULT now(),
 updated_at timestamptz NOT NULL DEFAULT now(),
 UNIQUE (sandbox_id,service_id)
);
CREATE INDEX IF NOT EXISTS sandbox_service_assignments_due ON sandbox_service_assignments(next_reconcile_at,id) WHERE approved_at IS NOT NULL AND revoked_at IS NULL;
ALTER TABLE sandbox_service_grants ADD COLUMN IF NOT EXISTS assignment_id text REFERENCES sandbox_service_assignments(id) ON DELETE CASCADE;
CREATE INDEX IF NOT EXISTS sandbox_service_grants_assignment ON sandbox_service_grants(assignment_id) WHERE assignment_id IS NOT NULL;
"#).await?;
        Ok(())
    }
    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        // A rollback must not turn automatically maintained grants into legacy
        // independent approvals. Revoke them before removing their provenance.
        manager.get_connection().execute_unprepared(r#"
UPDATE sandbox_service_grants SET revoked_at=COALESCE(revoked_at,now()),version=version+1 WHERE assignment_id IS NOT NULL;
ALTER TABLE sandbox_service_grants DROP COLUMN IF EXISTS assignment_id;
DROP TABLE IF EXISTS sandbox_service_assignments;
"#).await?;
        Ok(())
    }
}
