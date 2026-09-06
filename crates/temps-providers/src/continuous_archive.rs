// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Shared guard for services with a continuous, standing archiving process
//! (Postgres WAL-G `archive_command`, MariaDB's binlog shipper) that runs
//! independently of any single backup run.
//!
//! Unlike a one-shot snapshot engine (MongoDB dump, Redis RDB copy, S3
//! mirror), these mechanisms write to a destination that persists across
//! runs. Left to re-resolve that destination from whatever a backup schedule
//! currently says, they drift: a schedule edit — even an unrelated one, for
//! MariaDB's every-tick schedule scan — can silently redirect a running
//! archive stream mid-flight, orphaning everything shipped to the previous
//! source. `external_services.continuous_archive_s3_source_id` makes that
//! destination an explicit, persisted decision instead: pinned once
//! (defaulting to the instance's Cloud-managed source when one exists), and
//! only ever moved by a deliberate, audited repoint.

use sea_orm::{ActiveModelTrait, ColumnTrait, DatabaseConnection, EntityTrait, QueryFilter, Set};
use temps_entities::{external_services, s3_sources};

#[derive(Debug, thiserror::Error)]
pub enum ContinuousArchiveError {
    /// The service's pin (existing or newly-defaulted) does not match what
    /// the caller requested. Permanent until the operator acts — retrying
    /// the same request will never succeed.
    #[error("{0}")]
    Mismatch(String),
    /// A database error occurred while reading or persisting the pin.
    #[error("Database error while {context}: {source}")]
    Database {
        context: String,
        #[source]
        source: sea_orm::DbErr,
    },
}

/// Ensure `service`'s continuous archive source is pinned to `requested_source_id`,
/// rejecting the request instead of silently moving the pin if it already
/// points elsewhere.
///
/// - Already pinned and matching: no-op.
/// - Already pinned and mismatched: `Mismatch` — the caller must either
///   target the pinned source or explicitly repoint the service.
/// - Unpinned, and this instance has a Cloud-managed S3 source that differs
///   from what was requested: `Mismatch` — new services default to Cloud,
///   so using something else requires an explicit repoint too.
/// - Unpinned otherwise: pins to `requested_source_id` and returns it.
///
/// Returns the pinned source ID on success (equal to `requested_source_id`
/// unless a mismatch error was returned instead).
pub async fn ensure_continuous_archive_source_pin(
    db: &DatabaseConnection,
    service: &external_services::Model,
    requested_source_id: i32,
    mechanism: &str,
) -> Result<i32, ContinuousArchiveError> {
    if let Some(pinned_source_id) = service.continuous_archive_s3_source_id {
        if pinned_source_id == requested_source_id {
            return Ok(pinned_source_id);
        }
        return Err(ContinuousArchiveError::Mismatch(format!(
            "service {} ('{}') has its {mechanism} pinned to S3 source {pinned_source_id}, but \
             this operation would use S3 source {requested_source_id} instead. Archiving cannot \
             silently move between sources without stranding data already written under the \
             pinned source. Point the relevant schedule at S3 source {pinned_source_id}, or \
             explicitly repoint the service's continuous archive source if you intend to move it.",
            service.id, service.name,
        )));
    }

    let default_source_id = s3_sources::Entity::find()
        .filter(s3_sources::Column::ManagedByCloud.eq(true))
        .one(db)
        .await
        .map_err(|e| ContinuousArchiveError::Database {
            context: format!(
                "looking up the Cloud-managed S3 source for service {}",
                service.id
            ),
            source: e,
        })?
        .map(|source| source.id)
        .unwrap_or(requested_source_id);

    if default_source_id != requested_source_id {
        return Err(ContinuousArchiveError::Mismatch(format!(
            "service {} ('{}') has no {mechanism} source pinned yet, and this instance has a \
             Cloud-managed S3 source ({default_source_id}) that new services default to — but \
             this operation would use S3 source {requested_source_id} instead. Point the \
             relevant schedule at S3 source {default_source_id}, or explicitly repoint the \
             service's continuous archive source if you intend to use {requested_source_id} \
             instead.",
            service.id, service.name,
        )));
    }

    let now = chrono::Utc::now();
    external_services::ActiveModel {
        id: Set(service.id),
        continuous_archive_s3_source_id: Set(Some(default_source_id)),
        continuous_archive_pinned_at: Set(Some(now)),
        ..Default::default()
    }
    .update(db)
    .await
    .map_err(|e| ContinuousArchiveError::Database {
        context: format!(
            "pinning service {} to continuous archive source {default_source_id}",
            service.id
        ),
        source: e,
    })?;

    Ok(default_source_id)
}

#[cfg(test)]
mod tests {
    use super::*;
    use sea_orm::{DatabaseBackend, MockDatabase, MockExecResult};

    fn service_row(continuous_archive_s3_source_id: Option<i32>) -> external_services::Model {
        let now = chrono::Utc::now();
        external_services::Model {
            id: 42,
            name: "test-svc".to_string(),
            service_type: "postgres".to_string(),
            topology: "standalone".to_string(),
            status: "running".to_string(),
            created_at: now,
            updated_at: now,
            node_id: None,
            version: None,
            slug: None,
            config: None,
            error_message: None,
            health_status: None,
            last_health_check_at: None,
            last_health_error: None,
            consecutive_health_failures: 0,
            health_metadata: None,
            metrics_enabled: false,
            default_backup_provisioned: false,
            ai_data_access: false,
            container_name: None,
            created_by_user_id: None,
            continuous_archive_s3_source_id,
            continuous_archive_pinned_at: continuous_archive_s3_source_id.map(|_| now),
        }
    }

    fn s3_source_row(id: i32, managed_by_cloud: bool) -> s3_sources::Model {
        let now = chrono::Utc::now();
        s3_sources::Model {
            id,
            name: format!("source-{id}"),
            bucket_name: "backups".to_string(),
            region: "us-east-1".to_string(),
            endpoint: None,
            bucket_path: String::new(),
            access_key_id: "ciphertext".to_string(),
            secret_key: "ciphertext".to_string(),
            session_token: None,
            credentials_expire_at: None,
            force_path_style: Some(true),
            is_default: false,
            managed_by_cloud,
            lifecycle_reconcile_failed_at: None,
            lifecycle_reconcile_generation: 0,
            created_at: now,
            updated_at: now,
        }
    }

    #[tokio::test]
    async fn matching_pin_is_a_pure_no_op() {
        let db = MockDatabase::new(DatabaseBackend::Postgres).into_connection();
        let service = service_row(Some(5));

        let result =
            ensure_continuous_archive_source_pin(&db, &service, 5, "WAL-G continuous archiving")
                .await;

        assert_eq!(result.unwrap(), 5);
        assert!(db.into_transaction_log().is_empty());
    }

    #[tokio::test]
    async fn mismatched_pin_is_rejected_not_silently_moved() {
        let db = MockDatabase::new(DatabaseBackend::Postgres).into_connection();
        let service = service_row(Some(5));

        let err =
            ensure_continuous_archive_source_pin(&db, &service, 9, "MariaDB binlog archiving")
                .await
                .unwrap_err();

        match err {
            ContinuousArchiveError::Mismatch(message) => {
                assert!(message.contains("pinned to S3 source 5"));
                assert!(
                    message.contains("requested S3 source 9")
                        || message.contains("use S3 source 9")
                );
            }
            other => panic!("expected Mismatch, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn first_use_with_no_managed_source_pins_to_the_requested_source() {
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results::<s3_sources::Model, _, _>(vec![vec![]])
            .append_exec_results(vec![MockExecResult {
                last_insert_id: 0,
                rows_affected: 1,
            }])
            .append_query_results(vec![vec![service_row(Some(7))]])
            .into_connection();
        let service = service_row(None);

        let result =
            ensure_continuous_archive_source_pin(&db, &service, 7, "WAL-G continuous archiving")
                .await;

        assert_eq!(result.unwrap(), 7);
        let log = format!("{:?}", db.into_transaction_log()).to_lowercase();
        assert!(log.contains("update"));
        assert!(log.contains("external_services"));
    }

    #[tokio::test]
    async fn first_use_against_the_wrong_source_is_rejected_when_a_managed_source_exists() {
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results(vec![vec![s3_source_row(2, true)]])
            .into_connection();
        let service = service_row(None);

        let err =
            ensure_continuous_archive_source_pin(&db, &service, 7, "MariaDB binlog archiving")
                .await
                .unwrap_err();

        match err {
            ContinuousArchiveError::Mismatch(message) => {
                assert!(message.contains("Cloud-managed S3 source (2)"));
                assert!(message.contains("S3 source 7"));
            }
            other => panic!("expected Mismatch, got {other:?}"),
        }
    }
}
