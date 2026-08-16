//! Partial index on `visitor` scoped to non-crawler rows.
//!
//! `idx_visitor_project_last_seen (project_id, last_seen DESC)` (added in
//! `m20260214_000002_add_analytics_performance_indexes`) already anchors the
//! main visitor-listing queries, but every one of them also carries a
//! `v.is_crawler = false` predicate: unconditionally in `get_live_visitors`
//! (polled every 2s from the live-visitors page), and whenever a caller of
//! `get_visitors`/the facet queries passes `include_crawlers = false`. A plain
//! composite index still has to fetch each matching row before discarding the
//! crawler ones; a partial index excludes them from the index itself, so a
//! table with meaningful bot/scraper volume (the exact situation that
//! motivated this migration) doesn't pay to scan past them on every poll.
//!
//! This is additive to, not a replacement for, `idx_visitor_project_last_seen`:
//! queries that legitimately want crawlers included (`include_crawlers` unset
//! or `true`) still use the existing full index.

use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        let db = manager.get_connection();

        db.execute_unprepared(
            r#"CREATE INDEX IF NOT EXISTS idx_visitor_project_last_seen_non_crawler
               ON visitor (project_id, last_seen DESC)
               WHERE is_crawler = false"#,
        )
        .await?;

        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        let db = manager.get_connection();

        db.execute_unprepared("DROP INDEX IF EXISTS idx_visitor_project_last_seen_non_crawler")
            .await?;

        Ok(())
    }
}
