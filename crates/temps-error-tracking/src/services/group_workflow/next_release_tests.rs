// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! "Resolve in the next release" against a real database: a group resolved
//! after the current release stays resolved for events from that release, an
//! older one, or none, and reopens on the first event from a newer one.

use super::tests::{create_project, status_of};
use super::*;
use crate::services::error_ingestion_service::ErrorIngestionService;
use crate::services::types::CreateErrorEventData;
use sea_orm::{EntityTrait, Statement};
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

fn event(project_id: i32, release: Option<&str>) -> CreateErrorEventData {
    CreateErrorEventData {
        source: Some("sentry".to_string()),
        exception_type: Some("TypeError".to_string()),
        exception_value: Some("cart total is undefined".to_string()),
        release_version: release.map(str::to_string),
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

async fn resolve_in_next_release(
    db: &Arc<DatabaseConnection>,
    project: i32,
    id: i32,
) -> Result<BulkGroupOutcome, ErrorTrackingError> {
    GroupWorkflowService::new(db.clone())
        .bulk_update_status(
            project,
            BulkGroupSelector::Ids(vec![id]),
            BulkGroupAction::Resolve,
            None,
            true,
        )
        .await
}

/// A ready deployment of `project` carrying `commit_sha`, created `age_secs` ago.
async fn create_deployment(db: &DatabaseConnection, project: i32, commit_sha: &str, age_secs: i64) {
    db.execute(Statement::from_sql_and_values(
        DatabaseBackend::Postgres,
        "INSERT INTO environments (name, slug, subdomain, host, upstreams, created_at, updated_at, project_id) \
         SELECT 'production', 'production', 'prod-' || $1::text, 'prod.example.test', '[]', NOW(), NOW(), $1 \
         WHERE NOT EXISTS (SELECT 1 FROM environments WHERE project_id = $1)",
        [project.into()],
    ))
    .await
    .expect("insert environment");
    db.execute(Statement::from_sql_and_values(
        DatabaseBackend::Postgres,
        "INSERT INTO deployments (project_id, environment_id, created_at, updated_at, ready_at, \
                                  slug, state, metadata, commit_sha) \
         SELECT $1, e.id, NOW() - make_interval(secs => $3), NOW(), NOW(), \
                'deploy-' || $2, 'completed', '{}'::json, $2 \
         FROM environments e WHERE e.project_id = $1 LIMIT 1",
        [project.into(), commit_sha.into(), (age_secs as f64).into()],
    ))
    .await
    .expect("insert deployment");
}

#[tokio::test]
async fn resolving_in_the_next_release_needs_a_release() {
    let test_db = test_db!("next release without releases");
    let db = test_db.connection_arc();
    let project = create_project(db.as_ref()).await;
    let ingestion = ErrorIngestionService::new(db.clone());
    let id = ingestion
        .process_error_event(event(project, None))
        .await
        .expect("ingest");

    let error = resolve_in_next_release(&db, project, id)
        .await
        .expect_err("no release to come after");
    assert!(
        matches!(&error, ErrorTrackingError::Validation(message) if message.contains("no release")),
        "{error:?}"
    );
    assert_eq!(status_of(db.as_ref(), id).await, "unresolved");
}

#[tokio::test]
async fn only_an_event_from_a_newer_release_reopens_the_group() {
    let test_db = test_db!("next release reopen");
    let db = test_db.connection_arc();
    let project = create_project(db.as_ref()).await;
    let ingestion = ErrorIngestionService::new(db.clone());
    ingestion
        .process_error_event(event(project, Some("1.0.0")))
        .await
        .expect("ingest 1.0.0");
    let id = ingestion
        .process_error_event(event(project, Some("1.1.0")))
        .await
        .expect("ingest 1.1.0");

    resolve_in_next_release(&db, project, id)
        .await
        .expect("resolve in the next release");
    let resolved = group(db.as_ref(), id).await;
    assert_eq!(resolved.status, "resolved");
    assert!(resolved.resolved_in_next_release);
    let after: String = db
        .query_one(Statement::from_sql_and_values(
            DatabaseBackend::Postgres,
            "SELECT version FROM error_releases WHERE id = $1",
            [resolved.resolved_in_release_after_id.into()],
        ))
        .await
        .expect("read release")
        .expect("release exists")
        .try_get("", "version")
        .expect("version");
    assert_eq!(after, "1.1.0", "resolved after the newest release");

    // The fix has not shipped: the current release, an older one and an
    // event with no release do not reopen it.
    for release in [Some("1.1.0"), Some("1.0.0"), None] {
        ingestion
            .process_error_event(event(project, release))
            .await
            .expect("ingest");
        assert_eq!(
            status_of(db.as_ref(), id).await,
            "resolved",
            "an event from {release:?} keeps it resolved"
        );
    }
    let before = group(db.as_ref(), id).await.total_count;
    assert_eq!(before, 5, "every event is still counted");

    ingestion
        .process_error_event(event(project, Some("1.2.0")))
        .await
        .expect("ingest 1.2.0");
    let reopened = group(db.as_ref(), id).await;
    assert_eq!(reopened.status, "unresolved", "a newer release reopens it");
    assert!(
        !reopened.resolved_in_next_release,
        "the condition is cleared"
    );
    assert_eq!(reopened.resolved_in_release_after_id, None);
}

#[tokio::test]
async fn a_deployed_release_with_no_events_yet_counts_as_the_current_one() {
    let test_db = test_db!("next release deployed");
    let db = test_db.connection_arc();
    let project = create_project(db.as_ref()).await;
    create_deployment(db.as_ref(), project, "aaa111", 3600).await;
    let ingestion = ErrorIngestionService::new(db.clone());
    let id = ingestion
        .process_error_event(event(project, Some("aaa111")))
        .await
        .expect("ingest aaa111");
    // Deployed a minute ago; no event has reported it yet.
    create_deployment(db.as_ref(), project, "bbb222", 60).await;

    resolve_in_next_release(&db, project, id)
        .await
        .expect("resolve in the next release");
    ingestion
        .process_error_event(event(project, Some("bbb222")))
        .await
        .expect("ingest bbb222");
    assert_eq!(
        status_of(db.as_ref(), id).await,
        "resolved",
        "the deployment that was live when the group was resolved does not carry the fix"
    );

    create_deployment(db.as_ref(), project, "ccc333", 0).await;
    ingestion
        .process_error_event(event(project, Some("ccc333")))
        .await
        .expect("ingest ccc333");
    assert_eq!(status_of(db.as_ref(), id).await, "unresolved");
}

#[tokio::test]
async fn a_plain_status_write_clears_the_next_release_condition() {
    let test_db = test_db!("next release cleared");
    let db = test_db.connection_arc();
    let project = create_project(db.as_ref()).await;
    let ingestion = ErrorIngestionService::new(db.clone());
    let id = ingestion
        .process_error_event(event(project, Some("2.0.0")))
        .await
        .expect("ingest");
    resolve_in_next_release(&db, project, id)
        .await
        .expect("resolve in the next release");
    let again = resolve_in_next_release(&db, project, id)
        .await
        .expect("re-resolving against the same release");
    assert!(again.updated_ids.is_empty(), "nothing would change");
    assert_eq!(again.skipped, 1);

    // Another group's event registers a newer release; re-resolving now
    // re-bases the condition on it.
    let mut newer = event(project, Some("3.0.0"));
    newer.exception_type = Some("RangeError".to_string());
    newer.exception_value = Some("page index out of range".to_string());
    ingestion
        .process_error_event(newer)
        .await
        .expect("ingest 3.0.0");
    let outcome = resolve_in_next_release(&db, project, id)
        .await
        .expect("re-resolving re-bases the condition");
    assert_eq!(
        outcome.updated_ids,
        vec![id],
        "a group resolved after an older release is re-written"
    );

    GroupWorkflowService::new(db.clone())
        .bulk_update_status(
            project,
            BulkGroupSelector::Ids(vec![id]),
            BulkGroupAction::Unresolve,
            None,
            false,
        )
        .await
        .expect("reopen");
    let reopened = group(db.as_ref(), id).await;
    assert!(!reopened.resolved_in_next_release);
    assert_eq!(reopened.resolved_in_release_after_id, None);

    let error = GroupWorkflowService::new(db.clone())
        .bulk_update_status(
            project,
            BulkGroupSelector::Ids(vec![id]),
            BulkGroupAction::Ignore,
            None,
            true,
        )
        .await
        .expect_err("next release goes with resolve only");
    assert!(matches!(error, ErrorTrackingError::Validation(_)));
}
