//! Adds `'deployment'` to the `service_endpoints.owner_kind` CHECK constraint
//! so the route layer can publish per-deployment FQDNs (e.g.
//! `<env-slug>.<project-slug>.temps.local`) into the same registry that
//! already serves Tier 2 / Tier 3 records.
//!
//! For deployment-owned records, `owner_id = deployments.id`. Cleanup on
//! deployment teardown is symmetric to existing kinds: the registry calls
//! `delete_by_owner(Deployment, deployment_id)` from the same path that
//! already drops Tier 2 records when a service member is removed.

use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        // The original migration named the constraint
        // `service_endpoints_owner_kind_valid`. Drop both possible names
        // so this migration is robust against earlier dev iterations that
        // may have used `_check`.
        let stmts = [
            "ALTER TABLE service_endpoints DROP CONSTRAINT IF EXISTS service_endpoints_owner_kind_valid",
            "ALTER TABLE service_endpoints DROP CONSTRAINT IF EXISTS service_endpoints_owner_kind_check",
            "ALTER TABLE service_endpoints ADD CONSTRAINT service_endpoints_owner_kind_valid \
             CHECK (owner_kind IN ('service_member', 'service_role', 'node', 'static', 'deployment'))",
        ];
        let conn = manager.get_connection();
        for s in stmts {
            conn.execute_unprepared(s).await?;
        }
        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        let stmts = [
            "DELETE FROM service_endpoints WHERE owner_kind = 'deployment'",
            "ALTER TABLE service_endpoints DROP CONSTRAINT IF EXISTS service_endpoints_owner_kind_valid",
            "ALTER TABLE service_endpoints ADD CONSTRAINT service_endpoints_owner_kind_valid \
             CHECK (owner_kind IN ('service_member', 'service_role', 'node', 'static'))",
        ];
        let conn = manager.get_connection();
        for s in stmts {
            conn.execute_unprepared(s).await?;
        }
        Ok(())
    }
}
