// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! In-app surveys (ADR 110 D1).
//!
//! One table. A survey is a project-scoped definition: an ordered list of
//! questions and the conditions under which a browser shows it. Answers are
//! not stored here: the browser SDK records them as ordinary analytics events
//! (`survey_shown`, `survey_dismissed`, `survey_sent`), so they need no new
//! store and appear in the existing breakdowns, goals and funnels.
//!
//! `questions` and `targeting` are JSONB validated by the service on every
//! write; the CHECK constraints guard the two scalar columns a bad write could
//! corrupt without a JSON parser.

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
CREATE TABLE IF NOT EXISTS surveys (
    id SERIAL PRIMARY KEY,
    project_id INTEGER NOT NULL REFERENCES projects(id) ON DELETE CASCADE,
    name VARCHAR(200) NOT NULL,
    description VARCHAR(1000),
    status VARCHAR(16) NOT NULL DEFAULT 'draft',
    questions JSONB NOT NULL DEFAULT '[]'::jsonb,
    targeting JSONB NOT NULL DEFAULT '{}'::jsonb,
    created_at TIMESTAMPTZ NOT NULL DEFAULT CURRENT_TIMESTAMP,
    updated_at TIMESTAMPTZ NOT NULL DEFAULT CURRENT_TIMESTAMP,
    CONSTRAINT ck_surveys_status
        CHECK (status IN ('draft', 'active', 'paused', 'completed'))
);

-- The public endpoint loads a project's active surveys; the console lists a
-- project's surveys newest first.
CREATE INDEX IF NOT EXISTS idx_surveys_project_status ON surveys (project_id, status);
CREATE INDEX IF NOT EXISTS idx_surveys_project_created ON surveys (project_id, created_at DESC);
"#,
            )
            .await?;
        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .get_connection()
            .execute_unprepared("DROP TABLE IF EXISTS surveys;")
            .await?;
        Ok(())
    }
}
