// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Persists the logical database strategy on each project-to-service link.
//!
//! Existing links keep the historical per-project/environment behavior. The
//! constraint also protects callers that bypass the HTTP and service layers.

use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .alter_table(
                Table::alter()
                    .table(ProjectServices::Table)
                    .add_column(
                        ColumnDef::new(ProjectServices::DatabaseProvisioningMode)
                            .string()
                            .not_null()
                            .default("project_environment"),
                    )
                    .add_column(
                        ColumnDef::new(ProjectServices::CustomDatabaseName)
                            .string()
                            .null(),
                    )
                    .to_owned(),
            )
            .await?;

        manager
            .get_connection()
            .execute_unprepared(
                "ALTER TABLE project_services \
                 ADD CONSTRAINT chk_project_services_database_provisioning \
                 CHECK (\
                   (database_provisioning_mode IN ('project', 'project_environment') \
                    AND custom_database_name IS NULL) \
                   OR \
                   (database_provisioning_mode = 'custom' \
                    AND custom_database_name IS NOT NULL \
                    AND custom_database_name ~ '^[a-z_][a-z0-9_]{0,62}$')\
                 )",
            )
            .await?;

        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .get_connection()
            .execute_unprepared(
                "ALTER TABLE project_services \
                 DROP CONSTRAINT IF EXISTS chk_project_services_database_provisioning",
            )
            .await?;

        manager
            .alter_table(
                Table::alter()
                    .table(ProjectServices::Table)
                    .drop_column(ProjectServices::CustomDatabaseName)
                    .drop_column(ProjectServices::DatabaseProvisioningMode)
                    .to_owned(),
            )
            .await
    }
}

#[derive(DeriveIden)]
enum ProjectServices {
    Table,
    DatabaseProvisioningMode,
    CustomDatabaseName,
}

#[cfg(test)]
mod tests {
    use super::*;
    use sea_orm::{ConnectionTrait, Database, DatabaseBackend, Statement};
    use testcontainers::{core::WaitFor, runners::AsyncRunner, GenericImage, ImageExt};

    #[tokio::test]
    async fn database_provisioning_migration_preserves_rows_and_reverses() {
        let container = match GenericImage::new("postgres", "17-alpine")
            .with_wait_for(WaitFor::message_on_stderr(
                "database system is ready to accept connections",
            ))
            .with_env_var("POSTGRES_PASSWORD", "migration-test")
            .start()
            .await
        {
            Ok(container) => container,
            Err(error) => {
                eprintln!(
                    "Skipping database provisioning migration test: Docker unavailable: {error}"
                );
                return;
            }
        };
        let port = container
            .get_host_port_ipv4(5432)
            .await
            .expect("PostgreSQL port");
        let db = Database::connect(format!(
            "postgres://postgres:migration-test@127.0.0.1:{port}/postgres"
        ))
        .await
        .expect("connect to disposable migration database");
        db.execute_unprepared("CREATE TABLE project_services (id INTEGER PRIMARY KEY); INSERT INTO project_services VALUES (1)")
            .await.expect("create legacy table and row");
        let manager = SchemaManager::new(&db);
        Migration
            .up(&manager)
            .await
            .expect("apply provisioning migration");
        let row = db.query_one(Statement::from_string(DatabaseBackend::Postgres,
            "SELECT database_provisioning_mode, custom_database_name FROM project_services WHERE id = 1"))
            .await.expect("read legacy row").expect("legacy row retained");
        assert_eq!(
            row.try_get::<String>("", "database_provisioning_mode")
                .expect("mode"),
            "project_environment"
        );
        assert_eq!(
            row.try_get::<Option<String>>("", "custom_database_name")
                .expect("custom name"),
            None
        );
        db.execute_unprepared("INSERT INTO project_services VALUES (2, 'project', NULL), (3, 'custom', 'shared_catalog')")
            .await.expect("accept project and custom modes");
        for (mode, name) in [
            ("custom", "NULL"),
            ("custom", "'bad-name'"),
            ("project", "'unexpected'"),
            ("unknown", "NULL"),
        ] {
            // All values are fixed test literals, never user input.
            assert!(
                db.execute_unprepared(&format!(
                    "INSERT INTO project_services VALUES (4, '{mode}', {name})"
                ))
                .await
                .is_err(),
                "constraint must reject {mode}/{name}"
            );
        }
        Migration
            .down(&manager)
            .await
            .expect("reverse provisioning migration");
        assert!(!manager
            .has_column("project_services", "database_provisioning_mode")
            .await
            .expect("mode column lookup"));
        assert!(!manager
            .has_column("project_services", "custom_database_name")
            .await
            .expect("custom column lookup"));
        let row = db
            .query_one(Statement::from_string(
                DatabaseBackend::Postgres,
                "SELECT COUNT(*) AS count FROM project_services",
            ))
            .await
            .expect("count retained rows")
            .expect("count row");
        assert_eq!(row.try_get::<i64>("", "count").expect("row count"), 3);
        Migration
            .up(&manager)
            .await
            .expect("apply again after rollback");
        eprintln!("Provisioning migration: up, legacy backfill, CHECK validation, down with row retention, and up again passed");
    }
}
