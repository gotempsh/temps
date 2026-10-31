// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Fixtures shared by the backup service tests.

use super::*;
use bollard::Docker;
use sea_orm::{DatabaseBackend, MockDatabase};
use temps_core::notifications::{
    EmailMessage, NotificationData, NotificationError, NotificationService,
};
use temps_core::EncryptionService;

/// Minimal `backup_schedules::Model` fixture shared by the schedule
/// tests below.
pub(super) fn make_test_schedule(
    id: i32,
    s3_source_id: i32,
) -> temps_entities::backup_schedules::Model {
    temps_entities::backup_schedules::Model {
        id,
        name: format!("test-schedule-{}", id),
        backup_type: "full".to_string(),
        retention_period: 7,
        s3_source_id,
        schedule_expression: "0 0 * * * *".to_string(),
        enabled: true,
        last_run: None,
        next_run: Some(chrono::Utc::now() - chrono::Duration::minutes(1)),
        created_at: chrono::Utc::now(),
        updated_at: chrono::Utc::now(),
        description: None,
        tags: "[]".to_string(),
        max_runtime_secs: None,
        target_all_services: true,
        include_control_plane: true,
        generated_kind: None,
    }
}

/// A service over a mock database whose Docker client is never dialled:
/// these tests exercise authorization and validation only, so they must
/// not depend on a local Docker socket being present.
pub(super) fn make_scope_test_service(db: Arc<DatabaseConnection>) -> BackupService {
    let docker = Docker::connect_with_http("http://127.0.0.1:9", 1, bollard::API_DEFAULT_VERSION)
        .expect("building an HTTP Docker client does not connect");
    let external_service_manager = Arc::new(temps_providers::ExternalServiceManager::new(
        db.clone(),
        Arc::new(
            EncryptionService::new(
                "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef",
            )
            .unwrap(),
        ),
        Arc::new(docker),
        Arc::new(temps_providers::DnsRegistry::new(db.clone())),
    ));
    BackupService::new(
        db.clone(),
        external_service_manager,
        create_mock_alarm_service(),
        create_mock_config_service(),
        Arc::new(EncryptionService::new("test_encryption_key_1234567890ab").unwrap()),
        Arc::new(NoopJobQueue),
    )
}

pub(super) fn make_external_service_backup(
    id: i32,
    backup_id: i32,
    service_id: i32,
) -> temps_entities::external_service_backups::Model {
    temps_entities::external_service_backups::Model {
        id,
        service_id,
        backup_id,
        backup_type: "full".to_string(),
        state: "completed".to_string(),
        started_at: Utc::now(),
        finished_at: Some(Utc::now()),
        size_bytes: Some(1024),
        s3_location: format!("s3://bucket/{backup_id}/{service_id}"),
        error_message: None,
        metadata: serde_json::json!({}),
        checksum: None,
        compression_type: "gzip".to_string(),
        created_by: 1,
        expires_at: None,
        service_name_snapshot: Some(format!("service-{service_id}")),
        service_type_snapshot: Some("postgres".to_string()),
    }
}

// Simple mock notification service for testing
pub(super) struct TestNotificationService;

#[async_trait::async_trait]
impl NotificationService for TestNotificationService {
    async fn send_email(&self, _message: EmailMessage) -> Result<(), NotificationError> {
        Ok(())
    }

    async fn send_notification(
        &self,
        _notification: NotificationData,
    ) -> Result<(), NotificationError> {
        Ok(())
    }

    async fn is_configured(&self) -> Result<bool, NotificationError> {
        Ok(true)
    }
}

pub(super) fn create_mock_config_service() -> Arc<temps_config::ConfigService> {
    let server_config = temps_config::ServerConfig::new(
        "127.0.0.1:3000".to_string(),
        "postgres://localhost:5432/test".to_string(),
        None,
        Some("127.0.0.1:3001".to_string()),
    )
    .unwrap();

    // Create a mock database connection
    let db = Arc::new(MockDatabase::new(DatabaseBackend::Postgres).into_connection());

    Arc::new(temps_config::ConfigService::new(
        Arc::new(server_config),
        db,
    ))
}

pub(super) struct NoopJobQueue;

#[async_trait::async_trait]
impl temps_core::JobQueue for NoopJobQueue {
    async fn send(&self, _job: temps_core::Job) -> Result<(), temps_core::QueueError> {
        Ok(())
    }

    fn subscribe(&self) -> Box<dyn temps_core::JobReceiver> {
        unimplemented!("NoopJobQueue does not support subscribing in tests")
    }
}

pub(super) fn create_mock_alarm_service() -> Arc<AlarmService> {
    let db = Arc::new(MockDatabase::new(DatabaseBackend::Postgres).into_connection());
    Arc::new(AlarmService::new(
        db,
        Arc::new(TestNotificationService),
        Arc::new(NoopJobQueue),
    ))
}

pub(super) fn create_mock_external_service_manager(
    db: Arc<sea_orm::DatabaseConnection>,
) -> Arc<temps_providers::ExternalServiceManager> {
    // Create a mock encryption service with a test key
    let test_key = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";
    let encryption_service = Arc::new(EncryptionService::new(test_key).unwrap());

    // Create Docker connection
    let docker = Docker::connect_with_local_defaults().unwrap();

    let dns_registry = Arc::new(temps_providers::DnsRegistry::new(db.clone()));
    Arc::new(temps_providers::ExternalServiceManager::new(
        db,
        encryption_service,
        Arc::new(docker),
        dns_registry,
    ))
}

/// Helper: build a minimal backup model for MockDatabase.
pub(super) fn make_test_backup_model(id: i32) -> temps_entities::backups::Model {
    use chrono::Utc;
    temps_entities::backups::Model {
        id,
        name: format!("Backup {}", id),
        backup_id: format!("uuid-{}", id),
        schedule_id: None,
        schedule_run_id: None,
        backup_type: "full".to_string(),
        state: "completed".to_string(),
        started_at: Utc::now(),
        finished_at: Some(Utc::now()),
        s3_source_id: 1,
        s3_location: "s3://bucket/path".to_string(),
        error_message: None,
        metadata: "{}".to_string(),
        checksum: None,
        compression_type: "lz4".to_string(),
        created_by: 1,
        expires_at: None,
        size_bytes: Some(1024),
        file_count: None,
        tags: "[]".to_string(),
    }
}

pub(super) fn skip_if_no_docker() -> bool {
    match bollard::Docker::connect_with_local_defaults() {
        Ok(d) => {
            // A `ping` would be more accurate but is async; the
            // synchronous build is enough to keep tests green when the
            // daemon socket is missing entirely.
            drop(d);
            false
        }
        Err(_) => {
            println!("Docker not available, skipping test");
            true
        }
    }
}

pub(super) fn build_service_for_mock(
    db: Arc<sea_orm::DatabaseConnection>,
) -> Result<BackupService, ()> {
    if skip_if_no_docker() {
        return Err(());
    }
    Ok(BackupService::new(
        db.clone(),
        create_mock_external_service_manager(db),
        create_mock_alarm_service(),
        create_mock_config_service(),
        Arc::new(EncryptionService::new("test_encryption_key_1234567890ab").unwrap()),
        Arc::new(NoopJobQueue),
    ))
}
