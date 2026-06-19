use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        let db = manager.get_connection();
        db.execute_unprepared(
            r#"
DO $$
BEGIN
    IF NOT EXISTS (
        SELECT 1 FROM information_schema.columns
        WHERE table_schema = current_schema()
          AND table_name = 'api_keys' AND column_name = 'service_id'
    ) THEN
        ALTER TABLE api_keys
            ADD COLUMN service_id INT REFERENCES external_services(id) ON DELETE CASCADE;
    END IF;
END
$$;
"#,
        )
        .await?;
        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        let db = manager.get_connection();
        db.execute_unprepared(
            r#"
DO $$
BEGIN
    IF EXISTS (
        SELECT 1 FROM information_schema.columns
        WHERE table_schema = current_schema()
          AND table_name = 'api_keys' AND column_name = 'service_id'
    ) THEN
        ALTER TABLE api_keys DROP COLUMN service_id;
    END IF;
END
$$;
"#,
        )
        .await?;
        Ok(())
    }
}
