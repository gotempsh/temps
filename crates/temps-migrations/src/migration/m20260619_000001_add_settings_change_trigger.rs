use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        let db = manager.get_connection();

        // Emit a NOTIFY on the `settings_change` channel whenever the singleton
        // settings row (id = 1) is written. The `ConfigService` per-process
        // settings cache LISTENs on this channel and invalidates immediately so
        // an out-of-process writer (e.g. the console process in the ADR-017
        // split topology) is picked up at once instead of waiting out the 5s
        // cache TTL. Fires on BOTH INSERT (first-run row creation) and UPDATE
        // (every subsequent change) because `update_settings` inserts on first
        // run and updates thereafter.
        db.execute_unprepared(
            r#"
                CREATE OR REPLACE FUNCTION notify_settings_change()
                RETURNS TRIGGER AS $$
                BEGIN
                    PERFORM pg_notify('settings_change', json_build_object(
                        'action', TG_OP,
                        'ts', CURRENT_TIMESTAMP
                    )::text);
                    RETURN COALESCE(NEW, OLD);
                END;
                $$ LANGUAGE plpgsql;
                "#,
        )
        .await?;

        db.execute_unprepared(
            r#"
                CREATE TRIGGER settings_change_trigger
                AFTER INSERT OR UPDATE ON settings
                FOR EACH ROW
                EXECUTE FUNCTION notify_settings_change();
                "#,
        )
        .await?;

        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        let db = manager.get_connection();

        db.execute_unprepared(
            r#"
                DROP TRIGGER IF EXISTS settings_change_trigger ON settings;
                DROP FUNCTION IF EXISTS notify_settings_change();
                "#,
        )
        .await?;

        Ok(())
    }
}
