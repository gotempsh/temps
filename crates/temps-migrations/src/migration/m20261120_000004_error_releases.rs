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
            "ALTER TABLE error_groups \
             ADD COLUMN IF NOT EXISTS resolved_in_next_release BOOLEAN NOT NULL DEFAULT FALSE, \
             ADD COLUMN IF NOT EXISTS resolved_in_release_after_id INTEGER \
                 REFERENCES error_releases(id) ON DELETE SET NULL",
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
