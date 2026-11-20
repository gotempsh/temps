// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! The architecture blueprint of an AI workspace (ADR-053): one per workspace,
//! versioned. The model proposes a version, the user accepts or rejects it, and
//! every chat of the workspace builds against the accepted one.
//!
//! A version's document never changes once written; deciding it only changes
//! its status. Which project or service realises a part of the design is kept
//! on the blueprint itself (`bindings`), so binding a part does not need a new
//! version.

use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .create_table(
                Table::create()
                    .table(AiBlueprints::Table)
                    .if_not_exists()
                    .col(
                        ColumnDef::new(AiBlueprints::Id)
                            .big_integer()
                            .not_null()
                            .auto_increment()
                            .primary_key(),
                    )
                    .col(
                        ColumnDef::new(AiBlueprints::ApplicationId)
                            .big_integer()
                            .not_null()
                            .unique_key(),
                    )
                    .col(ColumnDef::new(AiBlueprints::AcceptedVersion).integer())
                    .col(
                        ColumnDef::new(AiBlueprints::Bindings)
                            .json_binary()
                            .not_null()
                            .default(Expr::cust("'{}'::jsonb")),
                    )
                    .col(
                        ColumnDef::new(AiBlueprints::CreatedAt)
                            .timestamp_with_time_zone()
                            .not_null()
                            .default(Expr::current_timestamp()),
                    )
                    .col(
                        ColumnDef::new(AiBlueprints::UpdatedAt)
                            .timestamp_with_time_zone()
                            .not_null()
                            .default(Expr::current_timestamp()),
                    )
                    .foreign_key(
                        ForeignKey::create()
                            .name("fk_ai_blueprints_application")
                            .from(AiBlueprints::Table, AiBlueprints::ApplicationId)
                            .to(AiApplications::Table, AiApplications::Id)
                            .on_delete(ForeignKeyAction::Cascade),
                    )
                    .to_owned(),
            )
            .await?;

        manager
            .create_table(
                Table::create()
                    .table(AiBlueprintVersions::Table)
                    .if_not_exists()
                    .col(
                        ColumnDef::new(AiBlueprintVersions::Id)
                            .big_integer()
                            .not_null()
                            .auto_increment()
                            .primary_key(),
                    )
                    .col(
                        ColumnDef::new(AiBlueprintVersions::BlueprintId)
                            .big_integer()
                            .not_null(),
                    )
                    .col(
                        ColumnDef::new(AiBlueprintVersions::Version)
                            .integer()
                            .not_null(),
                    )
                    .col(
                        ColumnDef::new(AiBlueprintVersions::Document)
                            .json_binary()
                            .not_null(),
                    )
                    .col(
                        ColumnDef::new(AiBlueprintVersions::ChangeSummary)
                            .string_len(500)
                            .not_null(),
                    )
                    .col(
                        ColumnDef::new(AiBlueprintVersions::Status)
                            .string_len(16)
                            .not_null(),
                    )
                    .col(ColumnDef::new(AiBlueprintVersions::ProposedIn).big_integer())
                    .col(
                        ColumnDef::new(AiBlueprintVersions::CreatedBy)
                            .integer()
                            .not_null(),
                    )
                    .col(ColumnDef::new(AiBlueprintVersions::DecidedBy).integer())
                    .col(ColumnDef::new(AiBlueprintVersions::DecidedAt).timestamp_with_time_zone())
                    .col(
                        ColumnDef::new(AiBlueprintVersions::CreatedAt)
                            .timestamp_with_time_zone()
                            .not_null()
                            .default(Expr::current_timestamp()),
                    )
                    .foreign_key(
                        ForeignKey::create()
                            .name("fk_ai_blueprint_versions_blueprint")
                            .from(AiBlueprintVersions::Table, AiBlueprintVersions::BlueprintId)
                            .to(AiBlueprints::Table, AiBlueprints::Id)
                            .on_delete(ForeignKeyAction::Cascade),
                    )
                    .foreign_key(
                        ForeignKey::create()
                            .name("fk_ai_blueprint_versions_conversation")
                            .from(AiBlueprintVersions::Table, AiBlueprintVersions::ProposedIn)
                            .to(AiConversations::Table, AiConversations::Id)
                            .on_delete(ForeignKeyAction::SetNull),
                    )
                    .to_owned(),
            )
            .await?;

        manager
            .create_index(
                Index::create()
                    .name("idx_ai_blueprint_versions_blueprint_version")
                    .table(AiBlueprintVersions::Table)
                    .col(AiBlueprintVersions::BlueprintId)
                    .col(AiBlueprintVersions::Version)
                    .unique()
                    .to_owned(),
            )
            .await?;

        // A status outside the four is a bug, and at most one version of a
        // blueprint waits for the user at a time.
        let db = manager.get_connection();
        db.execute_unprepared(
            "ALTER TABLE ai_blueprint_versions \
               ADD CONSTRAINT ai_blueprint_versions_status_check \
               CHECK (status IN ('proposed', 'accepted', 'rejected', 'superseded'))",
        )
        .await?;
        db.execute_unprepared(
            "CREATE UNIQUE INDEX IF NOT EXISTS idx_ai_blueprint_versions_one_proposal \
               ON ai_blueprint_versions (blueprint_id) WHERE status = 'proposed'",
        )
        .await?;
        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .drop_table(Table::drop().table(AiBlueprintVersions::Table).to_owned())
            .await?;
        manager
            .drop_table(Table::drop().table(AiBlueprints::Table).to_owned())
            .await
    }
}

#[derive(DeriveIden)]
enum AiBlueprints {
    Table,
    Id,
    ApplicationId,
    AcceptedVersion,
    Bindings,
    CreatedAt,
    UpdatedAt,
}

#[derive(DeriveIden)]
enum AiBlueprintVersions {
    Table,
    Id,
    BlueprintId,
    Version,
    Document,
    ChangeSummary,
    Status,
    ProposedIn,
    CreatedBy,
    DecidedBy,
    DecidedAt,
    CreatedAt,
}

#[derive(DeriveIden)]
enum AiApplications {
    Table,
    Id,
}

#[derive(DeriveIden)]
enum AiConversations {
    Table,
    Id,
}
