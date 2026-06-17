//! Service layer for PostgreSQL major-version upgrades.
//!
//! Entry point is `start_major_upgrade`, which validates the request,
//! inserts a `postgres_major_upgrades` row, and spawns the orchestrator.
//! The DB-level partial-unique index on active rows is the authoritative
//! concurrency lock; this service also performs a pre-flight check so
//! the error surfaces as `ConcurrentUpgrade` (409) instead of a raw
//! Postgres unique-violation.

use std::sync::{Arc, OnceLock};

use bollard::Docker;
use sea_orm::{
    ActiveModelTrait, ActiveValue::Set, ColumnTrait, DatabaseConnection, EntityTrait, QueryFilter,
};
use temps_core::telemetry::{NoopTelemetryReporter, TelemetryReporter};
use temps_entities::{external_services, postgres_major_upgrades};
use temps_logs::LogService;
use uuid::Uuid;

use crate::externalsvc::postgres_upgrade::{
    phase, status, validate_os_family, PostgresContainerLifecycle, PostgresUpgradeError,
    PostgresUpgradeOrchestrator, PreUpgradeBackupProvider,
};

/// Pure pre-flight validation for a major upgrade request. Separated so
/// unit tests can exercise the validation surface without a real Docker
/// daemon or DB. Returns the first error encountered.
pub fn validate_start_request(
    req: &StartMajorUpgradeRequest,
    service_type: &str,
) -> Result<(), PostgresUpgradeError> {
    let st = service_type.to_ascii_lowercase();
    if st != "postgres" && st != "postgresql" {
        return Err(PostgresUpgradeError::WrongServiceType {
            service_id: req.service_id,
            service_type: service_type.to_string(),
        });
    }

    validate_os_family(req.service_id, &req.from_image, &req.to_image)?;

    let from_n: u32 = req.from_version.parse().unwrap_or(0);
    let to_n: u32 = req.to_version.parse().unwrap_or(0);
    if to_n == 0 || from_n == 0 || to_n <= from_n {
        return Err(PostgresUpgradeError::InvalidVersionTransition {
            service_id: req.service_id,
            from_version: req.from_version.clone(),
            to_version: req.to_version.clone(),
            reason: "to_version must be a greater major version than from_version".into(),
        });
    }

    Ok(())
}

/// Request payload for starting a major-version upgrade.
#[derive(Debug, Clone)]
pub struct StartMajorUpgradeRequest {
    pub service_id: i32,
    pub from_version: String,
    pub to_version: String,
    pub from_image: String,
    pub to_image: String,
    pub created_by: i32,
}

pub struct PostgresUpgradeService {
    db: Arc<DatabaseConnection>,
    docker: Arc<Docker>,
    backup_provider: Arc<dyn PreUpgradeBackupProvider>,
    lifecycle: Arc<dyn PostgresContainerLifecycle>,
    log_service: Arc<LogService>,
    /// Late-bound anonymous telemetry reporter. Set via [`Self::set_telemetry`]
    /// after construction. Falls back to no-op when unset.
    telemetry: OnceLock<Arc<dyn TelemetryReporter>>,
}

impl PostgresUpgradeService {
    pub fn new(
        db: Arc<DatabaseConnection>,
        docker: Arc<Docker>,
        backup_provider: Arc<dyn PreUpgradeBackupProvider>,
        lifecycle: Arc<dyn PostgresContainerLifecycle>,
        log_service: Arc<LogService>,
    ) -> Self {
        Self {
            db,
            docker,
            backup_provider,
            lifecycle,
            log_service,
            telemetry: OnceLock::new(),
        }
    }

    /// Attach a telemetry reporter so orchestrators spawned by this service
    /// can emit anonymous product events on completion.
    pub fn set_telemetry(&self, reporter: Arc<dyn TelemetryReporter>) {
        let _ = self.telemetry.set(reporter);
    }

    fn telemetry(&self) -> Arc<dyn TelemetryReporter> {
        self.telemetry
            .get()
            .cloned()
            .unwrap_or_else(|| Arc::new(NoopTelemetryReporter))
    }

    /// Validate the request, insert the upgrade row, and spawn the
    /// orchestrator. Returns the new row so callers can surface the
    /// upgrade id + log id to the user immediately.
    pub async fn start_major_upgrade(
        &self,
        req: StartMajorUpgradeRequest,
    ) -> Result<postgres_major_upgrades::Model, PostgresUpgradeError> {
        // 1-3. Fetch the service and run the full pre-flight validation.
        let svc = external_services::Entity::find_by_id(req.service_id)
            .one(self.db.as_ref())
            .await?
            .ok_or(PostgresUpgradeError::NotFound {
                upgrade_id: req.service_id,
            })?;
        validate_start_request(&req, &svc.service_type)?;

        // 4. Defensive concurrency check. The partial-unique index is the
        //    real lock; this just turns the race into a typed 409.
        if let Some(existing) = postgres_major_upgrades::Entity::find()
            .filter(postgres_major_upgrades::Column::ServiceId.eq(req.service_id))
            .filter(
                postgres_major_upgrades::Column::Status
                    .eq(status::PENDING)
                    .or(postgres_major_upgrades::Column::Status.eq(status::RUNNING)),
            )
            .one(self.db.as_ref())
            .await?
        {
            return Err(PostgresUpgradeError::ConcurrentUpgrade {
                service_id: req.service_id,
                existing_upgrade_id: existing.id,
                status: existing.status,
            });
        }

        // 5. Insert the row. Any pre-existing default-S3 gap is caught by
        //    the orchestrator's pre_backup phase — we don't probe it here
        //    to keep the service call cheap and the error surface typed.
        let log_id = format!("pg-upgrade-{}", Uuid::new_v4());
        let active = postgres_major_upgrades::ActiveModel {
            service_id: Set(req.service_id),
            from_version: Set(req.from_version.clone()),
            to_version: Set(req.to_version.clone()),
            from_image: Set(req.from_image.clone()),
            to_image: Set(req.to_image.clone()),
            status: Set(status::PENDING.to_string()),
            phase: Set(phase::PRE_BACKUP.to_string()),
            log_id: Set(log_id.clone()),
            attempt: Set(1),
            created_by: Set(req.created_by),
            ..Default::default()
        };
        let inserted = active.insert(self.db.as_ref()).await?;

        // 6. Spawn the orchestrator; it owns its own Arc clones so the task
        //    lifetime is independent of this request.
        let orchestrator = PostgresUpgradeOrchestrator::new(
            self.db.clone(),
            self.docker.clone(),
            self.backup_provider.clone(),
            self.lifecycle.clone(),
            self.log_service.clone(),
        );
        orchestrator.set_telemetry(self.telemetry());
        let upgrade_id = inserted.id;
        let log_id_for_err = log_id.clone();
        let log_service = self.log_service.clone();
        let db_for_err = self.db.clone();
        tokio::spawn(async move {
            if let Err(e) = orchestrator.run(upgrade_id).await {
                tracing::error!(
                    upgrade_id,
                    "postgres major upgrade orchestrator failed: {}",
                    e
                );
                let reason = e.to_string();
                let _ = log_service
                    .log_error(&log_id_for_err, format!("orchestrator failed: {}", reason))
                    .await;
                // Don't stamp failed when the error is a cancel request —
                // the cancel handler already wrote `status=cancelled`.
                if !matches!(e, PostgresUpgradeError::CancelRequested { .. }) {
                    Self::mark_failed(&db_for_err, &log_service, upgrade_id, &reason).await;
                }
            }
        });

        Ok(inserted)
    }

    /// Read the accumulated log content for an upgrade.
    ///
    /// The orchestrator writes via `LogService::log_info/warning/error` which
    /// route to the structured JSONL backend at `{base}/{log_id}.jsonl`.
    /// `get_log_content` instead targets the legacy plain-text path
    /// `{base}/{log_id}.log`, so calling it here would always return empty.
    /// Render each JSONL entry as a timestamped text line for display.
    /// Missing files surface as an empty string so the UI doesn't have to
    /// special-case a 404.
    pub async fn read_log(&self, log_id: &str) -> Result<String, std::io::Error> {
        let entries = self.log_service.get_structured_logs(log_id).await?;
        let mut out = String::new();
        for entry in entries {
            let level = match entry.level {
                temps_logs::LogLevel::Info => "INFO",
                temps_logs::LogLevel::Success => "OK",
                temps_logs::LogLevel::Warning => "WARN",
                temps_logs::LogLevel::Error => "ERROR",
            };
            out.push_str(&format!(
                "{} [{}] {}\n",
                entry.timestamp.format("%Y-%m-%d %H:%M:%S%.3fZ"),
                level,
                entry.message
            ));
        }
        Ok(out)
    }

    /// Retry a failed or cancelled upgrade by resetting status to `pending`
    /// and re-invoking the orchestrator. The `phase` is preserved so the
    /// state machine resumes from where it stopped.
    pub async fn retry_major_upgrade(
        &self,
        upgrade_id: i32,
    ) -> Result<postgres_major_upgrades::Model, PostgresUpgradeError> {
        let row = postgres_major_upgrades::Entity::find_by_id(upgrade_id)
            .one(self.db.as_ref())
            .await?
            .ok_or(PostgresUpgradeError::NotFound { upgrade_id })?;

        if row.status != status::FAILED && row.status != status::CANCELLED {
            return Err(PostgresUpgradeError::InvalidVersionTransition {
                service_id: row.service_id,
                from_version: row.from_version.clone(),
                to_version: row.to_version.clone(),
                reason: format!(
                    "cannot retry an upgrade with status '{}'; only 'failed' or 'cancelled' are retriable",
                    row.status
                ),
            });
        }

        let attempt = row.attempt + 1;
        let row_id = row.id;
        let log_id = row.log_id.clone();
        let mut active: postgres_major_upgrades::ActiveModel = row.into();
        active.status = Set(status::PENDING.to_string());
        active.attempt = Set(attempt);
        active.error_message = Set(None);
        active.finished_at = Set(None);
        let updated = active.update(self.db.as_ref()).await?;

        let orchestrator = PostgresUpgradeOrchestrator::new(
            self.db.clone(),
            self.docker.clone(),
            self.backup_provider.clone(),
            self.lifecycle.clone(),
            self.log_service.clone(),
        );
        orchestrator.set_telemetry(self.telemetry());
        let log_service = self.log_service.clone();
        let db_for_err = self.db.clone();
        tokio::spawn(async move {
            if let Err(e) = orchestrator.run(row_id).await {
                tracing::error!(upgrade_id = row_id, "retry orchestrator failed: {}", e);
                let reason = e.to_string();
                let _ = log_service
                    .log_error(&log_id, format!("retry orchestrator failed: {}", reason))
                    .await;
                if !matches!(e, PostgresUpgradeError::CancelRequested { .. }) {
                    Self::mark_failed(&db_for_err, &log_service, row_id, &reason).await;
                }
            }
        });

        Ok(updated)
    }

    /// Cancel an in-flight upgrade. Flips the row to `cancelled`; the running
    /// orchestrator notices at the next phase boundary and exits cleanly.
    /// Already-terminal rows (completed/failed/cancelled) return
    /// `NotCancellable` so the caller gets a 409 instead of a silent no-op.
    pub async fn cancel_major_upgrade(
        &self,
        upgrade_id: i32,
    ) -> Result<postgres_major_upgrades::Model, PostgresUpgradeError> {
        let row = postgres_major_upgrades::Entity::find_by_id(upgrade_id)
            .one(self.db.as_ref())
            .await?
            .ok_or(PostgresUpgradeError::NotFound { upgrade_id })?;

        if row.status != status::PENDING && row.status != status::RUNNING {
            return Err(PostgresUpgradeError::NotCancellable {
                upgrade_id,
                status: row.status.clone(),
            });
        }

        let log_id = row.log_id.clone();
        let mut active: postgres_major_upgrades::ActiveModel = row.into();
        active.status = Set(status::CANCELLED.to_string());
        active.finished_at = Set(Some(chrono::Utc::now()));
        active.error_message = Set(Some("Cancelled by user".to_string()));
        let updated = active.update(self.db.as_ref()).await?;

        // Best-effort log line so the detail page shows the cancel marker
        // even if the orchestrator has already exited.
        let _ = self
            .log_service
            .log_warning(&log_id, "cancel requested by user")
            .await;

        Ok(updated)
    }

    /// Stamp `status=failed` + `error_message` + `finished_at` when the
    /// orchestrator returns an error. The orchestrator itself only writes
    /// phase/status on happy-path transitions, so a phase that bubbles an
    /// Err (dumper timeout, restore failure, etc.) would otherwise leave
    /// the row stuck at `status=running` forever. Idempotent: skips if the
    /// row is already terminal.
    async fn mark_failed(
        db: &DatabaseConnection,
        log_service: &LogService,
        upgrade_id: i32,
        reason: &str,
    ) {
        let row = match postgres_major_upgrades::Entity::find_by_id(upgrade_id)
            .one(db)
            .await
        {
            Ok(Some(r)) => r,
            Ok(None) => {
                tracing::warn!(upgrade_id, "mark_failed: row not found");
                return;
            }
            Err(e) => {
                tracing::error!(upgrade_id, "mark_failed: load failed: {}", e);
                return;
            }
        };
        if matches!(
            row.status.as_str(),
            status::COMPLETED | status::FAILED | status::CANCELLED | status::ROLLED_BACK
        ) {
            return;
        }
        let log_id = row.log_id.clone();
        let mut active: postgres_major_upgrades::ActiveModel = row.into();
        active.status = Set(status::FAILED.to_string());
        active.finished_at = Set(Some(chrono::Utc::now()));
        active.error_message = Set(Some(reason.to_string()));
        if let Err(e) = active.update(db).await {
            tracing::error!(upgrade_id, "mark_failed: update failed: {}", e);
        }
        let _ = log_service
            .log_error(&log_id, format!("upgrade marked failed: {}", reason))
            .await;
    }

    /// Roll a completed upgrade back to its pre-upgrade PGDATA and old
    /// image. Runs synchronously (vs. the spawn-and-return pattern used by
    /// `start_major_upgrade`) because rollbacks are rare, relatively quick
    /// (a single volume copy + container restart), and the UI benefits from
    /// a blocking 2xx/5xx response.
    pub async fn rollback_major_upgrade(
        &self,
        upgrade_id: i32,
    ) -> Result<postgres_major_upgrades::Model, PostgresUpgradeError> {
        let orchestrator = PostgresUpgradeOrchestrator::new(
            self.db.clone(),
            self.docker.clone(),
            self.backup_provider.clone(),
            self.lifecycle.clone(),
            self.log_service.clone(),
        );
        orchestrator.set_telemetry(self.telemetry());
        orchestrator.rollback(upgrade_id).await
    }

    /// Scheduled-job entry point that expires rollback volumes past the
    /// 7-day retention window. Delegates to the orchestrator so the logic
    /// lives next to the snapshot/restore phases it mirrors.
    pub async fn sweep_expired_rollback_volumes(&self) -> Result<u64, PostgresUpgradeError> {
        let orchestrator = PostgresUpgradeOrchestrator::new(
            self.db.clone(),
            self.docker.clone(),
            self.backup_provider.clone(),
            self.lifecycle.clone(),
            self.log_service.clone(),
        );
        orchestrator.set_telemetry(self.telemetry());
        orchestrator.sweep_expired_rollback_volumes().await
    }

    /// Boot-time recovery: find every upgrade in a non-terminal state and
    /// re-spawn the orchestrator for it. Called from
    /// `BackupPlugin::initialize_plugin_services` so a `temps serve` restart
    /// does not leave upgrades stuck in `running`.
    ///
    /// Each phase is idempotent, so re-running the orchestrator from the
    /// current phase is safe. Returns the number of upgrades resumed.
    pub async fn resume_active_upgrades(&self) -> Result<u64, PostgresUpgradeError> {
        let rows = postgres_major_upgrades::Entity::find()
            .filter(
                postgres_major_upgrades::Column::Status
                    .eq(status::PENDING)
                    .or(postgres_major_upgrades::Column::Status.eq(status::RUNNING)),
            )
            .all(self.db.as_ref())
            .await?;

        let mut resumed: u64 = 0;
        for row in rows {
            let upgrade_id = row.id;
            let log_id = row.log_id.clone();

            // Record the resumption in the upgrade's own log stream so the
            // user sees a clear marker on the detail page.
            let _ = self
                .log_service
                .log_warning(
                    &log_id,
                    format!(
                        "resuming upgrade after server restart (phase='{}', attempt={})",
                        row.phase, row.attempt
                    ),
                )
                .await;

            let orchestrator = PostgresUpgradeOrchestrator::new(
                self.db.clone(),
                self.docker.clone(),
                self.backup_provider.clone(),
                self.lifecycle.clone(),
                self.log_service.clone(),
            );
            orchestrator.set_telemetry(self.telemetry());
            let log_service = self.log_service.clone();
            let log_id_for_err = log_id.clone();
            let db_for_err = self.db.clone();
            tokio::spawn(async move {
                if let Err(e) = orchestrator.run(upgrade_id).await {
                    tracing::error!(upgrade_id, "resumed orchestrator failed: {}", e);
                    let reason = e.to_string();
                    let _ = log_service
                        .log_error(
                            &log_id_for_err,
                            format!("resumed orchestrator failed: {}", reason),
                        )
                        .await;
                    if !matches!(e, PostgresUpgradeError::CancelRequested { .. }) {
                        Self::mark_failed(&db_for_err, &log_service, upgrade_id, &reason).await;
                    }
                }
            });
            resumed += 1;
        }

        tracing::info!(resumed, "resumed in-flight Postgres major upgrades");
        Ok(resumed)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::externalsvc::postgres_upgrade::PreUpgradeBackupProvider;
    use async_trait::async_trait;

    struct StubProvider;

    #[async_trait]
    impl PreUpgradeBackupProvider for StubProvider {
        async fn default_s3_source_id(&self, _service_id: i32) -> Result<Option<i32>, String> {
            Ok(Some(1))
        }

        async fn create_pre_upgrade_backup(
            &self,
            _service_id: i32,
            _s3_source_id: i32,
            _created_by: i32,
        ) -> Result<i32, String> {
            Ok(42)
        }
    }

    #[test]
    fn request_struct_basic_fields() {
        // Smoke: ensure the request struct compiles and fields are settable.
        let req = StartMajorUpgradeRequest {
            service_id: 1,
            from_version: "16".into(),
            to_version: "17".into(),
            from_image: "postgres:16-bookworm".into(),
            to_image: "postgres:17-bookworm".into(),
            created_by: 1,
        };
        assert_eq!(req.from_version, "16");
        assert_eq!(req.to_version, "17");
    }

    #[test]
    fn stub_provider_is_object_safe() {
        let _: Arc<dyn PreUpgradeBackupProvider> = Arc::new(StubProvider);
    }

    fn sample_request() -> StartMajorUpgradeRequest {
        StartMajorUpgradeRequest {
            service_id: 7,
            from_version: "16".into(),
            to_version: "17".into(),
            from_image: "postgres:16-bookworm".into(),
            to_image: "postgres:17-bookworm".into(),
            created_by: 1,
        }
    }

    #[test]
    fn validate_rejects_non_postgres_service() {
        let req = sample_request();
        let err = validate_start_request(&req, "redis")
            .expect_err("redis service type should be rejected");
        assert!(matches!(
            err,
            PostgresUpgradeError::WrongServiceType { service_id: 7, .. }
        ));
    }

    #[test]
    fn validate_rejects_cross_os_upgrade() {
        let mut req = sample_request();
        req.from_image = "postgres:16-alpine".into();
        // to_image stays bookworm
        let err = validate_start_request(&req, "postgres")
            .expect_err("alpine -> bookworm must be rejected");
        assert!(matches!(
            err,
            PostgresUpgradeError::OsFamilyMismatch { service_id: 7, .. }
        ));
    }

    #[test]
    fn validate_rejects_downgrade() {
        let mut req = sample_request();
        req.from_version = "17".into();
        req.to_version = "16".into();
        req.from_image = "postgres:17-bookworm".into();
        req.to_image = "postgres:16-bookworm".into();
        let err = validate_start_request(&req, "postgres").expect_err("downgrade must be rejected");
        assert!(matches!(
            err,
            PostgresUpgradeError::InvalidVersionTransition { service_id: 7, .. }
        ));
    }

    #[test]
    fn validate_rejects_same_version() {
        let mut req = sample_request();
        req.to_version = "16".into();
        req.to_image = "postgres:16-bookworm".into();
        let err =
            validate_start_request(&req, "postgres").expect_err("same-version must be rejected");
        assert!(matches!(
            err,
            PostgresUpgradeError::InvalidVersionTransition { .. }
        ));
    }

    #[test]
    fn validate_accepts_normal_upgrade() {
        let req = sample_request();
        validate_start_request(&req, "postgres").expect("16->17 bookworm should pass");
        validate_start_request(&req, "postgresql")
            .expect("postgresql service_type should also pass");
    }

    #[test]
    fn validate_accepts_custom_repo_upgrade() {
        let mut req = sample_request();
        req.from_image = "gotempsh/postgres-ha:16-bookworm".into();
        req.to_image = "gotempsh/postgres-ha:17-bookworm".into();
        validate_start_request(&req, "postgres").expect("custom repo 16->17 should pass");
    }
}
