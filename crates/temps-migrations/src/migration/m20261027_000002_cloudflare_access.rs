// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Cloudflare Access for tunneled signed-in addresses (ADR 065 D9).
//!
//! - `cloudflare_access_providers`: the separate Cloudflare token with
//!   Access permissions, sealed with `EncryptionService` and never returned,
//!   the account it belongs to, the team domain (`auth_domain`) its Access
//!   JWTs are issued by, and the team's signing keys (`jwks`) as last
//!   fetched from `https://<team>/cdn-cgi/access/certs`.
//! - `gate_access_provider_id`, `gate_access_app_id` and `gate_access_aud`
//!   on `project_custom_domains` and `environment_domains`: the Access app
//!   in front of an address whose `gate` is `cloudflare_access`. The AUD is
//!   what a listener checks a JWT's `aud` against.
//! - `cloudflare_access_keys_notify`: a key rotation reaches listeners with
//!   the next route snapshot, which is rebuilt on `route_table_changes`.
//!
//! `gate_access_provider_id` references its token `ON DELETE RESTRICT`, so
//! a token is never deleted from under an address bound to it.
//!
//! Every new column is nullable and nothing is backfilled (D11).

use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

const UP: &str = r#"
CREATE TABLE IF NOT EXISTS cloudflare_access_providers (
    id SERIAL PRIMARY KEY,
    name VARCHAR(255) NOT NULL,
    account_id VARCHAR(64) NOT NULL,
    team_domain VARCHAR(255) NOT NULL,
    api_token_encrypted TEXT NOT NULL,
    jwks JSONB,
    jwks_fetched_at TIMESTAMPTZ,
    created_by INTEGER,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

ALTER TABLE project_custom_domains
    ADD COLUMN IF NOT EXISTS gate_access_provider_id INTEGER
        REFERENCES cloudflare_access_providers(id) ON DELETE RESTRICT,
    ADD COLUMN IF NOT EXISTS gate_access_app_id VARCHAR(64),
    ADD COLUMN IF NOT EXISTS gate_access_aud VARCHAR(128);

ALTER TABLE environment_domains
    ADD COLUMN IF NOT EXISTS gate_access_provider_id INTEGER
        REFERENCES cloudflare_access_providers(id) ON DELETE RESTRICT,
    ADD COLUMN IF NOT EXISTS gate_access_app_id VARCHAR(64),
    ADD COLUMN IF NOT EXISTS gate_access_aud VARCHAR(128);

CREATE OR REPLACE FUNCTION cloudflare_access_keys_notify() RETURNS trigger AS $$
BEGIN
    IF NEW.jwks IS DISTINCT FROM OLD.jwks OR NEW.team_domain IS DISTINCT FROM OLD.team_domain THEN
        PERFORM pg_notify('route_table_changes', 'cloudflare_access_keys');
    END IF;
    RETURN NULL;
END;
$$ LANGUAGE plpgsql;

DROP TRIGGER IF EXISTS cloudflare_access_keys ON cloudflare_access_providers;
CREATE TRIGGER cloudflare_access_keys
    AFTER UPDATE OF jwks, team_domain ON cloudflare_access_providers
    FOR EACH ROW EXECUTE FUNCTION cloudflare_access_keys_notify();
"#;

const DOWN: &str = r#"
DROP TRIGGER IF EXISTS cloudflare_access_keys ON cloudflare_access_providers;
DROP FUNCTION IF EXISTS cloudflare_access_keys_notify();
ALTER TABLE environment_domains
    DROP COLUMN IF EXISTS gate_access_aud,
    DROP COLUMN IF EXISTS gate_access_app_id,
    DROP COLUMN IF EXISTS gate_access_provider_id;
ALTER TABLE project_custom_domains
    DROP COLUMN IF EXISTS gate_access_aud,
    DROP COLUMN IF EXISTS gate_access_app_id,
    DROP COLUMN IF EXISTS gate_access_provider_id;
DROP TABLE IF EXISTS cloudflare_access_providers;
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

    /// Existing addresses gain no Access app, a key rotation announces a
    /// route reload, and `down` then `up` again is clean.
    #[tokio::test]
    async fn access_columns_are_empty_and_key_rotation_notifies(
    ) -> Result<(), Box<dyn std::error::Error>> {
        let Ok(docker) = bollard::Docker::connect_with_local_defaults() else {
            eprintln!("Docker unavailable; skipping Cloudflare Access migration integration");
            return Ok(());
        };
        if docker.ping().await.is_err() {
            eprintln!("Docker unavailable; skipping Cloudflare Access migration integration");
            return Ok(());
        }
        let container = GenericImage::new("postgres", "17-alpine")
            .with_wait_for(WaitFor::message_on_stderr(
                "database system is ready to accept connections",
            ))
            .with_env_var("POSTGRES_PASSWORD", "cloudflare-access-test")
            .start()
            .await?;
        let port = container.get_host_port_ipv4(5432).await?;
        let db = Database::connect(format!(
            "postgres://postgres:cloudflare-access-test@127.0.0.1:{port}/postgres"
        ))
        .await?;
        db.execute_unprepared(
            "CREATE TABLE project_custom_domains(id SERIAL PRIMARY KEY, domain TEXT); \
             CREATE TABLE environment_domains(id SERIAL PRIMARY KEY, domain TEXT); \
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
                        "SELECT (gate_access_provider_id IS NULL AND gate_access_app_id IS NULL \
                         AND gate_access_aud IS NULL) AS open FROM {table} LIMIT 1"
                    ),
                ))
                .await?
                .ok_or_else(|| DbErr::RecordNotFound(table.to_string()))?;
            assert!(
                row.try_get::<bool>("", "open")?,
                "{table} gained an Access app"
            );
        }

        db.execute_unprepared(
            "INSERT INTO cloudflare_access_providers(name, account_id, team_domain, \
                 api_token_encrypted) VALUES ('access', 'acc', 'team.cloudflareaccess.com', 's'); \
             UPDATE cloudflare_access_providers SET jwks = '{\"keys\": []}'::jsonb;",
        )
        .await?;
        let triggers = db
            .query_one(Statement::from_string(
                DatabaseBackend::Postgres,
                "SELECT count(*)::INT AS n FROM pg_trigger WHERE tgname = 'cloudflare_access_keys'"
                    .to_string(),
            ))
            .await?
            .ok_or_else(|| DbErr::RecordNotFound("pg_trigger".to_string()))?;
        assert_eq!(triggers.try_get::<i32>("", "n")?, 1);

        // A token an address is bound to cannot be deleted under it.
        db.execute_unprepared(
            "UPDATE environment_domains SET gate_access_provider_id = \
                 (SELECT id FROM cloudflare_access_providers LIMIT 1);",
        )
        .await?;
        assert!(
            db.execute_unprepared("DELETE FROM cloudflare_access_providers;")
                .await
                .is_err(),
            "a token bound to an address was deleted"
        );
        db.execute_unprepared("UPDATE environment_domains SET gate_access_provider_id = NULL;")
            .await?;

        Migration.down(&manager).await?;
        Migration.up(&manager).await?;
        Ok(())
    }
}
