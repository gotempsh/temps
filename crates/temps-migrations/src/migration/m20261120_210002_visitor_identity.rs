// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Identify a visitor (ADR 109 D4).
//!
//! - `visitor.user_id`: the app's own id for the person, set by `identify`.
//!   `identity_source` says who set it (`browser`, a claim the page made, or
//!   `server`, the app's backend), `identified_at` when, and
//!   `trait_sources` which side set each `custom_data` key that `identify`
//!   wrote. All four are nullable without a default, so adding them is a
//!   catalog change that rewrites no rows of this large, hot table.
//! - The `identity_source` check is added `NOT VALID`: every existing row is
//!   NULL, and validating would scan the table under a lock that stops
//!   ingestion. New and updated rows are checked all the same.
//! - The `user_id` lookup index is not built here (a migration runs in a
//!   transaction, where only a blocking `CREATE INDEX` is legal); the
//!   post-migration step builds `idx_visitor_user_id` concurrently.
//! - `project_analytics_settings.accept_browser_identify`: whether `identify`
//!   from the browser is applied. `true` by default, also for projects
//!   without a settings row.

use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

pub(crate) const UP_SQL: &str = r#"
ALTER TABLE visitor
    ADD COLUMN IF NOT EXISTS user_id TEXT,
    ADD COLUMN IF NOT EXISTS identity_source TEXT,
    ADD COLUMN IF NOT EXISTS identified_at TIMESTAMPTZ,
    ADD COLUMN IF NOT EXISTS trait_sources JSONB;

ALTER TABLE visitor DROP CONSTRAINT IF EXISTS ck_visitor_identity_source;
ALTER TABLE visitor ADD CONSTRAINT ck_visitor_identity_source
    CHECK (identity_source IN ('browser', 'server')) NOT VALID;

ALTER TABLE project_analytics_settings
    ADD COLUMN IF NOT EXISTS accept_browser_identify BOOLEAN NOT NULL DEFAULT TRUE;
"#;

pub(crate) const DOWN_SQL: &str = r#"
DROP INDEX IF EXISTS idx_visitor_user_id;
ALTER TABLE project_analytics_settings DROP COLUMN IF EXISTS accept_browser_identify;
ALTER TABLE visitor DROP CONSTRAINT IF EXISTS ck_visitor_identity_source;
ALTER TABLE visitor
    DROP COLUMN IF EXISTS trait_sources,
    DROP COLUMN IF EXISTS identified_at,
    DROP COLUMN IF EXISTS identity_source,
    DROP COLUMN IF EXISTS user_id;
"#;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager.get_connection().execute_unprepared(UP_SQL).await?;
        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager.get_connection().execute_unprepared(DOWN_SQL).await?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Nothing here may rewrite or scan `visitor`: no default on its new
    /// columns, no validated constraint, no index build.
    #[test]
    fn the_visitor_changes_take_no_table_lock_for_long() {
        let visitor_part = UP_SQL
            .split("ALTER TABLE project_analytics_settings")
            .next()
            .unwrap_or_default();
        assert!(!visitor_part.contains("DEFAULT"), "{visitor_part}");
        assert!(!visitor_part.contains("NOT NULL"), "{visitor_part}");
        assert!(visitor_part.contains("NOT VALID"), "{visitor_part}");
        assert!(!UP_SQL.contains("CREATE INDEX"));
    }

    #[test]
    fn down_removes_everything_up_adds() {
        for column in [
            "user_id",
            "identity_source",
            "identified_at",
            "trait_sources",
            "accept_browser_identify",
        ] {
            assert!(UP_SQL.contains(column));
            assert!(DOWN_SQL.contains(&format!("DROP COLUMN IF EXISTS {column}")));
        }
        assert!(DOWN_SQL.contains("idx_visitor_user_id"));
    }
}
