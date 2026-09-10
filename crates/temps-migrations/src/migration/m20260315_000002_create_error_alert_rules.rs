// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        // Create error_alert_rules table
        manager
            .create_table(
                Table::create()
                    .table(ErrorAlertRules::Table)
                    .if_not_exists()
                    .col(
                        ColumnDef::new(ErrorAlertRules::Id)
                            .integer()
                            .not_null()
                            .auto_increment()
                            .primary_key(),
                    )
                    .col(
                        ColumnDef::new(ErrorAlertRules::ProjectId)
                            .integer()
                            .not_null(),
                    )
                    .col(ColumnDef::new(ErrorAlertRules::Name).string().not_null())
                    .col(
                        ColumnDef::new(ErrorAlertRules::TriggerType)
                            .string()
                            .not_null(),
                    )
                    .col(
                        ColumnDef::new(ErrorAlertRules::TriggerConfig)
                            .json_binary()
                            .not_null()
                            .default("{}"),
                    )
                    .col(
                        ColumnDef::new(ErrorAlertRules::EnvironmentFilter)
                            .integer()
                            .null(),
                    )
                    .col(
                        ColumnDef::new(ErrorAlertRules::ErrorLevelFilter)
                            .string()
                            .null(),
                    )
                    .col(
                        ColumnDef::new(ErrorAlertRules::NotificationPriority)
                            .string()
                            .not_null()
                            .default("High"),
                    )
                    .col(
                        ColumnDef::new(ErrorAlertRules::CooldownMinutes)
                            .integer()
                            .not_null()
                            .default(30),
                    )
                    .col(
                        ColumnDef::new(ErrorAlertRules::Enabled)
                            .boolean()
                            .not_null()
                            .default(true),
                    )
                    .col(
                        ColumnDef::new(ErrorAlertRules::CreatedAt)
                            .timestamp_with_time_zone()
                            .not_null()
                            .default(Expr::current_timestamp()),
                    )
                    .col(
                        ColumnDef::new(ErrorAlertRules::UpdatedAt)
                            .timestamp_with_time_zone()
                            .not_null()
                            .default(Expr::current_timestamp()),
                    )
                    .foreign_key(
                        ForeignKey::create()
                            .name("fk_error_alert_rules_project_id")
                            .from(ErrorAlertRules::Table, ErrorAlertRules::ProjectId)
                            .to(Projects::Table, Projects::Id)
                            .on_delete(ForeignKeyAction::Cascade),
                    )
                    .to_owned(),
            )
            .await?;

        // Create error_alert_fires table
        manager
            .create_table(
                Table::create()
                    .table(ErrorAlertFires::Table)
                    .if_not_exists()
                    .col(
                        ColumnDef::new(ErrorAlertFires::Id)
                            .integer()
                            .not_null()
                            .auto_increment()
                            .primary_key(),
                    )
                    .col(ColumnDef::new(ErrorAlertFires::RuleId).integer().not_null())
                    .col(
                        ColumnDef::new(ErrorAlertFires::ErrorGroupId)
                            .integer()
                            .not_null(),
                    )
                    .col(
                        ColumnDef::new(ErrorAlertFires::FiredAt)
                            .timestamp_with_time_zone()
                            .not_null()
                            .default(Expr::current_timestamp()),
                    )
                    .col(
                        ColumnDef::new(ErrorAlertFires::NotificationSent)
                            .boolean()
                            .not_null()
                            .default(true),
                    )
                    .foreign_key(
                        ForeignKey::create()
                            .name("fk_error_alert_fires_rule_id")
                            .from(ErrorAlertFires::Table, ErrorAlertFires::RuleId)
                            .to(ErrorAlertRules::Table, ErrorAlertRules::Id)
                            .on_delete(ForeignKeyAction::Cascade),
                    )
                    .foreign_key(
                        ForeignKey::create()
                            .name("fk_error_alert_fires_error_group_id")
                            .from(ErrorAlertFires::Table, ErrorAlertFires::ErrorGroupId)
                            .to(ErrorGroups::Table, ErrorGroups::Id)
                            .on_delete(ForeignKeyAction::Cascade),
                    )
                    .to_owned(),
            )
            .await?;

        // Index for fast rule lookup by project
        manager
            .create_index(
                Index::create()
                    .name("idx_error_alert_rules_project_id")
                    .table(ErrorAlertRules::Table)
                    .col(ErrorAlertRules::ProjectId)
                    .to_owned(),
            )
            .await?;

        // Index for cooldown check: find latest fire for a rule+group pair
        manager
            .create_index(
                Index::create()
                    .name("idx_error_alert_fires_rule_group")
                    .table(ErrorAlertFires::Table)
                    .col(ErrorAlertFires::RuleId)
                    .col(ErrorAlertFires::ErrorGroupId)
                    .col(ErrorAlertFires::FiredAt)
                    .to_owned(),
            )
            .await?;

        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .drop_table(Table::drop().table(ErrorAlertFires::Table).to_owned())
            .await?;
        manager
            .drop_table(Table::drop().table(ErrorAlertRules::Table).to_owned())
            .await?;
        Ok(())
    }
}

#[derive(DeriveIden)]
enum ErrorAlertRules {
    Table,
    Id,
    ProjectId,
    Name,
    TriggerType,
    TriggerConfig,
    EnvironmentFilter,
    ErrorLevelFilter,
    NotificationPriority,
    CooldownMinutes,
    Enabled,
    CreatedAt,
    UpdatedAt,
}

#[derive(DeriveIden)]
enum ErrorAlertFires {
    Table,
    Id,
    RuleId,
    ErrorGroupId,
    FiredAt,
    NotificationSent,
}

#[derive(DeriveIden)]
enum Projects {
    Table,
    Id,
}

#[derive(DeriveIden)]
enum ErrorGroups {
    Table,
    Id,
}
