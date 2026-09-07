// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        let db = manager.get_connection();

        db.execute_unprepared(
            r#"
ALTER TABLE s3_sources
    ADD COLUMN backing_service_id INTEGER NULL;
ALTER TABLE s3_sources
    ADD CONSTRAINT fk_s3_sources_backing_service_id
    FOREIGN KEY (backing_service_id) REFERENCES external_services(id) ON DELETE SET NULL;

ALTER TABLE backup_schedules
    ADD COLUMN generated_kind TEXT NULL;

ALTER TABLE external_service_backups
    ADD COLUMN service_name_snapshot TEXT NULL,
    ADD COLUMN service_type_snapshot TEXT NULL;
UPDATE external_service_backups esb
SET service_name_snapshot = es.name,
    service_type_snapshot = es.service_type
FROM external_services es
WHERE es.id = esb.service_id;
ALTER TABLE external_service_backups
    DROP CONSTRAINT fk_external_service_backups_service_id;
"#,
        )
        .await?;

        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        let db = manager.get_connection();

        db.execute_unprepared(
            r#"
DELETE FROM external_service_backups esb
WHERE NOT EXISTS (
    SELECT 1 FROM external_services es WHERE es.id = esb.service_id
);
ALTER TABLE external_service_backups
    ADD CONSTRAINT fk_external_service_backups_service_id
    FOREIGN KEY (service_id) REFERENCES external_services(id) ON DELETE CASCADE;
ALTER TABLE external_service_backups
    DROP COLUMN service_name_snapshot,
    DROP COLUMN service_type_snapshot;

ALTER TABLE backup_schedules DROP COLUMN generated_kind;

ALTER TABLE s3_sources DROP CONSTRAINT fk_s3_sources_backing_service_id;
ALTER TABLE s3_sources DROP COLUMN backing_service_id;
"#,
        )
        .await?;

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn migration_preserves_backup_rows_when_services_are_deleted() {
        let sql = include_str!("m20260902_000001_backup_safety_and_provenance.rs");
        assert!(sql.contains("DROP CONSTRAINT fk_external_service_backups_service_id"));
        assert!(sql.contains("service_name_snapshot"));
        assert!(sql.contains("service_type_snapshot"));
        assert!(sql.contains("ON DELETE SET NULL"));
    }
}
