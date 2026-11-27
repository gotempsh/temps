// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

use super::*;

fn install_test_crypto_provider() {
    let _ = rustls::crypto::ring::default_provider().install_default();
}

fn provider(api_url: String) -> GitHubProvider {
    GitHubProvider::new(
        Some(api_url),
        AuthMethod::PersonalAccessToken {
            token: "test-token".to_string(),
        },
    )
}

fn branch_json(name: &str, sha: &str, protected: bool) -> serde_json::Value {
    serde_json::json!({
        "name": name,
        "commit": {
            "sha": sha,
            "url": format!("https://api.github.test/repos/temps/example/commits/{sha}")
        },
        "protected": protected
    })
}

#[tokio::test]
async fn a_listing_cut_short_by_the_cap_reports_itself_truncated() {
    install_test_crypto_provider();
    let mut server = mockito::Server::new_async().await;
    // One oversized page: the cap drops part of it, so the repository
    // certainly holds branches this listing never saw.
    let body: Vec<serde_json::Value> = (0..=MAX_GITHUB_BRANCHES)
        .map(|index| branch_json(&format!("branch-{index}"), &format!("{index:040x}"), false))
        .collect();
    let page = server
        .mock("GET", "/repos/temps/example/branches")
        .match_query(mockito::Matcher::Exact("per_page=100".to_string()))
        .with_status(200)
        .with_header("content-type", "application/json")
        .with_body(serde_json::Value::Array(body).to_string())
        .create_async()
        .await;

    let listed = provider(server.url())
        .list_branches("test-token", "temps", "example")
        .await
        .expect("a capped listing still succeeds");

    assert_eq!(listed.branches.len(), MAX_GITHUB_BRANCHES);
    assert!(
        listed.truncated,
        "a listing stopped by the cap must say branches were left unread"
    );
    page.assert_async().await;
}

#[tokio::test]
async fn a_listing_the_provider_finished_reports_itself_complete() {
    install_test_crypto_provider();
    let mut server = mockito::Server::new_async().await;
    let page = server
        .mock("GET", "/repos/temps/example/branches")
        .match_query(mockito::Matcher::Exact("per_page=100".to_string()))
        .with_status(200)
        .with_header("content-type", "application/json")
        .with_body(
            serde_json::json!([
                branch_json("main", "a".repeat(40).as_str(), true),
                branch_json("topic", "b".repeat(40).as_str(), false)
            ])
            .to_string(),
        )
        .create_async()
        .await;

    let listed = provider(server.url())
        .list_branches("test-token", "temps", "example")
        .await
        .expect("a short page is the whole listing");

    assert_eq!(listed.branches.len(), 2);
    assert!(!listed.truncated);
    page.assert_async().await;
}

#[tokio::test]
async fn get_branch_finds_a_branch_in_one_request() {
    install_test_crypto_provider();
    let mut server = mockito::Server::new_async().await;
    let lookup = server
        .mock("GET", "/repos/temps/example/branches/release%2F2026-01")
        .with_status(200)
        .with_header("content-type", "application/json")
        .with_body(branch_json("release/2026-01", &"c".repeat(40), true).to_string())
        .create_async()
        .await;

    let found = provider(server.url())
        .get_branch("test-token", "temps", "example", "release/2026-01")
        .await
        .expect("the lookup should succeed")
        .expect("the branch exists");

    assert_eq!(found.name, "release/2026-01");
    assert_eq!(found.commit_sha, "c".repeat(40));
    assert!(found.protected);
    lookup.assert_async().await;
}

#[tokio::test]
async fn get_branch_reports_a_404_as_a_missing_branch() {
    install_test_crypto_provider();
    let mut server = mockito::Server::new_async().await;
    let lookup = server
        .mock("GET", "/repos/temps/example/branches/gone")
        .with_status(404)
        .with_header("content-type", "application/json")
        .with_body(r#"{"message":"Branch not found"}"#)
        .create_async()
        .await;

    let found = provider(server.url())
        .get_branch("test-token", "temps", "example", "gone")
        .await
        .expect("a 404 is an answer, not a failure");

    assert!(found.is_none());
    lookup.assert_async().await;
}

#[tokio::test]
async fn get_branch_keeps_a_provider_failure_distinct_from_a_missing_branch() {
    install_test_crypto_provider();
    let mut server = mockito::Server::new_async().await;
    let lookup = server
        .mock("GET", "/repos/temps/example/branches/main")
        .with_status(500)
        .with_header("content-type", "application/json")
        .with_body(r#"{"message":"Server Error"}"#)
        // `send_with_retry` retries a 5xx, so the exact count is not the
        // point here -- only that the failure never became `Ok(None)`.
        .expect_at_least(1)
        .create_async()
        .await;

    let error = provider(server.url())
        .get_branch("test-token", "temps", "example", "main")
        .await
        .expect_err("an unavailable provider must not read as a missing branch");

    assert!(
        !matches!(error, GitProviderError::CommitNotFound { .. }),
        "{error}"
    );
    lookup.assert_async().await;
}
