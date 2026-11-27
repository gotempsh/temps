// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

use super::*;
use crate::services::git_provider_manager::test_support::*;
use crate::services::github_provider::GitHubProvider;
use sea_orm::{DatabaseBackend, MockDatabase};

const OWNER: &str = "example-owner";
const REPO: &str = "example-repository";
const ENCRYPTION_KEY: &str = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";

/// A PAT connection (id 11) to a synced `example-owner/example-repository`,
/// whose GitHub provider answers from `server`.
async fn manager(server: &mockito::ServerGuard) -> GitProviderManager {
    let encryption_service =
        Arc::new(temps_core::EncryptionService::new(ENCRYPTION_KEY).expect("test encryption key"));
    let mut connection = connection_fixture(11, None);
    connection.access_token = Some(
        encryption_service
            .encrypt_string("test-access-token")
            .expect("test access token encrypts"),
    );
    let auth_method = AuthMethod::PersonalAccessToken {
        token: "unused-constructor-token".to_string(),
    };
    let mut provider = github_provider_fixture();
    provider.auth_config = serde_json::to_value(&auth_method).expect("auth method serializes");
    let db = Arc::new(
        MockDatabase::new(DatabaseBackend::Postgres)
            // The synced repository and the connection, then the token
            // check's connection and provider lookups. The provider service
            // itself comes from the cache below.
            .append_query_results([[repository_fixture(11)]])
            .append_query_results([[connection.clone()]])
            .append_query_results([[connection]])
            .append_query_results([[provider]])
            .into_connection(),
    );
    let manager = GitProviderManager::new(
        db.clone(),
        encryption_service,
        Arc::new(MockJobQueue) as Arc<dyn JobQueue>,
        create_test_config_service(db),
        Arc::new(temps_core::NoopAuditLogger),
    );
    let service: Arc<dyn GitProviderService> =
        Arc::new(GitHubProvider::new(Some(server.url()), auth_method));
    manager.providers_cache.write().await.insert(7, service);
    manager
}

async fn mock_valid_token(server: &mut mockito::ServerGuard) -> mockito::Mock {
    server
        .mock("GET", "/rate_limit")
        .with_status(200)
        .with_header("content-type", "application/json")
        .with_header("x-ratelimit-remaining", "4999")
        .with_body(r#"{"resources":{"core":{"remaining":4999}}}"#)
        .create_async()
        .await
}

fn repository_body(default_branch: &str) -> String {
    serde_json::json!({
        "id": 1,
        "name": REPO,
        "full_name": format!("{OWNER}/{REPO}"),
        "owner": { "login": OWNER },
        "description": null,
        "private": true,
        "default_branch": default_branch,
        "clone_url": format!("https://github.com/{OWNER}/{REPO}.git"),
        "ssh_url": format!("git@github.com:{OWNER}/{REPO}.git"),
        "html_url": format!("https://github.com/{OWNER}/{REPO}"),
        "language": null,
        "size": 0,
        "stargazers_count": 0,
        "forks_count": 0,
        "created_at": "2026-01-01T00:00:00Z",
        "updated_at": "2026-01-01T00:00:00Z",
        "pushed_at": null
    })
    .to_string()
}

async fn mock_repository(
    server: &mut mockito::ServerGuard,
    status: usize,
    body: String,
) -> mockito::Mock {
    server
        .mock("GET", format!("/repos/{OWNER}/{REPO}").as_str())
        .with_status(status)
        .with_header("content-type", "application/json")
        .with_body(body)
        .expect(1)
        .create_async()
        .await
}

async fn mock_branch(
    server: &mut mockito::ServerGuard,
    name: &str,
    status: usize,
    body: String,
) -> mockito::Mock {
    server
        .mock(
            "GET",
            format!("/repos/{OWNER}/{REPO}/branches/{name}").as_str(),
        )
        .with_status(status)
        .with_header("content-type", "application/json")
        .with_body(body)
        .expect_at_least(1)
        .create_async()
        .await
}

#[tokio::test]
async fn a_found_branch_is_marked_default_from_the_live_repository() {
    let mut server = mockito::Server::new_async().await;
    let _token = mock_valid_token(&mut server).await;
    let branch = mock_branch(
        &mut server,
        "trunk",
        200,
        serde_json::json!({ "name": "trunk", "commit": { "sha": "a".repeat(40) } }).to_string(),
    )
    .await;
    // The synced copy says `main`; the provider's live answer wins.
    let repository = mock_repository(&mut server, 200, repository_body("trunk")).await;

    let outcome = manager(&server)
        .await
        .look_up_branch(11, OWNER, REPO, "trunk")
        .await
        .expect("the lookup answers");

    let BranchLookupOutcome::Found {
        branch: found,
        is_default,
    } = outcome
    else {
        panic!("expected the branch to be found, got {outcome:?}");
    };
    assert_eq!(found.name, "trunk");
    assert_eq!(found.commit_sha, "a".repeat(40));
    assert!(is_default);
    branch.assert_async().await;
    repository.assert_async().await;
}

#[tokio::test]
async fn a_found_branch_falls_back_to_the_synced_default_when_the_repository_cannot_be_read() {
    let mut server = mockito::Server::new_async().await;
    let _token = mock_valid_token(&mut server).await;
    let _branch = mock_branch(
        &mut server,
        "main",
        200,
        serde_json::json!({ "name": "main", "commit": { "sha": "b".repeat(40) } }).to_string(),
    )
    .await;
    let _repository =
        mock_repository(&mut server, 404, r#"{"message":"Not Found"}"#.to_string()).await;

    let outcome = manager(&server)
        .await
        .look_up_branch(11, OWNER, REPO, "main")
        .await
        .expect("a found branch is an answer however the default is read");

    assert!(
        matches!(
            outcome,
            BranchLookupOutcome::Found {
                is_default: true,
                ..
            }
        ),
        "the synced default `main` applies: {outcome:?}"
    );
}

#[tokio::test]
async fn a_branch_404_in_a_readable_repository_is_missing() {
    let mut server = mockito::Server::new_async().await;
    let _token = mock_valid_token(&mut server).await;
    let branch = mock_branch(
        &mut server,
        "gone",
        404,
        r#"{"message":"Branch not found"}"#.to_string(),
    )
    .await;
    let repository = mock_repository(&mut server, 200, repository_body("main")).await;

    let outcome = manager(&server)
        .await
        .look_up_branch(11, OWNER, REPO, "gone")
        .await
        .expect("the lookup answers");

    assert!(
        matches!(outcome, BranchLookupOutcome::Missing),
        "{outcome:?}"
    );
    branch.assert_async().await;
    repository.assert_async().await;
}

#[tokio::test]
async fn a_branch_404_in_a_repository_the_connection_cannot_read_is_not_missing() {
    let mut server = mockito::Server::new_async().await;
    let _token = mock_valid_token(&mut server).await;
    let _branch = mock_branch(
        &mut server,
        "main",
        404,
        r#"{"message":"Not Found"}"#.to_string(),
    )
    .await;
    let repository =
        mock_repository(&mut server, 404, r#"{"message":"Not Found"}"#.to_string()).await;

    let error = manager(&server)
        .await
        .look_up_branch(11, OWNER, REPO, "main")
        .await
        .expect_err("an unreadable repository settles nothing about its branch");

    let BranchLookupError::Provider {
        step,
        connection_id,
        ref branch,
        ..
    } = error
    else {
        panic!("expected a provider failure, got {error:?}");
    };
    assert_eq!(step, BranchLookupStep::ConfirmRepository);
    assert_eq!(connection_id, 11);
    assert_eq!(branch, "main");
    assert!(
        error
            .to_string()
            .contains("example-owner/example-repository"),
        "{error}"
    );
    repository.assert_async().await;
}

#[tokio::test]
async fn a_failed_branch_read_names_the_branch_and_the_step() {
    let mut server = mockito::Server::new_async().await;
    let _token = mock_valid_token(&mut server).await;
    // `send_with_retry` retries a 5xx, so the count is not the point.
    let _branch = mock_branch(
        &mut server,
        "main",
        500,
        r#"{"message":"boom"}"#.to_string(),
    )
    .await;

    let error = manager(&server)
        .await
        .look_up_branch(11, OWNER, REPO, "main")
        .await
        .expect_err("a provider failure is not an answer");

    assert!(
        matches!(
            error,
            BranchLookupError::Provider {
                step: BranchLookupStep::ReadBranch,
                ..
            }
        ),
        "{error:?}"
    );
    assert!(error.to_string().contains("'main'"), "{error}");
}

#[tokio::test]
async fn a_repository_that_is_not_synced_fails_before_any_provider_request() {
    let server = mockito::Server::new_async().await;
    let db = Arc::new(
        MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results([Vec::<repositories::Model>::new()])
            .into_connection(),
    );
    let manager = GitProviderManager::new(
        db.clone(),
        Arc::new(temps_core::EncryptionService::new(ENCRYPTION_KEY).expect("test key")),
        Arc::new(MockJobQueue) as Arc<dyn JobQueue>,
        create_test_config_service(db),
        Arc::new(temps_core::NoopAuditLogger),
    );

    let error = manager
        .look_up_branch(11, OWNER, REPO, "main")
        .await
        .expect_err("an unsynced repository cannot be looked up");

    let BranchLookupError::Connection {
        connection_id,
        ref source,
        ..
    } = error
    else {
        panic!("expected a connection failure, got {error:?}");
    };
    assert_eq!(connection_id, 11);
    assert!(
        matches!(**source, GitProviderManagerError::RepositoryNotFound(_)),
        "{source:?}"
    );
    assert!(error.to_string().contains("connection 11"), "{error}");
    drop(server);
}
