// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! The Temps gate for signed-in addresses (ADR 065 D7/D8).
//!
//! - `gate`, `gate_allow` and `gate_not_before` on `project_custom_domains`
//!   and `environment_domains`. `gate` is `temps` or `cloudflare_access` for
//!   a `signed_in` address and null otherwise; `gate_allow` is the optional
//!   list of teams and users, `{"teams": [..], "users": [..]}`;
//!   `gate_not_before` refuses every token issued before it (audience, gate
//!   or list changed, or an admin signed everyone out).
//! - `gate_keys`: the single Ed25519 gate key, generated on first use, the
//!   private half sealed with `EncryptionService`.
//! - `gate_sessions`: one row per sign-in to an address, tied to the console
//!   session that created it. No foreign keys on purpose: a revoked session
//!   must stay listed in the route snapshot until it expires, so deleting
//!   the console session or the user must not delete the row.
//! - `gate_codes`: single-use sign-in codes, 60 seconds, by hash. Redeeming
//!   deletes the row, so a code is consumed whether or not it then checks out.
//! - `gate_revocation_notify`: revocation reaches listeners with the next
//!   route snapshot (D8), and the snapshot is rebuilt on `route_table_changes`.
//!   A console sign-out, a removal from a team or a project's teams, an admin
//!   demoted or a user deleted changes no route, so these triggers announce
//!   one, and only when the user concerned has a live gate session (any live
//!   session for a change to a project's teams). An install with no signed-in
//!   address never has one, so nothing changes for it (D11).
//!
//! Every new column is nullable and nothing is backfilled: no address has a
//! gate until someone makes it signed in (D11).

use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

const UP: &str = r#"
ALTER TABLE project_custom_domains
    ADD COLUMN IF NOT EXISTS gate VARCHAR(32),
    ADD COLUMN IF NOT EXISTS gate_allow JSONB,
    ADD COLUMN IF NOT EXISTS gate_not_before TIMESTAMPTZ;

ALTER TABLE environment_domains
    ADD COLUMN IF NOT EXISTS gate VARCHAR(32),
    ADD COLUMN IF NOT EXISTS gate_allow JSONB,
    ADD COLUMN IF NOT EXISTS gate_not_before TIMESTAMPTZ;

CREATE TABLE IF NOT EXISTS gate_keys (
    id INTEGER PRIMARY KEY CHECK (id = 1),
    public_key VARCHAR(64) NOT NULL,
    private_key_encrypted TEXT NOT NULL,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE TABLE IF NOT EXISTS gate_sessions (
    id VARCHAR(64) PRIMARY KEY,
    address_id VARCHAR(64) NOT NULL,
    user_id INTEGER NOT NULL,
    console_session_id INTEGER NOT NULL,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    expires_at TIMESTAMPTZ NOT NULL,
    revoked_at TIMESTAMPTZ
);
CREATE INDEX IF NOT EXISTS gate_sessions_expires_at ON gate_sessions (expires_at);
CREATE INDEX IF NOT EXISTS gate_sessions_user_id ON gate_sessions (user_id);

CREATE TABLE IF NOT EXISTS gate_codes (
    code_hash VARCHAR(64) PRIMARY KEY,
    address_id VARCHAR(64) NOT NULL,
    user_id INTEGER NOT NULL,
    console_session_id INTEGER NOT NULL,
    code_challenge VARCHAR(64) NOT NULL,
    expires_at TIMESTAMPTZ NOT NULL,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now()
);
CREATE INDEX IF NOT EXISTS gate_codes_expires_at ON gate_codes (expires_at);

CREATE OR REPLACE FUNCTION gate_revocation_notify() RETURNS trigger AS $$
DECLARE
    affected_user INTEGER;
BEGIN
    IF TG_TABLE_NAME = 'project_team_access' THEN
        affected_user := NULL;
    ELSIF TG_TABLE_NAME = 'users' THEN
        affected_user := OLD.id;
    ELSE
        affected_user := OLD.user_id;
    END IF;
    IF EXISTS (
        SELECT 1 FROM gate_sessions
        WHERE expires_at > now()
          AND revoked_at IS NULL
          AND (affected_user IS NULL OR user_id = affected_user)
    ) THEN
        PERFORM pg_notify('route_table_changes', 'gate_revocation');
    END IF;
    RETURN NULL;
END;
$$ LANGUAGE plpgsql;

DO $$
BEGIN
    IF to_regclass('sessions') IS NOT NULL THEN
        DROP TRIGGER IF EXISTS gate_revocation_sessions ON sessions;
        CREATE TRIGGER gate_revocation_sessions
            AFTER DELETE OR UPDATE OF expires_at, mfa_pending ON sessions
            FOR EACH ROW EXECUTE FUNCTION gate_revocation_notify();
    END IF;
    IF to_regclass('team_members') IS NOT NULL THEN
        DROP TRIGGER IF EXISTS gate_revocation_team_members ON team_members;
        CREATE TRIGGER gate_revocation_team_members
            AFTER DELETE ON team_members
            FOR EACH ROW EXECUTE FUNCTION gate_revocation_notify();
    END IF;
    IF to_regclass('project_team_access') IS NOT NULL THEN
        DROP TRIGGER IF EXISTS gate_revocation_project_team_access ON project_team_access;
        CREATE TRIGGER gate_revocation_project_team_access
            AFTER INSERT OR DELETE ON project_team_access
            FOR EACH ROW EXECUTE FUNCTION gate_revocation_notify();
    END IF;
    IF to_regclass('users') IS NOT NULL THEN
        DROP TRIGGER IF EXISTS gate_revocation_users ON users;
        CREATE TRIGGER gate_revocation_users
            AFTER DELETE OR UPDATE OF deleted_at ON users
            FOR EACH ROW EXECUTE FUNCTION gate_revocation_notify();
    END IF;
    IF to_regclass('user_roles') IS NOT NULL THEN
        DROP TRIGGER IF EXISTS gate_revocation_user_roles ON user_roles;
        CREATE TRIGGER gate_revocation_user_roles
            AFTER DELETE ON user_roles
            FOR EACH ROW EXECUTE FUNCTION gate_revocation_notify();
    END IF;
END $$;
"#;

const DOWN: &str = r#"
DO $$
BEGIN
    IF to_regclass('sessions') IS NOT NULL THEN
        DROP TRIGGER IF EXISTS gate_revocation_sessions ON sessions;
    END IF;
    IF to_regclass('team_members') IS NOT NULL THEN
        DROP TRIGGER IF EXISTS gate_revocation_team_members ON team_members;
    END IF;
    IF to_regclass('project_team_access') IS NOT NULL THEN
        DROP TRIGGER IF EXISTS gate_revocation_project_team_access ON project_team_access;
    END IF;
    IF to_regclass('users') IS NOT NULL THEN
        DROP TRIGGER IF EXISTS gate_revocation_users ON users;
    END IF;
    IF to_regclass('user_roles') IS NOT NULL THEN
        DROP TRIGGER IF EXISTS gate_revocation_user_roles ON user_roles;
    END IF;
END $$;
DROP FUNCTION IF EXISTS gate_revocation_notify();
DROP TABLE IF EXISTS gate_codes;
DROP TABLE IF EXISTS gate_sessions;
DROP TABLE IF EXISTS gate_keys;
ALTER TABLE environment_domains
    DROP COLUMN IF EXISTS gate_not_before,
    DROP COLUMN IF EXISTS gate_allow,
    DROP COLUMN IF EXISTS gate;
ALTER TABLE project_custom_domains
    DROP COLUMN IF EXISTS gate_not_before,
    DROP COLUMN IF EXISTS gate_allow,
    DROP COLUMN IF EXISTS gate;
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

    /// Existing addresses gain no gate, the key table holds one key, and
    /// `down` then `up` again is clean.
    #[tokio::test]
    async fn gate_columns_are_empty_and_the_key_is_a_singleton(
    ) -> Result<(), Box<dyn std::error::Error>> {
        let Ok(docker) = bollard::Docker::connect_with_local_defaults() else {
            eprintln!("Docker unavailable; skipping signed-in gate migration integration");
            return Ok(());
        };
        if docker.ping().await.is_err() {
            eprintln!("Docker unavailable; skipping signed-in gate migration integration");
            return Ok(());
        }
        let container = GenericImage::new("postgres", "17-alpine")
            .with_wait_for(WaitFor::message_on_stderr(
                "database system is ready to accept connections",
            ))
            .with_env_var("POSTGRES_PASSWORD", "signed-in-gate-test")
            .start()
            .await?;
        let port = container.get_host_port_ipv4(5432).await?;
        let db = Database::connect(format!(
            "postgres://postgres:signed-in-gate-test@127.0.0.1:{port}/postgres"
        ))
        .await?;
        db.execute_unprepared(
            "CREATE TABLE project_custom_domains(id SERIAL PRIMARY KEY, domain TEXT); \
             CREATE TABLE environment_domains(id SERIAL PRIMARY KEY, domain TEXT); \
             CREATE TABLE node_public_ingress(node_id INTEGER PRIMARY KEY); \
             CREATE TABLE sessions(id SERIAL PRIMARY KEY, user_id INTEGER, \
                 expires_at TIMESTAMPTZ, mfa_pending BOOLEAN); \
             INSERT INTO sessions(user_id, expires_at, mfa_pending) \
                 VALUES (7, now() + interval '1 hour', false); \
             INSERT INTO project_custom_domains(domain) VALUES ('app.example.com'); \
             INSERT INTO environment_domains(domain) VALUES ('www.example.com');",
        )
        .await?;

        let manager = SchemaManager::new(&db);
        Migration.up(&manager).await?;

        for table in ["project_custom_domains", "environment_domains"] {
            let row = db
                .query_one(Statement::from_string(
                    DatabaseBackend::Postgres,
                    format!(
                        "SELECT (gate IS NULL AND gate_allow IS NULL AND gate_not_before IS NULL) \
                         AS ungated FROM {table} LIMIT 1"
                    ),
                ))
                .await?
                .ok_or_else(|| DbErr::RecordNotFound(table.to_string()))?;
            assert!(row.try_get::<bool>("", "ungated")?, "{table} gained a gate");
        }

        db.execute_unprepared(
            "INSERT INTO gate_keys(id, public_key, private_key_encrypted) VALUES (1, 'p', 's')",
        )
        .await?;
        assert!(
            db.execute_unprepared(
                "INSERT INTO gate_keys(id, public_key, private_key_encrypted) VALUES (2, 'p', 's')"
            )
            .await
            .is_err(),
            "a second gate key is refused"
        );

        // A console sign-out of a user with a live gate session runs the
        // revocation trigger (and, without one, does nothing but succeed).
        db.execute_unprepared("DELETE FROM sessions WHERE user_id = 7")
            .await?;
        db.execute_unprepared(
            "INSERT INTO sessions(user_id, expires_at, mfa_pending) \
                 VALUES (7, now() + interval '1 hour', false); \
             INSERT INTO gate_sessions(id, address_id, user_id, console_session_id, expires_at) \
                 VALUES ('g1', 'custom:1', 7, 2, now() + interval '1 hour'); \
             DELETE FROM sessions WHERE user_id = 7;",
        )
        .await?;
        let triggers = db
            .query_one(Statement::from_string(
                DatabaseBackend::Postgres,
                "SELECT count(*)::INT AS n FROM pg_trigger WHERE tgname = 'gate_revocation_sessions'"
                    .to_string(),
            ))
            .await?
            .ok_or_else(|| DbErr::RecordNotFound("pg_trigger".to_string()))?;
        assert_eq!(triggers.try_get::<i32>("", "n")?, 1);

        Migration.down(&manager).await?;
        Migration.up(&manager).await?;
        Ok(())
    }
}
