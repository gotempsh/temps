// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Add optional immutable API key scopes. Existing keys remain account-wide.
//! Foreign keys delete scoped credentials with their resources; there is no
//! backfill. Downgrade refuses to broaden a surviving scoped credential.

use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager.get_connection().execute_unprepared(r#"
            ALTER TABLE api_keys ADD COLUMN IF NOT EXISTS project_id integer REFERENCES projects(id) ON DELETE CASCADE;
            ALTER TABLE api_keys ADD COLUMN IF NOT EXISTS environment_id integer REFERENCES environments(id) ON DELETE CASCADE;
            DO $$ BEGIN
              IF NOT EXISTS (SELECT 1 FROM pg_constraint WHERE conrelid = 'api_keys'::regclass AND conname = 'api_key_resource_scope') THEN
                ALTER TABLE api_keys ADD CONSTRAINT api_key_resource_scope CHECK (
                  (environment_id IS NULL OR project_id IS NOT NULL) AND
                  ((project_id IS NOT NULL AND left(key_prefix,4) = 'tkp_') OR
                   (project_id IS NULL AND left(key_prefix,4) <> 'tkp_'))
                );
              END IF;
            END $$;
            CREATE INDEX IF NOT EXISTS api_keys_project_scope_idx ON api_keys(project_id) WHERE project_id IS NOT NULL;
            CREATE INDEX IF NOT EXISTS api_keys_environment_scope_idx ON api_keys(environment_id) WHERE environment_id IS NOT NULL;
        "#).await?;
        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        let row = manager.get_connection().query_one(sea_orm::Statement::from_string(
            sea_orm::DatabaseBackend::Postgres,
            "SELECT EXISTS(SELECT 1 FROM api_keys WHERE project_id IS NOT NULL OR environment_id IS NOT NULL) AS scoped".to_string()
        )).await?.ok_or_else(|| DbErr::Custom("Cannot inspect scoped API keys before removing scope columns".into()))?;
        if row.try_get::<bool>("", "scoped")? {
            return Err(DbErr::Custom("Cannot remove API key scope columns while scoped keys exist; delete those keys before downgrading to avoid broadening their privileges".into()));
        }
        manager.get_connection().execute_unprepared(
            "ALTER TABLE api_keys DROP COLUMN IF EXISTS environment_id; ALTER TABLE api_keys DROP COLUMN IF EXISTS project_id"
        ).await?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[tokio::test]
    async fn scoped_keys_survive_upgrade_and_refuse_unsafe_downgrade() {
        if !temps_test_utils::external_database_configured() && !temps_test_utils::docker_available().await {
            eprintln!("Skipping API key scope migration: database unavailable");
            return;
        }
        let mut test = temps_test_utils::TestDatabase::new().await.unwrap();
        let db = test.connection_arc();
        db.execute_unprepared("CREATE TABLE projects(id integer PRIMARY KEY); CREATE TABLE environments(id integer PRIMARY KEY); CREATE TABLE api_keys(id integer PRIMARY KEY, key_prefix text NOT NULL); INSERT INTO projects VALUES (7); INSERT INTO environments VALUES (9); INSERT INTO api_keys VALUES(1,'tk_old')").await.unwrap();
        let manager = SchemaManager::new(db.as_ref());
        Migration.up(&manager).await.unwrap();
        Migration.up(&manager).await.unwrap();
        db.execute_unprepared("INSERT INTO api_keys VALUES(2,'tkp_test',7,9)").await.unwrap();
        for sql in [
            "INSERT INTO api_keys VALUES(3,'tk_bad',7,9)",
            "INSERT INTO api_keys VALUES(3,'tkp_bad',NULL,9)",
            "INSERT INTO api_keys VALUES(3,'tkp_bad',8,9)",
            "UPDATE api_keys SET key_prefix='tk_old' WHERE id=2",
        ] { assert!(db.execute_unprepared(sql).await.is_err(), "{sql}"); }
        assert!(Migration.down(&manager).await.is_err());
        db.execute_unprepared("DELETE FROM environments WHERE id=9").await.unwrap();
        let row = db.query_one(sea_orm::Statement::from_string(sea_orm::DbBackend::Postgres, "SELECT count(*) AS count FROM api_keys")).await.unwrap().unwrap();
        assert_eq!(row.try_get::<i64>("", "count").unwrap(), 1);
        Migration.down(&manager).await.unwrap();
        test.cleanup().await;
    }
}
