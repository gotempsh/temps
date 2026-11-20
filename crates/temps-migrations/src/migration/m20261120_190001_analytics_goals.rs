// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Conversion goals (ADR 109 D2): `analytics_goals`.
//!
//! A goal is a named, per-project rule that says which events count as a
//! conversion: either an event (name plus optional filters, the same filter
//! vocabulary funnel steps use) or a page view whose path matches a pattern.
//! A goal may name a numeric event property and a currency as its revenue
//! value. The console calls these "Conversions"; the table keeps the name the
//! ADR fixes so it does not collide with the AI activity report's "goals".
//!
//! Nothing on the ingest path reads this table: goals are matched at query
//! time against `events`, so adding, editing or deleting one never touches
//! stored events and applies retroactively to the whole history.

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
CREATE TABLE IF NOT EXISTS analytics_goals (
    id SERIAL PRIMARY KEY,
    project_id INTEGER NOT NULL REFERENCES projects(id) ON DELETE CASCADE,
    name VARCHAR(100) NOT NULL,
    kind VARCHAR(16) NOT NULL,
    event_name VARCHAR(255),
    event_filter JSONB NOT NULL DEFAULT '[]'::jsonb,
    page_path VARCHAR(512),
    revenue_property VARCHAR(64),
    revenue_currency VARCHAR(3),
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    CONSTRAINT ck_analytics_goals_kind CHECK (
        (kind = 'event' AND event_name IS NOT NULL AND page_path IS NULL)
        OR (kind = 'page' AND page_path IS NOT NULL AND event_name IS NULL
            AND revenue_property IS NULL)
    ),
    CONSTRAINT ck_analytics_goals_revenue CHECK (
        (revenue_property IS NULL) = (revenue_currency IS NULL)
    )
);

CREATE UNIQUE INDEX IF NOT EXISTS uq_analytics_goals_project_name
    ON analytics_goals (project_id, lower(name));
"#,
            )
            .await?;
        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .get_connection()
            .execute_unprepared("DROP TABLE IF EXISTS analytics_goals;")
            .await?;
        Ok(())
    }
}
