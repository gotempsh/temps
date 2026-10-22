// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! ADR-055 Phase 8: ingress endpoints and access policies.
//!
//! - `environments.ingress_mode` (`legacy` | `shadow` | `enforce`, default
//!   `legacy`): which evaluator decides the environment's requests. Every
//!   existing environment stays on the legacy chain.
//! - `sandboxes.preview_access`: existing sandboxes are `legacy` (unchanged
//!   behaviour); the column default then becomes `temps_login`, so only
//!   sandboxes created from now on require a Temps login when they have no
//!   preview password.
//! - `ingress_endpoints`: one per environment, backfilled here and created
//!   by trigger for every later environment. Policies compiled from the
//!   environment's existing access settings are not stored (they keep one
//!   source of truth); `ingress_access_policies` holds the ones added on top.
//! - `ingress_login_grants`: signed `TempsLogin` grants, kept so they can be
//!   revoked before they expire.
//!
//! Change feed: the proxy rebuilds its compiled endpoints on
//! `route_table_changes`, so the endpoint and policy tables, IP access rule
//! writes and grant revocations notify it. Grant inserts deliberately do not
//! (one happens per login).

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
ALTER TABLE environments
    ADD COLUMN ingress_mode TEXT NOT NULL DEFAULT 'legacy',
    ADD CONSTRAINT environments_ingress_mode_valid
        CHECK (ingress_mode IN ('legacy', 'shadow', 'enforce'));

ALTER TABLE sandboxes
    ADD COLUMN preview_access TEXT NOT NULL DEFAULT 'legacy',
    ADD CONSTRAINT sandboxes_preview_access_valid
        CHECK (preview_access IN ('legacy', 'temps_login', 'public'));
ALTER TABLE sandboxes ALTER COLUMN preview_access SET DEFAULT 'temps_login';

CREATE TABLE ingress_endpoints (
    id BIGSERIAL PRIMARY KEY,
    owner_kind TEXT NOT NULL,
    owner_id BIGINT NOT NULL,
    environment_id INTEGER REFERENCES environments(id) ON DELETE CASCADE,
    sandbox_id INTEGER REFERENCES sandboxes(id) ON DELETE CASCADE,
    -- ADR-055 Phase 2 workload; no foreign key until that table exists.
    workload_id BIGINT,
    target JSONB NOT NULL DEFAULT '{"type":"product"}'::jsonb,
    path_prefix TEXT,
    protocol TEXT NOT NULL DEFAULT 'http',
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    CONSTRAINT ingress_endpoints_owner_kind_valid
        CHECK (owner_kind IN ('environment', 'sandbox', 'database', 'function')),
    CONSTRAINT ingress_endpoints_protocol_valid
        CHECK (protocol IN ('http', 'tls_passthrough', 'tcp')),
    CONSTRAINT ingress_endpoints_target_is_object
        CHECK (jsonb_typeof(target) = 'object'),
    CONSTRAINT ingress_endpoints_environment_owner
        CHECK ((owner_kind = 'environment') = (environment_id IS NOT NULL)
               AND (environment_id IS NULL OR owner_id = environment_id)),
    CONSTRAINT ingress_endpoints_sandbox_owner
        CHECK ((owner_kind = 'sandbox') = (sandbox_id IS NOT NULL)
               AND (sandbox_id IS NULL OR owner_id = sandbox_id)),
    CONSTRAINT ingress_endpoints_path_prefix_valid
        CHECK (path_prefix IS NULL OR (path_prefix LIKE '/%' AND path_prefix <> '/'))
);

CREATE UNIQUE INDEX ingress_endpoints_owner_prefix_unique
    ON ingress_endpoints (owner_kind, owner_id, COALESCE(path_prefix, ''));

CREATE TABLE ingress_access_policies (
    id BIGSERIAL PRIMARY KEY,
    endpoint_id BIGINT NOT NULL REFERENCES ingress_endpoints(id) ON DELETE CASCADE,
    position INTEGER NOT NULL,
    policy JSONB NOT NULL,
    created_by INTEGER REFERENCES users(id) ON DELETE SET NULL,
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    CONSTRAINT ingress_access_policies_policy_is_typed
        CHECK (jsonb_typeof(policy) = 'object' AND policy ? 'type'),
    CONSTRAINT ingress_access_policies_position_valid CHECK (position >= 0),
    UNIQUE (endpoint_id, position)
);

CREATE TABLE ingress_login_grants (
    id BIGSERIAL PRIMARY KEY,
    user_id INTEGER NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    owner_kind TEXT NOT NULL,
    owner_id BIGINT NOT NULL,
    host TEXT NOT NULL,
    permission TEXT NOT NULL,
    expires_at TIMESTAMPTZ NOT NULL,
    revoked_at TIMESTAMPTZ,
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    CONSTRAINT ingress_login_grants_owner_kind_valid
        CHECK (owner_kind IN ('environment', 'sandbox', 'database', 'function'))
);

-- The snapshot carries revoked, unexpired grants only.
CREATE INDEX ingress_login_grants_revoked
    ON ingress_login_grants (expires_at) WHERE revoked_at IS NOT NULL;
CREATE INDEX ingress_login_grants_user ON ingress_login_grants (user_id);
CREATE INDEX ingress_login_grants_expiry ON ingress_login_grants (expires_at);

-- Backfill: every live environment gets its endpoint.
INSERT INTO ingress_endpoints (owner_kind, owner_id, environment_id)
SELECT 'environment', id, id FROM environments
ON CONFLICT DO NOTHING;

CREATE FUNCTION ingress_endpoints_for_new_environment() RETURNS trigger AS $$
BEGIN
    INSERT INTO ingress_endpoints (owner_kind, owner_id, environment_id)
    VALUES ('environment', NEW.id, NEW.id)
    ON CONFLICT DO NOTHING;
    RETURN NEW;
END;
$$ LANGUAGE plpgsql;

CREATE TRIGGER ingress_endpoints_for_new_environment
    AFTER INSERT ON environments
    FOR EACH ROW EXECUTE FUNCTION ingress_endpoints_for_new_environment();

CREATE TRIGGER ingress_endpoints_changes_trigger
    AFTER INSERT OR UPDATE OR DELETE ON ingress_endpoints
    FOR EACH STATEMENT EXECUTE FUNCTION notify_route_table_change();

CREATE TRIGGER ingress_access_policies_changes_trigger
    AFTER INSERT OR UPDATE OR DELETE ON ingress_access_policies
    FOR EACH STATEMENT EXECUTE FUNCTION notify_route_table_change();

CREATE TRIGGER ingress_login_grants_revocation_trigger
    AFTER UPDATE OF revoked_at ON ingress_login_grants
    FOR EACH STATEMENT EXECUTE FUNCTION notify_route_table_change();

CREATE TRIGGER project_ip_access_rules_route_change_trigger
    AFTER INSERT OR UPDATE OR DELETE ON project_ip_access_rules
    FOR EACH STATEMENT EXECUTE FUNCTION notify_route_table_change();
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
DROP TRIGGER IF EXISTS project_ip_access_rules_route_change_trigger ON project_ip_access_rules;
DROP TRIGGER IF EXISTS ingress_endpoints_for_new_environment ON environments;
DROP FUNCTION IF EXISTS ingress_endpoints_for_new_environment();
DROP TABLE IF EXISTS ingress_login_grants;
DROP TABLE IF EXISTS ingress_access_policies;
DROP TABLE IF EXISTS ingress_endpoints;
ALTER TABLE sandboxes DROP CONSTRAINT IF EXISTS sandboxes_preview_access_valid;
ALTER TABLE sandboxes DROP COLUMN IF EXISTS preview_access;
ALTER TABLE environments DROP CONSTRAINT IF EXISTS environments_ingress_mode_valid;
ALTER TABLE environments DROP COLUMN IF EXISTS ingress_mode;
"#,
            )
            .await?;
        Ok(())
    }
}
