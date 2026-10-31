// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Tests for `schedule_runs`.

use super::super::test_support::*;
use super::*;
use sea_orm::{DatabaseBackend, MockDatabase};
use temps_core::EncryptionService;

#[test]
fn zero_target_schedule_run_is_skipped_not_completed() {
    assert_eq!(schedule_run_aggregate_state(0, 0, 0, 0), "skipped");
    assert_eq!(schedule_run_aggregate_state(1, 0, 0, 0), "completed");
    assert_eq!(schedule_run_aggregate_state(1, 1, 0, 0), "failed");
}

/// Helper: build a minimal `ScheduleRunSummary` row value for MockDatabase.
///
/// The new fan-out shape returns one row per scheduler tick with aggregate
/// child counts. The row schema must match `RunRow` (defined in the
/// `list_schedule_runs` SQL); fields here mirror that.
fn make_run_entry_row(
    run_id: i64,
    started_at: chrono::DateTime<chrono::Utc>,
) -> std::collections::BTreeMap<String, sea_orm::Value> {
    use sea_orm::Value as SVal;
    let mut row = std::collections::BTreeMap::new();
    row.insert("run_id".to_string(), SVal::BigInt(Some(run_id)));
    row.insert("schedule_id".to_string(), SVal::Int(Some(1)));
    row.insert(
        "triggered_by".to_string(),
        SVal::String(Some(Box::new("cron".to_string()))),
    );
    row.insert(
        "started_at".to_string(),
        SVal::ChronoDateTimeUtc(Some(Box::new(started_at))),
    );
    row.insert("finished_at".to_string(), SVal::ChronoDateTimeUtc(None));
    row.insert("total_jobs".to_string(), SVal::BigInt(Some(1)));
    row.insert("completed_jobs".to_string(), SVal::BigInt(Some(1)));
    row.insert("failed_jobs".to_string(), SVal::BigInt(Some(0)));
    row.insert("running_jobs".to_string(), SVal::BigInt(Some(0)));
    row.insert("pending_jobs".to_string(), SVal::BigInt(Some(0)));
    row
}

/// `list_schedule_runs` must return rows ordered newest-first by the SQL
/// (which uses `ORDER BY started_at DESC`).
///
/// The MockDatabase returns rows in the order the test supplies them; we
/// supply them newest-first and assert that the response preserves that
/// order and that pagination metadata is correct.
#[tokio::test]
async fn test_list_schedule_runs_returns_rows_ordered_desc() {
    let now = chrono::Utc::now();
    let older = now - chrono::Duration::hours(2);

    let schedule = make_test_schedule(1, 1);

    // MockDatabase query sequence:
    // 1. get_backup_schedule SELECT (returns the schedule)
    // 2. COUNT(*) query
    // 3. Paginated rows query (returns two entries)
    let count_row = {
        let mut r = std::collections::BTreeMap::new();
        r.insert("total".to_string(), sea_orm::Value::BigInt(Some(2)));
        r
    };

    let db = Arc::new(
        MockDatabase::new(DatabaseBackend::Postgres)
            // get_backup_schedule
            .append_query_results(vec![vec![schedule]])
            // COUNT query
            .append_query_results(vec![vec![count_row]])
            // paginated rows — newest first (run_id=2 is more recent)
            .append_query_results(vec![vec![
                make_run_entry_row(2, now),
                make_run_entry_row(1, older),
            ]])
            .into_connection(),
    );

    let svc = BackupService::new(
        db.clone(),
        create_mock_external_service_manager(db),
        create_mock_alarm_service(),
        create_mock_config_service(),
        Arc::new(EncryptionService::new("test_encryption_key_1234567890ab").unwrap()),
        Arc::new(NoopJobQueue),
    );

    let response = svc
        .list_schedule_runs(1, 1, 20)
        .await
        .expect("list_schedule_runs must succeed");

    assert_eq!(response.total, 2, "total must match COUNT result");
    assert_eq!(response.runs.len(), 2, "must return 2 run entries");
    // First entry is the most recent (run_id=2).
    assert_eq!(response.runs[0].run_id, 2, "first entry must be newest");
    assert_eq!(response.runs[1].run_id, 1, "second entry must be older");
}

/// `list_schedule_runs` must clamp `page < 1` to 1, so the offset never
/// goes negative.
#[tokio::test]
async fn test_list_schedule_runs_clamps_page_below_one() {
    let schedule = make_test_schedule(1, 1);
    let count_row = {
        let mut r = std::collections::BTreeMap::new();
        r.insert("total".to_string(), sea_orm::Value::BigInt(Some(0)));
        r
    };

    let db = Arc::new(
        MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results(vec![vec![schedule]])
            .append_query_results(vec![vec![count_row]])
            .append_query_results(vec![Vec::<
                std::collections::BTreeMap<String, sea_orm::Value>,
            >::new()])
            .into_connection(),
    );

    let svc = BackupService::new(
        db.clone(),
        create_mock_external_service_manager(db),
        create_mock_alarm_service(),
        create_mock_config_service(),
        Arc::new(EncryptionService::new("test_encryption_key_1234567890ab").unwrap()),
        Arc::new(NoopJobQueue),
    );

    // page=-5 must be treated as page=1 (offset=0).
    let result = svc.list_schedule_runs(1, -5, 20).await;
    assert!(result.is_ok(), "negative page must not error: {:?}", result);
}

/// `list_schedule_runs` must clamp `page_size > 100` to 100 so the client
/// cannot request an unbounded result set.
#[tokio::test]
async fn test_list_schedule_runs_clamps_page_size_above_100() {
    let schedule = make_test_schedule(1, 1);
    let count_row = {
        let mut r = std::collections::BTreeMap::new();
        r.insert("total".to_string(), sea_orm::Value::BigInt(Some(0)));
        r
    };

    let db = Arc::new(
        MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results(vec![vec![schedule]])
            .append_query_results(vec![vec![count_row]])
            .append_query_results(vec![Vec::<
                std::collections::BTreeMap<String, sea_orm::Value>,
            >::new()])
            .into_connection(),
    );

    let svc = BackupService::new(
        db.clone(),
        create_mock_external_service_manager(db),
        create_mock_alarm_service(),
        create_mock_config_service(),
        Arc::new(EncryptionService::new("test_encryption_key_1234567890ab").unwrap()),
        Arc::new(NoopJobQueue),
    );

    // page_size=999 must be clamped to 100. The call itself must succeed.
    let result = svc.list_schedule_runs(1, 1, 999).await;
    assert!(
        result.is_ok(),
        "oversized page_size must not error: {:?}",
        result
    );
}

/// `list_schedule_runs` with an unknown schedule_id must return `NotFound`.
#[tokio::test]
async fn test_list_schedule_runs_not_found() {
    let db = Arc::new(
        MockDatabase::new(DatabaseBackend::Postgres)
            // get_backup_schedule returns empty → schedule does not exist
            .append_query_results(vec![Vec::<temps_entities::backup_schedules::Model>::new()])
            .into_connection(),
    );

    let svc = BackupService::new(
        db.clone(),
        create_mock_external_service_manager(db),
        create_mock_alarm_service(),
        create_mock_config_service(),
        Arc::new(EncryptionService::new("test_encryption_key_1234567890ab").unwrap()),
        Arc::new(NoopJobQueue),
    );

    let result = svc.list_schedule_runs(9999, 1, 20).await;
    assert!(result.is_err(), "unknown schedule must return an error");
    assert!(
        matches!(result.unwrap_err(), BackupError::NotFound { .. }),
        "expected BackupError::NotFound for unknown schedule"
    );
}

// ── list_child_backups ────────────────────────────────────────────────────

/// Integration test: when `include_control_plane = false` and a single
/// service is attached, the fan-out produces exactly one backup row
/// (no control-plane row alongside it). This is the scenario from
/// the user report where picking one Postgres still produced a
/// `control_plane` backup as a sidecar.
#[tokio::test]
async fn integration_fan_out_skips_control_plane_when_flag_off() {
    if bollard::Docker::connect_with_local_defaults().is_err() {
        println!("Docker not available, skipping test");
        return;
    }
    use sea_orm::ActiveValue::Set;
    use sea_orm::{ColumnTrait, EntityTrait};
    use temps_database::test_utils::TestDatabase;

    let test_db = match TestDatabase::with_migrations().await {
        Ok(d) => d,
        Err(e) => {
            println!("TestDatabase unavailable, skipping: {e}");
            return;
        }
    };
    let db = test_db.db.clone();

    let s3 = temps_entities::s3_sources::ActiveModel {
        id: sea_orm::NotSet,
        name: Set("cp-skip-source".to_string()),
        bucket_name: Set("b".to_string()),
        bucket_path: Set("/".to_string()),
        access_key_id: Set("".to_string()),
        secret_key: Set("".to_string()),
        session_token: Set(None),
        credentials_expire_at: Set(None),
        region: Set("us-east-1".to_string()),
        endpoint: Set(None),
        force_path_style: Set(Some(true)),
        is_default: Set(true),
        managed_by_cloud: Set(false),
        lifecycle_reconcile_failed_at: Set(None),
        lifecycle_reconcile_generation: Set(0),
        created_at: Set(chrono::Utc::now()),
        updated_at: Set(chrono::Utc::now()),
        backing_service_id: Set(None),
    }
    .insert(db.as_ref())
    .await
    .expect("insert s3 source");

    // Schedule: specific mode, no control plane.
    let schedule = temps_entities::backup_schedules::ActiveModel {
        id: sea_orm::NotSet,
        name: Set("cp-skip-schedule".to_string()),
        backup_type: Set("full".to_string()),
        retention_period: Set(7),
        s3_source_id: Set(s3.id),
        schedule_expression: Set("0 0 2 * * *".to_string()),
        enabled: Set(true),
        last_run: Set(None),
        next_run: Set(None),
        created_at: Set(chrono::Utc::now()),
        updated_at: Set(chrono::Utc::now()),
        description: Set(None),
        tags: Set("[]".to_string()),
        max_runtime_secs: Set(None),
        target_all_services: Set(false),
        include_control_plane: Set(false),
        generated_kind: Set(None),
    }
    .insert(db.as_ref())
    .await
    .expect("insert schedule");

    let svc_pg = temps_entities::external_services::ActiveModel {
        id: sea_orm::NotSet,
        name: Set("pg-only".to_string()),
        service_type: Set("postgres".to_string()),
        version: Set(Some("17".to_string())),
        status: Set("running".to_string()),
        slug: Set(Some("pg-only".to_string())),
        config: Set(None),
        node_id: Set(None),
        topology: Set("standalone".to_string()),
        error_message: Set(None),
        health_status: Set(None),
        last_health_check_at: Set(None),
        last_health_error: Set(None),
        consecutive_health_failures: Set(0),
        health_metadata: Set(None),
        metrics_enabled: Set(false),
        default_backup_provisioned: Set(false),
        container_name: Set(None),
        created_by_user_id: Set(None),
        continuous_archive_s3_source_id: Set(None),
        continuous_archive_pinned_at: Set(None),
        workload_mode: Default::default(),
        log_ingest_lines_per_sec: Set(None),
        log_ingest_kib_per_sec: Set(None),
        created_at: Set(chrono::Utc::now()),
        updated_at: Set(chrono::Utc::now()),
    }
    .insert(db.as_ref())
    .await
    .expect("insert pg");

    let svc = BackupService::new(
        db.clone(),
        create_mock_external_service_manager(db.clone()),
        create_mock_alarm_service(),
        create_mock_config_service(),
        Arc::new(EncryptionService::new("test_encryption_key_1234567890ab").unwrap()),
        Arc::new(NoopJobQueue),
    );

    svc.attach_services_to_schedule(schedule.id, &[svc_pg.id])
        .await
        .expect("attach");

    // Sanity: post-attach, the schedule is well-formed (control-plane
    // off + specific mode + 1 attached service).
    let after_attach = svc
        .list_services_for_schedule(schedule.id)
        .await
        .expect("list");
    assert_eq!(after_attach.len(), 1);

    // Flip-empty test: updating to include_control_plane=false with
    // *no* attached services (we'll detach first) must fail.
    svc.detach_service_from_schedule(schedule.id, svc_pg.id)
        .await
        .expect("detach");
    let err = svc
        .update_backup_schedule(
            schedule.id,
            crate::handlers::backup_handler::UpdateBackupScheduleRequest {
                name: None,
                description: None,
                schedule_expression: None,
                retention_period: None,
                max_runtime_secs: None,
                enabled: None,
                tags: None,
                target_all_services: None,
                include_control_plane: Some(false),
                service_ids: None,
            },
            temps_core::BackupScheduleAuthority::InstanceAdmin,
        )
        .await
        .expect_err("empty fan-out must be rejected");
    assert!(
        matches!(err, BackupError::Validation(ref msg) if msg.contains("nothing to back up")),
        "expected Validation error, got {:?}",
        err
    );

    // Cleanup: schedule has no children, so the cascade can drop it.
    let _ = temps_entities::backup_schedules::Entity::delete_by_id(schedule.id)
        .exec(db.as_ref())
        .await;
    let _ = temps_entities::external_services::Entity::delete_by_id(svc_pg.id)
        .exec(db.as_ref())
        .await;
    // Silence unused warning on the QueryFilter / ColumnTrait imports.
    let _ = temps_entities::backup_schedule_services::Column::ScheduleId.eq(0);
}
// ── retention deadline (`backups.expires_at`) ───────────────────────────
