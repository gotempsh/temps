// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Attachments sent with error events and feedback (ADR 111, D4).
//!
//! The bytes live in the project's blob storage; `error_attachments` holds
//! one row per stored file: who it belongs to (`owner_kind` `error` with the
//! error event id and timestamp, or `feedback` with the feedback id), its
//! name, type and size. A row is written before its object and marked
//! `stored_at` once the object is in place, so a crash between the two
//! leaves a row the sweeper removes, never an unreferenced object.
//!
//! A file's replay identity is its owner, name and SHA-256: an SDK that
//! resends an envelope adds nothing, while a different file sent later for
//! the same owner is a new row. `ordinal` is the display order, assigned
//! under the project's usage row lock.
//!
//! `removing_at` is set when the sweeper claims an unfinished write for
//! removal. A resend can resume an unfinished write only while it is
//! unclaimed, and a claimed row is never marked stored, so a resumed upload
//! and the removal of the same row never both go ahead.
//!
//! `error_attachment_usage` holds one row per project that has attachments:
//! the bytes and files stored, and the per-project quota, which the admin
//! can change. Neither table has a foreign key to `projects`: a deleted
//! project's rows stay until the sweeper has deleted their objects, which a
//! cascade would leave behind; a soft-deleted project counts as deleted. Error events are a hypertable, so the link to
//! an event is its id and timestamp, checked by the sweeper.

use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        let db = manager.get_connection();
        db.execute_unprepared(
            "CREATE TABLE IF NOT EXISTS error_attachments ( \
                 id BIGSERIAL PRIMARY KEY, \
                 project_id INTEGER NOT NULL, \
                 owner_kind TEXT NOT NULL CHECK (owner_kind IN ('error', 'feedback')), \
                 owner_id BIGINT NOT NULL, \
                 ordinal SMALLINT NOT NULL, \
                 error_group_id INTEGER, \
                 event_time TIMESTAMPTZ, \
                 filename TEXT NOT NULL, \
                 content_type TEXT NOT NULL, \
                 attachment_type TEXT NOT NULL, \
                 size_bytes BIGINT NOT NULL CHECK (size_bytes >= 0), \
                 content_sha256 BYTEA NOT NULL CHECK (length(content_sha256) = 32), \
                 created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(), \
                 stored_at TIMESTAMPTZ, \
                 removing_at TIMESTAMPTZ, \
                 UNIQUE (project_id, owner_kind, owner_id, ordinal), \
                 UNIQUE (project_id, owner_kind, owner_id, filename, content_sha256))",
        )
        .await?;
        // The sweeper walks error attachments oldest event first.
        db.execute_unprepared(
            "CREATE INDEX IF NOT EXISTS idx_error_attachments_event_time \
             ON error_attachments (event_time, id) WHERE owner_kind = 'error'",
        )
        .await?;
        // Feedback attachments are found by their feedback, to sweep those
        // whose feedback is gone.
        db.execute_unprepared(
            "CREATE INDEX IF NOT EXISTS idx_error_attachments_feedback \
             ON error_attachments (owner_id) WHERE owner_kind = 'feedback'",
        )
        .await?;
        db.execute_unprepared(
            "CREATE INDEX IF NOT EXISTS idx_error_attachments_unstored \
             ON error_attachments (id) WHERE stored_at IS NULL",
        )
        .await?;
        db.execute_unprepared(
            "CREATE TABLE IF NOT EXISTS error_attachment_usage ( \
                 project_id INTEGER PRIMARY KEY, \
                 bytes BIGINT NOT NULL DEFAULT 0 CHECK (bytes >= 0), \
                 files BIGINT NOT NULL DEFAULT 0 CHECK (files >= 0), \
                 quota_bytes BIGINT NOT NULL DEFAULT 268435456 CHECK (quota_bytes >= 0), \
                 updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW())",
        )
        .await?;
        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        let db = manager.get_connection();
        // Dropping the rows of stored files would leave their objects in
        // blob storage with nothing left to find or delete them, so the
        // rollback refuses while any attachment is listed.
        let present = db
            .query_one(sea_orm::Statement::from_string(
                sea_orm::DatabaseBackend::Postgres,
                "SELECT to_regclass('error_attachments') IS NOT NULL AS present".to_owned(),
            ))
            .await?
            .map(|row| row.try_get::<bool>("", "present"))
            .transpose()?
            .unwrap_or(false);
        if present {
            // Block new attachment rows until this rollback commits (each
            // Postgres migration runs in its own transaction), so the check
            // below and the drops see the same rows: a file inserted after
            // the check would otherwise be dropped with its object left
            // behind. ACCESS EXCLUSIVE is what DROP takes anyway.
            db.execute_unprepared("LOCK TABLE error_attachments IN ACCESS EXCLUSIVE MODE")
                .await?;
            let listed = db
                .query_one(sea_orm::Statement::from_string(
                    sea_orm::DatabaseBackend::Postgres,
                    "SELECT EXISTS (SELECT 1 FROM error_attachments) AS listed".to_owned(),
                ))
                .await?
                .map(|row| row.try_get::<bool>("", "listed"))
                .transpose()?
                .unwrap_or(false);
            if listed {
                return Err(DbErr::Migration(
                    "error_attachments still lists files kept in blob storage; delete them \
                     (temps errors attachments delete, or delete their projects and let the \
                     sweeper run) before rolling back this migration"
                        .to_owned(),
                ));
            }
        }
        db.execute_unprepared("DROP TABLE IF EXISTS error_attachments")
            .await?;
        db.execute_unprepared("DROP TABLE IF EXISTS error_attachment_usage")
            .await?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn the_rollback_refuses_while_files_are_listed_and_identity_is_name_and_content() {
        if !temps_test_utils::external_database_configured()
            && !temps_test_utils::docker_available().await
        {
            eprintln!("Skipping error attachments migration: database unavailable");
            return;
        }
        let test = temps_test_utils::TestDatabase::new()
            .await
            .expect("test database");
        let db = test.connection_arc();
        let manager = SchemaManager::new(db.as_ref());
        Migration.up(&manager).await.expect("up");
        Migration.up(&manager).await.expect("up again");

        let insert = |ordinal: i32, content: &str| {
            format!(
                "INSERT INTO error_attachments (project_id, owner_kind, owner_id, ordinal, \
                     filename, content_type, attachment_type, size_bytes, content_sha256) \
                 VALUES (7, 'error', 11, {ordinal}, 'a.txt', 'text/plain', 'event.attachment', \
                     3, sha256('{content}'::bytea))"
            )
        };
        db.execute_unprepared(&insert(0, "abc"))
            .await
            .expect("first file");
        // The same name and content for the same owner is the same file.
        assert!(db.execute_unprepared(&insert(1, "abc")).await.is_err());
        db.execute_unprepared(&insert(1, "xyz"))
            .await
            .expect("same name, other content");
        assert!(
            db.execute_unprepared(
                "INSERT INTO error_attachments (project_id, owner_kind, owner_id, ordinal, \
                     filename, content_type, attachment_type, size_bytes, content_sha256) \
                 VALUES (7, 'error', 11, 2, 'b.txt', 'text/plain', 'event.attachment', 1, '\\x00'::bytea)"
            )
            .await
            .is_err(),
            "a hash must be 32 bytes"
        );

        let refused = down_in_transaction(&db).await;
        assert!(
            matches!(&refused, Err(DbErr::Migration(message)) if message.contains("still lists files")),
            "{refused:?}"
        );
        db.execute_unprepared("DELETE FROM error_attachments")
            .await
            .expect("delete rows");
        down_in_transaction(&db).await.expect("down");
        down_in_transaction(&db).await.expect("down again");
    }

    /// The rollback as the migrator runs it on Postgres: in a transaction.
    async fn down_in_transaction(db: &sea_orm::DatabaseConnection) -> Result<(), DbErr> {
        use sea_orm::TransactionTrait;
        let txn = db.begin().await?;
        Migration.down(&SchemaManager::new(&txn)).await?;
        txn.commit().await
    }

    #[tokio::test]
    async fn the_rollback_waits_for_a_file_being_inserted_and_then_refuses() {
        use sea_orm::TransactionTrait;
        if !temps_test_utils::external_database_configured()
            && !temps_test_utils::docker_available().await
        {
            eprintln!("Skipping error attachments migration: database unavailable");
            return;
        }
        let test = temps_test_utils::TestDatabase::new()
            .await
            .expect("test database");
        let db = test.connection_arc();
        Migration.up(&SchemaManager::new(db.as_ref())).await.expect("up");

        // An upload reserving its row, not yet committed when the rollback
        // starts: the rollback's check cannot see it.
        let upload = db.begin().await.expect("upload transaction");
        upload
            .execute_unprepared(
                "INSERT INTO error_attachments (project_id, owner_kind, owner_id, ordinal, \
                     filename, content_type, attachment_type, size_bytes, content_sha256) \
                 VALUES (7, 'error', 11, 0, 'a.txt', 'text/plain', 'event.attachment', \
                     3, sha256('abc'::bytea))",
            )
            .await
            .expect("reserve a file");

        let rollback_db = db.clone();
        let rollback =
            tokio::spawn(async move { down_in_transaction(rollback_db.as_ref()).await });
        tokio::time::sleep(std::time::Duration::from_millis(300)).await;
        assert!(
            !rollback.is_finished(),
            "the rollback waits for the insert under way"
        );
        upload.commit().await.expect("commit the file");

        let refused = tokio::time::timeout(std::time::Duration::from_secs(10), rollback)
            .await
            .expect("the rollback finishes once the insert commits")
            .expect("rollback task");
        assert!(
            matches!(&refused, Err(DbErr::Migration(message)) if message.contains("still lists files")),
            "the committed file is seen, not dropped: {refused:?}"
        );
        let kept = db
            .query_one(sea_orm::Statement::from_string(
                sea_orm::DatabaseBackend::Postgres,
                "SELECT COUNT(*) AS n FROM error_attachments".to_owned(),
            ))
            .await
            .expect("count")
            .map(|row| row.try_get::<i64>("", "n"))
            .transpose()
            .expect("n");
        assert_eq!(kept, Some(1));
    }
}
