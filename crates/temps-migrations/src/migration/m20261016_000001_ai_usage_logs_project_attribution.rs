// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Attribute AI gateway usage to the project and deployment token that made it.
//!
//! Deployment tokens carry no user, so their usage rows were stored with a NULL
//! `user_id` and could not be tied back to a project. These nullable columns
//! are populated from the authenticated deployment token. They are deliberately
//! not foreign keys: usage is an append-only ledger that must survive a project
//! or token being deleted. Rows written before this migration stay NULL.
//!
//! The partial indexes serve per-project aggregation (spend caps, cost reports)
//! and only cover rows that are attributed, so they stay small on instances
//! where most traffic comes from human users. The project index also carries
//! the cost and BYOK columns (`INCLUDE`), so the monthly-budget SUM is answered
//! from the index alone instead of fetching every row the project wrote.

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
ALTER TABLE ai_usage_logs ADD COLUMN IF NOT EXISTS project_id INTEGER;
ALTER TABLE ai_usage_logs ADD COLUMN IF NOT EXISTS token_id INTEGER;

CREATE INDEX IF NOT EXISTS idx_ai_usage_logs_project_timestamp
    ON ai_usage_logs (project_id, timestamp DESC)
    INCLUDE (estimated_cost_microcents, is_byok)
    WHERE project_id IS NOT NULL;
CREATE INDEX IF NOT EXISTS idx_ai_usage_logs_token_timestamp
    ON ai_usage_logs (token_id, timestamp DESC)
    WHERE token_id IS NOT NULL;
"#,
            )
            .await?;
        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .get_connection()
            .execute_unprepared(
                r#"
DROP INDEX IF EXISTS idx_ai_usage_logs_token_timestamp;
DROP INDEX IF EXISTS idx_ai_usage_logs_project_timestamp;
ALTER TABLE ai_usage_logs DROP COLUMN IF EXISTS token_id;
ALTER TABLE ai_usage_logs DROP COLUMN IF EXISTS project_id;
"#,
            )
            .await?;
        Ok(())
    }
}
