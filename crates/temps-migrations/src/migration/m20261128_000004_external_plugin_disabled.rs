// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! An administrator can switch a plugin off without removing it.
//!
//! `disabled` cuts the plugin off from Temps: no channel, no proxied
//! requests, no events, no AI tools. Its grants, its pinned build and its
//! service are kept, so switching it on again restores it as it was.
//! `disabled_at` and `disabled_by` say when and who, for the list of plugins;
//! the audit log has the full record.
//!
//! Existing rows are not disabled.
//!
//! `down` drops the columns. The previous binary does not read them, and a
//! plugin that was disabled runs again under it.

use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .get_connection()
            .execute_unprepared(
                "ALTER TABLE external_plugin_actors \
                 ADD COLUMN IF NOT EXISTS disabled BOOLEAN NOT NULL DEFAULT FALSE, \
                 ADD COLUMN IF NOT EXISTS disabled_at TIMESTAMPTZ NULL, \
                 ADD COLUMN IF NOT EXISTS disabled_by INTEGER NULL \
                     REFERENCES users(id) ON DELETE SET NULL",
            )
            .await?;
        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .get_connection()
            .execute_unprepared(
                "ALTER TABLE external_plugin_actors \
                 DROP COLUMN IF EXISTS disabled, \
                 DROP COLUMN IF EXISTS disabled_at, \
                 DROP COLUMN IF EXISTS disabled_by",
            )
            .await?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use sea_orm::{ConnectionTrait, Statement};

    #[tokio::test]
    async fn an_existing_plugin_stays_on_and_outlives_the_user_who_disabled_it() {
        if !temps_test_utils::external_database_configured()
            && !temps_test_utils::docker_available().await
        {
            eprintln!("Skipping external plugin disabled migration: database unavailable");
            return;
        }
        let test = temps_test_utils::TestDatabase::new().await.unwrap();
        let db = test.connection_arc();
        // The tables as the previous migration leaves them, reduced to what
        // this one touches.
        db.execute_unprepared(
            "CREATE TABLE users (id SERIAL PRIMARY KEY);
             INSERT INTO users DEFAULT VALUES;
             CREATE TABLE external_plugin_actors (plugin_name TEXT PRIMARY KEY);
             INSERT INTO external_plugin_actors (plugin_name) VALUES ('existing');",
        )
        .await
        .unwrap();

        let manager = SchemaManager::new(db.as_ref());
        Migration.up(&manager).await.unwrap();
        // Idempotent.
        Migration.up(&manager).await.unwrap();

        let state = || async {
            let row = db
                .query_one(Statement::from_string(
                    sea_orm::DbBackend::Postgres,
                    "SELECT disabled, disabled_at IS NOT NULL AS dated, disabled_by \
                     FROM external_plugin_actors WHERE plugin_name = 'existing'"
                        .to_string(),
                ))
                .await
                .unwrap()
                .unwrap();
            (
                row.try_get::<bool>("", "disabled").unwrap(),
                row.try_get::<bool>("", "dated").unwrap(),
                row.try_get::<Option<i32>>("", "disabled_by").unwrap(),
            )
        };
        assert_eq!(state().await, (false, false, None));

        db.execute_unprepared(
            "UPDATE external_plugin_actors SET disabled = TRUE, disabled_at = NOW(), \
                 disabled_by = (SELECT id FROM users LIMIT 1) WHERE plugin_name = 'existing'",
        )
        .await
        .unwrap();
        assert!(matches!(state().await, (true, true, Some(_))));
        // Only an existing user can be named.
        db.execute_unprepared(
            "UPDATE external_plugin_actors SET disabled_by = 999999 WHERE plugin_name = 'existing'",
        )
        .await
        .expect_err("a user that does not exist is refused");
        // Deleting the user keeps the plugin disabled.
        db.execute_unprepared("DELETE FROM users").await.unwrap();
        assert_eq!(state().await, (true, true, None));

        Migration.down(&manager).await.unwrap();
        // Idempotent, and the row survives.
        Migration.down(&manager).await.unwrap();
        let rows = db
            .query_all(Statement::from_string(
                sea_orm::DbBackend::Postgres,
                "SELECT plugin_name FROM external_plugin_actors".to_string(),
            ))
            .await
            .unwrap();
        assert_eq!(rows.len(), 1);
        db.execute_unprepared("SELECT disabled FROM external_plugin_actors")
            .await
            .expect_err("the columns are gone");
    }
}
