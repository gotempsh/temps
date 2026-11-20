// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Ordered releases per project, and "resolve in the next release" on an
//! error group (ADR 111, D1.5).
//!
//! `error_releases` holds one row per (project, release version) Temps has
//! seen. Its order is `released_at`: the creation time of the first Temps
//! deployment that carried the release (a git deployment's release is its
//! commit SHA, ADR 033), or the time the release was first seen when no
//! deployment matches (a release the SDK sets itself). `released_at` is
//! written once and never moves, so the order is stable.
//!
//! A group resolved "in the next release" records the newest release of its
//! project at that moment in `resolved_in_release_after_id`. Ingest keeps it
//! resolved for events from that release or older ones, and for events with
//! no release, and reopens it for an event from a newer release.
//!
//! The backfill registers the releases of the last 90 days of stored error
//! events (`data.environment.release`), bounded by time because
//! `error_events` is a hypertable.

use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        let db = manager.get_connection();
        db.execute_unprepared(
            "CREATE TABLE IF NOT EXISTS error_releases ( \
                 id SERIAL PRIMARY KEY, \
                 project_id INTEGER NOT NULL REFERENCES projects(id) ON DELETE CASCADE, \
                 version TEXT NOT NULL, \
                 released_at TIMESTAMPTZ NOT NULL, \
                 first_seen_at TIMESTAMPTZ NOT NULL, \
                 deployment_id INTEGER REFERENCES deployments(id) ON DELETE SET NULL, \
                 created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(), \
                 CONSTRAINT error_releases_project_version UNIQUE (project_id, version))",
        )
        .await?;
        db.execute_unprepared(
            "CREATE INDEX IF NOT EXISTS idx_error_releases_project_order \
             ON error_releases (project_id, released_at DESC, id DESC)",
        )
        .await?;
        db.execute_unprepared(
            "INSERT INTO error_releases (project_id, version, released_at, first_seen_at, deployment_id) \
             SELECT s.project_id, s.release, COALESCE(d.created_at, s.first_seen), s.first_seen, d.id \
             FROM ( \
                 SELECT project_id, data #>> '{environment,release}' AS release, \
                        MIN(timestamp) AS first_seen \
                 FROM error_events \
                 WHERE timestamp >= NOW() - INTERVAL '90 days' \
                   AND COALESCE(data #>> '{environment,release}', '') <> '' \
                   AND length(data #>> '{environment,release}') <= 250 \
                 GROUP BY 1, 2) s \
             LEFT JOIN LATERAL ( \
                 SELECT id, created_at FROM deployments \
                 WHERE project_id = s.project_id AND commit_sha = s.release \
                 ORDER BY created_at, id LIMIT 1) d ON TRUE \
             ON CONFLICT (project_id, version) DO NOTHING",
        )
        .await?;
        // After the backfill: inside the migration's transaction the
        // exclusive lock this takes on error_groups is held until commit, so
        // taking it last keeps ingest from waiting on the event scan.
        db.execute_unprepared(
            "ALTER TABLE error_groups \
             ADD COLUMN IF NOT EXISTS resolved_in_next_release BOOLEAN NOT NULL DEFAULT FALSE, \
             ADD COLUMN IF NOT EXISTS resolved_in_release_after_id INTEGER \
                 REFERENCES error_releases(id) ON DELETE SET NULL",
        )
        .await?;
        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        let db = manager.get_connection();
        db.execute_unprepared(
            "ALTER TABLE error_groups \
             DROP COLUMN IF EXISTS resolved_in_release_after_id, \
             DROP COLUMN IF EXISTS resolved_in_next_release",
        )
        .await?;
        db.execute_unprepared("DROP TABLE IF EXISTS error_releases")
            .await?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use sea_orm::{ConnectionTrait, Database, DatabaseBackend, Statement};
    use testcontainers::{core::WaitFor, runners::AsyncRunner, GenericImage, ImageExt};

    #[tokio::test]
    async fn existing_events_register_their_releases_and_groups_gain_the_columns(
    ) -> Result<(), Box<dyn std::error::Error>> {
        let Ok(docker) = bollard::Docker::connect_with_local_defaults() else {
            eprintln!("Docker unavailable; skipping error release backfill integration");
            return Ok(());
        };
        if docker.ping().await.is_err() {
            eprintln!("Docker unavailable; skipping error release backfill integration");
            return Ok(());
        }
        let container = GenericImage::new("postgres", "17-alpine")
            .with_wait_for(WaitFor::message_on_stderr(
                "database system is ready to accept connections",
            ))
            .with_env_var("POSTGRES_PASSWORD", "error-release-test")
            .start()
            .await?;
        let port = container.get_host_port_ipv4(5432).await?;
        let db = Database::connect(format!(
            "postgres://postgres:error-release-test@127.0.0.1:{port}/postgres"
        ))
        .await?;
        // Only the columns the migration reads. Project 1 has a release
        // deployed as a commit SHA, one set in the SDK, one too old and one
        // too long; project 2 has its own copy of the SDK release.
        let sha = "0123456789abcdef0123456789abcdef01234567";
        db.execute_unprepared(&format!(
            "CREATE TABLE projects(id INTEGER PRIMARY KEY); \
             CREATE TABLE deployments(id INTEGER PRIMARY KEY, project_id INTEGER, \
                 commit_sha TEXT, created_at TIMESTAMPTZ); \
             CREATE TABLE error_groups(id INTEGER PRIMARY KEY); \
             CREATE TABLE error_events(project_id INTEGER, timestamp TIMESTAMPTZ, data JSONB); \
             INSERT INTO projects VALUES (1), (2); \
             INSERT INTO deployments VALUES (7, 1, '{sha}', '2026-11-01T00:00:00Z'); \
             INSERT INTO error_groups VALUES (1); \
             INSERT INTO error_events VALUES \
               (1, NOW() - INTERVAL '2 days', '{{\"environment\":{{\"release\":\"{sha}\"}}}}'), \
               (1, NOW() - INTERVAL '1 day', '{{\"environment\":{{\"release\":\"synthetic@1\"}}}}'), \
               (1, NOW() - INTERVAL '3 days', '{{\"environment\":{{\"release\":\"synthetic@1\"}}}}'), \
               (1, NOW() - INTERVAL '100 days', '{{\"environment\":{{\"release\":\"synthetic@old\"}}}}'), \
               (1, NOW(), '{{\"environment\":{{\"release\":\"{long}\"}}}}'), \
               (1, NOW(), '{{}}'), \
               (2, NOW(), '{{\"environment\":{{\"release\":\"synthetic@1\"}}}}');",
            long = "r".repeat(251),
        ))
        .await?;

        Migration.up(&SchemaManager::new(&db)).await?;

        let rows = db
            .query_all(Statement::from_string(
                DatabaseBackend::Postgres,
                "SELECT project_id, version, deployment_id, \
                        released_at = '2026-11-01T00:00:00Z'::timestamptz AS deployed_order, \
                        first_seen_at < NOW() - INTERVAL '2 days' AS first_seen_kept \
                 FROM error_releases ORDER BY project_id, version",
            ))
            .await?;
        let found: Vec<(i32, String, Option<i32>, bool, bool)> = rows
            .iter()
            .map(|row| {
                Ok((
                    row.try_get("", "project_id")?,
                    row.try_get("", "version")?,
                    row.try_get("", "deployment_id")?,
                    row.try_get("", "deployed_order")?,
                    row.try_get("", "first_seen_kept")?,
                ))
            })
            .collect::<Result<_, DbErr>>()?;
        assert_eq!(
            found,
            vec![
                (1, sha.to_string(), Some(7), true, true),
                (1, "synthetic@1".to_string(), None, false, true),
                (2, "synthetic@1".to_string(), None, false, false),
            ]
        );
        db.execute_unprepared(
            "UPDATE error_groups SET resolved_in_next_release = TRUE, \
             resolved_in_release_after_id = (SELECT MIN(id) FROM error_releases)",
        )
        .await?;
        // Running it again changes nothing.
        Migration.up(&SchemaManager::new(&db)).await?;
        let count = db
            .query_one(Statement::from_string(
                DatabaseBackend::Postgres,
                "SELECT COUNT(*)::bigint AS n FROM error_releases",
            ))
            .await?
            .ok_or_else(|| DbErr::RecordNotFound("release count".into()))?
            .try_get::<i64>("", "n")?;
        assert_eq!(count, 3);
        Ok(())
    }
}
