// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

use sea_orm::{
    ConnectionTrait, DatabaseBackend, DatabaseConnection, DbErr, Statement, TransactionTrait,
};

/// Complete the legacy-link upgrade outside Migrator::up's DDL transaction.
/// A durable keyset cursor commits with each batch. Once exhausted, its row is
/// removed permanently so a later startup cannot recreate deleted consumer links.
pub async fn backfill_legacy_vault_links(db: &DatabaseConnection) -> Result<(), DbErr> {
    let exists = db.query_one(Statement::from_string(DatabaseBackend::Postgres,
        "SELECT to_regclass(quote_ident(current_schema()) || '.access_vault_link_backfill') IS NOT NULL AS present"))
        .await?.ok_or_else(|| DbErr::Custom("Cannot check legacy vault-link backfill table availability".into()))?;
    if !exists.try_get::<bool>("", "present")? {
        return Ok(());
    }
    while backfill_batch(db).await? {}
    Ok(())
}

async fn backfill_batch(db: &DatabaseConnection) -> Result<bool, DbErr> {
    let tx = db.begin().await?;
    tx.execute_unprepared("SET LOCAL lock_timeout='5s'; SET LOCAL statement_timeout='60s'")
        .await?;
    let cursor = tx
        .query_one(Statement::from_string(
            DatabaseBackend::Postgres,
            "SELECT cursor_id FROM access_vault_link_backfill WHERE singleton FOR UPDATE",
        ))
        .await?;
    let Some(cursor) = cursor else {
        tx.commit().await?;
        return Ok(false);
    };
    let cursor: Option<String> = cursor.try_get("", "cursor_id")?;
    // Scan all vault IDs, including ineligible ones, so each source row is visited
    // once and sparse eligible rows do not cause repeated full-table scans.
    let row = tx
        .query_one(Statement::from_sql_and_values(
            DatabaseBackend::Postgres,
            r#"
WITH batch AS MATERIALIZED (
 SELECT id, project_id, created_at, retired_at FROM access_vaults
 WHERE ($1::text IS NULL OR id > $1) ORDER BY id LIMIT 1000
), inserted AS (
 INSERT INTO access_vault_links(id,vault_id,project_id,created_at)
 SELECT 'link_legacy_' || id,id,project_id,created_at FROM batch
 WHERE project_id IS NOT NULL AND retired_at IS NULL ON CONFLICT DO NOTHING
)
SELECT max(id) AS last_id FROM batch
"#,
            [cursor.into()],
        ))
        .await?
        .ok_or_else(|| DbErr::Custom("Legacy vault-link batch returned no cursor".into()))?;
    let last: Option<String> = row.try_get("", "last_id")?;
    if let Some(ref id) = last {
        tx.execute(Statement::from_sql_and_values(
            DatabaseBackend::Postgres,
            "UPDATE access_vault_link_backfill SET cursor_id=$1 WHERE singleton",
            [id.clone().into()],
        ))
        .await?;
    } else {
        tx.execute_unprepared("DELETE FROM access_vault_link_backfill WHERE singleton")
            .await?;
    }
    tx.commit().await?;
    Ok(last.is_some())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn batches_commit_without_ddl_locks_and_resume_without_recreating_deleted_links() {
        if !temps_test_utils::external_database_configured()
            && !temps_test_utils::docker_available().await
        {
            eprintln!("Skipping legacy-link batches: database unavailable");
            return;
        }
        let mut test = temps_test_utils::TestDatabase::with_migrations()
            .await
            .unwrap();
        let db = test.connection();
        db.execute_unprepared("INSERT INTO projects(id,name,repo_name,repo_owner,directory,main_branch,preset,slug,created_at,updated_at) VALUES(9941,'Fixture','repo','owner','/','main','nodejs','backfill-fixture',now(),now()); INSERT INTO access_vaults(id,project_id,name,service_ids) SELECT 'vault_' || lpad(n::text,4,'0'),9941,'Fixture',ARRAY[]::text[] FROM generate_series(1,1001) n").await.unwrap();
        assert!(backfill_batch(db).await.unwrap());
        // A separately acquired connection can observe the committed batch and
        // take a write lock. An enclosing DDL transaction would block both.
        let observer = db.begin().await.unwrap();
        observer
            .execute_unprepared(
                "SET LOCAL lock_timeout='1s'; LOCK TABLE access_vaults IN ROW EXCLUSIVE MODE",
            )
            .await
            .unwrap();
        let count: i64 = observer
            .query_one(Statement::from_string(
                DatabaseBackend::Postgres,
                "SELECT count(*) AS n FROM access_vault_links",
            ))
            .await
            .unwrap()
            .unwrap()
            .try_get("", "n")
            .unwrap();
        assert_eq!(count, 1000);
        observer.commit().await.unwrap();
        db.execute_unprepared("DELETE FROM access_vault_links WHERE id='link_legacy_vault_0001'")
            .await
            .unwrap();
        // Resume as after a process interruption. Earlier IDs must not be revisited.
        backfill_legacy_vault_links(db).await.unwrap();
        backfill_legacy_vault_links(db).await.unwrap();
        let row = db.query_one(Statement::from_string(DatabaseBackend::Postgres,"SELECT (SELECT count(*) FROM access_vault_links) AS n, (SELECT count(*) FROM access_vault_link_backfill) AS pending, EXISTS(SELECT 1 FROM access_vault_links WHERE id='link_legacy_vault_0001') AS recreated")).await.unwrap().unwrap();
        assert_eq!(row.try_get::<i64>("", "n").unwrap(), 1000);
        assert_eq!(row.try_get::<i64>("", "pending").unwrap(), 0);
        assert!(!row.try_get::<bool>("", "recreated").unwrap());
        test.cleanup().await;
    }
}
