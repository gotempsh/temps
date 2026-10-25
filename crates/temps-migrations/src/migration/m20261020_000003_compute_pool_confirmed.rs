// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! When an operator confirmed the cluster compute pool (ADR 056).
//!
//! The console asks for the pool to be confirmed once before it offers to add
//! the first worker, because the pool can no longer change in place after a
//! node uses it. Clusters that already have a node, or whose control plane
//! already holds a subnet, made that choice: they are backfilled as
//! confirmed. Joining from the CLI never checks it.

use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        let conn = manager.get_connection();
        conn.execute_unprepared(
            r#"
ALTER TABLE network_config
    ADD COLUMN IF NOT EXISTS compute_pool_confirmed_at TIMESTAMPTZ;
UPDATE network_config
   SET compute_pool_confirmed_at = NOW()
 WHERE compute_pool_confirmed_at IS NULL
   AND (control_plane_compute_cidr IS NOT NULL OR EXISTS (SELECT 1 FROM nodes));
"#,
        )
        .await?;
        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .get_connection()
            .execute_unprepared(
                "ALTER TABLE network_config DROP COLUMN IF EXISTS compute_pool_confirmed_at",
            )
            .await?;
        Ok(())
    }
}
