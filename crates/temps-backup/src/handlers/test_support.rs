// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Test doubles and a [`BackupAppState`] builder shared by the handler tests.

use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use bollard::Docker;
use sea_orm::DatabaseConnection;
use temps_core::notifications::{
    EmailMessage, NotificationData, NotificationError, NotificationService,
};
use temps_core::{
    AuditLogger, AuditOperation, ProjectAccessChecker, SensitiveAction,
    SensitiveActionAuthorizationError, SensitiveActionAuthorizer, SensitiveActionDecision,
    SensitiveActionPrincipal,
};
use temps_deployment_logs::LogService;
use temps_providers::externalsvc::postgres_upgrade::{
    PostgresConnection, PostgresContainerLifecycle, PreUpgradeBackupProvider,
};
use temps_providers::postgres_upgrade_service::PostgresUpgradeService;

use super::types::BackupAppState;
use crate::execution::BackupExecutorBuilder;
use crate::services::{BackupService, RestoreService};

pub(crate) struct NoopNotifications;

#[async_trait]
impl NotificationService for NoopNotifications {
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
        Ok(false)
    }
}

pub(crate) struct NoopJobQueue;

#[async_trait]
impl temps_core::JobQueue for NoopJobQueue {
    async fn send(&self, _job: temps_core::Job) -> Result<(), temps_core::QueueError> {
        Ok(())
    }

    fn subscribe(&self) -> Box<dyn temps_core::JobReceiver> {
        unimplemented!("NoopJobQueue does not support subscribing in tests")
    }
}

#[derive(Default)]
pub(crate) struct RecordingAuditLogger {
    pub(crate) operations: Mutex<Vec<(String, String)>>,
}

#[async_trait]
impl AuditLogger for RecordingAuditLogger {
    async fn create_audit_log(
        &self,
        operation: &dyn AuditOperation,
    ) -> temps_core::AuditResult<()> {
        self.operations
            .lock()
            .expect("recording audit mutex should not be poisoned")
            .push((operation.operation_type(), operation.serialize()?));
        Ok(())
    }
}

pub(crate) struct AllowSensitiveActions;

#[async_trait]
impl SensitiveActionAuthorizer for AllowSensitiveActions {
    async fn authorize(
        &self,
        _action: &SensitiveAction,
        _principal: &SensitiveActionPrincipal,
    ) -> Result<SensitiveActionDecision, SensitiveActionAuthorizationError> {
        Ok(SensitiveActionDecision::Allow)
    }
}

pub(crate) struct StubBackupProvider;

#[async_trait]
impl PreUpgradeBackupProvider for StubBackupProvider {
    async fn default_s3_source_id(&self, _service_id: i32) -> Result<Option<i32>, String> {
        Ok(Some(1))
    }

    async fn create_pre_upgrade_backup(
        &self,
        _service_id: i32,
        _s3_source_id: i32,
        _created_by: i32,
    ) -> Result<i32, String> {
        Ok(1)
    }
}

pub(crate) struct StubLifecycle;

#[async_trait]
impl PostgresContainerLifecycle for StubLifecycle {
    async fn container_name(&self, service_id: i32) -> Result<String, String> {
        Ok(format!("postgres-{service_id}"))
    }

    async fn connection_params(&self, _service_id: i32) -> Result<PostgresConnection, String> {
        Ok(PostgresConnection {
            username: "postgres".to_string(),
            password: "secret".to_string(),
            database: "postgres".to_string(),
            port: "5432".to_string(),
        })
    }

    async fn docker_image(&self, _service_id: i32) -> Result<String, String> {
        Ok("postgres:16-bookworm".to_string())
    }

    async fn stop_and_remove(&self, _service_id: i32) -> Result<(), String> {
        Ok(())
    }

    async fn create_and_start(&self, _service_id: i32, _image: &str) -> Result<(), String> {
        Ok(())
    }

    async fn set_docker_image(&self, _service_id: i32, _image: &str) -> Result<(), String> {
        Ok(())
    }
}

pub(crate) fn server_config() -> temps_config::ServerConfig {
    temps_config::ServerConfig {
        address: "127.0.0.1:3000".to_string(),
        database_url: "postgres://localhost/test".to_string(),
        tls_address: None,
        console_address: "127.0.0.1:3001".to_string(),
        console_admin_address: None,
        console_ingress_address: None,
        admin_allowed_ips: Vec::new(),
        admin_allowed_hosts: Vec::new(),
        admin_trust_forwarded_for: false,
        data_dir: std::env::temp_dir().join("temps-pg-upgrade-handler-tests"),
        auth_secret: "test-auth-secret".to_string(),
        encryption_key: "test-encryption-key".to_string(),
        api_base_url: "/api".to_string(),
        postgres_max_connections: Some(1),
        postgres_min_connections: Some(0),
        postgres_connect_timeout_secs: Some(1),
        postgres_acquire_timeout_secs: Some(1),
        postgres_idle_timeout_secs: Some(1),
        postgres_max_lifetime_secs: Some(1),
        clickhouse_url: None,
        clickhouse_database: None,
        clickhouse_user: None,
        clickhouse_password: None,
        docker_extra_networks: Vec::new(),
    }
}

pub(crate) fn test_state(
    db: Arc<DatabaseConnection>,
    audit_service: Arc<dyn AuditLogger>,
    project_access_checker: Option<Arc<dyn ProjectAccessChecker>>,
) -> Arc<BackupAppState> {
    let encryption = Arc::new(temps_core::EncryptionService::new_from_password(
        "pg-upgrade-handler-tests",
    ));
    let docker = Arc::new(
        Docker::connect_with_local_defaults()
            .expect("Docker client configuration should be available"),
    );
    let external_service_manager = Arc::new(temps_providers::ExternalServiceManager::new(
        db.clone(),
        encryption.clone(),
        docker.clone(),
        Arc::new(temps_providers::DnsRegistry::new(db.clone())),
    ));
    let alarm_service = Arc::new(temps_alarms::alarm_service::AlarmService::new(
        db.clone(),
        Arc::new(NoopNotifications),
        Arc::new(NoopJobQueue),
    ));
    let backup_service = Arc::new(BackupService::new(
        db.clone(),
        external_service_manager.clone(),
        alarm_service,
        Arc::new(temps_config::ConfigService::new(
            Arc::new(server_config()),
            db.clone(),
        )),
        encryption.clone(),
        Arc::new(NoopJobQueue),
    ));
    let restore_service = Arc::new(RestoreService::new(
        db.clone(),
        external_service_manager,
        encryption,
    ));
    let pg_upgrade_service = Arc::new(PostgresUpgradeService::new(
        db.clone(),
        Arc::new(temps_core::DockerHandle::available(docker)),
        Arc::new(StubBackupProvider),
        Arc::new(StubLifecycle),
        Arc::new(LogService::new(
            std::env::temp_dir().join("temps-pg-upgrade-tests"),
        )),
    ));

    Arc::new(BackupAppState {
        backup_service,
        restore_service,
        audit_service,
        pg_upgrade_service,
        db: db.clone(),
        backup_executor: Arc::new(BackupExecutorBuilder::new(db).build()),
        telemetry: Arc::new(temps_core::NoopTelemetryReporter),
        project_access_checker,
        sensitive_action_authorizer: Arc::new(AllowSensitiveActions),
    })
}
