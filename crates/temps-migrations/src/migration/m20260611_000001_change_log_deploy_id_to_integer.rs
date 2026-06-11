//! Change `deploy_id` from `uuid` to `integer` on the log aggregator tables.
//!
//! A Temps deployment is identified by `deployments.id` (an `i32` auto-increment
//! PK). The container label `sh.temps.deploy_id` is set to that integer, so the
//! collector should have been storing an `i32` all along. The column was
//! mistyped as `uuid`, which meant `Uuid::parse_str("171")` always failed and
//! every chunk stored `deploy_id = NULL`.
//!
//! Because the column is 100% NULL, retyping it to `integer` is lossless — we
//! drop the deploy_id index, `ALTER COLUMN ... TYPE integer USING NULL`, and
//! recreate the index under the same name.
//!
//! `log_chunks` is created by the log aggregator migration and always exists.
//! `log_events` has no migration of its own (the entity is retained but the
//! table is no longer provisioned), so every statement that touches it is
//! guarded with `to_regclass` / `IF EXISTS` and runs only when the table is
//! actually present.

use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        let db = manager.get_connection();

        // ── log_chunks (always present) ─────────────────────────────────
        db.execute_unprepared(
            r#"
            DROP INDEX IF EXISTS idx_log_chunks_deploy_id;
            ALTER TABLE log_chunks
                ALTER COLUMN deploy_id TYPE integer USING NULL;
            CREATE INDEX idx_log_chunks_deploy_id
                ON log_chunks (deploy_id);
            "#,
        )
        .await?;

        // ── log_events (only if the table exists) ───────────────────────
        db.execute_unprepared(
            r#"
            DO $$
            BEGIN
                IF to_regclass('public.log_events') IS NOT NULL THEN
                    DROP INDEX IF EXISTS idx_log_events_deploy_id;
                    ALTER TABLE log_events
                        ALTER COLUMN deploy_id TYPE integer USING NULL;
                    CREATE INDEX idx_log_events_deploy_id
                        ON log_events (deploy_id);
                END IF;
            END $$;
            "#,
        )
        .await?;

        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        let db = manager.get_connection();

        // ── log_chunks (always present) ─────────────────────────────────
        db.execute_unprepared(
            r#"
            DROP INDEX IF EXISTS idx_log_chunks_deploy_id;
            ALTER TABLE log_chunks
                ALTER COLUMN deploy_id TYPE uuid USING NULL;
            CREATE INDEX idx_log_chunks_deploy_id
                ON log_chunks (deploy_id);
            "#,
        )
        .await?;

        // ── log_events (only if the table exists) ───────────────────────
        db.execute_unprepared(
            r#"
            DO $$
            BEGIN
                IF to_regclass('public.log_events') IS NOT NULL THEN
                    DROP INDEX IF EXISTS idx_log_events_deploy_id;
                    ALTER TABLE log_events
                        ALTER COLUMN deploy_id TYPE uuid USING NULL;
                    CREATE INDEX idx_log_events_deploy_id
                        ON log_events (deploy_id);
                END IF;
            END $$;
            "#,
        )
        .await?;

        Ok(())
    }
}
