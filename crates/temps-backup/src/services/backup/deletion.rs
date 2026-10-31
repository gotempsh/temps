// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Deleting a backup, its S3 objects and its index entry.

use super::*;

/// Resolve the UUID-scoped directory containing one backup artifact.
///
/// Stored locations are database data and may pre-date the current engine.
/// Treat them as untrusted: matching the bucket is not enough, because the S3
/// credential can usually delete every object in that bucket. A deletable
/// location must live below the source's configured root and use one of Temps'
/// known per-snapshot layouts.
pub(super) fn validated_snapshot_prefix(
    location: &str,
    source: &S3Source,
    expected_backup_id: &str,
) -> Result<String, BackupError> {
    let key = s3_key_from_location(location, &source.bucket_name)?;
    let configured_root = source.bucket_path.trim_matches('/');
    let relative = if configured_root.is_empty() {
        key.as_str()
    } else {
        key.strip_prefix(configured_root)
            .and_then(|rest| rest.strip_prefix('/'))
            .ok_or_else(|| {
                BackupError::Validation(format!(
                    "Refusing to delete backup location '{}' outside configured S3 root '{}'",
                    location, configured_root
                ))
            })?
    };
    let segments: Vec<&str> = relative
        .split('/')
        .filter(|part| !part.is_empty())
        .collect();
    let uuid_index = segments
        .iter()
        .rposition(|segment| Uuid::parse_str(segment).is_ok())
        .ok_or_else(|| {
            BackupError::Validation(format!(
                "Refusing to delete backup location '{}' without a UUID-scoped snapshot directory",
                location
            ))
        })?;
    if segments[uuid_index] != expected_backup_id {
        return Err(BackupError::Validation(format!(
            "Refusing to delete backup location '{}' owned by snapshot '{}' instead of '{}'",
            location, segments[uuid_index], expected_backup_id
        )));
    }

    // A location is either the snapshot prefix itself or one object directly
    // below it. Anything deeper is not an artifact location produced by Temps.
    if segments.len() > uuid_index + 2 {
        return Err(BackupError::Validation(format!(
            "Refusing to delete unexpected backup layout '{}'",
            location
        )));
    }

    let known_layout = match segments.as_slice() {
        // Control-plane: backups/YYYY/MM/DD/<uuid>[/artifact]
        ["backups", year, month, day, ..]
            if [year, month, day]
                .iter()
                .all(|part| part.bytes().all(|byte| byte.is_ascii_digit()))
                && year.len() == 4
                && month.len() == 2
                && day.len() == 2
                && uuid_index == 4 =>
        {
            true
        }
        // External engines: external_services/<engine>/<service>/YYYY/MM/DD/<uuid>[/artifact]
        ["external_services", engine, service, year, month, day, ..]
            if matches!(*engine, "postgres" | "redis" | "mongodb" | "mariadb")
                && !service.is_empty()
                && [year, month, day]
                    .iter()
                    .all(|part| part.bytes().all(|byte| byte.is_ascii_digit()))
                && year.len() == 4
                && month.len() == 2
                && day.len() == 2
                && uuid_index == 6 =>
        {
            true
        }
        // S3 mirror snapshots omit the date hierarchy.
        ["external_services", "s3", service, ..] if !service.is_empty() && uuid_index == 3 => true,
        _ => false,
    };
    if !known_layout {
        return Err(BackupError::Validation(format!(
            "Refusing to delete unknown backup layout '{}'",
            location
        )));
    }

    let relative_prefix = segments[..=uuid_index].join("/");
    Ok(build_s3_key(configured_root, &relative_prefix))
}

impl BackupService {
    pub async fn delete_backup(&self, backup_id: &str) -> Result<(Backup, u64), BackupError> {
        info!(backup_id, "Deleting backup");

        let backup = temps_entities::backups::Entity::find()
            .filter(temps_entities::backups::Column::BackupId.eq(backup_id))
            .one(self.db.as_ref())
            .await?
            .ok_or_else(|| BackupError::NotFound {
                resource: "Backup".to_string(),
                detail: format!("backup id {}", backup_id),
            })?;

        self.delete_backup_model(backup).await
    }

    pub(super) async fn delete_backup_model(
        &self,
        backup: Backup,
    ) -> Result<(Backup, u64), BackupError> {
        // Phase one is intentionally short: establish a durable tombstone while
        // holding the row lock, then release the database connection before any
        // network I/O. A `deleting` row is retryable and cannot be restored.
        let transaction = self.db.begin().await?;
        let backup = temps_entities::backups::Entity::find_by_id(backup.id)
            .lock_exclusive()
            .one(&transaction)
            .await?
            .ok_or_else(|| BackupError::NotFound {
                resource: "Backup".to_string(),
                detail: format!("backup id {}", backup.backup_id),
            })?;

        if matches!(backup.state.as_str(), "pending" | "running") {
            return Err(BackupError::BackupNotTerminal {
                backup_id: backup.backup_id.clone(),
                state: backup.state.clone(),
            });
        }

        let restore_count = temps_entities::restore_runs::Entity::find()
            .filter(temps_entities::restore_runs::Column::SourceBackupId.eq(backup.id))
            .count(&transaction)
            .await?;
        if restore_count > 0 {
            return Err(BackupError::BackupHasRestoreHistory {
                backup_id: backup.backup_id.clone(),
                restore_count,
            });
        }

        let s3_source = temps_entities::s3_sources::Entity::find_by_id(backup.s3_source_id)
            .one(&transaction)
            .await?
            .ok_or_else(|| BackupError::NotFound {
                resource: "S3Source".to_string(),
                detail: "S3 source not found".to_string(),
            })?;

        let children = temps_entities::external_service_backups::Entity::find()
            .filter(temps_entities::external_service_backups::Column::BackupId.eq(backup.id))
            .all(&transaction)
            .await?;
        let metadata: serde_json::Value = serde_json::from_str(&backup.metadata)?;
        let engine = metadata
            .get("engine")
            .and_then(serde_json::Value::as_str)
            .unwrap_or("unknown");

        let is_walg = matches!(engine, "postgres_walg" | "postgres_cluster")
            || backup.s3_location.trim_end_matches('/').ends_with("/walg")
            || children
                .iter()
                .any(|child| child.s3_location.trim_end_matches('/').ends_with("/walg"));
        let mut prefixes = HashSet::new();
        let walg_plan = if is_walg {
            let target = validated_walg_target(&metadata, &backup.backup_id)?;
            let service_id = metadata
                .get("external_service_id")
                .or_else(|| metadata.get("service_id"))
                .and_then(serde_json::Value::as_i64)
                .and_then(|value| i32::try_from(value).ok())
                .ok_or_else(|| {
                    BackupError::Validation(format!(
                        "WAL-G backup {} has no external service identity",
                        backup.backup_id
                    ))
                })?;
            let service = temps_entities::external_services::Entity::find_by_id(service_id)
                .one(&transaction)
                .await?;
            let (service_name, service_type) = deletion_walg_service_identity(
                &children,
                service_id,
                &backup.backup_id,
                service
                    .as_ref()
                    .map(|service| (service.name.as_str(), service.service_type.as_str())),
            )?;
            let deletion_engine =
                WalgDeletionEngine::resolve(engine, &service_type, &backup.backup_id)?;
            deletion_engine.validate_repository(
                &backup.s3_location,
                &s3_source.bucket_name,
                &s3_source.bucket_path,
                &service_name,
            )?;
            let container = if let Some(service) = service.as_ref() {
                Some(if deletion_engine == WalgDeletionEngine::PostgresCluster {
                    // The monitor knows the primary; `service_members.role`
                    // is `replica` for every data node.
                    self.external_service_manager
                        .get_cluster_primary_member(service_id)
                        .await
                        .map_err(|e| {
                            BackupError::ExternalService(format!(
                                "Cannot delete WAL-G cluster backup {}: {}",
                                backup.backup_id, e
                            ))
                        })?
                        .ok_or_else(|| {
                            BackupError::Validation(format!(
                                "WAL-G cluster backup {} belongs to service {}, which is not a PostgreSQL cluster",
                                backup.backup_id, service_id
                            ))
                        })?
                        .container_name
                } else {
                    crate::engines::dispatch::service_container_name(service)
                })
            } else {
                if !matches!(
                    deletion_engine,
                    WalgDeletionEngine::Postgres | WalgDeletionEngine::PostgresCluster
                ) {
                    return Err(BackupError::Validation(format!("Backup {} requires its {} service for safe WAL-G cleanup; orphan cleanup is supported for PostgreSQL repositories", backup.backup_id, service_type)));
                }
                None
            };
            Some((service_name, container, target, deletion_engine))
        } else {
            for location in std::iter::once(backup.s3_location.as_str())
                .chain(children.iter().map(|child| child.s3_location.as_str()))
                .filter(|location| !location.is_empty())
            {
                prefixes.insert(validated_snapshot_prefix(
                    location,
                    &s3_source,
                    &backup.backup_id,
                )?);
            }
            if prefixes.is_empty() && backup.state != "failed" {
                // A `failed` backup never finished uploading, so there is
                // nothing on remote storage to attribute — safe to fall
                // through and delete only the bookkeeping rows below.
                // Any other state reaching here with no artifact indicates
                // a lost reference to real remote data, which must not be
                // silently discarded.
                return Err(BackupError::Validation(format!(
                    "Backup {} has no attributable remote artifact",
                    backup.backup_id
                )));
            }
            if prefixes.len() > 100 {
                return Err(BackupError::Validation(format!(
                    "Backup {} references {} snapshot prefixes; refusing an unbounded deletion",
                    backup.backup_id,
                    prefixes.len()
                )));
            }
            None
        };

        if backup.state != "deleting" {
            let mut active: temps_entities::backups::ActiveModel = backup.clone().into();
            active.state = Set("deleting".to_string());
            active.update(&transaction).await?;
        }
        transaction.commit().await?;

        let s3_client = self.create_s3_client(&s3_source).await?;
        let remote_deadline = time::Instant::now() + std::time::Duration::from_secs(300);
        let has_control_plane_artifact = prefixes.iter().any(|prefix| {
            let root = s3_source.bucket_path.trim_matches('/');
            let relative = if root.is_empty() {
                prefix.as_str()
            } else {
                prefix
                    .strip_prefix(root)
                    .and_then(|value| value.strip_prefix('/'))
                    .unwrap_or("")
            };
            relative.starts_with("backups/")
        });
        let mut deleted_objects = 0_u64;
        if let Some((service_name, container, target, deletion_engine)) = walg_plan {
            time::timeout_at(
                remote_deadline,
                self.delete_walg_target_with_optional_service(
                    &backup,
                    &s3_source,
                    &service_name,
                    container.as_deref(),
                    &target,
                    deletion_engine,
                ),
            )
            .await
            .map_err(|_| BackupError::PartialDeletion {
                backup_id: backup.backup_id.clone(),
                deleted_objects: 0,
                reason: "Timed out waiting for WAL-G exact-target deletion".to_string(),
            })??;
            deleted_objects = 1;
        }
        for prefix in prefixes {
            match self
                .delete_s3_prefix(
                    &s3_client,
                    &s3_source.bucket_name,
                    &prefix,
                    &backup.backup_id,
                    remote_deadline,
                )
                .await
            {
                Ok(count) => deleted_objects += count,
                Err(BackupError::PartialDeletion {
                    deleted_objects: count,
                    reason,
                    ..
                }) => {
                    return Err(BackupError::PartialDeletion {
                        backup_id: backup.backup_id.clone(),
                        deleted_objects: deleted_objects + count,
                        reason,
                    });
                }
                Err(error) => {
                    return Err(BackupError::PartialDeletion {
                        backup_id: backup.backup_id.clone(),
                        deleted_objects,
                        reason: error.to_string(),
                    });
                }
            }
        }
        if has_control_plane_artifact {
            let index_deadline = std::cmp::min(
                remote_deadline,
                time::Instant::now() + std::time::Duration::from_secs(60),
            );
            let index_result = time::timeout_at(
                index_deadline,
                self.remove_backup_from_index(&s3_client, &s3_source, &backup.backup_id),
            )
            .await;
            if let Err(error) = index_result
                .map_err(|_| BackupError::S3("Backup index update timed out".to_string()))
                .and_then(|result| result)
            {
                return Err(BackupError::PartialDeletion {
                    backup_id: backup.backup_id.clone(),
                    deleted_objects,
                    reason: format!("Failed to remove backup from backups/index.json: {}", error),
                });
            }
        }

        // Phase two re-locks the tombstone. If the process died after remote
        // deletion, a retry reaches this point idempotently (zero objects).
        let transaction = self.db.begin().await?;
        let current = temps_entities::backups::Entity::find_by_id(backup.id)
            .lock_exclusive()
            .one(&transaction)
            .await?;
        let Some(current) = current else {
            transaction.rollback().await?;
            return Ok((backup, deleted_objects));
        };
        if current.state != "deleting" {
            return Err(BackupError::BackupNotTerminal {
                backup_id: current.backup_id,
                state: current.state,
            });
        }
        let delete_result = temps_entities::backups::Entity::delete_many()
            .filter(temps_entities::backups::Column::Id.eq(backup.id))
            .exec(&transaction)
            .await;
        if let Err(error) = delete_result {
            if deleted_objects > 0 {
                return Err(BackupError::PartialDeletion {
                    backup_id: backup.backup_id.clone(),
                    deleted_objects,
                    reason: format!("Database row deletion failed: {}", error),
                });
            }
            return Err(BackupError::Database(error));
        }

        if let Err(error) = transaction.commit().await {
            if deleted_objects > 0 {
                return Err(BackupError::PartialDeletion {
                    backup_id: backup.backup_id.clone(),
                    deleted_objects,
                    reason: format!("Database commit failed after object deletion: {}", error),
                });
            }
            return Err(BackupError::Database(error));
        }

        info!(backup_id = %backup.backup_id, "Backup deleted successfully");
        // Tell anyone cataloging this instance's backups elsewhere. Best
        // effort: the deletion is done either way, and the Cloud mirror has
        // its own presence check for events that never arrive.
        if let Err(error) = self
            .queue
            .send(temps_core::Job::BackupDeleted(
                temps_core::BackupDeletedJob {
                    backup_id: backup.id,
                    backup_uuid: backup.backup_id.clone(),
                    engine: engine.to_string(),
                    s3_location: backup.s3_location.clone(),
                },
            ))
            .await
        {
            warn!(backup_id = %backup.backup_id, error = %error, "could not publish BackupDeleted");
        }
        Ok((backup, deleted_objects))
    }

    async fn delete_s3_prefix(
        &self,
        client: &S3Client,
        bucket: &str,
        prefix: &str,
        backup_id: &str,
        deadline: time::Instant,
    ) -> Result<u64, BackupError> {
        let normalized = format!("{}/", prefix.trim_end_matches('/'));
        let mut continuation: Option<String> = None;
        let mut deleted_objects = 0_u64;
        loop {
            let mut request = client.list_objects_v2().bucket(bucket).prefix(&normalized);
            if let Some(token) = continuation.take() {
                request = request.continuation_token(token);
            }
            let response = time::timeout_at(deadline, request.send())
                .await
                .map_err(|_| BackupError::PartialDeletion {
                    backup_id: backup_id.to_string(),
                    deleted_objects,
                    reason: format!("Timed out listing s3://{}/{}", bucket, normalized),
                })?
                .map_err(|e| {
                    let reason = format!(
                        "Failed to list prefix s3://{}/{}: {}",
                        bucket, normalized, e
                    );
                    if deleted_objects > 0 {
                        BackupError::PartialDeletion {
                            backup_id: backup_id.to_string(),
                            deleted_objects,
                            reason,
                        }
                    } else {
                        BackupError::S3(format!("{} for backup {}", reason, backup_id))
                    }
                })?;
            for object in response.contents() {
                let Some(key) = object.key() else { continue };
                if deleted_objects >= 10_000 {
                    return Err(BackupError::PartialDeletion {
                        backup_id: backup_id.to_string(),
                        deleted_objects,
                        reason: format!(
                            "Safety limit reached while deleting s3://{}/{}; retry to continue",
                            bucket, normalized
                        ),
                    });
                }
                let deletion = time::timeout_at(
                    deadline,
                    client.delete_object().bucket(bucket).key(key).send(),
                )
                .await;
                if let Err(error) = deletion
                    .map_err(|_| "request timed out".to_string())
                    .and_then(|result| result.map_err(|error| error.to_string()))
                {
                    // S3 may have applied a DeleteObject even when the client
                    // lost the response, so any failed deletion request is an
                    // unknown/partial destructive outcome, including the first.
                    return Err(BackupError::PartialDeletion {
                        backup_id: backup_id.to_string(),
                        deleted_objects,
                        reason: format!(
                            "Delete result for s3://{}/{} is unknown: {}",
                            bucket, key, error
                        ),
                    });
                }
                deleted_objects += 1;
            }
            if response.is_truncated().unwrap_or(false) {
                continuation = response.next_continuation_token().map(str::to_owned);
                if continuation.is_none() {
                    break;
                }
            } else {
                break;
            }
        }
        Ok(deleted_objects)
    }
}

#[cfg(test)]
mod tests {
    use super::super::test_support::*;
    use super::*;
    use sea_orm::{DatabaseBackend, MockDatabase, MockExecResult};
    use temps_core::EncryptionService;
    use temps_entities::s3_sources;

    #[test]
    fn snapshot_prefix_validation_requires_configured_root_and_exact_backup_id() {
        let source = s3_sources::Model {
            id: 1,
            name: "test".to_string(),
            bucket_name: "bucket".to_string(),
            bucket_path: "tenant".to_string(),
            access_key_id: "key".to_string(),
            secret_key: "secret".to_string(),
            session_token: None,
            credentials_expire_at: None,
            region: "us-east-1".to_string(),
            endpoint: None,
            force_path_style: Some(true),
            is_default: false,
            managed_by_cloud: false,
            lifecycle_reconcile_failed_at: None,
            lifecycle_reconcile_generation: 0,
            created_at: Utc::now(),
            updated_at: Utc::now(),
            backing_service_id: None,
        };
        let id = "4dc29e1a-1234-4abc-8def-123456789abc";
        assert_eq!(
            validated_snapshot_prefix(
                &format!("s3://bucket/tenant/external_services/s3/assets-prod/{id}/metadata.json"),
                &source,
                id,
            )
            .expect("known snapshot layout should validate"),
            format!("tenant/external_services/s3/assets-prod/{id}")
        );
        assert!(validated_snapshot_prefix(
            &format!("s3://bucket/other/external_services/s3/assets-prod/{id}"),
            &source,
            id,
        )
        .is_err());
        assert!(validated_snapshot_prefix(
            "s3://bucket/tenant/external_services/s3/assets-prod/00000000-0000-4000-8000-000000000000",
            &source,
            id,
        )
        .is_err());
    }

    #[tokio::test]
    async fn delete_backup_refuses_restore_history_before_object_deletion() {
        let mut count_row = std::collections::BTreeMap::new();
        count_row.insert("num_items".to_string(), sea_orm::Value::BigInt(Some(1)));
        let backup = temps_entities::backups::Model {
            id: 42,
            name: "restore-source".to_string(),
            backup_id: "backup-uuid".to_string(),
            schedule_id: Some(7),
            backup_type: "full".to_string(),
            state: "completed".to_string(),
            started_at: Utc::now(),
            finished_at: Some(Utc::now()),
            size_bytes: Some(100),
            file_count: None,
            s3_source_id: 1,
            s3_location: "backups/archive.sql.gz".to_string(),
            error_message: None,
            metadata: r#"{"engine":"postgres_pgdump"}"#.to_string(),
            checksum: None,
            compression_type: "gzip".to_string(),
            created_by: 1,
            expires_at: None,
            tags: "[]".to_string(),
            schedule_run_id: None,
        };
        let db = Arc::new(
            MockDatabase::new(DatabaseBackend::Postgres)
                .append_query_results(vec![vec![backup.clone()]])
                .append_query_results(vec![vec![count_row]])
                .into_connection(),
        );
        let service = build_service_for_mock(db).expect("mock service should construct");

        let error = service
            .delete_backup_model(backup)
            .await
            .expect_err("restore history must block backup deletion");
        assert!(matches!(
            error,
            BackupError::BackupHasRestoreHistory {
                backup_id,
                restore_count: 1
            } if backup_id == "backup-uuid"
        ));
    }

    #[tokio::test]
    async fn delete_backup_model_removes_failed_backup_with_no_remote_artifact() {
        // Regression test: a backup whose upload never completed (state
        // "failed") has no s3_location on the parent row or any child
        // external_service_backups row. Retention cleanup must still be
        // able to delete this bookkeeping row instead of hard-failing with
        // "no attributable remote artifact" forever.
        let failed_backup = temps_entities::backups::Model {
            id: 55,
            name: "failed-backup".to_string(),
            backup_id: "failed-backup-uuid".to_string(),
            schedule_id: Some(9),
            backup_type: "full".to_string(),
            state: "failed".to_string(),
            started_at: Utc::now(),
            finished_at: Some(Utc::now()),
            size_bytes: None,
            file_count: None,
            s3_source_id: 1,
            s3_location: "".to_string(),
            error_message: Some("upload failed".to_string()),
            metadata: "{}".to_string(),
            checksum: None,
            compression_type: "gzip".to_string(),
            created_by: 1,
            expires_at: None,
            tags: "[]".to_string(),
            schedule_run_id: None,
        };

        let mut count_row = std::collections::BTreeMap::new();
        count_row.insert("num_items".to_string(), sea_orm::Value::BigInt(Some(0)));

        let encryption_service =
            Arc::new(EncryptionService::new("test_encryption_key_1234567890ab").unwrap());
        let encrypted_access_key = encryption_service.encrypt_string("test-key").unwrap();
        let encrypted_secret_key = encryption_service.encrypt_string("test-secret").unwrap();
        let s3_source = s3_sources::Model {
            id: 1,
            name: "test-source".to_string(),
            bucket_name: "test-bucket".to_string(),
            bucket_path: "/backups".to_string(),
            access_key_id: encrypted_access_key,
            secret_key: encrypted_secret_key,
            session_token: None,
            credentials_expire_at: None,
            region: "us-east-1".to_string(),
            endpoint: Some("http://localhost:9000".to_string()),
            force_path_style: Some(true),
            is_default: false,
            managed_by_cloud: false,
            lifecycle_reconcile_failed_at: None,
            lifecycle_reconcile_generation: 0,
            created_at: Utc::now(),
            updated_at: Utc::now(),
            backing_service_id: None,
        };

        let deleting_backup = temps_entities::backups::Model {
            state: "deleting".to_string(),
            ..failed_backup.clone()
        };

        let db = Arc::new(
            MockDatabase::new(DatabaseBackend::Postgres)
                // Phase one: lock + fetch the backup row.
                .append_query_results(vec![vec![failed_backup.clone()]])
                // restore_runs count
                .append_query_results(vec![vec![count_row]])
                // s3_sources lookup
                .append_query_results(vec![vec![s3_source.clone()]])
                // external_service_backups children (none — the upload never got that far)
                .append_query_results(vec![
                    Vec::<temps_entities::external_service_backups::Model>::new(),
                ])
                // state -> "deleting" (UPDATE ... RETURNING)
                .append_query_results(vec![vec![deleting_backup.clone()]])
                // Phase two: re-lock the tombstone before deleting.
                .append_query_results(vec![vec![deleting_backup]])
                // Final DB row deletion.
                .append_exec_results(vec![MockExecResult {
                    last_insert_id: 0,
                    rows_affected: 1,
                }])
                .into_connection(),
        );
        let service = build_service_for_mock(db).expect("mock service should construct");

        let (_, deleted_objects) = service
            .delete_backup_model(failed_backup)
            .await
            .expect("a failed backup with no remote artifact must still be deletable");
        assert_eq!(
            deleted_objects, 0,
            "nothing was ever uploaded, so nothing should be deleted remotely"
        );
    }
}
