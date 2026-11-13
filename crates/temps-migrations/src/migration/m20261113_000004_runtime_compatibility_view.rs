// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Keep previous binaries' non-null runtime reader valid during binary rollback.
//! Renaming preserves rows, indexes and foreign keys without a history rewrite.
//! The old name remains an automatically writable project-only view. New readers
//! use the complete storage table; rollback cannot expose general rows to old code.
use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager.get_connection().execute_unprepared(r#"
DO $$ BEGIN
 IF to_regclass('sandbox_access_runtime_records') IS NULL THEN
  ALTER TABLE sandbox_access_runtimes RENAME TO sandbox_access_runtime_records;
 END IF;
END $$;
CREATE OR REPLACE VIEW sandbox_access_runtimes AS
 SELECT * FROM sandbox_access_runtime_records WHERE project_id IS NOT NULL
 WITH CASCADED CHECK OPTION;
DO $$ DECLARE definition text; BEGIN
 FOR definition IN SELECT pg_get_functiondef(p.oid) FROM pg_proc p
 JOIN pg_namespace n ON n.oid=p.pronamespace WHERE n.nspname=current_schema()
 AND p.proname IN ('sandbox_access_authorization_changed','sandbox_access_grant_changed','sandbox_access_service_changed')
 LOOP
  EXECUTE replace(definition,'sandbox_access_runtimes','sandbox_access_runtime_records');
 END LOOP;
END $$;
"#).await?;
        Ok(())
    }
    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager.get_connection().execute_unprepared(r#"
DO $$ DECLARE definition text; BEGIN
 IF EXISTS(SELECT 1 FROM sandbox_access_runtime_records WHERE project_id IS NULL) THEN
  RAISE EXCEPTION 'Cannot remove runtime compatibility while projectless runtime records exist';
 END IF;
 FOR definition IN SELECT pg_get_functiondef(p.oid) FROM pg_proc p
 JOIN pg_namespace n ON n.oid=p.pronamespace WHERE n.nspname=current_schema()
 AND p.proname IN ('sandbox_access_authorization_changed','sandbox_access_grant_changed','sandbox_access_service_changed')
 LOOP
  EXECUTE replace(definition,'sandbox_access_runtime_records','sandbox_access_runtimes');
 END LOOP;
END $$;
DROP VIEW sandbox_access_runtimes;
ALTER TABLE sandbox_access_runtime_records RENAME TO sandbox_access_runtimes;
"#).await?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use sea_orm::{DbBackend, FromQueryResult, Statement};

    #[derive(Debug, FromQueryResult)]
    struct PreviousRuntime { project_id: i32 }

    #[tokio::test]
    async fn previous_reader_and_writes_survive_projectless_rows() {
        if !temps_test_utils::external_database_configured() && !temps_test_utils::docker_available().await {
            eprintln!("Skipping runtime compatibility view: database unavailable"); return;
        }
        let mut test = temps_test_utils::TestDatabase::new().await.unwrap();
        let db = test.connection_arc();
        db.execute_unprepared("CREATE TABLE sandbox_access_runtimes(id integer PRIMARY KEY, project_id integer, closed_at timestamptz); CREATE TABLE consumer(runtime_id integer REFERENCES sandbox_access_runtimes(id)); CREATE FUNCTION sandbox_access_grant_changed() RETURNS bigint LANGUAGE sql AS 'SELECT count(*) FROM sandbox_access_runtimes'").await.unwrap();
        let manager=SchemaManager::new(db.as_ref());
        Migration.up(&manager).await.unwrap();
        db.execute_unprepared("INSERT INTO sandbox_access_runtime_records VALUES(1,7,NULL),(2,NULL,NULL); INSERT INTO consumer VALUES(2)").await.unwrap();
        let rows=PreviousRuntime::find_by_statement(Statement::from_string(DbBackend::Postgres,"SELECT project_id FROM sandbox_access_runtimes WHERE closed_at IS NULL")).all(db.as_ref()).await.unwrap();
        assert_eq!(rows.len(),1);
        assert_eq!(rows[0].project_id,7);
        db.execute_unprepared("INSERT INTO sandbox_access_runtimes VALUES(3,8,NULL); UPDATE sandbox_access_runtimes SET project_id=9 WHERE id=3").await.unwrap();
        assert!(db.execute_unprepared("INSERT INTO sandbox_access_runtimes VALUES(4,NULL,NULL)").await.is_err());
        let row=db.query_one(Statement::from_string(DbBackend::Postgres,"SELECT sandbox_access_grant_changed()=3 AS all_rows")).await.unwrap().unwrap();
        assert!(row.try_get::<bool>("","all_rows").unwrap());
        assert!(Migration.down(&manager).await.unwrap_err().to_string().contains("projectless runtime records"));
        db.execute_unprepared("DELETE FROM consumer; DELETE FROM sandbox_access_runtime_records WHERE project_id IS NULL").await.unwrap();
        Migration.down(&manager).await.unwrap();
        let row=db.query_one(Statement::from_string(DbBackend::Postgres,"SELECT sandbox_access_grant_changed()=2 AS restored")).await.unwrap().unwrap();
        assert!(row.try_get::<bool>("","restored").unwrap());
        test.cleanup().await;
    }
}
