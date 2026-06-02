use sea_orm_migration::prelude::*;

/// Creates `service_metrics_status` — a tiny one-row-per-source table tracking
/// the last time metrics were received for each (source_kind, source_id).
///
/// This is a plain table (NOT a hypertable). It is upserted on every metrics
/// write_batch so the UI can show "last received at …" with a single O(1)
/// primary-key lookup instead of an expensive `MAX(time)` scan over the
/// `service_metrics` hypertable chunks.
///
/// Idempotent: uses `CREATE TABLE IF NOT EXISTS`.
#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        let db = manager.get_connection();

        db.execute_unprepared(
            r#"
CREATE TABLE IF NOT EXISTS service_metrics_status (
    source_kind      TEXT        NOT NULL,
    source_id        INT         NOT NULL,
    last_received_at TIMESTAMPTZ NOT NULL,
    PRIMARY KEY (source_kind, source_id)
);
"#,
        )
        .await?;

        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        let db = manager.get_connection();
        db.execute_unprepared("DROP TABLE IF EXISTS service_metrics_status;")
            .await?;
        Ok(())
    }
}
