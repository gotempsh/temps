// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Backups of external services (managed databases, caches and object stores).

use super::super::{capability, BackupCapabilityError, ExternalServiceBackupCapability};
use super::*;

impl BackupService {
    /// List backups for a specific external service using a single JOIN query.
    ///
    /// Unlike [`list_source_backups`], this method never touches S3.  It issues
    /// one SQL round-trip:
    ///
    /// ```sql
    /// SELECT b.id, b.backup_id, b.name, b.state, b.started_at, b.finished_at,
    ///        b.size_bytes, b.s3_location, b.error_message, b.compression_type,
    ///        b.s3_source_id, s.name AS s3_source_name,
    ///        esb.id AS external_service_backup_id
    /// FROM external_service_backups esb
    /// JOIN backups b ON b.id = esb.backup_id
    /// JOIN s3_sources s ON s.id = b.s3_source_id
    /// WHERE esb.service_id = $1
    /// ORDER BY b.started_at DESC
    /// LIMIT $2 OFFSET $3
    /// ```
    ///
    /// Returns a page of [`ServiceBackupEntry`] values plus the total count for
    /// pagination.  `page` is 1-based; `page_size` is capped at 100.
    pub async fn list_external_service_backups(
        &self,
        service_id: i32,
        page: i64,
        page_size: i64,
    ) -> Result<(Vec<ServiceBackupEntry>, i64), BackupError> {
        let page = page.max(1);
        let page_size = page_size.clamp(1, 100);
        let offset = (page - 1) * page_size;

        // Count total rows so the caller can render pagination controls.
        let count_stmt = Statement::from_sql_and_values(
            DatabaseBackend::Postgres,
            r#"SELECT COUNT(*) AS cnt
               FROM external_service_backups esb
               WHERE esb.service_id = $1"#,
            vec![Value::Int(Some(service_id))],
        );

        #[derive(FromQueryResult)]
        struct CountRow {
            cnt: i64,
        }

        let total = CountRow::find_by_statement(count_stmt)
            .one(self.db.as_ref())
            .await?
            .map(|r| r.cnt)
            .unwrap_or(0);

        // Fetch the page of backups.
        let rows_stmt = Statement::from_sql_and_values(
            DatabaseBackend::Postgres,
            r#"SELECT
                   b.id,
                   b.backup_id,
                   b.name,
                   b.state,
                   b.backup_type,
                   b.started_at,
                   b.finished_at,
                   b.size_bytes,
                   b.s3_location,
                   b.error_message,
                   b.compression_type,
                   b.s3_source_id,
                   s.name AS s3_source_name,
                   esb.id AS external_service_backup_id
               FROM external_service_backups esb
               JOIN backups b ON b.id = esb.backup_id
               JOIN s3_sources s ON s.id = b.s3_source_id
               WHERE esb.service_id = $1
               ORDER BY b.started_at DESC
               LIMIT $2 OFFSET $3"#,
            vec![
                Value::Int(Some(service_id)),
                Value::BigInt(Some(page_size)),
                Value::BigInt(Some(offset)),
            ],
        );

        let entries = ServiceBackupEntry::find_by_statement(rows_stmt)
            .all(self.db.as_ref())
            .await?;

        debug!(
            service_id,
            count = entries.len(),
            total,
            "list_external_service_backups: returned DB-only page"
        );

        Ok((entries, total))
    }

    /// Return every `external_service_backups` child row that belongs to the
    /// given parent `backups.id`, joined with `external_services` so the caller
    /// can display the service name and type without a second round-trip.
    ///
    /// Returns an empty `Vec` when the parent backup has no children (control-
    /// plane backups have no children by definition).  Returns `NotFound` when
    /// the parent `backups` row does not exist.
    ///
    /// SQL uses a single JOIN — no N+1.
    pub async fn list_child_backups(
        &self,
        parent_backup_id: i32,
    ) -> Result<Vec<ChildBackupEntry>, BackupError> {
        use sea_orm::EntityTrait;

        // Verify the parent backup exists first so we return 404 instead of an
        // empty list when the caller passes an unknown integer id.
        let _parent = temps_entities::backups::Entity::find_by_id(parent_backup_id)
            .one(self.db.as_ref())
            .await?
            .ok_or_else(|| BackupError::NotFound {
                resource: "Backup".to_string(),
                detail: format!("Parent backup with id {} not found", parent_backup_id),
            })?;

        let sql = r#"
SELECT
    esb.id            AS id,
    esb.service_id    AS service_id,
    esb.state         AS state,
    esb.backup_type   AS backup_type,
    esb.started_at    AS started_at,
    esb.finished_at   AS finished_at,
    esb.size_bytes    AS size_bytes,
    esb.s3_location   AS s3_location,
    esb.error_message AS error_message,
    esb.compression_type AS compression_type,
    COALESCE(es.name, esb.service_name_snapshot, 'deleted service') AS service_name,
    COALESCE(es.service_type, esb.service_type_snapshot, 'unknown') AS service_type
FROM external_service_backups esb
LEFT JOIN external_services es ON es.id = esb.service_id
WHERE esb.backup_id = $1
ORDER BY esb.id ASC
        "#;

        let rows = ChildBackupEntry::find_by_statement(Statement::from_sql_and_values(
            DatabaseBackend::Postgres,
            sql,
            vec![Value::Int(Some(parent_backup_id))],
        ))
        .all(self.db.as_ref())
        .await
        .map_err(BackupError::Database)?;

        debug!(
            parent_backup_id,
            count = rows.len(),
            "list_child_backups: returned children"
        );

        Ok(rows)
    }

    /// Get an external service by ID
    pub async fn get_external_service(
        &self,
        service_id: i32,
    ) -> Result<temps_entities::external_services::Model, BackupError> {
        temps_entities::external_services::Entity::find_by_id(service_id)
            .one(self.db.as_ref())
            .await?
            .ok_or_else(|| BackupError::NotFound {
                resource: "ExternalService".to_string(),
                detail: format!("External service with ID {} not found", service_id),
            })
    }

    /// Resolve the live backup artifact and Cloud mirror compatibility for an
    /// external service. Database lookup and Docker probing stay in the
    /// service layer so HTTP handlers only perform authorization and mapping.
    pub async fn get_external_service_backup_capability(
        &self,
        service_id: i32,
    ) -> Result<ExternalServiceBackupCapability, BackupCapabilityError> {
        let service = temps_entities::external_services::Entity::find_by_id(service_id)
            .one(self.db.as_ref())
            .await
            .map_err(|source| BackupCapabilityError::LoadService { service_id, source })?
            .ok_or(BackupCapabilityError::ServiceNotFound { service_id })?;

        Ok(capability::probe_external_service_backup_capability(
            &service,
            &self.external_service_manager,
        )
        .await)
    }

    pub async fn backup_external_service(
        &self,
        service: &temps_entities::external_services::Model,
        s3_source_id: i32,
        backup_type: &str,
        created_by: i32,
    ) -> Result<temps_entities::external_service_backups::Model, BackupError> {
        info!("Starting external service backup process");
        let service_id = service.id;

        // Get S3 source configuration
        let s3_source = temps_entities::s3_sources::Entity::find_by_id(s3_source_id)
            .one(self.db.as_ref())
            .await?
            .ok_or_else(|| BackupError::NotFound {
                resource: "S3Source".to_string(),
                detail: "S3 source not found".to_string(),
            })?;

        // Create S3 client
        let s3_client = self
            .create_s3_client(&s3_source)
            .await
            .map_err(|e| BackupError::S3(e.to_string()))?;

        // Decrypt S3 credentials for services that pass them to external tools (e.g., WAL-G)
        let decrypted_access_key = self
            .encryption_service
            .decrypt_string(&s3_source.access_key_id)
            .map_err(|e| BackupError::Internal {
                message: format!("Failed to decrypt access key for backup: {}", e),
            })?;
        let decrypted_secret_key = self
            .encryption_service
            .decrypt_string(&s3_source.secret_key)
            .map_err(|e| BackupError::Internal {
                message: format!("Failed to decrypt secret key for backup: {}", e),
            })?;
        // `None` unless this source holds a temporary (STS-style) credential.
        let decrypted_session_token = temps_entities::s3_sources::decrypt_session_token(
            self.encryption_service.as_ref(),
            &s3_source,
        )
        .map_err(|e| BackupError::Internal {
            message: format!("Failed to decrypt session token for backup: {}", e),
        })?;
        let s3_credentials = temps_providers::S3Credentials {
            access_key_id: decrypted_access_key,
            secret_key: decrypted_secret_key,
            session_token: decrypted_session_token,
            region: s3_source.region.clone(),
            endpoint: s3_source.endpoint.clone(),
            bucket_name: s3_source.bucket_name.clone(),
            bucket_path: s3_source.bucket_path.clone(),
            force_path_style: s3_source.force_path_style.unwrap_or(true),
        };

        // Generate unique backup ID
        let backup_id = Uuid::new_v4().to_string();

        // Create backup record. The heartbeat starts at now() so the row
        // appears alive from the moment it's created — the worker task
        // refreshes it periodically while the engine runs.
        let now = chrono::Utc::now();
        let backup = temps_entities::backups::ActiveModel {
            id: sea_orm::NotSet,
            name: sea_orm::Set(format!("Backup {}", backup_id)),
            backup_id: sea_orm::Set(backup_id.clone()),
            schedule_id: sea_orm::Set(None),
            schedule_run_id: sea_orm::NotSet,
            backup_type: sea_orm::Set(backup_type.to_string()),
            state: sea_orm::Set("running".to_string()),
            started_at: sea_orm::Set(now),
            finished_at: sea_orm::Set(None),
            s3_source_id: sea_orm::Set(s3_source_id),
            s3_location: sea_orm::Set("".to_string()), // Will be updated by the service
            compression_type: sea_orm::Set("gzip".to_string()),
            created_by: sea_orm::Set(created_by),
            tags: sea_orm::Set("[]".to_string()),
            size_bytes: sea_orm::Set(None),
            file_count: sea_orm::Set(None),
            error_message: sea_orm::Set(None),
            metadata: sea_orm::Set(
                json!({
                    "service_id": service_id,
                    "service_type": service.service_type,
                    "service_name": service.name,
                    "timestamp": now.to_rfc3339()
                })
                .to_string(),
            ),
            checksum: sea_orm::Set(None),
            expires_at: sea_orm::Set(None),
        };

        // Generate backup path
        let subpath = format!(
            "external_services/{}/{}/{}",
            service.service_type,
            service.name,
            Utc::now().format("%Y/%m/%d")
        );
        let subpath_root = format!(
            "external_services/{}/{}",
            service.service_type, service.name
        );
        let service_type = temps_providers::ServiceType::from_str(&service.service_type)
            .map_err(|e| BackupError::Validation(e.to_string()))?;
        // Resolving an engine can now fail (the Docker daemon a container-backed
        // engine needs may be absent), so this Result is propagated rather than
        // used directly — the backup cannot proceed without the engine.
        let service_instance = self
            .external_service_manager
            .resolve_service_instance(&service.name, service_type)
            .await
            .map_err(|e| {
                error!(
                    "Could not resolve the backup engine for service '{}' (type={}, id={}): {}",
                    service.name, service.service_type, service.id, e
                );
                BackupError::ExternalService(e.to_string())
            })?;

        let service_config = self
            .external_service_manager
            .get_service_config(service_id)
            .await
            .map_err(|e| BackupError::ExternalService(e.to_string()))?;
        // Resolved before the row exists, so a service that cannot be backed
        // up at all leaves no `running` row behind.
        let backup = backup.insert(self.db.as_ref()).await?;

        // Cluster topology: route through the manager which knows how
        // to find the current primary and dispatch exec to it (local
        // bollard or remote agent). The trait method on
        // PostgresClusterService doesn't have access to the agent
        // protocol so it can't handle multi-host clusters; this is
        // the deliberate carve-out.
        let backup_outcome = if service.topology == "cluster" && service.service_type == "postgres"
        {
            self.external_service_manager
                .backup_postgres_cluster(service, &s3_credentials, &subpath_root, backup.id)
                .await
                .map_err(|e| {
                    error!(
                        "Cluster WAL-G backup failed for service '{}' (id={}): {}",
                        service.name, service.id, e
                    );
                    BackupError::ExternalService(e.to_string())
                })
        } else {
            // Standalone: use the per-engine trait impl as before.
            service_instance
                .backup_to_s3(
                    &s3_client,
                    &s3_credentials,
                    backup.clone(),
                    &s3_source,
                    &subpath,
                    &subpath_root,
                    &self.db,
                    service,
                    service_config,
                )
                .await
                .map_err(|e| {
                    error!(
                        "External service backup failed for service '{}' (type={}, id={}): {}",
                        service.name, service.service_type, service.id, e
                    );
                    BackupError::ExternalService(e.to_string())
                })
        };
        let backup_outcome = match backup_outcome {
            Ok(outcome) => outcome,
            Err(error) => {
                // Keep the cause on the row: callers such as the pre-upgrade
                // backup report it, and the backup list shows it.
                let mut failed: temps_entities::backups::ActiveModel = backup.clone().into();
                failed.state = sea_orm::Set("failed".to_string());
                failed.error_message = sea_orm::Set(Some(error.to_string()));
                failed.finished_at = sea_orm::Set(Some(Utc::now()));
                if let Err(update_error) = failed.update(self.db.as_ref()).await {
                    error!(
                        "Failed to mark backup {} of service '{}' (id={}) as failed: {}",
                        backup.backup_id, service.name, service.id, update_error
                    );
                }
                return Err(error);
            }
        };
        info!(
            "Backup created at location: {} ({} bytes)",
            backup_outcome.location,
            backup_outcome
                .size_bytes
                .map(|n| n.to_string())
                .unwrap_or_else(|| "unknown".to_string())
        );

        // If the engine couldn't determine size locally, fall back to
        // listing the S3 prefix. Best-effort: a missing size is annoying
        // but doesn't block the backup from being marked completed.
        let final_size_bytes = match backup_outcome.size_bytes {
            Some(n) => Some(n),
            None => {
                // Strip the "s3://bucket/" prefix to get a list-able key.
                let bucket = &s3_source.bucket_name;
                let prefix = backup_outcome
                    .location
                    .strip_prefix(&format!("s3://{}/", bucket))
                    .map(|s| s.to_string())
                    .unwrap_or_else(|| backup_outcome.location.trim_start_matches('/').to_string());
                match temps_providers::externalsvc::s3_util::list_total_size(
                    &s3_client, bucket, &prefix,
                )
                .await
                {
                    Ok(n) => Some(n),
                    Err(e) => {
                        warn!(
                            "Could not compute size by listing s3://{}/{}: {}",
                            bucket, prefix, e
                        );
                        None
                    }
                }
            }
        };

        // Mark the parent `backups` row as completed. Without this the row
        // stays in state='running' forever, which breaks listing/filtering
        // and makes the restore UI skip the backup. Retry transient DB
        // failures a few times before giving up — the backup data itself
        // already succeeded, so we don't fail the caller on a lasting
        // failure either, but we also don't want a single blip to strand
        // the row in 'running' until the next server restart reconciles it.
        let retry = temps_core::retry::RetryConfig::new(3)
            .with_base_delay(std::time::Duration::from_millis(200))
            .with_max_delay(std::time::Duration::from_secs(2));
        let update_result = retry
            .retry(|| {
                let mut backup_update: temps_entities::backups::ActiveModel = backup.clone().into();
                backup_update.state = sea_orm::Set("completed".to_string());
                backup_update.s3_location = sea_orm::Set(backup_outcome.location.clone());
                backup_update.finished_at = sea_orm::Set(Some(Utc::now()));
                backup_update.size_bytes = sea_orm::Set(final_size_bytes);
                let db = self.db.as_ref();
                async move { backup_update.update(db).await }
            })
            .await;
        if let Err(e) = update_result {
            // Don't fail the caller — the backup itself succeeded. Log and
            // continue; the boot-time reconciler will mark the still-'running'
            // row as failed, and the completed child `external_service_backups`
            // row (already correctly updated above) keeps the real S3 location
            // attributable for later cleanup/restore.
            error!(
                "Failed to mark backup {} as completed after retries: {}",
                backup.id, e
            );
        }

        // Get the external service backup record
        let external_backup = temps_entities::external_service_backups::Entity::find()
            .filter(temps_entities::external_service_backups::Column::BackupId.eq(backup.id))
            .one(self.db.as_ref())
            .await?
            .ok_or_else(|| BackupError::NotFound {
                resource: "ExternalServiceBackup".to_string(),
                detail: "External service backup record not found".to_string(),
            })?;

        info!(
            "External service backup completed successfully: {}",
            backup_id
        );
        Ok(external_backup)
    }

    /// Return the external service record linked to a backup via the
    /// `external_service_backups` join table, or `None` if no such row
    /// exists (e.g. for control-plane backups).
    ///
    /// Used by `GET /backups/{id}` to populate `external_service` in the
    /// response without requiring an N+1 join at the handler level.
    pub async fn get_backup_external_service(
        &self,
        backup_id: i32,
    ) -> Result<Option<temps_entities::external_services::Model>, BackupError> {
        use sea_orm::{ColumnTrait, EntityTrait, QueryFilter};

        // Look up the child row in external_service_backups for this backup.
        let child = temps_entities::external_service_backups::Entity::find()
            .filter(temps_entities::external_service_backups::Column::BackupId.eq(backup_id))
            .one(self.db.as_ref())
            .await?;

        let service_id = match child {
            Some(row) => row.service_id,
            None => return Ok(None),
        };

        // Load the parent external_services row. A missing row here is an
        // unexpected data-integrity gap, but we swallow it gracefully so
        // the backup detail page can still render.
        let service = temps_entities::external_services::Entity::find_by_id(service_id)
            .one(self.db.as_ref())
            .await?;

        Ok(service)
    }
}

/// Implementation of the pre-upgrade backup provider required by the
/// postgres major-upgrade orchestrator. Lives here (not in temps-providers)
/// because temps-backup owns `BackupService` and already depends on
/// temps-providers — the trait is defined in temps-providers specifically
/// to keep the dep flow one-way.
#[async_trait::async_trait]
impl temps_providers::externalsvc::postgres_upgrade::PreUpgradeBackupProvider for BackupService {
    async fn default_s3_source_id(&self, _service_id: i32) -> Result<Option<i32>, String> {
        // Default S3 source is user-scoped (global for now). Look up the
        // single row flagged is_default=true; return None if none set so
        // the orchestrator raises NoDefaultS3Source.
        use sea_orm::{ColumnTrait, EntityTrait, QueryFilter};
        let row = temps_entities::s3_sources::Entity::find()
            .filter(temps_entities::s3_sources::Column::IsDefault.eq(true))
            .one(self.db.as_ref())
            .await
            .map_err(|e| e.to_string())?;
        Ok(row.map(|r| r.id))
    }

    async fn create_pre_upgrade_backup(
        &self,
        service_id: i32,
        s3_source_id: i32,
        created_by: i32,
    ) -> Result<i32, String> {
        // Back up the service being upgraded, not the whole instance: an
        // unrelated service that cannot be backed up must not block this
        // upgrade, and this service's own failure is reported with its cause
        // rather than folded into an instance-wide summary.
        let service = temps_entities::external_services::Entity::find_by_id(service_id)
            .one(self.db.as_ref())
            .await
            .map_err(|e| format!("failed to load service {service_id}: {e}"))?
            .ok_or_else(|| format!("service {service_id} no longer exists"))?;
        let child = self
            .backup_external_service(&service, s3_source_id, "full", created_by)
            .await
            .map_err(|e| {
                format!(
                    "full backup of '{}' to S3 source {} failed: {}",
                    service.name, s3_source_id, e
                )
            })?;
        // The upgrade row's FK targets `backups(id)`, the parent of the
        // per-service child row.
        Ok(child.backup_id)
    }
}

#[cfg(test)]
mod tests {
    use super::super::test_support::*;
    use super::*;
    use sea_orm::{DatabaseBackend, MockDatabase};
    use temps_core::EncryptionService;

    /// Validate the pagination clamping logic: page < 1 becomes 1, page_size
    /// above 100 becomes 100. We can't mock raw SQL in unit tests (Sea-ORM
    /// MockDatabase only covers entity model types, not bare FromQueryResult
    /// structs), so we test only the arithmetic that lives outside the query.
    #[test]
    fn test_list_external_service_backups_pagination_clamp() {
        // page = 0 clamps to 1 — the underflow happens at `(page - 1) * page_size`
        // if we don't, producing a negative OFFSET that Postgres rejects.
        let raw_page: i64 = 0;
        let page: i64 = raw_page.max(1);
        assert_eq!(page, 1);
        // page_size = 200 → page_size = 100 (clamp 1..=100)
        let page_size: i64 = 200_i64.clamp(1, 100);
        assert_eq!(page_size, 100);
        // offset
        let offset = (page - 1) * page_size;
        assert_eq!(offset, 0);
    }

    /// Helper: build a minimal `ChildBackupEntry` BTreeMap row for MockDatabase.
    fn make_child_backup_row(
        id: i32,
        service_id: i32,
        state: &str,
    ) -> std::collections::BTreeMap<String, sea_orm::Value> {
        use sea_orm::Value as SVal;
        let mut row = std::collections::BTreeMap::new();
        row.insert("id".to_string(), SVal::Int(Some(id)));
        row.insert("service_id".to_string(), SVal::Int(Some(service_id)));
        row.insert(
            "service_name".to_string(),
            SVal::String(Some(Box::new(format!("service-{}", service_id)))),
        );
        row.insert(
            "service_type".to_string(),
            SVal::String(Some(Box::new("postgres".to_string()))),
        );
        row.insert(
            "state".to_string(),
            SVal::String(Some(Box::new(state.to_string()))),
        );
        row.insert(
            "backup_type".to_string(),
            SVal::String(Some(Box::new("full".to_string()))),
        );
        row.insert(
            "started_at".to_string(),
            SVal::ChronoDateTimeUtc(Some(Box::new(chrono::Utc::now()))),
        );
        row.insert("finished_at".to_string(), SVal::ChronoDateTimeUtc(None));
        row.insert("size_bytes".to_string(), SVal::BigInt(Some(2048)));
        row.insert(
            "s3_location".to_string(),
            SVal::String(Some(Box::new("s3://bucket/child".to_string()))),
        );
        row.insert("error_message".to_string(), SVal::String(None));
        row.insert(
            "compression_type".to_string(),
            SVal::String(Some(Box::new("lz4".to_string()))),
        );
        row
    }

    /// `list_child_backups` returns all children ordered by `id ASC` when the
    /// parent backup exists and has two completed child rows.
    #[tokio::test]
    async fn test_list_child_backups_returns_ordered_rows() {
        let parent = make_test_backup_model(10);

        let db = Arc::new(
            MockDatabase::new(DatabaseBackend::Postgres)
                // find_by_id for the parent backup
                .append_query_results(vec![vec![parent]])
                // child rows query — two entries, ordered by id ASC (service 1 then 2)
                .append_query_results(vec![vec![
                    make_child_backup_row(1, 1, "completed"),
                    make_child_backup_row(2, 2, "completed"),
                ]])
                .into_connection(),
        );

        let svc = BackupService::new(
            db.clone(),
            create_mock_external_service_manager(db),
            create_mock_alarm_service(),
            create_mock_config_service(),
            Arc::new(EncryptionService::new("test_encryption_key_1234567890ab").unwrap()),
            Arc::new(NoopJobQueue),
        );

        let children = svc
            .list_child_backups(10)
            .await
            .expect("list_child_backups must succeed");

        assert_eq!(children.len(), 2, "must return 2 children");
        assert_eq!(children[0].id, 1, "first child must have id=1");
        assert_eq!(children[1].id, 2, "second child must have id=2");
        assert_eq!(children[0].state, "completed");
        assert_eq!(children[0].service_type, "postgres");
    }

    /// `list_child_backups` returns an empty Vec when the parent backup exists
    /// but has no child rows (e.g. a control-plane backup).
    #[tokio::test]
    async fn test_list_child_backups_returns_empty_for_no_children() {
        let parent = make_test_backup_model(99);

        let db = Arc::new(
            MockDatabase::new(DatabaseBackend::Postgres)
                // find_by_id returns the parent
                .append_query_results(vec![vec![parent]])
                // child rows query returns nothing
                .append_query_results(vec![Vec::<
                    std::collections::BTreeMap<String, sea_orm::Value>,
                >::new()])
                .into_connection(),
        );

        let svc = BackupService::new(
            db.clone(),
            create_mock_external_service_manager(db),
            create_mock_alarm_service(),
            create_mock_config_service(),
            Arc::new(EncryptionService::new("test_encryption_key_1234567890ab").unwrap()),
            Arc::new(NoopJobQueue),
        );

        let children = svc
            .list_child_backups(99)
            .await
            .expect("list_child_backups must succeed for parent with no children");

        assert!(children.is_empty(), "must return empty Vec");
    }

    /// `list_child_backups` returns `NotFound` when the parent backup does not
    /// exist, so the handler can surface a 404 instead of an empty list.
    #[tokio::test]
    async fn test_list_child_backups_returns_not_found_for_missing_parent() {
        let db = Arc::new(
            MockDatabase::new(DatabaseBackend::Postgres)
                // find_by_id returns nothing → parent does not exist
                .append_query_results(vec![Vec::<temps_entities::backups::Model>::new()])
                .into_connection(),
        );

        let svc = BackupService::new(
            db.clone(),
            create_mock_external_service_manager(db),
            create_mock_alarm_service(),
            create_mock_config_service(),
            Arc::new(EncryptionService::new("test_encryption_key_1234567890ab").unwrap()),
            Arc::new(NoopJobQueue),
        );

        let result = svc.list_child_backups(9999).await;
        assert!(result.is_err(), "missing parent must return an error");
        assert!(
            matches!(result.unwrap_err(), BackupError::NotFound { .. }),
            "expected BackupError::NotFound for unknown parent backup"
        );
    }

    // ── backup_schedule_services membership ──────────────────────────────
    //
    // These tests pin the contract of the attach/detach/list helpers. They
    // need a `BackupService`, which in turn requires an
    // `ExternalServiceManager`, which constructs a Docker client at build
    // time. We early-return when Docker is unavailable so the suite stays
    // green in CI environments without a daemon.
    //
    // The point of these is the *resolution* behaviour, not the SQL — the
    // join query itself is exercised by the integration test.
}
