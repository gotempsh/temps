// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Project edge rules (ADR-089): ordered if/then rules the proxy evaluates
//! before forwarding a request.
//!
//! # Tables
//!
//! * `project_edge_rules` -- one row per rule. `expression` and `action` are
//!   the engine's JSON (`temps_edge_rules::{Group, Action}`); the service
//!   validates them before every write. `position` orders a project's rules
//!   (first match decides); the unique constraint is deferred so a reorder can
//!   swap positions inside one transaction. `revision` is the rule's own edit
//!   counter for optimistic concurrency, bumped by the service.
//! * `project_edge_rule_versions` -- the change feed. One row per project,
//!   holding the last value drawn from `project_edge_rule_versions_seq` when
//!   any of its rules changed. A trigger keeps it current for inserts,
//!   updates and deletes (including the cascade from a deleted project), so a
//!   manual edit is picked up too.
//! * `project_edge_rule_stats` -- match counters the proxies flush in batches.
//!   Kept apart from the rule row so a counter write never looks like an edit.
//!
//! # Change feed
//!
//! A proxy polls the sequence head (O(1)); only when it moved does it read
//! the versions above the one it last applied and reload just those
//! projects' rules (at most 100 per project). The trigger takes one
//! transaction-scoped advisory lock first, so writers are serialised until
//! they commit and versions become visible in order: a reader that has seen
//! version N has seen every version below it.

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
CREATE TABLE project_edge_rules (
    id SERIAL PRIMARY KEY,
    project_id INTEGER NOT NULL REFERENCES projects(id) ON DELETE CASCADE,
    -- NULL applies the rule to every environment of the project.
    environment_id INTEGER REFERENCES environments(id) ON DELETE CASCADE,
    -- 0-based evaluation order within the project.
    position INTEGER NOT NULL,
    name VARCHAR(80) NOT NULL,
    enabled BOOLEAN NOT NULL DEFAULT TRUE,
    mode VARCHAR(16) NOT NULL DEFAULT 'enforce',
    expression JSONB NOT NULL,
    action JSONB NOT NULL,
    -- Edit counter for optimistic concurrency (If-Match style).
    revision BIGINT NOT NULL DEFAULT 1,
    created_by INTEGER REFERENCES users(id) ON DELETE SET NULL,
    updated_by INTEGER REFERENCES users(id) ON DELETE SET NULL,
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    CONSTRAINT project_edge_rules_mode_valid CHECK (mode IN ('enforce', 'log_only')),
    CONSTRAINT project_edge_rules_position_non_negative CHECK (position >= 0),
    CONSTRAINT project_edge_rules_expression_is_object
        CHECK (jsonb_typeof(expression) = 'object'),
    CONSTRAINT project_edge_rules_action_is_object CHECK (jsonb_typeof(action) = 'object'),
    CONSTRAINT project_edge_rules_position_unique UNIQUE (project_id, position)
        DEFERRABLE INITIALLY DEFERRED
);

CREATE INDEX project_edge_rules_environment ON project_edge_rules (environment_id)
    WHERE environment_id IS NOT NULL;

CREATE SEQUENCE project_edge_rule_versions_seq;

-- No foreign key: a deleted project must still leave a version behind so
-- every proxy drops its rules.
CREATE TABLE project_edge_rule_versions (
    project_id INTEGER PRIMARY KEY,
    version BIGINT NOT NULL
);

CREATE INDEX project_edge_rule_versions_version ON project_edge_rule_versions (version);

CREATE FUNCTION project_edge_rules_bump_version() RETURNS trigger AS $$
DECLARE
    affected INTEGER;
BEGIN
    PERFORM pg_advisory_xact_lock(hashtext('project_edge_rule_versions_seq'));
    IF TG_OP = 'DELETE' THEN
        affected := OLD.project_id;
    ELSE
        affected := NEW.project_id;
    END IF;
    INSERT INTO project_edge_rule_versions (project_id, version)
        VALUES (affected, nextval('project_edge_rule_versions_seq'))
        ON CONFLICT (project_id) DO UPDATE SET version = EXCLUDED.version;
    -- A rule moved to another project (manual edit) changes both.
    IF TG_OP = 'UPDATE' AND OLD.project_id <> NEW.project_id THEN
        INSERT INTO project_edge_rule_versions (project_id, version)
            VALUES (OLD.project_id, nextval('project_edge_rule_versions_seq'))
            ON CONFLICT (project_id) DO UPDATE SET version = EXCLUDED.version;
    END IF;
    RETURN NULL;
END;
$$ LANGUAGE plpgsql;

CREATE TRIGGER project_edge_rules_bump_version
    AFTER INSERT OR UPDATE OR DELETE ON project_edge_rules
    FOR EACH ROW EXECUTE FUNCTION project_edge_rules_bump_version();

CREATE TABLE project_edge_rule_stats (
    rule_id INTEGER PRIMARY KEY REFERENCES project_edge_rules(id) ON DELETE CASCADE,
    hits BIGINT NOT NULL DEFAULT 0,
    last_hit_at TIMESTAMPTZ
);
"#,
            )
            .await?;
        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .get_connection()
            .execute_unprepared(
                r#"
DROP TABLE IF EXISTS project_edge_rule_stats;
DROP TABLE IF EXISTS project_edge_rules;
DROP FUNCTION IF EXISTS project_edge_rules_bump_version();
DROP TABLE IF EXISTS project_edge_rule_versions;
DROP SEQUENCE IF EXISTS project_edge_rule_versions_seq;
"#,
            )
            .await?;
        Ok(())
    }
}
