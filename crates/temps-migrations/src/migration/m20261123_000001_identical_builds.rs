// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Coordinating identical builds (ADR 113).
//!
//! `projects.identical_build_mode` and `projects.identical_build_scope` say
//! what a project's builds do when an identical build is already running.
//! The defaults (`parallel`, `fingerprint`) are exactly today's behaviour, so
//! nothing changes for an existing project on upgrade; both are constant
//! defaults, a catalog-only change with no table rewrite, and CHECK
//! constraints keep anything else out.
//!
//! `identical_builds` is the durable rendezvous: one row per coordinated
//! build, so coordination survives a restart and works across processes.
//! The partial unique index on `queue_key` where `state = 'building'` is what
//! lets exactly one build of a group run; the others wait. Rows are small
//! (keyed hashes, no build inputs) and are pruned after seven days by the
//! coordinator.
//!
//! The previous binary reads neither the columns nor the table, and its
//! inserts take the defaults, so a rollback keeps working with this schema in
//! place. `down` drops the table and the columns: every project goes back to
//! building in parallel, and only the coordination history is lost.

use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .get_connection()
            .execute_unprepared(
                "ALTER TABLE projects
                    ADD COLUMN IF NOT EXISTS identical_build_mode text NOT NULL DEFAULT 'parallel';
                 ALTER TABLE projects
                    ADD COLUMN IF NOT EXISTS identical_build_scope text NOT NULL DEFAULT 'fingerprint';
                 ALTER TABLE projects
                    DROP CONSTRAINT IF EXISTS projects_identical_build_mode_check;
                 ALTER TABLE projects
                    ADD CONSTRAINT projects_identical_build_mode_check
                    CHECK (identical_build_mode IN ('parallel', 'queue', 'share'));
                 ALTER TABLE projects
                    DROP CONSTRAINT IF EXISTS projects_identical_build_scope_check;
                 ALTER TABLE projects
                    ADD CONSTRAINT projects_identical_build_scope_check
                    CHECK (identical_build_scope IN ('fingerprint', 'source'));

                 CREATE TABLE IF NOT EXISTS identical_builds (
                    id serial PRIMARY KEY,
                    deployment_id integer NOT NULL
                        REFERENCES deployments(id) ON DELETE CASCADE,
                    project_id integer NOT NULL
                        REFERENCES projects(id) ON DELETE CASCADE,
                    job_id text NOT NULL,
                    mode text NOT NULL CHECK (mode IN ('queue', 'share')),
                    queue_key text NOT NULL,
                    fingerprint text NOT NULL,
                    access_boundary text NOT NULL,
                    state text NOT NULL CHECK (state IN (
                        'waiting', 'building', 'succeeded', 'failed',
                        'cancelled', 'reused', 'timed_out', 'abandoned'
                    )),
                    source_build_id integer
                        REFERENCES identical_builds(id) ON DELETE SET NULL,
                    image jsonb,
                    repository text NOT NULL,
                    commit_sha text NOT NULL,
                    detail text,
                    heartbeat_at timestamptz NOT NULL DEFAULT now(),
                    created_at timestamptz NOT NULL DEFAULT now(),
                    updated_at timestamptz NOT NULL DEFAULT now(),
                    finished_at timestamptz
                 );
                 CREATE UNIQUE INDEX IF NOT EXISTS identical_builds_deployment_job
                    ON identical_builds (deployment_id, job_id);
                 CREATE UNIQUE INDEX IF NOT EXISTS identical_builds_one_builder_per_group
                    ON identical_builds (queue_key) WHERE state = 'building';
                 CREATE INDEX IF NOT EXISTS identical_builds_waiting
                    ON identical_builds (queue_key, id) WHERE state = 'waiting';
                 CREATE INDEX IF NOT EXISTS identical_builds_reusable
                    ON identical_builds (fingerprint, finished_at DESC)
                    WHERE state = 'succeeded' AND mode = 'share';
                 CREATE INDEX IF NOT EXISTS identical_builds_project_created
                    ON identical_builds (project_id, created_at DESC);
                 CREATE INDEX IF NOT EXISTS identical_builds_source_build
                    ON identical_builds (source_build_id) WHERE source_build_id IS NOT NULL;",
            )
            .await?;
        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        // Dropping the columns puts every project back on parallel builds,
        // the safe direction; the table only holds coordination history.
        manager
            .get_connection()
            .execute_unprepared(
                "DROP TABLE IF EXISTS identical_builds;
                 ALTER TABLE projects
                    DROP CONSTRAINT IF EXISTS projects_identical_build_scope_check;
                 ALTER TABLE projects
                    DROP CONSTRAINT IF EXISTS projects_identical_build_mode_check;
                 ALTER TABLE projects DROP COLUMN IF EXISTS identical_build_scope;
                 ALTER TABLE projects DROP COLUMN IF EXISTS identical_build_mode;",
            )
            .await?;
        Ok(())
    }
}
