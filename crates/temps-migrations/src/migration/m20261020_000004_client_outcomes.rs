// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0
use sea_orm_migration::prelude::*;
#[derive(DeriveMigrationName)]
pub struct Migration;
#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager.get_connection().execute_unprepared(r#"
            CREATE TABLE telemetry_client_outcomes (
                id BIGSERIAL PRIMARY KEY,
                project_id INTEGER NOT NULL REFERENCES projects(id) ON DELETE CASCADE,
                environment_id INTEGER REFERENCES environments(id) ON DELETE CASCADE,
                deployment_id INTEGER REFERENCES deployments(id) ON DELETE CASCADE,
                bucket TIMESTAMPTZ NOT NULL,
                reason VARCHAR(32) NOT NULL,
                category VARCHAR(32) NOT NULL,
                unit VARCHAR(16) NOT NULL CHECK(unit IN ('items','bytes','milliseconds','unknown')),
                quantity BIGINT NOT NULL CHECK(quantity>0),
                first_reported_at TIMESTAMPTZ NOT NULL,
                last_reported_at TIMESTAMPTZ NOT NULL,
                last_received_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
                UNIQUE NULLS NOT DISTINCT(project_id,environment_id,deployment_id,bucket,reason,category)
            );
            CREATE INDEX telemetry_client_outcomes_recent ON telemetry_client_outcomes(project_id,bucket DESC,id DESC);
            CREATE INDEX telemetry_client_outcomes_retention ON telemetry_client_outcomes(bucket);
        "#).await?;
        Ok(())
    }
    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .get_connection()
            .execute_unprepared("DROP TABLE telemetry_client_outcomes")
            .await?;
        Ok(())
    }
}
