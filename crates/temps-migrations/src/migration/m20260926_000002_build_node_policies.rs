// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager.get_connection().execute_unprepared(
            "CREATE TABLE build_node_policies (
                scope TEXT PRIMARY KEY,
                project_id INTEGER UNIQUE REFERENCES projects(id) ON DELETE CASCADE,
                node_ids INTEGER[] NOT NULL,
                CONSTRAINT build_node_policy_scope CHECK (
                    (project_id IS NULL AND scope = 'global') OR
                    (project_id IS NOT NULL AND project_id > 0 AND scope = 'project:' || project_id::text)
                ),
                CONSTRAINT build_node_policy_nodes CHECK (
                    cardinality(node_ids) BETWEEN 1 AND 100 AND
                    array_position(node_ids, NULL) IS NULL AND 0 < ALL(node_ids)
                )
            )"
        ).await?;
        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .get_connection()
            .execute_unprepared("DROP TABLE build_node_policies")
            .await?;
        Ok(())
    }
}
