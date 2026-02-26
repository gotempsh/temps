use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        let db = manager.get_connection();

        // Enable compression on the proxy_logs hypertable
        db.execute_unprepared(
            "ALTER TABLE proxy_logs SET (
                timescaledb.compress,
                timescaledb.compress_segmentby = 'project_id',
                timescaledb.compress_orderby = 'timestamp DESC'
            )",
        )
        .await?;

        // Add compression policy: compress chunks older than 7 days
        db.execute_unprepared(
            "SELECT add_compression_policy('proxy_logs', INTERVAL '7 days', if_not_exists => TRUE)",
        )
        .await?;

        // Add retention policy: drop chunks older than 30 days
        db.execute_unprepared(
            "SELECT add_retention_policy('proxy_logs', drop_after => INTERVAL '30 days', if_not_exists => TRUE)",
        )
        .await?;

        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        let db = manager.get_connection();

        db.execute_unprepared("SELECT remove_retention_policy('proxy_logs', if_exists => TRUE)")
            .await?;

        db.execute_unprepared("SELECT remove_compression_policy('proxy_logs', if_exists => TRUE)")
            .await?;

        Ok(())
    }
}
