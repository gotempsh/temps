// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

use super::{build_index_with_retry, MaintenanceProgress, SchemaError, SchemaResult};

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
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

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
        build(&mut connection, &|_| {}).await.unwrap();
        build(&mut connection, &|_| {}).await.unwrap();
        let valid=sqlx::query_scalar::<_,i64>("SELECT count(*) FROM pg_index i JOIN pg_class c ON c.oid=i.indexrelid JOIN pg_namespace n ON n.oid=c.relnamespace WHERE n.nspname=current_schema() AND c.relname=ANY($1) AND i.indisvalid").bind(&names).fetch_one(&mut *connection).await.unwrap();
        assert_eq!(valid, INDEXES.len() as i64);
        drop(connection);
        drop(db);
        test.cleanup().await;
    }
}
