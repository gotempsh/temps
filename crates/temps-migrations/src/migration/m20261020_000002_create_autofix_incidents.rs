// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! ADR-054: autofix incidents that run as chats in a workspace.
//!
//! - `project_autofix_settings`: one row per project that has touched autofix:
//!   mode, confidence threshold, protected paths, pause switch and the user
//!   unattended incidents act as. A separate table rather than columns on
//!   `projects`, so the project model and its many constructors stay as they
//!   are; a missing row means "off".
//! - `autofix_incidents`: one row per attempt to fix one error group. The
//!   partial unique index lets a group have at most one open incident. A
//!   closed incident still holding a sandbox is found through its own partial
//!   index, so the reconcile loop reads only leftovers.
//! - `autofix_incident_events`: the append-only timeline the server writes at
//!   every action.

use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .get_connection()
            .execute_unprepared(
                r#"
CREATE TABLE IF NOT EXISTS project_autofix_settings (
    project_id INTEGER PRIMARY KEY REFERENCES projects(id) ON DELETE CASCADE,
    mode VARCHAR(16) NOT NULL DEFAULT 'off',
    min_confidence SMALLINT NOT NULL DEFAULT 4,
    protected_paths JSONB NOT NULL DEFAULT '[]'::jsonb,
    test_command TEXT,
    paused BOOLEAN NOT NULL DEFAULT FALSE,
    owner_user_id INTEGER REFERENCES users(id) ON DELETE SET NULL,
    application_id BIGINT REFERENCES ai_applications(id) ON DELETE SET NULL,
    ai_provider VARCHAR(128),
    ai_model VARCHAR(255),
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    CONSTRAINT project_autofix_settings_mode_check
        CHECK (mode IN ('off', 'propose', 'auto_merge')),
    CONSTRAINT project_autofix_settings_confidence_check
        CHECK (min_confidence BETWEEN 0 AND 5)
);

CREATE TABLE IF NOT EXISTS autofix_incidents (
    id BIGSERIAL PRIMARY KEY,
    public_id VARCHAR(40) NOT NULL UNIQUE,
    project_id INTEGER NOT NULL REFERENCES projects(id) ON DELETE CASCADE,
    error_group_id INTEGER NOT NULL REFERENCES error_groups(id) ON DELETE CASCADE,
    environment_id INTEGER,
    deployment_id INTEGER,
    title TEXT NOT NULL,
    trigger VARCHAR(16) NOT NULL,
    status VARCHAR(24) NOT NULL,
    phase VARCHAR(16) NOT NULL DEFAULT 'queued',
    source_kind VARCHAR(24),
    delivery VARCHAR(24),
    owner_user_id INTEGER NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    application_id BIGINT REFERENCES ai_applications(id) ON DELETE SET NULL,
    conversation_id BIGINT REFERENCES ai_conversations(id) ON DELETE SET NULL,
    sandbox_id VARCHAR(64),
    base_revision VARCHAR(128),
    head_revision VARCHAR(128),
    branch VARCHAR(255),
    pr_url TEXT,
    pr_number INTEGER,
    patch TEXT,
    summary TEXT,
    confidence SMALLINT,
    evidence JSONB,
    failure_reason TEXT,
    decision VARCHAR(16),
    decided_by_user_id INTEGER REFERENCES users(id) ON DELETE SET NULL,
    decided_at TIMESTAMPTZ,
    attempts INTEGER NOT NULL DEFAULT 0,
    closed_at TIMESTAMPTZ,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    CONSTRAINT autofix_incidents_status_check
        CHECK (status IN ('open', 'working', 'awaiting_review',
                          'merged', 'applied', 'rejected', 'failed', 'stopped')),
    CONSTRAINT autofix_incidents_trigger_check
        CHECK (trigger IN ('manual', 'automatic')),
    CONSTRAINT autofix_incidents_confidence_check
        CHECK (confidence IS NULL OR confidence BETWEEN 0 AND 5)
);

CREATE UNIQUE INDEX IF NOT EXISTS uq_autofix_incidents_one_open_per_group
    ON autofix_incidents (error_group_id)
    WHERE status IN ('open', 'working', 'awaiting_review');

CREATE INDEX IF NOT EXISTS idx_autofix_incidents_project_created
    ON autofix_incidents (project_id, created_at DESC, id DESC);

CREATE INDEX IF NOT EXISTS idx_autofix_incidents_queue
    ON autofix_incidents (created_at, id)
    WHERE status IN ('open', 'working');

CREATE INDEX IF NOT EXISTS idx_autofix_incidents_closed_with_sandbox
    ON autofix_incidents (updated_at)
    WHERE sandbox_id IS NOT NULL
      AND status IN ('merged', 'applied', 'rejected', 'failed', 'stopped');

CREATE TABLE IF NOT EXISTS autofix_incident_events (
    id BIGSERIAL PRIMARY KEY,
    incident_id BIGINT NOT NULL REFERENCES autofix_incidents(id) ON DELETE CASCADE,
    actor VARCHAR(32) NOT NULL,
    kind VARCHAR(48) NOT NULL,
    summary TEXT NOT NULL,
    details JSONB,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE INDEX IF NOT EXISTS idx_autofix_incident_events_incident
    ON autofix_incident_events (incident_id, id);
"#,
            )
            .await?;
        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .get_connection()
            .execute_unprepared(
                "DROP TABLE IF EXISTS autofix_incident_events; \
                 DROP TABLE IF EXISTS autofix_incidents; \
                 DROP TABLE IF EXISTS project_autofix_settings;",
            )
            .await?;
        Ok(())
    }
}
