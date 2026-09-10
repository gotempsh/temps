// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        // ── Step 1: Create project_agents table ──────────────────────────────
        manager
            .create_table(
                Table::create()
                    .table(ProjectAgents::Table)
                    .if_not_exists()
                    .col(
                        ColumnDef::new(ProjectAgents::Id)
                            .integer()
                            .not_null()
                            .auto_increment()
                            .primary_key(),
                    )
                    .col(
                        ColumnDef::new(ProjectAgents::ProjectId)
                            .integer()
                            .not_null(),
                    )
                    .col(
                        ColumnDef::new(ProjectAgents::Slug)
                            .string_len(100)
                            .not_null(),
                    )
                    .col(
                        ColumnDef::new(ProjectAgents::Name)
                            .string_len(200)
                            .not_null(),
                    )
                    .col(ColumnDef::new(ProjectAgents::Description).text().null())
                    .col(
                        ColumnDef::new(ProjectAgents::Source)
                            .string_len(20)
                            .not_null()
                            .default("dashboard"),
                    )
                    .col(
                        ColumnDef::new(ProjectAgents::Enabled)
                            .boolean()
                            .not_null()
                            .default(true),
                    )
                    .col(
                        ColumnDef::new(ProjectAgents::TriggerConfig)
                            .json_binary()
                            .not_null()
                            .default("{}"),
                    )
                    .col(ColumnDef::new(ProjectAgents::Prompt).text().null())
                    .col(
                        ColumnDef::new(ProjectAgents::AiProvider)
                            .string_len(50)
                            .not_null()
                            .default("claude_cli"),
                    )
                    .col(ColumnDef::new(ProjectAgents::ApiKeyEncrypted).text().null())
                    .col(
                        ColumnDef::new(ProjectAgents::AiProviderKeyId)
                            .integer()
                            .null(),
                    )
                    .col(
                        ColumnDef::new(ProjectAgents::MaxTurns)
                            .integer()
                            .not_null()
                            .default(25),
                    )
                    .col(
                        ColumnDef::new(ProjectAgents::TimeoutSeconds)
                            .integer()
                            .not_null()
                            .default(600),
                    )
                    .col(
                        ColumnDef::new(ProjectAgents::DailyBudgetCents)
                            .integer()
                            .not_null()
                            .default(500),
                    )
                    .col(
                        ColumnDef::new(ProjectAgents::CooldownMinutes)
                            .integer()
                            .not_null()
                            .default(30),
                    )
                    .col(
                        ColumnDef::new(ProjectAgents::BranchPrefix)
                            .string_len(100)
                            .not_null()
                            .default("agents/"),
                    )
                    .col(
                        ColumnDef::new(ProjectAgents::Deliverable)
                            .string_len(20)
                            .not_null()
                            .default("pull_request"),
                    )
                    .col(
                        ColumnDef::new(ProjectAgents::CreatedAt)
                            .timestamp_with_time_zone()
                            .not_null()
                            .default(Expr::current_timestamp()),
                    )
                    .col(
                        ColumnDef::new(ProjectAgents::UpdatedAt)
                            .timestamp_with_time_zone()
                            .not_null()
                            .default(Expr::current_timestamp()),
                    )
                    .foreign_key(
                        ForeignKey::create()
                            .name("fk_project_agents_project_id")
                            .from(ProjectAgents::Table, ProjectAgents::ProjectId)
                            .to(Projects::Table, Projects::Id)
                            .on_delete(ForeignKeyAction::Cascade),
                    )
                    .foreign_key(
                        ForeignKey::create()
                            .name("fk_project_agents_ai_provider_key_id")
                            .from(ProjectAgents::Table, ProjectAgents::AiProviderKeyId)
                            .to(AiProviderKeys::Table, AiProviderKeys::Id)
                            .on_delete(ForeignKeyAction::SetNull),
                    )
                    .to_owned(),
            )
            .await?;

        // Unique: one agent slug per project
        manager
            .create_index(
                Index::create()
                    .name("idx_project_agents_project_slug")
                    .table(ProjectAgents::Table)
                    .col(ProjectAgents::ProjectId)
                    .col(ProjectAgents::Slug)
                    .unique()
                    .to_owned(),
            )
            .await?;

        manager
            .create_index(
                Index::create()
                    .name("idx_project_agents_project_id")
                    .table(ProjectAgents::Table)
                    .col(ProjectAgents::ProjectId)
                    .to_owned(),
            )
            .await?;

        // ── Step 2: Migrate autopilot_configs → project_agents ───────────────
        let db = manager.get_connection();
        db.execute_unprepared(
            r#"
            INSERT INTO project_agents (
                project_id, slug, name, description, source, enabled,
                trigger_config, prompt,
                ai_provider, api_key_encrypted, ai_provider_key_id,
                max_turns, timeout_seconds,
                daily_budget_cents, cooldown_minutes,
                branch_prefix, deliverable,
                created_at, updated_at
            )
            SELECT
                project_id,
                'error-fixer',
                'Error Fixer',
                'Automatically fixes production errors',
                'dashboard',
                enabled,
                jsonb_build_object(
                    'error', jsonb_build_object(
                        'new_issue', trigger_on_new_error,
                        'regression', trigger_on_regression
                    ),
                    'schedule', jsonb_build_object('cron', null::text),
                    'manual', true
                ),
                NULL,
                ai_provider,
                api_key_encrypted,
                ai_provider_key_id,
                max_turns_per_run,
                600,
                daily_budget_cents,
                cooldown_minutes,
                CASE WHEN branch_prefix = '' THEN 'agents/error-fixer/' ELSE branch_prefix END,
                'pull_request',
                created_at,
                updated_at
            FROM autopilot_configs
            "#,
        )
        .await?;

        // ── Step 3: Rename autopilot_runs → agent_runs ───────────────────────
        db.execute_unprepared("ALTER TABLE autopilot_runs RENAME TO agent_runs")
            .await?;

        // Add agent_id column
        manager
            .alter_table(
                Table::alter()
                    .table(AgentRuns::Table)
                    .add_column(ColumnDef::new(AgentRuns::AgentId).integer().null())
                    .add_foreign_key(
                        &TableForeignKey::new()
                            .name("fk_agent_runs_agent_id")
                            .from_tbl(AgentRuns::Table)
                            .from_col(AgentRuns::AgentId)
                            .to_tbl(ProjectAgents::Table)
                            .to_col(ProjectAgents::Id)
                            .on_delete(ForeignKeyAction::SetNull)
                            .to_owned(),
                    )
                    .to_owned(),
            )
            .await?;

        // Populate agent_id from migrated project_agents
        db.execute_unprepared(
            r#"
            UPDATE agent_runs r
            SET agent_id = pa.id
            FROM project_agents pa
            WHERE r.project_id = pa.project_id
              AND pa.slug = 'error-fixer'
            "#,
        )
        .await?;

        manager
            .create_index(
                Index::create()
                    .name("idx_agent_runs_agent_id")
                    .table(AgentRuns::Table)
                    .col(AgentRuns::AgentId)
                    .to_owned(),
            )
            .await?;

        // ── Step 4: Rename autopilot_run_logs → agent_run_logs ───────────────
        db.execute_unprepared("ALTER TABLE autopilot_run_logs RENAME TO agent_run_logs")
            .await?;

        // ── Step 5: Rename indexes to match new table names ──────────────────
        // (Postgres keeps old index names after ALTER TABLE RENAME)
        db.execute_unprepared(
            "ALTER INDEX IF EXISTS idx_autopilot_runs_project_id RENAME TO idx_agent_runs_project_id",
        )
        .await?;
        db.execute_unprepared(
            "ALTER INDEX IF EXISTS idx_autopilot_runs_status RENAME TO idx_agent_runs_status",
        )
        .await?;
        db.execute_unprepared(
            "ALTER INDEX IF EXISTS idx_autopilot_runs_trigger_source RENAME TO idx_agent_runs_trigger_source",
        )
        .await?;
        db.execute_unprepared(
            "ALTER INDEX IF EXISTS idx_autopilot_run_logs_run_id_created_at RENAME TO idx_agent_run_logs_run_id_created_at",
        )
        .await?;

        // ── Step 6: Drop FK from agent_runs → autopilot_configs, then drop the table ──
        // The renamed agent_runs table still has the old FK constraint
        db.execute_unprepared(
            "ALTER TABLE agent_runs DROP CONSTRAINT IF EXISTS fk_autopilot_runs_config_id",
        )
        .await?;

        manager
            .drop_table(Table::drop().table(AutopilotConfigs::Table).to_owned())
            .await?;

        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        let db = manager.get_connection();

        // Recreate autopilot_configs (simplified — full rollback would need original schema)
        // For now just drop the new tables and rename back
        db.execute_unprepared("ALTER TABLE agent_run_logs RENAME TO autopilot_run_logs")
            .await?;

        // Drop agent_id column from agent_runs before renaming
        manager
            .alter_table(
                Table::alter()
                    .table(AgentRuns::Table)
                    .drop_column(AgentRuns::AgentId)
                    .to_owned(),
            )
            .await?;

        db.execute_unprepared("ALTER TABLE agent_runs RENAME TO autopilot_runs")
            .await?;

        manager
            .drop_table(Table::drop().table(ProjectAgents::Table).to_owned())
            .await?;

        Ok(())
    }
}

#[derive(DeriveIden)]
enum ProjectAgents {
    Table,
    Id,
    ProjectId,
    Slug,
    Name,
    Description,
    Source,
    Enabled,
    TriggerConfig,
    Prompt,
    AiProvider,
    ApiKeyEncrypted,
    AiProviderKeyId,
    MaxTurns,
    TimeoutSeconds,
    DailyBudgetCents,
    CooldownMinutes,
    BranchPrefix,
    Deliverable,
    CreatedAt,
    UpdatedAt,
}

#[derive(DeriveIden)]
enum AgentRuns {
    Table,
    AgentId,
}

#[derive(DeriveIden)]
enum AutopilotConfigs {
    Table,
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
