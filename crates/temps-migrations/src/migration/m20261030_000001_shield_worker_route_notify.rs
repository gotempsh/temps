// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Shield changes reach workers with the next route snapshot (ADR 066, D10
//! and I3).
//!
//! Workers receive each route's Shield settings (mode, exempt paths, bypass
//! token hashes) and the current Shield snapshot revision in the route
//! snapshot, which is rebuilt on `route_table_changes`. These triggers
//! announce every change to them, whichever code path writes it, so a
//! worker's waiting long poll returns at once:
//!
//! - a new `shield_snapshots` revision;
//! - a bypass token created, revoked, moved or deleted (not a use, which
//!   only touches `last_used_at`);
//! - `shield_mode` or `attack_mode` of a project, and those or
//!   `shield_exempt_paths` of an environment.

use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

const UP: &str = r#"
CREATE OR REPLACE FUNCTION shield_route_change_notify() RETURNS trigger AS $$
BEGIN
    PERFORM pg_notify('route_table_changes', 'shield');
    RETURN NULL;
END;
$$ LANGUAGE plpgsql;

DROP TRIGGER IF EXISTS shield_snapshot_stored ON shield_snapshots;
CREATE TRIGGER shield_snapshot_stored
    AFTER INSERT ON shield_snapshots
    FOR EACH STATEMENT EXECUTE FUNCTION shield_route_change_notify();

DROP TRIGGER IF EXISTS shield_bypass_tokens_changed ON shield_bypass_tokens;
CREATE TRIGGER shield_bypass_tokens_changed
    AFTER INSERT OR DELETE OR UPDATE OF project_id, environment_id, token_hash, revoked_at
    ON shield_bypass_tokens
    FOR EACH STATEMENT EXECUTE FUNCTION shield_route_change_notify();

DROP TRIGGER IF EXISTS shield_project_mode_changed ON projects;
CREATE TRIGGER shield_project_mode_changed
    AFTER UPDATE OF shield_mode, attack_mode ON projects
    FOR EACH ROW
    WHEN (OLD.shield_mode IS DISTINCT FROM NEW.shield_mode
          OR OLD.attack_mode IS DISTINCT FROM NEW.attack_mode)
    EXECUTE FUNCTION shield_route_change_notify();

DROP TRIGGER IF EXISTS shield_environment_policy_changed ON environments;
CREATE TRIGGER shield_environment_policy_changed
    AFTER UPDATE OF shield_mode, attack_mode, shield_exempt_paths ON environments
    FOR EACH ROW
    WHEN (OLD.shield_mode IS DISTINCT FROM NEW.shield_mode
          OR OLD.attack_mode IS DISTINCT FROM NEW.attack_mode
          OR OLD.shield_exempt_paths IS DISTINCT FROM NEW.shield_exempt_paths)
    EXECUTE FUNCTION shield_route_change_notify();
"#;

const DOWN: &str = r#"
DROP TRIGGER IF EXISTS shield_environment_policy_changed ON environments;
DROP TRIGGER IF EXISTS shield_project_mode_changed ON projects;
DROP TRIGGER IF EXISTS shield_bypass_tokens_changed ON shield_bypass_tokens;
DROP TRIGGER IF EXISTS shield_snapshot_stored ON shield_snapshots;
DROP FUNCTION IF EXISTS shield_route_change_notify();
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
    use sea_orm::{Database, DatabaseBackend, Statement};
    use testcontainers::{core::WaitFor, runners::AsyncRunner, GenericImage, ImageExt};

    /// Every Shield change a worker route carries announces a route reload,
    /// a token's use and an update that changes nothing do not, and `down`
    /// then `up` again is clean.
    #[tokio::test]
    async fn shield_changes_announce_a_route_reload() -> Result<(), Box<dyn std::error::Error>> {
        let Ok(docker) = bollard::Docker::connect_with_local_defaults() else {
            eprintln!("Docker unavailable; skipping Shield route notify migration integration");
            return Ok(());
        };
        if docker.ping().await.is_err() {
            eprintln!("Docker unavailable; skipping Shield route notify migration integration");
            return Ok(());
        }
        let container = GenericImage::new("postgres", "17-alpine")
            .with_wait_for(WaitFor::message_on_stderr(
                "database system is ready to accept connections",
            ))
            .with_env_var("POSTGRES_PASSWORD", "shield-notify-test")
            .start()
            .await?;
        let port = container.get_host_port_ipv4(5432).await?;
        let db = Database::connect(format!(
            "postgres://postgres:shield-notify-test@127.0.0.1:{port}/postgres"
        ))
        .await?;
        db.execute_unprepared(
            "CREATE TABLE projects(id SERIAL PRIMARY KEY, shield_mode TEXT NOT NULL DEFAULT 'log', \
                 attack_mode BOOLEAN NOT NULL DEFAULT false); \
             CREATE TABLE environments(id SERIAL PRIMARY KEY, shield_mode TEXT, attack_mode BOOLEAN, \
                 shield_exempt_paths JSONB NOT NULL DEFAULT '[]'::jsonb); \
             CREATE TABLE shield_snapshots(revision BIGSERIAL PRIMARY KEY, sha256 TEXT, body BYTEA); \
             CREATE TABLE shield_bypass_tokens(id BIGSERIAL PRIMARY KEY, project_id INTEGER, \
                 environment_id INTEGER, token_hash TEXT, last_used_at TIMESTAMPTZ, \
                 revoked_at TIMESTAMPTZ); \
             INSERT INTO projects DEFAULT VALUES; INSERT INTO environments DEFAULT VALUES;",
        )
        .await?;

        let manager = SchemaManager::new(&db);
        Migration.up(&manager).await?;

        let mut listener = sqlx::postgres::PgListener::connect(&format!(
            "postgres://postgres:shield-notify-test@127.0.0.1:{port}/postgres"
        ))
        .await?;
        listener.listen("route_table_changes").await?;
        for (sql, expected) in [
            (
                "INSERT INTO shield_snapshots(sha256, body) VALUES ('00', '\\x00')",
                true,
            ),
            (
                "INSERT INTO shield_bypass_tokens(project_id, token_hash) VALUES (1, 'aa')",
                true,
            ),
            // A use is not a change of what workers hold.
            (
                "UPDATE shield_bypass_tokens SET last_used_at = now()",
                false,
            ),
            ("UPDATE shield_bypass_tokens SET revoked_at = now()", true),
            ("UPDATE projects SET shield_mode = 'selective'", true),
            ("UPDATE projects SET shield_mode = 'selective'", false),
            (
                "UPDATE environments SET shield_exempt_paths = '[\"/api/*\"]'::jsonb",
                true,
            ),
            ("UPDATE environments SET attack_mode = true", true),
            ("DELETE FROM shield_bypass_tokens", true),
        ] {
            db.execute_unprepared(sql).await?;
            let wait = std::time::Duration::from_millis(if expected { 5000 } else { 300 });
            let announced = match tokio::time::timeout(wait, listener.recv()).await {
                Ok(notification) => {
                    assert_eq!(notification?.payload(), "shield", "{sql}");
                    true
                }
                Err(_) => false,
            };
            assert_eq!(announced, expected, "{sql}");
        }
        let triggers = db
            .query_one(Statement::from_string(
                DatabaseBackend::Postgres,
                "SELECT count(*)::INT AS n FROM pg_trigger WHERE tgname IN \
                 ('shield_snapshot_stored', 'shield_bypass_tokens_changed', \
                  'shield_project_mode_changed', 'shield_environment_policy_changed')"
                    .to_string(),
            ))
            .await?
            .ok_or_else(|| DbErr::RecordNotFound("pg_trigger".to_string()))?;
        assert_eq!(triggers.try_get::<i32>("", "n")?, 4);

        Migration.down(&manager).await?;
        Migration.up(&manager).await?;
        Ok(())
    }
}
