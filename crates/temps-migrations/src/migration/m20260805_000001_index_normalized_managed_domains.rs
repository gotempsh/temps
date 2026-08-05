//! Indexes managed DNS zones by the canonical form used for authoritative
//! suffix lookup. Existing rows may contain mixed case, surrounding whitespace,
//! a wildcard prefix, or a trailing root dot, so canonicalization stays in the
//! index rather than requiring an unsafe data rewrite.

use sea_orm_migration::prelude::*;

const INDEX_NAME: &str = "idx_dns_managed_domains_normalized_domain";
const NORMALIZED_DOMAIN_SQL: &str =
    "LOWER(REGEXP_REPLACE(RTRIM(BTRIM(\"domain\"), '.'), '^((\\*\\.)+)', ''))";

#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        // SeaORM 1.1 wraps PostgreSQL migrations in a transaction, so
        // CREATE INDEX CONCURRENTLY is not legal here. Bound how long the
        // ordinary idempotent build may wait on a conflicting table lock.
        manager
            .get_connection()
            .execute_unprepared("SET LOCAL lock_timeout = '5s'")
            .await?;
        manager
            .get_connection()
            .execute_unprepared(&format!(
                "CREATE INDEX IF NOT EXISTS {INDEX_NAME} ON dns_managed_domains (({NORMALIZED_DOMAIN_SQL}))"
            ))
            .await?;
        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .get_connection()
            .execute_unprepared("SET LOCAL lock_timeout = '5s'")
            .await?;
        manager
            .get_connection()
            .execute_unprepared(&format!("DROP INDEX IF EXISTS {INDEX_NAME}"))
            .await?;
        Ok(())
    }
}
