// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Recovery sets, publishing a completed fan-out run as one restorable set.

use super::*;

/// Result of attempting to publish a native whole-instance recovery set for a
/// terminal backup event.
///
/// Only fully successful scheduled runs that include a control-plane backup
/// are published. This prevents `temps backup restore` from presenting a
/// partial fan-out run as a recoverable instance snapshot.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RecoverySetPublication {
    NotScheduled,
    Pending {
        schedule_run_id: i64,
    },
    Incomplete {
        schedule_run_id: i64,
        failed_backup_ids: Vec<i32>,
    },
    NoControlPlane {
        schedule_run_id: i64,
    },
    AlreadyPublished {
        schedule_run_id: i64,
        backup_id: String,
    },
    Published {
        schedule_run_id: i64,
        backup_id: String,
    },
}

impl BackupService {
    async fn sha256_s3_object(
        s3_client: &S3Client,
        bucket: &str,
        location: &str,
    ) -> Result<String, BackupError> {
        use sha2::{Digest, Sha256};
        use tokio::io::AsyncReadExt;

        let key = if let Some(uri) = location.strip_prefix("s3://") {
            let (location_bucket, key) = uri.split_once('/').ok_or_else(|| {
                BackupError::Validation(format!("Invalid S3 backup location '{}'", location))
            })?;
            if location_bucket != bucket {
                return Err(BackupError::Validation(format!(
                    "Backup location bucket '{}' does not match source bucket '{}'",
                    location_bucket, bucket
                )));
            }
            key
        } else {
            location.trim_start_matches('/')
        };
        if key.is_empty() {
            return Err(BackupError::Validation(
                "Control-plane backup location has no object key".to_string(),
            ));
        }

        let response = s3_client
            .get_object()
            .bucket(bucket)
            .key(key)
            .send()
            .await
            .map_err(|error| {
                BackupError::S3(crate::engines::v2_common::describe_sdk_error(
                    "download control-plane backup for integrity hashing",
                    &error,
                ))
            })?;
        let mut reader = response.body.into_async_read();
        let mut hasher = Sha256::new();
        let mut buffer = vec![0_u8; 64 * 1024];
        loop {
            let read = reader
                .read(&mut buffer)
                .await
                .map_err(|error| BackupError::Internal {
                    message: format!(
                        "Failed to hash control-plane backup {}: {}",
                        location, error
                    ),
                })?;
            if read == 0 {
                break;
            }
            hasher.update(&buffer[..read]);
        }
        Ok(hex::encode(hasher.finalize()))
    }

    /// Publish the aggregate recovery manifest consumed by `temps backup
    /// restore` once every backup in a fan-out schedule run has completed.
    ///
    /// The executor publishes one terminal event per child. Those events are
    /// intentionally treated as hints: the database is re-read here and is
    /// the source of truth. The operation is idempotent and refuses to publish
    /// when any child failed or is still live.
    pub async fn publish_recovery_set_if_complete(
        &self,
        terminal_backup_id: i32,
    ) -> Result<RecoverySetPublication, BackupError> {
        let terminal_backup = temps_entities::backups::Entity::find_by_id(terminal_backup_id)
            .one(self.db.as_ref())
            .await?
            .ok_or_else(|| BackupError::NotFound {
                resource: "Backup".to_string(),
                detail: format!("terminal backup row {}", terminal_backup_id),
            })?;

        let Some(schedule_run_id) = terminal_backup.schedule_run_id else {
            return Ok(RecoverySetPublication::NotScheduled);
        };

        let run_backups = temps_entities::backups::Entity::find()
            .filter(temps_entities::backups::Column::ScheduleRunId.eq(schedule_run_id))
            .order_by_asc(temps_entities::backups::Column::Id)
            .all(self.db.as_ref())
            .await?;

        if run_backups
            .iter()
            .any(|backup| matches!(backup.state.as_str(), "pending" | "running"))
        {
            return Ok(RecoverySetPublication::Pending { schedule_run_id });
        }

        let failed_backup_ids = run_backups
            .iter()
            .filter(|backup| backup.state != "completed")
            .map(|backup| backup.id)
            .collect::<Vec<_>>();
        if !failed_backup_ids.is_empty() {
            return Ok(RecoverySetPublication::Incomplete {
                schedule_run_id,
                failed_backup_ids,
            });
        }

        let mut parsed_metadata = Vec::with_capacity(run_backups.len());
        for backup in &run_backups {
            let metadata =
                serde_json::from_str::<serde_json::Value>(&backup.metadata).map_err(|error| {
                    BackupError::Validation(format!(
                        "Backup {} in schedule run {} has invalid metadata: {}",
                        backup.id, schedule_run_id, error
                    ))
                })?;
            parsed_metadata.push((backup, metadata));
        }

        let Some((control_plane, control_plane_metadata)) =
            parsed_metadata.iter().find(|(_, metadata)| {
                metadata.get("engine").and_then(serde_json::Value::as_str) == Some("control_plane")
            })
        else {
            return Ok(RecoverySetPublication::NoControlPlane { schedule_run_id });
        };

        if control_plane_metadata
            .get("recovery_set_published")
            .and_then(serde_json::Value::as_bool)
            == Some(true)
        {
            return Ok(RecoverySetPublication::AlreadyPublished {
                schedule_run_id,
                backup_id: control_plane.backup_id.clone(),
            });
        }

        let mut service_ids = BTreeSet::new();
        for (backup, metadata) in &parsed_metadata {
            if backup.id == control_plane.id {
                continue;
            }
            let service_id = metadata
                .get("external_service_id")
                .or_else(|| metadata.get("service_id"))
                .and_then(serde_json::Value::as_i64)
                .and_then(|value| i32::try_from(value).ok())
                .ok_or_else(|| {
                    BackupError::Validation(format!(
                        "External-service backup {} in schedule run {} has no service identity",
                        backup.id, schedule_run_id
                    ))
                })?;
            service_ids.insert(service_id);
        }
        let expected_service_ids = control_plane_metadata
            .get("expected_service_ids")
            .and_then(serde_json::Value::as_array)
            .ok_or_else(|| {
                BackupError::Validation(format!(
                    "Control-plane backup {} in schedule run {} has no expected service snapshot",
                    control_plane.id, schedule_run_id
                ))
            })?
            .iter()
            .map(|value| {
                value
                    .as_i64()
                    .and_then(|id| i32::try_from(id).ok())
                    .ok_or_else(|| {
                        BackupError::Validation(format!(
                            "Control-plane backup {} has an invalid expected service id",
                            control_plane.id
                        ))
                    })
            })
            .collect::<Result<BTreeSet<_>, _>>()?;
        if expected_service_ids != service_ids {
            return Err(BackupError::Validation(format!(
                "Schedule run {} backup coverage mismatch: expected services {:?}, completed services {:?}",
                schedule_run_id, expected_service_ids, service_ids
            )));
        }

        let services = if service_ids.is_empty() {
            Vec::new()
        } else {
            temps_entities::external_services::Entity::find()
                .filter(
                    temps_entities::external_services::Column::Id
                        .is_in(service_ids.iter().copied()),
                )
                .all(self.db.as_ref())
                .await?
        };
        let services_by_id = services
            .into_iter()
            .map(|service| (service.id, service))
            .collect::<BTreeMap<_, _>>();

        let mut external_service_backups = Vec::with_capacity(service_ids.len());
        for (backup, metadata) in &parsed_metadata {
            if backup.id == control_plane.id {
                continue;
            }
            let service_id = metadata
                .get("external_service_id")
                .or_else(|| metadata.get("service_id"))
                .and_then(serde_json::Value::as_i64)
                .and_then(|value| i32::try_from(value).ok())
                .ok_or_else(|| {
                    BackupError::Validation(format!(
                        "External-service backup {} in schedule run {} has no service identity",
                        backup.id, schedule_run_id
                    ))
                })?;
            let service = services_by_id
                .get(&service_id)
                .ok_or_else(|| BackupError::NotFound {
                    resource: "ExternalService".to_string(),
                    detail: format!(
                        "service {} referenced by backup {} in schedule run {}",
                        service_id, backup.id, schedule_run_id
                    ),
                })?;
            external_service_backups.push(json!({
                "backup_id": backup.id,
                "service_id": service_id,
                "s3_location": backup.s3_location,
                "state": backup.state,
                "size_bytes": backup.size_bytes,
                "type": backup.backup_type,
                "metadata": {
                    "service_type": service.service_type,
                    "service_name": service.name,
                }
            }));
        }

        let s3_source = temps_entities::s3_sources::Entity::find_by_id(control_plane.s3_source_id)
            .one(self.db.as_ref())
            .await?
            .ok_or_else(|| BackupError::NotFound {
                resource: "S3Source".to_string(),
                detail: format!(
                    "source {} referenced by recovery set {}",
                    control_plane.s3_source_id, control_plane.backup_id
                ),
            })?;
        let s3_client = self.create_s3_client(&s3_source).await?;
        let control_plane_sha256 = Self::sha256_s3_object(
            &s3_client,
            &s3_source.bucket_name,
            &control_plane.s3_location,
        )
        .await?;

        // Server configuration contains the instance encryption key and other
        // credentials. Legacy manifests wrote it in plaintext. New recovery
        // sets encrypt it with a key derived from the S3 secret already needed
        // to download the backup, so bucket contents alone do not expose it.
        let server_config = yaml_serde::to_string(&self.config_service.get_server_config())
            .map_err(|error| {
                BackupError::Configuration(format!(
                    "Failed to serialize server config for recovery set {}: {}",
                    control_plane.backup_id, error
                ))
            })?;
        let s3_secret = self
            .encryption_service
            .decrypt_string(&s3_source.secret_key)
            .map_err(|error| {
                BackupError::Configuration(format!(
                    "Failed to decrypt S3 secret for recovery set {}: {}",
                    control_plane.backup_id, error
                ))
            })?;
        // Argon2id with a fresh salt per ciphertext (F-009); the CLI still accepts
        // legacy SHA-256-keyed manifests written by older servers.
        let encrypted_server_config =
            temps_core::EncryptionService::encrypt_string_with_password(&s3_secret, &server_config)
                .map_err(|error| BackupError::Internal {
                    message: format!(
                        "Failed to encrypt server config for recovery set {}: {}",
                        control_plane.backup_id, error
                    ),
                })?;

        let mut metadata = json!({
            "recovery_set_version": 2,
            "complete": true,
            "schedule_run_id": schedule_run_id,
            "backup_id": control_plane.backup_id,
            "name": control_plane.name,
            "type": control_plane.backup_type,
            "created_at": control_plane.started_at.to_rfc3339(),
            "created_by": control_plane.created_by,
            "size_bytes": control_plane.size_bytes.unwrap_or(0),
            "compression_type": control_plane.compression_type,
            "source": {
                "id": s3_source.id,
                "name": s3_source.name,
                "bucket": s3_source.bucket_name,
                "path": s3_source.bucket_path,
            },
            "schedule_id": control_plane.schedule_id,
            "state": control_plane.state,
            "tags": serde_json::from_str::<Vec<String>>(&control_plane.tags).unwrap_or_default(),
            "checksum": control_plane.checksum,
            "artifact_sha256": control_plane_sha256,
            "server_config_encrypted": encrypted_server_config,
            "server_config_encryption": "aes-256-gcm+s3-secret-argon2id",
            "external_service_backups": external_service_backups,
            "metadata": control_plane_metadata,
        });
        let authenticated_payload = serde_json::to_string(&metadata)?;
        let manifest_authentication = temps_core::EncryptionService::encrypt_string_with_password(
            &s3_secret,
            &authenticated_payload,
        )
        .map_err(|error| BackupError::Internal {
            message: format!(
                "Failed to authenticate recovery manifest {}: {}",
                control_plane.backup_id, error
            ),
        })?;
        metadata["manifest_authentication"] = json!(manifest_authentication);
        let metadata_key =
            crate::engines::v2_common::derive_metadata_key(&control_plane.s3_location);
        s3_client
            .put_object()
            .bucket(&s3_source.bucket_name)
            .key(&metadata_key)
            .body(serde_json::to_vec(&metadata)?.into())
            .content_type("application/json")
            .send()
            .await
            .map_err(|error| {
                BackupError::S3(crate::engines::v2_common::describe_sdk_error(
                    "put recovery metadata",
                    &error,
                ))
            })?;
        self.update_backup_index(&s3_client, &s3_source, control_plane)
            .await?;

        let mut updated_metadata =
            control_plane_metadata.as_object().cloned().ok_or_else(|| {
                BackupError::Validation(format!(
                    "Control-plane backup {} metadata is not a JSON object",
                    control_plane.id
                ))
            })?;
        updated_metadata.insert(
            "recovery_set_published".to_string(),
            serde_json::Value::Bool(true),
        );
        updated_metadata.insert(
            "recovery_set_version".to_string(),
            serde_json::Value::Number(2.into()),
        );
        let mut active = (*control_plane).clone().into_active_model();
        active.metadata = Set(serde_json::Value::Object(updated_metadata).to_string());
        active.update(self.db.as_ref()).await?;

        Ok(RecoverySetPublication::Published {
            schedule_run_id,
            backup_id: control_plane.backup_id.clone(),
        })
    }

    /// Heal terminal events lost to process restarts or broadcast lag. The
    /// marker on the control-plane row keeps this bounded to unpublished
    /// successful runs; each invocation processes at most 100 candidates.
    pub async fn reconcile_completed_recovery_sets(&self) -> Result<(), BackupError> {
        #[derive(FromQueryResult)]
        struct Candidate {
            id: i32,
        }

        let candidates = Candidate::find_by_statement(Statement::from_string(
            DatabaseBackend::Postgres,
            r#"
SELECT cp.id
  FROM backups cp
 WHERE cp.schedule_run_id IS NOT NULL
   AND cp.state = 'completed'
   AND cp.metadata::jsonb ->> 'engine' = 'control_plane'
   AND COALESCE((cp.metadata::jsonb ->> 'recovery_set_published')::boolean, false) = false
   AND NOT EXISTS (
       SELECT 1
         FROM backups sibling
        WHERE sibling.schedule_run_id = cp.schedule_run_id
          AND sibling.state <> 'completed'
   )
 ORDER BY cp.id DESC
 LIMIT 100
            "#
            .to_string(),
        ))
        .all(self.db.as_ref())
        .await?;

        for candidate in candidates {
            match self.publish_recovery_set_if_complete(candidate.id).await {
                Ok(RecoverySetPublication::Published {
                    schedule_run_id,
                    backup_id,
                }) => info!(
                    schedule_run_id,
                    backup_id, "Published recovered native recovery-set manifest"
                ),
                Ok(_) => {}
                Err(error) => warn!(
                    backup_id = candidate.id,
                    error = %error,
                    "Failed to reconcile native recovery-set manifest"
                ),
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::super::test_support::*;
    use super::*;
    use sea_orm::{DatabaseBackend, MockDatabase};

    #[tokio::test]
    async fn recovery_set_waits_for_every_fan_out_child() {
        let mut control_plane = make_test_backup_model(100);
        control_plane.schedule_id = Some(5);
        control_plane.schedule_run_id = Some(50);
        control_plane.metadata = serde_json::json!({"engine": "control_plane"}).to_string();

        let mut service_backup = make_test_backup_model(101);
        service_backup.schedule_id = Some(5);
        service_backup.schedule_run_id = Some(50);
        service_backup.state = "running".to_string();
        service_backup.metadata =
            serde_json::json!({"engine": "postgres_pgdump", "service_id": 7}).to_string();

        let db = Arc::new(
            MockDatabase::new(DatabaseBackend::Postgres)
                .append_query_results(vec![vec![control_plane.clone()]])
                .append_query_results(vec![vec![control_plane, service_backup]])
                .into_connection(),
        );
        let service = make_scope_test_service(db);

        let outcome = service
            .publish_recovery_set_if_complete(100)
            .await
            .expect("live siblings should produce a typed pending outcome");

        assert_eq!(
            outcome,
            RecoverySetPublication::Pending {
                schedule_run_id: 50
            }
        );
    }

    #[tokio::test]
    async fn recovery_set_refuses_partial_fan_out_run() {
        let mut control_plane = make_test_backup_model(110);
        control_plane.schedule_id = Some(6);
        control_plane.schedule_run_id = Some(60);
        control_plane.metadata = serde_json::json!({"engine": "control_plane"}).to_string();

        let mut failed_service = make_test_backup_model(111);
        failed_service.schedule_id = Some(6);
        failed_service.schedule_run_id = Some(60);
        failed_service.state = "failed".to_string();
        failed_service.error_message = Some("container missing".to_string());
        failed_service.metadata =
            serde_json::json!({"engine": "postgres_pgdump", "service_id": 8}).to_string();

        let db = Arc::new(
            MockDatabase::new(DatabaseBackend::Postgres)
                .append_query_results(vec![vec![failed_service.clone()]])
                .append_query_results(vec![vec![control_plane, failed_service]])
                .into_connection(),
        );
        let service = make_scope_test_service(db);

        let outcome = service
            .publish_recovery_set_if_complete(111)
            .await
            .expect("failed siblings should produce a typed incomplete outcome");

        assert_eq!(
            outcome,
            RecoverySetPublication::Incomplete {
                schedule_run_id: 60,
                failed_backup_ids: vec![111],
            }
        );
    }

    #[tokio::test]
    async fn recovery_set_requires_a_control_plane_backup() {
        let mut service_backup = make_test_backup_model(120);
        service_backup.schedule_id = Some(7);
        service_backup.schedule_run_id = Some(70);
        service_backup.metadata =
            serde_json::json!({"engine": "redis", "service_id": 9}).to_string();

        let db = Arc::new(
            MockDatabase::new(DatabaseBackend::Postgres)
                .append_query_results(vec![vec![service_backup.clone()]])
                .append_query_results(vec![vec![service_backup]])
                .into_connection(),
        );
        let service = make_scope_test_service(db);

        let outcome = service
            .publish_recovery_set_if_complete(120)
            .await
            .expect("service-only schedules are valid but not whole-instance recovery sets");

        assert_eq!(
            outcome,
            RecoverySetPublication::NoControlPlane {
                schedule_run_id: 70
            }
        );
    }

    #[tokio::test]
    async fn recovery_set_requires_exact_expected_service_coverage() {
        let mut control_plane = make_test_backup_model(125);
        control_plane.schedule_id = Some(7);
        control_plane.schedule_run_id = Some(75);
        control_plane.metadata = serde_json::json!({
            "engine": "control_plane",
            "expected_service_ids": [9]
        })
        .to_string();

        let db = Arc::new(
            MockDatabase::new(DatabaseBackend::Postgres)
                .append_query_results(vec![vec![control_plane.clone()]])
                .append_query_results(vec![vec![control_plane]])
                .into_connection(),
        );
        let service = make_scope_test_service(db);

        let error = service
            .publish_recovery_set_if_complete(125)
            .await
            .expect_err("missing expected service backups must prevent publication");

        assert!(
            matches!(error, BackupError::Validation(message) if message.contains("coverage mismatch"))
        );
    }

    #[tokio::test]
    async fn recovery_set_publication_is_idempotent() {
        let mut control_plane = make_test_backup_model(130);
        control_plane.schedule_id = Some(8);
        control_plane.schedule_run_id = Some(80);
        control_plane.metadata = serde_json::json!({
            "engine": "control_plane",
            "recovery_set_published": true,
            "recovery_set_version": 2
        })
        .to_string();

        let db = Arc::new(
            MockDatabase::new(DatabaseBackend::Postgres)
                .append_query_results(vec![vec![control_plane.clone()]])
                .append_query_results(vec![vec![control_plane]])
                .into_connection(),
        );
        let service = make_scope_test_service(db);

        let outcome = service
            .publish_recovery_set_if_complete(130)
            .await
            .expect("already-published recovery sets should be a no-op");

        assert_eq!(
            outcome,
            RecoverySetPublication::AlreadyPublished {
                schedule_run_id: 80,
                backup_id: "uuid-130".to_string(),
            }
        );
    }
}
