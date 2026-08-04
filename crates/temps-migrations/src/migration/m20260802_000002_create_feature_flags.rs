//! Creates the feature-flag tables (ADR-034, Phase 1).
//!
//! Two tables, mirroring the `env_vars` / `env_var_environments` split: the
//! definition is project-scoped, the value is overridden per environment.
//!
//! Three columns land here without a Phase 1 reader, on purpose — each is
//! expensive or impossible to introduce correctly later:
//!
//! - `feature_flags.salt`: percentage bucketing must be stable from the first
//!   rollout. A salt added after users are already assigned reshuffles them.
//! - `feature_flags.client_visible`: defaults to `false`. Flipping a security
//!   default later would expose server-only flags that predate the change.
//! - `feature_flag_environments.rules`: the evaluator already reserves an
//!   ordered step for targeting between the kill switch and the environment
//!   value, so writing rules later cannot change what an existing flag serves.

use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .create_table(
                Table::create()
                    .table(FeatureFlags::Table)
                    .if_not_exists()
                    .col(
                        ColumnDef::new(FeatureFlags::Id)
                            .integer()
                            .not_null()
                            .auto_increment()
                            .primary_key(),
                    )
                    .col(ColumnDef::new(FeatureFlags::ProjectId).integer().not_null())
                    .col(ColumnDef::new(FeatureFlags::Key).string_len(128).not_null())
                    .col(
                        ColumnDef::new(FeatureFlags::ValueType)
                            .string_len(16)
                            .not_null(),
                    )
                    .col(
                        ColumnDef::new(FeatureFlags::DefaultValue)
                            .json_binary()
                            .not_null(),
                    )
                    .col(
                        ColumnDef::new(FeatureFlags::Description)
                            .string_len(512)
                            .null(),
                    )
                    .col(ColumnDef::new(FeatureFlags::Salt).string_len(32).not_null())
                    .col(
                        ColumnDef::new(FeatureFlags::ClientVisible)
                            .boolean()
                            .not_null()
                            .default(false),
                    )
                    .col(
                        ColumnDef::new(FeatureFlags::ArchivedAt)
                            .timestamp_with_time_zone()
                            .null(),
                    )
                    .col(
                        ColumnDef::new(FeatureFlags::CreatedAt)
                            .timestamp_with_time_zone()
                            .not_null()
                            .default(Expr::current_timestamp()),
                    )
                    .col(
                        ColumnDef::new(FeatureFlags::UpdatedAt)
                            .timestamp_with_time_zone()
                            .not_null()
                            .default(Expr::current_timestamp()),
                    )
                    .to_owned(),
            )
            .await?;

        manager
            .create_foreign_key(
                ForeignKey::create()
                    .name("fk_feature_flags_project")
                    .from(FeatureFlags::Table, FeatureFlags::ProjectId)
                    .to(Projects::Table, Projects::Id)
                    .on_delete(ForeignKeyAction::Cascade)
                    .to_owned(),
            )
            .await?;

        // Flag keys are unique per project and immutable after create.
        manager
            .create_index(
                Index::create()
                    .name("idx_feature_flags_project_key")
                    .table(FeatureFlags::Table)
                    .col(FeatureFlags::ProjectId)
                    .col(FeatureFlags::Key)
                    .unique()
                    .if_not_exists()
                    .to_owned(),
            )
            .await?;

        manager
            .create_table(
                Table::create()
                    .table(FeatureFlagEnvironments::Table)
                    .if_not_exists()
                    .col(
                        ColumnDef::new(FeatureFlagEnvironments::Id)
                            .integer()
                            .not_null()
                            .auto_increment()
                            .primary_key(),
                    )
                    .col(
                        ColumnDef::new(FeatureFlagEnvironments::FlagId)
                            .integer()
                            .not_null(),
                    )
                    .col(
                        ColumnDef::new(FeatureFlagEnvironments::EnvironmentId)
                            .integer()
                            .not_null(),
                    )
                    .col(
                        ColumnDef::new(FeatureFlagEnvironments::Enabled)
                            .boolean()
                            .not_null()
                            .default(true),
                    )
                    .col(
                        ColumnDef::new(FeatureFlagEnvironments::Value)
                            .json_binary()
                            .null(),
                    )
                    .col(
                        ColumnDef::new(FeatureFlagEnvironments::Rules)
                            .json_binary()
                            .not_null()
                            .default(Expr::cust("'[]'::jsonb")),
                    )
                    .col(
                        ColumnDef::new(FeatureFlagEnvironments::CreatedAt)
                            .timestamp_with_time_zone()
                            .not_null()
                            .default(Expr::current_timestamp()),
                    )
                    .col(
                        ColumnDef::new(FeatureFlagEnvironments::UpdatedAt)
                            .timestamp_with_time_zone()
                            .not_null()
                            .default(Expr::current_timestamp()),
                    )
                    .to_owned(),
            )
            .await?;

        manager
            .create_foreign_key(
                ForeignKey::create()
                    .name("fk_feature_flag_environments_flag")
                    .from(
                        FeatureFlagEnvironments::Table,
                        FeatureFlagEnvironments::FlagId,
                    )
                    .to(FeatureFlags::Table, FeatureFlags::Id)
                    .on_delete(ForeignKeyAction::Cascade)
                    .to_owned(),
            )
            .await?;

        manager
            .create_foreign_key(
                ForeignKey::create()
                    .name("fk_feature_flag_environments_environment")
                    .from(
                        FeatureFlagEnvironments::Table,
                        FeatureFlagEnvironments::EnvironmentId,
                    )
                    .to(Environments::Table, Environments::Id)
                    .on_delete(ForeignKeyAction::Cascade)
                    .to_owned(),
            )
            .await?;

        // One override row per (flag, environment).
        manager
            .create_index(
                Index::create()
                    .name("idx_feature_flag_environments_flag_env")
                    .table(FeatureFlagEnvironments::Table)
                    .col(FeatureFlagEnvironments::FlagId)
                    .col(FeatureFlagEnvironments::EnvironmentId)
                    .unique()
                    .if_not_exists()
                    .to_owned(),
            )
            .await?;

        // Snapshot endpoint reads every override for one environment.
        manager
            .create_index(
                Index::create()
                    .name("idx_feature_flag_environments_environment")
                    .table(FeatureFlagEnvironments::Table)
                    .col(FeatureFlagEnvironments::EnvironmentId)
                    .if_not_exists()
                    .to_owned(),
            )
            .await?;

        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .drop_table(
                Table::drop()
                    .table(FeatureFlagEnvironments::Table)
                    .if_exists()
                    .to_owned(),
            )
            .await?;

        manager
            .drop_table(
                Table::drop()
                    .table(FeatureFlags::Table)
                    .if_exists()
                    .to_owned(),
            )
            .await?;

        Ok(())
    }
}

#[derive(DeriveIden)]
enum FeatureFlags {
    Table,
    Id,
    ProjectId,
    Key,
    ValueType,
    DefaultValue,
    Description,
    Salt,
    ClientVisible,
    ArchivedAt,
    CreatedAt,
    UpdatedAt,
}

#[derive(DeriveIden)]
enum FeatureFlagEnvironments {
    Table,
    Id,
    FlagId,
    EnvironmentId,
    Enabled,
    Value,
    Rules,
    CreatedAt,
    UpdatedAt,
}

#[derive(DeriveIden)]
enum Projects {
    Table,
    Id,
}

#[derive(DeriveIden)]
enum Environments {
    Table,
    Id,
}
