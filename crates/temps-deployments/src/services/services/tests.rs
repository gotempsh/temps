// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

use super::cleanup::{recorded_container_ownership, RecordedContainerOwnership};
use super::redeploy::check_redeploy_plan_batch;
use super::reuse::deployment_asset_origin;
use super::*;

use chrono::Utc;
use mockall::mock;
use sea_orm::{ActiveModelTrait, EntityTrait, Set};

use std::sync::Arc;
use temps_core::EncryptionService;

struct NoopImageBuilder;

#[async_trait::async_trait]
impl temps_deployer::ImageBuilder for NoopImageBuilder {
    async fn build_image(
        &self,
        request: temps_deployer::BuildRequest,
    ) -> Result<temps_deployer::BuildResult, temps_deployer::BuilderError> {
        Ok(temps_deployer::BuildResult {
            image_id: "sha256:test".to_string(),
            image_name: request.image_name,
            size_bytes: 0,
            build_duration_ms: 0,
        })
    }

    async fn build_image_with_callback(
        &self,
        request: temps_deployer::BuildRequestWithCallback,
    ) -> Result<temps_deployer::BuildResult, temps_deployer::BuilderError> {
        self.build_image(request.request).await
    }

    async fn import_image(
        &self,
        _image_path: std::path::PathBuf,
        _tag: &str,
    ) -> Result<String, temps_deployer::BuilderError> {
        Ok("sha256:test".to_string())
    }

    async fn save_image(
        &self,
        _image_name: &str,
        _output_path: &std::path::Path,
    ) -> Result<(), temps_deployer::BuilderError> {
        Ok(())
    }

    async fn extract_from_image(
        &self,
        _image_name: &str,
        _source_path: &str,
        _destination_path: &std::path::Path,
    ) -> Result<(), temps_deployer::BuilderError> {
        Ok(())
    }

    async fn list_images(&self) -> Result<Vec<String>, temps_deployer::BuilderError> {
        Ok(Vec::new())
    }

    async fn remove_image(&self, _image_name: &str) -> Result<(), temps_deployer::BuilderError> {
        Ok(())
    }

    async fn inspect_image(
        &self,
        image_name: &str,
    ) -> Result<temps_deployer::ImageInfo, temps_deployer::BuilderError> {
        Ok(temps_deployer::ImageInfo {
            id: "sha256:test".to_string(),
            architecture: "amd64".to_string(),
            os: "linux".to_string(),
            platform: "linux/amd64".to_string(),
            size_bytes: 0,
            tags: vec![image_name.to_string()],
            created: None,
            working_dir: None,
            config: None,
        })
    }

    fn get_native_platform(&self) -> String {
        "linux/amd64".to_string()
    }
}

#[test]
fn record_not_inserted_is_not_treated_as_a_unique_violation() {
    // This insert never uses `on_conflict().do_nothing()`, so a bare
    // `RecordNotInserted` cannot be produced by the race this check
    // exists to catch, and must not be misclassified as one.
    assert!(!is_unique_violation(&sea_orm::DbErr::RecordNotInserted));
}

#[test]
fn unrelated_database_errors_are_not_treated_as_a_unique_violation() {
    assert!(!is_unique_violation(&sea_orm::DbErr::Custom(
        "connection reset by peer".to_string()
    )));
}

use temps_database::test_utils::TestDatabase;
use temps_entities::{
    deployment_config::DeploymentConfig, deployments, env_vars, environments, preset::Preset,
    projects, upstream_config::UpstreamList,
};

// Mock for other services
mock! {
    LogService {}
}

mock! {
    ConfigService {}
}

mock! {
    QueueService {}
    #[async_trait::async_trait]
    impl temps_core::JobQueue for QueueService {
        async fn send(&self, job: temps_core::Job) -> Result<(), temps_core::QueueError>;
        fn subscribe(&self) -> Box<dyn temps_core::JobReceiver>;
    }
}

mock! {
    DockerLogService {}
}

mock! {
    JobReceiver {}
    #[async_trait::async_trait]
    impl temps_core::JobReceiver for JobReceiver {
        async fn recv(&mut self) -> Result<temps_core::Job, temps_core::QueueError>;
    }
}

mock! {
    ContainerDeployer {}
    #[async_trait::async_trait]
    impl temps_deployer::ContainerDeployer for ContainerDeployer {
        async fn exec_in_container(&self, container_id: &str, command: Vec<String>, timeout: std::time::Duration) -> Result<temps_deployer::ExecResult, temps_deployer::DeployerError>;
        async fn deploy_container(&self, request: temps_deployer::DeployRequest) -> Result<temps_deployer::DeployResult, temps_deployer::DeployerError>;
        async fn start_container(&self, container_id: &str) -> Result<(), temps_deployer::DeployerError>;
        async fn stop_container(&self, container_id: &str) -> Result<(), temps_deployer::DeployerError>;
        async fn pause_container(&self, container_id: &str) -> Result<(), temps_deployer::DeployerError>;
        async fn resume_container(&self, container_id: &str) -> Result<(), temps_deployer::DeployerError>;
        async fn remove_container(&self, container_id: &str) -> Result<(), temps_deployer::DeployerError>;
        async fn get_container_info(&self, container_id: &str) -> Result<temps_deployer::ContainerInfo, temps_deployer::DeployerError>;
        async fn get_container_stats(&self, container_id: &str) -> Result<temps_deployer::ContainerStats, temps_deployer::DeployerError>;
        async fn list_containers(&self) -> Result<Vec<temps_deployer::ContainerInfo>, temps_deployer::DeployerError>;
        async fn get_container_logs(&self, container_id: &str) -> Result<String, temps_deployer::DeployerError>;
        async fn stream_container_logs(&self, container_id: &str) -> Result<Box<dyn futures::Stream<Item = String> + Unpin + Send>, temps_deployer::DeployerError>;
        async fn image_exists(&self, image_name: &str) -> Result<bool, temps_deployer::DeployerError>;
    }
}
fn create_test_external_service_manager(
    db: Arc<temps_database::DbConnection>,
) -> Arc<temps_providers::ExternalServiceManager> {
    let encryption_service = create_test_encryption_service();
    let docker = Arc::new(bollard::Docker::connect_with_local_defaults().ok().unwrap());
    let dns_registry = Arc::new(temps_providers::DnsRegistry::new(db.clone()));
    Arc::new(temps_providers::ExternalServiceManager::new(
        db,
        encryption_service,
        docker,
        dns_registry,
    ))
}

fn create_test_encryption_service() -> Arc<EncryptionService> {
    Arc::new(
        EncryptionService::new("0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef")
            .unwrap(),
    )
}

async fn setup_test_data(
    db: &Arc<temps_database::DbConnection>,
) -> Result<(projects::Model, environments::Model, deployments::Model), Box<dyn std::error::Error>>
{
    // Create test project
    let project = projects::ActiveModel {
        name: Set("Test Project".to_string()),
        slug: Set("test-project".to_string()),
        repo_owner: Set("test-owner".to_string()),
        repo_name: Set("test-repo".to_string()),
        main_branch: Set("main".to_string()),
        git_provider_connection_id: Set(Some(1)),
        preset: Set(Preset::NextJs),
        directory: Set("/".to_string()),
        created_at: Set(Utc::now()),
        updated_at: Set(Utc::now()),
        deleted_at: Set(None),
        is_deleted: Set(false),
        deployment_config: Set(Some(DeploymentConfig::default())),
        last_deployment: Set(None),
        ..Default::default()
    };
    let project = project.insert(db.as_ref()).await?;

    // Create test environment
    let environment = environments::ActiveModel {
        project_id: Set(project.id),
        name: Set("Test Environment".to_string()),
        slug: Set("test".to_string()),
        host: Set("test.example.com".to_string()), // Add required host field
        upstreams: Set(UpstreamList::default()),   // Add required upstreams field (empty array)
        current_deployment_id: Set(None),
        subdomain: Set("test.example.com".to_string()),
        created_at: Set(Utc::now()),
        updated_at: Set(Utc::now()),
        ..Default::default()
    };
    let environment = environment.insert(db.as_ref()).await?;

    // Create test deployment
    let deployment = deployments::ActiveModel {
        project_id: Set(project.id),
        environment_id: Set(environment.id),
        slug: Set("test-deployment-123".to_string()),
        state: Set("deployed".to_string()),
        metadata: Set(Some(
            temps_entities::deployments::DeploymentMetadata::default(),
        )),
        image_name: Set(Some("nginx:latest".to_string())),
        created_at: Set(Utc::now()),
        updated_at: Set(Utc::now()),
        ..Default::default()
    };
    let deployment = deployment.insert(db.as_ref()).await?;

    Ok((project, environment, deployment))
}

/// ADR 045: proves `DeployImageJob::persist_docker_socket_mounted`'s
/// write is visible through the *exact* read path exec authorization
/// uses -- `DeploymentService::deployment_docker_socket_mounted` --
/// against a real database, and that it does not disturb a sibling
/// deployment's row. `guard_exec_against`'s own tests exercise the
/// boolean as a hand-supplied literal, which would keep passing even if
/// this write silently stopped happening; this is the test that would
/// actually fail in that case.
#[tokio::test]
async fn persist_docker_socket_mounted_is_visible_through_the_deployment_service() {
    let test_db = match TestDatabase::with_migrations().await {
        Ok(db) => db,
        Err(error)
            if temps_database::test_utils::is_container_runtime_unavailable(&error.to_string()) =>
        {
            println!("Docker unavailable; skipping: {error}");
            return;
        }
        Err(error) => panic!("failed to prepare test database: {error}"),
    };
    let db = test_db.connection_arc();
    let (project, environment, deployment) = setup_test_data(&db)
        .await
        .expect("create deployment fixtures");
    // A second deployment of the *same* project/environment -- the write
    // must target only the row it was asked to persist, not every
    // deployment of that project.
    let sibling_deployment = deployments::ActiveModel {
        project_id: Set(project.id),
        environment_id: Set(environment.id),
        slug: Set("test-deployment-sibling".to_string()),
        state: Set("deployed".to_string()),
        metadata: Set(Some(
            temps_entities::deployments::DeploymentMetadata::default(),
        )),
        created_at: Set(Utc::now()),
        updated_at: Set(Utc::now()),
        ..Default::default()
    }
    .insert(db.as_ref())
    .await
    .expect("insert sibling deployment");

    let deployment_service = create_deployment_service_for_test(db.clone());
    assert!(
        !deployment_service
            .deployment_docker_socket_mounted(deployment.id)
            .await
            .expect("read freshly-created deployment"),
        "a freshly-created deployment must not already read as socket-mounted"
    );

    let job = crate::jobs::DeployImageJobBuilder::new(
        project.slug.clone(),
        temps_core::docker_socket_grant::DeployCaller::Platform,
    )
    .job_id("deploy".to_string())
    .build_job_id("build".to_string())
    .target(crate::jobs::DeploymentTarget::Docker {
        registry_url: "local".to_string(),
        network: None,
    })
    .service_name(project.slug.clone())
    .failed_container_retention(db.clone(), deployment.id)
    .build(Arc::new(MockContainerDeployer::new()))
    .expect("valid deploy job");
    let context = temps_core::WorkflowContext::new(
        "run-persist".to_string(),
        deployment.id,
        project.id,
        environment.id,
        Arc::new(NoopLogWriter),
    );

    job.persist_docker_socket_mounted(&context).await;

    assert!(
        deployment_service
            .deployment_docker_socket_mounted(deployment.id)
            .await
            .expect("read the deployment after the write"),
        "the write must be visible through the read path exec authorization uses"
    );
    assert!(
        !deployment_service
            .deployment_docker_socket_mounted(sibling_deployment.id)
            .await
            .expect("read the sibling deployment"),
        "persisting one deployment's mount must not affect a sibling deployment's row"
    );
}

/// The hosting check used by rollback and promotion: it lives in the
/// service so the handlers never read the project row themselves.
#[tokio::test]
async fn externally_hosted_projects_are_refused_before_a_deployment_is_created() {
    let test_db = match TestDatabase::with_migrations().await {
        Ok(db) => db,
        Err(error)
            if temps_database::test_utils::is_container_runtime_unavailable(&error.to_string()) =>
        {
            println!("Docker unavailable; skipping: {error}");
            return;
        }
        Err(error) => panic!("failed to prepare test database: {error}"),
    };
    let db = test_db.connection_arc();
    let (project, _environment, _deployment) = setup_test_data(&db)
        .await
        .expect("create deployment fixtures");
    let service = create_deployment_service_for_test(db.clone());

    assert!(
        service
            .ensure_project_hosted_locally(project.id)
            .await
            .is_ok(),
        "a hosted project may be deployed"
    );
    // A missing project is left for the operation itself to report.
    assert!(service
        .ensure_project_hosted_locally(project.id + 10_000)
        .await
        .is_ok());

    let mut external: projects::ActiveModel = project.clone().into();
    external.hosting = Set(temps_entities::types::Hosting::External);
    external
        .update(db.as_ref())
        .await
        .expect("mark the project external");

    match service.ensure_project_hosted_locally(project.id).await {
        Err(DeploymentError::ProjectHostedExternally {
            project_id,
            project_name,
            reason,
        }) => {
            assert_eq!(project_id, project.id);
            assert_eq!(project_name, project.name);
            assert!(reason.contains(&project.name), "{reason}");
        }
        other => panic!("expected ProjectHostedExternally, got {other:?}"),
    }
}

struct NoopLogWriter;

#[async_trait::async_trait]
impl temps_core::LogWriter for NoopLogWriter {
    async fn write_log(&self, _message: String) -> Result<(), temps_core::WorkflowError> {
        Ok(())
    }

    fn stage_id(&self) -> i32 {
        1
    }
}

#[tokio::test]
async fn legacy_asset_origin_walks_partial_reuse_metadata_to_original_build() {
    let test_db = match TestDatabase::with_migrations().await {
        Ok(db) => db,
        Err(error) => {
            println!("Test database not available, skipping: {error}");
            return;
        }
    };
    let db = test_db.connection_arc().clone();
    let (project, environment, original) = setup_test_data(&db)
        .await
        .expect("create deployment fixtures");

    let first_reuse = deployments::ActiveModel {
        project_id: Set(project.id),
        environment_id: Set(environment.id),
        slug: Set("legacy-promotion".to_string()),
        state: Set("deployed".to_string()),
        metadata: Set(Some(
            temps_entities::deployments::DeploymentMetadata::default(),
        )),
        context_vars: Set(Some(serde_json::json!({
            "trigger": "promotion",
            "source_deployment_id": original.id,
            "source_environment_id": original.environment_id,
        }))),
        created_at: Set(Utc::now()),
        updated_at: Set(Utc::now()),
        ..Default::default()
    }
    .insert(db.as_ref())
    .await
    .expect("insert legacy promotion");

    let second_reuse = deployments::ActiveModel {
        project_id: Set(project.id),
        environment_id: Set(environment.id),
        slug: Set("legacy-rollback".to_string()),
        state: Set("deployed".to_string()),
        metadata: Set(Some(
            temps_entities::deployments::DeploymentMetadata::default(),
        )),
        context_vars: Set(Some(serde_json::json!({
            "trigger": "rollback",
            "source_deployment_id": first_reuse.id,
        }))),
        created_at: Set(Utc::now()),
        updated_at: Set(Utc::now()),
        ..Default::default()
    }
    .insert(db.as_ref())
    .await
    .expect("insert legacy rollback");

    let origin = deployment_asset_origin(db.as_ref(), &second_reuse)
        .await
        .expect("legacy reuse metadata should resolve");

    assert_eq!(origin.deployment_id, original.id);
    assert_eq!(origin.environment_id, original.environment_id);
    assert_eq!(origin.slug, original.slug);
}

#[tokio::test]
async fn latest_deployment_media_keeps_current_media_and_reports_latest_attempt() {
    let test_db = match TestDatabase::with_migrations().await {
        Ok(db) => db,
        Err(error) => {
            println!("Test database not available, skipping: {error}");
            return;
        }
    };
    let db = test_db.connection_arc().clone();
    let (project, environment, old_deployment) = setup_test_data(&db)
        .await
        .expect("create deployment fixtures");
    let service = create_deployment_service_for_test(db.clone());
    let status_without_media = service
        .get_latest_deployment_media(&[project.id])
        .await
        .expect("query a latest attempt without current or historical media");
    assert_eq!(status_without_media.len(), 1);
    assert_eq!(status_without_media[0].latest_attempt_status, "deployed");
    assert_eq!(status_without_media[0].url, None);
    assert_eq!(status_without_media[0].screenshot_location, None);

    let old_screenshot = "screenshots/old.webp".to_string();
    let mut old: deployments::ActiveModel = old_deployment.into();
    old.screenshot_location = Set(Some(old_screenshot));
    old.created_at = Set(Utc::now() - chrono::Duration::hours(1));
    old.update(db.as_ref())
        .await
        .expect("update old deployment");

    let newest_slug = "newest-deployment".to_string();
    let newest_screenshot = "screenshots/newest.webp".to_string();
    let newest = deployments::ActiveModel {
        project_id: Set(project.id),
        environment_id: Set(environment.id),
        slug: Set(newest_slug.clone()),
        state: Set("completed".to_string()),
        screenshot_location: Set(Some(newest_screenshot.clone())),
        metadata: Set(Some(
            temps_entities::deployments::DeploymentMetadata::default(),
        )),
        created_at: Set(Utc::now()),
        updated_at: Set(Utc::now()),
        ..Default::default()
    }
    .insert(db.as_ref())
    .await
    .expect("insert newest deployment");
    let mut environment: environments::ActiveModel = environment.into();
    environment.current_deployment_id = Set(Some(newest.id));
    let environment = environment
        .update(db.as_ref())
        .await
        .expect("set current deployment");

    let media = service
        .get_latest_deployment_media(&[project.id, i32::MAX])
        .await
        .expect("query latest deployment media");

    assert_eq!(media.len(), 1);
    assert_eq!(media[0].project_id, project.id);
    assert_eq!(media[0].latest_attempt_status, "completed");
    assert_eq!(
        media[0].screenshot_location.as_deref(),
        Some(newest_screenshot.as_str())
    );
    assert!(media[0]
        .url
        .as_deref()
        .is_some_and(|url| url.contains(&newest_slug)));

    let failed_attempt = deployments::ActiveModel {
        project_id: Set(project.id),
        environment_id: Set(environment.id),
        slug: Set("failed-attempt".to_string()),
        state: Set("failed".to_string()),
        screenshot_location: Set(None),
        metadata: Set(Some(
            temps_entities::deployments::DeploymentMetadata::default(),
        )),
        created_at: Set(Utc::now() + chrono::Duration::seconds(1)),
        updated_at: Set(Utc::now() + chrono::Duration::seconds(1)),
        ..Default::default()
    }
    .insert(db.as_ref())
    .await
    .expect("insert failed latest attempt");
    let media_after_failure = service
        .get_latest_deployment_media(&[project.id])
        .await
        .expect("query current media after a failed attempt");
    assert_eq!(media_after_failure[0].latest_attempt_status, "failed");
    assert_eq!(
        media_after_failure[0].screenshot_location.as_deref(),
        Some(newest_screenshot.as_str())
    );
    assert!(media_after_failure[0]
        .url
        .as_deref()
        .is_some_and(|url| url.contains(&newest_slug)));
    assert_ne!(failed_attempt.id, newest.id);

    let mut environment: environments::ActiveModel = environment.into();
    environment.current_deployment_id = Set(None);
    environment
        .update(db.as_ref())
        .await
        .expect("clear current deployment");
    let historical_media = service
        .get_latest_deployment_media(&[project.id])
        .await
        .expect("query historical screenshot fallback");
    assert_eq!(historical_media.len(), 1);
    assert_eq!(historical_media[0].latest_attempt_status, "failed");
    assert_eq!(historical_media[0].url, None);
    assert_eq!(
        historical_media[0].screenshot_location.as_deref(),
        Some(newest_screenshot.as_str())
    );
}

async fn setup_test_environment_variables(
    db: &Arc<temps_database::DbConnection>,
    project_id: i32,
    environment_id: i32,
) -> Result<(), Box<dyn std::error::Error>> {
    // Create project-level environment variables
    let project_env = env_vars::ActiveModel {
        project_id: Set(project_id),
        environment_id: Set(None),
        key: Set("PROJECT_VAR".to_string()),
        value: Set("project_value".to_string()),
        created_at: Set(Utc::now()),
        updated_at: Set(Utc::now()),
        ..Default::default()
    };
    project_env.insert(db.as_ref()).await?;

    // Create environment-specific environment variables
    let env_specific = env_vars::ActiveModel {
        project_id: Set(project_id),
        environment_id: Set(Some(environment_id)),
        key: Set("ENV_VAR".to_string()),
        value: Set("env_value".to_string()),
        created_at: Set(Utc::now()),
        updated_at: Set(Utc::now()),
        ..Default::default()
    };
    env_specific.insert(db.as_ref()).await?;

    // Override project var at environment level
    let env_override = env_vars::ActiveModel {
        project_id: Set(project_id),
        environment_id: Set(Some(environment_id)),
        key: Set("PROJECT_VAR".to_string()),
        value: Set("overridden_value".to_string()),
        created_at: Set(Utc::now()),
        updated_at: Set(Utc::now()),
        ..Default::default()
    };
    env_override.insert(db.as_ref()).await?;

    Ok(())
}

fn spawn_test_readiness_proxy() -> String {
    let listener =
        std::net::TcpListener::bind("127.0.0.1:0").expect("bind rollback readiness proxy");
    listener
        .set_nonblocking(true)
        .expect("make rollback readiness proxy nonblocking");
    let address = listener.local_addr().expect("read readiness proxy address");
    let listener =
        tokio::net::TcpListener::from_std(listener).expect("create async rollback readiness proxy");
    tokio::spawn(async move {
        loop {
            let Ok((mut stream, _)) = listener.accept().await else {
                break;
            };
            tokio::spawn(async move {
                use tokio::io::{AsyncReadExt, AsyncWriteExt};
                let mut request = [0_u8; 1024];
                let _ = stream.read(&mut request).await;
                let _ = stream
                    .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 0\r\nConnection: close\r\n\r\n")
                    .await;
            });
        }
    });
    address.to_string()
}

fn create_deployment_service_for_test(db: Arc<temps_database::DbConnection>) -> DeploymentService {
    // Create mock log service
    let log_service = Arc::new(temps_deployment_logs::LogService::new(std::env::temp_dir()));

    // Create a minimal real config service for testing
    // We need to provide the database URL that the test database is using
    let test_db_url = "postgresql://test_user:test_password@localhost:5432/test_db";
    let proxy_address = spawn_test_readiness_proxy();
    let readiness_host_port = proxy_address
        .rsplit_once(':')
        .and_then(|(_, port)| port.parse::<u16>().ok())
        .expect("read test readiness proxy port");
    let server_config = Arc::new(
        temps_config::ServerConfig::new(
            proxy_address,
            test_db_url.to_string(),
            None,
            Some("127.0.0.1:3001".to_string()),
        )
        .expect("Failed to create test server config"),
    );
    let config_service = Arc::new(temps_config::ConfigService::new(server_config, db.clone()));

    // Use a real broadcast queue so that mark_complete's route-ready
    // wait can be satisfied. We spawn a background task that listens for
    // any job on the queue and automatically responds with a
    // RouteTableUpdated event, simulating what the PG route listener does
    // in production.
    let (queue_service, _keep_alive) =
        temps_queue::BroadcastQueueService::create_job_queue_arc_with_receiver(64);
    {
        let queue_for_auto_responder = queue_service.clone();
        let mut auto_rx = queue_service.subscribe();
        tokio::spawn(async move {
            loop {
                match auto_rx.recv().await {
                    Ok(temps_core::Job::RouteTableUpdated(_)) => {
                        // Don't echo RouteTableUpdated events (avoid infinite loop)
                    }
                    Ok(_job) => {
                        // Ignore other jobs — the route table update is
                        // triggered by the DB PG trigger, not by queue
                        // events. We just need to keep this receiver alive.
                    }
                    Err(_) => break,
                }
            }
            drop(queue_for_auto_responder);
        });
    }
    // Spawn a second task that periodically sends RouteTableUpdated for
    // any deployment currently going through mark_complete. Since we don't
    // know the exact IDs, we listen for the `current_deployment_id` DB
    // change. In tests, instead we just send a broadly-matching event
    // after a short delay.
    //
    // In practice, for integration tests the simplest approach is to have
    // `wait_for_route_ready` accept an environment_id of None as a
    // wildcard, but that would weaken production safety. Instead, we
    // directly send the right event from a monitoring task on the DB.
    //
    // For unit tests: we use a simpler approach — we send the
    // RouteTableUpdated from a DB-watching perspective. Since tests use
    // real DB, we poll the environments table for current_deployment_id
    // changes and then send the corresponding RouteTableUpdated.
    {
        let queue_for_watcher = queue_service.clone();
        let db_for_watcher = db.clone();
        tokio::spawn(async move {
            use sea_orm::{ColumnTrait, EntityTrait, QueryFilter};
            // Poll every 50ms for environments with a current_deployment_id
            // that doesn't have a matching completed deployment yet.
            // This simulates the PG route listener.
            let mut seen: std::collections::HashSet<(i32, i32)> = std::collections::HashSet::new();
            loop {
                tokio::time::sleep(std::time::Duration::from_millis(50)).await;

                let envs = match temps_entities::environments::Entity::find()
                    .filter(temps_entities::environments::Column::CurrentDeploymentId.is_not_null())
                    .all(db_for_watcher.as_ref())
                    .await
                {
                    Ok(envs) => envs,
                    Err(_) => continue,
                };

                for env in envs {
                    if let Some(dep_id) = env.current_deployment_id {
                        if seen.insert((env.id, dep_id)) {
                            let _ = queue_for_watcher
                                .send(temps_core::Job::RouteTableUpdated(
                                    temps_core::RouteTableUpdatedJob {
                                        environment_id: Some(env.id),
                                        deployment_id: Some(dep_id),
                                        route_count: 1,
                                    },
                                ))
                                .await;
                        }
                    }
                }
            }
        });
    }

    // Create real docker log service for testing
    // For tests, we'll create a basic Docker connection (may fail but that's OK for tests)
    let docker = Arc::new(bollard::Docker::connect_with_local_defaults().unwrap());
    let docker_handle = Arc::new(temps_core::DockerHandle::available(docker.clone()));
    let docker_log_service = Arc::new(temps_deployment_logs::DockerLogService::new(
        docker_handle.clone(),
    ));

    // Create mock deployer with all required methods
    let mut deployer = MockContainerDeployer::new();
    deployer.expect_deploy_container().returning(move |_| {
        Ok(temps_deployer::DeployResult {
            container_id: "test-container".to_string(),
            container_name: "test-container".to_string(),
            container_port: 3000,
            host_port: readiness_host_port,
            status: temps_deployer::ContainerStatus::Running,
            docker_socket_mounted: false,
        })
    });
    deployer.expect_start_container().returning(|_| Ok(()));
    deployer.expect_stop_container().returning(|_| Ok(()));
    deployer.expect_pause_container().returning(|_| Ok(()));
    deployer.expect_resume_container().returning(|_| Ok(()));
    deployer.expect_remove_container().returning(|_| Ok(()));
    deployer
        .expect_get_container_logs()
        .returning(|_| Ok("test logs".to_string()));
    deployer.expect_get_container_info().returning(|_| {
        use std::collections::HashMap;
        Ok(temps_deployer::ContainerInfo {
            container_id: "test-container".to_string(),
            container_name: "test-container".to_string(),
            image_name: "nginx:latest".to_string(),
            created_at: chrono::Utc::now(),
            ports: vec![],
            environment_vars: HashMap::new(),
            status: temps_deployer::ContainerStatus::Running,
            restart_count: Some(0),
            labels: std::collections::HashMap::new(),
            ..Default::default()
        })
    });
    deployer.expect_list_containers().returning(|| Ok(vec![]));
    deployer.expect_stream_container_logs().returning(|_| {
        use futures::stream;
        let stream = stream::empty();
        Ok(Box::new(stream))
    });
    deployer.expect_image_exists().times(0);
    let deployer: Arc<dyn temps_deployer::ContainerDeployer> = Arc::new(deployer);

    // For tests, we'll create a service that directly accepts the trait
    DeploymentService {
        db,
        log_service,
        config_service,
        queue_service,
        docker_log_service,
        docker_handle,
        deployer,
        image_builder: Arc::new(NoopImageBuilder),
        encryption_service: create_test_encryption_service(),
        telemetry: std::sync::OnceLock::new(),
        environment_addresses: std::sync::OnceLock::new(),
        env_resolver: std::sync::OnceLock::new(),
        compose_executor: std::sync::OnceLock::new(),
        audit_logger: std::sync::OnceLock::new(),
        node_scheduler: std::sync::OnceLock::new(),
        workflow_planner: std::sync::OnceLock::new(),
        workload_plane: std::sync::OnceLock::new(),
    }
}

async fn configure_test_service_for_http_readiness(
    service: &DeploymentService,
) -> Result<(), Box<dyn std::error::Error>> {
    let mut settings = service.config_service.get_settings().await?;
    settings.external_url = Some("http://temps-test.local".to_string());
    service.config_service.update_settings(settings).await?;
    Ok(())
}

/// Stub `get_container_info` so the runtime ownership check in
/// `cleanup_project_containers` sees a container whose labels match the
/// project/environment being deleted.
///
/// Cleanup refuses to remove a container that is not provably ours, so
/// every cleanup test has to present the labels a real managed container
/// would carry — otherwise the test fails on the guard rather than on the
/// behaviour it is actually asserting.
fn expect_owned_container_info(
    deployer: &mut MockContainerDeployer,
    project_id: i32,
    environment_id: i32,
) {
    deployer.expect_get_container_info().returning(move |id| {
        Ok(temps_deployer::ContainerInfo {
            container_id: id.to_string(),
            container_name: id.to_string(),
            image_name: "nginx:latest".to_string(),
            created_at: Utc::now(),
            ports: vec![],
            environment_vars: std::collections::HashMap::new(),
            status: temps_deployer::ContainerStatus::Running,
            restart_count: Some(0),
            labels: std::collections::HashMap::from([
                ("sh.temps.managed".to_string(), "true".to_string()),
                ("sh.temps.project_id".to_string(), project_id.to_string()),
                (
                    "sh.temps.environment".to_string(),
                    environment_id.to_string(),
                ),
            ]),
            ..Default::default()
        })
    });
}

fn create_cleanup_service_for_test(
    db: Arc<temps_database::DbConnection>,
    deployer: Arc<dyn temps_deployer::ContainerDeployer>,
) -> DeploymentService {
    let server_config = Arc::new(
        temps_config::ServerConfig::new(
            "127.0.0.1:8080".to_string(),
            "postgresql://test_user:test_password@localhost:5432/test_db".to_string(),
            None,
            None,
        )
        .expect("create test server config"),
    );
    let config_service = Arc::new(temps_config::ConfigService::new(server_config, db.clone()));
    let (queue_service, _receiver) =
        temps_queue::BroadcastQueueService::create_job_queue_arc_with_receiver(8);
    let docker = Arc::new(
        bollard::Docker::connect_with_local_defaults().expect("create Docker client config"),
    );
    let docker_handle = Arc::new(temps_core::DockerHandle::available(docker));

    DeploymentService {
        db,
        log_service: Arc::new(temps_deployment_logs::LogService::new(std::env::temp_dir())),
        config_service,
        queue_service,
        docker_log_service: Arc::new(temps_deployment_logs::DockerLogService::new(
            docker_handle.clone(),
        )),
        docker_handle,
        deployer,
        image_builder: Arc::new(NoopImageBuilder),
        encryption_service: create_test_encryption_service(),
        telemetry: std::sync::OnceLock::new(),
        environment_addresses: std::sync::OnceLock::new(),
        env_resolver: std::sync::OnceLock::new(),
        compose_executor: std::sync::OnceLock::new(),
        audit_logger: std::sync::OnceLock::new(),
        node_scheduler: std::sync::OnceLock::new(),
        workflow_planner: std::sync::OnceLock::new(),
        workload_plane: std::sync::OnceLock::new(),
    }
}

async fn database_integration_tests_available() -> bool {
    std::env::var_os("TEMPS_TEST_DATABASE_URL").is_some()
        || tokio::process::Command::new("docker")
            .arg("info")
            .output()
            .await
            .map(|output| output.status.success())
            .unwrap_or(false)
}

#[tokio::test]
async fn cleanup_does_not_discover_foreign_containers_with_colliding_database_ids(
) -> Result<(), Box<dyn std::error::Error>> {
    if !database_integration_tests_available().await {
        eprintln!("Docker unavailable; skipping cleanup isolation integration test");
        return Ok(());
    }
    let test_db = TestDatabase::with_migrations().await?;
    let db = test_db.connection_arc();
    let (project, _environment, _deployment, container) = setup_test_deployment(&db).await?;
    deployment_containers::Entity::delete_by_id(container.id)
        .exec(db.as_ref())
        .await?;
    // Another database can own live containers carrying identical numeric
    // project/environment labels. Discovery is not an ownership authority.
    let mut deployer = MockContainerDeployer::new();
    let label = project.id.to_string();
    deployer.expect_list_containers().returning(move || {
        Ok(vec![temps_deployer::ContainerInfo {
            container_id: "foreign-container".to_string(),
            labels: HashMap::from([
                ("sh.temps.managed".to_string(), "true".to_string()),
                ("sh.temps.project_id".to_string(), label.clone()),
            ]),
            ..Default::default()
        }])
    });
    deployer.expect_get_container_info().never();
    deployer.expect_remove_container().never();
    let service = create_cleanup_service_for_test(db.clone(), Arc::new(deployer));
    let result =
        temps_core::DeploymentContainerCleaner::cleanup_project_containers(&service, project.id)
            .await;
    assert!(
        result.is_ok(),
        "recorded cleanup should finish while preserving ambiguous resources: {result:?}"
    );
    assert!(projects::Entity::find_by_id(project.id)
        .one(db.as_ref())
        .await?
        .is_some());
    Ok(())
}

/// Record `container` as the replica the workload plane started for
/// `deployment` in a Firecracker microVM, as an `enforce` deploy does.
async fn insert_plane_replica(
    db: &sea_orm::DatabaseConnection,
    deployment: &deployments::Model,
    container: &deployment_containers::Model,
) -> Result<(), Box<dyn std::error::Error>> {
    let now = chrono::Utc::now();
    let workload = temps_entities::workloads::ActiveModel {
        owner_kind: Set(crate::services::plane_deployer::DEPLOYMENT_OWNER_KIND.to_string()),
        owner_id: Set(i64::from(deployment.id)),
        name: Set(container.container_name.clone()),
        spec: Set(serde_json::json!({})),
        generation: Set(1),
        observed_generation: Set(1),
        change_seq: Set(0),
        status: Set(serde_json::json!({})),
        phase: Set("running".to_string()),
        owner_workload_id: Set(None),
        finalizers: Set(Vec::new()),
        expires_at: Set(None),
        created_at: Set(now),
        updated_at: Set(now),
        deleted_at: Set(None),
        ..Default::default()
    }
    .insert(db)
    .await?;
    temps_entities::workload_instances::ActiveModel {
        workload_id: Set(workload.id),
        revision: Set(1),
        attempt: Set(1),
        node_id: Set(None),
        driver: Set("firecracker".to_string()),
        handle: Set(serde_json::json!({ "id": container.container_id })),
        address: Set(None),
        phase: Set("running".to_string()),
        ready: Set(true),
        conditions: Set(serde_json::json!([])),
        exit_code: Set(None),
        published_ports: Set(serde_json::json!([])),
        started_at: Set(Some(now)),
        finished_at: Set(None),
        change_seq: Set(0),
        created_at: Set(now),
        updated_at: Set(now),
        ..Default::default()
    }
    .insert(db)
    .await?;
    Ok(())
}

/// Tearing down a deployment retires a replica the workload plane
/// started instead of asking this host's Docker, which cannot see a
/// microVM: without a plane the teardown fails and keeps the row; once
/// the workload is retired, the row is tombstoned and Docker is still
/// never asked.
#[tokio::test]
async fn teardown_retires_a_plane_replica_instead_of_stopping_it_in_docker(
) -> Result<(), Box<dyn std::error::Error>> {
    if !database_integration_tests_available().await {
        eprintln!("Docker unavailable; skipping plane replica teardown integration test");
        return Ok(());
    }
    let test_db = TestDatabase::with_migrations().await?;
    let db = test_db.connection_arc();
    let (project, _environment, deployment, container) = setup_test_deployment(&db).await?;
    insert_plane_replica(db.as_ref(), &deployment, &container).await?;

    let mut deployer = MockContainerDeployer::new();
    deployer.expect_stop_container().never();
    let service = create_cleanup_service_for_test(db.clone(), Arc::new(deployer));

    let error = service
        .teardown_deployment(project.id, deployment.id)
        .await
        .expect_err("no plane runs to retire the replica");
    assert!(
        matches!(
            error,
            DeploymentError::ReplicaRetire(
                crate::services::plane_deployer::ReplicaRetireError::PlaneUnavailable { .. }
            )
        ),
        "{error}"
    );
    let row = deployment_containers::Entity::find_by_id(container.id)
        .one(db.as_ref())
        .await?
        .ok_or("container row")?;
    assert!(
        row.deleted_at.is_none(),
        "an unretired replica keeps its row"
    );

    temps_entities::workloads::Entity::update_many()
        .col_expr(
            temps_entities::workloads::Column::DeletedAt,
            sea_orm::sea_query::Expr::value(chrono::Utc::now()),
        )
        .exec(db.as_ref())
        .await?;
    service
        .teardown_deployment(project.id, deployment.id)
        .await?;
    let row = deployment_containers::Entity::find_by_id(container.id)
        .one(db.as_ref())
        .await?
        .ok_or("container row")?;
    assert!(row.deleted_at.is_some());
    assert_eq!(row.status.as_deref(), Some("stopped"));
    Ok(())
}

/// A replica started through the workload plane is retired by the
/// plane, never by the node's Docker: without a plane to retire it, the
/// cleanup fails and the row stays fenced (`removing`) rather than being
/// recorded as removed off a Docker 404.
#[tokio::test]
async fn cleanup_never_tombstones_a_plane_replica_it_could_not_retire(
) -> Result<(), Box<dyn std::error::Error>> {
    if !database_integration_tests_available().await {
        eprintln!("Docker unavailable; skipping plane replica cleanup integration test");
        return Ok(());
    }
    let test_db = TestDatabase::with_migrations().await?;
    let db = test_db.connection_arc();
    let (project, _environment, deployment, container) = setup_test_deployment(&db).await?;
    insert_plane_replica(db.as_ref(), &deployment, &container).await?;

    // Docker would answer 404 for a microVM: it is never asked.
    let mut deployer = MockContainerDeployer::new();
    deployer
        .expect_list_containers()
        .returning(|| Ok(Vec::new()));
    deployer.expect_get_container_info().never();
    deployer.expect_remove_container().never();
    let service = create_cleanup_service_for_test(db.clone(), Arc::new(deployer));
    let result =
        temps_core::DeploymentContainerCleaner::cleanup_project_containers(&service, project.id)
            .await;
    let error = result.expect_err("no plane runs to retire the replica");
    assert!(
        error.to_string().contains(&container.container_id),
        "{error}"
    );
    let row = deployment_containers::Entity::find_by_id(container.id)
        .one(db.as_ref())
        .await?
        .ok_or("container row")?;
    assert_eq!(row.status.as_deref(), Some("removing"));
    Ok(())
}

struct DisposableNetwork(String);
impl Drop for DisposableNetwork {
    fn drop(&mut self) {
        let _ = std::process::Command::new("docker")
            .args(["network", "rm", &self.0])
            .output();
    }
}

#[tokio::test]
async fn cleanup_preserves_ambiguous_compose_network_with_executor_initialized(
) -> Result<(), Box<dyn std::error::Error>> {
    if !database_integration_tests_available().await {
        return Ok(());
    }
    let docker = match bollard::Docker::connect_with_local_defaults() {
        Ok(docker) if docker.ping().await.is_ok() => docker,
        _ => return Ok(()),
    };
    if !tokio::process::Command::new("docker")
        .arg("info")
        .output()
        .await
        .is_ok_and(|output| output.status.success())
    {
        return Ok(());
    }
    let test_db = TestDatabase::with_migrations().await?;
    let db = test_db.connection_arc();
    // Use a random high database-local ID so even a regressed teardown
    // cannot select another live instance's ordinary Compose namespace.
    let nonce = uuid::Uuid::new_v4();
    let project_sequence = 1_000_000_000_i64
        + i64::from(u32::from_be_bytes(nonce.as_bytes()[..4].try_into().unwrap()) % 900_000_000);
    db.query_one(Statement::from_sql_and_values(
        DatabaseBackend::Postgres,
        "SELECT setval(pg_get_serial_sequence('projects', 'id'), $1)",
        [project_sequence.into()],
    ))
    .await?;

    let (project, environment, _deployment, container) = setup_test_deployment(&db).await?;
    deployment_containers::Entity::delete_by_id(container.id)
        .exec(db.as_ref())
        .await?;
    let network_name = format!("temps-cleanup-isolation-{}", uuid::Uuid::new_v4());
    let label = format!(
        "com.docker.compose.project=temps-{}-{}",
        project.id, environment.id
    );
    let created = tokio::process::Command::new("docker")
        .args(["network", "create", "--label", &label, &network_name])
        .output()
        .await?;
    assert!(
        created.status.success(),
        "create disposable network: {}",
        String::from_utf8_lossy(&created.stderr)
    );
    let _network_guard = DisposableNetwork(network_name.clone());
    let mut deployer = MockContainerDeployer::new();
    deployer.expect_list_containers().returning(|| Ok(vec![]));
    deployer.expect_remove_container().never();
    let service = create_cleanup_service_for_test(db.clone(), Arc::new(deployer));
    let directory = tempfile::tempdir()?;
    service.set_compose_executor(Arc::new(temps_deployer::compose::ComposeExecutor::new(
        Arc::new(docker),
        directory.path().to_path_buf(),
    )));
    let result =
        temps_core::DeploymentContainerCleaner::cleanup_project_containers(&service, project.id)
            .await;
    let survived = tokio::process::Command::new("docker")
        .args(["network", "inspect", &network_name])
        .output()
        .await?;
    let cleanup = tokio::process::Command::new("docker")
        .args(["network", "rm", &network_name])
        .output()
        .await?;
    assert!(
        cleanup.status.success(),
        "remove only the disposable network"
    );
    assert!(
        survived.status.success(),
        "cleanup must preserve the ambiguous foreign network"
    );
    assert!(
        result.is_ok(),
        "recorded cleanup should finish while preserving ambiguous resources: {result:?}"
    );
    Ok(())
}

#[tokio::test]
async fn cleanup_project_containers_removes_container_before_database_cascade(
) -> Result<(), Box<dyn std::error::Error>> {
    if !database_integration_tests_available().await {
        eprintln!("Docker unavailable; skipping project container cleanup integration test");
        return Ok(());
    }

    let test_db = TestDatabase::with_migrations().await?;
    let db = test_db.connection_arc();
    let (project, environment, _deployment, container) = setup_test_deployment(&db).await?;

    let expected_container_id = container.container_id.clone();
    let mut deployer = MockContainerDeployer::new();
    deployer.expect_list_containers().returning(|| Ok(vec![]));
    expect_owned_container_info(&mut deployer, project.id, environment.id);
    deployer
        .expect_remove_container()
        .withf(move |container_id| container_id == expected_container_id)
        .times(1)
        .returning(|_| Ok(()));
    let service = create_cleanup_service_for_test(db.clone(), Arc::new(deployer));

    let removed =
        temps_core::DeploymentContainerCleaner::cleanup_project_containers(&service, project.id)
            .await?;
    assert_eq!(removed, 1);

    let cleaned = deployment_containers::Entity::find_by_id(container.id)
        .one(db.as_ref())
        .await?
        .expect("container cleanup record remains until project cascade");
    assert_eq!(cleaned.status.as_deref(), Some("removed"));
    assert!(cleaned.deleted_at.is_some());

    projects::Entity::delete_by_id(project.id)
        .exec(db.as_ref())
        .await?;
    assert!(
        deployment_containers::Entity::find_by_id(container.id)
            .one(db.as_ref())
            .await?
            .is_none(),
        "the database cascade must happen only after external cleanup succeeds"
    );

    Ok(())
}

#[tokio::test]
async fn cleanup_proceeds_when_optional_resource_listings_fail(
) -> Result<(), Box<dyn std::error::Error>> {
    if !database_integration_tests_available().await {
        return Ok(());
    }
    let test_db = TestDatabase::with_migrations().await?;
    let db = test_db.connection_arc();
    let (project, environment, _deployment, container) = setup_test_deployment(&db).await?;
    let expected_id = container.container_id.clone();
    let mut deployer = MockContainerDeployer::new();
    deployer.expect_list_containers().returning(|| {
        Err(temps_deployer::DeployerError::Other(
            "listing unavailable".into(),
        ))
    });
    expect_owned_container_info(&mut deployer, project.id, environment.id);
    deployer
        .expect_remove_container()
        .withf(move |id| id == expected_id)
        .times(1)
        .returning(|_| Ok(()));
    let mut service = create_cleanup_service_for_test(db.clone(), Arc::new(deployer));
    // A closed loopback endpoint fails both optional Compose list calls.
    // The recorded container's independent executor still works.
    let listener = std::net::TcpListener::bind("127.0.0.1:0")?;
    let address = listener.local_addr()?;
    drop(listener);
    let docker = Arc::new(bollard::Docker::connect_with_http(
        &format!("http://{address}"),
        1,
        bollard::API_DEFAULT_VERSION,
    )?);
    service.docker_handle = Arc::new(temps_core::DockerHandle::available(docker.clone()));
    let directory = tempfile::tempdir()?;
    service.set_compose_executor(Arc::new(temps_deployer::compose::ComposeExecutor::new(
        docker,
        directory.path().to_path_buf(),
    )));
    let removed =
        temps_core::DeploymentContainerCleaner::cleanup_project_containers(&service, project.id)
            .await?;
    assert_eq!(removed, 1);
    assert_eq!(
        deployment_containers::Entity::find_by_id(container.id)
            .one(db.as_ref())
            .await?
            .unwrap()
            .status
            .as_deref(),
        Some("removed")
    );
    Ok(())
}

#[tokio::test]
async fn ambiguous_cleanup_failure_keeps_container_fenced_for_retry(
) -> Result<(), Box<dyn std::error::Error>> {
    if !database_integration_tests_available().await {
        eprintln!("Docker unavailable; skipping cleanup failure integration test");
        return Ok(());
    }

    let test_db = TestDatabase::with_migrations().await?;
    let db = test_db.connection_arc();
    let (project, environment, _deployment, container) = setup_test_deployment(&db).await?;

    let mut deployer = MockContainerDeployer::new();
    deployer.expect_list_containers().returning(|| Ok(vec![]));
    expect_owned_container_info(&mut deployer, project.id, environment.id);
    deployer.expect_remove_container().times(1).returning(|_| {
        Err(temps_deployer::DeployerError::NetworkError(
            "worker unavailable".to_string(),
        ))
    });
    let service = create_cleanup_service_for_test(db.clone(), Arc::new(deployer));

    let result =
        temps_core::DeploymentContainerCleaner::cleanup_project_containers(&service, project.id)
            .await;
    assert!(matches!(
        result,
        Err(temps_core::ContainerCleanupError::Removal { .. })
    ));

    let fenced = deployment_containers::Entity::find_by_id(container.id)
        .one(db.as_ref())
        .await?
        .expect("failed cleanup must preserve the container record");
    assert_eq!(fenced.status.as_deref(), Some("removing"));
    assert!(fenced.deleted_at.is_some());
    assert!(
        projects::Entity::find_by_id(project.id)
            .one(db.as_ref())
            .await?
            .is_some(),
        "the project must remain recoverable after container cleanup fails"
    );

    Ok(())
}

/// The runtime ownership guard is the last line of defence against
/// deleting a container that merely *recorded* a matching id: if the
/// container Docker actually has under that id belongs to someone else,
/// cleanup must refuse to touch it rather than remove another tenant's
/// workload.
#[tokio::test]
async fn cleanup_refuses_to_remove_a_container_owned_by_another_project(
) -> Result<(), Box<dyn std::error::Error>> {
    if !database_integration_tests_available().await {
        eprintln!("Docker unavailable; skipping cleanup ownership integration test");
        return Ok(());
    }

    let test_db = TestDatabase::with_migrations().await?;
    let db = test_db.connection_arc();
    let (project, environment, _deployment, container) = setup_test_deployment(&db).await?;

    let mut deployer = MockContainerDeployer::new();
    deployer.expect_list_containers().returning(|| Ok(vec![]));
    // Same container id, but the running container claims a different project.
    expect_owned_container_info(&mut deployer, project.id + 1, environment.id);
    // The whole point: removal must never be attempted.
    deployer.expect_remove_container().never();
    let service = create_cleanup_service_for_test(db.clone(), Arc::new(deployer));

    let result =
        temps_core::DeploymentContainerCleaner::cleanup_project_containers(&service, project.id)
            .await;
    assert!(
        matches!(
            result,
            Err(temps_core::ContainerCleanupError::Removal { .. })
        ),
        "cleanup must fail closed when runtime labels do not match, got {result:?}"
    );

    let preserved = deployment_containers::Entity::find_by_id(container.id)
        .one(db.as_ref())
        .await?
        .expect("a refused cleanup must preserve the container record");
    assert!(
        preserved.deleted_at.is_none(),
        "a container we refused to touch must not be marked deleted"
    );

    Ok(())
}

fn runtime_container(id: &str, labels: &[(&str, &str)]) -> temps_deployer::ContainerInfo {
    temps_deployer::ContainerInfo {
        container_id: id.to_string(),
        labels: labels
            .iter()
            .map(|(key, value)| (key.to_string(), value.to_string()))
            .collect(),
        ..Default::default()
    }
}

/// Docker returns the full 64-character ID; rows may record it in full or
/// as a prefix.
const RUNTIME_ID: &str = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";

#[test]
fn ownership_accepts_a_container_with_matching_labels() {
    let info = runtime_container(
        RUNTIME_ID,
        &[
            ("sh.temps.managed", "true"),
            ("sh.temps.project_id", "11"),
            ("sh.temps.environment", "22"),
        ],
    );
    assert_eq!(
        recorded_container_ownership(RUNTIME_ID, &info, 11, 22),
        Ok(RecordedContainerOwnership::Labelled)
    );
    // Labels are authoritative enough when they fully match, even if the
    // row recorded a reference Docker resolved by name.
    assert_eq!(
        recorded_container_ownership("app-production", &info, 11, 22),
        Ok(RecordedContainerOwnership::Labelled)
    );
}

#[test]
fn ownership_falls_back_to_the_recorded_id_for_unlabelled_containers() {
    let unlabelled = runtime_container(RUNTIME_ID, &[]);
    assert_eq!(
        recorded_container_ownership(RUNTIME_ID, &unlabelled, 11, 22),
        Ok(RecordedContainerOwnership::Recorded {
            missing_labels: vec![
                "sh.temps.managed",
                "sh.temps.project_id",
                "sh.temps.environment"
            ],
        })
    );
}

#[test]
fn ownership_accepts_legacy_label_schemes_on_the_recorded_container() {
    // Before `sh.temps.managed` existed.
    let pre_managed = runtime_container(
        RUNTIME_ID,
        &[
            ("sh.temps.project_id", "11"),
            ("sh.temps.environment", "22"),
        ],
    );
    assert_eq!(
        recorded_container_ownership(RUNTIME_ID, &pre_managed, 11, 22),
        Ok(RecordedContainerOwnership::Recorded {
            missing_labels: vec!["sh.temps.managed"],
        })
    );
    // The first label scheme wrote the project as a UUID.
    let uuid_project = runtime_container(
        RUNTIME_ID,
        &[
            (
                "sh.temps.project_id",
                "00000000-0000-5000-8000-000000000000",
            ),
            ("sh.temps.environment", "22"),
        ],
    );
    assert!(recorded_container_ownership(RUNTIME_ID, &uuid_project, 11, 22).is_ok());
    // Early Compose stacks used `sh.temps.environment_id`.
    let compose_environment_id = runtime_container(
        RUNTIME_ID,
        &[
            ("sh.temps.managed", "true"),
            ("sh.temps.project_id", "11"),
            ("sh.temps.environment_id", "22"),
        ],
    );
    assert_eq!(
        recorded_container_ownership(RUNTIME_ID, &compose_environment_id, 11, 22),
        Ok(RecordedContainerOwnership::Labelled)
    );
}

#[test]
fn ownership_refuses_containers_labelled_for_another_owner() {
    let cases: &[&[(&str, &str)]] = &[
        &[
            ("sh.temps.managed", "true"),
            ("sh.temps.project_id", "12"),
            ("sh.temps.environment", "22"),
        ],
        &[
            ("sh.temps.managed", "true"),
            ("sh.temps.project_id", "11"),
            ("sh.temps.environment", "23"),
        ],
        // Legacy key, different environment.
        &[
            ("sh.temps.project_id", "11"),
            ("sh.temps.environment_id", "23"),
        ],
        // The current key matches but the legacy one contradicts it.
        &[
            ("sh.temps.project_id", "11"),
            ("sh.temps.environment", "22"),
            ("sh.temps.environment_id", "23"),
        ],
        // Explicitly not managed by Temps.
        &[
            ("sh.temps.managed", "false"),
            ("sh.temps.project_id", "11"),
            ("sh.temps.environment", "22"),
        ],
    ];
    for labels in cases {
        // Refused even though Docker resolved the recorded ID exactly: a
        // label naming someone else outweighs our own record.
        let info = runtime_container(RUNTIME_ID, labels);
        let reason = recorded_container_ownership(RUNTIME_ID, &info, 11, 22)
            .expect_err(&format!("labels {labels:?} must be refused"));
        assert!(
            reason.contains("different owner") && reason.contains("project 11"),
            "{reason}"
        );
    }
}

#[test]
fn ownership_refuses_an_unlabelled_container_docker_resolved_elsewhere() {
    // The recorded reference is a name that now resolves to a container
    // carrying no labels: nothing ties it to this project.
    let info = runtime_container(RUNTIME_ID, &[]);
    let reason = recorded_container_ownership("app-production", &info, 11, 22)
        .expect_err("a renamed-onto container must be refused");
    assert!(reason.contains(RUNTIME_ID), "{reason}");
    assert!(recorded_container_ownership("", &info, 11, 22).is_err());
    // Docker resolves unique ID prefixes, so a short value -- a short ID,
    // or an importer's source app name that happens to be hex -- can
    // land on any container. Only the full, exact ID is an identity.
    for reference in [&RUNTIME_ID[..2], &RUNTIME_ID[..12], &RUNTIME_ID[..63]] {
        assert!(
            recorded_container_ownership(reference, &info, 11, 22).is_err(),
            "prefix {reference} must not prove ownership"
        );
    }
    let uppercase = RUNTIME_ID.to_uppercase();
    let uppercase_info = runtime_container(&uppercase, &[]);
    assert!(recorded_container_ownership(&uppercase, &uppercase_info, 11, 22).is_err());
}

/// The bug this guards: a project whose container predates Temps'
/// ownership labels (and was stopped by scaling to zero) could never be
/// deleted, because cleanup refused its own recorded container.
#[tokio::test]
async fn cleanup_removes_a_recorded_container_that_predates_ownership_labels(
) -> Result<(), Box<dyn std::error::Error>> {
    if !database_integration_tests_available().await {
        eprintln!("Docker unavailable; skipping legacy-label cleanup integration test");
        return Ok(());
    }

    let test_db = TestDatabase::with_migrations().await?;
    let db = test_db.connection_arc();
    let (project, _environment, _deployment, container) = setup_test_deployment(&db).await?;
    let mut stopped: deployment_containers::ActiveModel = container.clone().into();
    stopped.status = Set(Some("stopped".to_string()));
    stopped.container_id = Set(RUNTIME_ID.to_string());
    stopped.update(db.as_ref()).await?;

    let expected_container_id = RUNTIME_ID.to_string();
    let mut deployer = MockContainerDeployer::new();
    deployer.expect_list_containers().returning(|| Ok(vec![]));
    deployer.expect_get_container_info().returning(|id| {
        Ok(temps_deployer::ContainerInfo {
            container_id: id.to_string(),
            status: temps_deployer::ContainerStatus::Stopped,
            ..Default::default()
        })
    });
    deployer
        .expect_remove_container()
        .withf(move |container_id| container_id == expected_container_id)
        .times(1)
        .returning(|_| Ok(()));
    let service = create_cleanup_service_for_test(db.clone(), Arc::new(deployer));

    let removed =
        temps_core::DeploymentContainerCleaner::cleanup_project_containers(&service, project.id)
            .await?;
    assert_eq!(removed, 1);

    let cleaned = deployment_containers::Entity::find_by_id(container.id)
        .one(db.as_ref())
        .await?
        .expect("container cleanup record remains until project cascade");
    assert_eq!(cleaned.status.as_deref(), Some("removed"));
    Ok(())
}

/// A row may record a reference Docker resolves (a name). Once that
/// resolved container is verified as ours, removal must target its exact
/// ID -- re-resolving the reference could reach a different container.
#[tokio::test]
async fn cleanup_removes_the_inspected_container_id_not_the_recorded_reference(
) -> Result<(), Box<dyn std::error::Error>> {
    if !database_integration_tests_available().await {
        eprintln!("Docker unavailable; skipping resolved-id cleanup integration test");
        return Ok(());
    }

    let test_db = TestDatabase::with_migrations().await?;
    let db = test_db.connection_arc();
    let (project, environment, _deployment, container) = setup_test_deployment(&db).await?;
    let (project_id, environment_id) = (project.id, environment.id);

    let mut deployer = MockContainerDeployer::new();
    deployer.expect_list_containers().returning(|| Ok(vec![]));
    deployer.expect_get_container_info().returning(move |_| {
        Ok(runtime_container(
            RUNTIME_ID,
            &[
                ("sh.temps.managed", "true"),
                ("sh.temps.project_id", &project_id.to_string()),
                ("sh.temps.environment", &environment_id.to_string()),
            ],
        ))
    });
    deployer
        .expect_remove_container()
        .withf(|container_id| container_id == RUNTIME_ID)
        .times(1)
        .returning(|_| Ok(()));
    let service = create_cleanup_service_for_test(db.clone(), Arc::new(deployer));

    let removed =
        temps_core::DeploymentContainerCleaner::cleanup_project_containers(&service, project.id)
            .await?;
    assert_eq!(removed, 1);
    assert_ne!(container.container_id, RUNTIME_ID);
    Ok(())
}

/// A stopped container's environment label still has to match: a
/// recorded ID that carries another environment of the same project is
/// refused, and its row is left untouched.
#[tokio::test]
async fn cleanup_refuses_a_container_labelled_for_another_environment(
) -> Result<(), Box<dyn std::error::Error>> {
    if !database_integration_tests_available().await {
        eprintln!("Docker unavailable; skipping cleanup environment integration test");
        return Ok(());
    }

    let test_db = TestDatabase::with_migrations().await?;
    let db = test_db.connection_arc();
    let (project, environment, _deployment, container) = setup_test_deployment(&db).await?;

    let mut deployer = MockContainerDeployer::new();
    deployer.expect_list_containers().returning(|| Ok(vec![]));
    expect_owned_container_info(&mut deployer, project.id, environment.id + 1);
    deployer.expect_remove_container().never();
    let service = create_cleanup_service_for_test(db.clone(), Arc::new(deployer));

    let result =
        temps_core::DeploymentContainerCleaner::cleanup_project_containers(&service, project.id)
            .await;
    assert!(
        matches!(
            &result,
            Err(temps_core::ContainerCleanupError::Removal { reason, .. })
                if reason.contains("sh.temps.environment")
        ),
        "cleanup must refuse a container labelled for another environment, got {result:?}"
    );
    let preserved = deployment_containers::Entity::find_by_id(container.id)
        .one(db.as_ref())
        .await?
        .expect("a refused cleanup must preserve the container record");
    assert!(preserved.deleted_at.is_none());
    assert_eq!(preserved.status, container.status);
    Ok(())
}

/// A scaled-to-zero environment's container may already be gone from
/// Docker. That is the state cleanup wants, not an error.
#[tokio::test]
async fn cleanup_treats_an_already_gone_container_as_removed(
) -> Result<(), Box<dyn std::error::Error>> {
    if !database_integration_tests_available().await {
        eprintln!("Docker unavailable; skipping gone-container cleanup integration test");
        return Ok(());
    }

    let test_db = TestDatabase::with_migrations().await?;
    let db = test_db.connection_arc();
    let (project, _environment, _deployment, container) = setup_test_deployment(&db).await?;
    let mut stopped: deployment_containers::ActiveModel = container.clone().into();
    stopped.status = Set(Some("stopped".to_string()));
    stopped.update(db.as_ref()).await?;

    let mut deployer = MockContainerDeployer::new();
    deployer.expect_list_containers().returning(|| Ok(vec![]));
    deployer
        .expect_get_container_info()
        .times(1)
        .returning(|id| {
            Err(temps_deployer::DeployerError::ContainerNotFound(format!(
                "Container {id} not found"
            )))
        });
    deployer.expect_remove_container().never();
    let service = create_cleanup_service_for_test(db.clone(), Arc::new(deployer));

    let removed =
        temps_core::DeploymentContainerCleaner::cleanup_project_containers(&service, project.id)
            .await?;
    assert_eq!(removed, 1);
    let finalized = deployment_containers::Entity::find_by_id(container.id)
        .one(db.as_ref())
        .await?
        .expect("cleanup record remains until project cascade");
    assert_eq!(finalized.status.as_deref(), Some("removed"));
    assert!(finalized.deleted_at.is_some());
    Ok(())
}

#[tokio::test]
async fn interrupted_cleanup_is_idempotent_when_container_is_already_absent(
) -> Result<(), Box<dyn std::error::Error>> {
    if !database_integration_tests_available().await {
        eprintln!("Docker unavailable; skipping interrupted cleanup integration test");
        return Ok(());
    }

    let test_db = TestDatabase::with_migrations().await?;
    let db = test_db.connection_arc();
    let (project, _environment, _deployment, container) = setup_test_deployment(&db).await?;

    let mut interrupted: deployment_containers::ActiveModel = container.clone().into();
    interrupted.status = Set(Some("removing".to_string()));
    interrupted.deleted_at = Set(Some(Utc::now()));
    interrupted.update(db.as_ref()).await?;

    let mut deployer = MockContainerDeployer::new();
    deployer.expect_list_containers().returning(|| Ok(vec![]));
    deployer
        .expect_get_container_info()
        .times(1)
        .returning(|_| {
            Err(temps_deployer::DeployerError::ContainerNotFound(
                "already removed".to_string(),
            ))
        });
    deployer.expect_remove_container().never();
    let service = create_cleanup_service_for_test(db.clone(), Arc::new(deployer));

    let removed =
        temps_core::DeploymentContainerCleaner::cleanup_project_containers(&service, project.id)
            .await?;
    assert_eq!(removed, 1);

    let finalized = deployment_containers::Entity::find_by_id(container.id)
        .one(db.as_ref())
        .await?
        .expect("cleanup record remains until project cascade");
    assert_eq!(finalized.status.as_deref(), Some("removed"));
    assert!(finalized.deleted_at.is_some());

    Ok(())
}

/// Rows Temps knows are gone or no longer manages -- removed, or
/// orphaned by a node removal -- never block a project deletion, even on
/// a control plane with no Docker daemon. A row with no node that may
/// still exist does block it, and the error names both places it can be:
/// a local container from when this server ran workloads, or a worker
/// removed before Temps recorded orphaned containers.
/// Redeploy planning works in bounded batches: an oversized request is
/// refused before anything is loaded.
#[test]
fn redeploy_planning_is_limited_to_one_batch() {
    assert!(check_redeploy_plan_batch(0).is_ok());
    assert!(check_redeploy_plan_batch(MAX_REDEPLOY_PLAN_BATCH).is_ok());
    let error = check_redeploy_plan_batch(MAX_REDEPLOY_PLAN_BATCH + 1).unwrap_err();
    assert!(
        matches!(&error, DeploymentError::InvalidInput(message)
            if message.contains(&format!("{}", MAX_REDEPLOY_PLAN_BATCH + 1))),
        "{error:?}"
    );
}

#[tokio::test]
async fn cleanup_on_a_control_plane_skips_only_rows_known_to_be_gone(
) -> Result<(), Box<dyn std::error::Error>> {
    if !database_integration_tests_available().await {
        eprintln!("Docker unavailable; skipping control-plane cleanup integration test");
        return Ok(());
    }

    let test_db = TestDatabase::with_migrations().await?;
    let db = test_db.connection_arc();
    let (project, _environment, _deployment, container) = setup_test_deployment(&db).await?;
    let insert_row = |suffix: &'static str, status: &'static str| {
        let mut row: deployment_containers::ActiveModel = container.clone().into();
        row.id = sea_orm::ActiveValue::NotSet;
        row.container_id = Set(format!("{}-{suffix}", container.container_id));
        row.status = Set(Some(status.to_string()));
        row.deleted_at = Set(Some(Utc::now()));
        row.node_id = Set(None);
        row
    };
    let mut known_gone = Vec::new();
    for (suffix, status) in [
        ("removed", CONTAINER_STATUS_REMOVED),
        ("orphaned", CONTAINER_STATUS_ORPHANED),
    ] {
        known_gone.push(insert_row(suffix, status).insert(db.as_ref()).await?);
    }
    deployment_containers::Entity::delete_by_id(container.id)
        .exec(db.as_ref())
        .await?;
    let control_plane = |db: Arc<temps_database::DbConnection>| {
        let mut deployer = MockContainerDeployer::new();
        deployer.expect_list_containers().returning(|| Ok(vec![]));
        deployer.expect_get_container_info().never();
        deployer.expect_remove_container().never();
        let mut service = create_cleanup_service_for_test(db, Arc::new(deployer));
        service.docker_handle = Arc::new(temps_core::DockerHandle::disabled(
            "control-plane",
            "started with --profile control-plane",
        ));
        service
    };

    // Only rows known to be gone: nothing removed, nothing failed.
    let removed = temps_core::DeploymentContainerCleaner::cleanup_project_containers(
        &control_plane(db.clone()),
        project.id,
    )
    .await?;
    assert_eq!(removed, 0);
    for row in &known_gone {
        let unchanged = deployment_containers::Entity::find_by_id(row.id)
            .one(db.as_ref())
            .await?
            .expect("row remains until the project cascade");
        assert_eq!(unchanged.status, row.status);
    }

    // A row that may still exist stops the deletion and explains why.
    insert_row("retired", "retired").insert(db.as_ref()).await?;
    let result = temps_core::DeploymentContainerCleaner::cleanup_project_containers(
        &control_plane(db.clone()),
        project.id,
    )
    .await;
    match result {
        Err(temps_core::ContainerCleanupError::Removal {
            node_id: None,
            reason,
            ..
        }) => {
            assert!(reason.contains("records no node"), "{reason}");
            assert!(reason.contains("profile that runs workloads"), "{reason}");
            assert!(reason.contains("docker rm -f"), "{reason}");
        }
        other => panic!("expected the deletion to stop, got {other:?}"),
    }
    Ok(())
}

#[tokio::test]
async fn cleanup_still_fails_for_an_unreachable_container_not_known_to_be_gone(
) -> Result<(), Box<dyn std::error::Error>> {
    if !database_integration_tests_available().await {
        eprintln!("Docker unavailable; skipping unreachable-node cleanup integration test");
        return Ok(());
    }

    // Arrange: the container's worker still exists but cannot be reached.
    let test_db = TestDatabase::with_migrations().await?;
    let db = test_db.connection_arc();
    let (project, _environment, _deployment, container) = setup_test_deployment(&db).await?;
    // No agent token, so the node can never be dialled.
    let node = nodes::ActiveModel {
        name: Set("worker-unreachable".to_string()),
        token_hash: Set("unused".to_string()),
        token_encrypted: Set(None),
        address: Set("https://127.0.0.1:1".to_string()),
        private_address: Set("127.0.0.1".to_string()),
        role: Set("worker".to_string()),
        status: Set("offline".to_string()),
        labels: Set(serde_json::json!({})),
        capacity: Set(serde_json::json!({})),
        dns_resolver_consecutive_failures: Set(0),
        build_limits: Set(None),
        public_ingress_enabled: Set(false),
        public_ingress_unsupported_reasons: Set(serde_json::json!([])),
        created_at: Set(Utc::now()),
        updated_at: Set(Utc::now()),
        ..Default::default()
    }
    .insert(db.as_ref())
    .await?;
    let mut active: deployment_containers::ActiveModel = container.clone().into();
    active.node_id = Set(Some(node.id));
    active.status = Set(Some("retired".to_string()));
    active.deleted_at = Set(Some(Utc::now()));
    active.update(db.as_ref()).await?;
    let mut deployer = MockContainerDeployer::new();
    deployer.expect_list_containers().returning(|| Ok(vec![]));
    deployer.expect_remove_container().never();
    let service = create_cleanup_service_for_test(db.clone(), Arc::new(deployer));

    // Act
    let result =
        temps_core::DeploymentContainerCleaner::cleanup_project_containers(&service, project.id)
            .await;

    // Assert: it may still exist on that node, so deletion must stop and
    // say which container and node.
    match result {
        Err(temps_core::ContainerCleanupError::Removal {
            container_id,
            node_id,
            ..
        }) => {
            assert_eq!(container_id, container.container_id);
            assert_eq!(node_id, Some(node.id));
        }
        other => panic!("expected a removal failure, got {other:?}"),
    }
    Ok(())
}

#[tokio::test]
async fn cancel_all_project_deployments_is_project_scoped() -> Result<(), Box<dyn std::error::Error>>
{
    if !database_integration_tests_available().await {
        eprintln!("Docker unavailable; skipping project cancellation integration test");
        return Ok(());
    }

    let test_db = TestDatabase::with_migrations().await?;
    let db = test_db.connection_arc();
    let (project, _environment, deployment, _container) = setup_test_deployment(&db).await?;
    let mut active: deployments::ActiveModel = deployment.clone().into();
    active.state = Set("running".to_string());
    active.update(db.as_ref()).await?;
    let stopped_deployment = deployments::ActiveModel {
        project_id: Set(project.id),
        environment_id: Set(deployment.environment_id),
        state: Set("stopped".to_string()),
        slug: Set("stopped-deployment".to_string()),
        metadata: Set(Some(Default::default())),
        ..Default::default()
    }
    .insert(db.as_ref())
    .await?;

    let other_project = projects::ActiveModel {
        name: Set("Other Project".to_string()),
        slug: Set("other-project".to_string()),
        repo_name: Set("other-repo".to_string()),
        repo_owner: Set("other-owner".to_string()),
        preset: Set(Preset::NextJs),
        main_branch: Set("main".to_string()),
        directory: Set("/".to_string()),
        ..Default::default()
    }
    .insert(db.as_ref())
    .await?;
    let other_environment = environments::ActiveModel {
        project_id: Set(other_project.id),
        name: Set("Production".to_string()),
        slug: Set("other-prod".to_string()),
        host: Set("other.example.com".to_string()),
        upstreams: Set(UpstreamList::default()),
        subdomain: Set("other.example.com".to_string()),
        ..Default::default()
    }
    .insert(db.as_ref())
    .await?;
    let other_deployment = deployments::ActiveModel {
        project_id: Set(other_project.id),
        environment_id: Set(other_environment.id),
        state: Set("running".to_string()),
        slug: Set("other-deployment".to_string()),
        metadata: Set(Some(Default::default())),
        ..Default::default()
    }
    .insert(db.as_ref())
    .await?;

    let service = create_deployment_service_for_test(db.clone());
    let cancelled = service.cancel_all_project_deployments(project.id).await?;
    assert_eq!(cancelled, 1);

    let cancelled_deployment = deployments::Entity::find_by_id(deployment.id)
        .one(db.as_ref())
        .await?
        .expect("target deployment remains for history");
    assert_eq!(cancelled_deployment.state, "cancelled");
    assert_eq!(
        cancelled_deployment.cancelled_reason.as_deref(),
        Some("Project deleted")
    );

    let untouched = deployments::Entity::find_by_id(other_deployment.id)
        .one(db.as_ref())
        .await?
        .expect("other project deployment remains");
    assert_eq!(untouched.state, "running");
    let stopped = deployments::Entity::find_by_id(stopped_deployment.id)
        .one(db.as_ref())
        .await?
        .expect("stopped deployment remains for history");
    assert_eq!(stopped.state, "stopped");
    assert_ne!(project.id, other_project.id);

    Ok(())
}

#[tokio::test]
async fn test_pause_deployment() -> Result<(), Box<dyn std::error::Error>> {
    let test_db = TestDatabase::with_migrations().await?;
    let db = test_db.connection_arc();

    // Setup test data
    let (_project, _environment, deployment) = setup_test_data(&db).await?;

    let deployment_service = create_deployment_service_for_test(db.clone());

    // Test pause deployment
    deployment_service
        .pause_deployment(deployment.project_id, deployment.id)
        .await?;

    // Verify deployment state was updated
    let updated_deployment = deployments::Entity::find_by_id(deployment.id)
        .one(db.as_ref())
        .await?
        .unwrap();
    assert_eq!(updated_deployment.state, "paused");

    Ok(())
}

#[tokio::test]
async fn test_resume_deployment() -> Result<(), Box<dyn std::error::Error>> {
    let test_db = TestDatabase::with_migrations().await?;
    let db = test_db.connection_arc();

    // Setup test data
    let (_project, environment, mut deployment) = setup_test_data(&db).await?;
    setup_test_environment_variables(&db, deployment.project_id, environment.id).await?;

    // Set deployment to paused state
    let mut active_deployment: deployments::ActiveModel = deployment.clone().into();
    active_deployment.state = Set("paused".to_string());
    deployment = active_deployment.update(db.as_ref()).await?;

    let deployment_service = create_deployment_service_for_test(db.clone());

    // Test resume deployment
    deployment_service
        .resume_deployment(deployment.project_id, deployment.id)
        .await?;

    // Verify deployment state was updated
    let updated_deployment = deployments::Entity::find_by_id(deployment.id)
        .one(db.as_ref())
        .await?
        .unwrap();
    assert_eq!(updated_deployment.state, "deployed");

    Ok(())
}

// Regression coverage for the pause/resume Docker-op mismatch bug: the
// two tests above use `setup_test_data`, which never inserts a
// `deployment_containers` row, so the container loop inside
// `pause_deployment`/`resume_deployment` never actually executes and
// neither test can observe which Docker operation gets called. These
// two variants use `setup_test_deployment` (which inserts one real
// container) and pin down the *exact* deployer method invoked via a
// narrowly-scoped mock, so a regression back to `pause_container`/
// `resume_container` (the old, broken ops) — or simply forgetting to
// touch the container at all — fails the test instead of passing it
// silently.
#[tokio::test]
async fn test_pause_deployment_stops_real_container() -> Result<(), Box<dyn std::error::Error>> {
    let test_db = TestDatabase::with_migrations().await?;
    let db = test_db.connection_arc();

    let (_project, _environment, deployment, container) = setup_test_deployment(&db).await?;
    assert_eq!(container.status.as_deref(), Some("running"));

    // Only `stop_container` is wired up on this mock. If pause
    // regressed to calling `pause_container`/`resume_container`
    // instead, the mock has no expectation for that method and panics.
    let expected_container_id = container.container_id.clone();
    let mut deployer = MockContainerDeployer::new();
    deployer
        .expect_stop_container()
        .withf(move |id| id == expected_container_id)
        .times(1)
        .returning(|_| Ok(()));
    let deployer: Arc<dyn temps_deployer::ContainerDeployer> = Arc::new(deployer);

    let deployment_service = create_cleanup_service_for_test(db.clone(), deployer);

    deployment_service
        .pause_deployment(deployment.project_id, deployment.id)
        .await?;

    let updated_deployment = deployments::Entity::find_by_id(deployment.id)
        .one(db.as_ref())
        .await?
        .unwrap();
    assert_eq!(updated_deployment.state, "paused");

    let updated_container = deployment_containers::Entity::find_by_id(container.id)
        .one(db.as_ref())
        .await?
        .unwrap();
    assert_eq!(updated_container.status.as_deref(), Some("stopped"));

    Ok(())
}

#[tokio::test]
async fn test_resume_deployment_starts_real_container() -> Result<(), Box<dyn std::error::Error>> {
    let test_db = TestDatabase::with_migrations().await?;
    let db = test_db.connection_arc();

    let (_project, _environment, deployment, container) = setup_test_deployment(&db).await?;

    // Simulate the post-pause state: deployment "paused", container
    // "stopped" but not removed (pause_deployment keeps the Docker
    // container object around rather than force-removing it).
    let mut active_deployment: deployments::ActiveModel = deployment.clone().into();
    active_deployment.state = Set("paused".to_string());
    let deployment = active_deployment.update(db.as_ref()).await?;

    let mut active_container: deployment_containers::ActiveModel = container.clone().into();
    active_container.status = Set(Some("stopped".to_string()));
    let container = active_container.update(db.as_ref()).await?;

    // Only `start_container` (a plain `docker start`) is wired up. If
    // resume regressed to calling `resume_container` (Docker's
    // unpause/cgroup-freeze reverse, which always fails against a
    // container that was merely stopped, not paused), the mock has no
    // expectation for that method and panics.
    let expected_container_id = container.container_id.clone();
    let mut deployer = MockContainerDeployer::new();
    deployer
        .expect_start_container()
        .withf(move |id| id == expected_container_id)
        .times(1)
        .returning(|_| Ok(()));
    let deployer: Arc<dyn temps_deployer::ContainerDeployer> = Arc::new(deployer);

    let deployment_service = create_cleanup_service_for_test(db.clone(), deployer);

    deployment_service
        .resume_deployment(deployment.project_id, deployment.id)
        .await?;

    let updated_deployment = deployments::Entity::find_by_id(deployment.id)
        .one(db.as_ref())
        .await?
        .unwrap();
    assert_eq!(updated_deployment.state, "deployed");

    let updated_container = deployment_containers::Entity::find_by_id(container.id)
        .one(db.as_ref())
        .await?
        .unwrap();
    assert_eq!(updated_container.status.as_deref(), Some("running"));

    Ok(())
}

#[tokio::test]
async fn test_rollback_to_deployment() -> Result<(), Box<dyn std::error::Error>> {
    let test_db = TestDatabase::with_migrations().await?;
    let db = test_db.connection_arc();

    // Setup test data
    let (_project, mut environment, target_deployment) = setup_test_data(&db).await?;
    let expected_command = vec!["start".to_string(), "--optimized".to_string()];
    let expected_runtime = temps_entities::preset::ContainerRuntimeOverrides {
        command: Some(vec!["worker".into(), "argument with spaces".into()]),
        entrypoint: Some(vec![]),
    };
    let expected_health_check_path = "/realms/master".to_string();
    let mut active_target: deployments::ActiveModel = target_deployment.into();
    active_target.metadata = Set(Some(temps_entities::deployments::DeploymentMetadata {
        command: Some(expected_command.clone()),
        runtime_overrides: Some(expected_runtime.clone()),
        health_check_path: Some(expected_health_check_path.clone()),
        ..Default::default()
    }));
    let target_deployment = active_target.update(db.as_ref()).await?;
    setup_test_environment_variables(&db, target_deployment.project_id, environment.id).await?;

    // Create container for target deployment (required for rollback)
    let now = Utc::now();
    let target_container = deployment_containers::ActiveModel {
        deployment_id: Set(target_deployment.id),
        container_id: Set("container-rollback-target".to_string()),
        container_name: Set("app-rollback-target".to_string()),
        container_port: Set(Some(8080)),
        image_name: Set(Some("nginx:target".to_string())),
        status: Set(Some("running".to_string())),
        created_at: Set(now),
        deployed_at: Set(now),
        ..Default::default()
    };
    target_container.insert(db.as_ref()).await?;

    // Create current deployment that will be stopped
    let current_deployment = deployments::ActiveModel {
        project_id: Set(target_deployment.project_id),
        environment_id: Set(environment.id),
        slug: Set("current-deployment-456".to_string()),
        state: Set("deployed".to_string()),
        metadata: Set(Some(
            temps_entities::deployments::DeploymentMetadata::default(),
        )),
        image_name: Set(Some("nginx:current".to_string())),
        created_at: Set(Utc::now()),
        updated_at: Set(Utc::now()),
        ..Default::default()
    };
    let current_deployment = current_deployment.insert(db.as_ref()).await?;

    // Create container for current deployment
    let current_container = deployment_containers::ActiveModel {
        deployment_id: Set(current_deployment.id),
        container_id: Set("container-rollback-current".to_string()),
        container_name: Set("app-rollback-current".to_string()),
        container_port: Set(Some(8080)),
        image_name: Set(Some("nginx:current".to_string())),
        status: Set(Some("running".to_string())),
        created_at: Set(now),
        deployed_at: Set(now),
        ..Default::default()
    };
    current_container.insert(db.as_ref()).await?;

    // Update environment to point to current deployment
    let mut active_environment: environments::ActiveModel = environment.into();
    active_environment.current_deployment_id = Set(Some(current_deployment.id));
    environment = active_environment.update(db.as_ref()).await?;

    let deployment_service = create_deployment_service_for_test(db.clone());
    configure_test_service_for_http_readiness(&deployment_service).await?;

    // Test rollback
    let result = deployment_service
        .rollback_to_deployment(target_deployment.project_id, target_deployment.id)
        .await?;

    // Verify result - rollback now creates a NEW deployment record
    // The returned deployment ID should be different from the target (it's the new rollback deployment)
    assert_ne!(result.id, target_deployment.id);
    assert!(result.is_current);

    // Verify the new rollback deployment has the correct metadata
    let rollback_dep = deployments::Entity::find_by_id(result.id)
        .one(db.as_ref())
        .await?
        .unwrap();
    let metadata = rollback_dep.metadata.unwrap();
    assert!(metadata.is_rollback);
    assert_eq!(metadata.rolled_back_from_id, Some(target_deployment.id));
    assert_eq!(metadata.runtime_overrides, Some(expected_runtime.clone()));
    assert_eq!(metadata.command, Some(expected_command));
    assert_eq!(
        metadata.health_check_path.as_deref(),
        Some(expected_health_check_path.as_str())
    );

    // Verify environment was updated to point to the NEW rollback deployment
    let updated_environment = environments::Entity::find_by_id(environment.id)
        .one(db.as_ref())
        .await?
        .unwrap();
    assert_eq!(updated_environment.current_deployment_id, Some(result.id));

    // Changing the project to static uploads must not reinterpret an
    // older deployment that retained a container image.
    let project = projects::Entity::find_by_id(target_deployment.project_id)
        .one(db.as_ref())
        .await?
        .ok_or("rollback project disappeared")?;
    let mut active_project: projects::ActiveModel = project.into();
    active_project.source_type = Set(temps_entities::source_type::SourceType::StaticFiles);
    active_project.update(db.as_ref()).await?;
    let repeated = deployment_service
        .rollback_to_deployment(target_deployment.project_id, target_deployment.id)
        .await?;
    let persisted = deployments::Entity::find_by_id(repeated.id)
        .one(db.as_ref())
        .await?
        .ok_or("container rollback was not persisted")?;
    assert_eq!(persisted.image_name, target_deployment.image_name);
    assert_eq!(persisted.static_dir_location, None);
    assert_eq!(
        persisted.metadata.unwrap().runtime_overrides,
        Some(expected_runtime)
    );

    Ok(())
}

#[tokio::test]
async fn test_promote_deployment_preserves_image_runtime() -> Result<(), Box<dyn std::error::Error>>
{
    let test_db = TestDatabase::with_migrations().await?;
    let db = test_db.connection_arc();

    let (project, _source_environment, source) = setup_test_data(&db).await?;
    // A new project source type does not change the stored image artifact.
    let mut active_project: projects::ActiveModel = project.clone().into();
    active_project.source_type = Set(temps_entities::source_type::SourceType::StaticFiles);
    active_project.update(db.as_ref()).await?;
    let expected_command = vec!["start".to_string(), "--optimized".to_string()];
    let expected_runtime = temps_entities::preset::ContainerRuntimeOverrides {
        command: Some(vec!["worker".into(), "argument with spaces".into()]),
        entrypoint: Some(vec!["/app/entrypoint.sh".into()]),
    };
    let expected_health_check_path = "/realms/master".to_string();
    let mut active_source: deployments::ActiveModel = source.into();
    active_source.metadata = Set(Some(temps_entities::deployments::DeploymentMetadata {
        command: Some(expected_command.clone()),
        runtime_overrides: Some(expected_runtime.clone()),
        health_check_path: Some(expected_health_check_path.clone()),
        ..Default::default()
    }));
    let source = active_source.update(db.as_ref()).await?;

    let now = Utc::now();
    let target_environment = environments::ActiveModel {
        project_id: Set(project.id),
        name: Set("Promotion Target".to_string()),
        slug: Set("promotion-target".to_string()),
        host: Set("promotion-target.example.com".to_string()),
        upstreams: Set(UpstreamList::default()),
        current_deployment_id: Set(None),
        subdomain: Set("promotion-target.example.com".to_string()),
        created_at: Set(now),
        updated_at: Set(now),
        ..Default::default()
    }
    .insert(db.as_ref())
    .await?;

    let deployment_service = create_deployment_service_for_test(db.clone());
    configure_test_service_for_http_readiness(&deployment_service).await?;

    let promoted = deployment_service
        .promote_deployment(project.id, source.id, target_environment.id)
        .await?;

    let promoted_model = deployments::Entity::find_by_id(promoted.id)
        .one(db.as_ref())
        .await?
        .ok_or("promoted deployment was not persisted")?;
    assert_eq!(promoted_model.image_name, source.image_name);
    assert_eq!(promoted_model.static_dir_location, None);
    let metadata = promoted_model
        .metadata
        .ok_or("promoted deployment metadata was not persisted")?;
    assert_eq!(metadata.runtime_overrides, Some(expected_runtime));
    assert_eq!(metadata.command, Some(expected_command));
    assert_eq!(
        metadata.health_check_path.as_deref(),
        Some(expected_health_check_path.as_str())
    );

    Ok(())
}

#[tokio::test]
async fn static_vite_rollback_reuses_bundle_without_an_image(
) -> Result<(), Box<dyn std::error::Error>> {
    let test_db = TestDatabase::with_migrations().await?;
    let db = test_db.connection_arc();
    let (project, environment, target) = setup_test_data(&db).await?;

    let mut active_project: projects::ActiveModel = project.clone().into();
    active_project.source_type = Set(temps_entities::source_type::SourceType::StaticFiles);
    active_project.preset = Set(temps_entities::preset::Preset::Vite);
    active_project.preset_config = Set(Some(
        temps_entities::preset::PresetConfig::default_for_preset(
            temps_entities::preset::Preset::Vite,
        ),
    ));
    active_project.update(db.as_ref()).await?;

    let mut active_target: deployments::ActiveModel = target.into();
    active_target.state = Set("stopped".to_string());
    active_target.image_name = Set(None);
    active_target.static_dir_location = Set(Some("cas/static/original-bundle".to_string()));
    let target = active_target.update(db.as_ref()).await?;

    let current = deployments::ActiveModel {
        project_id: Set(project.id),
        environment_id: Set(environment.id),
        slug: Set("current-static-bundle".to_string()),
        state: Set("completed".to_string()),
        metadata: Set(Some(
            temps_entities::deployments::DeploymentMetadata::default(),
        )),
        static_dir_location: Set(Some("cas/static/replacement-bundle".to_string())),
        ..Default::default()
    }
    .insert(db.as_ref())
    .await?;
    let mut active_environment: environments::ActiveModel = environment.into();
    active_environment.current_deployment_id = Set(Some(current.id));
    let environment = active_environment.update(db.as_ref()).await?;

    let service = create_deployment_service_for_test(db.clone());
    configure_test_service_for_http_readiness(&service).await?;
    let rolled_back = service
        .rollback_to_deployment(project.id, target.id)
        .await?;

    assert_ne!(rolled_back.id, target.id);
    let persisted = deployments::Entity::find_by_id(rolled_back.id)
        .one(db.as_ref())
        .await?
        .ok_or("static rollback was not persisted")?;
    assert_eq!(persisted.state, "completed");
    assert_eq!(persisted.image_name, None);
    assert_eq!(persisted.static_dir_location, target.static_dir_location);
    let metadata = persisted
        .metadata
        .ok_or("rollback metadata was not persisted")?;
    assert!(metadata.is_rollback);
    assert_eq!(metadata.rolled_back_from_id, Some(target.id));
    let environment = environments::Entity::find_by_id(environment.id)
        .one(db.as_ref())
        .await?
        .ok_or("static environment disappeared")?;
    assert_eq!(environment.current_deployment_id, Some(rolled_back.id));
    Ok(())
}

#[tokio::test]
async fn static_promotion_reuses_assets_without_an_image() -> Result<(), Box<dyn std::error::Error>>
{
    let test_db = TestDatabase::with_migrations().await?;
    let db = test_db.connection_arc();
    let (project, _source_environment, source) = setup_test_data(&db).await?;

    let mut active_project: projects::ActiveModel = project.clone().into();
    active_project.preset = Set(temps_entities::preset::Preset::Static);
    active_project.preset_config = Set(Some(
        temps_entities::preset::PresetConfig::default_for_preset(
            temps_entities::preset::Preset::Static,
        ),
    ));
    active_project.update(db.as_ref()).await?;

    let mut active_source: deployments::ActiveModel = source.into();
    active_source.image_name = Set(None);
    active_source.static_dir_location = Set(Some("cas/static/site-asset".to_string()));
    let source = active_source.update(db.as_ref()).await?;

    let target_environment = environments::ActiveModel {
        project_id: Set(project.id),
        name: Set("Static Target".to_string()),
        slug: Set("static-target".to_string()),
        host: Set("static-target.example.com".to_string()),
        upstreams: Set(UpstreamList::default()),
        subdomain: Set("static-target.example.com".to_string()),
        ..Default::default()
    }
    .insert(db.as_ref())
    .await?;

    let deployment_service = create_deployment_service_for_test(db.clone());
    configure_test_service_for_http_readiness(&deployment_service).await?;
    let promoted = deployment_service
        .promote_deployment(project.id, source.id, target_environment.id)
        .await?;

    let persisted = deployments::Entity::find_by_id(promoted.id)
        .one(db.as_ref())
        .await?
        .ok_or("static promotion was not persisted")?;
    assert_eq!(persisted.state, "completed");
    assert_eq!(persisted.image_name, None);
    assert_eq!(persisted.static_dir_location, source.static_dir_location);
    assert_eq!(persisted.promoted_from_deployment_id, Some(source.id));

    let target_environment = environments::Entity::find_by_id(target_environment.id)
        .one(db.as_ref())
        .await?
        .ok_or("static target environment disappeared")?;
    assert_eq!(target_environment.current_deployment_id, Some(promoted.id));
    Ok(())
}

#[tokio::test]
async fn static_artifact_promotion_uses_bundle_when_project_preset_changes(
) -> Result<(), Box<dyn std::error::Error>> {
    let test_db = TestDatabase::with_migrations().await?;
    let db = test_db.connection_arc();
    let (project, _environment, source) = setup_test_data(&db).await?;

    // The default Node project represents a changed framework. The
    // immutable source deployment still contains a static bundle.
    let mut active_source: deployments::ActiveModel = source.into();
    active_source.image_name = Set(None);
    active_source.static_dir_location = Set(Some("cas/static/retained-bundle".to_string()));
    let source = active_source.update(db.as_ref()).await?;
    let target_environment = environments::ActiveModel {
        project_id: Set(project.id),
        name: Set("Static Target".to_string()),
        slug: Set("static-target".to_string()),
        host: Set("static-target.example.com".to_string()),
        upstreams: Set(UpstreamList::default()),
        subdomain: Set("static-target.example.com".to_string()),
        ..Default::default()
    }
    .insert(db.as_ref())
    .await?;

    let service = create_deployment_service_for_test(db.clone());
    configure_test_service_for_http_readiness(&service).await?;
    let promoted = service
        .promote_deployment(project.id, source.id, target_environment.id)
        .await?;
    let persisted = deployments::Entity::find_by_id(promoted.id)
        .one(db.as_ref())
        .await?
        .ok_or("static promotion was not persisted")?;
    assert_eq!(persisted.state, "completed");
    assert_eq!(persisted.image_name, None);
    assert_eq!(persisted.static_dir_location, source.static_dir_location);
    Ok(())
}

#[tokio::test]
async fn static_reuse_cannot_activate_behind_a_newer_generation(
) -> Result<(), Box<dyn std::error::Error>> {
    let test_db = TestDatabase::with_migrations().await?;
    let db = test_db.connection_arc();
    let (project, environment, _) = setup_test_data(&db).await?;
    let mut active_project: projects::ActiveModel = project.clone().into();
    active_project.preset = Set(temps_entities::preset::Preset::Static);
    active_project.update(db.as_ref()).await?;

    let now = Utc::now();
    let candidate = deployments::ActiveModel {
        project_id: Set(project.id),
        environment_id: Set(environment.id),
        slug: Set("static-candidate".to_string()),
        state: Set("running".to_string()),
        metadata: Set(Some(
            temps_entities::deployments::DeploymentMetadata::default(),
        )),
        static_dir_location: Set(Some("cas/static/candidate".to_string())),
        created_at: Set(now),
        updated_at: Set(now),
        ..Default::default()
    }
    .insert(db.as_ref())
    .await?;
    deployments::ActiveModel {
        project_id: Set(project.id),
        environment_id: Set(environment.id),
        slug: Set("newer-static-candidate".to_string()),
        state: Set("running".to_string()),
        metadata: Set(Some(
            temps_entities::deployments::DeploymentMetadata::default(),
        )),
        static_dir_location: Set(Some("cas/static/newer".to_string())),
        created_at: Set(now + chrono::Duration::seconds(1)),
        updated_at: Set(now + chrono::Duration::seconds(1)),
        ..Default::default()
    }
    .insert(db.as_ref())
    .await?;

    let service = create_deployment_service_for_test(db.clone());
    let result = service
        .complete_static_reuse(&project, &environment, &candidate, now)
        .await;
    assert!(result.is_err());

    let persisted_candidate = deployments::Entity::find_by_id(candidate.id)
        .one(db.as_ref())
        .await?
        .ok_or("static candidate disappeared")?;
    assert_eq!(persisted_candidate.state, "stopped");
    let persisted_environment = environments::Entity::find_by_id(environment.id)
        .one(db.as_ref())
        .await?
        .ok_or("environment disappeared")?;
    assert_ne!(
        persisted_environment.current_deployment_id,
        Some(candidate.id)
    );
    Ok(())
}

#[tokio::test]
async fn static_post_completion_error_returns_success_when_release_is_live(
) -> Result<(), Box<dyn std::error::Error>> {
    let test_db = TestDatabase::with_migrations().await?;
    let db = test_db.connection_arc();
    let (project, environment, previous) = setup_test_data(&db).await?;
    let completed = deployments::ActiveModel {
        project_id: Set(project.id),
        environment_id: Set(environment.id),
        slug: Set("completed-static-reuse".to_string()),
        state: Set("completed".to_string()),
        metadata: Set(Some(
            temps_entities::deployments::DeploymentMetadata::default(),
        )),
        static_dir_location: Set(Some("cas/static/completed".to_string())),
        created_at: Set(Utc::now()),
        updated_at: Set(Utc::now()),
        ..Default::default()
    }
    .insert(db.as_ref())
    .await?;
    let mut active_environment: environments::ActiveModel = environment.into();
    active_environment.current_deployment_id = Set(Some(completed.id));
    active_environment.sleeping = Set(true);
    active_environment.update(db.as_ref()).await?;

    let service = create_deployment_service_for_test(db.clone());
    let mut event_receiver = service.queue_service.subscribe();
    service
        .reconcile_static_completion_error(
            completed.environment_id,
            completed.id,
            Some(previous.id),
            "project timestamp write failed",
        )
        .await?;

    let success_event = tokio::time::timeout(std::time::Duration::from_secs(2), async {
        loop {
            match event_receiver.recv().await {
                Ok(temps_core::Job::DeploymentSucceeded(event))
                    if event.deployment_id == completed.id =>
                {
                    break event;
                }
                Ok(_) => continue,
                Err(error) => panic!("queue closed before success event: {error}"),
            }
        }
    })
    .await?;
    assert_eq!(success_event.environment_id, completed.environment_id);

    let persisted_environment = environments::Entity::find_by_id(completed.environment_id)
        .one(db.as_ref())
        .await?
        .ok_or("environment disappeared")?;
    assert_eq!(
        persisted_environment.current_deployment_id,
        Some(completed.id)
    );
    assert!(
        !persisted_environment.sleeping,
        "a reconciled completed static release must be routable"
    );
    Ok(())
}

#[tokio::test]
async fn static_pre_completion_error_restores_previous_selected_release(
) -> Result<(), Box<dyn std::error::Error>> {
    let test_db = TestDatabase::with_migrations().await?;
    let db = test_db.connection_arc();
    let (project, environment, previous) = setup_test_data(&db).await?;
    let candidate = deployments::ActiveModel {
        project_id: Set(project.id),
        environment_id: Set(environment.id),
        slug: Set("incomplete-static-reuse".to_string()),
        state: Set("running".to_string()),
        metadata: Set(Some(
            temps_entities::deployments::DeploymentMetadata::default(),
        )),
        static_dir_location: Set(Some("cas/static/incomplete".to_string())),
        created_at: Set(Utc::now()),
        updated_at: Set(Utc::now()),
        ..Default::default()
    }
    .insert(db.as_ref())
    .await?;
    let mut active_environment: environments::ActiveModel = environment.into();
    active_environment.current_deployment_id = Set(Some(candidate.id));
    active_environment.update(db.as_ref()).await?;

    let service = create_deployment_service_for_test(db.clone());
    let result = service
        .reconcile_static_completion_error(
            candidate.environment_id,
            candidate.id,
            Some(previous.id),
            "route confirmation log failed",
        )
        .await;
    assert!(result.is_err());

    let persisted_environment = environments::Entity::find_by_id(candidate.environment_id)
        .one(db.as_ref())
        .await?
        .ok_or("environment disappeared")?;
    assert_eq!(
        persisted_environment.current_deployment_id,
        Some(previous.id)
    );
    Ok(())
}

#[tokio::test]
async fn reuse_setup_failure_persists_terminal_deployment_state(
) -> Result<(), Box<dyn std::error::Error>> {
    let test_db = TestDatabase::with_migrations().await?;
    let db = test_db.connection_arc();
    let (_project, _environment, deployment) = setup_test_data(&db).await?;
    let service = create_deployment_service_for_test(db.clone());

    let mut active_deployment: deployments::ActiveModel = deployment.into();
    active_deployment.state = Set("running".to_string());
    let deployment = active_deployment.update(db.as_ref()).await?;

    let error = service
        .fail_reuse_deployment(
            deployment.id,
            "Failed to configure remote worker",
            "worker registry pull failed",
        )
        .await;
    assert!(error.to_string().contains("worker registry pull failed"));

    let persisted = deployments::Entity::find_by_id(deployment.id)
        .one(db.as_ref())
        .await?
        .ok_or("failed deployment disappeared")?;
    assert_eq!(persisted.state, "failed");
    assert!(persisted.finished_at.is_some());
    assert!(persisted
        .cancelled_reason
        .as_deref()
        .is_some_and(
            |reason| reason.contains("Failed to configure remote worker")
                && reason.contains("worker registry pull failed")
        ));
    Ok(())
}

#[tokio::test]
async fn reuse_failure_preserves_existing_cancellation() -> Result<(), Box<dyn std::error::Error>> {
    let test_db = TestDatabase::with_migrations().await?;
    let db = test_db.connection_arc();
    let (_project, _environment, deployment) = setup_test_data(&db).await?;
    let mut active_deployment: deployments::ActiveModel = deployment.into();
    active_deployment.state = Set("cancelled".to_string());
    active_deployment.cancelled_reason = Set(Some("cancelled by user".to_string()));
    active_deployment.finished_at = Set(Some(Utc::now()));
    let deployment = active_deployment.update(db.as_ref()).await?;

    let service = create_deployment_service_for_test(db.clone());
    let error = service
        .fail_reuse_deployment(deployment.id, "Failed after cancellation", "worker stopped")
        .await;
    assert!(error
        .to_string()
        .contains("terminal state 'cancelled' was preserved"));

    let persisted = deployments::Entity::find_by_id(deployment.id)
        .one(db.as_ref())
        .await?
        .ok_or("cancelled deployment disappeared")?;
    assert_eq!(persisted.state, "cancelled");
    assert_eq!(
        persisted.cancelled_reason.as_deref(),
        Some("cancelled by user")
    );
    Ok(())
}

/// When the target deployment carries a git commit on a git-sourced
/// project AND the stored image is gone (pruned), rollback should rebuild
/// from source (enqueue a GitPushEvent) rather than fail. We assert it does
/// NOT take the image-reuse path: that path synchronously inserts a
/// brand-new deployment row (different id) and flips the environment
/// pointer. The rebuild path enqueues an async job, so within the test the
/// only deployments present are the originals — no extra image-reuse row.
#[tokio::test]
async fn test_rollback_rebuilds_pruned_local_git_image_from_recorded_source(
) -> Result<(), Box<dyn std::error::Error>> {
    let test_db = TestDatabase::with_migrations().await?;
    let db = test_db.connection_arc();

    // setup_test_data creates a Git-source project (SourceType default).
    let (_project, _environment, target_deployment) = setup_test_data(&db).await?;

    // A normal Git build stores a local-only image name. Once that image is
    // pruned, no worker can pull it from a registry, so the recorded commit
    // is the durable recovery source.
    let mut active: deployments::ActiveModel = target_deployment.clone().into();
    active.commit_sha = Set(Some("abc1234deadbeef".to_string()));
    active.branch_ref = Set(Some("main".to_string()));
    active.image_name = Set(Some(format!("temps-test-project:{}", target_deployment.id)));
    let target_deployment = active.update(db.as_ref()).await?;

    let count_before = deployments::Entity::find()
        .filter(deployments::Column::ProjectId.eq(target_deployment.project_id))
        .count(db.as_ref())
        .await?;

    // A local-only generated image reference must rebuild from source.
    let deployment_service = create_deployment_service_for_rebuild(db.clone());

    let result = deployment_service
        .rollback_to_deployment(target_deployment.project_id, target_deployment.id)
        .await?;

    // The image-reuse path would have inserted a new deployment row and
    // returned its (different) id. The rebuild path enqueues a job instead,
    // so no synchronous row is added and we get the target back as a
    // stand-in (the queued pipeline row isn't visible in-test).
    let count_after = deployments::Entity::find()
        .filter(deployments::Column::ProjectId.eq(target_deployment.project_id))
        .count(db.as_ref())
        .await?;
    assert_eq!(
        count_before, count_after,
        "rebuild-from-source must not synchronously create an image-reuse deployment"
    );
    assert_eq!(
        result.id, target_deployment.id,
        "rebuild path returns the target as a stand-in while the job is queued"
    );

    Ok(())
}

/// When the target deployment carries a git commit on a git-sourced project
/// AND the stored image reference remains available, rollback should REUSE
/// that image rather
/// than pay for a full rebuild from source. Reuse is near-instant and
/// byte-identical to the deployment we're rolling back to. We assert it
/// takes the image-reuse path: that path synchronously inserts a brand-new
/// rollback deployment row (a different id from the target) and returns it.
#[tokio::test]
async fn test_rollback_reuses_stored_image_for_git_projects(
) -> Result<(), Box<dyn std::error::Error>> {
    let test_db = TestDatabase::with_migrations().await?;
    let db = test_db.connection_arc();

    // setup_test_data creates a Git-source project (SourceType default)
    // with a non-static preset (NextJs) and image_name "nginx:latest".
    let (_project, _environment, target_deployment) = setup_test_data(&db).await?;

    // A git commit does not force a rebuild while the deployment still
    // carries a reusable image reference.
    let mut active: deployments::ActiveModel = target_deployment.clone().into();
    active.commit_sha = Set(Some("abc1234deadbeef".to_string()));
    active.branch_ref = Set(Some("main".to_string()));
    let target_deployment = active.update(db.as_ref()).await?;

    let count_before = deployments::Entity::find()
        .filter(deployments::Column::ProjectId.eq(target_deployment.project_id))
        .count(db.as_ref())
        .await?;

    // The stored image reference is sufficient; execution verifies it on
    // the selected node without consulting the control-plane cache.
    let deployment_service = create_deployment_service_for_test(db.clone());
    configure_test_service_for_http_readiness(&deployment_service).await?;

    let result = deployment_service
        .rollback_to_deployment(target_deployment.project_id, target_deployment.id)
        .await?;

    // The image-reuse path synchronously inserts a fresh rollback row, so
    // the count grows and the returned id differs from the target's. (The
    // rebuild path would have left the count unchanged and returned the
    // target as a stand-in.)
    let count_after = deployments::Entity::find()
        .filter(deployments::Column::ProjectId.eq(target_deployment.project_id))
        .count(db.as_ref())
        .await?;
    assert_eq!(
        count_before + 1,
        count_after,
        "image reuse must synchronously create a new rollback deployment"
    );
    assert_ne!(
        result.id, target_deployment.id,
        "image reuse returns the freshly-created rollback deployment, not the target"
    );

    Ok(())
}

/// Rollback and promotion reuse an existing image, so they hand the deploy
/// job its tag directly — which must not make every such image look
/// registry-sourced. The source is classified from the deployment that
/// produced the image, since the rollback/promotion rows in between record
/// no image metadata of their own.
#[tokio::test]
async fn test_reuse_image_provenance_classifies_from_origin_deployment(
) -> Result<(), Box<dyn std::error::Error>> {
    use crate::jobs::DeployImageSource;

    let test_db = match TestDatabase::with_migrations().await {
        Ok(db) => db,
        Err(e) => {
            println!("Docker/Postgres not available, skipping: {e}");
            return Ok(());
        }
    };
    let db = test_db.connection_arc();
    // A Git-source project whose deployment was built on the control plane.
    let (project, environment, origin) = setup_test_data(&db).await?;
    let service = create_deployment_service_for_test(db.clone());

    let local_build = origin.image_name.clone().unwrap_or_default();
    assert_eq!(
        service
            .reuse_image_provenance(&project, &origin, origin.id, &local_build)
            .await?
            .source,
        DeployImageSource::ControlPlaneLocal,
        "a control-plane git build must be streamed, never pulled"
    );

    // A rollback row, as `rollback_to_deployment_as` writes it: no image
    // metadata, pointing back at its origin.
    let rollback_row = deployments::ActiveModel {
        project_id: Set(project.id),
        environment_id: Set(environment.id),
        slug: Set(format!("{}-rollback", project.slug)),
        state: Set("completed".to_string()),
        metadata: Set(Some(temps_entities::deployments::DeploymentMetadata {
            is_rollback: true,
            rolled_back_from_id: Some(origin.id),
            ..Default::default()
        })),
        context_vars: Set(Some(serde_json::json!({
            "trigger": "rollback",
            "source_deployment_id": origin.id,
            "source_deployment_slug": origin.slug.clone(),
            "source_environment_id": origin.environment_id,
        }))),
        created_at: Set(Utc::now()),
        updated_at: Set(Utc::now()),
        ..Default::default()
    }
    .insert(db.as_ref())
    .await?;

    // External registry image at the origin → registry, even though the
    // rollback row alone (git project, no metadata) would read as local.
    let registry_image = "ghcr.io/example-org/app:v1";
    let mut active: deployments::ActiveModel = origin.clone().into();
    active.image_name = Set(Some(registry_image.to_string()));
    active.metadata = Set(Some(temps_entities::deployments::DeploymentMetadata {
        external_image_ref: Some(registry_image.to_string()),
        deployment_source_type: Some(temps_entities::source_type::SourceType::DockerImage),
        ..Default::default()
    }));
    let origin = active.update(db.as_ref()).await?;
    assert_eq!(
        service
            .reuse_image_provenance(&project, &rollback_row, origin.id, registry_image)
            .await?
            .source,
        DeployImageSource::Registry
    );
    assert_eq!(
        service
            .reuse_image_provenance(&project, &rollback_row, rollback_row.id, registry_image)
            .await?
            .source,
        DeployImageSource::ControlPlaneLocal,
        "without its origin the rollback row carries no evidence of a registry"
    );
    assert_eq!(
        service
            .reuse_image_provenance(&project, &rollback_row, origin.id, registry_image)
            .await?
            .expected_identity,
        None,
        "no registered external image: nothing to verify a local copy against"
    );

    // A registered external image with a recorded digest binds the reuse
    // to that exact image, so a re-pointed tag on the control plane is
    // never exported in its place.
    let external_image = temps_entities::external_images::ActiveModel {
        project_id: Set(project.id),
        image_ref: Set(registry_image.to_string()),
        digest: Set(Some("sha256:0d1e2f".to_string())),
        tag: Set(Some("v1".to_string())),
        pushed_at: Set(Utc::now()),
        created_at: Set(Utc::now()),
        ..Default::default()
    }
    .insert(db.as_ref())
    .await?;
    let mut active: deployments::ActiveModel = origin.clone().into();
    active.metadata = Set(Some(temps_entities::deployments::DeploymentMetadata {
        external_image_ref: Some(registry_image.to_string()),
        external_image_id: Some(external_image.id),
        deployment_source_type: Some(temps_entities::source_type::SourceType::DockerImage),
        ..Default::default()
    }));
    let origin = active.update(db.as_ref()).await?;
    let provenance = service
        .reuse_image_provenance(&project, &rollback_row, origin.id, registry_image)
        .await?;
    assert_eq!(provenance.source, DeployImageSource::Registry);
    assert_eq!(
        provenance.expected_identity,
        Some(crate::jobs::ExpectedImageIdentity::RepoDigest(
            "sha256:0d1e2f".to_string()
        ))
    );

    // Uploaded image at the origin → control-plane-local, even though the
    // upload handler also records it as a docker_image deployment with an
    // external_image_ref.
    let uploaded_image = format!(
        "temps.internal/project-{}/environment-{}/upload-0f3c9a:immutable",
        project.id, environment.id
    );
    let mut active: deployments::ActiveModel = origin.clone().into();
    active.image_name = Set(Some(uploaded_image.clone()));
    active.metadata = Set(Some(temps_entities::deployments::DeploymentMetadata {
        external_image_ref: Some(uploaded_image.clone()),
        deployment_source_type: Some(temps_entities::source_type::SourceType::DockerImage),
        image_uploaded_locally: true,
        uploaded_image_id: Some("sha256:0f3c9a".to_string()),
        ..Default::default()
    }));
    let origin = active.update(db.as_ref()).await?;
    assert_eq!(
        service
            .reuse_image_provenance(&project, &rollback_row, origin.id, &uploaded_image)
            .await?
            .source,
        DeployImageSource::ControlPlaneLocal,
        "an uploaded image must be streamed from the control plane, never pulled"
    );

    Ok(())
}

#[tokio::test]
async fn test_rollback_to_deployment_invalid_state() -> Result<(), Box<dyn std::error::Error>> {
    let test_db = TestDatabase::with_migrations().await?;
    let db = test_db.connection_arc();

    // Setup test data
    let (_project, environment, mut target_deployment) = setup_test_data(&db).await?;

    // Update the deployment state to "failed" to make it invalid for rollback
    let mut active_deployment: deployments::ActiveModel = target_deployment.into();
    active_deployment.state = Set("failed".to_string());
    target_deployment = active_deployment.update(db.as_ref()).await?;

    let deployment_service = create_deployment_service_for_test(db.clone());

    // Test rollback to invalid deployment state
    let result = deployment_service
        .rollback_to_deployment(target_deployment.project_id, target_deployment.id)
        .await;

    // Verify error is thrown
    assert!(result.is_err());
    match result.unwrap_err() {
        DeploymentError::InvalidDeploymentState(msg) => {
            assert!(msg.contains("failed"));
            assert!(msg.contains("deployed"));
        }
        e => panic!("Expected InvalidDeploymentState error, got: {:?}", e),
    }

    // A second, distinct invalid state must still be rejected too --
    // this isn't just re-asserting "failed" from above. Reuse the same
    // project/environment (rather than calling `setup_test_data` again,
    // which would collide on its hard-coded project slug) with a fresh
    // deployment row.
    let other_target = deployments::ActiveModel {
        project_id: Set(target_deployment.project_id),
        environment_id: Set(environment.id),
        slug: Set("test-deployment-creating".to_string()),
        state: Set("creating".to_string()),
        metadata: Set(Some(
            temps_entities::deployments::DeploymentMetadata::default(),
        )),
        created_at: Set(Utc::now()),
        updated_at: Set(Utc::now()),
        ..Default::default()
    };
    let other_target = other_target.insert(db.as_ref()).await?;

    let result = deployment_service
        .rollback_to_deployment(other_target.project_id, other_target.id)
        .await;
    assert!(result.is_err());
    match result.unwrap_err() {
        DeploymentError::InvalidDeploymentState(msg) => {
            assert!(msg.contains("creating"));
        }
        e => panic!("Expected InvalidDeploymentState error, got: {:?}", e),
    }

    Ok(())
}

// Regression coverage for `valid_rollback_states` gaining "stopped":
// the only pre-existing invalid-state test above only ever asserted on
// "failed" being rejected, so a regression that dropped "stopped" from
// the allow-list (reintroducing "Cannot rollback to deployment in
// 'stopped' state" for the primary real-world rollback target -- a
// superseded, previously-successful deployment) would pass every
// existing test in this file.
#[tokio::test]
async fn test_rollback_to_deployment_accepts_stopped_state(
) -> Result<(), Box<dyn std::error::Error>> {
    let test_db = TestDatabase::with_migrations().await?;
    let db = test_db.connection_arc();

    // Setup test data
    let (_project, mut environment, target_deployment) = setup_test_data(&db).await?;
    setup_test_environment_variables(&db, target_deployment.project_id, environment.id).await?;

    // Create container for target deployment (required for rollback)
    let now = Utc::now();
    let target_container = deployment_containers::ActiveModel {
        deployment_id: Set(target_deployment.id),
        container_id: Set("container-rollback-stopped-target".to_string()),
        container_name: Set("app-rollback-stopped-target".to_string()),
        container_port: Set(Some(8080)),
        image_name: Set(Some("nginx:target".to_string())),
        status: Set(Some("stopped".to_string())),
        created_at: Set(now),
        deployed_at: Set(now),
        ..Default::default()
    };
    target_container.insert(db.as_ref()).await?;

    // This is the state `cancel_previous_deployments`/`teardown_deployment`
    // actually leave a superseded-but-successful deployment in -- the
    // primary real-world rollback target.
    let mut active_target: deployments::ActiveModel = target_deployment.into();
    active_target.state = Set("stopped".to_string());
    let target_deployment = active_target.update(db.as_ref()).await?;

    // Create current deployment that will be stopped by the rollback
    let current_deployment = deployments::ActiveModel {
        project_id: Set(target_deployment.project_id),
        environment_id: Set(environment.id),
        slug: Set("current-deployment-for-stopped-rollback".to_string()),
        state: Set("deployed".to_string()),
        metadata: Set(Some(
            temps_entities::deployments::DeploymentMetadata::default(),
        )),
        image_name: Set(Some("nginx:current".to_string())),
        created_at: Set(Utc::now()),
        updated_at: Set(Utc::now()),
        ..Default::default()
    };
    let current_deployment = current_deployment.insert(db.as_ref()).await?;

    let current_container = deployment_containers::ActiveModel {
        deployment_id: Set(current_deployment.id),
        container_id: Set("container-rollback-stopped-current".to_string()),
        container_name: Set("app-rollback-stopped-current".to_string()),
        container_port: Set(Some(8080)),
        image_name: Set(Some("nginx:current".to_string())),
        status: Set(Some("running".to_string())),
        created_at: Set(now),
        deployed_at: Set(now),
        ..Default::default()
    };
    current_container.insert(db.as_ref()).await?;

    let mut active_environment: environments::ActiveModel = environment.into();
    active_environment.current_deployment_id = Set(Some(current_deployment.id));
    environment = active_environment.update(db.as_ref()).await?;

    let deployment_service = create_deployment_service_for_test(db.clone());
    // Phase 2.75 probes the public URL over HTTP via the test proxy.
    // Without this the URL scheme defaults to "https", which causes the
    // TLS handshake to fail against the plain-HTTP test proxy and the
    // readiness gate times out rather than succeeding.
    configure_test_service_for_http_readiness(&deployment_service).await?;

    // Rollback to a "stopped" target must succeed, not bounce off the
    // InvalidDeploymentState guard.
    let result = deployment_service
        .rollback_to_deployment(target_deployment.project_id, target_deployment.id)
        .await?;

    assert_ne!(result.id, target_deployment.id);
    assert!(result.is_current);

    let updated_environment = environments::Entity::find_by_id(environment.id)
        .one(db.as_ref())
        .await?
        .unwrap();
    assert_eq!(updated_environment.current_deployment_id, Some(result.id));

    Ok(())
}

/// Minimal service for the source-rebuild path, which has no container job.
fn create_deployment_service_for_rebuild(
    db: Arc<temps_database::DbConnection>,
) -> DeploymentService {
    let log_service = Arc::new(temps_deployment_logs::LogService::new(std::env::temp_dir()));
    let test_db_url = "postgresql://test_user:test_password@localhost:5432/test_db";
    let server_config = Arc::new(
        temps_config::ServerConfig::new(
            "127.0.0.1:8080".to_string(),
            test_db_url.to_string(),
            None,
            None,
        )
        .expect("Failed to create test server config"),
    );
    let config_service = Arc::new(temps_config::ConfigService::new(server_config, db.clone()));

    let mut queue_service = MockQueueService::new();
    queue_service.expect_send().returning(|_| Ok(()));
    queue_service
        .expect_subscribe()
        .returning(|| Box::new(MockJobReceiver::new()));
    let queue_service: Arc<dyn temps_core::JobQueue> = Arc::new(queue_service);

    let docker = Arc::new(bollard::Docker::connect_with_local_defaults().unwrap());
    let docker_handle = Arc::new(temps_core::DockerHandle::available(docker));
    let docker_log_service = Arc::new(temps_deployment_logs::DockerLogService::new(
        docker_handle.clone(),
    ));

    let mut deployer = MockContainerDeployer::new();
    deployer.expect_deploy_container().returning(|_| {
        Ok(temps_deployer::DeployResult {
            container_id: "test-container".to_string(),
            container_name: "test-container".to_string(),
            container_port: 3000,
            host_port: 3000,
            status: temps_deployer::ContainerStatus::Running,
            docker_socket_mounted: false,
        })
    });
    deployer.expect_stop_container().returning(|_| Ok(()));
    deployer.expect_remove_container().returning(|_| Ok(()));
    deployer
        .expect_get_container_info()
        .returning(|container_id| {
            Ok(temps_deployer::ContainerInfo {
                container_id: container_id.to_string(),
                container_name: "test-container".to_string(),
                image_name: "nginx:latest".to_string(),
                created_at: chrono::Utc::now(),
                status: temps_deployer::ContainerStatus::Running,
                ..Default::default()
            })
        });
    deployer
        .expect_list_containers()
        .returning(|| Ok(Vec::new()));
    deployer
        .expect_stream_container_logs()
        .returning(|_| Ok(Box::new(futures::stream::empty())));
    deployer.expect_image_exists().times(0);
    let deployer: Arc<dyn temps_deployer::ContainerDeployer> = Arc::new(deployer);

    DeploymentService {
        db,
        log_service,
        config_service,
        queue_service,
        docker_log_service,
        docker_handle,
        deployer,
        image_builder: Arc::new(NoopImageBuilder),
        encryption_service: create_test_encryption_service(),
        telemetry: std::sync::OnceLock::new(),
        environment_addresses: std::sync::OnceLock::new(),
        env_resolver: std::sync::OnceLock::new(),
        compose_executor: std::sync::OnceLock::new(),
        audit_logger: std::sync::OnceLock::new(),
        node_scheduler: std::sync::OnceLock::new(),
        workflow_planner: std::sync::OnceLock::new(),
        workload_plane: std::sync::OnceLock::new(),
    }
}

#[tokio::test]
async fn test_rollback_does_not_probe_control_plane_image_cache(
) -> Result<(), Box<dyn std::error::Error>> {
    let test_db = TestDatabase::with_migrations().await?;
    let db = test_db.connection_arc();

    // Setup test data (creates a non-static project with a deployed deployment)
    let (_project, _environment, target_deployment) = setup_test_data(&db).await?;

    // The deployer reports that the image is absent on the control plane.
    // Artifact reuse must still proceed because the selected worker owns
    // verification/pull of this registry image.
    let deployment_service = create_deployment_service_for_test(db.clone());
    configure_test_service_for_http_readiness(&deployment_service).await?;

    let result = deployment_service
        .rollback_to_deployment(target_deployment.project_id, target_deployment.id)
        .await?;

    assert_ne!(result.id, target_deployment.id);
    let persisted = deployments::Entity::find_by_id(result.id)
        .one(db.as_ref())
        .await?
        .ok_or("rollback deployment was not persisted")?;
    assert_eq!(persisted.image_name, target_deployment.image_name);
    assert_eq!(persisted.state, "completed");

    Ok(())
}

#[tokio::test]
async fn failed_rollback_keeps_current_deployment_active() -> Result<(), Box<dyn std::error::Error>>
{
    let test_db = TestDatabase::with_migrations().await?;
    let db = test_db.connection_arc();
    let (project, environment, target_deployment) = setup_test_data(&db).await?;

    let current_deployment = deployments::ActiveModel {
        project_id: Set(project.id),
        environment_id: Set(environment.id),
        slug: Set("current-deployment-before-failed-rollback".to_string()),
        state: Set("completed".to_string()),
        metadata: Set(Some(
            temps_entities::deployments::DeploymentMetadata::default(),
        )),
        image_name: Set(Some("nginx:current".to_string())),
        created_at: Set(Utc::now()),
        updated_at: Set(Utc::now()),
        ..Default::default()
    }
    .insert(db.as_ref())
    .await?;
    let mut active_environment: environments::ActiveModel = environment.clone().into();
    active_environment.current_deployment_id = Set(Some(current_deployment.id));
    active_environment.update(db.as_ref()).await?;

    let mut failing_deployer = MockContainerDeployer::new();
    failing_deployer
        .expect_deploy_container()
        .times(1)
        .returning(|_| {
            Err(temps_deployer::DeployerError::DeploymentFailed(
                "candidate failed to start".to_string(),
            ))
        });
    failing_deployer.expect_image_exists().times(0);

    let mut service = create_deployment_service_for_test(db.clone());
    service.deployer = Arc::new(failing_deployer);
    let result = service
        .rollback_to_deployment(project.id, target_deployment.id)
        .await;
    assert!(result.is_err());

    let persisted_environment = environments::Entity::find_by_id(environment.id)
        .one(db.as_ref())
        .await?
        .ok_or("environment disappeared")?;
    assert_eq!(
        persisted_environment.current_deployment_id,
        Some(current_deployment.id),
        "a failed candidate must not replace the active deployment"
    );
    let persisted_current = deployments::Entity::find_by_id(current_deployment.id)
        .one(db.as_ref())
        .await?
        .ok_or("current deployment disappeared")?;
    assert_eq!(
        persisted_current.state, "completed",
        "a failed candidate must not stop the active deployment"
    );

    Ok(())
}

#[tokio::test]
async fn test_teardown_deployment() -> Result<(), Box<dyn std::error::Error>> {
    let test_db = TestDatabase::with_migrations().await?;
    let db = test_db.connection_arc();

    // Setup test data
    let (_project, _environment, deployment) = setup_test_data(&db).await?;

    let deployment_service = create_deployment_service_for_test(db.clone());

    // Test teardown deployment
    deployment_service
        .teardown_deployment(deployment.project_id, deployment.id)
        .await?;

    // Verify deployment state was updated
    let updated_deployment = deployments::Entity::find_by_id(deployment.id)
        .one(db.as_ref())
        .await?
        .unwrap();
    assert_eq!(updated_deployment.state, "stopped");

    Ok(())
}

#[tokio::test]
async fn test_teardown_environment() -> Result<(), Box<dyn std::error::Error>> {
    let test_db = TestDatabase::with_migrations().await?;
    let db = test_db.connection_arc();

    // Setup test data with multiple deployments
    let (_project, environment, deployment1) = setup_test_data(&db).await?;

    // Create second deployment in same environment
    let deployment2 = deployments::ActiveModel {
        project_id: Set(deployment1.project_id),
        environment_id: Set(environment.id),
        slug: Set("deployment2-456".to_string()),
        state: Set("deployed".to_string()),
        metadata: Set(Some(
            temps_entities::deployments::DeploymentMetadata::default(),
        )),
        created_at: Set(Utc::now()),
        updated_at: Set(Utc::now()),
        ..Default::default()
    };
    let deployment2 = deployment2.insert(db.as_ref()).await?;

    let deployment_service = create_deployment_service_for_test(db.clone());

    // Test teardown environment
    deployment_service
        .teardown_environment(deployment1.project_id, environment.id)
        .await?;

    // Verify both deployments were stopped
    let updated_deployment1 = deployments::Entity::find_by_id(deployment1.id)
        .one(db.as_ref())
        .await?
        .unwrap();
    assert_eq!(updated_deployment1.state, "stopped");

    let updated_deployment2 = deployments::Entity::find_by_id(deployment2.id)
        .one(db.as_ref())
        .await?
        .unwrap();
    assert_eq!(updated_deployment2.state, "stopped");

    Ok(())
}

#[tokio::test]
async fn test_get_last_deployment_distinguishes_empty_project_from_missing_project(
) -> Result<(), Box<dyn std::error::Error>> {
    if !database_integration_tests_available().await {
        eprintln!("Docker unavailable; skipping get_last_deployment integration test");
        return Ok(());
    }
    let test_db = TestDatabase::with_migrations().await?;
    let db = test_db.connection_arc();
    let service = create_deployment_service_for_test(db.clone());

    // Project with a deployment -> Some(latest).
    let (project, _environment, deployment) = setup_test_data(&db).await?;
    let last = service
        .get_last_deployment(project.id)
        .await?
        .expect("project with a deployment must return it");
    assert_eq!(last.id, deployment.id);

    // Project that exists but was never deployed -> Ok(None).
    let empty_project = projects::ActiveModel {
        name: Set("Never Deployed".to_string()),
        slug: Set("never-deployed".to_string()),
        repo_owner: Set("test-owner".to_string()),
        repo_name: Set("test-repo-empty".to_string()),
        main_branch: Set("main".to_string()),
        preset: Set(Preset::NextJs),
        directory: Set("/".to_string()),
        ..Default::default()
    }
    .insert(db.as_ref())
    .await?;
    assert!(service
        .get_last_deployment(empty_project.id)
        .await?
        .is_none());

    // A soft-deleted project keeps its deployment rows but reads as not found.
    let mut deleted: projects::ActiveModel = projects::Entity::find_by_id(project.id)
        .one(db.as_ref())
        .await?
        .expect("project exists")
        .into();
    deleted.is_deleted = Set(true);
    deleted.update(db.as_ref()).await?;
    let result = service.get_last_deployment(project.id).await;
    assert!(matches!(
        result,
        Err(DeploymentError::NotFound(message))
            if message == format!("project {} not found", project.id)
    ));

    // Unknown project -> NotFound naming the project.
    let missing_id = empty_project.id + 10_000;
    let result = service.get_last_deployment(missing_id).await;
    assert!(matches!(
        result,
        Err(DeploymentError::NotFound(message))
            if message == format!("project {missing_id} not found")
    ));
    Ok(())
}

#[tokio::test]
async fn test_deployment_not_found() -> Result<(), Box<dyn std::error::Error>> {
    let test_db = TestDatabase::with_migrations().await?;
    let db = test_db.connection_arc();

    let deployment_service = create_deployment_service_for_test(db);

    // Test with non-existent deployment
    let result = deployment_service.pause_deployment(999, 999).await;
    assert!(result.is_err());

    if let Err(DeploymentError::NotFound(_)) = result {
        // Expected error type
    } else {
        panic!("Expected NotFound error");
    }

    Ok(())
}

#[tokio::test]
async fn test_deployment_without_container() -> Result<(), Box<dyn std::error::Error>> {
    let test_db = TestDatabase::with_migrations().await?;
    let db = test_db.connection_arc();

    // Setup test data
    let (_project, _environment, deployment) = setup_test_data(&db).await?;

    // Note: container_id field no longer exists after workflow refactoring

    let deployment_service = create_deployment_service_for_test(db.clone());

    // Test pause deployment without container - should succeed but not call stop_containers
    deployment_service
        .pause_deployment(deployment.project_id, deployment.id)
        .await?;

    // Verify deployment state was still updated
    let updated_deployment = deployments::Entity::find_by_id(deployment.id)
        .one(db.as_ref())
        .await?
        .unwrap();
    assert_eq!(updated_deployment.state, "paused");

    Ok(())
}

#[tokio::test]
async fn test_deployment_jobs_creation() -> Result<(), Box<dyn std::error::Error>> {
    use crate::services::workflow_planner::WorkflowPlanner;
    use temps_entities::deployment_jobs;

    let test_db = TestDatabase::with_migrations().await?;
    let db = test_db.connection_arc();
    let log_service = Arc::new(temps_deployment_logs::LogService::new(std::env::temp_dir()));
    // Setup test data
    let (_project, _environment, deployment) = setup_test_data(&db).await?;
    // Create config service
    let server_config = Arc::new(
        temps_config::ServerConfig::new(
            "127.0.0.1:3000".to_string(),
            "postgresql://test".to_string(),
            None,
            Some("127.0.0.1:8000".to_string()),
        )
        .unwrap(),
    );
    let config_service = Arc::new(temps_config::ConfigService::new(server_config, db.clone()));
    // Create workflow planner
    let dsn_service = Arc::new(temps_error_tracking::DSNService::new(db.clone()));
    let external_service_manager = create_test_external_service_manager(db.clone());
    let workflow_planner = WorkflowPlanner::new(
        db.clone(),
        log_service.clone(),
        external_service_manager.clone(),
        config_service,
        dsn_service,
        create_test_encryption_service(),
    );

    // Create deployment jobs using workflow planner
    let created_jobs = workflow_planner
        .create_deployment_jobs(
            deployment.id,
            temps_core::docker_socket_grant::DeployCaller::Platform,
        )
        .await?;

    // Verify jobs were created
    assert!(
        !created_jobs.is_empty(),
        "Should have created at least one job"
    );

    // Verify jobs are in database
    let db_jobs = deployment_jobs::Entity::find()
        .filter(deployment_jobs::Column::DeploymentId.eq(deployment.id))
        .all(db.as_ref())
        .await?;

    assert_eq!(
        db_jobs.len(),
        created_jobs.len(),
        "Number of jobs in DB should match created jobs"
    );

    // Verify job properties
    for job in &db_jobs {
        assert_eq!(job.deployment_id, deployment.id);
        assert!(!job.job_id.is_empty(), "Job ID should not be empty");
        assert!(!job.job_type.is_empty(), "Job type should not be empty");
        assert!(!job.name.is_empty(), "Job name should not be empty");
        assert_eq!(job.status, temps_entities::types::JobStatus::Pending);

        // Verify execution order was set
        assert!(
            job.execution_order.is_some(),
            "Execution order should be set"
        );
    }
    // Verify first job is download_repo (for projects with git info)
    let first_job = db_jobs.first().expect("Should have at least one job");
    assert_eq!(first_job.job_id, "download_repo");
    assert_eq!(first_job.job_type, "DownloadRepoJob");

    // Verify job has no dependencies (should be first)
    assert!(
        first_job.dependencies.is_none()
            || first_job
                .dependencies
                .as_ref()
                .unwrap()
                .as_array()
                .unwrap()
                .is_empty(),
        "First job should have no dependencies"
    );

    Ok(())
}

#[tokio::test]
async fn test_deployment_jobs_with_log_ids() -> Result<(), Box<dyn std::error::Error>> {
    use crate::services::workflow_planner::WorkflowPlanner;

    let test_db = TestDatabase::with_migrations().await?;
    let db = test_db.connection_arc();
    let log_service = Arc::new(temps_deployment_logs::LogService::new(std::env::temp_dir()));
    // Setup test data
    let (_project, _environment, deployment) = setup_test_data(&db).await?;

    // Create config service
    let server_config = Arc::new(
        temps_config::ServerConfig::new(
            "127.0.0.1:3000".to_string(),
            "postgresql://test".to_string(),
            None,
            Some("127.0.0.1:8000".to_string()),
        )
        .unwrap(),
    );
    let config_service = Arc::new(temps_config::ConfigService::new(server_config, db.clone()));

    // Create workflow planner
    let dsn_service = Arc::new(temps_error_tracking::DSNService::new(db.clone()));
    let external_service_manager = create_test_external_service_manager(db.clone());
    let workflow_planner = WorkflowPlanner::new(
        db.clone(),
        log_service.clone(),
        external_service_manager.clone(),
        config_service,
        dsn_service,
        create_test_encryption_service(),
    );

    // Create deployment jobs
    let created_jobs = workflow_planner
        .create_deployment_jobs(
            deployment.id,
            temps_core::docker_socket_grant::DeployCaller::Platform,
        )
        .await?;

    // Verify each job can be used to generate a log_id
    for job in &created_jobs {
        let log_id = format!("deployment-{}-job-{}", deployment.id, job.job_id);

        // Log IDs should be unique and well-formed
        assert!(!log_id.is_empty());
        assert!(log_id.starts_with(&format!("deployment-{}", deployment.id)));
        assert!(log_id.contains(&job.job_id));

        println!("Job '{}' has log_id: {}", job.name, log_id);
    }

    Ok(())
}

#[tokio::test]
async fn test_list_environment_containers() -> Result<(), Box<dyn std::error::Error>> {
    use temps_entities::deployment_containers;

    let test_db = TestDatabase::with_migrations().await?;
    let db = test_db.connection_arc();

    // Setup test data
    let (_project, mut environment, deployment) = setup_test_data(&db).await?;

    // Update environment to have current deployment
    let mut active_environment: environments::ActiveModel = environment.into();
    active_environment.current_deployment_id = Set(Some(deployment.id));
    environment = active_environment.update(db.as_ref()).await?;

    // Create deployment_containers entries
    let now = Utc::now();
    let container1 = deployment_containers::ActiveModel {
        deployment_id: Set(deployment.id),
        container_id: Set("container-123".to_string()),
        container_name: Set("test-container-1".to_string()),
        container_port: Set(Some(8080)),
        image_name: Set(Some("nginx:latest".to_string())),
        status: Set(Some("running".to_string())),
        created_at: Set(now),
        deployed_at: Set(now),
        ..Default::default()
    };
    container1.insert(db.as_ref()).await?;

    let container2 = deployment_containers::ActiveModel {
        deployment_id: Set(deployment.id),
        container_id: Set("container-456".to_string()),
        container_name: Set("test-container-2".to_string()),
        container_port: Set(Some(5432)),
        image_name: Set(Some("postgres:15".to_string())),
        status: Set(Some("running".to_string())),
        created_at: Set(now),
        deployed_at: Set(now),
        ..Default::default()
    };
    container2.insert(db.as_ref()).await?;

    let deployment_service = create_deployment_service_for_test(db.clone());

    // Test list containers
    let containers = deployment_service
        .list_environment_containers(deployment.project_id, environment.id)
        .await?;

    // Verify we got container info (mocked deployer returns container info)
    assert_eq!(containers.len(), 2, "Should return 2 containers");

    Ok(())
}

#[tokio::test]
async fn test_list_environment_containers_no_deployment() -> Result<(), Box<dyn std::error::Error>>
{
    let test_db = TestDatabase::with_migrations().await?;
    let db = test_db.connection_arc();

    // Setup test data without current deployment
    let (project, environment, _deployment) = setup_test_data(&db).await?;

    let deployment_service = create_deployment_service_for_test(db.clone());

    // Test list containers - should return empty for no active deployment
    let containers = deployment_service
        .list_environment_containers(project.id, environment.id)
        .await?;

    assert_eq!(
        containers.len(),
        0,
        "Should return no containers when no active deployment"
    );

    Ok(())
}

#[tokio::test]
async fn test_get_container_logs_by_id_validation() -> Result<(), Box<dyn std::error::Error>> {
    use temps_entities::deployment_containers;

    let test_db = TestDatabase::with_migrations().await?;
    let db = test_db.connection_arc();

    // Setup test data
    let (_project, mut environment, deployment) = setup_test_data(&db).await?;

    // Update environment to have current deployment
    let mut active_environment: environments::ActiveModel = environment.into();
    active_environment.current_deployment_id = Set(Some(deployment.id));
    environment = active_environment.update(db.as_ref()).await?;

    // Create a container for the deployment
    let now = Utc::now();
    let container = deployment_containers::ActiveModel {
        deployment_id: Set(deployment.id),
        container_id: Set("valid-container-id".to_string()),
        container_name: Set("test-container".to_string()),
        container_port: Set(Some(8080)),
        image_name: Set(Some("nginx:latest".to_string())),
        status: Set(Some("running".to_string())),
        created_at: Set(now),
        deployed_at: Set(now),
        ..Default::default()
    };
    container.insert(db.as_ref()).await?;

    let deployment_service = create_deployment_service_for_test(db.clone());

    // Test with invalid container ID - should fail
    let result = deployment_service
        .get_container_logs_by_id(
            deployment.project_id,
            environment.id,
            "invalid-container-id".to_string(),
            ContainerLogParams {
                start_date: None,
                end_date: None,
                tail: None,
                timestamps: false,
                follow: false,
            },
        )
        .await;

    assert!(result.is_err(), "Should fail with invalid container ID");
    match result {
        Err(DeploymentError::NotFound(msg)) => {
            assert!(
                msg.contains("Container"),
                "Error should mention container not found"
            );
        }
        _ => panic!("Expected NotFound error"),
    }

    Ok(())
}

#[tokio::test]
async fn retained_failed_compose_container_passes_log_ownership_lookup(
) -> Result<(), Box<dyn std::error::Error>> {
    use temps_entities::deployment_containers;

    let test_db = TestDatabase::with_migrations().await?;
    let db = test_db.connection_arc();
    let (project, environment, current_deployment) = setup_test_data(&db).await?;

    let mut active_environment: environments::ActiveModel = environment.clone().into();
    active_environment.current_deployment_id = Set(Some(current_deployment.id));
    active_environment.update(db.as_ref()).await?;

    let failed_deployment = deployments::ActiveModel {
        project_id: Set(project.id),
        environment_id: Set(environment.id),
        slug: Set("failed-compose-candidate".to_string()),
        state: Set("failed".to_string()),
        metadata: Set(Some(
            temps_entities::deployments::DeploymentMetadata::default(),
        )),
        created_at: Set(Utc::now()),
        updated_at: Set(Utc::now()),
        ..Default::default()
    }
    .insert(db.as_ref())
    .await?;
    deployment_containers::ActiveModel {
        deployment_id: Set(failed_deployment.id),
        container_id: Set("retained-failed-container".to_string()),
        container_name: Set("temps-retained-app-1".to_string()),
        container_port: Set(Some(80)),
        image_name: Set(Some("nginx:alpine".to_string())),
        status: Set(Some("retained:running".to_string())),
        service_name: Set(Some("app".to_string())),
        created_at: Set(Utc::now()),
        deployed_at: Set(Utc::now()),
        ready_at: Set(None),
        deleted_at: Set(None),
        ..Default::default()
    }
    .insert(db.as_ref())
    .await?;

    let deployment_service = create_deployment_service_for_test(db.clone());
    let result = deployment_service
        .get_container_logs_by_id(
            project.id,
            environment.id,
            "retained-failed-container".to_string(),
            ContainerLogParams {
                start_date: None,
                end_date: None,
                tail: Some("10".to_string()),
                timestamps: false,
                follow: false,
            },
        )
        .await;

    assert!(
        !matches!(result, Err(DeploymentError::NotFound(_))),
        "a live retained container must pass project/environment ownership lookup even when another deployment is current"
    );

    Ok(())
}

#[tokio::test]
async fn test_list_containers_not_server_project() -> Result<(), Box<dyn std::error::Error>> {
    let test_db = TestDatabase::with_migrations().await?;
    let db = test_db.connection_arc();

    // Create a non-server project (static site)
    let project = projects::ActiveModel {
        name: Set("Static Site".to_string()),
        slug: Set("static-site".to_string()),
        repo_name: Set("static-site-repo".to_string()),
        repo_owner: Set("test-owner".to_string()),
        preset: Set(Preset::Static), // Static preset doesn't require a server
        main_branch: Set("main".to_string()),
        directory: Set("/".to_string()),
        created_at: Set(Utc::now()),
        updated_at: Set(Utc::now()),
        ..Default::default()
    };
    let project = project.insert(db.as_ref()).await?;

    // Create environment
    let environment = environments::ActiveModel {
        project_id: Set(project.id),
        name: Set("Test".to_string()),
        slug: Set("test".to_string()),
        host: Set("test.example.com".to_string()),
        upstreams: Set(UpstreamList::default()),
        subdomain: Set("test.example.com".to_string()),
        created_at: Set(Utc::now()),
        updated_at: Set(Utc::now()),
        ..Default::default()
    };
    let environment = environment.insert(db.as_ref()).await?;

    let deployment_service = create_deployment_service_for_test(db.clone());

    // Test list containers on non-server project - should fail
    let result = deployment_service
        .list_environment_containers(project.id, environment.id)
        .await;

    assert!(result.is_err(), "Should fail for non-server projects");
    match result {
        Err(DeploymentError::Other(msg)) => {
            assert!(
                msg.contains("server-type"),
                "Error should mention server-type projects"
            );
        }
        _ => panic!("Expected Other error"),
    }

    Ok(())
}

#[tokio::test]
async fn test_get_container_detail_success() -> Result<(), Box<dyn std::error::Error>> {
    let test_db = TestDatabase::with_migrations().await?;
    let db = test_db.connection_arc();

    // Setup: Create project, environment, deployment, and container
    let (project, environment, _deployment, container) = setup_test_deployment(&db).await?;

    let deployment_service = create_deployment_service_for_test(db.clone());

    // Test: Get container detail
    let (result_container, result_env) = deployment_service
        .get_container_detail(project.id, environment.id, container.container_id.clone())
        .await?;

    // Verify container details
    assert_eq!(result_container.id, container.id);
    assert_eq!(result_container.container_id, "container-123");
    assert_eq!(result_container.container_name, "test-container-1");
    assert_eq!(result_container.status, Some("running".to_string()));

    // Verify environment info
    assert_eq!(result_env.id, environment.id);
    assert_eq!(result_env.name, environment.name);

    Ok(())
}

#[tokio::test]
async fn test_get_container_detail_not_found() -> Result<(), Box<dyn std::error::Error>> {
    let test_db = TestDatabase::with_migrations().await?;
    let db = test_db.connection_arc();

    // Setup: Create project and environment (no container)
    let project = projects::ActiveModel {
        name: Set("Test Project".to_string()),
        slug: Set("test-project".to_string()),
        repo_name: Set("test-repo".to_string()),
        repo_owner: Set("test-owner".to_string()),
        preset: Set(Preset::NextJs),
        main_branch: Set("main".to_string()),
        directory: Set("/".to_string()),
        created_at: Set(Utc::now()),
        updated_at: Set(Utc::now()),
        ..Default::default()
    };
    let project = project.insert(db.as_ref()).await?;

    let environment = environments::ActiveModel {
        project_id: Set(project.id),
        name: Set("Production".to_string()),
        slug: Set("prod".to_string()),
        host: Set("prod.example.com".to_string()),
        upstreams: Set(UpstreamList::default()),
        subdomain: Set("prod.example.com".to_string()),
        created_at: Set(Utc::now()),
        updated_at: Set(Utc::now()),
        ..Default::default()
    };
    let environment = environment.insert(db.as_ref()).await?;

    let deployment_service = create_deployment_service_for_test(db.clone());

    // Test: Try to get non-existent container
    let result = deployment_service
        .get_container_detail(project.id, environment.id, "non-existent".to_string())
        .await;

    assert!(result.is_err(), "Should fail when container not found");
    match result {
        Err(DeploymentError::NotFound(msg)) => {
            assert!(msg.contains("Container"));
        }
        _ => panic!("Expected NotFound error"),
    }

    Ok(())
}

#[tokio::test]
async fn test_list_environment_container_history_filters_by_deployment_and_limits(
) -> Result<(), Box<dyn std::error::Error>> {
    let test_db = TestDatabase::with_migrations().await?;
    let db = test_db.connection_arc();

    // First deployment/container comes from the shared fixture.
    let (project, environment, deployment_one, container_one) = setup_test_deployment(&db).await?;

    // A second deployment on the SAME environment, with three containers
    // that have all since been superseded by a later redeploy (deleted_at
    // set) — simulates an environment with a lot of replaced history.
    let deployment_two = deployments::ActiveModel {
        project_id: Set(project.id),
        environment_id: Set(environment.id),
        state: Set("deployed".to_string()),
        slug: Set("test-deployment-two".to_string()),
        metadata: Set(Some(Default::default())),
        created_at: Set(Utc::now()),
        updated_at: Set(Utc::now()),
        ..Default::default()
    };
    let deployment_two = deployment_two.insert(db.as_ref()).await?;

    let now = Utc::now();
    for (idx, container_id) in ["container-456", "container-789", "container-999"]
        .iter()
        .enumerate()
    {
        let container = deployment_containers::ActiveModel {
            deployment_id: Set(deployment_two.id),
            container_id: Set(container_id.to_string()),
            container_name: Set(format!("test-container-{}", idx + 2)),
            container_port: Set(Some(8080)),
            image_name: Set(Some("nginx:latest".to_string())),
            status: Set(Some("stopped".to_string())),
            created_at: Set(now + chrono::Duration::seconds(idx as i64 + 1)),
            deployed_at: Set(now + chrono::Duration::seconds(idx as i64 + 1)),
            deleted_at: Set(Some(now + chrono::Duration::seconds(idx as i64 + 10))),
            ..Default::default()
        };
        container.insert(db.as_ref()).await?;
    }

    let deployment_service = create_deployment_service_for_test(db.clone());

    // No filter, no limit override: sees every container across both
    // deployments (1 current + 3 replaced), and total_count matches.
    let (all, all_total) = deployment_service
        .list_environment_container_history(project.id, environment.id, None, None)
        .await?;
    assert_eq!(all.len(), 4);
    assert_eq!(all_total, 4);

    // Filtered to deployment_two: only its three (replaced) containers
    // come back, deployment_one's current container is excluded.
    let (filtered, filtered_total) = deployment_service
        .list_environment_container_history(
            project.id,
            environment.id,
            Some(deployment_two.id),
            None,
        )
        .await?;
    assert_eq!(filtered_total, 3);
    assert!(filtered
        .iter()
        .all(|c| c.deployment_id == deployment_two.id));
    assert!(!filtered.iter().any(|c| c.id == container_one.id));

    // limit=1 across all deployments: the single currently-running
    // container (container_one) is NEVER subject to the cap -- only
    // replaced containers are capped, so exactly 1 (of 3) replaced rows
    // joins it. total_count still reports the unfiltered total (4).
    let (limited, limited_total) = deployment_service
        .list_environment_container_history(project.id, environment.id, None, Some(1))
        .await?;
    assert_eq!(limited.len(), 2);
    assert_eq!(limited_total, 4);
    assert!(
        limited.iter().any(|c| c.id == container_one.id),
        "the running container must never be dropped by `limit`"
    );
    assert_eq!(
        limited.iter().filter(|c| c.deleted_at.is_some()).count(),
        1,
        "limit=1 should cap replaced containers to exactly 1"
    );

    // Filtering by a deployment ID from a different environment 404s
    // rather than silently returning nothing.
    let other_project = projects::ActiveModel {
        name: Set("Other Project".to_string()),
        slug: Set("other-project".to_string()),
        repo_name: Set("other-repo".to_string()),
        repo_owner: Set("other-owner".to_string()),
        preset: Set(Preset::NextJs),
        main_branch: Set("main".to_string()),
        directory: Set("/".to_string()),
        created_at: Set(Utc::now()),
        updated_at: Set(Utc::now()),
        ..Default::default()
    };
    let other_project = other_project.insert(db.as_ref()).await?;
    let other_environment = environments::ActiveModel {
        project_id: Set(other_project.id),
        name: Set("Other".to_string()),
        slug: Set("other".to_string()),
        host: Set("other.example.com".to_string()),
        upstreams: Set(UpstreamList::default()),
        subdomain: Set("other.example.com".to_string()),
        created_at: Set(Utc::now()),
        updated_at: Set(Utc::now()),
        ..Default::default()
    };
    let other_environment = other_environment.insert(db.as_ref()).await?;
    let result = deployment_service
        .list_environment_container_history(
            other_project.id,
            other_environment.id,
            Some(deployment_one.id),
            None,
        )
        .await;
    assert!(matches!(result, Err(DeploymentError::NotFound(_))));

    Ok(())
}

#[tokio::test]
async fn test_stop_container_success() -> Result<(), Box<dyn std::error::Error>> {
    let test_db = TestDatabase::with_migrations().await?;
    let db = test_db.connection_arc();

    // Setup
    let (project, environment, _deployment, container) = setup_test_deployment(&db).await?;

    let deployment_service = create_deployment_service_for_test(db.clone());

    // Test: Stop container
    deployment_service
        .stop_container(project.id, environment.id, container.container_id.clone())
        .await?;

    // Verify: Check that container status is updated in database
    let updated_container = deployment_containers::Entity::find_by_id(container.id)
        .one(db.as_ref())
        .await?
        .expect("Container should exist");

    assert_eq!(updated_container.status, Some("stopped".to_string()));
    assert!(
        updated_container.finished_at.is_some(),
        "the stop time is recorded for the health monitor"
    );

    Ok(())
}

#[tokio::test]
async fn test_start_container_success() -> Result<(), Box<dyn std::error::Error>> {
    let test_db = TestDatabase::with_migrations().await?;
    let db = test_db.connection_arc();

    // Setup
    let (project, environment, _deployment, mut container) = setup_test_deployment(&db).await?;

    // Set container status to stopped
    let mut active_container: deployment_containers::ActiveModel = container.into();
    active_container.status = Set(Some("stopped".to_string()));
    container = active_container.update(db.as_ref()).await?;

    let deployment_service = create_deployment_service_for_test(db.clone());

    // Test: Start container
    deployment_service
        .start_container(project.id, environment.id, container.container_id.clone())
        .await?;

    // Verify: Check that container status is updated to running
    let updated_container = deployment_containers::Entity::find_by_id(container.id)
        .one(db.as_ref())
        .await?
        .expect("Container should exist");

    assert_eq!(updated_container.status, Some("running".to_string()));

    Ok(())
}

#[tokio::test]
async fn test_restart_container_success() -> Result<(), Box<dyn std::error::Error>> {
    let test_db = TestDatabase::with_migrations().await?;
    let db = test_db.connection_arc();

    // Setup
    let (project, environment, _deployment, container) = setup_test_deployment(&db).await?;

    let deployment_service = create_deployment_service_for_test(db.clone());

    // Test: Restart container (stop + start)
    deployment_service
        .restart_container(project.id, environment.id, container.container_id.clone())
        .await?;

    // Verify: Container should be running after restart
    let updated_container = deployment_containers::Entity::find_by_id(container.id)
        .one(db.as_ref())
        .await?
        .expect("Container should exist");

    assert_eq!(updated_container.status, Some("running".to_string()));

    Ok(())
}

#[tokio::test]
async fn test_get_container_env_variables() -> Result<(), Box<dyn std::error::Error>> {
    let test_db = TestDatabase::with_migrations().await?;
    let db = test_db.connection_arc();

    // Setup
    let (project, environment, _deployment, container) = setup_test_deployment(&db).await?;

    let deployment_service = create_deployment_service_for_test(db.clone());

    // Test: Get container environment variables
    let env_vars = deployment_service
        .get_container_env_variables(project.id, environment.id, container.container_id.clone())
        .await?;

    // The mock returns empty HashMap, so we should get empty vec
    assert_eq!(env_vars.len(), 0);

    Ok(())
}

#[tokio::test]
async fn test_stop_all_containers_success() -> Result<(), Box<dyn std::error::Error>> {
    let test_db = TestDatabase::with_migrations().await?;
    let db = test_db.connection_arc();

    // Setup
    let (project, environment, deployment, _container) = setup_test_deployment(&db).await?;

    // Create second container
    let now = Utc::now();
    let container2 = deployment_containers::ActiveModel {
        deployment_id: Set(deployment.id),
        container_id: Set("container-789".to_string()),
        container_name: Set("test-container-3".to_string()),
        container_port: Set(Some(9090)),
        image_name: Set(Some("redis:latest".to_string())),
        status: Set(Some("running".to_string())),
        created_at: Set(now),
        deployed_at: Set(now),
        ..Default::default()
    };
    container2.insert(db.as_ref()).await?;

    let deployment_service = create_deployment_service_for_test(db.clone());

    // Test: Stop all containers
    deployment_service
        .stop_all_containers(project.id, environment.id)
        .await?;

    // Verify: Both containers should be stopped
    let containers = deployment_containers::Entity::find()
        .filter(deployment_containers::Column::DeploymentId.eq(deployment.id))
        .all(db.as_ref())
        .await?;

    for container in containers {
        assert_eq!(container.status, Some("stopped".to_string()));
    }

    Ok(())
}

#[tokio::test]
async fn test_start_all_containers_success() -> Result<(), Box<dyn std::error::Error>> {
    let test_db = TestDatabase::with_migrations().await?;
    let db = test_db.connection_arc();

    // Setup
    let (project, environment, deployment, container1) = setup_test_deployment(&db).await?;

    // Set all containers to stopped
    let mut active_container: deployment_containers::ActiveModel = container1.into();
    active_container.status = Set(Some("stopped".to_string()));
    active_container.update(db.as_ref()).await?;

    let deployment_service = create_deployment_service_for_test(db.clone());

    // Test: Start all containers
    deployment_service
        .start_all_containers(project.id, environment.id)
        .await?;

    // Verify: All containers should be running
    let containers = deployment_containers::Entity::find()
        .filter(deployment_containers::Column::DeploymentId.eq(deployment.id))
        .all(db.as_ref())
        .await?;

    for container in containers {
        assert_eq!(container.status, Some("running".to_string()));
    }

    Ok(())
}

#[tokio::test]
async fn test_restart_all_containers_success() -> Result<(), Box<dyn std::error::Error>> {
    let test_db = TestDatabase::with_migrations().await?;
    let db = test_db.connection_arc();

    // Setup (this creates 1 container)
    let (project, environment, deployment, _container) = setup_test_deployment(&db).await?;

    // Create multiple additional containers (3 more, total 4)
    let now = Utc::now();
    for i in 1..=3 {
        let container = deployment_containers::ActiveModel {
            deployment_id: Set(deployment.id),
            container_id: Set(format!("container-{}", i * 100)),
            container_name: Set(format!("test-container-{}", i)),
            container_port: Set(Some(8000 + i)),
            image_name: Set(Some("nginx:latest".to_string())),
            status: Set(Some("running".to_string())),
            created_at: Set(now),
            deployed_at: Set(now),
            ..Default::default()
        };
        container.insert(db.as_ref()).await?;
    }

    let deployment_service = create_deployment_service_for_test(db.clone());

    // Test: Restart all containers
    deployment_service
        .restart_all_containers(project.id, environment.id)
        .await?;

    // Verify: All containers should still be running (1 from setup + 3 created = 4 total)
    let containers = deployment_containers::Entity::find()
        .filter(deployment_containers::Column::DeploymentId.eq(deployment.id))
        .all(db.as_ref())
        .await?;

    assert_eq!(
        containers.len(),
        4,
        "Should have 4 containers (1 from setup + 3 created)"
    );
    for container in containers {
        assert_eq!(container.status, Some("running".to_string()));
    }

    Ok(())
}

#[tokio::test]
async fn test_container_operations_wrong_environment() -> Result<(), Box<dyn std::error::Error>> {
    let test_db = TestDatabase::with_migrations().await?;
    let db = test_db.connection_arc();

    // Setup: Create two environments
    let project = projects::ActiveModel {
        name: Set("Test Project".to_string()),
        slug: Set("test-project".to_string()),
        repo_name: Set("test-repo".to_string()),
        repo_owner: Set("test-owner".to_string()),
        preset: Set(Preset::NextJs),
        main_branch: Set("main".to_string()),
        directory: Set("/".to_string()),
        created_at: Set(Utc::now()),
        updated_at: Set(Utc::now()),
        ..Default::default()
    };
    let project = project.insert(db.as_ref()).await?;

    let env1 = environments::ActiveModel {
        project_id: Set(project.id),
        name: Set("Environment 1".to_string()),
        slug: Set("env1".to_string()),
        host: Set("env1.example.com".to_string()),
        upstreams: Set(UpstreamList::default()),
        subdomain: Set("env1.example.com".to_string()),
        created_at: Set(Utc::now()),
        updated_at: Set(Utc::now()),
        ..Default::default()
    };
    let env1 = env1.insert(db.as_ref()).await?;

    let env2 = environments::ActiveModel {
        project_id: Set(project.id),
        name: Set("Environment 2".to_string()),
        slug: Set("env2".to_string()),
        host: Set("env2.example.com".to_string()),
        upstreams: Set(UpstreamList::default()),
        subdomain: Set("env2.example.com".to_string()),
        created_at: Set(Utc::now()),
        updated_at: Set(Utc::now()),
        ..Default::default()
    };
    let env2 = env2.insert(db.as_ref()).await?;

    // Create deployment and container in env1
    let deployment = deployments::ActiveModel {
        project_id: Set(project.id),
        environment_id: Set(env1.id),
        state: Set("deployed".to_string()),
        slug: Set("test-deployment".to_string()),
        metadata: Set(Some(Default::default())),
        created_at: Set(Utc::now()),
        updated_at: Set(Utc::now()),
        ..Default::default()
    };
    let deployment = deployment.insert(db.as_ref()).await?;

    let now = Utc::now();
    let container = deployment_containers::ActiveModel {
        deployment_id: Set(deployment.id),
        container_id: Set("container-123".to_string()),
        container_name: Set("test-container".to_string()),
        container_port: Set(Some(8080)),
        image_name: Set(Some("nginx:latest".to_string())),
        status: Set(Some("running".to_string())),
        created_at: Set(now),
        deployed_at: Set(now),
        ..Default::default()
    };
    container.insert(db.as_ref()).await?;

    let deployment_service = create_deployment_service_for_test(db.clone());

    // Test: Try to operate on container from wrong environment
    let result = deployment_service
        .get_container_detail(project.id, env2.id, "container-123".to_string())
        .await;

    assert!(
        result.is_err(),
        "Should fail when environment doesn't match"
    );

    Ok(())
}

#[tokio::test]
async fn test_rollback_to_multiple_deployments_with_deleted_containers(
) -> Result<(), Box<dyn std::error::Error>> {
    let test_db = TestDatabase::with_migrations().await?;
    let db = test_db.connection_arc();

    // Setup: Create project and environment
    let project = projects::ActiveModel {
        name: Set("Multi-Deploy Project".to_string()),
        slug: Set("multi-deploy".to_string()),
        repo_name: Set("test-repo".to_string()),
        repo_owner: Set("test-owner".to_string()),
        preset: Set(Preset::NextJs),
        main_branch: Set("main".to_string()),
        directory: Set("/".to_string()),
        created_at: Set(Utc::now()),
        updated_at: Set(Utc::now()),
        ..Default::default()
    };
    let project = project.insert(db.as_ref()).await?;

    let environment = environments::ActiveModel {
        project_id: Set(project.id),
        name: Set("Production".to_string()),
        slug: Set("prod".to_string()),
        host: Set("prod.example.com".to_string()),
        upstreams: Set(UpstreamList::default()),
        subdomain: Set("prod.example.com".to_string()),
        created_at: Set(Utc::now()),
        updated_at: Set(Utc::now()),
        ..Default::default()
    };
    let environment = environment.insert(db.as_ref()).await?;

    // Create 3 deployments
    let deployment1 = deployments::ActiveModel {
        project_id: Set(project.id),
        environment_id: Set(environment.id),
        state: Set("deployed".to_string()),
        slug: Set("deployment-1".to_string()),
        image_name: Set(Some("app:v1".to_string())),
        metadata: Set(Some(Default::default())),
        created_at: Set(Utc::now()),
        updated_at: Set(Utc::now()),
        ..Default::default()
    };
    let deployment1 = deployment1.insert(db.as_ref()).await?;

    let deployment2 = deployments::ActiveModel {
        project_id: Set(project.id),
        environment_id: Set(environment.id),
        state: Set("deployed".to_string()),
        slug: Set("deployment-2".to_string()),
        image_name: Set(Some("app:v2".to_string())),
        metadata: Set(Some(Default::default())),
        created_at: Set(Utc::now()),
        updated_at: Set(Utc::now()),
        ..Default::default()
    };
    let deployment2 = deployment2.insert(db.as_ref()).await?;

    let deployment3 = deployments::ActiveModel {
        project_id: Set(project.id),
        environment_id: Set(environment.id),
        state: Set("deployed".to_string()),
        slug: Set("deployment-3".to_string()),
        image_name: Set(Some("app:v3".to_string())),
        metadata: Set(Some(Default::default())),
        created_at: Set(Utc::now()),
        updated_at: Set(Utc::now()),
        ..Default::default()
    };
    let deployment3 = deployment3.insert(db.as_ref()).await?;

    // Create containers for each deployment
    let now = Utc::now();

    let container1 = deployment_containers::ActiveModel {
        deployment_id: Set(deployment1.id),
        container_id: Set("container-v1".to_string()),
        container_name: Set("app-container-v1".to_string()),
        container_port: Set(Some(8080)),
        image_name: Set(Some("app:v1".to_string())),
        status: Set(Some("running".to_string())),
        created_at: Set(now),
        deployed_at: Set(now),
        ..Default::default()
    };
    container1.insert(db.as_ref()).await?;

    let container2 = deployment_containers::ActiveModel {
        deployment_id: Set(deployment2.id),
        container_id: Set("container-v2".to_string()),
        container_name: Set("app-container-v2".to_string()),
        container_port: Set(Some(8080)),
        image_name: Set(Some("app:v2".to_string())),
        status: Set(Some("running".to_string())),
        created_at: Set(now),
        deployed_at: Set(now),
        ..Default::default()
    };
    container2.insert(db.as_ref()).await?;

    let container3 = deployment_containers::ActiveModel {
        deployment_id: Set(deployment3.id),
        container_id: Set("container-v3".to_string()),
        container_name: Set("app-container-v3".to_string()),
        container_port: Set(Some(8080)),
        image_name: Set(Some("app:v3".to_string())),
        status: Set(Some("running".to_string())),
        created_at: Set(now),
        deployed_at: Set(now),
        ..Default::default()
    };
    container3.insert(db.as_ref()).await?;

    // Set current deployment to deployment3
    let mut active_environment: environments::ActiveModel = environment.into();
    active_environment.current_deployment_id = Set(Some(deployment3.id));
    let environment = active_environment.update(db.as_ref()).await?;

    let deployment_service = create_deployment_service_for_test(db.clone());
    configure_test_service_for_http_readiness(&deployment_service).await?;

    // Test 1: Rollback to deployment2
    // Rollback now creates a NEW deployment record with is_rollback metadata
    println!("Test 1: Rolling back to deployment 2");
    let rollback1 = deployment_service
        .rollback_to_deployment(project.id, deployment2.id)
        .await?;

    // Verify the new rollback deployment is now current (not the original deployment2)
    let updated_env = environments::Entity::find_by_id(environment.id)
        .one(db.as_ref())
        .await?
        .expect("Environment should exist");
    assert_eq!(updated_env.current_deployment_id, Some(rollback1.id));
    // Verify rollback metadata points to the original deployment
    let rollback1_dep = deployments::Entity::find_by_id(rollback1.id)
        .one(db.as_ref())
        .await?
        .unwrap();
    let meta1 = rollback1_dep.metadata.unwrap();
    assert!(meta1.is_rollback);
    assert_eq!(meta1.rolled_back_from_id, Some(deployment2.id));

    // Test 2: Rollback to deployment1 (containers redeployed)
    println!("Test 2: Rolling back to deployment 1 (containers redeployed)");
    let rollback2 = deployment_service
        .rollback_to_deployment(project.id, deployment1.id)
        .await?;

    // Verify the new rollback deployment is now current
    let updated_env = environments::Entity::find_by_id(environment.id)
        .one(db.as_ref())
        .await?
        .expect("Environment should exist");
    assert_eq!(updated_env.current_deployment_id, Some(rollback2.id));
    let rollback2_dep = deployments::Entity::find_by_id(rollback2.id)
        .one(db.as_ref())
        .await?
        .unwrap();
    let meta2 = rollback2_dep.metadata.unwrap();
    assert!(meta2.is_rollback);
    assert_eq!(meta2.rolled_back_from_id, Some(deployment1.id));

    // Test 3: Verify rollback chain (3 -> 2 -> 1)
    println!("Test 3: Full rollback chain (3 -> 2 -> 1)");

    let rollback3 = deployment_service
        .rollback_to_deployment(project.id, deployment3.id)
        .await?;
    let updated_env = environments::Entity::find_by_id(environment.id)
        .one(db.as_ref())
        .await?
        .expect("Environment should exist");
    assert_eq!(updated_env.current_deployment_id, Some(rollback3.id));

    let rollback4 = deployment_service
        .rollback_to_deployment(project.id, deployment2.id)
        .await?;
    let updated_env = environments::Entity::find_by_id(environment.id)
        .one(db.as_ref())
        .await?
        .expect("Environment should exist");
    assert_eq!(updated_env.current_deployment_id, Some(rollback4.id));

    let rollback5 = deployment_service
        .rollback_to_deployment(project.id, deployment1.id)
        .await?;
    let updated_env = environments::Entity::find_by_id(environment.id)
        .one(db.as_ref())
        .await?
        .expect("Environment should exist");
    assert_eq!(updated_env.current_deployment_id, Some(rollback5.id));

    println!("All rollback tests passed!");
    Ok(())
}

/// A worker's `/agent/*` API clears a container's variables, so a job
/// run beside a container on a worker must get them recomputed from its
/// deployment, never an empty import.
#[tokio::test]
async fn a_job_beside_a_worker_container_gets_its_deployments_variables(
) -> Result<(), Box<dyn std::error::Error>> {
    use crate::services::workflow_planner::WorkflowPlanner;
    use temps_entities::{env_var_environments, env_vars};

    const SEEDED: &str = "seeded-job-import-value-5f1c";
    if !database_integration_tests_available().await {
        eprintln!("Docker unavailable; skipping the worker job import test");
        return Ok(());
    }

    let test_db = TestDatabase::with_migrations().await?;
    let db = test_db.connection_arc();
    let (project, environment, _deployment, container) = setup_test_deployment(&db).await?;
    let node = nodes::ActiveModel {
        name: Set("worker-a".to_string()),
        token_hash: Set("unused".to_string()),
        token_encrypted: Set(None),
        address: Set("https://127.0.0.1:1".to_string()),
        private_address: Set("127.0.0.1".to_string()),
        role: Set("worker".to_string()),
        status: Set("active".to_string()),
        labels: Set(serde_json::json!({})),
        capacity: Set(serde_json::json!({})),
        dns_resolver_consecutive_failures: Set(0),
        build_limits: Set(None),
        public_ingress_enabled: Set(false),
        public_ingress_unsupported_reasons: Set(serde_json::json!([])),
        created_at: Set(Utc::now()),
        updated_at: Set(Utc::now()),
        ..Default::default()
    }
    .insert(db.as_ref())
    .await?;
    let mut active: deployment_containers::ActiveModel = container.clone().into();
    active.node_id = Set(Some(node.id));
    active.update(db.as_ref()).await?;
    let var = env_vars::ActiveModel {
        project_id: Set(project.id),
        environment_id: Set(Some(environment.id)),
        key: Set("APP_SECRET".to_string()),
        value: Set(SEEDED.to_string()),
        created_at: Set(Utc::now()),
        updated_at: Set(Utc::now()),
        ..Default::default()
    }
    .insert(db.as_ref())
    .await?;
    env_var_environments::ActiveModel {
        env_var_id: Set(var.id),
        environment_id: Set(environment.id),
        ..Default::default()
    }
    .insert(db.as_ref())
    .await?;

    let service = create_deployment_service_for_test(db.clone());

    // Without the planner the import is refused by name, never empty.
    let refused = service
        .job_import_env_variables(project.id, environment.id, container.container_id.clone())
        .await
        .expect_err("a worker container's variables cannot be read without the planner");
    let message = refused.to_string();
    assert!(
        message.contains("container-123") && message.contains(&format!("node {}", node.id)),
        "{message}"
    );

    let server_config = Arc::new(temps_config::ServerConfig::new(
        "127.0.0.1:3000".to_string(),
        "postgresql://test".to_string(),
        None,
        Some("127.0.0.1:8000".to_string()),
    )?);
    let planner = WorkflowPlanner::new(
        db.clone(),
        Arc::new(temps_deployment_logs::LogService::new(std::env::temp_dir())),
        create_test_external_service_manager(db.clone()),
        Arc::new(temps_config::ConfigService::new(server_config, db.clone())),
        Arc::new(temps_error_tracking::DSNService::new(db.clone())),
        create_test_encryption_service(),
    );
    service.set_workflow_planner(Arc::new(planner));

    let vars: HashMap<String, String> = service
        .job_import_env_variables(project.id, environment.id, container.container_id.clone())
        .await?
        .into_iter()
        .collect();
    assert_eq!(vars.get("APP_SECRET").map(String::as_str), Some(SEEDED));
    assert_eq!(vars.get("HOST").map(String::as_str), Some("0.0.0.0"));
    assert!(vars.contains_key("TEMPS_ASSET_PREFIX"), "{:?}", vars.keys());

    // An unknown container is not found, by name.
    let other = service
        .job_import_env_variables(project.id, environment.id, "no-such-container".to_string())
        .await;
    assert!(matches!(other, Err(DeploymentError::NotFound(_))));
    Ok(())
}

// Helper function to setup a test deployment with a container
async fn setup_test_deployment(
    db: &Arc<temps_database::DbConnection>,
) -> Result<
    (
        projects::Model,
        environments::Model,
        deployments::Model,
        deployment_containers::Model,
    ),
    Box<dyn std::error::Error>,
> {
    let project = projects::ActiveModel {
        name: Set("Test Project".to_string()),
        slug: Set("test-project".to_string()),
        repo_name: Set("test-repo".to_string()),
        repo_owner: Set("test-owner".to_string()),
        preset: Set(Preset::NextJs),
        main_branch: Set("main".to_string()),
        directory: Set("/".to_string()),
        created_at: Set(Utc::now()),
        updated_at: Set(Utc::now()),
        ..Default::default()
    };
    let project = project.insert(db.as_ref()).await?;

    let environment = environments::ActiveModel {
        project_id: Set(project.id),
        name: Set("Production".to_string()),
        slug: Set("prod".to_string()),
        host: Set("prod.example.com".to_string()),
        upstreams: Set(UpstreamList::default()),
        subdomain: Set("prod.example.com".to_string()),
        created_at: Set(Utc::now()),
        updated_at: Set(Utc::now()),
        ..Default::default()
    };
    let environment = environment.insert(db.as_ref()).await?;

    let deployment = deployments::ActiveModel {
        project_id: Set(project.id),
        environment_id: Set(environment.id),
        state: Set("deployed".to_string()),
        slug: Set("test-deployment".to_string()),
        metadata: Set(Some(Default::default())),
        created_at: Set(Utc::now()),
        updated_at: Set(Utc::now()),
        ..Default::default()
    };
    let deployment = deployment.insert(db.as_ref()).await?;

    let now = Utc::now();
    let container = deployment_containers::ActiveModel {
        deployment_id: Set(deployment.id),
        container_id: Set("container-123".to_string()),
        container_name: Set("test-container-1".to_string()),
        container_port: Set(Some(8080)),
        image_name: Set(Some("nginx:latest".to_string())),
        status: Set(Some("running".to_string())),
        created_at: Set(now),
        deployed_at: Set(now),
        ..Default::default()
    };
    let container = container.insert(db.as_ref()).await?;

    Ok((project, environment, deployment, container))
}

/// Insert a captured-log metadata row and write its backing file under the
/// service's log base path so the service can read it back. Returns the row.
async fn seed_captured_log(
    db: &Arc<temps_database::DbConnection>,
    log_base: &std::path::Path,
    deployment: &deployments::Model,
    container_name: &str,
    content: &str,
) -> Result<deployment_container_logs::Model, Box<dyn std::error::Error>> {
    let row = deployment_container_logs::ActiveModel {
        deployment_id: Set(deployment.id),
        project_id: Set(deployment.project_id),
        environment_id: Set(deployment.environment_id),
        container_id: Set(format!("cid-{}", container_name)),
        container_name: Set(container_name.to_string()),
        service_name: Set(None),
        node_id: Set(None),
        log_path: Set(String::new()), // filled in after we know the row id
        size_bytes: Set(content.len() as i64),
        truncated: Set(false),
        ..Default::default()
    };
    let row = row.insert(db.as_ref()).await?;

    // Mirror the path scheme used by capture_container_logs.
    let log_path = format!("deployment-container-logs/{}/{}.log", deployment.id, row.id);
    let full_path = log_base.join(&log_path);
    if let Some(parent) = full_path.parent() {
        tokio::fs::create_dir_all(parent).await?;
    }
    tokio::fs::write(&full_path, content.as_bytes()).await?;

    let mut active: deployment_container_logs::ActiveModel = row.into();
    active.log_path = Set(log_path);
    let row = active.update(db.as_ref()).await?;
    Ok(row)
}

#[tokio::test]
async fn test_get_deployment_jobs_enforces_project_ownership(
) -> Result<(), Box<dyn std::error::Error>> {
    if std::env::var_os("TEMPS_TEST_DATABASE_URL").is_none()
        && !tokio::process::Command::new("docker")
            .arg("info")
            .output()
            .await
            .map(|output| output.status.success())
            .unwrap_or(false)
    {
        eprintln!("Docker unavailable; skipping deployment ownership test");
        return Ok(());
    }
    let test_db = TestDatabase::with_migrations().await?;
    let db = test_db.connection_arc();
    let (_project, _environment, deployment) = setup_test_data(&db).await?;

    let job = temps_entities::deployment_jobs::ActiveModel {
        deployment_id: Set(deployment.id),
        job_id: Set("sensitive-build".to_string()),
        job_type: Set("BuildImageJob".to_string()),
        name: Set("Sensitive Build".to_string()),
        log_id: Set("sensitive-build-log".to_string()),
        status: Set(temps_entities::types::JobStatus::Success),
        job_config: Set(Some(serde_json::json!({
            "build_args": {"DATABASE_PASSWORD": "must-not-cross-projects"}
        }))),
        ..Default::default()
    };
    job.insert(db.as_ref()).await?;

    let service = create_deployment_service_for_test(db.clone());
    let own_jobs = service
        .get_deployment_jobs(deployment.project_id, deployment.id)
        .await?;
    assert_eq!(own_jobs.len(), 1);

    let foreign_result = service
        .get_deployment_jobs(deployment.project_id + 999, deployment.id)
        .await;
    assert!(matches!(foreign_result, Err(DeploymentError::NotFound(_))));

    Ok(())
}

#[tokio::test]
async fn test_failure_report_preview_stops_at_failed_job_and_redacts_secrets(
) -> Result<(), Box<dyn std::error::Error>> {
    if std::env::var_os("TEMPS_TEST_DATABASE_URL").is_none()
        && !tokio::process::Command::new("docker")
            .arg("info")
            .output()
            .await
            .map(|output| output.status.success())
            .unwrap_or(false)
    {
        eprintln!("Docker unavailable; skipping failure-report preview test");
        return Ok(());
    }
    let test_db = TestDatabase::with_migrations().await?;
    let db = test_db.connection_arc();
    let (_project, _environment, deployment) = setup_test_data(&db).await?;

    let enc = create_test_encryption_service();
    let log_base = std::env::temp_dir();
    let log_service = Arc::new(temps_deployment_logs::LogService::new(log_base.clone()));

    let unique = uuid::Uuid::new_v4().simple().to_string();
    let mut secrets_config = serde_json::Map::new();
    let mut secret_map = std::collections::HashMap::new();
    secret_map.insert("DB_PASSWORD".to_string(), "super-secret-value".to_string());
    crate::services::sensitive_envelope::write_sealed(
        &mut secrets_config,
        enc.as_ref(),
        "secrets",
        &secret_map,
    )?;

    let jobs = [
        (
            "download_repo",
            "Download Repo",
            0,
            temps_entities::types::JobStatus::Success,
            None,
            None,
        ),
        (
            "build_image",
            "Build Image",
            1,
            temps_entities::types::JobStatus::Failure,
            Some("build failed: exit code 1".to_string()),
            Some(serde_json::Value::Object(secrets_config)),
        ),
        (
            "deploy_container",
            "Deploy Container",
            2,
            temps_entities::types::JobStatus::Skipped,
            None,
            None,
        ),
    ];

    for (job_id, name, order, status, error_message, job_config) in jobs {
        let log_id = format!("{unique}-{job_id}");
        // `download_repo` deliberately never writes its log file, to
        // exercise a job that finished too fast to log anything (e.g.
        // PrepareSourceBundleJob) -- the preview must still succeed.
        if job_id != "download_repo" {
            tokio::fs::write(
                log_base.join(format!("{log_id}.log")),
                format!("log output for {job_id}, secret is super-secret-value\n"),
            )
            .await?;
        }

        temps_entities::deployment_jobs::ActiveModel {
            deployment_id: Set(deployment.id),
            job_id: Set(job_id.to_string()),
            job_type: Set(format!("{job_id}Job")),
            name: Set(name.to_string()),
            log_id: Set(log_id),
            status: Set(status),
            error_message: Set(error_message),
            job_config: Set(job_config),
            execution_order: Set(Some(order)),
            ..Default::default()
        }
        .insert(db.as_ref())
        .await?;
    }

    let deployment_service = Arc::new(create_deployment_service_for_test(db.clone()));
    let failure_service =
        crate::services::FailureReportService::new(deployment_service, log_service, enc)?;

    let preview = failure_service
        .build_preview(deployment.project_id, deployment.id, "build_image")
        .await?;

    assert_eq!(
        preview.error_message.as_deref(),
        Some("build failed: exit code 1")
    );
    assert!(preview.redacted_log.contains("download_repo"));
    assert!(
        preview
            .redacted_log
            .contains("no log output for this stage"),
        "a job with no log file must degrade to a placeholder, not fail the whole preview"
    );
    assert!(preview.redacted_log.contains("build_image"));
    assert!(
        !preview.redacted_log.contains("deploy_container"),
        "trace must stop at the failed job, not include later jobs"
    );
    assert!(
        !preview.redacted_log.contains("super-secret-value"),
        "known secret value must be redacted from the trace"
    );

    Ok(())
}

#[tokio::test]
async fn test_get_public_repo_reference_none_for_private_none_for_public(
) -> Result<(), Box<dyn std::error::Error>> {
    let test_db = TestDatabase::with_migrations().await?;
    let db = test_db.connection_arc();
    // setup_test_data's project doesn't set is_public_repo, so it defaults
    // to private -- must never leak repo_owner/repo_name into a public
    // GitHub issue template.
    let (project, _environment, deployment) = setup_test_data(&db).await?;
    let service = create_deployment_service_for_test(db.clone());

    let private_result = service
        .get_public_repo_reference(project.id, deployment.id)
        .await?;
    assert!(
        private_result.is_none(),
        "a private (or unlinked) repo must never be surfaced"
    );

    let mut project_update: projects::ActiveModel = project.clone().into();
    project_update.is_public_repo = Set(true);
    project_update.update(db.as_ref()).await?;

    let mut deployment_update: deployments::ActiveModel = deployment.clone().into();
    deployment_update.branch_ref = Set(Some("feat/some-branch".to_string()));
    deployment_update.update(db.as_ref()).await?;

    let public_result = service
        .get_public_repo_reference(project.id, deployment.id)
        .await?
        .expect("public repo must return a reference");
    assert_eq!(public_result.owner, "test-owner");
    assert_eq!(public_result.repo, "test-repo");
    assert_eq!(public_result.branch, "feat/some-branch");

    Ok(())
}

#[tokio::test]
async fn test_list_deployment_container_logs_returns_captured_rows(
) -> Result<(), Box<dyn std::error::Error>> {
    let test_db = TestDatabase::with_migrations().await?;
    let db = test_db.connection_arc();
    let (_project, _environment, deployment) = setup_test_data(&db).await?;

    let log_base = std::env::temp_dir();
    seed_captured_log(&db, &log_base, &deployment, "web-1", "old logs").await?;
    seed_captured_log(&db, &log_base, &deployment, "web-2", "newer logs").await?;

    let service = create_deployment_service_for_test(db.clone());
    let logs = service
        .list_deployment_container_logs(deployment.project_id, deployment.id)
        .await?;

    assert_eq!(logs.len(), 2);
    // All captured logs belong to the requested deployment.
    assert!(logs.iter().all(|l| l.deployment_id == deployment.id));
    Ok(())
}

#[tokio::test]
async fn test_list_deployment_container_logs_wrong_project_not_found(
) -> Result<(), Box<dyn std::error::Error>> {
    let test_db = TestDatabase::with_migrations().await?;
    let db = test_db.connection_arc();
    let (_project, _environment, deployment) = setup_test_data(&db).await?;

    let service = create_deployment_service_for_test(db.clone());
    // A different project id must not see this deployment's logs — IDOR guard.
    let result = service
        .list_deployment_container_logs(deployment.project_id + 999, deployment.id)
        .await;

    assert!(matches!(result, Err(DeploymentError::NotFound(_))));
    Ok(())
}

#[tokio::test]
async fn test_get_deployment_container_log_content_reads_file(
) -> Result<(), Box<dyn std::error::Error>> {
    let test_db = TestDatabase::with_migrations().await?;
    let db = test_db.connection_arc();
    let (_project, _environment, deployment) = setup_test_data(&db).await?;

    let log_base = std::env::temp_dir();
    let row = seed_captured_log(&db, &log_base, &deployment, "web-2", "hello from web-2").await?;

    let service = create_deployment_service_for_test(db.clone());
    let (got_row, content) = service
        .get_deployment_container_log_content(deployment.project_id, deployment.id, row.id)
        .await?;

    assert_eq!(got_row.id, row.id);
    assert_eq!(content, "hello from web-2");
    Ok(())
}

#[tokio::test]
async fn test_get_deployment_container_log_content_wrong_project_not_found(
) -> Result<(), Box<dyn std::error::Error>> {
    let test_db = TestDatabase::with_migrations().await?;
    let db = test_db.connection_arc();
    let (_project, _environment, deployment) = setup_test_data(&db).await?;

    let log_base = std::env::temp_dir();
    let row = seed_captured_log(&db, &log_base, &deployment, "web-2", "secret").await?;

    let service = create_deployment_service_for_test(db.clone());
    // Reading with a foreign project id must be denied even with the right log id.
    let result = service
        .get_deployment_container_log_content(deployment.project_id + 999, deployment.id, row.id)
        .await;

    assert!(matches!(result, Err(DeploymentError::NotFound(_))));
    Ok(())
}

#[test]
fn resolve_resource_usage_is_opt_in_with_env_over_project() {
    let cfg = |cpu: Option<i32>, mem: Option<i32>| DeploymentConfig {
        cpu_limit: cpu,
        memory_limit: mem,
        ..Default::default()
    };

    // 1. Nothing configured anywhere -> fully uncapped (the default).
    let none = DeploymentService::resolve_resource_usage(None, None);
    assert_eq!(none.cpu_limit, None);
    assert_eq!(none.memory_limit, None);

    // 2. Project sets a limit, env doesn't -> inherit project (microcores → `u`, MB → `Mi`).
    let proj = cfg(Some(2_000_000), Some(512));
    let inherited = DeploymentService::resolve_resource_usage(None, Some(&proj));
    assert_eq!(inherited.cpu_limit.as_deref(), Some("2000000u"));
    assert_eq!(inherited.memory_limit.as_deref(), Some("512Mi"));

    // 3. Env overrides project per-field (env cpu wins, project memory inherited).
    let env = cfg(Some(500_000), None);
    let overridden = DeploymentService::resolve_resource_usage(Some(&env), Some(&proj));
    assert_eq!(overridden.cpu_limit.as_deref(), Some("500000u"));
    assert_eq!(overridden.memory_limit.as_deref(), Some("512Mi"));

    // 4. Env config present but with all-None limits -> still uncapped
    //    (an environment existing must NOT imply a limit).
    let empty_env = cfg(None, None);
    let still_none =
        DeploymentService::resolve_resource_usage(Some(&empty_env), Some(&cfg(None, None)));
    assert_eq!(still_none.cpu_limit, None);
    assert_eq!(still_none.memory_limit, None);
}

#[tokio::test]
async fn test_trigger_image_deployment_rejects_empty_image_ref(
) -> Result<(), Box<dyn std::error::Error>> {
    if !database_integration_tests_available().await {
        eprintln!("Docker unavailable; skipping image deployment integration test");
        return Ok(());
    }
    let test_db = TestDatabase::with_migrations().await?;
    let db = test_db.connection_arc();
    let (project, environment, _) = setup_test_data(&db).await?;
    let mut deployment_service = create_deployment_service_for_test(db);
    let mut queue = MockQueueService::new();
    queue.expect_send().times(0);
    deployment_service.queue_service = Arc::new(queue);

    let result = deployment_service
        .trigger_image_deployment(project.id, Some(environment.id), String::new(), None, None)
        .await;

    assert!(matches!(
        result,
        Err(DeploymentError::InvalidInput(message)) if message == "Image reference is missing"
    ));
    Ok(())
}

#[tokio::test]
async fn test_trigger_image_deployment_sends_deploy_image_requested_job(
) -> Result<(), Box<dyn std::error::Error>> {
    if !database_integration_tests_available().await {
        eprintln!("Docker unavailable; skipping image deployment integration test");
        return Ok(());
    }
    let test_db = TestDatabase::with_migrations().await?;
    let db = test_db.connection_arc();

    let (project, environment, _) = setup_test_data(&db).await?;
    let deployment_service = create_deployment_service_for_test(db.clone());
    let mut receiver = deployment_service.queue_service.subscribe();

    deployment_service
        .trigger_image_deployment(
            project.id,
            Some(environment.id),
            "ghcr.io/org/app:latest".to_string(),
            Some("/healthz".to_string()),
            Some(vec!["serve".to_string()]),
        )
        .await?;

    // The auto-responder spawned in create_deployment_service_for_test also
    // listens on this queue (to keep RouteTableUpdated flowing) — drain past
    // whatever else it produces until our own job shows up.
    let job = loop {
        match receiver.recv().await {
            Ok(temps_core::Job::DeployImageRequested(job)) => break job,
            Ok(_) => continue,
            Err(e) => panic!("queue closed before DeployImageRequested arrived: {}", e),
        }
    };

    assert_eq!(job.project_id, project.id);
    assert_eq!(job.target_environment_id, Some(environment.id));
    assert_eq!(job.image_ref, "ghcr.io/org/app:latest");
    assert_eq!(job.health_check_path.as_deref(), Some("/healthz"));
    assert_eq!(job.command, Some(vec!["serve".to_string()]));
    assert_eq!(job.recovery_of_deployment_id, None);
    Ok(())
}

#[tokio::test]
async fn test_trigger_image_deployment_missing_project_returns_not_found_without_queueing(
) -> Result<(), Box<dyn std::error::Error>> {
    if !database_integration_tests_available().await {
        eprintln!("Docker unavailable; skipping image deployment integration test");
        return Ok(());
    }

    // Arrange
    let test_db = TestDatabase::with_migrations().await?;
    let mut service = create_deployment_service_for_test(test_db.connection_arc());
    let mut queue = MockQueueService::new();
    queue.expect_send().times(0);
    service.queue_service = Arc::new(queue);
    let project_id = i32::MAX;

    // Act: project lookup must precede image validation.
    let result = service
        .trigger_image_deployment(project_id, None, String::new(), None, None)
        .await;

    // Assert
    assert!(matches!(
        result,
        Err(DeploymentError::NotFound(message))
            if message == format!("project {project_id} not found")
    ));
    Ok(())
}

#[tokio::test]
async fn test_trigger_image_deployment_external_project_returns_invalid_input_without_queueing(
) -> Result<(), Box<dyn std::error::Error>> {
    if !database_integration_tests_available().await {
        eprintln!("Docker unavailable; skipping image deployment integration test");
        return Ok(());
    }

    // Arrange
    let test_db = TestDatabase::with_migrations().await?;
    let db = test_db.connection_arc();
    let (project, environment, _) = setup_test_data(&db).await?;
    let mut active_project: projects::ActiveModel = project.clone().into();
    active_project.source_type = Set(temps_entities::source_type::SourceType::External);
    active_project.update(db.as_ref()).await?;
    let mut service = create_deployment_service_for_test(db);
    let mut queue = MockQueueService::new();
    queue.expect_send().times(0);
    service.queue_service = Arc::new(queue);

    // Act: the monitoring-only guard must also precede image validation.
    for image_ref in ["registry.example/app:latest", ""] {
        let result = service
            .trigger_image_deployment(
                project.id,
                Some(environment.id),
                image_ref.to_string(),
                None,
                None,
            )
            .await;

        // Assert
        assert!(matches!(
            result,
            Err(DeploymentError::InvalidInput(message))
                if message == format!(
                    "Project {} is for monitoring only. Configure hosting before deploying.",
                    project.id
                )
        ));
    }
    Ok(())
}

#[tokio::test]
async fn redeploy_environment_uses_affected_deployment_and_target_environment(
) -> Result<(), Box<dyn std::error::Error>> {
    let test_db = TestDatabase::with_migrations().await?;
    let db = test_db.connection_arc();
    let (project, environment, affected, _) = setup_test_deployment(&db).await?;

    let mut affected_active: deployments::ActiveModel = affected.clone().into();
    affected_active.metadata = Set(Some(temps_entities::deployments::DeploymentMetadata {
        external_image_ref: Some("registry.example/app:known-good".to_string()),
        ..Default::default()
    }));
    affected_active.update(db.as_ref()).await?;

    // A newer failed deployment must never supersede the workload whose
    // container is actually being migrated.
    deployments::ActiveModel {
        project_id: Set(project.id),
        environment_id: Set(environment.id),
        state: Set("failed".to_string()),
        slug: Set("failed-newer-deployment".to_string()),
        metadata: Set(Some(temps_entities::deployments::DeploymentMetadata {
            external_image_ref: Some("registry.example/app:failed".to_string()),
            ..Default::default()
        })),
        created_at: Set(Utc::now() + chrono::Duration::seconds(1)),
        updated_at: Set(Utc::now() + chrono::Duration::seconds(1)),
        ..Default::default()
    }
    .insert(db.as_ref())
    .await?;

    let service = create_deployment_service_for_test(db);
    let mut receiver = service.queue_service.subscribe();
    service
        .redeploy_environment(project.id, environment.id, affected.id)
        .await?;

    let job = loop {
        match receiver.recv().await {
            Ok(temps_core::Job::DeployImageRequested(job)) => break job,
            Ok(_) => continue,
            Err(e) => panic!("queue closed before DeployImageRequested arrived: {}", e),
        }
    };
    assert_eq!(job.project_id, project.id);
    assert_eq!(job.target_environment_id, Some(environment.id));
    assert_eq!(job.image_ref, "registry.example/app:known-good");
    assert_eq!(job.recovery_of_deployment_id, None);
    Ok(())
}

#[tokio::test]
async fn test_redeploy_environment_for_failover_marks_git_job_as_recovery(
) -> Result<(), Box<dyn std::error::Error>> {
    if !database_integration_tests_available().await {
        eprintln!("Docker unavailable; skipping failover git queue marker integration test");
        return Ok(());
    }

    // Arrange
    let test_db = TestDatabase::with_migrations().await?;
    let db = test_db.connection_arc();
    let (project, environment, deployment, _) = setup_test_deployment(&db).await?;
    let service = create_deployment_service_for_test(db);
    let mut receiver = service.queue_service.subscribe();

    // Act
    service
        .redeploy_environment_for_failover(project.id, environment.id, deployment.id)
        .await?;

    let job = loop {
        match receiver.recv().await {
            Ok(temps_core::Job::GitPushEvent(job)) => break job,
            Ok(_) => continue,
            Err(e) => panic!("queue closed before GitPushEvent arrived: {e}"),
        }
    };

    // Assert
    assert_eq!(job.project_id, project.id);
    assert_eq!(job.target_environment_id, Some(environment.id));
    assert_eq!(job.recovery_of_deployment_id, Some(deployment.id));
    Ok(())
}

#[tokio::test]
async fn test_redeploy_environment_for_failover_marks_image_job_as_recovery(
) -> Result<(), Box<dyn std::error::Error>> {
    if !database_integration_tests_available().await {
        eprintln!("Docker unavailable; skipping failover image queue marker integration test");
        return Ok(());
    }

    // Arrange
    let test_db = TestDatabase::with_migrations().await?;
    let db = test_db.connection_arc();
    let (project, environment, deployment, _) = setup_test_deployment(&db).await?;
    let mut active: deployments::ActiveModel = deployment.clone().into();
    active.metadata = Set(Some(temps_entities::deployments::DeploymentMetadata {
        external_image_ref: Some("registry.example/app:known-good".to_string()),
        ..Default::default()
    }));
    active.update(db.as_ref()).await?;

    let service = create_deployment_service_for_test(db);
    let mut receiver = service.queue_service.subscribe();

    // Act
    service
        .redeploy_environment_for_failover(project.id, environment.id, deployment.id)
        .await?;

    let job = loop {
        match receiver.recv().await {
            Ok(temps_core::Job::DeployImageRequested(job)) => break job,
            Ok(_) => continue,
            Err(e) => panic!("queue closed before DeployImageRequested arrived: {e}"),
        }
    };

    // Assert
    assert_eq!(job.project_id, project.id);
    assert_eq!(job.target_environment_id, Some(environment.id));
    assert_eq!(job.image_ref, "registry.example/app:known-good");
    assert_eq!(job.recovery_of_deployment_id, Some(deployment.id));
    Ok(())
}

/// Point the service's data directory at `data_dir` so uploaded-source
/// archives can be staged without touching the real one.
fn with_test_data_dir(
    mut service: DeploymentService,
    db: Arc<temps_database::DbConnection>,
    data_dir: &std::path::Path,
) -> Result<DeploymentService, Box<dyn std::error::Error>> {
    let mut config = temps_config::ServerConfig::new(
        "127.0.0.1:3000".to_string(),
        "postgresql://test_user:test_password@localhost:5432/test_db".to_string(),
        None,
        Some("127.0.0.1:3001".to_string()),
    )?;
    config.data_dir = data_dir.to_path_buf();
    service.config_service = Arc::new(temps_config::ConfigService::new(Arc::new(config), db));
    Ok(service)
}

/// Turn the fixture into an uploaded-source project (no repository) whose
/// deployment records `bundle_path` as its source.
async fn make_uploaded_source(
    db: &temps_database::DbConnection,
    project: &projects::Model,
    deployment: &deployments::Model,
    bundle_path: &str,
) -> Result<(), Box<dyn std::error::Error>> {
    let mut active_project: projects::ActiveModel = project.clone().into();
    active_project.repo_owner = Set(String::new());
    active_project.repo_name = Set(String::new());
    active_project.source_type = Set(temps_entities::source_type::SourceType::UploadedSource);
    active_project.update(db).await?;
    let mut active: deployments::ActiveModel = deployment.clone().into();
    active.metadata = Set(Some(temps_entities::deployments::DeploymentMetadata {
        source_bundle_id: Some(41),
        source_bundle_path: Some(bundle_path.to_string()),
        source_bundle_content_type: Some("application/zip".to_string()),
        deployment_source_type: Some(temps_entities::source_type::SourceType::UploadedSource),
        ..Default::default()
    }));
    active.update(db).await?;
    Ok(())
}

#[tokio::test]
async fn uploaded_source_redeploy_rebuilds_from_the_retained_bundle_not_git(
) -> Result<(), Box<dyn std::error::Error>> {
    if !database_integration_tests_available().await {
        eprintln!("Docker unavailable; skipping uploaded-source redeploy integration test");
        return Ok(());
    }

    // Arrange: an uploaded-source project has no repository, which made
    // drain fail with "Project repo_owner is missing".
    let test_db = TestDatabase::with_migrations().await?;
    let db = test_db.connection_arc();
    let (project, environment, deployment, _) = setup_test_deployment(&db).await?;
    let bundle_path = "source-bundles/drain-fixture/source.zip";
    make_uploaded_source(db.as_ref(), &project, &deployment, bundle_path).await?;
    let data_dir = tempfile::tempdir()?;
    std::fs::create_dir_all(data_dir.path().join("source-bundles/drain-fixture"))?;
    std::fs::write(data_dir.path().join(bundle_path), b"PK\x05\x06")?;
    let service = with_test_data_dir(
        create_deployment_service_for_test(db.clone()),
        db,
        data_dir.path(),
    )?;
    let mut receiver = service.queue_service.subscribe();

    // Act
    service
        .check_redeployable(project.id, environment.id, deployment.id)
        .await?;
    service
        .redeploy_environment_for_failover(project.id, environment.id, deployment.id)
        .await?;

    // Assert: the bundle lineage is redeployed, not the Git pipeline.
    let job = loop {
        match receiver.recv().await {
            Ok(temps_core::Job::RedeploySourceBundleRequested(job)) => break job,
            Ok(temps_core::Job::GitPushEvent(job)) => {
                panic!("uploaded source was sent down the Git path: {job:?}")
            }
            Ok(_) => continue,
            Err(e) => panic!("queue closed before RedeploySourceBundleRequested arrived: {e}"),
        }
    };
    assert_eq!(job.project_id, project.id);
    assert_eq!(job.environment_id, environment.id);
    assert_eq!(job.source_deployment_id, deployment.id);
    assert_eq!(job.recovery_of_deployment_id, Some(deployment.id));
    Ok(())
}

#[tokio::test]
async fn uploaded_source_redeploy_without_its_archive_is_refused_before_queueing(
) -> Result<(), Box<dyn std::error::Error>> {
    if !database_integration_tests_available().await {
        eprintln!("Docker unavailable; skipping uploaded-source redeploy integration test");
        return Ok(());
    }

    // Arrange: the deployment records a bundle the server no longer has.
    let test_db = TestDatabase::with_migrations().await?;
    let db = test_db.connection_arc();
    let (project, environment, deployment, _) = setup_test_deployment(&db).await?;
    make_uploaded_source(
        db.as_ref(),
        &project,
        &deployment,
        "source-bundles/drain-fixture/missing.zip",
    )
    .await?;
    let data_dir = tempfile::tempdir()?;
    let mut service = with_test_data_dir(
        create_deployment_service_for_test(db.clone()),
        db,
        data_dir.path(),
    )?;
    let mut queue = MockQueueService::new();
    queue.expect_send().times(0);
    service.queue_service = Arc::new(queue);

    // Act
    let checked = service
        .check_redeployable(project.id, environment.id, deployment.id)
        .await;
    let redeployed = service
        .redeploy_environment(project.id, environment.id, deployment.id)
        .await;

    // Assert: both name the deployment and the missing archive.
    for result in [checked, redeployed] {
        match result {
            Err(DeploymentError::InvalidDeploymentState(message)) => {
                assert!(
                    message.contains(&format!("Deployment {}", deployment.id)),
                    "{message}"
                );
                assert!(message.contains("missing.zip"), "{message}");
                assert!(message.contains("no longer retained"), "{message}");
            }
            other => panic!("expected a refusal, got {other:?}"),
        }
    }
    Ok(())
}

#[tokio::test]
async fn redeploy_with_no_source_lineage_is_refused_before_queueing(
) -> Result<(), Box<dyn std::error::Error>> {
    if !database_integration_tests_available().await {
        eprintln!("Docker unavailable; skipping redeploy lineage integration test");
        return Ok(());
    }

    // Arrange: no repository, no image, no uploaded bundle.
    let test_db = TestDatabase::with_migrations().await?;
    let db = test_db.connection_arc();
    let (project, environment, deployment, _) = setup_test_deployment(&db).await?;
    let mut active_project: projects::ActiveModel = project.clone().into();
    active_project.repo_owner = Set(String::new());
    active_project.repo_name = Set(String::new());
    active_project.update(db.as_ref()).await?;
    let mut active: deployments::ActiveModel = deployment.clone().into();
    active.metadata = Set(Some(deployments::DeploymentMetadata::default()));
    active.update(db.as_ref()).await?;
    let mut service = create_deployment_service_for_test(db);
    let mut queue = MockQueueService::new();
    queue.expect_send().times(0);
    service.queue_service = Arc::new(queue);

    // Act
    let result = service
        .check_redeployable(project.id, environment.id, deployment.id)
        .await;

    // Assert
    match result {
        Err(DeploymentError::InvalidDeploymentState(message)) => {
            assert!(message.contains("records no Git repository"), "{message}");
            assert!(message.contains(&project.slug), "{message}");
        }
        other => panic!("expected a refusal, got {other:?}"),
    }
    Ok(())
}

#[tokio::test]
async fn redeploy_environment_rejects_deployment_from_another_environment(
) -> Result<(), Box<dyn std::error::Error>> {
    let test_db = TestDatabase::with_migrations().await?;
    let db = test_db.connection_arc();
    let (project, environment, deployment, _) = setup_test_deployment(&db).await?;
    let service = create_deployment_service_for_test(db);

    let result = service
        .redeploy_environment(project.id, environment.id + 1, deployment.id)
        .await;

    assert!(matches!(result, Err(DeploymentError::NotFound(_))));
    Ok(())
}

async fn insert_build_outputs(
    db: &temps_database::DbConnection,
    deployment_id: i32,
    outputs: serde_json::Value,
) -> Result<(), Box<dyn std::error::Error>> {
    use temps_entities::{deployment_jobs, types::JobStatus};
    deployment_jobs::ActiveModel {
        deployment_id: Set(deployment_id),
        job_id: Set("build_image".to_string()),
        job_type: Set("BuildImageJob".to_string()),
        name: Set("Build Image".to_string()),
        status: Set(JobStatus::Success),
        log_id: Set(format!("build-{deployment_id}.log")),
        outputs: Set(Some(outputs)),
        ..Default::default()
    }
    .insert(db)
    .await?;
    Ok(())
}

/// A rollback or promotion of a mixed-architecture worker build reuses
/// each architecture's image from the worker that built it, so every
/// replica again runs the image of its own node's architecture. A
/// single-architecture worker build on a control plane that holds its own
/// copy keeps the existing single-tag path, and outputs of another tag are
/// never applied.
#[tokio::test]
async fn rollback_reuses_each_architecture_from_the_worker_that_built_it(
) -> Result<(), Box<dyn std::error::Error>> {
    let test_db = TestDatabase::with_migrations().await?;
    let db = test_db.connection_arc();
    let (_project, _environment, deployment) = setup_test_data(&db).await?;
    insert_build_outputs(
        &db,
        deployment.id,
        serde_json::json!({
            "image_tag": "app-1:latest",
            "image_id": "sha256:amd",
            "builder_node_id": 4,
            "image_tags_by_platform": {
                "linux/amd64": "app-1:latest",
                "linux/arm64": "app-1:latest-arm64"
            },
            "builder_node_ids_by_platform": { "linux/amd64": 4, "linux/arm64": 5 },
            "image_ids_by_platform": { "linux/amd64": "sha256:amd", "linux/arm64": "sha256:arm" }
        }),
    )
    .await?;
    let service = create_deployment_service_for_test(db.clone());

    let reused = service
        .reused_worker_build(deployment.project_id, deployment.id, "app-1:latest")
        .await?
        .expect("a per-architecture build is always reused from its workers");
    assert_eq!(reused.builder_node_id, Some(4));
    assert_eq!(reused.image_id.as_deref(), Some("sha256:amd"));
    assert_eq!(
        reused.builder_node_ids_by_platform,
        HashMap::from([
            ("linux/amd64".to_string(), 4),
            ("linux/arm64".to_string(), 5)
        ])
    );
    assert_eq!(
        reused
            .image_tags_by_platform
            .get("linux/arm64")
            .map(String::as_str),
        Some("app-1:latest-arm64")
    );
    assert_eq!(
        reused
            .image_ids_by_platform
            .get("linux/arm64")
            .map(String::as_str),
        Some("sha256:arm")
    );
    assert!(service
        .reused_worker_build(deployment.project_id, deployment.id, "app-1:other")
        .await?
        .is_none());
    // Another project's deployment never lends its build to this one.
    assert!(service
        .reused_worker_build(deployment.project_id + 1000, deployment.id, "app-1:latest")
        .await?
        .is_none());
    Ok(())
}

#[tokio::test]
async fn rollback_of_a_single_worker_build_needs_the_workers_only_without_a_local_copy(
) -> Result<(), Box<dyn std::error::Error>> {
    let test_db = TestDatabase::with_migrations().await?;
    let db = test_db.connection_arc();
    let (_project, _environment, deployment) = setup_test_data(&db).await?;
    insert_build_outputs(
        &db,
        deployment.id,
        serde_json::json!({
            "image_tag": "app-1:latest",
            "image_id": "sha256:arm",
            "builder_node_id": 5,
            "image_tags_by_platform": { "linux/arm64": "app-1:latest" }
        }),
    )
    .await?;

    // Full profile: the control plane copied the image when it was built.
    let full = create_deployment_service_for_test(db.clone());
    full.set_node_scheduler(Arc::new(crate::services::NodeScheduler::new(Arc::new(
        crate::services::NodeService::new(db.clone()),
    ))));
    assert!(full
        .reused_worker_build(deployment.project_id, deployment.id, "app-1:latest")
        .await?
        .is_none());

    // Control-plane profile: there is no local copy, so the worker is the source.
    let control_plane = create_deployment_service_for_test(db.clone());
    control_plane.set_node_scheduler(Arc::new(
        crate::services::NodeScheduler::new(Arc::new(crate::services::NodeService::new(
            db.clone(),
        )))
        .with_local_workloads_enabled(false),
    ));
    let reused = control_plane
        .reused_worker_build(deployment.project_id, deployment.id, "app-1:latest")
        .await?
        .expect("the worker is the only source of the image");
    assert_eq!(reused.builder_node_id, Some(5));
    assert!(!reused.is_per_architecture());
    Ok(())
}

/// A node that reported the image's configuration was not asked to send
/// the image back, so even a full-profile control plane has no copy: the
/// reuse must name the worker and carry the reported ports.
#[tokio::test]
async fn rollback_of_a_build_that_was_not_copied_back_uses_the_worker_and_its_ports(
) -> Result<(), Box<dyn std::error::Error>> {
    let test_db = TestDatabase::with_migrations().await?;
    let db = test_db.connection_arc();
    let (_project, _environment, deployment) = setup_test_data(&db).await?;
    insert_build_outputs(
        &db,
        deployment.id,
        serde_json::json!({
            "image_tag": "app-1:latest",
            "image_id": "sha256:arm",
            "builder_node_id": 5,
            "image_tags_by_platform": { "linux/arm64": "app-1:latest" },
            "image_config": { "exposed_ports": [8080], "working_dir": "/app" }
        }),
    )
    .await?;
    let full = create_deployment_service_for_test(db.clone());
    full.set_node_scheduler(Arc::new(crate::services::NodeScheduler::new(Arc::new(
        crate::services::NodeService::new(db.clone()),
    ))));

    let reused = full
        .reused_worker_build(deployment.project_id, deployment.id, "app-1:latest")
        .await?
        .expect("the worker is the only holder of an image that was not copied back");

    assert_eq!(reused.builder_node_id, Some(5));
    assert_eq!(
        reused.image_config.map(|config| config.exposed_ports),
        Some(vec![8080])
    );
    Ok(())
}
