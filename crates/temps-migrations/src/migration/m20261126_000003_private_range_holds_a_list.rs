// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Let `private_range` hold every range a private listener admits (ADR-115 D8).
//!
//! A private address stored its listener's private block, one CIDR, so
//! `VARCHAR(64)` was enough. It now stores what the listener admits, a
//! comma-separated list (`100.64.0.0/10,fd7a:115c:a1e0::/48`) of up to
//! `MAX_ADMITS` ranges, which can be longer, so both columns become `TEXT`.
//!
//! `down` narrows them back and fails, changing nothing, while any stored
//! list is longer than 64 characters: truncating one would change who may
//! reach that address.

use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

const UP: &str = r#"
ALTER TABLE project_custom_domains ALTER COLUMN private_range TYPE TEXT;
ALTER TABLE environment_domains ALTER COLUMN private_range TYPE TEXT;
"#;

const DOWN: &str = r#"
ALTER TABLE project_custom_domains ALTER COLUMN private_range TYPE VARCHAR(64);
ALTER TABLE environment_domains ALTER COLUMN private_range TYPE VARCHAR(64);
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
    use temps_test_containers::TestContainerExt;
    use testcontainers::{core::WaitFor, runners::AsyncRunner, GenericImage, ImageExt};

    /// Five admitted ranges (72 characters) fit after `up`, and `down` refuses
    /// to cut such a list short rather than change who may connect.
    #[tokio::test]
    async fn a_range_list_longer_than_64_characters_is_stored_whole(
    ) -> Result<(), Box<dyn std::error::Error>> {
        let Ok(docker) = bollard::Docker::connect_with_local_defaults() else {
            eprintln!("Docker unavailable; skipping private range migration integration");
            return Ok(());
        };
        if docker.ping().await.is_err() {
            eprintln!("Docker unavailable; skipping private range migration integration");
            return Ok(());
        }
        let container = GenericImage::new("postgres", "17-alpine")
            .with_wait_for(WaitFor::message_on_stderr(
                "database system is ready to accept connections",
            ))
            .with_env_var("POSTGRES_PASSWORD", "private-range-test")
            .with_test_labels()
            .start()
            .await?;
        let port = container.get_host_port_ipv4(5432).await?;
        let db = Database::connect(format!(
            "postgres://postgres:private-range-test@127.0.0.1:{port}/postgres"
        ))
        .await?;
        // The two columns as m20261025_000001_private_audience made them.
        db.execute_unprepared(
            "CREATE TABLE project_custom_domains(id SERIAL PRIMARY KEY, private_range VARCHAR(64)); \
             CREATE TABLE environment_domains(id SERIAL PRIMARY KEY, private_range VARCHAR(64)); \
             INSERT INTO project_custom_domains(private_range) VALUES ('100.64.0.0/10'); \
             INSERT INTO environment_domains(private_range) VALUES (NULL);",
        )
        .await?;
        let list = "10.8.0.0/24,fd42:42:42::/64,192.168.1.0/24,fd00:1:2:3::/64,172.20.0.0/16";
        assert!(list.len() > 64);
        let store = format!(
            "UPDATE project_custom_domains SET private_range = '{list}'; \
             UPDATE environment_domains SET private_range = '{list}';"
        );
        assert!(
            db.execute_unprepared(&store).await.is_err(),
            "VARCHAR(64) refuses the list"
        );

        let manager = SchemaManager::new(&db);
        Migration.up(&manager).await?;
        db.execute_unprepared(&store).await?;
        for table in ["project_custom_domains", "environment_domains"] {
            let row = db
                .query_one(Statement::from_string(
                    DatabaseBackend::Postgres,
                    format!("SELECT private_range FROM {table} LIMIT 1"),
                ))
                .await?
                .ok_or_else(|| DbErr::RecordNotFound(table.to_string()))?;
            assert_eq!(row.try_get::<String>("", "private_range")?, list);
        }

        assert!(
            Migration.down(&manager).await.is_err(),
            "down must not truncate a stored list"
        );
        db.execute_unprepared(
            "UPDATE project_custom_domains SET private_range = '100.64.0.0/10'; \
             UPDATE environment_domains SET private_range = NULL;",
        )
        .await?;
        Migration.down(&manager).await?;
        Migration.up(&manager).await?;
        Ok(())
    }
}
