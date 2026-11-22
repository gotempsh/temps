// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

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
            CREATE TABLE telemetry_client_outcome_receipts (
                id BIGSERIAL PRIMARY KEY,
                project_id INTEGER NOT NULL REFERENCES projects(id) ON DELETE CASCADE,
                environment_id INTEGER REFERENCES environments(id) ON DELETE CASCADE,
                deployment_id INTEGER REFERENCES deployments(id) ON DELETE CASCADE,
                source_key VARCHAR(64) NOT NULL CHECK(source_key ~ '^[0-9a-f]{64}$'),
                expires_at TIMESTAMPTZ NOT NULL,
                UNIQUE NULLS NOT DISTINCT(project_id,environment_id,deployment_id,source_key)
            );
            CREATE INDEX telemetry_client_outcome_receipts_retention
                ON telemetry_client_outcome_receipts(expires_at);
            CREATE TABLE telemetry_client_outcome_receipt_budgets (
                id BIGSERIAL PRIMARY KEY,
                project_id INTEGER REFERENCES projects(id) ON DELETE CASCADE,
                bucket TIMESTAMPTZ NOT NULL,
                used INTEGER NOT NULL,
                CONSTRAINT client_outcome_receipt_capacity CHECK (
                    used>=0 AND used<=CASE WHEN project_id IS NULL THEN 131072 ELSE 32768 END
                ),
                UNIQUE NULLS NOT DISTINCT(project_id,bucket)
            );
            CREATE INDEX telemetry_client_outcome_receipt_budgets_retention
                ON telemetry_client_outcome_receipt_budgets(bucket);
        "#,
            )
            .await?;
        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .get_connection()
            .execute_unprepared("DROP TABLE telemetry_client_outcome_receipt_budgets; DROP TABLE telemetry_client_outcome_receipts")
            .await?;
        Ok(())
    }
}
