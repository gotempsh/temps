// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Shield reputation lists and the request log's verdict (ADR 066, phase 2).
//!
//! - `shield_sources`: one row per list the control plane fetches, named
//!   after the feeds `temps_shield::feeds` defines. `list` holds the last
//!   good copy, encoded as a `temps_shield::SourceList`; `enabled` starts at
//!   the feed's default.
//! - `shield_snapshots`: compiled revisions every ingress loads, newest
//!   last; the compiler keeps the last ten. `sources_updated_at` is the
//!   newest `shield_sources.updated_at` a revision was built from, which is
//!   how the compiler knows a list changed since.
//! - `proxy_logs.shield_verdict` and `shield_reasons`: what Shield would
//!   have done with the request and why. Nullable, so a request no list
//!   holds stores nothing.

use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

const UP: &str = r#"
CREATE TABLE IF NOT EXISTS shield_sources (
    name VARCHAR(64) PRIMARY KEY,
    enabled BOOLEAN NOT NULL,
    etags JSONB NOT NULL DEFAULT '{}'::jsonb,
    last_attempt_at TIMESTAMPTZ,
    last_fetched_at TIMESTAMPTZ,
    last_error TEXT,
    published_at TIMESTAMPTZ,
    entries INTEGER NOT NULL DEFAULT 0,
    sha256 VARCHAR(64),
    list BYTEA,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE TABLE IF NOT EXISTS shield_snapshots (
    revision BIGSERIAL PRIMARY KEY,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    sha256 VARCHAR(64) NOT NULL,
    body BYTEA NOT NULL,
    sources_updated_at TIMESTAMPTZ
);

ALTER TABLE proxy_logs ADD COLUMN IF NOT EXISTS shield_verdict TEXT;
ALTER TABLE proxy_logs ADD COLUMN IF NOT EXISTS shield_reasons JSONB;
"#;

const DOWN: &str = r#"
ALTER TABLE proxy_logs DROP COLUMN IF EXISTS shield_reasons;
ALTER TABLE proxy_logs DROP COLUMN IF EXISTS shield_verdict;
DROP TABLE IF EXISTS shield_snapshots;
DROP TABLE IF EXISTS shield_sources;
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

    /// Existing request logs gain empty verdicts, snapshot revisions count
    /// up from 1, and `down` then `up` again is clean.
    #[tokio::test]
    async fn shield_tables_and_log_columns() -> Result<(), Box<dyn std::error::Error>> {
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
        // Only the table this migration alters.
        db.execute_unprepared(
            "CREATE TABLE proxy_logs(id SERIAL PRIMARY KEY, path TEXT); \
             INSERT INTO proxy_logs(path) VALUES ('/');",
        )
        .await?;

        let manager = SchemaManager::new(&db);
        Migration.up(&manager).await?;

        let row = db
            .query_one(Statement::from_string(
                DatabaseBackend::Postgres,
                "SELECT (shield_verdict IS NULL AND shield_reasons IS NULL) AS empty \
                 FROM proxy_logs LIMIT 1"
                    .to_string(),
            ))
            .await?
            .ok_or_else(|| DbErr::RecordNotFound("proxy_logs".into()))?;
        assert!(row.try_get::<bool>("", "empty")?);

        let row = db
            .query_one(Statement::from_string(
                DatabaseBackend::Postgres,
                "INSERT INTO shield_snapshots(sha256, body) VALUES ('00', '\\x00') \
                 RETURNING revision"
                    .to_string(),
            ))
            .await?
            .ok_or_else(|| DbErr::RecordNotFound("shield_snapshots".into()))?;
        assert_eq!(row.try_get::<i64>("", "revision")?, 1);
        db.execute_unprepared("INSERT INTO shield_sources(name, enabled) VALUES ('feodo', true)")
            .await?;

        Migration.down(&manager).await?;
        Migration.up(&manager).await?;
        Migration.up(&manager).await?;
        Ok(())
    }
}
