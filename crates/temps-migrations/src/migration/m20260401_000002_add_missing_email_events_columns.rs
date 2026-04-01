use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        let db = manager.get_connection();

        // Add link_url and link_index if they don't exist
        // These were part of the original CREATE TABLE in m20260320 but may be missing
        // if the table was created by an earlier migration version.
        db.execute_unprepared(
            "ALTER TABLE email_events ADD COLUMN IF NOT EXISTS link_url TEXT NULL",
        )
        .await?;

        db.execute_unprepared(
            "ALTER TABLE email_events ADD COLUMN IF NOT EXISTS link_index INTEGER NULL",
        )
        .await?;

        Ok(())
    }

    async fn down(&self, _manager: &SchemaManager) -> Result<(), DbErr> {
        // Don't drop these columns on rollback — they may have been part of the original schema
        Ok(())
    }
}
