// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! `GET /git/public/{provider}/{owner}/{repo}/branch`: the guards that answer
//! before any provider request, through the router, and the mapping of the
//! provider's answer to the response.
//!
//! The public providers talk to fixed public hosts, so a request that gets
//! past the guards would leave the machine; the success and missing-branch
//! responses are tested on `public_branch_response`, which the handler
//! returns directly.

use super::*;
use crate::handlers::public_route_tests::state;
use crate::services::public_repo::PublicBranch;
use axum::body::Body;
use axum::http::Request;
use axum::routing::get;
use axum::Router;
use tower::ServiceExt;

const CUSTOM_ORIGIN: &str = "base_url=https%3A%2F%2Fgitlab.example.com";

fn router() -> Router<Arc<AppState>> {
    Router::new().route(
        "/git/public/{provider}/{owner}/{repo}/branch",
        get(get_public_branch),
    )
}

async fn send(uri: &str, auth: Option<AuthContext>) -> axum::response::Response {
    let mut builder = Request::builder().uri(uri);
    if let Some(auth) = auth {
        builder = builder.extension(auth);
    }
    router()
        .with_state(state())
        .oneshot(builder.body(Body::empty()).expect("request builds"))
        .await
        .expect("router responds")
}

/// A Problem Details body: its status, title and detail.
async fn problem(response: axum::response::Response) -> (StatusCode, serde_json::Value) {
    let status = response.status();
    let content_type = response
        .headers()
        .get(axum::http::header::CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .unwrap_or_default()
        .to_string();
    assert!(
        content_type.starts_with("application/problem+json"),
        "errors are Problem Details, got {content_type:?}"
    );
    let body = axum::body::to_bytes(response.into_body(), 64 * 1024)
        .await
        .expect("read the body");
    (status, serde_json::from_slice(&body).expect("a JSON body"))
}

fn principal_without_repository_read() -> AuthContext {
    AuthContext::new_deployment_token(1, None, None, 1, "test-token".to_string(), Vec::new())
}

#[tokio::test]
async fn an_empty_branch_name_is_rejected_before_any_provider_request() {
    let (status, body) =
        problem(send("/git/public/github/temps/example/branch?name=%20", None).await).await;

    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(body["title"], "Invalid Branch Name");
}

#[tokio::test]
async fn a_custom_gitlab_origin_needs_a_signed_in_caller() {
    let uri = format!("/git/public/gitlab/temps/example/branch?name=main&{CUSTOM_ORIGIN}");
    let (status, body) = problem(send(&uri, None).await).await;

    assert_eq!(status, StatusCode::UNAUTHORIZED);
    assert_eq!(body["title"], "Authentication Required");
}

#[tokio::test]
async fn a_custom_gitlab_origin_needs_the_repository_read_permission() {
    let uri = format!("/git/public/gitlab/temps/example/branch?name=main&{CUSTOM_ORIGIN}");
    let (status, body) = problem(send(&uri, Some(principal_without_repository_read())).await).await;

    assert_eq!(status, StatusCode::FORBIDDEN);
    assert_eq!(body["title"], "Insufficient Permissions");
}

#[test]
fn a_branch_the_provider_returns_is_the_response_with_its_default_flag() {
    let found = public_branch_response(
        Some(PublicBranch {
            name: "main".to_string(),
            commit_sha: "a".repeat(40),
            protected: true,
        }),
        "main",
        "temps",
        "example",
        "main",
    )
    .expect("a found branch is a 200");

    assert_eq!(found.name, "main");
    assert_eq!(found.commit_sha, "a".repeat(40));
    assert!(found.protected);
    assert!(found.is_default);

    let other = public_branch_response(
        Some(PublicBranch {
            name: "release/2026-01".to_string(),
            commit_sha: "b".repeat(40),
            protected: false,
        }),
        "main",
        "temps",
        "example",
        "release/2026-01",
    )
    .expect("a found branch is a 200");
    assert!(!other.is_default);
}

#[test]
fn a_branch_the_provider_does_not_have_is_a_404_naming_it() {
    let error = public_branch_response(None, "main", "temps", "example", "gone")
        .expect_err("a missing branch is not a 200");

    assert_eq!(error.status_code, StatusCode::NOT_FOUND);
    let detail = error.body["detail"].as_str().unwrap_or_default();
    assert!(
        detail.contains("temps/example"),
        "names the repository: {detail}"
    );
    assert!(detail.contains("'gone'"), "names the branch: {detail}");
}
