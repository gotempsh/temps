// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! A plugin is pinned to the exact build an administrator approved.
//!
//! `pinned_build` identifies that build and `pinned_deployment_id` names the
//! deployment it came from, for display. When the plugin's service runs any
//! other build, Temps does not attach to it: it records the build in the
//! `pending_*` columns, with the manifest it answered and the identifier an
//! approval must quote, and waits for an administrator.
//!
//! Existing rows get no pin. A plugin without one is pinned to the build
//! that runs when Temps next attaches to it, which is what adding it did.
//!
//! `down` drops the columns. The previous binary does not read them.

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
                 ADD COLUMN IF NOT EXISTS pinned_build TEXT NULL, \
                 ADD COLUMN IF NOT EXISTS pinned_deployment_id INTEGER NULL, \
                 ADD COLUMN IF NOT EXISTS pending_build TEXT NULL, \
                 ADD COLUMN IF NOT EXISTS pending_deployment_id INTEGER NULL, \
                 ADD COLUMN IF NOT EXISTS pending_manifest JSONB NULL, \
                 ADD COLUMN IF NOT EXISTS pending_review TEXT NULL, \
                 ADD COLUMN IF NOT EXISTS pending_since TIMESTAMPTZ NULL",
            )
            .await?;
        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .get_connection()
            .execute_unprepared(
                "ALTER TABLE external_plugin_actors \
                 DROP COLUMN IF EXISTS pinned_build, \
                 DROP COLUMN IF EXISTS pinned_deployment_id, \
                 DROP COLUMN IF EXISTS pending_build, \
                 DROP COLUMN IF EXISTS pending_deployment_id, \
                 DROP COLUMN IF EXISTS pending_manifest, \
                 DROP COLUMN IF EXISTS pending_review, \
                 DROP COLUMN IF EXISTS pending_since",
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
    async fn an_existing_plugin_has_no_pin_and_no_update_waiting() {
        if !temps_test_utils::external_database_configured()
            && !temps_test_utils::docker_available().await
        {
            eprintln!("Skipping external plugin pinned build migration: database unavailable");
            return;
        }
        let test = temps_test_utils::TestDatabase::new().await.unwrap();
        let db = test.connection_arc();
        // The table as the previous migration leaves it, reduced to what
        // this one touches.
        db.execute_unprepared(
            "CREATE TABLE external_plugin_actors (plugin_name TEXT PRIMARY KEY);
             INSERT INTO external_plugin_actors (plugin_name) VALUES ('existing');",
        )
        .await
        .unwrap();

        let manager = SchemaManager::new(db.as_ref());
        Migration.up(&manager).await.unwrap();
        // Idempotent.
        Migration.up(&manager).await.unwrap();

        let row = db
            .query_one(Statement::from_string(
                sea_orm::DbBackend::Postgres,
                "SELECT pinned_build, pinned_deployment_id, pending_build, pending_deployment_id, \
                        pending_manifest, pending_review, pending_since \
                 FROM external_plugin_actors WHERE plugin_name = 'existing'"
                    .to_string(),
            ))
            .await
            .unwrap()
            .unwrap();
        assert_eq!(
            row.try_get::<Option<String>>("", "pinned_build").unwrap(),
            None
        );
        assert_eq!(
            row.try_get::<Option<i32>>("", "pinned_deployment_id")
                .unwrap(),
            None
        );
        assert_eq!(
            row.try_get::<Option<String>>("", "pending_build").unwrap(),
            None
        );
        assert_eq!(
            row.try_get::<Option<String>>("", "pending_review").unwrap(),
            None
        );

        // A pin and a waiting update can be stored together.
        db.execute_unprepared(
            "UPDATE external_plugin_actors SET pinned_build = 'build-1', pinned_deployment_id = 7, \
                 pending_build = 'build-2', pending_deployment_id = 8, \
                 pending_manifest = '{\"name\":\"existing\"}'::jsonb, pending_review = 'review-2', \
                 pending_since = NOW() \
             WHERE plugin_name = 'existing'",
        )
        .await
        .unwrap();

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
        db.execute_unprepared("SELECT pinned_build FROM external_plugin_actors")
            .await
            .expect_err("the columns are gone");
    }
}
