// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Per-project / per-environment client-IP allow/deny lists.
//!
//! One row is one scope's policy: `environment_id IS NULL` is the project-wide
//! default, a non-NULL `environment_id` overrides it for that environment
//! (including `mode = 'disabled'`, which exempts the environment from a
//! project-wide list). The proxy loads every row into an in-memory snapshot,
//! so the table is only ever read off the request path.
//!
//! # Change feed
//!
//! Proxies must notice a write within seconds without rescanning the table.
//! Every insert/update draws a value from `project_ip_access_rules_revision_seq`
//! into the row's `revision` (by trigger, so a manual edit is covered too), and
//! every delete draws one into a short-lived tombstone. A proxy polls the
//! sequence head (O(1)) and, only when it moved, reads the rows and tombstones
//! above the revision it last applied. Tombstones are pruned after an hour by
//! the delete trigger itself; a proxy that was away for half an hour does a
//! full reload anyway.
//!
//! Both triggers first take one transaction-scoped advisory lock, so writers
//! to this table are serialised until they commit. That makes revisions
//! visible in commit order: a reader that has seen revision N has seen every
//! revision below it, which is what lets it advance by the revisions it read
//! rather than rescanning the table to be safe. Writes here are rare operator
//! actions, so the serialisation costs nothing.

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
CREATE SEQUENCE project_ip_access_rules_revision_seq;

CREATE TABLE project_ip_access_rules (
    id SERIAL PRIMARY KEY,
    project_id INTEGER NOT NULL REFERENCES projects(id) ON DELETE CASCADE,
    -- NULL = the project-wide policy; set = override for one environment.
    environment_id INTEGER REFERENCES environments(id) ON DELETE CASCADE,
    -- 'disabled' keeps the entries but enforces nothing.
    mode VARCHAR(16) NOT NULL DEFAULT 'disabled',
    -- JSON array of {"cidr": "10.0.0.0/8", "description": "Office"}; the
    -- service validates, canonicalises and bounds it before every write.
    cidrs JSONB NOT NULL DEFAULT '[]'::jsonb,
    updated_by INTEGER REFERENCES users(id) ON DELETE SET NULL,
    -- Change-feed position; assigned by trigger on every insert and update.
    revision BIGINT NOT NULL DEFAULT 0,
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    CONSTRAINT project_ip_access_rules_mode_valid
        CHECK (mode IN ('disabled', 'allow', 'deny')),
    CONSTRAINT project_ip_access_rules_cidrs_is_array
        CHECK (jsonb_typeof(cidrs) = 'array')
);

-- One policy per scope. NULL environment ids never collide in a plain UNIQUE
-- constraint, so the project-wide row is keyed through COALESCE (environment
-- ids are SERIAL, so 0 is never a real environment).
CREATE UNIQUE INDEX project_ip_access_rules_scope_unique
    ON project_ip_access_rules (project_id, COALESCE(environment_id, 0));

-- Delta reads: rows changed since a proxy's last applied revision.
CREATE INDEX project_ip_access_rules_revision
    ON project_ip_access_rules (revision);

-- A deleted rule leaves a scope behind so a proxy can drop it without
-- listing every surviving rule. No foreign keys: the referenced rows are gone
-- by definition (and project/environment deletes cascade through here).
CREATE TABLE project_ip_access_rule_tombstones (
    revision BIGINT PRIMARY KEY,
    rule_id INTEGER NOT NULL,
    project_id INTEGER NOT NULL,
    environment_id INTEGER,
    deleted_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

CREATE INDEX project_ip_access_rule_tombstones_deleted_at
    ON project_ip_access_rule_tombstones (deleted_at);

CREATE FUNCTION project_ip_access_rules_assign_revision() RETURNS trigger AS $$
BEGIN
    PERFORM pg_advisory_xact_lock(hashtext('project_ip_access_rules_revision_seq'));
    NEW.revision := nextval('project_ip_access_rules_revision_seq');
    RETURN NEW;
END;
$$ LANGUAGE plpgsql;

CREATE TRIGGER project_ip_access_rules_assign_revision
    BEFORE INSERT OR UPDATE ON project_ip_access_rules
    FOR EACH ROW EXECUTE FUNCTION project_ip_access_rules_assign_revision();

CREATE FUNCTION project_ip_access_rules_record_delete() RETURNS trigger AS $$
BEGIN
    PERFORM pg_advisory_xact_lock(hashtext('project_ip_access_rules_revision_seq'));
    DELETE FROM project_ip_access_rule_tombstones
        WHERE deleted_at < NOW() - INTERVAL '1 hour';
    INSERT INTO project_ip_access_rule_tombstones
        (revision, rule_id, project_id, environment_id)
        VALUES (nextval('project_ip_access_rules_revision_seq'),
                OLD.id, OLD.project_id, OLD.environment_id);
    RETURN OLD;
END;
$$ LANGUAGE plpgsql;

CREATE TRIGGER project_ip_access_rules_record_delete
    AFTER DELETE ON project_ip_access_rules
    FOR EACH ROW EXECUTE FUNCTION project_ip_access_rules_record_delete();
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
DROP TABLE IF EXISTS project_ip_access_rules;
DROP TABLE IF EXISTS project_ip_access_rule_tombstones;
DROP FUNCTION IF EXISTS project_ip_access_rules_assign_revision();
DROP FUNCTION IF EXISTS project_ip_access_rules_record_delete();
DROP SEQUENCE IF EXISTS project_ip_access_rules_revision_seq;
"#,
            )
            .await?;
        Ok(())
    }
}
