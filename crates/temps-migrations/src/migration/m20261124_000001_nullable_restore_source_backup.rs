// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Allow restores of backups discovered in object storage without a local row.
//! The foreign key remains in place for locally tracked backups. Downgrade is
//! refused while orphan restore runs exist because they cannot satisfy NOT NULL.

use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        let db = manager.get_connection();
        db.execute_unprepared(
            "ALTER TABLE restore_runs ALTER COLUMN source_backup_id DROP NOT NULL",
        )
        .await?;
        db.execute_unprepared(
            "UPDATE restore_runs SET source_backup_id = NULL WHERE source_backup_id = 0",
        )
        .await?;
        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .get_connection()
            .execute_unprepared(
                "ALTER TABLE restore_runs ALTER COLUMN source_backup_id SET NOT NULL",
            )
            .await?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use temps_test_utils::TestDatabase;

    #[tokio::test]
    async fn orphan_history_keeps_tracked_backup_foreign_keys(
    ) -> Result<(), Box<dyn std::error::Error>> {
        let Ok(docker) = bollard::Docker::connect_with_local_defaults() else {
            eprintln!("Docker unavailable, skipping restore history migration test");
            return Ok(());
        };
        if docker.ping().await.is_err() {
            eprintln!("Docker unavailable, skipping restore history migration test");
            return Ok(());
        }
        let test_db = TestDatabase::new().await?;
        let db = test_db.db.as_ref();
        db.execute_unprepared(
            "CREATE TABLE backups(id INTEGER PRIMARY KEY);
             CREATE TABLE restore_runs(id INTEGER PRIMARY KEY, source_backup_id INTEGER NOT NULL REFERENCES backups(id) ON DELETE RESTRICT);
             INSERT INTO backups VALUES (0), (1);
             INSERT INTO restore_runs VALUES (10, 0), (11, 1);",
        ).await?;
        let manager = SchemaManager::new(db);
        Migration.up(&manager).await?;
        db.execute_unprepared(
            "DELETE FROM backups WHERE id = 0; INSERT INTO restore_runs VALUES (12, NULL)",
        )
        .await?;
        assert!(db
            .execute_unprepared("DELETE FROM backups WHERE id = 1")
            .await
            .is_err());
        assert!(db
            .execute_unprepared("INSERT INTO restore_runs VALUES (13, 99)")
            .await
            .is_err());
        assert!(Migration.down(&manager).await.is_err());
        db.execute_unprepared("DELETE FROM restore_runs WHERE source_backup_id IS NULL")
            .await?;
        Migration.down(&manager).await?;
        assert!(db
            .execute_unprepared("INSERT INTO restore_runs VALUES (14, NULL)")
            .await
            .is_err());
        Ok(())
    }
}
