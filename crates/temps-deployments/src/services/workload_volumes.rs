// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Opt-in volumes for single-container deployments (ADR-055 §12, Phase 9).
//!
//! A volume belongs to an environment (`environment/<id>`), so it outlives
//! the revisions that mount it. Attaching one mounts it in every replica the
//! environment deploys from then on, and pins the environment to the
//! volume's node (§8).
//!
//! What a deploy with volumes needs, checked here for the status the
//! console shows and again by the deploy job, which fails with the same
//! reason instead of starting a replica without its data:
//!
//! - `environments.workload_mode = enforce`: only the workload plane mounts
//!   volumes; `legacy` and `shadow` keep today's deploy path unchanged;
//! - one replica: a node-local volume cannot be shared between replicas;
//! - the control plane (node 0): the plane runs long-running replicas only
//!   there for now; a volume placed on a worker reports `Scheduled = False`
//!   with the pin as the reason.
//!
//! An environment with no attachment deploys exactly as before; nothing
//! here runs on the deploy path for it beyond one indexed query.

use std::sync::Arc;

use sea_orm::{DatabaseConnection, DbErr, EntityTrait};
use temps_entities::environment_workload_mode::EnvironmentWorkloadMode;
use temps_entities::{environments, projects};
use temps_workload_plane::scheduler::CONTROL_PLANE_NODE_ID;
use temps_workload_plane::volumes::{named_backing_ref, VolumeStoreError};
use temps_workload_plane::{NewVolume, Owner, VolumeRecord, VolumeStore};
use temps_workload_volumes::{BackingKind, OnOwnerDelete, VolumeProvider};

/// Owner kind of an environment's volumes.
pub const ENVIRONMENT_VOLUME_OWNER: &str = "environment";

/// The volume owner of environment `environment_id`.
pub fn environment_volume_owner(environment_id: i32) -> Owner {
    Owner::new(ENVIRONMENT_VOLUME_OWNER, i64::from(environment_id))
}

#[derive(Debug, thiserror::Error)]
pub enum WorkloadVolumeError {
    #[error("Environment {environment_id} not found")]
    EnvironmentNotFound { environment_id: i32 },

    #[error("Volume {volume_id} not found in environment {environment_id}")]
    NotFound { volume_id: i64, environment_id: i32 },

    #[error("Environment {environment_id} already has a volume named '{name}'")]
    NameTaken { environment_id: i32, name: String },

    #[error("Environment {environment_id} already mounts volume '{other}' at '{mount_path}'")]
    MountPathTaken {
        environment_id: i32,
        mount_path: String,
        other: String,
    },

    #[error("Invalid volume for environment {environment_id}: {message}")]
    Validation {
        environment_id: i32,
        message: String,
    },

    #[error(
        "Volume {volume_id} ('{name}') of environment {environment_id} could not be removed: \
         {reason}"
    )]
    InUse {
        volume_id: i64,
        environment_id: i32,
        name: String,
        reason: String,
    },

    #[error("Volume storage failed for environment {environment_id}: {reason}")]
    Storage { environment_id: i32, reason: String },

    #[error("Database error while {operation} for environment {environment_id}: {source}")]
    Database {
        operation: &'static str,
        environment_id: i32,
        #[source]
        source: DbErr,
    },
}

impl WorkloadVolumeError {
    fn from_store(environment_id: i32, error: VolumeStoreError) -> Self {
        match error {
            VolumeStoreError::NotFound { volume_id, .. } => WorkloadVolumeError::NotFound {
                volume_id,
                environment_id,
            },
            VolumeStoreError::NameTaken { name, .. } => WorkloadVolumeError::NameTaken {
                environment_id,
                name,
            },
            VolumeStoreError::MountPathTaken {
                mount_path, other, ..
            } => WorkloadVolumeError::MountPathTaken {
                environment_id,
                mount_path,
                other,
            },
            VolumeStoreError::Invalid { name, source, .. } => WorkloadVolumeError::Validation {
                environment_id,
                message: format!("volume '{name}': {source}"),
            },
            VolumeStoreError::Validation { message, .. } => WorkloadVolumeError::Validation {
                environment_id,
                message,
            },
            VolumeStoreError::Corrupt { .. } | VolumeStoreError::Database { .. } => {
                WorkloadVolumeError::Storage {
                    environment_id,
                    reason: error.to_string(),
                }
            }
        }
    }
}

/// A volume to create for an environment.
#[derive(Debug, Clone)]
pub struct CreateEnvironmentVolume {
    pub name: String,
    pub size_mb: Option<u64>,
    pub retain_on_delete: bool,
}

/// Whether the environment's volumes are mounted on its next deploy, and
/// why not.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EnvironmentVolumeStatus {
    pub environment_id: i32,
    pub workload_mode: EnvironmentWorkloadMode,
    pub replicas: i32,
    pub attached: usize,
    /// `true` when the next deploy mounts the attached volumes.
    pub active: bool,
    /// What blocks it, when `active` is false and something is attached
    /// (or would be).
    pub reason: Option<String>,
    /// Where the console sends the operator to fix it.
    pub setup_path: Option<String>,
}

/// What deleting a volume did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeletedEnvironmentVolume {
    pub volume: VolumeRecord,
    /// The Docker volume was removed (its data is gone).
    pub backing_removed: bool,
    /// Why the backing was kept, when it was.
    pub backing_kept_reason: Option<String>,
}

/// Why environment volumes cannot be mounted with these settings, if they
/// cannot. Shared by the status endpoint and the deploy job.
pub fn volumes_blocker(
    environment_id: i32,
    mode: EnvironmentWorkloadMode,
    replicas: i32,
    node_id: Option<i32>,
) -> Option<String> {
    if mode != EnvironmentWorkloadMode::Enforce {
        return Some(format!(
            "Environment {environment_id} has workload_mode '{}'; volumes are mounted only \
             through the workload plane. Set workload_mode to enforce, or detach the volumes",
            mode.as_str()
        ));
    }
    if replicas != 1 {
        return Some(format!(
            "Environment {environment_id} runs {replicas} replicas; a node-local volume can be \
             mounted by one replica only. Set replicas to 1, or detach the volumes"
        ));
    }
    match node_id {
        Some(node) if node != CONTROL_PLANE_NODE_ID => Some(format!(
            "A volume of environment {environment_id} lives on node {node}; deployment replicas \
             with volumes run only on the control plane (node 0) in this release, so the \
             replica would stay unscheduled. Move the workload's data to a volume on node 0"
        )),
        _ => None,
    }
}

pub struct WorkloadVolumeService {
    db: Arc<DatabaseConnection>,
    store: VolumeStore,
    /// Removes backings on the control plane. `None` without Docker: a
    /// deleted volume's backing is then kept, and the response says so.
    provider: Option<Arc<dyn VolumeProvider>>,
}

impl WorkloadVolumeService {
    pub fn new(db: Arc<DatabaseConnection>, provider: Option<Arc<dyn VolumeProvider>>) -> Self {
        Self {
            store: VolumeStore::new(db.clone()),
            db,
            provider,
        }
    }

    async fn environment(
        &self,
        environment_id: i32,
    ) -> Result<(environments::Model, Option<projects::Model>), WorkloadVolumeError> {
        environments::Entity::find_by_id(environment_id)
            .find_also_related(projects::Entity)
            .one(self.db.as_ref())
            .await
            .map_err(|source| WorkloadVolumeError::Database {
                operation: "reading the environment",
                environment_id,
                source,
            })?
            .ok_or(WorkloadVolumeError::EnvironmentNotFound { environment_id })
    }

    pub async fn create(
        &self,
        environment_id: i32,
        request: CreateEnvironmentVolume,
    ) -> Result<VolumeRecord, WorkloadVolumeError> {
        self.environment(environment_id).await?;
        self.store
            .create(NewVolume {
                owner: environment_volume_owner(environment_id),
                name: request.name,
                backing: BackingKind::Named,
                node_id: None,
                size_mb: request.size_mb,
                retain_on_delete: request.retain_on_delete,
            })
            .await
            .map_err(|e| WorkloadVolumeError::from_store(environment_id, e))
    }

    pub async fn list(
        &self,
        environment_id: i32,
        page: Option<u64>,
        page_size: Option<u64>,
    ) -> Result<(Vec<VolumeRecord>, u64), WorkloadVolumeError> {
        self.environment(environment_id).await?;
        self.store
            .list(&environment_volume_owner(environment_id), page, page_size)
            .await
            .map_err(|e| WorkloadVolumeError::from_store(environment_id, e))
    }

    pub async fn attach(
        &self,
        environment_id: i32,
        volume_id: i64,
        mount_path: &str,
        read_only: bool,
    ) -> Result<VolumeRecord, WorkloadVolumeError> {
        self.environment(environment_id).await?;
        self.store
            .attach(
                &environment_volume_owner(environment_id),
                volume_id,
                mount_path,
                read_only,
            )
            .await
            .map_err(|e| WorkloadVolumeError::from_store(environment_id, e))
    }

    pub async fn detach(
        &self,
        environment_id: i32,
        volume_id: i64,
    ) -> Result<VolumeRecord, WorkloadVolumeError> {
        self.store
            .detach(&environment_volume_owner(environment_id), volume_id)
            .await
            .map_err(|e| WorkloadVolumeError::from_store(environment_id, e))
    }

    /// Delete a volume. Its data is removed unless it is retained, or lives
    /// on a worker (which this release cannot reach to remove it; the
    /// response says so). A volume a running replica still mounts is
    /// refused by Docker and the row is kept.
    pub async fn delete(
        &self,
        environment_id: i32,
        volume_id: i64,
    ) -> Result<DeletedEnvironmentVolume, WorkloadVolumeError> {
        let owner = environment_volume_owner(environment_id);
        let volume = self
            .store
            .get(&owner, volume_id)
            .await
            .map_err(|e| WorkloadVolumeError::from_store(environment_id, e))?;
        let kept = match (
            temps_workload_plane::volumes::volume_disposition(&volume),
            volume.node_id,
            self.provider.as_ref(),
        ) {
            (OnOwnerDelete::Retain, _, _) => Some("retain_on_delete is set".to_string()),
            // Never created: nothing to remove.
            (OnOwnerDelete::Delete, None, _) => None,
            (OnOwnerDelete::Delete, Some(node), Some(provider))
                if node == CONTROL_PLANE_NODE_ID =>
            {
                provider
                    .delete(&volume.as_volume(temps_workload::WorkloadRef::new(
                        temps_workload::WorkloadKind::Deployment,
                        0,
                    )))
                    .await
                    .map_err(|e| WorkloadVolumeError::InUse {
                        volume_id,
                        environment_id,
                        name: volume.name.clone(),
                        reason: format!(
                            "{e}. A replica that still mounts it must be removed first: detach \
                             the volume and redeploy"
                        ),
                    })?;
                None
            }
            (OnOwnerDelete::Delete, Some(node), None) if node == CONTROL_PLANE_NODE_ID => Some(
                "this process has no Docker daemon to remove it; remove the Docker volume by \
                 hand"
                    .to_string(),
            ),
            (OnOwnerDelete::Delete, Some(node), _) => Some(format!(
                "it lives on node {node}, where this release cannot remove it; remove Docker \
                 volume '{}' on that node",
                named_backing_ref(&owner, &volume.name)
            )),
        };
        let deleted = self
            .store
            .delete(&owner, volume_id)
            .await
            .map_err(|e| WorkloadVolumeError::from_store(environment_id, e))?;
        Ok(DeletedEnvironmentVolume {
            backing_removed: kept.is_none() && volume.node_id.is_some(),
            backing_kept_reason: kept,
            volume: deleted.volume,
        })
    }

    /// Whether the next deploy mounts the environment's volumes.
    pub async fn status(
        &self,
        environment_id: i32,
    ) -> Result<EnvironmentVolumeStatus, WorkloadVolumeError> {
        let (environment, project) = self.environment(environment_id).await?;
        let replicas = environment
            .deployment_config
            .as_ref()
            .map(|c| c.replicas)
            .or_else(|| {
                project
                    .as_ref()
                    .and_then(|p| p.deployment_config.as_ref())
                    .map(|c| c.replicas)
            })
            .map(|replicas| replicas.max(1))
            .unwrap_or(1);
        let records = self
            .store
            .all_of(&environment_volume_owner(environment_id))
            .await
            .map_err(|e| WorkloadVolumeError::from_store(environment_id, e))?;
        let attached: Vec<&VolumeRecord> =
            records.iter().filter(|r| r.attachment.is_some()).collect();
        let node = attached.iter().find_map(|r| r.node_id);
        let blocker = volumes_blocker(environment_id, environment.workload_mode, replicas, node);
        Ok(EnvironmentVolumeStatus {
            environment_id,
            workload_mode: environment.workload_mode,
            replicas,
            attached: attached.len(),
            active: !attached.is_empty() && blocker.is_none(),
            setup_path: blocker
                .as_ref()
                .map(|_| format!("/environments/{environment_id}/settings")),
            reason: blocker,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn volumes_need_enforce_one_replica_and_the_control_plane() {
        assert_eq!(
            volumes_blocker(3, EnvironmentWorkloadMode::Enforce, 1, None),
            None
        );
        assert_eq!(
            volumes_blocker(3, EnvironmentWorkloadMode::Enforce, 1, Some(0)),
            None
        );
        let legacy = volumes_blocker(3, EnvironmentWorkloadMode::Legacy, 1, None).expect("legacy");
        assert!(legacy.contains("workload_mode 'legacy'"), "{legacy}");
        assert!(legacy.contains("Environment 3"), "{legacy}");
        let shadow = volumes_blocker(3, EnvironmentWorkloadMode::Shadow, 1, None).expect("shadow");
        assert!(shadow.contains("enforce"), "{shadow}");
        let two = volumes_blocker(3, EnvironmentWorkloadMode::Enforce, 2, None).expect("two");
        assert!(two.contains("2 replicas"), "{two}");
        let worker =
            volumes_blocker(3, EnvironmentWorkloadMode::Enforce, 1, Some(7)).expect("worker");
        assert!(worker.contains("node 7"), "{worker}");
    }

    #[test]
    fn store_errors_keep_the_environment_and_the_volume() {
        let error = WorkloadVolumeError::from_store(
            3,
            VolumeStoreError::NotFound {
                volume_id: 9,
                owner: "environment/3".into(),
            },
        );
        assert!(matches!(
            error,
            WorkloadVolumeError::NotFound {
                volume_id: 9,
                environment_id: 3
            }
        ));
        assert_eq!(error.to_string(), "Volume 9 not found in environment 3");
    }
}
