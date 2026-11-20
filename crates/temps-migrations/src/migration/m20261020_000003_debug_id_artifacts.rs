// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0
use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .get_connection()
            .execute_unprepared(
                r#"
            ALTER TABLE source_maps ADD COLUMN debug_id TEXT,
                ADD COLUMN artifact_bundle TEXT, ADD COLUMN artifact_bundle_count INTEGER;
            DROP INDEX idx_source_maps_project_release_file;
            CREATE UNIQUE INDEX idx_source_maps_project_release_file
                ON source_maps(project_id,release,file_path,COALESCE(debug_id,''));
            CREATE UNIQUE INDEX source_maps_debug_id ON source_maps(project_id,debug_id)
                WHERE debug_id IS NOT NULL;
            CREATE INDEX source_maps_artifact_bundle ON source_maps(project_id,artifact_bundle)
                WHERE artifact_bundle IS NOT NULL;
            CREATE TABLE source_map_upload_chunks (
                project_id INTEGER NOT NULL REFERENCES projects(id) ON DELETE CASCADE,
                checksum VARCHAR(40) NOT NULL,
                data BYTEA NOT NULL CHECK(octet_length(data) BETWEEN 1 AND 8388608),
                created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
                PRIMARY KEY(project_id,checksum)
            );
            CREATE INDEX source_map_upload_chunks_expiry ON source_map_upload_chunks(created_at);
        "#,
            )
            .await?;
        Ok(())
    }
    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager.get_connection().execute_unprepared(r#"
            DROP TABLE source_map_upload_chunks;
            DROP INDEX source_maps_artifact_bundle;
            DROP INDEX source_maps_debug_id;
            DROP INDEX idx_source_maps_project_release_file;
            CREATE UNIQUE INDEX idx_source_maps_project_release_file ON source_maps(project_id,release,file_path);
            ALTER TABLE source_maps DROP COLUMN artifact_bundle_count,
                DROP COLUMN artifact_bundle, DROP COLUMN debug_id;
        "#).await?;
        Ok(())
    }
}
