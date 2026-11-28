// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! The write permission of a plugin is narrowed by area and by project.
//!
//! `write_areas` lists what a plugin granted `api_write` may change
//! (`projects`, `environments`, `deployments`). `write_project_ids` limits
//! those changes to the listed projects; NULL means every project.
//!
//! Existing grants get no write area, so a plugin that was granted
//! `api_write` can change nothing until an administrator chooses what it may
//! change. Nothing widens.
//!
//! `down` drops both columns; the previous binary then treats `api_write` as
//! it did before.

use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        let db = manager.get_connection();
        db.execute_unprepared(
            "ALTER TABLE external_plugin_grants \
             ADD COLUMN IF NOT EXISTS write_areas TEXT[] NOT NULL DEFAULT '{}', \
             ADD COLUMN IF NOT EXISTS write_project_ids INTEGER[] NULL",
        )
        .await?;
        db.execute_unprepared(
            "ALTER TABLE external_plugin_grants \
             DROP CONSTRAINT IF EXISTS external_plugin_grants_write_areas_check",
        )
        .await?;
        db.execute_unprepared(
            "ALTER TABLE external_plugin_grants \
             ADD CONSTRAINT external_plugin_grants_write_areas_check \
             CHECK (write_areas <@ ARRAY['projects', 'environments', 'deployments']::text[])",
        )
        .await?;
        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        let db = manager.get_connection();
        db.execute_unprepared(
            "ALTER TABLE external_plugin_grants \
             DROP CONSTRAINT IF EXISTS external_plugin_grants_write_areas_check, \
             DROP COLUMN IF EXISTS write_project_ids, \
             DROP COLUMN IF EXISTS write_areas",
        )
        .await?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use sea_orm::{ConnectionTrait, DatabaseConnection, Statement};

    async fn scope(db: &DatabaseConnection, actor: &str) -> (Vec<String>, Option<Vec<i32>>) {
        let row = db
            .query_one(Statement::from_string(
                sea_orm::DbBackend::Postgres,
                format!(
                    "SELECT write_areas, write_project_ids FROM external_plugin_grants \
                     WHERE actor_id = '{actor}'"
                ),
            ))
            .await
            .unwrap()
            .unwrap();
        (
            row.try_get("", "write_areas").unwrap(),
            row.try_get("", "write_project_ids").unwrap(),
        )
    }

    #[tokio::test]
    async fn an_existing_write_grant_changes_nothing_until_areas_are_chosen() {
        if !temps_test_utils::external_database_configured()
            && !temps_test_utils::docker_available().await
        {
            eprintln!("Skipping external plugin write scope migration: database unavailable");
            return;
        }
        let test = temps_test_utils::TestDatabase::new().await.unwrap();
        let db = test.connection_arc();
        // The table as earlier migrations leave it, reduced to what this one
        // touches.
        db.execute_unprepared(
            "CREATE TABLE external_plugin_grants (
                 actor_id TEXT PRIMARY KEY,
                 permissions TEXT[] NOT NULL DEFAULT '{}'
             );
             INSERT INTO external_plugin_grants (actor_id, permissions)
                 VALUES ('writer', ARRAY['api_write']);",
        )
        .await
        .unwrap();

        let manager = SchemaManager::new(db.as_ref());
        Migration.up(&manager).await.unwrap();
        // Idempotent.
        Migration.up(&manager).await.unwrap();

        // A grant that existed keeps its permission and gains no write area.
        assert_eq!(scope(&db, "writer").await, (Vec::<String>::new(), None));
        db.execute_unprepared(
            "UPDATE external_plugin_grants \
             SET write_areas = ARRAY['deployments'], write_project_ids = ARRAY[7, 9] \
             WHERE actor_id = 'writer'",
        )
        .await
        .unwrap();
        assert_eq!(
            scope(&db, "writer").await,
            (vec!["deployments".to_string()], Some(vec![7, 9]))
        );
        db.execute_unprepared(
            "UPDATE external_plugin_grants SET write_areas = ARRAY['settings'] \
             WHERE actor_id = 'writer'",
        )
        .await
        .expect_err("an unknown area is refused");

        Migration.down(&manager).await.unwrap();
        // Idempotent, and the grant survives.
        Migration.down(&manager).await.unwrap();
        let kept: Vec<String> = db
            .query_one(Statement::from_string(
                sea_orm::DbBackend::Postgres,
                "SELECT permissions FROM external_plugin_grants WHERE actor_id = 'writer'"
                    .to_string(),
            ))
            .await
            .unwrap()
            .unwrap()
            .try_get("", "permissions")
            .unwrap();
        assert_eq!(kept, ["api_write"]);
    }
}
