// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

use super::{build_index_with_retry, MaintenanceProgress, SchemaError, SchemaResult};

// NOT VALID enforces every new write immediately. Existing rows were already
// protected by the former project-qualified foreign keys. Validate history
// after DDL commits, under PostgreSQL's write-compatible validation locks.
const CONSTRAINTS: &[(&str, &str)] = &[
    ("grant_project_provenance", "sandbox_service_grants"),
    (
        "runtime_project_provenance",
        "sandbox_access_runtime_records",
    ),
    ("access_vaults_project_id_fkey", "access_vaults"),
    (
        "sandbox_access_services_project_id_fkey",
        "sandbox_access_services",
    ),
    ("access_bundles_vault_id_fkey", "access_bundles"),
    ("access_sessions_vault_id_fkey", "access_sessions"),
    (
        "sandbox_service_grants_service_id_fkey",
        "sandbox_service_grants",
    ),
    (
        "sandbox_access_authorizations_grant_consumer",
        "sandbox_access_authorizations",
    ),
    (
        "sandbox_access_authorizations_runtime_consumer",
        "sandbox_access_authorizations",
    ),
];

// These are performance indexes, not constraints. Retained session and grant
// history can be large, so build them after the migration transaction commits.
const INDEXES: &[(&str, &str, &str)] = &[
    (
        "access_sessions_link",
        "access_sessions",
        "((COALESCE(link_id, CASE WHEN legacy_project_link THEN 'link_legacy_' || vault_id END)))",
    ),
    (
        "sandbox_service_grants_service",
        "sandbox_service_grants",
        "(service_id)",
    ),
    (
        "access_sessions_grants",
        "access_sessions",
        "USING gin(grant_ids)",
    ),
    (
        "access_vaults_page",
        "access_vaults",
        "(created_at DESC,id DESC) WHERE retired_at IS NULL",
    ),
    (
        "access_vaults_services",
        "access_vaults",
        "USING gin(service_ids)",
    ),
    (
        "access_bundles_page",
        "access_bundles",
        "(vault_id,created_at DESC,id DESC) WHERE retired_at IS NULL",
    ),
    (
        "access_sessions_page",
        "access_sessions",
        "(created_at DESC,id DESC)",
    ),
    (
        "access_sessions_sandbox",
        "access_sessions",
        "(sandbox_id,created_at DESC,id DESC)",
    ),
    (
        "sandbox_access_services_page",
        "sandbox_access_services",
        "(created_at DESC,id DESC)",
    ),
];

pub(super) async fn build<F>(connection: &mut sqlx::PgConnection, progress: &F) -> SchemaResult<()>
where
    F: Fn(MaintenanceProgress<'_>),
{
    // Older schemas and deliberately minimal test schemas do not have the
    // general-vault migration. Do not build against partially migrated tables.
    let available = sqlx::query_scalar::<_, bool>("SELECT EXISTS (SELECT 1 FROM information_schema.columns WHERE table_schema=current_schema() AND table_name='access_sessions' AND column_name='legacy_project_link')")
        .fetch_one(&mut *connection).await.map_err(|error| SchemaError::VaultIndex {
            index: "access_sessions_link", operation: "check schema for", reason: error.to_string(),
        })?;
    if !available {
        return Ok(());
    }
    for &(name, table, definition) in INDEXES {
        progress(MaintenanceProgress::IndexStarted { name });
        let started = std::time::Instant::now();
        build_index_with_retry(
            connection,
            name,
            table,
            &format!("CREATE INDEX CONCURRENTLY IF NOT EXISTS {name} ON {table} {definition}"),
            &format!("DROP INDEX CONCURRENTLY IF EXISTS {name}"),
        )
        .await
        .map_err(|reason| SchemaError::VaultIndex {
            index: name,
            operation: "build",
            reason,
        })?;
        progress(MaintenanceProgress::IndexFinished {
            name,
            elapsed: started.elapsed(),
        });
    }
    for &(constraint, table) in CONSTRAINTS {
        // Older partial upgrades may not yet have installed provenance constraints.
        if matches!(
            constraint,
            "grant_project_provenance" | "runtime_project_provenance"
        ) {
            let exists: bool = sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM pg_constraint c JOIN pg_namespace n ON n.oid=c.connamespace WHERE n.nspname=current_schema() AND c.conname=$1)")
                .bind(constraint).fetch_one(&mut *connection).await.map_err(|error| SchemaError::VaultConstraint { constraint, table, reason:error.to_string() })?;
            if !exists {
                continue;
            }
        }
        sqlx::query(&format!(
            "ALTER TABLE {table} VALIDATE CONSTRAINT {constraint}"
        ))
        .execute(&mut *connection)
        .await
        .map_err(|error| SchemaError::VaultConstraint {
            constraint,
            table,
            reason: error.to_string(),
        })?;
    }
    Ok(())
}

/// Constraint-supporting indexes must exist before the general-vault DDL. Build
/// retained history concurrently, with a disposable session for locks/timeouts.
pub(crate) async fn prepare_consumer_constraints(
    db: &sea_orm::DatabaseConnection,
) -> SchemaResult<()> {
    let prepare = async {
        let mut connection =
            db.get_postgres_connection_pool()
                .acquire()
                .await
                .map_err(|error| SchemaError::VaultIndex {
                    index: "sandbox_consumer_constraints",
                    operation: "acquire preparation connection for",
                    reason: error.to_string(),
                })?;
        connection.close_on_drop();
        for setting in ["SET lock_timeout=0", "SET statement_timeout='600s'"] {
            sqlx::query(setting)
                .execute(&mut *connection)
                .await
                .map_err(|error| SchemaError::VaultIndex {
                    index: "sandbox_consumer_constraints",
                    operation: "bound preparation time for",
                    reason: error.to_string(),
                })?;
        }
        sqlx::query("SELECT pg_advisory_lock(hashtext(current_database()),hashtext(current_schema() || ':vault_consumer_indexes'))").execute(&mut *connection).await.map_err(|error| SchemaError::VaultIndex {
            index: "sandbox_consumer_constraints", operation: "lock preparation of", reason: error.to_string(),
        })?;
        sqlx::query("SET lock_timeout='5s'")
            .execute(&mut *connection)
            .await
            .map_err(|error| SchemaError::VaultIndex {
                index: "sandbox_consumer_constraints",
                operation: "bound table lock wait for",
                reason: error.to_string(),
            })?;
        for &(name, table) in temps_migrations::GENERAL_VAULT_CONSUMER_INDEXES {
            let table = if table == "sandbox_access_runtimes" {
                let renamed: bool = sqlx::query_scalar("SELECT to_regclass(quote_ident(current_schema()) || '.sandbox_access_runtime_records') IS NOT NULL")
                    .fetch_one(&mut *connection).await.map_err(|error| SchemaError::VaultIndex {
                        index: name, operation: "resolve runtime storage for", reason: error.to_string(),
                    })?;
                if renamed {
                    "sandbox_access_runtime_records"
                } else {
                    table
                }
            } else {
                table
            };
            let exists = sqlx::query_scalar::<_, bool>("SELECT to_regclass(quote_ident(current_schema()) || '.' || quote_ident($1)) IS NOT NULL")
                .bind(table).fetch_one(&mut *connection).await.map_err(|error| SchemaError::VaultIndex {
                    index: name, operation: "check source table for", reason: error.to_string(),
                })?;
            if !exists {
                continue;
            }
            tracing::info!(
                index = name,
                table,
                "Preparing vault consumer constraint index concurrently"
            );
            build_index_with_retry(
                &mut connection, name, table,
                &format!("CREATE UNIQUE INDEX CONCURRENTLY IF NOT EXISTS {name} ON {table}(id,sandbox_id)"),
                &format!("DROP INDEX CONCURRENTLY IF EXISTS {name}"),
            ).await.map_err(|reason| SchemaError::VaultIndex { index: name, operation: "prepare", reason })?;
        }
        Ok(())
    };
    tokio::time::timeout(std::time::Duration::from_secs(600), prepare)
        .await
        .map_err(|_| SchemaError::VaultIndex {
            index: "sandbox_consumer_constraints",
            operation: "prepare",
            reason: "preparation exceeded 600 seconds; retry the migration".into(),
        })?
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn preparation_waits_for_another_upgrader_beyond_table_lock_timeout() {
        if !temps_test_utils::external_database_configured()
            && !temps_test_utils::docker_available().await
        {
            eprintln!("Skipping preparation lock test: database unavailable");
            return;
        }
        let mut test = temps_test_utils::TestDatabase::new().await.unwrap();
        let db = test.connection_arc();
        let mut holder = db.get_postgres_connection_pool().acquire().await.unwrap();
        let lock_sql = "SELECT pg_advisory_lock(hashtext(current_database()),hashtext(current_schema() || ':vault_consumer_indexes'))";
        sqlx::query(lock_sql).execute(&mut *holder).await.unwrap();
        let waiting_db = db.clone();
        let waiter = tokio::spawn(async move { prepare_consumer_constraints(&waiting_db).await });
        tokio::time::timeout(std::time::Duration::from_secs(5), async {
            loop {
                let waiting: bool = sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM pg_locks WHERE locktype='advisory' AND NOT granted AND database=(SELECT oid FROM pg_database WHERE datname=current_database()))")
                    .fetch_one(&mut *holder).await.unwrap();
                if waiting { break; }
                tokio::time::sleep(std::time::Duration::from_millis(20)).await;
            }
        }).await.unwrap();
        tokio::time::sleep(std::time::Duration::from_secs(6)).await;
        let was_waiting = !waiter.is_finished();
        sqlx::query("SELECT pg_advisory_unlock_all()")
            .execute(&mut *holder)
            .await
            .unwrap();
        let result = tokio::time::timeout(std::time::Duration::from_secs(30), waiter)
            .await
            .unwrap()
            .unwrap();
        drop(holder);
        test.cleanup().await;
        assert!(
            was_waiting,
            "a healthy concurrent upgrade must not fail after five seconds"
        );
        result.unwrap();
    }

    #[tokio::test]
    async fn retained_history_indexes_build_after_migration_and_are_idempotent() {
        if !temps_test_utils::external_database_configured()
            && !temps_test_utils::docker_available().await
        {
            eprintln!("Skipping vault index maintenance: database unavailable");
            return;
        }
        let mut test = temps_test_utils::TestDatabase::with_migrations()
            .await
            .unwrap();
        let db = test.connection_arc();
        let mut connection = db.get_postgres_connection_pool().acquire().await.unwrap();
        let names: Vec<&str> = INDEXES.iter().map(|i| i.0).collect();
        let before=sqlx::query_scalar::<_,i64>("SELECT count(*) FROM pg_indexes WHERE schemaname=current_schema() AND indexname=ANY($1)").bind(&names).fetch_one(&mut *connection).await.unwrap();
        assert_eq!(
            before, 0,
            "the migration must not scan retained history to build indexes"
        );
        let constraints: Vec<&str> = CONSTRAINTS.iter().map(|c| c.0).collect();
        let unvalidated: i64 = sqlx::query_scalar("SELECT count(*) FROM pg_constraint c JOIN pg_namespace n ON n.oid=c.connamespace WHERE n.nspname=current_schema() AND c.conname=ANY($1) AND NOT c.convalidated").bind(&constraints).fetch_one(&mut *connection).await.unwrap();
        assert_eq!(unvalidated, CONSTRAINTS.len() as i64);
        build(&mut connection, &|_| {}).await.unwrap();
        build(&mut connection, &|_| {}).await.unwrap();
        let validated: i64 = sqlx::query_scalar("SELECT count(*) FROM pg_constraint c JOIN pg_namespace n ON n.oid=c.connamespace WHERE n.nspname=current_schema() AND c.conname=ANY($1) AND c.convalidated").bind(&constraints).fetch_one(&mut *connection).await.unwrap();
        assert_eq!(validated, CONSTRAINTS.len() as i64);
        let valid=sqlx::query_scalar::<_,i64>("SELECT count(*) FROM pg_index i JOIN pg_class c ON c.oid=i.indexrelid JOIN pg_namespace n ON n.oid=c.relnamespace WHERE n.nspname=current_schema() AND c.relname=ANY($1) AND i.indisvalid").bind(&names).fetch_one(&mut *connection).await.unwrap();
        assert_eq!(valid, INDEXES.len() as i64);
        drop(connection);
        drop(db);
        test.cleanup().await;
    }
    #[tokio::test]
    async fn populated_upgrade_prepares_constraints_without_blocking_writes() {
        use sea_orm::{ConnectionTrait, DatabaseBackend, Statement, TransactionTrait};
        use sea_orm_migration::MigratorTrait;
        if !temps_test_utils::external_database_configured()
            && !temps_test_utils::docker_available().await
        {
            eprintln!("Skipping vault constraint preparation: database unavailable");
            return;
        }
        for streaming in [false, true] {
            let mut test = temps_test_utils::TestDatabase::new().await.unwrap();
            let db = test.connection_arc();
            let previous = temps_migrations::Migrator::migrations()
                .iter()
                .position(|m| m.name() == temps_migrations::GENERAL_VAULT_MIGRATION)
                .unwrap();
            temps_migrations::Migrator::up(db.as_ref(), Some(previous as u32))
                .await
                .unwrap();
            db.execute_unprepared("INSERT INTO users(id,name,email) VALUES(9931,'Fixture','index-fixture@example.com'); INSERT INTO projects(id,name,repo_name,repo_owner,directory,main_branch,preset,slug,created_at,updated_at) VALUES(9931,'Fixture','repo','owner','/','main','nodejs','index-fixture',now(),now()); INSERT INTO sandboxes(id,public_id,name,user_id,project_id,status,backend) VALUES(9931,'sbx_index_fixture','Fixture',9931,9931,'running','docker'); INSERT INTO sandbox_access_services(id,project_id,name,policy,max_ttl_seconds,enabled) VALUES('svc_index',9931,'Fixture','{}',60,true); INSERT INTO sandbox_service_grants(id,project_id,service_id,sandbox_id,service_version,policy_snapshot,created_by,expires_at) VALUES('grant_index',9931,'svc_index',9931,1,'{}',9931,now()+interval '1 hour')").await.unwrap();
            db.execute_unprepared("INSERT INTO access_vaults(id,project_id,name,service_ids) VALUES('vault_backfill',9931,'Fixture',ARRAY['svc_index'])").await.unwrap();
            let writer = db.begin().await.unwrap();
            writer
                .execute_unprepared(
                    "UPDATE sandbox_service_grants SET version=version WHERE id='grant_index'",
                )
                .await
                .unwrap();
            let pid: i32 = writer
                .query_one(Statement::from_string(
                    DatabaseBackend::Postgres,
                    "SELECT pg_backend_pid() AS pid",
                ))
                .await
                .unwrap()
                .unwrap()
                .try_get("", "pid")
                .unwrap();
            let upgrading = db.clone();
            let upgrade = tokio::spawn(async move {
                if streaming {
                    let report = crate::run_migrations_streaming(upgrading.as_ref(), |_| {})
                        .await
                        .unwrap();
                    assert!(report.failed().is_none(), "{report:?}");
                } else {
                    crate::run_migrations(upgrading.as_ref()).await.unwrap();
                }
            });
            tokio::time::timeout(std::time::Duration::from_secs(10), async {
                loop {
                    let waiting: bool = db.query_one(Statement::from_sql_and_values(DatabaseBackend::Postgres,
                        "SELECT EXISTS (SELECT 1 FROM pg_stat_activity WHERE $1=ANY(pg_blocking_pids(pid)) AND query LIKE 'CREATE UNIQUE INDEX CONCURRENTLY%') AS waiting", [pid.into()])).await.unwrap().unwrap().try_get("", "waiting").unwrap();
                    if waiting { break; }
                    tokio::time::sleep(std::time::Duration::from_millis(10)).await;
                }
            }).await.unwrap();
            // A concurrent build may wait for an existing writer, but it must
            // not prevent another writer from issuing access in the meantime.
            let concurrent = db.begin().await.unwrap();
            concurrent
                .execute_unprepared("SET LOCAL lock_timeout='1s'")
                .await
                .unwrap();
            let inserted = concurrent.execute_unprepared("INSERT INTO sandbox_service_grants(id,project_id,service_id,sandbox_id,service_version,policy_snapshot,created_by,expires_at) VALUES('grant_during_index',9931,'svc_index',9931,1,'{}',9931,now()+interval '1 hour')").await;
            concurrent.commit().await.unwrap();
            writer.rollback().await.unwrap();
            tokio::time::timeout(std::time::Duration::from_secs(30), upgrade)
                .await
                .unwrap()
                .unwrap();
            assert!(
                inserted.is_ok(),
                "access writes must continue while indexes are prepared: {inserted:?}"
            );
            prepare_consumer_constraints(db.as_ref()).await.unwrap();
            let names: Vec<&str> = temps_migrations::GENERAL_VAULT_CONSUMER_INDEXES
                .iter()
                .map(|i| i.0)
                .collect();
            let valid: i64 = sqlx::query_scalar("SELECT count(*) FROM pg_index i JOIN pg_class c ON c.oid=i.indexrelid JOIN pg_namespace n ON n.oid=c.relnamespace WHERE n.nspname=current_schema() AND c.relname=ANY($1) AND i.indisvalid AND i.indisunique").bind(&names).fetch_one(db.get_postgres_connection_pool()).await.unwrap();
            assert_eq!(valid, 2);
            let links: i64 = sqlx::query_scalar(
                "SELECT count(*) FROM access_vault_links WHERE id='link_legacy_vault_backfill'",
            )
            .fetch_one(db.get_postgres_connection_pool())
            .await
            .unwrap();
            assert_eq!(
                links, 1,
                "both migration runners finish the committed link backfill"
            );
            db.execute_unprepared(
                "DELETE FROM access_vault_links WHERE id='link_legacy_vault_backfill'",
            )
            .await
            .unwrap();
            crate::run_migrations(db.as_ref()).await.unwrap();
            let links: i64 = sqlx::query_scalar(
                "SELECT count(*) FROM access_vault_links WHERE id='link_legacy_vault_backfill'",
            )
            .fetch_one(db.get_postgres_connection_pool())
            .await
            .unwrap();
            assert_eq!(links, 0, "restarts do not reauthorize a removed consumer");
            drop(db);
            test.cleanup().await;
        }
    }
}
