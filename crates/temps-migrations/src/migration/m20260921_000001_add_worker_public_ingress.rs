// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .alter_table(
                Table::alter()
                    .table(Nodes::Table)
                    .add_column(
                        ColumnDef::new(Nodes::PublicIngressEnabled)
                            .boolean()
                            .not_null()
                            .default(false),
                    )
                    .add_column(ColumnDef::new(Nodes::PublicIngressRunning).boolean().null())
                    .add_column(ColumnDef::new(Nodes::PublicIngressLastError).text().null())
                    .add_column(
                        ColumnDef::new(Nodes::PublicIngressCertificateCount)
                            .integer()
                            .null(),
                    )
                    .add_column(
                        ColumnDef::new(Nodes::PublicIngressRouteCount)
                            .integer()
                            .null(),
                    )
                    .add_column(
                        ColumnDef::new(Nodes::PublicIngressUnsupportedRouteCount)
                            .integer()
                            .null(),
                    )
                    .add_column(
                        ColumnDef::new(Nodes::PublicIngressUnsupportedReasons)
                            .json_binary()
                            .not_null()
                            .default("[]"),
                    )
                    .to_owned(),
            )
            .await
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .alter_table(
                Table::alter()
                    .table(Nodes::Table)
                    .drop_column(Nodes::PublicIngressCertificateCount)
                    .drop_column(Nodes::PublicIngressRouteCount)
                    .drop_column(Nodes::PublicIngressUnsupportedRouteCount)
                    .drop_column(Nodes::PublicIngressUnsupportedReasons)
                    .drop_column(Nodes::PublicIngressLastError)
                    .drop_column(Nodes::PublicIngressRunning)
                    .drop_column(Nodes::PublicIngressEnabled)
                    .to_owned(),
            )
            .await
    }
}

#[derive(DeriveIden)]
enum Nodes {
    Table,
    PublicIngressEnabled,
    PublicIngressRunning,
    PublicIngressLastError,
    PublicIngressCertificateCount,
    PublicIngressRouteCount,
    PublicIngressUnsupportedRouteCount,
    PublicIngressUnsupportedReasons,
}

#[cfg(test)]
mod tests {
    use super::*;
    use sea_orm::{ConnectionTrait, Database};
    use testcontainers::{core::WaitFor, runners::AsyncRunner, GenericImage, ImageExt};

    #[tokio::test]
    async fn worker_public_ingress_migration_reverses_and_reapplies() {
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
                eprintln!("Skipping worker ingress migration test: Docker unavailable: {error}");
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
        db.execute_unprepared(
            "CREATE TABLE nodes (id INTEGER PRIMARY KEY); INSERT INTO nodes VALUES (1)",
        )
        .await
        .expect("create legacy nodes table");
        let manager = SchemaManager::new(&db);

        Migration
            .up(&manager)
            .await
            .expect("apply ingress migration");
        db.execute_unprepared(
            "UPDATE nodes SET public_ingress_enabled = TRUE, public_ingress_running = TRUE, public_ingress_certificate_count = 2, public_ingress_route_count = 3, public_ingress_unsupported_route_count = 1 WHERE id = 1",
        )
        .await
        .expect("new ingress columns are writable");
        Migration
            .down(&manager)
            .await
            .expect("reverse ingress migration");
        Migration
            .up(&manager)
            .await
            .expect("reapply ingress migration");

        let enabled: bool = db
            .query_one(sea_orm::Statement::from_string(
                sea_orm::DatabaseBackend::Postgres,
                "SELECT public_ingress_enabled FROM nodes WHERE id = 1",
            ))
            .await
            .expect("query reapplied schema")
            .expect("legacy row retained")
            .try_get("", "public_ingress_enabled")
            .expect("decode enabled flag");
        assert!(
            !enabled,
            "reapplied migration restores safe default-off state"
        );
    }
}
