// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! The performance scanner against a real database and the in-memory span
//! store: findings become `performance` groups, a trace read twice counts
//! once, and the budget and window hold.

use std::collections::BTreeMap;

use super::*;
use crate::services::group_workflow::tests::create_project;
use sea_orm::EntityTrait;
use temps_entities::error_groups;
use temps_otel::test_support::MockOtelStorage;
use temps_otel::types::{ResourceInfo, SpanKind, SpanRecord, SpanStatusCode};
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

fn span(
    project_id: i32,
    trace: &str,
    id: &str,
    parent: Option<&str>,
    start: DateTime<Utc>,
    ms: i64,
) -> SpanRecord {
    SpanRecord {
        ingest_identity: None,
        project_id,
        deployment_id: None,
        resource: ResourceInfo {
            service_name: "shop-api".into(),
            service_version: Some("4.2.0".into()),
            deployment_environment: Some("production".into()),
            attributes: BTreeMap::new(),
        },
        trace_id: trace.into(),
        span_id: id.into(),
        parent_span_id: parent.map(str::to_string),
        name: if parent.is_none() {
            "GET /checkout".into()
        } else {
            "SELECT".into()
        },
        kind: if parent.is_none() {
            SpanKind::Server
        } else {
            SpanKind::Client
        },
        start_time: start,
        end_time: start + Duration::milliseconds(ms),
        duration_ms: ms as f64,
        status_code: SpanStatusCode::Ok,
        status_message: String::new(),
        attributes: BTreeMap::new(),
        events: Vec::new(),
    }
}

/// A trace with six identical queries under its root: one N+1.
fn n_plus_one_trace(project_id: i32, trace: &str, start: DateTime<Utc>) -> Vec<SpanRecord> {
    let mut spans = vec![span(project_id, trace, "root", None, start, 400)];
    for n in 0..6 {
        let mut query = span(
            project_id,
            trace,
            &format!("q{n}"),
            Some("root"),
            start + Duration::milliseconds(n * 10),
            5,
        );
        query
            .attributes
            .insert("db.system".into(), "postgresql".into());
        query.attributes.insert(
            "db.statement".into(),
            format!("SELECT * FROM items WHERE cart_id = {n}"),
        );
        spans.push(query);
    }
    spans
}

async fn performance_groups(db: &DatabaseConnection, project_id: i32) -> Vec<error_groups::Model> {
    use sea_orm::{ColumnTrait, QueryFilter};
    error_groups::Entity::find()
        .filter(error_groups::Column::ProjectId.eq(project_id))
        .filter(error_groups::Column::IssueType.eq("performance"))
        .all(db)
        .await
        .expect("groups")
}

#[tokio::test]
async fn findings_become_performance_groups_and_a_reread_trace_counts_once() {
    let test_db = test_db!("performance scanner");
    let db = test_db.connection_arc();
    let project = create_project(db.as_ref()).await;
    let now = Utc::now();
    let storage = Arc::new(MockOtelStorage::new());
    {
        let mut spans = storage.spans.lock().expect("spans");
        spans.extend(n_plus_one_trace(
            project,
            "trace-one",
            now - Duration::minutes(5),
        ));
        spans.extend(n_plus_one_trace(
            project,
            "trace-two",
            now - Duration::minutes(4),
        ));
        // Too recent: may still be receiving spans.
        spans.extend(n_plus_one_trace(
            project,
            "trace-new",
            now - Duration::seconds(10),
        ));
    }
    let tracking = Arc::new(ErrorTrackingService::new(db.clone()));
    let scanner = PerformanceScanner::new(db.clone(), storage.clone(), tracking);

    let report = scanner.tick(now).await.expect("tick");
    assert_eq!(report.traces, 2, "the settling trace waits");
    assert_eq!(report.stored, 2);
    let groups = performance_groups(db.as_ref(), project).await;
    assert_eq!(groups.len(), 1, "the same pattern is one issue");
    let group = &groups[0];
    assert_eq!(group.error_type, "N+1 Query");
    assert_eq!(group.total_count, 2);
    assert!(
        group
            .title
            .contains("SELECT * FROM items WHERE cart_id = ?"),
        "{}",
        group.title
    );

    let state = scan_state(db.as_ref(), project)
        .await
        .expect("state")
        .expect("saved");
    assert_eq!(state.traces_scanned, 2);
    assert_eq!(state.issues_found, 2);
    assert_eq!(state.scanned_until, now - SETTLE);
    assert_eq!(state.last_error, None);

    // Forget where the scan stopped: the same traces are read again and
    // count nothing.
    db.execute(Statement::from_sql_and_values(
        DatabaseBackend::Postgres,
        "DELETE FROM performance_scan_state WHERE project_id = $1",
        [project.into()],
    ))
    .await
    .expect("reset state");
    let report = scanner.tick(now).await.expect("tick again");
    assert_eq!(report.traces, 2);
    assert_eq!(report.stored, 0, "already stored");
    assert_eq!(
        performance_groups(db.as_ref(), project).await[0].total_count,
        2
    );
}

#[tokio::test]
async fn a_busy_project_is_sampled_to_its_budget_and_old_traces_are_skipped() {
    let test_db = test_db!("performance scanner budget");
    let db = test_db.connection_arc();
    let project = create_project(db.as_ref()).await;
    let now = Utc::now();
    let storage = Arc::new(MockOtelStorage::new());
    {
        let mut spans = storage.spans.lock().expect("spans");
        for n in 0..(TRACES_PER_PROJECT as i64 + 5) {
            spans.extend(n_plus_one_trace(
                project,
                &format!("busy-{n}"),
                now - Duration::minutes(10) + Duration::seconds(n),
            ));
        }
        // Older than the lookback: never read.
        spans.extend(n_plus_one_trace(project, "stale", now - Duration::hours(2)));
    }
    let tracking = Arc::new(ErrorTrackingService::new(db.clone()));
    let scanner = PerformanceScanner::new(db.clone(), storage, tracking);

    let report = scanner.tick(now).await.expect("tick");
    assert_eq!(report.traces, TRACES_PER_PROJECT as usize);
    let groups = performance_groups(db.as_ref(), project).await;
    assert_eq!(groups[0].total_count, TRACES_PER_PROJECT as i32);

    let report = scanner
        .tick(now + Duration::seconds(30))
        .await
        .expect("next tick");
    assert_eq!(report.traces, 0, "the window moved past the busy minutes");
}

#[tokio::test]
async fn the_list_separates_performance_issues_from_errors() {
    use crate::services::error_crud_service::ErrorCRUDService;
    use crate::services::types::GroupListRefinement;
    let test_db = test_db!("performance issue list");
    let db = test_db.connection_arc();
    let project = create_project(db.as_ref()).await;
    let now = Utc::now();
    let storage = Arc::new(MockOtelStorage::new());
    storage
        .spans
        .lock()
        .expect("spans")
        .extend(n_plus_one_trace(
            project,
            "trace-list",
            now - Duration::minutes(3),
        ));
    let tracking = Arc::new(ErrorTrackingService::new(db.clone()));
    tracking
        .process_error_event_with_receipt(CreateErrorEventData {
            source: Some("sentry".into()),
            exception_type: Some("TypeError".into()),
            exception_value: Some("basket is undefined".into()),
            project_id: project,
            ..Default::default()
        })
        .await
        .expect("an ordinary error");
    PerformanceScanner::new(db.clone(), storage, tracking)
        .tick(now)
        .await
        .expect("tick");

    let crud = ErrorCRUDService::new(db.clone());
    let list = |issue_type: Option<&str>| {
        let crud = &crud;
        let issue_type = issue_type.map(str::to_string);
        async move {
            crud.list_error_groups(
                project,
                None,
                None,
                None,
                None,
                None,
                None,
                None,
                None,
                &GroupListRefinement {
                    issue_type,
                    ..Default::default()
                },
            )
            .await
        }
    };
    let (performance, _) = list(Some("performance")).await.expect("performance");
    assert_eq!(performance.len(), 1);
    assert_eq!(performance[0].issue_type, "performance");
    let (errors, _) = list(Some("error")).await.expect("errors");
    assert_eq!(errors.len(), 1);
    assert_eq!(errors[0].error_type, "TypeError");
    assert_eq!(list(None).await.expect("all").0.len(), 2);
    let error = list(Some("crash")).await.expect_err("unknown type");
    assert!(
        error.to_string().contains("error or performance"),
        "{error}"
    );
}
