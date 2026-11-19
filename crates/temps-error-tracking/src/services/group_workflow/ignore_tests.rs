// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! "Ignore until" against a real database: an ignored group stays ignored
//! while its conditions hold, reopens on the event that meets one, and a
//! plain ignore keeps reopening on the next event as it always did.

use super::tests::{create_project, status_of};
use super::*;
use crate::services::error_ingestion_service::ErrorIngestionService;
use crate::services::types::CreateErrorEventData;
use chrono::Duration;
use sea_orm::EntityTrait;
use temps_entities::error_groups;
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

fn event(project_id: i32, user: Option<&str>) -> CreateErrorEventData {
    CreateErrorEventData {
        source: Some("sentry".to_string()),
        exception_type: Some("TypeError".to_string()),
        exception_value: Some("checkout total is undefined".to_string()),
        user_id: user.map(str::to_string),
        project_id,
        ..Default::default()
    }
}

async fn group(db: &DatabaseConnection, id: i32) -> error_groups::Model {
    error_groups::Entity::find_by_id(id)
        .one(db)
        .await
        .expect("load group")
        .expect("group exists")
}

/// Ingest one event, then ignore its group with `conditions`.
async fn ignored_group(
    db: &Arc<DatabaseConnection>,
    project: i32,
    conditions: IgnoreConditions,
) -> (ErrorIngestionService, i32) {
    let ingestion = ErrorIngestionService::new(db.clone());
    let id = ingestion
        .process_error_event(event(project, None))
        .await
        .expect("ingest the first event");
    GroupWorkflowService::new(db.clone())
        .bulk_update_status(
            project,
            BulkGroupSelector::Ids(vec![id]),
            BulkGroupAction::Ignore,
            Some(conditions),
        )
        .await
        .expect("ignore the group");
    (ingestion, id)
}

#[tokio::test]
async fn a_plain_ignore_still_reopens_on_the_next_event() {
    let test_db = test_db!("plain ignore");
    let db = test_db.connection_arc();
    let project = create_project(db.as_ref()).await;
    let (ingestion, id) = ignored_group(&db, project, IgnoreConditions::default()).await;
    assert_eq!(status_of(db.as_ref(), id).await, "ignored");

    ingestion
        .process_error_event(event(project, None))
        .await
        .expect("ingest");
    assert_eq!(status_of(db.as_ref(), id).await, "unresolved");
}

#[tokio::test]
async fn an_ignore_for_more_occurrences_reopens_on_the_one_that_reaches_it() {
    let test_db = test_db!("ignore by count");
    let db = test_db.connection_arc();
    let project = create_project(db.as_ref()).await;
    let conditions = IgnoreConditions {
        event_count: Some(2),
        ..Default::default()
    };
    let (ingestion, id) = ignored_group(&db, project, conditions).await;
    let ignored = group(db.as_ref(), id).await;
    assert_eq!(
        ignored.ignore_until_event_count,
        Some(ignored.total_count + 2),
        "a relative count is stored as the total that reopens the group"
    );

    ingestion
        .process_error_event(event(project, None))
        .await
        .expect("ingest");
    assert_eq!(status_of(db.as_ref(), id).await, "ignored", "1 of 2 more");

    ingestion
        .process_error_event(event(project, None))
        .await
        .expect("ingest");
    let reopened = group(db.as_ref(), id).await;
    assert_eq!(reopened.status, "unresolved", "2 of 2 more reopens it");
    assert_eq!(
        reopened.ignore_until_event_count, None,
        "conditions are cleared"
    );
    assert_eq!(
        reopened.total_count,
        ignored.total_count + 2,
        "every event counted"
    );
}

#[tokio::test]
async fn an_ignore_until_new_users_counts_each_user_once() {
    let test_db = test_db!("ignore by users");
    let db = test_db.connection_arc();
    let project = create_project(db.as_ref()).await;
    let conditions = IgnoreConditions {
        user_count: Some(2),
        ..Default::default()
    };
    let (ingestion, id) = ignored_group(&db, project, conditions).await;

    for _ in 0..3 {
        ingestion
            .process_error_event(event(project, Some("user-a")))
            .await
            .expect("ingest");
    }
    ingestion
        .process_error_event(event(project, None))
        .await
        .expect("an event without a user");
    let waiting = group(db.as_ref(), id).await;
    assert_eq!(waiting.status, "ignored", "one distinct user so far");
    assert_eq!(waiting.ignore_user_count, 1);

    ingestion
        .process_error_event(event(project, Some("user-b")))
        .await
        .expect("ingest");
    let reopened = group(db.as_ref(), id).await;
    assert_eq!(reopened.status, "unresolved", "the second user reopens it");
    assert_eq!(reopened.ignore_user_count, 0, "the user count is cleared");
    let kept: Vec<_> = db
        .query_all(Statement::from_sql_and_values(
            DatabaseBackend::Postgres,
            "SELECT user_key FROM error_group_ignore_users WHERE error_group_id = $1",
            [id.into()],
        ))
        .await
        .expect("read ignore users");
    assert!(
        kept.is_empty(),
        "the users counted for the ignore are forgotten"
    );
}

#[tokio::test]
async fn an_ignore_until_a_time_holds_until_it_passes() {
    let test_db = test_db!("ignore by time");
    let db = test_db.connection_arc();
    let project = create_project(db.as_ref()).await;
    let conditions = IgnoreConditions {
        until: Some(Utc::now() + Duration::days(7)),
        ..Default::default()
    };
    let (ingestion, id) = ignored_group(&db, project, conditions).await;

    ingestion
        .process_error_event(event(project, None))
        .await
        .expect("ingest");
    assert_eq!(
        status_of(db.as_ref(), id).await,
        "ignored",
        "within the week"
    );

    // The week is over.
    db.execute(Statement::from_sql_and_values(
        DatabaseBackend::Postgres,
        "UPDATE error_groups SET ignore_until = NOW() - INTERVAL '1 minute' WHERE id = $1",
        [id.into()],
    ))
    .await
    .expect("expire the ignore");
    ingestion
        .process_error_event(event(project, None))
        .await
        .expect("ingest");
    assert_eq!(status_of(db.as_ref(), id).await, "unresolved");
}

#[tokio::test]
async fn an_ignore_forever_never_reopens() {
    let test_db = test_db!("ignore forever");
    let db = test_db.connection_arc();
    let project = create_project(db.as_ref()).await;
    let conditions = IgnoreConditions {
        forever: true,
        ..Default::default()
    };
    let (ingestion, id) = ignored_group(&db, project, conditions).await;
    for user in ["a", "b", "c"] {
        ingestion
            .process_error_event(event(project, Some(user)))
            .await
            .expect("ingest");
    }
    assert_eq!(status_of(db.as_ref(), id).await, "ignored");
}

#[tokio::test]
async fn a_new_ignore_replaces_the_conditions_and_resolving_clears_them() {
    let test_db = test_db!("ignore replacement");
    let db = test_db.connection_arc();
    let project = create_project(db.as_ref()).await;
    let (_ingestion, id) = ignored_group(
        &db,
        project,
        IgnoreConditions {
            event_count: Some(5),
            ..Default::default()
        },
    )
    .await;
    let service = GroupWorkflowService::new(db.clone());

    // Ignoring an ignored group again with conditions is not a no-op.
    let outcome = service
        .bulk_update_status(
            project,
            BulkGroupSelector::Ids(vec![id]),
            BulkGroupAction::Ignore,
            Some(IgnoreConditions {
                forever: true,
                ..Default::default()
            }),
        )
        .await
        .expect("re-ignore");
    assert_eq!(outcome.updated_ids, vec![id]);
    let reignored = group(db.as_ref(), id).await;
    assert!(reignored.ignore_forever);
    assert_eq!(reignored.ignore_until_event_count, None);

    service
        .bulk_update_status(
            project,
            BulkGroupSelector::Ids(vec![id]),
            BulkGroupAction::Resolve,
            None,
        )
        .await
        .expect("resolve");
    let resolved = group(db.as_ref(), id).await;
    assert_eq!(resolved.status, "resolved");
    assert!(
        !resolved.ignore_forever,
        "resolving clears the ignore conditions"
    );
}

#[tokio::test]
async fn conditions_on_a_status_other_than_ignored_are_refused() {
    let test_db = test_db!("ignore validation");
    let db = test_db.connection_arc();
    let project = create_project(db.as_ref()).await;
    let error = GroupWorkflowService::new(db.clone())
        .bulk_update_status(
            project,
            BulkGroupSelector::Ids(vec![1]),
            BulkGroupAction::Resolve,
            Some(IgnoreConditions {
                event_count: Some(3),
                ..Default::default()
            }),
        )
        .await
        .expect_err("conditions only go with ignore");
    assert!(
        matches!(error, ErrorTrackingError::Validation(_)),
        "{error}"
    );
}
