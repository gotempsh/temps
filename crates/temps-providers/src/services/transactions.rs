// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Transaction and lock helpers, and service-name reservation.

use super::*;
use sea_orm::{ColumnTrait, DatabaseConnection, EntityTrait, QueryFilter, TransactionTrait};
use temps_entities::external_services;
use tracing::warn;

/// Reserve the name only until this transaction commits. A clone restore
/// persists an active target-name reservation before it releases the lock;
/// Docker and WAL-G work never hold this transaction open.
pub async fn reserve_service_identity(
    txn: &sea_orm::DatabaseTransaction,
    name: &str,
    exclude_service_id: Option<i32>,
    exclude_restore_run_id: Option<i32>,
) -> Result<(), ServiceIdentityReservationError> {
    use sea_orm::ConnectionTrait;
    txn.execute(sea_orm::Statement::from_sql_and_values(
        sea_orm::DatabaseBackend::Postgres,
        "SELECT pg_advisory_xact_lock(hashtextextended('external-service-name:' || $1, 0))",
        [name.into()],
    ))
    .await?;
    let mut query =
        external_services::Entity::find().filter(external_services::Column::Name.eq(name));
    if let Some(id) = exclude_service_id {
        query = query.filter(external_services::Column::Id.ne(id));
    }
    if let Some(existing) = query.one(txn).await? {
        return Err(ServiceIdentityReservationError::Registered {
            name: name.to_string(),
            existing_service_id: existing.id,
        });
    }
    let mut query = temps_entities::restore_runs::Entity::find()
        .filter(temps_entities::restore_runs::Column::TargetServiceName.eq(name))
        .filter(temps_entities::restore_runs::Column::Status.is_in(["pending", "running"]));
    if let Some(id) = exclude_restore_run_id {
        query = query.filter(temps_entities::restore_runs::Column::Id.ne(id));
    }
    if let Some(run) = query.one(txn).await? {
        return Err(ServiceIdentityReservationError::Restoring {
            name: name.to_string(),
            restore_run_id: run.id,
        });
    }
    Ok(())
}

pub(super) async fn reserve_service_name(
    txn: &sea_orm::DatabaseTransaction,
    name: &str,
    exclude_service_id: Option<i32>,
) -> Result<(), ExternalServiceError> {
    reserve_service_identity(txn, name, exclude_service_id, None)
        .await
        .map_err(ExternalServiceError::from)
}

/// Longest a service-config transaction waits for a row or advisory lock
/// held by another transaction. Every holder of these locks is a short
/// database transaction, so a wait this long means the holder is stuck;
/// failing with [`ExternalServiceError::LockTimeout`] names the service and
/// the operation where an unbounded wait would hang the request (and, in a
/// test, the whole CI job) with nothing in the log.
pub(super) const SERVICE_LOCK_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(30);

/// Open a transaction whose lock waits are capped at `timeout`.
///
/// `SET LOCAL` scopes the cap to this transaction, so it cannot leak onto
/// the pooled connection's next user.
pub(super) async fn begin_bounded_transaction(
    db: &DatabaseConnection,
    service_id: i32,
    operation: &'static str,
    timeout: std::time::Duration,
) -> Result<sea_orm::DatabaseTransaction, ExternalServiceError> {
    use sea_orm::ConnectionTrait;
    let txn = db.begin().await?;
    // The value is an integer we format ourselves; `SET` cannot bind a
    // parameter.
    let statement = format!(
        "SET LOCAL lock_timeout = '{}ms'",
        timeout.as_millis().max(1)
    );
    if let Err(error) = txn.execute_unprepared(&statement).await {
        let error = lock_timeout_or_database_error(service_id, operation, timeout, error.into());
        return Err(rollback_before_error(txn, error).await);
    }
    Ok(txn)
}

/// Whether a database error message is Postgres's "lock_not_available"
/// (SQLSTATE 55P03), which `lock_timeout` raises as "canceling statement
/// due to lock timeout".
pub(super) fn is_lock_timeout_message(message: &str) -> bool {
    message.contains("55P03") || message.contains("due to lock timeout")
}

/// Turn a lock-wait failure into [`ExternalServiceError::LockTimeout`];
/// every other error passes through unchanged.
pub(super) fn lock_timeout_or_database_error(
    service_id: i32,
    operation: &'static str,
    timeout: std::time::Duration,
    error: ExternalServiceError,
) -> ExternalServiceError {
    match error {
        ExternalServiceError::DatabaseError { reason } if is_lock_timeout_message(&reason) => {
            ExternalServiceError::LockTimeout {
                service_id,
                operation,
                timeout_secs: timeout.as_secs().max(1),
            }
        }
        other => other,
    }
}

/// Roll `txn` back now and hand back `error`, for a locked check that
/// fails. Dropping a sea-orm transaction only queues its ROLLBACK; sqlx
/// sends it later, from a task, when the connection returns to the pool.
/// Until then the connection stays idle in the transaction and keeps every
/// lock it took: the `FOR UPDATE` row lock, and the table locks a `DROP` or
/// `ALTER` waits on. On a current-thread runtime that is blocked, as by a
/// test database's synchronous cleanup, it is never sent at all.
pub(super) async fn rollback_before_error(
    txn: sea_orm::DatabaseTransaction,
    error: ExternalServiceError,
) -> ExternalServiceError {
    if let Err(rollback_error) = txn.rollback().await {
        warn!(
            error = %error,
            rollback_error = %rollback_error,
            "Could not roll back a transaction after a failed locked check"
        );
    }
    error
}

/// Detect a Postgres unique-constraint violation by inspecting the
/// error chain. Sea-ORM wraps `SqlxError`, which wraps the libpq
/// `SQLSTATE`. The reliable signal is `SQLSTATE 23505` ("unique
/// violation"). We match on substring rather than parsing the full
/// error chain because `tokio_postgres::Error::source()` is hidden
/// inside Sea-ORM's wrapper and there's no stable accessor.
///
/// False positives would only happen if the error message text
/// contains "23505" by accident, which a postgres protocol error
/// won't.
pub(super) fn is_unique_violation(e: &sea_orm::DbErr) -> bool {
    let s = e.to_string();
    s.contains("23505") || s.contains("duplicate key value")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::services::test_support::*;
    use chrono::Utc;
    use sea_orm::TransactionTrait;
    use temps_entities::external_services;

    /// A lock another transaction holds forever must surface as a typed
    /// error naming the service and operation, not as a wait that never ends.
    #[cfg(feature = "docker-tests")]
    #[tokio::test]
    async fn test_bounded_transaction_gives_up_on_a_held_lock() {
        use sea_orm::ConnectionTrait;
        let test_db = match temps_database::test_utils::TestDatabase::with_migrations().await {
            Ok(test_db) => test_db,
            Err(error) => {
                let error = format!("test database is unavailable: {error}");
                if temps_database::test_utils::is_container_runtime_unavailable(&error) {
                    eprintln!("Skipping Docker-dependent test: {error}");
                    return;
                }
                panic!("Failed to set up test database: {error}");
            }
        };
        let db = test_db.db.clone();
        let advisory = "SELECT pg_advisory_xact_lock(7243)";

        let holder = db.begin().await.unwrap();
        holder.execute_unprepared(advisory).await.unwrap();

        let started = std::time::Instant::now();
        let waiter = begin_bounded_transaction(
            &db,
            42,
            "saving the configuration of",
            std::time::Duration::from_millis(300),
        )
        .await
        .unwrap();
        let waited = waiter.execute_unprepared(advisory).await;
        let error = lock_timeout_or_database_error(
            42,
            "saving the configuration of",
            std::time::Duration::from_millis(300),
            waited.unwrap_err().into(),
        );
        assert!(
            matches!(
                error,
                ExternalServiceError::LockTimeout {
                    service_id: 42,
                    operation: "saving the configuration of",
                    ..
                }
            ),
            "a held lock must surface as LockTimeout: {error:?}"
        );
        assert!(
            started.elapsed() < std::time::Duration::from_secs(8),
            "the wait must be bounded by the lock timeout, took {:?}",
            started.elapsed()
        );

        // The timeout aborts the waiter's transaction only; once the holder
        // lets go, the same lock is available again.
        let error = rollback_before_error(waiter, error).await;
        assert!(matches!(error, ExternalServiceError::LockTimeout { .. }));
        holder.rollback().await.unwrap();
        let retry = begin_bounded_transaction(
            &db,
            42,
            "saving the configuration of",
            std::time::Duration::from_millis(300),
        )
        .await
        .unwrap();
        retry.execute_unprepared(advisory).await.unwrap();
        retry.rollback().await.unwrap();
    }

    #[test]
    fn lock_timeout_message_is_recognised() {
        assert!(is_lock_timeout_message(
            "Query Error: error returned from database: canceling statement due to lock timeout"
        ));
        assert!(is_lock_timeout_message("error with SQLSTATE 55P03"));
        assert!(!is_lock_timeout_message(
            "duplicate key value violates unique constraint"
        ));
    }

    #[test]
    fn only_lock_timeouts_are_reclassified() {
        let timeout = std::time::Duration::from_secs(30);
        let classified = lock_timeout_or_database_error(
            9,
            "saving the configuration of",
            timeout,
            ExternalServiceError::DatabaseError {
                reason: "canceling statement due to lock timeout".to_string(),
            },
        );
        match &classified {
            ExternalServiceError::LockTimeout {
                service_id,
                operation,
                timeout_secs,
            } => {
                assert_eq!(*service_id, 9);
                assert_eq!(*operation, "saving the configuration of");
                assert_eq!(*timeout_secs, 30);
            }
            other => panic!("expected LockTimeout, got {other:?}"),
        }
        let message = classified.to_string();
        assert!(message.contains("service 9"), "{message}");
        assert!(message.contains("saving the configuration of"), "{message}");
        assert!(message.contains("30s"), "{message}");

        let unrelated = lock_timeout_or_database_error(
            9,
            "saving the configuration of",
            timeout,
            ExternalServiceError::DatabaseError {
                reason: "connection reset by peer".to_string(),
            },
        );
        assert!(
            matches!(unrelated, ExternalServiceError::DatabaseError { .. }),
            "{unrelated:?}"
        );
        let other_variant = lock_timeout_or_database_error(
            9,
            "saving the configuration of",
            timeout,
            ExternalServiceError::ServiceNotFound { id: 9 },
        );
        assert!(
            matches!(
                other_variant,
                ExternalServiceError::ServiceNotFound { id: 9 }
            ),
            "{other_variant:?}"
        );
    }

    #[tokio::test]
    async fn service_name_reservation_locks_before_check_and_rejects_collision() {
        let existing = encrypted_service_model(17, serde_json::json!({}));
        let db = sea_orm::MockDatabase::new(sea_orm::DatabaseBackend::Postgres)
            .append_exec_results([sea_orm::MockExecResult {
                last_insert_id: 0,
                rows_affected: 1,
            }])
            .append_query_results([vec![existing]])
            .into_connection();
        let txn = db.begin().await.unwrap();
        let error = reserve_service_name(&txn, "postgres-test", None)
            .await
            .unwrap_err();
        assert!(matches!(
            error,
            ExternalServiceError::ServiceNameConflict {
                existing_service_id: 17,
                ..
            }
        ));
        txn.rollback().await.unwrap();
        let log = db.into_transaction_log();
        let statements = log[0].statements();
        let lock = statements
            .iter()
            .position(|statement| statement.sql.contains("pg_advisory_xact_lock"))
            .unwrap();
        let check = statements
            .iter()
            .position(|statement| {
                statement.sql.contains("SELECT") && statement.sql.contains("external_services")
            })
            .unwrap();
        assert!(lock < check);
        assert!(statements
            .iter()
            .all(|statement| !statement.sql.contains("INSERT")));
    }

    fn active_name_restore(id: i32, name: &str) -> temps_entities::restore_runs::Model {
        temps_entities::restore_runs::Model {
            id,
            source_backup_id: 1,
            source_service_id: 17,
            target_service_id: None,
            target_service_name: Some(name.to_string()),
            mode: "new_service".to_string(),
            status: "running".to_string(),
            phase: "provision".to_string(),
            recovery_target: None,
            parameter_overrides: serde_json::json!({}),
            resume_token: None,
            log_id: "owned-log".to_string(),
            error_message: None,
            attempt: 1,
            started_at: Some(Utc::now()),
            finished_at: None,
            created_by: 1,
            created_at: Utc::now(),
            updated_at: Utc::now(),
            cancel_requested_at: None,
            cancel_requested_by: None,
        }
    }

    #[tokio::test]
    async fn service_name_reservation_refuses_a_durable_clone_reservation() {
        let db = sea_orm::MockDatabase::new(sea_orm::DatabaseBackend::Postgres)
            .append_exec_results([sea_orm::MockExecResult {
                last_insert_id: 0,
                rows_affected: 1,
            }])
            .append_query_results([Vec::<external_services::Model>::new()])
            .append_query_results([vec![active_name_restore(41, "copy")]])
            .into_connection();
        let txn = db.begin().await.unwrap();
        let error = reserve_service_name(&txn, "copy", None).await.unwrap_err();
        assert!(matches!(
            error,
            ExternalServiceError::ServiceNameRestoreConflict {
                restore_run_id: 41,
                ..
            }
        ));
        txn.rollback().await.unwrap();
        let log = db.into_transaction_log();
        assert!(log[0]
            .statements()
            .iter()
            .all(|s| !s.sql.contains("INSERT")));
    }
}
