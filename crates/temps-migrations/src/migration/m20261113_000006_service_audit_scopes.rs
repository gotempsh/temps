// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Snapshot vault membership per service write, rather than reassigning events
//! when membership changes. No backfill can reconstruct old event-time scope;
//! legacy audit events retain their existing historical-membership fallback.
//! No foreign keys: audit scope must survive deletion of either resource.
use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager.get_connection().execute_unprepared("CREATE TABLE IF NOT EXISTS access_service_audit_scopes(scope_key text NOT NULL, vault_id text NOT NULL, PRIMARY KEY(scope_key,vault_id))").await?;
        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager.get_connection().execute_unprepared("DO $$ BEGIN IF EXISTS(SELECT 1 FROM access_service_audit_scopes) THEN RAISE EXCEPTION 'Cannot downgrade while access_service_audit_scopes contains event-time audit scope'; END IF; END $$; DROP TABLE IF EXISTS access_service_audit_scopes").await?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn audit_scopes_rollback_only_when_empty() {
        if !temps_test_utils::external_database_configured()
            && !temps_test_utils::docker_available().await
        {
            eprintln!("Skipping service audit scope migration: database unavailable");
            return;
        }
        let mut test = temps_test_utils::TestDatabase::new().await.unwrap();
        let db = test.connection_arc();
        let manager = SchemaManager::new(db.as_ref());
        Migration.up(&manager).await.unwrap();
        Migration.down(&manager).await.unwrap();
        Migration.up(&manager).await.unwrap();
        db.execute_unprepared(
            "INSERT INTO access_service_audit_scopes VALUES('svc_fixture:1','vault_fixture')",
        )
        .await
        .unwrap();
        assert!(Migration
            .down(&manager)
            .await
            .unwrap_err()
            .to_string()
            .contains("event-time audit scope"));
        test.cleanup().await;
    }
}
