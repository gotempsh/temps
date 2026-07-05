use sea_orm_migration::prelude::*;

/// Migration: add a UNIQUE index on visitor(visitor_id, project_id).
///
/// The stateless cookie codec (WS1) now resolves visitor UUIDs directly via
/// ON CONFLICT … DO UPDATE upserts, so duplicate (visitor_id, project_id) rows
/// must be impossible. The migration:
///
///   1. Deduplicates any existing duplicate pairs: for each (visitor_id,
///      project_id) group, retain the row with the smallest `id` and NULL out
///      FK references in dependent tables (`proxy_logs.visitor_id`,
///      `request_sessions.visitor_id`) that point to the rows that will be
///      deleted, then delete those rows.
///   2. Creates `UNIQUE INDEX CONCURRENTLY IF NOT EXISTS` so this works on
///      production without blocking reads.
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
