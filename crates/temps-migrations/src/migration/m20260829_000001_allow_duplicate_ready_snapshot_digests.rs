// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Allow multiple snapshot rows to reference one content-addressed artifact.
//!
//! Snapshot deduplication is implemented by keeping distinct, user-owned rows
//! that share `content_digest` and artifact paths. The original unique partial
//! index contradicted that model and rejected the second ready row.

use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        let connection = manager.get_connection();
        connection
            .execute_unprepared("DROP INDEX IF EXISTS idx_sandbox_snapshots_digest_ready")
            .await?;
        connection
            .execute_unprepared(
                "CREATE INDEX IF NOT EXISTS idx_sandbox_snapshots_digest_ready \
                 ON sandbox_snapshots (content_digest) WHERE status = 'ready'",
            )
            .await?;
        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        let connection = manager.get_connection();
        connection
            .execute_unprepared("DROP INDEX IF EXISTS idx_sandbox_snapshots_digest_ready")
            .await?;
        connection
            .execute_unprepared(
                r#"
DO $$
BEGIN
    IF EXISTS (
        SELECT 1
        FROM sandbox_snapshots
        WHERE status = 'ready'
        GROUP BY content_digest
        HAVING COUNT(*) > 1
    ) THEN
        RAISE EXCEPTION 'cannot restore unique snapshot digest index while ready rows share artifacts';
    END IF;
END
$$
"#,
            )
            .await?;
        connection
            .execute_unprepared(
                "CREATE UNIQUE INDEX IF NOT EXISTS idx_sandbox_snapshots_digest_ready \
                 ON sandbox_snapshots (content_digest) WHERE status = 'ready'",
            )
            .await?;
        Ok(())
    }
}
