// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! The Tailscale ways in for a private address (ADR 065 D1, D4, D11).
//!
//! - `private_via` on `project_custom_domains` and `environment_domains`:
//!   how a `private` (or `tailnet_users`) address is served: `listener` (the
//!   phase 2 private listener), `tailscale_serve` (the worker's machine name
//!   through Tailscale Serve) or `tailscale_service` (a Tailscale Service).
//!   `private_service` holds the `svc:` name of a Service.
//! - Every row whose audience is `private` is backfilled `listener`, the only
//!   way in a phase 2 address can have, and then a check makes a private
//!   address without a way in (or any other address with one) impossible.
//!   The check is added `NOT VALID` before the backfill, so it holds for
//!   every write from then on, and validated after it: no window in which a
//!   phase 2 address is unserved, and no long exclusive lock (the validation
//!   takes `SHARE UPDATE EXCLUSIVE`, which does not block writes).
//! - `tailnet_serve_entries`: what a worker must configure in its tailscaled
//!   Serve config for an address, one row per (node, address), shared by the
//!   runs that need it through `tailnet_serve_refs`. `node_id` deliberately
//!   has no foreign key: a worker that leaves the cluster may still serve the
//!   entry, and the row is what says so until the machine is proven clean.
//! - `node_public_ingress.tailscale`: the worker's Tailscale facts as it last
//!   reported them (version, backend state, tags, MagicDNS and HTTPS, its
//!   machine name, consent, root).
//!
//! Nothing changes for an install that never uses Tailscale: the new columns
//! are empty on every public address, and the tables stay empty.

use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

const UP: &str = r#"
ALTER TABLE project_custom_domains
    ADD COLUMN IF NOT EXISTS private_via VARCHAR(32),
    ADD COLUMN IF NOT EXISTS private_service VARCHAR(80);

ALTER TABLE environment_domains
    ADD COLUMN IF NOT EXISTS private_via VARCHAR(32),
    ADD COLUMN IF NOT EXISTS private_service VARCHAR(80);

ALTER TABLE project_custom_domains
    DROP CONSTRAINT IF EXISTS project_custom_domains_private_via_check;
ALTER TABLE project_custom_domains
    ADD CONSTRAINT project_custom_domains_private_via_check CHECK (
        (audience IN ('private', 'tailnet_users')) = (private_via IS NOT NULL)
        AND (private_via IS NULL
             OR private_via IN ('listener', 'tailscale_serve', 'tailscale_service'))
        AND COALESCE(private_via = 'tailscale_service', FALSE) = (private_service IS NOT NULL)
    ) NOT VALID;

ALTER TABLE environment_domains
    DROP CONSTRAINT IF EXISTS environment_domains_private_via_check;
ALTER TABLE environment_domains
    ADD CONSTRAINT environment_domains_private_via_check CHECK (
        (audience IN ('private', 'tailnet_users')) = (private_via IS NOT NULL)
        AND (private_via IS NULL
             OR private_via IN ('listener', 'tailscale_serve', 'tailscale_service'))
        AND COALESCE(private_via = 'tailscale_service', FALSE) = (private_service IS NOT NULL)
    ) NOT VALID;

UPDATE project_custom_domains SET private_via = 'listener'
    WHERE audience = 'private' AND private_via IS NULL;
UPDATE environment_domains SET private_via = 'listener'
    WHERE audience = 'private' AND private_via IS NULL;

ALTER TABLE project_custom_domains
    VALIDATE CONSTRAINT project_custom_domains_private_via_check;
ALTER TABLE environment_domains
    VALIDATE CONSTRAINT environment_domains_private_via_check;

ALTER TABLE node_public_ingress
    ADD COLUMN IF NOT EXISTS tailscale JSONB;

CREATE TABLE IF NOT EXISTS tailnet_serve_entries (
    id SERIAL PRIMARY KEY,
    node_id INTEGER NOT NULL,
    node_name VARCHAR(255) NOT NULL,
    address_id VARCHAR(32) NOT NULL,
    hostname VARCHAR(255) NOT NULL,
    private_via VARCHAR(32) NOT NULL
        CHECK (private_via IN ('tailscale_serve', 'tailscale_service')),
    service_name VARCHAR(80),
    tailnet VARCHAR(255),
    service_addrs JSONB,
    state VARCHAR(16) NOT NULL DEFAULT 'active' CHECK (state IN ('active', 'removing')),
    report VARCHAR(16) CHECK (report IN ('applied', 'conflict', 'error')),
    report_detail TEXT,
    applied_etag TEXT,
    reported_at TIMESTAMPTZ,
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    CONSTRAINT tailnet_serve_entries_node_address UNIQUE (node_id, address_id),
    CONSTRAINT tailnet_serve_entries_service_check
        CHECK ((private_via = 'tailscale_service') = (service_name IS NOT NULL))
);

CREATE INDEX IF NOT EXISTS idx_tailnet_serve_entries_service
    ON tailnet_serve_entries (service_name) WHERE service_name IS NOT NULL;

CREATE TABLE IF NOT EXISTS tailnet_serve_refs (
    id SERIAL PRIMARY KEY,
    entry_id INTEGER NOT NULL REFERENCES tailnet_serve_entries(id) ON DELETE CASCADE,
    run_id INTEGER NOT NULL REFERENCES reachability_runs(id) ON DELETE CASCADE,
    kind VARCHAR(16) NOT NULL CHECK (kind IN ('created', 'found')),
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    CONSTRAINT tailnet_serve_refs_entry_run UNIQUE (entry_id, run_id)
);
"#;

const DOWN: &str = r#"
DROP TABLE IF EXISTS tailnet_serve_refs;
DROP TABLE IF EXISTS tailnet_serve_entries;
ALTER TABLE node_public_ingress DROP COLUMN IF EXISTS tailscale;
ALTER TABLE environment_domains
    DROP CONSTRAINT IF EXISTS environment_domains_private_via_check;
ALTER TABLE project_custom_domains
    DROP CONSTRAINT IF EXISTS project_custom_domains_private_via_check;
ALTER TABLE environment_domains
    DROP COLUMN IF EXISTS private_service,
    DROP COLUMN IF EXISTS private_via;
ALTER TABLE project_custom_domains
    DROP COLUMN IF EXISTS private_service,
    DROP COLUMN IF EXISTS private_via;
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

    /// Phase 2 private addresses are backfilled `listener` and public ones
    /// get nothing; afterwards no write can leave a private address without
    /// a way in, give a public one a way in, or a Service without its name;
    /// serve entries are unique per (node, address) and their references go
    /// with them; `down` then `up` again is clean.
    #[tokio::test]
    async fn private_via_is_backfilled_and_then_always_present(
    ) -> Result<(), Box<dyn std::error::Error>> {
        let Ok(docker) = bollard::Docker::connect_with_local_defaults() else {
            eprintln!("Docker unavailable; skipping tailnet serve migration integration");
            return Ok(());
        };
        if docker.ping().await.is_err() {
            eprintln!("Docker unavailable; skipping tailnet serve migration integration");
            return Ok(());
        }
        let container = GenericImage::new("postgres", "17-alpine")
            .with_wait_for(WaitFor::message_on_stderr(
                "database system is ready to accept connections",
            ))
            .with_env_var("POSTGRES_PASSWORD", "tailnet-serve-test")
            .start()
            .await?;
        let port = container.get_host_port_ipv4(5432).await?;
        let db = Database::connect(format!(
            "postgres://postgres:tailnet-serve-test@127.0.0.1:{port}/postgres"
        ))
        .await?;
        // Only the tables this migration touches or references.
        db.execute_unprepared(
            "CREATE TABLE project_custom_domains(id SERIAL PRIMARY KEY, domain TEXT, \
               audience VARCHAR(16) NOT NULL DEFAULT 'public'); \
             CREATE TABLE environment_domains(id SERIAL PRIMARY KEY, domain TEXT, \
               audience VARCHAR(16) NOT NULL DEFAULT 'public'); \
             CREATE TABLE node_public_ingress(node_id SERIAL PRIMARY KEY); \
             CREATE TABLE reachability_runs(id SERIAL PRIMARY KEY); \
             INSERT INTO project_custom_domains(domain, audience) VALUES \
               ('app.example.com', 'public'), ('private.example.com', 'private'); \
             INSERT INTO environment_domains(domain, audience) VALUES \
               ('www.example.com', 'private'); \
             INSERT INTO reachability_runs DEFAULT VALUES;",
        )
        .await?;

        let manager = SchemaManager::new(&db);
        Migration.up(&manager).await?;

        let via = |table: &'static str, id: i32| {
            let db = &db;
            async move {
                let row = db
                    .query_one(Statement::from_string(
                        DatabaseBackend::Postgres,
                        format!("SELECT private_via FROM {table} WHERE id = {id}"),
                    ))
                    .await?
                    .ok_or_else(|| DbErr::RecordNotFound(table.to_string()))?;
                row.try_get::<Option<String>>("", "private_via")
            }
        };
        assert_eq!(via("project_custom_domains", 1).await?, None);
        assert_eq!(
            via("project_custom_domains", 2).await?.as_deref(),
            Some("listener")
        );
        assert_eq!(
            via("environment_domains", 1).await?.as_deref(),
            Some("listener")
        );

        for refused in [
            // A private address with no way in.
            "UPDATE project_custom_domains SET private_via = NULL WHERE id = 2",
            // A public address with one.
            "UPDATE project_custom_domains SET private_via = 'listener' WHERE id = 1",
            // A way in nobody knows.
            "UPDATE project_custom_domains SET private_via = 'funnel' WHERE id = 2",
            // A Service without its name, and a name without a Service.
            "UPDATE project_custom_domains SET private_via = 'tailscale_service' WHERE id = 2",
            "UPDATE project_custom_domains SET private_service = 'svc:myapp' WHERE id = 2",
            // A newly private address with no way in.
            "UPDATE environment_domains SET private_via = NULL WHERE id = 1",
        ] {
            assert!(
                db.execute_unprepared(refused).await.is_err(),
                "must be refused: {refused}"
            );
        }
        db.execute_unprepared(
            "UPDATE project_custom_domains SET private_via = 'tailscale_service', \
               private_service = 'svc:myapp' WHERE id = 2; \
             UPDATE project_custom_domains SET audience = 'tailnet_users', \
               private_via = 'tailscale_serve', private_service = NULL WHERE id = 2;",
        )
        .await?;

        db.execute_unprepared(
            "INSERT INTO tailnet_serve_entries(node_id, node_name, address_id, hostname, \
               private_via) VALUES (7, 'master', 'c2', 'master.example.ts.net', \
               'tailscale_serve'); \
             INSERT INTO tailnet_serve_refs(entry_id, run_id, kind) VALUES (1, 1, 'created');",
        )
        .await?;
        assert!(
            db.execute_unprepared(
                "INSERT INTO tailnet_serve_entries(node_id, node_name, address_id, hostname, \
                   private_via) VALUES (7, 'master', 'c2', 'other.example.ts.net', \
                   'tailscale_serve')"
            )
            .await
            .is_err(),
            "one entry per (node, address)"
        );
        assert!(
            db.execute_unprepared(
                "INSERT INTO tailnet_serve_entries(node_id, node_name, address_id, hostname, \
                   private_via) VALUES (7, 'master', 'c3', 'myapp.example.ts.net', \
                   'tailscale_service')"
            )
            .await
            .is_err(),
            "a Service entry names its Service"
        );
        db.execute_unprepared("DELETE FROM tailnet_serve_entries WHERE id = 1")
            .await?;
        let refs = db
            .query_one(Statement::from_string(
                DatabaseBackend::Postgres,
                "SELECT COUNT(*)::INT AS n FROM tailnet_serve_refs".to_string(),
            ))
            .await?
            .ok_or_else(|| DbErr::RecordNotFound("tailnet_serve_refs".into()))?;
        assert_eq!(refs.try_get::<i32>("", "n")?, 0);

        db.execute_unprepared(
            "UPDATE project_custom_domains SET audience = 'private', private_via = 'listener' \
             WHERE id = 2",
        )
        .await?;
        Migration.down(&manager).await?;
        Migration.up(&manager).await?;
        assert_eq!(via("project_custom_domains", 1).await?, None);
        assert_eq!(
            via("project_custom_domains", 2).await?.as_deref(),
            Some("listener")
        );
        Ok(())
    }
}
