// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

use super::create::initial_deployment_config;
use super::templates::applied_service_template_from_model;
use super::*;

use sea_orm::{ActiveModelTrait, Set};

use std::sync::Arc;
use std::sync::Mutex;
use temps_core::async_trait::async_trait;
use temps_core::{JobQueue, QueueError};
use temps_database::test_utils::TestDatabase;
use temps_entities::preset::Preset;
// Mock JobQueue for testing
struct MockJobQueue {
    jobs: Arc<Mutex<Vec<Job>>>,
}

impl MockJobQueue {
    fn new() -> Self {
        Self {
            jobs: Arc::new(Mutex::new(Vec::new())),
        }
    }

    async fn get_jobs(&self) -> Vec<Job> {
        self.jobs.lock().unwrap().clone()
    }
}

#[async_trait]
impl JobQueue for MockJobQueue {
    async fn send(&self, job: Job) -> Result<(), QueueError> {
        self.jobs.lock().unwrap().push(job);
        Ok(())
    }

    fn subscribe(&self) -> Box<dyn temps_core::JobReceiver> {
        unimplemented!("Not needed for these tests")
    }
}

// Helper function to create test services
async fn create_test_services(
    db: Arc<temps_database::DbConnection>,
    mock_queue: Arc<MockJobQueue>,
) -> ProjectService {
    // Create ConfigService
    let server_config = Arc::new(
        temps_config::ServerConfig::new(
            "127.0.0.1:3000".to_string(),
            "postgresql://test".to_string(),
            None,
            None,
        )
        .unwrap(),
    );
    let config_service = Arc::new(temps_config::ConfigService::new(server_config, db.clone()));

    // Create ExternalServiceManager
    let encryption_service = Arc::new(
        temps_core::EncryptionService::new(
            "0000000000000000000000000000000000000000000000000000000000000000",
        )
        .expect("Failed to create encryption service"),
    );

    // Create Docker client for ExternalServiceManager
    let docker = Arc::new(
        bollard::Docker::connect_with_local_defaults()
            .or_else(|_| bollard::Docker::connect_with_http_defaults())
            .expect("a Docker client can be configured without connecting for mock tests"),
    );

    let external_service_manager = Arc::new(temps_providers::ExternalServiceManager::new(
        db.clone(),
        encryption_service.clone(),
        docker,
        Arc::new(temps_providers::DnsRegistry::new(db.clone())),
    ));

    // Create GitProviderManager
    let git_provider_manager = Arc::new(temps_git::GitProviderManager::new(
        db.clone(),
        encryption_service.clone(),
        mock_queue.clone() as Arc<dyn temps_core::JobQueue>,
        config_service.clone(),
    ));

    // Create EnvironmentService
    let environment_service = Arc::new(temps_environments::EnvironmentService::new(
        db.clone(),
        config_service.clone(),
    ));

    ProjectService::new(
        db,
        mock_queue,
        config_service,
        external_service_manager,
        git_provider_manager,
        environment_service,
        encryption_service,
    )
}

async fn insert_reinstall_test_project(
    db: &temps_database::DbConnection,
    slug: &str,
    connection_id: Option<i32>,
) -> projects::Model {
    temps_entities::projects::ActiveModel {
        name: Set(slug.to_string()),
        slug: Set(slug.to_string()),
        repo_name: Set("example-repository".to_string()),
        repo_owner: Set("example-owner".to_string()),
        preset: Set(temps_entities::preset::Preset::NextJs),
        main_branch: Set("main".to_string()),
        directory: Set("/".to_string()),
        git_provider_connection_id: Set(connection_id),
        ..Default::default()
    }
    .insert(db)
    .await
    .unwrap()
}

async fn insert_github_connection(db: &temps_database::DbConnection, auth_method: &str) -> i32 {
    let now = chrono::Utc::now();
    let provider = temps_entities::git_providers::ActiveModel {
        name: Set(format!("github-{auth_method}")),
        provider_type: Set("github".to_string()),
        auth_method: Set(auth_method.to_string()),
        auth_config: Set(serde_json::json!({})),
        is_active: Set(true),
        is_default: Set(false),
        created_at: Set(now),
        updated_at: Set(now),
        ..Default::default()
    }
    .insert(db)
    .await
    .unwrap();
    temps_entities::git_provider_connections::ActiveModel {
        provider_id: Set(provider.id),
        account_name: Set("example-owner".to_string()),
        account_type: Set("User".to_string()),
        is_active: Set(true),
        is_expired: Set(false),
        syncing: Set(false),
        synced_repository_count: Set(0),
        health_status: Set("unknown".to_string()),
        consecutive_health_failures: Set(0),
        created_at: Set(now),
        updated_at: Set(now),
        ..Default::default()
    }
    .insert(db)
    .await
    .unwrap()
    .id
}

#[tokio::test]
async fn reinstall_github_webhook_rejects_a_missing_project() {
    let test_db = TestDatabase::with_migrations().await.unwrap();
    let service = create_test_services(test_db.db.clone(), Arc::new(MockJobQueue::new())).await;

    let result = service.reinstall_github_webhook(999_999).await;

    assert!(
        matches!(result, Err(ProjectError::NotFound(ref msg)) if msg.contains("999999")),
        "{result:?}"
    );
}

#[tokio::test]
async fn reinstall_github_webhook_requires_a_git_connection() {
    let test_db = TestDatabase::with_migrations().await.unwrap();
    let db = test_db.db.clone();
    let service = create_test_services(db.clone(), Arc::new(MockJobQueue::new())).await;
    let project = insert_reinstall_test_project(db.as_ref(), "no-connection", None).await;

    let result = service.reinstall_github_webhook(project.id).await;

    assert!(
        matches!(result, Err(ProjectError::InvalidInput(ref msg)) if msg.contains("no git provider connection")),
        "{result:?}"
    );
}

/// A GitHub App delivers pushes through its own webhook; installing a
/// repository webhook as well would deploy every push twice, so the
/// endpoint refuses (without calling GitHub) and says why.
#[tokio::test]
async fn reinstall_github_webhook_refuses_github_app_connections() {
    let test_db = TestDatabase::with_migrations().await.unwrap();
    let db = test_db.db.clone();
    let service = create_test_services(db.clone(), Arc::new(MockJobQueue::new())).await;
    let connection_id = insert_github_connection(db.as_ref(), "github_app").await;
    let project =
        insert_reinstall_test_project(db.as_ref(), "app-connection", Some(connection_id)).await;

    let result = service.reinstall_github_webhook(project.id).await;

    assert!(
        matches!(result, Err(ProjectError::InvalidInput(ref msg)) if msg.contains("GitHub App")),
        "{result:?}"
    );
    let stored = projects::Entity::find_by_id(project.id)
        .one(db.as_ref())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(stored.github_webhook_id, None);
    assert_eq!(stored.github_webhook_signing_token, None);
}

/// A personal-access-token GitHub connection whose API is `api_url`.
async fn insert_mock_github_pat_connection(
    db: &temps_database::DbConnection,
    api_url: String,
) -> i32 {
    let encryption = temps_core::EncryptionService::new(
        "0000000000000000000000000000000000000000000000000000000000000000",
    )
    .unwrap();
    let provider = temps_entities::git_providers::ActiveModel {
        name: Set("github-pat-mock".to_string()),
        provider_type: Set("github".to_string()),
        base_url: Set(None),
        api_url: Set(Some(api_url)),
        auth_method: Set("pat".to_string()),
        auth_config: Set(serde_json::json!({
            "PersonalAccessToken": { "token": "synthetic-token" }
        })),
        webhook_secret: Set(None),
        is_active: Set(true),
        is_default: Set(false),
        ..Default::default()
    }
    .insert(db)
    .await
    .unwrap();
    temps_entities::git_provider_connections::ActiveModel {
        provider_id: Set(provider.id),
        account_name: Set("example-owner".to_string()),
        account_type: Set("User".to_string()),
        access_token: Set(Some(encryption.encrypt_string("synthetic-token").unwrap())),
        is_active: Set(true),
        is_expired: Set(false),
        syncing: Set(false),
        synced_repository_count: Set(0),
        health_status: Set("unknown".to_string()),
        consecutive_health_failures: Set(0),
        ..Default::default()
    }
    .insert(db)
    .await
    .unwrap()
    .id
}

/// Mock GitHub's "create a repository webhook" for `owner/repo`,
/// answering with `hook_id` and recording the request body.
async fn mock_github_hook_creation(
    server: &mut mockito::Server,
    repo: &str,
    hook_id: i64,
) -> (
    mockito::Mock,
    Arc<std::sync::Mutex<Option<serde_json::Value>>>,
) {
    mock_github_hook_request(
        server,
        "POST",
        &format!("/repos/example-owner/{repo}/hooks"),
        201,
        hook_id,
    )
    .await
}

/// Mock one GitHub webhook request, answering `status` with a hook whose
/// ID is `hook_id` and recording the request body. Expects one call.
async fn mock_github_hook_request(
    server: &mut mockito::Server,
    method: &str,
    path: &str,
    status: usize,
    hook_id: i64,
) -> (
    mockito::Mock,
    Arc<std::sync::Mutex<Option<serde_json::Value>>>,
) {
    let captured = Arc::new(std::sync::Mutex::new(None));
    let sink = captured.clone();
    let mock = server
        .mock(method, path)
        .with_status(status)
        .with_header("content-type", "application/json")
        .with_body_from_request(move |request| {
            let body = request.body().map(|b| b.to_vec()).unwrap_or_default();
            *sink.lock().unwrap() = serde_json::from_slice(&body).ok();
            if (200..300).contains(&status) {
                serde_json::json!({ "id": hook_id })
            } else {
                serde_json::json!({ "message": "Not Found" })
            }
            .to_string()
            .into_bytes()
        })
        .expect(1)
        .create_async()
        .await;
    (mock, captured)
}

/// Mocks for webhook calls a test expects not to happen.
async fn forbid_github_hook_calls(
    server: &mut mockito::Server,
    methods: &[&str],
) -> Vec<mockito::Mock> {
    let mut forbidden = Vec::new();
    for method in methods {
        forbidden.push(
            server
                .mock(method, mockito::Matcher::Regex("/hooks".into()))
                .expect(0)
                .create_async()
                .await,
        );
    }
    forbidden
}

async fn insert_project_with_github_hook(
    db: &temps_database::DbConnection,
    slug: &str,
    connection_id: i32,
    hook_id: i64,
    encrypted_secret: String,
) -> projects::Model {
    let project = insert_reinstall_test_project(db, slug, Some(connection_id)).await;
    let mut active: projects::ActiveModel = project.into();
    active.directory = Set(".".to_string());
    active.github_webhook_id = Set(Some(hook_id));
    active.github_webhook_signing_token = Set(Some(encrypted_secret));
    active.update(db).await.unwrap()
}

fn decrypt_with_test_key(encrypted: &str) -> String {
    temps_core::EncryptionService::new(
        "0000000000000000000000000000000000000000000000000000000000000000",
    )
    .unwrap()
    .decrypt_string(encrypted)
    .unwrap()
}

/// Reinstalling gives the project's existing hook a new secret in place:
/// GitHub refuses a second hook with the same URL and events, so it must
/// not register another one.
#[tokio::test]
async fn reinstall_github_webhook_updates_the_existing_hook_in_place() {
    let test_db = TestDatabase::with_migrations().await.unwrap();
    let db = test_db.db.clone();
    let service = create_test_services(db.clone(), Arc::new(MockJobQueue::new())).await;
    let mut server = mockito::Server::new_async().await;
    let connection_id = insert_mock_github_pat_connection(db.as_ref(), server.url()).await;
    let project = insert_project_with_github_hook(
        db.as_ref(),
        "reinstall-update",
        connection_id,
        11,
        test_db_encrypt("old-secret"),
    )
    .await;
    let (update, captured) = mock_github_hook_request(
        &mut server,
        "PATCH",
        "/repos/example-owner/example-repository/hooks/11",
        200,
        11,
    )
    .await;
    let forbidden = forbid_github_hook_calls(&mut server, &["POST", "DELETE"]).await;

    let hook_id = service.reinstall_github_webhook(project.id).await.unwrap();

    assert_eq!(hook_id, 11);
    update.assert_async().await;
    for mock in forbidden {
        mock.assert_async().await;
    }
    let request = captured.lock().unwrap().clone().expect("hook update body");
    assert_eq!(request["events"], serde_json::json!(["push"]));
    assert_eq!(request["active"], serde_json::json!(true));
    assert!(
        request["config"]["url"]
            .as_str()
            .unwrap()
            .ends_with(&format!(
                "/api/webhook/git/github/repo-events?project_id={}",
                project.id
            )),
        "{request}"
    );
    let stored = projects::Entity::find_by_id(project.id)
        .one(db.as_ref())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(stored.github_webhook_id, Some(11));
    let stored_secret =
        decrypt_with_test_key(stored.github_webhook_signing_token.as_deref().unwrap());
    assert_eq!(
        request["config"]["secret"],
        serde_json::json!(stored_secret)
    );
    assert_eq!(stored_secret.len(), 64);
    assert_ne!(stored_secret, "old-secret");
}

/// When the stored hook was deleted on GitHub, reinstalling registers a
/// new one and stores its ID and secret.
#[tokio::test]
async fn reinstall_github_webhook_registers_a_new_hook_when_the_old_one_is_gone() {
    let test_db = TestDatabase::with_migrations().await.unwrap();
    let db = test_db.db.clone();
    let service = create_test_services(db.clone(), Arc::new(MockJobQueue::new())).await;
    let mut server = mockito::Server::new_async().await;
    let connection_id = insert_mock_github_pat_connection(db.as_ref(), server.url()).await;
    let project = insert_project_with_github_hook(
        db.as_ref(),
        "reinstall-recreate",
        connection_id,
        11,
        test_db_encrypt("old-secret"),
    )
    .await;
    let (update, _) = mock_github_hook_request(
        &mut server,
        "PATCH",
        "/repos/example-owner/example-repository/hooks/11",
        404,
        11,
    )
    .await;
    let (create, captured) = mock_github_hook_creation(&mut server, "example-repository", 22).await;
    let forbidden = forbid_github_hook_calls(&mut server, &["DELETE"]).await;

    let hook_id = service.reinstall_github_webhook(project.id).await.unwrap();

    assert_eq!(hook_id, 22);
    update.assert_async().await;
    create.assert_async().await;
    for mock in forbidden {
        mock.assert_async().await;
    }
    let request = captured
        .lock()
        .unwrap()
        .clone()
        .expect("hook creation body");
    let stored = projects::Entity::find_by_id(project.id)
        .one(db.as_ref())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(stored.github_webhook_id, Some(22));
    assert_eq!(
        request["config"]["secret"],
        serde_json::json!(decrypt_with_test_key(
            stored.github_webhook_signing_token.as_deref().unwrap()
        ))
    );
}

/// A reinstall GitHub refuses leaves the project's existing hook in place
/// on GitHub and its ID and secret in the database.
#[tokio::test]
async fn failed_github_webhook_reinstall_keeps_the_existing_hook() {
    let test_db = TestDatabase::with_migrations().await.unwrap();
    let db = test_db.db.clone();
    let service = create_test_services(db.clone(), Arc::new(MockJobQueue::new())).await;
    let mut server = mockito::Server::new_async().await;
    let connection_id = insert_mock_github_pat_connection(db.as_ref(), server.url()).await;
    let old_secret = test_db_encrypt("old-secret");
    let project = insert_project_with_github_hook(
        db.as_ref(),
        "reinstall-failure",
        connection_id,
        11,
        old_secret.clone(),
    )
    .await;
    server
        .mock("PATCH", "/repos/example-owner/example-repository/hooks/11")
        .with_status(403)
        .with_header("content-type", "application/json")
        .with_body(r#"{"message":"Must have admin rights to Repository."}"#)
        .create_async()
        .await;
    let forbidden = forbid_github_hook_calls(&mut server, &["POST", "DELETE"]).await;

    let result = service.reinstall_github_webhook(project.id).await;

    assert!(
        matches!(result, Err(ProjectError::GitHubError(_))),
        "{result:?}"
    );
    for mock in forbidden {
        mock.assert_async().await;
    }
    let stored = projects::Entity::find_by_id(project.id)
        .one(db.as_ref())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(stored.github_webhook_id, Some(11));
    assert_eq!(stored.github_webhook_signing_token, Some(old_secret));
}

/// A Git settings save resends the connection ID with every edit. Only a
/// change of repository replaces the hook: a branch change keeps it.
#[tokio::test]
async fn git_settings_saves_replace_the_github_webhook_only_on_a_repository_change() {
    let test_db = TestDatabase::with_migrations().await.unwrap();
    let db = test_db.db.clone();
    let service = create_test_services(db.clone(), Arc::new(MockJobQueue::new())).await;
    let mut server = mockito::Server::new_async().await;
    let connection_id = insert_mock_github_pat_connection(db.as_ref(), server.url()).await;
    let old_secret = test_db_encrypt("old-secret");
    let project = insert_project_with_github_hook(
        db.as_ref(),
        "settings-save",
        connection_id,
        11,
        old_secret.clone(),
    )
    .await;
    let save = |branch: &str, repo: &str| {
        service.update_git_settings(
            project.id,
            Some(connection_id),
            branch.to_string(),
            "example-owner".to_string(),
            repo.to_string(),
            None,
            ".".to_string(),
            None,
            None,
            None,
            None,
            DeployCaller::default(),
        )
    };

    // Each save verifies the branch exists.
    server
        .mock("GET", mockito::Matcher::Regex("/commits/release$".into()))
        .with_status(200)
        .with_header("content-type", "application/json")
        .with_body(
            serde_json::json!({
                "sha": "0123456789abcdef0123456789abcdef01234567",
                "commit": {
                    "message": "release",
                    "author": {
                        "name": "Example Author",
                        "email": "author@example.com",
                        "date": "2026-01-01T00:00:00Z"
                    }
                }
            })
            .to_string(),
        )
        .create_async()
        .await;
    let untouched = forbid_github_hook_calls(&mut server, &["POST", "PATCH", "DELETE"]).await;
    save("release", "example-repository").await.unwrap();
    for mock in untouched {
        mock.assert_async().await;
        mock.remove_async().await;
    }
    let stored = projects::Entity::find_by_id(project.id)
        .one(db.as_ref())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(stored.github_webhook_id, Some(11));
    assert_eq!(stored.github_webhook_signing_token, Some(old_secret));

    let delete = server
        .mock("DELETE", "/repos/example-owner/example-repository/hooks/11")
        .with_status(204)
        .expect(1)
        .create_async()
        .await;
    let (create, _) = mock_github_hook_creation(&mut server, "other-repository", 33).await;
    save("release", "other-repository").await.unwrap();
    delete.assert_async().await;
    create.assert_async().await;
    let stored = projects::Entity::find_by_id(project.id)
        .one(db.as_ref())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(stored.github_webhook_id, Some(33));
    assert!(stored.github_webhook_signing_token.is_some());
}

/// Creating a project on a GitHub PAT connection registers its
/// repository webhook and stores the hook ID and secret.
#[tokio::test]
async fn creating_a_project_on_a_github_pat_connection_installs_its_webhook() {
    let test_db = TestDatabase::with_migrations().await.unwrap();
    let db = test_db.db.clone();
    let service = create_test_services(db.clone(), Arc::new(MockJobQueue::new())).await;
    let mut server = mockito::Server::new_async().await;
    let connection_id = insert_mock_github_pat_connection(db.as_ref(), server.url()).await;
    let (create, captured) = mock_github_hook_creation(&mut server, "starter", 44).await;
    let mut request = template_service_request("Webhook Install", "webhook-install");
    request.repo_owner = Some("example-owner".to_string());
    request.git_provider_connection_id = Some(connection_id);

    let project = service.create_project(request).await.unwrap();

    create.assert_async().await;
    let stored = projects::Entity::find_by_id(project.id)
        .one(db.as_ref())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(stored.github_webhook_id, Some(44));
    let request = captured
        .lock()
        .unwrap()
        .clone()
        .expect("hook creation body");
    assert!(
        request["config"]["url"]
            .as_str()
            .unwrap()
            .ends_with(&format!(
                "/api/webhook/git/github/repo-events?project_id={}",
                project.id
            )),
        "{request}"
    );
    assert_eq!(
        request["config"]["secret"],
        serde_json::json!(decrypt_with_test_key(
            stored.github_webhook_signing_token.as_deref().unwrap()
        ))
    );
}

fn test_db_encrypt(value: &str) -> String {
    temps_core::EncryptionService::new(
        "0000000000000000000000000000000000000000000000000000000000000000",
    )
    .unwrap()
    .encrypt_string(value)
    .unwrap()
}

#[tokio::test]
async fn begin_project_deletion_persists_idempotent_deployment_fence() {
    let test_db = TestDatabase::with_migrations().await.unwrap();
    let db = test_db.db.clone();
    let service = create_test_services(db.clone(), Arc::new(MockJobQueue::new())).await;
    let project = temps_entities::projects::ActiveModel {
        name: Set("Deleting Project".to_string()),
        slug: Set("deleting-project".to_string()),
        repo_name: Set("repo".to_string()),
        repo_owner: Set("owner".to_string()),
        preset: Set(temps_entities::preset::Preset::NextJs),
        main_branch: Set("main".to_string()),
        directory: Set("/".to_string()),
        ..Default::default()
    }
    .insert(db.as_ref())
    .await
    .unwrap();

    service.begin_project_deletion(project.id).await.unwrap();
    service.begin_project_deletion(project.id).await.unwrap();

    let fenced = temps_entities::projects::Entity::find_by_id(project.id)
        .one(db.as_ref())
        .await
        .unwrap()
        .expect("project remains until container cleanup completes");
    assert!(fenced.is_deleted);
    assert!(fenced.deleted_at.is_some());
}

/// A project whose app runs elsewhere is never deployed: creating it
/// queues no first deployment even with a repository, and it can be
/// switched to hosted and back.
#[tokio::test]
async fn an_external_project_is_not_deployed_on_creation() {
    let Ok(test_db) = TestDatabase::with_migrations().await else {
        eprintln!("Test database unavailable, skipping");
        return;
    };
    let db = test_db.db.clone();
    let queue = Arc::new(MockJobQueue::new());
    let service = create_test_services(db.clone(), queue.clone()).await;

    let project = service
        .create_project(CreateProjectRequest {
            cloudflare_enabled: None,
            delivery_provider: None,
            name: "Marketing Site".to_string(),
            expected_slug: None,
            repo_name: Some("site".to_string()),
            repo_owner: Some("acme".to_string()),
            directory: "/".to_string(),
            main_branch: "main".to_string(),
            preset: Preset::Nixpacks.to_string(),
            preset_config: None,
            environment_variables: None,
            git_url: None,
            git_provider_connection_id: None,
            automatic_deploy: false,
            exposed_port: None,
            cpu_request: None,
            cpu_limit: None,
            memory_request: None,
            memory_limit: None,
            is_public_repo: None,
            storage_service_ids: vec![],
            storage_service_claim_ids: vec![],
            storage_service_claim_user_id: None,
            source_type: temps_entities::source_type::SourceType::Git,
            template_slug: None,
            hosting: temps_entities::types::Hosting::External,
        })
        .await
        .unwrap();

    assert_eq!(project.hosting, temps_entities::types::Hosting::External);
    let deploys = queue
        .get_jobs()
        .await
        .into_iter()
        .filter(|job| matches!(job, Job::GitPushEvent(_)))
        .count();
    assert_eq!(deploys, 0, "an external project must not be deployed");

    let hosted = service
        .set_hosting(
            project.id,
            temps_entities::types::Hosting::Hosted,
            temps_core::docker_socket_grant::DeployCaller::default(),
        )
        .await
        .unwrap();
    assert_eq!(hosted.hosting, temps_entities::types::Hosting::Hosted);
    let external = service
        .set_hosting(
            project.id,
            temps_entities::types::Hosting::External,
            temps_core::docker_socket_grant::DeployCaller::default(),
        )
        .await
        .unwrap();
    assert_eq!(external.hosting, temps_entities::types::Hosting::External);
}

fn template_service_request(name: &str, slug: &str) -> CreateProjectRequest {
    CreateProjectRequest {
        cloudflare_enabled: None,
        delivery_provider: None,
        name: name.to_string(),
        expected_slug: Some(slug.to_string()),
        repo_name: Some("starter".to_string()),
        repo_owner: Some("example".to_string()),
        directory: ".".to_string(),
        main_branch: "main".to_string(),
        preset: Preset::Nixpacks.to_string(),
        preset_config: None,
        environment_variables: None,
        git_url: None,
        git_provider_connection_id: None,
        automatic_deploy: false,
        exposed_port: None,
        cpu_request: None,
        cpu_limit: None,
        memory_request: None,
        memory_limit: None,
        is_public_repo: None,
        storage_service_ids: vec![],
        storage_service_claim_ids: vec![],
        storage_service_claim_user_id: None,
        source_type: temps_entities::source_type::SourceType::Git,
        template_slug: None,
        hosting: Default::default(),
    }
}

/// The final ownership token appears only on a fully initialized project
/// (it has its environment), is absent from every other creation path,
/// never reaches a serialized project row, and can be held by only one
/// project.
#[tokio::test]
async fn a_template_service_takes_its_instantiation_token_once_initialized() {
    let Ok(test_db) = TestDatabase::with_migrations().await else {
        eprintln!("Test database unavailable, skipping");
        return;
    };
    let db = test_db.db.clone();
    let service = create_test_services(db.clone(), Arc::new(MockJobQueue::new())).await;
    let caller = SlugClaimCaller::project_writer();

    let token = temps_entities::projects::instantiation_token(41, "backend");
    let owned = service
        .create_template_service_as(
            template_service_request("Owned Backend", "owned-backend"),
            None,
            &caller,
            true,
            Some(token.clone()),
        )
        .await
        .unwrap();
    let stored = temps_entities::projects::Entity::find_by_id(owned.id)
        .one(db.as_ref())
        .await
        .unwrap()
        .expect("project row");
    assert_eq!(stored.instantiation_token.as_deref(), Some(token.as_str()));
    assert!(
        environment_count(db.as_ref(), owned.id).await > 0,
        "a project holding the final token is initialized"
    );
    let serialized = serde_json::to_value(&stored).unwrap();
    assert!(
        serialized.get("instantiation_token").is_none(),
        "the ownership token must not be serialized: {serialized}"
    );

    // Every other creation path leaves the column NULL.
    let plain = service
        .create_template_service_as(
            template_service_request("Plain Template Service", "plain-template-service"),
            None,
            &caller,
            true,
            None,
        )
        .await
        .unwrap();
    let ordinary = service
        .create_project(template_service_request("Ordinary", "ordinary"))
        .await
        .unwrap();
    for id in [plain.id, ordinary.id] {
        let row = temps_entities::projects::Entity::find_by_id(id)
            .one(db.as_ref())
            .await
            .unwrap()
            .expect("project row");
        assert_eq!(row.instantiation_token, None, "project {id}");
    }

    // A second project cannot carry the same token.
    let error = service
        .create_template_service_as(
            template_service_request("Imposter", "imposter"),
            None,
            &caller,
            true,
            Some(token.clone()),
        )
        .await
        .err()
        .expect("a token has at most one project");
    let message = error.to_string();
    assert!(
        message.contains(&token) && message.contains("owned-backend"),
        "the error names the token and its holder: {message}"
    );
    assert!(
        service.get_project_by_slug("imposter").await.is_err(),
        "the refused creation left no project behind"
    );
}

async fn environment_count(db: &sea_orm::DatabaseConnection, project_id: i32) -> u64 {
    use sea_orm::PaginatorTrait;
    temps_entities::environments::Entity::find()
        .filter(temps_entities::environments::Column::ProjectId.eq(project_id))
        .count(db)
        .await
        .unwrap()
}

async fn project_by_slug(
    db: &sea_orm::DatabaseConnection,
    slug: &str,
) -> Option<temps_entities::projects::Model> {
    temps_entities::projects::Entity::find()
        .filter(temps_entities::projects::Column::Slug.eq(slug))
        .one(db)
        .await
        .unwrap()
}

/// A creation that fails inside initialization (here, after the
/// environment and a first variable exist: a duplicate variable) leaves
/// no project, environment or variable at all, tokened or not -- neither the final token nor the pending one
/// stays behind to be adopted, and the same token creates a project on
/// the next attempt.
#[tokio::test]
async fn a_failed_initialization_leaves_no_tokened_project_behind() {
    let Ok(test_db) = TestDatabase::with_migrations().await else {
        eprintln!("Test database unavailable, skipping");
        return;
    };
    let db = test_db.db.clone();
    let service = create_test_services(db.clone(), Arc::new(MockJobQueue::new())).await;
    let caller = SlugClaimCaller::project_writer();
    let token = temps_entities::projects::instantiation_token(52, "backend");

    let mut request = template_service_request("Half Backend", "half-backend");
    // The second `API_TOKEN` is refused once the environment and the
    // first one exist, so initialization is cut short half-way.
    request.environment_variables = Some(vec![
        CreateProjectEnvVar {
            key: "API_TOKEN".to_string(),
            value: "first".to_string(),
            is_secret: false,
        },
        CreateProjectEnvVar {
            key: "API_TOKEN".to_string(),
            value: "second".to_string(),
            is_secret: false,
        },
    ]);
    let error = service
        .create_template_service_as(request, None, &caller, true, Some(token.clone()))
        .await
        .err()
        .expect("a duplicate variable fails initialization");
    assert!(
        matches!(error, ProjectError::EnvVarCreationFailed { .. }),
        "the failure happens inside initialization, after the insert: {error:?}"
    );

    assert!(
        project_by_slug(db.as_ref(), "half-backend").await.is_none(),
        "the half-initialized project was rolled back"
    );
    assert_eq!(
        temps_entities::environments::Entity::find()
            .all(db.as_ref())
            .await
            .unwrap()
            .len(),
        0,
        "its environment went with it"
    );
    let holders = temps_entities::projects::Entity::find()
        .filter(temps_entities::projects::Column::InstantiationToken.is_in([
            token.clone(),
            temps_entities::projects::pending_instantiation_token(&token),
        ]))
        .all(db.as_ref())
        .await
        .unwrap();
    assert!(
        holders.is_empty(),
        "no project holds the token: {holders:?}"
    );

    let retried = service
        .create_template_service_as(
            template_service_request("Half Backend", "half-backend"),
            None,
            &caller,
            true,
            Some(token.clone()),
        )
        .await
        .unwrap();
    let stored = temps_entities::projects::Entity::find_by_id(retried.id)
        .one(db.as_ref())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(stored.instantiation_token.as_deref(), Some(token.as_str()));
}

/// The crash window: a project inserted and never sealed (the process
/// stopped before it finished initializing) holds the pending token. It
/// is not the final token an adopter looks for, it blocks a second
/// creation for the same service until discarded, and discarding it
/// frees the service for a clean creation.
#[tokio::test]
async fn an_unfinished_instantiation_project_is_discarded_not_adopted() {
    let Ok(test_db) = TestDatabase::with_migrations().await else {
        eprintln!("Test database unavailable, skipping");
        return;
    };
    let db = test_db.db.clone();
    let service = create_test_services(db.clone(), Arc::new(MockJobQueue::new())).await;
    let caller = SlugClaimCaller::project_writer();
    let token = temps_entities::projects::instantiation_token(53, "backend");
    let pending = temps_entities::projects::pending_instantiation_token(&token);

    // What a process that stopped right after the INSERT leaves.
    let orphan = temps_entities::projects::ActiveModel {
        name: Set("Orphan Backend".to_string()),
        slug: Set("orphan-backend".to_string()),
        repo_name: Set("starter".to_string()),
        repo_owner: Set("example".to_string()),
        directory: Set(".".to_string()),
        main_branch: Set("main".to_string()),
        preset: Set(Preset::Nixpacks),
        source_type: Set(temps_entities::source_type::SourceType::Git),
        instantiation_token: Set(Some(pending.clone())),
        ..Default::default()
    }
    .insert(db.as_ref())
    .await
    .unwrap();
    assert_eq!(environment_count(db.as_ref(), orphan.id).await, 0);
    assert!(
        temps_entities::projects::Entity::find()
            .filter(temps_entities::projects::Column::InstantiationToken.eq(token.as_str()))
            .one(db.as_ref())
            .await
            .unwrap()
            .is_none(),
        "an unfinished project never holds the final token"
    );

    // It cannot be mistaken for a second creation succeeding.
    let blocked = service
        .create_template_service_as(
            template_service_request("Replacement Backend", "replacement-backend"),
            None,
            &caller,
            true,
            Some(token.clone()),
        )
        .await
        .err()
        .expect("the unfinished project still holds the service");
    assert!(
        blocked.to_string().contains("orphan-backend"),
        "the error names the holder: {blocked}"
    );
    assert!(project_by_slug(db.as_ref(), "replacement-backend")
        .await
        .is_none());

    // Only a pending token can be discarded this way, and only the row
    // that still holds it.
    assert!(service
        .discard_unfinished_instantiation_project(orphan.id, &token)
        .await
        .is_err());
    assert!(!service
        .discard_unfinished_instantiation_project(
            orphan.id,
            &temps_entities::projects::pending_instantiation_token("instantiation:99:service:x"),
        )
        .await
        .unwrap());
    assert!(project_by_slug(db.as_ref(), "orphan-backend")
        .await
        .is_some());

    assert!(service
        .discard_unfinished_instantiation_project(orphan.id, &pending)
        .await
        .unwrap());
    assert!(project_by_slug(db.as_ref(), "orphan-backend")
        .await
        .is_none());

    // The service can now be created, completely, under the same token.
    let created = service
        .create_template_service_as(
            template_service_request("Orphan Backend", "orphan-backend"),
            None,
            &caller,
            true,
            Some(token.clone()),
        )
        .await
        .unwrap();
    assert!(environment_count(db.as_ref(), created.id).await > 0);
    let stored = temps_entities::projects::Entity::find_by_id(created.id)
        .one(db.as_ref())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(stored.instantiation_token.as_deref(), Some(token.as_str()));

    // A sealed project is never discarded through its (old) pending token.
    assert!(!service
        .discard_unfinished_instantiation_project(created.id, &pending)
        .await
        .unwrap());
    assert!(project_by_slug(db.as_ref(), "orphan-backend")
        .await
        .is_some());
}

/// Opting a Git project into alternate sources must leave everything that
/// makes it a Git project intact. If `source_type` flipped here, the
/// project would lose rollback rebuild-from-source (which keys off
/// `source_type == Git` in temps-deployments) while still looking fine.
#[tokio::test]
async fn allowing_alternate_sources_preserves_git_configuration() {
    let Ok(test_db) = TestDatabase::with_migrations().await else {
        eprintln!("Test database unavailable, skipping");
        return;
    };
    let db = test_db.db.clone();
    let service = create_test_services(db.clone(), Arc::new(MockJobQueue::new())).await;
    let project = temps_entities::projects::ActiveModel {
        name: Set("Flexible Project".to_string()),
        slug: Set("flexible-project".to_string()),
        repo_name: Set("repo".to_string()),
        repo_owner: Set("owner".to_string()),
        preset: Set(Preset::NextJs),
        main_branch: Set("main".to_string()),
        directory: Set("/".to_string()),
        source_type: Set(temps_entities::source_type::SourceType::Git),
        ..Default::default()
    }
    .insert(db.as_ref())
    .await
    .unwrap();
    assert_eq!(project.allow_alternate_sources, None, "defaults to off");

    let opted_in = service
        .set_allow_alternate_sources(
            project.id,
            true,
            temps_core::docker_socket_grant::DeployCaller::default(),
        )
        .await
        .unwrap();

    assert_eq!(opted_in.allow_alternate_sources, Some(true));
    assert_eq!(
        opted_in.source_type,
        temps_entities::source_type::SourceType::Git,
        "the project must remain git-sourced"
    );
    assert_eq!(opted_in.repo_owner.as_deref(), Some("owner"));
    assert_eq!(opted_in.repo_name.as_deref(), Some("repo"));
    assert_eq!(opted_in.main_branch, "main");

    // And it must be reversible, without disturbing git config either.
    let opted_out = service
        .set_allow_alternate_sources(
            project.id,
            false,
            temps_core::docker_socket_grant::DeployCaller::default(),
        )
        .await
        .unwrap();
    assert_eq!(opted_out.allow_alternate_sources, Some(false));
    assert_eq!(
        opted_out.source_type,
        temps_entities::source_type::SourceType::Git
    );
    assert_eq!(opted_out.repo_owner.as_deref(), Some("owner"));
}

/// The console draws a provider logo for each project. Before this, it had
/// nothing authoritative to draw it from and guessed from the clone URL —
/// which reports GitHub for every project whose URL is missing, and for
/// self-hosted instances whose hostname doesn't happen to spell out the
/// vendor. A GitLab-connected project must report GitLab.
#[tokio::test]
async fn connected_projects_report_the_provider_they_are_linked_to() {
    let Ok(test_db) = TestDatabase::with_migrations().await else {
        eprintln!("Test database unavailable, skipping");
        return;
    };
    let db = test_db.db.clone();
    let service = create_test_services(db.clone(), Arc::new(MockJobQueue::new())).await;

    // Self-hosted GitLab on a hostname that says nothing about the vendor,
    // and a project connected to it that stores no clone URL at all.
    let provider = temps_entities::git_providers::ActiveModel {
        name: Set("Internal Git".to_string()),
        provider_type: Set("gitlab".to_string()),
        base_url: Set(Some("https://code.example.internal".to_string())),
        api_url: Set(None),
        auth_method: Set("pat".to_string()),
        auth_config: Set(serde_json::json!({})),
        webhook_secret: Set(None),
        is_active: Set(true),
        is_default: Set(false),
        ..Default::default()
    }
    .insert(db.as_ref())
    .await
    .unwrap();

    let connection = temps_entities::git_provider_connections::ActiveModel {
        provider_id: Set(provider.id),
        user_id: Set(None),
        account_name: Set("platform-team".to_string()),
        account_type: Set("Organization".to_string()),
        is_active: Set(true),
        ..Default::default()
    }
    .insert(db.as_ref())
    .await
    .unwrap();

    let connected = temps_entities::projects::ActiveModel {
        name: Set("Connected Project".to_string()),
        slug: Set("connected-project".to_string()),
        repo_name: Set("api".to_string()),
        repo_owner: Set("platform-team".to_string()),
        preset: Set(Preset::NextJs),
        main_branch: Set("main".to_string()),
        directory: Set("/".to_string()),
        git_url: Set(None),
        git_provider_connection_id: Set(Some(connection.id)),
        ..Default::default()
    }
    .insert(db.as_ref())
    .await
    .unwrap();

    let standalone = temps_entities::projects::ActiveModel {
        name: Set("Image Project".to_string()),
        slug: Set("image-project".to_string()),
        repo_name: Set("image".to_string()),
        repo_owner: Set("owner".to_string()),
        preset: Set(Preset::NextJs),
        main_branch: Set("main".to_string()),
        directory: Set("/".to_string()),
        git_provider_connection_id: Set(None),
        ..Default::default()
    }
    .insert(db.as_ref())
    .await
    .unwrap();

    let fetched = service.get_project(connected.id).await.unwrap();
    assert_eq!(fetched.git_provider_type.as_deref(), Some("gitlab"));

    let by_slug = service
        .get_project_by_slug("connected-project")
        .await
        .unwrap();
    assert_eq!(by_slug.git_provider_type.as_deref(), Some("gitlab"));

    // A project with no connection has no provider to report — it must stay
    // empty rather than fall back to a guess.
    let unconnected = service.get_project(standalone.id).await.unwrap();
    assert_eq!(unconnected.git_provider_type, None);

    // The dashboard list carries the provider in the same query.
    let (listed, total) = service
        .get_projects_paginated_excluding_search(1, 100, &[], None)
        .await
        .unwrap();
    let matching = |id: i32| listed.iter().filter(move |project| project.id == id);
    let listed_connected = matching(connected.id)
        .next()
        .expect("connected project is listed");
    let listed_standalone = matching(standalone.id)
        .next()
        .expect("standalone project is listed");
    assert_eq!(
        listed_connected.git_provider_type.as_deref(),
        Some("gitlab")
    );
    assert_eq!(listed_standalone.git_provider_type, None);

    // The provider join must not fan a project out into several rows, and
    // must not drift from the count query, which stays join-free.
    assert_eq!(matching(connected.id).count(), 1, "no duplicate rows");
    assert_eq!(
        total,
        listed.len() as i64,
        "count query and page agree once the provider join is in the page query"
    );

    // A write answers with the provider too. A client that trusts its own
    // mutation response would otherwise downgrade a connected project to
    // "no provider" until its next read.
    let after_write = service
        .set_allow_alternate_sources(
            connected.id,
            true,
            temps_core::docker_socket_grant::DeployCaller::default(),
        )
        .await
        .unwrap();
    assert_eq!(
        after_write.git_provider_type.as_deref(),
        Some("gitlab"),
        "an update response must not drop the connected provider"
    );

    let after_write_unconnected = service
        .set_allow_alternate_sources(
            standalone.id,
            true,
            temps_core::docker_socket_grant::DeployCaller::default(),
        )
        .await
        .unwrap();
    assert_eq!(after_write_unconnected.git_provider_type, None);
}

#[tokio::test]
async fn test_update_project_emits_event() {
    // Setup test database
    let test_db = TestDatabase::with_migrations().await.unwrap();
    let db = test_db.db.clone();

    // Create mock queue service
    let mock_queue = Arc::new(MockJobQueue::new());

    // Create project service
    let project_service = create_test_services(db.clone(), mock_queue.clone()).await;

    // Insert a test project
    let project = temps_entities::projects::ActiveModel {
        name: Set("Test Project".to_string()),
        slug: Set("test-project".to_string()),
        repo_name: Set("test-repo".to_string()),
        repo_owner: Set("test-owner".to_string()),
        directory: Set("test-project".to_string()),
        git_provider_connection_id: Set(None),
        main_branch: Set("main".to_string()),
        preset: Set(Preset::Nixpacks),
        ..Default::default()
    };

    let inserted_project = project.insert(db.as_ref()).await.unwrap();

    // Update the project
    let update_request = CreateProjectRequest {
        cloudflare_enabled: None,
        delivery_provider: None,
        name: "Updated Test Project".to_string(),
        expected_slug: None,
        repo_name: None,
        repo_owner: None,
        directory: "/".to_string(),
        main_branch: "develop".to_string(),
        preset: Preset::Nixpacks.to_string(),
        preset_config: None,
        environment_variables: None,
        git_url: None,
        git_provider_connection_id: None,
        automatic_deploy: false,
        exposed_port: None,
        cpu_request: None,
        cpu_limit: None,
        memory_request: None,
        memory_limit: None,
        is_public_repo: None,
        storage_service_ids: vec![],
        storage_service_claim_ids: vec![],
        storage_service_claim_user_id: None,
        source_type: temps_entities::source_type::SourceType::Git,
        template_slug: None,
        hosting: Default::default(),
    };

    let result = project_service
        .update_project(
            inserted_project.id,
            update_request,
            temps_core::docker_socket_grant::DeployCaller::default(),
        )
        .await;

    assert!(result.is_ok(), "update_project should succeed");

    // Verify event was emitted
    let jobs = mock_queue.get_jobs().await;
    assert_eq!(jobs.len(), 1, "Should emit exactly one job");

    match &jobs[0] {
        Job::ProjectUpdated(job) => {
            assert_eq!(job.project_id, inserted_project.id);
            assert_eq!(job.project_name, "Updated Test Project");
        }
        _ => panic!("Expected ProjectUpdated job"),
    }
}

/// ADR 066 I10: a Shield mode is stored with the attack-mode column the
/// older consoles read, and a legacy attack-mode write maps onto a mode.
#[tokio::test]
async fn shield_mode_and_legacy_attack_mode_are_written_together() {
    if !docker_available().await {
        println!("Docker not available, skipping");
        return;
    }
    let test_db = TestDatabase::with_migrations().await.unwrap();
    let db = test_db.db.clone();
    let project_service = create_test_services(db.clone(), Arc::new(MockJobQueue::new())).await;
    let project = temps_entities::projects::ActiveModel {
        name: Set("Shield".to_string()),
        slug: Set("shield-modes".to_string()),
        repo_name: Set("shield-repo".to_string()),
        repo_owner: Set("test-owner".to_string()),
        directory: Set("shield-dir".to_string()),
        main_branch: Set("main".to_string()),
        preset: Set(Preset::Nixpacks),
        ..Default::default()
    }
    .insert(db.as_ref())
    .await
    .unwrap();
    let stored = |id| {
        let db = db.clone();
        async move {
            let row = projects::Entity::find_by_id(id)
                .one(db.as_ref())
                .await
                .unwrap()
                .unwrap();
            (row.shield_mode, row.attack_mode)
        }
    };
    assert_eq!(stored(project.id).await, ("log".to_string(), false));

    for (params, expected) in [
        (
            UpdateProjectSettingsParams {
                shield_mode: Some(temps_shield::ShieldMode::Attack),
                ..Default::default()
            },
            ("attack", true),
        ),
        (
            UpdateProjectSettingsParams {
                shield_mode: Some(temps_shield::ShieldMode::Selective),
                ..Default::default()
            },
            ("selective", false),
        ),
        (
            UpdateProjectSettingsParams {
                attack_mode: Some(true),
                ..Default::default()
            },
            ("attack", true),
        ),
        (
            UpdateProjectSettingsParams {
                attack_mode: Some(false),
                ..Default::default()
            },
            ("log", false),
        ),
    ] {
        let update = project_service
            .update_project_settings(project.id, params)
            .await
            .expect("settings update succeeds");
        assert_eq!(update.project.shield_mode, expected.0);
        assert_eq!(update.project.attack_mode, expected.1);
        assert_eq!(
            stored(project.id).await,
            (expected.0.to_string(), expected.1)
        );
    }
}

/// Renaming must change only the display name — the slug is the routing
/// identifier, and a rename that silently moved a project's URL would break
/// every domain already pointing at it.
#[tokio::test]
async fn test_update_project_settings_renames_without_touching_slug() {
    // create_test_services needs a Docker daemon; skip rather than fail
    // where one isn't available (CLAUDE.md: no #[ignore] on Docker tests).
    if !docker_available().await {
        println!("Docker not available, skipping");
        return;
    }
    let test_db = TestDatabase::with_migrations().await.unwrap();
    let db = test_db.db.clone();
    let mock_queue = Arc::new(MockJobQueue::new());
    let project_service = create_test_services(db.clone(), mock_queue.clone()).await;

    let project = temps_entities::projects::ActiveModel {
        name: Set("Old Name".to_string()),
        slug: Set("rename-keeps-slug".to_string()),
        repo_name: Set("rename-repo".to_string()),
        repo_owner: Set("test-owner".to_string()),
        directory: Set("rename-dir".to_string()),
        git_provider_connection_id: Set(None),
        main_branch: Set("main".to_string()),
        preset: Set(Preset::Nixpacks),
        ..Default::default()
    };
    let inserted_project = project.insert(db.as_ref()).await.unwrap();

    let updated = project_service
        .update_project_settings(
            inserted_project.id,
            UpdateProjectSettingsParams {
                name: Some("  New Display Name  ".to_string()),
                ..Default::default()
            },
        )
        .await
        .expect("rename should succeed");

    // The service reports the transition it actually performed — both ends
    // captured under one lock — so the audit never has to infer it from a
    // racy read-before-write, nor splice two requests' renames together.
    let rename = updated.rename.as_ref().expect("a rename was performed");
    assert_eq!(rename.from, "Old Name");
    assert_eq!(rename.to, "New Display Name");

    let updated = updated.project;

    // Trimmed, persisted, and the slug is exactly as it was.
    assert_eq!(updated.name, "New Display Name");
    assert_eq!(updated.slug, "rename-keeps-slug");

    let reloaded = projects::Entity::find_by_id(inserted_project.id)
        .one(db.as_ref())
        .await
        .unwrap()
        .expect("project should still exist");
    assert_eq!(reloaded.name, "New Display Name");
    assert_eq!(reloaded.slug, "rename-keeps-slug");
}

/// The rename can also travel inside the git-settings transaction, which is
/// a separate implementation from the tail path above: it mutates a
/// pre-lock ActiveModel and takes the row lock afterwards. Cover it, and
/// assert the accompanying git change commits alongside the rename.
#[tokio::test]
async fn test_update_project_settings_renames_alongside_git_settings() {
    // create_test_services needs a Docker daemon; skip rather than fail
    // where one isn't available (CLAUDE.md: no #[ignore] on Docker tests).
    if !docker_available().await {
        println!("Docker not available, skipping");
        return;
    }
    let test_db = TestDatabase::with_migrations().await.unwrap();
    let db = test_db.db.clone();
    let mock_queue = Arc::new(MockJobQueue::new());
    let project_service = create_test_services(db.clone(), mock_queue.clone()).await;

    let project = temps_entities::projects::ActiveModel {
        name: Set("Before Git Rename".to_string()),
        slug: Set("rename-with-git".to_string()),
        repo_name: Set("git-rename-repo".to_string()),
        repo_owner: Set("test-owner".to_string()),
        directory: Set("git-rename-dir".to_string()),
        git_provider_connection_id: Set(None),
        main_branch: Set("main".to_string()),
        preset: Set(Preset::Nixpacks),
        ..Default::default()
    };
    let inserted_project = project.insert(db.as_ref()).await.unwrap();

    // `main_branch` makes `needs_git_update` true, routing the rename
    // through the git-settings transaction rather than the tail path.
    let updated = project_service
        .update_project_settings(
            inserted_project.id,
            UpdateProjectSettingsParams {
                name: Some("  After Git Rename  ".to_string()),
                main_branch: Some("develop".to_string()),
                ..Default::default()
            },
        )
        .await
        .expect("rename alongside git settings should succeed");

    let rename = updated
        .rename
        .as_ref()
        .expect("the git path must report the rename it performed");
    assert_eq!(rename.from, "Before Git Rename");
    assert_eq!(rename.to, "After Git Rename");
    assert_eq!(updated.project.name, "After Git Rename");

    // Both halves of the transaction committed together.
    let stored = projects::Entity::find_by_id(inserted_project.id)
        .one(db.as_ref())
        .await
        .unwrap()
        .expect("project should still exist");
    assert_eq!(stored.name, "After Git Rename");
    assert_eq!(stored.main_branch, "develop");
    assert_eq!(stored.slug, "rename-with-git");
}

/// Validation of the accompanying git settings happens before the rename is
/// staged, so a request that renames *and* sends an unusable preset leaves
/// the name untouched. (Failures after the rename is staged are covered by
/// the slug-conflict test, which exercises the rollback itself.)
#[tokio::test]
async fn test_update_project_settings_rename_not_applied_when_preset_is_invalid() {
    // create_test_services needs a Docker daemon; skip rather than fail
    // where one isn't available (CLAUDE.md: no #[ignore] on Docker tests).
    if !docker_available().await {
        println!("Docker not available, skipping");
        return;
    }
    let test_db = TestDatabase::with_migrations().await.unwrap();
    let db = test_db.db.clone();
    let mock_queue = Arc::new(MockJobQueue::new());
    let project_service = create_test_services(db.clone(), mock_queue.clone()).await;

    let project = temps_entities::projects::ActiveModel {
        name: Set("Keep This Name".to_string()),
        slug: Set("rename-git-rollback".to_string()),
        repo_name: Set("git-rollback-repo".to_string()),
        repo_owner: Set("test-owner".to_string()),
        directory: Set("git-rollback-dir".to_string()),
        git_provider_connection_id: Set(None),
        main_branch: Set("main".to_string()),
        preset: Set(Preset::Nixpacks),
        ..Default::default()
    };
    let inserted_project = project.insert(db.as_ref()).await.unwrap();

    let result = project_service
        .update_project_settings(
            inserted_project.id,
            UpdateProjectSettingsParams {
                name: Some("Renamed Before Failure".to_string()),
                preset: Some("definitely-not-a-real-preset".to_string()),
                ..Default::default()
            },
        )
        .await;

    assert!(
        matches!(result, Err(ProjectError::InvalidInput(_))),
        "an unknown preset should fail validation, not something unrelated"
    );

    let stored = projects::Entity::find_by_id(inserted_project.id)
        .one(db.as_ref())
        .await
        .unwrap()
        .expect("project should still exist");
    assert_eq!(
        stored.name, "Keep This Name",
        "a request rejected during validation must not have renamed anything"
    );
}

/// Re-submitting the name a project already has performs no write, so the
/// service must report no rename — otherwise the audit records a transition
/// that never happened.
#[tokio::test]
async fn test_update_project_settings_reports_no_rename_for_a_no_op() {
    // create_test_services needs a Docker daemon; skip rather than fail
    // where one isn't available (CLAUDE.md: no #[ignore] on Docker tests).
    if !docker_available().await {
        println!("Docker not available, skipping");
        return;
    }
    let test_db = TestDatabase::with_migrations().await.unwrap();
    let db = test_db.db.clone();
    let mock_queue = Arc::new(MockJobQueue::new());
    let project_service = create_test_services(db.clone(), mock_queue.clone()).await;

    let project = temps_entities::projects::ActiveModel {
        name: Set("Unchanged Name".to_string()),
        slug: Set("rename-noop".to_string()),
        repo_name: Set("noop-repo".to_string()),
        repo_owner: Set("test-owner".to_string()),
        directory: Set("noop-dir".to_string()),
        git_provider_connection_id: Set(None),
        main_branch: Set("main".to_string()),
        preset: Set(Preset::Nixpacks),
        ..Default::default()
    };
    let inserted_project = project.insert(db.as_ref()).await.unwrap();

    // Whitespace that trims back to the stored name is still a no-op.
    let updated = project_service
        .update_project_settings(
            inserted_project.id,
            UpdateProjectSettingsParams {
                name: Some("  Unchanged Name  ".to_string()),
                ..Default::default()
            },
        )
        .await
        .expect("no-op rename should succeed");

    assert!(
        updated.rename.is_none(),
        "a no-op rename must not report a transition"
    );
    assert_eq!(updated.project.name, "Unchanged Name");
}

/// A request that renames *and* fails on something else must not leave the
/// rename behind: the endpoint reporting an error while the project silently
/// kept a new name is exactly the kind of half-applied write a self-hosted
/// operator has no way to diagnose.
#[tokio::test]
async fn test_update_project_settings_rename_does_not_persist_when_slug_conflicts() {
    // create_test_services needs a Docker daemon; skip rather than fail
    // where one isn't available (CLAUDE.md: no #[ignore] on Docker tests).
    if !docker_available().await {
        println!("Docker not available, skipping");
        return;
    }
    let test_db = TestDatabase::with_migrations().await.unwrap();
    let db = test_db.db.clone();
    let mock_queue = Arc::new(MockJobQueue::new());
    let project_service = create_test_services(db.clone(), mock_queue.clone()).await;

    // The project we will try to rename...
    let target = temps_entities::projects::ActiveModel {
        name: Set("Original Name".to_string()),
        slug: Set("rename-rollback-target".to_string()),
        repo_name: Set("rollback-repo".to_string()),
        repo_owner: Set("test-owner".to_string()),
        directory: Set("rollback-dir".to_string()),
        git_provider_connection_id: Set(None),
        main_branch: Set("main".to_string()),
        preset: Set(Preset::Nixpacks),
        ..Default::default()
    };
    let target = target.insert(db.as_ref()).await.unwrap();

    // ...and a second project already holding the slug we will collide with.
    let occupier = temps_entities::projects::ActiveModel {
        name: Set("Occupier".to_string()),
        slug: Set("rename-rollback-taken".to_string()),
        repo_name: Set("occupier-repo".to_string()),
        repo_owner: Set("test-owner".to_string()),
        directory: Set("occupier-dir".to_string()),
        git_provider_connection_id: Set(None),
        main_branch: Set("main".to_string()),
        preset: Set(Preset::Nixpacks),
        ..Default::default()
    };
    occupier.insert(db.as_ref()).await.unwrap();

    let result = project_service
        .update_project_settings(
            target.id,
            UpdateProjectSettingsParams {
                name: Some("Renamed Before Failure".to_string()),
                slug: Some("rename-rollback-taken".to_string()),
                ..Default::default()
            },
        )
        .await;

    assert!(
        matches!(result, Err(ProjectError::SlugAlreadyExists(_))),
        "conflicting slug should be rejected"
    );

    let reloaded = projects::Entity::find_by_id(target.id)
        .one(db.as_ref())
        .await
        .unwrap()
        .expect("project should still exist");
    assert_eq!(
        reloaded.name, "Original Name",
        "rename must not survive a failed request"
    );
    assert_eq!(reloaded.slug, "rename-rollback-target");
}

#[tokio::test]
async fn test_update_project_settings_rejects_blank_and_overlong_names() {
    // create_test_services needs a Docker daemon; skip rather than fail
    // where one isn't available (CLAUDE.md: no #[ignore] on Docker tests).
    if !docker_available().await {
        println!("Docker not available, skipping");
        return;
    }
    let test_db = TestDatabase::with_migrations().await.unwrap();
    let db = test_db.db.clone();
    let mock_queue = Arc::new(MockJobQueue::new());
    let project_service = create_test_services(db.clone(), mock_queue.clone()).await;

    let project = temps_entities::projects::ActiveModel {
        name: Set("Keep Me".to_string()),
        slug: Set("rename-validation".to_string()),
        repo_name: Set("rename-validation-repo".to_string()),
        repo_owner: Set("test-owner".to_string()),
        directory: Set("rename-validation".to_string()),
        git_provider_connection_id: Set(None),
        main_branch: Set("main".to_string()),
        preset: Set(Preset::Nixpacks),
        ..Default::default()
    };
    let inserted_project = project.insert(db.as_ref()).await.unwrap();

    for candidate in ["   ".to_string(), "x".repeat(101)] {
        let result = project_service
            .update_project_settings(
                inserted_project.id,
                UpdateProjectSettingsParams {
                    name: Some(candidate.clone()),
                    ..Default::default()
                },
            )
            .await;

        assert!(
            matches!(result, Err(ProjectError::InvalidInput(_))),
            "name {:?} should be rejected as invalid input",
            candidate
        );
    }

    // A rejected rename must not have partially applied.
    let reloaded = projects::Entity::find_by_id(inserted_project.id)
        .one(db.as_ref())
        .await
        .unwrap()
        .expect("project should still exist");
    assert_eq!(reloaded.name, "Keep Me");
}

#[tokio::test]
async fn test_update_project_settings_emits_event() {
    // Setup test database
    let test_db = TestDatabase::with_migrations().await.unwrap();
    let db = test_db.db.clone();

    // Create mock queue service
    let mock_queue = Arc::new(MockJobQueue::new());

    // Create project service
    let project_service = create_test_services(db.clone(), mock_queue.clone()).await;

    // Insert a test project
    let project = temps_entities::projects::ActiveModel {
        name: Set("Settings Test Project".to_string()),
        slug: Set("settings-test-project".to_string()),
        repo_name: Set("settings-test-repo".to_string()),
        repo_owner: Set("test-owner".to_string()),
        directory: Set("settings-test-project".to_string()),
        git_provider_connection_id: Set(None),
        main_branch: Set("main".to_string()),
        preset: Set(Preset::Nixpacks),
        ..Default::default()
    };

    let inserted_project = project.insert(db.as_ref()).await.unwrap();

    // Update project settings
    let result = project_service
        .update_project_settings(
            inserted_project.id,
            UpdateProjectSettingsParams {
                slug: Some("new-slug".to_string()),
                main_branch: Some("develop".to_string()),
                preset: Some(Preset::Nixpacks.to_string()),
                ..Default::default()
            },
        )
        .await;

    assert!(result.is_ok(), "update_project_settings should succeed");

    // Verify event was emitted
    let jobs = mock_queue.get_jobs().await;
    assert_eq!(jobs.len(), 1, "Should emit exactly one job");

    match &jobs[0] {
        Job::ProjectUpdated(job) => {
            assert_eq!(job.project_id, inserted_project.id);
            assert_eq!(job.project_name, "Settings Test Project");
        }
        _ => panic!("Expected ProjectUpdated job"),
    }
}

#[tokio::test]
async fn test_update_project_event_includes_correct_data() {
    // Setup test database
    let test_db = TestDatabase::with_migrations().await.unwrap();
    let db = test_db.db.clone();

    // Create mock queue service
    let mock_queue = Arc::new(MockJobQueue::new());

    // Create project service
    let project_service = create_test_services(db.clone(), mock_queue.clone()).await;

    // Insert a test project with specific name
    let project = temps_entities::projects::ActiveModel {
        name: Set("Event Data Test".to_string()),
        slug: Set("event-data-test".to_string()),
        repo_name: Set("event-data-repo".to_string()),
        repo_owner: Set("test-owner".to_string()),
        directory: Set("event-data-test".to_string()),
        git_provider_connection_id: Set(None),
        main_branch: Set("main".to_string()),
        preset: Set(Preset::Nixpacks),
        ..Default::default()
    };

    let inserted_project = project.insert(db.as_ref()).await.unwrap();
    let project_id = inserted_project.id;

    // Update the project name
    let update_request = CreateProjectRequest {
        cloudflare_enabled: None,
        delivery_provider: None,
        name: "Event Data Test Updated".to_string(),
        expected_slug: None,
        repo_name: None,
        repo_owner: None,
        directory: "/".to_string(),
        main_branch: "main".to_string(),
        preset: Preset::Nixpacks.as_str().to_string(),
        preset_config: None,
        environment_variables: None,
        automatic_deploy: false,
        storage_service_ids: vec![],
        storage_service_claim_ids: vec![],
        storage_service_claim_user_id: None,
        is_public_repo: None,
        git_url: None,
        git_provider_connection_id: None,
        exposed_port: None,
        cpu_request: None,
        cpu_limit: None,
        memory_request: None,
        memory_limit: None,
        source_type: temps_entities::source_type::SourceType::Git,
        template_slug: None,
        hosting: Default::default(),
    };

    project_service
        .update_project(
            project_id,
            update_request,
            temps_core::docker_socket_grant::DeployCaller::default(),
        )
        .await
        .unwrap();

    // Verify the event contains the updated name
    let jobs = mock_queue.get_jobs().await;
    assert_eq!(jobs.len(), 1);

    if let Job::ProjectUpdated(job) = &jobs[0] {
        assert_eq!(job.project_id, project_id);
        assert_eq!(
            job.project_name, "Event Data Test Updated",
            "Event should contain the updated project name"
        );
    } else {
        panic!("Expected ProjectUpdated job");
    }
}

/// Docker is required by `create_test_services` because it constructs an
/// `ExternalServiceManager`. When Docker isn't available locally
/// (CI without docker-in-docker, dev machines without daemon) skip
/// rather than failing — matches the `cargo test` discipline in CLAUDE.md.
/// Pins an assumption the git-settings path depends on: it builds its
/// `ActiveModel` from a project snapshot loaded *before* the transaction's
/// row lock, so if `update()` wrote back every column of that snapshot, an
/// unrelated concurrent change would be silently lost.
///
/// It does not: `Model -> ActiveModel` marks columns `Unchanged`, and
/// `update()` emits a SET clause containing only the columns explicitly
/// `Set`. This test exists because that is a Sea-ORM behaviour rather than
/// something visible in our code — if it ever changed, the git-settings path
/// would start clobbering concurrent writes with no other signal.
#[tokio::test]
async fn test_partial_update_does_not_clobber_concurrent_columns() {
    let test_db = TestDatabase::with_migrations().await.unwrap();
    let db = test_db.db.clone();

    let inserted = temps_entities::projects::ActiveModel {
        name: Set("Probe Original".to_string()),
        slug: Set("probe-stale-snapshot".to_string()),
        repo_name: Set("probe-repo".to_string()),
        repo_owner: Set("test-owner".to_string()),
        directory: Set("probe-dir".to_string()),
        git_provider_connection_id: Set(None),
        main_branch: Set("main".to_string()),
        preset: Set(Preset::Nixpacks),
        ..Default::default()
    }
    .insert(db.as_ref())
    .await
    .unwrap();

    // A stale snapshot, taken before the "concurrent" write below.
    let stale = projects::Entity::find_by_id(inserted.id)
        .one(db.as_ref())
        .await
        .unwrap()
        .unwrap();

    // Concurrent, unrelated update: someone else changes attack_mode.
    let mut other: projects::ActiveModel = projects::Entity::find_by_id(inserted.id)
        .one(db.as_ref())
        .await
        .unwrap()
        .unwrap()
        .into();
    other.attack_mode = Set(true);
    other.update(db.as_ref()).await.unwrap();

    // Now write the stale snapshot back, setting only `directory`.
    let mut from_stale: projects::ActiveModel = stale.into();
    from_stale.directory = Set("probe-dir-changed".to_string());
    from_stale.update(db.as_ref()).await.unwrap();

    let final_row = projects::Entity::find_by_id(inserted.id)
        .one(db.as_ref())
        .await
        .unwrap()
        .unwrap();

    assert_eq!(final_row.directory, "probe-dir-changed");
    assert!(
        final_row.attack_mode,
        "LOST UPDATE: the stale snapshot clobbered the concurrent attack_mode change"
    );
}

async fn docker_available() -> bool {
    match bollard::Docker::connect_with_local_defaults() {
        Ok(d) => d.ping().await.is_ok(),
        Err(_) => false,
    }
}

#[tokio::test]
async fn service_template_runtime_update_is_atomic_and_can_clear_resources() {
    if !docker_available().await {
        println!("Docker not available, skipping");
        return;
    }
    let test_db = TestDatabase::with_migrations().await.unwrap();
    let db = test_db.db.clone();
    let mock_queue = Arc::new(MockJobQueue::new());
    let project_service = create_test_services(db.clone(), mock_queue).await;

    let old_runtime = temps_entities::preset::ImageRuntimeConfig {
        image_ref: "quay.io/keycloak/keycloak:26.7.2".to_string(),
        command: Some(vec!["start".to_string()]),
        health_check_path: Some("/realms/master".to_string()),
    };
    let applied = temps_core::templates::ServiceTemplateInstance::new(
        temps_core::templates::SERVICE_TEMPLATE_SCHEMA_VERSION,
        temps_core::templates::bundled_template_by_slug("keycloak")
            .expect("bundled Keycloak release"),
    );
    let project = projects::ActiveModel {
        name: Set("Template runtime".to_string()),
        slug: Set("template-runtime".to_string()),
        repo_name: Set(String::new()),
        repo_owner: Set(String::new()),
        directory: Set(".".to_string()),
        main_branch: Set("main".to_string()),
        preset: Set(Preset::Dockerfile),
        preset_config: Set(Some(temps_entities::preset::PresetConfig::Dockerfile(
            temps_entities::preset::DockerfileConfig {
                image_runtime: Some(old_runtime.clone()),
                ..Default::default()
            },
        ))),
        deployment_config: Set(Some(temps_entities::deployment_config::DeploymentConfig {
            cpu_request: Some(500_000),
            memory_request: Some(512),
            exposed_port: Some(8080),
            ..Default::default()
        })),
        source_type: Set(temps_entities::source_type::SourceType::DockerImage),
        project_type: Set(ProjectType::Service),
        template_slug: Set(Some(applied.slug.clone())),
        service_template: Set(Some(
            serde_json::to_value(applied).expect("serialize applied service release"),
        )),
        ..Default::default()
    }
    .insert(db.as_ref())
    .await
    .unwrap();

    project_service
        .update_service_template_runtime(
            project.id,
            UpdateServiceTemplateRuntimeRequest {
                image_ref: "quay.io/keycloak/keycloak:27.0.0".to_string(),
                command: Vec::new(),
                health_check_path: "/ready".to_string(),
                cpu_request: None,
                cpu_limit: None,
                memory_request: None,
                memory_limit: None,
                exposed_port: None,
            },
            temps_core::CeilingEnforcement::Bypass,
            DeployCaller::Platform,
        )
        .await
        .unwrap();

    let updated = projects::Entity::find_by_id(project.id)
        .one(db.as_ref())
        .await
        .unwrap()
        .unwrap();
    let Some(temps_entities::preset::PresetConfig::Dockerfile(config)) =
        updated.preset_config.as_ref()
    else {
        panic!("expected Dockerfile config");
    };
    let runtime = config.image_runtime.as_ref().unwrap();
    assert_eq!(runtime.image_ref, "quay.io/keycloak/keycloak:27.0.0");
    assert_eq!(runtime.command, None);
    assert_eq!(runtime.health_check_path.as_deref(), Some("/ready"));
    let deployment_config = updated.deployment_config.as_ref().unwrap();
    assert_eq!(deployment_config.cpu_request, None);
    assert_eq!(deployment_config.memory_request, None);
    assert_eq!(deployment_config.exposed_port, None);

    let invalid = project_service
        .update_service_template_runtime(
            project.id,
            UpdateServiceTemplateRuntimeRequest {
                image_ref: "quay.io/keycloak/keycloak:99.0.0".to_string(),
                command: vec!["start".to_string()],
                health_check_path: "https://attacker.example".to_string(),
                cpu_request: Some(750_000),
                cpu_limit: None,
                memory_request: None,
                memory_limit: None,
                exposed_port: Some(9090),
            },
            temps_core::CeilingEnforcement::Bypass,
            DeployCaller::Platform,
        )
        .await;
    assert!(matches!(invalid, Err(ProjectError::InvalidInput(_))));

    let unchanged = projects::Entity::find_by_id(project.id)
        .one(db.as_ref())
        .await
        .unwrap()
        .unwrap();
    let Some(temps_entities::preset::PresetConfig::Dockerfile(config)) =
        unchanged.preset_config.as_ref()
    else {
        panic!("expected Dockerfile config");
    };
    assert_eq!(
        config.image_runtime.as_ref().unwrap().image_ref,
        "quay.io/keycloak/keycloak:27.0.0"
    );
    assert_eq!(unchanged.deployment_config.unwrap().exposed_port, None);
}

#[tokio::test]
async fn service_template_upgrade_persists_release_and_preserves_runtime_overrides() {
    if !docker_available().await {
        println!("Docker not available, skipping");
        return;
    }
    let test_db = TestDatabase::with_migrations().await.unwrap();
    let db = test_db.db.clone();
    let mock_queue = Arc::new(MockJobQueue::new());
    let project_service = create_test_services(db.clone(), mock_queue).await;
    let mut applied = temps_core::templates::ServiceTemplateInstance::new(
        temps_core::templates::SERVICE_TEMPLATE_SCHEMA_VERSION,
        temps_core::templates::bundled_template_by_slug("browserless")
            .expect("bundled Browserless release"),
    );
    applied.template.services[0]
        .env_vars
        .push(temps_core::templates::EnvVarTemplate {
            name: "OPTIONAL_SETTING".to_string(),
            example: None,
            default: None,
            description: Some("Becomes required and sensitive in the next release".to_string()),
            required: false,
            secret: false,
            default_generator: None,
        });
    let mut request = create_request("Browserless upgrade");
    request.preset = Preset::Dockerfile.to_string();
    request.source_type = temps_entities::source_type::SourceType::DockerImage;
    request.template_slug = Some(applied.slug.clone());
    request.preset_config = Some(
        serde_json::to_value(temps_entities::preset::PresetConfig::Dockerfile(
            temps_entities::preset::DockerfileConfig {
                image_runtime: Some(temps_entities::preset::ImageRuntimeConfig {
                    // User customization: an upgrade must not replace this.
                    image_ref: "registry.example/browserless-custom:7".to_string(),
                    command: applied.template.services[0].command.clone(),
                    health_check_path: applied.template.services[0].health_check_path.clone(),
                }),
                ..Default::default()
            },
        ))
        .expect("Dockerfile config serialization"),
    );
    request.exposed_port = applied.template.services[0].exposed_port;
    request.cpu_request = applied.template.services[0]
        .resources
        .as_ref()
        .and_then(|resources| resources.cpu_request);
    request.cpu_limit = applied.template.services[0]
        .resources
        .as_ref()
        .and_then(|resources| resources.cpu_limit);
    request.memory_request = applied.template.services[0]
        .resources
        .as_ref()
        .and_then(|resources| resources.memory_request);
    request.memory_limit = applied.template.services[0]
        .resources
        .as_ref()
        .and_then(|resources| resources.memory_limit);
    request.environment_variables = Some(vec![
        CreateProjectEnvVar {
            key: "TOKEN".to_string(),
            value: "existing-secret".to_string(),
            is_secret: true,
        },
        CreateProjectEnvVar {
            key: "EXTERNAL".to_string(),
            value: "https://browser.example.test".to_string(),
            is_secret: false,
        },
        CreateProjectEnvVar {
            key: "CONCURRENT".to_string(),
            value: "2".to_string(),
            is_secret: false,
        },
        CreateProjectEnvVar {
            key: "OPTIONAL_SETTING".to_string(),
            value: String::new(),
            is_secret: false,
        },
    ]);

    let created = project_service
        .create_service_project(request, applied.clone())
        .await
        .expect("service project creation");
    assert_eq!(
        created.service_template_image_url,
        applied.template.image_url.clone()
    );
    assert_eq!(
        created.service_template_version.as_deref(),
        Some(applied.version.as_str())
    );
    let stored = projects::Entity::find_by_id(created.id)
        .one(db.as_ref())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(stored.project_type, ProjectType::Service);
    assert_eq!(
        applied_service_template_from_model(&stored)
            .unwrap()
            .version,
        applied.version
    );

    // A legacy/global fallback can coexist with a production-scoped row.
    // The production row remains authoritative even when empty, and every
    // applicable duplicate must be promoted if the template later marks
    // the key sensitive.
    let encrypted_global = project_service
        .encryption_service
        .encrypt_string("global-readable")
        .expect("encrypt global fallback");
    env_vars::ActiveModel {
        project_id: Set(created.id),
        environment_id: Set(None),
        key: Set("OPTIONAL_SETTING".to_string()),
        value: Set(encrypted_global),
        include_in_preview: Set(false),
        is_encrypted: Set(true),
        is_secret: Set(false),
        ..Default::default()
    }
    .insert(db.as_ref())
    .await
    .expect("insert global duplicate");
    let production_environment = environments::Entity::find()
        .filter(environments::Column::ProjectId.eq(created.id))
        .filter(environments::Column::IsPreview.eq(false))
        .one(db.as_ref())
        .await
        .expect("query production environment")
        .expect("production environment");
    let mut preview_environment: environments::ActiveModel = production_environment.into();
    preview_environment.id = sea_orm::NotSet;
    preview_environment.name = Set("Preview".to_string());
    preview_environment.slug = Set("preview-secret-promotion".to_string());
    preview_environment.subdomain = Set("preview-secret-promotion".to_string());
    preview_environment.host = Set("preview-secret-promotion.localho.st".to_string());
    preview_environment.branch = Set(Some("preview-secret-promotion".to_string()));
    preview_environment.is_preview = Set(true);
    preview_environment.current_deployment_id = Set(None);
    preview_environment.last_deployment = Set(None);
    let preview_environment = preview_environment
        .insert(db.as_ref())
        .await
        .expect("insert preview environment");
    let encrypted_preview = project_service
        .encryption_service
        .encrypt_string("preview-readable")
        .expect("encrypt preview value");
    env_vars::ActiveModel {
        project_id: Set(created.id),
        environment_id: Set(Some(preview_environment.id)),
        key: Set("OPTIONAL_SETTING".to_string()),
        value: Set(encrypted_preview),
        include_in_preview: Set(true),
        is_encrypted: Set(true),
        is_secret: Set(false),
        ..Default::default()
    }
    .insert(db.as_ref())
    .await
    .expect("insert preview duplicate");

    let mut target = applied.clone();
    target.version = "1.1.0".to_string();
    target.template.version = target.version.clone();
    target.template.services[0].image = Some(
        "ghcr.io/browserless/chromium@sha256:bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb"
            .to_string(),
    );
    target.template.services[0].exposed_port = Some(3100);
    target.template.services[0].health_check_path = Some("/ready".to_string());
    target.template.services[0]
        .resources
        .as_mut()
        .unwrap()
        .memory_limit = Some(2_560);
    let promoted_definition = target.template.services[0]
        .env_vars
        .iter_mut()
        .find(|definition| definition.name == "OPTIONAL_SETTING")
        .expect("optional setting definition");
    promoted_definition.required = true;
    promoted_definition.secret = true;

    let missing_value = project_service
        .upgrade_service_template(
            created.id,
            target.clone(),
            Vec::new(),
            temps_core::CeilingEnforcement::Bypass,
            DeployCaller::Platform,
        )
        .await;
    assert!(matches!(missing_value, Err(ProjectError::InvalidInput(_))));

    project_service
        .upgrade_service_template(
            created.id,
            target.clone(),
            vec![CreateProjectEnvVar {
                key: "OPTIONAL_SETTING".to_string(),
                value: "configured-after-upgrade".to_string(),
                is_secret: false,
            }],
            temps_core::CeilingEnforcement::Bypass,
            DeployCaller::Platform,
        )
        .await
        .expect("same-family template upgrade");

    let upgraded = projects::Entity::find_by_id(created.id)
        .one(db.as_ref())
        .await
        .unwrap()
        .unwrap();
    let snapshot = applied_service_template_from_model(&upgraded).unwrap();
    assert_eq!(snapshot.version, "1.1.0");
    assert_eq!(snapshot, target);
    let Some(temps_entities::preset::PresetConfig::Dockerfile(config)) =
        upgraded.preset_config.as_ref()
    else {
        panic!("expected Dockerfile config");
    };
    let runtime = config.image_runtime.as_ref().unwrap();
    assert_eq!(
        runtime.image_ref, "registry.example/browserless-custom:7",
        "a user-pinned image must survive a catalog default change"
    );
    assert_eq!(runtime.health_check_path.as_deref(), Some("/ready"));
    let deployment = upgraded.deployment_config.unwrap();
    assert_eq!(deployment.exposed_port, Some(3100));
    assert_eq!(deployment.memory_limit, Some(2_560));
    let promoted = env_vars::Entity::find()
        .filter(env_vars::Column::ProjectId.eq(created.id))
        .filter(env_vars::Column::Key.eq("OPTIONAL_SETTING"))
        .all(db.as_ref())
        .await
        .unwrap();
    assert!(
        promoted.len() == 3 && promoted.iter().all(|variable| variable.is_secret),
        "upgrade must protect every project-scoped duplicate"
    );
    let values = promoted
        .iter()
        .map(|variable| {
            project_service
                .encryption_service
                .decrypt_string(&variable.value)
                .expect("decrypt promoted value")
        })
        .collect::<HashSet<_>>();
    assert_eq!(
        values,
        [
            "configured-after-upgrade",
            "global-readable",
            "preview-readable",
        ]
        .into_iter()
        .map(str::to_string)
        .collect()
    );
}

/// An app template with two source-built services and one pinned image.
/// The git host is not GitHub, so creating its services never asks a
/// public API whether `api/` or `web/` exists.
const STACK_APP_TEMPLATE: &str = r#"
version: "3"
templates:
  - slug: stack-app
    name: Stack app
    version: 1.0.0
    kind: app
    git:
      url: https://git.example.test/example/stack-app.git
      ref: v1.0.0
    databases: [postgres]
    services:
      - name: api
        directory: api
        preset: dockerfile
        exposed_port: 8080
        database_bindings:
          postgres:
            DATABASE_URL: POSTGRES_URL
      - name: web
        directory: web
        preset: vite
      - name: cache
        preset: dockerfile
        image: registry.example.test/cache@sha256:0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef
        exposed_port: 6379
"#;

struct DiscardingAudit;

#[async_trait]
impl temps_core::AuditLogger for DiscardingAudit {
    async fn create_audit_log(
        &self,
        _operation: &dyn temps_core::AuditOperation,
    ) -> anyhow::Result<()> {
        Ok(())
    }
}

fn stack_app_template() -> temps_core::templates::ProjectTemplate {
    temps_core::templates::TemplatesConfig::from_yaml(STACK_APP_TEMPLATE)
        .expect("the test template parses")
        .templates
        .into_iter()
        .next()
        .expect("the test template is listed")
}

fn stack_app_service_request(
    template: &temps_core::templates::ProjectTemplate,
    service_name: &str,
    instantiation_id: i32,
) -> crate::template_creation::TemplateServiceRequest {
    let service = template
        .service(service_name)
        .cloned()
        .expect("the template declares the service");
    let release = temps_core::templates::ServiceTemplateInstance::new(
        temps_core::templates::SERVICE_TEMPLATE_SCHEMA_VERSION,
        template.clone(),
    )
    .for_service(service_name);
    let repository = (!service.is_image()).then(|| {
        let source = service.source(&template.git);
        crate::template_creation::RepositoryTarget {
            repo_owner: "example".to_string(),
            repo_name: "stack-app".to_string(),
            branch: source.r#ref.clone(),
            git_url: Some(template.git.url.clone()),
            git_provider_connection_id: None,
            is_public: true,
            directory: service.directory.clone().unwrap_or_else(|| ".".to_string()),
            automatic_deploy: false,
        }
    });
    crate::template_creation::TemplateServiceRequest {
        template: template.clone(),
        service,
        release: Some(release),
        project_name: format!("Stack {service_name}"),
        project_slug: format!("stack-{service_name}"),
        provenance: Some(template.slug.clone()),
        variables: Vec::new(),
        variable_templates: Vec::new(),
        storage_service_ids: Vec::new(),
        storage_service_claim_ids: Vec::new(),
        repository,
        runtime_overrides: Default::default(),
        defer_initial_deployment: true,
        instantiation_token: Some(temps_entities::projects::instantiation_token(
            instantiation_id,
            service_name,
        )),
        deploy_mode: "public_repo",
        cloudflare_enabled: None,
        delivery_provider: None,
    }
}

/// `projects_service_template_identity` only exists in a real database,
/// and the mock-database tests cannot see it: it once rejected every
/// source-built service of an app, which none of them noticed. This
/// creates all three kinds of service an app can have through the
/// creator the instantiation engine uses.
#[tokio::test]
async fn app_template_services_satisfy_the_service_identity_constraint() {
    if !docker_available().await {
        println!("Docker not available, skipping");
        return;
    }
    let test_db = TestDatabase::with_migrations().await.unwrap();
    let db = test_db.db.clone();
    let project_service =
        Arc::new(create_test_services(db.clone(), Arc::new(MockJobQueue::new())).await);
    let server_config = Arc::new(
        temps_config::ServerConfig::new(
            "127.0.0.1:3000".to_string(),
            "postgresql://test".to_string(),
            None,
            None,
        )
        .unwrap(),
    );
    let creator = crate::template_creation::TemplateServiceCreator {
        project_service: project_service.clone(),
        config_service: Arc::new(temps_config::ConfigService::new(server_config, db.clone())),
        public_hostname_resolver: Arc::new(temps_core::StandardHostnameResolver),
        audit_service: Arc::new(DiscardingAudit),
        telemetry: Arc::new(temps_core::NoopTelemetryReporter),
    };
    let session = SlugClaimSession::admin(&db).await;
    let slug_claim = session.caller();
    let caller = crate::template_creation::TemplateCreationCaller {
        auth: &session.auth,
        slug_claim: &slug_claim,
        ip_address: None,
        user_agent: "test".to_string(),
    };
    let template = stack_app_template();

    let mut stored = std::collections::BTreeMap::new();
    for service_name in ["api", "web", "cache"] {
        let created = creator
            .create(
                stack_app_service_request(&template, service_name, 41),
                &caller,
            )
            .await
            .unwrap_or_else(|problem| {
                panic!("service '{service_name}' must be created: {problem:?}")
            });
        let row = projects::Entity::find_by_id(created.project.id)
            .one(db.as_ref())
            .await
            .unwrap()
            .expect("the created service is stored");
        stored.insert(service_name, (created, row));
    }

    // Source-built: the lifecycle the preset gives them, so the normal
    // build pipeline runs, plus the release and the template it is from.
    let (api, api_row) = &stored["api"];
    assert_eq!(api_row.project_type, ProjectType::Server);
    assert_eq!(
        api_row.source_type,
        temps_entities::source_type::SourceType::Git
    );
    let (_, web_row) = &stored["web"];
    assert_eq!(web_row.project_type, ProjectType::Static);

    // An image-only service is a service project, as before.
    let (_, cache_row) = &stored["cache"];
    assert_eq!(cache_row.project_type, ProjectType::Service);
    assert_eq!(
        cache_row.source_type,
        temps_entities::source_type::SourceType::DockerImage
    );

    for (service_name, (created, row)) in &stored {
        assert_eq!(row.template_slug.as_deref(), Some("stack-app"));
        assert_eq!(
            row.instantiation_token.as_deref(),
            Some(temps_entities::projects::instantiation_token(41, service_name).as_str()),
            "'{service_name}' is sealed with the final token"
        );
        let snapshot = serde_json::from_value::<temps_core::templates::ServiceTemplateInstance>(
            row.service_template
                .clone()
                .unwrap_or_else(|| panic!("'{service_name}' keeps its release")),
        )
        .expect("the stored release decodes");
        snapshot.validate().expect("the stored release is valid");
        assert_eq!(snapshot.service.as_deref(), Some(*service_name));
        assert_eq!(
            snapshot.spec().map(|spec| spec.name.as_str()),
            Some(*service_name)
        );
        assert_eq!(
            created.project.service_template_version.as_deref(),
            Some("1.0.0")
        );
        let environments = environments::Entity::find()
            .filter(environments::Column::ProjectId.eq(row.id))
            .count(db.as_ref())
            .await
            .unwrap();
        assert!(environments > 0, "'{service_name}' has its environment");
    }

    // What the deployment environment reads back: the source-built
    // backend still carries its managed-database bindings.
    let api_release = serde_json::from_value::<temps_core::templates::ServiceTemplateInstance>(
        api_row.service_template.clone().expect("api release"),
    )
    .unwrap();
    assert_eq!(
        api_release
            .spec()
            .and_then(|spec| spec.database_bindings.get("postgres"))
            .and_then(|bindings| bindings.get("DATABASE_URL"))
            .map(String::as_str),
        Some("POSTGRES_URL")
    );
    assert_eq!(api.project.project_type, "server");

    // Upgrades are for image releases only: a source-built service
    // reports that plainly instead of failing a constraint.
    let error = project_service
        .get_applied_service_template(api_row.id)
        .await
        .expect_err("a source-built service has no upgradeable release");
    assert!(
        matches!(&error, ProjectError::InvalidInput(message)
            if message.contains(&api_row.id.to_string())),
        "{error:?}"
    );
    assert!(project_service
        .get_applied_service_template(cache_row.id)
        .await
        .is_ok());
}

/// Provenance is what lets a project carry a release without being a
/// service project, so a release with none must never reach the table.
#[tokio::test]
async fn a_release_without_template_provenance_is_refused_before_it_is_stored() {
    if !docker_available().await {
        println!("Docker not available, skipping");
        return;
    }
    let test_db = TestDatabase::with_migrations().await.unwrap();
    let db = test_db.db.clone();
    let project_service = create_test_services(db.clone(), Arc::new(MockJobQueue::new())).await;
    let template = stack_app_template();
    let release = temps_core::templates::ServiceTemplateInstance::new(
        temps_core::templates::SERVICE_TEMPLATE_SCHEMA_VERSION,
        template,
    )
    .for_service("api");
    let mut request = create_request("Provenance missing");
    request.preset = "dockerfile".to_string();
    request.template_slug = None;

    let result = project_service
        .create_template_service_as(
            request,
            Some(release),
            &SlugClaimCaller::project_writer(),
            true,
            None,
        )
        .await;
    assert!(
        matches!(result, Err(ProjectError::InvalidInput(_))),
        "a release needs its template slug"
    );
    assert_eq!(
        projects::Entity::find().count(db.as_ref()).await.unwrap(),
        0,
        "nothing was inserted"
    );
}

/// A one-service app cannot be moved onto a release with several
/// services: only the first service's runtime would be applied while the
/// whole snapshot was persisted.
#[tokio::test]
async fn service_template_upgrade_rejects_a_multi_service_target() {
    if !docker_available().await {
        println!("Docker not available, skipping");
        return;
    }
    let test_db = TestDatabase::with_migrations().await.unwrap();
    let db = test_db.db.clone();
    let mock_queue = Arc::new(MockJobQueue::new());
    let project_service = create_test_services(db.clone(), mock_queue).await;
    let applied = temps_core::templates::ServiceTemplateInstance::new(
        temps_core::templates::SERVICE_TEMPLATE_SCHEMA_VERSION,
        temps_core::templates::bundled_template_by_slug("browserless")
            .expect("bundled Browserless release"),
    );
    let mut request = create_request("Single service upgrade");
    request.preset = Preset::Dockerfile.to_string();
    request.source_type = temps_entities::source_type::SourceType::DockerImage;
    request.template_slug = Some(applied.slug.clone());
    request.preset_config = Some(
        serde_json::to_value(temps_entities::preset::PresetConfig::Dockerfile(
            temps_entities::preset::DockerfileConfig {
                image_runtime: Some(temps_entities::preset::ImageRuntimeConfig {
                    image_ref: applied.template.services[0]
                        .image
                        .clone()
                        .unwrap_or_default(),
                    command: applied.template.services[0].command.clone(),
                    health_check_path: applied.template.services[0].health_check_path.clone(),
                }),
                ..Default::default()
            },
        ))
        .expect("Dockerfile config serialization"),
    );
    request.exposed_port = applied.template.services[0].exposed_port;
    request.environment_variables = Some(vec![
        CreateProjectEnvVar {
            key: "TOKEN".to_string(),
            value: "existing-secret".to_string(),
            is_secret: true,
        },
        CreateProjectEnvVar {
            key: "EXTERNAL".to_string(),
            value: "https://browser.example.test".to_string(),
            is_secret: false,
        },
    ]);
    let created = project_service
        .create_service_project(request, applied.clone())
        .await
        .expect("service project creation");

    let mut target = applied.clone();
    target.version = "1.1.0".to_string();
    target.template.version = target.version.clone();
    let mut worker = target.template.services[0].clone();
    worker.name = "worker".to_string();
    target.template.services.push(worker);
    assert!(target.template.is_multi_service());

    let refused = project_service
        .upgrade_service_template(
            created.id,
            target,
            Vec::new(),
            temps_core::CeilingEnforcement::Bypass,
            DeployCaller::Platform,
        )
        .await;
    match refused {
        Err(ProjectError::SingleServiceUpgradeToMultiService {
            project_id,
            template_slug,
            target_version,
            service_count,
        }) => {
            assert_eq!(project_id, created.id);
            assert_eq!(template_slug, applied.slug);
            assert_eq!(target_version, "1.1.0");
            assert_eq!(service_count, 2);
        }
        Err(other) => panic!("expected SingleServiceUpgradeToMultiService, got {other:?}"),
        Ok(_) => panic!("a multi-service target must be refused"),
    }

    // Nothing was persisted: the project still carries the applied release.
    let stored = projects::Entity::find_by_id(created.id)
        .one(db.as_ref())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        applied_service_template_from_model(&stored)
            .unwrap()
            .version,
        applied.version
    );
}

// ── ADR 045: claiming a slug the host grants the Docker socket to ────
//
// The rule itself, against an explicit grant. `ProjectService` answers
// from a snapshot of the process grant, which is a `OnceLock` frozen at
// first use and therefore not settable from a parallel test — the same
// reason `docker_socket_bind_for` is a free function in the deployer.
mod reserved_slug {
    use super::*;
    use temps_core::docker_socket_grant::DockerSocketGrant;

    fn guard(granted: &str, slug: &str, authority: SlugClaimAuthority) -> Result<(), ProjectError> {
        guard_change(granted, slug, authority, ReservedSlugChange::Claim)
    }

    fn guard_change(
        granted: &str,
        slug: &str,
        authority: SlugClaimAuthority,
        change: ReservedSlugChange,
    ) -> Result<(), ProjectError> {
        ProjectService::guard_reserved_slug_against(
            &DockerSocketGrant::parse(Some(granted)),
            slug,
            authority,
            None,
            change,
        )
    }

    #[test]
    fn a_project_writer_cannot_claim_a_granted_slug() {
        let error = guard(
            "node-daemon",
            "node-daemon",
            SlugClaimAuthority::ProjectWriter,
        )
        .expect_err("a non-admin claim on host root must be refused");
        match error {
            ProjectError::DockerSocketSlugReserved {
                ref slug,
                change: ReservedSlugChange::Claim,
            } => {
                assert_eq!(slug, "node-daemon");
                // The operator reading the 403 has nobody to ask: it must
                // name the ADR, the variable and who may do it.
                let rendered = error.to_string();
                assert!(rendered.contains("ADR 045"), "{rendered}");
                assert!(
                    rendered.contains("TEMPS_DOCKER_SOCKET_PROJECTS"),
                    "{rendered}"
                );
            }
            other => panic!("expected DockerSocketSlugReserved, got {other:?}"),
        }
    }

    #[test]
    fn an_instance_admin_may_claim_a_granted_slug() {
        // Whoever can set the variable on the host is the same person who
        // is allowed to point a project at it.
        guard(
            "node-daemon",
            "node-daemon",
            SlugClaimAuthority::InstanceAdmin,
        )
        .expect("an admin may claim a granted slug");
    }

    #[test]
    fn an_unrelated_slug_is_unaffected_for_anyone() {
        guard("node-daemon", "my-app", SlugClaimAuthority::ProjectWriter)
            .expect("ordinary project creation is untouched");
        // Exact match, same as the bind: a near miss is not reserved
        // *and* would not be granted, so the two cannot disagree.
        guard(
            "node-daemon",
            "node-daemon-2",
            SlugClaimAuthority::ProjectWriter,
        )
        .expect("a near miss is not the granted slug");
    }

    #[test]
    fn nothing_is_reserved_when_this_host_grants_nothing() {
        // The state of every install that never set the variable.
        ProjectService::guard_reserved_slug_against(
            &DockerSocketGrant::default(),
            "node-daemon",
            SlugClaimAuthority::ProjectWriter,
            None,
            ReservedSlugChange::Claim,
        )
        .expect("an empty grant reserves nothing");
    }

    #[test]
    fn giving_up_a_granted_slug_is_admin_only_too() {
        // The symmetric half: guarding only the claim would leave
        // "rename it away, then create your own project with it" open to
        // any project writer, which is the same host-root outcome by two
        // requests instead of one.
        let error = guard_change(
            "node-daemon",
            "node-daemon",
            SlugClaimAuthority::ProjectWriter,
            ReservedSlugChange::Release,
        )
        .expect_err("a non-admin must not rename a granted project away");
        match error {
            ProjectError::DockerSocketSlugReserved {
                ref slug,
                change: ReservedSlugChange::Release,
            } => {
                assert_eq!(slug, "node-daemon");
                // The two refusals must not read alike: this one is about
                // losing access, not gaining it.
                let rendered = error.to_string();
                assert!(
                    rendered.contains("Renaming this project away"),
                    "{rendered}"
                );
                assert!(rendered.contains("revoke"), "{rendered}");
                assert!(rendered.contains("instance admin"), "{rendered}");
            }
            other => panic!("expected a Release refusal, got {other:?}"),
        }

        guard_change(
            "node-daemon",
            "node-daemon",
            SlugClaimAuthority::InstanceAdmin,
            ReservedSlugChange::Release,
        )
        .expect("an admin may rename a granted project away");

        // A project that never held a granted slug can be renamed freely.
        guard_change(
            "node-daemon",
            "my-app",
            SlugClaimAuthority::ProjectWriter,
            ReservedSlugChange::Release,
        )
        .expect("an ungranted slug is not reserved in either direction");
    }

    #[test]
    fn authority_defaults_to_the_fail_closed_answer() {
        // A caller that never thought about this must not be able to
        // claim host root by omission.
        assert_eq!(
            SlugClaimAuthority::default(),
            SlugClaimAuthority::ProjectWriter
        );
        assert!(!SlugClaimAuthority::default().may_claim_reserved_slug());
        assert!(SlugClaimAuthority::from_instance_admin(true).may_claim_reserved_slug());
        assert!(!SlugClaimAuthority::from_instance_admin(false).may_claim_reserved_slug());
    }
}

// ── ADR 045: writes that decide what a granted project runs ──────────
//
// The two-step version of the deploy attack: a project writer does not
// deploy anything, they change the input a later deployment executes as
// host root. Same injected-grant treatment as `reserved_slug` above, for
// the same reason.
mod granted_project_write {
    use super::*;
    use temps_core::docker_socket_grant::DockerSocketGrant;

    fn guard(granted: &str, slug: &str, caller: DeployCaller) -> Result<(), ProjectError> {
        ProjectService::guard_granted_project_write_against(
            &DockerSocketGrant::parse(Some(granted)),
            slug,
            "the runtime image and command",
            caller,
        )
    }

    /// The planted-payload path: `Role::User` holds `ProjectsWrite`, and
    /// the command it persists becomes the default for the *next*
    /// deployment — including one an admin starts.
    #[test]
    fn a_project_writer_cannot_change_what_a_granted_project_runs() {
        let error = guard("node-daemon", "node-daemon", DeployCaller::ProjectWriter)
            .expect_err("a non-admin must not decide what runs as host root");
        match error {
            ProjectError::DockerSocketWriteRequiresAdmin {
                ref slug,
                ref field,
            } => {
                assert_eq!(slug, "node-daemon");
                assert_eq!(field, "the runtime image and command");
                // The operator reading the 403 is alone: it has to name
                // the ADR, the variable, and which field was refused.
                let rendered = error.to_string();
                assert!(rendered.contains("ADR 045"), "{rendered}");
                assert!(
                    rendered.contains("TEMPS_DOCKER_SOCKET_PROJECTS"),
                    "{rendered}"
                );
                assert!(
                    rendered.contains("the runtime image and command"),
                    "{rendered}"
                );
            }
            other => panic!("expected DockerSocketWriteRequiresAdmin, got {other:?}"),
        }
    }

    #[test]
    fn an_instance_admin_may_change_it() {
        guard("node-daemon", "node-daemon", DeployCaller::InstanceAdmin)
            .expect("an admin may change what a granted project runs");
    }

    #[test]
    fn every_other_project_is_untouched() {
        for caller in [
            DeployCaller::ProjectWriter,
            DeployCaller::InstanceAdmin,
            DeployCaller::Platform,
        ] {
            guard("node-daemon", "my-app", caller)
                .expect("an undeclared project is untouched by ADR 045");
            // Exact match, same as the bind: a near miss is neither
            // declared nor granted, so the two cannot disagree.
            guard("node-daemon", "node-daemon-2", caller)
                .expect("a near miss is not the declared slug");
            ProjectService::guard_granted_project_write_against(
                &DockerSocketGrant::default(),
                "node-daemon",
                "the source repository",
                caller,
            )
            .expect("an install that never set the variable declares nothing");
        }
    }
}

/// What the acting user has done about MFA — the whole input to the
/// step-up policy (ADR 045).
#[derive(Clone, Copy)]
enum MfaState {
    /// No factor enrolled. `DefaultSensitiveActionAuthorizer` has nothing
    /// to re-verify and allows the action through without a challenge —
    /// deliberate, and documented in the ADR.
    NotEnrolled,
    /// Enrolled, but the session has not verified recently (or ever).
    EnrolledStale,
    /// Enrolled and verified inside the step-up window.
    EnrolledVerified,
}

static NEXT_SLUG_CLAIM_USER: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(1);

/// A signed-in caller for the reserved-slug guard: their session, and the
/// real step-up policy evaluated against the same database the session
/// lives in.
///
/// Built through [`SlugClaimCaller::from_request`], exactly as the
/// handlers build it, so a test cannot assert an authority the request
/// itself would not have carried.
struct SlugClaimSession {
    auth: temps_auth::AuthContext,
    authorizer: Arc<dyn temps_core::SensitiveActionAuthorizer>,
}

impl SlugClaimSession {
    async fn create(
        db: &Arc<temps_database::DbConnection>,
        role: temps_auth::Role,
        mfa: MfaState,
    ) -> Self {
        let seq = NEXT_SLUG_CLAIM_USER.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let user = temps_entities::users::ActiveModel {
            name: Set(format!("Slug Claimant {seq}")),
            email: Set(format!("slug-claimant-{seq}@example.com")),
            mfa_enabled: Set(!matches!(mfa, MfaState::NotEnrolled)),
            mfa_secret: Set(match mfa {
                MfaState::NotEnrolled => None,
                _ => Some("TOTPSECRET".to_string()),
            }),
            ..Default::default()
        }
        .insert(db.as_ref())
        .await
        .expect("the acting user is inserted");
        let session = temps_entities::sessions::ActiveModel {
            user_id: Set(user.id),
            session_token: Set(format!("slug-claim-session-{seq}")),
            expires_at: Set(chrono::Utc::now() + chrono::Duration::hours(1)),
            mfa_pending: Set(false),
            step_up_expires_at: Set(match mfa {
                MfaState::EnrolledVerified => {
                    Some(chrono::Utc::now() + chrono::Duration::minutes(5))
                }
                _ => None,
            }),
            ..Default::default()
        }
        .insert(db.as_ref())
        .await
        .expect("the acting session is inserted");
        Self {
            auth: temps_auth::AuthContext::new_persisted_session(user, role, session.id),
            authorizer: Arc::new(temps_auth::DefaultSensitiveActionAuthorizer::new(
                db.clone(),
            )),
        }
    }

    /// An instance admin who never enrolled MFA — the shape most existing
    /// ADR 045 tests need, and the documented pass-through case.
    async fn admin(db: &Arc<temps_database::DbConnection>) -> Self {
        Self::create(db, temps_auth::Role::Admin, MfaState::NotEnrolled).await
    }

    /// An ordinary project writer, MFA enrolled and *not* stepped up, so
    /// a test can prove the 403 comes out before step-up is consulted.
    async fn project_writer(db: &Arc<temps_database::DbConnection>) -> Self {
        Self::create(db, temps_auth::Role::User, MfaState::EnrolledStale).await
    }

    fn caller(&self) -> SlugClaimCaller<'_> {
        SlugClaimCaller::from_request(&self.auth, self.authorizer.as_ref())
    }
}

/// A non-admin creating a project whose slug this host grants the Docker
/// socket to is refused — including when the slug is only *derived* from
/// the display name, which is how the claim would be made in practice.
#[tokio::test]
async fn create_project_refuses_a_non_admin_claim_on_a_granted_slug() {
    if !docker_available().await {
        println!("Docker not available, skipping");
        return;
    }
    let test_db = TestDatabase::with_migrations().await.unwrap();
    let db = test_db.db.clone();
    let project_service = create_test_services(db.clone(), Arc::new(MockJobQueue::new()))
        .await
        .with_docker_socket_grant(temps_core::docker_socket_grant::DockerSocketGrant::parse(
            Some("node-daemon"),
        ));
    let writer = SlugClaimSession::project_writer(&db).await;

    // `Project`/`ProjectSettingsUpdate` are not `Debug`, so the error is
    // taken by match rather than `expect_err`.
    let error = match project_service
        .create_project_as(create_request("Node Daemon"), &writer.caller())
        .await
    {
        Ok(project) => panic!("a project writer claimed a granted slug: {}", project.slug),
        Err(error) => error,
    };
    assert!(matches!(
        error,
        ProjectError::DockerSocketSlugReserved {
            ref slug,
            change: ReservedSlugChange::Claim,
        } if slug == "node-daemon"
    ));

    // Nothing was written: the guard runs before the insert.
    let existing = projects::Entity::find()
        .filter(projects::Column::Slug.eq("node-daemon"))
        .one(db.as_ref())
        .await
        .unwrap();
    assert!(existing.is_none(), "no project row may be created");
}

#[tokio::test]
async fn create_project_allows_an_instance_admin_to_claim_a_granted_slug() {
    if !docker_available().await {
        println!("Docker not available, skipping");
        return;
    }
    let test_db = TestDatabase::with_migrations().await.unwrap();
    let db = test_db.db.clone();
    let project_service = create_test_services(db.clone(), Arc::new(MockJobQueue::new()))
        .await
        .with_docker_socket_grant(temps_core::docker_socket_grant::DockerSocketGrant::parse(
            Some("node-daemon"),
        ));
    let admin = SlugClaimSession::admin(&db).await;

    let project = project_service
        .create_project_as(create_request("Node Daemon"), &admin.caller())
        .await
        .expect("an admin may create the granted project");
    assert_eq!(project.slug, "node-daemon");
}

#[tokio::test]
async fn update_project_settings_refuses_a_non_admin_rename_onto_a_granted_slug() {
    if !docker_available().await {
        println!("Docker not available, skipping");
        return;
    }
    let test_db = TestDatabase::with_migrations().await.unwrap();
    let db = test_db.db.clone();
    let project_service = create_test_services(db.clone(), Arc::new(MockJobQueue::new()))
        .await
        .with_docker_socket_grant(temps_core::docker_socket_grant::DockerSocketGrant::parse(
            Some("node-daemon"),
        ));
    let writer = SlugClaimSession::project_writer(&db).await;
    let admin = SlugClaimSession::admin(&db).await;

    let project = temps_entities::projects::ActiveModel {
        name: Set("Ordinary App".to_string()),
        slug: Set("ordinary-app".to_string()),
        repo_name: Set("repo".to_string()),
        repo_owner: Set("owner".to_string()),
        directory: Set("/".to_string()),
        main_branch: Set("main".to_string()),
        preset: Set(Preset::Nixpacks),
        ..Default::default()
    }
    .insert(db.as_ref())
    .await
    .unwrap();

    let error = match project_service
        .update_project_settings_as(
            project.id,
            UpdateProjectSettingsParams {
                slug: Some("node-daemon".to_string()),
                ..Default::default()
            },
            &writer.caller(),
        )
        .await
    {
        Ok(_) => panic!("a project writer renamed a project onto a granted slug"),
        Err(error) => error,
    };
    assert!(matches!(
        error,
        ProjectError::DockerSocketSlugReserved {
            ref slug,
            change: ReservedSlugChange::Claim,
        } if slug == "node-daemon"
    ));

    let reloaded = projects::Entity::find_by_id(project.id)
        .one(db.as_ref())
        .await
        .unwrap()
        .expect("project still exists");
    assert_eq!(reloaded.slug, "ordinary-app", "the rename must not persist");

    // The same rename from an admin goes through.
    project_service
        .update_project_settings_as(
            project.id,
            UpdateProjectSettingsParams {
                slug: Some("node-daemon".to_string()),
                ..Default::default()
            },
            &admin.caller(),
        )
        .await
        .expect("an admin may claim the granted slug");
}

/// ADR 045, the git-push path: the slug guard and the deploy guard both
/// leave this open. A project writer who cannot rename the project and
/// cannot deploy it can still repoint its repository and push — and the
/// resulting build+deploy arrives from a provider webhook with no
/// authenticated principal, so there is nothing for the deploy guard to
/// refuse and no HTTP deploy request is ever made.
#[tokio::test]
async fn update_project_settings_refuses_a_non_admin_repo_change_on_a_granted_project() {
    if !docker_available().await {
        println!("Docker not available, skipping");
        return;
    }
    let test_db = TestDatabase::with_migrations().await.unwrap();
    let db = test_db.db.clone();
    let project_service = create_test_services(db.clone(), Arc::new(MockJobQueue::new()))
        .await
        .with_docker_socket_grant(temps_core::docker_socket_grant::DockerSocketGrant::parse(
            Some("node-daemon"),
        ));
    let writer = SlugClaimSession::project_writer(&db).await;

    let project = temps_entities::projects::ActiveModel {
        name: Set("Node Daemon".to_string()),
        slug: Set("node-daemon".to_string()),
        repo_name: Set("node-daemon".to_string()),
        repo_owner: Set("operator".to_string()),
        directory: Set("/".to_string()),
        main_branch: Set("main".to_string()),
        preset: Set(Preset::Nixpacks),
        ..Default::default()
    }
    .insert(db.as_ref())
    .await
    .unwrap();

    let error = match project_service
        .update_project_settings_as(
            project.id,
            UpdateProjectSettingsParams {
                repo_owner: Some("attacker".to_string()),
                repo_name: Some("payload".to_string()),
                ..Default::default()
            },
            &writer.caller(),
        )
        .await
    {
        Ok(_) => panic!("a project writer repointed a granted project's repository"),
        Err(error) => error,
    };
    assert!(matches!(
        error,
        ProjectError::DockerSocketWriteRequiresAdmin { ref slug, .. } if slug == "node-daemon"
    ));

    // Nothing was written: the guard runs before the first of this
    // endpoint's several independent writes.
    let reloaded = projects::Entity::find_by_id(project.id)
        .one(db.as_ref())
        .await
        .unwrap()
        .expect("project still exists");
    assert_eq!(reloaded.repo_owner, "operator");
    assert_eq!(reloaded.repo_name, "node-daemon");

    // A project no host declares is untouched — the ordinary case, and
    // the only one on an install that never set the variable.
    let ordinary = temps_entities::projects::ActiveModel {
        name: Set("Ordinary App".to_string()),
        slug: Set("ordinary-app".to_string()),
        repo_name: Set("repo".to_string()),
        repo_owner: Set("owner".to_string()),
        directory: Set("/".to_string()),
        main_branch: Set("main".to_string()),
        preset: Set(Preset::Nixpacks),
        ..Default::default()
    }
    .insert(db.as_ref())
    .await
    .unwrap();
    project_service
        .update_project_settings_as(
            ordinary.id,
            UpdateProjectSettingsParams {
                main_branch: Some("release".to_string()),
                ..Default::default()
            },
            &writer.caller(),
        )
        .await
        .expect("an undeclared project's source is writable by any project writer");
}

/// `git_provider_connection_id` alone, with owner/name/branch untouched.
/// `DownloadRepoJob` resolves the clone host from the *connection*, not
/// from a stored URL, so repointing only the connection is the same
/// escalation as the repo-owner/repo-name case above through a field the
/// guard's predicate previously missed.
#[tokio::test]
async fn update_project_settings_refuses_a_non_admin_connection_change_on_a_granted_project() {
    if !docker_available().await {
        println!("Docker not available, skipping");
        return;
    }
    let test_db = TestDatabase::with_migrations().await.unwrap();
    let db = test_db.db.clone();
    let project_service = create_test_services(db.clone(), Arc::new(MockJobQueue::new()))
        .await
        .with_docker_socket_grant(temps_core::docker_socket_grant::DockerSocketGrant::parse(
            Some("node-daemon"),
        ));
    let writer = SlugClaimSession::project_writer(&db).await;

    // Two real connections on the same provider -- the escalation is
    // repointing *which one* the project trusts, not whether the target
    // id exists, so both ends of the move must resolve.
    let provider = temps_entities::git_providers::ActiveModel {
        name: Set("Operator Git".to_string()),
        provider_type: Set("gitea".to_string()),
        base_url: Set(Some("https://git.example.internal".to_string())),
        api_url: Set(None),
        auth_method: Set("pat".to_string()),
        auth_config: Set(serde_json::json!({})),
        webhook_secret: Set(None),
        is_active: Set(true),
        is_default: Set(false),
        ..Default::default()
    }
    .insert(db.as_ref())
    .await
    .unwrap();
    let operator_connection = temps_entities::git_provider_connections::ActiveModel {
        provider_id: Set(provider.id),
        user_id: Set(None),
        account_name: Set("operator".to_string()),
        account_type: Set("Organization".to_string()),
        is_active: Set(true),
        ..Default::default()
    }
    .insert(db.as_ref())
    .await
    .unwrap();
    let attacker_connection = temps_entities::git_provider_connections::ActiveModel {
        provider_id: Set(provider.id),
        user_id: Set(None),
        account_name: Set("attacker".to_string()),
        account_type: Set("User".to_string()),
        is_active: Set(true),
        ..Default::default()
    }
    .insert(db.as_ref())
    .await
    .unwrap();

    let project = temps_entities::projects::ActiveModel {
        name: Set("Node Daemon".to_string()),
        slug: Set("node-daemon".to_string()),
        repo_name: Set("node-daemon".to_string()),
        repo_owner: Set("operator".to_string()),
        directory: Set("/".to_string()),
        main_branch: Set("main".to_string()),
        preset: Set(Preset::Nixpacks),
        git_provider_connection_id: Set(Some(operator_connection.id)),
        ..Default::default()
    }
    .insert(db.as_ref())
    .await
    .unwrap();

    let error = match project_service
        .update_project_settings_as(
            project.id,
            UpdateProjectSettingsParams {
                git_provider_connection_id: Some(attacker_connection.id),
                ..Default::default()
            },
            &writer.caller(),
        )
        .await
    {
        Ok(_) => panic!("a project writer repointed a granted project's git connection"),
        Err(error) => error,
    };
    assert!(matches!(
        error,
        ProjectError::DockerSocketWriteRequiresAdmin { ref slug, .. } if slug == "node-daemon"
    ));

    let reloaded = projects::Entity::find_by_id(project.id)
        .one(db.as_ref())
        .await
        .unwrap()
        .expect("project still exists");
    assert_eq!(
        reloaded.git_provider_connection_id,
        Some(operator_connection.id)
    );

    // An instance admin may still repoint it.
    let admin = SlugClaimSession::admin(&db).await;
    project_service
        .update_project_settings_as(
            project.id,
            UpdateProjectSettingsParams {
                git_provider_connection_id: Some(attacker_connection.id),
                ..Default::default()
            },
            &admin.caller(),
        )
        .await
        .expect("an instance admin may repoint a granted project's git connection");
}

/// `enable_preview_environments` was missing from the guard predicate:
/// once on, any push to an untracked branch gets a preview environment
/// auto-created and deployed as `DeployCaller::Platform`, which neither
/// the deploy gate nor the exec gate refuses.
#[tokio::test]
async fn update_project_settings_refuses_a_non_admin_enabling_preview_environments_on_a_granted_project(
) {
    if !docker_available().await {
        println!("Docker not available, skipping");
        return;
    }
    let test_db = TestDatabase::with_migrations().await.unwrap();
    let db = test_db.db.clone();
    let project_service = create_test_services(db.clone(), Arc::new(MockJobQueue::new()))
        .await
        .with_docker_socket_grant(temps_core::docker_socket_grant::DockerSocketGrant::parse(
            Some("node-daemon"),
        ));
    let writer = SlugClaimSession::project_writer(&db).await;

    let project = temps_entities::projects::ActiveModel {
        name: Set("Node Daemon".to_string()),
        slug: Set("node-daemon".to_string()),
        repo_name: Set("node-daemon".to_string()),
        repo_owner: Set("operator".to_string()),
        directory: Set("/".to_string()),
        main_branch: Set("main".to_string()),
        preset: Set(Preset::Nixpacks),
        ..Default::default()
    }
    .insert(db.as_ref())
    .await
    .unwrap();

    let error = match project_service
        .update_project_settings_as(
            project.id,
            UpdateProjectSettingsParams {
                enable_preview_environments: Some(true),
                ..Default::default()
            },
            &writer.caller(),
        )
        .await
    {
        Ok(_) => panic!("a project writer enabled preview environments on a granted project"),
        Err(error) => error,
    };
    assert!(matches!(
        error,
        ProjectError::DockerSocketWriteRequiresAdmin { ref slug, .. } if slug == "node-daemon"
    ));

    let admin = SlugClaimSession::admin(&db).await;
    project_service
        .update_project_settings_as(
            project.id,
            UpdateProjectSettingsParams {
                enable_preview_environments: Some(true),
                ..Default::default()
            },
            &admin.caller(),
        )
        .await
        .expect("an instance admin may enable preview environments on a granted project");
}

/// ADR 045: `update_project` took no `caller` at all until this test —
/// it unconditionally rewrites the same source-definition fields
/// `update_project_settings_as` gates, through a sibling handler that
/// bypassed the guard entirely.
#[tokio::test]
async fn update_project_refuses_a_non_admin_on_a_granted_project() {
    if !docker_available().await {
        println!("Docker not available, skipping");
        return;
    }
    let test_db = TestDatabase::with_migrations().await.unwrap();
    let db = test_db.db.clone();
    let project_service = create_test_services(db.clone(), Arc::new(MockJobQueue::new()))
        .await
        .with_docker_socket_grant(temps_core::docker_socket_grant::DockerSocketGrant::parse(
            Some("node-daemon"),
        ));

    let project = temps_entities::projects::ActiveModel {
        name: Set("Node Daemon".to_string()),
        slug: Set("node-daemon".to_string()),
        repo_name: Set("node-daemon".to_string()),
        repo_owner: Set("operator".to_string()),
        directory: Set("/".to_string()),
        main_branch: Set("main".to_string()),
        preset: Set(Preset::Nixpacks),
        ..Default::default()
    }
    .insert(db.as_ref())
    .await
    .unwrap();

    let mut attacker_request = create_request("Node Daemon");
    attacker_request.repo_owner = Some("attacker".to_string());
    attacker_request.repo_name = Some("payload".to_string());

    let error = match project_service
        .update_project(
            project.id,
            attacker_request,
            temps_core::docker_socket_grant::DeployCaller::ProjectWriter,
        )
        .await
    {
        Ok(_) => panic!("a project writer repointed a granted project's repository"),
        Err(error) => error,
    };
    assert!(matches!(
        error,
        ProjectError::DockerSocketWriteRequiresAdmin { ref slug, .. } if slug == "node-daemon"
    ));

    let reloaded = projects::Entity::find_by_id(project.id)
        .one(db.as_ref())
        .await
        .unwrap()
        .expect("project still exists");
    assert_eq!(reloaded.repo_owner, "operator");
    assert_eq!(reloaded.repo_name, "node-daemon");

    // An instance admin may still update it.
    let mut admin_request = create_request("Node Daemon");
    admin_request.repo_owner = Some("attacker".to_string());
    admin_request.repo_name = Some("payload".to_string());
    project_service
        .update_project(
            project.id,
            admin_request,
            temps_core::docker_socket_grant::DeployCaller::InstanceAdmin,
        )
        .await
        .expect("an instance admin may update a granted project");
}

/// A new environment is a new push-deploy target bound to whatever
/// branch the request names, and inherits the project's
/// `automatic_deploy` — reachable by `EnvironmentsCreate`
/// (`Role::User`) with no ADR-045 check until this test.
#[tokio::test]
async fn update_automatic_deploy_refuses_a_non_admin_on_a_granted_project() {
    if !docker_available().await {
        println!("Docker not available, skipping");
        return;
    }
    let test_db = TestDatabase::with_migrations().await.unwrap();
    let db = test_db.db.clone();
    let project_service = create_test_services(db.clone(), Arc::new(MockJobQueue::new()))
        .await
        .with_docker_socket_grant(temps_core::docker_socket_grant::DockerSocketGrant::parse(
            Some("node-daemon"),
        ));

    let project = temps_entities::projects::ActiveModel {
        name: Set("Node Daemon".to_string()),
        slug: Set("node-daemon".to_string()),
        repo_name: Set("node-daemon".to_string()),
        repo_owner: Set("operator".to_string()),
        directory: Set("/".to_string()),
        main_branch: Set("main".to_string()),
        preset: Set(Preset::Nixpacks),
        ..Default::default()
    }
    .insert(db.as_ref())
    .await
    .unwrap();

    let error = match project_service
        .update_automatic_deploy(
            project.id,
            true,
            temps_core::docker_socket_grant::DeployCaller::ProjectWriter,
        )
        .await
    {
        Ok(_) => panic!("a project writer armed auto-deploy on a granted project"),
        Err(error) => error,
    };
    assert!(matches!(
        error,
        ProjectError::DockerSocketWriteRequiresAdmin { ref slug, .. } if slug == "node-daemon"
    ));

    project_service
        .update_automatic_deploy(
            project.id,
            true,
            temps_core::docker_socket_grant::DeployCaller::InstanceAdmin,
        )
        .await
        .expect("an instance admin may arm auto-deploy on a granted project");
}

/// The symmetric case: renaming a project *away* from a granted slug is
/// admin-only too. It revokes that project's host Docker access on every
/// host, and frees the slug for the next project created — so allowing it
/// would let any project writer reach host root in two requests.
#[tokio::test]
async fn update_project_settings_refuses_a_non_admin_rename_away_from_a_granted_slug() {
    if !docker_available().await {
        println!("Docker not available, skipping");
        return;
    }
    let test_db = TestDatabase::with_migrations().await.unwrap();
    let db = test_db.db.clone();
    let project_service = create_test_services(db.clone(), Arc::new(MockJobQueue::new()))
        .await
        .with_docker_socket_grant(temps_core::docker_socket_grant::DockerSocketGrant::parse(
            Some("node-daemon"),
        ));
    let writer = SlugClaimSession::project_writer(&db).await;
    let admin = SlugClaimSession::admin(&db).await;

    let project = temps_entities::projects::ActiveModel {
        name: Set("Node Daemon".to_string()),
        slug: Set("node-daemon".to_string()),
        repo_name: Set("repo".to_string()),
        repo_owner: Set("owner".to_string()),
        directory: Set("/".to_string()),
        main_branch: Set("main".to_string()),
        preset: Set(Preset::Nixpacks),
        ..Default::default()
    }
    .insert(db.as_ref())
    .await
    .unwrap();

    let error = match project_service
        .update_project_settings_as(
            project.id,
            UpdateProjectSettingsParams {
                slug: Some("something-else".to_string()),
                ..Default::default()
            },
            &writer.caller(),
        )
        .await
    {
        Ok(_) => panic!("a project writer renamed a granted project away from its slug"),
        Err(error) => error,
    };
    assert!(
        matches!(
            error,
            ProjectError::DockerSocketSlugReserved {
                ref slug,
                change: ReservedSlugChange::Release,
            } if slug == "node-daemon"
        ),
        "the refusal must name the slug being given up, not the new one: {error}"
    );

    let reloaded = projects::Entity::find_by_id(project.id)
        .one(db.as_ref())
        .await
        .unwrap()
        .expect("project still exists");
    assert_eq!(reloaded.slug, "node-daemon", "the rename must not persist");

    // The same rename from an admin goes through.
    let updated = project_service
        .update_project_settings_as(
            project.id,
            UpdateProjectSettingsParams {
                slug: Some("something-else".to_string()),
                ..Default::default()
            },
            &admin.caller(),
        )
        .await
        .expect("an admin may rename a granted project away");
    assert_eq!(updated.project.slug, "something-else");
}

/// Only a slug *change* is gated. An existing granted project must keep
/// working through ordinary settings saves by anyone who can write it —
/// including one that re-sends its current (granted) slug unchanged.
#[tokio::test]
async fn update_project_settings_leaves_an_already_granted_project_alone() {
    if !docker_available().await {
        println!("Docker not available, skipping");
        return;
    }
    let test_db = TestDatabase::with_migrations().await.unwrap();
    let db = test_db.db.clone();
    let project_service = create_test_services(db.clone(), Arc::new(MockJobQueue::new()))
        .await
        .with_docker_socket_grant(temps_core::docker_socket_grant::DockerSocketGrant::parse(
            Some("node-daemon"),
        ));
    let writer = SlugClaimSession::project_writer(&db).await;

    let project = temps_entities::projects::ActiveModel {
        name: Set("Node Daemon".to_string()),
        slug: Set("node-daemon".to_string()),
        repo_name: Set("repo".to_string()),
        repo_owner: Set("owner".to_string()),
        directory: Set("/".to_string()),
        main_branch: Set("main".to_string()),
        preset: Set(Preset::Nixpacks),
        ..Default::default()
    }
    .insert(db.as_ref())
    .await
    .unwrap();

    // No slug in the request at all.
    project_service
        .update_project_settings_as(
            project.id,
            UpdateProjectSettingsParams {
                name: Some("Node Daemon v2".to_string()),
                ..Default::default()
            },
            &writer.caller(),
        )
        .await
        .expect("an unrelated settings change is not a claim");

    // The current slug, re-sent unchanged (what a settings form posts).
    let updated = project_service
        .update_project_settings_as(
            project.id,
            UpdateProjectSettingsParams {
                slug: Some("node-daemon".to_string()),
                ..Default::default()
            },
            &writer.caller(),
        )
        .await
        .expect("re-sending the existing slug is not a claim");
    assert_eq!(updated.project.slug, "node-daemon");
}

/// The capability query asks Postgres which nodes advertise the slug,
/// rather than loading the fleet and filtering in Rust. Exercised against
/// a real database because the whole point of the change is the SQL — a
/// mock would assert the shape of a query nobody ran.
#[tokio::test]
async fn docker_socket_capability_matches_advertised_slugs_in_the_database() {
    if !docker_available().await {
        println!("Docker not available, skipping");
        return;
    }
    let test_db = TestDatabase::with_migrations().await.unwrap();
    let db = test_db.db.clone();
    let project_service = create_test_services(db.clone(), Arc::new(MockJobQueue::new()))
        .await
        .with_docker_socket_grant(temps_core::docker_socket_grant::DockerSocketGrant::parse(
            Some("node-daemon"),
        ));

    let insert_node = |name: &'static str, capacity: serde_json::Value| {
        let db = db.clone();
        async move {
            temps_entities::nodes::ActiveModel {
                name: Set(name.to_string()),
                token_hash: Set(format!("hash-{name}")),
                address: Set(format!("https://{name}.invalid:3100")),
                private_address: Set("10.100.0.2".to_string()),
                role: Set("worker".to_string()),
                status: Set("active".to_string()),
                labels: Set(serde_json::json!({})),
                capacity: Set(capacity),
                ..Default::default()
            }
            .insert(db.as_ref())
            .await
            .expect("the node is inserted");
        }
    };

    insert_node(
        "worker-b",
        serde_json::json!({ "docker_socket_projects": ["node-daemon"] }),
    )
    .await;
    insert_node(
        "worker-a",
        serde_json::json!({ "docker_socket_projects": ["other-thing", "node-daemon"] }),
    )
    .await;
    // Near miss, wrong key, malformed value and no capacity at all: every
    // one of these must fail closed rather than widen the grant.
    insert_node(
        "worker-c",
        serde_json::json!({ "docker_socket_projects": ["node-daemon-2"] }),
    )
    .await;
    insert_node(
        "worker-d",
        serde_json::json!({ "docker_socket_projects": "node-daemon" }),
    )
    .await;
    insert_node("worker-e", serde_json::json!({ "cpu_cores": 4 })).await;

    let capability = project_service
        .docker_socket_capability("node-daemon")
        .await
        .expect("the capability is computed");
    assert!(capability.granted, "the control plane declares this slug");
    assert_eq!(
        capability.nodes,
        vec![
            // This control plane grants it too, by declaring it.
            "control-plane".to_string(),
            "worker-a".to_string(),
            "worker-b".to_string()
        ],
        "only exact advertisements count, ordered by name"
    );

    // A slug this control plane never declared is not granted, whatever
    // any node advertises — the declare/provide split of ADR 045.
    let undeclared = project_service
        .docker_socket_capability("other-thing")
        .await
        .expect("the capability is computed");
    assert!(!undeclared.granted);
}

// ── ADR 045: MFA step-up on top of the instance-admin check ─────────
//
// The admin check answers "may this principal move host root between
// projects". These answer "and is this really them, right now". The order
// is load-bearing and is asserted explicitly below.

/// A granted project, so both directions of the guard have something to
/// bite on.
async fn insert_granted_project(
    db: &Arc<temps_database::DbConnection>,
) -> temps_entities::projects::Model {
    temps_entities::projects::ActiveModel {
        name: Set("Node Daemon".to_string()),
        slug: Set("node-daemon".to_string()),
        repo_name: Set("repo".to_string()),
        repo_owner: Set("owner".to_string()),
        directory: Set("/".to_string()),
        main_branch: Set("main".to_string()),
        preset: Set(Preset::Nixpacks),
        ..Default::default()
    }
    .insert(db.as_ref())
    .await
    .expect("the granted project is inserted")
}

/// The 428 body the console's step-up dialog reads. Asserted here rather
/// than trusted, because `ProjectError::SlugClaimStepUpRequired` passes
/// the `Problem` through verbatim — if the contract ever changed, this is
/// where it must fail.
fn assert_step_up_challenge(error: &ProjectError, expected_action: &str) {
    let ProjectError::SlugClaimStepUpRequired { problem } = error else {
        panic!("expected a step-up challenge, got {error:?}");
    };
    assert_eq!(
        problem.status_code,
        axum::http::StatusCode::PRECONDITION_REQUIRED
    );
    assert_eq!(
        problem.body.get("error_code").and_then(|v| v.as_str()),
        Some("STEP_UP_REQUIRED")
    );
    assert_eq!(
        problem.body.get("action").and_then(|v| v.as_str()),
        Some(expected_action),
        "the challenge must name the exact action, not a shared one"
    );
}

/// An MFA-enrolled admin whose session has not verified recently is
/// challenged for the claim, and nothing is written until they do.
#[tokio::test]
async fn claiming_a_granted_slug_challenges_an_mfa_enrolled_admin() {
    if !docker_available().await {
        println!("Docker not available, skipping");
        return;
    }
    let test_db = TestDatabase::with_migrations().await.unwrap();
    let db = test_db.db.clone();
    let project_service = create_test_services(db.clone(), Arc::new(MockJobQueue::new()))
        .await
        .with_docker_socket_grant(temps_core::docker_socket_grant::DockerSocketGrant::parse(
            Some("node-daemon"),
        ));
    let stale =
        SlugClaimSession::create(&db, temps_auth::Role::Admin, MfaState::EnrolledStale).await;

    let error = match project_service
        .create_project_as(create_request("Node Daemon"), &stale.caller())
        .await
    {
        Ok(project) => panic!(
            "an admin claimed host root without recent verification: {}",
            project.slug
        ),
        Err(error) => error,
    };
    assert_step_up_challenge(&error, "claim_docker_socket_slug");

    // The challenge is a refusal, not a warning: the project must not
    // exist yet.
    assert!(
        projects::Entity::find()
            .filter(projects::Column::Slug.eq("node-daemon"))
            .one(db.as_ref())
            .await
            .unwrap()
            .is_none(),
        "a challenged claim must not create the project"
    );

    // Same admin, same authority — only the verification is new.
    let verified =
        SlugClaimSession::create(&db, temps_auth::Role::Admin, MfaState::EnrolledVerified).await;
    let project = project_service
        .create_project_as(create_request("Node Daemon"), &verified.caller())
        .await
        .expect("a verified admin may claim the granted slug");
    assert_eq!(project.slug, "node-daemon");
}

/// Giving the slug up is a separate action with its own name, so an
/// operator reading the audit trail can tell a grant from a revocation.
#[tokio::test]
async fn releasing_a_granted_slug_challenges_with_its_own_action_name() {
    if !docker_available().await {
        println!("Docker not available, skipping");
        return;
    }
    let test_db = TestDatabase::with_migrations().await.unwrap();
    let db = test_db.db.clone();
    let project_service = create_test_services(db.clone(), Arc::new(MockJobQueue::new()))
        .await
        .with_docker_socket_grant(temps_core::docker_socket_grant::DockerSocketGrant::parse(
            Some("node-daemon"),
        ));
    let project = insert_granted_project(&db).await;
    let stale =
        SlugClaimSession::create(&db, temps_auth::Role::Admin, MfaState::EnrolledStale).await;

    let error = match project_service
        .update_project_settings_as(
            project.id,
            UpdateProjectSettingsParams {
                slug: Some("something-else".to_string()),
                ..Default::default()
            },
            &stale.caller(),
        )
        .await
    {
        Ok(_) => panic!("an admin revoked host Docker access without recent verification"),
        Err(error) => error,
    };
    // The claim half of the rename passes first (`something-else` is not
    // reserved), so the challenge that surfaces is the release.
    assert_step_up_challenge(&error, "release_docker_socket_slug");

    let reloaded = projects::Entity::find_by_id(project.id)
        .one(db.as_ref())
        .await
        .unwrap()
        .expect("project still exists");
    assert_eq!(
        reloaded.slug, "node-daemon",
        "a challenged release must not persist"
    );

    let verified =
        SlugClaimSession::create(&db, temps_auth::Role::Admin, MfaState::EnrolledVerified).await;
    let updated = project_service
        .update_project_settings_as(
            project.id,
            UpdateProjectSettingsParams {
                slug: Some("something-else".to_string()),
                ..Default::default()
            },
            &verified.caller(),
        )
        .await
        .expect("a verified admin may release the granted slug");
    assert_eq!(updated.project.slug, "something-else");
}

/// An admin who never enrolled MFA is allowed through with no challenge.
///
/// Asserted rather than left implicit, because it is a deliberate policy
/// choice of `DefaultSensitiveActionAuthorizer` (there is no second factor
/// to re-verify, and denying would lock the only admin out of their own
/// instance), not an oversight in this guard. An operator who wants the
/// stronger rule enrolls MFA — or registers an authorizer that denies
/// unenrolled principals. See ADR 045.
#[tokio::test]
async fn an_admin_without_mfa_is_allowed_through_without_a_challenge() {
    if !docker_available().await {
        println!("Docker not available, skipping");
        return;
    }
    let test_db = TestDatabase::with_migrations().await.unwrap();
    let db = test_db.db.clone();
    let project_service = create_test_services(db.clone(), Arc::new(MockJobQueue::new()))
        .await
        .with_docker_socket_grant(temps_core::docker_socket_grant::DockerSocketGrant::parse(
            Some("node-daemon"),
        ));
    let unenrolled =
        SlugClaimSession::create(&db, temps_auth::Role::Admin, MfaState::NotEnrolled).await;

    let project = project_service
        .create_project_as(create_request("Node Daemon"), &unenrolled.caller())
        .await
        .expect("an admin with no enrolled factor is not challenged");
    assert_eq!(project.slug, "node-daemon");
}

/// Order matters: a project writer is refused by the authority check,
/// *before* step-up is considered. They must get the 403 that explains the
/// rule, never an MFA prompt for an operation they still could not perform
/// after satisfying it.
#[tokio::test]
async fn a_non_admin_is_refused_before_step_up_is_considered() {
    if !docker_available().await {
        println!("Docker not available, skipping");
        return;
    }
    let test_db = TestDatabase::with_migrations().await.unwrap();
    let db = test_db.db.clone();
    let project_service = create_test_services(db.clone(), Arc::new(MockJobQueue::new()))
        .await
        .with_docker_socket_grant(temps_core::docker_socket_grant::DockerSocketGrant::parse(
            Some("node-daemon"),
        ));
    // MFA enrolled and stale: this session *would* be challenged if the
    // guard ever reached step-up for it.
    let writer =
        SlugClaimSession::create(&db, temps_auth::Role::User, MfaState::EnrolledStale).await;

    let error = match project_service
        .create_project_as(create_request("Node Daemon"), &writer.caller())
        .await
    {
        Ok(project) => panic!("a project writer claimed a granted slug: {}", project.slug),
        Err(error) => error,
    };
    assert!(
        matches!(
            error,
            ProjectError::DockerSocketSlugReserved {
                change: ReservedSlugChange::Claim,
                ..
            }
        ),
        "a non-admin must be refused outright, not challenged: {error:?}"
    );
}

/// The challenge is scoped to reserved slugs. Every other project an
/// MFA-enrolled admin creates or renames goes through untouched — this is
/// the regression that would turn one operator control into friction on
/// every project in the console.
#[tokio::test]
async fn an_unreserved_slug_never_challenges_anyone() {
    if !docker_available().await {
        println!("Docker not available, skipping");
        return;
    }
    let test_db = TestDatabase::with_migrations().await.unwrap();
    let db = test_db.db.clone();
    let project_service = create_test_services(db.clone(), Arc::new(MockJobQueue::new()))
        .await
        .with_docker_socket_grant(temps_core::docker_socket_grant::DockerSocketGrant::parse(
            Some("node-daemon"),
        ));
    let stale =
        SlugClaimSession::create(&db, temps_auth::Role::Admin, MfaState::EnrolledStale).await;

    let project = project_service
        .create_project_as(create_request("Ordinary App"), &stale.caller())
        .await
        .expect("an ordinary project is not a sensitive action");
    assert_eq!(project.slug, "ordinary-app");

    let updated = project_service
        .update_project_settings_as(
            project.id,
            UpdateProjectSettingsParams {
                slug: Some("still-ordinary".to_string()),
                ..Default::default()
            },
            &stale.caller(),
        )
        .await
        .expect("renaming between two unreserved slugs is not a sensitive action");
    assert_eq!(updated.project.slug, "still-ordinary");
}

/// [`ProjectService::preflight_guard_reserved_slug`] exists so a caller
/// with an irreversible side effect (a template fork's upstream repo
/// creation) between planning a slug and creating the project can fail
/// fast, before that side effect runs. It must therefore apply the exact
/// same authority check as the creation-time guard it stands in front
/// of: a project writer preflighting a granted slug is refused outright,
/// with no MFA prompt, identically to `create_project_as`.
#[tokio::test]
async fn preflight_guard_reserved_slug_refuses_a_non_admin() {
    if !docker_available().await {
        println!("Docker not available, skipping");
        return;
    }
    let test_db = TestDatabase::with_migrations().await.unwrap();
    let db = test_db.db.clone();
    let project_service = create_test_services(db.clone(), Arc::new(MockJobQueue::new()))
        .await
        .with_docker_socket_grant(temps_core::docker_socket_grant::DockerSocketGrant::parse(
            Some("node-daemon"),
        ));
    let writer =
        SlugClaimSession::create(&db, temps_auth::Role::User, MfaState::EnrolledStale).await;

    let error = project_service
        .preflight_guard_reserved_slug("node-daemon", &writer.caller())
        .await
        .expect_err("a project writer must not pass the preflight for a granted slug");
    assert!(
        matches!(
            error,
            ProjectError::DockerSocketSlugReserved {
                change: ReservedSlugChange::Claim,
                ..
            }
        ),
        "a non-admin must be refused outright, not challenged: {error:?}"
    );
}

/// The preflight is a sensitive action too: an admin whose MFA is
/// enrolled but stale gets the same 428 step-up challenge the
/// creation-time guard would give, so the console can prompt for
/// re-verification before the caller does anything irreversible.
#[tokio::test]
async fn preflight_guard_reserved_slug_challenges_stale_mfa() {
    if !docker_available().await {
        println!("Docker not available, skipping");
        return;
    }
    let test_db = TestDatabase::with_migrations().await.unwrap();
    let db = test_db.db.clone();
    let project_service = create_test_services(db.clone(), Arc::new(MockJobQueue::new()))
        .await
        .with_docker_socket_grant(temps_core::docker_socket_grant::DockerSocketGrant::parse(
            Some("node-daemon"),
        ));
    let stale =
        SlugClaimSession::create(&db, temps_auth::Role::Admin, MfaState::EnrolledStale).await;

    let error = project_service
        .preflight_guard_reserved_slug("node-daemon", &stale.caller())
        .await
        .expect_err("stale MFA must be challenged before an irreversible side effect");
    assert_step_up_challenge(&error, "claim_docker_socket_slug");
}

/// The success path: a recently-verified admin passes the preflight for
/// the exact slug they plan to create, clearing the way for the
/// irreversible side effect (e.g. the template fork's repository push)
/// to run before the authoritative creation-time guard is reached.
#[tokio::test]
async fn preflight_guard_reserved_slug_allows_a_verified_admin() {
    if !docker_available().await {
        println!("Docker not available, skipping");
        return;
    }
    let test_db = TestDatabase::with_migrations().await.unwrap();
    let db = test_db.db.clone();
    let project_service = create_test_services(db.clone(), Arc::new(MockJobQueue::new()))
        .await
        .with_docker_socket_grant(temps_core::docker_socket_grant::DockerSocketGrant::parse(
            Some("node-daemon"),
        ));
    let verified =
        SlugClaimSession::create(&db, temps_auth::Role::Admin, MfaState::EnrolledVerified).await;

    project_service
        .preflight_guard_reserved_slug("node-daemon", &verified.caller())
        .await
        .expect("a recently-verified admin may preflight a granted slug");
}

/// The preflight is scoped to reserved slugs, exactly like the
/// creation-time guard: an ordinary slug never challenges anyone, so a
/// template fork onto a non-granted name sees no MFA friction.
#[tokio::test]
async fn preflight_guard_reserved_slug_ignores_an_unreserved_slug() {
    if !docker_available().await {
        println!("Docker not available, skipping");
        return;
    }
    let test_db = TestDatabase::with_migrations().await.unwrap();
    let db = test_db.db.clone();
    let project_service = create_test_services(db.clone(), Arc::new(MockJobQueue::new()))
        .await
        .with_docker_socket_grant(temps_core::docker_socket_grant::DockerSocketGrant::parse(
            Some("node-daemon"),
        ));
    let writer =
        SlugClaimSession::create(&db, temps_auth::Role::User, MfaState::EnrolledStale).await;

    project_service
        .preflight_guard_reserved_slug("ordinary-app", &writer.caller())
        .await
        .expect("an unreserved slug is not a sensitive action for anyone");
}

fn create_request(name: &str) -> CreateProjectRequest {
    CreateProjectRequest {
        cloudflare_enabled: None,
        delivery_provider: None,
        name: name.to_string(),
        expected_slug: None,
        repo_name: Some("repo".to_string()),
        repo_owner: Some("owner".to_string()),
        directory: "/".to_string(),
        main_branch: "main".to_string(),
        preset: Preset::Nixpacks.to_string(),
        preset_config: None,
        environment_variables: None,
        git_url: None,
        git_provider_connection_id: None,
        automatic_deploy: false,
        exposed_port: None,
        cpu_request: None,
        cpu_limit: None,
        memory_request: None,
        memory_limit: None,
        is_public_repo: None,
        storage_service_ids: vec![],
        storage_service_claim_ids: vec![],
        storage_service_claim_user_id: None,
        source_type: temps_entities::source_type::SourceType::Git,
        template_slug: None,
        hosting: Default::default(),
    }
}

fn project_for_hosting_lock_test(slug: &str) -> projects::Model {
    serde_json::from_value(serde_json::json!({
        "id": 10, "name": "My project", "slug": slug,
        "repo_name": "repo", "repo_owner": "owner", "directory": "/",
        "main_branch": "main", "preset": "nixpacks", "pull_only_root_directory": false,
        "created_at": chrono::Utc::now(), "updated_at": chrono::Utc::now(),
        "is_deleted": false, "is_public_repo": false, "attack_mode": false, "shield_mode": "log", "ai_write_actions_enabled": false,
        "error_source_context_enabled": false, "vulnerability_scanning_enabled": false,
        "enable_preview_environments": false, "preview_envs_on_demand": false,
        "preview_envs_idle_timeout_seconds": 300, "preview_envs_wake_timeout_seconds": 30,
        "source_type": temps_entities::source_type::SourceType::External,
        "project_type": ProjectType::Server, "cross_project_trace_sharing": true,
        "cloud_telemetry_fidelity": "metered", "cloud_telemetry_attribute_allowlist": [],
        "cloud_telemetry_write_mode": "local", "cloud_analytics_write_mode": "local",
        "hosting": "hosted"
    }))
    .expect("valid project fixture")
}

#[tokio::test]
async fn health_probe_updates_require_admin_on_socket_granted_projects() {
    use sea_orm::{DatabaseBackend, MockDatabase};
    for (slug, caller, allowed) in [
        ("node-daemon", DeployCaller::ProjectWriter, false),
        ("node-daemon", DeployCaller::InstanceAdmin, true),
        ("ordinary-app", DeployCaller::ProjectWriter, true),
    ] {
        let project = project_for_hosting_lock_test(slug);
        let mut updated = project.clone();
        let health = temps_core::repo_config::HealthCheckConfig {
            probe_type: Some("exec".into()),
            command: vec!["check-worker".into()],
            ..Default::default()
        };
        updated.deployment_config = Some(temps_entities::deployment_config::DeploymentConfig {
            health: Some(health.clone()),
            ..Default::default()
        });
        let db = Arc::new(
            MockDatabase::new(DatabaseBackend::Postgres)
                .append_query_results([vec![project], vec![updated]])
                .into_connection(),
        );
        let service = create_test_services(db.clone(), Arc::new(MockJobQueue::new()))
            .await
            .with_docker_socket_grant(temps_core::docker_socket_grant::DockerSocketGrant::parse(
                Some("node-daemon"),
            ));
        let request = serde_json::from_value(serde_json::json!({"health": health})).unwrap();
        let result = service
            .update_project_deployment_config(
                10,
                request,
                temps_core::CeilingEnforcement::Bypass,
                caller,
            )
            .await;
        if allowed {
            assert_eq!(
                result.unwrap().deployment_config.unwrap().health,
                Some(health)
            );
        } else {
            assert!(matches!(
                result,
                Err(ProjectError::DockerSocketWriteRequiresAdmin { .. })
            ));
        }
    }
}

#[tokio::test]
async fn git_settings_merge_startup_overrides_from_locked_row() {
    use sea_orm::{DatabaseBackend, MockDatabase};
    use temps_entities::preset::{ContainerRuntimeOverrides, DockerfileConfig, PresetConfig};
    for preset in [None, Some("dockerfile".to_string())] {
        let mut stale = project_for_hosting_lock_test("startup-test");
        stale.source_type = temps_entities::source_type::SourceType::Git;
        stale.directory = ".".into();
        stale.preset = Preset::Dockerfile;
        stale.preset_config = Some(PresetConfig::Dockerfile(DockerfileConfig::default()));
        let mut current = stale.clone();
        current.preset_config = Some(PresetConfig::Dockerfile(DockerfileConfig {
            runtime: ContainerRuntimeOverrides {
                command: Some(vec!["concurrent-command".into()]),
                entrypoint: Some(vec!["concurrent-entrypoint".into()]),
            },
            ..Default::default()
        }));
        let db = Arc::new(
            MockDatabase::new(DatabaseBackend::Postgres)
                .append_query_results(vec![vec![stale], vec![current.clone()], vec![current]])
                .into_connection(),
        );
        let service = create_test_services(db.clone(), Arc::new(MockJobQueue::new())).await;
        service
            .update_git_settings(
                10,
                None,
                "main".into(),
                "owner".into(),
                "repo".into(),
                preset,
                ".".into(),
                Some(serde_json::json!({"target": "production"})),
                None,
                None,
                None,
                DeployCaller::Platform,
            )
            .await
            .unwrap();
        drop(service);
        let transactions = Arc::try_unwrap(db).unwrap().into_transaction_log();
        let writes = transactions
            .iter()
            .map(|txn| format!("{txn:?}"))
            .filter(|txn| txn.contains("UPDATE"))
            .collect::<Vec<_>>()
            .join("\n");
        assert!(
            writes.contains("concurrent-command"),
            "stale command overwrote locked value: {writes}"
        );
        assert!(
            writes.contains("concurrent-entrypoint"),
            "stale entrypoint overwrote locked value: {writes}"
        );
        assert!(
            writes.contains("production"),
            "incoming patch was not applied: {writes}"
        );
        assert!(
            writes.contains("FOR UPDATE"),
            "merge and write must hold the row lock: {writes}"
        );
    }
}

#[tokio::test]
async fn git_settings_reject_concurrent_conversion_to_service() {
    use sea_orm::{DatabaseBackend, MockDatabase};
    let mut stale = project_for_hosting_lock_test("startup-test");
    stale.source_type = temps_entities::source_type::SourceType::Git;
    stale.directory = ".".into();
    let mut current = stale.clone();
    current.project_type = ProjectType::Service;
    let db = Arc::new(
        MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results(vec![vec![stale], vec![current]])
            .into_connection(),
    );
    let service = create_test_services(db.clone(), Arc::new(MockJobQueue::new())).await;
    let error = service
        .update_git_settings(
            10,
            None,
            "main".into(),
            "owner".into(),
            "repo".into(),
            Some("dockerfile".into()),
            ".".into(),
            None,
            None,
            None,
            None,
            DeployCaller::Platform,
        )
        .await
        .err()
        .expect("a concurrent service conversion must prevent the Git write");
    assert!(
        matches!(error, ProjectError::InvalidInput(message) if message.contains("service project"))
    );
}

#[tokio::test]
async fn hosting_mutations_authorize_the_locked_project_slug() {
    use sea_orm::{DatabaseBackend, MockDatabase};
    for mutation in 0..7 {
        let db = Arc::new(
            MockDatabase::new(DatabaseBackend::Postgres)
                .append_query_results(vec![vec![project_for_hosting_lock_test("ordinary-app")]])
                .append_query_results(vec![vec![project_for_hosting_lock_test("node-daemon")]])
                .into_connection(),
        );
        let queue = Arc::new(MockJobQueue::new());
        let service = create_test_services(db.clone(), queue.clone())
            .await
            .with_docker_socket_grant(temps_core::docker_socket_grant::DockerSocketGrant::parse(
                Some("node-daemon"),
            ));
        let error = match mutation {
            0..=4 => {
                let mut params = UpdateProjectSettingsParams::default();
                match mutation {
                    0 => params.slug = Some("renamed-app".into()),
                    1 => params.git_provider_connection_id = Some(0),
                    2 => params.enable_preview_environments = Some(true),
                    3 => params.main_branch = Some("release".into()),
                    _ => params.preset = Some("nixpacks".into()),
                }
                service
                    .update_project_settings_as(10, params, &SlugClaimCaller::project_writer())
                    .await
                    .err()
            }
            5 => service
                .set_source_type(
                    10,
                    temps_entities::source_type::SourceType::DockerImage,
                    DeployCaller::default(),
                )
                .await
                .err(),
            _ => service
                .update_git_settings(
                    10,
                    None,
                    "main".into(),
                    "owner".into(),
                    "repo".into(),
                    None,
                    "/".into(),
                    None,
                    None,
                    None,
                    None,
                    DeployCaller::default(),
                )
                .await
                .err(),
        }
        .expect("a writer cannot mutate a project now holding host Docker access");
        if mutation == 0 {
            assert!(matches!(
                error,
                ProjectError::DockerSocketSlugReserved {
                    change: ReservedSlugChange::Release,
                    ..
                }
            ));
        } else {
            assert!(
                matches!(error, ProjectError::DockerSocketWriteRequiresAdmin { .. }),
                "mutation {mutation}: {error}"
            );
        }
        assert!(queue.get_jobs().await.is_empty());
        drop(service);
        let log = format!(
            "{:?}",
            Arc::try_unwrap(db)
                .expect("only test owns db")
                .into_transaction_log()
        );
        assert!(
            log.contains("FOR UPDATE"),
            "authorization must check the locked project: {log}"
        );
        assert!(
            !log.contains("UPDATE ") && !log.contains("environment_domains"),
            "authorization failure must precede any write: {log}"
        );
    }
}

#[tokio::test]
async fn external_hosting_activation_rolls_back_domains_and_source_on_failure() {
    if !docker_available().await {
        return;
    }
    let test_db = TestDatabase::with_migrations().await.unwrap();
    let db = test_db.db.clone();
    let queue = Arc::new(MockJobQueue::new());
    let service = create_test_services(db.clone(), queue.clone()).await;
    let mut request = create_request("External rollback");
    request.source_type = temps_entities::source_type::SourceType::External;
    request.repo_name = None;
    request.repo_owner = None;
    request.preset.clear();
    let project = service.create_project(request).await.unwrap();
    let staging = service
        .environment_service
        .create_new_environment(project.id, "staging".into(), "develop".into(), None)
        .await
        .unwrap();
    service
        .environment_service
        .update_environment_subdomain(project.id, staging.id, "fail-managed-staging".into())
        .await
        .unwrap();
    db.execute(Statement::from_string(sea_orm::DatabaseBackend::Postgres,
        "ALTER TABLE environment_domains ADD CONSTRAINT fail_managed_domain CHECK (domain <> 'fail-managed-staging')".to_owned()))
        .await.unwrap();
    let error = service
        .set_source_type(
            project.id,
            temps_entities::source_type::SourceType::DockerImage,
            DeployCaller::default(),
        )
        .await
        .err()
        .expect("failed managed domain registration must abort hosting activation");
    assert!(
        matches!(&error, ProjectError::DatabaseError { reason } if reason.contains(&format!("project {}", project.id)) && reason.contains(&format!("environment {}", staging.id)))
    );
    let stored = projects::Entity::find_by_id(project.id)
        .one(db.as_ref())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        stored.source_type,
        temps_entities::source_type::SourceType::External
    );
    for env in service
        .environment_service
        .get_environments(project.id)
        .await
        .unwrap()
    {
        assert_eq!(
            temps_entities::environment_domains::Entity::find()
                .filter(temps_entities::environment_domains::Column::EnvironmentId.eq(env.id))
                .count(db.as_ref())
                .await
                .unwrap(),
            0,
            "even the earlier environment's domain rolls back"
        );
    }
    assert!(!queue
        .get_jobs()
        .await
        .iter()
        .any(|job| matches!(job, Job::EnvironmentCreated(_))));
}

#[tokio::test]
async fn external_project_creates_telemetry_environment_and_can_enable_hosting() {
    if !docker_available().await {
        return;
    }
    let test_db = TestDatabase::with_migrations().await.unwrap();
    let db = test_db.db.clone();
    let queue = Arc::new(MockJobQueue::new());
    let mut service = create_test_services(db.clone(), queue.clone()).await;
    service.environment_service = Arc::new(
        temps_environments::EnvironmentService::new(db.clone(), service.config_service.clone())
            .with_queue_service(queue.clone()),
    );
    let external_request = || {
        let mut request = create_request("External app");
        request.source_type = temps_entities::source_type::SourceType::External;
        request.repo_name = None;
        request.repo_owner = None;
        request.preset.clear();
        request
    };
    let project = service.create_project(external_request()).await.unwrap();
    assert_eq!(
        project.source_type,
        temps_entities::source_type::SourceType::External
    );
    assert!(project.preset.is_none());
    let envs = environments::Entity::find()
        .filter(environments::Column::ProjectId.eq(project.id))
        .all(db.as_ref())
        .await
        .unwrap();
    assert_eq!(envs.len(), 1);
    assert!(envs[0].current_deployment_id.is_none());
    assert_eq!(
        temps_entities::environment_domains::Entity::find()
            .filter(temps_entities::environment_domains::Column::EnvironmentId.eq(envs[0].id))
            .count(db.as_ref())
            .await
            .unwrap(),
        0
    );
    assert!(!queue
        .get_jobs()
        .await
        .iter()
        .any(|job| matches!(job, Job::EnvironmentCreated(_) | Job::GitPushEvent(_))));
    assert!(matches!(
        service
            .set_source_type(
                project.id,
                temps_entities::source_type::SourceType::External,
                DeployCaller::default()
            )
            .await,
        Err(ProjectError::InvalidInput(_))
    ));
    let staging = service
        .environment_service
        .create_new_environment(project.id, "staging".into(), "develop".into(), None)
        .await
        .unwrap();
    let preview = service
        .environment_service
        .create_environment(
            project.id,
            "preview".into(),
            None,
            None,
            None,
            None,
            "feature".into(),
        )
        .await
        .unwrap();
    let custom = temps_entities::environment_domains::ActiveModel {
        environment_id: Set(staging.id),
        domain: Set("custom.example.com".into()),
        ..Default::default()
    }
    .insert(db.as_ref())
    .await
    .unwrap();
    service
        .environment_service
        .update_environment_subdomain(project.id, staging.id, "external-staging".into())
        .await
        .unwrap();
    service
        .update_project_settings(
            project.id,
            UpdateProjectSettingsParams {
                slug: Some("external-renamed".into()),
                ..Default::default()
            },
        )
        .await
        .unwrap();
    // All three restoration APIs must remove legacy managed rows without
    // reintroducing an endpoint or an automatic health-check job.
    for restore_path in 0..3 {
        let env_id = if restore_path == 0 {
            staging.id
        } else {
            preview.id
        };
        let env = environments::Entity::find_by_id(env_id)
            .one(db.as_ref())
            .await
            .unwrap()
            .unwrap();
        temps_entities::environment_domains::ActiveModel {
            environment_id: Set(env.id),
            domain: Set(env.subdomain.clone()),
            ..Default::default()
        }
        .insert(db.as_ref())
        .await
        .unwrap();
        service
            .environment_service
            .delete_environment(project.id, env.id)
            .await
            .unwrap();
        let restored = match restore_path {
            0 => service
                .environment_service
                .create_new_environment(project.id, "staging".into(), "develop".into(), None)
                .await
                .unwrap(),
            1 => service
                .environment_service
                .create_environment(
                    project.id,
                    "preview".into(),
                    None,
                    None,
                    None,
                    None,
                    "feature".into(),
                )
                .await
                .unwrap(),
            _ => service
                .environment_service
                .get_or_create_environment_for_branch(project.id, "feature")
                .await
                .unwrap(),
        };
        assert_eq!(restored.id, env.id);
        assert!(restored.deleted_at.is_none());
        assert_eq!(
            temps_entities::environment_domains::Entity::find()
                .filter(temps_entities::environment_domains::Column::EnvironmentId.eq(env.id))
                .filter(temps_entities::environment_domains::Column::Domain.eq(&env.subdomain))
                .count(db.as_ref())
                .await
                .unwrap(),
            0
        );
    }
    let custom_after = temps_entities::environment_domains::Entity::find_by_id(custom.id)
        .one(db.as_ref())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(custom_after.domain, "custom.example.com");
    assert!(!queue
        .get_jobs()
        .await
        .iter()
        .any(|job| matches!(job, Job::EnvironmentCreated(_))));
    // Simulate duplicate rows left by earlier versions of External creation.
    let production = environments::Entity::find_by_id(envs[0].id)
        .one(db.as_ref())
        .await
        .unwrap()
        .unwrap();
    for _ in 0..2 {
        temps_entities::environment_domains::ActiveModel {
            environment_id: Set(production.id),
            domain: Set(production.subdomain.clone()),
            ..Default::default()
        }
        .insert(db.as_ref())
        .await
        .unwrap();
    }
    let mut git_request = external_request();
    git_request.name = "External to git".to_string();
    let git_project = service.create_project(git_request).await.unwrap();
    let git_environment = environments::Entity::find()
        .filter(environments::Column::ProjectId.eq(git_project.id))
        .one(db.as_ref())
        .await
        .unwrap()
        .unwrap();
    let git_custom = temps_entities::environment_domains::ActiveModel {
        environment_id: Set(git_environment.id),
        domain: Set("git-custom.example.com".into()),
        ..Default::default()
    }
    .insert(db.as_ref())
    .await
    .unwrap();
    temps_entities::environment_domains::ActiveModel {
        environment_id: Set(git_environment.id),
        domain: Set(git_environment.subdomain.clone()),
        ..Default::default()
    }
    .insert(db.as_ref())
    .await
    .unwrap();
    let git_hosted = service
        .update_git_settings(
            git_project.id,
            None,
            "release".to_string(),
            "example".to_string(),
            "app".to_string(),
            Some("nextjs".to_string()),
            "/".to_string(),
            None,
            Some("https://github.com/example/app.git".to_string()),
            Some(true),
            None,
            DeployCaller::default(),
        )
        .await
        .unwrap();
    assert_eq!(git_hosted.id, git_project.id);
    assert_eq!(
        git_hosted.source_type,
        temps_entities::source_type::SourceType::Git
    );
    let preserved = environments::Entity::find_by_id(git_environment.id)
        .one(db.as_ref())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(preserved.project_id, git_project.id);
    assert_eq!(preserved.branch.as_deref(), Some("release"));
    assert!(
        temps_entities::environment_domains::Entity::find_by_id(git_custom.id)
            .one(db.as_ref())
            .await
            .unwrap()
            .is_some()
    );
    assert_eq!(
        temps_entities::environment_domains::Entity::find()
            .filter(
                temps_entities::environment_domains::Column::EnvironmentId.eq(git_environment.id)
            )
            .count(db.as_ref())
            .await
            .unwrap(),
        2
    );
    let mut request = external_request();
    request.repo_owner = Some("owner".to_string());
    assert!(matches!(
        service.create_project(request).await,
        Err(ProjectError::InvalidInput(_))
    ));
    let hosted = service
        .set_source_type(
            project.id,
            temps_entities::source_type::SourceType::DockerImage,
            DeployCaller::default(),
        )
        .await
        .unwrap();
    assert_eq!(hosted.id, project.id);
    for env_id in [production.id, staging.id, preview.id] {
        let env = environments::Entity::find_by_id(env_id)
            .one(db.as_ref())
            .await
            .unwrap()
            .unwrap();
        assert_eq!(
            temps_entities::environment_domains::Entity::find()
                .filter(temps_entities::environment_domains::Column::EnvironmentId.eq(env_id))
                .filter(temps_entities::environment_domains::Column::Domain.eq(&env.subdomain))
                .count(db.as_ref())
                .await
                .unwrap(),
            1,
            "one managed row per active environment"
        );
    }
    let monitored_envs = || async {
        queue.get_jobs().await.iter().filter(|job| matches!(job, Job::EnvironmentCreated(created) if created.project_id == project.id)).count()
    };
    assert_eq!(monitored_envs().await, 3);
    service
        .set_source_type(
            project.id,
            temps_entities::source_type::SourceType::DockerImage,
            DeployCaller::default(),
        )
        .await
        .unwrap();
    let txn = db.begin().await.unwrap();
    service
        .activate_hosting_domains(&txn, project.id)
        .await
        .unwrap();
    txn.commit().await.unwrap();
    assert_eq!(
        monitored_envs().await,
        3,
        "repeated activation does not enqueue duplicate monitors"
    );
    assert!(
        temps_entities::environment_domains::Entity::find_by_id(custom.id)
            .one(db.as_ref())
            .await
            .unwrap()
            .is_some()
    );
    assert_eq!(
        temps_entities::environment_domains::Entity::find()
            .filter(temps_entities::environment_domains::Column::EnvironmentId.eq(envs[0].id))
            .count(db.as_ref())
            .await
            .unwrap(),
        1
    );
    assert!(matches!(
        service
            .set_source_type(
                project.id,
                temps_entities::source_type::SourceType::External,
                DeployCaller::default()
            )
            .await,
        Err(ProjectError::InvalidInput(_))
    ));
    assert!(matches!(
        service
            .set_source_type(
                -1,
                temps_entities::source_type::SourceType::DockerImage,
                DeployCaller::default()
            )
            .await,
        Err(ProjectError::NotFound(_))
    ));
}

#[test]
fn curated_template_resources_override_project_defaults() {
    let mut request = create_request("Keycloak");
    request.exposed_port = Some(8080);
    request.cpu_request = Some(500_000);
    request.cpu_limit = Some(1_000_000);
    request.memory_request = Some(512);
    request.memory_limit = Some(1_536);

    let config = initial_deployment_config(&request);

    assert_eq!(config.exposed_port, Some(8080));
    assert_eq!(config.cpu_request, Some(500_000));
    assert_eq!(config.cpu_limit, Some(1_000_000));
    assert_eq!(config.memory_request, Some(512));
    assert_eq!(config.memory_limit, Some(1_536));
}

async fn insert_search_test_project(
    db: &temps_database::DbConnection,
    name: &str,
    slug: &str,
) -> temps_entities::projects::Model {
    temps_entities::projects::ActiveModel {
        name: Set(name.to_string()),
        slug: Set(slug.to_string()),
        repo_name: Set(slug.to_string()),
        repo_owner: Set("search-tests".to_string()),
        preset: Set(temps_entities::preset::Preset::NextJs),
        main_branch: Set("main".to_string()),
        directory: Set("/".to_string()),
        ..Default::default()
    }
    .insert(db)
    .await
    .expect("insert search test project")
}

#[tokio::test]
async fn project_search_is_case_insensitive_bounded_and_honors_hidden_projects() {
    if !docker_available().await {
        println!("Docker not available, skipping");
        return;
    }
    let test_db = TestDatabase::with_migrations().await.unwrap();
    let db = test_db.db.clone();
    let service = create_test_services(db.clone(), Arc::new(MockJobQueue::new())).await;

    let alpha = insert_search_test_project(db.as_ref(), "Acme Alpha", "alpha").await;
    let beta = insert_search_test_project(db.as_ref(), "ACME Beta", "beta").await;
    let slug_match = insert_search_test_project(db.as_ref(), "Gamma", "Special-SLUG").await;
    let hidden = insert_search_test_project(db.as_ref(), "Acme Secret", "acme-secret").await;
    insert_search_test_project(db.as_ref(), "Unrelated", "unrelated").await;
    let literal_wildcard =
        insert_search_test_project(db.as_ref(), "Percent%Project", "percent-project").await;

    let (first_page, total) = service
        .get_projects_paginated_excluding_search(1, 1, &[hidden.id], Some("aCmE"))
        .await
        .expect("mixed-case name search");
    let (second_page, second_total) = service
        .get_projects_paginated_excluding_search(2, 1, &[hidden.id], Some("ACME"))
        .await
        .expect("second filtered page");

    assert_eq!(
        total, 2,
        "filtered total must omit hidden and unrelated rows"
    );
    assert_eq!(second_total, 2);
    let visible_ids = [first_page[0].id, second_page[0].id];
    assert!(visible_ids.contains(&alpha.id));
    assert!(visible_ids.contains(&beta.id));
    assert!(!visible_ids.contains(&hidden.id));

    let (slug_results, slug_total) = service
        .get_projects_paginated_excluding_search(1, 25, &[], Some("special-slug"))
        .await
        .expect("mixed-case slug search");
    assert_eq!(slug_total, 1);
    assert_eq!(slug_results[0].id, slug_match.id);

    let (literal_results, literal_total) = service
        .get_projects_paginated_excluding_search(1, 25, &[], Some("%"))
        .await
        .expect("LIKE wildcard characters are treated literally");
    assert_eq!(literal_total, 1);
    assert_eq!(literal_results[0].id, literal_wildcard.id);

    for result in [
        service
            .get_projects_paginated_excluding_search(0, 25, &[], None)
            .await,
        service
            .get_projects_paginated_excluding_search(1, 0, &[], None)
            .await,
        service
            .get_projects_paginated_excluding_search(1, 101, &[], None)
            .await,
        service
            .get_projects_paginated_excluding_search(i64::MAX, 100, &[], None)
            .await,
    ] {
        assert!(matches!(result, Err(ProjectError::InvalidInput(_))));
    }
}

#[tokio::test]
async fn create_project_rejects_a_name_another_active_project_uses() {
    if !docker_available().await {
        println!("Docker not available, skipping");
        return;
    }
    let test_db = TestDatabase::with_migrations().await.unwrap();
    let db = test_db.db.clone();
    let project_service = create_test_services(db.clone(), Arc::new(MockJobQueue::new())).await;

    let _first = project_service
        .create_project(create_request("whoami"))
        .await
        .expect("the first project with a name is accepted");

    // Same name in a different case: still the same project to a reader.
    let error = match project_service
        .create_project(create_request("  WhoAmI "))
        .await
    {
        Ok(project) => panic!("duplicate name was accepted as slug {}", project.slug),
        Err(error) => error,
    };
    assert!(
        matches!(
            error,
            ProjectError::NameAlreadyExists { ref name }
                if name == "WhoAmI"
        ),
        "unexpected error: {error}"
    );
    assert!(
        !error.to_string().contains("slug"),
        "collision must not disclose an inaccessible project identifier: {error}"
    );

    let count = projects::Entity::find()
        .filter(projects::Column::IsDeleted.eq(false))
        .count(db.as_ref())
        .await
        .unwrap();
    assert_eq!(count, 1, "no second project row may be created");
}

#[tokio::test]
async fn create_project_reuses_the_name_of_a_deleted_project() {
    if !docker_available().await {
        println!("Docker not available, skipping");
        return;
    }
    let test_db = TestDatabase::with_migrations().await.unwrap();
    let db = test_db.db.clone();
    let project_service = create_test_services(db.clone(), Arc::new(MockJobQueue::new())).await;

    let deleted = projects::ActiveModel {
        name: Set("whoami".to_string()),
        slug: Set("whoami".to_string()),
        repo_name: Set("repo".to_string()),
        repo_owner: Set("owner".to_string()),
        directory: Set("/".to_string()),
        main_branch: Set("main".to_string()),
        preset: Set(Preset::Nixpacks),
        is_deleted: Set(true),
        deleted_at: Set(Some(chrono::Utc::now())),
        ..Default::default()
    };
    deleted.insert(db.as_ref()).await.unwrap();

    let project = project_service
        .create_project(create_request("whoami"))
        .await
        .expect("a deleted project's name is free to reuse");
    assert_eq!(project.name, "whoami");
    // The deleted project's slug stays reserved.
    assert_ne!(project.slug, "whoami");
    assert!(project.slug.starts_with("whoami-"));
}

#[tokio::test]
async fn legacy_duplicate_names_allow_unrelated_updates_and_unchanged_settings() {
    if !docker_available().await {
        return;
    }
    let test_db = TestDatabase::with_migrations().await.unwrap();
    let service = create_test_services(test_db.db.clone(), Arc::new(MockJobQueue::new())).await;
    let first = service
        .create_project(create_request("legacy"))
        .await
        .unwrap();
    let second = service
        .create_project(create_request("other"))
        .await
        .unwrap();
    // Seed a state permitted before display-name uniqueness enforcement.
    let second = projects::ActiveModel {
        id: Set(second.id),
        name: Set("legacy".to_string()),
        ..Default::default()
    };
    second.update(test_db.db.as_ref()).await.unwrap();
    let update = projects::ActiveModel {
        id: Set(first.id),
        name: Set("legacy".to_string()),
        directory: Set("/changed".to_string()),
        ..Default::default()
    };
    assert_eq!(
        service
            .persist_named_project(update, false)
            .await
            .unwrap()
            .directory,
        "/changed"
    );
    service
        .update_project_settings(
            first.id,
            UpdateProjectSettingsParams {
                name: Some("legacy".to_string()),
                ..Default::default()
            },
        )
        .await
        .unwrap();
    service
        .update_project_settings(
            first.id,
            UpdateProjectSettingsParams {
                name: Some("LEGACY".to_string()),
                ..Default::default()
            },
        )
        .await
        .unwrap();
}

#[tokio::test(flavor = "multi_thread")]
async fn concurrent_project_renames_keep_names_unique() {
    if !docker_available().await {
        return;
    }
    let test_db = TestDatabase::with_migrations().await.unwrap();
    let service = create_test_services(test_db.db.clone(), Arc::new(MockJobQueue::new())).await;
    let a = service
        .create_project(create_request("first"))
        .await
        .unwrap();
    let b = service
        .create_project(create_request("second"))
        .await
        .unwrap();
    // Both callers already loaded and validated their original rows.
    let mut a = projects::ActiveModel {
        id: Set(a.id),
        ..Default::default()
    };
    let mut b = projects::ActiveModel {
        id: Set(b.id),
        ..Default::default()
    };
    a.name = Set("Shared".to_string());
    b.name = Set("shared".to_string());
    let (a, b) = tokio::join!(
        service.persist_named_project(a, false),
        service.persist_named_project(b, false)
    );
    assert_eq!(usize::from(a.is_ok()) + usize::from(b.is_ok()), 1);
    assert!(
        matches!(a, Err(ProjectError::NameAlreadyExists { .. }))
            || matches!(b, Err(ProjectError::NameAlreadyExists { .. }))
    );
}

#[tokio::test]
async fn rename_onto_another_projects_name_is_rejected_but_recasing_is_not() {
    if !docker_available().await {
        println!("Docker not available, skipping");
        return;
    }
    let test_db = TestDatabase::with_migrations().await.unwrap();
    let db = test_db.db.clone();
    let project_service = create_test_services(db.clone(), Arc::new(MockJobQueue::new())).await;

    let _api = project_service
        .create_project(create_request("api"))
        .await
        .expect("create api");
    let web = project_service
        .create_project(create_request("web"))
        .await
        .expect("create web");

    let result = project_service
        .update_project_settings(
            web.id,
            UpdateProjectSettingsParams {
                name: Some("API".to_string()),
                ..Default::default()
            },
        )
        .await;
    assert!(
        matches!(result, Err(ProjectError::NameAlreadyExists { .. })),
        "renaming onto another project's name must be refused"
    );
    let reloaded = projects::Entity::find_by_id(web.id)
        .one(db.as_ref())
        .await
        .unwrap()
        .expect("web still exists");
    assert_eq!(reloaded.name, "web");

    // A project may change the case of its own name.
    let updated = match project_service
        .update_project_settings(
            web.id,
            UpdateProjectSettingsParams {
                name: Some("Web".to_string()),
                ..Default::default()
            },
        )
        .await
    {
        Ok(updated) => updated,
        Err(error) => panic!("re-casing its own name failed: {error}"),
    };
    assert_eq!(updated.project.name, "Web");
}

#[tokio::test]
async fn test_create_project_succeeds_and_creates_default_environment() {
    if !docker_available().await {
        println!("Docker not available, skipping");
        return;
    }
    let test_db = TestDatabase::with_migrations().await.unwrap();
    let db = test_db.db.clone();
    let mock_queue = Arc::new(MockJobQueue::new());
    let project_service = create_test_services(db.clone(), mock_queue.clone()).await;

    let result = project_service
        .create_project(create_request("My Project"))
        .await
        .expect("create_project should succeed");

    assert_eq!(result.name, "My Project");
    assert_eq!(result.slug, "my-project");

    // Default production environment should exist for the new project
    use temps_entities::environments;
    let env_count = environments::Entity::find()
        .filter(environments::Column::ProjectId.eq(result.id))
        .count(db.as_ref())
        .await
        .unwrap();
    assert_eq!(env_count, 1, "should auto-create one environment");

    let created_project = projects::Entity::find_by_id(result.id)
        .one(db.as_ref())
        .await
        .unwrap()
        .expect("created project row should exist");
    let project_config = created_project
        .deployment_config
        .expect("new project should seed deployment_config");
    assert_eq!(project_config.memory_limit, Some(DEFAULT_MEMORY_LIMIT));

    let production = environments::Entity::find()
        .filter(environments::Column::ProjectId.eq(result.id))
        .one(db.as_ref())
        .await
        .unwrap()
        .expect("production environment should exist");
    let env_config = production
        .deployment_config
        .expect("default environment should seed deployment_config");
    assert_eq!(env_config.memory_limit, Some(DEFAULT_MEMORY_LIMIT));
}

#[tokio::test]
async fn test_create_project_applies_resource_profile_to_default_environment() {
    use temps_entities::environments;

    if !docker_available().await {
        println!("Docker not available, skipping");
        return;
    }
    let test_db = TestDatabase::with_migrations().await.unwrap();
    let db = test_db.db.clone();
    let mock_queue = Arc::new(MockJobQueue::new());
    let project_service = create_test_services(db.clone(), mock_queue).await;
    let mut request = create_request("Keycloak Resources");
    request.cpu_request = Some(500_000);
    request.memory_request = Some(512);
    request.memory_limit = Some(1_536);

    let project = project_service
        .create_project(request)
        .await
        .expect("project with curated resources should be created");

    let production = environments::Entity::find()
        .filter(environments::Column::ProjectId.eq(project.id))
        .one(db.as_ref())
        .await
        .unwrap()
        .expect("production environment should exist");
    let config = production
        .deployment_config
        .expect("production should inherit the project's resource profile");

    assert_eq!(config.cpu_request, Some(500_000));
    assert_eq!(config.memory_request, Some(512));
    assert_eq!(config.memory_limit, Some(1_536));
}

#[tokio::test]
async fn test_create_project_persists_curated_template_provenance() {
    if !docker_available().await {
        println!("Docker not available, skipping");
        return;
    }
    let test_db = TestDatabase::with_migrations().await.unwrap();
    let db = test_db.db.clone();
    let mock_queue = Arc::new(MockJobQueue::new());
    let project_service = create_test_services(db.clone(), mock_queue).await;
    let mut request = create_request("Observability Starter");
    request.template_slug = Some("observability-starter".to_string());

    let created = project_service
        .create_project(request)
        .await
        .expect("template project creation should succeed");
    let persisted = projects::Entity::find_by_id(created.id)
        .one(db.as_ref())
        .await
        .expect("template project query should succeed")
        .expect("template project should exist");

    assert_eq!(
        persisted.template_slug.as_deref(),
        Some("observability-starter")
    );
}

#[tokio::test]
async fn test_create_project_rejects_template_slug_longer_than_schema_limit() {
    if !docker_available().await {
        println!("Docker not available, skipping");
        return;
    }
    let test_db = TestDatabase::with_migrations().await.unwrap();
    let db = test_db.db.clone();
    let mock_queue = Arc::new(MockJobQueue::new());
    let project_service = create_test_services(db, mock_queue).await;
    let mut request = create_request("Custom Template");
    request.template_slug = Some("x".repeat(temps_core::templates::MAX_TEMPLATE_SLUG_CHARS + 1));

    let error = match project_service.create_project(request).await {
        Ok(_) => panic!("oversized template slug must be rejected before insertion"),
        Err(error) => error,
    };
    assert!(matches!(
        error,
        ProjectError::InvalidInput(message) if message.contains("255")
    ));
}

#[tokio::test]
async fn test_create_project_nixpacks_node_stores_provider_and_returns_runtime_slug() {
    if !docker_available().await {
        println!("Docker not available, skipping");
        return;
    }
    let test_db = TestDatabase::with_migrations().await.unwrap();
    let db = test_db.db.clone();
    let mock_queue = Arc::new(MockJobQueue::new());
    let project_service = create_test_services(db.clone(), mock_queue.clone()).await;

    let mut request = create_request("Nixpacks Node App");
    request.preset = "nixpacks-node".to_string();

    let result = project_service
        .create_project(request)
        .await
        .expect("create with nixpacks-node should succeed");

    // API/UI surface reconstructs the provider-specific slug
    assert_eq!(result.preset.as_deref(), Some("nixpacks-node"));

    let row = projects::Entity::find_by_id(result.id)
        .one(db.as_ref())
        .await
        .unwrap()
        .expect("project row");

    // Persistable column stays the single Nixpacks enum variant
    assert_eq!(row.preset, Preset::Nixpacks);

    match row.preset_config {
        Some(temps_entities::preset::PresetConfig::Nixpacks(cfg)) => {
            assert_eq!(
                cfg.providers,
                vec![temps_entities::preset::NixpacksProvider::Node]
            );
        }
        other => panic!("expected Nixpacks preset_config with providers=[node], got {other:?}"),
    }
}

#[tokio::test]
async fn test_create_project_rejects_unknown_nixpacks_provider_slug() {
    if !docker_available().await {
        println!("Docker not available, skipping");
        return;
    }
    let test_db = TestDatabase::with_migrations().await.unwrap();
    let db = test_db.db.clone();
    let mock_queue = Arc::new(MockJobQueue::new());
    let project_service = create_test_services(db.clone(), mock_queue.clone()).await;

    let mut request = create_request("Bad Nixpacks");
    request.preset = "nixpacks-not-a-real-provider".to_string();

    match project_service.create_project(request).await {
        Err(ProjectError::InvalidInput(message)) => {
            assert!(message.contains("nixpacks-not-a-real-provider"));
        }
        Err(other) => panic!("expected InvalidInput, got {other:?}"),
        Ok(_) => panic!("unknown nixpacks provider slug must be rejected"),
    }
}

#[tokio::test]
async fn test_update_project_leaving_nixpacks_clears_stale_preset_config() {
    if !docker_available().await {
        println!("Docker not available, skipping");
        return;
    }
    let test_db = TestDatabase::with_migrations().await.unwrap();
    let db = test_db.db.clone();
    let mock_queue = Arc::new(MockJobQueue::new());
    let project_service = create_test_services(db.clone(), mock_queue.clone()).await;

    let mut create = create_request("Leave Nixpacks");
    create.preset = "nixpacks-node".to_string();
    let created = project_service
        .create_project(create)
        .await
        .expect("create nixpacks-node");

    let mut update = create_request("Leave Nixpacks");
    update.preset = "nextjs".to_string();
    let updated = project_service
        .update_project(
            created.id,
            update,
            temps_core::docker_socket_grant::DeployCaller::default(),
        )
        .await
        .expect("switch to nextjs");

    assert_eq!(updated.preset.as_deref(), Some("nextjs"));
    assert!(
        updated.preset_config.is_none(),
        "stale Nixpacks preset_config must be cleared, got {:?}",
        updated.preset_config
    );

    let row = projects::Entity::find_by_id(created.id)
        .one(db.as_ref())
        .await
        .unwrap()
        .expect("project row");
    assert_eq!(row.preset, Preset::NextJs);
    assert!(row.preset_config.is_none());
}

#[tokio::test]
async fn test_partial_preset_config_patch_preserves_nixpacks_providers() {
    if !docker_available().await {
        println!("Docker not available, skipping");
        return;
    }
    let test_db = TestDatabase::with_migrations().await.unwrap();
    let db = test_db.db.clone();
    let mock_queue = Arc::new(MockJobQueue::new());
    let project_service = create_test_services(db.clone(), mock_queue.clone()).await;

    let mut create = create_request("Preserve Provider");
    create.preset = "nixpacks-node".to_string();
    let created = project_service
        .create_project(create)
        .await
        .expect("create nixpacks-node");

    let toml = "[start]\ncmd = \"npm start\"";
    let updated = project_service
        .update_project_settings(
            created.id,
            UpdateProjectSettingsParams {
                preset_config: Some(serde_json::json!({ "nixpacksConfig": toml })),
                ..Default::default()
            },
        )
        .await
        .expect("partial preset_config patch")
        .project;

    assert_eq!(updated.preset.as_deref(), Some("nixpacks-node"));

    let row = projects::Entity::find_by_id(created.id)
        .one(db.as_ref())
        .await
        .unwrap()
        .expect("project row");
    match row.preset_config {
        Some(temps_entities::preset::PresetConfig::Nixpacks(cfg)) => {
            assert_eq!(
                cfg.providers,
                vec![temps_entities::preset::NixpacksProvider::Node]
            );
            assert_eq!(cfg.nixpacks_config.as_deref(), Some(toml));
        }
        other => panic!("expected Nixpacks config with providers=[node], got {other:?}"),
    }
}

#[tokio::test]
async fn test_partial_preset_config_patch_preserves_docker_compose_fields() {
    if !docker_available().await {
        println!("Docker not available, skipping");
        return;
    }
    let test_db = TestDatabase::with_migrations().await.unwrap();
    let db = test_db.db.clone();
    let mock_queue = Arc::new(MockJobQueue::new());
    let project_service = create_test_services(db.clone(), mock_queue.clone()).await;

    let mut create = create_request("Preserve Compose Fields");
    create.preset = "docker-compose".to_string();
    create.preset_config = Some(serde_json::json!({
        "composePath": "compose.yml",
        "composeServices": [
            {"name": "postgres", "image": "postgres:17-alpine", "looksLikeDatabase": true},
            {"name": "hub", "image": "ghcr.io/getpaseo/hub:latest", "looksLikeDatabase": false}
        ]
    }));
    let created = project_service
        .create_project(create)
        .await
        .expect("create docker-compose project");

    // A settings patch must not wipe Compose fields already captured at
    // project creation.
    let updated = project_service
        .update_project_settings(
            created.id,
            UpdateProjectSettingsParams {
                preset_config: Some(serde_json::json!({
                    "excludedServices": ["postgres"]
                })),
                ..Default::default()
            },
        )
        .await
        .expect("partial excludedServices patch")
        .project;

    assert_eq!(updated.preset.as_deref(), Some("docker-compose"));

    let row = projects::Entity::find_by_id(created.id)
        .one(db.as_ref())
        .await
        .unwrap()
        .expect("project row");
    match row.preset_config {
        Some(temps_entities::preset::PresetConfig::DockerCompose(cfg)) => {
            assert_eq!(cfg.compose_path, Some("compose.yml".to_string()));
            assert_eq!(cfg.excluded_services, vec!["postgres".to_string()]);
            assert_eq!(cfg.compose_services.len(), 2);
            assert_eq!(cfg.compose_services[0].name, "postgres");
        }
        other => panic!("expected DockerCompose config, got {other:?}"),
    }

    // A patch touching only relaxedCapabilityServices must not wipe the
    // other DockerCompose fields either — same bug class, new field.
    // "postgres" is still present and looksLikeDatabase in the snapshot
    // at this point, so the server-side database-service check passes.
    let updated = project_service
        .update_project_settings(
            created.id,
            UpdateProjectSettingsParams {
                preset_config: Some(
                    serde_json::json!({ "relaxedCapabilityServices": ["postgres"] }),
                ),
                ..Default::default()
            },
        )
        .await
        .expect("partial relaxedCapabilityServices patch")
        .project;
    assert_eq!(updated.preset.as_deref(), Some("docker-compose"));

    let row = projects::Entity::find_by_id(created.id)
        .one(db.as_ref())
        .await
        .unwrap()
        .expect("project row");
    match row.preset_config {
        Some(temps_entities::preset::PresetConfig::DockerCompose(cfg)) => {
            assert_eq!(
                cfg.relaxed_capability_services,
                vec!["postgres".to_string()]
            );
            // Still preserved from the earlier patch.
            assert_eq!(cfg.compose_services.len(), 2);
            assert_eq!(cfg.excluded_services, vec!["postgres".to_string()]);
        }
        other => panic!("expected DockerCompose config, got {other:?}"),
    }

    // A patch explicitly replacing composeServices to drop "postgres"
    // entirely must still succeed even though it leaves
    // relaxedCapabilityServices pointing at a service that no longer
    // exists in the new snapshot — this patch doesn't touch that field,
    // so the server-side database-service check must not re-run against
    // the now-stale reference and wedge the update.
    let updated = project_service
        .update_project_settings(
            created.id,
            UpdateProjectSettingsParams {
                preset_config: Some(serde_json::json!({
                "composeServices": [
                    {"name": "hub", "image": "ghcr.io/getpaseo/hub:latest", "looksLikeDatabase": false}
                ]
            })),
                ..Default::default()
            },
        )
        .await
        .expect("explicit composeServices patch, even though it strands relaxedCapabilityServices")
        .project;
    assert_eq!(updated.preset.as_deref(), Some("docker-compose"));

    let row = projects::Entity::find_by_id(created.id)
        .one(db.as_ref())
        .await
        .unwrap()
        .expect("project row");
    match row.preset_config {
        Some(temps_entities::preset::PresetConfig::DockerCompose(cfg)) => {
            assert_eq!(cfg.compose_services.len(), 1);
            assert_eq!(cfg.compose_services[0].name, "hub");
            // excludedServices was omitted from this patch too, so it must
            // still survive from the previous update.
            assert_eq!(cfg.excluded_services, vec!["postgres".to_string()]);
            // relaxedCapabilityServices survives too, even though it now
            // references a service absent from the new snapshot.
            assert_eq!(
                cfg.relaxed_capability_services,
                vec!["postgres".to_string()]
            );
        }
        other => panic!("expected DockerCompose config, got {other:?}"),
    }

    // And a subsequent unrelated patch must not wipe (or re-reject)
    // relaxedCapabilityServices either.
    let updated = project_service
        .update_project_settings(
            created.id,
            UpdateProjectSettingsParams {
                preset_config: Some(serde_json::json!({ "excludedServices": [] })),
                ..Default::default()
            },
        )
        .await
        .expect("unrelated excludedServices patch")
        .project;
    assert_eq!(updated.preset.as_deref(), Some("docker-compose"));

    let row = projects::Entity::find_by_id(created.id)
        .one(db.as_ref())
        .await
        .unwrap()
        .expect("project row");
    match row.preset_config {
        Some(temps_entities::preset::PresetConfig::DockerCompose(cfg)) => {
            assert_eq!(
                cfg.relaxed_capability_services,
                vec!["postgres".to_string()]
            );
        }
        other => panic!("expected DockerCompose config, got {other:?}"),
    }
}

#[tokio::test]
async fn test_relaxed_capability_services_allows_non_database_service_via_settings_update() {
    if !docker_available().await {
        println!("Docker not available, skipping");
        return;
    }
    let test_db = TestDatabase::with_migrations().await.unwrap();
    let db = test_db.db.clone();
    let mock_queue = Arc::new(MockJobQueue::new());
    let project_service = create_test_services(db.clone(), mock_queue.clone()).await;

    let mut create = create_request("Allow Non-DB Relax");
    create.preset = "docker-compose".to_string();
    create.preset_config = Some(serde_json::json!({
        "composePath": "compose.yml",
        "composeServices": [
            {"name": "postgres", "image": "postgres:17-alpine", "looksLikeDatabase": true},
            {"name": "gitea", "image": "gitea/gitea:latest", "looksLikeDatabase": false}
        ]
    }));
    let created = project_service
        .create_project(create)
        .await
        .expect("create docker-compose project");

    // "gitea" is a real service in the snapshot and not flagged
    // looksLikeDatabase, but the fix isn't database-specific (confirmed
    // live: Gitea's own official image hits the identical ownership-fix
    // failure at startup) — the toggle must accept any real service.
    let result = project_service
        .update_project_settings(
            created.id,
            UpdateProjectSettingsParams {
                preset_config: Some(serde_json::json!({ "relaxedCapabilityServices": ["gitea"] })),
                ..Default::default()
            },
        )
        .await
        .expect("relaxedCapabilityServices patch for a real non-database service")
        .project;
    assert_eq!(result.preset.as_deref(), Some("docker-compose"));

    let row = projects::Entity::find_by_id(created.id)
        .one(db.as_ref())
        .await
        .unwrap()
        .expect("project row");
    match row.preset_config {
        Some(temps_entities::preset::PresetConfig::DockerCompose(cfg)) => {
            assert_eq!(cfg.relaxed_capability_services, vec!["gitea".to_string()]);
        }
        other => panic!("expected DockerCompose config, got {other:?}"),
    }
}

#[tokio::test]
async fn test_relaxed_capability_services_rejects_phantom_service_via_settings_update() {
    if !docker_available().await {
        println!("Docker not available, skipping");
        return;
    }
    let test_db = TestDatabase::with_migrations().await.unwrap();
    let db = test_db.db.clone();
    let mock_queue = Arc::new(MockJobQueue::new());
    let project_service = create_test_services(db.clone(), mock_queue.clone()).await;

    let mut create = create_request("Reject Phantom Relax");
    create.preset = "docker-compose".to_string();
    create.preset_config = Some(serde_json::json!({
        "composePath": "compose.yml",
        "composeServices": [
            {"name": "postgres", "image": "postgres:17-alpine", "looksLikeDatabase": true}
        ]
    }));
    let created = project_service
        .create_project(create)
        .await
        .expect("create docker-compose project");

    // A name that doesn't correspond to any service in the compose file
    // at all — typo or a fabricated API request — must still be
    // rejected.
    let result = project_service
        .update_project_settings(
            created.id,
            UpdateProjectSettingsParams {
                preset_config: Some(
                    serde_json::json!({ "relaxedCapabilityServices": ["does-not-exist"] }),
                ),
                ..Default::default()
            },
        )
        .await;

    assert!(matches!(result, Err(ProjectError::InvalidInput(_))));

    let row = projects::Entity::find_by_id(created.id)
        .one(db.as_ref())
        .await
        .unwrap()
        .expect("project row");
    match row.preset_config {
        Some(temps_entities::preset::PresetConfig::DockerCompose(cfg)) => {
            assert!(cfg.relaxed_capability_services.is_empty());
        }
        other => panic!("expected DockerCompose config, got {other:?}"),
    }
}

#[tokio::test]
async fn test_explicit_empty_providers_resets_nixpacks_to_auto() {
    if !docker_available().await {
        println!("Docker not available, skipping");
        return;
    }
    let test_db = TestDatabase::with_migrations().await.unwrap();
    let db = test_db.db.clone();
    let mock_queue = Arc::new(MockJobQueue::new());
    let project_service = create_test_services(db.clone(), mock_queue.clone()).await;

    let mut create = create_request("Clear Provider");
    create.preset = "nixpacks-node".to_string();
    let created = project_service
        .create_project(create)
        .await
        .expect("create nixpacks-node");

    let updated = project_service
        .update_project_settings(
            created.id,
            UpdateProjectSettingsParams {
                preset_config: Some(serde_json::json!({ "providers": [] })),
                ..Default::default()
            },
        )
        .await
        .expect("explicit empty providers")
        .project;

    assert_eq!(updated.preset.as_deref(), Some("nixpacks"));

    let row = projects::Entity::find_by_id(created.id)
        .one(db.as_ref())
        .await
        .unwrap()
        .expect("project row");
    match row.preset_config {
        Some(temps_entities::preset::PresetConfig::Nixpacks(cfg)) => {
            assert!(cfg.providers.is_empty());
        }
        other => panic!("expected Nixpacks config without providers, got {other:?}"),
    }
}

#[tokio::test]
async fn test_update_project_base_nixpacks_resets_provider_and_preserves_toml() {
    if !docker_available().await {
        println!("Docker not available, skipping");
        return;
    }
    let test_db = TestDatabase::with_migrations().await.unwrap();
    let db = test_db.db.clone();
    let mock_queue = Arc::new(MockJobQueue::new());
    let project_service = create_test_services(db.clone(), mock_queue).await;

    let toml = "[start]\ncmd = \"npm start\"";
    let mut create = create_request("Reset Through Full Update");
    create.preset = "nixpacks-node".to_string();
    create.preset_config = Some(serde_json::json!({ "nixpacksConfig": toml }));
    let created = project_service
        .create_project(create)
        .await
        .expect("create nixpacks-node project");

    let mut update = create_request("Reset Through Full Update");
    update.preset = "nixpacks".to_string();
    let updated = project_service
        .update_project(
            created.id,
            update,
            temps_core::docker_socket_grant::DeployCaller::default(),
        )
        .await
        .expect("select base nixpacks");

    assert_eq!(updated.preset.as_deref(), Some("nixpacks"));
    let row = projects::Entity::find_by_id(created.id)
        .one(db.as_ref())
        .await
        .unwrap()
        .expect("project row");
    match row.preset_config {
        Some(temps_entities::preset::PresetConfig::Nixpacks(config)) => {
            assert!(config.providers.is_empty());
            assert_eq!(config.nixpacks_config.as_deref(), Some(toml));
        }
        other => panic!("expected base Nixpacks config, got {other:?}"),
    }
}

#[tokio::test]
async fn test_nixpacks_config_rejects_unknown_provider() {
    if !docker_available().await {
        println!("Docker not available, skipping");
        return;
    }
    let test_db = TestDatabase::with_migrations().await.unwrap();
    let db = test_db.db.clone();
    let mock_queue = Arc::new(MockJobQueue::new());
    let project_service = create_test_services(db, mock_queue).await;

    let created = project_service
        .create_project(create_request("Reject Invalid Provider"))
        .await
        .expect("create nixpacks project");

    let result = project_service
        .update_project_settings(
            created.id,
            UpdateProjectSettingsParams {
                preset_config: Some(serde_json::json!({ "providers": ["not-real"] })),
                ..Default::default()
            },
        )
        .await;

    match result {
        Err(ProjectError::InvalidInput(message)) => {
            assert!(message.contains("not-real"));
        }
        Err(other) => panic!("expected InvalidInput, got {other:?}"),
        Ok(_) => panic!("invalid provider must be rejected"),
    }

    let row = projects::Entity::find_by_id(created.id)
        .one(project_service.db.as_ref())
        .await
        .unwrap()
        .expect("project row");
    match row.preset_config {
        Some(temps_entities::preset::PresetConfig::Nixpacks(config)) => {
            assert!(config.providers.is_empty());
        }
        other => panic!("expected unchanged Nixpacks config, got {other:?}"),
    }
}

#[tokio::test]
async fn test_nixpacks_invalid_inline_toml_is_rejected_during_create() {
    if !docker_available().await {
        println!("Docker not available, skipping");
        return;
    }
    let test_db = TestDatabase::with_migrations().await.unwrap();
    let db = test_db.db.clone();
    let mock_queue = Arc::new(MockJobQueue::new());
    let project_service = create_test_services(db, mock_queue).await;

    let mut request = create_request("Invalid Nixpacks TOML");
    request.preset_config = Some(serde_json::json!({
        "nixpacksConfig": "secret_token = [\"do-not-echo\""
    }));
    let result = project_service.create_project(request).await;

    match result {
        Err(ProjectError::InvalidInput(message)) => {
            assert!(message.contains("failed to parse Nixpacks TOML"));
            assert!(
                !message.contains("do-not-echo"),
                "validation errors must not echo inline config contents"
            );
        }
        Err(other) => panic!("expected InvalidInput, got {other:?}"),
        Ok(_) => panic!("invalid Nixpacks TOML must be rejected"),
    }
}

#[tokio::test]
async fn test_nixpacks_invalid_inline_toml_is_rejected_during_config_only_settings_update() {
    if !docker_available().await {
        println!("Docker not available, skipping");
        return;
    }
    let test_db = TestDatabase::with_migrations().await.unwrap();
    let db = test_db.db.clone();
    let mock_queue = Arc::new(MockJobQueue::new());
    let project_service = create_test_services(db.clone(), mock_queue).await;

    let created = project_service
        .create_project(create_request("Invalid Settings TOML"))
        .await
        .expect("create nixpacks project");
    let original_config = projects::Entity::find_by_id(created.id)
        .one(db.as_ref())
        .await
        .unwrap()
        .expect("project row")
        .preset_config;

    let result = project_service
        .update_project_settings(
            created.id,
            UpdateProjectSettingsParams {
                preset_config: Some(serde_json::json!({ "nixpacksConfig": "invalid = [" })),
                ..Default::default()
            },
        )
        .await;

    match result {
        Err(ProjectError::InvalidInput(message)) => {
            assert!(message.contains("failed to parse Nixpacks TOML"));
        }
        Err(other) => panic!("expected InvalidInput, got {other:?}"),
        Ok(_) => panic!("invalid Nixpacks TOML must be rejected"),
    }

    let persisted_config = projects::Entity::find_by_id(created.id)
        .one(db.as_ref())
        .await
        .unwrap()
        .expect("project row")
        .preset_config;
    assert_eq!(persisted_config, original_config);
}

#[tokio::test]
async fn test_nixpacks_invalid_inline_toml_is_rejected_during_config_only_git_update() {
    if !docker_available().await {
        println!("Docker not available, skipping");
        return;
    }
    let test_db = TestDatabase::with_migrations().await.unwrap();
    let db = test_db.db.clone();
    let mock_queue = Arc::new(MockJobQueue::new());
    let project_service = create_test_services(db.clone(), mock_queue).await;

    let created = project_service
        .create_project(create_request("Invalid Git Settings TOML"))
        .await
        .expect("create nixpacks project");
    let original_config = projects::Entity::find_by_id(created.id)
        .one(db.as_ref())
        .await
        .unwrap()
        .expect("project row")
        .preset_config;

    let result = project_service
        .update_git_settings(
            created.id,
            None,
            "main".to_string(),
            "owner".to_string(),
            "repo".to_string(),
            None,
            ".".to_string(),
            Some(serde_json::json!({ "nixpacksConfig": "invalid = [" })),
            None,
            None,
            None,
            DeployCaller::Platform,
        )
        .await;

    match result {
        Err(ProjectError::InvalidInput(message)) => {
            assert!(message.contains("failed to parse Nixpacks TOML"));
        }
        Err(other) => panic!("expected InvalidInput, got {other:?}"),
        Ok(_) => panic!("invalid Nixpacks TOML must be rejected"),
    }

    let persisted_config = projects::Entity::find_by_id(created.id)
        .one(db.as_ref())
        .await
        .unwrap()
        .expect("project row")
        .preset_config;
    assert_eq!(persisted_config, original_config);
}

#[tokio::test]
async fn test_config_only_settings_update_preserves_custom_dockerfile_variant() {
    if !docker_available().await {
        println!("Docker not available, skipping");
        return;
    }
    let test_db = TestDatabase::with_migrations().await.unwrap();
    let db = test_db.db.clone();
    let mock_queue = Arc::new(MockJobQueue::new());
    let project_service = create_test_services(db.clone(), mock_queue).await;

    let mut request = create_request("Custom Settings Variant");
    request.preset = "custom".to_string();
    request.preset_config = Some(serde_json::json!({
        "dockerfilePath": "Dockerfile.custom",
        "buildContext": "."
    }));
    let created = project_service
        .create_project(request)
        .await
        .expect("create custom Dockerfile project");

    project_service
        .update_project_settings(
            created.id,
            UpdateProjectSettingsParams {
                preset_config: Some(serde_json::json!({
                    "dockerfilePath": "Dockerfile.updated"
                })),
                ..Default::default()
            },
        )
        .await
        .expect("update custom Dockerfile config");

    let persisted_config = projects::Entity::find_by_id(created.id)
        .one(db.as_ref())
        .await
        .unwrap()
        .expect("project row")
        .preset_config
        .expect("preset config");
    match &persisted_config {
        temps_entities::preset::PresetConfig::Dockerfile(config) => {
            assert_eq!(
                config.variant,
                temps_entities::preset::DockerfileVariant::Custom
            );
            assert_eq!(
                config.dockerfile_path.as_deref(),
                Some("Dockerfile.updated")
            );
        }
        other => panic!("expected Dockerfile config, got {other:?}"),
    }
    let runtime = temps_presets::get_preset_for_storage(
        temps_entities::preset::Preset::Dockerfile,
        Some(&persisted_config),
    )
    .expect("resolve stored preset")
    .expect("runtime preset");
    assert_eq!(runtime.slug(), "custom");
}

#[tokio::test]
async fn test_config_only_git_update_preserves_custom_dockerfile_variant() {
    if !docker_available().await {
        println!("Docker not available, skipping");
        return;
    }
    let test_db = TestDatabase::with_migrations().await.unwrap();
    let db = test_db.db.clone();
    let mock_queue = Arc::new(MockJobQueue::new());
    let project_service = create_test_services(db.clone(), mock_queue).await;

    let mut request = create_request("Custom Git Variant");
    request.preset = "custom".to_string();
    request.preset_config = Some(serde_json::json!({
        "dockerfilePath": "Dockerfile.custom",
        "buildContext": "."
    }));
    let created = project_service
        .create_project(request)
        .await
        .expect("create custom Dockerfile project");

    project_service
        .update_git_settings(
            created.id,
            None,
            "main".to_string(),
            "owner".to_string(),
            "repo".to_string(),
            None,
            ".".to_string(),
            Some(serde_json::json!({
                "dockerfilePath": "Dockerfile.updated"
            })),
            None,
            None,
            None,
            DeployCaller::Platform,
        )
        .await
        .expect("update custom Dockerfile Git config");

    let persisted_config = projects::Entity::find_by_id(created.id)
        .one(db.as_ref())
        .await
        .unwrap()
        .expect("project row")
        .preset_config
        .expect("preset config");
    match &persisted_config {
        temps_entities::preset::PresetConfig::Dockerfile(config) => {
            assert_eq!(
                config.variant,
                temps_entities::preset::DockerfileVariant::Custom
            );
            assert_eq!(
                config.dockerfile_path.as_deref(),
                Some("Dockerfile.updated")
            );
        }
        other => panic!("expected Dockerfile config, got {other:?}"),
    }
    let runtime = temps_presets::get_preset_for_storage(
        temps_entities::preset::Preset::Dockerfile,
        Some(&persisted_config),
    )
    .expect("resolve stored preset")
    .expect("runtime preset");
    assert_eq!(runtime.slug(), "custom");
}

#[tokio::test]
async fn test_nixpacks_supports_multiple_ordered_providers() {
    if !docker_available().await {
        println!("Docker not available, skipping");
        return;
    }
    let test_db = TestDatabase::with_migrations().await.unwrap();
    let db = test_db.db.clone();
    let mock_queue = Arc::new(MockJobQueue::new());
    let project_service = create_test_services(db.clone(), mock_queue).await;

    let mut request = create_request("Multiple Providers");
    request.preset_config = Some(serde_json::json!({
        "providers": ["...", "python"]
    }));
    let created = project_service
        .create_project(request)
        .await
        .expect("create multi-provider Nixpacks project");

    assert_eq!(created.preset.as_deref(), Some("nixpacks"));
    let row = projects::Entity::find_by_id(created.id)
        .one(db.as_ref())
        .await
        .unwrap()
        .expect("project row");
    match row.preset_config {
        Some(temps_entities::preset::PresetConfig::Nixpacks(config)) => {
            assert_eq!(
                config.providers,
                vec![
                    temps_entities::preset::NixpacksProvider::Auto,
                    temps_entities::preset::NixpacksProvider::Python,
                ]
            );
        }
        other => panic!("expected multi-provider Nixpacks config, got {other:?}"),
    }
}

#[tokio::test]
async fn test_git_settings_persist_multiple_ordered_nixpacks_providers() {
    if !docker_available().await {
        println!("Docker not available, skipping");
        return;
    }
    let test_db = TestDatabase::with_migrations().await.unwrap();
    let db = test_db.db.clone();
    let mock_queue = Arc::new(MockJobQueue::new());
    let project_service = create_test_services(db.clone(), mock_queue).await;

    let created = project_service
        .create_project(create_request("Git Settings Providers"))
        .await
        .expect("create project");
    let updated = project_service
        .update_git_settings(
            created.id,
            None,
            "main".to_string(),
            "owner".to_string(),
            "repo".to_string(),
            Some("nixpacks".to_string()),
            ".".to_string(),
            Some(serde_json::json!({ "providers": ["...", "python"] })),
            None,
            None,
            None,
            DeployCaller::Platform,
        )
        .await
        .expect("update git settings");

    assert_eq!(updated.preset.as_deref(), Some("nixpacks"));
    let row = projects::Entity::find_by_id(created.id)
        .one(db.as_ref())
        .await
        .unwrap()
        .expect("project row");
    match row.preset_config {
        Some(temps_entities::preset::PresetConfig::Nixpacks(config)) => {
            assert_eq!(
                config.providers,
                vec![
                    temps_entities::preset::NixpacksProvider::Auto,
                    temps_entities::preset::NixpacksProvider::Python,
                ]
            );
        }
        other => panic!("expected multi-provider Nixpacks config, got {other:?}"),
    }
}

#[tokio::test]
async fn test_preset_and_config_update_use_effective_new_preset() {
    if !docker_available().await {
        println!("Docker not available, skipping");
        return;
    }
    let test_db = TestDatabase::with_migrations().await.unwrap();
    let db = test_db.db.clone();
    let mock_queue = Arc::new(MockJobQueue::new());
    let project_service = create_test_services(db.clone(), mock_queue).await;

    let mut request = create_request("Atomic Preset Update");
    request.preset = "nextjs".to_string();
    let created = project_service
        .create_project(request)
        .await
        .expect("create nextjs project");

    let toml = "[start]\ncmd = \"npm start\"";
    let updated = project_service
        .update_project_settings(
            created.id,
            UpdateProjectSettingsParams {
                preset: Some("nixpacks-node".to_string()),
                preset_config: Some(serde_json::json!({ "nixpacksConfig": toml })),
                ..Default::default()
            },
        )
        .await
        .expect("update preset and config together")
        .project;

    assert_eq!(updated.preset.as_deref(), Some("nixpacks-node"));
    let row = projects::Entity::find_by_id(created.id)
        .one(db.as_ref())
        .await
        .unwrap()
        .expect("project row");
    match row.preset_config {
        Some(temps_entities::preset::PresetConfig::Nixpacks(config)) => {
            assert_eq!(
                config.providers,
                vec![temps_entities::preset::NixpacksProvider::Node]
            );
            assert_eq!(config.nixpacks_config.as_deref(), Some(toml));
        }
        other => panic!("expected updated Nixpacks config, got {other:?}"),
    }
}

#[tokio::test]
async fn test_reusing_a_deleted_projects_name_gets_suffixed_slug() {
    if !docker_available().await {
        println!("Docker not available, skipping");
        return;
    }
    let test_db = TestDatabase::with_migrations().await.unwrap();
    let db = test_db.db.clone();
    let mock_queue = Arc::new(MockJobQueue::new());
    let project_service = create_test_services(db.clone(), mock_queue.clone()).await;

    let first = project_service
        .create_project(create_request("Duplicate Name"))
        .await
        .expect("first create should succeed");
    // While the first project is active its name is taken...
    assert!(matches!(
        project_service.plan_project_slug("Duplicate Name").await,
        Err(ProjectError::NameAlreadyExists { .. })
    ));
    // ...once deleted the name is free, but its slug stays reserved.
    let mut deleted: projects::ActiveModel = projects::Entity::find_by_id(first.id)
        .one(db.as_ref())
        .await
        .unwrap()
        .expect("first project row")
        .into();
    deleted.is_deleted = Set(true);
    deleted.deleted_at = Set(Some(chrono::Utc::now()));
    deleted.update(db.as_ref()).await.unwrap();

    let planned_slug = project_service
        .plan_project_slug("Duplicate Name")
        .await
        .expect("slug planning should succeed");
    let mut second_request = create_request("Duplicate Name");
    second_request.expected_slug = Some(planned_slug.clone());
    let second = project_service
        .create_project(second_request)
        .await
        .expect("reusing a deleted project's name should succeed with a suffixed slug");

    assert_eq!(first.slug, "duplicate-name");
    assert!(
        second.slug.starts_with("duplicate-name-"),
        "second slug should be suffixed, got {}",
        second.slug
    );
    assert_ne!(first.id, second.id);
    assert_eq!(second.slug, planned_slug);
}

#[tokio::test]
async fn test_create_project_slug_conflict_returns_typed_error() {
    if !docker_available().await {
        println!("Docker not available, skipping");
        return;
    }
    let test_db = TestDatabase::with_migrations().await.unwrap();
    let db = test_db.db.clone();
    let mock_queue = Arc::new(MockJobQueue::new());
    let project_service = create_test_services(db.clone(), mock_queue.clone()).await;

    // Pre-insert a project occupying the slug we're about to ask for, but
    // bypass `generate_unique_project_slug` (which would have suffixed it)
    // by inserting directly. This simulates the race window between the
    // SELECT in `generate_unique_project_slug` and the INSERT below.
    let pre_existing = temps_entities::projects::ActiveModel {
        name: Set("Race".to_string()),
        slug: Set("squatted-slug".to_string()),
        repo_name: Set("r".to_string()),
        repo_owner: Set("o".to_string()),
        directory: Set(".".to_string()),
        git_provider_connection_id: Set(None),
        main_branch: Set("main".to_string()),
        preset: Set(Preset::Nixpacks),
        ..Default::default()
    };
    pre_existing.insert(db.as_ref()).await.unwrap();

    // Now drive `create_project` straight at that slug by patching the
    // ActiveModel. Since we don't have a hook to inject the slug, we
    // instead synthesize the unique-violation by trying to insert a
    // second row with the same slug directly and verifying our
    // detector classifies it as a conflict.
    let dup = temps_entities::projects::ActiveModel {
        name: Set("Race 2".to_string()),
        slug: Set("squatted-slug".to_string()),
        repo_name: Set("r".to_string()),
        repo_owner: Set("o".to_string()),
        directory: Set(".".to_string()),
        git_provider_connection_id: Set(None),
        main_branch: Set("main".to_string()),
        preset: Set(Preset::Nixpacks),
        ..Default::default()
    };
    let err = dup.insert(db.as_ref()).await.unwrap_err();
    assert!(
        super::super::types::is_unique_violation(&err),
        "expected unique-violation classification, got {:?}",
        err
    );
    // And the From<ProjectError> for Problem path should map this branch
    // to 409 once it's wrapped as SlugConflict — exercise the type:
    let project_err = ProjectError::SlugConflict {
        slug: "squatted-slug".to_string(),
    };
    let problem: temps_core::problemdetails::Problem = project_err.into();
    let response = axum::response::IntoResponse::into_response(problem);
    assert_eq!(response.status(), axum::http::StatusCode::CONFLICT);

    // ensure the `_` binding suppresses the unused warning on mock_queue
    let _ = project_service;
}

#[tokio::test]
async fn test_create_project_rolls_back_on_invalid_storage_service() {
    if !docker_available().await {
        println!("Docker not available, skipping");
        return;
    }
    let test_db = TestDatabase::with_migrations().await.unwrap();
    let db = test_db.db.clone();
    let mock_queue = Arc::new(MockJobQueue::new());
    let project_service = create_test_services(db.clone(), mock_queue.clone()).await;

    // Reference a storage_service_id that doesn't exist. The pre-insert
    // verification (`found_count != ids.len()`) returns InvalidInput
    // BEFORE the project insert, so no rollback is needed for this path.
    // To exercise rollback we'd need to fail during a post-insert step,
    // which requires forcing a failure inside finalize. The simplest
    // mid-flight failure is exhausted resources / constraint violations
    // we can't easily inject here without mocking. So this test verifies
    // the early-validation path produces 400 InvalidInput and creates
    // zero projects.
    let req = CreateProjectRequest {
        cloudflare_enabled: None,
        delivery_provider: None,
        storage_service_ids: vec![999_999],
        ..create_request("rollback-test")
    };

    let result = project_service.create_project(req).await;
    match result {
        Ok(_) => panic!("should reject unknown storage service id"),
        Err(ProjectError::InvalidInput(_)) => {}
        Err(other) => panic!("expected InvalidInput, got {:?}", other),
    }

    // No project should have been inserted
    use temps_entities::projects;
    let count = projects::Entity::find()
        .filter(projects::Column::Name.eq("rollback-test"))
        .count(db.as_ref())
        .await
        .unwrap();
    assert_eq!(count, 0, "no project should remain after validation error");
}

#[tokio::test]
async fn test_is_unique_violation_detects_record_not_inserted() {
    // Pure unit test, no DB needed — guards the classifier itself.
    let err = sea_orm::DbErr::RecordNotInserted;
    assert!(super::super::types::is_unique_violation(&err));

    let err = sea_orm::DbErr::Custom("23505: duplicate key".to_string());
    assert!(super::super::types::is_unique_violation(&err));

    let err = sea_orm::DbErr::Custom("connection refused".to_string());
    assert!(!super::super::types::is_unique_violation(&err));
}

// ── Regression test: git provider connections are installation-scoped ───
//
// Connections belong to the installation (workspace-wide), not to the
// user who created them — a GitHub App installation is inherently
// shared, and PAT connections are meant to be usable by any project
// maintainer, not gated to their creator. update_git_settings must not
// reject a connection just because a different user created it; access
// to the project itself is what `permission_guard!`/`project_scope_guard!`
// already enforce in the handler.

#[tokio::test]
async fn test_update_git_settings_allows_connection_created_by_different_user() {
    if !docker_available().await {
        println!("Docker not available, skipping");
        return;
    }
    let test_db = TestDatabase::with_migrations().await.unwrap();
    let db = test_db.db.clone();
    let mock_queue = Arc::new(MockJobQueue::new());
    let project_service = create_test_services(db.clone(), mock_queue.clone()).await;

    use temps_entities::{git_provider_connections, git_providers, users};
    let creator = users::ActiveModel {
        email: Set("git-connection-creator@example.com".to_string()),
        name: Set("Connection Creator".to_string()),
        ..Default::default()
    }
    .insert(db.as_ref())
    .await
    .unwrap();

    // Create a git provider (required FK for connections).
    let provider = git_providers::ActiveModel {
        name: Set("Scoping Test Provider".to_string()),
        provider_type: Set("github".to_string()),
        base_url: Set(None),
        api_url: Set(None),
        auth_method: Set("oauth".to_string()),
        auth_config: Set(serde_json::json!({})),
        webhook_secret: Set(None),
        is_active: Set(true),
        is_default: Set(false),
        ..Default::default()
    }
    .insert(db.as_ref())
    .await
    .unwrap();

    // Connection created by `creator` — a different caller must still be
    // able to attach it to a project they have write access to.
    let connection = git_provider_connections::ActiveModel {
        provider_id: Set(provider.id),
        user_id: Set(Some(creator.id)),
        account_name: Set("creator-account".to_string()),
        account_type: Set("User".to_string()),
        access_token: Set(None),
        refresh_token: Set(None),
        token_expires_at: Set(None),
        refresh_token_expires_at: Set(None),
        installation_id: Set(None),
        metadata: Set(None),
        is_active: Set(true),
        is_expired: Set(false),
        syncing: Set(false),
        last_synced_at: Set(None),
        ..Default::default()
    }
    .insert(db.as_ref())
    .await
    .unwrap();

    let project = temps_entities::projects::ActiveModel {
        name: Set("Scoping Test Project".to_string()),
        slug: Set("scoping-test-project".to_string()),
        repo_name: Set("test-repo".to_string()),
        repo_owner: Set("test-owner".to_string()),
        directory: Set(".".to_string()),
        git_provider_connection_id: Set(None),
        main_branch: Set("main".to_string()),
        preset: Set(Preset::Nixpacks),
        ..Default::default()
    }
    .insert(db.as_ref())
    .await
    .unwrap();

    let result = project_service
        .update_git_settings(
            project.id,
            Some(connection.id),
            "main".to_string(),
            "test-owner".to_string(),
            "test-repo".to_string(),
            None,
            ".".to_string(),
            None,
            None,
            None,
            None,
            DeployCaller::Platform,
        )
        .await;

    // The connection lookup itself must succeed regardless of who
    // created it — GitProviderConnectionNotFound must not fire here.
    // (The call may still fail later, e.g. verifying the branch against
    // a real git host, which this test doesn't stub.)
    assert!(
        !matches!(
            result,
            Err(ProjectError::GitProviderConnectionNotFound { .. })
        ),
        "connection created by a different user was rejected; connections must be installation-scoped, not user-scoped"
    );
}

// ── Root directory checked against the repository (issue #1350) ─────

#[test]
fn a_source_is_rechecked_only_when_its_location_changes() {
    let project = temps_entities::projects::Model {
        git_provider_connection_id: Some(3),
        repo_owner: "example".to_string(),
        repo_name: "monorepo".to_string(),
        main_branch: "main".to_string(),
        // Stored before normalization existed.
        directory: "./apps/api".to_string(),
        log_ingest_lines_per_sec: None,
        log_ingest_kib_per_sec: None,
        ..project_for_hosting_lock_test("directory-check")
    };
    let same = SourceDirectoryTarget {
        connection_id: Some(3),
        is_public_repo: false,
        git_url: None,
        repo_owner: "example",
        repo_name: "monorepo",
        branch: "main",
        directory: "apps/api",
    };
    assert!(!same.differs_from(&project));
    for changed in [
        SourceDirectoryTarget {
            directory: "apps/web",
            ..same
        },
        SourceDirectoryTarget {
            branch: "release",
            ..same
        },
        SourceDirectoryTarget {
            repo_owner: "other",
            ..same
        },
        SourceDirectoryTarget {
            repo_name: "other",
            ..same
        },
        SourceDirectoryTarget {
            connection_id: Some(4),
            ..same
        },
        // A connectionless project moved to a public URL (or between
        // providers) is checked against the new repository.
        SourceDirectoryTarget {
            is_public_repo: true,
            ..same
        },
        SourceDirectoryTarget {
            git_url: Some("https://github.com/example/monorepo"),
            ..same
        },
    ] {
        assert!(changed.differs_from(&project), "{changed:?}");
    }
}

/// A save that points the root directory at a path the repository does
/// not have is refused with the repository, branch and what is there
/// instead; a path that exists saves; an unreachable provider saves too.
#[tokio::test]
async fn root_directory_changes_are_checked_against_the_repository() {
    if !docker_available().await {
        println!("Docker not available, skipping");
        return;
    }
    use temps_entities::{git_provider_connections, git_providers};
    let test_db = TestDatabase::with_migrations().await.unwrap();
    let db = test_db.db.clone();
    let project_service = create_test_services(db.clone(), Arc::new(MockJobQueue::new())).await;

    let mut server = mockito::Server::new_async().await;
    let listing = |entries: &[(&str, &str)]| {
        serde_json::Value::Array(
            entries
                .iter()
                .map(|(name, kind)| serde_json::json!({ "name": name, "path": name, "type": kind }))
                .collect(),
        )
        .to_string()
    };
    let main_ref = || mockito::Matcher::UrlEncoded("ref".into(), "main".into());
    server
        .mock("GET", "/repos/example/monorepo/contents/")
        .match_query(main_ref())
        .with_status(200)
        .with_header("content-type", "application/json")
        .with_body(listing(&[("apps", "dir"), ("README.md", "file")]))
        .create_async()
        .await;
    server
        .mock("GET", "/repos/example/monorepo/contents/apps")
        .match_query(main_ref())
        .with_status(200)
        .with_header("content-type", "application/json")
        .with_body(listing(&[("api", "dir"), ("web", "dir")]))
        .create_async()
        .await;

    let encryption = temps_core::EncryptionService::new(
        "0000000000000000000000000000000000000000000000000000000000000000",
    )
    .unwrap();
    let provider = git_providers::ActiveModel {
        name: Set("Directory Check Provider".to_string()),
        provider_type: Set("github".to_string()),
        base_url: Set(None),
        api_url: Set(Some(server.url())),
        auth_method: Set("pat".to_string()),
        auth_config: Set(serde_json::json!({
            "PersonalAccessToken": { "token": "synthetic-token" }
        })),
        webhook_secret: Set(None),
        is_active: Set(true),
        is_default: Set(false),
        ..Default::default()
    }
    .insert(db.as_ref())
    .await
    .unwrap();
    let connection = git_provider_connections::ActiveModel {
        provider_id: Set(provider.id),
        user_id: Set(None),
        account_name: Set("example".to_string()),
        account_type: Set("User".to_string()),
        access_token: Set(Some(encryption.encrypt_string("synthetic-token").unwrap())),
        refresh_token: Set(None),
        token_expires_at: Set(None),
        refresh_token_expires_at: Set(None),
        installation_id: Set(None),
        metadata: Set(None),
        is_active: Set(true),
        is_expired: Set(false),
        syncing: Set(false),
        last_synced_at: Set(None),
        ..Default::default()
    }
    .insert(db.as_ref())
    .await
    .unwrap();
    let project = temps_entities::projects::ActiveModel {
        name: Set("Directory Check Project".to_string()),
        slug: Set("directory-check-project".to_string()),
        repo_name: Set("monorepo".to_string()),
        repo_owner: Set("example".to_string()),
        directory: Set(".".to_string()),
        git_provider_connection_id: Set(Some(connection.id)),
        main_branch: Set("main".to_string()),
        preset: Set(Preset::Nixpacks),
        ..Default::default()
    }
    .insert(db.as_ref())
    .await
    .unwrap();
    let stored_directory = |db: Arc<temps_database::DbConnection>, id: i32| async move {
        temps_entities::projects::Entity::find_by_id(id)
            .one(db.as_ref())
            .await
            .unwrap()
            .unwrap()
            .directory
    };

    let Err(error) = project_service
        .update_project_settings(
            project.id,
            UpdateProjectSettingsParams {
                directory: Some("apps/apii".to_string()),
                ..Default::default()
            },
        )
        .await
    else {
        panic!("a directory the repository does not have must be refused");
    };
    assert!(
        matches!(error, ProjectError::DirectoryNotInRepository { .. }),
        "{error:?}"
    );
    let message = error.to_string();
    for expected in [
        "'apps/apii'",
        "example/monorepo",
        "branch 'main'",
        "'apps' has no 'apii'; it contains: api, web",
    ] {
        assert!(
            message.contains(expected),
            "{expected} missing from: {message}"
        );
    }
    assert_eq!(stored_directory(db.clone(), project.id).await, ".");

    project_service
        .update_project_settings(
            project.id,
            UpdateProjectSettingsParams {
                directory: Some("apps/api".to_string()),
                ..Default::default()
            },
        )
        .await
        .expect("a directory the repository has is saved");
    assert_eq!(stored_directory(db.clone(), project.id).await, "apps/api");

    // The provider going away must not block saving.
    drop(server);
    project_service
        .update_project_settings(
            project.id,
            UpdateProjectSettingsParams {
                directory: Some("apps/worker".to_string()),
                ..Default::default()
            },
        )
        .await
        .expect("an unreachable provider saves unverified");
    assert_eq!(
        stored_directory(db.clone(), project.id).await,
        "apps/worker"
    );
}

#[tokio::test]
async fn test_update_project_settings_normalizes_blank_directory() {
    // Regression: saving project settings with an empty "Base directory"
    // used to persist "" verbatim, after which every deployment failed with
    // "directory must be a non-empty relative path (got '')".
    let test_db = TestDatabase::with_migrations().await.unwrap();
    let db = test_db.db.clone();
    let mock_queue = Arc::new(MockJobQueue::new());
    let project_service = create_test_services(db.clone(), mock_queue.clone()).await;

    let inserted_project = temps_entities::projects::ActiveModel {
        name: Set("Blank Dir Project".to_string()),
        slug: Set("blank-dir-project".to_string()),
        repo_name: Set("blank-dir-repo".to_string()),
        repo_owner: Set("test-owner".to_string()),
        directory: Set("apps/web".to_string()),
        git_provider_connection_id: Set(None),
        main_branch: Set("main".to_string()),
        preset: Set(Preset::DockerCompose),
        ..Default::default()
    }
    .insert(db.as_ref())
    .await
    .unwrap();

    project_service
        .update_project_settings(
            inserted_project.id,
            UpdateProjectSettingsParams {
                main_branch: Some("main".to_string()),
                directory: Some(String::new()),
                ..Default::default()
            },
        )
        .await
        .expect("update_project_settings should succeed");

    let stored = temps_entities::projects::Entity::find_by_id(inserted_project.id)
        .one(db.as_ref())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        stored.directory, ".",
        "a blank directory must be stored as the repo-root marker, not \"\""
    );
}

/// `ImageRetentionSettings::effective_default_hours` clamps an
/// out-of-range *global* default instead of failing (see
/// `test_out_of_range_operator_setting_is_clamped` in
/// `docker_cleanup_service`). The *per-project* override validated here
/// is a different, intentional behavior: it rejects rather than clamps,
/// because it comes straight from an operator-typed API/CLI/form value
/// rather than a settings row an admin edited once.
#[tokio::test]
async fn test_update_project_settings_rejects_out_of_range_image_retention_hours() {
    if !docker_available().await {
        println!("Docker not available, skipping");
        return;
    }
    let test_db = TestDatabase::with_migrations().await.unwrap();
    let db = test_db.db.clone();
    let mock_queue = Arc::new(MockJobQueue::new());
    let project_service = create_test_services(db.clone(), mock_queue.clone()).await;

    let inserted_project = temps_entities::projects::ActiveModel {
        name: Set("Retention Validation Project".to_string()),
        slug: Set("retention-validation-project".to_string()),
        repo_name: Set("retention-validation-repo".to_string()),
        repo_owner: Set("test-owner".to_string()),
        directory: Set(".".to_string()),
        git_provider_connection_id: Set(None),
        main_branch: Set("main".to_string()),
        preset: Set(Preset::DockerCompose),
        ..Default::default()
    }
    .insert(db.as_ref())
    .await
    .unwrap();

    let update_with_hours = |hours: i32| {
        project_service.update_project_settings(
            inserted_project.id,
            UpdateProjectSettingsParams {
                image_retention_hours: Some(Some(hours)),
                ..Default::default()
            },
        )
    };

    let below_range = update_with_hours(0).await;
    assert!(
        matches!(below_range, Err(ProjectError::InvalidInput(_))),
        "0 hours must be rejected, not clamped"
    );

    let above_range = update_with_hours(8761).await;
    assert!(
        matches!(above_range, Err(ProjectError::InvalidInput(_))),
        "8761 hours must be rejected, not clamped"
    );

    let stored = temps_entities::projects::Entity::find_by_id(inserted_project.id)
        .one(db.as_ref())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        stored.image_retention_hours, None,
        "a rejected update must never reach the database"
    );
}

#[tokio::test]
async fn test_update_project_settings_log_ingest_override_set_lift_reset_and_reject() {
    if !docker_available().await {
        println!("Docker not available, skipping");
        return;
    }
    let test_db = TestDatabase::with_migrations().await.unwrap();
    let db = test_db.db.clone();
    let mock_queue = Arc::new(MockJobQueue::new());
    let project_service = create_test_services(db.clone(), mock_queue.clone()).await;

    let inserted_project = temps_entities::projects::ActiveModel {
        name: Set("Ingest Override Project".to_string()),
        slug: Set("ingest-override-project".to_string()),
        repo_name: Set("ingest-override-repo".to_string()),
        repo_owner: Set("test-owner".to_string()),
        directory: Set(".".to_string()),
        git_provider_connection_id: Set(None),
        main_branch: Set("main".to_string()),
        preset: Set(Preset::DockerCompose),
        ..Default::default()
    }
    .insert(db.as_ref())
    .await
    .unwrap();
    let id = inserted_project.id;
    let stored = || async {
        let p = temps_entities::projects::Entity::find_by_id(id)
            .one(db.as_ref())
            .await
            .unwrap()
            .unwrap();
        (p.log_ingest_lines_per_sec, p.log_ingest_kib_per_sec)
    };

    project_service
        .update_project_settings(
            id,
            UpdateProjectSettingsParams {
                log_ingest_lines_per_sec: Some(Some(2_000)),
                log_ingest_kib_per_sec: Some(Some(0)),
                ..Default::default()
            },
        )
        .await
        .unwrap();
    assert_eq!(
        stored().await,
        (Some(2_000), Some(0)),
        "0 = unlimited is stored"
    );

    // Omitting a key leaves it; an explicit null returns to the default.
    project_service
        .update_project_settings(
            id,
            UpdateProjectSettingsParams {
                log_ingest_lines_per_sec: Some(None),
                ..Default::default()
            },
        )
        .await
        .unwrap();
    assert_eq!(stored().await, (None, Some(0)));

    for (lines, kib) in [(Some(Some(-1)), None), (None, Some(Some(1_048_577)))] {
        let rejected = project_service
            .update_project_settings(
                id,
                UpdateProjectSettingsParams {
                    log_ingest_lines_per_sec: lines,
                    log_ingest_kib_per_sec: kib,
                    ..Default::default()
                },
            )
            .await;
        match rejected {
            Err(ProjectError::InvalidInput(msg)) => {
                assert!(msg.contains(&format!("Project {id}")), "{msg}")
            }
            Err(other) => panic!("expected InvalidInput, got {other:?}"),
            Ok(_) => panic!("out-of-range override must be rejected"),
        }
    }
    assert_eq!(
        stored().await,
        (None, Some(0)),
        "rejected updates never write"
    );
}

#[tokio::test]
async fn test_update_git_settings_normalizes_blank_directory() {
    let test_db = TestDatabase::with_migrations().await.unwrap();
    let db = test_db.db.clone();
    let mock_queue = Arc::new(MockJobQueue::new());
    let project_service = create_test_services(db.clone(), mock_queue.clone()).await;

    let inserted_project = temps_entities::projects::ActiveModel {
        name: Set("Blank Git Dir Project".to_string()),
        slug: Set("blank-git-dir-project".to_string()),
        repo_name: Set("blank-git-dir-repo".to_string()),
        repo_owner: Set("test-owner".to_string()),
        directory: Set("apps/web".to_string()),
        git_provider_connection_id: Set(None),
        main_branch: Set("main".to_string()),
        preset: Set(Preset::DockerCompose),
        ..Default::default()
    }
    .insert(db.as_ref())
    .await
    .unwrap();

    project_service
        .update_git_settings(
            inserted_project.id,
            None,
            "main".to_string(),
            "test-owner".to_string(),
            "blank-git-dir-repo".to_string(),
            None,
            "/".to_string(), // absolute root, equally invalid downstream
            None,
            Some("https://github.com/test-owner/blank-git-dir-repo".to_string()),
            Some(true),
            None,
            DeployCaller::Platform,
        )
        .await
        .expect("update_git_settings should succeed");

    let stored = temps_entities::projects::Entity::find_by_id(inserted_project.id)
        .one(db.as_ref())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(stored.directory, ".");
    assert!(
        !stored.pull_only_root_directory,
        "root directory must clear the sparse-checkout flag"
    );
}

#[tokio::test]
async fn test_update_git_settings_persists_pull_only_root_directory() {
    let test_db = TestDatabase::with_migrations().await.unwrap();
    let db = test_db.db.clone();
    let mock_queue = Arc::new(MockJobQueue::new());
    let project_service = create_test_services(db.clone(), mock_queue.clone()).await;

    let inserted_project = temps_entities::projects::ActiveModel {
        name: Set("Sparse Git Dir Project".to_string()),
        slug: Set("sparse-git-dir-project".to_string()),
        repo_name: Set("sparse-git-dir-repo".to_string()),
        repo_owner: Set("test-owner".to_string()),
        directory: Set(".".to_string()),
        git_provider_connection_id: Set(None),
        main_branch: Set("main".to_string()),
        preset: Set(Preset::DockerCompose),
        ..Default::default()
    }
    .insert(db.as_ref())
    .await
    .unwrap();

    project_service
        .update_git_settings(
            inserted_project.id,
            None,
            "main".to_string(),
            "test-owner".to_string(),
            "sparse-git-dir-repo".to_string(),
            None,
            "apps/web".to_string(),
            None,
            Some("https://github.com/test-owner/sparse-git-dir-repo".to_string()),
            Some(true),
            Some(true),
            DeployCaller::Platform,
        )
        .await
        .expect("update_git_settings should persist the sparse-checkout flag");

    let stored = temps_entities::projects::Entity::find_by_id(inserted_project.id)
        .one(db.as_ref())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(stored.directory, "apps/web");
    assert!(stored.pull_only_root_directory);

    project_service
        .update_git_settings(
            inserted_project.id,
            None,
            "main".to_string(),
            "test-owner".to_string(),
            "sparse-git-dir-repo".to_string(),
            None,
            ".".to_string(),
            None,
            Some("https://github.com/test-owner/sparse-git-dir-repo".to_string()),
            Some(true),
            Some(true),
            DeployCaller::Platform,
        )
        .await
        .expect("resetting directory to root should succeed");

    let stored = temps_entities::projects::Entity::find_by_id(inserted_project.id)
        .one(db.as_ref())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(stored.directory, ".");
    assert!(
        !stored.pull_only_root_directory,
        "checking the box at repository root must not persist"
    );
}

#[tokio::test]
async fn changing_compose_public_ports_requests_a_route_reload() {
    if !docker_available().await {
        println!("Docker not available, skipping");
        return;
    }
    let test_db = TestDatabase::with_migrations().await.unwrap();
    let db = test_db.db.clone();
    let mock_queue = Arc::new(MockJobQueue::new());
    let project_service = create_test_services(db.clone(), mock_queue.clone()).await;

    let inserted_project = temps_entities::projects::ActiveModel {
        name: Set("Compose route reload".to_string()),
        slug: Set("compose-route-reload".to_string()),
        repo_name: Set("repo".to_string()),
        repo_owner: Set("owner".to_string()),
        directory: Set(".".to_string()),
        git_provider_connection_id: Set(None),
        main_branch: Set("main".to_string()),
        preset: Set(Preset::DockerCompose),
        preset_config: Set(Some(temps_entities::preset::PresetConfig::DockerCompose(
            temps_entities::preset::DockerComposeConfig {
                public_ports: vec![temps_entities::preset::ComposePublicPort {
                    service: "web".to_string(),
                    port: 80,
                    ..Default::default()
                }],
                ..Default::default()
            },
        ))),
        ..Default::default()
    }
    .insert(db.as_ref())
    .await
    .unwrap();

    project_service
        .update_git_settings(
            inserted_project.id,
            None,
            "main".to_string(),
            "owner".to_string(),
            "repo".to_string(),
            None,
            ".".to_string(),
            Some(serde_json::json!({
                "publicPorts": [{ "service": "web", "port": 8080 }]
            })),
            None,
            None,
            None,
            DeployCaller::Platform,
        )
        .await
        .expect("compose port save should succeed");

    let jobs = mock_queue.get_jobs().await;
    assert!(jobs.iter().any(|job| matches!(
        job,
        Job::ForceRouteReload(ForceRouteReloadJob {
            environment_id: None,
            deployment_id: None,
        })
    )));
}

/// Assert that the row `id` was free when the code under test returned,
/// before this test's runtime ran anything else.
///
/// `probe`, a `FOR UPDATE NOWAIT` select of that row, runs from another
/// thread on its own connection while this runtime is blocked, as it is in
/// `TestDatabase`'s drop; a transaction left for the pool to roll back
/// still holds its row lock then. On failure this lets the pool roll it
/// back before panicking, so the test reports why instead of hanging in
/// that drop.
async fn assert_row_released(test_db: &TestDatabase, probe: &'static str, id: i32) {
    let Err(reason) = row_lockable_while_blocked(&test_db.database_url, probe, id) else {
        return;
    };
    let _ = tokio::time::timeout(std::time::Duration::from_secs(10), async {
        while test_db
            .db
            .execute(Statement::from_sql_and_values(
                sea_orm::DatabaseBackend::Postgres,
                probe,
                [id.into()],
            ))
            .await
            .is_err()
        {
            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        }
    })
    .await;
    panic!("the row was still locked when the call returned: {reason}");
}

/// Run `probe` for the row `id` on another thread with its own runtime
/// and connection, blocking the caller until it completes.
fn row_lockable_while_blocked(
    database_url: &str,
    probe: &'static str,
    id: i32,
) -> Result<(), String> {
    let database_url = database_url.to_string();
    std::thread::spawn(move || {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .map_err(|error| format!("lock probe runtime: {error}"))?;
        runtime.block_on(async move {
            let connection = sea_orm::Database::connect(&database_url)
                .await
                .map_err(|error| format!("lock probe connection: {error}"))?;
            let locked = connection
                .execute(Statement::from_sql_and_values(
                    sea_orm::DatabaseBackend::Postgres,
                    probe,
                    [id.into()],
                ))
                .await
                .map(|_| ())
                .map_err(|error| format!("{probe} ({id}): {error}"));
            let _ = connection.close().await;
            locked
        })
    })
    .join()
    .map_err(|_| "lock probe thread panicked".to_string())?
}

#[tokio::test]
async fn deletion_refuses_projects_and_environments_with_delivery_bindings() {
    if !docker_available().await {
        println!("Docker not available, skipping");
        return;
    }
    let test_db = TestDatabase::with_migrations()
        .await
        .expect("test database");
    let db = test_db.db.clone();
    let service = create_test_services(db.clone(), Arc::new(MockJobQueue::new())).await;
    let project = temps_entities::projects::ActiveModel {
        name: Set("Delivered Project".to_string()),
        slug: Set("delivered-project".to_string()),
        repo_name: Set("repo".to_string()),
        repo_owner: Set("owner".to_string()),
        preset: Set(Preset::NextJs),
        main_branch: Set("main".to_string()),
        directory: Set("/".to_string()),
        source_type: Set(temps_entities::source_type::SourceType::Git),
        ..Default::default()
    }
    .insert(db.as_ref())
    .await
    .expect("insert project");
    db.execute(Statement::from_sql_and_values(
        sea_orm::DatabaseBackend::Postgres,
        "INSERT INTO environments (name, slug, subdomain, host, upstreams, created_at, updated_at, project_id) VALUES ('staging', 'staging', 'staging', 'staging.example.test', '[]', now(), now(), $1)",
        [project.id.into()],
    ))
    .await
    .expect("insert environment");
    let environment = temps_entities::environments::Entity::find()
        .filter(temps_entities::environments::Column::ProjectId.eq(project.id))
        .one(db.as_ref())
        .await
        .expect("select environment")
        .expect("environment row");
    let provider = dns_providers::ActiveModel {
        name: Set("Manual".into()),
        provider_type: Set("manual".into()),
        credentials: Set("{}".into()),
        is_active: Set(true),
        ..Default::default()
    }
    .insert(db.as_ref())
    .await
    .expect("insert DNS provider");
    let profile = delivery_profiles::ActiveModel {
        name: Set("Direct".into()),
        provider_kind: Set("direct".into()),
        created_at: Set(chrono::Utc::now()),
        updated_at: Set(chrono::Utc::now()),
        ..Default::default()
    }
    .insert(db.as_ref())
    .await
    .expect("insert delivery profile");
    let custom_domain = temps_entities::project_custom_domains::ActiveModel {
        project_id: Set(project.id),
        environment_id: Set(environment.id),
        domain: Set("app.example.test".into()),
        status: Set("active".into()),
        created_at: Set(chrono::Utc::now()),
        updated_at: Set(chrono::Utc::now()),
        ..Default::default()
    }
    .insert(db.as_ref())
    .await
    .expect("insert custom domain");
    let binding = domain_delivery_bindings::ActiveModel {
        hostname: Set("app.example.test".into()),
        project_id: Set(project.id),
        environment_id: Set(environment.id),
        custom_domain_id: Set(custom_domain.id),
        profile_id: Set(profile.id),
        profile_source: Set("project".into()),
        dns_provider_id: Set(provider.id),
        zone: Set("example.test".into()),
        origin_target: Set("192.0.2.42".into()),
        record_type: Set("A".into()),
        proxied: Set(false),
        status: Set("dns_configured".into()),
        created_at: Set(chrono::Utc::now()),
        updated_at: Set(chrono::Utc::now()),
        ..Default::default()
    }
    .insert(db.as_ref())
    .await
    .expect("insert delivery binding");

    let error = service
        .begin_project_deletion(project.id)
        .await
        .expect_err("a project with delivery bindings must not be fenced for deletion");
    assert!(
        matches!(
            &error,
            ProjectError::DeliveryBindingsExist { project_id, binding_count: 1, hostnames, binding_ids }
                if *project_id == project.id
                    && hostnames == &vec!["app.example.test".to_string()]
                    && binding_ids == &vec![binding.id]
        ),
        "unexpected error: {error}"
    );
    let message = error.to_string();
    assert!(
        message.contains(&format!("app.example.test (binding {})", binding.id)),
        "{message}"
    );
    assert!(message.contains("DNS management permissions"), "{message}");
    // The refusal ended its transaction before returning.
    assert_row_released(
        &test_db,
        "SELECT id FROM projects WHERE id = $1 FOR UPDATE NOWAIT",
        project.id,
    )
    .await;
    let unfenced = projects::Entity::find_by_id(project.id)
        .one(db.as_ref())
        .await
        .expect("select project")
        .expect("project row");
    assert!(
        !unfenced.is_deleted,
        "the guard must run before the deletion fence"
    );
    assert!(matches!(
        service.delete_project(project.id, &project.name).await,
        Err(ProjectError::DeliveryBindingsExist { .. })
    ));

    let environment_service =
        temps_environments::EnvironmentService::new(db.clone(), service.config_service.clone());
    let env_error = environment_service
        .delete_environment(project.id, environment.id)
        .await
        .expect_err("an environment with delivery bindings must not be soft-deleted");
    assert!(matches!(
        env_error,
        temps_environments::EnvironmentError::DeliveryBindingsExist { environment_id, .. }
            if environment_id == environment.id
    ));
    assert_row_released(
        &test_db,
        "SELECT id FROM environments WHERE id = $1 FOR UPDATE NOWAIT",
        environment.id,
    )
    .await;
    let problem = temps_core::problemdetails::Problem::from(error);
    assert_eq!(problem.status_code, axum::http::StatusCode::CONFLICT);
}

#[tokio::test]
async fn cloudflare_capability_reports_setup_and_future_default() {
    if !docker_available().await {
        println!("Docker not available, skipping");
        return;
    }
    let test_db = TestDatabase::with_migrations()
        .await
        .expect("test database");
    let db = test_db.db.clone();
    let service = create_test_services(db.clone(), Arc::new(MockJobQueue::new())).await;
    let missing = service
        .cloudflare_project_capability()
        .await
        .expect("capability query");
    assert!(!missing.configured);
    assert!(!missing.default_enabled);
    assert_eq!(missing.setup_path.as_deref(), Some("/dns-providers"));

    let app_settings = temps_core::AppSettings {
        cloudflare_new_projects: true,
        ..Default::default()
    };
    settings::ActiveModel {
        id: Set(1),
        data: Set(app_settings.to_json()),
        created_at: Set(chrono::Utc::now()),
        updated_at: Set(chrono::Utc::now()),
    }
    .insert(db.as_ref())
    .await
    .expect("save future-project default");
    dns_providers::ActiveModel {
        name: Set("Cloudflare".into()),
        provider_type: Set("cloudflare".into()),
        credentials: Set("{}".into()),
        is_active: Set(true),
        ..Default::default()
    }
    .insert(db.as_ref())
    .await
    .expect("save active DNS provider");
    let no_profile = service
        .cloudflare_project_capability()
        .await
        .expect("capability query");
    assert!(!no_profile.configured);
    assert!(no_profile.default_enabled);
    assert_eq!(no_profile.setup_path.as_deref(), Some("/delivery-profiles"));

    delivery_profiles::ActiveModel {
        name: Set("Cloudflare".into()),
        provider_kind: Set("cloudflare".into()),
        created_at: Set(chrono::Utc::now()),
        updated_at: Set(chrono::Utc::now()),
        ..Default::default()
    }
    .insert(db.as_ref())
    .await
    .expect("save delivery profile");
    let ready = service
        .cloudflare_project_capability()
        .await
        .expect("capability query");
    assert!(ready.configured);
    assert!(ready.default_enabled);
    assert!(ready.reason.is_none());

    settings::Entity::update_many()
        .col_expr(
            settings::Column::Data,
            sea_orm::sea_query::Expr::value(
                temps_core::AppSettings {
                    bunny_new_projects: true,
                    ..Default::default()
                }
                .to_json(),
            ),
        )
        .filter(settings::Column::Id.eq(1))
        .exec(db.as_ref())
        .await
        .expect("switch future default to Bunny");
    let no_bunny = service
        .cloudflare_project_capability()
        .await
        .expect("capability query");
    assert!(!no_bunny.bunny_configured);
    assert!(no_bunny.bunny_default_enabled);
    delivery_profiles::ActiveModel {
        name: Set("Bunny".into()),
        provider_kind: Set("bunny".into()),
        bunny_pull_zone_id: Set(Some(42)),
        bunny_hostname: Set(Some("temps-edge.b-cdn.net".into())),
        bunny_api_key_encrypted: Set(Some("encrypted-test-key".into())),
        ..Default::default()
    }
    .insert(db.as_ref())
    .await
    .expect("save Bunny profile");
    let bunny_ready = service
        .cloudflare_project_capability()
        .await
        .expect("capability query");
    assert!(bunny_ready.bunny_configured);
    assert!(bunny_ready.bunny_default_enabled);
    assert!(!bunny_ready.default_enabled);
}

/// An instance default that points at a provider that is not set up must
/// not make project creation fail: importers and the starter project never
/// mention delivery. An explicit request for the same provider still fails.
#[tokio::test]
async fn create_project_degrades_an_unusable_inherited_delivery_default() {
    if !docker_available().await {
        println!("Docker not available, skipping");
        return;
    }
    let test_db = TestDatabase::with_migrations()
        .await
        .expect("test database");
    let db = test_db.db.clone();
    let service = create_test_services(db.clone(), Arc::new(MockJobQueue::new())).await;
    let set_defaults = |cloudflare: bool, bunny: bool| {
        let db = db.clone();
        async move {
            let data = temps_core::AppSettings {
                cloudflare_new_projects: cloudflare,
                bunny_new_projects: bunny,
                ..Default::default()
            }
            .to_json();
            settings::Entity::delete_many()
                .exec(db.as_ref())
                .await
                .expect("clear settings");
            settings::ActiveModel {
                id: Set(1),
                data: Set(data),
                created_at: Set(chrono::Utc::now()),
                updated_at: Set(chrono::Utc::now()),
            }
            .insert(db.as_ref())
            .await
            .expect("save delivery defaults");
        }
    };
    let delivery_profile_of = |project_id: i32| {
        let db = db.clone();
        async move {
            project_delivery_settings::Entity::find_by_id(project_id)
                .one(db.as_ref())
                .await
                .expect("select delivery settings")
                .and_then(|row| row.default_profile_id)
        }
    };

    set_defaults(true, false).await;
    let inherited = service
        .create_project(create_request("Inherited Cloudflare"))
        .await
        .expect("an unusable inherited Cloudflare default must not block creation");
    assert_eq!(delivery_profile_of(inherited.id).await, None);

    let mut explicit = create_request("Explicit Cloudflare");
    explicit.delivery_provider = Some("cloudflare".into());
    match service.create_project(explicit).await {
        Err(ProjectError::InvalidInput(message)) => {
            assert!(message.contains("Cloudflare DNS provider"), "{message}");
            assert!(message.contains("Explicit Cloudflare"), "{message}");
        }
        Err(other) => panic!("expected InvalidInput for explicit Cloudflare, got {other:?}"),
        Ok(project) => panic!(
            "explicit Cloudflare without a provider created project {}",
            project.id
        ),
    }
    let mut legacy_explicit = create_request("Legacy Cloudflare");
    legacy_explicit.cloudflare_enabled = Some(true);
    assert!(matches!(
        service.create_project(legacy_explicit).await,
        Err(ProjectError::InvalidInput(_))
    ));

    set_defaults(false, true).await;
    let inherited_bunny = service
        .create_project(create_request("Inherited Bunny"))
        .await
        .expect("an unusable inherited Bunny default must not block creation");
    assert_eq!(delivery_profile_of(inherited_bunny.id).await, None);
    let mut explicit_bunny = create_request("Explicit Bunny");
    explicit_bunny.delivery_provider = Some("bunny".into());
    match service.create_project(explicit_bunny).await {
        Err(ProjectError::InvalidInput(message)) => {
            assert!(message.contains("Bunny delivery profile"), "{message}");
        }
        Err(other) => panic!("expected InvalidInput for explicit Bunny, got {other:?}"),
        Ok(project) => panic!(
            "explicit Bunny without a profile created project {}",
            project.id
        ),
    }

    // Once Bunny is usable, the legacy Cloudflare opt-out keeps the Bunny
    // default instead of switching delivery off entirely.
    let bunny_profile = delivery_profiles::ActiveModel {
        name: Set("Bunny".into()),
        provider_kind: Set("bunny".into()),
        bunny_pull_zone_id: Set(Some(42)),
        bunny_hostname: Set(Some("edge.example.com".into())),
        bunny_api_key_encrypted: Set(Some("encrypted-test-key".into())),
        ..Default::default()
    }
    .insert(db.as_ref())
    .await
    .expect("save Bunny profile");
    let mut opted_out_of_cloudflare = create_request("Legacy Opt Out");
    opted_out_of_cloudflare.cloudflare_enabled = Some(false);
    let with_bunny = service
        .create_project(opted_out_of_cloudflare)
        .await
        .expect("create with Bunny default");
    assert_eq!(
        delivery_profile_of(with_bunny.id).await,
        Some(bunny_profile.id)
    );
}

// ── GroupFilter enum tests ──────────────────────────────────────────────

#[test]
fn group_filter_ungrouped_and_in_group_are_distinct_variants() {
    // Regression: ensure GroupFilter::Ungrouped and InGroup(id) are
    // distinct and don't accidentally match each other.
    assert!(
        !matches!(GroupFilter::Ungrouped, GroupFilter::InGroup(_)),
        "Ungrouped must not match InGroup"
    );
    assert!(
        !matches!(GroupFilter::InGroup(1), GroupFilter::Ungrouped),
        "InGroup must not match Ungrouped"
    );
    // InGroup preserves the group id.
    assert!(
        matches!(GroupFilter::InGroup(42), GroupFilter::InGroup(42)),
        "InGroup(42) must match InGroup(42)"
    );
}
