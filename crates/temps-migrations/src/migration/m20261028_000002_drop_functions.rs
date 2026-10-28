// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Removes what the retired functions migration
//! (`m20261023_000020_functions`) created on a database that applied it
//! before the feature was dropped: the `functions` table, the workloads it
//! applied to the plane, and their ingress endpoints. A database that never
//! had it is left as it is.

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
DO $$
BEGIN
    IF to_regclass('functions') IS NOT NULL THEN
        DELETE FROM workloads
        WHERE id IN (SELECT workload_id FROM functions WHERE workload_id IS NOT NULL);
        DROP TABLE functions;
    END IF;
END
$$;
DELETE FROM ingress_endpoints WHERE owner_kind = 'function';
"#,
            )
            .await?;
        Ok(())
    }

    async fn down(&self, _manager: &SchemaManager) -> Result<(), DbErr> {
        // Nothing to restore: the feature no longer exists.
        Ok(())
    }
}
