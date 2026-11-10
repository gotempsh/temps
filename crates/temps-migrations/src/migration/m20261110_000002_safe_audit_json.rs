// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Safely read historical text audit payloads on PostgreSQL 14 and newer.
//! No rows are rewritten; rollback removes the parser and its maintenance indexes.
use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager.get_connection().execute_unprepared(r#"
CREATE OR REPLACE FUNCTION temps_audit_json(payload text) RETURNS jsonb
LANGUAGE plpgsql IMMUTABLE STRICT AS $$
BEGIN
 RETURN payload::jsonb;
EXCEPTION WHEN data_exception THEN
 RETURN NULL;
END;
$$;
"#).await?;
        Ok(())
    }
    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager.get_connection().execute_unprepared("DROP INDEX IF EXISTS audit_logs_resource_id; DROP INDEX IF EXISTS audit_logs_vault_id; DROP INDEX IF EXISTS audit_logs_service_scope_key; DROP FUNCTION IF EXISTS temps_audit_json(text)").await?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use sea_orm::{DbBackend, Statement};
    #[tokio::test]
    async fn safe_audit_json_handles_legacy_payloads_and_rolls_back() {
        if !temps_test_utils::external_database_configured() && !temps_test_utils::docker_available().await {
            eprintln!("Skipping audit JSON migration: database unavailable");
            return;
        }
        let mut test = temps_test_utils::TestDatabase::new().await.unwrap();
        let db = test.connection_arc();
        let manager = SchemaManager::new(db.as_ref());
        Migration.up(&manager).await.unwrap();
        let row = db.query_one(Statement::from_string(DbBackend::Postgres, r#"SELECT
            temps_audit_json('{"resource_id":"vault_fixture"}')->>'resource_id' = 'vault_fixture'
            AND temps_audit_json('not JSON') IS NULL
            AND temps_audit_json('{"broken":}') IS NULL
            AND temps_audit_json(NULL) IS NULL
            AND temps_audit_json('[]') = '[]'::jsonb AS valid"#)).await.unwrap().unwrap();
        assert!(row.try_get::<bool>("", "valid").unwrap());
        // Maintenance builds these after migration commit; rollback must also
        // work after those indexes have been installed.
        db.execute_unprepared("CREATE TABLE audit_logs(data text); CREATE INDEX audit_logs_resource_id ON audit_logs ((temps_audit_json(data)->>'resource_id')); CREATE INDEX audit_logs_vault_id ON audit_logs ((temps_audit_json(data)->>'vault_id')); CREATE INDEX audit_logs_service_scope_key ON audit_logs ((temps_audit_json(data)->>'service_scope_key'))").await.unwrap();
        Migration.down(&manager).await.unwrap();
        let row = db.query_one(Statement::from_string(DbBackend::Postgres, "SELECT to_regprocedure('temps_audit_json(text)') IS NULL AS removed")).await.unwrap().unwrap();
        assert!(row.try_get::<bool>("", "removed").unwrap());
        test.cleanup().await;
    }
}
