use sea_orm::DatabaseBackend;
use sea_orm_migration::prelude::*;

const DEFAULT_COMPRESSION_AFTER_HOURS: u32 = 24;
const PREVIOUS_COMPRESSION_AFTER_HOURS: u32 = 7 * 24;

#[derive(DeriveMigrationName)]
pub struct Migration;

fn replace_policy_sql(table: &str, after_hours: u32) -> String {
    format!(
        "SELECT remove_compression_policy('{table}', if_exists => TRUE); \
         SELECT add_compression_policy(\
             '{table}', \
             compress_after => make_interval(hours => {after_hours}), \
             if_not_exists => TRUE\
         )"
    )
}

async fn replace_policy(
    manager: &SchemaManager<'_>,
    table: &str,
    after_hours: u32,
) -> Result<(), DbErr> {
    manager
        .get_connection()
        .execute_unprepared(&replace_policy_sql(table, after_hours))
        .await?;
    Ok(())
}

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        if manager.get_database_backend() != DatabaseBackend::Postgres {
            return Ok(());
        }

        // Proxy logs and spans are append-only. Keeping seven days of chunks
        // uncompressed wastes disk and cache without making writes safer; one
        // day still leaves a full chunk for normal late arrivals.
        replace_policy(manager, "proxy_logs", DEFAULT_COMPRESSION_AFTER_HOURS).await?;
        replace_policy(manager, "otel_spans", DEFAULT_COMPRESSION_AFTER_HOURS).await?;
        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        if manager.get_database_backend() != DatabaseBackend::Postgres {
            return Ok(());
        }

        replace_policy(manager, "otel_spans", PREVIOUS_COMPRESSION_AFTER_HOURS).await?;
        replace_policy(manager, "proxy_logs", PREVIOUS_COMPRESSION_AFTER_HOURS).await?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn policy_sql_uses_24_hour_interval() {
        let sql = replace_policy_sql("otel_spans", DEFAULT_COMPRESSION_AFTER_HOURS);
        assert!(sql.contains("remove_compression_policy('otel_spans', if_exists => TRUE)"));
        assert!(sql.contains("make_interval(hours => 24)"));
        assert!(sql.contains("if_not_exists => TRUE"));
    }
}
