// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Shield acts on its verdicts (ADR 066, phases 4 to 7).
//!
//! - `projects.shield_mode` (default `log`) and `environments.shield_mode`
//!   (NULL inherits) replace attack mode (D1): a project or environment in
//!   attack mode starts in `attack`, and an environment that turned attack
//!   mode off under a project that had it on keeps it off as `log`. The API
//!   writes `attack_mode` too for one release, then it is dropped.
//! - `environments.shield_exempt_paths`: paths never challenged (D7).
//! - `shield_bypass_tokens`: tokens monitors and API clients send to skip
//!   Shield, stored as SHA-256 (D7).
//! - `shield_operator_entries`: operator rules, platform-wide or per
//!   project, on a range or a fingerprint (D2).
//! - `shield_observed`: what the fleet learned from events (D9).
//! - `shield_events`: events without ClickHouse, kept 7 days (I8).

use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

const UP: &str = r#"
ALTER TABLE projects ADD COLUMN IF NOT EXISTS shield_mode TEXT NOT NULL DEFAULT 'log';
ALTER TABLE environments ADD COLUMN IF NOT EXISTS shield_mode TEXT;
ALTER TABLE environments
    ADD COLUMN IF NOT EXISTS shield_exempt_paths JSONB NOT NULL DEFAULT '[]'::jsonb;

UPDATE projects SET shield_mode = 'attack' WHERE attack_mode AND shield_mode = 'log';
UPDATE environments SET shield_mode = 'attack'
    WHERE attack_mode IS TRUE AND shield_mode IS NULL;
UPDATE environments e SET shield_mode = 'log'
    FROM projects p
    WHERE e.project_id = p.id AND e.attack_mode IS FALSE AND p.attack_mode
      AND e.shield_mode IS NULL;

DO $$ BEGIN
    ALTER TABLE projects ADD CONSTRAINT projects_shield_mode_check
        CHECK (shield_mode IN ('off', 'log', 'selective', 'attack'));
EXCEPTION WHEN duplicate_object THEN NULL;
END $$;
DO $$ BEGIN
    ALTER TABLE environments ADD CONSTRAINT environments_shield_mode_check
        CHECK (shield_mode IS NULL OR shield_mode IN ('off', 'log', 'selective', 'attack'));
EXCEPTION WHEN duplicate_object THEN NULL;
END $$;

CREATE TABLE IF NOT EXISTS shield_bypass_tokens (
    id BIGSERIAL PRIMARY KEY,
    project_id INTEGER NOT NULL REFERENCES projects(id) ON DELETE CASCADE,
    environment_id INTEGER REFERENCES environments(id) ON DELETE CASCADE,
    name VARCHAR(128) NOT NULL,
    token_hash VARCHAR(64) NOT NULL UNIQUE,
    created_by INTEGER REFERENCES users(id) ON DELETE SET NULL,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    last_used_at TIMESTAMPTZ,
    revoked_at TIMESTAMPTZ
);
CREATE INDEX IF NOT EXISTS shield_bypass_tokens_project
    ON shield_bypass_tokens (project_id) WHERE revoked_at IS NULL;

CREATE TABLE IF NOT EXISTS shield_operator_entries (
    id BIGSERIAL PRIMARY KEY,
    scope VARCHAR(16) NOT NULL CHECK (scope IN ('platform', 'project')),
    project_id INTEGER REFERENCES projects(id) ON DELETE CASCADE,
    network CIDR,
    ja4 VARCHAR(64),
    verdict VARCHAR(16) NOT NULL CHECK (verdict IN ('allow', 'log', 'challenge', 'block')),
    note TEXT,
    created_by INTEGER REFERENCES users(id) ON DELETE SET NULL,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    expires_at TIMESTAMPTZ,
    CHECK ((network IS NULL) <> (ja4 IS NULL)),
    CHECK ((scope = 'project') = (project_id IS NOT NULL))
);

CREATE TABLE IF NOT EXISTS shield_observed (
    id BIGSERIAL PRIMARY KEY,
    kind VARCHAR(8) NOT NULL CHECK (kind IN ('ip', 'ja4')),
    subject VARCHAR(64) NOT NULL,
    verdict VARCHAR(16) NOT NULL CHECK (verdict IN ('challenge', 'block')),
    reason TEXT NOT NULL,
    first_seen TIMESTAMPTZ NOT NULL,
    last_seen TIMESTAMPTZ NOT NULL,
    hits BIGINT NOT NULL DEFAULT 0,
    expires_at TIMESTAMPTZ NOT NULL,
    UNIQUE (kind, subject)
);
CREATE INDEX IF NOT EXISTS shield_observed_expires ON shield_observed (expires_at);

CREATE TABLE IF NOT EXISTS shield_events (
    id BIGSERIAL PRIMARY KEY,
    timestamp TIMESTAMPTZ NOT NULL,
    project_id INTEGER NOT NULL,
    environment_id INTEGER NOT NULL,
    node_id INTEGER,
    request_id VARCHAR(64) NOT NULL,
    kind VARCHAR(32) NOT NULL,
    ip VARCHAR(64) NOT NULL,
    ja4 VARCHAR(64),
    verdict VARCHAR(16) NOT NULL,
    mode VARCHAR(16) NOT NULL,
    reasons JSONB NOT NULL DEFAULT '[]'::jsonb,
    signature VARCHAR(64),
    host TEXT NOT NULL,
    path TEXT NOT NULL,
    user_agent TEXT NOT NULL,
    difficulty SMALLINT,
    solve_ms INTEGER
);
CREATE INDEX IF NOT EXISTS shield_events_environment_time
    ON shield_events (environment_id, timestamp DESC);
CREATE INDEX IF NOT EXISTS shield_events_time ON shield_events (timestamp);
CREATE INDEX IF NOT EXISTS shield_events_ip_time ON shield_events (ip, timestamp);
"#;

const DOWN: &str = r#"
DROP TABLE IF EXISTS shield_events;
DROP TABLE IF EXISTS shield_observed;
DROP TABLE IF EXISTS shield_operator_entries;
DROP TABLE IF EXISTS shield_bypass_tokens;
ALTER TABLE environments DROP CONSTRAINT IF EXISTS environments_shield_mode_check;
ALTER TABLE projects DROP CONSTRAINT IF EXISTS projects_shield_mode_check;
ALTER TABLE environments DROP COLUMN IF EXISTS shield_exempt_paths;
ALTER TABLE environments DROP COLUMN IF EXISTS shield_mode;
ALTER TABLE projects DROP COLUMN IF EXISTS shield_mode;
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

    /// Attack mode carries over as Shield mode `attack`, and an environment
    /// that had turned it off keeps it off; `down` then `up` again is clean.
    #[tokio::test]
    async fn attack_mode_becomes_a_shield_mode() -> Result<(), Box<dyn std::error::Error>> {
        let Ok(docker) = bollard::Docker::connect_with_local_defaults() else {
            eprintln!("Docker unavailable; skipping shield migration integration");
            return Ok(());
        };
        if docker.ping().await.is_err() {
            eprintln!("Docker unavailable; skipping shield migration integration");
            return Ok(());
        }
        let container = GenericImage::new("postgres", "17-alpine")
            .with_wait_for(WaitFor::message_on_stderr(
                "database system is ready to accept connections",
            ))
            .with_env_var("POSTGRES_PASSWORD", "shield-test")
            .start()
            .await?;
        let port = container.get_host_port_ipv4(5432).await?;
        let db = Database::connect(format!(
            "postgres://postgres:shield-test@127.0.0.1:{port}/postgres"
        ))
        .await?;
        // Only the tables this migration reads and alters.
        db.execute_unprepared(
            "CREATE TABLE users(id SERIAL PRIMARY KEY); \
             CREATE TABLE projects(id SERIAL PRIMARY KEY, attack_mode BOOLEAN NOT NULL); \
             CREATE TABLE environments(id SERIAL PRIMARY KEY, project_id INTEGER NOT NULL, \
                 attack_mode BOOLEAN); \
             INSERT INTO projects(id, attack_mode) VALUES (1, true), (2, false); \
             INSERT INTO environments(id, project_id, attack_mode) VALUES \
                 (1, 1, NULL), (2, 1, false), (3, 2, true), (4, 2, NULL);",
        )
        .await?;

        let manager = SchemaManager::new(&db);
        Migration.up(&manager).await?;

        let modes = |sql: &'static str| {
            let db = &db;
            async move {
                let rows = db
                    .query_all(Statement::from_string(
                        DatabaseBackend::Postgres,
                        sql.to_string(),
                    ))
                    .await?;
                rows.iter()
                    .map(|row| row.try_get::<Option<String>>("", "mode"))
                    .collect::<Result<Vec<_>, _>>()
            }
        };
        assert_eq!(
            modes("SELECT shield_mode AS mode FROM projects ORDER BY id").await?,
            [Some("attack".to_string()), Some("log".to_string())]
        );
        assert_eq!(
            modes("SELECT shield_mode AS mode FROM environments ORDER BY id").await?,
            [
                None,
                Some("log".to_string()),
                Some("attack".to_string()),
                None
            ]
        );
        assert!(db
            .execute_unprepared("UPDATE projects SET shield_mode = 'loud' WHERE id = 1")
            .await
            .is_err());
        db.execute_unprepared(
            "INSERT INTO shield_operator_entries(scope, network, verdict) \
             VALUES ('platform', '185.93.89.0/24', 'block')",
        )
        .await?;
        assert!(db
            .execute_unprepared(
                "INSERT INTO shield_operator_entries(scope, network, ja4, verdict) \
                 VALUES ('platform', '10.0.0.0/8', 't13d', 'block')",
            )
            .await
            .is_err());

        Migration.down(&manager).await?;
        Migration.up(&manager).await?;
        Migration.up(&manager).await?;
        Ok(())
    }
}
