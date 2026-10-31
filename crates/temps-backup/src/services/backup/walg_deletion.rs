// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Deleting WAL-G backups, target validation, repository identity and the
//! helper container that runs `wal-g delete`.

use super::*;

/// Owns one uniquely named, labelled cleanup helper before creation starts.
/// The create task retains this guard even if its caller is cancelled.
struct OrphanWalgHelper {
    docker: bollard::Docker,
    name: String,
    backup_id: String,
    id: Option<String>,
    armed: bool,
}

impl Drop for OrphanWalgHelper {
    fn drop(&mut self) {
        if !self.armed {
            return;
        }
        let docker = self.docker.clone();
        let name = self.name.clone();
        let backup_id = self.backup_id.clone();
        let known_id = self.id.take();
        if let Ok(runtime) = tokio::runtime::Handle::try_current() {
            runtime.spawn(async move {
                let cleanup = async {
                    let id = match known_id {
                        Some(id) => id,
                        None => {
                            let container = match docker.inspect_container(&name, None::<bollard::query_parameters::InspectContainerOptions>).await {
                                Ok(container) => container,
                                Err(bollard::errors::Error::DockerResponseServerError { status_code: 404, .. }) => return Ok(()),
                                Err(error) => return Err(error),
                            };
                            // A name lookup after an ambiguous create failure
                            // may remove only this request's labelled helper.
                            let owned = container.config.as_ref()
                                .and_then(|config| config.labels.as_ref())
                                .and_then(|labels| labels.get("temps.backup_cleanup_id"))
                                == Some(&backup_id);
                            if !owned {
                                warn!(helper_name = %name, "Refusing to remove an unowned WAL-G helper name");
                                return Ok(());
                            }
                            let Some(id) = container.id else { return Ok(()) };
                            id
                        }
                    };
                    docker.remove_container(&id, Some(bollard::query_parameters::RemoveContainerOptions {
                        force: true, v: true, ..Default::default()
                    })).await
                };
                match time::timeout(std::time::Duration::from_secs(10), cleanup).await {
                    Ok(Ok(())) | Ok(Err(bollard::errors::Error::DockerResponseServerError { status_code: 404, .. })) => {}
                    Ok(Err(error)) => warn!(helper_name = %name, %error, "Orphan WAL-G helper removal failed; inspect this labelled helper before retrying cleanup"),
                    Err(error) => warn!(helper_name = %name, %error, "Orphan WAL-G helper removal timed out; inspect this labelled helper before retrying cleanup"),
                }
            });
        }
    }
}

async fn create_owned_orphan_helper(
    docker: bollard::Docker,
    name: String,
    backup_id: String,
    config: bollard::models::ContainerCreateBody,
) -> Result<(String, OrphanWalgHelper), BackupError> {
    match docker
        .inspect_container(
            &name,
            None::<bollard::query_parameters::InspectContainerOptions>,
        )
        .await
    {
        Ok(_) => {
            return Err(BackupError::Validation(format!(
                "Refusing to reuse existing orphan cleanup helper '{name}'"
            )))
        }
        Err(bollard::errors::Error::DockerResponseServerError {
            status_code: 404, ..
        }) => {}
        Err(error) => {
            return Err(BackupError::ExternalService(format!(
                "Failed to check new orphan cleanup helper '{name}': {error}"
            )))
        }
    }
    // Do not cancel an in-flight Docker create merely because the enclosing
    // deletion deadline expires. Docker may already have committed creation.
    // When this task finishes after its receiver disappeared, its owned guard
    // is dropped and removes the helper even if it was never started.
    tokio::spawn(async move {
        let mut owner = OrphanWalgHelper {
            docker: docker.clone(),
            name: name.clone(),
            backup_id,
            id: None,
            armed: true,
        };
        let created = docker
            .create_container(
                Some(bollard::query_parameters::CreateContainerOptions {
                    name: Some(name.clone()),
                    ..Default::default()
                }),
                config,
            )
            .await
            .map_err(|error| {
                if matches!(
                    error,
                    bollard::errors::Error::DockerResponseServerError {
                        status_code: 409,
                        ..
                    }
                ) {
                    owner.armed = false;
                }
                BackupError::ExternalService(format!(
                    "Failed to create orphan cleanup helper '{name}': {error}"
                ))
            })?;
        owner.id = Some(created.id.clone());
        Ok::<_, BackupError>((created.id, owner))
    })
    .await
    .map_err(|error| {
        BackupError::ExternalService(format!(
            "Orphan cleanup helper creation task failed: {error}"
        ))
    })?
}

/// Backup provenance is retained independently of the live service. Require
/// agreement across every retained child before choosing an orphan repository.
fn retained_walg_service_identity(
    children: &[temps_entities::external_service_backups::Model],
    service_id: i32,
    backup_id: &str,
) -> Result<(String, String), BackupError> {
    let mut identity = None;
    for child in children {
        if child.service_id != service_id {
            return Err(BackupError::Validation(format!("Backup {backup_id} has inconsistent retained service identity; refusing orphan deletion")));
        }
        let candidate = child.service_name_snapshot.as_deref()
            .filter(|name| !name.is_empty())
            .zip(child.service_type_snapshot.as_deref().filter(|kind| !kind.is_empty()))
            .ok_or_else(|| BackupError::Validation(format!("Backup {backup_id} has no retained service provenance. Its repository cannot be safely deleted after service {service_id} was removed")))?;
        if identity.is_some_and(|previous| previous != candidate) {
            return Err(BackupError::Validation(format!("Backup {backup_id} has conflicting retained service provenance; refusing orphan deletion")));
        }
        identity = Some(candidate);
    }
    identity
        .map(|(name, kind)| (name.to_string(), kind.to_string()))
        .ok_or_else(|| {
            BackupError::Validation(format!(
                "Backup {backup_id} has no retained service provenance; refusing orphan deletion"
            ))
        })
}

/// Prefer the admitted repository identity even while the live service has
/// been renamed. A live identity is only a compatibility fallback for legacy
/// backups with no snapshots; partial or conflicting snapshots fail closed.
pub(super) fn deletion_walg_service_identity(
    children: &[temps_entities::external_service_backups::Model],
    service_id: i32,
    backup_id: &str,
    live_identity: Option<(&str, &str)>,
) -> Result<(String, String), BackupError> {
    if live_identity.is_none()
        || children.iter().any(|child| {
            child.service_name_snapshot.is_some() || child.service_type_snapshot.is_some()
        })
    {
        return retained_walg_service_identity(children, service_id, backup_id);
    }
    if children.iter().any(|child| child.service_id != service_id) {
        return Err(BackupError::Validation(format!(
            "Backup {backup_id} has inconsistent retained service identity; refusing deletion"
        )));
    }
    match live_identity {
        Some((name, kind)) => Ok((name.to_string(), kind.to_string())),
        None => retained_walg_service_identity(children, service_id, backup_id),
    }
}

/// A validated engine selection, never inferred from an operator-controlled S3 path.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum WalgDeletionEngine {
    Postgres,
    PostgresCluster,
    Redis,
    MongoDb,
    MariaDb,
}

impl WalgDeletionEngine {
    pub(super) fn resolve(
        engine: &str,
        service_type: &str,
        backup_id: &str,
    ) -> Result<Self, BackupError> {
        let selected = match (engine, service_type) {
            ("postgres_walg", "postgres" | "postgresql" | "timescale" | "timescaledb") => Self::Postgres,
            ("postgres_cluster", "postgres" | "postgresql" | "timescale" | "timescaledb") => Self::PostgresCluster,
            ("redis", "redis") => Self::Redis,
            ("mongodb", "mongodb" | "mongo") => Self::MongoDb,
            ("mariadb_physical", "mariadb") => Self::MariaDb,
            _ => return Err(BackupError::Validation(format!(
                "Backup {backup_id} WAL-G engine '{engine}' is unsupported or incompatible with service type '{service_type}'; refusing deletion"
            ))),
        };
        Ok(selected)
    }

    fn namespace(self) -> &'static str {
        match self {
            Self::Postgres | Self::PostgresCluster => "postgres",
            Self::Redis => "redis",
            Self::MongoDb => "mongodb",
            Self::MariaDb => "mariadb",
        }
    }

    fn archives(self) -> bool {
        matches!(self, Self::Postgres | Self::PostgresCluster | Self::MariaDb)
    }

    pub(super) fn validate_repository(
        self,
        location: &str,
        bucket: &str,
        bucket_path: &str,
        service_name: &str,
    ) -> Result<(), BackupError> {
        let key = s3_key_from_location(location, bucket)?;
        let expected = build_s3_key(
            bucket_path,
            &format!("external_services/{}/{service_name}/walg", self.namespace()),
        );
        if key.trim_end_matches('/') != expected {
            return Err(BackupError::Validation(format!(
                "Refusing WAL-G deletion for unexpected repository '{location}' (expected s3://{bucket}/{expected})"
            )));
        }
        Ok(())
    }
}

/// The complete user data WAL-G writes into a snapshot sentinel.
#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub(super) struct WalgTargetUserData {
    temps_backup_id: String,
}

#[derive(Debug, Deserialize)]
struct WalgIdentityMetadata {
    walg_identity_version: u8,
    walg_target_user_data: WalgTargetUserData,
    walg_full_backup: Option<bool>,
}

pub(super) fn validated_walg_target(
    metadata: &serde_json::Value,
    backup_id: &str,
) -> Result<WalgTargetUserData, BackupError> {
    let parsed_id = Uuid::parse_str(backup_id).map_err(|_| {
        BackupError::Validation(format!(
            "Backup {backup_id} has an invalid WAL-G backup UUID; refusing deletion"
        ))
    })?;
    let identity: WalgIdentityMetadata = serde_json::from_value(metadata.clone()).map_err(|error| {
        BackupError::Unsupported(format!(
            "Backup {backup_id} has no verified exact WAL-G identity ({error}); retain it until a read-only repository inventory proves which snapshot belongs to this backup"
        ))
    })?;
    if parsed_id.to_string() != backup_id
        || identity.walg_identity_version != 1
        || identity.walg_target_user_data.temps_backup_id != backup_id
    {
        return Err(BackupError::Unsupported(format!(
            "Backup {backup_id} has no verified exact WAL-G identity; retain it until a read-only repository inventory proves which snapshot belongs to this backup"
        )));
    }
    if identity.walg_full_backup != Some(true) {
        return Err(BackupError::Validation(format!(
            "Backup {backup_id} lacks proof of an independent full WAL-G snapshot; refusing deletion that could affect dependent backups"
        )));
    }
    Ok(identity.walg_target_user_data)
}

/// Redis/MongoDB inventories use top-level sentinel objects. Never recursively
/// infer a target name from arbitrary nested metadata or from backup timestamps.
fn stream_walg_target_name(
    repository: &serde_json::Value,
    target: &WalgTargetUserData,
) -> Result<Option<String>, BackupError> {
    let entries = repository.as_array().ok_or_else(|| {
        BackupError::Validation(
            "WAL-G stream inventory is not an array; refusing deletion".to_string(),
        )
    })?;
    let mut found = None;
    for entry in entries {
        let name = entry.get("BackupName").and_then(serde_json::Value::as_str)
            .ok_or_else(|| BackupError::Validation("WAL-G stream inventory contains an entry without BackupName; refusing deletion".to_string()))?;
        let Some(user_data) = entry.get("UserData") else {
            continue;
        };
        if user_data
            .get("temps_backup_id")
            .and_then(serde_json::Value::as_str)
            != Some(target.temps_backup_id.as_str())
        {
            continue;
        }
        let exact: WalgTargetUserData =
            serde_json::from_value(user_data.clone()).map_err(|_| {
                BackupError::Validation(
                    "WAL-G stream identity has unexpected additional fields; refusing deletion"
                        .to_string(),
                )
            })?;
        if exact != *target {
            continue;
        }
        if name.len() != 23
            || !name.starts_with("stream_")
            || chrono::NaiveDateTime::parse_from_str(&name[7..], "%Y%m%dT%H%M%SZ").is_err()
        {
            return Err(BackupError::Validation(format!(
                "WAL-G stream inventory contains unsafe backup name '{name}'; refusing deletion"
            )));
        }
        if found.replace(name.to_string()).is_some() {
            return Err(BackupError::Validation("Multiple WAL-G stream snapshots match the selected backup UUID; refusing ambiguous deletion".to_string()));
        }
    }
    Ok(found)
}

impl BackupService {
    pub(super) async fn delete_walg_target_with_optional_service(
        &self,
        backup: &Backup,
        source: &S3Source,
        service_name: &str,
        container: Option<&str>,
        target: &WalgTargetUserData,
        engine: WalgDeletionEngine,
    ) -> Result<(), BackupError> {
        if let Some(container) = container {
            return self
                .delete_walg_target(backup, source, service_name, container, target, engine)
                .await;
        }
        // The helper has no database volume and never starts PostgreSQL. It
        // executes only the existing identity-checked WAL-G deletion path.
        let docker = bollard::Docker::connect_with_local_defaults().map_err(|error| {
            BackupError::ExternalService(format!(
                "Failed to connect to Docker for orphan backup {} cleanup: {error}",
                backup.backup_id
            ))
        })?;
        let image_ref = "gotempsh/postgres-walg:18-bookworm";
        let image = match docker.inspect_image(image_ref).await {
            Ok(image) => image,
            Err(bollard::errors::Error::DockerResponseServerError {
                status_code: 404, ..
            }) => {
                self.pull_postgres_image(image_ref).await.map_err(|error| {
                    BackupError::ExternalService(format!(
                        "Failed to pull WAL-G cleanup image for backup {}: {error}",
                        backup.backup_id
                    ))
                })?;
                docker.inspect_image(image_ref).await.map_err(|error| {
                    BackupError::ExternalService(format!(
                        "Failed to inspect WAL-G cleanup image for backup {}: {error}",
                        backup.backup_id
                    ))
                })?
            }
            Err(error) => {
                return Err(BackupError::ExternalService(format!(
                    "Failed to inspect WAL-G cleanup image for backup {}: {error}",
                    backup.backup_id
                )))
            }
        };
        let image_id = image.id.ok_or_else(|| {
            BackupError::Configuration(format!(
                "WAL-G cleanup image for backup {} has no immutable image identity",
                backup.backup_id
            ))
        })?;
        let helper = format!("temps-backup-cleanup-{}", Uuid::new_v4());
        let (created_id, mut helper_owner) = create_owned_orphan_helper(
            docker.clone(),
            helper.clone(),
            backup.backup_id.clone(),
            bollard::models::ContainerCreateBody {
                image: Some(image_id),
                entrypoint: Some(vec!["sh".to_string(), "-c".to_string()]),
                cmd: Some(vec!["sleep 360".to_string()]),
                user: Some("65534:65534".to_string()),
                labels: Some(std::collections::HashMap::from([(
                    "temps.backup_cleanup_id".to_string(),
                    backup.backup_id.clone(),
                )])),
                host_config: Some(bollard::models::HostConfig {
                    auto_remove: Some(true),
                    readonly_rootfs: Some(true),
                    memory: Some(128 * 1024 * 1024),
                    pids_limit: Some(64),
                    cap_drop: Some(vec!["ALL".to_string()]),
                    security_opt: Some(vec!["no-new-privileges:true".to_string()]),
                    network_mode: Some(temps_core::NETWORK_NAME.to_string()),
                    tmpfs: Some(std::collections::HashMap::from([
                        ("/tmp".to_string(), "rw,noexec,nosuid,size=64m".to_string()),
                        // Override the image's declared database volume;
                        // cleanup must not allocate a persistent PGDATA.
                        (
                            "/var/lib/postgresql".to_string(),
                            "rw,noexec,nosuid,size=1m,mode=1777".to_string(),
                        ),
                    ])),
                    ..Default::default()
                }),
                ..Default::default()
            },
        )
        .await?;
        let result = async {
            docker
                .start_container(
                    &created_id,
                    None::<bollard::query_parameters::StartContainerOptions>,
                )
                .await
                .map_err(|error| {
                    BackupError::ExternalService(format!(
                        "Failed to start orphan backup {} cleanup helper: {error}",
                        backup.backup_id
                    ))
                })?;
            self.delete_walg_target(backup, source, service_name, &helper, target, engine)
                .await
        }
        .await;
        // Auto-removal plus the bounded sleep also clean a helper if the
        // outer deadline cancels this future before explicit cleanup runs.
        let cleanup = docker
            .remove_container(
                &created_id,
                Some(bollard::query_parameters::RemoveContainerOptions {
                    force: true,
                    v: true,
                    ..Default::default()
                }),
            )
            .await;
        if let Err(error) = cleanup {
            if !matches!(
                error,
                bollard::errors::Error::DockerResponseServerError {
                    status_code: 404,
                    ..
                }
            ) {
                return Err(BackupError::PartialDeletion { backup_id: backup.backup_id.clone(), deleted_objects: 0, reason: format!("WAL-G cleanup result: {result:?}; helper '{helper}' cleanup failed: {error}; inspect the helper labelled temps.backup_cleanup_id={} and remove only that owned helper if cleanup remains unsuccessful", backup.backup_id) });
            }
        }
        helper_owner.armed = false;
        result
    }

    async fn delete_walg_target(
        &self,
        backup: &Backup,
        source: &S3Source,
        service_name: &str,
        container: &str,
        target_user_data: &WalgTargetUserData,
        engine: WalgDeletionEngine,
    ) -> Result<(), BackupError> {
        engine.validate_repository(
            &backup.s3_location,
            &source.bucket_name,
            &source.bucket_path,
            service_name,
        )?;

        let access_key = self
            .encryption_service
            .decrypt_string(&source.access_key_id)
            .map_err(|error| {
                BackupError::Configuration(format!(
                    "Failed to decrypt access key for S3 source {}: {}",
                    source.id, error
                ))
            })?;
        let secret_key = self
            .encryption_service
            .decrypt_string(&source.secret_key)
            .map_err(|error| {
                BackupError::Configuration(format!(
                    "Failed to decrypt secret key for S3 source {}: {}",
                    source.id, error
                ))
            })?;
        // `None` unless this source holds a temporary (STS-style) credential.
        let session_token = temps_entities::s3_sources::decrypt_session_token(
            self.encryption_service.as_ref(),
            source,
        )
        .map_err(|error| {
            BackupError::Configuration(format!(
                "Failed to decrypt session token for S3 source {}: {}",
                source.id, error
            ))
        })?;
        let docker = bollard::Docker::connect_with_local_defaults().map_err(|error| {
            BackupError::ExternalService(format!("Failed to connect to Docker: {}", error))
        })?;
        let endpoint = temps_providers::externalsvc::S3Credentials {
            access_key_id: access_key.clone(),
            secret_key: secret_key.clone(),
            session_token: session_token.clone(),
            region: source.region.clone(),
            endpoint: source.endpoint.clone(),
            bucket_name: source.bucket_name.clone(),
            bucket_path: source.bucket_path.clone(),
            force_path_style: source.force_path_style.unwrap_or(true),
        }
        .resolve_endpoint_for_container(&docker, container)
        .await;
        let mut env = vec![
            format!(
                "WALG_S3_PREFIX={}",
                backup.s3_location.trim_end_matches('/')
            ),
            format!(
                "WALG_TARGET_USER_DATA={}",
                serde_json::to_string(target_user_data)?
            ),
            format!("AWS_ACCESS_KEY_ID={}", access_key),
            format!("AWS_SECRET_ACCESS_KEY={}", secret_key),
            format!("AWS_REGION={}", source.region),
        ];
        // Absent for a long-lived credential, so its environment is unchanged.
        env.extend(temps_providers::externalsvc::aws_session_token_env(
            session_token.as_deref(),
        ));
        if let Some(endpoint) = endpoint {
            env.push(format!(
                "AWS_ENDPOINT={}",
                if endpoint.starts_with("http") {
                    endpoint
                } else {
                    format!("http://{}", endpoint)
                }
            ));
        }
        if source.force_path_style.unwrap_or(true) {
            env.push("AWS_S3_FORCE_PATH_STYLE=true".to_string());
        }
        let cancellation = tokio_util::sync::CancellationToken::new();
        let mut identity_remains = true;
        let mut last_delete_detail = String::new();
        for attempt in 1..=3 {
            let mut attempt_env = env.clone();
            let delete_command = if engine.archives() {
                "timeout -k 5s 35s wal-g delete target --target-user-data \"$WALG_TARGET_USER_DATA\" --confirm"
            } else {
                let inventory = crate::engines::postgres_walg::run_walg_exec(
                    &docker,
                    container,
                    "timeout -k 5s 35s wal-g backup-list --json --detail",
                    &env,
                    &cancellation,
                )
                .await
                .map_err(|error| BackupError::ExternalService(error.to_string()))?;
                if inventory.exit_code != 0 {
                    return Err(BackupError::ExternalService(format!(
                        "Backup {} WAL-G stream inventory failed before deletion: {}",
                        backup.backup_id, inventory.stderr
                    )));
                }
                let repository: serde_json::Value = serde_json::from_str(&inventory.stdout)?;
                let Some(name) = stream_walg_target_name(&repository, target_user_data)? else {
                    // A successful complete inventory also makes retries after a
                    // committed remote delete safe: no matching snapshot remains.
                    return Ok(());
                };
                let recheck = crate::engines::postgres_walg::run_walg_exec(
                    &docker,
                    container,
                    "timeout -k 5s 35s wal-g backup-list --json --detail",
                    &env,
                    &cancellation,
                )
                .await
                .map_err(|error| BackupError::ExternalService(error.to_string()))?;
                if recheck.exit_code != 0
                    || stream_walg_target_name(
                        &serde_json::from_str::<serde_json::Value>(&recheck.stdout)?,
                        target_user_data,
                    )? != Some(name.clone())
                {
                    return Err(BackupError::Validation(format!("Backup {} WAL-G stream inventory changed before deletion; retry with a fresh inventory", backup.backup_id)));
                }
                attempt_env.push(format!("WALG_DELETE_BACKUP_NAME={name}"));
                "timeout -k 5s 35s wal-g backup-delete \"$WALG_DELETE_BACKUP_NAME\" --confirm"
            };
            let result = crate::engines::postgres_walg::run_walg_exec(
                &docker,
                container,
                delete_command,
                &attempt_env,
                &cancellation,
            )
            .await
            .map_err(|error| BackupError::ExternalService(error.to_string()))?;
            last_delete_detail = format!(
                "attempt {} exited {}: {}",
                attempt,
                result.exit_code,
                if result.stderr.trim().is_empty() {
                    result.stdout.trim()
                } else {
                    result.stderr.trim()
                }
            );

            // WAL-G user data is not guaranteed unique. Always prove that no
            // matching full backup remains, even when delete reports success,
            // and repeat within the shared deadline if the repository held a
            // duplicate snapshot for the same Temps backup identity.
            let list = crate::engines::postgres_walg::run_walg_exec(
                &docker,
                container,
                "timeout -k 5s 35s wal-g backup-list --json --detail",
                &env,
                &cancellation,
            )
            .await
            .map_err(|error| BackupError::ExternalService(error.to_string()))?;
            if list.exit_code != 0 {
                return Err(BackupError::PartialDeletion {
                    backup_id: backup.backup_id.clone(),
                    deleted_objects: 0,
                    reason: format!(
                        "wal-g repository verification failed after {}: {}",
                        last_delete_detail,
                        if list.stderr.trim().is_empty() {
                            list.stdout.trim()
                        } else {
                            list.stderr.trim()
                        }
                    ),
                });
            }
            let repository: serde_json::Value = serde_json::from_str(&list.stdout).map_err(|error| {
                BackupError::PartialDeletion {
                    backup_id: backup.backup_id.clone(),
                    deleted_objects: 0,
                    reason: format!(
                        "wal-g target deletion failed and repository state could not be parsed: {}; output: {}",
                        error,
                        list.stdout.trim()
                    ),
                }
            })?;
            identity_remains = if engine.archives() {
                json_contains_backup_identity(&repository, &backup.backup_id)
            } else {
                stream_walg_target_name(&repository, target_user_data)
                    .map_err(|error| BackupError::PartialDeletion {
                        backup_id: backup.backup_id.clone(),
                        deleted_objects: 0,
                        reason: format!("WAL-G stream inventory could not verify deletion after {last_delete_detail}: {error}"),
                    })?
                    .is_some()
            };
            if !identity_remains {
                break;
            }
        }
        if identity_remains {
            return Err(BackupError::PartialDeletion {
                backup_id: backup.backup_id.clone(),
                deleted_objects: 0,
                reason: format!(
                    "wal-g exact-target deletion left matching snapshots after three attempts; {}",
                    last_delete_detail
                ),
            });
        }
        // Stream backups have no PostgreSQL WAL/MySQL binlog archive to collect.
        if !engine.archives() {
            return Ok(());
        }
        let garbage = crate::engines::postgres_walg::run_walg_exec(
            &docker,
            container,
            "timeout -k 5s 35s wal-g delete garbage ARCHIVES",
            &env,
            &cancellation,
        )
        .await
        .map_err(|error| BackupError::ExternalService(error.to_string()))?;
        if garbage.exit_code != 0 {
            return Err(BackupError::PartialDeletion {
                backup_id: backup.backup_id.clone(),
                deleted_objects: 1,
                reason: format!(
                    "WAL-G target was deleted but archive garbage collection failed: {}",
                    if garbage.stderr.trim().is_empty() {
                        garbage.stdout.trim()
                    } else {
                        garbage.stderr.trim()
                    }
                ),
            });
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::super::test_support::*;
    use super::*;

    async fn fake_helper_docker(
        create_failure: bool,
    ) -> (
        bollard::Docker,
        Arc<tokio::sync::Notify>,
        Arc<tokio::sync::Notify>,
        Arc<tokio::sync::Notify>,
    ) {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let started = Arc::new(tokio::sync::Notify::new());
        let release = Arc::new(tokio::sync::Notify::new());
        let deleted = Arc::new(tokio::sync::Notify::new());
        let (server_started, server_release, server_deleted) =
            (started.clone(), release.clone(), deleted.clone());
        tokio::spawn(async move {
            let mut created = false;
            while let Ok((mut stream, _)) = listener.accept().await {
                let mut request = [0u8; 8192];
                let read = stream.read(&mut request).await.unwrap();
                let request = String::from_utf8_lossy(&request[..read]);
                let (status, body) = if request.starts_with("POST ")
                    && request.contains("/containers/create")
                {
                    server_started.notify_one();
                    server_release.notified().await;
                    created = true;
                    if create_failure {
                        (
                            "500 Internal Server Error",
                            r#"{"message":"ambiguous create response"}"#,
                        )
                    } else {
                        ("201 Created", r#"{"Id":"owned-helper-id","Warnings":[]}"#)
                    }
                } else if request.starts_with("DELETE ") {
                    assert!(created, "a helper must have been committed before removal");
                    assert!(
                        request.contains("/containers/owned-helper-id?"),
                        "cleanup must use the immutable owned ID: {request}"
                    );
                    assert!(request.contains("force=true") && request.contains("v=true"));
                    ("204 No Content", "")
                } else if created {
                    (
                        "200 OK",
                        r#"{"Id":"owned-helper-id","Name":"/owned-helper","Config":{"Labels":{"temps.backup_cleanup_id":"owned-backup"}}}"#,
                    )
                } else {
                    ("404 Not Found", r#"{"message":"No such container"}"#)
                };
                let response = format!("HTTP/1.1 {status}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len());
                stream.write_all(response.as_bytes()).await.unwrap();
                if request.starts_with("DELETE ") {
                    server_deleted.notify_one();
                    break;
                }
            }
        });
        let docker = bollard::Docker::connect_with_http(
            &format!("http://{address}"),
            10,
            bollard::API_DEFAULT_VERSION,
        )
        .unwrap();
        (docker, started, release, deleted)
    }

    #[tokio::test]
    async fn orphan_helper_cancellation_during_create_removes_unstarted_helper() {
        let (docker, started, release, deleted) = fake_helper_docker(false).await;
        let caller = tokio::spawn(create_owned_orphan_helper(
            docker,
            "owned-helper".to_string(),
            "owned-backup".to_string(),
            Default::default(),
        ));
        time::timeout(std::time::Duration::from_secs(10), started.notified())
            .await
            .unwrap();
        caller.abort();
        assert!(matches!(caller.await, Err(error) if error.is_cancelled()));
        // Docker commits after the deletion request has been cancelled.
        release.notify_one();
        time::timeout(std::time::Duration::from_secs(10), deleted.notified())
            .await
            .expect("the unstarted helper must be removed after the create reply arrives");
    }

    #[tokio::test]
    async fn orphan_helper_drop_before_start_removes_created_helper() {
        let (docker, _, release, deleted) = fake_helper_docker(false).await;
        release.notify_one();
        let (_, owner) = create_owned_orphan_helper(
            docker,
            "owned-helper".to_string(),
            "owned-backup".to_string(),
            Default::default(),
        )
        .await
        .unwrap();
        drop(owner);
        time::timeout(std::time::Duration::from_secs(10), deleted.notified())
            .await
            .expect("Created-state helpers have no idle expiry and require owned cleanup");
    }

    #[tokio::test]
    async fn orphan_helper_ambiguous_create_failure_checks_label_before_cleanup() {
        let (docker, _, release, deleted) = fake_helper_docker(true).await;
        release.notify_one();
        assert!(create_owned_orphan_helper(
            docker,
            "owned-helper".to_string(),
            "owned-backup".to_string(),
            Default::default()
        )
        .await
        .is_err());
        time::timeout(std::time::Duration::from_secs(10), deleted.notified())
            .await
            .expect(
                "a committed labelled helper must be removed after an ambiguous create failure",
            );
    }

    #[test]
    fn orphan_walg_cleanup_uses_consistent_retained_identity() {
        let children = vec![
            make_external_service_backup(1, 801, 101),
            make_external_service_backup(2, 801, 101),
        ];
        assert_eq!(
            retained_walg_service_identity(&children, 101, "backup-801").unwrap(),
            ("service-101".to_string(), "postgres".to_string())
        );
        let engine =
            WalgDeletionEngine::resolve("postgres_walg", "postgres", "backup-801").unwrap();
        assert!(engine
            .validate_repository(
                "s3://bucket/external_services/postgres/service-101/walg",
                "bucket",
                "",
                "service-101",
            )
            .is_ok());
        assert!(engine
            .validate_repository(
                "s3://bucket/external_services/postgres/another-service/walg",
                "bucket",
                "",
                "service-101",
            )
            .is_err());
    }

    #[test]
    fn renamed_live_service_keeps_original_walg_repository_identity() {
        let child = make_external_service_backup(1, 801, 101);
        let identity = deletion_walg_service_identity(
            std::slice::from_ref(&child),
            101,
            "backup-801",
            Some(("renamed-service", "postgres")),
        )
        .unwrap();
        assert_eq!(
            identity,
            ("service-101".to_string(), "postgres".to_string())
        );
        let engine =
            WalgDeletionEngine::resolve("postgres_walg", &identity.1, "backup-801").unwrap();
        assert!(engine
            .validate_repository(
                "s3://bucket/owned/external_services/postgres/service-101/walg",
                "bucket",
                "owned",
                &identity.0
            )
            .is_ok());
        assert!(engine
            .validate_repository(
                "s3://bucket/owned/external_services/postgres/renamed-service/walg",
                "bucket",
                "owned",
                &identity.0
            )
            .is_err());
        assert!(engine
            .validate_repository(
                "s3://other/owned/external_services/postgres/service-101/walg",
                "bucket",
                "owned",
                &identity.0
            )
            .is_err());
        let mut legacy = child.clone();
        legacy.service_name_snapshot = None;
        legacy.service_type_snapshot = None;
        assert_eq!(
            deletion_walg_service_identity(
                std::slice::from_ref(&legacy),
                101,
                "backup-801",
                Some(("renamed-service", "postgres"))
            )
            .unwrap()
            .0,
            "renamed-service"
        );
        assert!(deletion_walg_service_identity(
            std::slice::from_ref(&legacy),
            101,
            "backup-801",
            None
        )
        .is_err());
        legacy.service_type_snapshot = Some("postgres".to_string());
        assert!(deletion_walg_service_identity(
            &[child, legacy],
            101,
            "backup-801",
            Some(("renamed-service", "postgres"))
        )
        .is_err());
    }

    #[test]
    fn orphan_walg_cleanup_refuses_missing_or_conflicting_provenance() {
        let child = make_external_service_backup(1, 801, 101);
        assert!(retained_walg_service_identity(&[], 101, "backup-801").is_err());
        assert!(
            retained_walg_service_identity(std::slice::from_ref(&child), 102, "backup-801")
                .is_err()
        );
        let mut missing = child.clone();
        missing.service_name_snapshot = None;
        assert!(retained_walg_service_identity(&[missing], 101, "backup-801").is_err());
        let mut conflicting = child.clone();
        conflicting.service_type_snapshot = Some("redis".to_string());
        assert!(retained_walg_service_identity(&[child, conflicting], 101, "backup-801").is_err());
    }

    #[test]
    fn walg_deletion_preserves_engine_repository_and_managed_prefix() {
        for (engine, service_type, namespace, archives) in [
            ("postgres_walg", "postgres", "postgres", true),
            ("postgres_cluster", "timescaledb", "postgres", true),
            ("redis", "redis", "redis", false),
            ("mongodb", "mongo", "mongodb", false),
            ("mariadb_physical", "mariadb", "mariadb", true),
        ] {
            let plan = WalgDeletionEngine::resolve(engine, service_type, "backup-fixture").unwrap();
            assert_eq!(plan.archives(), archives);
            for prefix in ["", "tenant/managed-backups"] {
                let root = build_s3_key(
                    prefix,
                    &format!("external_services/{namespace}/fixture/walg"),
                );
                assert!(plan
                    .validate_repository(
                        &format!("s3://bucket/{root}"),
                        "bucket",
                        prefix,
                        "fixture"
                    )
                    .is_ok());
                assert!(plan
                    .validate_repository(&format!("s3://other/{root}"), "bucket", prefix, "fixture")
                    .is_err());
                assert!(plan
                    .validate_repository(
                        &format!("s3://bucket/{root}/another"),
                        "bucket",
                        prefix,
                        "fixture"
                    )
                    .is_err());
                assert!(plan
                    .validate_repository(
                        &format!("s3://bucket/{root}"),
                        "bucket",
                        prefix,
                        "other-service"
                    )
                    .is_err());
            }
        }
        let redis = WalgDeletionEngine::resolve("redis", "redis", "backup-fixture").unwrap();
        assert!(redis
            .validate_repository(
                "s3://bucket/external_services/postgres/fixture/walg",
                "bucket",
                "",
                "fixture"
            )
            .is_err());
    }

    #[test]
    fn walg_deletion_stream_inventory_is_exact_unique_and_path_safe() {
        let target = WalgTargetUserData {
            temps_backup_id: "00000000-0000-4000-8000-000000000001".to_string(),
        };
        let entry = json!({"BackupName":"stream_20260923T120000Z", "UserData":target});
        assert_eq!(
            stream_walg_target_name(&json!([entry.clone()]), &target).unwrap(),
            Some("stream_20260923T120000Z".to_string())
        );
        assert!(stream_walg_target_name(&json!([entry.clone(), entry.clone()]), &target).is_err());
        assert_eq!(stream_walg_target_name(&json!([]), &target).unwrap(), None);
        assert!(stream_walg_target_name(&json!({}), &target).is_err());
        assert!(stream_walg_target_name(&json!([{}]), &target).is_err());
        for name in [
            "../other",
            "--all",
            "stream_20269999T999999Z",
            "stream_20260923T120000Z/",
            "stream_20260923T120000Z;rm",
        ] {
            let mut unsafe_entry = entry.clone();
            unsafe_entry["BackupName"] = json!(name);
            assert!(stream_walg_target_name(&json!([unsafe_entry]), &target).is_err());
        }
        let mut other = entry.clone();
        other["UserData"]["temps_backup_id"] = json!("00000000-0000-4000-8000-000000000002");
        assert_eq!(
            stream_walg_target_name(&json!([other, entry.clone()]), &target).unwrap(),
            Some("stream_20260923T120000Z".to_string())
        );
        let mut augmented = entry;
        augmented["UserData"]["extra"] = json!(true);
        assert!(stream_walg_target_name(&json!([augmented]), &target).is_err());
    }

    #[test]
    fn walg_deletion_rejects_unknown_or_mismatched_engine() {
        for (engine, service_type) in [
            ("unknown", "redis"),
            ("redis", "postgres"),
            ("postgres_walg", "redis"),
            ("mariadb_dump", "mariadb"),
        ] {
            assert!(matches!(
                WalgDeletionEngine::resolve(engine, service_type, "backup-fixture"),
                Err(BackupError::Validation(_))
            ));
        }
    }

    #[test]
    fn walg_deletion_requires_exact_identity_and_independent_full_snapshot() {
        let metadata = json!({"walg_identity_version":1, "walg_target_user_data":{"temps_backup_id":"00000000-0000-4000-8000-000000000001"}, "walg_full_backup":true});
        assert_eq!(
            validated_walg_target(&metadata, "00000000-0000-4000-8000-000000000001").unwrap(),
            WalgTargetUserData {
                temps_backup_id: "00000000-0000-4000-8000-000000000001".to_string(),
            }
        );
        assert!(validated_walg_target(&metadata, "00000000-0000-4000-8000-000000000002").is_err());
        for field in [
            "walg_identity_version",
            "walg_target_user_data",
            "walg_full_backup",
        ] {
            let mut incomplete = metadata.clone();
            incomplete.as_object_mut().unwrap().remove(field);
            assert!(
                validated_walg_target(&incomplete, "00000000-0000-4000-8000-000000000001").is_err(),
                "{field}"
            );
        }
        assert!(matches!(
            validated_walg_target(&metadata, "not-a-uuid"),
            Err(BackupError::Validation(_))
        ));
        let mut augmented = metadata.clone();
        augmented["walg_target_user_data"]["unexpected"] = json!(true);
        assert!(validated_walg_target(&augmented, "00000000-0000-4000-8000-000000000001").is_err());
        let mut delta = metadata.clone();
        delta["walg_full_backup"] = json!(false);
        assert!(validated_walg_target(&delta, "00000000-0000-4000-8000-000000000001").is_err());
        assert!(matches!(
            validated_walg_target(&json!({}), "00000000-0000-4000-8000-000000000003"),
            Err(BackupError::Unsupported(_))
        ));
    }
}
