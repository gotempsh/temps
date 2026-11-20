// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0
use sea_orm_migration::prelude::*;
#[derive(DeriveMigrationName)]
pub struct Migration;
#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        // A constant default avoids rewriting historical log chunks. Existing
        // records remain explicitly untyped; ingestion supplies types for new
        // records from either native OTLP or a protocol adapter.
        manager.get_connection().execute_unprepared(r#"
            ALTER TABLE otel_log_events ADD COLUMN attribute_types JSONB NOT NULL DEFAULT '{}';
            CREATE FUNCTION temps_log_numeric(value TEXT) RETURNS NUMERIC
                LANGUAGE plpgsql IMMUTABLE STRICT PARALLEL SAFE AS $$
            BEGIN
                IF octet_length(value)>16384 OR value !~ '^[+-]?([0-9]+(\.[0-9]*)?|\.[0-9]+)([eE][+-]?[0-9]+)?$' THEN
                    RETURN NULL;
                END IF;
                RETURN value::numeric;
            EXCEPTION WHEN data_exception THEN
                RETURN NULL;
            END $$;
        "#).await?;
        Ok(())
    }
    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager.get_connection().execute_unprepared("DROP FUNCTION temps_log_numeric(TEXT); ALTER TABLE otel_log_events DROP COLUMN attribute_types").await?;
        Ok(())
    }
}
