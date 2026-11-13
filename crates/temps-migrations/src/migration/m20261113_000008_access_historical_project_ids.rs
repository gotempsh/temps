// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Sandbox project IDs can intentionally outlive the project. Grant/runtime
//! provenance must accept the same historical IDs on subsequent approvals.
//! Consumer foreign keys remain authoritative; no history is rewritten.
use sea_orm_migration::prelude::*;
#[derive(DeriveMigrationName)]
pub struct Migration;
#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager.get_connection().execute_unprepared("ALTER TABLE sandbox_service_grants DROP CONSTRAINT IF EXISTS grant_project_provenance; ALTER TABLE sandbox_access_runtime_records DROP CONSTRAINT IF EXISTS runtime_project_provenance").await?;
        Ok(())
    }
    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager.get_connection().execute_unprepared("DO $$ BEGIN IF EXISTS(SELECT 1 FROM sandbox_service_grants) OR EXISTS(SELECT 1 FROM sandbox_access_runtime_records) THEN RAISE EXCEPTION 'Cannot restore project references while grants or runtime records retain historical project IDs'; END IF; END $$; ALTER TABLE sandbox_service_grants ADD CONSTRAINT grant_project_provenance FOREIGN KEY(project_id) REFERENCES projects(id) ON DELETE SET NULL; ALTER TABLE sandbox_access_runtime_records ADD CONSTRAINT runtime_project_provenance FOREIGN KEY(project_id) REFERENCES projects(id) ON DELETE SET NULL").await?;
        Ok(())
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[tokio::test]
    async fn new_approvals_accept_historical_project_ids_and_down_refuses_them() {
        if !temps_test_utils::external_database_configured() && !temps_test_utils::docker_available().await { return; }
        let mut test=temps_test_utils::TestDatabase::new().await.unwrap();
        let db=test.connection_arc();
        db.execute_unprepared("CREATE TABLE projects(id integer PRIMARY KEY); CREATE TABLE sandbox_service_grants(project_id integer CONSTRAINT grant_project_provenance REFERENCES projects(id) ON DELETE SET NULL); CREATE TABLE sandbox_access_runtime_records(project_id integer CONSTRAINT runtime_project_provenance REFERENCES projects(id) ON DELETE SET NULL)").await.unwrap();
        let manager=SchemaManager::new(db.as_ref());
        Migration.up(&manager).await.unwrap();
        db.execute_unprepared("INSERT INTO sandbox_service_grants VALUES(1); INSERT INTO sandbox_access_runtime_records VALUES(1)").await.unwrap();
        assert!(Migration.down(&manager).await.unwrap_err().to_string().contains("historical project IDs"));
        db.execute_unprepared("DELETE FROM sandbox_service_grants; DELETE FROM sandbox_access_runtime_records").await.unwrap();
        Migration.down(&manager).await.unwrap();
        test.cleanup().await;
    }
}
