// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Internal access grants with a sandbox caller (ADR 105): a project lets one
//! sandbox call its `*.temps.local` routes through the internal proxy.
//!
//! - Its own table, not a nullable column of `internal_access_grants`: a
//!   binary rolled back to before this migration decodes that table's
//!   `caller_project_id` as `NOT NULL`, and one sandbox row would fail its
//!   whole grant list.
//! - A grant is owned by the **target** project. A `NULL` environment id means
//!   every environment of it, present and future.
//! - A unique index over the scope refuses a duplicate, also between two
//!   concurrent requests.
//! - Hard deletes cascade through the foreign keys. A sandbox marked
//!   destroyed, a project fenced for deletion and a soft-deleted environment
//!   are invisible to foreign keys, so row triggers remove their grants in the
//!   transaction that marks them.
//! - Every write announces a route reload on `route_table_changes`, so a
//!   revoked grant, or a destroyed sandbox, leaves every node's caller lists
//!   at once. So does a sandbox moved to another node while it holds a
//!   grant: a snapshot lists a sandbox only on the node it runs on. Its
//!   container name comes from its public id, which never changes, so
//!   relabelling it (`sandboxes.name`) announces nothing.

use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

const UP: &str = r#"
CREATE TABLE IF NOT EXISTS internal_access_sandbox_grants (
    id SERIAL PRIMARY KEY,
    target_project_id INTEGER NOT NULL REFERENCES projects(id) ON DELETE CASCADE,
    -- NULL = every environment of the target project.
    target_environment_id INTEGER REFERENCES environments(id) ON DELETE CASCADE,
    sandbox_id INTEGER NOT NULL REFERENCES sandboxes(id) ON DELETE CASCADE,
    description VARCHAR(200),
    created_by INTEGER REFERENCES users(id) ON DELETE SET NULL,
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

-- Environment ids are SERIAL, so 0 never names a real one.
CREATE UNIQUE INDEX IF NOT EXISTS internal_access_sandbox_grants_scope_unique
    ON internal_access_sandbox_grants (
        target_project_id,
        COALESCE(target_environment_id, 0),
        sandbox_id
    );

-- The sandbox side of the cleanup trigger and of the sandbox page's list.
CREATE INDEX IF NOT EXISTS internal_access_sandbox_grants_sandbox
    ON internal_access_sandbox_grants (sandbox_id);
CREATE INDEX IF NOT EXISTS internal_access_sandbox_grants_target_environment
    ON internal_access_sandbox_grants (target_environment_id)
    WHERE target_environment_id IS NOT NULL;

CREATE OR REPLACE FUNCTION internal_access_sandbox_grants_notify() RETURNS trigger AS $$
BEGIN
    PERFORM pg_notify('route_table_changes', 'internal_access');
    RETURN NULL;
END;
$$ LANGUAGE plpgsql;

DROP TRIGGER IF EXISTS internal_access_sandbox_grants_changed ON internal_access_sandbox_grants;
CREATE TRIGGER internal_access_sandbox_grants_changed
    AFTER INSERT OR UPDATE OR DELETE ON internal_access_sandbox_grants
    FOR EACH ROW EXECUTE FUNCTION internal_access_sandbox_grants_notify();

CREATE OR REPLACE FUNCTION internal_access_sandbox_grants_drop_for_environment()
RETURNS trigger AS $$
BEGIN
    DELETE FROM internal_access_sandbox_grants WHERE target_environment_id = NEW.id;
    RETURN NULL;
END;
$$ LANGUAGE plpgsql;

DROP TRIGGER IF EXISTS internal_access_sandbox_grants_environment_deleted ON environments;
CREATE TRIGGER internal_access_sandbox_grants_environment_deleted
    AFTER UPDATE OF deleted_at ON environments
    FOR EACH ROW
    WHEN (OLD.deleted_at IS NULL AND NEW.deleted_at IS NOT NULL)
    EXECUTE FUNCTION internal_access_sandbox_grants_drop_for_environment();

CREATE OR REPLACE FUNCTION internal_access_sandbox_grants_drop_for_project()
RETURNS trigger AS $$
BEGIN
    DELETE FROM internal_access_sandbox_grants WHERE target_project_id = NEW.id;
    RETURN NULL;
END;
$$ LANGUAGE plpgsql;

DROP TRIGGER IF EXISTS internal_access_sandbox_grants_project_deleted ON projects;
CREATE TRIGGER internal_access_sandbox_grants_project_deleted
    AFTER UPDATE OF is_deleted ON projects
    FOR EACH ROW
    WHEN (NOT OLD.is_deleted AND NEW.is_deleted)
    EXECUTE FUNCTION internal_access_sandbox_grants_drop_for_project();

-- A destroyed sandbox's grants are removed. A sandbox moved to another node
-- while it holds one announces a reload: a snapshot lists it only on its node.
CREATE OR REPLACE FUNCTION internal_access_sandbox_grants_sandbox_changed()
RETURNS trigger AS $$
BEGIN
    IF NEW.status = 'destroyed' THEN
        DELETE FROM internal_access_sandbox_grants WHERE sandbox_id = NEW.id;
    ELSIF EXISTS (SELECT 1 FROM internal_access_sandbox_grants WHERE sandbox_id = NEW.id) THEN
        PERFORM pg_notify('route_table_changes', 'internal_access');
    END IF;
    RETURN NULL;
END;
$$ LANGUAGE plpgsql;

DROP TRIGGER IF EXISTS internal_access_sandbox_grants_sandbox_changed ON sandboxes;
CREATE TRIGGER internal_access_sandbox_grants_sandbox_changed
    AFTER UPDATE OF status, node_id ON sandboxes
    FOR EACH ROW
    WHEN ((NEW.status = 'destroyed' AND OLD.status IS DISTINCT FROM 'destroyed')
          OR OLD.node_id IS DISTINCT FROM NEW.node_id)
    EXECUTE FUNCTION internal_access_sandbox_grants_sandbox_changed();
"#;

const DOWN: &str = r#"
DROP TRIGGER IF EXISTS internal_access_sandbox_grants_sandbox_changed ON sandboxes;
DROP TRIGGER IF EXISTS internal_access_sandbox_grants_project_deleted ON projects;
DROP TRIGGER IF EXISTS internal_access_sandbox_grants_environment_deleted ON environments;
DROP FUNCTION IF EXISTS internal_access_sandbox_grants_sandbox_changed();
DROP FUNCTION IF EXISTS internal_access_sandbox_grants_drop_for_project();
DROP FUNCTION IF EXISTS internal_access_sandbox_grants_drop_for_environment();
DROP TABLE IF EXISTS internal_access_sandbox_grants;
DROP FUNCTION IF EXISTS internal_access_sandbox_grants_notify();
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
                "SELECT count(*)::BIGINT AS n FROM internal_access_sandbox_grants",
            ))
            .await?
            .ok_or_else(|| {
                DbErr::RecordNotFound("internal_access_sandbox_grants count".to_string())
            })?;
        row.try_get::<i64>("", "n")
    }

    /// Whether a reload is announced within `wait`.
    async fn announced(
        listener: &mut sqlx::postgres::PgListener,
        wait: std::time::Duration,
    ) -> Result<bool, Box<dyn std::error::Error>> {
        match tokio::time::timeout(wait, listener.recv()).await {
            Ok(notification) => {
                assert_eq!(notification?.payload(), "internal_access");
                Ok(true)
            }
            Err(_) => Ok(false),
        }
    }

    /// Uniqueness, cleanup on every kind of deletion (a destroyed sandbox
    /// included), the reload announcements, and a clean `down` then `up`.
    #[tokio::test]
    async fn sandbox_grants_are_unique_cleaned_up_and_announced(
    ) -> Result<(), Box<dyn std::error::Error>> {
        let Ok(docker) = bollard::Docker::connect_with_local_defaults() else {
            eprintln!("Docker unavailable; skipping sandbox grants migration test");
            return Ok(());
        };
        if docker.ping().await.is_err() {
            eprintln!("Docker unavailable; skipping sandbox grants migration test");
            return Ok(());
        }
        let container = GenericImage::new("postgres", "17-alpine")
            .with_wait_for(WaitFor::message_on_stderr(
                "database system is ready to accept connections",
            ))
            .with_env_var("POSTGRES_PASSWORD", "sandbox-grants-test")
            .start()
            .await?;
        let port = container.get_host_port_ipv4(5432).await?;
        let url = format!("postgres://postgres:sandbox-grants-test@127.0.0.1:{port}/postgres");
        let db = Database::connect(&url).await?;
        db.execute_unprepared(
            "CREATE TABLE users(id SERIAL PRIMARY KEY); \
             CREATE TABLE projects(id SERIAL PRIMARY KEY, \
                 is_deleted BOOLEAN NOT NULL DEFAULT false); \
             CREATE TABLE environments(id SERIAL PRIMARY KEY, \
                 project_id INTEGER NOT NULL REFERENCES projects(id) ON DELETE CASCADE, \
                 deleted_at TIMESTAMPTZ); \
             CREATE TABLE sandboxes(id SERIAL PRIMARY KEY, name TEXT NOT NULL, \
                 status TEXT NOT NULL DEFAULT 'running', node_id INTEGER, \
                 last_activity_at TIMESTAMPTZ NOT NULL DEFAULT now()); \
             INSERT INTO projects DEFAULT VALUES; INSERT INTO projects DEFAULT VALUES; \
             INSERT INTO environments(project_id) VALUES (1), (1), (2); \
             INSERT INTO sandboxes(name) VALUES ('temps-sandbox-a'), ('temps-sandbox-b');",
        )
        .await?;

        let manager = SchemaManager::new(&db);
        Migration.up(&manager).await?;

        let mut listener = sqlx::postgres::PgListener::connect(&url).await?;
        listener.listen("route_table_changes").await?;
        let soon = std::time::Duration::from_secs(5);
        let quiet = std::time::Duration::from_millis(300);

        // Sandbox 1 may call all of project 1; sandbox 2 only environment 1.
        db.execute_unprepared(
            "INSERT INTO internal_access_sandbox_grants(target_project_id, sandbox_id) \
             VALUES (1, 1)",
        )
        .await?;
        assert!(announced(&mut listener, soon).await?, "a new grant");
        db.execute_unprepared(
            "INSERT INTO internal_access_sandbox_grants(target_project_id, \
             target_environment_id, sandbox_id) VALUES (1, 1, 2)",
        )
        .await?;
        assert!(announced(&mut listener, soon).await?);
        assert_eq!(grant_count(&db).await?, 2);

        // A duplicate is refused by the database.
        assert!(db
            .execute_unprepared(
                "INSERT INTO internal_access_sandbox_grants(target_project_id, sandbox_id) \
                 VALUES (1, 1)"
            )
            .await
            .is_err());

        // Activity, or stopping a sandbox, announces nothing and keeps its
        // grant: a stopped sandbox reaches nothing, and its grant applies again
        // when it is woken.
        db.execute_unprepared("UPDATE sandboxes SET last_activity_at = now() WHERE id = 1")
            .await?;
        assert!(!announced(&mut listener, quiet).await?, "activity only");
        db.execute_unprepared("UPDATE sandboxes SET status = 'stopped' WHERE id = 1")
            .await?;
        assert!(!announced(&mut listener, quiet).await?, "a stopped sandbox");
        assert_eq!(grant_count(&db).await?, 2, "stopping keeps the grant");
        // Relabelling one changes nothing a snapshot holds.
        db.execute_unprepared("UPDATE sandboxes SET name = 'agent scratchpad' WHERE id = 1")
            .await?;
        assert!(!announced(&mut listener, quiet).await?, "a relabelled sandbox");
        // Moving one that holds a grant announces a reload.
        db.execute_unprepared("UPDATE sandboxes SET node_id = 3 WHERE id = 1")
            .await?;
        assert!(announced(&mut listener, soon).await?, "a moved sandbox");

        // Destroying a sandbox removes its grants, and announces the removal.
        db.execute_unprepared("UPDATE sandboxes SET status = 'destroyed' WHERE id = 1")
            .await?;
        assert_eq!(grant_count(&db).await?, 1);
        assert!(announced(&mut listener, soon).await?, "a destroyed sandbox");

        // Soft-deleting the target environment removes the grant naming it.
        db.execute_unprepared("UPDATE environments SET deleted_at = now() WHERE id = 1")
            .await?;
        assert_eq!(grant_count(&db).await?, 0);

        // Fencing the target project removes its grants.
        db.execute_unprepared(
            "INSERT INTO internal_access_sandbox_grants(target_project_id, sandbox_id) \
             VALUES (2, 2)",
        )
        .await?;
        db.execute_unprepared("UPDATE projects SET is_deleted = true WHERE id = 2")
            .await?;
        assert_eq!(grant_count(&db).await?, 0);

        // A hard delete of the sandbox row cascades.
        db.execute_unprepared(
            "INSERT INTO internal_access_sandbox_grants(target_project_id, sandbox_id) \
             VALUES (1, 2)",
        )
        .await?;
        db.execute_unprepared("DELETE FROM sandboxes WHERE id = 2")
            .await?;
        assert_eq!(grant_count(&db).await?, 0);

        Migration.down(&manager).await?;
        Migration.up(&manager).await?;
        assert_eq!(grant_count(&db).await?, 0);
        Ok(())
    }
}
