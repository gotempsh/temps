// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Every plugin belongs to a tier, recorded on its actor row.
//!
//! `community` is a plugin anyone published: it runs as a Temps service in
//! its own container and is never given the control-plane database or the
//! instance data directory. `verified` is reserved for a plugin that comes
//! from the signed registry and was reviewed.
//!
//! Existing rows become `community`, the tier with the least access, so no
//! plugin gains anything from this migration. Nothing but a reviewed registry
//! entry may ever set `verified`.
//!
//! `down` drops the column. The previous binary does not read it.

use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        let db = manager.get_connection();
        db.execute_unprepared(
            "ALTER TABLE external_plugin_actors \
             ADD COLUMN IF NOT EXISTS tier TEXT NOT NULL DEFAULT 'community'",
        )
        .await?;
        db.execute_unprepared(
            "ALTER TABLE external_plugin_actors \
             DROP CONSTRAINT IF EXISTS external_plugin_actors_tier_check",
        )
        .await?;
        db.execute_unprepared(
            "ALTER TABLE external_plugin_actors \
             ADD CONSTRAINT external_plugin_actors_tier_check \
             CHECK (tier IN ('community', 'verified'))",
        )
        .await?;
        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        let db = manager.get_connection();
        db.execute_unprepared(
            "ALTER TABLE external_plugin_actors \
             DROP CONSTRAINT IF EXISTS external_plugin_actors_tier_check, \
             DROP COLUMN IF EXISTS tier",
        )
        .await?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use sea_orm::{ConnectionTrait, DatabaseConnection, Statement};

    async fn tier(db: &DatabaseConnection, name: &str) -> String {
        db.query_one(Statement::from_string(
            sea_orm::DbBackend::Postgres,
            format!("SELECT tier FROM external_plugin_actors WHERE plugin_name = '{name}'"),
        ))
        .await
        .unwrap()
        .unwrap()
        .try_get("", "tier")
        .unwrap()
    }

    #[tokio::test]
    async fn existing_plugins_become_community_and_no_other_tier_can_be_stored() {
        if !temps_test_utils::external_database_configured()
            && !temps_test_utils::docker_available().await
        {
            eprintln!("Skipping external plugin tier migration: database unavailable");
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

        // A plugin that existed gets the tier with the least access.
        assert_eq!(tier(&db, "existing").await, "community");
        // So does a new one that names no tier.
        db.execute_unprepared("INSERT INTO external_plugin_actors (plugin_name) VALUES ('new')")
            .await
            .unwrap();
        assert_eq!(tier(&db, "new").await, "community");
        db.execute_unprepared(
            "INSERT INTO external_plugin_actors (plugin_name, tier) VALUES ('reviewed', 'verified')",
        )
        .await
        .unwrap();
        db.execute_unprepared(
            "INSERT INTO external_plugin_actors (plugin_name, tier) VALUES ('odd', 'trusted')",
        )
        .await
        .expect_err("an unknown tier is refused");

        Migration.down(&manager).await.unwrap();
        // Idempotent, and the rows survive.
        Migration.down(&manager).await.unwrap();
        let rows = db
            .query_all(Statement::from_string(
                sea_orm::DbBackend::Postgres,
                "SELECT plugin_name FROM external_plugin_actors ORDER BY plugin_name".to_string(),
            ))
            .await
            .unwrap();
        assert_eq!(rows.len(), 3);
        db.execute_unprepared("SELECT tier FROM external_plugin_actors")
            .await
            .expect_err("the column is gone");
    }
}
