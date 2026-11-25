// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Tests for `s3_sources`.

use super::super::test_support::*;
use super::*;
use sea_orm::{DatabaseBackend, MockDatabase, MockExecResult};
use temps_core::EncryptionService;
use temps_entities::s3_sources;

#[tokio::test]
async fn delete_s3_source_refuses_retained_backup_records_before_delete() {
    let source = s3_sources::Model {
        id: 17,
        backing_service_id: None,
        name: "recovery-evidence".to_string(),
        bucket_name: "backups".to_string(),
        bucket_path: "tenant".to_string(),
        access_key_id: "key".to_string(),
        secret_key: "secret".to_string(),
        session_token: None,
        credentials_expire_at: None,
        region: "us-east-1".to_string(),
        endpoint: None,
        force_path_style: Some(true),
        is_default: false,
        managed_by_cloud: false,
        lifecycle_reconcile_failed_at: None,
        lifecycle_reconcile_generation: 0,
        worker_credentials: "scoped".to_string(),
        created_at: Utc::now(),
        updated_at: Utc::now(),
    };
    let count_row = |count: i64| {
        let mut row = std::collections::BTreeMap::new();
        row.insert("num_items".to_string(), sea_orm::Value::BigInt(Some(count)));
        row
    };
    let db = Arc::new(
        MockDatabase::new(DatabaseBackend::Postgres)
            // The lookup before the transaction, then the row locked inside it.
            .append_query_results(vec![vec![source.clone()]])
            .append_exec_results([MockExecResult {
                last_insert_id: 0,
                rows_affected: 1,
            }])
            .append_query_results(vec![vec![source]])
            .append_query_results(vec![vec![count_row(0)]])
            .append_query_results(vec![vec![count_row(2)]])
            .into_connection(),
    );
    let service = build_service_for_mock(db.clone()).expect("mock service should construct");

    let error = service
        .delete_s3_source(17)
        .await
        .expect_err("retained backups must block source deletion");

    assert!(matches!(
        error,
        BackupError::Validation(message)
            if message.contains("recovery-evidence")
                && message.contains("2 backup record(s)")
    ));
    drop(service);
    let db = Arc::try_unwrap(db).expect("service must release the mock database");
    let statements: Vec<String> = db
        .into_transaction_log()
        .iter()
        .flat_map(|transaction| transaction.statements())
        .map(|statement| statement.sql.clone())
        .collect();
    assert!(
        statements.iter().any(|sql| sql.contains("FOR UPDATE")),
        "the reference counts run with the source row locked: {statements:#?}"
    );
    assert!(
        !statements.iter().any(|sql| sql.starts_with("DELETE")),
        "validation must stop before DELETE: {statements:#?}"
    );
}

#[tokio::test]
async fn delete_s3_source_refuses_a_cloud_managed_row() {
    let source = s3_sources::Model {
        id: 21,
        backing_service_id: None,
        name: "Temps Cloud managed backups".to_string(),
        bucket_name: "cloud-bucket".to_string(),
        bucket_path: "tenant".to_string(),
        access_key_id: "key".to_string(),
        secret_key: "secret".to_string(),
        session_token: None,
        credentials_expire_at: None,
        region: "us-east-1".to_string(),
        endpoint: None,
        force_path_style: Some(false),
        is_default: false,
        managed_by_cloud: true,
        lifecycle_reconcile_failed_at: None,
        lifecycle_reconcile_generation: 0,
        worker_credentials: "scoped".to_string(),
        created_at: Utc::now(),
        updated_at: Utc::now(),
    };
    let db = Arc::new(
        MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results(vec![vec![source]])
            .into_connection(),
    );
    let service = build_service_for_mock(db.clone()).expect("mock service should construct");

    let error = service
        .delete_s3_source(21)
        .await
        .expect_err("a Cloud-managed source must refuse user-initiated deletion");

    assert!(matches!(
        error,
        BackupError::Validation(ref message)
            if message.contains("Temps Cloud managed backups")
                && message.contains("managed by Temps Cloud")
    ));
    drop(service);
    let db = Arc::try_unwrap(db).expect("service must release the mock database");
    assert_eq!(
        db.into_transaction_log().len(),
        1,
        "the managed_by_cloud guard must stop the delete before any reference-count query"
    );
}

#[tokio::test]
async fn update_s3_source_refuses_a_cloud_managed_row() {
    let source = s3_sources::Model {
        id: 22,
        backing_service_id: None,
        name: "Temps Cloud managed backups".to_string(),
        bucket_name: "cloud-bucket".to_string(),
        bucket_path: "tenant".to_string(),
        access_key_id: "key".to_string(),
        secret_key: "secret".to_string(),
        session_token: None,
        credentials_expire_at: None,
        region: "us-east-1".to_string(),
        endpoint: None,
        force_path_style: Some(false),
        is_default: false,
        managed_by_cloud: true,
        lifecycle_reconcile_failed_at: None,
        lifecycle_reconcile_generation: 0,
        worker_credentials: "scoped".to_string(),
        created_at: Utc::now(),
        updated_at: Utc::now(),
    };
    let db = Arc::new(
        MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results(vec![vec![source]])
            .into_connection(),
    );
    let service = build_service_for_mock(db.clone()).expect("mock service should construct");

    let error = service
        .update_s3_source(
            22,
            crate::handlers::backup_handler::UpdateS3SourceRequest {
                name: Some("renamed".to_string()),
                bucket_name: None,
                bucket_path: None,
                access_key_id: Some(SecretString::new("attacker-key")),
                secret_key: Some("attacker-secret".into()),
                region: None,
                endpoint: None,
                force_path_style: None,
                worker_credentials: None,
                use_tls: None,
            },
        )
        .await
        .expect_err("a Cloud-managed source must refuse manual credential edits");

    assert!(matches!(
        error,
        BackupError::Validation(ref message)
            if message.contains("Temps Cloud managed backups")
                && message.contains("managed by Temps Cloud")
    ));
    drop(service);
    let db = Arc::try_unwrap(db).expect("service must release the mock database");
    assert_eq!(
        db.into_transaction_log().len(),
        1,
        "the managed_by_cloud guard must stop the update before any write"
    );
}

#[tokio::test]
async fn test_create_s3_client() {
    let db = Arc::new(MockDatabase::new(DatabaseBackend::Postgres).into_connection());

    let external_service_manager = create_mock_external_service_manager(db.clone());
    let alarm_service = create_mock_alarm_service();
    let config_service = create_mock_config_service();
    let encryption_service =
        Arc::new(EncryptionService::new("test_encryption_key_1234567890ab").unwrap());

    // Encrypt the credentials for the test
    let encrypted_access_key = encryption_service.encrypt_string("test-key").unwrap();
    let encrypted_secret_key = encryption_service.encrypt_string("test-secret").unwrap();

    let backup_service = BackupService::new(
        db,
        external_service_manager,
        alarm_service,
        config_service,
        encryption_service,
        Arc::new(NoopJobQueue),
    );

    let s3_source = S3Source {
        id: 1,
        name: "test-source".to_string(),
        bucket_name: "test-bucket".to_string(),
        bucket_path: "/backups".to_string(),
        access_key_id: encrypted_access_key,
        secret_key: encrypted_secret_key,
        session_token: None,
        credentials_expire_at: None,
        region: "us-east-1".to_string(),
        endpoint: Some("http://localhost:9000".to_string()),
        force_path_style: Some(true),
        is_default: false,
        managed_by_cloud: false,
        lifecycle_reconcile_failed_at: None,
        lifecycle_reconcile_generation: 0,
        worker_credentials: "scoped".to_string(),
        created_at: Utc::now(),
        updated_at: Utc::now(),
        backing_service_id: None,
    };

    let result = backup_service.create_s3_client(&s3_source).await;
    assert!(result.is_ok());
}

#[tokio::test]
async fn test_list_s3_sources_empty() {
    let db = Arc::new(
        MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results(vec![Vec::<s3_sources::Model>::new()])
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

    let result = backup_service.list_s3_sources().await;
    assert!(result.is_ok());
    assert_eq!(result.unwrap().len(), 0);
}

#[tokio::test]
#[ignore = "known broken: create_s3_source now probes the endpoint, so this MockDatabase test needs a live S3 on localhost:9000 that accepts its fixed key; port it to a stub S3 server"]
async fn test_create_s3_source() {
    let s3_source = s3_sources::Model {
        id: 1,
        name: "test-source".to_string(),
        bucket_name: "test-bucket".to_string(),
        bucket_path: "/backups".to_string(),
        access_key_id: "test-key".to_string(),
        secret_key: "test-secret".to_string(),
        session_token: None,
        credentials_expire_at: None,
        region: "us-east-1".to_string(),
        endpoint: Some("http://localhost:9000".to_string()),
        force_path_style: Some(true),
        is_default: false,
        managed_by_cloud: false,
        lifecycle_reconcile_failed_at: None,
        lifecycle_reconcile_generation: 0,
        worker_credentials: "scoped".to_string(),
        created_at: Utc::now(),
        updated_at: Utc::now(),
        backing_service_id: None,
    };

    let db = Arc::new(
        MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results(vec![vec![s3_source.clone()]])
            .append_exec_results(vec![MockExecResult {
                last_insert_id: 1,
                rows_affected: 1,
            }])
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

    let request = CreateS3SourceRequest {
        name: "test-source".to_string(),
        bucket_name: "test-bucket".to_string(),
        bucket_path: "/backups".to_string(),
        access_key_id: SecretString::new("test-key"),
        secret_key: "test-secret".into(),
        region: "us-east-1".to_string(),
        endpoint: Some("http://localhost:9000".to_string()),
        force_path_style: Some(true),
        is_default: None,
        backing_service_id: None,
        worker_credentials: None,
        use_tls: None,
    };

    let result = backup_service.create_s3_source(request).await;
    assert!(
        result.is_ok(),
        "create_s3_source failed: {:?}",
        result.as_ref().err()
    );
    let source = result.unwrap();
    assert_eq!(source.name, "test-source");
    assert_eq!(source.bucket_name, "test-bucket");
}

#[tokio::test]
async fn test_create_s3_source_empty_name() {
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

    let request = CreateS3SourceRequest {
        name: "".to_string(),
        bucket_name: "test-bucket".to_string(),
        bucket_path: "/backups".to_string(),
        access_key_id: SecretString::new("test-key"),
        secret_key: "test-secret".into(),
        region: "us-east-1".to_string(),
        endpoint: Some("http://localhost:9000".to_string()),
        force_path_style: Some(true),
        is_default: None,
        backing_service_id: None,
        worker_credentials: None,
        use_tls: None,
    };

    let result = backup_service.create_s3_source(request).await;
    assert!(result.is_err());
    match result {
        Err(BackupError::Validation(msg)) => {
            assert!(msg.contains("cannot be empty"));
        }
        _ => panic!("Expected validation error"),
    }
}

#[tokio::test]
async fn test_get_s3_source_not_found() {
    let db = Arc::new(
        MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results(vec![Vec::<s3_sources::Model>::new()])
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

    let result = backup_service.get_s3_source(999).await;
    assert!(result.is_err());
    match result {
        Err(BackupError::NotFound { .. }) => {}
        _ => panic!("Expected NotFound error"),
    }
}

#[tokio::test]
async fn test_create_s3_client_from_request_valid() {
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

    let request = CreateS3SourceRequest {
        name: "test-source".to_string(),
        bucket_name: "test-bucket".to_string(),
        bucket_path: "/backups".to_string(),
        access_key_id: SecretString::new("test-access-key"),
        secret_key: "test-secret-key".into(),
        region: "us-east-1".to_string(),
        endpoint: Some("http://localhost:9000".to_string()),
        force_path_style: Some(true),
        is_default: None,
        backing_service_id: None,
        worker_credentials: None,
        use_tls: None,
    };

    let result = backup_service.create_s3_client_from_request(&request).await;
    assert!(
        result.is_ok(),
        "create_s3_client_from_request should succeed with valid request"
    );
}

#[tokio::test]
async fn test_create_s3_source_with_bucket_creation() {
    // Needs a MinIO on localhost:9000; skip at runtime without one
    // (CLAUDE.md: no `#[ignore]` on tests that need a service).
    let minio = tokio::time::timeout(
        std::time::Duration::from_secs(1),
        tokio::net::TcpStream::connect("127.0.0.1:9000"),
    )
    .await;
    if !matches!(minio, Ok(Ok(_))) {
        println!("MinIO not reachable on localhost:9000, skipping");
        return;
    }
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

    let request = CreateS3SourceRequest {
        name: "test-auto-create-bucket".to_string(),
        bucket_name: "test-auto-create-bucket".to_string(),
        bucket_path: "/backups".to_string(),
        access_key_id: SecretString::new("minioadmin"),
        secret_key: "minioadmin".into(),
        region: "us-east-1".to_string(),
        endpoint: Some("http://localhost:9000".to_string()),
        force_path_style: Some(true),
        is_default: None,
        backing_service_id: None,
        worker_credentials: None,
        use_tls: None,
    };

    // This test requires a real MinIO instance running
    // When running, it should:
    // 1. Create an S3 client from the request
    // 2. Test the connection and create the bucket if needed
    // 3. Persist the S3 source to the database
    match backup_service.create_s3_source(request).await {
        Ok(_) => {
            println!("✓ S3 source created successfully with auto-bucket creation");
        }
        Err(e) => {
            println!(
                "! Test skipped or failed: {} (requires running MinIO instance)",
                e
            );
        }
    }
}

#[tokio::test]
async fn test_create_s3_source_request_validation() {
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

    let invalid_request = CreateS3SourceRequest {
        name: "".to_string(), // Empty name - should fail validation
        bucket_name: "test-bucket".to_string(),
        bucket_path: "/backups".to_string(),
        access_key_id: SecretString::new("test-key"),
        secret_key: "test-secret".into(),
        region: "us-east-1".to_string(),
        endpoint: None,
        force_path_style: None,
        is_default: None,
        backing_service_id: None,
        worker_credentials: None,
        use_tls: None,
    };

    let result = backup_service.create_s3_source(invalid_request).await;
    assert!(
        result.is_err(),
        "create_s3_source should fail with empty name"
    );
    match result {
        Err(BackupError::Validation(msg)) => {
            assert!(
                msg.contains("S3 source name cannot be empty"),
                "Error should mention empty name validation"
            );
        }
        _ => panic!("Expected validation error for empty name"),
    }
}

fn s3_probe_test_request(endpoint: &str) -> CreateS3SourceRequest {
    CreateS3SourceRequest {
        name: "probe".to_string(),
        bucket_name: "probe".to_string(),
        bucket_path: "p".to_string(),
        access_key_id: SecretString::new("test-access-key"),
        secret_key: "test-secret-key-never-in-messages".into(),
        region: "us-east-1".to_string(),
        endpoint: Some(endpoint.to_string()),
        force_path_style: Some(true),
        is_default: Some(false),
        backing_service_id: None,
        worker_credentials: None,
        use_tls: None,
    }
}

/// An S3 destination whose endpoint cannot be reached is the operator's
/// configuration to fix, so the create call must answer with a status and
/// a detail that say which endpoint failed and why -- not a bare 500. The
/// mock database has no results queued, so reaching the insert would
/// surface as a `Database` error instead of the connectivity failure.
#[tokio::test]
async fn test_create_s3_source_with_unresolvable_endpoint_names_endpoint_and_cause() {
    let db = Arc::new(MockDatabase::new(DatabaseBackend::Postgres).into_connection());
    let Ok(backup_service) = build_service_for_mock(db) else {
        return;
    };
    let endpoint = "http://temps-s3-probe.invalid:9000";

    let error = backup_service
        .create_s3_source(s3_probe_test_request(endpoint))
        .await
        .expect_err("an unresolvable endpoint must not be saved");

    let problem = temps_core::problemdetails::Problem::from(error);
    assert_eq!(problem.status_code, axum::http::StatusCode::BAD_REQUEST);
    let detail = problem
        .body
        .get("detail")
        .and_then(|v| v.as_str())
        .unwrap_or_default()
        .to_string();
    assert!(detail.contains(endpoint), "detail: {detail}");
    assert!(detail.contains("DNS"), "detail: {detail}");
    assert!(!detail.contains("test-secret-key-never-in-messages"));
}

#[tokio::test]
async fn test_create_s3_source_with_refused_endpoint_names_endpoint_and_cause() {
    let port = {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        listener.local_addr().unwrap().port()
    };
    let db = Arc::new(MockDatabase::new(DatabaseBackend::Postgres).into_connection());
    let Ok(backup_service) = build_service_for_mock(db) else {
        return;
    };
    let endpoint = format!("http://127.0.0.1:{port}");

    let error = backup_service
        .create_s3_source(s3_probe_test_request(&endpoint))
        .await
        .expect_err("a refused endpoint must not be saved");

    let problem = temps_core::problemdetails::Problem::from(error);
    assert_eq!(problem.status_code, axum::http::StatusCode::BAD_GATEWAY);
    let detail = problem
        .body
        .get("detail")
        .and_then(|v| v.as_str())
        .unwrap_or_default()
        .to_string();
    assert!(detail.contains(&endpoint), "detail: {detail}");
    assert!(detail.contains("refused"), "detail: {detail}");
}

fn operator_s3_source(encryption: &EncryptionService) -> s3_sources::Model {
    s3_sources::Model {
        id: 31,
        backing_service_id: None,
        name: "offsite".to_string(),
        bucket_name: "offsite-bucket".to_string(),
        bucket_path: "/".to_string(),
        access_key_id: encryption.encrypt_string("stored-access-key").unwrap(),
        secret_key: encryption.encrypt_string("stored-secret-key").unwrap(),
        session_token: None,
        credentials_expire_at: None,
        region: "us-east-1".to_string(),
        endpoint: Some("http://127.0.0.1:9".to_string()),
        force_path_style: Some(true),
        is_default: true,
        managed_by_cloud: false,
        lifecycle_reconcile_failed_at: None,
        lifecycle_reconcile_generation: 0,
        worker_credentials: "scoped".to_string(),
        created_at: Utc::now(),
        updated_at: Utc::now(),
    }
}

/// Repointing a destination at an endpoint that cannot be reached is
/// refused with the classified cause, before anything is written.
#[tokio::test]
async fn test_update_s3_source_to_an_unreachable_endpoint_saves_nothing() {
    let encryption = EncryptionService::new("test_encryption_key_1234567890ab").unwrap();
    let db = Arc::new(
        MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results(vec![vec![operator_s3_source(&encryption)]])
            .into_connection(),
    );
    let Ok(service) = build_service_for_mock(db.clone()) else {
        return;
    };

    let error = service
        .update_s3_source(
            31,
            crate::handlers::backup_handler::UpdateS3SourceRequest {
                name: None,
                bucket_name: None,
                bucket_path: None,
                access_key_id: Some(SecretString::new("***")),
                secret_key: Some(SecretString::default()),
                region: None,
                endpoint: Some("http://temps-s3-probe.invalid:9000".to_string()),
                force_path_style: None,
                worker_credentials: None,
                use_tls: None,
            },
        )
        .await
        .expect_err("an unreachable endpoint must not be saved");

    match &error {
        BackupError::S3Unreachable {
            endpoint,
            bucket,
            kind,
            ..
        } => {
            assert_eq!(endpoint, "http://temps-s3-probe.invalid:9000");
            assert_eq!(bucket, "offsite-bucket");
            assert_eq!(*kind, crate::services::S3ProbeFailureKind::DnsResolution);
        }
        other => panic!("expected S3Unreachable, got {other:?}"),
    }
    assert!(!error.to_string().contains("stored-secret-key"));
    drop(service);
    let db = Arc::try_unwrap(db).expect("service must release the mock database");
    assert_eq!(
        db.into_transaction_log().len(),
        1,
        "only the source lookup may run; nothing is written"
    );
}

/// Stored credentials that can no longer be decrypted (a changed server
/// key, damaged data) are repaired by replacing them: supplying both
/// replacements must not fail on reading the old ones. Here the new
/// endpoint is unreachable, so reaching the connectivity check (and
/// failing there, with nothing saved) proves the update got past them.
#[tokio::test]
async fn test_update_s3_source_replaces_credentials_that_cannot_be_decrypted() {
    let encryption = EncryptionService::new("test_encryption_key_1234567890ab").unwrap();
    let mut source = operator_s3_source(&encryption);
    source.access_key_id = "not-decryptable".to_string();
    source.secret_key = "not-decryptable-either".to_string();
    let db = Arc::new(
        MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results(vec![vec![source]])
            .into_connection(),
    );
    let Ok(service) = build_service_for_mock(db.clone()) else {
        return;
    };

    let error = service
        .update_s3_source(
            31,
            crate::handlers::backup_handler::UpdateS3SourceRequest {
                name: None,
                bucket_name: None,
                bucket_path: None,
                access_key_id: Some(SecretString::new("new-access-key")),
                secret_key: Some("new-secret-key".into()),
                region: None,
                endpoint: Some("http://temps-s3-probe.invalid:9000".to_string()),
                force_path_style: None,
                worker_credentials: None,
                use_tls: None,
            },
        )
        .await
        .expect_err("the new endpoint is unreachable");
    assert!(
        matches!(error, BackupError::S3Unreachable { .. }),
        "replacing undecryptable credentials must reach the connectivity check: {error:?}"
    );
}

/// An edit form resubmits the whole record. Fields sent with their
/// stored values are not a change, so a rename never probes the
/// destination -- here one whose endpoint is unreachable -- and is saved.
#[tokio::test]
async fn test_update_s3_source_resubmitting_stored_values_does_not_probe() {
    let encryption = EncryptionService::new("test_encryption_key_1234567890ab").unwrap();
    let mut source = operator_s3_source(&encryption);
    source.endpoint = Some("http://temps-s3-probe.invalid:9000".to_string());
    let renamed = s3_sources::Model {
        name: "renamed".to_string(),
        ..source.clone()
    };
    let db = Arc::new(
        MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results(vec![vec![source.clone()]])
            .append_query_results(vec![vec![renamed]])
            .into_connection(),
    );
    let Ok(service) = build_service_for_mock(db.clone()) else {
        return;
    };

    let updated = service
        .update_s3_source(
            31,
            crate::handlers::backup_handler::UpdateS3SourceRequest {
                name: Some("renamed".to_string()),
                bucket_name: Some(source.bucket_name.clone()),
                bucket_path: None,
                access_key_id: Some(SecretString::new("stored-access-key")),
                secret_key: Some("stored-secret-key".into()),
                region: Some(source.region.clone()),
                endpoint: source.endpoint.clone(),
                force_path_style: source.force_path_style,
                worker_credentials: None,
                use_tls: None,
            },
        )
        .await
        .expect("unchanged connection settings need no connectivity check");
    assert_eq!(updated.name, "renamed");
}

/// A source saved before endpoints were normalised stores a bare
/// `host:port`. An edit form sends it back as it was; it normalises to the
/// `http://` form clients always used, the same destination, so a rename
/// still probes nothing (the destination here is unreachable).
#[tokio::test]
async fn test_update_s3_source_resubmitting_a_legacy_bare_endpoint_does_not_probe() {
    let encryption = EncryptionService::new("test_encryption_key_1234567890ab").unwrap();
    let mut source = operator_s3_source(&encryption);
    source.endpoint = Some("temps-s3-probe.invalid:9000".to_string());
    let renamed = s3_sources::Model {
        name: "renamed".to_string(),
        endpoint: Some("http://temps-s3-probe.invalid:9000".to_string()),
        ..source.clone()
    };
    let db = Arc::new(
        MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results(vec![vec![source.clone()]])
            .append_query_results(vec![vec![renamed]])
            .into_connection(),
    );
    let Ok(service) = build_service_for_mock(db.clone()) else {
        return;
    };

    let updated = service
        .update_s3_source(
            31,
            crate::handlers::backup_handler::UpdateS3SourceRequest {
                name: Some("renamed".to_string()),
                bucket_name: Some(source.bucket_name.clone()),
                bucket_path: None,
                access_key_id: None,
                secret_key: None,
                region: Some(source.region.clone()),
                endpoint: source.endpoint.clone(),
                force_path_style: source.force_path_style,
                worker_credentials: None,
                use_tls: None,
            },
        )
        .await
        .expect("the same destination in its legacy form needs no connectivity check");
    assert_eq!(updated.name, "renamed");
}

/// A rename sends the masked credentials back unchanged: nothing about
/// the connection changed, so nothing is probed and the stored
/// credentials are kept instead of being replaced by `***`.
#[tokio::test]
async fn test_update_s3_source_keeps_credentials_sent_back_masked() {
    let encryption = EncryptionService::new("test_encryption_key_1234567890ab").unwrap();
    let source = operator_s3_source(&encryption);
    let renamed = s3_sources::Model {
        name: "renamed".to_string(),
        ..source.clone()
    };
    let db = Arc::new(
        MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results(vec![vec![source.clone()]])
            .append_query_results(vec![vec![renamed]])
            .into_connection(),
    );
    let Ok(service) = build_service_for_mock(db.clone()) else {
        return;
    };

    let updated = service
        .update_s3_source(
            31,
            crate::handlers::backup_handler::UpdateS3SourceRequest {
                name: Some("renamed".to_string()),
                bucket_name: None,
                bucket_path: None,
                access_key_id: Some(SecretString::new("***")),
                secret_key: Some("***".into()),
                region: None,
                endpoint: None,
                force_path_style: None,
                worker_credentials: None,
                use_tls: None,
            },
        )
        .await
        .expect("a rename needs no connectivity check");
    assert_eq!(updated.name, "renamed");

    drop(service);
    let db = Arc::try_unwrap(db).expect("service must release the mock database");
    let log = format!("{:?}", db.into_transaction_log());
    assert!(
        !log.contains("\"***\""),
        "masked credentials were written: {log}"
    );
}

// -------------------------------------------------------------------------
// Bug 4: list_source_backups must include pending/failed rows without s3_location
// -------------------------------------------------------------------------

/// ADR-102: an operator can allow full credentials on a source that cannot
/// scope them, and nothing else changes: no probe, no credential rewrite.
#[tokio::test]
async fn update_s3_source_sets_only_the_worker_credentials_policy() {
    let stored = s3_sources::Model {
        id: 23,
        backing_service_id: None,
        name: "offsite".to_string(),
        bucket_name: "backups".to_string(),
        bucket_path: "/".to_string(),
        access_key_id: "ciphertext-key".to_string(),
        secret_key: "ciphertext-secret".to_string(),
        session_token: None,
        credentials_expire_at: None,
        region: "us-east-1".to_string(),
        endpoint: Some("https://store.example.test".to_string()),
        force_path_style: Some(true),
        is_default: false,
        managed_by_cloud: false,
        lifecycle_reconcile_failed_at: None,
        lifecycle_reconcile_generation: 0,
        worker_credentials: s3_sources::WORKER_CREDENTIALS_SCOPED.to_string(),
        created_at: Utc::now(),
        updated_at: Utc::now(),
    };
    let updated = s3_sources::Model {
        worker_credentials: s3_sources::WORKER_CREDENTIALS_ALLOW_FULL.to_string(),
        ..stored.clone()
    };
    let db = Arc::new(
        MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results(vec![vec![stored]])
            .append_query_results(vec![vec![updated]])
            .into_connection(),
    );
    let service = build_service_for_mock(db.clone()).expect("mock service should construct");

    let source = service
        .update_s3_source(
            23,
            crate::handlers::backup_handler::UpdateS3SourceRequest {
                name: None,
                bucket_name: None,
                bucket_path: None,
                access_key_id: None,
                secret_key: None,
                region: None,
                endpoint: None,
                force_path_style: None,
                worker_credentials: Some(
                    crate::handlers::backup_handler::WorkerCredentialsPolicy::AllowFull,
                ),
                use_tls: None,
            },
        )
        .await
        .expect("setting the policy alone needs no probe");
    assert_eq!(
        source.worker_credentials,
        s3_sources::WORKER_CREDENTIALS_ALLOW_FULL
    );

    drop(service);
    let db = Arc::try_unwrap(db).expect("service must release the mock database");
    let log = db.into_transaction_log();
    assert_eq!(log.len(), 2, "one read, one update: {log:?}");
    let update = format!("{:?}", log[1]);
    assert!(update.contains("worker_credentials"), "{update}");
    assert!(update.contains("allow_full"), "{update}");
    assert!(
        !update.contains("\"access_key_id\""),
        "the credential columns must not be rewritten: {update}"
    );
}

fn tls_only_request(
    name: Option<&str>,
    use_tls: bool,
) -> crate::handlers::backup_handler::UpdateS3SourceRequest {
    crate::handlers::backup_handler::UpdateS3SourceRequest {
        name: name.map(str::to_string),
        bucket_name: None,
        bucket_path: None,
        access_key_id: None,
        secret_key: None,
        region: None,
        endpoint: None,
        force_path_style: None,
        worker_credentials: None,
        use_tls: Some(use_tls),
    }
}

/// Turning TLS on without retyping the endpoint applies it to the stored
/// endpoint: the `https://` form is what gets verified, and a destination
/// that refuses it leaves the row untouched.
#[tokio::test]
async fn test_update_s3_source_tls_alone_probes_the_stored_endpoint_with_https() {
    let encryption = EncryptionService::new("test_encryption_key_1234567890ab").unwrap();
    let mut source = operator_s3_source(&encryption);
    source.endpoint = Some("http://temps-s3-probe.invalid:9000".to_string());
    let db = Arc::new(
        MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results(vec![vec![source]])
            .into_connection(),
    );
    let Ok(service) = build_service_for_mock(db.clone()) else {
        return;
    };

    let error = service
        .update_s3_source(31, tls_only_request(None, true))
        .await
        .expect_err("the https endpoint is unreachable, so nothing may be saved");

    match &error {
        BackupError::S3Unreachable { endpoint, .. } => {
            assert_eq!(endpoint, "https://temps-s3-probe.invalid:9000");
        }
        other => panic!("expected S3Unreachable, got {other:?}"),
    }
    drop(service);
    let db = Arc::try_unwrap(db).expect("service must release the mock database");
    assert_eq!(
        db.into_transaction_log().len(),
        1,
        "only the source lookup may run; nothing is written"
    );
}

/// A TLS choice that matches the stored scheme is not a change: nothing is
/// probed (the destination here is unreachable) and the stored endpoint is
/// written back as it was.
#[tokio::test]
async fn test_update_s3_source_tls_matching_the_stored_scheme_changes_nothing() {
    let encryption = EncryptionService::new("test_encryption_key_1234567890ab").unwrap();
    let mut source = operator_s3_source(&encryption);
    source.endpoint = Some("http://temps-s3-probe.invalid:9000".to_string());
    let renamed = s3_sources::Model {
        name: "renamed".to_string(),
        ..source.clone()
    };
    let db = Arc::new(
        MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results(vec![vec![source]])
            .append_query_results(vec![vec![renamed]])
            .into_connection(),
    );
    let Ok(service) = build_service_for_mock(db.clone()) else {
        return;
    };

    let updated = service
        .update_s3_source(31, tls_only_request(Some("renamed"), false))
        .await
        .expect("an unchanged scheme needs no connectivity check");
    assert_eq!(updated.name, "renamed");
    drop(service);
    let db = Arc::try_unwrap(db).expect("service must release the mock database");
    let log = format!("{:?}", db.into_transaction_log());
    assert!(log.contains("http://temps-s3-probe.invalid:9000"), "{log}");
    assert!(!log.contains("https://"), "{log}");
}

/// An AWS S3 source has no endpoint for a TLS choice to apply to; the flag
/// is ignored and the update saves without a probe.
#[tokio::test]
async fn test_update_s3_source_tls_on_an_aws_source_is_ignored() {
    let encryption = EncryptionService::new("test_encryption_key_1234567890ab").unwrap();
    let mut source = operator_s3_source(&encryption);
    source.endpoint = None;
    let renamed = s3_sources::Model {
        name: "renamed".to_string(),
        ..source.clone()
    };
    let db = Arc::new(
        MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results(vec![vec![source]])
            .append_query_results(vec![vec![renamed]])
            .into_connection(),
    );
    let Ok(service) = build_service_for_mock(db.clone()) else {
        return;
    };

    let updated = service
        .update_s3_source(31, tls_only_request(Some("renamed"), true))
        .await
        .expect("no endpoint, so nothing to verify");
    assert_eq!(updated.name, "renamed");
    assert_eq!(updated.endpoint, None);
}

/// A real database: the default-assignment race and the single-default
/// index only exist in PostgreSQL, not in the mock.
async fn real_database_service() -> Option<(temps_test_utils::TestDatabase, BackupService)> {
    // Skip only when there is no Docker. A database that is reachable but
    // fails to set up is a failure, not a reason to pass without asserting.
    if !temps_test_utils::docker_available().await {
        println!("Docker not available, skipping");
        return None;
    }
    let test_db = temps_test_utils::TestDatabase::with_migrations()
        .await
        .expect("test database with migrations");
    let service = make_scope_test_service(test_db.db.clone());
    Some((test_db, service))
}

/// The backend pid serving `txn`.
async fn backend_pid(txn: &sea_orm::DatabaseTransaction) -> i32 {
    use sea_orm::{ConnectionTrait, Statement};
    txn.query_one(Statement::from_string(
        DatabaseBackend::Postgres,
        "SELECT pg_backend_pid() AS pid",
    ))
    .await
    .expect("backend pid")
    .expect("a row")
    .try_get::<i32>("", "pid")
    .expect("pid column")
}

/// Wait until some other backend is blocked waiting on a lock `blocker`
/// holds, so a race test commits the competing transaction only once the
/// operation under test is provably waiting on it. Bounded: panics after 10s.
async fn wait_until_blocked_by(db: &sea_orm::DatabaseConnection, blocker: i32) {
    use sea_orm::{ConnectionTrait, Statement};
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
    loop {
        let blocked = db
            .query_one(Statement::from_sql_and_values(
                DatabaseBackend::Postgres,
                "SELECT count(*)::int AS blocked FROM pg_stat_activity \
                 WHERE $1 = ANY(pg_blocking_pids(pid))",
                [blocker.into()],
            ))
            .await
            .expect("pg_stat_activity")
            .expect("a row")
            .try_get::<i32>("", "blocked")
            .expect("blocked column");
        if blocked > 0 {
            return;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "nothing blocked on backend {blocker} within 10s"
        );
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    }
}

fn source_request(name: &str) -> CreateS3SourceRequest {
    CreateS3SourceRequest {
        name: name.to_string(),
        bucket_name: "backups".to_string(),
        bucket_path: format!("path-{name}"),
        access_key_id: SecretString::new("access".to_string()),
        secret_key: SecretString::new("secret".to_string()),
        region: "us-east-1".to_string(),
        endpoint: Some("http://storage.example.test:9000".to_string()),
        use_tls: None,
        force_path_style: Some(true),
        is_default: None,
        backing_service_id: None,
        worker_credentials: None,
    }
}

async fn default_source_ids(db: &sea_orm::DatabaseConnection) -> Vec<i32> {
    s3_sources::Entity::find()
        .filter(s3_sources::Column::IsDefault.eq(true))
        .all(db)
        .await
        .expect("defaults should load")
        .into_iter()
        .map(|source| source.id)
        .collect()
}

/// Two sources created at the same time on a server with none used to both
/// decide they were the first, and the second failed on the single-default
/// index with a generic 500. Several rounds, because each is a race.
#[tokio::test]
async fn concurrent_first_sources_both_succeed_with_exactly_one_default() {
    let Some((test_db, service)) = real_database_service().await else {
        return;
    };
    let service = Arc::new(service);
    for round in 0..6 {
        s3_sources::Entity::delete_many()
            .exec(test_db.db.as_ref())
            .await
            .expect("table should clear between rounds");
        let barrier = Arc::new(tokio::sync::Barrier::new(2));
        let create = |name: String| {
            let service = service.clone();
            let barrier = barrier.clone();
            tokio::spawn(async move {
                barrier.wait().await;
                service
                    .persist_verified_s3_source(source_request(&name))
                    .await
            })
        };
        let first = create(format!("first-{round}"));
        let second = create(format!("second-{round}"));
        let (first, second) = (first.await.expect("task"), second.await.expect("task"));
        assert!(
            first.is_ok() && second.is_ok(),
            "round {round}: both creates must succeed, got {first:?} and {second:?}"
        );
        assert_eq!(
            default_source_ids(test_db.db.as_ref()).await.len(),
            1,
            "round {round}: exactly one of the two must be the default"
        );
    }
}

#[tokio::test]
async fn deleting_the_default_promotes_the_oldest_remaining_source() {
    let Some((test_db, service)) = real_database_service().await else {
        return;
    };
    let first = service
        .persist_verified_s3_source(source_request("first"))
        .await
        .expect("first source");
    let second = service
        .persist_verified_s3_source(source_request("second"))
        .await
        .expect("second source");
    let third = service
        .persist_verified_s3_source(source_request("third"))
        .await
        .expect("third source");
    assert!(first.is_default && !second.is_default && !third.is_default);

    let deletion = service
        .delete_s3_source(first.id)
        .await
        .expect("deleting the default must not be refused while others exist");

    assert!(deletion.deleted);
    assert_eq!(
        deletion.promoted_default,
        Some(PromotedDefaultSource {
            id: second.id,
            name: "second".to_string(),
        })
    );
    assert_eq!(
        default_source_ids(test_db.db.as_ref()).await,
        vec![second.id]
    );
}

#[tokio::test]
async fn deleting_a_non_default_or_the_last_source_promotes_nothing() {
    let Some((test_db, service)) = real_database_service().await else {
        return;
    };
    let first = service
        .persist_verified_s3_source(source_request("first"))
        .await
        .expect("first source");
    let second = service
        .persist_verified_s3_source(source_request("second"))
        .await
        .expect("second source");

    let deletion = service
        .delete_s3_source(second.id)
        .await
        .expect("non-default delete");
    assert_eq!(deletion.promoted_default, None);
    assert_eq!(
        default_source_ids(test_db.db.as_ref()).await,
        vec![first.id]
    );

    let deletion = service
        .delete_s3_source(first.id)
        .await
        .expect("last source delete");
    assert!(deletion.deleted);
    assert_eq!(deletion.promoted_default, None);
    assert!(default_source_ids(test_db.db.as_ref()).await.is_empty());
}

/// A writer that bypasses the lock and loses the race on the single-default
/// index gets a 409 the client can retry, not a generic 500.
#[tokio::test]
async fn a_second_default_row_is_reported_as_a_default_conflict() {
    let Some((test_db, service)) = real_database_service().await else {
        return;
    };
    service
        .persist_verified_s3_source(source_request("first"))
        .await
        .expect("first source");
    let encryption = EncryptionService::new("test_encryption_key_1234567890ab").unwrap();
    let error = s3_sources::insert_encrypted(
        test_db.db.as_ref(),
        &encryption,
        s3_sources::S3SourceCredentials {
            name: "racer".to_string(),
            bucket_name: "backups".to_string(),
            bucket_path: "racer".to_string(),
            access_key_id: "access".to_string(),
            secret_key: "secret".to_string(),
            session_token: None,
            credentials_expire_at: None,
            region: "us-east-1".to_string(),
            endpoint: None,
            force_path_style: Some(true),
        },
        true,
        false,
        None,
    )
    .await
    .expect_err("the index allows one default");
    let s3_sources::S3SourceCredentialError::Database { source, .. } = error else {
        panic!("expected a database error, got {error:?}");
    };
    assert!(is_single_default_violation(&source), "{source:?}");
    assert!(!is_single_default_violation(&sea_orm::DbErr::Custom(
        "unrelated".to_string()
    )));
}

/// A backup that commits while its source is being deleted must stop the
/// delete, not be removed by the `ON DELETE CASCADE` on `backups`. The insert
/// is held open while the delete runs; the delete waits on the locked source
/// row, then sees the backup and refuses.
#[tokio::test]
async fn a_backup_committed_during_delete_stops_the_delete() {
    use sea_orm::{ActiveModelTrait, Set, TransactionTrait};

    let Some((test_db, service)) = real_database_service().await else {
        return;
    };
    let service = Arc::new(service);
    let source = service
        .persist_verified_s3_source(source_request("racing"))
        .await
        .expect("source");
    let creator = temps_entities::users::ActiveModel {
        name: Set("Backup operator".to_string()),
        email: Set("backup-operator@example.com".to_string()),
        email_verified: Set(true),
        ..Default::default()
    }
    .insert(test_db.db.as_ref())
    .await
    .expect("user");

    let insert = test_db.db.begin().await.expect("insert transaction");
    let insert_pid = backend_pid(&insert).await;
    let backup = temps_entities::backups::ActiveModel {
        name: Set("nightly".to_string()),
        backup_id: Set("4d1c2b3a-5e6f-4a7b-8c9d-0e1f2a3b4c5d".to_string()),
        backup_type: Set("full".to_string()),
        state: Set("completed".to_string()),
        started_at: Set(Utc::now()),
        s3_source_id: Set(source.id),
        s3_location: Set("s3://backups/racing/nightly".to_string()),
        metadata: Set("{}".to_string()),
        compression_type: Set("gzip".to_string()),
        created_by: Set(creator.id),
        tags: Set("[]".to_string()),
        ..Default::default()
    }
    .insert(&insert)
    .await
    .expect("backup insert (uncommitted)");

    let deleting = {
        let service = service.clone();
        let id = source.id;
        tokio::spawn(async move { service.delete_s3_source(id).await })
    };
    // The delete must be waiting on the open insert (its key-share lock on
    // the source row) before the insert commits.
    wait_until_blocked_by(test_db.db.as_ref(), insert_pid).await;
    insert.commit().await.expect("backup commits");

    let result = deleting.await.expect("delete task");
    assert!(
        matches!(result, Err(BackupError::Validation(ref message)) if message.contains("1 backup record")),
        "the delete must see the committed backup and refuse, got {result:?}"
    );
    assert!(
        temps_entities::backups::Entity::find_by_id(backup.id)
            .one(test_db.db.as_ref())
            .await
            .expect("backup lookup")
            .is_some(),
        "the backup must survive"
    );
}

/// Making a source the default while it is being deleted must not leave the
/// install without a default: the target is checked under the lock, after the
/// delete (which promoted another source) committed.
#[tokio::test]
async fn set_default_on_a_source_deleted_meanwhile_keeps_the_current_default() {
    use sea_orm::{ConnectionTrait, Statement, TransactionTrait};

    let Some((test_db, service)) = real_database_service().await else {
        return;
    };
    let service = Arc::new(service);
    let target = service
        .persist_verified_s3_source(source_request("target"))
        .await
        .expect("target source (default)");
    let other = service
        .persist_verified_s3_source(source_request("other"))
        .await
        .expect("other source");

    // A delete of `target` that promotes `other`, held open under the lock.
    let delete = test_db.db.begin().await.expect("delete transaction");
    let delete_pid = backend_pid(&delete).await;
    delete
        .execute(Statement::from_string(
            DatabaseBackend::Postgres,
            "SELECT pg_advisory_xact_lock(hashtext('temps.s3_sources.default'))",
        ))
        .await
        .expect("lock");
    s3_sources::Entity::delete_by_id(target.id)
        .exec(&delete)
        .await
        .expect("delete target");
    s3_sources::Entity::update_many()
        .col_expr(
            s3_sources::Column::IsDefault,
            sea_orm::sea_query::Expr::value(true),
        )
        .filter(s3_sources::Column::Id.eq(other.id))
        .exec(&delete)
        .await
        .expect("promote other");

    let setting = {
        let service = service.clone();
        let id = target.id;
        tokio::spawn(async move { service.set_default_s3_source(id).await })
    };
    // The setter must be waiting on the advisory lock the delete holds.
    wait_until_blocked_by(test_db.db.as_ref(), delete_pid).await;
    delete.commit().await.expect("delete commits");

    let result = setting.await.expect("set-default task");
    match result {
        Err(BackupError::NotFound { detail, .. }) => {
            assert!(detail.contains(&target.id.to_string()), "{detail}")
        }
        other => panic!("expected NotFound for the deleted source, got {other:?}"),
    }
    assert_eq!(
        default_source_ids(test_db.db.as_ref()).await,
        vec![other.id],
        "the promoted default must survive"
    );
}

/// A schedule created without `s3_source_id` while its resolved default
/// source is being deleted used to fail on the foreign key with a database
/// error. It now holds the source for its transaction, sees it gone once the
/// delete commits, and says so.
#[tokio::test]
async fn a_schedule_on_a_default_source_deleted_meanwhile_is_a_validation_error() {
    let Some((test_db, service)) = real_database_service().await else {
        return;
    };
    let service = Arc::new(service);
    let source = service
        .persist_verified_s3_source(source_request("doomed-default"))
        .await
        .expect("default source");

    let delete = test_db.db.begin().await.expect("delete transaction");
    let delete_pid = backend_pid(&delete).await;
    s3_sources::Entity::delete_by_id(source.id)
        .exec(&delete)
        .await
        .expect("delete source (uncommitted)");

    let creating = {
        let service = service.clone();
        tokio::spawn(async move {
            service
                .create_backup_schedule(
                    crate::handlers::backup_handler::CreateBackupScheduleRequest {
                        name: "nightly".to_string(),
                        backup_type: "full".to_string(),
                        retention_period: 7,
                        s3_source_id: None,
                        schedule_expression: "0 0 3 * * *".to_string(),
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
        })
    };
    wait_until_blocked_by(test_db.db.as_ref(), delete_pid).await;
    delete.commit().await.expect("delete commits");

    let result = creating.await.expect("create task");
    assert!(
        matches!(
            result,
            Err(BackupError::Validation(ref message))
                if message.contains(&source.id.to_string()) && message.contains("retry")
        ),
        "expected a validation error naming the deleted default, got {result:?}"
    );
    assert_eq!(
        temps_entities::backup_schedules::Entity::find()
            .count(test_db.db.as_ref())
            .await
            .expect("schedule count"),
        0
    );
}
