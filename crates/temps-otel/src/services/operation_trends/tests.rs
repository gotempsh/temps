// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

use super::*;

#[test]
fn a_p95_well_above_the_band_is_a_regression() {
    let baseline = [100.0, 102.0, 98.0, 101.0, 99.0, 100.0, 103.0, 97.0, 100.0];
    let comparison = compare(&baseline, &[150.0, 148.0, 152.0]);
    assert_eq!(comparison.kind, ChangeKind::Regressed);
    assert_eq!(comparison.baseline_p95_ms, Some(100.0));
    assert_eq!(comparison.after_p95_ms, Some(150.0));
    assert!((comparison.change_ratio.unwrap() - 0.5).abs() < 1e-9);
}

#[test]
fn a_p95_well_below_the_band_is_an_improvement() {
    let baseline = [100.0; 10];
    let comparison = compare(&baseline, &[60.0, 62.0]);
    assert_eq!(comparison.kind, ChangeKind::Improved);
}

#[test]
fn a_wobble_on_a_flat_baseline_is_not_a_change() {
    // MAD of a flat baseline is 0; the scale floor and the relative minimum
    // keep a 10% wobble from reading as a regression.
    let comparison = compare(&[100.0; 10], &[110.0, 110.0]);
    assert_eq!(comparison.kind, ChangeKind::Unchanged);
}

#[test]
fn a_noisy_baseline_absorbs_a_change_inside_its_spread() {
    let baseline = [80.0, 140.0, 95.0, 160.0, 70.0, 150.0, 100.0, 130.0, 90.0];
    let comparison = compare(&baseline, &[150.0, 140.0]);
    assert_eq!(comparison.kind, ChangeKind::Unchanged);
}

#[test]
fn too_little_history_on_either_side_is_said_so() {
    let short = compare(&[100.0; 7], &[300.0, 300.0]);
    assert_eq!(short.kind, ChangeKind::InsufficientBaseline);
    let early = compare(&[100.0; 10], &[300.0]);
    assert_eq!(early.kind, ChangeKind::InsufficientAfter);
    assert_eq!(early.after_hours, 1);
}

#[test]
fn hours_round_down_and_up() {
    let at = DateTime::parse_from_rfc3339("2026-11-20T10:25:00Z")
        .unwrap()
        .with_timezone(&Utc);
    assert_eq!(hour_floor(at).to_rfc3339(), "2026-11-20T10:00:00+00:00");
    assert_eq!(hour_ceil(at).to_rfc3339(), "2026-11-20T11:00:00+00:00");
    let exact = hour_floor(at);
    assert_eq!(hour_ceil(exact), exact);
}

mod sampling {
    use super::*;
    use crate::test_support::MockOtelStorage;
    use crate::types::{ResourceInfo, SpanKind, SpanRecord, SpanStatusCode};

    #[allow(
        clippy::too_many_arguments,
        reason = "a test fixture spelling out one span"
    )]
    fn span(
        trace: &str,
        id: &str,
        parent: Option<&str>,
        name: &str,
        kind: SpanKind,
        start: DateTime<Utc>,
        duration_ms: i64,
        attributes: &[(&str, &str)],
    ) -> SpanRecord {
        SpanRecord {
            ingest_identity: None,
            project_id: 1,
            deployment_id: None,
            resource: ResourceInfo {
                service_name: "checkout-api".to_string(),
                service_version: None,
                deployment_environment: None,
                attributes: Default::default(),
            },
            trace_id: trace.to_string(),
            span_id: id.to_string(),
            parent_span_id: parent.map(str::to_string),
            name: name.to_string(),
            kind,
            start_time: start,
            end_time: start + Duration::milliseconds(duration_ms),
            duration_ms: duration_ms as f64,
            status_code: SpanStatusCode::Ok,
            status_message: String::new(),
            attributes: attributes
                .iter()
                .map(|(k, v)| (k.to_string(), v.to_string()))
                .collect(),
            events: Vec::new(),
        }
    }

    /// 19 fast calls whose time goes to a cache read, and one slow call
    /// whose time goes to a query.
    async fn service() -> (OperationTrendService, DateTime<Utc>) {
        let storage = Arc::new(MockOtelStorage::new());
        let now = Utc::now();
        let mut spans = Vec::new();
        for i in 0..19 {
            let trace = format!("fast{i:02}");
            let at = now - Duration::minutes(30) + Duration::seconds(i);
            spans.push(span(
                &trace,
                "a",
                None,
                "GET /cart",
                SpanKind::Server,
                at,
                20,
                &[("tier", "free")],
            ));
            spans.push(span(
                &trace,
                "b",
                Some("a"),
                "cache.get",
                SpanKind::Client,
                at,
                15,
                &[],
            ));
            // A span whose name merely contains the operation's.
            spans.push(span(
                &trace,
                "c",
                Some("a"),
                "GET /cart/items",
                SpanKind::Internal,
                at,
                1,
                &[],
            ));
        }
        let at = now - Duration::minutes(10);
        spans.push(span(
            "slow",
            "a",
            None,
            "GET /cart",
            SpanKind::Server,
            at,
            900,
            &[("tier", "paid")],
        ));
        spans.push(span(
            "slow",
            "b",
            Some("a"),
            "SELECT cart_items",
            SpanKind::Client,
            at,
            850,
            &[("db.system", "postgresql")],
        ));
        storage.store_spans(spans).await.expect("spans");
        let db = Arc::new(sea_orm::MockDatabase::new(DatabaseBackend::Postgres).into_connection());
        (OperationTrendService::new(db, storage), now)
    }

    #[tokio::test]
    async fn a_recent_sample_reads_at_most_the_budget_and_exact_calls_only() {
        let (service, now) = service().await;
        let result = service
            .breakdown(
                1,
                "checkout-api",
                "GET /cart",
                now - Duration::days(1),
                now,
                BreakdownSample::Recent,
                Some("tier"),
            )
            .await
            .expect("breakdown");
        assert_eq!(result.breakdown.traces, BREAKDOWN_TRACES);
        assert_eq!(result.breakdown.instances, BREAKDOWN_TRACES as u64);
        assert_eq!(result.slow_threshold_ms, None);
        assert!(result
            .breakdown
            .by_attribute
            .iter()
            .any(|s| s.value.as_deref() == Some("free")));
    }

    #[tokio::test]
    async fn a_slow_sample_starts_from_the_p95_and_shows_where_slow_calls_go() {
        let (service, now) = service().await;
        let result = service
            .breakdown(
                1,
                "checkout-api",
                "GET /cart",
                now - Duration::days(1),
                now,
                BreakdownSample::Slow,
                None,
            )
            .await
            .expect("breakdown");
        let threshold = result.slow_threshold_ms.expect("threshold");
        assert!(threshold > 20.0, "p95 {threshold}");
        assert_eq!(result.breakdown.instances, 1);
        assert_eq!(result.breakdown.groups[0].span_name, "SELECT cart_items");
        assert_eq!(result.breakdown.groups[0].category, "database");
    }

    #[tokio::test]
    async fn a_breakdown_request_is_validated() {
        let (service, now) = service().await;
        for (svc, name, start, attribute) in [
            ("", "GET /cart", now - Duration::days(1), None),
            (
                "checkout-api",
                "GET /cart",
                now - Duration::days(MAX_BREAKDOWN_DAYS + 1),
                None,
            ),
            (
                "checkout-api",
                "GET /cart",
                now - Duration::days(1),
                Some(" "),
            ),
        ] {
            let error = service
                .breakdown(1, svc, name, start, now, BreakdownSample::Recent, attribute)
                .await
                .expect_err("invalid");
            assert!(matches!(error, OtelError::Validation { .. }), "{error}");
        }
        assert_eq!(BreakdownSample::parse("slow"), Some(BreakdownSample::Slow));
        assert_eq!(BreakdownSample::parse("fastest"), None);
    }
}

mod database {
    use super::*;
    use crate::test_support::MockOtelStorage;
    use crate::types::{ResourceInfo, SpanKind, SpanRecord, SpanStatusCode};
    use sea_orm::{ActiveModelTrait, Set};
    use temps_test_utils::TestDatabase;

    async fn create_project(db: &DatabaseConnection, slug: &str) -> i32 {
        temps_entities::projects::ActiveModel {
            name: Set(format!("Rollup {slug}")),
            slug: Set(slug.to_string()),
            repo_name: Set("synthetic".into()),
            repo_owner: Set("synthetic".into()),
            directory: Set("/synthetic".into()),
            main_branch: Set("main".into()),
            preset: Set(temps_entities::preset::Preset::NextJs),
            ..Default::default()
        }
        .insert(db)
        .await
        .expect("project")
        .id
    }

    fn span(project_id: i32, name: &str, at: DateTime<Utc>, duration_ms: f64) -> SpanRecord {
        let seq = rand_id();
        SpanRecord {
            ingest_identity: None,
            project_id,
            deployment_id: None,
            resource: ResourceInfo {
                service_name: "checkout-api".to_string(),
                service_version: None,
                deployment_environment: None,
                attributes: Default::default(),
            },
            trace_id: format!("{seq:032x}"),
            span_id: format!("{:016x}", seq as u64),
            parent_span_id: None,
            name: name.to_string(),
            kind: SpanKind::Server,
            start_time: at,
            end_time: at + Duration::milliseconds(duration_ms as i64),
            duration_ms,
            status_code: SpanStatusCode::Ok,
            status_message: String::new(),
            attributes: Default::default(),
            events: Vec::new(),
        }
    }

    fn rand_id() -> u128 {
        use std::sync::atomic::{AtomicU64, Ordering};
        static NEXT: AtomicU64 = AtomicU64::new(1);
        NEXT.fetch_add(1, Ordering::Relaxed) as u128
    }

    async fn insert_deployment(
        db: &DatabaseConnection,
        project_id: i32,
        environment_id: i32,
        ready_at: Option<DateTime<Utc>>,
    ) -> i32 {
        let row = db
            .query_one(Statement::from_sql_and_values(
                DatabaseBackend::Postgres,
                "INSERT INTO deployments (project_id, environment_id, slug, state, metadata, ready_at, created_at, updated_at) \
                 VALUES ($1, $2, $3, 'deployed', '{}'::jsonb, $4, NOW(), NOW()) RETURNING id",
                [
                    project_id.into(),
                    environment_id.into(),
                    format!("deploy-{}", rand_id()).into(),
                    ready_at.into(),
                ],
            ))
            .await
            .expect("deployment")
            .expect("row");
        row.try_get("", "id").expect("id")
    }

    async fn insert_environment(db: &DatabaseConnection, project_id: i32) -> i32 {
        temps_entities::environments::ActiveModel {
            project_id: Set(project_id),
            name: Set("production".to_string()),
            slug: Set("production".to_string()),
            subdomain: Set(format!("rollup-{project_id}")),
            host: Set(format!("rollup-{project_id}.local")),
            upstreams: Set(temps_entities::upstream_config::UpstreamList::default()),
            ..Default::default()
        }
        .insert(db)
        .await
        .expect("environment")
        .id
    }

    #[tokio::test]
    async fn the_job_rolls_complete_hours_up_and_resumes_where_it_stopped() {
        let test_db = TestDatabase::with_migrations().await.expect("database");
        let db = test_db.connection_arc();
        let project_id = create_project(&db, "rollup-hours").await;
        let storage = Arc::new(MockOtelStorage::new());
        let now = DateTime::parse_from_rfc3339("2026-11-20T12:30:00Z")
            .unwrap()
            .with_timezone(&Utc);
        let hour = hour_floor(now) - Duration::hours(2);
        storage
            .store_spans(
                (0..10)
                    .map(|i| {
                        span(
                            project_id,
                            "GET /cart",
                            hour + Duration::minutes(i),
                            100.0 + i as f64,
                        )
                    })
                    .collect(),
            )
            .await
            .expect("spans");
        let service = OperationTrendService::new(db.clone(), storage.clone());

        let report = service.tick(now).await.expect("tick");
        assert_eq!(report.projects, 1);
        assert_eq!(report.hours, HOURS_PER_PROJECT_PER_TICK as usize);
        let state = service
            .tracking_state(project_id)
            .await
            .expect("state")
            .expect("saved");
        assert_eq!(
            state.rolled_until,
            hour_floor(now) - Duration::hours(BACKFILL_HOURS)
                + Duration::hours(HOURS_PER_PROJECT_PER_TICK)
        );

        // Later ticks catch up to the last complete hour and no further.
        for _ in 0..5 {
            service.tick(now).await.expect("tick");
        }
        let state = service
            .tracking_state(project_id)
            .await
            .expect("state")
            .expect("saved");
        assert_eq!(state.rolled_until, hour_floor(now));
        assert_eq!(state.overflow_operations, 0);

        let trend = service
            .trend(
                project_id,
                "checkout-api",
                "GET /cart",
                now - Duration::days(1),
                now,
            )
            .await
            .expect("trend");
        assert_eq!(trend.points.len(), 1, "only the hour with spans has a row");
        assert_eq!(trend.points[0].bucket_start, hour);
        assert_eq!(trend.points[0].count, 10);
        assert!(trend.points[0].p95_ms > 100.0);

        // Rolling the same hour again replaces its row.
        service.roll_up_hour(project_id, hour).await.expect("again");
        let again = service
            .trend(
                project_id,
                "checkout-api",
                "GET /cart",
                now - Duration::days(1),
                now,
            )
            .await
            .expect("trend");
        assert_eq!(again.points.len(), 1);
    }

    #[tokio::test]
    async fn operations_over_the_cap_are_counted_not_kept() {
        let test_db = TestDatabase::with_migrations().await.expect("database");
        let db = test_db.connection_arc();
        let project_id = create_project(&db, "rollup-cap").await;
        let storage = Arc::new(MockOtelStorage::new());
        let hour = DateTime::parse_from_rfc3339("2026-11-20T08:00:00Z")
            .unwrap()
            .with_timezone(&Utc);
        let extra = 7;
        storage
            .store_spans(
                (0..MAX_TRACKED_OPERATIONS as i64 + extra)
                    .map(|i| {
                        span(
                            project_id,
                            &format!("GET /item/{i}"),
                            hour + Duration::seconds(i),
                            10.0 + i as f64,
                        )
                    })
                    .collect(),
            )
            .await
            .expect("spans");
        let service = OperationTrendService::new(db.clone(), storage);
        let (tracked, overflow) = service
            .roll_up_hour(project_id, hour)
            .await
            .expect("rollup");
        assert_eq!(tracked, MAX_TRACKED_OPERATIONS as usize);
        assert_eq!(overflow, extra as usize);
        let rows: i64 = db
            .query_one(Statement::from_sql_and_values(
                DatabaseBackend::Postgres,
                "SELECT COUNT(*) AS n FROM operation_rollups WHERE project_id = $1",
                [project_id.into()],
            ))
            .await
            .unwrap()
            .unwrap()
            .try_get("", "n")
            .unwrap();
        assert_eq!(rows, MAX_TRACKED_OPERATIONS as i64);
        // The slowest operations by total time are the ones kept.
        let kept_slowest: i64 = db
            .query_one(Statement::from_sql_and_values(
                DatabaseBackend::Postgres,
                "SELECT COUNT(*) AS n FROM operation_rollups WHERE project_id = $1 AND span_name = $2",
                [project_id.into(), format!("GET /item/{}", MAX_TRACKED_OPERATIONS as i64 + extra - 1).into()],
            ))
            .await
            .unwrap()
            .unwrap()
            .try_get("", "n")
            .unwrap();
        assert_eq!(kept_slowest, 1);
    }

    #[tokio::test]
    async fn a_deployment_that_slowed_an_operation_is_reported() {
        let test_db = TestDatabase::with_migrations().await.expect("database");
        let db = test_db.connection_arc();
        let project_id = create_project(&db, "rollup-regression").await;
        let environment_id = insert_environment(&db, project_id).await;
        let storage = Arc::new(MockOtelStorage::new());
        let live_at = DateTime::parse_from_rfc3339("2026-11-19T12:20:00Z")
            .unwrap()
            .with_timezone(&Utc);
        let now = live_at + Duration::hours(6);
        let mut spans = Vec::new();
        for h in 1..=24 {
            let at = hour_floor(live_at) - Duration::hours(h);
            for i in 0..5 {
                spans.push(span(
                    project_id,
                    "GET /cart",
                    at + Duration::minutes(i),
                    100.0 + (h % 3) as f64,
                ));
                spans.push(span(
                    project_id,
                    "GET /home",
                    at + Duration::minutes(i),
                    50.0,
                ));
            }
        }
        for h in 0..4 {
            let at = hour_ceil(live_at) + Duration::hours(h);
            for i in 0..5 {
                spans.push(span(
                    project_id,
                    "GET /cart",
                    at + Duration::minutes(i),
                    180.0,
                ));
                spans.push(span(
                    project_id,
                    "GET /home",
                    at + Duration::minutes(i),
                    51.0,
                ));
            }
        }
        storage.store_spans(spans).await.expect("spans");
        let service = OperationTrendService::new(db.clone(), storage);
        let mut hour = hour_floor(live_at) - Duration::hours(24);
        while hour < hour_floor(now) {
            service
                .roll_up_hour(project_id, hour)
                .await
                .expect("rollup");
            hour += Duration::hours(1);
        }
        let deployment_id = insert_deployment(&db, project_id, environment_id, Some(live_at)).await;

        let result = service
            .regressions(project_id, deployment_id, now)
            .await
            .expect("regressions");
        assert_eq!(result.regressed, 1);
        assert_eq!(result.unchanged, 1);
        assert_eq!(result.changes.len(), 1);
        let change = &result.changes[0];
        assert_eq!(change.span_name, "GET /cart");
        assert_eq!(change.comparison.kind, ChangeKind::Regressed);
        assert_eq!(change.comparison.after_hours, 4);
        assert_eq!(result.after_start, hour_ceil(live_at));

        let trend = service
            .trend(
                project_id,
                "checkout-api",
                "GET /cart",
                now - Duration::days(2),
                now,
            )
            .await
            .expect("trend");
        assert_eq!(trend.deployments.len(), 1);
        assert_eq!(trend.deployments[0].deployment_id, deployment_id);

        // Another project's deployment is not found here.
        let other = create_project(&db, "rollup-other").await;
        let missing = service.regressions(other, deployment_id, now).await;
        assert!(matches!(
            missing,
            Err(OtelError::DeploymentNotFound { project_id, deployment_id: id })
                if project_id == other && id == deployment_id
        ));

        // A deployment that never went live cannot be compared.
        let pending = insert_deployment(&db, project_id, environment_id, None).await;
        let error = service
            .regressions(project_id, pending, now)
            .await
            .expect_err("not live");
        assert!(error.to_string().contains("never went live"));
    }

    #[tokio::test]
    async fn a_trend_window_is_validated() {
        let test_db = TestDatabase::with_migrations().await.expect("database");
        let db = test_db.connection_arc();
        let service = OperationTrendService::new(db, Arc::new(MockOtelStorage::new()));
        let now = Utc::now();
        for (service_name, span_name, start, end) in [
            ("", "GET /", now - Duration::days(1), now),
            ("api", "GET /", now, now - Duration::days(1)),
            (
                "api",
                "GET /",
                now - Duration::days(MAX_TREND_DAYS + 1),
                now,
            ),
        ] {
            let error = service
                .trend(1, service_name, span_name, start, end)
                .await
                .expect_err("invalid");
            assert!(matches!(error, OtelError::Validation { .. }), "{error}");
        }
    }
}
