// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Tunnel environment hosts (ADR 068 D3).
//!
//! - `ingress_connectors.environment_hosts`: the tunnel publishes every
//!   environment as `<label>.<connector.domain>`. Off by default, and at most
//!   one connector has it on (a partial unique index), so an environment's
//!   generated host never depends on which tunnel was asked first.
//! - `environment_domains.generated_by_connector_id`: the connector a row was
//!   generated for. Temps only ever renames or removes rows carrying it; a
//!   row the user added is never touched. Deleting the connector removes the
//!   rows it generated, after their records were withdrawn.
//! - Two generated rows never hold one hostname (a unique index on the
//!   lowercased name), so the route table never has to pick between them.
//!
//! Nothing changes for an install that never turns `environment_hosts` on.

use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

const UP: &str = r#"
ALTER TABLE ingress_connectors
    ADD COLUMN IF NOT EXISTS environment_hosts BOOLEAN NOT NULL DEFAULT FALSE;

CREATE UNIQUE INDEX IF NOT EXISTS ingress_connectors_one_environment_hosts
    ON ingress_connectors ((environment_hosts))
    WHERE environment_hosts;

ALTER TABLE environment_domains
    ADD COLUMN IF NOT EXISTS generated_by_connector_id INTEGER
        REFERENCES ingress_connectors(id) ON DELETE CASCADE;

CREATE INDEX IF NOT EXISTS environment_domains_generated_by_connector
    ON environment_domains (generated_by_connector_id)
    WHERE generated_by_connector_id IS NOT NULL;

CREATE UNIQUE INDEX IF NOT EXISTS environment_domains_generated_domain_unique
    ON environment_domains (lower(domain))
    WHERE generated_by_connector_id IS NOT NULL;
"#;

const DOWN: &str = r#"
DROP INDEX IF EXISTS environment_domains_generated_domain_unique;
DROP INDEX IF EXISTS environment_domains_generated_by_connector;
ALTER TABLE environment_domains DROP COLUMN IF EXISTS generated_by_connector_id;
DROP INDEX IF EXISTS ingress_connectors_one_environment_hosts;
ALTER TABLE ingress_connectors DROP COLUMN IF EXISTS environment_hosts;
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

    /// Existing connectors stay off, only one may turn on, user rows carry no
    /// connector, and generated rows go with their connector.
    #[tokio::test]
    async fn environment_hosts_are_opt_in_single_and_owned(
    ) -> Result<(), Box<dyn std::error::Error>> {
        let Ok(docker) = bollard::Docker::connect_with_local_defaults() else {
            eprintln!("Docker unavailable; skipping tunnel environment hosts migration test");
            return Ok(());
        };
        if docker.ping().await.is_err() {
            eprintln!("Docker unavailable; skipping tunnel environment hosts migration test");
            return Ok(());
        }
        let container = GenericImage::new("postgres", "17-alpine")
            .with_wait_for(WaitFor::message_on_stderr(
                "database system is ready to accept connections",
            ))
            .with_env_var("POSTGRES_PASSWORD", "environment-hosts-test")
            .start()
            .await?;
        let port = container.get_host_port_ipv4(5432).await?;
        let db = Database::connect(format!(
            "postgres://postgres:environment-hosts-test@127.0.0.1:{port}/postgres"
        ))
        .await?;
        // Only the tables this migration touches.
        db.execute_unprepared(
            "CREATE TABLE ingress_connectors(id SERIAL PRIMARY KEY, name TEXT); \
             CREATE TABLE environment_domains(id SERIAL PRIMARY KEY, domain TEXT); \
             INSERT INTO ingress_connectors(name) VALUES ('edge'), ('backup'); \
             INSERT INTO environment_domains(domain) VALUES ('app-production');",
        )
        .await?;

        let manager = SchemaManager::new(&db);
        Migration.up(&manager).await?;

        let row = db
            .query_one(Statement::from_string(
                DatabaseBackend::Postgres,
                "SELECT bool_or(environment_hosts) AS any_on FROM ingress_connectors".to_string(),
            ))
            .await?
            .ok_or_else(|| DbErr::RecordNotFound("ingress_connectors".into()))?;
        assert!(
            !row.try_get::<bool>("", "any_on")?,
            "existing tunnels stay off"
        );

        db.execute_unprepared(
            "UPDATE ingress_connectors SET environment_hosts = TRUE WHERE id = 1",
        )
        .await?;
        let second = db
            .execute_unprepared(
                "UPDATE ingress_connectors SET environment_hosts = TRUE WHERE id = 2",
            )
            .await;
        assert!(second.is_err(), "at most one tunnel has environment hosts");

        db.execute_unprepared(
            "INSERT INTO environment_domains(domain, generated_by_connector_id) \
             VALUES ('app-production.tunnel.example.com', 1);",
        )
        .await?;
        let duplicate = db
            .execute_unprepared(
                "INSERT INTO environment_domains(domain, generated_by_connector_id) \
                 VALUES ('App-Production.tunnel.example.com', 1)",
            )
            .await;
        assert!(
            duplicate.is_err(),
            "two generated rows never hold one hostname"
        );
        db.execute_unprepared("DELETE FROM ingress_connectors WHERE id = 1")
            .await?;
        let row = db
            .query_one(Statement::from_string(
                DatabaseBackend::Postgres,
                "SELECT count(*)::int AS left_over, \
                 bool_and(generated_by_connector_id IS NULL) AS user_rows \
                 FROM environment_domains"
                    .to_string(),
            ))
            .await?
            .ok_or_else(|| DbErr::RecordNotFound("environment_domains".into()))?;
        assert_eq!(row.try_get::<i32>("", "left_over")?, 1);
        assert!(
            row.try_get::<bool>("", "user_rows")?,
            "the generated row went with its tunnel; the user's row stayed"
        );

        Migration.down(&manager).await?;
        Migration.up(&manager).await?;
        Ok(())
    }
}
