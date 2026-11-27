// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Per-link variable aliases for a project's linked service.
//!
//! A linked database injects its own names (`POSTGRES_URL`); most apps ask for
//! another (`DATABASE_URL`). `env_aliases` maps the app's name onto the
//! injected one (`{"DATABASE_URL": "POSTGRES_URL"}`), so the link injects
//! both. Existing links get `{}`: they inject exactly what they did before,
//! and an older binary that does not read the column is unaffected.

use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .get_connection()
            .execute_unprepared(
                "ALTER TABLE project_services \
                 ADD COLUMN IF NOT EXISTS env_aliases jsonb NOT NULL DEFAULT '{}'::jsonb",
            )
            .await?;
        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        // An alias is the operator's own configuration and nothing else
        // records it: re-upgrading would bring back only empty maps, and
        // the next deployment would lose the names the app reads. Refused
        // while any link has one.
        manager.get_connection().execute_unprepared(DOWN).await?;
        Ok(())
    }
}

const DOWN: &str = r#"
DO $$
BEGIN
    IF EXISTS (
        SELECT 1 FROM information_schema.columns
        WHERE table_schema = current_schema()
          AND table_name = 'project_services' AND column_name = 'env_aliases'
    ) THEN
        LOCK TABLE project_services IN ACCESS EXCLUSIVE MODE;
        IF EXISTS (SELECT 1 FROM project_services WHERE env_aliases <> '{}'::jsonb) THEN
            RAISE EXCEPTION 'Cannot drop project_services.env_aliases while linked services have variable aliases (such as DATABASE_URL for POSTGRES_URL): they would be lost. Remove the aliases from those links, or set the variables on the projects, before downgrading';
        END IF;
        ALTER TABLE project_services DROP COLUMN IF EXISTS env_aliases;
    END IF;
END $$;
"#;

#[cfg(test)]
mod tests {
    use super::*;
    use sea_orm::{ConnectionTrait, DatabaseConnection, Statement};

    async fn aliases(db: &DatabaseConnection, id: i32) -> String {
        db.query_one(Statement::from_string(
            sea_orm::DbBackend::Postgres,
            format!("SELECT env_aliases::text AS aliases FROM project_services WHERE id = {id}"),
        ))
        .await
        .unwrap()
        .unwrap()
        .try_get::<String>("", "aliases")
        .unwrap()
    }

    #[tokio::test]
    async fn existing_links_get_no_aliases_and_down_keeps_the_ones_configured() {
        if !temps_test_utils::external_database_configured()
            && !temps_test_utils::docker_available().await
        {
            eprintln!("Skipping project_services env_aliases migration: database unavailable");
            return;
        }
        let mut test = temps_test_utils::TestDatabase::new().await.unwrap();
        let db = test.connection_arc();
        db.execute_unprepared(
            "CREATE TABLE project_services (
                 id SERIAL PRIMARY KEY,
                 project_id INTEGER NOT NULL,
                 service_id INTEGER NOT NULL
             );
             INSERT INTO project_services (project_id, service_id) VALUES (1, 1);",
        )
        .await
        .unwrap();

        let manager = SchemaManager::new(db.as_ref());
        Migration.up(&manager).await.unwrap();
        // Idempotent.
        Migration.up(&manager).await.unwrap();
        // A link made before keeps injecting exactly what it did.
        assert_eq!(aliases(&db, 1).await, "{}");
        // An older binary inserts without the column.
        db.execute_unprepared(
            "INSERT INTO project_services (project_id, service_id) VALUES (2, 2)",
        )
        .await
        .unwrap();
        assert_eq!(aliases(&db, 2).await, "{}");
        db.execute_unprepared(
            r#"UPDATE project_services SET env_aliases = '{"DATABASE_URL": "POSTGRES_URL"}' WHERE id = 2"#,
        )
        .await
        .unwrap();

        // Down refuses while an alias would be lost, and changes nothing.
        let refused = Migration.down(&manager).await.expect_err("an alias is set");
        assert!(
            refused.to_string().contains("variable aliases"),
            "{refused}"
        );
        assert!(aliases(&db, 2).await.contains("DATABASE_URL"));

        db.execute_unprepared("UPDATE project_services SET env_aliases = '{}'")
            .await
            .unwrap();
        Migration.down(&manager).await.unwrap();
        // Idempotent, and the column is gone.
        Migration.down(&manager).await.unwrap();
        assert!(db
            .execute_unprepared("SELECT env_aliases FROM project_services")
            .await
            .is_err());
        test.cleanup().await;
    }
}
