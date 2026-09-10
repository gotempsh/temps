// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Add multi-step "action plan" grouping to `ai_pending_actions`.
//!
//! A plan is an ordered chain of proposed mutations (e.g. "upgrade the
//! environment's resources, then redeploy it"). Steps are confirmed one at a
//! time in order — a step becomes confirmable only once every earlier step has
//! executed, and a failed/rejected step halts the plan (the rest are marked
//! `skipped`).
//!
//! Two additive columns:
//!   - `plan_public_id` — groups the steps of one plan; NULL for a standalone
//!     single-action proposal (a plan of one is modelled as NULL, not a group).
//!   - `step_index`     — 0-based order of the step within its plan (0 for
//!     standalone actions).
//!
//! No new status column: the existing `status` text column gains the value
//! `"skipped"` (a plain string — no enum type to alter).

use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .alter_table(
                Table::alter()
                    .table(AiPendingActions::Table)
                    .add_column(ColumnDef::new(AiPendingActions::PlanPublicId).text())
                    .add_column(
                        ColumnDef::new(AiPendingActions::StepIndex)
                            .integer()
                            .not_null()
                            .default(0),
                    )
                    .to_owned(),
            )
            .await?;

        // Steps of one plan are always fetched together and ordered; index the
        // group key + order for the plan-confirmation lookups.
        manager
            .get_connection()
            .execute_unprepared(
                "CREATE INDEX IF NOT EXISTS idx_ai_pending_actions_plan \
                 ON ai_pending_actions (plan_public_id, step_index);",
            )
            .await?;

        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .get_connection()
            .execute_unprepared("DROP INDEX IF EXISTS idx_ai_pending_actions_plan;")
            .await?;

        manager
            .alter_table(
                Table::alter()
                    .table(AiPendingActions::Table)
                    .drop_column(AiPendingActions::PlanPublicId)
                    .drop_column(AiPendingActions::StepIndex)
                    .to_owned(),
            )
            .await?;

        Ok(())
    }
}

#[derive(DeriveIden)]
enum AiPendingActions {
    Table,
    PlanPublicId,
    StepIndex,
}
