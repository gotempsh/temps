// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Issue-workflow tests against a real database: a bulk update must touch
//! exactly the selected groups of one project and nothing else.

use super::*;
use sea_orm::{ActiveModelTrait, EntityTrait, Set};
use temps_entities::{error_groups, projects};
use temps_test_utils::{is_container_runtime_unavailable, TestDatabase};

/// Skips with a message when no database is reachable, as Docker-dependent
/// tests elsewhere in the workspace do.
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

pub(crate) async fn create_project(db: &DatabaseConnection) -> i32 {
    let now = Utc::now();
    projects::ActiveModel {
        name: Set("Workflow Project".to_string()),
        repo_name: Set("repo".to_string()),
        repo_owner: Set("owner".to_string()),
        directory: Set("/".to_string()),
        main_branch: Set("main".to_string()),
        slug: Set(format!("workflow-{}", uuid::Uuid::new_v4())),
        preset: Set(temps_entities::preset::Preset::NextJs),
        created_at: Set(now),
        updated_at: Set(now),
        ..Default::default()
    }
    .insert(db)
    .await
    .expect("insert project")
    .id
}

pub(super) struct GroupSpec<'a> {
    pub(super) title: &'a str,
    pub(super) error_type: &'a str,
    pub(super) status: &'a str,
    pub(super) last_seen_offset_secs: i64,
}

impl Default for GroupSpec<'_> {
    fn default() -> Self {
        Self {
            title: "TypeError: x is undefined",
            error_type: "TypeError",
            status: "unresolved",
            last_seen_offset_secs: 0,
        }
    }
}

pub(super) async fn create_group(
    db: &DatabaseConnection,
    project_id: i32,
    spec: GroupSpec<'_>,
) -> i32 {
    let now = Utc::now();
    error_groups::ActiveModel {
        title: Set(spec.title.to_string()),
        error_type: Set(spec.error_type.to_string()),
        message_template: Set(Some(spec.title.to_string())),
        embedding: Set(None),
        first_seen: Set(now - chrono::Duration::days(1)),
        last_seen: Set(now - chrono::Duration::seconds(spec.last_seen_offset_secs)),
        total_count: Set(3),
        status: Set(spec.status.to_string()),
        assigned_to: Set(None),
        project_id: Set(project_id),
        environment_id: Set(None),
        deployment_id: Set(None),
        visitor_id: Set(None),
        created_at: Set(now),
        updated_at: Set(now),
        ..Default::default()
    }
    .insert(db)
    .await
    .expect("insert error group")
    .id
}

pub(super) async fn status_of(db: &DatabaseConnection, group_id: i32) -> String {
    error_groups::Entity::find_by_id(group_id)
        .one(db)
        .await
        .expect("load group")
        .expect("group exists")
        .status
}

#[tokio::test]
async fn bulk_resolve_by_ids_updates_only_the_named_groups() {
    let test_db = test_db!("bulk resolve by ids");
    let db = test_db.connection_arc();
    let project = create_project(db.as_ref()).await;
    let other_project = create_project(db.as_ref()).await;
    let first = create_group(db.as_ref(), project, GroupSpec::default()).await;
    let second = create_group(db.as_ref(), project, GroupSpec::default()).await;
    let untouched = create_group(db.as_ref(), project, GroupSpec::default()).await;
    let elsewhere = create_group(db.as_ref(), other_project, GroupSpec::default()).await;

    let service = GroupWorkflowService::new(db.clone());
    let outcome = service
        .bulk_update_status(
            project,
            BulkGroupSelector::Ids(vec![first, second, first]),
            BulkGroupAction::Resolve,
            None,
            false,
        )
        .await
        .expect("bulk resolve succeeds");

    let mut updated = outcome.updated_ids.clone();
    updated.sort_unstable();
    let mut expected = vec![first, second];
    expected.sort_unstable();
    assert_eq!(updated, expected, "a repeated id is updated once");
    assert_eq!(outcome.skipped, 0);
    assert_eq!(outcome.remaining, 0);
    assert_eq!(status_of(db.as_ref(), first).await, "resolved");
    assert_eq!(status_of(db.as_ref(), second).await, "resolved");
    assert_eq!(status_of(db.as_ref(), untouched).await, "unresolved");
    assert_eq!(
        status_of(db.as_ref(), elsewhere).await,
        "unresolved",
        "another project's group is never touched"
    );
}

#[tokio::test]
async fn a_group_already_in_the_target_status_is_skipped_not_updated() {
    let test_db = test_db!("bulk skip");
    let db = test_db.connection_arc();
    let project = create_project(db.as_ref()).await;
    let open = create_group(db.as_ref(), project, GroupSpec::default()).await;
    let already = create_group(
        db.as_ref(),
        project,
        GroupSpec {
            status: "resolved",
            ..GroupSpec::default()
        },
    )
    .await;

    let outcome = GroupWorkflowService::new(db.clone())
        .bulk_update_status(
            project,
            BulkGroupSelector::Ids(vec![open, already]),
            BulkGroupAction::Resolve,
            None,
            false,
        )
        .await
        .expect("bulk resolve succeeds");

    assert_eq!(outcome.updated_ids, vec![open]);
    assert_eq!(
        outcome.skipped, 1,
        "the already-resolved group counts as skipped"
    );
}

#[tokio::test]
async fn an_id_from_another_project_fails_the_whole_request_before_writing() {
    let test_db = test_db!("bulk cross-project rejection");
    let db = test_db.connection_arc();
    let project = create_project(db.as_ref()).await;
    let other_project = create_project(db.as_ref()).await;
    let mine = create_group(db.as_ref(), project, GroupSpec::default()).await;
    let theirs = create_group(db.as_ref(), other_project, GroupSpec::default()).await;

    let error = GroupWorkflowService::new(db.clone())
        .bulk_update_status(
            project,
            BulkGroupSelector::Ids(vec![mine, theirs]),
            BulkGroupAction::Resolve,
            None,
            false,
        )
        .await
        .expect_err("a foreign group id must be refused");

    match error {
        ErrorTrackingError::GroupsNotFound {
            project_id,
            group_ids,
        } => {
            assert_eq!(project_id, project);
            assert_eq!(group_ids, vec![theirs]);
        }
        other => panic!("expected GroupsNotFound, got {other:?}"),
    }
    assert_eq!(
        status_of(db.as_ref(), mine).await,
        "unresolved",
        "nothing is written when one id is rejected"
    );
}

#[tokio::test]
async fn bulk_by_filter_matches_status_and_search_and_reports_nothing_remaining() {
    let test_db = test_db!("bulk filter");
    let db = test_db.connection_arc();
    let project = create_project(db.as_ref()).await;
    let matching = create_group(
        db.as_ref(),
        project,
        GroupSpec {
            title: "TimeoutError: upstream took too long",
            error_type: "TimeoutError",
            ..GroupSpec::default()
        },
    )
    .await;
    let wrong_text = create_group(
        db.as_ref(),
        project,
        GroupSpec {
            title: "ValueError: bad input",
            error_type: "ValueError",
            ..GroupSpec::default()
        },
    )
    .await;
    let wrong_status = create_group(
        db.as_ref(),
        project,
        GroupSpec {
            title: "TimeoutError: upstream took too long",
            error_type: "TimeoutError",
            status: "ignored",
            ..GroupSpec::default()
        },
    )
    .await;

    let outcome = GroupWorkflowService::new(db.clone())
        .bulk_update_status(
            project,
            BulkGroupSelector::Filter(BulkGroupFilter {
                status: Some("unresolved".to_string()),
                search: Some("timeouterror".to_string()),
                ..BulkGroupFilter::default()
            }),
            BulkGroupAction::Resolve,
            None,
            false,
        )
        .await
        .expect("bulk resolve by filter succeeds");

    assert_eq!(outcome.updated_ids, vec![matching]);
    assert_eq!(
        outcome.remaining, 0,
        "nothing is left for a follow-up request"
    );
    assert_eq!(status_of(db.as_ref(), wrong_text).await, "unresolved");
    assert_eq!(status_of(db.as_ref(), wrong_status).await, "ignored");
}

#[tokio::test]
async fn a_filter_search_treats_wildcards_as_literal_text() {
    let test_db = test_db!("bulk filter escaping");
    let db = test_db.connection_arc();
    let project = create_project(db.as_ref()).await;
    let literal = create_group(
        db.as_ref(),
        project,
        GroupSpec {
            title: "Error: 100% of requests failed",
            ..GroupSpec::default()
        },
    )
    .await;
    let other = create_group(
        db.as_ref(),
        project,
        GroupSpec {
            title: "Error: everything else",
            ..GroupSpec::default()
        },
    )
    .await;

    let outcome = GroupWorkflowService::new(db.clone())
        .bulk_update_status(
            project,
            BulkGroupSelector::Filter(BulkGroupFilter {
                search: Some("100%".to_string()),
                ..BulkGroupFilter::default()
            }),
            BulkGroupAction::Ignore,
            None,
            false,
        )
        .await
        .expect("bulk ignore by filter succeeds");

    assert_eq!(
        outcome.updated_ids,
        vec![literal],
        "`%` must match itself, not every group"
    );
    assert_eq!(status_of(db.as_ref(), other).await, "unresolved");
}

#[tokio::test]
async fn an_event_window_filter_selects_only_groups_with_events_in_it() {
    let test_db = test_db!("bulk window filter");
    let db = test_db.connection_arc();
    let db_ref = db.as_ref();
    let project = create_project(db_ref).await;
    let inside = create_group(db_ref, project, GroupSpec::default()).await;
    let outside = create_group(db_ref, project, GroupSpec::default()).await;
    let now = Utc::now();
    for (group, timestamp) in [
        (inside, now - chrono::Duration::minutes(5)),
        (outside, now - chrono::Duration::days(30)),
    ] {
        db_ref
            .execute(Statement::from_sql_and_values(
                DatabaseBackend::Postgres,
                "INSERT INTO error_events (error_group_id, project_id, fingerprint_hash, \
                 timestamp, exception_type, source, created_at) \
                 VALUES ($1,$2,$3,$4,$5,'test',NOW())",
                [
                    group.into(),
                    project.into(),
                    format!("fp-{group}").into(),
                    timestamp.into(),
                    "TypeError".into(),
                ],
            ))
            .await
            .expect("insert error event");
    }

    let outcome = GroupWorkflowService::new(db.clone())
        .bulk_update_status(
            project,
            BulkGroupSelector::Filter(BulkGroupFilter {
                start_date: Some(now - chrono::Duration::hours(1)),
                end_date: Some(now),
                ..BulkGroupFilter::default()
            }),
            BulkGroupAction::Resolve,
            None,
            false,
        )
        .await
        .expect("bulk resolve in a window succeeds");

    assert_eq!(outcome.updated_ids, vec![inside]);
    assert_eq!(status_of(db_ref, outside).await, "unresolved");
}

#[tokio::test]
async fn an_empty_or_oversized_id_list_is_a_validation_error() {
    let test_db = test_db!("bulk selection limits");
    let db = test_db.connection_arc();
    let service = GroupWorkflowService::new(db.clone());

    let empty = service
        .bulk_update_status(
            1,
            BulkGroupSelector::Ids(vec![]),
            BulkGroupAction::Resolve,
            None,
            false,
        )
        .await
        .expect_err("an empty id list is refused");
    assert!(
        matches!(empty, ErrorTrackingError::Validation(_)),
        "{empty}"
    );

    let too_many: Vec<i32> = (1..=(BULK_GROUP_LIMIT as i32 + 1)).collect();
    let oversized = service
        .bulk_update_status(
            1,
            BulkGroupSelector::Ids(too_many),
            BulkGroupAction::Resolve,
            None,
            false,
        )
        .await
        .expect_err("more ids than the cap is refused");
    assert!(
        oversized
            .to_string()
            .contains(&BULK_GROUP_LIMIT.to_string()),
        "the error names the cap: {oversized}"
    );
}

#[tokio::test]
async fn a_half_open_date_range_and_an_unknown_status_are_refused() {
    let test_db = test_db!("bulk filter validation");
    let db = test_db.connection_arc();
    let service = GroupWorkflowService::new(db.clone());

    let half_open = service
        .bulk_update_status(
            1,
            BulkGroupSelector::Filter(BulkGroupFilter {
                start_date: Some(Utc::now()),
                ..BulkGroupFilter::default()
            }),
            BulkGroupAction::Resolve,
            None,
            false,
        )
        .await
        .expect_err("a start without an end is refused");
    assert!(half_open.to_string().contains("start_date"), "{half_open}");

    let unknown_status = service
        .bulk_update_status(
            1,
            BulkGroupSelector::Filter(BulkGroupFilter {
                status: Some("nonsense".to_string()),
                ..BulkGroupFilter::default()
            }),
            BulkGroupAction::Resolve,
            None,
            false,
        )
        .await
        .expect_err("an unknown status filter is refused");
    assert!(
        unknown_status.to_string().contains("nonsense"),
        "{unknown_status}"
    );
}

#[test]
fn every_action_maps_to_a_known_group_status() {
    for action in [
        BulkGroupAction::Resolve,
        BulkGroupAction::Unresolve,
        BulkGroupAction::Ignore,
    ] {
        validate_group_status(action.target_status())
            .unwrap_or_else(|error| panic!("{action:?} target status is invalid: {error}"));
    }
}
