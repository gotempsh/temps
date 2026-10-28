// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! What fleet learning finds is a proposal (ADR 066, D9).
//!
//! A `shield_observed` row no longer applies by itself: it waits as
//! `pending` until an instance admin approves or rejects it, and only
//! approved rows go into a snapshot. Rows learned before this migration
//! become pending too, so nothing learned keeps applying unreviewed.

use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

const UP: &str = r#"
ALTER TABLE shield_observed
    ADD COLUMN IF NOT EXISTS status VARCHAR(16) NOT NULL DEFAULT 'pending',
    ADD COLUMN IF NOT EXISTS decided_at TIMESTAMPTZ,
    ADD COLUMN IF NOT EXISTS decided_by INTEGER REFERENCES users(id) ON DELETE SET NULL;

DO $$ BEGIN
    ALTER TABLE shield_observed ADD CONSTRAINT shield_observed_status_check
        CHECK (status IN ('pending', 'approved', 'rejected'));
EXCEPTION WHEN duplicate_object THEN NULL;
END $$;

CREATE INDEX IF NOT EXISTS shield_observed_status ON shield_observed (status, last_seen DESC);
"#;

const DOWN: &str = r#"
DROP INDEX IF EXISTS shield_observed_status;
ALTER TABLE shield_observed
    DROP CONSTRAINT IF EXISTS shield_observed_status_check,
    DROP COLUMN IF EXISTS decided_by,
    DROP COLUMN IF EXISTS decided_at,
    DROP COLUMN IF EXISTS status;
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

    /// The learned row's status, and whether it has no decision.
    async fn proposal(db: &sea_orm::DatabaseConnection) -> Result<(String, bool), DbErr> {
        let row = db
            .query_one(Statement::from_string(
                DatabaseBackend::Postgres,
                "SELECT status, decided_at IS NULL AS undecided FROM shield_observed \
                 WHERE subject = '203.0.113.7'",
            ))
            .await?
            .ok_or_else(|| DbErr::RecordNotFound("shield_observed".into()))?;
        Ok((row.try_get("", "status")?, row.try_get("", "undecided")?))
    }

    /// Rows learned before the migration wait for review instead of
    /// applying, an unknown status is refused, and `down` then `up` again
    /// is clean.
    #[tokio::test]
    async fn learned_rows_become_pending_proposals() -> Result<(), Box<dyn std::error::Error>> {
        let Ok(docker) = bollard::Docker::connect_with_local_defaults() else {
            eprintln!("Docker unavailable; skipping Shield proposals migration integration");
            return Ok(());
        };
        if docker.ping().await.is_err() {
            eprintln!("Docker unavailable; skipping Shield proposals migration integration");
            return Ok(());
        }
        let container = GenericImage::new("postgres", "17-alpine")
            .with_wait_for(WaitFor::message_on_stderr(
                "database system is ready to accept connections",
            ))
            .with_env_var("POSTGRES_PASSWORD", "shield-proposals-test")
            .start()
            .await?;
        let port = container.get_host_port_ipv4(5432).await?;
        let db = Database::connect(format!(
            "postgres://postgres:shield-proposals-test@127.0.0.1:{port}/postgres"
        ))
        .await?;
        // The table as the previous migration left it, with a learned row.
        db.execute_unprepared(
            "CREATE TABLE users(id SERIAL PRIMARY KEY); \
             CREATE TABLE shield_observed ( \
                 id BIGSERIAL PRIMARY KEY, kind VARCHAR(8) NOT NULL, \
                 subject VARCHAR(64) NOT NULL, verdict VARCHAR(16) NOT NULL, \
                 reason TEXT NOT NULL, first_seen TIMESTAMPTZ NOT NULL, \
                 last_seen TIMESTAMPTZ NOT NULL, hits BIGINT NOT NULL DEFAULT 0, \
                 expires_at TIMESTAMPTZ NOT NULL, UNIQUE (kind, subject)); \
             INSERT INTO shield_observed (kind, subject, verdict, reason, first_seen, \
                 last_seen, hits, expires_at) \
             VALUES ('ip', '203.0.113.7', 'block', 'exploit CVE-2021-41773', now(), now(), 1, \
                 now() + interval '7 days');",
        )
        .await?;

        let manager = SchemaManager::new(&db);
        Migration.up(&manager).await?;
        assert_eq!(proposal(&db).await?, ("pending".to_string(), true));
        assert!(
            db.execute_unprepared("UPDATE shield_observed SET status = 'applied'")
                .await
                .is_err(),
            "only pending, approved and rejected are stored"
        );

        Migration.down(&manager).await?;
        Migration.up(&manager).await?;
        assert_eq!(proposal(&db).await?, ("pending".to_string(), true));
        Ok(())
    }
}
