// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Project identity on general grants and runtimes is provenance. Deleting it
//! must not delete authorization for a surviving directly linked sandbox.
//! Validate replacement foreign keys outside the DDL transaction. Downgrade
//! refuses live records because restoring cascades could destroy their access.
use sea_orm_migration::prelude::*;
#[derive(DeriveMigrationName)]
pub struct Migration;
#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager.get_connection().execute_unprepared("DO $$ DECLARE c record; BEGIN FOR c IN SELECT conname FROM pg_constraint WHERE conrelid='sandbox_access_authorizations'::regclass AND contype='f' AND cardinality(conkey)=3 LOOP EXECUTE format('ALTER TABLE sandbox_access_authorizations DROP CONSTRAINT %I',c.conname); END LOOP; END $$; ALTER TABLE sandbox_service_grants DROP CONSTRAINT IF EXISTS sandbox_service_grants_project_id_fkey; ALTER TABLE sandbox_service_grants ADD CONSTRAINT grant_project_provenance FOREIGN KEY(project_id) REFERENCES projects(id) ON DELETE SET NULL NOT VALID; ALTER TABLE sandbox_access_runtime_records DROP CONSTRAINT IF EXISTS sandbox_access_runtimes_project_id_fkey; ALTER TABLE sandbox_access_runtime_records ADD CONSTRAINT runtime_project_provenance FOREIGN KEY(project_id) REFERENCES projects(id) ON DELETE SET NULL NOT VALID").await?;
        Ok(())
    }
    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager.get_connection().execute_unprepared("DO $$ BEGIN IF EXISTS(SELECT 1 FROM sandbox_service_grants) OR EXISTS(SELECT 1 FROM sandbox_access_runtime_records) THEN RAISE EXCEPTION 'Cannot restore project cascades while access grants or runtime records exist'; END IF; END $$; ALTER TABLE sandbox_access_authorizations ADD FOREIGN KEY(grant_id,project_id,sandbox_id) REFERENCES sandbox_service_grants(id,project_id,sandbox_id) ON DELETE CASCADE; ALTER TABLE sandbox_access_authorizations ADD FOREIGN KEY(runtime_id,project_id,sandbox_id) REFERENCES sandbox_access_runtime_records(id,project_id,sandbox_id) ON DELETE CASCADE; ALTER TABLE sandbox_service_grants DROP CONSTRAINT grant_project_provenance; ALTER TABLE sandbox_service_grants ADD CONSTRAINT sandbox_service_grants_project_id_fkey FOREIGN KEY(project_id) REFERENCES projects(id) ON DELETE CASCADE; ALTER TABLE sandbox_access_runtime_records DROP CONSTRAINT runtime_project_provenance; ALTER TABLE sandbox_access_runtime_records ADD CONSTRAINT sandbox_access_runtimes_project_id_fkey FOREIGN KEY(project_id) REFERENCES projects(id) ON DELETE CASCADE").await?;
        Ok(())
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    use sea_orm::{DbBackend, Statement};
    #[tokio::test]
    async fn project_deletion_preserves_grants_runtimes_and_authorizations() {
        if !temps_test_utils::external_database_configured() && !temps_test_utils::docker_available().await { return; }
        let mut test = temps_test_utils::TestDatabase::new().await.unwrap();
        let db = test.connection_arc();
        db.execute_unprepared("CREATE TABLE projects(id integer PRIMARY KEY); CREATE TABLE sandbox_service_grants(id integer PRIMARY KEY,project_id integer CONSTRAINT sandbox_service_grants_project_id_fkey REFERENCES projects(id) ON DELETE CASCADE,sandbox_id integer, UNIQUE(id,project_id,sandbox_id)); CREATE TABLE sandbox_access_runtime_records(id integer PRIMARY KEY,project_id integer CONSTRAINT sandbox_access_runtimes_project_id_fkey REFERENCES projects(id) ON DELETE CASCADE,sandbox_id integer, UNIQUE(id,project_id,sandbox_id)); CREATE TABLE sandbox_access_authorizations(project_id integer,sandbox_id integer,grant_id integer REFERENCES sandbox_service_grants(id) ON DELETE CASCADE,runtime_id integer REFERENCES sandbox_access_runtime_records(id) ON DELETE CASCADE, FOREIGN KEY(grant_id,project_id,sandbox_id) REFERENCES sandbox_service_grants(id,project_id,sandbox_id), FOREIGN KEY(runtime_id,project_id,sandbox_id) REFERENCES sandbox_access_runtime_records(id,project_id,sandbox_id)); INSERT INTO projects VALUES(1); INSERT INTO sandbox_service_grants VALUES(2,1,4); INSERT INTO sandbox_access_runtime_records VALUES(3,1,4); INSERT INTO sandbox_access_authorizations VALUES(1,4,2,3)").await.unwrap();
        let manager=SchemaManager::new(db.as_ref());
        Migration.up(&manager).await.unwrap();
        db.execute_unprepared("DELETE FROM projects WHERE id=1").await.unwrap();
        let row=db.query_one(Statement::from_string(DbBackend::Postgres,"SELECT EXISTS(SELECT 1 FROM sandbox_access_authorizations a JOIN sandbox_service_grants g ON g.id=a.grant_id JOIN sandbox_access_runtime_records r ON r.id=a.runtime_id WHERE g.project_id IS NULL AND r.project_id IS NULL) AS preserved")).await.unwrap().unwrap();
        assert!(row.try_get::<bool>("","preserved").unwrap());
        assert!(Migration.down(&manager).await.unwrap_err().to_string().contains("access grants or runtime records"));
        db.execute_unprepared("DELETE FROM sandbox_service_grants; DELETE FROM sandbox_access_runtime_records").await.unwrap();
        Migration.down(&manager).await.unwrap();
        test.cleanup().await;
    }
}
