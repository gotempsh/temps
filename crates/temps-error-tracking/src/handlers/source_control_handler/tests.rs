// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

use super::*;
use crate::handlers::group_workflow_handler::tests::{
    app_state, metadata, test_user, RecordingAuditLogger,
};
use axum_test::TestServer;
use temps_authz::Role;

fn state_without_queries() -> Arc<AppState> {
    let db =
        Arc::new(sea_orm::MockDatabase::new(sea_orm::DatabaseBackend::Postgres).into_connection());
    app_state(&db, Arc::new(RecordingAuditLogger::default()))
}

fn detail(problem: &Problem) -> String {
    problem
        .body
        .get("detail")
        .and_then(|d| d.as_str())
        .unwrap_or_default()
        .to_string()
}

#[tokio::test]
async fn without_a_token_every_route_answers_401() {
    let server = TestServer::new(routes().with_state(state_without_queries()));
    for uri in [
        "/projects/1/error-tracking/source-control",
        "/projects/1/error-groups/2/issues",
        "/projects/1/error-groups/2/suspect-commits",
        "/projects/1/error-ownership-rules",
    ] {
        let response = server.get(uri).await;
        assert_eq!(response.status_code(), StatusCode::UNAUTHORIZED, "{uri}");
    }
    for uri in [
        "/projects/1/error-groups/2/issues",
        "/projects/1/error-groups/2/issues/link",
        "/projects/1/error-groups/2/issues/refresh",
    ] {
        let response = server
            .post(uri)
            .json(&serde_json::json!({ "url": "https://tracker.example.test/1" }))
            .await;
        assert_eq!(response.status_code(), StatusCode::UNAUTHORIZED, "{uri}");
    }
    let response = server.delete("/projects/1/error-groups/2/issues/3").await;
    assert_eq!(response.status_code(), StatusCode::UNAUTHORIZED);
    let response = server
        .put("/projects/1/error-ownership-rules")
        .json(&serde_json::json!({ "rules": [] }))
        .await;
    assert_eq!(response.status_code(), StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn a_reader_cannot_open_link_unlink_or_change_ownership() {
    let reader = || RequireAuth(test_user(3, Role::Reader));
    let problem = create_error_group_issue(
        State(state_without_queries()),
        reader(),
        Path((1, 2)),
        Extension(metadata()),
        Json(CreateIssueRequest::default()),
    )
    .await
    .expect_err("refused");
    assert_eq!(problem.status_code, StatusCode::FORBIDDEN);
    let problem = link_error_group_issue(
        State(state_without_queries()),
        reader(),
        Path((1, 2)),
        Extension(metadata()),
        Json(LinkIssueRequest {
            url: "https://tracker.example.test/1".into(),
            title: None,
        }),
    )
    .await
    .expect_err("refused");
    assert_eq!(problem.status_code, StatusCode::FORBIDDEN);
    let problem = unlink_error_group_issue(
        State(state_without_queries()),
        reader(),
        Path((1, 2, 3)),
        Extension(metadata()),
    )
    .await
    .expect_err("refused");
    assert_eq!(problem.status_code, StatusCode::FORBIDDEN);
    let problem = replace_error_ownership_rules(
        State(state_without_queries()),
        reader(),
        Path(1),
        Extension(metadata()),
        Json(ReplaceOwnershipRulesRequest { rules: vec![] }),
    )
    .await
    .expect_err("refused");
    assert_eq!(problem.status_code, StatusCode::FORBIDDEN);
}

#[tokio::test]
async fn without_the_git_integration_opening_an_issue_is_a_503_pointing_at_git_providers() {
    let audit = Arc::new(RecordingAuditLogger::default());
    let db =
        Arc::new(sea_orm::MockDatabase::new(sea_orm::DatabaseBackend::Postgres).into_connection());
    let problem = create_error_group_issue(
        State(app_state(&db, audit.clone())),
        RequireAuth(test_user(3, Role::User)),
        Path((1, 2)),
        Extension(metadata()),
        Json(CreateIssueRequest::default()),
    )
    .await
    .expect_err("no integration");
    assert_eq!(problem.status_code, StatusCode::SERVICE_UNAVAILABLE);
    assert_eq!(
        problem.body.get("setup_path").and_then(|v| v.as_str()),
        Some("/git-providers")
    );
    assert_eq!(
        problem.body.get("project_id").and_then(|v| v.as_i64()),
        Some(1)
    );
    assert!(audit.entries.lock().expect("audit lock").is_empty());
}

#[tokio::test]
async fn a_bad_url_or_pattern_is_a_400_that_says_what_is_wrong() {
    let problem = link_error_group_issue(
        State(state_without_queries()),
        RequireAuth(test_user(3, Role::User)),
        Path((1, 2)),
        Extension(metadata()),
        Json(LinkIssueRequest {
            url: "javascript:alert(1)".into(),
            title: None,
        }),
    )
    .await
    .expect_err("not http");
    assert_eq!(problem.status_code, StatusCode::BAD_REQUEST);
    assert!(
        detail(&problem).contains("not an http or https URL"),
        "{}",
        detail(&problem)
    );

    let problem = replace_error_ownership_rules(
        State(state_without_queries()),
        RequireAuth(test_user(3, Role::User)),
        Path(1),
        Extension(metadata()),
        Json(ReplaceOwnershipRulesRequest {
            rules: vec![OwnershipRuleRequest {
                pattern: "src/../secrets".into(),
                owner_user_id: 4,
            }],
        }),
    )
    .await
    .expect_err("bad pattern");
    assert_eq!(problem.status_code, StatusCode::BAD_REQUEST);
    assert!(
        detail(&problem).contains("src/../secrets"),
        "{}",
        detail(&problem)
    );
}

/// The caller may open every project; user 99 may open none.
struct OnlyCaller;

#[async_trait::async_trait]
impl temps_core::ProjectAccessChecker for OnlyCaller {
    async fn user_can_access_project(
        &self,
        user_id: i32,
        _project_id: i32,
    ) -> Result<bool, Box<dyn std::error::Error + Send + Sync>> {
        Ok(user_id != 99)
    }
}

#[tokio::test]
async fn an_owner_who_cannot_open_the_project_is_refused() {
    let db =
        Arc::new(sea_orm::MockDatabase::new(sea_orm::DatabaseBackend::Postgres).into_connection());
    let audit = Arc::new(RecordingAuditLogger::default());
    let state = app_state(&db, audit.clone());
    let state = Arc::new(AppState {
        error_tracking_service: state.error_tracking_service.clone(),
        alert_service: state.alert_service.clone(),
        audit_service: audit.clone(),
        project_access_checker: Some(Arc::new(OnlyCaller)),
    });
    let problem = replace_error_ownership_rules(
        State(state),
        RequireAuth(test_user(3, Role::User)),
        Path(1),
        Extension(metadata()),
        Json(ReplaceOwnershipRulesRequest {
            rules: vec![OwnershipRuleRequest {
                pattern: "src/billing/".into(),
                owner_user_id: 99,
            }],
        }),
    )
    .await
    .expect_err("owner without access");
    assert_eq!(problem.status_code, StatusCode::BAD_REQUEST);
    assert!(
        detail(&problem).contains("user 99 cannot open project 1"),
        "{}",
        detail(&problem)
    );
    assert!(audit.entries.lock().expect("audit lock").is_empty());
}

#[test]
fn repository_failures_map_to_the_status_that_says_whose_fault_it_is() {
    use temps_core::SourceControlError as E;
    let problem = Problem::from(ErrorTrackingError::SourceControl {
        project_id: 5,
        source: E::NotConnected { project_id: 5 },
    });
    assert_eq!(problem.status_code, StatusCode::CONFLICT);
    assert!(detail(&problem).contains("Project 5 is not connected"));
    let problem = Problem::from(ErrorTrackingError::SourceControl {
        project_id: 5,
        source: E::Forbidden {
            project_id: 5,
            provider: "github".into(),
            repository: "acme/shop".into(),
            operation: "create an issue",
            status: 403,
        },
    });
    assert_eq!(problem.status_code, StatusCode::BAD_GATEWAY);
    assert!(detail(&problem).contains("acme/shop"));
    let problem = Problem::from(ErrorTrackingError::SourceControl {
        project_id: 5,
        source: E::MissingPermission {
            project_id: 5,
            provider: "github".into(),
            repository: "acme/shop".into(),
            operation: "create an issue",
            permission: "issues=write".into(),
        },
    });
    assert_eq!(problem.status_code, StatusCode::BAD_GATEWAY);
    assert_eq!(
        problem.body.get("type").and_then(|v| v.as_str()),
        Some("https://temps.sh/probs/source-control-missing-permission")
    );
    assert!(detail(&problem).contains("issues=write"));
    let problem = Problem::from(ErrorTrackingError::IssueAlreadyLinked {
        group_id: 2,
        url: "https://github.com/acme/shop/issues/1".into(),
    });
    assert_eq!(problem.status_code, StatusCode::CONFLICT);
    assert_eq!(
        problem.body.get("url").and_then(|v| v.as_str()),
        Some("https://github.com/acme/shop/issues/1")
    );
}
