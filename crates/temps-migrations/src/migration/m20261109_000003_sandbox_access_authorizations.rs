// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Fresh authorization is separate from legacy saved intentions. No rows are
//! backfilled into the authorization table. Host identity is copied into the
//! deletion outbox so retirement needs no surviving sandbox/project row.
use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;
#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager.get_connection().execute_unprepared(r#"
ALTER TABLE sandbox_access_authorities ADD COLUMN IF NOT EXISTS desired_revision BIGINT NOT NULL DEFAULT 0 CHECK (desired_revision >= 0);
ALTER TABLE sandbox_access_runtimes ADD COLUMN IF NOT EXISTS authority_id UUID REFERENCES sandbox_access_authorities(id) ON DELETE RESTRICT;
ALTER TABLE sandbox_access_runtimes ADD COLUMN IF NOT EXISTS provider_backend TEXT;
ALTER TABLE sandbox_access_runtimes ADD COLUMN IF NOT EXISTS provider_name TEXT;
ALTER TABLE sandbox_access_runtimes ADD COLUMN IF NOT EXISTS lease_expires_at TIMESTAMPTZ;
ALTER TABLE sandbox_access_delivery ADD COLUMN IF NOT EXISTS worker_boot_id UUID;
ALTER TABLE sandbox_access_delivery ADD COLUMN IF NOT EXISTS provider_identity TEXT;
ALTER TABLE sandbox_access_delivery ADD COLUMN IF NOT EXISTS provider_backend TEXT;
ALTER TABLE sandbox_access_delivery ADD COLUMN IF NOT EXISTS provider_name TEXT;
DO $$ BEGIN
    IF NOT EXISTS (SELECT 1 FROM pg_constraint WHERE conrelid='sandbox_access_runtimes'::regclass AND conname='sandbox_access_runtime_target') THEN
        ALTER TABLE sandbox_access_runtimes ADD CONSTRAINT sandbox_access_runtime_target CHECK (
            (authority_id IS NULL AND provider_backend IS NULL AND provider_name IS NULL AND lease_expires_at IS NULL) OR
            (authority_id IS NOT NULL AND provider_backend IS NOT NULL AND provider_backend IN ('docker','firecracker','microsandbox') AND provider_name IS NOT NULL AND length(provider_name) BETWEEN 1 AND 255));
    END IF;
    IF NOT EXISTS (SELECT 1 FROM pg_constraint WHERE conrelid='sandbox_access_delivery'::regclass AND conname='sandbox_access_delivery_target') THEN
        ALTER TABLE sandbox_access_delivery ADD CONSTRAINT sandbox_access_delivery_target CHECK (
            (worker_boot_id IS NULL AND provider_identity IS NULL AND provider_backend IS NULL AND provider_name IS NULL) OR
            (worker_boot_id IS NOT NULL AND provider_identity IS NOT NULL AND length(provider_identity) BETWEEN 1 AND 512 AND provider_backend IS NOT NULL AND provider_backend IN ('docker','firecracker','microsandbox') AND provider_name IS NOT NULL AND length(provider_name) BETWEEN 1 AND 255));
    END IF;
END $$;
ALTER TABLE sandbox_access_runtimes DROP CONSTRAINT IF EXISTS sandbox_access_runtimes_sandbox_id_key;
CREATE UNIQUE INDEX IF NOT EXISTS sandbox_access_runtime_current ON sandbox_access_runtimes(sandbox_id) WHERE closed_at IS NULL;
CREATE UNIQUE INDEX IF NOT EXISTS sandbox_access_runtime_scope ON sandbox_access_runtimes(id,project_id,sandbox_id);
CREATE UNIQUE INDEX IF NOT EXISTS sandbox_service_grant_scope ON sandbox_service_grants(id,project_id,sandbox_id);
CREATE INDEX IF NOT EXISTS sandbox_access_runtime_authority ON sandbox_access_runtimes(authority_id) WHERE authority_id IS NOT NULL;
CREATE TABLE IF NOT EXISTS sandbox_access_authorizations (
    grant_id TEXT PRIMARY KEY,
    request_id UUID NOT NULL UNIQUE,
    requested_ttl INTEGER NOT NULL CHECK (requested_ttl BETWEEN 60 AND 86400),
    runtime_id UUID NOT NULL,
    project_id INTEGER NOT NULL,
    sandbox_id INTEGER NOT NULL,
    approved_by INTEGER REFERENCES users(id) ON DELETE SET NULL,
    approved_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    FOREIGN KEY (grant_id,project_id,sandbox_id) REFERENCES sandbox_service_grants(id,project_id,sandbox_id) ON DELETE CASCADE,
    FOREIGN KEY (runtime_id,project_id,sandbox_id) REFERENCES sandbox_access_runtimes(id,project_id,sandbox_id) ON DELETE CASCADE
);
CREATE INDEX IF NOT EXISTS sandbox_access_authorizations_runtime ON sandbox_access_authorizations(runtime_id,approved_at DESC,grant_id DESC);
CREATE OR REPLACE FUNCTION sandbox_access_enqueue_retirement() RETURNS trigger AS $$
BEGIN
    INSERT INTO sandbox_access_delivery (runtime_id,node_id,desired_version,operation,worker_boot_id,provider_identity,provider_backend,provider_name)
    VALUES (OLD.id,OLD.node_id,OLD.desired_version,'retire',OLD.authority_id,
        CASE WHEN OLD.authority_id IS NOT NULL THEN OLD.provider_identity ELSE NULL END,OLD.provider_backend,OLD.provider_name)
    ON CONFLICT (runtime_id,desired_version,operation) DO NOTHING;
    IF OLD.authority_id IS NOT NULL THEN
        UPDATE sandbox_access_authorities SET desired_revision=desired_revision+1,updated_at=now() WHERE id=OLD.authority_id;
    END IF;
    RETURN OLD;
END;
$$ LANGUAGE plpgsql;
CREATE OR REPLACE FUNCTION sandbox_access_runtime_closed() RETURNS trigger AS $$
BEGIN
    IF OLD.closed_at IS NULL AND NEW.closed_at IS NOT NULL AND OLD.authority_id IS NOT NULL THEN
        UPDATE sandbox_access_authorities SET desired_revision=desired_revision+1,updated_at=now() WHERE id=OLD.authority_id;
    END IF;
    RETURN NULL;
END;
$$ LANGUAGE plpgsql;
DROP TRIGGER IF EXISTS sandbox_access_runtime_close_revision ON sandbox_access_runtimes;
CREATE TRIGGER sandbox_access_runtime_close_revision AFTER UPDATE OF closed_at ON sandbox_access_runtimes
    FOR EACH ROW EXECUTE FUNCTION sandbox_access_runtime_closed();
CREATE OR REPLACE FUNCTION sandbox_access_authorization_changed() RETURNS trigger AS $$
DECLARE affected_runtime UUID;
BEGIN
    IF TG_OP='UPDATE' AND NOT (
        OLD.approved_by IS NOT NULL AND NEW.approved_by IS NULL AND
        ROW(NEW.grant_id,NEW.request_id,NEW.requested_ttl,NEW.runtime_id,NEW.project_id,NEW.sandbox_id,NEW.approved_at)
            IS NOT DISTINCT FROM ROW(OLD.grant_id,OLD.request_id,OLD.requested_ttl,OLD.runtime_id,OLD.project_id,OLD.sandbox_id,OLD.approved_at)
    ) THEN RAISE EXCEPTION 'Sandbox access authorization is immutable; revoke and approve a new grant'; END IF;
    IF TG_OP='DELETE' THEN affected_runtime=OLD.runtime_id; ELSE affected_runtime=NEW.runtime_id; END IF;
    UPDATE sandbox_access_authorities SET desired_revision=desired_revision+1,updated_at=now()
        WHERE id=(SELECT authority_id FROM sandbox_access_runtimes WHERE id=affected_runtime);
    RETURN NULL;
END;
$$ LANGUAGE plpgsql;
DROP TRIGGER IF EXISTS sandbox_access_authorization_revision ON sandbox_access_authorizations;
CREATE TRIGGER sandbox_access_authorization_revision AFTER INSERT OR UPDATE OR DELETE ON sandbox_access_authorizations
    FOR EACH ROW EXECUTE FUNCTION sandbox_access_authorization_changed();
CREATE OR REPLACE FUNCTION sandbox_access_grant_changed() RETURNS trigger AS $$
BEGIN
    UPDATE sandbox_access_authorities SET desired_revision=desired_revision+1,updated_at=now()
        WHERE id IN (SELECT r.authority_id FROM sandbox_access_authorizations a JOIN sandbox_access_runtimes r ON r.id=a.runtime_id WHERE a.grant_id=OLD.id);
    RETURN NULL;
END;
$$ LANGUAGE plpgsql;
DROP TRIGGER IF EXISTS sandbox_access_grant_revision ON sandbox_service_grants;
CREATE TRIGGER sandbox_access_grant_revision AFTER UPDATE ON sandbox_service_grants
    FOR EACH ROW EXECUTE FUNCTION sandbox_access_grant_changed();
CREATE OR REPLACE FUNCTION sandbox_access_service_changed() RETURNS trigger AS $$
BEGIN
    UPDATE sandbox_access_authorities SET desired_revision=desired_revision+1,updated_at=now()
        WHERE id IN (SELECT r.authority_id FROM sandbox_access_authorizations a
            JOIN sandbox_access_runtimes r ON r.id=a.runtime_id
            JOIN sandbox_service_grants g ON g.id=a.grant_id
            WHERE g.service_id=OLD.id AND g.expires_at>now() AND g.revoked_at IS NULL);
    RETURN NULL;
END;
$$ LANGUAGE plpgsql;
DROP TRIGGER IF EXISTS sandbox_access_service_revision ON sandbox_access_services;
CREATE TRIGGER sandbox_access_service_revision AFTER UPDATE ON sandbox_access_services
    FOR EACH ROW EXECUTE FUNCTION sandbox_access_service_changed();
"#).await?;
        Ok(())
    }
    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager.get_connection().execute_unprepared(r#"
DO $$ BEGIN
    IF to_regclass('sandbox_access_authorizations') IS NOT NULL THEN
        IF EXISTS (SELECT 1 FROM sandbox_access_authorizations) THEN RAISE EXCEPTION 'Cannot downgrade live sandbox authorizations'; END IF;
    END IF;
    IF EXISTS (SELECT 1 FROM pg_attribute WHERE attrelid=to_regclass('sandbox_access_runtimes') AND attname='authority_id' AND NOT attisdropped) THEN
        IF EXISTS (SELECT 1 FROM sandbox_access_runtimes WHERE authority_id IS NOT NULL) THEN RAISE EXCEPTION 'Cannot downgrade registered sandbox bindings'; END IF;
    END IF;
    IF EXISTS (SELECT 1 FROM pg_attribute WHERE attrelid=to_regclass('sandbox_access_delivery') AND attname='worker_boot_id' AND NOT attisdropped) THEN
        IF EXISTS (SELECT 1 FROM sandbox_access_delivery WHERE worker_boot_id IS NOT NULL AND completed_at IS NULL) THEN RAISE EXCEPTION 'Cannot downgrade pending worker retirements'; END IF;
    END IF;
END $$;
DROP TRIGGER IF EXISTS sandbox_access_grant_revision ON sandbox_service_grants;
DROP TRIGGER IF EXISTS sandbox_access_service_revision ON sandbox_access_services;
DROP TRIGGER IF EXISTS sandbox_access_runtime_close_revision ON sandbox_access_runtimes;
DROP FUNCTION IF EXISTS sandbox_access_runtime_closed();
DROP TABLE IF EXISTS sandbox_access_authorizations;
DROP FUNCTION IF EXISTS sandbox_access_authorization_changed();
DROP FUNCTION IF EXISTS sandbox_access_grant_changed();
DROP FUNCTION IF EXISTS sandbox_access_service_changed();
DROP INDEX IF EXISTS sandbox_access_runtime_current;
DO $$ BEGIN
    IF to_regclass('sandbox_access_runtimes') IS NOT NULL AND NOT EXISTS (SELECT 1 FROM pg_constraint WHERE conrelid=to_regclass('sandbox_access_runtimes') AND conname='sandbox_access_runtimes_sandbox_id_key') THEN
        ALTER TABLE sandbox_access_runtimes ADD CONSTRAINT sandbox_access_runtimes_sandbox_id_key UNIQUE(sandbox_id);
    END IF;
END $$;
DROP INDEX IF EXISTS sandbox_access_runtime_scope;
DROP INDEX IF EXISTS sandbox_service_grant_scope;
DROP INDEX IF EXISTS sandbox_access_runtime_authority;
ALTER TABLE IF EXISTS sandbox_access_delivery DROP CONSTRAINT IF EXISTS sandbox_access_delivery_target;
ALTER TABLE IF EXISTS sandbox_access_runtimes DROP CONSTRAINT IF EXISTS sandbox_access_runtime_target;
ALTER TABLE IF EXISTS sandbox_access_delivery DROP COLUMN IF EXISTS worker_boot_id, DROP COLUMN IF EXISTS provider_identity, DROP COLUMN IF EXISTS provider_backend, DROP COLUMN IF EXISTS provider_name;
ALTER TABLE IF EXISTS sandbox_access_runtimes DROP COLUMN IF EXISTS authority_id, DROP COLUMN IF EXISTS provider_backend, DROP COLUMN IF EXISTS provider_name, DROP COLUMN IF EXISTS lease_expires_at;
ALTER TABLE IF EXISTS sandbox_access_authorities DROP COLUMN IF EXISTS desired_revision;
CREATE OR REPLACE FUNCTION sandbox_access_enqueue_retirement() RETURNS trigger AS $$
BEGIN
    INSERT INTO sandbox_access_delivery (runtime_id,node_id,desired_version,operation)
    VALUES (OLD.id,OLD.node_id,OLD.desired_version,'retire') ON CONFLICT (runtime_id,desired_version,operation) DO NOTHING;
    RETURN OLD;
END;
$$ LANGUAGE plpgsql;
"#).await?;
        Ok(())
    }
}
