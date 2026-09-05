// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

use std::path::{Path, PathBuf};
use std::sync::Arc;

use async_trait::async_trait;
use chrono::Utc;
use sea_orm::{
    ActiveModelTrait, ColumnTrait, DatabaseConnection, EntityTrait, QueryFilter, Set,
    TransactionTrait,
};
use sha2::{Digest, Sha256};
use temps_core::{
    DeploymentCreatedJob, Job, JobQueue, SourceDropDeployer, SourceDropDeployment, SourceDropError,
    SourceDropRequest,
};
use temps_entities::deployments::DeploymentMetadata;
use temps_entities::source_type::SourceType;
use temps_entities::{deployments, environments, projects, source_bundles};
use tokio::io::{AsyncReadExt, AsyncWriteExt};

use super::{DeploymentGateSlot, JobProcessorService, WorkflowExecutionService, WorkflowPlanner};

const MAX_SOURCE_BYTES: u64 = 500 * 1024 * 1024;

/// Runs the server-side half of `temps drop` after a caller has prepared a ZIP.
/// Both the CLI upload handler and application chat can use this service, so
/// job planning, queueing, deployment gates, and rollback stay identical.
pub struct SourceDropService {
    db: Arc<DatabaseConnection>,
    data_dir: PathBuf,
    workflow_planner: Arc<WorkflowPlanner>,
    workflow_executor: Arc<WorkflowExecutionService>,
    queue: Arc<dyn JobQueue>,
    deployment_gate: DeploymentGateSlot,
}

impl SourceDropService {
    pub fn new(
        db: Arc<DatabaseConnection>,
        data_dir: PathBuf,
        workflow_planner: Arc<WorkflowPlanner>,
        workflow_executor: Arc<WorkflowExecutionService>,
        queue: Arc<dyn JobQueue>,
        deployment_gate: DeploymentGateSlot,
    ) -> Self {
        Self {
            db,
            data_dir,
            workflow_planner,
            workflow_executor,
            queue,
            deployment_gate,
        }
    }

    async fn rollback(
        &self,
        deployment_id: Option<i32>,
        bundle_id: Option<i32>,
        archive_path: &Path,
    ) {
        if let Some(deployment_id) = deployment_id {
            if let Err(error) = deployments::Entity::delete_by_id(deployment_id)
                .exec(self.db.as_ref())
                .await
            {
                tracing::error!(deployment_id, %error, "failed to roll back source-drop deployment");
            }
        }
        if let Some(bundle_id) = bundle_id {
            if let Err(error) = source_bundles::Entity::delete_by_id(bundle_id)
                .exec(self.db.as_ref())
                .await
            {
                tracing::error!(bundle_id, %error, "failed to roll back source-drop bundle");
            }
        }
        if let Err(error) = tokio::fs::remove_file(archive_path).await {
            if error.kind() != std::io::ErrorKind::NotFound {
                tracing::error!(path = %archive_path.display(), %error, "failed to remove rolled-back source-drop archive");
            }
        }
    }
}

async fn stage_archive(
    data_dir: &Path,
    source: &Path,
) -> Result<(String, PathBuf, u64, String), SourceDropError> {
    let relative_path = format!("source-bundles/{}.zip", uuid::Uuid::new_v4());
    let absolute_path = data_dir.join(&relative_path);
    let parent = absolute_path
        .parent()
        .ok_or_else(|| SourceDropError::Storage {
            reason: "generated source archive has no parent directory".to_string(),
        })?;
    tokio::fs::create_dir_all(parent)
        .await
        .map_err(|error| SourceDropError::Storage {
            reason: format!("could not create {}: {error}", parent.display()),
        })?;
    let mut input =
        tokio::fs::File::open(source)
            .await
            .map_err(|error| SourceDropError::Storage {
                reason: format!("could not open {}: {error}", source.display()),
            })?;
    let mut output = tokio::fs::File::create(&absolute_path)
        .await
        .map_err(|error| SourceDropError::Storage {
            reason: format!("could not create {}: {error}", absolute_path.display()),
        })?;
    let mut size_bytes = 0_u64;
    let mut hasher = Sha256::new();
    let mut buffer = vec![0_u8; 64 * 1024];
    loop {
        let read = input
            .read(&mut buffer)
            .await
            .map_err(|error| SourceDropError::Storage {
                reason: format!("could not read {}: {error}", source.display()),
            })?;
        if read == 0 {
            break;
        }
        size_bytes = size_bytes.saturating_add(read as u64);
        if size_bytes > MAX_SOURCE_BYTES {
            let _ = tokio::fs::remove_file(&absolute_path).await;
            return Err(SourceDropError::ArchiveTooLarge {
                max_bytes: MAX_SOURCE_BYTES,
            });
        }
        hasher.update(&buffer[..read]);
        output
            .write_all(&buffer[..read])
            .await
            .map_err(|error| SourceDropError::Storage {
                reason: format!("could not write {}: {error}", absolute_path.display()),
            })?;
    }
    output
        .flush()
        .await
        .map_err(|error| SourceDropError::Storage {
            reason: format!("could not flush {}: {error}", absolute_path.display()),
        })?;
    let validation_path = absolute_path.clone();
    let validation = tokio::task::spawn_blocking(move || -> Result<(), std::io::Error> {
        let mut file = std::fs::File::open(&validation_path)?;
        temps_core::archive_security::validate_zip_metadata(&mut file)?;
        zip::ZipArchive::new(file)
            .map(|_| ())
            .map_err(std::io::Error::other)
    })
    .await
    .map_err(|error| SourceDropError::InvalidArchive {
        reason: format!("validation task failed: {error}"),
    })?;
    if let Err(error) = validation {
        let _ = tokio::fs::remove_file(&absolute_path).await;
        return Err(SourceDropError::InvalidArchive {
            reason: error.to_string(),
        });
    }
    let checksum = format!("sha256:{}", hex::encode(hasher.finalize()));
    Ok((relative_path, absolute_path, size_bytes, checksum))
}

#[async_trait]
impl SourceDropDeployer for SourceDropService {
    async fn deploy_source_drop(
        &self,
        request: SourceDropRequest,
    ) -> Result<SourceDropDeployment, SourceDropError> {
        let project = projects::Entity::find_by_id(request.project_id)
            .filter(projects::Column::IsDeleted.eq(false))
            .one(self.db.as_ref())
            .await
            .map_err(|error| SourceDropError::Database {
                reason: error.to_string(),
            })?
            .ok_or(SourceDropError::ProjectNotFound {
                project_id: request.project_id,
            })?;
        if project.source_type != SourceType::UploadedSource
            && !project.allow_alternate_sources.unwrap_or(false)
        {
            return Err(SourceDropError::SourceNotAllowed {
                project_id: project.id,
                reason: format!("its configured source is '{}'; enable alternate sources or use an uploaded-source project", project.source_type),
            });
        }
        let environment = environments::Entity::find_by_id(request.environment_id)
            .filter(environments::Column::DeletedAt.is_null())
            .one(self.db.as_ref())
            .await
            .map_err(|error| SourceDropError::Database {
                reason: error.to_string(),
            })?
            .filter(|environment| environment.project_id == project.id)
            .ok_or(SourceDropError::EnvironmentNotFound {
                environment_id: request.environment_id,
                project_id: project.id,
            })?;
        let (relative_path, absolute_path, size_bytes, checksum) =
            stage_archive(&self.data_dir, &request.archive_path).await?;
        let transaction = self
            .db
            .begin()
            .await
            .map_err(|error| SourceDropError::Database {
                reason: error.to_string(),
            })?;
        let now = Utc::now();
        let bundle = match (source_bundles::ActiveModel {
            project_id: Set(project.id),
            archive_path: Set(relative_path.clone()),
            original_filename: Set(Some(request.original_filename)),
            content_type: Set("application/zip".to_string()),
            size_bytes: Set(size_bytes as i64),
            checksum: Set(checksum),
            directory: Set(project.directory.clone()),
            preset: Set(project.preset.as_str().to_string()),
            metadata: Set(None),
            uploaded_at: Set(now),
            created_at: Set(now),
            ..Default::default()
        })
        .insert(&transaction)
        .await
        {
            Ok(bundle) => bundle,
            Err(error) => {
                let _ = transaction.rollback().await;
                self.rollback(None, None, &absolute_path).await;
                return Err(SourceDropError::Database {
                    reason: format!("could not register source bundle: {error}"),
                });
            }
        };
        let deployment = match (deployments::ActiveModel {
            project_id: Set(project.id), environment_id: Set(environment.id),
            slug: Set(format!("{}-{}", project.slug, &uuid::Uuid::new_v4().simple().to_string()[..12])),
            state: Set("pending".to_string()),
            metadata: Set(Some(DeploymentMetadata {
                source_bundle_id: Some(bundle.id), source_bundle_path: Some(relative_path),
                source_bundle_content_type: Some("application/zip".to_string()), deployment_source_type: Some(SourceType::UploadedSource),
                ..Default::default()
            })),
            context_vars: Set(Some(serde_json::json!({"trigger":"drop","source":"application_workspace","bundle_id":bundle.id}))),
            created_at: Set(now), updated_at: Set(now), ..Default::default()
        }).insert(&transaction).await {
            Ok(deployment) => deployment,
            Err(error) => {
                let _ = transaction.rollback().await;
                self.rollback(None, None, &absolute_path).await;
                return Err(SourceDropError::Database { reason: format!("could not create deployment: {error}") });
            }
        };
        if let Err(error) = transaction.commit().await {
            self.rollback(None, None, &absolute_path).await;
            return Err(SourceDropError::Database {
                reason: format!("could not commit source deployment: {error}"),
            });
        }
        if let Err(error) = self
            .workflow_planner
            .create_deployment_jobs(deployment.id)
            .await
        {
            self.rollback(Some(deployment.id), Some(bundle.id), &absolute_path)
                .await;
            return Err(SourceDropError::Workflow {
                reason: error.to_string(),
            });
        }
        if let Err(error) = self
            .queue
            .send(Job::DeploymentCreated(DeploymentCreatedJob {
                deployment_id: deployment.id,
                project_id: project.id,
                environment_id: environment.id,
                environment_name: environment.name.clone(),
                branch: None,
                commit_sha: None,
            }))
            .await
        {
            self.rollback(Some(deployment.id), Some(bundle.id), &absolute_path)
                .await;
            return Err(SourceDropError::Queue {
                reason: error.to_string(),
            });
        }
        let db = self.db.clone();
        let workflow_executor = self.workflow_executor.clone();
        let deployment_gate = self.deployment_gate.read().await.clone();
        let environment_name = environment.name.clone();
        let deployment_id = deployment.id;
        let project_id = project.id;
        tokio::spawn(async move {
            JobProcessorService::gate_check_then_run(
                &db,
                &workflow_executor,
                &deployment_gate,
                project_id,
                &environment_name,
                deployment_id,
            )
            .await;
        });
        Ok(SourceDropDeployment {
            id: deployment.id,
            project_id,
            environment_id: environment.id,
            slug: deployment.slug,
            state: deployment.state,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn source_drop_limit_matches_the_public_upload_limit() {
        assert_eq!(MAX_SOURCE_BYTES, 500 * 1024 * 1024);
    }

    #[tokio::test]
    async fn stage_archive_rejects_invalid_zip_and_removes_staged_copy() {
        let data_dir = tempfile::tempdir().expect("data directory");
        let source = tempfile::NamedTempFile::new().expect("source file");
        tokio::fs::write(source.path(), b"not a zip")
            .await
            .expect("write source");

        let error = stage_archive(data_dir.path(), source.path())
            .await
            .expect_err("invalid archive must fail");

        assert!(matches!(error, SourceDropError::InvalidArchive { .. }));
        let staged = data_dir.path().join("source-bundles");
        let remaining = std::fs::read_dir(staged)
            .expect("staging directory")
            .collect::<Result<Vec<_>, _>>()
            .expect("read staging directory");
        assert!(
            remaining.is_empty(),
            "invalid staged archive must be removed"
        );
    }
}
