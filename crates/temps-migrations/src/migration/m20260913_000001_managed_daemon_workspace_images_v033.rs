// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

const UP_SQL: &str = "ALTER TABLE ai_application_workspaces
    DROP CONSTRAINT ai_application_workspaces_image_check,
    ADD CONSTRAINT ai_application_workspaces_image_check CHECK (image IS NULL OR image IN (
        'ghcr.io/gotempsh/temps-sandbox-node:0.1.0',
        'ghcr.io/gotempsh/temps-sandbox-nodejs:0.2.0',
        'ghcr.io/gotempsh/temps-sandbox-python:0.2.0',
        'ghcr.io/gotempsh/temps-sandbox-all:0.2.0',
        'ghcr.io/gotempsh/temps-sandbox-nodejs:0.3.0',
        'ghcr.io/gotempsh/temps-sandbox-python:0.3.0',
        'ghcr.io/gotempsh/temps-sandbox-all:0.3.0',
        'ghcr.io/gotempsh/temps-sandbox-nodejs:0.3.1',
        'ghcr.io/gotempsh/temps-sandbox-python:0.3.1',
        'ghcr.io/gotempsh/temps-sandbox-all:0.3.1',
        'ghcr.io/gotempsh/temps-sandbox-nodejs:0.3.2',
        'ghcr.io/gotempsh/temps-sandbox-python:0.3.2',
        'ghcr.io/gotempsh/temps-sandbox-all:0.3.2',
        'ghcr.io/gotempsh/temps-sandbox-nodejs:0.3.3',
        'ghcr.io/gotempsh/temps-sandbox-python:0.3.3',
        'ghcr.io/gotempsh/temps-sandbox-all:0.3.3'))";

const DOWN_SQL: &str = "ALTER TABLE ai_application_workspaces
    DROP CONSTRAINT ai_application_workspaces_image_check,
    ADD CONSTRAINT ai_application_workspaces_image_check CHECK (image IS NULL OR image IN (
        'ghcr.io/gotempsh/temps-sandbox-node:0.1.0',
        'ghcr.io/gotempsh/temps-sandbox-nodejs:0.2.0',
        'ghcr.io/gotempsh/temps-sandbox-python:0.2.0',
        'ghcr.io/gotempsh/temps-sandbox-all:0.2.0',
        'ghcr.io/gotempsh/temps-sandbox-nodejs:0.3.0',
        'ghcr.io/gotempsh/temps-sandbox-python:0.3.0',
        'ghcr.io/gotempsh/temps-sandbox-all:0.3.0',
        'ghcr.io/gotempsh/temps-sandbox-nodejs:0.3.1',
        'ghcr.io/gotempsh/temps-sandbox-python:0.3.1',
        'ghcr.io/gotempsh/temps-sandbox-all:0.3.1',
        'ghcr.io/gotempsh/temps-sandbox-nodejs:0.3.2',
        'ghcr.io/gotempsh/temps-sandbox-python:0.3.2',
        'ghcr.io/gotempsh/temps-sandbox-all:0.3.2'))";

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager.get_connection().execute_unprepared(UP_SQL).await?;
        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        // PostgreSQL rejects the downgrade while any workspace selects 0.3.3.
        manager
            .get_connection()
            .execute_unprepared(DOWN_SQL)
            .await?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn v033_constraint_preserves_history_and_accepts_new_managed_images() {
        for flavor in ["nodejs", "python", "all"] {
            for version in ["0.2.0", "0.3.0", "0.3.1", "0.3.2", "0.3.3"] {
                let image = format!("ghcr.io/gotempsh/temps-sandbox-{flavor}:{version}");
                assert!(UP_SQL.contains(&image), "missing {image}");
                assert_eq!(DOWN_SQL.contains(&image), version != "0.3.3");
            }
        }
        assert!(UP_SQL.contains("ghcr.io/gotempsh/temps-sandbox-node:0.1.0"));
        assert!(!UP_SQL.contains("ghcr.io/gotempsh/temps-sandbox-nodejs:0.1.0"));
    }
}
