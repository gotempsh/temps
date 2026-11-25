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
        let db = manager.get_connection();
        // Every run with no tracked backup blocks the rollback, whatever its
        // status. Active ones are counted separately because their rows are
        // what keeps a second restore off the same service: the operator must
        // finish or cancel them before deleting any history.
        let counts = db
            .query_one(sea_orm::Statement::from_string(
                manager.get_database_backend(),
                "SELECT count(*)::BIGINT AS untracked, \
                        count(*) FILTER (WHERE status IN ('pending', 'running'))::BIGINT AS active \
                 FROM restore_runs WHERE source_backup_id IS NULL",
            ))
            .await?;
        let (untracked, active) = match counts {
            Some(row) => (
                row.try_get::<i64>("", "untracked")?,
                row.try_get::<i64>("", "active")?,
            ),
            None => (0, 0),
        };
        if untracked > 0 {
            return Err(DbErr::Migration(format!(
                "cannot roll back m20261124_000001_nullable_restore_source_backup: {untracked} \
                 restore run(s) restored a backup found in S3 that this server does not track \
                 (restore_runs.source_backup_id IS NULL), {active} of them still pending or \
                 running, and the previous schema requires every restore run to reference a \
                 tracked backup. Keep this schema, or: stop starting new restores on the affected \
                 services; let every pending or running restore finish or cancel it, and confirm \
                 no restore helper is still running (stop any that survives); then \
                 delete only finished history (DELETE FROM restore_runs WHERE source_backup_id \
                 IS NULL AND status NOT IN ('pending', 'running')) and retry the rollback"
            )));
        }
        db.execute_unprepared("ALTER TABLE restore_runs ALTER COLUMN source_backup_id SET NOT NULL")
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
             CREATE TABLE restore_runs(id INTEGER PRIMARY KEY, source_backup_id INTEGER NOT NULL REFERENCES backups(id) ON DELETE RESTRICT, status TEXT NOT NULL DEFAULT 'completed');
             INSERT INTO backups VALUES (0), (1);
             INSERT INTO restore_runs VALUES (10, 0), (11, 1);",
        ).await?;
        let manager = SchemaManager::new(db);
        Migration.up(&manager).await?;
        db.execute_unprepared(
            "DELETE FROM backups WHERE id = 0; INSERT INTO restore_runs VALUES (12, NULL, 'running')",
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
        let refused = Migration
            .down(&manager)
            .await
            .expect_err("a restore run of an untracked backup blocks the rollback")
            .to_string();
        assert!(
            refused.contains("2 restore run(s)")
                && refused.contains("1 of them still pending or running")
                && refused.contains("status NOT IN ('pending', 'running')")
                && !refused.contains("violates not-null constraint"),
            "the rollback must say how many runs block it, how many are active, and \
             clean up only finished history: {refused}"
        );
        // Deleting only finished history leaves the running run, which still
        // blocks the rollback.
        db.execute_unprepared(
            "DELETE FROM restore_runs WHERE source_backup_id IS NULL \
             AND status NOT IN ('pending', 'running')",
        )
        .await?;
        let still_refused = Migration
            .down(&manager)
            .await
            .expect_err("an active run of an untracked backup still blocks the rollback")
            .to_string();
        assert!(
            still_refused.contains("1 restore run(s)")
                && still_refused.contains("1 of them still pending or running"),
            "an active run must keep blocking the rollback: {still_refused}"
        );
        db.execute_unprepared("UPDATE restore_runs SET status = 'cancelled' WHERE id = 12")
            .await?;
        db.execute_unprepared(
            "DELETE FROM restore_runs WHERE source_backup_id IS NULL \
             AND status NOT IN ('pending', 'running')",
        )
        .await?;
        Migration.down(&manager).await?;
        assert!(db
            .execute_unprepared("INSERT INTO restore_runs VALUES (14, NULL)")
            .await
            .is_err());
        Ok(())
    }
}
