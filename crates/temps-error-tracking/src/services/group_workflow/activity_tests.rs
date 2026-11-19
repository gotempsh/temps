// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Comments and the activity log against a real database (ADR 111, D1.7):
//! comment lifecycle and authorship, newest-first paging, project isolation,
//! and one `regressed` entry per reopening.

use super::tests::{create_group, create_project, GroupSpec};
use super::*;
use crate::services::error_tracking_service::ErrorTrackingService;
use crate::services::types::CreateErrorEventData;
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

#[tokio::test]
async fn a_comment_is_edited_only_by_its_author_and_deleted_by_a_moderator() {
    let test_db = test_db!("comment lifecycle");
    let db = test_db.connection_arc();
    let project = create_project(db.as_ref()).await;
    let group = create_group(db.as_ref(), project, GroupSpec::default()).await;
    let author = create_user(db.as_ref(), "ines").await;
    let other = create_user(db.as_ref(), "joel").await;
    let service = GroupWorkflowService::new(db.clone());

    let comment = service
        .add_comment(project, group, author, " began after the queue change ")
        .await
        .expect("comment");
    assert_eq!(comment.kind, ActivityKind::Comment);
    assert_eq!(
        comment.body.as_deref(),
        Some("began after the queue change")
    );
    assert_eq!(comment.actor_name.as_deref(), Some("ines"));
    assert_eq!(comment.edited_at, None);

    let error = service
        .edit_comment(project, group, comment.id, other, "not mine")
        .await
        .expect_err("another user's comment");
    assert!(
        matches!(error, ErrorTrackingError::NotCommentAuthor { comment_id, .. } if comment_id == comment.id),
        "{error:?}"
    );
    let edited = service
        .edit_comment(
            project,
            group,
            comment.id,
            author,
            "began after the queue rollout",
        )
        .await
        .expect("edit");
    assert_eq!(
        edited.body.as_deref(),
        Some("began after the queue rollout")
    );
    assert!(edited.edited_at.is_some());

    let error = service
        .delete_comment(project, group, comment.id, other, false)
        .await
        .expect_err("not the author, not a moderator");
    assert!(error.to_string().contains("or an administrator"), "{error}");
    service
        .delete_comment(project, group, comment.id, other, true)
        .await
        .expect("a moderator deletes it");
    let error = service
        .delete_comment(project, group, comment.id, author, false)
        .await
        .expect_err("already gone");
    assert!(
        matches!(error, ErrorTrackingError::CommentNotFound { group_id, comment_id }
            if group_id == group && comment_id == comment.id),
        "{error:?}"
    );
}

#[tokio::test]
async fn comments_are_validated_and_scoped_to_the_project() {
    let test_db = test_db!("comment validation");
    let db = test_db.connection_arc();
    let project = create_project(db.as_ref()).await;
    let other_project = create_project(db.as_ref()).await;
    let group = create_group(db.as_ref(), project, GroupSpec::default()).await;
    let user = create_user(db.as_ref(), "kai").await;
    let service = GroupWorkflowService::new(db.clone());

    for body in ["   ".to_string(), "x".repeat(MAX_COMMENT_CHARS + 1)] {
        let error = service
            .add_comment(project, group, user, &body)
            .await
            .expect_err("invalid body");
        assert!(
            matches!(error, ErrorTrackingError::Validation(_)),
            "{error:?}"
        );
    }
    let error = service
        .add_comment(other_project, group, user, "wrong project")
        .await
        .expect_err("group of another project");
    assert!(
        matches!(error, ErrorTrackingError::GroupNotInProject { project_id, group_id }
            if project_id == other_project && group_id == group),
        "{error:?}"
    );
    let comment = service
        .add_comment(project, group, user, "fine")
        .await
        .expect("comment");
    let error = service
        .edit_comment(other_project, group, comment.id, user, "moved")
        .await
        .expect_err("comment read through another project");
    assert!(
        matches!(error, ErrorTrackingError::CommentNotFound { .. }),
        "{error:?}"
    );
    let error = service
        .list_activity(other_project, group, 1, 20)
        .await
        .expect_err("list through another project");
    assert!(
        matches!(error, ErrorTrackingError::GroupNotInProject { .. }),
        "{error:?}"
    );

    // A bulk entry naming a group of another project skips it.
    service
        .record_activity(
            other_project,
            &[group],
            ActivityKind::StatusChanged,
            Some(user),
            ActivityDetails::default(),
        )
        .await
        .expect("record");
    assert_eq!(
        service
            .list_activity(project, group, 1, 20)
            .await
            .expect("list")
            .total,
        1,
        "only the comment"
    );
}

#[tokio::test]
async fn activity_pages_newest_first_and_records_many_groups_at_once() {
    let test_db = test_db!("activity paging");
    let db = test_db.connection_arc();
    let project = create_project(db.as_ref()).await;
    let first = create_group(db.as_ref(), project, GroupSpec::default()).await;
    let second = create_group(db.as_ref(), project, GroupSpec::default()).await;
    let user = create_user(db.as_ref(), "lou").await;
    let service = GroupWorkflowService::new(db.clone());

    service
        .record_activity(
            project,
            &[first, second],
            ActivityKind::StatusChanged,
            Some(user),
            ActivityDetails {
                status: Some("resolved".into()),
                bulk: true,
                ..Default::default()
            },
        )
        .await
        .expect("bulk entry");
    for n in 0..3 {
        service
            .add_comment(project, first, user, &format!("note {n}"))
            .await
            .expect("comment");
    }

    let page = service
        .list_activity(project, first, 1, 2)
        .await
        .expect("page 1");
    assert_eq!(page.total, 4);
    let bodies: Vec<_> = page.entries.iter().map(|e| e.body.as_deref()).collect();
    assert_eq!(bodies, vec![Some("note 2"), Some("note 1")]);
    let page = service
        .list_activity(project, first, 2, 2)
        .await
        .expect("page 2");
    assert_eq!(page.entries.len(), 2);
    let oldest = &page.entries[1];
    assert_eq!(oldest.kind, ActivityKind::StatusChanged);
    assert_eq!(oldest.details.status.as_deref(), Some("resolved"));
    assert!(oldest.details.bulk);
    assert_eq!(
        service
            .list_activity(project, second, 1, 20)
            .await
            .expect("second")
            .total,
        1,
        "the bulk entry landed on both groups"
    );
}

#[tokio::test]
async fn an_event_that_reopens_a_group_is_logged_once() {
    let test_db = test_db!("regression activity");
    let db = test_db.connection_arc();
    let project = create_project(db.as_ref()).await;
    let tracking = ErrorTrackingService::new(db.clone());
    let event = |release: &str| CreateErrorEventData {
        source: Some("sentry".to_string()),
        exception_type: Some("TypeError".to_string()),
        exception_value: Some("basket is undefined".to_string()),
        release_version: Some(release.to_string()),
        project_id: project,
        ..Default::default()
    };

    let receipt = tracking
        .process_error_event_with_receipt(event("1.0.0"))
        .await
        .expect("first event");
    let group = receipt.group_id;
    tracking
        .workflow
        .bulk_update_status(
            project,
            BulkGroupSelector::Ids(vec![group]),
            BulkGroupAction::Resolve,
            None,
            false,
        )
        .await
        .expect("resolve");
    for _ in 0..2 {
        tracking
            .process_error_event_with_receipt(event("1.0.1"))
            .await
            .expect("event after resolving");
    }

    let page = tracking
        .workflow
        .list_activity(project, group, 1, 20)
        .await
        .expect("activity");
    let regressions: Vec<_> = page
        .entries
        .iter()
        .filter(|entry| entry.kind == ActivityKind::Regressed)
        .collect();
    assert_eq!(
        regressions.len(),
        1,
        "the second event finds the group open"
    );
    assert_eq!(regressions[0].actor_user_id, None);
    assert_eq!(regressions[0].details.status.as_deref(), Some("resolved"));
    assert_eq!(regressions[0].details.release.as_deref(), Some("1.0.1"));
}
