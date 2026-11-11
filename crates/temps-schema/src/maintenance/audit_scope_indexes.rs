// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

use super::{build_index_with_retry, MaintenanceProgress, SchemaError, SchemaResult};

const INDEXES: &[(&str, &str, &str)] = &[
    ("audit_logs_resource_id_recent", "audit_logs", "((temps_audit_json(data)->>'resource_id'), audit_date DESC, id DESC) WHERE temps_audit_json(data)->>'resource_id' IS NOT NULL"),
    ("audit_logs_vault_id_recent", "audit_logs", "((temps_audit_json(data)->>'vault_id'), audit_date DESC, id DESC) WHERE temps_audit_json(data)->>'vault_id' IS NOT NULL"),
    ("audit_logs_service_scope_key_recent", "audit_logs", "((temps_audit_json(data)->>'service_scope_key'), audit_date DESC, id DESC) WHERE temps_audit_json(data)->>'service_scope_key' IS NOT NULL"),
    ("access_service_audit_scopes_vault", "access_service_audit_scopes", "(vault_id,scope_key)"),
];

/// Single-column predecessors of the `_recent` indexes. The composites serve the
/// same equality lookups, so keeping both only adds write cost to every audit insert.
const SUPERSEDED_INDEXES: &[&str] = &[
    "audit_logs_resource_id",
    "audit_logs_vault_id",
    "audit_logs_service_scope_key",
];

pub(super) async fn build<F>(connection: &mut sqlx::PgConnection, progress: &F) -> SchemaResult<()>
where
    F: Fn(MaintenanceProgress<'_>),
{
    let available: bool = sqlx::query_scalar("SELECT to_regclass('access_service_audit_scopes') IS NOT NULL AND to_regprocedure('temps_audit_json(text)') IS NOT NULL")
        .fetch_one(&mut *connection).await.map_err(|error| SchemaError::VaultIndex {
            index: "audit_logs_resource_id", operation: "check audit scope schema for", reason: error.to_string(),
        })?;
    if !available {
        return Ok(());
    }
    // Retained audit history is indexed outside the schema transaction so new
    // audit writes remain available while these expression indexes are built.
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
            operation: "build audit scope index",
            reason,
        })?;
        progress(MaintenanceProgress::IndexFinished {
            name,
            elapsed: started.elapsed(),
        });
    }
    for &name in SUPERSEDED_INDEXES {
        sqlx::query(&format!("DROP INDEX CONCURRENTLY IF EXISTS {name}"))
            .execute(&mut *connection)
            .await
            .map_err(|error| SchemaError::VaultIndex {
                index: name,
                operation: "drop superseded audit scope index",
                reason: error.to_string(),
            })?;
    }
    Ok(())
}
