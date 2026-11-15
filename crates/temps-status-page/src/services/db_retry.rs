// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Retry for single-row database writes that can hit a transient connection
//! failure or a deadlock.

use std::future::Future;
use std::time::Duration;

use sea_orm::DbErr;
use temps_core::retry::RetryConfig;
use tracing::error;

use super::types::StatusPageError;

/// Four attempts in total, waiting 50, 100 and 200 ms between them.
fn db_write_retry() -> RetryConfig {
    RetryConfig::new(4).with_base_delay(Duration::from_millis(50))
}

/// Whether `error` can clear on its own: the pool could not hand out a
/// connection, the connection dropped, or the query hit a deadlock, a timeout
/// or a connection problem. Everything else (constraint violations, missing
/// rows, bad input) fails the same way on every attempt.
pub(crate) fn is_transient_db_error(error: &DbErr) -> bool {
    match error {
        DbErr::ConnectionAcquire(_) | DbErr::Conn(_) => true,
        DbErr::Query(runtime_err) => {
            let message = runtime_err.to_string();
            message.contains("deadlock")
                || message.contains("timeout")
                || message.contains("connection")
        }
        _ => false,
    }
}

/// Run `operation`, retrying it while it fails with a transient database
/// error, and turn the final error into [`StatusPageError::Database`].
pub(crate) async fn with_db_retry<T, F, Fut>(
    operation_name: &str,
    operation: F,
) -> Result<T, StatusPageError>
where
    F: FnMut() -> Fut,
    Fut: Future<Output = Result<T, DbErr>>,
{
    db_write_retry()
        .retry_if(operation, is_transient_db_error)
        .await
        .map_err(|e| {
            error!("{} failed: {:?}", operation_name, e);
            StatusPageError::Database(e)
        })
}

#[cfg(test)]
mod tests {
    use super::*;
    use sea_orm::{ConnAcquireErr, RuntimeErr};
    use std::sync::atomic::{AtomicU32, Ordering};

    fn connection_dropped() -> DbErr {
        DbErr::Conn(RuntimeErr::Internal("connection reset by peer".to_string()))
    }

    #[test]
    fn connection_failures_and_deadlocks_are_transient() {
        assert!(is_transient_db_error(&DbErr::ConnectionAcquire(
            ConnAcquireErr::Timeout
        )));
        assert!(is_transient_db_error(&connection_dropped()));
        assert!(is_transient_db_error(&DbErr::Query(RuntimeErr::Internal(
            "deadlock detected".to_string()
        ))));
        assert!(is_transient_db_error(&DbErr::Query(RuntimeErr::Internal(
            "statement timeout".to_string()
        ))));
    }

    #[test]
    fn other_errors_are_not_transient() {
        assert!(!is_transient_db_error(&DbErr::RecordNotFound(
            "status_incidents id=4".to_string()
        )));
        assert!(!is_transient_db_error(&DbErr::Query(RuntimeErr::Internal(
            "duplicate key value violates unique constraint".to_string()
        ))));
        assert!(!is_transient_db_error(&DbErr::Custom(
            "bad input".to_string()
        )));
    }

    #[tokio::test]
    async fn a_transient_error_is_retried_until_the_write_succeeds() {
        let calls = AtomicU32::new(0);
        let result = with_db_retry("record_check for monitor 1", || {
            let attempt = calls.fetch_add(1, Ordering::SeqCst);
            async move {
                if attempt < 2 {
                    Err(connection_dropped())
                } else {
                    Ok(attempt)
                }
            }
        })
        .await;

        assert_eq!(result.expect("third attempt succeeds"), 2);
        assert_eq!(calls.load(Ordering::SeqCst), 3);
    }

    #[tokio::test]
    async fn a_transient_error_stops_after_four_attempts() {
        let calls = AtomicU32::new(0);
        let result: Result<(), _> = with_db_retry("create_incident for project 1", || {
            calls.fetch_add(1, Ordering::SeqCst);
            async { Err(connection_dropped()) }
        })
        .await;

        assert!(matches!(
            result,
            Err(StatusPageError::Database(DbErr::Conn(_)))
        ));
        assert_eq!(calls.load(Ordering::SeqCst), 4);
    }

    #[tokio::test]
    async fn a_non_transient_error_is_not_retried() {
        let calls = AtomicU32::new(0);
        let result: Result<(), _> = with_db_retry("create_incident for project 1", || {
            calls.fetch_add(1, Ordering::SeqCst);
            async { Err(DbErr::RecordNotFound("status_monitors id=9".to_string())) }
        })
        .await;

        assert!(matches!(
            result,
            Err(StatusPageError::Database(DbErr::RecordNotFound(_)))
        ));
        assert_eq!(calls.load(Ordering::SeqCst), 1);
    }
}
