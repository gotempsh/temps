// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! S3 sources, CRUD, connection tests, client construction and bucket
//! verification.

use super::*;

impl BackupService {
    pub(super) async fn create_s3_client(&self, s3_source: &S3Source) -> Result<S3Client> {
        // Decrypt credentials before using them
        let decrypted_access_key = self
            .encryption_service
            .decrypt_string(&s3_source.access_key_id)
            .map_err(|e| anyhow::anyhow!("Failed to decrypt access key: {}", e))?;

        let decrypted_secret_key = self
            .encryption_service
            .decrypt_string(&s3_source.secret_key)
            .map_err(|e| anyhow::anyhow!("Failed to decrypt secret key: {}", e))?;

        // `None` for a long-lived credential — the third argument stays exactly
        // what it was for every operator-configured source. `Some` only for a
        // temporary one, which SigV4 rejects without its session token.
        let decrypted_session_token = temps_entities::s3_sources::decrypt_session_token(
            self.encryption_service.as_ref(),
            s3_source,
        )
        .map_err(|e| anyhow::anyhow!("Failed to decrypt session token: {}", e))?;

        let creds = aws_sdk_s3::config::Credentials::new(
            decrypted_access_key,
            decrypted_secret_key,
            decrypted_session_token,
            None,
            "backup-service",
        );

        // Default to path style for MinIO and other S3-compatible stores.
        let config = crate::engines::v2_common::s3_config_builder(
            &s3_source.region,
            s3_source.force_path_style.unwrap_or(true),
            creds,
            s3_source.endpoint.as_deref(),
        )
        .build();

        Ok(S3Client::from_conf(config))
    }

    /// Create S3 client from request (before persistence)
    async fn create_s3_client_from_request(
        &self,
        request: &CreateS3SourceRequest,
    ) -> Result<S3Client, BackupError> {
        let creds = aws_sdk_s3::config::Credentials::new(
            request.access_key_id.expose().to_string(),
            request.secret_key.expose().to_string(),
            None,
            None,
            "backup-service",
        );

        let config = crate::engines::v2_common::s3_config_builder(
            &request.region,
            request.force_path_style.unwrap_or(true),
            creds,
            request.endpoint.as_deref(),
        )
        // This client only verifies a destination while an operator
        // waits; bound it so an endpoint that never answers cannot hold
        // the request open.
        .timeout_config(
            aws_sdk_s3::config::timeout::TimeoutConfig::builder()
                .operation_timeout(crate::services::s3_probe::S3_PROBE_OPERATION_TIMEOUT)
                .build(),
        )
        .build();
        Ok(S3Client::from_conf(config))
    }

    /// Verify that the destination described by `request` is reachable and
    /// accepts its credentials, classifying the failure when it is not.
    async fn verify_s3_destination(
        &self,
        request: &CreateS3SourceRequest,
        on_missing: crate::services::s3_probe::MissingBucket,
    ) -> Result<(), BackupError> {
        let client = self.create_s3_client_from_request(request).await?;
        verify_s3_bucket(
            &client,
            request.endpoint.as_deref(),
            &request.region,
            &request.bucket_name,
            on_missing,
        )
        .await
    }

    /// List all S3 sources
    pub async fn list_s3_sources(
        &self,
    ) -> Result<Vec<temps_entities::s3_sources::Model>, BackupError> {
        let sources = temps_entities::s3_sources::Entity::find()
            .all(self.db.as_ref())
            .await?;

        debug!("Listed {} S3 sources", sources.len());
        Ok(sources)
    }

    /// Create a new S3 source
    pub async fn create_s3_source(
        &self,
        request: CreateS3SourceRequest,
    ) -> Result<temps_entities::s3_sources::Model, BackupError> {
        // Validate the request
        if request.name.is_empty() {
            return Err(BackupError::Validation(
                "S3 source name cannot be empty".into(),
            ));
        }

        if let Some(service_id) = request.backing_service_id {
            let backing_service = temps_entities::external_services::Entity::find_by_id(service_id)
                .one(self.db.as_ref())
                .await?
                .ok_or_else(|| {
                    BackupError::Validation(format!(
                        "Backing service {} does not exist",
                        service_id
                    ))
                })?;
            if !matches!(backing_service.service_type.as_str(), "rustfs" | "s3") {
                return Err(BackupError::Validation(format!(
                    "Service {} has type '{}' and cannot back an S3 destination",
                    service_id, backing_service.service_type
                )));
            }
        }

        // Test S3 connection and auto-create bucket before persisting
        self.verify_s3_destination(&request, crate::services::s3_probe::MissingBucket::Create)
            .await?;

        // First source is automatically default; subsequent sources require an explicit
        // set-default call. An explicit `is_default: true` in the request is honored and
        // will swap default atomically.
        let existing_count = temps_entities::s3_sources::Entity::find()
            .count(self.db.as_ref())
            .await?;
        let explicit_default = request.is_default.unwrap_or(false);
        let should_be_default = existing_count == 0 || explicit_default;

        let txn = self.db.begin().await?;

        if should_be_default && existing_count > 0 {
            // Clear existing default before inserting new default
            temps_entities::s3_sources::Entity::update_many()
                .col_expr(
                    temps_entities::s3_sources::Column::IsDefault,
                    sea_orm::sea_query::Expr::value(false),
                )
                .filter(temps_entities::s3_sources::Column::IsDefault.eq(true))
                .exec(&txn)
                .await?;
        }

        // Encrypt sensitive credentials and insert — shared with
        // `temps-cloud`'s Cloud-managed backup credential provisioning so the
        // encryption call site and the persisted row shape never drift.
        let source = temps_entities::s3_sources::insert_encrypted(
            &txn,
            &self.encryption_service,
            temps_entities::s3_sources::S3SourceCredentials {
                name: request.name.clone(),
                bucket_name: request.bucket_name,
                bucket_path: request.bucket_path,
                access_key_id: request.access_key_id.into_exposed(),
                secret_key: request.secret_key.into_exposed(),
                // An operator typing credentials into the S3 Sources form is
                // always configuring a long-lived credential. Temporary,
                // prefix-scoped credentials only ever arrive from Temps Cloud
                // via `CloudService::provision_managed_backup_source`, so this
                // path stores NULL for both and behaves exactly as before.
                session_token: None,
                credentials_expire_at: None,
                region: request.region,
                endpoint: request.endpoint,
                force_path_style: request.force_path_style,
            },
            should_be_default,
            false,
            request.backing_service_id,
        )
        .await
        .map_err(|error| BackupError::Internal {
            message: format!("Failed to create S3 source '{}': {}", request.name, error),
        })?;
        txn.commit().await?;

        debug!(
            "Created new S3 source: {} (is_default={})",
            source.name, source.is_default
        );
        Ok(source)
    }

    /// Test an S3 connection using stored (encrypted) credentials for an existing source.
    /// Returns `Ok(())` on success, or `BackupError::S3Unreachable` naming the
    /// endpoint and the classified cause on failure.
    pub async fn test_s3_source_connection(&self, id: i32) -> Result<(), BackupError> {
        let source = self.get_s3_source(id).await?;
        // The source's own client, so a temporary credential keeps its
        // session token, bounded like every other reachability check.
        let client = self
            .create_s3_client(&source)
            .await
            .map_err(|e| BackupError::Internal {
                message: format!("Failed to build S3 client for source {}: {}", id, e),
            })?;
        let client = S3Client::from_conf(
            client
                .config()
                .to_builder()
                .timeout_config(
                    aws_sdk_s3::config::timeout::TimeoutConfig::builder()
                        .operation_timeout(crate::services::s3_probe::S3_PROBE_OPERATION_TIMEOUT)
                        .build(),
                )
                .build(),
        );
        verify_s3_bucket(
            &client,
            source.endpoint.as_deref(),
            &source.region,
            &source.bucket_name,
            crate::services::s3_probe::MissingBucket::Fail,
        )
        .await?;
        debug!(
            "S3 connection test succeeded for source {} (bucket {})",
            id, source.bucket_name
        );
        Ok(())
    }

    /// Test an S3 connection using credentials from a prospective request (before persistence).
    /// Does NOT create the bucket — only attempts a list to verify access.
    pub async fn test_s3_connection_from_request(
        &self,
        request: &CreateS3SourceRequest,
    ) -> Result<(), BackupError> {
        if request.access_key_id.is_empty() || request.secret_key.is_empty() {
            return Err(BackupError::Validation(
                "Access key and secret key are required to test connection".into(),
            ));
        }

        // A missing bucket is not a failure: the credentials work, and
        // creating the source creates the bucket.
        self.verify_s3_destination(request, crate::services::s3_probe::MissingBucket::Accept)
            .await
    }

    /// Atomically make the given source the default. All other sources will be set to
    /// is_default=false in the same transaction.
    pub async fn set_default_s3_source(
        &self,
        id: i32,
    ) -> Result<temps_entities::s3_sources::Model, BackupError> {
        // Verify target exists
        self.get_s3_source(id).await?;

        let txn = self.db.begin().await?;

        temps_entities::s3_sources::Entity::update_many()
            .col_expr(
                temps_entities::s3_sources::Column::IsDefault,
                sea_orm::sea_query::Expr::value(false),
            )
            .filter(temps_entities::s3_sources::Column::IsDefault.eq(true))
            .filter(temps_entities::s3_sources::Column::Id.ne(id))
            .exec(&txn)
            .await?;

        temps_entities::s3_sources::Entity::update_many()
            .col_expr(
                temps_entities::s3_sources::Column::IsDefault,
                sea_orm::sea_query::Expr::value(true),
            )
            .col_expr(
                temps_entities::s3_sources::Column::UpdatedAt,
                sea_orm::sea_query::Expr::value(Utc::now()),
            )
            .filter(temps_entities::s3_sources::Column::Id.eq(id))
            .exec(&txn)
            .await?;

        txn.commit().await?;

        let updated = self.get_s3_source(id).await?;
        info!("S3 source {} is now the default", updated.name);
        Ok(updated)
    }

    /// Return the currently-default S3 source, if any.
    pub async fn get_default_s3_source(
        &self,
    ) -> Result<Option<temps_entities::s3_sources::Model>, BackupError> {
        let source = temps_entities::s3_sources::Entity::find()
            .filter(temps_entities::s3_sources::Column::IsDefault.eq(true))
            .one(self.db.as_ref())
            .await?;
        Ok(source)
    }

    /// Get an S3 source by ID
    pub async fn get_s3_source(
        &self,
        id: i32,
    ) -> Result<temps_entities::s3_sources::Model, BackupError> {
        let source = temps_entities::s3_sources::Entity::find_by_id(id)
            .one(self.db.as_ref())
            .await?
            .ok_or_else(|| BackupError::NotFound {
                resource: "S3Source".to_string(),
                detail: "S3 source not found".to_string(),
            })?;

        Ok(source)
    }

    /// Resolve every managed service that can be identified as the provider
    /// of an S3 destination. New sources carry an explicit FK; legacy sources
    /// are matched conservatively by their encrypted access/secret pair so an
    /// upgrade cannot reintroduce recursive self-backups.
    pub(super) async fn source_backing_service_ids(
        &self,
        source_id: i32,
    ) -> Result<HashSet<i32>, BackupError> {
        let source = self.get_s3_source(source_id).await?;
        if let Some(service_id) = source.backing_service_id {
            return Ok(HashSet::from([service_id]));
        }

        let candidates = temps_entities::external_services::Entity::find()
            .filter(temps_entities::external_services::Column::ServiceType.is_in(["rustfs", "s3"]))
            .all(self.db.as_ref())
            .await?;
        if candidates.is_empty() {
            return Ok(HashSet::new());
        }

        let access_key = self
            .encryption_service
            .decrypt_string(&source.access_key_id)
            .map_err(|error| BackupError::Internal {
                message: format!(
                    "Failed to resolve backing service for S3 source {}: access key could not be decrypted: {}",
                    source_id, error
                ),
            })?;
        let secret_key = self
            .encryption_service
            .decrypt_string(&source.secret_key)
            .map_err(|error| BackupError::Internal {
                message: format!(
                    "Failed to resolve backing service for S3 source {}: secret key could not be decrypted: {}",
                    source_id, error
                ),
            })?;

        let mut service_ids = HashSet::new();
        for candidate in candidates {
            let config = self
                .external_service_manager
                .get_service_config(candidate.id)
                .await
                .map_err(|error| BackupError::Internal {
                    message: format!(
                        "Failed to inspect managed storage service {} for S3 source {}: {}",
                        candidate.id, source_id, error
                    ),
                })?;
            let candidate_access = config
                .parameters
                .get("access_key")
                .and_then(serde_json::Value::as_str);
            let candidate_secret = config
                .parameters
                .get("secret_key")
                .and_then(serde_json::Value::as_str);
            if candidate_access == Some(access_key.as_str())
                && candidate_secret == Some(secret_key.as_str())
            {
                service_ids.insert(candidate.id);
            }
        }

        Ok(service_ids)
    }

    /// Delete an S3 source
    pub async fn delete_s3_source(&self, id: i32) -> Result<bool, BackupError> {
        // First check if source exists and is not in use
        let source = self.get_s3_source(id).await?;

        // Refuse to delete a Cloud-managed source. It was not created by an
        // operator and cannot be recreated by one; only Temps Cloud's own
        // disconnect cleanup (which does not go through this method) may
        // remove it.
        if source.managed_by_cloud {
            return Err(BackupError::Validation(format!(
                "S3 source '{}' is managed by Temps Cloud and cannot be deleted manually. \
                 Disconnect Temps Cloud to remove it.",
                source.name
            )));
        }

        // Refuse to delete the default source while other sources exist. The caller
        // should set a different source as default first.
        if source.is_default {
            let other_count = temps_entities::s3_sources::Entity::find()
                .filter(temps_entities::s3_sources::Column::Id.ne(id))
                .count(self.db.as_ref())
                .await?;
            if other_count > 0 {
                return Err(BackupError::Validation(format!(
                    "S3 source '{}' is the default. Set a different source as default before deleting.",
                    source.name
                )));
            }
        }

        // Refuse to delete if any backup schedule still references this source.
        let schedule_count = temps_entities::backup_schedules::Entity::find()
            .filter(temps_entities::backup_schedules::Column::S3SourceId.eq(id))
            .count(self.db.as_ref())
            .await?;
        if schedule_count > 0 {
            return Err(BackupError::Validation(format!(
                "Cannot delete S3 source '{}': still referenced by {} backup schedule(s)",
                source.name, schedule_count
            )));
        }

        // Completed and failed backup records are retained as recovery evidence.
        // Deleting their source would cascade into `backups`, which is deliberately
        // prevented once a restore run references a backup. Check the direct
        // dependency up front so callers receive a stable validation error instead
        // of leaking a database foreign-key violation as HTTP 500.
        let backup_count = temps_entities::backups::Entity::find()
            .filter(temps_entities::backups::Column::S3SourceId.eq(id))
            .count(self.db.as_ref())
            .await?;
        if backup_count > 0 {
            return Err(BackupError::Validation(format!(
                "Cannot delete S3 source '{}': still referenced by {} backup record(s)",
                source.name, backup_count
            )));
        }

        let result = temps_entities::s3_sources::Entity::delete_by_id(id)
            .exec(self.db.as_ref())
            .await?;

        debug!("Deleted S3 source: {}", source.name);
        Ok(result.rows_affected > 0)
    }

    /// Resolve an optional `s3_source_id` into a concrete ID. If `Some`, returns it
    /// as-is (caller still validates existence). If `None`, returns the current default
    /// source. Returns `Validation` if no default has been configured.
    pub async fn resolve_s3_source_id(&self, requested: Option<i32>) -> Result<i32, BackupError> {
        if let Some(id) = requested {
            return Ok(id);
        }

        match self.get_default_s3_source().await? {
            Some(source) => Ok(source.id),
            None => Err(BackupError::Validation(
                "No S3 source specified and no default S3 source is configured. \
                 Create an S3 source or mark one as default first."
                    .to_string(),
            )),
        }
    }

    /// Update an S3 source
    pub async fn update_s3_source(
        &self,
        id: i32,
        request: crate::handlers::backup_handler::UpdateS3SourceRequest,
    ) -> Result<S3Source, BackupError> {
        use sea_orm::{ActiveModelTrait, EntityTrait, Set};

        let current = temps_entities::s3_sources::Entity::find_by_id(id)
            .one(self.db.as_ref())
            .await?
            .ok_or_else(|| BackupError::NotFound {
                resource: "S3Source".to_string(),
                detail: "S3 source not found".to_string(),
            })?;

        // Refuse to edit a Cloud-managed source. Its credentials are rotated
        // by Temps Cloud's own provisioning path, not by an operator editing
        // this row by hand.
        if current.managed_by_cloud {
            return Err(BackupError::Validation(format!(
                "S3 source '{}' is managed by Temps Cloud and cannot be edited manually.",
                current.name
            )));
        }

        // Responses mask stored credentials as `***`, and an edit form sends
        // that value back for a field the operator did not retype. It means
        // "unchanged": storing it would replace a working credential.
        let is_unchanged_credential =
            |value: Option<&str>| value.is_none_or(|v| v.is_empty() || v == "***");
        let mut request = request;
        if is_unchanged_credential(request.access_key_id.as_ref().map(SecretString::expose)) {
            request.access_key_id = None;
        }
        if is_unchanged_credential(request.secret_key.as_ref().map(SecretString::expose)) {
            request.secret_key = None;
        }

        // A change to where or how the destination is reached is verified
        // exactly like a new destination, before anything is saved. A field
        // sent back with its stored value (an edit form resubmitting the
        // whole record for a rename, say) is not a change, so a temporarily
        // unreachable destination never blocks an unrelated edit.
        let decrypt = |field: &str, value: &str| {
            self.encryption_service
                .decrypt_string(value)
                .map_err(|error| BackupError::Internal {
                    message: format!(
                        "Failed to decrypt the {} of S3 source {} ('{}'): {}",
                        field, current.id, current.name, error
                    ),
                })
        };
        let differs = |supplied: &Option<String>, stored: &str| {
            supplied.as_deref().is_some_and(|value| value != stored)
        };
        // A stored credential is only decrypted when a replacement was
        // supplied, to compare them. One that cannot be decrypted any more
        // (a changed server key, damaged data) is being replaced, which is
        // exactly how an operator repairs it, so it counts as a change.
        let credential_changed = |supplied: Option<&str>, stored: &str| {
            supplied.is_some_and(
                |value| match self.encryption_service.decrypt_string(stored) {
                    Ok(stored) => value != stored,
                    Err(_) => true,
                },
            )
        };
        let connection_changed = differs(&request.bucket_name, &current.bucket_name)
            || credential_changed(
                request.access_key_id.as_ref().map(SecretString::expose),
                &current.access_key_id,
            )
            || credential_changed(
                request.secret_key.as_ref().map(SecretString::expose),
                &current.secret_key,
            )
            || differs(&request.region, &current.region)
            || request
                .endpoint
                .as_deref()
                .is_some_and(|endpoint| current.endpoint.as_deref() != Some(endpoint))
            || request
                .force_path_style
                .is_some_and(|path_style| current.force_path_style != Some(path_style));
        if connection_changed {
            let probe = CreateS3SourceRequest {
                name: current.name.clone(),
                bucket_name: request
                    .bucket_name
                    .clone()
                    .unwrap_or_else(|| current.bucket_name.clone()),
                bucket_path: current.bucket_path.clone(),
                access_key_id: match &request.access_key_id {
                    Some(access_key_id) => access_key_id.clone(),
                    None => SecretString::new(decrypt("access key ID", &current.access_key_id)?),
                },
                secret_key: match &request.secret_key {
                    Some(secret_key) => secret_key.clone(),
                    None => SecretString::new(decrypt("secret key", &current.secret_key)?),
                },
                region: request
                    .region
                    .clone()
                    .unwrap_or_else(|| current.region.clone()),
                endpoint: request
                    .endpoint
                    .clone()
                    .or_else(|| current.endpoint.clone()),
                force_path_style: request.force_path_style.or(current.force_path_style),
                is_default: None,
                backing_service_id: current.backing_service_id,
            };
            self.verify_s3_destination(&probe, crate::services::s3_probe::MissingBucket::Create)
                .await?;
        }

        let mut active = current.into_active_model();

        if let Some(name) = request.name {
            active.name = Set(name);
        }
        if let Some(bucket_name) = request.bucket_name {
            active.bucket_name = Set(bucket_name);
        }
        if let Some(bucket_path) = request.bucket_path {
            active.bucket_path = Set(bucket_path);
        }
        if let Some(access_key_id) = request.access_key_id {
            // Encrypt access key before storing
            let encrypted_access_key = self
                .encryption_service
                .encrypt_string(access_key_id.expose())
                .map_err(|e| BackupError::Internal {
                    message: format!("Failed to encrypt access key: {}", e),
                })?;
            active.access_key_id = Set(encrypted_access_key);
        }
        if let Some(secret_key) = request.secret_key {
            // Encrypt secret key before storing
            let encrypted_secret_key = self
                .encryption_service
                .encrypt_string(secret_key.expose())
                .map_err(|e| BackupError::Internal {
                    message: format!("Failed to encrypt secret key: {}", e),
                })?;
            active.secret_key = Set(encrypted_secret_key);
        }
        if let Some(region) = request.region {
            active.region = Set(region);
        }
        if let Some(endpoint) = request.endpoint {
            active.endpoint = Set(Some(endpoint));
        }
        if let Some(force_path_style) = request.force_path_style {
            active.force_path_style = Set(Some(force_path_style));
        }

        active.updated_at = Set(chrono::Utc::now());

        let updated = active.update(self.db.as_ref()).await?;
        Ok(updated)
    }
}

/// Verify `bucket` through `client`, turning a failure into
/// [`BackupError::S3Unreachable`] that names the endpoint and the cause.
async fn verify_s3_bucket(
    client: &S3Client,
    endpoint: Option<&str>,
    region: &str,
    bucket: &str,
    on_missing: crate::services::s3_probe::MissingBucket,
) -> Result<(), BackupError> {
    crate::services::s3_probe::check_bucket(client, bucket, on_missing)
        .await
        .map_err(|failure| {
            let endpoint = crate::services::s3_probe::display_endpoint(endpoint, region);
            warn!(
                endpoint = %endpoint,
                bucket = %bucket,
                failure = failure.kind.slug(),
                cause = %failure.cause,
                "S3 destination could not be verified"
            );
            BackupError::S3Unreachable {
                endpoint,
                bucket: bucket.to_string(),
                kind: failure.kind,
                cause: failure.cause,
            }
        })
}

#[cfg(test)]
mod tests;
