// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Internal access grants (ADR 101): which other projects, or environments of
//! them, may call a project's `*.temps.local` routes through the internal
//! proxy.
//!
//! - A grant is owned by the **target** project. `NULL` environment ids mean
//!   "every environment, present and future" on that side.
//! - `CHECK` refuses a self-grant; a unique index over the four scope columns
//!   refuses a duplicate, also between two concurrent requests.
//! - Hard deletes cascade through the foreign keys. A project fenced for
//!   deletion (`is_deleted`) and a soft-deleted environment (`deleted_at`) are
//!   invisible to foreign keys, so row triggers remove their grants, on both
//!   sides, in the transaction that hides them.
//! - Every write announces a route reload on `route_table_changes`, so agents'
//!   long polls return with the new caller lists at once (ADR 101 D5). The
//!   trigger is row-level: a statement that matches nothing announces nothing.

use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

const UP: &str = r#"
CREATE TABLE IF NOT EXISTS internal_access_grants (
    id SERIAL PRIMARY KEY,
    target_project_id INTEGER NOT NULL REFERENCES projects(id) ON DELETE CASCADE,
    -- NULL = every environment of the target project.
    target_environment_id INTEGER REFERENCES environments(id) ON DELETE CASCADE,
    caller_project_id INTEGER NOT NULL REFERENCES projects(id) ON DELETE CASCADE,
    -- NULL = every environment of the caller project.
    caller_environment_id INTEGER REFERENCES environments(id) ON DELETE CASCADE,
    description VARCHAR(200),
    created_by INTEGER REFERENCES users(id) ON DELETE SET NULL,
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    CONSTRAINT internal_access_grants_not_self
        CHECK (caller_project_id <> target_project_id)
);

-- Environment ids are SERIAL, so 0 never names a real one.
CREATE UNIQUE INDEX IF NOT EXISTS internal_access_grants_scope_unique
    ON internal_access_grants (
        target_project_id,
        COALESCE(target_environment_id, 0),
        caller_project_id,
        COALESCE(caller_environment_id, 0)
    );

-- The caller side of the cleanup triggers and of "who can call me" reads.
CREATE INDEX IF NOT EXISTS internal_access_grants_caller_project
    ON internal_access_grants (caller_project_id);
CREATE INDEX IF NOT EXISTS internal_access_grants_target_environment
    ON internal_access_grants (target_environment_id)
    WHERE target_environment_id IS NOT NULL;
CREATE INDEX IF NOT EXISTS internal_access_grants_caller_environment
    ON internal_access_grants (caller_environment_id)
    WHERE caller_environment_id IS NOT NULL;

CREATE OR REPLACE FUNCTION internal_access_grants_notify() RETURNS trigger AS $$
BEGIN
    PERFORM pg_notify('route_table_changes', 'internal_access');
    RETURN NULL;
END;
$$ LANGUAGE plpgsql;

DROP TRIGGER IF EXISTS internal_access_grants_changed ON internal_access_grants;
CREATE TRIGGER internal_access_grants_changed
    AFTER INSERT OR UPDATE OR DELETE ON internal_access_grants
    FOR EACH ROW EXECUTE FUNCTION internal_access_grants_notify();

CREATE OR REPLACE FUNCTION internal_access_grants_drop_for_environment() RETURNS trigger AS $$
BEGIN
    DELETE FROM internal_access_grants
        WHERE target_environment_id = NEW.id OR caller_environment_id = NEW.id;
    RETURN NULL;
END;
$$ LANGUAGE plpgsql;

DROP TRIGGER IF EXISTS internal_access_grants_environment_deleted ON environments;
CREATE TRIGGER internal_access_grants_environment_deleted
    AFTER UPDATE OF deleted_at ON environments
    FOR EACH ROW
    WHEN (OLD.deleted_at IS NULL AND NEW.deleted_at IS NOT NULL)
    EXECUTE FUNCTION internal_access_grants_drop_for_environment();

CREATE OR REPLACE FUNCTION internal_access_grants_drop_for_project() RETURNS trigger AS $$
BEGIN
    DELETE FROM internal_access_grants
        WHERE target_project_id = NEW.id OR caller_project_id = NEW.id;
    RETURN NULL;
END;
$$ LANGUAGE plpgsql;

DROP TRIGGER IF EXISTS internal_access_grants_project_deleted ON projects;
CREATE TRIGGER internal_access_grants_project_deleted
    AFTER UPDATE OF is_deleted ON projects
    FOR EACH ROW
    WHEN (NOT OLD.is_deleted AND NEW.is_deleted)
    EXECUTE FUNCTION internal_access_grants_drop_for_project();
"#;

const DOWN: &str = r#"
DROP TRIGGER IF EXISTS internal_access_grants_project_deleted ON projects;
DROP TRIGGER IF EXISTS internal_access_grants_environment_deleted ON environments;
DROP FUNCTION IF EXISTS internal_access_grants_drop_for_project();
DROP FUNCTION IF EXISTS internal_access_grants_drop_for_environment();
DROP TABLE IF EXISTS internal_access_grants;
DROP FUNCTION IF EXISTS internal_access_grants_notify();
"#;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager.get_connection().execute_unprepared(UP).await?;
        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager.get_connection().execute_unprepared(DOWN).await?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use sea_orm::{ConnectionTrait, Database, DatabaseBackend, DatabaseConnection, Statement};
    use testcontainers::{core::WaitFor, runners::AsyncRunner, GenericImage, ImageExt};

    async fn grant_count(db: &DatabaseConnection) -> Result<i64, DbErr> {
        let row = db
            .query_one(Statement::from_string(
                DatabaseBackend::Postgres,
                "SELECT count(*)::BIGINT AS n FROM internal_access_grants",
            ))
            .await?
            .ok_or_else(|| DbErr::RecordNotFound("internal_access_grants count".to_string()))?;
        row.try_get::<i64>("", "n")
    }

    /// The table's constraints, cleanup on every kind of deletion, on both
    /// sides, the reload announcement, and a clean `down` then `up`.
    #[tokio::test]
    async fn grants_are_unique_cleaned_up_and_announced() -> Result<(), Box<dyn std::error::Error>>
    {
        let Ok(docker) = bollard::Docker::connect_with_local_defaults() else {
            eprintln!("Docker unavailable; skipping internal access grants migration test");
            return Ok(());
        };
        if docker.ping().await.is_err() {
            eprintln!("Docker unavailable; skipping internal access grants migration test");
            return Ok(());
        }
        let container = GenericImage::new("postgres", "17-alpine")
            .with_wait_for(WaitFor::message_on_stderr(
                "database system is ready to accept connections",
            ))
            .with_env_var("POSTGRES_PASSWORD", "internal-access-test")
            .start()
            .await?;
        let port = container.get_host_port_ipv4(5432).await?;
        let url = format!("postgres://postgres:internal-access-test@127.0.0.1:{port}/postgres");
        let db = Database::connect(&url).await?;
        db.execute_unprepared(
            "CREATE TABLE users(id SERIAL PRIMARY KEY); \
             CREATE TABLE projects(id SERIAL PRIMARY KEY, \
                 is_deleted BOOLEAN NOT NULL DEFAULT false); \
             CREATE TABLE environments(id SERIAL PRIMARY KEY, \
                 project_id INTEGER NOT NULL REFERENCES projects(id) ON DELETE CASCADE, \
                 deleted_at TIMESTAMPTZ); \
             INSERT INTO projects DEFAULT VALUES; INSERT INTO projects DEFAULT VALUES; \
             INSERT INTO projects DEFAULT VALUES; \
             INSERT INTO environments(project_id) VALUES (1), (1), (2), (2), (3);",
        )
        .await?;

        let manager = SchemaManager::new(&db);
        Migration.up(&manager).await?;

        let mut listener = sqlx::postgres::PgListener::connect(&url).await?;
        listener.listen("route_table_changes").await?;

        // Project 2 may call all of project 1; env 4 of project 2 may call env 1.
        for (sql, expected) in [
            (
                "INSERT INTO internal_access_grants(target_project_id, caller_project_id) \
                 VALUES (1, 2)",
                true,
            ),
            (
                "INSERT INTO internal_access_grants(target_project_id, target_environment_id, \
                 caller_project_id, caller_environment_id) VALUES (1, 1, 2, 4)",
                true,
            ),
            // Matches nothing: no announcement.
            ("DELETE FROM internal_access_grants WHERE id = 999", false),
        ] {
            db.execute_unprepared(sql).await?;
            let wait = std::time::Duration::from_millis(if expected { 5000 } else { 300 });
            let got = match tokio::time::timeout(wait, listener.recv()).await {
                Ok(notification) => {
                    assert_eq!(notification?.payload(), "internal_access", "{sql}");
                    true
                }
                Err(_) => false,
            };
            assert_eq!(got, expected, "{sql}");
        }
        assert_eq!(grant_count(&db).await?, 2);

        // A self-grant and a duplicate are refused by the database.
        assert!(db
            .execute_unprepared(
                "INSERT INTO internal_access_grants(target_project_id, caller_project_id) \
                 VALUES (1, 1)"
            )
            .await
            .is_err());
        assert!(db
            .execute_unprepared(
                "INSERT INTO internal_access_grants(target_project_id, caller_project_id) \
                 VALUES (1, 2)"
            )
            .await
            .is_err());
        assert_eq!(grant_count(&db).await?, 2);

        // Soft-deleting the caller environment removes only the grant naming it.
        db.execute_unprepared("UPDATE environments SET deleted_at = now() WHERE id = 4")
            .await?;
        assert_eq!(grant_count(&db).await?, 1);
        let removal = tokio::time::timeout(std::time::Duration::from_secs(5), listener.recv())
            .await
            .map_err(|_| "a cascaded removal must announce a reload")??;
        assert_eq!(removal.payload(), "internal_access");

        // Soft-deleting a target environment removes grants naming it as target.
        db.execute_unprepared(
            "INSERT INTO internal_access_grants(target_project_id, target_environment_id, \
             caller_project_id) VALUES (1, 2, 3)",
        )
        .await?;
        db.execute_unprepared("UPDATE environments SET deleted_at = now() WHERE id = 2")
            .await?;
        assert_eq!(grant_count(&db).await?, 1);

        // Fencing the caller project for deletion removes its grants.
        db.execute_unprepared(
            "INSERT INTO internal_access_grants(target_project_id, caller_project_id) \
             VALUES (3, 1)",
        )
        .await?;
        assert_eq!(grant_count(&db).await?, 2);
        db.execute_unprepared("UPDATE projects SET is_deleted = true WHERE id = 2")
            .await?;
        assert_eq!(
            grant_count(&db).await?,
            1,
            "project 2's grant as caller is gone"
        );

        // A hard delete of the target cascades.
        db.execute_unprepared("DELETE FROM projects WHERE id = 3")
            .await?;
        assert_eq!(grant_count(&db).await?, 0);

        Migration.down(&manager).await?;
        Migration.up(&manager).await?;
        assert_eq!(grant_count(&db).await?, 0);
        Ok(())
    }
}
