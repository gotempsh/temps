//! External service health monitor.
//!
//! Periodically probes every `external_services` row where `status = 'running'`
//! via a TCP connect to the service's effective address. Records each probe
//! in `external_service_health_checks` and updates the denormalized
//! `health_status` / `last_health_check_at` / `last_health_error` columns
//! on `external_services` so the UI can render a status badge in one query.
//!
//! When a service fails `CONSECUTIVE_FAILURES_BEFORE_ALERT` probes in a row,
//! the monitor sends a notification via the shared `NotificationService`.
//! A recovery notification is sent when the service returns to `operational`.

use crate::externalsvc::mariadb::{BinlogArchiveInterval, MariaDbConfig, MariaDbService};
use crate::externalsvc::postgres_wal_health::{self, PostgresWalHealth};
use crate::externalsvc::{HealthProbeStatus, S3Credentials, ServiceType};
use crate::services::ExternalServiceManager;
use bollard::Docker;
use chrono::Utc;
use sea_orm::{
    ActiveModelTrait, ActiveValue::Set, ColumnTrait, DatabaseConnection, EntityTrait, QueryFilter,
};
use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};
use temps_core::notifications::{
    NotificationData, NotificationPriority, NotificationService, NotificationType,
};
use temps_core::EncryptionService;
use temps_entities::{
    backup_schedule_services, backup_schedules, external_service_health_checks, external_services,
    s3_sources,
};
use thiserror::Error;
use tokio::sync::Mutex;
use tracing::{debug, error, info, warn};

/// Key under `external_services.health_metadata` for Postgres WAL probe output.
/// Future engines add sibling keys (e.g., `redis_memory`, `mongo_oplog`) so
/// the column stays generic.
const POSTGRES_WAL_KEY: &str = "postgres_wal";

/// How many failed probes in a row before we raise an alert.
const CONSECUTIVE_FAILURES_BEFORE_ALERT: i32 = 3;

/// Configuration for `ExternalServiceHealthMonitor`.
#[derive(Debug, Clone)]
pub struct ExternalServiceHealthConfig {
    /// How often to run a full check cycle (seconds).
    pub poll_interval_secs: u64,
    /// How many days of check history to keep before pruning. 0 disables pruning.
    pub retention_days: i64,
}

impl Default for ExternalServiceHealthConfig {
    fn default() -> Self {
        Self {
            poll_interval_secs: 30,
            retention_days: 30,
        }
    }
}

// Status strings come from `HealthProbeStatus::as_str` (operational|degraded|down)
// so the `external_service_health_checks.status` column stays in sync with
// the trait-level result type.

#[derive(Debug, Error)]
pub enum HealthMonitorError {
    #[error("Database error: {0}")]
    Database(#[from] sea_orm::DbErr),

    #[error("External service {id} not found")]
    ServiceNotFound { id: i32 },
}

/// Background loop that keeps `external_services.health_status` in sync with
/// reality and sends alerts when a service stays down for 3+ consecutive checks.
pub struct ExternalServiceHealthMonitor {
    db: Arc<DatabaseConnection>,
    manager: Arc<ExternalServiceManager>,
    notification_service: Arc<dyn NotificationService>,
    config: ExternalServiceHealthConfig,
    /// Docker handle used by the per-service MariaDB binlog archiver to read
    /// closed binlog segments out of the container.
    docker: Arc<Docker>,
    /// Decrypts `s3_sources` credentials so the archiver can build an S3 client.
    encryption_service: Arc<EncryptionService>,
    /// Last time we ran the binlog archiver for each MariaDB service, keyed by
    /// service id. The health loop ticks every `poll_interval_secs`; we gate
    /// archiving so it only fires once per service's `binlog_archive_interval`.
    last_binlog_archive: Arc<Mutex<HashMap<i32, Instant>>>,
}

impl ExternalServiceHealthMonitor {
    pub fn new(
        db: Arc<DatabaseConnection>,
        manager: Arc<ExternalServiceManager>,
        notification_service: Arc<dyn NotificationService>,
        config: ExternalServiceHealthConfig,
        docker: Arc<Docker>,
        encryption_service: Arc<EncryptionService>,
    ) -> Self {
        Self {
            db,
            manager,
            notification_service,
            config,
            docker,
            encryption_service,
            last_binlog_archive: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    /// Run forever. Spawn this onto a background task.
    pub async fn start(self: Arc<Self>) {
        info!(
            "Starting external service health monitor (poll interval: {}s)",
            self.config.poll_interval_secs
        );

        let mut prune_counter: u32 = 0;

        loop {
            if let Err(e) = self.run_cycle().await {
                error!("External service health check cycle failed: {}", e);
            }

            // Once an hour, prune old check rows.
            prune_counter = prune_counter.wrapping_add(1);
            if self.config.retention_days > 0
                && prune_counter
                    .is_multiple_of((3600 / self.config.poll_interval_secs.max(1)).max(1) as u32)
            {
                if let Err(e) = self.prune_old_checks().await {
                    warn!("Health check pruning failed: {}", e);
                }
            }

            tokio::time::sleep(Duration::from_secs(self.config.poll_interval_secs)).await;
        }
    }

    async fn run_cycle(&self) -> Result<(), HealthMonitorError> {
        let services = external_services::Entity::find()
            .all(self.db.as_ref())
            .await?;

        if services.is_empty() {
            debug!("No external services to health-check");
            return Ok(());
        }

        debug!("Health-checking {} external service(s)", services.len());

        for service in services {
            if let Err(e) = self.check_service(&service).await {
                warn!(
                    "Health check error for service {} ({}): {}",
                    service.id, service.name, e
                );
            }
        }

        Ok(())
    }

    /// Run a single health check for one service on demand (e.g. triggered by
    /// a user via the REST API). Writes the same history row + denormalized
    /// fields as the background loop and fires alerts on the Nth consecutive
    /// failure / recovery, so the consecutive-failure counter stays honest.
    pub async fn run_check_for(&self, service_id: i32) -> Result<(), HealthMonitorError> {
        let service = external_services::Entity::find_by_id(service_id)
            .one(self.db.as_ref())
            .await?
            .ok_or(HealthMonitorError::ServiceNotFound { id: service_id })?;

        self.check_service(&service).await
    }

    /// Check one service and record the result.
    async fn check_service(
        &self,
        service: &external_services::Model,
    ) -> Result<(), HealthMonitorError> {
        // Services that aren't supposed to be running should not be probed —
        // we just record them as down without false alerting (alert is gated
        // on consecutive failures and a stopped service starts at 0).
        let (mut status, response_time_ms, mut error_message) = if service.status != "running" {
            (
                HealthProbeStatus::Down,
                None,
                Some(format!(
                    "Service status is '{}', not running",
                    service.status
                )),
            )
        } else {
            self.probe_service(service).await
        };

        // Postgres standalone services get an additional WAL/archive probe.
        // The result is persisted under `health_metadata.postgres_wal` so the
        // UI can render warnings. WAL warnings downgrade Operational to
        // Degraded but never escalate Down upward — liveness wins.
        let wal_snapshot = if service.service_type == "postgres"
            && service.topology == "standalone"
            && !matches!(status, HealthProbeStatus::Down)
        {
            self.run_postgres_wal_probe(service).await
        } else {
            None
        };

        if let Some(snapshot) = &wal_snapshot {
            if snapshot.has_warnings() && matches!(status, HealthProbeStatus::Operational) {
                status = HealthProbeStatus::Degraded;
                if error_message.is_none() {
                    error_message = Some(format!(
                        "Postgres WAL health: {} warning(s) — see health_metadata for details",
                        snapshot.warnings.len()
                    ));
                }
            }
        }

        let now = Utc::now();

        // 1. Append history row
        let history = external_service_health_checks::ActiveModel {
            service_id: Set(service.id),
            checked_at: Set(now),
            status: Set(status.as_str().to_string()),
            response_time_ms: Set(response_time_ms),
            error_message: Set(error_message.clone()),
            ..Default::default()
        };
        if let Err(e) = history.insert(self.db.as_ref()).await {
            warn!(
                "Failed to record health check for service {}: {}",
                service.id, e
            );
        }

        // 2. Update denormalized fields on external_services
        let was_failing = service.consecutive_health_failures;
        let now_failing = if matches!(status, HealthProbeStatus::Down) {
            was_failing + 1
        } else {
            0
        };

        let merged_metadata = merge_health_metadata(
            service.health_metadata.as_ref(),
            POSTGRES_WAL_KEY,
            wal_snapshot.as_ref(),
        );

        let mut active: external_services::ActiveModel = service.clone().into();
        active.health_status = Set(Some(status.as_str().to_string()));
        active.last_health_check_at = Set(Some(now));
        active.last_health_error = Set(error_message.clone());
        active.consecutive_health_failures = Set(now_failing);
        if let Some(metadata) = merged_metadata {
            active.health_metadata = Set(Some(metadata));
        }
        if let Err(e) = active.update(self.db.as_ref()).await {
            warn!(
                "Failed to update health_status on service {}: {}",
                service.id, e
            );
        }

        // 3. Fire alerts on state transitions
        //    - Down for the Nth consecutive time → alert
        //    - Just recovered from N+ failures → recovery notice
        if matches!(status, HealthProbeStatus::Down)
            && now_failing == CONSECUTIVE_FAILURES_BEFORE_ALERT
        {
            self.send_down_alert(service, error_message.as_deref())
                .await;
        } else if !matches!(status, HealthProbeStatus::Down)
            && was_failing >= CONSECUTIVE_FAILURES_BEFORE_ALERT
        {
            self.send_recovered_alert(service).await;
        }

        // 4. MariaDB PITR: ship closed binary-log segments to S3 on the
        //    service's configured cadence. Only for running standalone
        //    MariaDB services that have a backup schedule (→ S3 destination).
        //    Failures here never affect health monitoring of other services.
        if service.service_type == "mariadb"
            && service.topology == "standalone"
            && service.status == "running"
            && !matches!(status, HealthProbeStatus::Down)
        {
            self.maybe_archive_mariadb_binlogs(service).await;
        }

        Ok(())
    }

    /// Per-service MariaDB binlog archiver tick. Gated so the actual ship only
    /// happens once per the service's `binlog_archive_interval`, even though
    /// the health loop calls this every `poll_interval_secs`.
    ///
    /// All failures are logged and swallowed — binlog archiving must never
    /// disrupt health monitoring.
    async fn maybe_archive_mariadb_binlogs(&self, service: &external_services::Model) {
        // Load config to read the configured ship cadence. Cheap relative to
        // the schedule scan below, and needed for the interval gate.
        let service_config = match self.manager.get_service_config(service.id).await {
            Ok(cfg) => cfg,
            Err(e) => {
                debug!(
                    service_id = service.id,
                    "Failed to load MariaDB config for binlog archive: {}", e
                );
                return;
            }
        };
        let mariadb_config: MariaDbConfig =
            match serde_json::from_value(service_config.parameters.clone()) {
                Ok(c) => c,
                Err(e) => {
                    debug!(
                        service_id = service.id,
                        "Failed to parse MariaDB config for binlog archive: {}", e
                    );
                    return;
                }
            };
        let interval = mariadb_config.binlog_archive_interval;

        // Interval gate (cheap, in-memory) FIRST: only proceed if enough
        // wall-clock time has elapsed since the last archive run. Checked
        // before the backup-schedule DB scan so we don't query every poll tick.
        if !self.binlog_interval_elapsed(service.id, interval).await {
            return;
        }

        // Discover the S3 destination from a backup schedule covering this
        // service. No schedule = no PITR destination configured = skip.
        let s3_source = match self.find_s3_source_for_service(service.id).await {
            Ok(Some(src)) => src,
            Ok(None) => {
                debug!(
                    service_id = service.id,
                    "MariaDB service has no backup schedule; skipping binlog archive"
                );
                return;
            }
            Err(e) => {
                debug!(
                    service_id = service.id,
                    "Failed to resolve S3 source for MariaDB binlog archive: {}", e
                );
                return;
            }
        };

        // Build a decrypted S3 client from the source row.
        let creds = match self.build_s3_credentials(&s3_source) {
            Ok(c) => c,
            Err(e) => {
                warn!(
                    service_id = service.id,
                    "Failed to build S3 credentials for MariaDB binlog archive: {}", e
                );
                return;
            }
        };
        let s3_client = creds.build_s3_client().await;

        let mariadb = MariaDbService::new(service.name.clone(), self.docker.clone());
        match mariadb
            .archive_binlogs(&s3_client, &s3_source, &mariadb_config)
            .await
        {
            Ok(shipped) => {
                if shipped > 0 {
                    info!(
                        service_id = service.id,
                        service = %service.name,
                        shipped,
                        "Archived MariaDB binlog segment(s) to S3"
                    );
                }
            }
            Err(e) => {
                warn!(
                    service_id = service.id,
                    service = %service.name,
                    "MariaDB binlog archive run failed: {}", e
                );
            }
        }
    }

    /// Check the per-service interval gate and, if elapsed, record `now` as the
    /// new last-archived time. Returns true when the caller should proceed.
    async fn binlog_interval_elapsed(
        &self,
        service_id: i32,
        interval: BinlogArchiveInterval,
    ) -> bool {
        let mut map = self.last_binlog_archive.lock().await;
        let now = Instant::now();
        match map.get(&service_id) {
            Some(last) if now.duration_since(*last) < Duration::from_secs(interval.seconds()) => {
                false
            }
            _ => {
                map.insert(service_id, now);
                true
            }
        }
    }

    /// Find the S3 source for a service via an enabled backup schedule that
    /// covers it. A schedule covers the service when `target_all_services` is
    /// true, or when the `backup_schedule_services` join links them. Prefers
    /// the most recently updated schedule when several apply.
    async fn find_s3_source_for_service(
        &self,
        service_id: i32,
    ) -> Result<Option<s3_sources::Model>, HealthMonitorError> {
        use sea_orm::QueryOrder;

        let schedules = backup_schedules::Entity::find()
            .filter(backup_schedules::Column::Enabled.eq(true))
            .order_by_desc(backup_schedules::Column::UpdatedAt)
            .all(self.db.as_ref())
            .await?;

        for schedule in schedules {
            let covers = if schedule.target_all_services {
                true
            } else {
                backup_schedule_services::Entity::find()
                    .filter(backup_schedule_services::Column::ScheduleId.eq(schedule.id))
                    .filter(backup_schedule_services::Column::ServiceId.eq(service_id))
                    .one(self.db.as_ref())
                    .await?
                    .is_some()
            };
            if !covers {
                continue;
            }
            if let Some(source) = s3_sources::Entity::find_by_id(schedule.s3_source_id)
                .one(self.db.as_ref())
                .await?
            {
                return Ok(Some(source));
            }
        }

        Ok(None)
    }

    /// Decrypt an `s3_sources` row into usable `S3Credentials`.
    fn build_s3_credentials(
        &self,
        s3_source: &s3_sources::Model,
    ) -> Result<S3Credentials, anyhow::Error> {
        let access_key_id = self
            .encryption_service
            .decrypt_string(&s3_source.access_key_id)
            .map_err(|e| anyhow::anyhow!("Failed to decrypt S3 access key: {}", e))?;
        let secret_key = self
            .encryption_service
            .decrypt_string(&s3_source.secret_key)
            .map_err(|e| anyhow::anyhow!("Failed to decrypt S3 secret key: {}", e))?;

        Ok(S3Credentials {
            access_key_id,
            secret_key,
            region: s3_source.region.clone(),
            endpoint: s3_source.endpoint.clone(),
            bucket_name: s3_source.bucket_name.clone(),
            bucket_path: s3_source.bucket_path.clone(),
            force_path_style: s3_source.force_path_style.unwrap_or(false),
        })
    }

    /// Run the WAL/archive probe for a standalone Postgres service.
    ///
    /// Best-effort: any failure returns `None` and is logged at debug level
    /// so a stricter Postgres connection (e.g., scram-sha-256 with a probe
    /// that uses the wrong auth flow) doesn't spam warnings on every cycle.
    async fn run_postgres_wal_probe(
        &self,
        service: &external_services::Model,
    ) -> Option<PostgresWalHealth> {
        let service_config = match self.manager.get_service_config(service.id).await {
            Ok(cfg) => cfg,
            Err(e) => {
                debug!(
                    "WAL probe skipped for service {} ({}): failed to load config: {}",
                    service.id, service.name, e
                );
                return None;
            }
        };

        let conn_str = postgres_wal_health::build_conn_str(&service_config.parameters)?;
        postgres_wal_health::probe_wal_health(&conn_str).await
    }

    /// Probe the service using its engine-specific health_probe implementation
    /// (Postgres `SELECT 1`, Redis `PING`, MongoDB `ping`, S3/RustFS `ListBuckets`).
    /// Returns (status, response_time_ms, error_message).
    async fn probe_service(
        &self,
        service: &external_services::Model,
    ) -> (HealthProbeStatus, Option<i32>, Option<String>) {
        let service_type = match ServiceType::from_str(&service.service_type) {
            Ok(t) => t,
            Err(_) => {
                return (
                    HealthProbeStatus::Down,
                    None,
                    Some(format!("Unknown service type: {}", service.service_type)),
                );
            }
        };

        // Cluster services need a fan-out probe — the standalone
        // ExternalService::health_probe path can't reach a multi-host
        // cluster (it falls through to localhost:5432). Route through the
        // manager's cluster-aware probe instead.
        if service.topology == "cluster" {
            let result = self.manager.probe_cluster(service).await;
            return (result.status, result.response_time_ms, result.error_message);
        }

        let service_config = match self.manager.get_service_config(service.id).await {
            Ok(cfg) => cfg,
            Err(e) => {
                return (
                    HealthProbeStatus::Down,
                    None,
                    Some(format!("Failed to load service config: {}", e)),
                );
            }
        };

        let instance = self
            .manager
            .get_service_instance(service.name.clone(), service_type);

        match instance.health_probe(service_config).await {
            Ok(result) => (result.status, result.response_time_ms, result.error_message),
            Err(e) => (
                HealthProbeStatus::Down,
                None,
                Some(format!("health_probe raised an error: {}", e)),
            ),
        }
    }

    async fn send_down_alert(
        &self,
        service: &external_services::Model,
        error_message: Option<&str>,
    ) {
        let title = format!("Service down: {}", service.name);
        let message = format!(
            "External service '{}' ({}) has failed {} consecutive health checks.\n\n\
             Last error: {}",
            service.name,
            service.service_type,
            CONSECUTIVE_FAILURES_BEFORE_ALERT,
            error_message.unwrap_or("(no details)")
        );

        let notification = NotificationData {
            id: uuid::Uuid::new_v4().to_string(),
            title,
            message,
            notification_type: NotificationType::Error,
            priority: NotificationPriority::Critical,
            severity: Some("critical".to_string()),
            timestamp: Utc::now(),
            metadata: [
                ("source".to_string(), "external_service_health".to_string()),
                ("service_id".to_string(), service.id.to_string()),
                ("service_name".to_string(), service.name.clone()),
                ("service_type".to_string(), service.service_type.clone()),
            ]
            .into_iter()
            .collect(),
            bypass_throttling: true,
        };

        if let Err(e) = self
            .notification_service
            .send_notification(notification)
            .await
        {
            error!(
                "Failed to send down-alert notification for service {}: {}",
                service.id, e
            );
        } else {
            info!(
                "Sent health-check down alert for service {} ({})",
                service.id, service.name
            );
        }
    }

    async fn send_recovered_alert(&self, service: &external_services::Model) {
        let title = format!("Service recovered: {}", service.name);
        let message = format!(
            "External service '{}' ({}) is responding to health checks again.",
            service.name, service.service_type,
        );

        let notification = NotificationData {
            id: uuid::Uuid::new_v4().to_string(),
            title,
            message,
            notification_type: NotificationType::Info,
            priority: NotificationPriority::Normal,
            severity: None,
            timestamp: Utc::now(),
            metadata: [
                ("source".to_string(), "external_service_health".to_string()),
                ("service_id".to_string(), service.id.to_string()),
                ("service_name".to_string(), service.name.clone()),
                ("status".to_string(), "recovered".to_string()),
            ]
            .into_iter()
            .collect(),
            bypass_throttling: false,
        };

        if let Err(e) = self
            .notification_service
            .send_notification(notification)
            .await
        {
            error!(
                "Failed to send recovery notification for service {}: {}",
                service.id, e
            );
        }
    }

    async fn prune_old_checks(&self) -> Result<(), HealthMonitorError> {
        let cutoff = Utc::now() - chrono::Duration::days(self.config.retention_days);
        let deleted = external_service_health_checks::Entity::delete_many()
            .filter(external_service_health_checks::Column::CheckedAt.lt(cutoff))
            .exec(self.db.as_ref())
            .await?;
        if deleted.rows_affected > 0 {
            info!(
                "Pruned {} external_service_health_checks rows older than {} days",
                deleted.rows_affected, self.config.retention_days
            );
        }
        Ok(())
    }
}

/// Merge a single engine snapshot into the existing `health_metadata` JSON
/// object under `key`. Preserves sibling keys that other engines may have
/// written, so future engines can plug in without coordinating writes.
///
/// Returns:
/// - `Some(updated)` when the merged object differs from the input or when a
///   new snapshot is being recorded.
/// - `None` when `snapshot` is `None` AND nothing in the input needs touching
///   (avoids gratuitous UPDATEEs on services with no metadata).
fn merge_health_metadata<T: serde::Serialize>(
    existing: Option<&sea_orm::JsonValue>,
    key: &str,
    snapshot: Option<&T>,
) -> Option<sea_orm::JsonValue> {
    let snapshot = snapshot?;
    let snapshot_value = match serde_json::to_value(snapshot) {
        Ok(v) => v,
        Err(e) => {
            warn!(
                "Failed to serialize health metadata snapshot for key '{}': {}",
                key, e
            );
            return None;
        }
    };

    let mut map = match existing {
        Some(serde_json::Value::Object(m)) => m.clone(),
        _ => serde_json::Map::new(),
    };
    map.insert(key.to_string(), snapshot_value);
    Some(serde_json::Value::Object(map))
}

// ── Tests ────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn health_status_strings() {
        assert_eq!(HealthProbeStatus::Operational.as_str(), "operational");
        assert_eq!(HealthProbeStatus::Degraded.as_str(), "degraded");
        assert_eq!(HealthProbeStatus::Down.as_str(), "down");
    }

    #[test]
    fn merge_writes_new_key_into_empty_metadata() {
        let merged =
            merge_health_metadata(None, "postgres_wal", Some(&serde_json::json!({"a": 1})));
        let merged = merged.expect("expected merged value");
        assert_eq!(merged["postgres_wal"]["a"], 1);
    }

    #[test]
    fn merge_preserves_sibling_keys() {
        let existing = serde_json::json!({"redis_memory": {"used_bytes": 42}});
        let merged = merge_health_metadata(
            Some(&existing),
            "postgres_wal",
            Some(&serde_json::json!({"pg_wal_bytes": 100})),
        )
        .expect("expected merged value");
        assert_eq!(merged["redis_memory"]["used_bytes"], 42);
        assert_eq!(merged["postgres_wal"]["pg_wal_bytes"], 100);
    }

    #[test]
    fn merge_overwrites_same_key() {
        let existing = serde_json::json!({"postgres_wal": {"old": true}});
        let merged = merge_health_metadata(
            Some(&existing),
            "postgres_wal",
            Some(&serde_json::json!({"new": true})),
        )
        .expect("expected merged value");
        assert!(merged["postgres_wal"].get("old").is_none());
        assert_eq!(merged["postgres_wal"]["new"], true);
    }

    #[test]
    fn merge_returns_none_when_snapshot_missing() {
        let existing = serde_json::json!({"postgres_wal": {"old": true}});
        let merged =
            merge_health_metadata::<serde_json::Value>(Some(&existing), "postgres_wal", None);
        assert!(merged.is_none());
    }
}
