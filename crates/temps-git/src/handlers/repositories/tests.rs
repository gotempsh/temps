// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Tests for the repository handlers, kept beside `repositories.rs` so the
//! handler file stays a readable size.

use super::{normalize_commit_sha, CommitExistsResponse, CommitShaValidationError};
use crate::services::git_provider::Commit;

#[test]
fn missing_commit_response_has_no_commit_metadata() {
    let response = CommitExistsResponse::missing();

    assert!(!response.exists);
    assert!(response.commit_sha.is_none());
    assert!(response.commit.is_none());
}

#[test]
fn found_commit_response_includes_provider_metadata() {
    let response = CommitExistsResponse::found(Commit {
        sha: "0123456789abcdef".to_string(),
        message: "Show commit details".to_string(),
        author: "Temps Contributor".to_string(),
        author_email: "contributor@example.com".to_string(),
        date: chrono::DateTime::UNIX_EPOCH,
    });

    assert!(response.exists);
    assert_eq!(response.commit_sha.as_deref(), Some("0123456789abcdef"));
    let commit = response.commit.as_ref();
    assert_eq!(
        commit.map(|value| value.sha.as_str()),
        Some("0123456789abcdef")
    );
    assert_eq!(
        commit.map(|value| value.message.as_str()),
        Some("Show commit details")
    );
    assert_eq!(
        commit.map(|value| value.author.as_str()),
        Some("Temps Contributor")
    );
    assert_eq!(
        commit.map(|value| value.date),
        Some(chrono::DateTime::UNIX_EPOCH)
    );
}

#[test]
fn commit_sha_validation_normalizes_hex_case() {
    assert_eq!(
        normalize_commit_sha("ABCDEF1234567"),
        Ok("abcdef1234567".to_string())
    );
}

#[test]
fn commit_sha_validation_rejects_short_long_and_non_hex_values() {
    assert_eq!(
        normalize_commit_sha("abcdef"),
        Err(CommitShaValidationError::InvalidLength)
    );
    assert_eq!(
        normalize_commit_sha("01234567890123456789012345678901234567890"),
        Err(CommitShaValidationError::InvalidLength)
    );
    assert_eq!(
        normalize_commit_sha("abcdefg"),
        Err(CommitShaValidationError::NonHexadecimal)
    );
    assert_eq!(
        normalize_commit_sha("abcdef/123456"),
        Err(CommitShaValidationError::NonHexadecimal)
    );
}

mod branch_list_response_tests {
    use super::super::BranchListResponse;
    use crate::services::git_provider::{Branch, BranchList};

    fn branch(name: &str) -> Branch {
        Branch {
            name: name.to_string(),
            commit_sha: "a".repeat(40),
            protected: false,
        }
    }

    #[test]
    fn a_capped_listing_tells_the_client_the_list_is_not_the_whole_set() {
        let response = BranchListResponse::from_branch_list(
            BranchList::truncated(vec![branch("topic")]),
            "main",
        );

        assert!(
            response.truncated,
            "a client that cannot see the cap reports existing branches as missing"
        );
        assert!(
            !response.branches[0].is_default,
            "the default branch is whatever the provider says, never the first row"
        );
    }

    #[test]
    fn a_whole_listing_marks_the_default_branch_and_reports_no_truncation() {
        let response = BranchListResponse::from_branch_list(
            BranchList::complete(vec![branch("topic"), branch("main")]),
            "main",
        );

        assert!(!response.truncated);
        assert_eq!(
            response
                .branches
                .iter()
                .filter(|info| info.is_default)
                .map(|info| info.name.as_str())
                .collect::<Vec<_>>(),
            vec!["main"]
        );
    }
}

mod branch_lookup_handler_tests {
    //! The exact single-branch endpoint: what a client may conclude from each
    //! status, and that nothing is concluded before authorization.
    use super::super::*;
    use crate::handlers::public_route_tests::{state, state_over};
    use axum::body::Body;
    use axum::http::Request;
    use axum::routing::get;
    use axum::Router;
    use sea_orm::{DatabaseBackend, MockDatabase};
    use temps_auth::Role;
    use temps_entities::{git_provider_connections, git_providers, repositories, users};
    use tower::ServiceExt;

    /// The test's provider credential, encrypted with the same key
    /// `public_route_tests` gives the state's `EncryptionService`.
    const ENCRYPTION_KEY: &str = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";

    fn router() -> Router<Arc<AppState>> {
        Router::new().route(
            "/repositories/{owner}/{repo}/branch",
            get(get_repository_branch),
        )
    }

    fn test_user() -> users::Model {
        let now = chrono::Utc::now();
        users::Model {
            id: 1,
            name: "Test User".to_string(),
            email: "test@example.com".to_string(),
            password_hash: None,
            email_verified: true,
            email_verification_token: None,
            email_verification_expires: None,
            password_reset_token: None,
            password_reset_expires: None,
            must_change_password: false,
            deleted_at: None,
            mfa_secret: None,
            mfa_enabled: false,
            mfa_recovery_codes: None,
            oidc_subject: None,
            oidc_provider_id: None,
            created_at: now,
            updated_at: now,
        }
    }

    fn repository_row() -> repositories::Model {
        let now = chrono::Utc::now();
        repositories::Model {
            id: 5,
            git_provider_connection_id: 7,
            owner: "temps".to_string(),
            name: "example".to_string(),
            full_name: "temps/example".to_string(),
            description: None,
            private: false,
            fork: false,
            created_at: now,
            updated_at: now,
            pushed_at: now,
            size: 0,
            stargazers_count: 0,
            watchers_count: 0,
            language: None,
            default_branch: "main".to_string(),
            open_issues_count: 0,
            topics: String::new(),
            repo_object: String::new(),
            installation_id: None,
            clone_url: None,
            ssh_url: None,
            preset: None,
        }
    }

    fn connection_row() -> git_provider_connections::Model {
        let now = chrono::Utc::now();
        let encrypted = temps_core::EncryptionService::new(ENCRYPTION_KEY)
            .expect("test encryption key")
            .encrypt_string("provider-token")
            .expect("encrypt the stored credential");
        git_provider_connections::Model {
            id: 7,
            provider_id: 3,
            user_id: Some(1),
            account_name: "temps".to_string(),
            account_type: "Organization".to_string(),
            access_token: Some(encrypted),
            refresh_token: None,
            token_expires_at: None,
            refresh_token_expires_at: None,
            installation_id: None,
            metadata: None,
            is_active: true,
            is_expired: false,
            syncing: false,
            last_synced_at: None,
            synced_repository_count: 0,
            health_status: "healthy".to_string(),
            health_message: None,
            last_health_check_at: None,
            consecutive_health_failures: 0,
            last_sync_error: None,
            last_sync_error_at: None,
            credential_generation: 1,
            token_refresh_lease_id: None,
            token_refresh_lease_expires_at: None,
            created_at: now,
            updated_at: now,
        }
    }

    fn provider_row(api_url: &str) -> git_providers::Model {
        let now = chrono::Utc::now();
        git_providers::Model {
            id: 3,
            name: "GitHub".to_string(),
            provider_type: "github".to_string(),
            base_url: Some("https://github.com".to_string()),
            api_url: Some(api_url.to_string()),
            auth_method: "pat".to_string(),
            auth_config: serde_json::json!({
                "type": "personal_access_token",
                "token": "provider-token"
            }),
            webhook_secret: None,
            is_active: true,
            is_default: true,
            created_at: now,
            updated_at: now,
        }
    }

    /// The rows the handler reads, in the order it reads them: the repository,
    /// the connection, the provider (building the provider service), then the
    /// connection and provider again for the access token.
    fn database(api_url: &str) -> MockDatabase {
        MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results([vec![repository_row()]])
            .append_query_results([vec![connection_row()]])
            .append_query_results([vec![provider_row(api_url)]])
            .append_query_results([vec![connection_row()]])
            .append_query_results([vec![provider_row(api_url)]])
    }

    fn request(auth: Option<AuthContext>) -> Request<Body> {
        let mut builder =
            Request::builder().uri("/repositories/temps/example/branch?connection_id=7&name=main");
        if let Some(auth) = auth {
            builder = builder.extension(auth);
        }
        builder.body(Body::empty()).expect("request builds")
    }

    /// A credential the provider accepts, so `token_needs_refresh` does not
    /// turn a healthy connection into a refresh attempt.
    async fn mock_valid_token(server: &mut mockito::ServerGuard) -> mockito::Mock {
        server
            .mock("GET", "/rate_limit")
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_header("x-ratelimit-remaining", "4999")
            .with_body(r#"{"resources":{"core":{"remaining":4999}}}"#)
            .expect_at_least(1)
            .create_async()
            .await
    }

    #[tokio::test]
    async fn an_unauthenticated_request_is_turned_away_before_any_database_work() {
        let response = router()
            .with_state(state())
            .oneshot(request(None))
            .await
            .expect("router responds");

        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn a_principal_without_the_read_permission_is_refused() {
        let auth = AuthContext::new_api_key(
            test_user(),
            None,
            Some(vec![temps_auth::Permission::ProjectsRead]),
            "scoped-key".to_string(),
            1,
        );

        let response = router()
            .with_state(state())
            .oneshot(request(Some(auth)))
            .await
            .expect("router responds");

        assert_eq!(response.status(), StatusCode::FORBIDDEN);
    }

    #[tokio::test]
    async fn an_empty_branch_name_is_rejected_without_calling_the_provider() {
        let response = router()
            .with_state(state())
            .oneshot(
                Request::builder()
                    .uri("/repositories/temps/example/branch?connection_id=7&name=%20")
                    .extension(AuthContext::new_session(test_user(), Role::Admin))
                    .body(Body::empty())
                    .expect("request builds"),
            )
            .await
            .expect("router responds");

        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn a_branch_the_provider_has_is_returned_with_its_commit_and_default_flag() {
        let mut server = mockito::Server::new_async().await;
        let token = mock_valid_token(&mut server).await;
        let branch = server
            .mock("GET", "/repos/temps/example/branches/main")
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(
                serde_json::json!({
                    "name": "main",
                    "commit": { "sha": "a".repeat(40) },
                    "protected": true
                })
                .to_string(),
            )
            .expect(1)
            .create_async()
            .await;
        // `is_default` comes from the provider's live default branch, as it
        // does for the branch list.
        let repository = server
            .mock("GET", "/repos/temps/example")
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(
                serde_json::json!({
                    "id": 1,
                    "name": "example",
                    "full_name": "temps/example",
                    "private": false,
                    "default_branch": "main",
                    "clone_url": "https://github.com/temps/example.git",
                    "html_url": "https://github.com/temps/example",
                    "owner": { "login": "temps", "id": 1 }
                })
                .to_string(),
            )
            .expect_at_least(1)
            .create_async()
            .await;

        let response = router()
            .with_state(state_over(database(&server.url())))
            .oneshot(request(Some(AuthContext::new_session(
                test_user(),
                Role::Admin,
            ))))
            .await
            .expect("router responds");

        assert_eq!(response.status(), StatusCode::OK);
        let body = axum::body::to_bytes(response.into_body(), 64 * 1024)
            .await
            .expect("read the response body");
        let found: BranchInfo = serde_json::from_slice(&body).expect("a branch DTO");
        assert_eq!(found.name, "main");
        assert_eq!(found.commit_sha, "a".repeat(40));
        assert!(found.protected);
        assert!(found.is_default);

        token.assert_async().await;
        branch.assert_async().await;
        repository.assert_async().await;
    }

    #[tokio::test]
    async fn a_branch_the_provider_does_not_have_is_a_404() {
        let mut server = mockito::Server::new_async().await;
        let token = mock_valid_token(&mut server).await;
        let branch = server
            .mock("GET", "/repos/temps/example/branches/main")
            .with_status(404)
            .with_header("content-type", "application/json")
            .with_body(r#"{"message":"Branch not found"}"#)
            .expect(1)
            .create_async()
            .await;

        let response = router()
            .with_state(state_over(database(&server.url())))
            .oneshot(request(Some(AuthContext::new_session(
                test_user(),
                Role::Admin,
            ))))
            .await
            .expect("router responds");

        assert_eq!(response.status(), StatusCode::NOT_FOUND);
        token.assert_async().await;
        branch.assert_async().await;
    }

    #[tokio::test]
    async fn an_exhausted_budget_is_refused_before_the_token_or_the_provider_is_touched() {
        // No rows and a provider that must not be called: if the handler read
        // the repository or fetched (and so validated) the connection's token
        // before checking the budget, this would fail with a 500 or hit the
        // provider instead of answering 429.
        let mut server = mockito::Server::new_async().await;
        let token = server
            .mock("GET", "/rate_limit")
            .expect(0)
            .create_async()
            .await;
        let branch = server
            .mock("GET", "/repos/temps/example/branches/main")
            .expect(0)
            .create_async()
            .await;
        let state = state_over(MockDatabase::new(DatabaseBackend::Postgres));
        let auth = AuthContext::new_session(test_user(), Role::Admin);
        let principal = provider_lookup_principal(&auth);
        let mut spent = 0;
        while state
            .cache_manager
            .commit_lookup_rate_limiter
            .check(&principal)
            .await
            .is_ok()
        {
            spent += 1;
            assert!(spent <= 10_000, "the lookup budget never ran out");
        }

        let response = router()
            .with_state(state)
            .oneshot(request(Some(auth)))
            .await
            .expect("router responds");

        assert_eq!(response.status(), StatusCode::TOO_MANY_REQUESTS);
        token.assert_async().await;
        branch.assert_async().await;
    }

    #[tokio::test]
    async fn the_branch_name_is_looked_up_exactly_as_given() {
        // Git allows U+00A0 in a ref. `release\u{a0}` is a different branch
        // from `release`, so the provider must be asked for the name as sent.
        let mut server = mockito::Server::new_async().await;
        let token = mock_valid_token(&mut server).await;
        let exact = server
            .mock("GET", "/repos/temps/example/branches/release%C2%A0")
            .with_status(404)
            .with_header("content-type", "application/json")
            .with_body(r#"{"message":"Branch not found"}"#)
            .expect(1)
            .create_async()
            .await;
        let trimmed = server
            .mock("GET", "/repos/temps/example/branches/release")
            .expect(0)
            .create_async()
            .await;

        let response = router()
            .with_state(state_over(database(&server.url())))
            .oneshot(
                Request::builder()
                    .uri("/repositories/temps/example/branch?connection_id=7&name=release%C2%A0")
                    .extension(AuthContext::new_session(test_user(), Role::Admin))
                    .body(Body::empty())
                    .expect("request builds"),
            )
            .await
            .expect("router responds");

        assert_eq!(response.status(), StatusCode::NOT_FOUND);
        token.assert_async().await;
        exact.assert_async().await;
        trimmed.assert_async().await;
    }

    #[tokio::test]
    async fn a_name_of_only_unicode_whitespace_is_asked_of_the_provider() {
        // U+00A0 on its own is a valid ref, so it is looked up, not refused.
        let mut server = mockito::Server::new_async().await;
        let token = mock_valid_token(&mut server).await;
        let exact = server
            .mock("GET", "/repos/temps/example/branches/%C2%A0")
            .with_status(404)
            .with_header("content-type", "application/json")
            .with_body(r#"{"message":"Branch not found"}"#)
            .expect(1)
            .create_async()
            .await;

        let response = router()
            .with_state(state_over(database(&server.url())))
            .oneshot(
                Request::builder()
                    .uri("/repositories/temps/example/branch?connection_id=7&name=%C2%A0")
                    .extension(AuthContext::new_session(test_user(), Role::Admin))
                    .body(Body::empty())
                    .expect("request builds"),
            )
            .await
            .expect("router responds");

        assert_eq!(response.status(), StatusCode::NOT_FOUND);
        token.assert_async().await;
        exact.assert_async().await;
    }
}
