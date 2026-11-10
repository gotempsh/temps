// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Runtime incarnations and durable delivery intent. Legacy grant-intent rows
//! are deliberately untouched: an upgrade does not activate old consent.
//! Outbox rows have no runtime/node FK so retirement survives parent deletion.
//! Payloads contain no credentials; the dispatcher compiles fresh policy from
//! authorized database state. Downgrade refuses to discard pending work.
use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager.get_connection().execute_unprepared(r#"
CREATE TABLE IF NOT EXISTS sandbox_access_runtimes (
    id UUID PRIMARY KEY,
    project_id INTEGER NOT NULL REFERENCES projects(id) ON DELETE CASCADE,
    sandbox_id INTEGER NOT NULL UNIQUE REFERENCES sandboxes(id) ON DELETE CASCADE,
    node_id INTEGER REFERENCES nodes(id) ON DELETE RESTRICT,
    provider_identity TEXT NOT NULL CHECK (length(provider_identity) BETWEEN 1 AND 512),
    desired_version BIGINT NOT NULL DEFAULT 0 CHECK (desired_version >= 0),
    applied_version BIGINT NOT NULL DEFAULT 0 CHECK (applied_version >= 0 AND applied_version <= desired_version),
    closed_at TIMESTAMPTZ,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now()
);
CREATE INDEX IF NOT EXISTS sandbox_access_runtimes_project ON sandbox_access_runtimes (project_id, created_at DESC, id DESC);
CREATE TABLE IF NOT EXISTS sandbox_access_delivery (
    id BIGSERIAL PRIMARY KEY,
    runtime_id UUID NOT NULL,
    node_id INTEGER,
    desired_version BIGINT NOT NULL CHECK (desired_version >= 0),
    operation TEXT NOT NULL CHECK (operation IN ('reconcile', 'retire')),
    attempts INTEGER NOT NULL DEFAULT 0 CHECK (attempts >= 0),
    available_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    claim_id UUID,
    claimed_until TIMESTAMPTZ,
    completed_at TIMESTAMPTZ,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    UNIQUE (runtime_id, desired_version, operation),
    CHECK ((claim_id IS NULL) = (claimed_until IS NULL))
);
CREATE INDEX IF NOT EXISTS sandbox_access_delivery_pending ON sandbox_access_delivery (available_at, id) WHERE completed_at IS NULL;
CREATE OR REPLACE FUNCTION sandbox_access_enqueue_retirement() RETURNS trigger AS $$
BEGIN
    INSERT INTO sandbox_access_delivery (runtime_id,node_id,desired_version,operation)
    VALUES (OLD.id,OLD.node_id,OLD.desired_version,'retire')
    ON CONFLICT (runtime_id,desired_version,operation) DO NOTHING;
    RETURN OLD;
END;
$$ LANGUAGE plpgsql;
DROP TRIGGER IF EXISTS sandbox_access_runtime_retirement ON sandbox_access_runtimes;
CREATE TRIGGER sandbox_access_runtime_retirement BEFORE DELETE ON sandbox_access_runtimes
    FOR EACH ROW EXECUTE FUNCTION sandbox_access_enqueue_retirement();
"#).await?;
        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager.get_connection().execute_unprepared(r#"
DO $$ BEGIN
    IF to_regclass('sandbox_access_runtimes') IS NOT NULL THEN
        IF EXISTS (SELECT 1 FROM sandbox_access_runtimes) THEN
            RAISE EXCEPTION 'Cannot downgrade sandbox access delivery while runtime incarnations exist';
        END IF;
    END IF;
    IF to_regclass('sandbox_access_delivery') IS NOT NULL THEN
        IF EXISTS (SELECT 1 FROM sandbox_access_delivery WHERE completed_at IS NULL) THEN
            RAISE EXCEPTION 'Cannot downgrade sandbox access delivery while worker commands are pending';
        END IF;
    END IF;
END $$;
DROP TABLE IF EXISTS sandbox_access_runtimes;
DROP FUNCTION IF EXISTS sandbox_access_enqueue_retirement();
DROP TABLE IF EXISTS sandbox_access_delivery;
"#).await?;
        Ok(())
    }
}
