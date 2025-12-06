//! Migration to alter visitor custom_data column from TEXT to JSONB
//!
//! This enables proper JSON querying and indexing capabilities for visitor custom data.

use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        // Convert TEXT to JSONB using PostgreSQL CAST
        // Existing TEXT data will be converted to JSONB (must be valid JSON)
        let db = manager.get_connection();
        db.execute_unprepared(
            r#"
            ALTER TABLE visitor
            ALTER COLUMN custom_data TYPE JSONB
            USING custom_data::JSONB
            "#,
        )
        .await?;

        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        // Convert back to TEXT
        let db = manager.get_connection();
        db.execute_unprepared(
            r#"
            ALTER TABLE visitor
            ALTER COLUMN custom_data TYPE TEXT
            USING custom_data::TEXT
            "#,
        )
        .await?;

        Ok(())
    }
}
