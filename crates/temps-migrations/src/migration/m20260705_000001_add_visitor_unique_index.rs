use sea_orm_migration::prelude::*;

/// Migration: add a UNIQUE index on visitor(visitor_id, project_id).
///
/// The stateless cookie codec (WS1) now resolves visitor UUIDs directly via
/// ON CONFLICT … DO UPDATE upserts, so duplicate (visitor_id, project_id) rows
/// must be impossible. The migration:
///
///   1. Deduplicates any existing duplicate pairs: for each (visitor_id,
///      project_id) group, retain the row with the smallest `id` (the
///      canonical row) and re-point FK references in ALL dependent tables to
///      the canonical row before deleting the duplicates. The eight tables
///      whose `visitor_id` FK is re-pointed are:
///        - `proxy_logs`             (ON DELETE SetNull)
///        - `request_sessions`       (ON DELETE SetNull)
///        - `session_replay_sessions` (ON DELETE Cascade — critical: without
///          this repoint the duplicate-visitor DELETE would CASCADE-DELETE all
///          associated session replay recordings, causing permanent data loss)
///        - `performance_metrics`    (ON DELETE SetNull)
///        - `request_logs`           (ON DELETE SetNull)
///        - `events`                 (ON DELETE SetNull)
///        - `error_groups`           (ON DELETE SetNull)
///        - `error_events`           (ON DELETE SetNull)
///   2. Creates `UNIQUE INDEX IF NOT EXISTS` so the upsert path is safe.
///      (CONCURRENTLY cannot run inside a transaction block; on large
///      production tables run this step manually outside a transaction after
///      the migration if blocking reads is a concern.)
///
/// Down removes the index (does NOT restore deleted rows).
pub struct Migration;

impl MigrationName for Migration {
    fn name(&self) -> &str {
        "m20260705_000001_add_visitor_unique_index"
    }
}

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        // Step 1: Re-point FK references from duplicate visitor rows to the
        // canonical (lowest-id) row for the same (visitor_id, project_id).
        // NULL FK refs first so we don't violate the FK constraint on delete.
        manager
            .get_connection()
            .execute_unprepared(
                r#"
                -- Re-point proxy_logs.visitor_id to the canonical visitor row
                UPDATE proxy_logs pl
                SET visitor_id = canonical.id
                FROM (
                    SELECT visitor_id, project_id, MIN(id) AS id
                    FROM visitor
                    GROUP BY visitor_id, project_id
                    HAVING COUNT(*) > 1
                ) canonical
                JOIN visitor dup
                    ON dup.visitor_id = canonical.visitor_id
                   AND dup.project_id = canonical.project_id
                   AND dup.id <> canonical.id
                WHERE pl.visitor_id = dup.id;
                "#,
            )
            .await?;

        manager
            .get_connection()
            .execute_unprepared(
                r#"
                -- Re-point request_sessions.visitor_id to the canonical visitor row
                UPDATE request_sessions rs
                SET visitor_id = canonical.id
                FROM (
                    SELECT visitor_id, project_id, MIN(id) AS id
                    FROM visitor
                    GROUP BY visitor_id, project_id
                    HAVING COUNT(*) > 1
                ) canonical
                JOIN visitor dup
                    ON dup.visitor_id = canonical.visitor_id
                   AND dup.project_id = canonical.project_id
                   AND dup.id <> canonical.id
                WHERE rs.visitor_id = dup.id;
                "#,
            )
            .await?;

        manager
            .get_connection()
            .execute_unprepared(
                r#"
                -- Re-point session_replay_sessions.visitor_id to the canonical visitor row
                UPDATE session_replay_sessions srs
                SET visitor_id = canonical.id
                FROM (
                    SELECT visitor_id, project_id, MIN(id) AS id
                    FROM visitor
                    GROUP BY visitor_id, project_id
                    HAVING COUNT(*) > 1
                ) canonical
                JOIN visitor dup
                    ON dup.visitor_id = canonical.visitor_id
                   AND dup.project_id = canonical.project_id
                   AND dup.id <> canonical.id
                WHERE srs.visitor_id = dup.id;
                "#,
            )
            .await?;

        manager
            .get_connection()
            .execute_unprepared(
                r#"
                -- Re-point performance_metrics.visitor_id to the canonical visitor row
                UPDATE performance_metrics pm
                SET visitor_id = canonical.id
                FROM (
                    SELECT visitor_id, project_id, MIN(id) AS id
                    FROM visitor
                    GROUP BY visitor_id, project_id
                    HAVING COUNT(*) > 1
                ) canonical
                JOIN visitor dup
                    ON dup.visitor_id = canonical.visitor_id
                   AND dup.project_id = canonical.project_id
                   AND dup.id <> canonical.id
                WHERE pm.visitor_id = dup.id;
                "#,
            )
            .await?;

        manager
            .get_connection()
            .execute_unprepared(
                r#"
                -- Re-point request_logs.visitor_id to the canonical visitor row
                UPDATE request_logs rl
                SET visitor_id = canonical.id
                FROM (
                    SELECT visitor_id, project_id, MIN(id) AS id
                    FROM visitor
                    GROUP BY visitor_id, project_id
                    HAVING COUNT(*) > 1
                ) canonical
                JOIN visitor dup
                    ON dup.visitor_id = canonical.visitor_id
                   AND dup.project_id = canonical.project_id
                   AND dup.id <> canonical.id
                WHERE rl.visitor_id = dup.id;
                "#,
            )
            .await?;

        manager
            .get_connection()
            .execute_unprepared(
                r#"
                -- Re-point events.visitor_id to the canonical visitor row
                UPDATE events ev
                SET visitor_id = canonical.id
                FROM (
                    SELECT visitor_id, project_id, MIN(id) AS id
                    FROM visitor
                    GROUP BY visitor_id, project_id
                    HAVING COUNT(*) > 1
                ) canonical
                JOIN visitor dup
                    ON dup.visitor_id = canonical.visitor_id
                   AND dup.project_id = canonical.project_id
                   AND dup.id <> canonical.id
                WHERE ev.visitor_id = dup.id;
                "#,
            )
            .await?;

        manager
            .get_connection()
            .execute_unprepared(
                r#"
                -- Re-point error_groups.visitor_id to the canonical visitor row
                UPDATE error_groups eg
                SET visitor_id = canonical.id
                FROM (
                    SELECT visitor_id, project_id, MIN(id) AS id
                    FROM visitor
                    GROUP BY visitor_id, project_id
                    HAVING COUNT(*) > 1
                ) canonical
                JOIN visitor dup
                    ON dup.visitor_id = canonical.visitor_id
                   AND dup.project_id = canonical.project_id
                   AND dup.id <> canonical.id
                WHERE eg.visitor_id = dup.id;
                "#,
            )
            .await?;

        manager
            .get_connection()
            .execute_unprepared(
                r#"
                -- Re-point error_events.visitor_id to the canonical visitor row
                UPDATE error_events ee
                SET visitor_id = canonical.id
                FROM (
                    SELECT visitor_id, project_id, MIN(id) AS id
                    FROM visitor
                    GROUP BY visitor_id, project_id
                    HAVING COUNT(*) > 1
                ) canonical
                JOIN visitor dup
                    ON dup.visitor_id = canonical.visitor_id
                   AND dup.project_id = canonical.project_id
                   AND dup.id <> canonical.id
                WHERE ee.visitor_id = dup.id;
                "#,
            )
            .await?;

        // Step 2: Delete the duplicate rows (keeping the one with MIN(id))
        manager
            .get_connection()
            .execute_unprepared(
                r#"
                DELETE FROM visitor
                WHERE id IN (
                    SELECT dup.id
                    FROM visitor dup
                    JOIN (
                        SELECT visitor_id, project_id, MIN(id) AS min_id
                        FROM visitor
                        GROUP BY visitor_id, project_id
                        HAVING COUNT(*) > 1
                    ) canonical
                        ON dup.visitor_id = canonical.visitor_id
                       AND dup.project_id = canonical.project_id
                       AND dup.id <> canonical.min_id
                );
                "#,
            )
            .await?;

        // Step 3: Create the unique index.
        // Note: CONCURRENTLY cannot be used inside a transaction block (which is
        // how sea-orm runs migrations). On large production tables consider
        // running this manually outside a transaction after the migration runs.
        manager
            .get_connection()
            .execute_unprepared(
                r#"
                CREATE UNIQUE INDEX IF NOT EXISTS
                    visitor_visitor_id_project_id_key
                ON visitor (visitor_id, project_id);
                "#,
            )
            .await?;

        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .get_connection()
            .execute_unprepared("DROP INDEX IF EXISTS visitor_visitor_id_project_id_key;")
            .await?;
        Ok(())
    }
}
