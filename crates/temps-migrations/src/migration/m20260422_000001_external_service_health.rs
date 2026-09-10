// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

use sea_orm::{ConnectionTrait, Statement};
use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        // 1. Add health-status columns on external_services
        manager
            .alter_table(
                Table::alter()
                    .table(ExternalServices::Table)
                    .add_column(
                        ColumnDef::new(ExternalServices::HealthStatus)
                            .string_len(20)
                            .null(),
                    )
                    .to_owned(),
            )
            .await?;

        manager
            .alter_table(
                Table::alter()
                    .table(ExternalServices::Table)
                    .add_column(
                        ColumnDef::new(ExternalServices::LastHealthCheckAt)
                            .timestamp_with_time_zone()
                            .null(),
                    )
                    .to_owned(),
            )
            .await?;

        manager
            .alter_table(
                Table::alter()
                    .table(ExternalServices::Table)
                    .add_column(
                        ColumnDef::new(ExternalServices::LastHealthError)
                            .text()
                            .null(),
                    )
                    .to_owned(),
            )
            .await?;

        manager
            .alter_table(
                Table::alter()
                    .table(ExternalServices::Table)
                    .add_column(
                        ColumnDef::new(ExternalServices::ConsecutiveHealthFailures)
                            .integer()
                            .not_null()
                            .default(0),
                    )
                    .to_owned(),
            )
            .await?;

        // 2. Create health check history table
        manager
            .create_table(
                Table::create()
                    .table(ExternalServiceHealthChecks::Table)
                    .if_not_exists()
                    .col(
                        ColumnDef::new(ExternalServiceHealthChecks::Id)
                            .big_integer()
                            .not_null()
                            .auto_increment()
                            .primary_key(),
                    )
                    .col(
                        ColumnDef::new(ExternalServiceHealthChecks::ServiceId)
                            .integer()
                            .not_null(),
                    )
                    .col(
                        ColumnDef::new(ExternalServiceHealthChecks::CheckedAt)
                            .timestamp_with_time_zone()
                            .not_null()
                            .default(Expr::current_timestamp()),
                    )
                    .col(
                        ColumnDef::new(ExternalServiceHealthChecks::Status)
                            .string_len(20)
                            .not_null(),
                    )
                    .col(
                        ColumnDef::new(ExternalServiceHealthChecks::ResponseTimeMs)
                            .integer()
                            .null(),
                    )
                    .col(
                        ColumnDef::new(ExternalServiceHealthChecks::ErrorMessage)
                            .text()
                            .null(),
                    )
                    .foreign_key(
                        ForeignKey::create()
                            .name("fk_external_service_health_checks_service")
                            .from(
                                ExternalServiceHealthChecks::Table,
                                ExternalServiceHealthChecks::ServiceId,
                            )
                            .to(ExternalServices::Table, ExternalServices::Id)
                            .on_delete(ForeignKeyAction::Cascade),
                    )
                    .to_owned(),
            )
            .await?;

        // Hot index for "latest check per service" and "last N for chart"
        manager
            .create_index(
                Index::create()
                    .name("idx_external_service_health_checks_service_time")
                    .table(ExternalServiceHealthChecks::Table)
                    .col(ExternalServiceHealthChecks::ServiceId)
                    .col((ExternalServiceHealthChecks::CheckedAt, IndexOrder::Desc))
                    .to_owned(),
            )
            .await?;

        // Retention helper: drop checks older than 30 days via a cleanup query.
        // We intentionally do NOT create a TimescaleDB hypertable here — the
        // insert volume is low (one row per service per 30s) and the id PK is
        // needed by Sea-ORM's ActiveModel API. A scheduled DELETE is enough.
        let db = manager.get_connection();
        db.execute(Statement::from_string(
            db.get_database_backend(),
            "CREATE INDEX IF NOT EXISTS idx_external_service_health_checks_status \
             ON external_service_health_checks (status) \
             WHERE status != 'operational'"
                .to_string(),
        ))
        .await?;

        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .drop_table(
                Table::drop()
                    .table(ExternalServiceHealthChecks::Table)
                    .to_owned(),
            )
            .await?;

        for col in [
            ExternalServices::HealthStatus,
            ExternalServices::LastHealthCheckAt,
            ExternalServices::LastHealthError,
            ExternalServices::ConsecutiveHealthFailures,
        ] {
            manager
                .alter_table(
                    Table::alter()
                        .table(ExternalServices::Table)
                        .drop_column(col)
                        .to_owned(),
                )
                .await?;
        }

        Ok(())
    }
}

#[derive(DeriveIden)]
enum ExternalServices {
    Table,
    Id,
    HealthStatus,
    LastHealthCheckAt,
    LastHealthError,
    ConsecutiveHealthFailures,
}

#[derive(DeriveIden)]
enum ExternalServiceHealthChecks {
    Table,
    Id,
    ServiceId,
    CheckedAt,
    Status,
    ResponseTimeMs,
    ErrorMessage,
}
