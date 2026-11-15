// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! What bounds a private address (ADR 064 D5), next to its `audience`.
//!
//! - `private_range` on `project_custom_domains` and `environment_domains`:
//!   the network a visitor must come from (`100.64.0.0/10` for a tailnet,
//!   the LAN block otherwise). The worker's private listener refuses a TCP
//!   peer outside it, for that address only.
//! - `private_node_id`: the worker whose private listener serves it. Only
//!   that worker's route snapshot carries the address; a worker that leaves
//!   clears it, and the address is then served by nobody until it is set up
//!   again.
//! - `private_listener`: the listener address DNS was pointed at, which the
//!   worker's inside check compares the hostname's answer with (D7).
//! - `private_checks` on `node_public_ingress`: the worker's last inside
//!   checks, by address id, as it reported them.
//!
//! All nullable: a public address has none of them, and nothing changes
//! for an install that never makes an address private.

use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

const UP: &str = r#"
ALTER TABLE project_custom_domains
    ADD COLUMN IF NOT EXISTS private_range VARCHAR(64),
    ADD COLUMN IF NOT EXISTS private_node_id INTEGER REFERENCES nodes(id) ON DELETE SET NULL,
    ADD COLUMN IF NOT EXISTS private_listener VARCHAR(64);

ALTER TABLE environment_domains
    ADD COLUMN IF NOT EXISTS private_range VARCHAR(64),
    ADD COLUMN IF NOT EXISTS private_node_id INTEGER REFERENCES nodes(id) ON DELETE SET NULL,
    ADD COLUMN IF NOT EXISTS private_listener VARCHAR(64);

ALTER TABLE node_public_ingress
    ADD COLUMN IF NOT EXISTS private_checks JSONB;
"#;

const DOWN: &str = r#"
ALTER TABLE node_public_ingress DROP COLUMN IF EXISTS private_checks;
ALTER TABLE environment_domains
    DROP COLUMN IF EXISTS private_listener,
    DROP COLUMN IF EXISTS private_node_id,
    DROP COLUMN IF EXISTS private_range;
ALTER TABLE project_custom_domains
    DROP COLUMN IF EXISTS private_listener,
    DROP COLUMN IF EXISTS private_node_id,
    DROP COLUMN IF EXISTS private_range;
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

    /// Existing addresses gain no bounds, a worker that goes away clears the
    /// one it served, and `down` then `up` again is clean.
    #[tokio::test]
    async fn private_bounds_are_optional_and_follow_the_worker(
    ) -> Result<(), Box<dyn std::error::Error>> {
        let Ok(docker) = bollard::Docker::connect_with_local_defaults() else {
            eprintln!("Docker unavailable; skipping private audience migration integration");
            return Ok(());
        };
        if docker.ping().await.is_err() {
            eprintln!("Docker unavailable; skipping private audience migration integration");
            return Ok(());
        }
        let container = GenericImage::new("postgres", "17-alpine")
            .with_wait_for(WaitFor::message_on_stderr(
                "database system is ready to accept connections",
            ))
            .with_env_var("POSTGRES_PASSWORD", "private-audience-test")
            .start()
            .await?;
        let port = container.get_host_port_ipv4(5432).await?;
        let db = Database::connect(format!(
            "postgres://postgres:private-audience-test@127.0.0.1:{port}/postgres"
        ))
        .await?;
        // Only the tables this migration touches or references.
        db.execute_unprepared(
            "CREATE TABLE nodes(id SERIAL PRIMARY KEY); \
             CREATE TABLE project_custom_domains(id SERIAL PRIMARY KEY, domain TEXT); \
             CREATE TABLE environment_domains(id SERIAL PRIMARY KEY, domain TEXT); \
             CREATE TABLE node_public_ingress(id SERIAL PRIMARY KEY); \
             INSERT INTO nodes DEFAULT VALUES; \
             INSERT INTO project_custom_domains(domain) VALUES ('app.example.com'); \
             INSERT INTO environment_domains(domain) VALUES ('www.example.com'); \
             INSERT INTO node_public_ingress DEFAULT VALUES;",
        )
        .await?;

        let manager = SchemaManager::new(&db);
        Migration.up(&manager).await?;

        let unbounded = |table: &'static str| {
            let db = &db;
            async move {
                let row = db
                    .query_one(Statement::from_string(
                        DatabaseBackend::Postgres,
                        format!(
                            "SELECT (private_range IS NULL AND private_node_id IS NULL \
                             AND private_listener IS NULL) AS unbounded FROM {table} LIMIT 1"
                        ),
                    ))
                    .await?
                    .ok_or_else(|| DbErr::RecordNotFound(table.to_string()))?;
                row.try_get::<bool>("", "unbounded")
            }
        };
        assert!(unbounded("project_custom_domains").await?);
        assert!(unbounded("environment_domains").await?);

        db.execute_unprepared(
            "UPDATE project_custom_domains SET private_range = '100.64.0.0/10', \
             private_node_id = 1, private_listener = '100.101.102.103' WHERE id = 1; \
             UPDATE node_public_ingress SET private_checks = '[]'::jsonb WHERE id = 1; \
             DELETE FROM nodes WHERE id = 1;",
        )
        .await?;
        let row = db
            .query_one(Statement::from_string(
                DatabaseBackend::Postgres,
                "SELECT private_node_id IS NULL AS cleared, private_range \
                 FROM project_custom_domains WHERE id = 1"
                    .to_string(),
            ))
            .await?
            .ok_or_else(|| DbErr::RecordNotFound("project_custom_domains".into()))?;
        assert!(
            row.try_get::<bool>("", "cleared")?,
            "a removed worker serves nothing"
        );
        assert_eq!(row.try_get::<String>("", "private_range")?, "100.64.0.0/10");

        Migration.down(&manager).await?;
        Migration.up(&manager).await?;
        assert!(unbounded("environment_domains").await?);
        Ok(())
    }
}
