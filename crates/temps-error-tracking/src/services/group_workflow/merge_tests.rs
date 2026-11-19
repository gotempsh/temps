// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Merge, unmerge and split against a real database. The properties that
//! matter: future events follow the alias, counts stay exact across a merge
//! and its reversal, and neither operation changes a group's status.

use super::merge::*;
use super::tests::{create_group, create_project, status_of, GroupSpec};
use super::*;
use crate::services::error_ingestion_service::ErrorIngestionService;
use crate::services::types::CreateErrorEventData;
use sea_orm::{ConnectionTrait, EntityTrait};
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

/// Register a fingerprint alias for a group, as ingest does.
async fn add_fingerprint(db: &DatabaseConnection, project_id: i32, group_id: i32, hash: &str) {
    db.execute(Statement::from_sql_and_values(
        DatabaseBackend::Postgres,
        "INSERT INTO error_group_fingerprints(project_id, fingerprint_hash, error_group_id) \
         VALUES ($1, $2, $3)",
        [project_id.into(), hash.into(), group_id.into()],
    ))
    .await
    .expect("insert fingerprint alias");
}

async fn group_of_fingerprint(db: &DatabaseConnection, project_id: i32, hash: &str) -> Option<i32> {
    db.query_one(Statement::from_sql_and_values(
        DatabaseBackend::Postgres,
        "SELECT error_group_id FROM error_group_fingerprints \
         WHERE project_id = $1 AND fingerprint_hash = $2",
        [project_id.into(), hash.into()],
    ))
    .await
    .expect("read fingerprint alias")
    .map(|row| row.try_get::<i32>("", "error_group_id").expect("group id"))
}

async fn count_of(db: &DatabaseConnection, group_id: i32) -> i32 {
    error_groups::Entity::find_by_id(group_id)
        .one(db)
        .await
        .expect("load group")
        .expect("group exists")
        .total_count
}

async fn merged_into(db: &DatabaseConnection, group_id: i32) -> Option<i32> {
    error_groups::Entity::find_by_id(group_id)
        .one(db)
        .await
        .expect("load group")
        .expect("group exists")
        .merged_into_group_id
}

#[tokio::test]
async fn merging_moves_the_alias_and_the_count_and_unmerging_restores_both() {
    let test_db = test_db!("merge and unmerge");
    let db = test_db.connection_arc();
    let db_ref = db.as_ref();
    let project = create_project(db_ref).await;
    let target = create_group(db_ref, project, GroupSpec::default()).await;
    let source = create_group(db_ref, project, GroupSpec::default()).await;
    add_fingerprint(db_ref, project, target, "target-fp").await;
    add_fingerprint(db_ref, project, source, "source-fp").await;
    let target_count_before = count_of(db_ref, target).await;
    let source_count_before = count_of(db_ref, source).await;

    let service = GroupWorkflowService::new(db.clone());
    let merge = service
        .merge_groups(project, target, vec![source])
        .await
        .expect("merge succeeds");

    assert_eq!(merge.target_group_id, target);
    assert_eq!(merge.merged_group_ids, vec![source]);
    assert_eq!(merge.moved_fingerprints, 1);
    assert_eq!(
        group_of_fingerprint(db_ref, project, "source-fp").await,
        Some(target),
        "the merged group's future events route to the survivor"
    );
    assert_eq!(
        count_of(db_ref, target).await,
        target_count_before + source_count_before,
        "the survivor accounts for both groups' events"
    );
    assert_eq!(
        count_of(db_ref, source).await,
        source_count_before,
        "the merged group keeps its own count so unmerge is exact"
    );
    assert_eq!(merged_into(db_ref, source).await, Some(target));
    assert_eq!(
        status_of(db_ref, source).await,
        "unresolved",
        "a merge never changes a group's status"
    );

    let unmerge = service
        .unmerge_group(project, source)
        .await
        .expect("unmerge succeeds");
    assert_eq!(unmerge.target_group_id, target);
    assert_eq!(unmerge.moved_fingerprints, 1);
    assert_eq!(
        group_of_fingerprint(db_ref, project, "source-fp").await,
        Some(source)
    );
    assert_eq!(count_of(db_ref, target).await, target_count_before);
    assert_eq!(count_of(db_ref, source).await, source_count_before);
    assert_eq!(merged_into(db_ref, source).await, None);
}

#[tokio::test]
async fn a_merged_group_is_hidden_from_the_list_and_the_status_counts() {
    let test_db = test_db!("merged group visibility");
    let db = test_db.connection_arc();
    let db_ref = db.as_ref();
    let project = create_project(db_ref).await;
    let target = create_group(db_ref, project, GroupSpec::default()).await;
    let source = create_group(db_ref, project, GroupSpec::default()).await;

    let crud = crate::services::error_crud_service::ErrorCRUDService::new(db.clone());
    let analytics =
        crate::services::error_analytics_service::ErrorAnalyticsService::new(db.clone());
    let (before, total_before) = crud
        .list_error_groups(project, None, None, None, None, None, None, None, None)
        .await
        .expect("list before merge");
    assert_eq!(before.len(), 2);
    assert_eq!(total_before, 2);

    GroupWorkflowService::new(db.clone())
        .merge_groups(project, target, vec![source])
        .await
        .expect("merge succeeds");

    let (after, total_after) = crud
        .list_error_groups(project, None, None, None, None, None, None, None, None)
        .await
        .expect("list after merge");
    assert_eq!(total_after, 1);
    assert_eq!(after.iter().map(|g| g.id).collect::<Vec<_>>(), vec![target]);
    let stats = analytics
        .get_error_stats(project, None)
        .await
        .expect("status counts");
    assert_eq!(stats.total_groups, 1);
    assert_eq!(stats.unresolved_groups, 1);

    // It is still readable by id, so a link to it can explain where it went.
    let merged = crud
        .get_error_group(source, project)
        .await
        .expect("the merged group is still readable");
    assert_eq!(merged.id, source);
}

#[tokio::test]
async fn an_event_for_a_merged_fingerprint_lands_in_the_surviving_group() {
    let test_db = test_db!("merge ingest routing");
    let db = test_db.connection_arc();
    let db_ref = db.as_ref();
    let project = create_project(db_ref).await;
    let ingestion = ErrorIngestionService::new(db.clone());

    // Two different errors, so two groups with two fingerprints.
    let first = CreateErrorEventData {
        source: Some("sentry".to_string()),
        exception_type: Some("TypeError".to_string()),
        exception_value: Some("first failure".to_string()),
        project_id: project,
        ..Default::default()
    };
    let second = CreateErrorEventData {
        source: Some("sentry".to_string()),
        exception_type: Some("RangeError".to_string()),
        exception_value: Some("second failure".to_string()),
        project_id: project,
        ..Default::default()
    };
    let target = ingestion
        .process_error_event(first.clone())
        .await
        .expect("ingest the first error");
    let source = ingestion
        .process_error_event(second.clone())
        .await
        .expect("ingest the second error");
    assert_ne!(target, source, "the two errors group separately");

    GroupWorkflowService::new(db.clone())
        .merge_groups(project, target, vec![source])
        .await
        .expect("merge succeeds");
    let count_after_merge = count_of(db_ref, target).await;

    let landed = ingestion
        .process_error_event(second)
        .await
        .expect("ingest the merged error again");
    assert_eq!(
        landed, target,
        "an event whose fingerprint was merged lands in the surviving group"
    );
    assert_eq!(count_of(db_ref, target).await, count_after_merge + 1);
    assert_eq!(
        status_of(db_ref, target).await,
        "unresolved",
        "ingest after a merge behaves as it did before"
    );
}

#[tokio::test]
async fn merge_refuses_a_group_that_is_already_merged_or_a_merge_target() {
    let test_db = test_db!("merge conflicts");
    let db = test_db.connection_arc();
    let db_ref = db.as_ref();
    let project = create_project(db_ref).await;
    let target = create_group(db_ref, project, GroupSpec::default()).await;
    let source = create_group(db_ref, project, GroupSpec::default()).await;
    let third = create_group(db_ref, project, GroupSpec::default()).await;
    let service = GroupWorkflowService::new(db.clone());
    service
        .merge_groups(project, target, vec![source])
        .await
        .expect("merge succeeds");

    let already = service
        .merge_groups(project, third, vec![source])
        .await
        .expect_err("a merged-away group cannot be merged again");
    assert!(
        matches!(
            already,
            ErrorTrackingError::GroupAlreadyMerged { group_id, merged_into_group_id }
                if group_id == source && merged_into_group_id == target
        ),
        "{already}"
    );

    let is_target = service
        .merge_groups(project, third, vec![target])
        .await
        .expect_err("a group that absorbed another cannot be merged away");
    assert!(
        matches!(is_target, ErrorTrackingError::GroupIsMergeTarget { group_id } if group_id == target),
        "{is_target}"
    );

    let into_merged = service
        .merge_groups(project, source, vec![third])
        .await
        .expect_err("a merged-away group cannot be a merge target");
    assert!(
        matches!(into_merged, ErrorTrackingError::GroupAlreadyMerged { group_id, .. } if group_id == source),
        "{into_merged}"
    );

    let itself = service
        .merge_groups(project, third, vec![third])
        .await
        .expect_err("a group cannot be merged into itself");
    assert!(
        matches!(itself, ErrorTrackingError::Validation(_)),
        "{itself}"
    );
}

#[tokio::test]
async fn merge_refuses_a_group_from_another_project_and_writes_nothing() {
    let test_db = test_db!("merge project isolation");
    let db = test_db.connection_arc();
    let db_ref = db.as_ref();
    let project = create_project(db_ref).await;
    let other_project = create_project(db_ref).await;
    let target = create_group(db_ref, project, GroupSpec::default()).await;
    let theirs = create_group(db_ref, other_project, GroupSpec::default()).await;
    add_fingerprint(db_ref, other_project, theirs, "their-fp").await;
    let count_before = count_of(db_ref, target).await;

    let error = GroupWorkflowService::new(db.clone())
        .merge_groups(project, target, vec![theirs])
        .await
        .expect_err("another project's group must be refused");
    assert!(
        matches!(
            error,
            ErrorTrackingError::GroupsNotFound { ref group_ids, .. } if group_ids == &vec![theirs]
        ),
        "{error}"
    );
    assert_eq!(count_of(db_ref, target).await, count_before);
    assert_eq!(
        group_of_fingerprint(db_ref, other_project, "their-fp").await,
        Some(theirs)
    );
    assert_eq!(merged_into(db_ref, theirs).await, None);
}

#[tokio::test]
async fn unmerge_refuses_a_group_that_is_not_merged() {
    let test_db = test_db!("unmerge conflict");
    let db = test_db.connection_arc();
    let project = create_project(db.as_ref()).await;
    let group = create_group(db.as_ref(), project, GroupSpec::default()).await;

    let error = GroupWorkflowService::new(db.clone())
        .unmerge_group(project, group)
        .await
        .expect_err("a group that is not merged cannot be unmerged");
    assert!(
        matches!(error, ErrorTrackingError::GroupNotMerged { group_id } if group_id == group),
        "{error}"
    );
}

#[tokio::test]
async fn splitting_sends_future_events_of_the_moved_fingerprint_to_the_new_group() {
    let test_db = test_db!("split");
    let db = test_db.connection_arc();
    let db_ref = db.as_ref();
    let project = create_project(db_ref).await;
    let group = create_group(db_ref, project, GroupSpec::default()).await;
    add_fingerprint(db_ref, project, group, "stays").await;
    add_fingerprint(db_ref, project, group, "moves").await;
    let count_before = count_of(db_ref, group).await;

    let service = GroupWorkflowService::new(db.clone());
    let split = service
        .split_group(
            project,
            group,
            vec!["moves".to_string()],
            Some("Second bug".to_string()),
        )
        .await
        .expect("split succeeds");

    assert_eq!(split.moved_fingerprints, 1);
    assert_eq!(
        group_of_fingerprint(db_ref, project, "moves").await,
        Some(split.new_group_id)
    );
    assert_eq!(
        group_of_fingerprint(db_ref, project, "stays").await,
        Some(group)
    );
    assert_eq!(
        count_of(db_ref, group).await,
        count_before,
        "stored events stay with the group they were ingested into"
    );
    let new_group = error_groups::Entity::find_by_id(split.new_group_id)
        .one(db_ref)
        .await
        .expect("load new group")
        .expect("new group exists");
    assert_eq!(new_group.title, "Second bug");
    assert_eq!(new_group.total_count, 0);
    assert_eq!(new_group.status, "unresolved");
    assert_eq!(new_group.project_id, project);
}

#[tokio::test]
async fn a_split_must_leave_the_group_a_fingerprint_and_cannot_take_unknown_ones() {
    let test_db = test_db!("split validation");
    let db = test_db.connection_arc();
    let db_ref = db.as_ref();
    let project = create_project(db_ref).await;
    let group = create_group(db_ref, project, GroupSpec::default()).await;
    add_fingerprint(db_ref, project, group, "only").await;
    let service = GroupWorkflowService::new(db.clone());

    let everything = service
        .split_group(project, group, vec!["only".to_string()], None)
        .await
        .expect_err("splitting every fingerprint is refused");
    assert!(
        matches!(everything, ErrorTrackingError::Validation(_)),
        "{everything}"
    );

    let unknown = service
        .split_group(project, group, vec!["not-here".to_string()], None)
        .await
        .expect_err("an unknown fingerprint is refused");
    assert!(
        matches!(
            unknown,
            ErrorTrackingError::FingerprintsNotInGroup { ref fingerprints, .. }
                if fingerprints == &vec!["not-here".to_string()]
        ),
        "{unknown}"
    );
}

#[tokio::test]
async fn a_split_cannot_take_a_fingerprint_that_belongs_to_a_merge() {
    let test_db = test_db!("split versus merge");
    let db = test_db.connection_arc();
    let db_ref = db.as_ref();
    let project = create_project(db_ref).await;
    let target = create_group(db_ref, project, GroupSpec::default()).await;
    let source = create_group(db_ref, project, GroupSpec::default()).await;
    add_fingerprint(db_ref, project, target, "own").await;
    add_fingerprint(db_ref, project, target, "own-two").await;
    add_fingerprint(db_ref, project, source, "merged-in").await;
    let service = GroupWorkflowService::new(db.clone());
    service
        .merge_groups(project, target, vec![source])
        .await
        .expect("merge succeeds");

    let error = service
        .split_group(project, target, vec!["merged-in".to_string()], None)
        .await
        .expect_err("a merged-in fingerprint belongs to the merge");
    assert!(
        matches!(error, ErrorTrackingError::FingerprintsNotInGroup { .. }),
        "{error}"
    );

    let listed = service
        .list_group_fingerprints(project, target)
        .await
        .expect("list fingerprints");
    assert_eq!(listed.len(), 3);
    assert_eq!(
        listed
            .iter()
            .filter(|f| f.merged_from_group_id == Some(source))
            .map(|f| f.fingerprint_hash.as_str())
            .collect::<Vec<_>>(),
        vec!["merged-in"]
    );
    assert_eq!(
        service
            .merged_group_ids(project, target)
            .await
            .expect("merged group ids"),
        vec![source]
    );
}

#[tokio::test]
async fn listing_fingerprints_refuses_a_group_from_another_project() {
    let test_db = test_db!("fingerprint listing isolation");
    let db = test_db.connection_arc();
    let db_ref = db.as_ref();
    let project = create_project(db_ref).await;
    let other_project = create_project(db_ref).await;
    let theirs = create_group(db_ref, other_project, GroupSpec::default()).await;

    let error = GroupWorkflowService::new(db.clone())
        .list_group_fingerprints(project, theirs)
        .await
        .expect_err("another project's group must not be readable");
    assert!(
        matches!(error, ErrorTrackingError::GroupNotFound),
        "{error}"
    );
}

#[tokio::test]
async fn merge_and_split_selections_are_capped() {
    let test_db = test_db!("merge and split caps");
    let db = test_db.connection_arc();
    let service = GroupWorkflowService::new(db.clone());

    let too_many_sources: Vec<i32> = (1..=(MAX_MERGE_SOURCES as i32 + 1)).collect();
    let merge = service
        .merge_groups(1, 9999, too_many_sources)
        .await
        .expect_err("more sources than the cap is refused");
    assert!(
        merge.to_string().contains(&MAX_MERGE_SOURCES.to_string()),
        "{merge}"
    );

    let too_many_fingerprints: Vec<String> = (0..=MAX_SPLIT_FINGERPRINTS)
        .map(|index| format!("fp-{index}"))
        .collect();
    let split = service
        .split_group(1, 9999, too_many_fingerprints, None)
        .await
        .expect_err("more fingerprints than the cap is refused");
    assert!(
        split
            .to_string()
            .contains(&MAX_SPLIT_FINGERPRINTS.to_string()),
        "{split}"
    );

    let empty = service
        .merge_groups(1, 9999, vec![])
        .await
        .expect_err("an empty merge is refused");
    assert!(
        matches!(empty, ErrorTrackingError::Validation(_)),
        "{empty}"
    );
}

#[tokio::test]
async fn an_event_that_read_the_alias_before_a_merge_is_counted_in_the_survivor() {
    let test_db = test_db!("merge race");
    let db = test_db.connection_arc();
    let db_ref = db.as_ref();
    let project = create_project(db_ref).await;
    let target = create_group(db_ref, project, GroupSpec::default()).await;
    let source = create_group(db_ref, project, GroupSpec::default()).await;
    add_fingerprint(db_ref, project, source, "fp-race").await;
    GroupWorkflowService::new(db.clone())
        .merge_groups(project, target, vec![source])
        .await
        .expect("merge succeeds");

    // Ingest resolved the alias to `source` just before the merge
    // committed; locking that group must land the event on the survivor.
    let (locked, status) = ErrorIngestionService::lock_group_following_merge(db_ref, source)
        .await
        .expect("lock the group");
    assert_eq!(locked, target);
    assert_eq!(status, "unresolved");
    let (locked, _) = ErrorIngestionService::lock_group_following_merge(db_ref, target)
        .await
        .expect("lock the survivor");
    assert_eq!(
        locked, target,
        "a group that is not merged is its own target"
    );
}

#[tokio::test]
async fn a_retried_event_after_a_merge_neither_counts_nor_reopens() {
    let test_db = test_db!("merge retry");
    let db = test_db.connection_arc();
    let db_ref = db.as_ref();
    let project = create_project(db_ref).await;
    let ingestion = ErrorIngestionService::new(db.clone());
    let event = |kind: &str, id: &str| CreateErrorEventData {
        source: Some("sentry".to_string()),
        exception_type: Some(kind.to_string()),
        exception_value: Some(format!("{kind} failure")),
        raw_sentry_event: Some(serde_json::json!({ "event_id": id })),
        project_id: project,
        ..Default::default()
    };
    let target = ingestion
        .process_error_event(event("TypeError", "0123456789abcdef0123456789abcdef"))
        .await
        .expect("ingest the first error");
    let retried = event("RangeError", "fedcba9876543210fedcba9876543210");
    let source = ingestion
        .process_error_event(retried.clone())
        .await
        .expect("ingest the second error");
    let service = GroupWorkflowService::new(db.clone());
    service
        .merge_groups(project, target, vec![source])
        .await
        .expect("merge succeeds");
    service
        .bulk_update_status(
            project,
            BulkGroupSelector::Ids(vec![target]),
            BulkGroupAction::Resolve,
            None,
        )
        .await
        .expect("resolve the survivor");
    let before = count_of(db_ref, target).await;

    // The SDK retries an event it already delivered before the merge.
    ingestion
        .process_error_event(retried)
        .await
        .expect("a retry is accepted");
    assert_eq!(
        count_of(db_ref, target).await,
        before,
        "a retry is never counted twice"
    );
    assert_eq!(
        status_of(db_ref, target).await,
        "resolved",
        "a retried event does not reopen the surviving group"
    );
}
