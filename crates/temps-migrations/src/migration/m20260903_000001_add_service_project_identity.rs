// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Give template-backed services an explicit product identity and durable
//! resolved release. `source_type` remains orthogonal: it describes the deploy
//! transport, while `project_type = service` describes the product lifecycle.

use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .alter_table(
                Table::alter()
                    .table(Projects::Table)
                    .add_column(
                        ColumnDef::new(Projects::ProjectType)
                            .string_len(32)
                            .not_null()
                            .default("server"),
                    )
                    .add_column(
                        ColumnDef::new(Projects::ServiceTemplate)
                            .json_binary()
                            .null(),
                    )
                    .to_owned(),
            )
            .await?;

        // Preserve the pre-existing static/server classification. Framework
        // presets that produce static output do not use the literal `static`
        // discriminator, and Nixpacks is static only when its sole selected
        // provider is `static`. Service projects are new in this migration and
        // are created explicitly by the application with an immutable template
        // snapshot.
        manager
            .get_connection()
            .execute_unprepared(
                "UPDATE projects SET project_type = 'static' \
                 WHERE preset IN ('static', 'vite', 'react', 'docusaurus', 'rsbuild') \
                    OR (preset = 'nixpacks' AND \
                        preset_config -> 'providers' = '[\"static\"]'::jsonb)",
            )
            .await?;
        manager
            .get_connection()
            .execute_unprepared(
                "ALTER TABLE projects \
                 ADD CONSTRAINT projects_service_template_identity \
                 CHECK (\
                    (project_type = 'service' AND service_template IS NOT NULL AND \
                        template_slug IS NOT NULL AND btrim(template_slug) <> '') OR \
                    (project_type IN ('server', 'static') AND service_template IS NULL)\
                 )",
            )
            .await?;
        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .get_connection()
            .execute_unprepared(
                "ALTER TABLE projects \
                 DROP CONSTRAINT IF EXISTS projects_service_template_identity",
            )
            .await?;
        manager
            .alter_table(
                Table::alter()
                    .table(Projects::Table)
                    .drop_column(Projects::ServiceTemplate)
                    .drop_column(Projects::ProjectType)
                    .to_owned(),
            )
            .await?;
        Ok(())
    }
}

#[derive(DeriveIden)]
enum Projects {
    Table,
    ProjectType,
    ServiceTemplate,
}
