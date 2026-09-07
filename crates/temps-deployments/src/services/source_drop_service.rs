// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

use std::path::{Path, PathBuf};
use std::sync::Arc;

use async_trait::async_trait;
use chrono::Utc;
use sea_orm::{
    ActiveModelTrait, ColumnTrait, DatabaseConnection, EntityTrait, QueryFilter, QueryOrder, Set,
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

#[async_trait]
trait SourceDropWorkflowPlanner: Send + Sync {
    async fn plan(&self, deployment_id: i32) -> Result<(), SourceDropError>;
}

#[async_trait]
impl SourceDropWorkflowPlanner for WorkflowPlanner {
    async fn plan(&self, deployment_id: i32) -> Result<(), SourceDropError> {
        self.create_deployment_jobs(deployment_id)
            .await
            .map(|_| ())
            .map_err(|error| SourceDropError::Workflow {
                reason: error.to_string(),
            })
    }
}

async fn enqueue_source_drop(
    queue: &dyn JobQueue,
    job: DeploymentCreatedJob,
) -> Result<(), SourceDropError> {
    queue
        .send(Job::DeploymentCreated(job))
        .await
        .map_err(|error| SourceDropError::Queue {
            reason: error.to_string(),
        })
}

async fn rollback_source_drop(
    db: &DatabaseConnection,
    deployment_id: Option<i32>,
    bundle_id: Option<i32>,
    restore_manual_project_id: Option<i32>,
    archive_path: &Path,
) -> Result<(), String> {
    let transaction = db
        .begin()
        .await
        .map_err(|error| format!("could not begin database rollback: {error}"))?;
    if let Some(project_id) = restore_manual_project_id {
        projects::Entity::update_many()
            .col_expr(
                projects::Column::SourceType,
                sea_orm::sea_query::Expr::value(SourceType::Manual),
            )
            .filter(projects::Column::Id.eq(project_id))
            .exec(&transaction)
            .await
            .map_err(|error| {
                format!("could not restore project {project_id} to manual source: {error}")
            })?;
    }
    if let Some(deployment_id) = deployment_id {
        deployments::Entity::delete_by_id(deployment_id)
            .exec(&transaction)
            .await
            .map_err(|error| format!("could not delete deployment {deployment_id}: {error}"))?;
    }
    if let Some(bundle_id) = bundle_id {
        source_bundles::Entity::delete_by_id(bundle_id)
            .exec(&transaction)
            .await
            .map_err(|error| format!("could not delete source bundle {bundle_id}: {error}"))?;
    }
    transaction
        .commit()
        .await
        .map_err(|error| format!("could not commit database rollback: {error}"))?;
    match tokio::fs::remove_file(archive_path).await {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(format!(
            "database rollback completed but archive {} could not be removed: {error}",
            archive_path.display()
        )),
    }
}

async fn compensate_source_drop(
    db: &DatabaseConnection,
    original: SourceDropError,
    deployment_id: Option<i32>,
    bundle_id: Option<i32>,
    restore_manual_project_id: Option<i32>,
    archive_path: &Path,
) -> SourceDropError {
    match rollback_source_drop(
        db,
        deployment_id,
        bundle_id,
        restore_manual_project_id,
        archive_path,
    )
    .await
    {
        Ok(()) => original,
        Err(cleanup) => SourceDropError::Compensation {
            original: Box::new(original),
            cleanup,
        },
    }
}

/// Runs the server-side half of `temps drop` after a caller has prepared a ZIP.
/// Both the CLI upload handler and application chat can use this service, so
/// job planning, queueing, deployment gates, and rollback stay identical.
pub struct SourceDropService {
    db: Arc<DatabaseConnection>,
    data_dir: PathBuf,
    workflow_planner: Arc<dyn SourceDropWorkflowPlanner>,
    workflow_executor: Option<Arc<WorkflowExecutionService>>,
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
            workflow_executor: Some(workflow_executor),
            queue,
            deployment_gate,
        }
    }

    async fn compensate(
        &self,
        original: SourceDropError,
        deployment_id: Option<i32>,
        bundle_id: Option<i32>,
        restore_manual_project_id: Option<i32>,
        archive_path: &Path,
    ) -> SourceDropError {
        compensate_source_drop(
            self.db.as_ref(),
            original,
            deployment_id,
            bundle_id,
            restore_manual_project_id,
            archive_path,
        )
        .await
    }

    #[cfg(test)]
    fn for_test(
        db: Arc<DatabaseConnection>,
        data_dir: PathBuf,
        workflow_planner: Arc<dyn SourceDropWorkflowPlanner>,
        queue: Arc<dyn JobQueue>,
    ) -> Self {
        Self {
            db,
            data_dir,
            workflow_planner,
            workflow_executor: None,
            queue,
            deployment_gate: Arc::new(tokio::sync::RwLock::new(None)),
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
        let promote_manual_source =
            request.promote_manual_source && project.source_type == SourceType::Manual;
        if project.source_type != SourceType::UploadedSource
            && !project.allow_alternate_sources.unwrap_or(false)
            && !promote_manual_source
        {
            return Err(SourceDropError::SourceNotAllowed {
                project_id: project.id,
                reason: format!("its configured source is '{}'; enable alternate sources or use an uploaded-source project", project.source_type),
            });
        }
        let environment = match request.environment_id {
            Some(environment_id) => environments::Entity::find_by_id(environment_id)
                .filter(environments::Column::DeletedAt.is_null())
                .one(self.db.as_ref())
                .await
                .map_err(|error| SourceDropError::Database {
                    reason: error.to_string(),
                })?
                .filter(|environment| environment.project_id == project.id)
                .ok_or(SourceDropError::EnvironmentNotFound {
                    environment_id,
                    project_id: project.id,
                })?,
            None => environments::Entity::find()
                .filter(environments::Column::ProjectId.eq(project.id))
                .filter(environments::Column::DeletedAt.is_null())
                .order_by_desc(environments::Column::Name.eq("production"))
                .order_by_asc(environments::Column::CreatedAt)
                .one(self.db.as_ref())
                .await
                .map_err(|error| SourceDropError::Database {
                    reason: error.to_string(),
                })?
                .ok_or(SourceDropError::NoEnvironment {
                    project_id: project.id,
                })?,
        };
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
                let original = SourceDropError::Database {
                    reason: format!("could not register source bundle: {error}"),
                };
                return Err(self
                    .compensate(original, None, None, None, &absolute_path)
                    .await);
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
                let original = SourceDropError::Database { reason: format!("could not create deployment: {error}") };
                return Err(self.compensate(original, None, None, None, &absolute_path).await);
            }
        };
        if let Err(error) = transaction.commit().await {
            let original = SourceDropError::Database {
                reason: format!("could not commit source deployment: {error}"),
            };
            return Err(self
                .compensate(
                    original,
                    Some(deployment.id),
                    Some(bundle.id),
                    None,
                    &absolute_path,
                )
                .await);
        }
        if let Err(error) = self.workflow_planner.plan(deployment.id).await {
            return Err(self
                .compensate(
                    error,
                    Some(deployment.id),
                    Some(bundle.id),
                    None,
                    &absolute_path,
                )
                .await);
        }
        if promote_manual_source {
            let mut active: projects::ActiveModel = project.clone().into();
            active.source_type = Set(SourceType::UploadedSource);
            if let Err(error) = active.update(self.db.as_ref()).await {
                let original = SourceDropError::Database {
                    reason: format!(
                        "could not promote manual project {} after preparing its source deployment: {error}",
                        project.id
                    ),
                };
                return Err(self
                    .compensate(
                        original,
                        Some(deployment.id),
                        Some(bundle.id),
                        Some(project.id),
                        &absolute_path,
                    )
                    .await);
            }
        }
        if let Err(error) = enqueue_source_drop(
            self.queue.as_ref(),
            DeploymentCreatedJob {
                deployment_id: deployment.id,
                project_id: project.id,
                environment_id: environment.id,
                environment_name: environment.name.clone(),
                branch: None,
                commit_sha: None,
            },
        )
        .await
        {
            return Err(self
                .compensate(
                    error,
                    Some(deployment.id),
                    Some(bundle.id),
                    promote_manual_source.then_some(project.id),
                    &absolute_path,
                )
                .await);
        }
        let db = self.db.clone();
        if let Some(workflow_executor) = self.workflow_executor.clone() {
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
        }
        Ok(SourceDropDeployment {
            id: deployment.id,
            project_id: project.id,
            environment_id: environment.id,
            slug: deployment.slug,
            state: deployment.state,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use sea_orm::{DatabaseBackend, MockDatabase, MockExecResult, PaginatorTrait};
    use std::sync::atomic::{AtomicUsize, Ordering};
    use temps_core::{JobReceiver, QueueError};
    use temps_database::test_utils::TestDatabase;
    use temps_entities::{preset::Preset, upstream_config::UpstreamList};

    struct RecordingPlanner {
        calls: AtomicUsize,
        fail: bool,
    }

    #[async_trait]
    impl SourceDropWorkflowPlanner for RecordingPlanner {
        async fn plan(&self, deployment_id: i32) -> Result<(), SourceDropError> {
            assert!(deployment_id > 0);
            self.calls.fetch_add(1, Ordering::SeqCst);
            if self.fail {
                Err(SourceDropError::Workflow {
                    reason: "planner rejected deployment".to_string(),
                })
            } else {
                Ok(())
            }
        }
    }

    struct RecordingQueue {
        calls: AtomicUsize,
        fail: bool,
    }

    struct DatabaseObservingPlanner {
        db: Arc<DatabaseConnection>,
        project_id: i32,
        calls: AtomicUsize,
        fail: bool,
    }

    #[async_trait]
    impl SourceDropWorkflowPlanner for DatabaseObservingPlanner {
        async fn plan(&self, _deployment_id: i32) -> Result<(), SourceDropError> {
            let project = projects::Entity::find_by_id(self.project_id)
                .one(self.db.as_ref())
                .await
                .map_err(|error| SourceDropError::Database {
                    reason: error.to_string(),
                })?
                .ok_or(SourceDropError::ProjectNotFound {
                    project_id: self.project_id,
                })?;
            assert_eq!(
                project.source_type,
                SourceType::Manual,
                "workflow planning must finish before project source promotion"
            );
            self.calls.fetch_add(1, Ordering::SeqCst);
            if self.fail {
                Err(SourceDropError::Workflow {
                    reason: "planner rejected deployment".to_string(),
                })
            } else {
                Ok(())
            }
        }
    }

    struct DatabaseObservingQueue {
        db: Arc<DatabaseConnection>,
        project_id: i32,
        calls: AtomicUsize,
        fail: bool,
    }

    #[async_trait]
    impl JobQueue for DatabaseObservingQueue {
        async fn send(&self, job: Job) -> Result<(), QueueError> {
            let Job::DeploymentCreated(_) = job else {
                return Err(QueueError::SendError(
                    "source drop dispatched an unexpected job".to_string(),
                ));
            };
            let project = projects::Entity::find_by_id(self.project_id)
                .one(self.db.as_ref())
                .await
                .map_err(|error| QueueError::SendError(error.to_string()))?
                .ok_or_else(|| QueueError::SendError("project disappeared".to_string()))?;
            assert_eq!(
                project.source_type,
                SourceType::UploadedSource,
                "manual-source promotion must commit before queue dispatch"
            );
            self.calls.fetch_add(1, Ordering::SeqCst);
            if self.fail {
                Err(QueueError::SendError("queue unavailable".to_string()))
            } else {
                Ok(())
            }
        }

        fn subscribe(&self) -> Box<dyn JobReceiver> {
            unimplemented!("source-drop dispatch tests do not receive jobs")
        }
    }

    async fn source_drop_fixture(
        db: &DatabaseConnection,
        slug: &str,
    ) -> Result<(projects::Model, environments::Model), sea_orm::DbErr> {
        let now = Utc::now();
        let project = projects::ActiveModel {
            name: Set(format!("Source drop {slug}")),
            slug: Set(slug.to_string()),
            repo_owner: Set(String::new()),
            repo_name: Set(String::new()),
            main_branch: Set("main".to_string()),
            preset: Set(Preset::NodeJs),
            directory: Set(".".to_string()),
            source_type: Set(SourceType::Manual),
            allow_alternate_sources: Set(Some(false)),
            created_at: Set(now),
            updated_at: Set(now),
            ..Default::default()
        }
        .insert(db)
        .await?;
        let environment = environments::ActiveModel {
            project_id: Set(project.id),
            name: Set("production".to_string()),
            slug: Set(format!("{slug}-production")),
            subdomain: Set(format!("{slug}.example.test")),
            host: Set(format!("{slug}.example.test")),
            upstreams: Set(UpstreamList::default()),
            created_at: Set(now),
            updated_at: Set(now),
            ..Default::default()
        }
        .insert(db)
        .await?;
        Ok((project, environment))
    }

    fn valid_source_archive(directory: &Path) -> PathBuf {
        use std::io::Write as _;

        let path = directory.join("source.zip");
        let file = std::fs::File::create(&path).expect("create source archive");
        let mut archive = zip::ZipWriter::new(file);
        archive
            .start_file("index.js", zip::write::SimpleFileOptions::default())
            .expect("start source entry");
        archive
            .write_all(b"console.log('ok')")
            .expect("write source entry");
        archive.finish().expect("finish source archive");
        path
    }

    async fn source_drop_test_database() -> Result<Option<TestDatabase>, Box<dyn std::error::Error>>
    {
        match TestDatabase::with_migrations().await {
            Ok(database) => Ok(Some(database)),
            Err(error)
                if temps_database::test_utils::is_container_runtime_unavailable(
                    &error.to_string(),
                ) =>
            {
                eprintln!("Skipping Docker-dependent source-drop test: {error}");
                Ok(None)
            }
            Err(error) => Err(error.into()),
        }
    }

    async fn assert_source_drop_rolled_back(
        db: &DatabaseConnection,
        project_id: i32,
        data_dir: &Path,
    ) -> Result<(), sea_orm::DbErr> {
        let project = projects::Entity::find_by_id(project_id)
            .one(db)
            .await?
            .expect("project remains after source-drop rollback");
        assert_eq!(project.source_type, SourceType::Manual);
        assert_eq!(
            deployments::Entity::find()
                .filter(deployments::Column::ProjectId.eq(project_id))
                .count(db)
                .await?,
            0
        );
        assert_eq!(
            source_bundles::Entity::find()
                .filter(source_bundles::Column::ProjectId.eq(project_id))
                .count(db)
                .await?,
            0
        );
        let staged_dir = data_dir.join("source-bundles");
        let staged_files = std::fs::read_dir(staged_dir)
            .expect("source bundle directory")
            .collect::<Result<Vec<_>, _>>()
            .expect("list source bundles");
        assert!(staged_files.is_empty(), "staged archive must be removed");
        Ok(())
    }

    #[async_trait]
    impl JobQueue for RecordingQueue {
        async fn send(&self, job: Job) -> Result<(), QueueError> {
            let Job::DeploymentCreated(job) = job else {
                panic!("source drop must enqueue a deployment-created job");
            };
            assert_eq!(job.deployment_id, 42);
            self.calls.fetch_add(1, Ordering::SeqCst);
            if self.fail {
                Err(QueueError::SendError("queue unavailable".to_string()))
            } else {
                Ok(())
            }
        }

        fn subscribe(&self) -> Box<dyn JobReceiver> {
            unimplemented!("source-drop dispatch tests do not receive jobs")
        }
    }

    fn deployment_job() -> DeploymentCreatedJob {
        DeploymentCreatedJob {
            deployment_id: 42,
            project_id: 7,
            environment_id: 9,
            environment_name: "production".to_string(),
            branch: None,
            commit_sha: None,
        }
    }

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

    #[tokio::test]
    async fn stage_archive_accepts_valid_zip_and_records_exact_checksum() {
        use std::io::Write as _;

        let data_dir = tempfile::tempdir().expect("data directory");
        let source_dir = tempfile::tempdir().expect("source directory");
        let source_path = source_dir.path().join("source.zip");
        let source_file = std::fs::File::create(&source_path).expect("create archive");
        let mut archive = zip::ZipWriter::new(source_file);
        archive
            .start_file("index.html", zip::write::SimpleFileOptions::default())
            .expect("start zip entry");
        archive.write_all(b"hello").expect("write zip entry");
        archive.finish().expect("finish archive");
        let source_bytes = std::fs::read(&source_path).expect("read source archive");

        let (relative, staged, size, checksum) = stage_archive(data_dir.path(), &source_path)
            .await
            .expect("valid archive must stage");

        assert!(relative.starts_with("source-bundles/"));
        assert_eq!(size, source_bytes.len() as u64);
        assert_eq!(
            checksum,
            format!("sha256:{}", hex::encode(Sha256::digest(&source_bytes)))
        );
        assert_eq!(
            tokio::fs::read(staged).await.expect("read staged archive"),
            source_bytes
        );
    }

    #[tokio::test]
    async fn planner_and_queue_failures_remain_typed_and_observable() {
        let failing_planner = RecordingPlanner {
            calls: AtomicUsize::new(0),
            fail: true,
        };
        let planner_error = failing_planner
            .plan(42)
            .await
            .expect_err("planner failure must be returned");
        assert!(matches!(planner_error, SourceDropError::Workflow { .. }));
        assert_eq!(failing_planner.calls.load(Ordering::SeqCst), 1);

        let failing_queue = RecordingQueue {
            calls: AtomicUsize::new(0),
            fail: true,
        };
        let queue_error = enqueue_source_drop(&failing_queue, deployment_job())
            .await
            .expect_err("queue failure must be returned");
        assert!(matches!(queue_error, SourceDropError::Queue { .. }));
        assert_eq!(failing_queue.calls.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn successful_dispatch_invokes_planner_and_queue_once() {
        let planner = RecordingPlanner {
            calls: AtomicUsize::new(0),
            fail: false,
        };
        let queue = RecordingQueue {
            calls: AtomicUsize::new(0),
            fail: false,
        };

        planner.plan(42).await.expect("planner succeeds");
        enqueue_source_drop(&queue, deployment_job())
            .await
            .expect("queue succeeds");

        assert_eq!(planner.calls.load(Ordering::SeqCst), 1);
        assert_eq!(queue.calls.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn deploy_source_drop_plans_promotes_then_queues(
    ) -> Result<(), Box<dyn std::error::Error>> {
        let Some(test_db) = source_drop_test_database().await? else {
            return Ok(());
        };
        let db = test_db.connection_arc();
        let (project, environment) = source_drop_fixture(db.as_ref(), "drop-success").await?;
        let data_dir = tempfile::tempdir()?;
        let source_dir = tempfile::tempdir()?;
        let archive_path = valid_source_archive(source_dir.path());
        let planner = Arc::new(DatabaseObservingPlanner {
            db: db.clone(),
            project_id: project.id,
            calls: AtomicUsize::new(0),
            fail: false,
        });
        let queue = Arc::new(DatabaseObservingQueue {
            db: db.clone(),
            project_id: project.id,
            calls: AtomicUsize::new(0),
            fail: false,
        });
        let service = SourceDropService::for_test(
            db.clone(),
            data_dir.path().to_path_buf(),
            planner.clone(),
            queue.clone(),
        );

        let deployed = service
            .deploy_source_drop(SourceDropRequest {
                project_id: project.id,
                environment_id: Some(environment.id),
                archive_path,
                original_filename: "source.zip".to_string(),
                promote_manual_source: true,
            })
            .await?;

        assert_eq!(deployed.project_id, project.id);
        assert_eq!(deployed.environment_id, environment.id);
        assert_eq!(planner.calls.load(Ordering::SeqCst), 1);
        assert_eq!(queue.calls.load(Ordering::SeqCst), 1);
        let promoted = projects::Entity::find_by_id(project.id)
            .one(db.as_ref())
            .await?
            .expect("project exists");
        assert_eq!(promoted.source_type, SourceType::UploadedSource);
        Ok(())
    }

    #[tokio::test]
    async fn deploy_source_drop_planner_failure_removes_all_staged_state(
    ) -> Result<(), Box<dyn std::error::Error>> {
        let Some(test_db) = source_drop_test_database().await? else {
            return Ok(());
        };
        let db = test_db.connection_arc();
        let (project, environment) = source_drop_fixture(db.as_ref(), "drop-plan-fail").await?;
        let data_dir = tempfile::tempdir()?;
        let source_dir = tempfile::tempdir()?;
        let planner = Arc::new(DatabaseObservingPlanner {
            db: db.clone(),
            project_id: project.id,
            calls: AtomicUsize::new(0),
            fail: true,
        });
        let queue = Arc::new(DatabaseObservingQueue {
            db: db.clone(),
            project_id: project.id,
            calls: AtomicUsize::new(0),
            fail: false,
        });
        let service = SourceDropService::for_test(
            db.clone(),
            data_dir.path().to_path_buf(),
            planner.clone(),
            queue.clone(),
        );

        let error = service
            .deploy_source_drop(SourceDropRequest {
                project_id: project.id,
                environment_id: Some(environment.id),
                archive_path: valid_source_archive(source_dir.path()),
                original_filename: "source.zip".to_string(),
                promote_manual_source: true,
            })
            .await
            .expect_err("planner failure must fail the full service operation");

        assert!(matches!(error, SourceDropError::Workflow { .. }));
        assert_eq!(planner.calls.load(Ordering::SeqCst), 1);
        assert_eq!(queue.calls.load(Ordering::SeqCst), 0);
        assert_source_drop_rolled_back(db.as_ref(), project.id, data_dir.path()).await?;
        Ok(())
    }

    #[tokio::test]
    async fn deploy_source_drop_queue_failure_restores_manual_project(
    ) -> Result<(), Box<dyn std::error::Error>> {
        let Some(test_db) = source_drop_test_database().await? else {
            return Ok(());
        };
        let db = test_db.connection_arc();
        let (project, environment) = source_drop_fixture(db.as_ref(), "drop-queue-fail").await?;
        let data_dir = tempfile::tempdir()?;
        let source_dir = tempfile::tempdir()?;
        let planner = Arc::new(DatabaseObservingPlanner {
            db: db.clone(),
            project_id: project.id,
            calls: AtomicUsize::new(0),
            fail: false,
        });
        let queue = Arc::new(DatabaseObservingQueue {
            db: db.clone(),
            project_id: project.id,
            calls: AtomicUsize::new(0),
            fail: true,
        });
        let service = SourceDropService::for_test(
            db.clone(),
            data_dir.path().to_path_buf(),
            planner.clone(),
            queue.clone(),
        );

        let error = service
            .deploy_source_drop(SourceDropRequest {
                project_id: project.id,
                environment_id: Some(environment.id),
                archive_path: valid_source_archive(source_dir.path()),
                original_filename: "source.zip".to_string(),
                promote_manual_source: true,
            })
            .await
            .expect_err("queue failure must fail the full service operation");

        assert!(matches!(error, SourceDropError::Queue { .. }));
        assert_eq!(planner.calls.load(Ordering::SeqCst), 1);
        assert_eq!(queue.calls.load(Ordering::SeqCst), 1);
        assert_source_drop_rolled_back(db.as_ref(), project.id, data_dir.path()).await?;
        Ok(())
    }

    #[tokio::test]
    async fn compensation_deletes_records_and_staged_archive() {
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_exec_results([
                MockExecResult {
                    last_insert_id: 0,
                    rows_affected: 1,
                },
                MockExecResult {
                    last_insert_id: 0,
                    rows_affected: 1,
                },
            ])
            .into_connection();
        let staged = tempfile::NamedTempFile::new().expect("staged archive");
        let staged_path = staged.path().to_path_buf();
        drop(staged);
        tokio::fs::write(&staged_path, b"archive")
            .await
            .expect("write staged archive");

        rollback_source_drop(&db, Some(42), Some(24), None, &staged_path)
            .await
            .expect("compensation succeeds");

        assert!(!staged_path.exists(), "staged archive must be removed");
        let log = format!("{:?}", db.into_transaction_log());
        assert!(log.contains("deployments"), "{log}");
        assert!(log.contains("source_bundles"), "{log}");
    }

    #[tokio::test]
    async fn compensation_failure_preserves_original_and_cleanup_context() {
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_exec_errors([sea_orm::DbErr::Custom(
                "rollback database unavailable".to_string(),
            )])
            .into_connection();
        let staged = tempfile::NamedTempFile::new().expect("staged archive");

        let error = compensate_source_drop(
            &db,
            SourceDropError::Queue {
                reason: "queue unavailable".to_string(),
            },
            Some(42),
            Some(24),
            Some(7),
            staged.path(),
        )
        .await;

        let SourceDropError::Compensation { original, cleanup } = error else {
            panic!("cleanup failure must return a typed compensation error");
        };
        assert!(matches!(*original, SourceDropError::Queue { .. }));
        assert!(
            cleanup.contains("rollback database unavailable"),
            "{cleanup}"
        );
        assert!(
            staged.path().exists(),
            "archive remains available for retry"
        );
    }
}
