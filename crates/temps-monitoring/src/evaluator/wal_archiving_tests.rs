// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! The default WAL archiving alarms, end to end through `evaluate_rule`:
//! archiving that keeps failing with no success fires, a success resolves
//! it, and a `pg_wal` backlog above its threshold fires.

use super::tests::{evaluator_with_db, latest, make_rule};
use super::*;
use sea_orm::{DatabaseBackend, MockDatabase};
use temps_entities::alarms;

/// The seeded rule for `metric_name`, as it is stored for service 1.
fn seeded_rule(metric_name: &str) -> monitoring_alert_rules::Model {
    let seed = postgres_default_seeds()
        .into_iter()
        .find(|seed| seed.metric_name == metric_name)
        .unwrap_or_else(|| panic!("{metric_name} is not a default PostgreSQL rule"));
    monitoring_alert_rules::Model {
        name: seed.name.into(),
        metric_name: seed.metric_name.into(),
        threshold: seed.threshold,
        comparator: seed.comparator.into(),
        severity: seed.severity.into(),
        for_duration_secs: seed.for_duration_secs,
        ..make_rule(Some(1), None)
    }
}

fn alarm(id: i32, status: &str) -> alarms::Model {
    let now = Utc::now();
    alarms::Model {
        id,
        project_id: Some(1),
        environment_id: None,
        deployment_id: None,
        container_id: None,
        service_id: Some(1),
        alarm_type: "database_metric_threshold".into(),
        severity: "critical".into(),
        status: status.into(),
        title: "Metric threshold breached".into(),
        message: None,
        metadata: None,
        fired_at: now,
        acknowledged_at: None,
        acknowledged_by: None,
        resolved_at: (status == "resolved").then_some(now),
        silenced_until: None,
        created_at: now,
        updated_at: now,
    }
}

/// A database that answers the cooldown check (no recent alarm) and the
/// alarm insert (alarm `id`).
fn firing_db(id: i32) -> sea_orm::DatabaseConnection {
    MockDatabase::new(DatabaseBackend::Postgres)
        .append_query_results(vec![Vec::<alarms::Model>::new(), vec![alarm(id, "firing")]])
        .into_connection()
}

async fn breaching_for(
    rule: &monitoring_alert_rules::Model,
    values: &[(&str, f64)],
    breach_age: chrono::Duration,
) -> AlertEvaluator {
    let evaluator = evaluator_with_db(firing_db(5));
    evaluator
        .breach_start
        .write()
        .await
        .insert(rule.id, Utc::now() - breach_age);
    evaluator
        .evaluate_rule(rule, &latest(values), (Some(1), None, None, Some(1)))
        .await
        .expect("evaluation succeeds");
    evaluator
}

#[test]
fn the_defaults_watch_archiving_and_the_wal_backlog() {
    let failing = seeded_rule("pg.archiver_failing");
    assert_eq!(failing.severity, "critical");
    assert_eq!(failing.for_duration_secs, 600);
    let backlog = seeded_rule("pg.wal_dir_to_max_wal_size_ratio");
    assert_eq!(backlog.threshold, 4.0);
    assert_eq!(backlog.comparator, ">");
    assert_eq!(backlog.for_duration_secs, 900);
    // Plain gauges: no volume gate may hold them back.
    assert!(volume_gate(&failing.metric_name).is_none());
    assert!(volume_gate(&backlog.metric_name).is_none());
}

#[tokio::test]
async fn archiving_failing_for_ten_minutes_with_no_success_fires() {
    let rule = seeded_rule("pg.archiver_failing");
    let evaluator = breaching_for(
        &rule,
        &[("pg.archiver_failing", 1.0)],
        chrono::Duration::minutes(11),
    )
    .await;
    assert_eq!(
        evaluator.firing_alarms.read().await.get(&rule.id),
        Some(&5),
        "ten minutes of failures with no success must fire"
    );
}

#[tokio::test]
async fn archiving_failing_for_less_than_ten_minutes_is_still_pending() {
    let rule = seeded_rule("pg.archiver_failing");
    let evaluator = breaching_for(
        &rule,
        &[("pg.archiver_failing", 1.0)],
        chrono::Duration::minutes(4),
    )
    .await;
    assert!(evaluator.firing_alarms.read().await.is_empty());
    assert!(evaluator.breach_start.read().await.contains_key(&rule.id));
}

#[tokio::test]
async fn a_successful_archive_resolves_the_alarm() {
    let rule = seeded_rule("pg.archiver_failing");
    let db = MockDatabase::new(DatabaseBackend::Postgres)
        .append_exec_results(vec![sea_orm::MockExecResult {
            last_insert_id: 0,
            rows_affected: 0,
        }])
        .append_query_results(vec![vec![alarm(5, "resolved")]])
        .into_connection();
    let evaluator = evaluator_with_db(db);
    evaluator.firing_alarms.write().await.insert(rule.id, 5);
    evaluator
        .breach_start
        .write()
        .await
        .insert(rule.id, Utc::now() - chrono::Duration::minutes(30));

    evaluator
        .evaluate_rule(
            &rule,
            &latest(&[("pg.archiver_failing", 0.0)]),
            (Some(1), None, None, Some(1)),
        )
        .await
        .expect("evaluation succeeds");

    assert!(
        !evaluator.firing_alarms.read().await.contains_key(&rule.id),
        "a success after the failures must resolve the alarm"
    );
    assert!(!evaluator.breach_start.read().await.contains_key(&rule.id));
}

#[tokio::test]
async fn a_wal_backlog_over_four_times_max_wal_size_fires() {
    let rule = seeded_rule("pg.wal_dir_to_max_wal_size_ratio");
    // The reported outage reached ~25x.
    let evaluator = breaching_for(
        &rule,
        &[("pg.wal_dir_to_max_wal_size_ratio", 25.0)],
        chrono::Duration::minutes(16),
    )
    .await;
    assert_eq!(evaluator.firing_alarms.read().await.get(&rule.id), Some(&5));
}

#[tokio::test]
async fn a_wal_directory_under_the_threshold_does_not_breach() {
    let rule = seeded_rule("pg.wal_dir_to_max_wal_size_ratio");
    let db = MockDatabase::new(DatabaseBackend::Postgres)
        .append_exec_results(vec![sea_orm::MockExecResult {
            last_insert_id: 0,
            rows_affected: 0,
        }])
        .into_connection();
    let evaluator = evaluator_with_db(db);
    evaluator
        .evaluate_rule(
            &rule,
            &latest(&[("pg.wal_dir_to_max_wal_size_ratio", 1.2)]),
            (Some(1), None, None, Some(1)),
        )
        .await
        .expect("evaluation succeeds");
    assert!(evaluator.firing_alarms.read().await.is_empty());
    assert!(!evaluator.breach_start.read().await.contains_key(&rule.id));
}
