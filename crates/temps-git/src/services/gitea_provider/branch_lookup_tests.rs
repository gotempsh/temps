// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

use super::*;

fn make_provider(base_url: &str) -> GiteaProvider {
    GiteaProvider::new(
        base_url.to_string(),
        AuthMethod::PersonalAccessToken {
            token: "test-token".to_string(),
        },
    )
}

fn branch_json(name: &str, sha: &str) -> serde_json::Value {
    serde_json::json!({
        "name": name,
        "commit": { "id": sha }
    })
}

#[tokio::test]
async fn a_listing_cut_short_by_the_cap_reports_itself_truncated() {
    let mut server = mockito::Server::new_async().await;
    // Full pages all the way to the cap: Gitea has more to give, and the
    // listing must not be read as the repository's whole branch set.
    let mut pages = Vec::new();
    for page_number in 1..=(MAX_GITEA_BRANCHES / 50) {
        let body: Vec<serde_json::Value> = (0..50)
            .map(|index| {
                let id = page_number * 50 + index;
                branch_json(&format!("branch-{id}"), &format!("{id:040x}"))
            })
            .collect();
        pages.push(
            server
                .mock("GET", "/api/v1/repos/temps/example/branches")
                .match_query(mockito::Matcher::Exact(format!(
                    "limit=50&page={page_number}"
                )))
                .with_status(200)
                .with_header("content-type", "application/json")
                .with_body(serde_json::Value::Array(body).to_string())
                .create_async()
                .await,
        );
    }

    let listed = make_provider(&server.url())
        .list_branches("test-token", "temps", "example")
        .await
        .expect("a capped listing still succeeds");

    assert_eq!(listed.branches.len(), MAX_GITEA_BRANCHES);
    assert!(
        listed.truncated,
        "a listing stopped by the cap must say branches were left unread"
    );
    for page in pages {
        page.assert_async().await;
    }
}

#[tokio::test]
async fn a_listing_the_provider_finished_reports_itself_complete() {
    let mut server = mockito::Server::new_async().await;
    let page = server
        .mock("GET", "/api/v1/repos/temps/example/branches")
        .match_query(mockito::Matcher::Exact("limit=50&page=1".to_string()))
        .with_status(200)
        .with_header("content-type", "application/json")
        .with_header("x-total-count", "2")
        .with_body(
            serde_json::json!([
                branch_json("main", &"a".repeat(40)),
                branch_json("topic", &"b".repeat(40))
            ])
            .to_string(),
        )
        .create_async()
        .await;

    let listed = make_provider(&server.url())
        .list_branches("test-token", "temps", "example")
        .await
        .expect("every branch the instance counts is listed");

    assert_eq!(listed.branches.len(), 2);
    assert!(!listed.truncated);
    page.assert_async().await;
}

/// `count` branches named `{prefix}-{n}`, as one page body.
fn page_body(prefix: &str, count: usize) -> String {
    let body: Vec<serde_json::Value> = (0..count)
        .map(|n| branch_json(&format!("{prefix}-{n}"), &format!("{n:040x}")))
        .collect();
    serde_json::Value::Array(body).to_string()
}

#[tokio::test]
async fn short_pages_from_an_instance_with_a_lower_page_size_are_not_the_end() {
    // The instance caps pages at 30 although 50 were asked for. The first
    // short page is not the last: the total says 45 branches exist.
    let mut server = mockito::Server::new_async().await;
    let first = server
        .mock("GET", "/api/v1/repos/temps/example/branches")
        .match_query(mockito::Matcher::Exact("limit=50&page=1".to_string()))
        .with_status(200)
        .with_header("content-type", "application/json")
        .with_header("x-total-count", "45")
        .with_body(page_body("first", 30))
        .expect(1)
        .create_async()
        .await;
    let second = server
        .mock("GET", "/api/v1/repos/temps/example/branches")
        .match_query(mockito::Matcher::Exact("limit=50&page=2".to_string()))
        .with_status(200)
        .with_header("content-type", "application/json")
        .with_header("x-total-count", "45")
        .with_body(page_body("second", 15))
        .expect(1)
        .create_async()
        .await;

    let listed = make_provider(&server.url())
        .list_branches("test-token", "temps", "example")
        .await
        .expect("the listing follows every page");

    assert_eq!(listed.branches.len(), 45);
    assert!(!listed.truncated);
    first.assert_async().await;
    second.assert_async().await;
}

#[tokio::test]
async fn without_a_total_a_short_page_is_followed_until_an_empty_one() {
    let mut server = mockito::Server::new_async().await;
    let first = server
        .mock("GET", "/api/v1/repos/temps/example/branches")
        .match_query(mockito::Matcher::Exact("limit=50&page=1".to_string()))
        .with_status(200)
        .with_header("content-type", "application/json")
        .with_body(page_body("first", 2))
        .expect(1)
        .create_async()
        .await;
    let empty = server
        .mock("GET", "/api/v1/repos/temps/example/branches")
        .match_query(mockito::Matcher::Exact("limit=50&page=2".to_string()))
        .with_status(200)
        .with_header("content-type", "application/json")
        .with_body("[]")
        .expect(1)
        .create_async()
        .await;

    let listed = make_provider(&server.url())
        .list_branches("test-token", "temps", "example")
        .await
        .expect("an empty page ends the listing");

    assert_eq!(listed.branches.len(), 2);
    assert!(!listed.truncated);
    first.assert_async().await;
    empty.assert_async().await;
}

#[tokio::test]
async fn get_branch_finds_a_branch_in_one_request() {
    let mut server = mockito::Server::new_async().await;
    let lookup = server
        .mock(
            "GET",
            "/api/v1/repos/temps/example/branches/release%2F2026-01",
        )
        .with_status(200)
        .with_header("content-type", "application/json")
        .with_body(branch_json("release/2026-01", &"c".repeat(40)).to_string())
        .create_async()
        .await;

    let found = make_provider(&server.url())
        .get_branch("test-token", "temps", "example", "release/2026-01")
        .await
        .expect("the lookup should succeed")
        .expect("the branch exists");

    assert_eq!(found.name, "release/2026-01");
    assert_eq!(found.commit_sha, "c".repeat(40));
    lookup.assert_async().await;
}

#[tokio::test]
async fn get_branch_reports_a_404_as_a_missing_branch() {
    let mut server = mockito::Server::new_async().await;
    let lookup = server
        .mock("GET", "/api/v1/repos/temps/example/branches/gone")
        .with_status(404)
        .with_header("content-type", "application/json")
        .with_body(r#"{"message":"branch does not exist"}"#)
        .create_async()
        .await;

    let found = make_provider(&server.url())
        .get_branch("test-token", "temps", "example", "gone")
        .await
        .expect("a 404 is an answer, not a failure");

    assert!(found.is_none());
    lookup.assert_async().await;
}

#[tokio::test]
async fn get_branch_keeps_a_provider_failure_distinct_from_a_missing_branch() {
    let mut server = mockito::Server::new_async().await;
    let lookup = server
        .mock("GET", "/api/v1/repos/temps/example/branches/main")
        .with_status(500)
        .with_header("content-type", "application/json")
        .with_body(r#"{"message":"internal error"}"#)
        // `send_with_retry` retries a 5xx, so the exact count is not the
        // point here -- only that the failure never became `Ok(None)`.
        .expect_at_least(1)
        .create_async()
        .await;

    let error = make_provider(&server.url())
        .get_branch("test-token", "temps", "example", "main")
        .await
        .expect_err("an unavailable provider must not read as a missing branch");

    assert!(error.to_string().contains("500"), "{error}");
    lookup.assert_async().await;
}
