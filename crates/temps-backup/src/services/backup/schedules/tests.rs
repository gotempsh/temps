// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Tests for `schedules`.

use super::super::test_support::*;
use super::*;
use sea_orm::{DatabaseBackend, MockDatabase, MockExecResult};
use temps_core::EncryptionService;
use temps_entities::{backup_schedules, s3_sources};

#[test]
fn explicit_target_rejects_destination_backing_service() {
    let backing = HashSet::from([41]);
    let error = reject_source_backing_targets(&backing, [7, 41])
        .expect_err("the destination service must never be an explicit target");
    assert!(matches!(error, BackupError::Validation(_)));
    assert!(reject_source_backing_targets(&backing, [7, 42]).is_ok());
}

#[test]
fn target_all_expansion_excludes_destination_backing_service() {
    fn service(id: i32) -> temps_entities::external_services::Model {
        let now = Utc::now();
        temps_entities::external_services::Model {
            id,
            name: format!("service-{id}"),
            service_type: "rustfs".to_string(),
            version: None,
            status: "running".to_string(),
            created_at: now,
            updated_at: now,
            slug: None,
            config: None,
            node_id: None,
            topology: "standalone".to_string(),
            error_message: None,
            health_status: None,
            last_health_check_at: None,
            last_health_error: None,
            consecutive_health_failures: 0,
            health_metadata: None,
            metrics_enabled: false,
            default_backup_provisioned: false,
            container_name: None,
            created_by_user_id: None,
            continuous_archive_s3_source_id: None,
            continuous_archive_pinned_at: None,
            workload_mode: Default::default(),
            log_ingest_lines_per_sec: None,
            log_ingest_kib_per_sec: None,
        }
    }

    let expanded = exclude_source_backing_services(
        vec![service(7), service(41), service(42)],
        &HashSet::from([41]),
    );
    assert_eq!(
        expanded
            .iter()
            .map(|service| service.id)
            .collect::<Vec<_>>(),
        vec![7, 42]
    );
}

#[tokio::test]
async fn test_validate_backup_schedule_valid() {
    let db = Arc::new(MockDatabase::new(DatabaseBackend::Postgres).into_connection());

    let external_service_manager = create_mock_external_service_manager(db.clone());
    let alarm_service = create_mock_alarm_service();
    let config_service = create_mock_config_service();
    let encryption_service =
        Arc::new(EncryptionService::new("test_encryption_key_1234567890ab").unwrap());
    let backup_service = BackupService::new(
        db,
        external_service_manager,
        alarm_service,
        config_service,
        encryption_service,
        Arc::new(NoopJobQueue),
    );

    // Valid schedule: every day at 2 AM (24 hours apart) - cron format with seconds
    let result = backup_service.validate_backup_schedule("0 0 2 * * *");
    assert!(
        result.is_ok(),
        "Expected valid schedule to pass: {:?}",
        result
    );
}

#[tokio::test]
async fn test_validate_backup_schedule_too_frequent() {
    let db = Arc::new(MockDatabase::new(DatabaseBackend::Postgres).into_connection());

    let external_service_manager = create_mock_external_service_manager(db.clone());
    let alarm_service = create_mock_alarm_service();
    let config_service = create_mock_config_service();
    let encryption_service =
        Arc::new(EncryptionService::new("test_encryption_key_1234567890ab").unwrap());

    let backup_service = BackupService::new(
        db,
        external_service_manager,
        alarm_service,
        config_service,
        encryption_service,
        Arc::new(NoopJobQueue),
    );

    // Invalid schedule: every 30 minutes (too frequent) - cron format with seconds
    let result = backup_service.validate_backup_schedule("0 */30 * * * *");
    assert!(result.is_err(), "Expected error for too frequent schedule");
    match result {
        Err(BackupError::Validation(msg)) => {
            assert!(
                msg.contains("at least 1 hour apart"),
                "Error message should mention minimum interval: {}",
                msg
            );
        }
        other => panic!("Expected validation error, got: {:?}", other),
    }
}

#[tokio::test]
async fn test_validate_backup_schedule_invalid_cron() {
    let db = Arc::new(MockDatabase::new(DatabaseBackend::Postgres).into_connection());

    let external_service_manager = create_mock_external_service_manager(db.clone());
    let alarm_service = create_mock_alarm_service();
    let config_service = create_mock_config_service();
    let encryption_service =
        Arc::new(EncryptionService::new("test_encryption_key_1234567890ab").unwrap());
    let backup_service = BackupService::new(
        db,
        external_service_manager,
        alarm_service,
        config_service,
        encryption_service,
        Arc::new(NoopJobQueue),
    );

    // Invalid cron expression
    let result = backup_service.validate_backup_schedule("invalid cron");
    assert!(result.is_err());
}

#[tokio::test]
async fn test_list_backup_schedules_empty() {
    let db = Arc::new(
        MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results(vec![Vec::<backup_schedules::Model>::new()])
            .into_connection(),
    );

    let external_service_manager = create_mock_external_service_manager(db.clone());
    let alarm_service = create_mock_alarm_service();
    let config_service = create_mock_config_service();
    let encryption_service =
        Arc::new(EncryptionService::new("test_encryption_key_1234567890ab").unwrap());
    let backup_service = BackupService::new(
        db,
        external_service_manager,
        alarm_service,
        config_service,
        encryption_service,
        Arc::new(NoopJobQueue),
    );

    let result = backup_service.list_backup_schedules().await;
    assert!(result.is_ok());
    assert_eq!(result.unwrap().len(), 0);
}

#[tokio::test]
async fn test_get_backup_schedule_not_found() {
    let db = Arc::new(
        MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results(vec![Vec::<backup_schedules::Model>::new()])
            .into_connection(),
    );

    let external_service_manager = create_mock_external_service_manager(db.clone());
    let alarm_service = create_mock_alarm_service();
    let config_service = create_mock_config_service();
    let encryption_service =
        Arc::new(EncryptionService::new("test_encryption_key_1234567890ab").unwrap());
    let backup_service = BackupService::new(
        db,
        external_service_manager,
        alarm_service,
        config_service,
        encryption_service,
        Arc::new(NoopJobQueue),
    );

    let result = backup_service.get_backup_schedule(999).await;
    assert!(result.is_err());
    match result {
        Err(BackupError::NotFound { .. }) => {}
        _ => panic!("Expected NotFound error"),
    }
}

/// Regression: `disable_backup_schedule` and `enable_backup_schedule`
/// must trigger the same S3 lifecycle reconcile that
/// `create_backup_schedule`/`update_backup_schedule`/
/// `delete_backup_schedule` already do. Without it, disabling a
/// source's last enabled schedule leaves a stale
/// `PutBucketLifecycleConfiguration` rule on the bucket that keeps
/// expiring objects indefinitely — and, after the hourly sweep was
/// scoped down to only actively-scheduled sources, there is no other
/// path that would ever clear it.
#[tokio::test]
async fn disable_and_enable_schedule_reconcile_s3_lifecycle_rules() {
    let docker = match bollard::Docker::connect_with_local_defaults() {
        Ok(docker) => docker,
        Err(error) => {
            println!("Docker not available, skipping test: {}", error);
            return;
        }
    };
    if let Err(error) = docker.ping().await {
        println!("Docker daemon not reachable, skipping test: {}", error);
        return;
    }

    use temps_database::test_utils::TestDatabase;
    use testcontainers::runners::AsyncRunner;

    let s3_container = crate::test_rustfs::rustfs_container_request()
        .start()
        .await
        .expect("Failed to start RustFS container");
    let s3_port = s3_container
        .get_host_port_ipv4(crate::test_rustfs::RUSTFS_S3_PORT)
        .await
        .expect("Failed to get RustFS port");
    let s3_endpoint = format!("http://localhost:{}", s3_port);
    crate::test_rustfs::wait_for_rustfs_ready(s3_port)
        .await
        .expect("RustFS did not become healthy");

    let test_db = TestDatabase::with_migrations()
        .await
        .expect("Failed to create test database");

    let s3_config = aws_sdk_s3::config::Builder::new()
        .behavior_version(aws_sdk_s3::config::BehaviorVersion::latest())
        .region(aws_sdk_s3::config::Region::new("us-east-1"))
        .credentials_provider(aws_sdk_s3::config::Credentials::new(
            crate::test_rustfs::RUSTFS_ACCESS_KEY,
            crate::test_rustfs::RUSTFS_SECRET_KEY,
            None,
            None,
            "test",
        ))
        .endpoint_url(&s3_endpoint)
        .force_path_style(true)
        .http_client(crate::engines::v2_common::bundled_roots_http_client())
        .build();
    let s3_client = aws_sdk_s3::Client::from_conf(s3_config);

    let bucket_name = "test-lifecycle-toggle";
    s3_client
        .create_bucket()
        .bucket(bucket_name)
        .send()
        .await
        .expect("Failed to create bucket");
    tokio::time::sleep(tokio::time::Duration::from_secs(1)).await;

    let external_service_manager = create_mock_external_service_manager(test_db.db.clone());
    let alarm_service = create_mock_alarm_service();
    let server_config = temps_config::ServerConfig::new(
        "127.0.0.1:3000".to_string(),
        test_db.database_url.clone(),
        None,
        None,
    )
    .unwrap();
    let config_service = Arc::new(temps_config::ConfigService::new(
        Arc::new(server_config),
        test_db.db.clone(),
    ));
    let encryption_service =
        Arc::new(EncryptionService::new("test_encryption_key_1234567890ab").unwrap());
    let backup_service = BackupService::new(
        test_db.db.clone(),
        external_service_manager,
        alarm_service,
        config_service,
        encryption_service,
        Arc::new(NoopJobQueue),
    );

    let s3_source = backup_service
        .create_s3_source(CreateS3SourceRequest {
            name: "test-rustfs-toggle".to_string(),
            bucket_name: bucket_name.to_string(),
            bucket_path: "/backups".to_string(),
            access_key_id: SecretString::new(crate::test_rustfs::RUSTFS_ACCESS_KEY),
            secret_key: crate::test_rustfs::RUSTFS_SECRET_KEY.into(),
            region: "us-east-1".to_string(),
            endpoint: Some(s3_endpoint.clone()),
            force_path_style: Some(true),
            is_default: None,
            backing_service_id: None,
        })
        .await
        .expect("Failed to create S3 source");

    let schedule = backup_service
        .create_backup_schedule(
            CreateBackupScheduleRequest {
                name: "toggle-schedule".to_string(),
                backup_type: "full".to_string(),
                retention_period: 7,
                s3_source_id: Some(s3_source.id),
                schedule_expression: "0 0 2 * * *".to_string(),
                enabled: true,
                description: None,
                tags: vec![],
                max_runtime_secs: None,
                target_all_services: None,
                include_control_plane: None,
                service_ids: vec![],
            },
            temps_core::BackupScheduleAuthority::InstanceAdmin,
        )
        .await
        .expect("Failed to create backup schedule");

    // Poll until the schedule creation's own reconcile has pushed the
    // retention-7d rule (proves the create-path reconcile ran, so the
    // baseline before disabling is "rule present").
    let rules_after_create = tokio::time::timeout(
        std::time::Duration::from_secs(10),
        poll_lifecycle_rule_count(&s3_client, bucket_name),
    )
    .await
    .expect("lifecycle rule must appear after schedule creation");
    assert_eq!(
        rules_after_create, 1,
        "expected exactly one rule (temps-retention-7d) after create"
    );

    backup_service
        .disable_backup_schedule(schedule.id)
        .await
        .expect("Failed to disable backup schedule");

    // The disable must clear the now-orphaned rule, not leave it
    // dangling on the bucket.
    let rules_after_disable = tokio::time::timeout(
        std::time::Duration::from_secs(10),
        poll_lifecycle_rule_count_becomes(&s3_client, bucket_name, 0),
    )
    .await
    .expect("lifecycle rule must be cleared after disabling the schedule");
    assert_eq!(
        rules_after_disable, 0,
        "disabling the sole schedule must clear the S3 lifecycle rule"
    );

    backup_service
        .enable_backup_schedule(schedule.id)
        .await
        .expect("Failed to enable backup schedule");

    // Re-enabling must push the rule back.
    let rules_after_enable = tokio::time::timeout(
        std::time::Duration::from_secs(10),
        poll_lifecycle_rule_count_becomes(&s3_client, bucket_name, 1),
    )
    .await
    .expect("lifecycle rule must reappear after re-enabling the schedule");
    assert_eq!(
        rules_after_enable, 1,
        "re-enabling the schedule must restore the S3 lifecycle rule"
    );
}

/// Number of lifecycle rules currently on the bucket, treating "no
/// lifecycle configuration at all" (`NoSuchLifecycleConfiguration`) as
/// zero rather than an error — that's the expected state before the
/// first reconcile, and after a reconcile clears the last rule.
async fn current_lifecycle_rule_count(client: &aws_sdk_s3::Client, bucket: &str) -> usize {
    match client
        .get_bucket_lifecycle_configuration()
        .bucket(bucket)
        .send()
        .await
    {
        Ok(resp) => resp.rules().len(),
        Err(err) => {
            let msg = format!("{err:?}");
            if msg.contains("NoSuchLifecycleConfiguration") {
                0
            } else {
                panic!("unexpected error reading bucket lifecycle config: {msg}");
            }
        }
    }
}

/// Polls until the bucket has at least one lifecycle rule, returning
/// the count once non-zero.
async fn poll_lifecycle_rule_count(client: &aws_sdk_s3::Client, bucket: &str) -> usize {
    loop {
        let count = current_lifecycle_rule_count(client, bucket).await;
        if count > 0 {
            return count;
        }
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    }
}

/// Polls until the bucket's lifecycle rule count equals `expected`.
async fn poll_lifecycle_rule_count_becomes(
    client: &aws_sdk_s3::Client,
    bucket: &str,
    expected: usize,
) -> usize {
    loop {
        let count = current_lifecycle_rule_count(client, bucket).await;
        if count == expected {
            return count;
        }
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    }
}

/// `update_backup_schedule` rejects an invalid cron expression before any
/// DB write: the early validation path returns `BackupError::Validation`
/// without reaching the active-model update step.
#[tokio::test]
async fn test_update_schedule_rejects_invalid_cron() {
    let schedule = make_test_schedule(1, 1);

    let db = Arc::new(
        MockDatabase::new(DatabaseBackend::Postgres)
            // get_backup_schedule SELECT
            .append_query_results(vec![vec![schedule]])
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

    let request = UpdateBackupScheduleRequest {
        name: None,
        description: None,
        schedule_expression: Some("not-a-cron".to_string()),
        retention_period: None,
        max_runtime_secs: None,
        enabled: None,
        tags: None,
        target_all_services: None,
        include_control_plane: None,
        service_ids: None,
    };

    let result = svc
        .update_backup_schedule(
            1,
            request,
            temps_core::BackupScheduleAuthority::InstanceAdmin,
        )
        .await;
    assert!(result.is_err(), "Invalid cron must be rejected");
    // Validation fires before any DB write — error is Validation, not Schedule,
    // because validate_backup_schedule wraps the cron parse error in Validation.
    match result.unwrap_err() {
        BackupError::Validation(_) | BackupError::Schedule(_) => {}
        other => panic!("Expected Validation or Schedule error, got: {:?}", other),
    }
}

/// When the cron expression changes, `next_run` must be recomputed to a
/// future timestamp. The updated model returned by the service must have a
/// non-None `next_run`.
#[tokio::test]
async fn test_update_schedule_recomputes_next_run_when_cron_changes() {
    let mut schedule = make_test_schedule(1, 1);
    // Use a cron that is definitely different from what `make_test_schedule` sets.
    schedule.schedule_expression = "0 0 0 * * *".to_string(); // daily

    let updated_row = temps_entities::backup_schedules::Model {
        schedule_expression: "0 0 2 * * *".to_string(), // 2 AM daily (new value)
        ..schedule.clone()
    };

    let db = Arc::new(
        MockDatabase::new(DatabaseBackend::Postgres)
            // get_backup_schedule SELECT
            .append_query_results(vec![vec![schedule]])
            // active.update() SELECT (Sea-ORM mock returns the next query result)
            .append_query_results(vec![vec![updated_row.clone()]])
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

    let request = UpdateBackupScheduleRequest {
        name: None,
        description: None,
        // Change from "0 0 0 * * *" to "0 0 2 * * *" — at least 1 h apart, valid.
        schedule_expression: Some("0 0 2 * * *".to_string()),
        retention_period: None,
        max_runtime_secs: None,
        enabled: None,
        tags: None,
        target_all_services: None,
        include_control_plane: None,
        service_ids: None,
    };

    let result = svc
        .update_backup_schedule(
            1,
            request,
            temps_core::BackupScheduleAuthority::InstanceAdmin,
        )
        .await;
    assert!(
        result.is_ok(),
        "Valid cron change must succeed: {:?}",
        result
    );
    let model = result.unwrap();
    assert_eq!(model.schedule_expression, "0 0 2 * * *");
}

/// When only `name` is set, the service must not blow up and must return
/// the updated model. The inactive fields are left at their existing values
/// (the active model only sets the columns that were `Some` in the request).
#[tokio::test]
async fn test_update_schedule_leaves_fields_untouched_when_absent() {
    let schedule = make_test_schedule(1, 1);

    let updated_row = temps_entities::backup_schedules::Model {
        name: "renamed".to_string(),
        ..schedule.clone()
    };

    let db = Arc::new(
        MockDatabase::new(DatabaseBackend::Postgres)
            // get_backup_schedule SELECT
            .append_query_results(vec![vec![schedule.clone()]])
            // active.update() returns the updated row
            .append_query_results(vec![vec![updated_row]])
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

    let request = UpdateBackupScheduleRequest {
        name: Some("renamed".to_string()),
        description: None,
        schedule_expression: None,
        retention_period: None,
        max_runtime_secs: None,
        enabled: None,
        tags: None,
        target_all_services: None,
        include_control_plane: None,
        service_ids: None,
    };

    let result = svc
        .update_backup_schedule(
            1,
            request,
            temps_core::BackupScheduleAuthority::InstanceAdmin,
        )
        .await;
    assert!(
        result.is_ok(),
        "Name-only update must succeed: {:?}",
        result
    );
    let model = result.unwrap();
    assert_eq!(model.name, "renamed");
    // Other fields unchanged from make_test_schedule defaults.
    assert_eq!(model.retention_period, schedule.retention_period);
    assert_eq!(model.schedule_expression, schedule.schedule_expression);
}

// ── NV1: instance-wide schedules need an instance administrator ────────

fn nv1_create_request(
    target_all_services: Option<bool>,
    include_control_plane: Option<bool>,
    service_ids: Vec<i32>,
) -> CreateBackupScheduleRequest {
    CreateBackupScheduleRequest {
        name: "nightly".to_string(),
        backup_type: "full".to_string(),
        retention_period: 7,
        s3_source_id: Some(1),
        schedule_expression: "0 0 2 * * *".to_string(),
        enabled: true,
        description: None,
        tags: vec![],
        max_runtime_secs: None,
        target_all_services,
        include_control_plane,
        service_ids,
    }
}

fn nv1_rename_request() -> UpdateBackupScheduleRequest {
    UpdateBackupScheduleRequest {
        name: Some("renamed".to_string()),
        description: None,
        schedule_expression: None,
        retention_period: None,
        max_runtime_secs: None,
        enabled: None,
        tags: None,
        target_all_services: None,
        include_control_plane: None,
        service_ids: None,
    }
}

fn nv1_project_scoped_schedule(id: i32) -> temps_entities::backup_schedules::Model {
    temps_entities::backup_schedules::Model {
        target_all_services: false,
        include_control_plane: false,
        ..make_test_schedule(id, 1)
    }
}

/// The rule itself: only the combination "scoped caller + instance-wide
/// reach" is refused; an administrator may do anything and a scoped
/// caller keeps project-scoped schedules.
#[test]
fn nv1_schedule_authority_refuses_only_scoped_instance_wide_schedules() {
    for (target_all, control_plane) in [(true, true), (true, false), (false, true)] {
        assert!(ensure_schedule_authority(
            BackupScheduleAuthority::InstanceAdmin,
            "backup schedule 1",
            target_all,
            control_plane,
            "create",
        )
        .is_ok());
        let refused = ensure_schedule_authority(
            BackupScheduleAuthority::Scoped,
            "backup schedule 1",
            target_all,
            control_plane,
            "create",
        );
        match refused {
            Err(BackupError::Forbidden { resource, detail }) => {
                assert_eq!(resource, "backup schedule 1");
                assert!(detail.contains("instance administrator"), "{detail}");
            }
            other => {
                panic!("scoped caller must be refused ({target_all}, {control_plane}): {other:?}")
            }
        }
    }
    assert!(ensure_schedule_authority(
        BackupScheduleAuthority::Scoped,
        "backup schedule 1",
        false,
        false,
        "create",
    )
    .is_ok());
}

/// A scoped caller (e.g. a `Role::Custom` key with `settings:write` +
/// `backups:write`) cannot create an instance-wide schedule through the
/// service, whichever flag makes it instance-wide -- including the
/// defaults -- and is refused before any database access.
#[tokio::test]
async fn nv1_scoped_caller_cannot_create_an_instance_wide_schedule() {
    for (target_all, control_plane) in [
        (None, None),
        (Some(true), Some(false)),
        (Some(false), Some(true)),
    ] {
        let db = Arc::new(MockDatabase::new(DatabaseBackend::Postgres).into_connection());
        let svc = make_scope_test_service(db);
        let result = svc
            .create_backup_schedule(
                nv1_create_request(target_all, control_plane, vec![]),
                BackupScheduleAuthority::Scoped,
            )
            .await;
        assert!(
            matches!(result, Err(BackupError::Forbidden { .. })),
            "({target_all:?}, {control_plane:?}) must be refused, got {result:?}"
        );
    }
}

/// A scoped caller still gets past the authority check for a
/// project-scoped schedule: the request reaches ordinary validation (here
/// the retention bound) exactly as before.
#[tokio::test]
async fn nv1_scoped_caller_keeps_creating_project_scoped_schedules() {
    let db = Arc::new(MockDatabase::new(DatabaseBackend::Postgres).into_connection());
    let svc = make_scope_test_service(db);
    let mut request = nv1_create_request(Some(false), Some(false), vec![42]);
    request.retention_period = 0;
    let result = svc
        .create_backup_schedule(request, BackupScheduleAuthority::Scoped)
        .await;
    assert!(
        matches!(result, Err(BackupError::Validation(_))),
        "a project-scoped create must reach validation, got {result:?}"
    );
}

/// Editing a project-scoped schedule as a scoped caller is unchanged.
#[tokio::test]
async fn nv1_scoped_caller_keeps_updating_project_scoped_schedules() {
    let schedule = nv1_project_scoped_schedule(1);
    let updated_row = temps_entities::backup_schedules::Model {
        name: "renamed".to_string(),
        ..schedule.clone()
    };
    // The schedule keeps one attached database, counted in the update's
    // transaction before the "nothing to back up" check.
    let mut count_row = BTreeMap::new();
    count_row.insert("num_items".to_string(), sea_orm::Value::BigInt(Some(1)));
    let db = Arc::new(
        MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results(vec![vec![schedule]])
            .append_query_results(vec![vec![count_row]])
            .append_query_results(vec![vec![updated_row]])
            .into_connection(),
    );
    let svc = make_scope_test_service(db);
    let model = svc
        .update_backup_schedule(1, nv1_rename_request(), BackupScheduleAuthority::Scoped)
        .await
        .expect("a project-scoped rename must still succeed for a scoped caller");
    assert_eq!(model.name, "renamed");
}

/// A scoped caller may neither edit an instance-wide schedule nor widen
/// a project-scoped one into one.
#[tokio::test]
async fn nv1_scoped_caller_cannot_edit_or_widen_into_an_instance_wide_schedule() {
    let db = Arc::new(
        MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results(vec![vec![make_test_schedule(1, 1)]])
            .into_connection(),
    );
    let svc = make_scope_test_service(db);
    let edit = svc
        .update_backup_schedule(1, nv1_rename_request(), BackupScheduleAuthority::Scoped)
        .await;
    assert!(
        matches!(edit, Err(BackupError::Forbidden { .. })),
        "editing an instance-wide schedule must be refused, got {edit:?}"
    );

    let db = Arc::new(
        MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results(vec![vec![nv1_project_scoped_schedule(2)]])
            .into_connection(),
    );
    let svc = make_scope_test_service(db);
    let widen = svc
        .update_backup_schedule(
            2,
            UpdateBackupScheduleRequest {
                include_control_plane: Some(true),
                ..nv1_rename_request()
            },
            BackupScheduleAuthority::Scoped,
        )
        .await;
    assert!(
        matches!(widen, Err(BackupError::Forbidden { .. })),
        "adding the control plane must be refused, got {widen:?}"
    );
}

/// Deleting an instance-wide schedule as a scoped caller is refused
/// before the DELETE runs: the mock holds no exec result, so reaching the
/// DELETE would surface as a database error, not `Forbidden`.
#[tokio::test]
async fn nv1_scoped_caller_cannot_delete_an_instance_wide_schedule() {
    let db = Arc::new(
        MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results(vec![vec![make_test_schedule(1, 1)]])
            .into_connection(),
    );
    let svc = make_scope_test_service(db);
    let result = svc
        .delete_backup_schedule(1, BackupScheduleAuthority::Scoped)
        .await;
    assert!(
        matches!(result, Err(BackupError::Forbidden { .. })),
        "got {result:?}"
    );
}

/// When `find_by_id` returns no row (empty result), the service must return
/// `BackupError::NotFound` without attempting an UPDATE.
#[tokio::test]
async fn test_update_schedule_not_found_returns_notfound() {
    let db = Arc::new(
        MockDatabase::new(DatabaseBackend::Postgres)
            // get_backup_schedule SELECT — empty result → schedule does not exist.
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

    let request = UpdateBackupScheduleRequest {
        name: Some("irrelevant".to_string()),
        description: None,
        schedule_expression: None,
        retention_period: None,
        max_runtime_secs: None,
        enabled: None,
        tags: None,
        target_all_services: None,
        include_control_plane: None,
        service_ids: None,
    };

    let result = svc
        .update_backup_schedule(
            999,
            request,
            temps_core::BackupScheduleAuthority::InstanceAdmin,
        )
        .await;
    assert!(result.is_err(), "Missing schedule must return NotFound");
    assert!(
        matches!(result.unwrap_err(), BackupError::NotFound { .. }),
        "Expected NotFound variant"
    );
}

// ── list_schedule_runs ────────────────────────────────────────────────────

#[tokio::test]
async fn attach_services_rejects_unknown_schedule() {
    let db = Arc::new(
        MockDatabase::new(DatabaseBackend::Postgres)
            // get_backup_schedule -> find_by_id returns empty
            .append_query_results(vec![Vec::<backup_schedules::Model>::new()])
            .into_connection(),
    );
    let Ok(svc) = build_service_for_mock(db) else {
        return;
    };

    let err = svc
        .attach_services_to_schedule(42, &[1, 2, 3])
        .await
        .expect_err("missing schedule should error");
    assert!(
        matches!(err, BackupError::NotFound { .. }),
        "expected NotFound, got {:?}",
        err
    );
}

#[tokio::test]
async fn attach_services_noop_on_empty_input() {
    let db = Arc::new(
        MockDatabase::new(DatabaseBackend::Postgres)
            // schedule lookup succeeds
            .append_query_results(vec![vec![make_test_schedule(7, 1)]])
            .into_connection(),
    );
    let Ok(svc) = build_service_for_mock(db) else {
        return;
    };

    // Empty list must short-circuit before any further query is issued —
    // we only queued one query result (the schedule lookup).
    let inserted = svc
        .attach_services_to_schedule(7, &[])
        .await
        .expect("empty attach should succeed");
    assert_eq!(inserted, 0);
}

// Note: validation-of-unknown-service-ids is covered by the integration
// test (`integration_attach_list_detach_round_trip`) because mocking
// Sea-ORM's `.count()` requires a query-result shape that `MockDatabase`
// does not accept generically. The integration test exercises the same
// code path against a real Postgres.

/// Regression for the schedule-creation UI: a weekly schedule targeting
/// specific databases must be creatable without a control-plane backup.
/// The schedule row and membership are committed together.
#[tokio::test]
async fn integration_create_weekly_specific_schedule_without_control_plane() {
    if bollard::Docker::connect_with_local_defaults().is_err() {
        println!("Docker not available, skipping test");
        return;
    }
    use chrono::{Datelike, Weekday};
    use sea_orm::ActiveValue::Set;
    use temps_database::test_utils::TestDatabase;

    let test_db = match TestDatabase::with_migrations().await {
        Ok(database) => database,
        Err(error) => {
            println!("TestDatabase unavailable, skipping: {error}");
            return;
        }
    };
    let db = test_db.db.clone();

    let s3_source = temps_entities::s3_sources::ActiveModel {
        name: Set("schedule-target-source".to_string()),
        bucket_name: Set("schedule-target-bucket".to_string()),
        bucket_path: Set("/".to_string()),
        access_key_id: Set(String::new()),
        secret_key: Set(String::new()),
        region: Set("us-east-1".to_string()),
        force_path_style: Set(Some(true)),
        is_default: Set(true),
        ..Default::default()
    }
    .insert(db.as_ref())
    .await
    .expect("insert S3 source");

    let database = temps_entities::external_services::ActiveModel {
        name: Set("selected-database".to_string()),
        service_type: Set("postgres".to_string()),
        status: Set("running".to_string()),
        topology: Set("standalone".to_string()),
        ..Default::default()
    }
    .insert(db.as_ref())
    .await
    .expect("insert selected database");

    let service = BackupService::new(
        db.clone(),
        create_mock_external_service_manager(db.clone()),
        create_mock_alarm_service(),
        create_mock_config_service(),
        Arc::new(EncryptionService::new("test_encryption_key_1234567890ab").unwrap()),
        Arc::new(NoopJobQueue),
    );

    let schedule = service
        .create_backup_schedule(
            CreateBackupScheduleRequest {
                name: "Weekly selected database".to_string(),
                backup_type: "full".to_string(),
                retention_period: 7,
                s3_source_id: Some(s3_source.id),
                schedule_expression: "0 0 0 * * SUN".to_string(),
                enabled: true,
                description: None,
                tags: vec![],
                max_runtime_secs: None,
                target_all_services: Some(false),
                include_control_plane: Some(false),
                service_ids: vec![database.id],
            },
            temps_core::BackupScheduleAuthority::InstanceAdmin,
        )
        .await
        .expect("weekly specific schedule should be created");

    assert!(!schedule.target_all_services);
    assert!(!schedule.include_control_plane);
    assert_eq!(
        schedule.next_run.map(|run| run.weekday()),
        Some(Weekday::Sun)
    );

    let memberships = temps_entities::backup_schedule_services::Entity::find()
        .filter(temps_entities::backup_schedule_services::Column::ScheduleId.eq(schedule.id))
        .all(db.as_ref())
        .await
        .expect("list schedule memberships");
    assert_eq!(memberships.len(), 1);
    assert_eq!(memberships[0].service_id, database.id);
}

#[tokio::test]
async fn detach_service_returns_false_when_no_row() {
    let db = Arc::new(
        MockDatabase::new(DatabaseBackend::Postgres)
            .append_exec_results(vec![MockExecResult {
                last_insert_id: 0,
                rows_affected: 0,
            }])
            .into_connection(),
    );
    let Ok(svc) = build_service_for_mock(db) else {
        return;
    };

    let removed = svc
        .detach_service_from_schedule(1, 2)
        .await
        .expect("detach should be idempotent");
    assert!(!removed, "no row → returns false");
}

#[tokio::test]
async fn detach_service_returns_true_when_removed() {
    let db = Arc::new(
        MockDatabase::new(DatabaseBackend::Postgres)
            .append_exec_results(vec![MockExecResult {
                last_insert_id: 0,
                rows_affected: 1,
            }])
            .into_connection(),
    );
    let Ok(svc) = build_service_for_mock(db) else {
        return;
    };

    let removed = svc
        .detach_service_from_schedule(1, 2)
        .await
        .expect("detach should succeed");
    assert!(removed);
}

#[tokio::test]
async fn list_services_for_unknown_schedule_returns_not_found() {
    let db = Arc::new(
        MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results(vec![Vec::<backup_schedules::Model>::new()])
            .into_connection(),
    );
    let Ok(svc) = build_service_for_mock(db) else {
        return;
    };

    let err = svc
        .list_services_for_schedule(404)
        .await
        .expect_err("missing schedule must error");
    assert!(matches!(err, BackupError::NotFound { .. }));
}

#[tokio::test]
async fn list_schedules_for_unknown_service_returns_not_found() {
    let db = Arc::new(
        MockDatabase::new(DatabaseBackend::Postgres)
            // external_services::Entity::find_by_id → empty
            .append_query_results(vec![Vec::<temps_entities::external_services::Model>::new()])
            .into_connection(),
    );
    let Ok(svc) = build_service_for_mock(db) else {
        return;
    };

    let err = svc
        .list_schedules_for_service(123)
        .await
        .expect_err("missing service must error");
    assert!(matches!(err, BackupError::NotFound { .. }));
}

/// Integration test: round-trip attach → list → detach against a real
/// Postgres backed by `TestDatabase::with_migrations`. Verifies the
/// migration creates the join table correctly, the FKs cascade on
/// service-and-schedule delete, and the resolver join returns the right
/// rows. Skips gracefully when Docker (and therefore the test Postgres)
/// is unavailable.
#[tokio::test]
async fn integration_attach_list_detach_round_trip() {
    if bollard::Docker::connect_with_local_defaults().is_err() {
        println!("Docker not available, skipping test");
        return;
    }
    use sea_orm::ActiveValue::Set;
    use sea_orm::{ColumnTrait, EntityTrait, QueryFilter};
    use temps_database::test_utils::TestDatabase;

    let test_db = match TestDatabase::with_migrations().await {
        Ok(d) => d,
        Err(e) => {
            println!("TestDatabase unavailable, skipping: {e}");
            return;
        }
    };
    let db = test_db.db.clone();

    // Seed an S3 source (FK target for schedule).
    let s3_source = temps_entities::s3_sources::ActiveModel {
        id: sea_orm::NotSet,
        name: Set("integration-source".to_string()),
        bucket_name: Set("test-bucket".to_string()),
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

    // Seed a schedule. Use 'specific' mode so the explicit-membership
    // path is exercised by this test (the integration test for the
    // 'all' branch lives in `integration_flip_to_all_clears_membership`).
    let schedule = temps_entities::backup_schedules::ActiveModel {
        id: sea_orm::NotSet,
        name: Set("integration-schedule".to_string()),
        backup_type: Set("full".to_string()),
        retention_period: Set(7),
        s3_source_id: Set(s3_source.id),
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
        include_control_plane: Set(true),
        generated_kind: Set(None),
    }
    .insert(db.as_ref())
    .await
    .expect("insert schedule");

    // Seed two external services.
    let mk_svc = |name: &str, svc_type: &str| temps_entities::external_services::ActiveModel {
        id: sea_orm::NotSet,
        name: Set(name.to_string()),
        service_type: Set(svc_type.to_string()),
        version: Set(Some("17".to_string())),
        status: Set("running".to_string()),
        slug: Set(Some(name.to_string())),
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
    };
    let pg = mk_svc("pg-prod", "postgres")
        .insert(db.as_ref())
        .await
        .expect("insert pg service");
    let redis = mk_svc("redis-prod", "redis")
        .insert(db.as_ref())
        .await
        .expect("insert redis service");

    // Build a service. We can't use build_service_for_mock because we
    // want the *real* DB, not a mock. The Docker handle is required by
    // ExternalServiceManager but unused by these methods.
    let svc = BackupService::new(
        db.clone(),
        create_mock_external_service_manager(db.clone()),
        create_mock_alarm_service(),
        create_mock_config_service(),
        Arc::new(EncryptionService::new("test_encryption_key_1234567890ab").unwrap()),
        Arc::new(NoopJobQueue),
    );

    // 1) Attach both services.
    let inserted = svc
        .attach_services_to_schedule(schedule.id, &[pg.id, redis.id])
        .await
        .expect("attach succeeds");
    assert_eq!(inserted, 2, "both rows inserted");

    // 2) Re-attaching is idempotent (ON CONFLICT DO NOTHING).
    let inserted_again = svc
        .attach_services_to_schedule(schedule.id, &[pg.id, redis.id])
        .await
        .expect("re-attach succeeds");
    assert_eq!(inserted_again, 0, "no new rows on duplicate attach");

    // 3) list_services_for_schedule returns both, ordered by name.
    let listed = svc
        .list_services_for_schedule(schedule.id)
        .await
        .expect("list services");
    assert_eq!(listed.len(), 2);
    // Sorted by name: pg-prod < redis-prod
    assert_eq!(listed[0].name, "pg-prod");
    assert_eq!(listed[1].name, "redis-prod");

    // 4) list_schedules_for_service returns the schedule for each.
    let pg_schedules = svc
        .list_schedules_for_service(pg.id)
        .await
        .expect("list schedules for pg");
    assert_eq!(pg_schedules.len(), 1);
    assert_eq!(pg_schedules[0].id, schedule.id);

    // 5) Detach one service.
    let removed = svc
        .detach_service_from_schedule(schedule.id, pg.id)
        .await
        .expect("detach succeeds");
    assert!(removed);
    let listed = svc
        .list_services_for_schedule(schedule.id)
        .await
        .expect("list after detach");
    assert_eq!(listed.len(), 1);
    assert_eq!(listed[0].name, "redis-prod");

    // 6) Detach again is idempotent (returns false, no error).
    let removed_again = svc
        .detach_service_from_schedule(schedule.id, pg.id)
        .await
        .expect("idempotent detach");
    assert!(!removed_again);

    // 7) Cascade: deleting the schedule removes all membership rows.
    temps_entities::backup_schedules::Entity::delete_by_id(schedule.id)
        .exec(db.as_ref())
        .await
        .expect("delete schedule");
    let leftover = temps_entities::backup_schedule_services::Entity::find()
        .filter(temps_entities::backup_schedule_services::Column::ScheduleId.eq(schedule.id))
        .all(db.as_ref())
        .await
        .expect("count leftover");
    assert!(
        leftover.is_empty(),
        "schedule delete must cascade to membership"
    );
}

/// Integration test: when `target_all_services = true`, flipping a
/// schedule's mode via `update_backup_schedule` clears all explicit
/// membership rows (clean-slate behaviour). When set back to false,
/// the rows are not magically restored — the user has to attach
/// again. Skips gracefully when Docker / test Postgres are absent.
#[tokio::test]
async fn integration_flip_to_all_clears_membership() {
    if bollard::Docker::connect_with_local_defaults().is_err() {
        println!("Docker not available, skipping test");
        return;
    }
    use sea_orm::ActiveValue::Set;
    use sea_orm::{ColumnTrait, EntityTrait, QueryFilter};
    use temps_database::test_utils::TestDatabase;

    let test_db = match TestDatabase::with_migrations().await {
        Ok(d) => d,
        Err(e) => {
            println!("TestDatabase unavailable, skipping: {e}");
            return;
        }
    };
    let db = test_db.db.clone();

    // Seed S3 source + schedule (start in 'specific' mode so we have
    // membership rows to clear).
    let s3 = temps_entities::s3_sources::ActiveModel {
        id: sea_orm::NotSet,
        name: Set("flip-source".to_string()),
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

    let schedule = temps_entities::backup_schedules::ActiveModel {
        id: sea_orm::NotSet,
        name: Set("flip-schedule".to_string()),
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
        // Start as specific so we can attach rows.
        target_all_services: Set(false),
        include_control_plane: Set(true),
        generated_kind: Set(None),
    }
    .insert(db.as_ref())
    .await
    .expect("insert schedule");

    let svc_a = temps_entities::external_services::ActiveModel {
        id: sea_orm::NotSet,
        name: Set("svc-a".to_string()),
        service_type: Set("postgres".to_string()),
        version: Set(Some("17".to_string())),
        status: Set("running".to_string()),
        slug: Set(Some("svc-a".to_string())),
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
    .expect("insert svc-a");

    let svc = BackupService::new(
        db.clone(),
        create_mock_external_service_manager(db.clone()),
        create_mock_alarm_service(),
        create_mock_config_service(),
        Arc::new(EncryptionService::new("test_encryption_key_1234567890ab").unwrap()),
        Arc::new(NoopJobQueue),
    );

    // Attach svc-a to the specific schedule.
    svc.attach_services_to_schedule(schedule.id, &[svc_a.id])
        .await
        .expect("attach");

    let listed = svc
        .list_services_for_schedule(schedule.id)
        .await
        .expect("list pre-flip");
    assert_eq!(listed.len(), 1, "precondition: one service attached");

    // Flip to target_all_services = true via the service-layer update
    // (mirrors what the handler does on PATCH).
    svc.update_backup_schedule(
        schedule.id,
        crate::handlers::backup_handler::UpdateBackupScheduleRequest {
            name: None,
            description: None,
            schedule_expression: None,
            retention_period: None,
            max_runtime_secs: None,
            enabled: None,
            tags: None,
            target_all_services: Some(true),
            include_control_plane: None,
            service_ids: None,
        },
        temps_core::BackupScheduleAuthority::InstanceAdmin,
    )
    .await
    .expect("update succeeds");

    // Membership table must now be empty for this schedule.
    let after = temps_entities::backup_schedule_services::Entity::find()
        .filter(temps_entities::backup_schedule_services::Column::ScheduleId.eq(schedule.id))
        .all(db.as_ref())
        .await
        .expect("count after flip");
    assert!(
        after.is_empty(),
        "flipping to target_all_services=true must clear membership rows"
    );

    // Flip back to specific — list must stay empty (we cleared it).
    svc.update_backup_schedule(
        schedule.id,
        crate::handlers::backup_handler::UpdateBackupScheduleRequest {
            name: None,
            description: None,
            schedule_expression: None,
            retention_period: None,
            max_runtime_secs: None,
            enabled: None,
            tags: None,
            target_all_services: Some(false),
            include_control_plane: None,
            service_ids: None,
        },
        temps_core::BackupScheduleAuthority::InstanceAdmin,
    )
    .await
    .expect("update back to specific");

    let after_specific = svc
        .list_services_for_schedule(schedule.id)
        .await
        .expect("list after flip-back");
    assert!(
        after_specific.is_empty(),
        "flipping back to specific must not magically restore membership"
    );

    // The edit form can switch from all databases to one explicit
    // database while disabling the control-plane target in one PATCH.
    // This used to fail because the service validated the intermediate
    // target state before the UI could attach the selected database.
    let selected = svc
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
                target_all_services: Some(false),
                include_control_plane: Some(false),
                service_ids: Some(vec![svc_a.id]),
            },
            temps_core::BackupScheduleAuthority::InstanceAdmin,
        )
        .await
        .expect("atomic specific-target update succeeds");
    assert!(!selected.target_all_services);
    assert!(!selected.include_control_plane);

    let selected_services = svc
        .list_services_for_schedule(schedule.id)
        .await
        .expect("list after atomic specific-target update");
    assert_eq!(selected_services.len(), 1);
    assert_eq!(selected_services[0].id, svc_a.id);
}

/// Unit test (no DB needed): create_backup_schedule rejects a request
/// that would produce a no-op schedule (include_control_plane=false
/// AND target_all_services=false). The validation runs before any
/// DB call, so we don't even need a working Docker daemon for this.
#[tokio::test]
async fn create_rejects_empty_fan_out() {
    // Build a service with a mock DB. We never reach the DB because
    // validation fires first.
    if skip_if_no_docker() {
        return;
    }
    let db = Arc::new(
        MockDatabase::new(DatabaseBackend::Postgres)
            // resolve_s3_source_id: caller passed Some(1) so this query
            // (find_by_id) is the next thing the service does.
            .append_query_results(vec![vec![s3_sources::Model {
                id: 1,
                name: "s".to_string(),
                bucket_name: "b".to_string(),
                bucket_path: "/".to_string(),
                access_key_id: "".to_string(),
                secret_key: "".to_string(),
                session_token: None,
                credentials_expire_at: None,
                region: "us-east-1".to_string(),
                endpoint: None,
                force_path_style: Some(true),
                is_default: true,
                managed_by_cloud: false,
                lifecycle_reconcile_failed_at: None,
                lifecycle_reconcile_generation: 0,
                created_at: Utc::now(),
                updated_at: Utc::now(),
                backing_service_id: None,
            }]])
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

    let request = CreateBackupScheduleRequest {
        name: "bad".to_string(),
        backup_type: "full".to_string(),
        retention_period: 7,
        s3_source_id: Some(1),
        schedule_expression: "0 0 2 * * *".to_string(),
        enabled: true,
        description: None,
        tags: vec![],
        max_runtime_secs: None,
        target_all_services: Some(false),
        include_control_plane: Some(false),
        service_ids: vec![],
    };

    let err = svc
        .create_backup_schedule(request, temps_core::BackupScheduleAuthority::InstanceAdmin)
        .await
        .expect_err("empty fan-out must be rejected");
    assert!(
        matches!(err, BackupError::Validation(ref msg) if msg.contains("control plane")),
        "expected Validation error mentioning control plane, got {:?}",
        err
    );
}

/// The daily base-backup cron expression used by the auto-provisioner
/// (`reconcile_default_external_service_schedules`) must satisfy
/// `validate_backup_schedule`: parse under the `cron` crate's 6-field
/// format and produce adjacent runs at least one hour apart. This guards
/// the load-bearing literal so a typo can't ship a schedule that the
/// validator rejects at provision time.
#[tokio::test]
async fn auto_provision_cron_expression_is_valid() {
    // Same expression as provision_default_schedule_for_service.
    const DAILY_3AM: &str = "0 0 3 * * *";

    // Parses under the cron crate (6-field sec/min/hour/dom/mon/dow).
    let schedule =
        Schedule::from_str(DAILY_3AM).expect("auto-provision cron expression must parse");

    // Two adjacent runs are 24h apart -> passes the >= 1h rule in
    // validate_backup_schedule.
    let next_two: Vec<_> = schedule.upcoming(Utc).take(2).collect();
    assert_eq!(next_two.len(), 2, "expected two upcoming runs");
    let gap = next_two[1] - next_two[0];
    assert_eq!(
        gap.num_hours(),
        24,
        "daily base-backup runs should be 24h apart, got {} hours",
        gap.num_hours()
    );
}

/// `reconcile_default_external_service_schedules` is a safe no-op when no
/// default S3 source is configured: `resolve_s3_source_id(None)` errors,
/// the reconcile swallows it and returns `Ok(())` so the periodic tick can
/// retry once storage is configured. The MockDatabase returns an empty
/// `s3_sources` result for the `is_default = true` lookup, so the service
/// never reaches the MariaDB query.
#[tokio::test]
async fn reconcile_default_schedules_noop_without_default_source() {
    if skip_if_no_docker() {
        return;
    }
    let db = Arc::new(
        MockDatabase::new(DatabaseBackend::Postgres)
            // get_default_s3_source: no default configured -> empty result.
            .append_query_results(vec![Vec::<s3_sources::Model>::new()])
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

    let result = svc.reconcile_default_external_service_schedules().await;
    assert!(
        result.is_ok(),
        "reconcile must be a no-op (Ok) when no default S3 source exists, got {:?}",
        result
    );
}

/// Full-path integration test (needs a real DB + Docker): with a default
/// S3 source and an unprovisioned MariaDB service, reconcile creates
/// exactly one daily schedule, attaches the service to it, flips
/// `default_backup_provisioned`, and is idempotent on a second call.
#[tokio::test]
async fn reconcile_default_schedules_provisions_mariadb_once() {
    if skip_if_no_docker() {
        return;
    }
    use sea_orm::ActiveValue::Set;
    use sea_orm::{ColumnTrait, EntityTrait, QueryFilter};
    use temps_database::test_utils::TestDatabase;

    let test_db = match TestDatabase::with_migrations().await {
        Ok(d) => d,
        Err(e) => {
            println!("TestDatabase unavailable, skipping: {e}");
            return;
        }
    };
    let db = test_db.db.clone();

    // Default S3 source so resolve_s3_source_id(None) succeeds.
    temps_entities::s3_sources::ActiveModel {
        id: sea_orm::NotSet,
        name: Set("auto-prov-source".to_string()),
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

    // One unprovisioned MariaDB service + one Postgres service (which must
    // be left alone — scope is MariaDB only).
    let maria = temps_entities::external_services::ActiveModel {
        id: sea_orm::NotSet,
        name: Set("maria-auto".to_string()),
        service_type: Set("mariadb".to_string()),
        status: Set("running".to_string()),
        topology: Set("standalone".to_string()),
        ..Default::default()
    }
    .insert(db.as_ref())
    .await
    .expect("insert mariadb service");

    let pg = temps_entities::external_services::ActiveModel {
        id: sea_orm::NotSet,
        name: Set("pg-untouched".to_string()),
        service_type: Set("postgres".to_string()),
        status: Set("running".to_string()),
        topology: Set("standalone".to_string()),
        ..Default::default()
    }
    .insert(db.as_ref())
    .await
    .expect("insert postgres service");

    let svc = BackupService::new(
        db.clone(),
        create_mock_external_service_manager(db.clone()),
        create_mock_alarm_service(),
        create_mock_config_service(),
        Arc::new(EncryptionService::new("test_encryption_key_1234567890ab").unwrap()),
        Arc::new(NoopJobQueue),
    );

    svc.reconcile_default_external_service_schedules()
        .await
        .expect("first reconcile succeeds");

    // Exactly one schedule created.
    let schedules = temps_entities::backup_schedules::Entity::find()
        .all(db.as_ref())
        .await
        .expect("list schedules");
    assert_eq!(
        schedules.len(),
        1,
        "expected exactly one auto-provisioned schedule"
    );
    let schedule = &schedules[0];
    assert_eq!(schedule.schedule_expression, "0 0 3 * * *");
    assert_eq!(schedule.backup_type, "full");
    assert_eq!(schedule.retention_period, 14);
    assert!(!schedule.target_all_services);
    assert!(!schedule.include_control_plane);

    // The MariaDB service is attached to it.
    let attached = svc
        .list_services_for_schedule(schedule.id)
        .await
        .expect("list attached services");
    assert_eq!(attached.len(), 1, "exactly the mariadb service attached");
    assert_eq!(attached[0].id, maria.id);

    // Latch flipped on MariaDB, untouched on Postgres.
    let maria_after = temps_entities::external_services::Entity::find_by_id(maria.id)
        .one(db.as_ref())
        .await
        .expect("reload mariadb")
        .expect("mariadb exists");
    assert!(
        maria_after.default_backup_provisioned,
        "mariadb latch must be set after provisioning"
    );
    let pg_after = temps_entities::external_services::Entity::find_by_id(pg.id)
        .one(db.as_ref())
        .await
        .expect("reload postgres")
        .expect("postgres exists");
    assert!(
        !pg_after.default_backup_provisioned,
        "non-mariadb services must never be provisioned"
    );

    // Idempotency: a second reconcile creates nothing new.
    svc.reconcile_default_external_service_schedules()
        .await
        .expect("second reconcile succeeds");
    let count_after = temps_entities::backup_schedules::Entity::find()
        .filter(temps_entities::backup_schedules::Column::Id.eq(schedule.id))
        .count(db.as_ref())
        .await
        .expect("count");
    let total = temps_entities::backup_schedules::Entity::find()
        .count(db.as_ref())
        .await
        .expect("count all");
    assert_eq!(count_after, 1);
    assert_eq!(total, 1, "second reconcile must not create a duplicate");
}
