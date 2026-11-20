// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Who an address is for, and the runs that make one reachable (ADR 064).
//!
//! - `audience` on `project_custom_domains` and `environment_domains`: who an
//!   address is for. Every address is `public` today; `private` (tailnet
//!   only) and `signed_in` (behind the Temps sign-in) are reserved for
//!   phases 2-4. The generated preview hostname has no row, and is always
//!   `public`.
//! - `reachability_plans`: what a plan was computed from (D4). `apply` acts
//!   only on a stored, unexpired, unused plan whose `fingerprint` still
//!   matches the cluster, so a stale tab or a replayed request never
//!   creates what nobody saw.
//! - `reachability_runs`: one "Make it reachable" run. `steps` is the
//!   per-step progress; `journal` records each resource *before* the call
//!   that creates it (`pending`, with the key that finds it again) and
//!   after (`done`, with its id), so a crash in between is resolved by
//!   looking the key up, and undo removes exactly what was created, newest
//!   first. Secrets are never stored here; a pasted token lives in memory
//!   for the length of the run.

use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

const UP: &str = r#"
ALTER TABLE project_custom_domains
    ADD COLUMN IF NOT EXISTS audience VARCHAR(16) NOT NULL DEFAULT 'public';
ALTER TABLE project_custom_domains
    DROP CONSTRAINT IF EXISTS project_custom_domains_audience_check;
ALTER TABLE project_custom_domains
    ADD CONSTRAINT project_custom_domains_audience_check
    CHECK (audience IN ('public', 'private', 'signed_in'));

ALTER TABLE environment_domains
    ADD COLUMN IF NOT EXISTS audience VARCHAR(16) NOT NULL DEFAULT 'public';
ALTER TABLE environment_domains
    DROP CONSTRAINT IF EXISTS environment_domains_audience_check;
ALTER TABLE environment_domains
    ADD CONSTRAINT environment_domains_audience_check
    CHECK (audience IN ('public', 'private', 'signed_in'));

CREATE TABLE IF NOT EXISTS reachability_plans (
    id VARCHAR(36) PRIMARY KEY,
    project_id INTEGER NOT NULL REFERENCES projects(id) ON DELETE CASCADE,
    environment_id INTEGER NOT NULL REFERENCES environments(id) ON DELETE CASCADE,
    audience VARCHAR(16) NOT NULL DEFAULT 'public',
    address VARCHAR(253),
    fingerprint VARCHAR(64) NOT NULL,
    plan JSONB NOT NULL,
    created_by INTEGER,
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    expires_at TIMESTAMPTZ NOT NULL,
    used_at TIMESTAMPTZ
);
CREATE INDEX IF NOT EXISTS idx_reachability_plans_expires
    ON reachability_plans (expires_at);

CREATE TABLE IF NOT EXISTS reachability_runs (
    id SERIAL PRIMARY KEY,
    project_id INTEGER REFERENCES projects(id) ON DELETE CASCADE,
    environment_id INTEGER REFERENCES environments(id) ON DELETE CASCADE,
    kind VARCHAR(32) NOT NULL,
    audience VARCHAR(16) NOT NULL DEFAULT 'public',
    address VARCHAR(253),
    option VARCHAR(64) NOT NULL,
    plan_id VARCHAR(36),
    state VARCHAR(16) NOT NULL,
    steps JSONB NOT NULL DEFAULT '[]'::jsonb,
    journal JSONB NOT NULL DEFAULT '[]'::jsonb,
    context JSONB NOT NULL DEFAULT '{}'::jsonb,
    undo JSONB,
    error TEXT,
    live_url TEXT,
    created_by INTEGER,
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    finished_at TIMESTAMPTZ
);
CREATE INDEX IF NOT EXISTS idx_reachability_runs_environment
    ON reachability_runs (environment_id, created_at DESC);
"#;

const DOWN: &str = r#"
DROP TABLE IF EXISTS reachability_runs;
DROP TABLE IF EXISTS reachability_plans;
ALTER TABLE environment_domains DROP CONSTRAINT IF EXISTS environment_domains_audience_check;
ALTER TABLE environment_domains DROP COLUMN IF EXISTS audience;
ALTER TABLE project_custom_domains DROP CONSTRAINT IF EXISTS project_custom_domains_audience_check;
ALTER TABLE project_custom_domains DROP COLUMN IF EXISTS audience;
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

    /// Existing addresses become `public`, an unknown audience is refused,
    /// plans and runs cascade with their environment, and `down` then `up`
    /// again is clean.
    #[tokio::test]
    async fn audience_defaults_public_and_runs_cascade() -> Result<(), Box<dyn std::error::Error>> {
        let Ok(docker) = bollard::Docker::connect_with_local_defaults() else {
            eprintln!("Docker unavailable; skipping reachability migration integration");
            return Ok(());
        };
        if docker.ping().await.is_err() {
            eprintln!("Docker unavailable; skipping reachability migration integration");
            return Ok(());
        }
        let container = GenericImage::new("postgres", "17-alpine")
            .with_wait_for(WaitFor::message_on_stderr(
                "database system is ready to accept connections",
            ))
            .with_env_var("POSTGRES_PASSWORD", "reachability-test")
            .start()
            .await?;
        let port = container.get_host_port_ipv4(5432).await?;
        let db = Database::connect(format!(
            "postgres://postgres:reachability-test@127.0.0.1:{port}/postgres"
        ))
        .await?;
        // Only the columns this migration touches or references.
        db.execute_unprepared(
            "CREATE TABLE projects(id SERIAL PRIMARY KEY); \
             CREATE TABLE environments(id SERIAL PRIMARY KEY, project_id INTEGER REFERENCES projects(id) ON DELETE CASCADE); \
             CREATE TABLE project_custom_domains(id SERIAL PRIMARY KEY, project_id INTEGER, domain TEXT); \
             CREATE TABLE environment_domains(id SERIAL PRIMARY KEY, environment_id INTEGER, domain TEXT); \
             INSERT INTO projects DEFAULT VALUES; \
             INSERT INTO environments(project_id) VALUES (1); \
             INSERT INTO project_custom_domains(project_id, domain) VALUES (1, 'app.example.com'); \
             INSERT INTO environment_domains(environment_id, domain) VALUES (1, 'www.example.com');",
        )
        .await?;

        let manager = SchemaManager::new(&db);
        Migration.up(&manager).await?;

        let audience = |table: &'static str| {
            let db = &db;
            async move {
                let row = db
                    .query_one(Statement::from_string(
                        DatabaseBackend::Postgres,
                        format!("SELECT audience FROM {table} LIMIT 1"),
                    ))
                    .await?
                    .ok_or_else(|| DbErr::RecordNotFound(table.to_string()))?;
                row.try_get::<String>("", "audience")
            }
        };
        assert_eq!(audience("project_custom_domains").await?, "public");
        assert_eq!(audience("environment_domains").await?, "public");

        assert!(
            db.execute_unprepared(
                "UPDATE environment_domains SET audience = 'everyone' WHERE id = 1"
            )
            .await
            .is_err(),
            "an unknown audience must be refused"
        );
        db.execute_unprepared(
            "UPDATE project_custom_domains SET audience = 'signed_in' WHERE id = 1",
        )
        .await?;

        db.execute_unprepared(
            "INSERT INTO reachability_runs(project_id, environment_id, kind, option, state) \
             VALUES (1, 1, 'apply', 'existing_tunnel', 'running')",
        )
        .await?;
        db.execute_unprepared(
            "INSERT INTO reachability_plans(id, project_id, environment_id, fingerprint, plan, expires_at) \
             VALUES ('p1', 1, 1, 'f', '{}'::jsonb, NOW() + INTERVAL '15 minutes')",
        )
        .await?;
        db.execute_unprepared("DELETE FROM environments WHERE id = 1")
            .await?;
        let plans = db
            .query_one(Statement::from_string(
                DatabaseBackend::Postgres,
                "SELECT COUNT(*)::INT AS n FROM reachability_plans".to_string(),
            ))
            .await?
            .ok_or_else(|| DbErr::RecordNotFound("count".into()))?
            .try_get::<i32>("", "n")?;
        assert_eq!(plans, 0, "a plan goes with its environment");
        let runs = db
            .query_one(Statement::from_string(
                DatabaseBackend::Postgres,
                "SELECT COUNT(*)::INT AS n FROM reachability_runs".to_string(),
            ))
            .await?
            .ok_or_else(|| DbErr::RecordNotFound("count".into()))?
            .try_get::<i32>("", "n")?;
        assert_eq!(runs, 0, "a run goes with its environment");

        Migration.down(&manager).await?;
        Migration.up(&manager).await?;
        assert_eq!(audience("project_custom_domains").await?, "public");
        Ok(())
    }
}
