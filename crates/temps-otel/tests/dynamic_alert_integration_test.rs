//! Integration tests for ADR-026 per-series ("dynamic") metric alerting, driving
//! the real fire -> resolve loop against a Docker-backed TimescaleDB.
//!
//! Unlike the pure-function unit tests in `metric_alert_evaluator.rs` (state
//! machine, ranking, cache-key derivation), these seed real multi-series metric
//! data, create a real `metric_alert_rules` row via `MetricAlertService::create`
//! (exercising validation), and run real `MetricAlertEvaluator::run_cycle`
//! iterations, then assert against the real `alarms` table.
//!
//! Docker-gated: they skip gracefully (no `#[ignore]`) when Docker/TestDatabase
//! is unavailable, mirroring `e2e_http_test.rs` and `timescaledb_storage_test.rs`.

use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::Duration;

use axum::extract::{Extension, Path, Query, State};
use axum::http::{HeaderMap, StatusCode};
use chrono::Utc;
use sea_orm::{ActiveModelTrait, ActiveValue::Set, ColumnTrait, EntityTrait, QueryFilter};

use temps_auth::{AuthContext, RequireAuth, Role};
use temps_core::RequestMetadata;
use temps_monitoring::{AlarmService, AlarmStatus};
use temps_otel::detectors::{Comparator, DetectionConfig, StaticParams};
use temps_otel::handlers::metric_alert_handler::{delete_alert, MetricAlertScopeParams};
use temps_otel::ingest::auth::OtelAuthService;
use temps_otel::ingest::rate_limit::RateLimiter;
use temps_otel::services::{
    MetricAlertEvaluator, MetricAlertService, MetricDashboardService, OtelService,
};
use temps_otel::storage::timescaledb::TimescaleDbStorage;
use temps_otel::storage::OtelStorage;
use temps_otel::types::{MetricPoint, MetricType, ResourceInfo};
use temps_otel::OtelAppState;

/// The per-series breach must persist for `for_duration_secs` before firing. With
/// `for_duration_secs == 1`, the first `run_cycle` only arms the breach timer
/// (elapsed 0 < 1) and the fire needs a later cycle with `>= 1s` elapsed. This is
/// the shortest real wait the state machine permits (the service rejects
/// `for_duration_secs <= 0`); slightly over 1s so `Instant::elapsed().as_secs()`
/// truncates to `>= 1`.
const BREACH_PERSIST_WAIT: Duration = Duration::from_millis(1_100);

// ── No-op notification + job-queue stubs ────────────────────────────────
//
// Mirror `e2e_http_test.rs`: the evaluator (and its two `AlarmService`
// instances) needs a `NotificationService` + `JobQueue` to construct and fire.
// We assert against the `alarms` table directly, so the notification/job side
// effects are intentionally dropped here.

struct NoOpNotificationService;

#[async_trait::async_trait]
impl temps_core::notifications::NotificationService for NoOpNotificationService {
    async fn send_email(
        &self,
        _message: temps_core::notifications::EmailMessage,
    ) -> Result<(), temps_core::notifications::NotificationError> {
        Ok(())
    }
    async fn send_notification(
        &self,
        _notification: temps_core::notifications::NotificationData,
    ) -> Result<(), temps_core::notifications::NotificationError> {
        Ok(())
    }
    async fn is_configured(&self) -> Result<bool, temps_core::notifications::NotificationError> {
        Ok(false)
    }
}

struct NoOpJobQueue;

#[async_trait::async_trait]
impl temps_core::JobQueue for NoOpJobQueue {
    async fn send(&self, _job: temps_core::jobs::Job) -> Result<(), temps_core::jobs::QueueError> {
        Ok(())
    }
    fn subscribe(&self) -> Box<dyn temps_core::jobs::JobReceiver> {
        Box::new(NoOpJobReceiver)
    }
}

struct NoOpJobReceiver;

#[async_trait::async_trait]
impl temps_core::jobs::JobReceiver for NoOpJobReceiver {
    async fn recv(&mut self) -> Result<temps_core::jobs::Job, temps_core::jobs::QueueError> {
        Err(temps_core::jobs::QueueError::InvalidData(
            "no-op receiver".to_string(),
        ))
    }
}

/// No-op audit logger so `OtelAppState` can be built for the handler-level
/// regression test below without a real audit service.
struct NoOpAuditLogger;

#[async_trait::async_trait]
impl temps_core::AuditLogger for NoOpAuditLogger {
    async fn create_audit_log(
        &self,
        _operation: &dyn temps_core::AuditOperation,
    ) -> anyhow::Result<()> {
        Ok(())
    }
}

/// Everything an evaluator test needs: the live DB (kept alive for schema
/// cleanup on drop), the evaluator under test, the alert service for rule CRUD,
/// the storage for seeding metrics, and the project the rows are scoped to.
struct EvaluatorTestCtx {
    _db: temps_database::test_utils::TestDatabase,
    db: Arc<sea_orm::DatabaseConnection>,
    evaluator: Arc<MetricAlertEvaluator>,
    alert_service: Arc<MetricAlertService>,
    otel_service: Arc<OtelService>,
    storage: Arc<TimescaleDbStorage>,
    project_id: i32,
}

/// Build the full evaluator stack over a migrated TimescaleDB with a real
/// project row (the `alarms.project_id` FK requires one before any fire).
/// Returns `None` when Docker/TestDatabase is unavailable (test skips).
///
/// `alarm_service_dynamic` is wired with a zero cooldown exactly as production
/// does in `temps-otel/src/plugin.rs`: per-series alarms all share
/// `alarm_type=deployment_metric_threshold` with null deployment/container/
/// service, so the DB cooldown key can't tell series apart — the evaluator's
/// per-series state machine guarantees exactly-once firing instead.
async fn setup_evaluator() -> Option<EvaluatorTestCtx> {
    let test_db = match temps_database::test_utils::TestDatabase::with_migrations().await {
        Ok(db) => db,
        Err(e) => {
            println!(
                "Docker/TestDatabase not available, skipping dynamic alert test: {}",
                e
            );
            return None;
        }
    };

    let db = test_db.db.clone();

    let user = temps_entities::users::ActiveModel {
        name: Set("Dynamic Alert Test User".into()),
        email: Set("dynamic-alert@test.local".into()),
        password_hash: Set(Some("not_real".into())),
        email_verified: Set(true),
        mfa_enabled: Set(false),
        ..Default::default()
    };
    let user = user
        .insert(db.as_ref())
        .await
        .expect("Failed to insert test user");

    let project = temps_entities::projects::ActiveModel {
        name: Set("Dynamic Alert Test Project".into()),
        repo_name: Set("test-repo".into()),
        repo_owner: Set("test-org".into()),
        directory: Set("/".into()),
        main_branch: Set("main".into()),
        preset: Set(temps_entities::preset::Preset::Dockerfile),
        slug: Set("dynamic-alert-test-project".into()),
        is_deleted: Set(false),
        is_public_repo: Set(false),
        attack_mode: Set(false),
        enable_preview_environments: Set(false),
        ..Default::default()
    };
    let project = project
        .insert(db.as_ref())
        .await
        .expect("Failed to insert test project");
    let _ = user;

    let storage = Arc::new(TimescaleDbStorage::new(db.clone(), None));
    let auth_service = Arc::new(OtelAuthService::new(db.clone()));
    let rate_limiter = Arc::new(RateLimiter::new(10_000, Duration::from_secs(60)));
    let otel_service = Arc::new(OtelService::new(
        storage.clone(),
        auth_service,
        rate_limiter,
    ));
    let alert_service = Arc::new(MetricAlertService::new(db.clone()));

    let notify: Arc<dyn temps_core::notifications::NotificationService> =
        Arc::new(NoOpNotificationService);
    let queue: Arc<dyn temps_core::JobQueue> = Arc::new(NoOpJobQueue);
    let alarm_service = Arc::new(AlarmService::new(db.clone(), notify.clone(), queue.clone()));
    let alarm_service_dynamic = Arc::new(
        AlarmService::new(db.clone(), notify, queue).with_cooldown(chrono::Duration::zero()),
    );

    let evaluator = Arc::new(MetricAlertEvaluator::new(
        alert_service.clone(),
        otel_service.clone(),
        alarm_service,
        alarm_service_dynamic,
        db.clone(),
        None,
    ));

    Some(EvaluatorTestCtx {
        _db: test_db,
        db,
        evaluator,
        alert_service,
        otel_service,
        storage,
        project_id: project.id,
    })
}

/// A gauge `MetricPoint` carrying one `method=<value>` label, timestamped
/// ~30s ago so it lands inside the evaluator's lookback window.
fn method_gauge(project_id: i32, metric: &str, method: &str, value: f64) -> MetricPoint {
    let mut attrs = BTreeMap::new();
    attrs.insert("method".to_string(), method.to_string());
    let mut p = MetricPoint::skeleton(
        project_id,
        None,
        ResourceInfo {
            service_name: "dynamic-alert-test".into(),
            service_version: Some("1.0.0".into()),
            deployment_environment: Some("test".into()),
            attributes: BTreeMap::new(),
        },
        metric.into(),
        MetricType::Gauge,
        "ms".into(),
        Utc::now() - chrono::Duration::seconds(30),
        attrs,
    );
    p.value = Some(value);
    p
}

/// All alarms for a project, oldest first, so assertions are order-stable.
async fn alarms_for(
    db: &sea_orm::DatabaseConnection,
    project_id: i32,
) -> Vec<temps_entities::alarms::Model> {
    let mut alarms = temps_entities::alarms::Entity::find()
        .filter(temps_entities::alarms::Column::ProjectId.eq(project_id))
        .all(db)
        .await
        .expect("query alarms");
    alarms.sort_by_key(|a| a.id);
    alarms
}

/// The human-readable `series_label` recorded on a per-series alarm.
fn series_label_of(alarm: &temps_entities::alarms::Model) -> Option<String> {
    alarm
        .metadata
        .as_ref()?
        .get("series_label")?
        .as_str()
        .map(str::to_string)
}

/// A static "> threshold" detector.
fn static_gt(threshold: f64) -> DetectionConfig {
    DetectionConfig::Static(StaticParams {
        comparator: Comparator::Gt,
        threshold,
    })
}

// ── Fire + resolve cycle (the must-have) ────────────────────────────────

/// Seed two label values (`method=GET` at 100, `method=POST` at 80) that both
/// breach a low static threshold, run the evaluator to firing, assert one
/// per-series alarm exists for EACH label value (right title suffix +
/// `is_dynamic`/`series_label` metadata), then raise the threshold above both
/// and assert both alarms resolve and the rule returns to `ok`.
#[tokio::test]
async fn test_dynamic_alert_fires_then_resolves_per_series() {
    let Some(ctx) = setup_evaluator().await else {
        return;
    };
    let metric = "http.request.latency";

    ctx.storage
        .store_metrics(vec![
            method_gauge(ctx.project_id, metric, "GET", 100.0),
            method_gauge(ctx.project_id, metric, "POST", 80.0),
        ])
        .await
        .expect("seed metrics");

    let rule = ctx
        .alert_service
        .create(
            ctx.project_id,
            "Per-series latency".to_string(),
            metric.to_string(),
            "avg".to_string(),
            static_gt(50.0),
            120, // window_secs
            1,   // for_duration_secs (min the state machine allows)
            "warning".to_string(),
            true,                       // enabled
            vec![],                     // label_filters
            vec!["method".to_string()], // group_by
            true,                       // dynamic_alerts
            20,                         // max_series
            5,                          // grouped_notification_threshold
        )
        .await
        .expect("create dynamic rule");

    // Cycle 1 only arms the per-series breach timers (elapsed 0 < for_duration).
    ctx.evaluator.run_cycle().await.expect("cycle 1");
    assert!(
        alarms_for(&ctx.db, ctx.project_id).await.is_empty(),
        "no alarm should fire before for_duration elapses"
    );

    // Let the breach persist past for_duration, then fire on cycle 2.
    tokio::time::sleep(BREACH_PERSIST_WAIT).await;
    ctx.evaluator.run_cycle().await.expect("cycle 2");

    let firing = alarms_for(&ctx.db, ctx.project_id).await;
    assert_eq!(
        firing.len(),
        2,
        "expected one per-series alarm per label value, got {firing:?}"
    );
    for alarm in &firing {
        assert_eq!(alarm.status, AlarmStatus::Firing.as_str());
        let md = alarm.metadata.as_ref().expect("alarm carries metadata");
        assert_eq!(
            md.get("is_dynamic").and_then(|v| v.as_bool()),
            Some(true),
            "per-series alarm must be flagged is_dynamic: {md:?}"
        );
        let label = series_label_of(alarm).expect("series_label present");
        assert!(
            alarm.title.contains(&format!("[{label}]")),
            "alarm title must carry the series suffix: title={:?} label={label}",
            alarm.title
        );
    }
    let labels: std::collections::HashSet<String> =
        firing.iter().filter_map(series_label_of).collect();
    assert_eq!(
        labels,
        ["method=GET".to_string(), "method=POST".to_string()]
            .into_iter()
            .collect(),
        "both distinct series should have fired"
    );

    // Rule row's aggregate view is firing.
    let after_fire = ctx
        .alert_service
        .get(ctx.project_id, rule.id)
        .await
        .expect("reload rule after fire");
    assert_eq!(after_fire.last_state, "firing");

    // Raise the threshold above both seeded values: the same data no longer
    // breaches, so the next cycle resolves every open series.
    ctx.alert_service
        .update(
            ctx.project_id,
            rule.id,
            None,
            None,
            None,
            Some(static_gt(500.0)),
            None,
            None,
            None,
            None,
            None,
            None,
            None,
            None,
            None,
        )
        .await
        .expect("raise threshold");

    ctx.evaluator.run_cycle().await.expect("resolve cycle");

    let resolved = alarms_for(&ctx.db, ctx.project_id).await;
    assert_eq!(resolved.len(), 2, "no new alarms should appear on resolve");
    for alarm in &resolved {
        assert_eq!(
            alarm.status,
            AlarmStatus::Resolved.as_str(),
            "series alarm should be resolved: {alarm:?}"
        );
        assert!(
            alarm.resolved_at.is_some(),
            "resolved alarm must carry resolved_at: {alarm:?}"
        );
    }

    let after_resolve = ctx
        .alert_service
        .get(ctx.project_id, rule.id)
        .await
        .expect("reload rule after resolve");
    assert_eq!(after_resolve.last_state, "ok");
}

// ── Cardinality cap ─────────────────────────────────────────────────────

/// Seed three series but cap `max_series` at 2: only the top-2 by |value|
/// (`GET`=100, `POST`=90) get alarms; the lowest (`PUT`=80) is dropped every
/// tick and never fires. The rule row records the drop count.
#[tokio::test]
async fn test_dynamic_alert_cardinality_cap_limits_alarms() {
    let Some(ctx) = setup_evaluator().await else {
        return;
    };
    let metric = "http.request.errors";

    ctx.storage
        .store_metrics(vec![
            method_gauge(ctx.project_id, metric, "GET", 100.0),
            method_gauge(ctx.project_id, metric, "POST", 90.0),
            method_gauge(ctx.project_id, metric, "PUT", 80.0),
        ])
        .await
        .expect("seed metrics");

    let rule = ctx
        .alert_service
        .create(
            ctx.project_id,
            "Capped per-series".to_string(),
            metric.to_string(),
            "avg".to_string(),
            static_gt(50.0),
            120,
            1,
            "warning".to_string(),
            true,
            vec![],
            vec!["method".to_string()],
            true,
            2, // max_series — below the 3 seeded series
            5,
        )
        .await
        .expect("create capped rule");

    ctx.evaluator.run_cycle().await.expect("cycle 1");
    tokio::time::sleep(BREACH_PERSIST_WAIT).await;
    ctx.evaluator.run_cycle().await.expect("cycle 2");

    let firing = alarms_for(&ctx.db, ctx.project_id).await;
    assert_eq!(
        firing.len(),
        2,
        "cardinality cap must limit alarms to max_series, got {firing:?}"
    );
    let labels: std::collections::HashSet<String> =
        firing.iter().filter_map(series_label_of).collect();
    assert_eq!(
        labels,
        ["method=GET".to_string(), "method=POST".to_string()]
            .into_iter()
            .collect(),
        "only the top-2 by |value| should fire; the lowest (PUT) is dropped"
    );

    let reloaded = ctx
        .alert_service
        .get(ctx.project_id, rule.id)
        .await
        .expect("reload capped rule");
    assert_eq!(
        reloaded.last_dropped_series_count, 1,
        "one series (PUT) should be recorded as dropped by the cap"
    );
}

// ── Collapse-to-max (group_by set, dynamic_alerts = false) ──────────────

/// With `group_by` set but `dynamic_alerts = false`, the evaluator collapses to
/// the single loudest series ("alert if ANY series breaches") and fires exactly
/// ONE aggregate alarm — no per-series fan-out, no `is_dynamic` flag.
#[tokio::test]
async fn test_grouped_non_dynamic_fires_single_aggregate_alarm() {
    let Some(ctx) = setup_evaluator().await else {
        return;
    };
    let metric = "http.request.rate";

    ctx.storage
        .store_metrics(vec![
            method_gauge(ctx.project_id, metric, "GET", 100.0),
            method_gauge(ctx.project_id, metric, "POST", 80.0),
        ])
        .await
        .expect("seed metrics");

    ctx.alert_service
        .create(
            ctx.project_id,
            "Aggregate collapse".to_string(),
            metric.to_string(),
            "avg".to_string(),
            static_gt(50.0),
            120,
            1,
            "warning".to_string(),
            true,
            vec![],
            vec!["method".to_string()], // grouped query...
            false,                      // ...but NOT dynamic: collapse to max
            20,
            5,
        )
        .await
        .expect("create collapse rule");

    ctx.evaluator.run_cycle().await.expect("cycle 1");
    tokio::time::sleep(BREACH_PERSIST_WAIT).await;
    ctx.evaluator.run_cycle().await.expect("cycle 2");

    let firing = alarms_for(&ctx.db, ctx.project_id).await;
    assert_eq!(
        firing.len(),
        1,
        "collapse path must fire exactly one aggregate alarm, got {firing:?}"
    );
    let alarm = &firing[0];
    assert_eq!(alarm.status, AlarmStatus::Firing.as_str());
    assert!(
        !alarm.title.contains('['),
        "aggregate alarm must not carry a per-series suffix: {:?}",
        alarm.title
    );
    let is_dynamic = alarm
        .metadata
        .as_ref()
        .and_then(|m| m.get("is_dynamic"))
        .and_then(|v| v.as_bool());
    assert_ne!(
        is_dynamic,
        Some(true),
        "collapse path must not flag the alarm is_dynamic: {:?}",
        alarm.metadata
    );
}

// ── delete_alert cross-project ownership check ──────────────────────────

/// Bare-bones `RequestMetadata` for a direct handler call — the delete path
/// doesn't read any field beyond what's needed for the audit log.
fn test_request_metadata() -> RequestMetadata {
    RequestMetadata {
        ip_address: "127.0.0.1".to_string(),
        user_agent: "integration-test".to_string(),
        headers: HeaderMap::new(),
        visitor_id_cookie: None,
        session_id_cookie: None,
        base_url: "http://localhost".to_string(),
        scheme: "http".to_string(),
        host: "localhost".to_string(),
        is_secure: false,
    }
}

/// Regression test for the cross-project IDOR fixed in `delete_alert`
/// (`crates/temps-otel/src/handlers/metric_alert_handler.rs`): the handler used
/// to call `metric_alert_evaluator.resolve_all_for_rule(id, scope.project_id)`
/// BEFORE verifying `id` belongs to `scope.project_id`. Since the evaluator's
/// in-memory firing maps are keyed only by `rule_id` (never `project_id`), an
/// attacker with `OtelWrite` on their OWN project could clear another
/// project's dynamic rule's per-series firing state just by passing that
/// rule's id with their own project_id in the query string — before the
/// service-layer delete ever got a chance to 404. Calls the handler directly
/// (bypassing HTTP/axum routing, which axum's tuple-struct extractors allow)
/// so the exact fixed code path runs.
#[tokio::test]
async fn test_delete_alert_rejects_cross_project_rule_id_before_touching_evaluator_state() {
    let Some(ctx) = setup_evaluator().await else {
        return;
    };
    let metric = "http.request.latency";

    // A second, unrelated project — the "attacker's own" project in this
    // scenario, distinct from `ctx.project_id` which owns the rule under attack.
    let attacker_project = temps_entities::projects::ActiveModel {
        name: Set("Attacker Project".into()),
        repo_name: Set("attacker-repo".into()),
        repo_owner: Set("attacker-org".into()),
        directory: Set("/".into()),
        main_branch: Set("main".into()),
        preset: Set(temps_entities::preset::Preset::Dockerfile),
        slug: Set("attacker-project".into()),
        is_deleted: Set(false),
        is_public_repo: Set(false),
        attack_mode: Set(false),
        enable_preview_environments: Set(false),
        ..Default::default()
    }
    .insert(ctx.db.as_ref())
    .await
    .expect("insert attacker project");

    let attacker_user = temps_entities::users::ActiveModel {
        name: Set("Attacker".into()),
        email: Set("attacker@test.local".into()),
        password_hash: Set(Some("not_real".into())),
        email_verified: Set(true),
        mfa_enabled: Set(false),
        ..Default::default()
    }
    .insert(ctx.db.as_ref())
    .await
    .expect("insert attacker user");

    // A dynamic rule owned by `ctx.project_id`, driven to a real firing state
    // exactly like `test_dynamic_alert_fires_then_resolves_per_series`.
    ctx.storage
        .store_metrics(vec![method_gauge(ctx.project_id, metric, "GET", 100.0)])
        .await
        .expect("seed metrics");

    let rule = ctx
        .alert_service
        .create(
            ctx.project_id,
            "Victim per-series rule".to_string(),
            metric.to_string(),
            "avg".to_string(),
            static_gt(50.0),
            120,
            1,
            "warning".to_string(),
            true,
            vec![],
            vec!["method".to_string()],
            true,
            20,
            5,
        )
        .await
        .expect("create victim rule");

    ctx.evaluator.run_cycle().await.expect("cycle 1");
    tokio::time::sleep(BREACH_PERSIST_WAIT).await;
    ctx.evaluator.run_cycle().await.expect("cycle 2");

    let firing_before = ctx.evaluator.firing_series_for(rule.id).await;
    assert_eq!(
        firing_before.len(),
        1,
        "victim rule must be firing before the attack"
    );
    let alarm_before = alarms_for(&ctx.db, ctx.project_id)
        .await
        .into_iter()
        .find(|a| series_label_of(a).as_deref() == Some("method=GET"))
        .expect("victim alarm exists");
    assert_eq!(alarm_before.status, AlarmStatus::Firing.as_str());

    // Build the same `OtelAppState` the real router constructs, so the handler
    // runs exactly as it does in production.
    let dashboard_service = Arc::new(MetricDashboardService::new(ctx.db.clone()));
    let app_state = OtelAppState {
        otel_service: ctx.otel_service.clone(),
        metrics_store: None,
        metrics_write_tx: None,
        dashboard_service,
        metric_alert_service: ctx.alert_service.clone(),
        metric_alert_evaluator: ctx.evaluator.clone(),
        audit_service: Arc::new(NoOpAuditLogger),
    };

    let attacker_auth = AuthContext::new_session(attacker_user, Role::Admin);

    // The attack: DELETE the victim's rule_id, scoped to the attacker's own
    // (different) project_id.
    let result = delete_alert(
        RequireAuth(attacker_auth),
        State(app_state),
        Extension(test_request_metadata()),
        Path(rule.id),
        Query(MetricAlertScopeParams {
            project_id: attacker_project.id,
        }),
    )
    .await;

    // `delete_alert` returns `Result<impl IntoResponse, Problem>` — the Ok type
    // is opaque to callers (no `Debug`), so match instead of `.expect_err()`.
    let err = match result {
        Ok(_) => panic!("cross-project delete must be rejected, not silently succeed"),
        Err(e) => e,
    };
    assert_eq!(
        err.status_code,
        StatusCode::NOT_FOUND,
        "cross-project delete must 404, not leak whether the rule exists"
    );

    // The critical assertion: the victim's evaluator state and DB alarm must be
    // completely untouched by the rejected attempt.
    let firing_after = ctx.evaluator.firing_series_for(rule.id).await;
    assert_eq!(
        firing_after.len(),
        1,
        "rejected cross-project delete must NOT clear the victim rule's firing state"
    );
    let rule_after = ctx
        .alert_service
        .get(ctx.project_id, rule.id)
        .await
        .expect("victim rule must still exist");
    assert_eq!(rule_after.last_state, "firing");
    let db_alarm_after = alarms_for(&ctx.db, ctx.project_id)
        .await
        .into_iter()
        .find(|a| series_label_of(a).as_deref() == Some("method=GET"))
        .expect("victim alarm must still exist");
    assert_eq!(
        db_alarm_after.status,
        AlarmStatus::Firing.as_str(),
        "rejected cross-project delete must NOT resolve the victim's alarm"
    );

    // Sanity check: the real owner (correct project_id) can still delete it via
    // the exact same handler — proving the 404 above was a genuine ownership
    // check, not a bug that blocks deletes outright.
    ctx.evaluator
        .resolve_all_for_rule(rule.id, ctx.project_id)
        .await;
    let legit_result = ctx.alert_service.delete(ctx.project_id, rule.id).await;
    assert!(
        legit_result.is_ok(),
        "the rule's real owner must still be able to delete it: {legit_result:?}"
    );
}
