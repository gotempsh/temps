use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        // Create autopilot_configs table (one per project)
        manager
            .create_table(
                Table::create()
                    .table(AutopilotConfigs::Table)
                    .if_not_exists()
                    .col(
                        ColumnDef::new(AutopilotConfigs::Id)
                            .integer()
                            .not_null()
                            .auto_increment()
                            .primary_key(),
                    )
                    .col(
                        ColumnDef::new(AutopilotConfigs::ProjectId)
                            .integer()
                            .not_null(),
                    )
                    .col(
                        ColumnDef::new(AutopilotConfigs::Enabled)
                            .boolean()
                            .not_null()
                            .default(false),
                    )
                    .col(
                        ColumnDef::new(AutopilotConfigs::AiProvider)
                            .string_len(50)
                            .not_null(),
                    )
                    .col(
                        ColumnDef::new(AutopilotConfigs::ApiKeyEncrypted)
                            .text()
                            .null(),
                    )
                    .col(
                        ColumnDef::new(AutopilotConfigs::AiProviderKeyId)
                            .integer()
                            .null(),
                    )
                    .col(
                        ColumnDef::new(AutopilotConfigs::DailyBudgetCents)
                            .integer()
                            .not_null()
                            .default(500),
                    )
                    .col(
                        ColumnDef::new(AutopilotConfigs::MaxTurnsPerRun)
                            .integer()
                            .not_null()
                            .default(25),
                    )
                    .col(
                        ColumnDef::new(AutopilotConfigs::CooldownMinutes)
                            .integer()
                            .not_null()
                            .default(30),
                    )
                    .col(
                        ColumnDef::new(AutopilotConfigs::TriggerOnNewError)
                            .boolean()
                            .not_null()
                            .default(true),
                    )
                    .col(
                        ColumnDef::new(AutopilotConfigs::TriggerOnRegression)
                            .boolean()
                            .not_null()
                            .default(true),
                    )
                    .col(
                        ColumnDef::new(AutopilotConfigs::TriggerOnAlarm)
                            .boolean()
                            .not_null()
                            .default(false),
                    )
                    .col(
                        ColumnDef::new(AutopilotConfigs::BranchPrefix)
                            .string_len(100)
                            .not_null()
                            .default("autopilot/"),
                    )
                    .col(
                        ColumnDef::new(AutopilotConfigs::CreatedAt)
                            .timestamp_with_time_zone()
                            .not_null()
                            .default(Expr::current_timestamp()),
                    )
                    .col(
                        ColumnDef::new(AutopilotConfigs::UpdatedAt)
                            .timestamp_with_time_zone()
                            .not_null()
                            .default(Expr::current_timestamp()),
                    )
                    .foreign_key(
                        ForeignKey::create()
                            .name("fk_autopilot_configs_project_id")
                            .from(AutopilotConfigs::Table, AutopilotConfigs::ProjectId)
                            .to(Projects::Table, Projects::Id)
                            .on_delete(ForeignKeyAction::Cascade),
                    )
                    .foreign_key(
                        ForeignKey::create()
                            .name("fk_autopilot_configs_ai_provider_key_id")
                            .from(AutopilotConfigs::Table, AutopilotConfigs::AiProviderKeyId)
                            .to(AiProviderKeys::Table, AiProviderKeys::Id)
                            .on_delete(ForeignKeyAction::SetNull),
                    )
                    .to_owned(),
            )
            .await?;

        // Unique index: one config per project
        manager
            .create_index(
                Index::create()
                    .name("idx_autopilot_configs_project_id")
                    .table(AutopilotConfigs::Table)
                    .col(AutopilotConfigs::ProjectId)
                    .unique()
                    .if_not_exists()
                    .to_owned(),
            )
            .await?;

        // Create autopilot_runs table (execution history)
        manager
            .create_table(
                Table::create()
                    .table(AutopilotRuns::Table)
                    .if_not_exists()
                    .col(
                        ColumnDef::new(AutopilotRuns::Id)
                            .integer()
                            .not_null()
                            .auto_increment()
                            .primary_key(),
                    )
                    .col(
                        ColumnDef::new(AutopilotRuns::ProjectId)
                            .integer()
                            .not_null(),
                    )
                    .col(ColumnDef::new(AutopilotRuns::ConfigId).integer().not_null())
                    .col(
                        ColumnDef::new(AutopilotRuns::TriggerType)
                            .string_len(50)
                            .not_null(),
                    )
                    .col(
                        ColumnDef::new(AutopilotRuns::TriggerSourceId)
                            .integer()
                            .null(),
                    )
                    .col(
                        ColumnDef::new(AutopilotRuns::TriggerSourceType)
                            .string_len(50)
                            .null(),
                    )
                    .col(
                        ColumnDef::new(AutopilotRuns::Status)
                            .string_len(30)
                            .not_null()
                            .default("pending"),
                    )
                    .col(
                        ColumnDef::new(AutopilotRuns::BranchName)
                            .string_len(255)
                            .null(),
                    )
                    .col(
                        ColumnDef::new(AutopilotRuns::CommitSha)
                            .string_len(64)
                            .null(),
                    )
                    .col(ColumnDef::new(AutopilotRuns::PrUrl).string_len(500).null())
                    .col(ColumnDef::new(AutopilotRuns::PrNumber).integer().null())
                    .col(
                        ColumnDef::new(AutopilotRuns::PreviewUrl)
                            .string_len(500)
                            .null(),
                    )
                    .col(
                        ColumnDef::new(AutopilotRuns::PreviewDeploymentId)
                            .integer()
                            .null(),
                    )
                    .col(ColumnDef::new(AutopilotRuns::ErrorMessage).text().null())
                    .col(ColumnDef::new(AutopilotRuns::AiOutput).text().null())
                    .col(ColumnDef::new(AutopilotRuns::AiReasoning).text().null())
                    .col(
                        ColumnDef::new(AutopilotRuns::AiModel)
                            .string_len(100)
                            .null(),
                    )
                    .col(
                        ColumnDef::new(AutopilotRuns::TokensInput)
                            .integer()
                            .not_null()
                            .default(0),
                    )
                    .col(
                        ColumnDef::new(AutopilotRuns::TokensOutput)
                            .integer()
                            .not_null()
                            .default(0),
                    )
                    .col(
                        ColumnDef::new(AutopilotRuns::EstimatedCostCents)
                            .integer()
                            .not_null()
                            .default(0),
                    )
                    .col(
                        ColumnDef::new(AutopilotRuns::FilesChanged)
                            .integer()
                            .not_null()
                            .default(0),
                    )
                    .col(
                        ColumnDef::new(AutopilotRuns::StartedAt)
                            .timestamp_with_time_zone()
                            .null(),
                    )
                    .col(
                        ColumnDef::new(AutopilotRuns::CompletedAt)
                            .timestamp_with_time_zone()
                            .null(),
                    )
                    .col(
                        ColumnDef::new(AutopilotRuns::CreatedAt)
                            .timestamp_with_time_zone()
                            .not_null()
                            .default(Expr::current_timestamp()),
                    )
                    .foreign_key(
                        ForeignKey::create()
                            .name("fk_autopilot_runs_project_id")
                            .from(AutopilotRuns::Table, AutopilotRuns::ProjectId)
                            .to(Projects::Table, Projects::Id)
                            .on_delete(ForeignKeyAction::Cascade),
                    )
                    .foreign_key(
                        ForeignKey::create()
                            .name("fk_autopilot_runs_config_id")
                            .from(AutopilotRuns::Table, AutopilotRuns::ConfigId)
                            .to(AutopilotConfigs::Table, AutopilotConfigs::Id)
                            .on_delete(ForeignKeyAction::Cascade),
                    )
                    .to_owned(),
            )
            .await?;

        // Indexes for autopilot_runs
        manager
            .create_index(
                Index::create()
                    .name("idx_autopilot_runs_project_id")
                    .table(AutopilotRuns::Table)
                    .col(AutopilotRuns::ProjectId)
                    .if_not_exists()
                    .to_owned(),
            )
            .await?;

        manager
            .create_index(
                Index::create()
                    .name("idx_autopilot_runs_status")
                    .table(AutopilotRuns::Table)
                    .col(AutopilotRuns::Status)
                    .if_not_exists()
                    .to_owned(),
            )
            .await?;

        manager
            .create_index(
                Index::create()
                    .name("idx_autopilot_runs_trigger_source")
                    .table(AutopilotRuns::Table)
                    .col(AutopilotRuns::TriggerSourceType)
                    .col(AutopilotRuns::TriggerSourceId)
                    .if_not_exists()
                    .to_owned(),
            )
            .await?;

        // Create autopilot_run_logs table (event log per run)
        manager
            .create_table(
                Table::create()
                    .table(AutopilotRunLogs::Table)
                    .if_not_exists()
                    .col(
                        ColumnDef::new(AutopilotRunLogs::Id)
                            .big_integer()
                            .not_null()
                            .auto_increment()
                            .primary_key(),
                    )
                    .col(ColumnDef::new(AutopilotRunLogs::RunId).integer().not_null())
                    .col(
                        ColumnDef::new(AutopilotRunLogs::Level)
                            .string_len(20)
                            .not_null(),
                    )
                    .col(ColumnDef::new(AutopilotRunLogs::Message).text().not_null())
                    .col(
                        ColumnDef::new(AutopilotRunLogs::Metadata)
                            .json_binary()
                            .null(),
                    )
                    .col(
                        ColumnDef::new(AutopilotRunLogs::CreatedAt)
                            .timestamp_with_time_zone()
                            .not_null()
                            .default(Expr::current_timestamp()),
                    )
                    .foreign_key(
                        ForeignKey::create()
                            .name("fk_autopilot_run_logs_run_id")
                            .from(AutopilotRunLogs::Table, AutopilotRunLogs::RunId)
                            .to(AutopilotRuns::Table, AutopilotRuns::Id)
                            .on_delete(ForeignKeyAction::Cascade),
                    )
                    .to_owned(),
            )
            .await?;

        manager
            .create_index(
                Index::create()
                    .name("idx_autopilot_run_logs_run_id_created_at")
                    .table(AutopilotRunLogs::Table)
                    .col(AutopilotRunLogs::RunId)
                    .col(AutopilotRunLogs::CreatedAt)
                    .if_not_exists()
                    .to_owned(),
            )
            .await?;

        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        // IF EXISTS on all three: m20260401_000001 renames/drops these
        // tables (autopilot -> agents) and its down() does not restore
        // autopilot_configs, so a full rollback reaches this point with
        // some of them already gone.
        manager
            .drop_table(
                Table::drop()
                    .table(AutopilotRunLogs::Table)
                    .if_exists()
                    .to_owned(),
            )
            .await?;
        manager
            .drop_table(
                Table::drop()
                    .table(AutopilotRuns::Table)
                    .if_exists()
                    .to_owned(),
            )
            .await?;
        manager
            .drop_table(
                Table::drop()
                    .table(AutopilotConfigs::Table)
                    .if_exists()
                    .to_owned(),
            )
            .await?;
        Ok(())
    }
}

#[derive(DeriveIden)]
enum AutopilotConfigs {
    Table,
    Id,
    ProjectId,
    Enabled,
    AiProvider,
    ApiKeyEncrypted,
    AiProviderKeyId,
    DailyBudgetCents,
    MaxTurnsPerRun,
    CooldownMinutes,
    TriggerOnNewError,
    TriggerOnRegression,
    TriggerOnAlarm,
    BranchPrefix,
    CreatedAt,
    UpdatedAt,
}

#[derive(DeriveIden)]
enum AutopilotRuns {
    Table,
    Id,
    ProjectId,
    ConfigId,
    TriggerType,
    TriggerSourceId,
    TriggerSourceType,
    Status,
    BranchName,
    CommitSha,
    PrUrl,
    PrNumber,
    PreviewUrl,
    PreviewDeploymentId,
    ErrorMessage,
    AiOutput,
    AiReasoning,
    AiModel,
    TokensInput,
    TokensOutput,
    EstimatedCostCents,
    FilesChanged,
    StartedAt,
    CompletedAt,
    CreatedAt,
}

#[derive(DeriveIden)]
enum AutopilotRunLogs {
    Table,
    Id,
    RunId,
    Level,
    Message,
    Metadata,
    CreatedAt,
}

#[derive(DeriveIden)]
enum Projects {
    Table,
    Id,
}

#[derive(DeriveIden)]
enum AiProviderKeys {
    Table,
    Id,
}
