// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! HTTP-level tests for Bitbucket's branch listing and exact branch lookup,
//! against a mock server standing in for `api.bitbucket.org/2.0`.

use super::*;
use mockito::Matcher;

fn provider(server: &mockito::ServerGuard) -> BitbucketProvider {
    BitbucketProvider::with_api_base(
        AuthMethod::PersonalAccessToken {
            token: "test-token".to_string(),
        },
        server.url(),
    )
}

fn branch_json(name: &str, hash: &str) -> serde_json::Value {
    serde_json::json!({ "name": name, "target": { "hash": hash } })
}

/// One page of `size` branches named `{prefix}-{n}`, with a `next` link when
/// the server says more pages follow.
fn page(prefix: &str, size: usize, next: bool) -> String {
    let values: Vec<_> = (0..size)
        .map(|n| branch_json(&format!("{prefix}-{n}"), &"a".repeat(40)))
        .collect();
    let mut body = serde_json::json!({ "values": values });
    if next {
        body["next"] = serde_json::json!("https://api.bitbucket.org/2.0/next-page");
    }
    body.to_string()
}

#[tokio::test]
async fn get_branch_finds_a_branch_in_one_request_with_its_name_encoded() {
    let mut server = mockito::Server::new_async().await;
    let lookup = server
        .mock(
            "GET",
            "/repositories/temps/example/refs/branches/release%2F2026-01",
        )
        .with_status(200)
        .with_header("content-type", "application/json")
        .with_body(branch_json("release/2026-01", &"b".repeat(40)).to_string())
        .expect(1)
        .create_async()
        .await;

    let found = provider(&server)
        .get_branch("unused", "temps", "example", "release/2026-01")
        .await
        .expect("lookup succeeds")
        .expect("branch exists");

    assert_eq!(found.name, "release/2026-01");
    assert_eq!(found.commit_sha, "b".repeat(40));
    lookup.assert_async().await;
}

#[tokio::test]
async fn get_branch_reports_a_404_as_a_missing_branch() {
    let mut server = mockito::Server::new_async().await;
    let lookup = server
        .mock("GET", "/repositories/temps/example/refs/branches/gone")
        .with_status(404)
        .with_header("content-type", "application/json")
        .with_body(r#"{"type":"error","error":{"message":"Branch not found"}}"#)
        .expect(1)
        .create_async()
        .await;

    let found = provider(&server)
        .get_branch("unused", "temps", "example", "gone")
        .await
        .expect("a 404 is an answer, not a failure");

    assert!(found.is_none());
    lookup.assert_async().await;
}

#[tokio::test]
async fn get_branch_keeps_a_provider_failure_distinct_from_a_missing_branch() {
    let mut server = mockito::Server::new_async().await;
    let lookup = server
        .mock("GET", "/repositories/temps/example/refs/branches/main")
        .with_status(403)
        .with_body(r#"{"type":"error","error":{"message":"Forbidden"}}"#)
        .expect(1)
        .create_async()
        .await;

    let result = provider(&server)
        .get_branch("unused", "temps", "example", "main")
        .await;

    match result {
        Err(GitProviderError::ApiError(message)) => {
            assert!(message.contains("main"), "names the branch: {message}");
            assert!(
                message.contains("temps/example"),
                "names the repo: {message}"
            );
        }
        other => panic!("a 403 must stay an error, got {other:?}"),
    }
    lookup.assert_async().await;
}

#[tokio::test]
async fn a_listing_the_provider_finished_reports_itself_complete() {
    let mut server = mockito::Server::new_async().await;
    let listing = server
        .mock("GET", "/repositories/temps/example/refs/branches")
        .match_query(Matcher::Any)
        .with_status(200)
        .with_header("content-type", "application/json")
        .with_body(page("topic", 3, false))
        .expect(1)
        .create_async()
        .await;

    let list = provider(&server)
        .list_branches("unused", "temps", "example")
        .await
        .expect("listing succeeds");

    assert!(!list.truncated);
    assert_eq!(list.branches.len(), 3);
    listing.assert_async().await;
}

#[tokio::test]
async fn a_listing_cut_short_by_the_cap_reports_itself_truncated() {
    // Every page says another follows, so only the cap ends the listing.
    let mut server = mockito::Server::new_async().await;
    let listing = server
        .mock("GET", "/repositories/temps/example/refs/branches")
        .match_query(Matcher::Any)
        .with_status(200)
        .with_header("content-type", "application/json")
        .with_body(page("topic", 50, true))
        .expect(MAX_BITBUCKET_BRANCHES / 50)
        .create_async()
        .await;

    let list = provider(&server)
        .list_branches("unused", "temps", "example")
        .await
        .expect("listing succeeds");

    assert!(list.truncated, "branches were left unread");
    assert_eq!(list.branches.len(), MAX_BITBUCKET_BRANCHES);
    listing.assert_async().await;
}

#[tokio::test]
async fn a_page_larger_than_the_remaining_budget_is_cut_and_reported_truncated() {
    // Bitbucket may answer with a bigger page than asked for. The cap must
    // stay a memory bound, and the dropped part must not read as complete.
    let mut server = mockito::Server::new_async().await;
    let listing = server
        .mock("GET", "/repositories/temps/example/refs/branches")
        .match_query(Matcher::Any)
        .with_status(200)
        .with_header("content-type", "application/json")
        .with_body(page("topic", MAX_BITBUCKET_BRANCHES + 5, false))
        .expect(1)
        .create_async()
        .await;

    let list = provider(&server)
        .list_branches("unused", "temps", "example")
        .await
        .expect("listing succeeds");

    assert!(list.truncated);
    assert_eq!(list.branches.len(), MAX_BITBUCKET_BRANCHES);
    listing.assert_async().await;
}
