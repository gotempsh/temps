// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0
use sea_orm_migration::prelude::*;
#[derive(DeriveMigrationName)]
pub struct Migration;
#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager.get_connection().execute_unprepared(r#"
            CREATE TABLE otel_ingest_receipts (
                id BIGSERIAL PRIMARY KEY,
                project_id INTEGER NOT NULL REFERENCES projects(id) ON DELETE CASCADE,
                signal TEXT NOT NULL CHECK(signal IN ('logs','spans','metrics')),
                source_key VARCHAR(64) NOT NULL CHECK(source_key ~ '^[0-9a-f]{64}$'),
                expires_at TIMESTAMPTZ NOT NULL,
                UNIQUE(project_id,signal,source_key)
            );
            CREATE INDEX otel_ingest_receipts_expiry ON otel_ingest_receipts(expires_at);
            CREATE TABLE otel_ingest_receipt_budgets (
                id BIGSERIAL PRIMARY KEY,
                project_id INTEGER REFERENCES projects(id) ON DELETE CASCADE,
                bucket TIMESTAMPTZ NOT NULL,
                used INTEGER NOT NULL,
                CONSTRAINT otel_ingest_receipt_capacity CHECK(used>=0 AND used<=CASE WHEN project_id IS NULL THEN 65536 ELSE 16384 END),
                UNIQUE NULLS NOT DISTINCT(project_id,bucket)
            );
            CREATE INDEX otel_ingest_receipt_budgets_expiry ON otel_ingest_receipt_budgets(bucket);
        "#).await?;
        Ok(())
    }
    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .get_connection()
            .execute_unprepared(
                "DROP TABLE otel_ingest_receipt_budgets; DROP TABLE otel_ingest_receipts",
            )
            .await?;
        Ok(())
    }
}
