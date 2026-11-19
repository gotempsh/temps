// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Per-service overrides of the Docker healthcheck a managed service's
//! container is created with (`temps_providers_core::healthcheck`). Every
//! column is nullable: NULL inherits the instance default
//! (`AppSettings.managed_service_healthchecks`), then Temps' built-in value.
//! Existing rows keep NULL, so nothing changes for them until an operator
//! sets an override.

use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .alter_table(
                Table::alter()
                    .table(ExternalServices::Table)
                    .add_column(
                        ColumnDef::new(ExternalServices::HealthcheckIntervalSecs)
                            .integer()
                            .null(),
                    )
                    .add_column(
                        ColumnDef::new(ExternalServices::HealthcheckStartIntervalSecs)
                            .integer()
                            .null(),
                    )
                    .add_column(
                        ColumnDef::new(ExternalServices::HealthcheckStartPeriodSecs)
                            .integer()
                            .null(),
                    )
                    .add_column(
                        ColumnDef::new(ExternalServices::HealthcheckTimeoutSecs)
                            .integer()
                            .null(),
                    )
                    .add_column(
                        ColumnDef::new(ExternalServices::HealthcheckRetries)
                            .integer()
                            .null(),
                    )
                    .to_owned(),
            )
            .await
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .alter_table(
                Table::alter()
                    .table(ExternalServices::Table)
                    .drop_column(ExternalServices::HealthcheckIntervalSecs)
                    .drop_column(ExternalServices::HealthcheckStartIntervalSecs)
                    .drop_column(ExternalServices::HealthcheckStartPeriodSecs)
                    .drop_column(ExternalServices::HealthcheckTimeoutSecs)
                    .drop_column(ExternalServices::HealthcheckRetries)
                    .to_owned(),
            )
            .await
    }
}

#[derive(DeriveIden)]
enum ExternalServices {
    Table,
    HealthcheckIntervalSecs,
    HealthcheckStartIntervalSecs,
    HealthcheckStartPeriodSecs,
    HealthcheckTimeoutSecs,
    HealthcheckRetries,
}

#[cfg(test)]
mod tests {
    use super::*;
    use sea_orm::{ConnectionTrait, Database, DatabaseBackend, DatabaseConnection, Statement};
    use testcontainers::{core::WaitFor, runners::AsyncRunner, GenericImage, ImageExt};

    const COLUMNS: [&str; 5] = [
        "healthcheck_interval_secs",
        "healthcheck_start_interval_secs",
        "healthcheck_start_period_secs",
        "healthcheck_timeout_secs",
        "healthcheck_retries",
    ];

    /// `(column, is_nullable)` of every healthcheck column present.
    async fn healthcheck_columns(db: &DatabaseConnection) -> Result<Vec<(String, String)>, DbErr> {
        let rows = db
            .query_all(Statement::from_string(
                DatabaseBackend::Postgres,
                "SELECT column_name::TEXT AS name, is_nullable::TEXT AS nullable \
                 FROM information_schema.columns \
                 WHERE table_name = 'external_services' AND column_name LIKE 'healthcheck_%' \
                 ORDER BY column_name",
            ))
            .await?;
        rows.iter()
            .map(|row| Ok((row.try_get("", "name")?, row.try_get("", "nullable")?)))
            .collect()
    }

    /// Existing rows read as "inherit", an override round-trips, and `down`
    /// then `up` leave the table as `up` alone does.
    #[tokio::test]
    async fn overrides_are_nullable_and_reversible() -> Result<(), Box<dyn std::error::Error>> {
        let Ok(docker) = bollard::Docker::connect_with_local_defaults() else {
            eprintln!("Docker unavailable; skipping healthcheck overrides migration test");
            return Ok(());
        };
        if docker.ping().await.is_err() {
            eprintln!("Docker unavailable; skipping healthcheck overrides migration test");
            return Ok(());
        }
        let container = GenericImage::new("postgres", "17-alpine")
            .with_wait_for(WaitFor::message_on_stderr(
                "database system is ready to accept connections",
            ))
            .with_env_var("POSTGRES_PASSWORD", "healthcheck-overrides-test")
            .start()
            .await?;
        let port = container.get_host_port_ipv4(5432).await?;
        let url =
            format!("postgres://postgres:healthcheck-overrides-test@127.0.0.1:{port}/postgres");
        let db = Database::connect(&url).await?;
        db.execute_unprepared(
            "CREATE TABLE external_services(id SERIAL PRIMARY KEY, name TEXT NOT NULL); \
             INSERT INTO external_services(name) VALUES ('existing');",
        )
        .await?;

        let manager = SchemaManager::new(&db);
        Migration.up(&manager).await?;

        let mut expected: Vec<(String, String)> = COLUMNS
            .iter()
            .map(|c| ((*c).to_owned(), "YES".to_owned()))
            .collect();
        expected.sort();
        assert_eq!(healthcheck_columns(&db).await?, expected);

        // The row from before the migration inherits every value.
        let row = db
            .query_one(Statement::from_string(
                DatabaseBackend::Postgres,
                "SELECT healthcheck_interval_secs, healthcheck_retries \
                 FROM external_services WHERE name = 'existing'",
            ))
            .await?
            .ok_or("the existing row is gone")?;
        assert_eq!(
            row.try_get::<Option<i32>>("", "healthcheck_interval_secs")?,
            None
        );
        assert_eq!(row.try_get::<Option<i32>>("", "healthcheck_retries")?, None);

        db.execute_unprepared(
            "UPDATE external_services SET healthcheck_interval_secs = 60, \
             healthcheck_start_interval_secs = 2 WHERE name = 'existing'",
        )
        .await?;
        let row = db
            .query_one(Statement::from_string(
                DatabaseBackend::Postgres,
                "SELECT healthcheck_interval_secs FROM external_services",
            ))
            .await?
            .ok_or("the existing row is gone")?;
        assert_eq!(
            row.try_get::<Option<i32>>("", "healthcheck_interval_secs")?,
            Some(60)
        );

        Migration.down(&manager).await?;
        assert!(healthcheck_columns(&db).await?.is_empty());
        Migration.up(&manager).await?;
        assert_eq!(healthcheck_columns(&db).await?, expected);
        Ok(())
    }
}
