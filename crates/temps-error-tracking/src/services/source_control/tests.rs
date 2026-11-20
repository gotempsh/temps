// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Issues, suspect commits and ownership rules (ADR 111, D5): pure helpers,
//! then the service against a real database and a fake repository.

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Mutex;

use super::*;
use async_trait::async_trait;
use temps_core::source_control::{IssueState, RepositoryIssue};

fn github() -> ProjectRepository {
    ProjectRepository {
        provider: "github".into(),
        repository: "acme/shop".into(),
        web_url: "https://github.com/acme/shop".into(),
        default_branch: "main".into(),
    }
}

#[test]
fn issue_urls_of_the_projects_repository_are_recognised() {
    let gh = github();
    assert_eq!(
        issue_number_in(&gh, "https://github.com/acme/shop/issues/12"),
        Some(12)
    );
    assert_eq!(
        issue_number_in(&gh, "https://github.com/acme/shop/issues/12#issuecomment-1"),
        Some(12)
    );
    assert_eq!(
        issue_number_in(&gh, "https://github.com/acme/shop/pull/12"),
        None
    );
    assert_eq!(
        issue_number_in(&gh, "https://github.com/acme/shopx/issues/12"),
        None
    );
    let gl = ProjectRepository {
        provider: "gitlab".into(),
        web_url: "https://git.example.test/acme/shop".into(),
        ..github()
    };
    assert_eq!(
        issue_number_in(&gl, "https://git.example.test/acme/shop/-/issues/3"),
        Some(3)
    );
    assert_eq!(
        issue_number_in(&gl, "https://git.example.test/acme/shop/issues/3"),
        None
    );
}

#[test]
fn pasted_urls_must_be_http_without_credentials() {
    assert!(validate_issue_url("https://tracker.example.test/browse/SHOP-12").is_ok());
    for bad in [
        "",
        "ftp://tracker.example.test/1",
        "javascript:alert(1)",
        "https://user:secret@tracker.example.test/1",
        "not a url",
    ] {
        assert!(validate_issue_url(bad).is_err(), "{bad}");
    }
    assert!(validate_issue_url(&format!("https://x.test/{}", "a".repeat(MAX_URL_CHARS))).is_err());
}

#[test]
fn an_issue_body_has_the_error_its_frames_and_a_link_but_nothing_else() {
    let source = IssueSource {
        group_id: 42,
        title: "TypeError: total is undefined".into(),
        error_type: "TypeError".into(),
        message: Some("total is *undefined*".into()),
        total_count: 7,
        first_seen: Utc::now(),
        last_seen: Utc::now(),
        frames: vec![frames::AppFrame {
            filename: "src/cart.ts".into(),
            function: Some("sum`up".into()),
            lineno: Some(41),
            in_app: true,
        }],
        source_root: None,
        console_url: Some("https://temps.example.test/projects/shop/errors/42".into()),
    };
    let body = issue_body(&source);
    assert!(
        body.starts_with("**TypeError**: total is \\*undefined\\*"),
        "{body}"
    );
    assert!(body.contains("- Events: 7"), "{body}");
    assert!(body.contains("`src/cart.ts:41` in `sum'up`"), "{body}");
    assert!(
        body.contains(
            "[Error group 42 in Temps](https://temps.example.test/projects/shop/errors/42)"
        ),
        "{body}"
    );
}

#[test]
fn an_issue_body_lists_frames_by_repository_path_and_leaves_out_the_rest() {
    let frame = |filename: &str, lineno: i64| frames::AppFrame {
        filename: filename.into(),
        function: None,
        lineno: Some(lineno),
        in_app: true,
    };
    let source = IssueSource {
        group_id: 42,
        title: "TypeError".into(),
        error_type: "TypeError".into(),
        message: None,
        total_count: 1,
        first_seen: Utc::now(),
        last_seen: Utc::now(),
        frames: vec![
            frame("https://shop.example.test/static/app.js?session=a1b2c3", 1),
            frame("webpack:///./src/cart.ts", 41),
            frame("<anonymous>", 2),
        ],
        source_root: Some("apps/web".into()),
        console_url: None,
    };
    let body = issue_body(&source);
    assert!(body.contains("`apps/web/src/cart.ts:41`"), "{body}");
    assert!(!body.contains("shop.example.test"), "{body}");
    assert!(!body.contains("session="), "{body}");
    assert!(!body.contains("anonymous"), "{body}");

    // No frame in the repository: no stack section at all.
    let body = issue_body(&IssueSource {
        frames: vec![frame(
            "https://shop.example.test/static/app.js?session=a1b2c3",
            1,
        )],
        ..source
    });
    assert!(!body.contains("Stack"), "{body}");
    assert!(!body.contains("session="), "{body}");
}

/// A repository in memory.
#[derive(Default)]
pub(crate) struct FakeRepository {
    pub repository: Mutex<Option<ProjectRepository>>,
    pub opened: Mutex<Vec<(String, String)>>,
    pub states: Mutex<HashMap<i64, IssueState>>,
    pub commit_calls: Mutex<Vec<(String, Option<String>)>>,
    pub refuse: AtomicBool,
}

impl FakeRepository {
    pub fn on(repository: ProjectRepository) -> Arc<Self> {
        let fake = Self::default();
        *fake.repository.lock().expect("lock") = Some(repository);
        Arc::new(fake)
    }
}

#[async_trait]
impl ProjectSourceControl for FakeRepository {
    async fn repository(
        &self,
        _project_id: i32,
    ) -> Result<Option<ProjectRepository>, SourceControlError> {
        Ok(self.repository.lock().expect("lock").clone())
    }

    async fn create_issue(
        &self,
        project_id: i32,
        title: &str,
        body: &str,
    ) -> Result<RepositoryIssue, SourceControlError> {
        if self.refuse.load(Ordering::SeqCst) {
            return Err(SourceControlError::Forbidden {
                project_id,
                provider: "github".into(),
                repository: "acme/shop".into(),
                operation: "create an issue",
                status: 403,
            });
        }
        let mut opened = self.opened.lock().expect("lock");
        opened.push((title.to_string(), body.to_string()));
        let number = opened.len() as i64;
        Ok(RepositoryIssue {
            number,
            url: format!("https://github.com/acme/shop/issues/{number}"),
            title: title.to_string(),
            state: IssueState::Open,
        })
    }

    async fn issue(
        &self,
        project_id: i32,
        number: i64,
    ) -> Result<RepositoryIssue, SourceControlError> {
        if number == 404 {
            return Err(SourceControlError::NotFound {
                project_id,
                provider: "github".into(),
                repository: "acme/shop".into(),
                what: format!("issue #{number}"),
            });
        }
        Ok(RepositoryIssue {
            number,
            url: format!("https://github.com/acme/shop/issues/{number}"),
            title: format!("Issue {number}"),
            state: self
                .states
                .lock()
                .expect("lock")
                .get(&number)
                .copied()
                .unwrap_or(IssueState::Open),
        })
    }

    async fn commits_touching(
        &self,
        _project_id: i32,
        path: &str,
        at: Option<&str>,
        _limit: usize,
    ) -> Result<Vec<RepositoryCommit>, SourceControlError> {
        self.commit_calls
            .lock()
            .expect("lock")
            .push((path.to_string(), at.map(str::to_string)));
        Ok(vec![RepositoryCommit {
            sha: "c0ffee".into(),
            summary: "Change the cart total".into(),
            author_name: Some("Sam Doe".into()),
            authored_at: None,
            url: "https://github.com/acme/shop/commit/c0ffee".into(),
        }])
    }
}

mod database {
    use super::*;
    use crate::services::error_tracking_service::ErrorTrackingService;
    use crate::services::group_workflow::tests::{create_group, create_project, GroupSpec};
    use crate::services::types::CreateErrorEventData;
    use serde_json::json;
    use temps_test_utils::{is_container_runtime_unavailable, TestDatabase};

    macro_rules! test_db {
        ($what:literal) => {
            match TestDatabase::with_migrations().await {
                Ok(db) => db,
                Err(error) if is_container_runtime_unavailable(&error.to_string()) => {
                    eprintln!(concat!("Skipping ", $what, ": {}"), error);
                    return;
                }
                Err(error) => panic!(concat!($what, " database setup failed: {}"), error),
            }
        };
    }

    async fn create_user(db: &DatabaseConnection, name: &str) -> i32 {
        db.query_one(Statement::from_sql_and_values(
            DatabaseBackend::Postgres,
            "INSERT INTO users (name, email, email_verified, must_change_password, mfa_enabled, \
                                created_at, updated_at) \
             VALUES ($1, $2, TRUE, FALSE, FALSE, NOW(), NOW()) RETURNING id",
            [
                name.into(),
                format!("{}-{}@example.test", name, uuid::Uuid::new_v4()).into(),
            ],
        ))
        .await
        .expect("insert user")
        .expect("user row")
        .try_get("", "id")
        .expect("user id")
    }

    fn service(db: &Arc<DatabaseConnection>) -> SourceControlService {
        SourceControlService::new(
            db.clone(),
            Arc::new(ownership::OwnershipService::new(db.clone())),
        )
    }

    /// An event of the group whose innermost in-app frame is `src/cart.ts`.
    async fn event_with_frames(db: &DatabaseConnection, project: i32, group: i32) {
        db.execute(Statement::from_sql_and_values(
            DatabaseBackend::Postgres,
            "INSERT INTO error_events (error_group_id, project_id, fingerprint_hash, timestamp, \
                                       exception_type, exception_value, source, data, created_at) \
             VALUES ($1, $2, 'synthetic', NOW(), 'TypeError', 'total is undefined', 'sentry', $3, NOW())",
            [
                group.into(),
                project.into(),
                json!({"sentry": {"exception": {"values": [{"type": "TypeError",
                    "stacktrace": {"frames": [
                        {"filename": "node_modules/lib/index.js", "in_app": false},
                        {"filename": "src/app.ts", "function": "main", "lineno": 3, "in_app": true},
                        {"filename": "webpack:///./src/cart.ts", "function": "total", "lineno": 41, "in_app": true}
                    ]}}]}}})
                .into(),
            ],
        ))
        .await
        .expect("insert event");
    }

    async fn activity_kinds(db: &DatabaseConnection, project: i32, group: i32) -> Vec<String> {
        db.query_all(Statement::from_sql_and_values(
            DatabaseBackend::Postgres,
            "SELECT kind FROM error_group_activity \
             WHERE project_id = $1 AND error_group_id = $2 ORDER BY id",
            [project.into(), group.into()],
        ))
        .await
        .expect("activity")
        .iter()
        .map(|row| row.try_get("", "kind").expect("kind"))
        .collect()
    }

    async fn assignee(db: &DatabaseConnection, group: i32) -> Option<i32> {
        db.query_one(Statement::from_sql_and_values(
            DatabaseBackend::Postgres,
            "SELECT assignee_user_id FROM error_groups WHERE id = $1",
            [group.into()],
        ))
        .await
        .expect("group")
        .expect("row")
        .try_get("", "assignee_user_id")
        .expect("assignee")
    }

    #[tokio::test]
    async fn the_status_says_what_is_missing_and_where_to_fix_it() {
        let test_db = test_db!("source control status");
        let db = test_db.connection_arc();
        let project = create_project(db.as_ref()).await;
        let service = service(&db);

        let status = service.status(project).await.expect("status");
        assert!(!status.configured);
        assert_eq!(status.setup_path.as_deref(), Some(GIT_PROVIDERS_PATH));

        let fake = Arc::new(FakeRepository::default());
        service.set_source_control(fake.clone());
        let status = service.status(project).await.expect("status");
        assert!(!status.configured);
        assert!(
            status
                .reason
                .as_deref()
                .is_some_and(|r| r.contains("not connected")),
            "{status:?}"
        );
        assert!(
            status
                .setup_path
                .as_deref()
                .is_some_and(|p| p.starts_with("/projects/") && p.ends_with("/settings/git")),
            "{status:?}"
        );

        *fake.repository.lock().expect("lock") = Some(ProjectRepository {
            provider: "bitbucket".into(),
            ..github()
        });
        let status = service.status(project).await.expect("status");
        assert!(!status.configured);
        assert!(status
            .reason
            .as_deref()
            .is_some_and(|r| r.contains("bitbucket")));

        *fake.repository.lock().expect("lock") = Some(github());
        let status = service.status(project).await.expect("status");
        assert!(status.configured);
        assert_eq!(status.repository.as_deref(), Some("acme/shop"));
        assert_eq!(status.reason, None);
    }

    #[tokio::test]
    async fn an_issue_is_opened_once_while_it_is_open() {
        let test_db = test_db!("open an issue");
        let db = test_db.connection_arc();
        let project = create_project(db.as_ref()).await;
        let group = create_group(db.as_ref(), project, GroupSpec::default()).await;
        event_with_frames(db.as_ref(), project, group).await;
        let user = create_user(db.as_ref(), "ines").await;
        let service = service(&db);
        let fake = FakeRepository::on(github());
        service.set_source_control(fake.clone());

        let link = service
            .create_issue(
                project,
                group,
                None,
                Some(user),
                Some("https://temps.example.test/"),
            )
            .await
            .expect("opened");
        assert_eq!(link.provider, "github");
        assert_eq!(link.number, Some(1));
        assert_eq!(link.state.as_deref(), Some("open"));
        assert!(link.state_checked_at.is_some());
        {
            let opened = fake.opened.lock().expect("lock");
            let (_, body) = &opened[0];
            assert!(body.contains("`src/cart.ts:41` in `total`"), "{body}");
            assert!(
                body.contains(&format!("/errors/{group})")),
                "the body links back to the group: {body}"
            );
        }

        let error = service
            .create_issue(project, group, None, Some(user), None)
            .await
            .expect_err("one open issue per group and repository");
        assert!(
            matches!(&error, ErrorTrackingError::IssueAlreadyLinked { group_id, url } if *group_id == group && url.ends_with("/issues/1")),
            "{error}"
        );
        assert_eq!(fake.opened.lock().expect("lock").len(), 1);

        // Closed in the repository: a refresh reads it, and a new one may open.
        fake.states
            .lock()
            .expect("lock")
            .insert(1, IssueState::Closed);
        let (links, failed) = service
            .refresh_links(project, group)
            .await
            .expect("refresh");
        assert_eq!(failed, 0);
        assert_eq!(links[0].state.as_deref(), Some("closed"));
        let again = service
            .create_issue(
                project,
                group,
                Some("Cart total fails again"),
                Some(user),
                None,
            )
            .await
            .expect("a closed issue does not block a new one");
        assert_eq!(again.number, Some(2));
        assert_eq!(again.title, "Cart total fails again");

        let activity = activity_kinds(db.as_ref(), project, group).await;
        assert_eq!(
            activity.iter().filter(|k| *k == "issue_linked").count(),
            2,
            "{activity:?}"
        );
    }

    #[tokio::test]
    async fn a_refused_issue_links_nothing_and_says_why() {
        let test_db = test_db!("refused issue");
        let db = test_db.connection_arc();
        let project = create_project(db.as_ref()).await;
        let group = create_group(db.as_ref(), project, GroupSpec::default()).await;
        let service = service(&db);

        let error = service
            .create_issue(project, group, None, None, None)
            .await
            .expect_err("no integration");
        assert!(matches!(
            error,
            ErrorTrackingError::SourceControlUnavailable { .. }
        ));

        let fake = FakeRepository::on(github());
        fake.refuse.store(true, Ordering::SeqCst);
        service.set_source_control(fake);
        let error = service
            .create_issue(project, group, None, None, None)
            .await
            .expect_err("refused");
        assert!(error.to_string().contains("HTTP 403"), "{error}");
        assert!(service
            .list_links(project, group)
            .await
            .expect("list")
            .is_empty());

        let error = service
            .create_issue(
                project,
                group,
                Some(&"t".repeat(MAX_ISSUE_TITLE_CHARS + 1)),
                None,
                None,
            )
            .await
            .expect_err("title too long");
        assert!(matches!(error, ErrorTrackingError::Validation(_)));
        let other = create_project(db.as_ref()).await;
        let error = service
            .create_issue(other, group, None, None, None)
            .await
            .expect_err("another project's group");
        assert!(matches!(
            error,
            ErrorTrackingError::GroupNotInProject { group_id, project_id } if group_id == group && project_id == other
        ));
    }

    #[tokio::test]
    async fn issues_are_linked_by_url_once_up_to_the_limit_and_unlinked() {
        let test_db = test_db!("link issues");
        let db = test_db.connection_arc();
        let project = create_project(db.as_ref()).await;
        let group = create_group(db.as_ref(), project, GroupSpec::default()).await;
        let service = service(&db);
        service.set_source_control(FakeRepository::on(github()));

        let jira = service
            .link_issue(
                project,
                group,
                "https://tracker.example.test/browse/SHOP-12",
                None,
                None,
            )
            .await
            .expect("plain link");
        assert_eq!(jira.provider, "link");
        assert_eq!(jira.state, None);
        assert_eq!(jira.title, "https://tracker.example.test/browse/SHOP-12");

        let own = service
            .link_issue(
                project,
                group,
                "https://github.com/acme/shop/issues/7",
                None,
                None,
            )
            .await
            .expect("own issue");
        assert_eq!(own.provider, "github");
        assert_eq!(own.number, Some(7));
        assert_eq!(own.title, "Issue 7");
        assert_eq!(own.state.as_deref(), Some("open"));

        let unreadable = service
            .link_issue(
                project,
                group,
                "https://github.com/acme/shop/issues/404",
                Some("Old one"),
                None,
            )
            .await
            .expect("an unreadable issue is still linked");
        assert_eq!(unreadable.state, None);
        assert_eq!(unreadable.title, "Old one");

        let error = service
            .link_issue(
                project,
                group,
                "https://github.com/acme/shop/issues/7",
                None,
                None,
            )
            .await
            .expect_err("duplicate");
        assert!(matches!(
            error,
            ErrorTrackingError::IssueAlreadyLinked { .. }
        ));
        let error = service
            .link_issue(project, group, "ftp://tracker.example.test/1", None, None)
            .await
            .expect_err("not http");
        assert!(matches!(error, ErrorTrackingError::Validation(_)));

        for n in 0..(MAX_LINKS_PER_GROUP - 3) {
            service
                .link_issue(
                    project,
                    group,
                    &format!("https://tracker.example.test/{n}"),
                    None,
                    None,
                )
                .await
                .expect("under the limit");
        }
        let error = service
            .link_issue(
                project,
                group,
                "https://tracker.example.test/last",
                None,
                None,
            )
            .await
            .expect_err("limit");
        assert!(matches!(
            error,
            ErrorTrackingError::IssueLinkLimit {
                limit: MAX_LINKS_PER_GROUP,
                ..
            }
        ));

        let removed = service
            .unlink_issue(project, group, jira.id, None)
            .await
            .expect("unlink");
        assert_eq!(removed.url, jira.url);
        let error = service
            .unlink_issue(project, group, jira.id, None)
            .await
            .expect_err("gone");
        assert!(matches!(
            error,
            ErrorTrackingError::IssueLinkNotFound { link_id, .. } if link_id == jira.id
        ));
        let activity = activity_kinds(db.as_ref(), project, group).await;
        assert_eq!(activity.last().map(String::as_str), Some("issue_unlinked"));
    }

    #[tokio::test]
    async fn a_group_at_the_link_limit_opens_no_issue() {
        let test_db = test_db!("issue at the link limit");
        let db = test_db.connection_arc();
        let project = create_project(db.as_ref()).await;
        let group = create_group(db.as_ref(), project, GroupSpec::default()).await;
        event_with_frames(db.as_ref(), project, group).await;
        let service = service(&db);
        let fake = FakeRepository::on(github());
        service.set_source_control(fake.clone());
        for n in 0..MAX_LINKS_PER_GROUP {
            service
                .link_issue(
                    project,
                    group,
                    &format!("https://tracker.example.test/{n}"),
                    None,
                    None,
                )
                .await
                .expect("under the limit");
        }

        let error = service
            .create_issue(project, group, None, None, None)
            .await
            .expect_err("the group is full");
        assert!(
            matches!(error, ErrorTrackingError::IssueLinkLimit { group_id, .. } if group_id == group),
            "{error}"
        );
        assert!(
            fake.opened.lock().expect("lock").is_empty(),
            "no issue is opened in the repository that could not be linked"
        );
    }

    #[tokio::test]
    async fn two_links_added_at_once_do_not_pass_the_limit() {
        use sea_orm::TransactionTrait;
        let test_db = test_db!("concurrent links");
        let db = test_db.connection_arc();
        let project = create_project(db.as_ref()).await;
        let group = create_group(db.as_ref(), project, GroupSpec::default()).await;
        let service = Arc::new(service(&db));
        for n in 0..(MAX_LINKS_PER_GROUP - 1) {
            service
                .link_issue(
                    project,
                    group,
                    &format!("https://tracker.example.test/{n}"),
                    None,
                    None,
                )
                .await
                .expect("under the limit");
        }

        // The last free slot, taken by a link not yet committed.
        let first = db.begin().await.expect("transaction");
        SourceControlService::insert_link(
            &first,
            project,
            group,
            "link",
            None,
            None,
            "https://tracker.example.test/first",
            "first",
            None,
            None,
        )
        .await
        .expect("the last slot");

        let second = tokio::spawn({
            let service = service.clone();
            async move {
                service
                    .link_issue(
                        project,
                        group,
                        "https://tracker.example.test/second",
                        None,
                        None,
                    )
                    .await
            }
        });
        tokio::time::sleep(std::time::Duration::from_millis(300)).await;
        assert!(
            !second.is_finished(),
            "the second link waits for the first to be counted"
        );
        first.commit().await.expect("commit the first");
        let error = tokio::time::timeout(std::time::Duration::from_secs(10), second)
            .await
            .expect("the second link finishes")
            .expect("task")
            .expect_err("the group is full");
        assert!(
            matches!(error, ErrorTrackingError::IssueLinkLimit { .. }),
            "{error}"
        );
        assert_eq!(
            service
                .list_links(project, group)
                .await
                .expect("links")
                .len(),
            MAX_LINKS_PER_GROUP
        );
    }

    #[tokio::test]
    async fn a_refresh_reads_only_issues_of_the_projects_repository() {
        let test_db = test_db!("refresh another repository");
        let db = test_db.connection_arc();
        let project = create_project(db.as_ref()).await;
        let group = create_group(db.as_ref(), project, GroupSpec::default()).await;
        let service = service(&db);
        let fake = FakeRepository::on(github());
        service.set_source_control(fake.clone());
        let own = service
            .link_issue(
                project,
                group,
                "https://github.com/acme/shop/issues/3",
                None,
                None,
            )
            .await
            .expect("own issue");
        // Linked while the project deployed from another repository, where
        // issue 3 is another issue.
        db.execute(Statement::from_sql_and_values(
            DatabaseBackend::Postgres,
            "INSERT INTO error_group_issue_links \
             (project_id, error_group_id, provider, repository, number, url, title, state, \
              state_checked_at) \
             VALUES ($1, $2, 'github', 'acme/legacy', 3, \
                     'https://github.com/acme/legacy/issues/3', 'Legacy issue', 'open', NOW())",
            [project.into(), group.into()],
        ))
        .await
        .expect("old link");
        fake.states
            .lock()
            .expect("lock")
            .insert(3, IssueState::Closed);

        let (links, failed) = service
            .refresh_links(project, group)
            .await
            .expect("refresh");
        assert_eq!(failed, 1, "the other repository's issue is not refreshed");
        let by_url = |url: &str| links.iter().find(|l| l.url == url).cloned().expect("link");
        assert_eq!(by_url(&own.url).state.as_deref(), Some("closed"));
        let legacy = by_url("https://github.com/acme/legacy/issues/3");
        assert_eq!(legacy.state.as_deref(), Some("open"));
        assert_eq!(legacy.title, "Legacy issue");
    }

    #[tokio::test]
    async fn suspect_commits_follow_the_innermost_application_frame() {
        let test_db = test_db!("suspect commits");
        let db = test_db.connection_arc();
        let project = create_project(db.as_ref()).await;
        let group = create_group(db.as_ref(), project, GroupSpec::default()).await;
        let bare = create_group(db.as_ref(), project, GroupSpec::default()).await;
        event_with_frames(db.as_ref(), project, group).await;
        let service = service(&db);

        let without = service
            .suspect_commits(project, group)
            .await
            .expect("no integration");
        assert!(without.commits.is_empty());
        assert!(without.reason.is_some());
        assert_eq!(without.path, None);

        let fake = FakeRepository::on(github());
        service.set_source_control(fake.clone());
        let found = service
            .suspect_commits(project, group)
            .await
            .expect("commits");
        assert_eq!(found.path.as_deref(), Some("src/cart.ts"));
        assert_eq!(found.function.as_deref(), Some("total"));
        assert_eq!(found.lineno, Some(41));
        assert_eq!(found.commits.len(), 1);
        assert_eq!(found.reason, None);
        assert_eq!(
            fake.commit_calls.lock().expect("lock").as_slice(),
            &[("src/cart.ts".to_string(), None)],
            "without a deployment the default branch is read"
        );

        let none = service
            .suspect_commits(project, bare)
            .await
            .expect("no frames");
        assert!(none.commits.is_empty());
        assert!(none
            .reason
            .as_deref()
            .is_some_and(|r| r.contains("no application frame")));
        let error = service
            .suspect_commits(project, 999_999)
            .await
            .expect_err("unknown group");
        assert!(matches!(
            error,
            ErrorTrackingError::GroupNotInProject {
                group_id: 999_999,
                ..
            }
        ));
    }

    fn sentry_event(project: i32, message: &str, file: &str) -> CreateErrorEventData {
        CreateErrorEventData {
            source: Some("sentry".into()),
            exception_type: Some("TypeError".into()),
            exception_value: Some(message.into()),
            project_id: project,
            raw_sentry_event: Some(json!({"exception": {"values": [{"type": "TypeError",
            "stacktrace": {"frames": [
                {"filename": "src/shared/http.ts", "function": "send", "in_app": true},
                {"filename": file, "function": message, "in_app": true}
            ]}}]}})),
            ..Default::default()
        }
    }

    #[tokio::test]
    async fn ownership_rules_assign_new_groups_by_their_innermost_frame() {
        let test_db = test_db!("ownership rules");
        let db = test_db.connection_arc();
        let project = create_project(db.as_ref()).await;
        let billing = create_user(db.as_ref(), "bea").await;
        let platform = create_user(db.as_ref(), "pau").await;
        let tracking = ErrorTrackingService::new(db.clone());
        let ownership = tracking.source_control.ownership.clone();
        let rule = |pattern: &str, owner_user_id: i32| ownership::OwnershipRuleInput {
            pattern: pattern.into(),
            owner_user_id,
        };

        let error = ownership
            .replace(project, &[rule("src/billing/", 987_654)])
            .await
            .expect_err("unknown owner");
        assert!(error.to_string().contains("987654"), "{error}");
        let error = ownership
            .replace(project, &[rule("../etc", billing)])
            .await
            .expect_err("bad pattern");
        assert!(matches!(error, ErrorTrackingError::Validation(_)));

        let (rules, previous) = ownership
            .replace(
                project,
                &[rule("src/shared/", platform), rule("src/billing/", billing)],
            )
            .await
            .expect("rules");
        assert!(previous.is_empty());
        assert_eq!(rules.len(), 2);
        assert_eq!(rules[1].owner_name.as_deref(), Some("bea"));

        // The innermost frame is in billing: billing owns the group, though
        // the shared rule comes first and matches an outer frame.
        let receipt = tracking
            .process_error_event_with_receipt(sentry_event(
                project,
                "invoiceTotal",
                "src/billing/invoice.ts",
            ))
            .await
            .expect("ingest");
        assert_eq!(assignee(db.as_ref(), receipt.group_id).await, Some(billing));
        let details: serde_json::Value = db
            .query_one(Statement::from_sql_and_values(
                DatabaseBackend::Postgres,
                "SELECT details FROM error_group_activity \
                 WHERE error_group_id = $1 AND kind = 'assigned'",
                [receipt.group_id.into()],
            ))
            .await
            .expect("activity")
            .expect("row")
            .try_get("", "details")
            .expect("details");
        assert_eq!(details["ownership_rule"], "src/billing/");

        // Another event of the group changes nothing; a group whose inner
        // frame matches no rule falls to the rule of an outer frame.
        let again = tracking
            .process_error_event_with_receipt(sentry_event(
                project,
                "invoiceTotal",
                "src/billing/invoice.ts",
            ))
            .await
            .expect("ingest again");
        assert_eq!(again.group_id, receipt.group_id);
        let other = tracking
            .process_error_event_with_receipt(sentry_event(project, "buildDocs", "docs/build.ts"))
            .await
            .expect("ingest other");
        assert_ne!(other.group_id, receipt.group_id);
        assert_eq!(assignee(db.as_ref(), other.group_id).await, Some(platform));

        let (rules, previous) = ownership.replace(project, &[]).await.expect("clear");
        assert!(rules.is_empty());
        assert_eq!(previous.len(), 2);
        assert!(!ownership.may_have_rules(project));
    }
}
