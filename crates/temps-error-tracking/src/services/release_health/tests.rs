// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

use super::recorder::*;
use super::*;
use chrono::{DateTime, TimeDelta, Utc};
use relay_event_schema::protocol::{SessionAggregates, SessionUpdate};
use sea_orm::{ActiveModelTrait, ConnectionTrait, DatabaseBackend, Set, Statement};

fn now() -> DateTime<Utc> {
    DateTime::parse_from_rfc3339("2026-11-20T12:30:00Z")
        .unwrap()
        .with_timezone(&Utc)
}

fn update(json: serde_json::Value) -> SessionUpdate {
    SessionUpdate::parse(json.to_string().as_bytes()).expect("session update")
}

fn session(
    sid: &str,
    did: Option<&str>,
    init: bool,
    status: &str,
    errors: u64,
    started: DateTime<Utc>,
) -> SessionUpdate {
    let mut json = serde_json::json!({
        "sid": sid,
        "init": init,
        "started": started.to_rfc3339(),
        "timestamp": started.to_rfc3339(),
        "status": status,
        "errors": errors,
        "attrs": {"release": "synthetic-app@1.0.0", "environment": "production"}
    });
    if let Some(did) = did {
        json["did"] = serde_json::json!(did);
    }
    update(json)
}

fn sid(n: u32) -> String {
    format!("{n:08x}-0000-4000-8000-000000000000")
}

#[test]
fn an_init_update_starts_a_session_in_its_start_hour_and_counts_its_user() {
    let started = now() - TimeDelta::minutes(10);
    let delta = delta_from_update(
        7,
        &session(&sid(1), Some("user-1"), true, "ok", 0, started),
        now(),
    )
    .expect("counted");
    assert_eq!(delta.key.project_id, 7);
    assert_eq!(delta.key.release, "synthetic-app@1.0.0");
    assert_eq!(delta.key.environment, "production");
    assert_eq!(
        delta.key.bucket_start.to_rfc3339(),
        "2026-11-20T12:00:00+00:00"
    );
    assert_eq!((delta.started, delta.crashed, delta.abnormal), (1, 0, 0));
    assert!(delta.user.is_some());
    assert!(delta.errored_session.is_none());
    assert!(delta.crashed_user.is_none());
}

#[test]
fn a_crash_is_errored_and_crashed_and_marks_its_user_crashed() {
    let delta = delta_from_update(
        7,
        &session(&sid(2), Some("user-2"), false, "crashed", 1, now()),
        now(),
    )
    .expect("counted");
    assert_eq!(delta.started, 0, "only init starts a session");
    assert_eq!(delta.crashed, 1);
    assert!(delta.errored_session.is_some());
    assert_eq!(delta.crashed_user, delta.user);
}

#[test]
fn errors_on_a_healthy_update_mark_the_session_errored_and_abnormal_is_its_own_count() {
    let errored = delta_from_update(7, &session(&sid(3), None, false, "ok", 2, now()), now())
        .expect("counted");
    assert!(errored.errored_session.is_some());
    assert_eq!(errored.crashed, 0);
    assert!(errored.user.is_none());

    let abnormal = delta_from_update(
        7,
        &session(&sid(4), None, false, "abnormal", 0, now()),
        now(),
    )
    .expect("counted");
    assert_eq!((abnormal.abnormal, abnormal.crashed), (1, 0));
    assert!(abnormal.errored_session.is_some());
}

#[test]
fn the_nil_uuid_is_not_a_user() {
    let delta = delta_from_update(
        7,
        &session(
            &sid(5),
            Some("00000000-0000-0000-0000-000000000000"),
            true,
            "ok",
            0,
            now(),
        ),
        now(),
    )
    .expect("counted");
    assert!(delta.user.is_none());
}

#[test]
fn bad_releases_environments_and_timestamps_are_rejected() {
    let mut long_release = session(&sid(6), None, true, "ok", 0, now());
    long_release.attributes.release = "r".repeat(251);
    assert_eq!(
        delta_from_update(7, &long_release, now()),
        Err(SessionRejection::Release)
    );
    let mut blank = session(&sid(6), None, true, "ok", 0, now());
    blank.attributes.release = "  ".into();
    assert_eq!(
        delta_from_update(7, &blank, now()),
        Err(SessionRejection::Release)
    );
    let mut environment = session(&sid(6), None, true, "ok", 0, now());
    environment.attributes.environment = Some("e".repeat(65));
    assert_eq!(
        delta_from_update(7, &environment, now()),
        Err(SessionRejection::Environment)
    );
    for started in [now() - TimeDelta::days(6), now() + TimeDelta::hours(2)] {
        assert_eq!(
            delta_from_update(7, &session(&sid(6), None, true, "ok", 0, started), now()),
            Err(SessionRejection::Timestamp)
        );
    }
}

#[test]
fn aggregates_count_every_ended_session_and_report_errored_exactly() {
    let aggregates = SessionAggregates::parse(
        serde_json::json!({
            "aggregates": [
                {"started": now().to_rfc3339(), "did": "user-9", "exited": 3, "errored": 2, "crashed": 1},
                {"started": (now() - TimeDelta::days(9)).to_rfc3339(), "exited": 1}
            ],
            "attrs": {"release": "synthetic-api@2.0.0"}
        })
        .to_string()
        .as_bytes(),
    )
    .expect("aggregates");
    let deltas = deltas_from_aggregates(7, &aggregates, now());
    assert_eq!(deltas.len(), 2);
    let delta = deltas[0].clone().expect("counted");
    assert_eq!(delta.key.environment, "", "no environment is the empty one");
    assert_eq!((delta.started, delta.crashed), (6, 1));
    assert_eq!(delta.errored_exact, 3, "errored and crashed are errored");
    assert!(delta.errored_session.is_none());
    assert!(delta.crashed_user.is_some());
    assert_eq!(deltas[1], Err(SessionRejection::Timestamp));
}

#[test]
fn repeated_updates_of_one_session_count_it_errored_once() {
    let mut pending = PendingBuckets::default();
    for (init, errors, status) in [(true, 0, "ok"), (false, 1, "ok"), (false, 3, "exited")] {
        pending.add(
            delta_from_update(
                7,
                &session(&sid(10), Some("user-10"), init, status, errors, now()),
                now(),
            )
            .unwrap(),
        );
    }
    assert_eq!(pending.len(), 1);
    let key = delta_from_update(7, &session(&sid(10), None, true, "ok", 0, now()), now())
        .unwrap()
        .key;
    let counts = pending.get(&key).expect("pending");
    assert_eq!(counts.started, 1);
    assert_eq!(counts.errored_sessions.as_ref().unwrap().count(None), 1);
    assert_eq!(counts.users.as_ref().unwrap().count(None), 1);
}

#[test]
fn a_full_queue_drops_and_counts_instead_of_waiting() {
    let (recorder, _rx) = SessionRecorder::new(2);
    let delta = delta_from_update(7, &session(&sid(11), None, true, "ok", 0, now()), now());
    let outcome = recorder.record([
        delta.clone(),
        delta.clone(),
        delta,
        Err(SessionRejection::Release),
    ]);
    assert_eq!(
        outcome,
        RecordOutcome {
            queued: 2,
            dropped: 1,
            rejected: 1
        }
    );
    use std::sync::atomic::Ordering::Relaxed;
    assert_eq!(recorder.stats().dropped_queue_full.load(Relaxed), 1);
    assert_eq!(recorder.stats().rejected.load(Relaxed), 1);
}

#[test]
fn rates_and_shares_are_bounded_and_absent_without_a_base() {
    assert_eq!(crash_free(0, 0), None);
    assert_eq!(crash_free(100, 5), Some(0.95));
    assert_eq!(crash_free(10, 20), Some(0.0));
    assert_eq!(share(1, 0), None);
    assert_eq!(share(1, 4), Some(0.25));
}

#[tokio::test]
async fn reads_refuse_bad_windows_pages_and_releases_before_any_query() {
    let db = Arc::new(sea_orm::MockDatabase::new(DatabaseBackend::Postgres).into_connection());
    let service = ReleaseHealthService::new(db);
    for (days, page, per_page) in [(0, 1, 20), (15, 1, 20), (7, 0, 20), (7, 1, 101)] {
        let error = service
            .list(1, None, days, page, per_page)
            .await
            .expect_err("refused");
        assert!(
            matches!(error, ErrorTrackingError::Validation(_)),
            "{error}"
        );
    }
    assert!(service
        .list(1, Some("e".repeat(65)), 7, 1, 20)
        .await
        .is_err());
    assert!(service.series(1, "".into(), None, 7).await.is_err());
    assert!(service.series(1, "r".repeat(251), None, 7).await.is_err());
}

async fn create_project(db: &sea_orm::DatabaseConnection) -> i32 {
    temps_entities::projects::ActiveModel {
        name: Set("Synthetic release health project".into()),
        slug: Set(format!("synthetic-health-{}", uuid::Uuid::new_v4())),
        repo_name: Set("synthetic".into()),
        repo_owner: Set("synthetic".into()),
        directory: Set("/synthetic".into()),
        main_branch: Set("main".into()),
        preset: Set(temps_entities::preset::Preset::NextJs),
        created_at: Set(Utc::now()),
        updated_at: Set(Utc::now()),
        ..Default::default()
    }
    .insert(db)
    .await
    .expect("project")
    .id
}

async fn create_deployment(db: &sea_orm::DatabaseConnection, project_id: i32, sha: &str) -> i32 {
    let environment_id = temps_entities::environments::ActiveModel {
        project_id: Set(project_id),
        name: Set("production".to_string()),
        slug: Set("production".to_string()),
        subdomain: Set(format!("health-{project_id}")),
        host: Set(format!("health-{project_id}.local")),
        upstreams: Set(temps_entities::upstream_config::UpstreamList::default()),
        ..Default::default()
    }
    .insert(db)
    .await
    .expect("environment")
    .id;
    db.query_one(Statement::from_sql_and_values(
        DatabaseBackend::Postgres,
        "INSERT INTO deployments (project_id, environment_id, slug, state, metadata, commit_sha, \
             commit_message, ready_at, created_at, updated_at) \
         VALUES ($1, $2, $3, 'deployed', '{}'::jsonb, $4, 'Synthetic change', NOW(), NOW(), NOW()) \
         RETURNING id",
        [
            project_id.into(),
            environment_id.into(),
            format!("deploy-{}", uuid::Uuid::new_v4().simple()).into(),
            sha.into(),
        ],
    ))
    .await
    .expect("deployment")
    .expect("row")
    .try_get("", "id")
    .expect("id")
}

fn release_session(release: &str, n: u32, status: &str, started: DateTime<Utc>) -> SessionUpdate {
    let mut update = session(
        &sid(n),
        Some(&format!("user-{n}")),
        true,
        status,
        u64::from(status == "crashed"),
        started,
    );
    update.attributes.release = release.into();
    update
}

fn counted(project_id: i32, update: &SessionUpdate) -> SessionDelta {
    delta_from_update(project_id, update, Utc::now()).expect("counted")
}

#[tokio::test]
async fn flushed_sessions_give_crash_free_rates_adoption_users_and_the_deployment() {
    let test_db = match temps_test_utils::TestDatabase::with_migrations().await {
        Ok(db) => db,
        Err(_) => {
            eprintln!(
                "Skipping release health database test: Docker-backed TestDatabase unavailable"
            );
            return;
        }
    };
    let db = test_db.connection_arc();
    let project_id = create_project(&db).await;
    let sha = "0123456789abcdef0123456789abcdef01234567";
    let deployment_id = create_deployment(&db, project_id, sha).await;
    crate::services::group_workflow::next_release::register_release(
        db.as_ref(),
        project_id,
        sha,
        None,
    )
    .await
    .expect("release registered");

    let started = Utc::now() - TimeDelta::minutes(5);
    let (recorder, rx) = SessionRecorder::new(10_000);
    let mut flusher = SessionFlusher::new(db.clone(), rx, recorder.stats_arc());
    // 100 sessions of the deployed release, 5 of them crashed, written in
    // two flushes so the second adds to the rows of the first; 50 healthy
    // sessions of an older release.
    for n in 0..100u32 {
        let status = if n < 5 { "crashed" } else { "ok" };
        flusher.add(counted(
            project_id,
            &release_session(sha, n, status, started),
        ));
        if n == 49 {
            flusher.flush().await;
            assert!(flusher.pending().is_empty());
        }
    }
    for n in 100..150u32 {
        flusher.add(counted(
            project_id,
            &release_session("synthetic-old", n, "ok", started),
        ));
    }
    // Counts of a project deleted since they were queued are dropped, not
    // retried forever.
    flusher.add(counted(
        project_id + 10_000,
        &release_session(sha, 1, "ok", started),
    ));
    flusher.flush().await;
    assert!(flusher.pending().is_empty());
    use std::sync::atomic::Ordering::Relaxed;
    assert_eq!(recorder.stats().lost_on_flush.load(Relaxed), 0);

    let service = ReleaseHealthService::new(db.clone());
    let page = service
        .list(project_id, None, 7, 1, 20)
        .await
        .expect("list");
    assert!(page.tracking.configured);
    assert_eq!(page.total, 2);
    assert_eq!(page.environments, vec!["production".to_string()]);
    let deployed = page
        .items
        .iter()
        .find(|r| r.release == sha)
        .expect("deployed release");
    assert_eq!(deployed.sessions, 100);
    assert_eq!(deployed.crashed_sessions, 5);
    assert_eq!(deployed.errored_sessions, 5);
    assert_eq!(deployed.crash_free_sessions, Some(0.95));
    assert!(
        (deployed.users - 100).abs() <= 15,
        "users {}",
        deployed.users
    );
    assert!((deployed.crashed_users - 5).abs() <= 1);
    assert!(deployed
        .crash_free_users
        .is_some_and(|r| (0.9..=1.0).contains(&r)));
    let adoption = deployed.adoption.expect("adoption");
    assert!(
        (adoption - 100.0 / 150.0).abs() < 1e-9,
        "adoption {adoption}"
    );
    let deployment = deployed.deployment.as_ref().expect("deployment");
    assert_eq!(deployment.deployment_id, deployment_id);
    assert_eq!(deployment.environment_name.as_deref(), Some("production"));
    assert_eq!(
        deployment.commit_message.as_deref(),
        Some("Synthetic change")
    );
    let old = page
        .items
        .iter()
        .find(|r| r.release == "synthetic-old")
        .unwrap();
    assert_eq!(old.crash_free_sessions, Some(1.0));
    assert!(old.deployment.is_none());

    // The environment filter and pagination.
    let staging = service
        .list(project_id, Some("staging".into()), 7, 1, 20)
        .await
        .unwrap();
    assert_eq!(staging.total, 0);
    let second = service.list(project_id, None, 7, 2, 1).await.unwrap();
    assert_eq!((second.total, second.items.len()), (2, 1));
    let beyond = service.list(project_id, None, 7, 5, 1).await.unwrap();
    assert_eq!((beyond.total, beyond.items.len()), (2, 0));

    let series = service
        .series(project_id, sha.into(), Some("production".into()), 7)
        .await
        .expect("series");
    assert_eq!(series.points.len(), 1);
    assert_eq!(series.points[0].sessions, 100);
    assert_eq!(series.points[0].errored_sessions, 5);
    assert_eq!(series.points[0].crash_free_sessions, Some(0.95));

    // Published for metric alert rules: both releases have enough sessions.
    let samples = service.publish(Utc::now()).await.expect("publish");
    let rate = samples
        .iter()
        .find(|s| {
            s.project_id == project_id
                && s.name == CRASH_FREE_SESSIONS_METRIC
                && s.labels.contains(&("release", sha.to_string()))
        })
        .expect("crash-free sample");
    assert_eq!(rate.value, 0.95);
    assert!(rate
        .labels
        .contains(&("environment", "production".to_string())));
    assert!(samples
        .iter()
        .any(|s| s.project_id == project_id && s.name == CRASH_FREE_USERS_METRIC));

    // A project with no session explains how to start.
    let empty = create_project(&db).await;
    let page = service.list(empty, None, 7, 1, 20).await.unwrap();
    assert!(!page.tracking.configured);
    assert!(page
        .tracking
        .reason
        .as_deref()
        .is_some_and(|r| r.contains("session tracking")));

    // Retention removes old hours only.
    assert_eq!(service.prune(Utc::now()).await.unwrap(), 0);
    assert!(
        service
            .prune(Utc::now() + TimeDelta::days(RETENTION_DAYS + 1))
            .await
            .unwrap()
            >= 2
    );
}

#[tokio::test]
async fn too_few_sessions_publish_a_count_but_no_rate() {
    let test_db = match temps_test_utils::TestDatabase::with_migrations().await {
        Ok(db) => db,
        Err(_) => {
            eprintln!(
                "Skipping release health database test: Docker-backed TestDatabase unavailable"
            );
            return;
        }
    };
    let db = test_db.connection_arc();
    let project_id = create_project(&db).await;
    let (recorder, rx) = SessionRecorder::new(100);
    let mut flusher = SessionFlusher::new(db.clone(), rx, recorder.stats_arc());
    let started = Utc::now() - TimeDelta::minutes(1);
    for n in 0..3u32 {
        let status = if n == 0 { "crashed" } else { "ok" };
        flusher.add(counted(
            project_id,
            &release_session("synthetic-new", n, status, started),
        ));
    }
    flusher.flush().await;
    let service = ReleaseHealthService::new(db);
    let samples: Vec<_> = service
        .publish(Utc::now())
        .await
        .unwrap()
        .into_iter()
        .filter(|s| s.project_id == project_id)
        .collect();
    assert_eq!(samples.len(), 1);
    assert_eq!(samples[0].name, SESSIONS_METRIC);
    assert_eq!(samples[0].value, 3.0);
}
