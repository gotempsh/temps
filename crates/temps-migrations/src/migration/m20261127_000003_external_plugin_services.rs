// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! A plugin can run as a Temps service: the container of an ordinary
//! deployment. The actor row then names the service (`project_id`) and the
//! environment the plugin runs in, and keeps the manifest it last answered
//! with, so a plugin whose service is stopped for being idle can still be
//! listed and routed to without starting it.
//!
//! Deleting the service or the environment clears the reference instead of
//! deleting the actor: the plugin stays listed, says that its service is
//! gone, and an administrator removes it or adds it again.
//!
//! `down` returns service plugins to `process` isolation, which the previous
//! binary understands. It has no executable to run for them, so they are
//! simply not started.

use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        let db = manager.get_connection();
        db.execute_unprepared(
            "ALTER TABLE external_plugin_actors \
             ADD COLUMN IF NOT EXISTS project_id INTEGER NULL \
                 REFERENCES projects(id) ON DELETE SET NULL, \
             ADD COLUMN IF NOT EXISTS environment_id INTEGER NULL \
                 REFERENCES environments(id) ON DELETE SET NULL, \
             ADD COLUMN IF NOT EXISTS manifest JSONB NULL",
        )
        .await?;
        db.execute_unprepared(
            "ALTER TABLE external_plugin_actors \
             DROP CONSTRAINT IF EXISTS external_plugin_actors_isolation_check",
        )
        .await?;
        db.execute_unprepared(
            "ALTER TABLE external_plugin_actors \
             ADD CONSTRAINT external_plugin_actors_isolation_check \
             CHECK (isolation IN ('container', 'process', 'service'))",
        )
        .await?;
        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        let db = manager.get_connection();
        db.execute_unprepared(
            "UPDATE external_plugin_actors SET isolation = 'process' WHERE isolation = 'service'",
        )
        .await?;
        db.execute_unprepared(
            "ALTER TABLE external_plugin_actors \
             DROP CONSTRAINT IF EXISTS external_plugin_actors_isolation_check",
        )
        .await?;
        db.execute_unprepared(
            "ALTER TABLE external_plugin_actors \
             ADD CONSTRAINT external_plugin_actors_isolation_check \
             CHECK (isolation IN ('container', 'process'))",
        )
        .await?;
        db.execute_unprepared(
            "ALTER TABLE external_plugin_actors \
             DROP COLUMN IF EXISTS manifest, \
             DROP COLUMN IF EXISTS environment_id, \
             DROP COLUMN IF EXISTS project_id",
        )
        .await?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use sea_orm::{ConnectionTrait, DatabaseConnection, Statement};

    /// isolation, project, environment and whether a manifest is kept.
    async fn row(db: &DatabaseConnection, name: &str) -> (String, Option<i32>, Option<i32>, bool) {
        let row = db
            .query_one(Statement::from_string(
                sea_orm::DbBackend::Postgres,
                format!(
                    "SELECT isolation, project_id, environment_id, manifest IS NOT NULL AS kept \
                     FROM external_plugin_actors WHERE plugin_name = '{name}'"
                ),
            ))
            .await
            .unwrap()
            .unwrap();
        (
            row.try_get("", "isolation").unwrap(),
            row.try_get("", "project_id").unwrap(),
            row.try_get("", "environment_id").unwrap(),
            row.try_get("", "kept").unwrap(),
        )
    }

    #[tokio::test]
    async fn a_plugin_row_can_name_a_service_outlives_it_and_down_returns_it_to_a_known_value() {
        if !temps_test_utils::external_database_configured()
            && !temps_test_utils::docker_available().await
        {
            eprintln!("Skipping external plugin services migration: database unavailable");
            return;
        }
        let test = temps_test_utils::TestDatabase::new().await.unwrap();
        let db = test.connection_arc();
        // The tables as the previous migration leaves them, reduced to what
        // this one touches.
        db.execute_unprepared(
            "CREATE TABLE projects (id SERIAL PRIMARY KEY);
             CREATE TABLE environments (
                 id SERIAL PRIMARY KEY,
                 project_id INTEGER NOT NULL REFERENCES projects(id) ON DELETE CASCADE
             );
             CREATE TABLE external_plugin_actors (
                 plugin_name TEXT PRIMARY KEY,
                 isolation TEXT NOT NULL DEFAULT 'process',
                 CONSTRAINT external_plugin_actors_isolation_check
                     CHECK (isolation IN ('container', 'process'))
             );
             INSERT INTO projects DEFAULT VALUES;
             INSERT INTO environments (project_id) VALUES (1);
             INSERT INTO external_plugin_actors (plugin_name, isolation)
                 VALUES ('installed', 'container');",
        )
        .await
        .unwrap();

        let manager = SchemaManager::new(db.as_ref());
        Migration.up(&manager).await.unwrap();
        // Idempotent.
        Migration.up(&manager).await.unwrap();

        // A plugin installed before is untouched and names no service.
        assert_eq!(
            row(&db, "installed").await,
            ("container".to_string(), None, None, false)
        );
        // A plugin can now run as a service, and nothing else is accepted.
        db.execute_unprepared(
            r#"INSERT INTO external_plugin_actors
                   (plugin_name, isolation, project_id, environment_id, manifest)
               VALUES ('reports', 'service', 1, 1, '{"name": "reports"}')"#,
        )
        .await
        .unwrap();
        db.execute_unprepared(
            "INSERT INTO external_plugin_actors (plugin_name, isolation) VALUES ('odd', 'host')",
        )
        .await
        .expect_err("an unknown isolation is refused");
        db.execute_unprepared(
            "INSERT INTO external_plugin_actors (plugin_name, isolation, project_id) \
             VALUES ('orphan', 'service', 999)",
        )
        .await
        .expect_err("a plugin cannot name a service that does not exist");

        // Deleting the service keeps the plugin row and clears the reference.
        db.execute_unprepared("DELETE FROM projects WHERE id = 1")
            .await
            .unwrap();
        assert_eq!(
            row(&db, "reports").await,
            ("service".to_string(), None, None, true)
        );

        // Down leaves every row with an isolation the previous binary knows.
        Migration.down(&manager).await.unwrap();
        let isolations = db
            .query_all(Statement::from_string(
                sea_orm::DbBackend::Postgres,
                "SELECT plugin_name, isolation FROM external_plugin_actors ORDER BY plugin_name",
            ))
            .await
            .unwrap()
            .into_iter()
            .map(|row| {
                (
                    row.try_get::<String>("", "plugin_name").unwrap(),
                    row.try_get::<String>("", "isolation").unwrap(),
                )
            })
            .collect::<Vec<_>>();
        assert_eq!(
            isolations,
            [
                ("installed".to_string(), "container".to_string()),
                ("reports".to_string(), "process".to_string())
            ]
        );
        db.execute_unprepared(
            "INSERT INTO external_plugin_actors (plugin_name, isolation) VALUES ('late', 'service')",
        )
        .await
        .expect_err("the previous constraint is back");
        let columns = db
            .query_all(Statement::from_string(
                sea_orm::DbBackend::Postgres,
                "SELECT column_name FROM information_schema.columns \
                 WHERE table_name = 'external_plugin_actors' AND table_schema = current_schema() \
                 ORDER BY column_name",
            ))
            .await
            .unwrap()
            .into_iter()
            .map(|row| row.try_get::<String>("", "column_name").unwrap())
            .collect::<Vec<_>>();
        assert_eq!(columns, ["isolation", "plugin_name"]);
        // And up again restores the columns.
        Migration.up(&manager).await.unwrap();
        assert_eq!(
            row(&db, "reports").await,
            ("process".to_string(), None, None, false)
        );
    }
}
