// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        // These indexes target the slow analytics queries that use correlated subqueries
        // and LATERAL joins on the events hypertable and visitor table.
        //
        // Key query patterns addressed:
        // 1. get_visitors: LEFT JOIN LATERAL on events WHERE visitor_id = v.id ORDER BY timestamp DESC LIMIT 1
        // 2. get_page_path_visitors / get_page_paths / get_page_path_detail: session-based time_on_page lookups
        // 3. get_visitor_journey: events WHERE visitor_id = $1 AND project_id = $2
        // 4. Visitor listing by project_id + last_seen

        let db = manager.get_connection();

        // Index 1: events(visitor_id, timestamp DESC)
        // Supports: get_visitors LATERAL join, get_visitor_journey session lookup
        // Without this index, every LATERAL subquery does a full hypertable scan.
        db.execute_unprepared(
            r#"CREATE INDEX IF NOT EXISTS idx_events_visitor_timestamp
               ON events (visitor_id, timestamp DESC)"#,
        )
        .await?;

        // Index 2: events(project_id, event_type, page_path, timestamp DESC)
        // Supports: get_page_paths, get_page_path_visitors, get_page_path_detail
        // These queries filter by project_id + event_type='page_view' + page_path + timestamp range
        db.execute_unprepared(
            r#"CREATE INDEX IF NOT EXISTS idx_events_project_type_path_time
               ON events (project_id, event_type, page_path, timestamp DESC)"#,
        )
        .await?;

        // Index 3: events(session_id, event_type, page_path, timestamp)
        // Supports: The LEAD() window function replacement for time_on_page computation.
        // The window function partitions by session_id and orders by timestamp,
        // needing fast access to all events in a session ordered by time.
        db.execute_unprepared(
            r#"CREATE INDEX IF NOT EXISTS idx_events_session_type_path_time
               ON events (session_id, event_type, page_path, timestamp)"#,
        )
        .await?;

        // Index 4: visitor(project_id, last_seen DESC)
        // Supports: get_visitors listing with ORDER BY last_seen DESC
        // The visitor table had NO indexes beyond the primary key.
        db.execute_unprepared(
            r#"CREATE INDEX IF NOT EXISTS idx_visitor_project_last_seen
               ON visitor (project_id, last_seen DESC)"#,
        )
        .await?;

        // Index 5: visitor(visitor_id)
        // Supports: UUID-based visitor lookups (e.g., from cookie visitor_id to DB record)
        db.execute_unprepared(
            r#"CREATE INDEX IF NOT EXISTS idx_visitor_visitor_id
               ON visitor (visitor_id)"#,
        )
        .await?;

        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        let db = manager.get_connection();

        db.execute_unprepared("DROP INDEX IF EXISTS idx_events_visitor_timestamp")
            .await?;
        db.execute_unprepared("DROP INDEX IF EXISTS idx_events_project_type_path_time")
            .await?;
        db.execute_unprepared("DROP INDEX IF EXISTS idx_events_session_type_path_time")
            .await?;
        db.execute_unprepared("DROP INDEX IF EXISTS idx_visitor_project_last_seen")
            .await?;
        db.execute_unprepared("DROP INDEX IF EXISTS idx_visitor_visitor_id")
            .await?;

        Ok(())
    }
}
