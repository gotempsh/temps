// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

use crate::error::AccessError;
use sea_orm::{ConnectionTrait, DatabaseBackend, Statement};

/// Service versions are immutable, including the vault scope of their writes.
pub(crate) fn service_audit_scope_key(id: &str, version: i32) -> String {
    format!("{id}:{version}")
}

pub(super) async fn capture_service_audit_scope<C: ConnectionTrait>(
    db: &C,
    id: &str,
    version: i32,
) -> Result<(), AccessError> {
    db.execute(Statement::from_sql_and_values(
        DatabaseBackend::Postgres,
        "INSERT INTO access_service_audit_scopes(scope_key,vault_id) SELECT $1,id FROM access_vaults WHERE retired_at IS NULL AND service_ids @> ARRAY[$2] ON CONFLICT DO NOTHING",
        [service_audit_scope_key(id, version).into(), id.into()],
    )).await.map_err(|source| AccessError::resource_database("capture service audit scope", id, source))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use sea_orm::{DbErr, MockDatabase};

    #[tokio::test]
    async fn captures_membership_before_retirement_without_reassigning_earlier_versions() {
        if !temps_test_utils::external_database_configured()
            && !temps_test_utils::docker_available().await
        {
            eprintln!("Skipping service audit scope: database unavailable");
            return;
        }
        let mut test = temps_test_utils::TestDatabase::with_migrations()
            .await
            .unwrap();
        let db = test.connection_arc();
        capture_service_audit_scope(db.as_ref(), "svc_fixture", 1)
            .await
            .unwrap();
        db.execute_unprepared("INSERT INTO access_vaults(id,name,service_ids) VALUES('vault_first','First',ARRAY['svc_fixture']),('vault_second','Second','{}')").await.unwrap();
        capture_service_audit_scope(db.as_ref(), "svc_fixture", 2)
            .await
            .unwrap();
        db.execute_unprepared("UPDATE access_vaults SET service_ids='{}' WHERE id='vault_first'; UPDATE access_vaults SET service_ids=ARRAY['svc_fixture'] WHERE id='vault_second'").await.unwrap();
        capture_service_audit_scope(db.as_ref(), "svc_fixture", 3)
            .await
            .unwrap();
        capture_service_audit_scope(db.as_ref(), "svc_fixture", 4)
            .await
            .unwrap();
        db.execute_unprepared("UPDATE access_vaults SET service_ids='{}'")
            .await
            .unwrap();
        let rows = db
            .query_all(Statement::from_string(
                DatabaseBackend::Postgres,
                "SELECT scope_key,vault_id FROM access_service_audit_scopes ORDER BY scope_key",
            ))
            .await
            .unwrap();
        let pairs: Vec<(String, String)> = rows
            .iter()
            .map(|row| {
                (
                    row.try_get("", "scope_key").unwrap(),
                    row.try_get("", "vault_id").unwrap(),
                )
            })
            .collect();
        assert_eq!(
            pairs,
            vec![
                ("svc_fixture:2".into(), "vault_first".into()),
                ("svc_fixture:3".into(), "vault_second".into()),
                ("svc_fixture:4".into(), "vault_second".into())
            ]
        );
        test.cleanup().await;
    }

    #[tokio::test]
    async fn scope_storage_failure_names_the_service_and_operation() {
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_exec_errors([DbErr::Custom("fixture storage failure".into())])
            .into_connection();
        let error = capture_service_audit_scope(&db, "svc_fixture", 3)
            .await
            .unwrap_err();
        assert!(
            matches!(error, AccessError::ResourceDatabase { resource_id, operation: "capture service audit scope", .. } if resource_id == "svc_fixture")
        );
    }
}
