// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

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

use crate::continuous_archive;
use crate::externalsvc::mariadb::{BinlogArchiveInterval, MariaDbConfig, MariaDbService};
use crate::externalsvc::postgres_wal_health::{self, PostgresWalHealth};
use crate::externalsvc::{HealthProbeStatus, S3Credentials};
use crate::services::{
    record_wal_outcome, ContainerStatsBaseline, ExternalServiceManager, PolledContainerStats,
    WalProbeOutcome,
};
use chrono::Utc;
use futures::{stream, StreamExt};
use sea_orm::{
    ActiveModelTrait, ActiveValue::Set, ColumnTrait, DatabaseConnection, EntityTrait, QueryFilter,
};
use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};
use temps_alarms::alarm_service::{AlarmService, AlarmSeverity, AlarmType, FireAlarmRequest};
use temps_core::DockerHandle;
use temps_core::EncryptionService;
use temps_entities::{
    backup_schedule_services, backup_schedules, external_service_backups,
    external_service_health_checks, external_services, project_services, s3_sources,
};
use temps_metrics::{MetricKind, MetricPoint, MetricsStore, SourceKind};
use thiserror::Error;
use tokio::sync::Mutex;
use tracing::{debug, error, info, warn};

/// How many failed probes in a row before we raise an alert.
pub(crate) const CONSECUTIVE_FAILURES_BEFORE_ALERT: i32 = 3;
/// Severity of the alarm raised when that streak is reached.
pub(crate) const DOWN_ALERT_SEVERITY: AlarmSeverity = AlarmSeverity::Critical;

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

    #[error("Failed to decrypt the {credential} of S3 source {s3_source_id}: {reason}")]
    S3CredentialDecryption {
        s3_source_id: i32,
        credential: &'static str,
        reason: String,
    },

    /// The MariaDB binlog-retention anchor could not be established, so no
    /// archived segment may be deleted this run. Carries the service and the
    /// concrete reason because the only signal an operator gets is the log
    /// line — an unexplained "not pruning" is indistinguishable from a bug.
    #[error(
        "MariaDB binlog retention anchor for service {service_id} ('{service_name}') is undetermined: {reason}"
    )]
    BinlogRetentionAnchor {
        service_id: i32,
        service_name: String,
        reason: String,
    },
}

/// How long before a scoped credential's own expiry the monitor stops
/// reusing it, so a run never starts with one about to lapse.
const BINLOG_CREDENTIAL_EXPIRY_MARGIN: Duration = Duration::from_secs(10 * 60);

/// What a worker MariaDB's binary-log credential was minted for: reused
/// only while every part still matches, so a change to the source (its
/// bucket, base path, keys or worker credentials setting all bump
/// `updated_at`), the service's name or its node mints a new one.
#[derive(Debug, Clone, PartialEq, Eq)]
struct BinlogCredentialGrant {
    s3_source_id: i32,
    s3_source_updated_at: chrono::DateTime<Utc>,
    node_id: i32,
    bucket: String,
    prefix: String,
}

impl BinlogCredentialGrant {
    fn of(service_name: &str, node_id: i32, s3_source: &s3_sources::Model) -> Self {
        Self {
            s3_source_id: s3_source.id,
            s3_source_updated_at: s3_source.updated_at,
            node_id,
            bucket: s3_source.bucket_name.clone(),
            prefix: crate::externalsvc::mariadb_node::binlog_key_prefix(
                &s3_source.bucket_path,
                service_name,
            ),
        }
    }
}

/// A worker MariaDB's binary-log archiving credential, what it was minted
/// for, and until when it may be reused.
struct BinlogArchiveCredential {
    grant: BinlogCredentialGrant,
    reuse_until: Instant,
    destination: crate::remote_service_client::WorkerS3Destination,
}

/// How long a freshly minted binary-log credential may be reused: the
/// refresh interval, cut short [`BINLOG_CREDENTIAL_EXPIRY_MARGIN`] before
/// its own expiry (a store may grant less than asked). `None` for one not
/// worth keeping: the source's full credential (never cached, so a source
/// switched to scoped stops receiving it on the next run) or one already
/// about to expire.
fn binlog_credential_reuse_window(
    scope: &crate::worker_s3_scope::WorkerCredentialScope,
    now: chrono::DateTime<Utc>,
) -> Option<Duration> {
    use crate::worker_s3_scope::{WorkerCredentialScope, ARCHIVE_CREDENTIAL_REFRESH_INTERVAL};
    let WorkerCredentialScope::Scoped { expires_at, .. } = scope else {
        return None;
    };
    let window = match expires_at {
        Some(expires_at) => (*expires_at - now)
            .to_std()
            .ok()?
            .checked_sub(BINLOG_CREDENTIAL_EXPIRY_MARGIN)?
            .min(ARCHIVE_CREDENTIAL_REFRESH_INTERVAL),
        None => ARCHIVE_CREDENTIAL_REFRESH_INTERVAL,
    };
    (!window.is_zero()).then_some(window)
}

/// Background loop that keeps `external_services.health_status` in sync with
/// reality and sends alerts when a service stays down for 3+ consecutive checks.
pub struct ExternalServiceHealthMonitor {
    db: Arc<DatabaseConnection>,
    manager: Arc<ExternalServiceManager>,
    alarm_service: Arc<AlarmService>,
    config: ExternalServiceHealthConfig,
    /// Docker handle used by the per-service MariaDB binlog archiver to read
    /// closed binlog segments out of the container. May be Disabled on
    /// control-plane profiles — binlog archiving is skipped gracefully.
    docker: Arc<DockerHandle>,
    /// Decrypts `s3_sources` credentials so the archiver can build an S3 client.
    encryption_service: Arc<EncryptionService>,
    /// Last time we ran the binlog archiver for each MariaDB service, keyed by
    /// service id. The health loop ticks every `poll_interval_secs`; we gate
    /// archiving so it only fires once per service's `binlog_archive_interval`.
    last_binlog_archive: Arc<Mutex<HashMap<i32, Instant>>>,
    /// The scoped credential each worker MariaDB ships its binary logs with
    /// (ADR-102), keyed by service id: reused across archive runs and minted
    /// again every [`ARCHIVE_CREDENTIAL_REFRESH_INTERVAL`], when the service
    /// moves to another node or source, or after a run that failed.
    ///
    /// [`ARCHIVE_CREDENTIAL_REFRESH_INTERVAL`]: crate::worker_s3_scope::ARCHIVE_CREDENTIAL_REFRESH_INTERVAL
    binlog_archive_credentials: Arc<Mutex<HashMap<i32, BinlogArchiveCredential>>>,
    /// Optional metrics store. When set, container CPU/memory samples for
    /// running services with `metrics_enabled` are written to it on every
    /// health tick (see `container_metric_points`, `source_kind = database`).
    metrics_store: Option<Arc<dyn MetricsStore>>,
    /// Previous docker-stats tick per service (`service_id` →
    /// `container_name` → CPU counters and cached CPU limit). The poll
    /// interval is the CPU rate window; the first tick per container seeds
    /// the baseline and emits memory only. Holds entries only for services
    /// that are sampled (`samples_container_metrics`), and per service only
    /// for its current containers.
    stats_baselines: Arc<StatsBaselines>,
}

impl ExternalServiceHealthMonitor {
    pub fn new(
        db: Arc<DatabaseConnection>,
        manager: Arc<ExternalServiceManager>,
        alarm_service: Arc<AlarmService>,
        config: ExternalServiceHealthConfig,
        docker: Arc<DockerHandle>,
        encryption_service: Arc<EncryptionService>,
    ) -> Self {
        Self {
            db,
            manager,
            alarm_service,
            config,
            docker,
            encryption_service,
            last_binlog_archive: Arc::new(Mutex::new(HashMap::new())),
            binlog_archive_credentials: Arc::new(Mutex::new(HashMap::new())),
            metrics_store: None,
            stats_baselines: Arc::new(StatsBaselines::default()),
        }
    }

    /// Attach a metrics store. When set, the monitor writes container
    /// CPU/memory samples for every running service with `metrics_enabled`
    /// alongside its health probes, giving external services the same
    /// resource-usage history deployment containers already have.
    pub fn with_metrics_store(mut self, store: Arc<dyn MetricsStore>) -> Self {
        self.metrics_store = Some(store);
        self
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

        // Services whose container stats are sampled this cycle. Baselines of
        // every other service (deleted, stopped, metrics off) are dropped
        // below, so the map stays bounded by the
        // sampled containers and a stopped service restarts from a fresh
        // baseline instead of one from before it stopped.
        let sampled_ids: std::collections::HashSet<i32> = services
            .iter()
            .filter(|s| samples_container_metrics(s))
            .map(|s| s.id)
            .collect();

        const MAX_CONCURRENT_HEALTH_CHECKS: usize = 8;
        stream::iter(services)
            .for_each_concurrent(MAX_CONCURRENT_HEALTH_CHECKS, |service| async move {
                if let Err(e) = self.check_service(&service).await {
                    warn!(
                        "Health check error for service {} ({}): {}",
                        service.id, service.name, e
                    );
                }
            })
            .await;

        self.stats_baselines.retain(&sampled_ids).await;

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
        let (mut status, response_time_ms, mut error_message) =
            match lifecycle_probe(&service.status) {
                LifecycleProbe::Probe => self.probe_service(service).await,
                LifecycleProbe::NotApplicable => {
                    // Still being provisioned, or deliberately stopped: there
                    // is nothing that should be answering yet, so this is
                    // neither a failed check nor downtime. Write no history
                    // row and no failure count (either would make a service
                    // that is merely starting look "Down", drag its uptime
                    // down, and after three ticks of a slow image pull, fire
                    // a "Service down" alert). Clear any verdict left over
                    // from before, so the first real probe after `running`
                    // is the first one the UI shows.
                    self.clear_health_verdict(service).await;
                    // A `starting` service waits on a readiness watch, which
                    // a restart of Temps loses; pick it back up so the
                    // service still reaches `running` or `failed`.
                    if service.status == crate::readiness::STARTING_STATUS {
                        self.manager.resume_readiness_watch(service).await;
                    }
                    return Ok(());
                }
                LifecycleProbe::Down => (
                    HealthProbeStatus::Down,
                    None,
                    Some(format!(
                        "Service status is '{}', not running",
                        service.status
                    )),
                ),
            };

        // Postgres standalone services get an additional WAL/archive probe.
        // The result is persisted under `health_metadata.postgres_wal` so the
        // UI can render warnings. WAL warnings downgrade Operational to
        // Degraded but never escalate Down upward — liveness wins.
        //
        // A database on a worker publishes its port on that worker, behind a
        // host recorded as `localhost`, so this process cannot connect: the
        // probe runs on the worker through its agent. When it cannot run,
        // the reason is recorded (`postgres_wal_unchecked`) so the page says
        // the check is not happening instead of showing nothing.
        let wal_outcome = if service.service_type == "postgres"
            && service.topology == "standalone"
            && !matches!(status, HealthProbeStatus::Down)
        {
            match service.node_id {
                None => self
                    .run_postgres_wal_probe(service)
                    .await
                    .map(WalProbeOutcome::Snapshot),
                Some(node_id) => {
                    let outcome = self
                        .manager
                        .probe_worker_postgres_wal(service, node_id)
                        .await;
                    if let WalProbeOutcome::Unchecked(reason) = &outcome {
                        debug!(
                            service_id = service.id,
                            service = %service.name,
                            node_id,
                            "WAL probe on worker produced no snapshot: {reason}"
                        );
                    }
                    Some(outcome)
                }
            }
        } else {
            None
        };
        let wal_snapshot = match &wal_outcome {
            Some(WalProbeOutcome::Snapshot(snapshot)) => Some(snapshot),
            _ => None,
        };

        if let Some(snapshot) = wal_snapshot {
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
        let recovering = !matches!(status, HealthProbeStatus::Down)
            && was_failing >= CONSECUTIVE_FAILURES_BEFORE_ALERT;
        let alarm_resolved = !recovering || self.send_recovered_alert(service).await;
        let now_failing = if matches!(status, HealthProbeStatus::Down) {
            was_failing + 1
        } else if !alarm_resolved {
            // Keep the recovery marker until resolution succeeds, so a
            // transient database failure cannot strand the active alarm.
            was_failing
        } else {
            0
        };

        let merged_metadata = wal_outcome.as_ref().and_then(|outcome| {
            record_wal_outcome(service.health_metadata.as_ref(), outcome, now)
                .map_err(|e| {
                    warn!(
                        "Failed to serialize WAL health for service {}: {}",
                        service.id, e
                    )
                })
                .ok()
        });

        // Partial update, not `service.clone().into()`: that stamps every
        // column from this cycle's snapshot, including
        // `continuous_archive_s3_source_id`/`continuous_archive_pinned_at`.
        // Since those can be written concurrently (by this same tick's own
        // binlog-archive step below, by a backup run's pin, or by a
        // deliberate repoint), a full-model save here would silently revert
        // a pin set after this cycle's snapshot was fetched but before this
        // update runs.
        let mut active = external_services::ActiveModel {
            id: Set(service.id),
            ..Default::default()
        };
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
        }

        // 4. MariaDB PITR: ship closed binary-log segments to S3 on the
        //    service's configured cadence. Only for running standalone
        //    MariaDB services that have a backup schedule (→ S3 destination).
        //    Failures here never affect health monitoring of other services.
        //
        //    A MariaDB on a worker node is shipped by the same archiver, run
        //    by its node's agent (ADR-099); the destination, its pin and the
        //    cadence are decided here either way.
        if service.service_type == "mariadb"
            && service.topology == "standalone"
            && service.status == "running"
            && !matches!(status, HealthProbeStatus::Down)
        {
            self.maybe_archive_mariadb_binlogs(service).await;
        }

        // 5. Container resource metrics: sample docker stats for every
        //    member container and write CPU/memory points to the metrics
        //    store. Gated on the same per-service `metrics_enabled` flag the
        //    engine-metrics scraper uses. A container on a worker is sampled
        //    through that node's agent. Failures are logged and swallowed —
        //    metrics must never disrupt health monitoring.
        if samples_container_metrics(service) {
            if let Some(store) = self.metrics_store.clone() {
                self.record_container_metrics(&store, service).await;
            }
        }

        Ok(())
    }

    /// Reset the denormalized health fields of a service that is not
    /// supposed to be answering, so the UI reads "pending check" instead of a
    /// stale or synthetic "down". A no-op (no write) when already clear.
    async fn clear_health_verdict(&self, service: &external_services::Model) {
        if service.health_status.is_none()
            && service.last_health_error.is_none()
            && service.consecutive_health_failures == 0
        {
            return;
        }
        // Resolve the alarm before resetting the counter that drives recovery.
        // If resolution fails, leave the old verdict/counter so the next tick
        // retries rather than losing the only evidence of an active outage.
        if (service.consecutive_health_failures >= CONSECUTIVE_FAILURES_BEFORE_ALERT
            || service.health_status.as_deref() == Some("down"))
            && !self.send_recovered_alert(service).await
        {
            return;
        }
        let active = external_services::ActiveModel {
            id: Set(service.id),
            health_status: Set(None),
            last_health_error: Set(None),
            consecutive_health_failures: Set(0),
            ..Default::default()
        };
        if let Err(e) = active.update(self.db.as_ref()).await {
            warn!(
                "Failed to clear health_status on service {} ('{}', status '{}'): {}",
                service.id, service.name, service.status, e
            );
        }
    }

    /// Sample container stats for one service and write the resulting
    /// CPU/memory points (`container_metric_points`) to the metrics store.
    ///
    /// Containers whose sample failed (stopped, agent unreachable) emit no
    /// points rather than zeros.
    async fn record_container_metrics(
        &self,
        store: &Arc<dyn MetricsStore>,
        service: &external_services::Model,
    ) {
        // Hold only this service's lock across the Docker and agent calls: a manual
        // check and the periodic loop sampling the same service take turns,
        // other services are sampled meanwhile, and the baselines stay in the
        // map if this future is dropped (a manual check whose client left).
        let service_baselines = self.stats_baselines.for_service(service.id).await;
        let sampled = {
            let mut baselines = service_baselines.lock().await;
            self.manager
                .sample_service_stats(service, &mut baselines)
                .await
        };

        let members = match sampled {
            Ok(members) => members,
            Err(e) => {
                debug!(
                    service_id = service.id,
                    service = %service.name,
                    "Container stats sampling failed: {}", e
                );
                return;
            }
        };
        for member in &members {
            if let Some(reason) = &member.sample.unavailable_reason {
                debug!(
                    service_id = service.id,
                    container = %member.sample.container_name,
                    "Container stats unavailable: {reason}"
                );
            }
        }

        let points = container_metric_points(service, &members, Utc::now());
        if points.is_empty() {
            return;
        }

        if let Err(e) = store.write_batch(points).await {
            warn!(
                service_id = service.id,
                service = %service.name,
                "Failed to write container metrics: {}", e
            );
        }
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

        // Once pinned, always ship to the pinned source — never re-resolve
        // from the current schedule state. Re-resolving every tick (the
        // previous behavior) meant editing *any* enabled schedule that
        // covers this service, even one unrelated via `target_all_services`,
        // could silently redirect an in-progress binlog stream mid-flight,
        // splitting the chain a PITR restore needs across buckets.
        let s3_source = if let Some(pinned_id) = service.continuous_archive_s3_source_id {
            match s3_sources::Entity::find_by_id(pinned_id)
                .one(self.db.as_ref())
                .await
            {
                Ok(Some(src)) => src,
                Ok(None) => {
                    warn!(
                        service_id = service.id,
                        "MariaDB service's pinned continuous archive S3 source {} no longer exists; skipping binlog archive",
                        pinned_id
                    );
                    return;
                }
                Err(e) => {
                    debug!(
                        service_id = service.id,
                        "Failed to load pinned S3 source for MariaDB binlog archive: {}", e
                    );
                    return;
                }
            }
        } else {
            // Unpinned yet: fall back to discovering a destination from a
            // backup schedule covering this service (no schedule = no PITR
            // destination configured = skip), then establish the pin —
            // deferring to the instance's Cloud-managed source when one
            // exists rather than whichever schedule happened to resolve
            // first, exactly like a WAL-G backup's first run.
            let candidate = match self.find_s3_source_for_service(service.id).await {
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
            if let Err(e) = continuous_archive::ensure_continuous_archive_source_pin(
                self.db.as_ref(),
                service,
                candidate.id,
                "MariaDB binlog archiving",
            )
            .await
            {
                warn!(
                    service_id = service.id,
                    "MariaDB binlog archiving destination is ambiguous, skipping this tick: {}", e
                );
                return;
            }
            candidate
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

        let worker_node = match crate::services::service_execution_route(service.node_id) {
            crate::services::ServiceExecutionRoute::Remote(node_id) => Some(node_id),
            _ => None,
        };
        let shipped = match worker_node {
            None => {
                let Some(docker_client) = self.docker.get() else {
                    info!(
                        service_id = service.id,
                        "Docker unavailable on this process; skipping MariaDB binlog archiving"
                    );
                    return;
                };
                MariaDbService::new(service.name.clone(), docker_client.clone())
                    .archive_binlogs(&s3_client, &s3_source, &mariadb_config)
                    .await
                    .map_err(|e| e.to_string())
            }
            Some(node_id) => self
                .archive_worker_mariadb_binlogs(service, node_id, &s3_source)
                .await
                .map_err(|reason| format!("on worker node {node_id}: {reason}")),
        };
        match shipped {
            Ok(shipped) => {
                if shipped > 0 {
                    info!(
                        service_id = service.id,
                        service = %service.name,
                        shipped,
                        "Archived MariaDB binlog segment(s) to S3"
                    );
                    // Retention counterpart to the ship. Only after something
                    // new landed — a no-op tick cannot have made an older
                    // segment newly unreachable, so there is nothing to prune.
                    // The prune reads and deletes S3 objects only; the engine
                    // handle it goes through is built on this host's Docker.
                    match self.docker.get() {
                        Some(docker_client) => {
                            let mariadb =
                                MariaDbService::new(service.name.clone(), docker_client.clone());
                            self.prune_mariadb_binlogs(service, &mariadb, &s3_client, &s3_source)
                                .await;
                        }
                        None => debug!(
                            service_id = service.id,
                            "No Docker handle on this process; archived MariaDB binlogs are \
                             pruned on a later run of a server that has one"
                        ),
                    }
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

    /// Delete archived binlog segments that no retained base backup can reach.
    ///
    /// `archive_binlogs` only ever uploads, so without this the `binlog/`
    /// prefix grows for the life of the service. The anchor is the recorded
    /// `binlog_file` of the OLDEST retained physical base backup: PITR replay
    /// always starts at its base's own coordinate, so nothing older than the
    /// oldest base is reachable from any retained backup.
    ///
    /// Failures are logged and swallowed, like the archive itself — losing a
    /// prune run costs storage, never data.
    async fn prune_mariadb_binlogs(
        &self,
        service: &external_services::Model,
        mariadb: &MariaDbService,
        s3_client: &aws_sdk_s3::Client,
        s3_source: &s3_sources::Model,
    ) {
        let anchor = match self
            .oldest_retained_mariadb_base_anchor(service, mariadb, s3_client, s3_source)
            .await
        {
            Ok(Some(anchor)) => anchor,
            Ok(None) => {
                debug!(
                    service_id = service.id,
                    service = %service.name,
                    "No retained MariaDB physical base with a binlog anchor; keeping all archived \
                     binlog segments"
                );
                return;
            }
            Err(e) => {
                warn!(
                    service_id = service.id,
                    service = %service.name,
                    "Could not determine MariaDB binlog retention anchor; keeping all archived \
                     segments: {}", e
                );
                return;
            }
        };

        match mariadb
            .prune_stale_binlogs(s3_client, s3_source, &anchor)
            .await
        {
            Ok(0) => {}
            Ok(pruned) => {
                info!(
                    service_id = service.id,
                    service = %service.name,
                    pruned,
                    anchor = %anchor,
                    "Pruned stale MariaDB binlog segments from S3"
                );
            }
            Err(e) => {
                warn!(
                    service_id = service.id,
                    service = %service.name,
                    anchor = %anchor,
                    "MariaDB binlog prune run failed: {}", e
                );
            }
        }
    }

    /// Resolve the retention anchor: the `binlog_file` recorded by the OLDEST
    /// still-retained physical base backup of this service.
    ///
    /// Backups are hard-deleted (`delete_backup_model` removes the rows), so
    /// "retained" is simply "the row still exists in a completed state".
    ///
    /// Every ambiguous case returns `Ok(None)` / `Err` — i.e. "do not prune":
    /// - no completed physical base at all (nothing anchors retention yet, and
    ///   a base backup may be about to run);
    /// - a completed physical-engine row whose `s3_location` we cannot read as
    ///   a base object, which would mean the true oldest base is invisible to
    ///   us and any anchor we picked would be too new;
    /// - the oldest base has no binlog coordinate (`pitr: false`), so we
    ///   cannot say which segments it would have needed.
    async fn oldest_retained_mariadb_base_anchor(
        &self,
        service: &external_services::Model,
        mariadb: &MariaDbService,
        s3_client: &aws_sdk_s3::Client,
        s3_source: &s3_sources::Model,
    ) -> Result<Option<String>, HealthMonitorError> {
        use sea_orm::QueryOrder;

        let rows = external_service_backups::Entity::find()
            .filter(external_service_backups::Column::ServiceId.eq(service.id))
            .filter(external_service_backups::Column::State.eq("completed"))
            .order_by_asc(external_service_backups::Column::StartedAt)
            .all(self.db.as_ref())
            .await?;

        let mut oldest_base: Option<&temps_entities::external_service_backups::Model> = None;
        for row in &rows {
            let is_physical_engine = row
                .metadata
                .get("engine")
                .and_then(|v| v.as_str())
                .is_some_and(|engine| engine == "mariadb_physical");
            let is_base_object = MariaDbService::is_physical_base_location(&row.s3_location);

            if is_physical_engine && !is_base_object {
                // A physical backup we can't locate. Its base could be older
                // than anything else we found, so we have no trustworthy
                // anchor — decline rather than prune against a newer one.
                return Err(HealthMonitorError::BinlogRetentionAnchor {
                    service_id: service.id,
                    service_name: service.name.clone(),
                    reason: format!(
                        "completed mariadb_physical backup row {} has no usable base location ('{}')",
                        row.id, row.s3_location
                    ),
                });
            }
            if is_base_object {
                oldest_base = Some(row);
                break;
            }
        }

        let Some(base) = oldest_base else {
            return Ok(None);
        };

        mariadb
            .base_binlog_anchor(s3_client, &s3_source.bucket_name, &base.s3_location)
            .await
            .map_err(|e| HealthMonitorError::BinlogRetentionAnchor {
                service_id: service.id,
                service_name: service.name.clone(),
                reason: format!(
                    "failed to read binlog anchor from base backup row {}: {}",
                    base.id, e
                ),
            })
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
    ) -> Result<S3Credentials, HealthMonitorError> {
        let decrypt_failed =
            |credential: &'static str, error: String| HealthMonitorError::S3CredentialDecryption {
                s3_source_id: s3_source.id,
                credential,
                reason: error,
            };
        let access_key_id = self
            .encryption_service
            .decrypt_string(&s3_source.access_key_id)
            .map_err(|e| decrypt_failed("access key", e.to_string()))?;
        let secret_key = self
            .encryption_service
            .decrypt_string(&s3_source.secret_key)
            .map_err(|e| decrypt_failed("secret key", e.to_string()))?;

        let session_token =
            temps_entities::s3_sources::decrypt_session_token(&self.encryption_service, s3_source)
                .map_err(|e| decrypt_failed("session token", e.to_string()))?;

        Ok(S3Credentials {
            access_key_id,
            secret_key,
            session_token,
            region: s3_source.region.clone(),
            endpoint: s3_source.endpoint.clone(),
            bucket_name: s3_source.bucket_name.clone(),
            bucket_path: s3_source.bucket_path.clone(),
            force_path_style: s3_source.effective_path_style(false).force_path_style,
        })
    }

    /// Ship the closed binary logs of `service`, a MariaDB on worker
    /// `node_id`, to `s3_source` with the control plane's archiver run by the
    /// node's agent, under a credential scoped to the service's `binlog/`
    /// prefix (ADR-102), never the source's own. Returns how many segments
    /// were shipped.
    async fn archive_worker_mariadb_binlogs(
        &self,
        service: &external_services::Model,
        node_id: i32,
        s3_source: &s3_sources::Model,
    ) -> Result<usize, String> {
        use crate::remote_service_client::MariadbBinlogArchiveResult;
        let destination = self
            .binlog_archive_destination(service, node_id, s3_source)
            .await?;
        let result = self
            .manager
            .mariadb_binlog_archive_worker(service.id, destination, s3_source.bucket_path.clone())
            .await;
        let shipped = match result {
            Ok(MariadbBinlogArchiveResult::Completed { shipped }) => Ok(shipped),
            Ok(
                MariadbBinlogArchiveResult::Refused { reason }
                | MariadbBinlogArchiveResult::Failed { reason },
            ) => {
                debug!(service_id = service.id, node_id, %reason, "Worker binlog archive did not complete");
                Err(reason)
            }
            Err(error) => Err(error.to_string()),
        };
        if shipped.is_err() {
            // The credential may be what failed (revoked, expired early):
            // the next run mints another instead of repeating the failure
            // for the rest of the refresh interval.
            self.binlog_archive_credentials
                .lock()
                .await
                .remove(&service.id);
        }
        shipped
    }

    /// The cached binary-log archiving credential of `service` on `node_id`
    /// for `s3_source`, or a new one from the manager when there is none,
    /// its reuse window has passed, or it was minted for anything else
    /// ([`BinlogCredentialGrant`]).
    async fn binlog_archive_destination(
        &self,
        service: &external_services::Model,
        node_id: i32,
        s3_source: &s3_sources::Model,
    ) -> Result<crate::remote_service_client::WorkerS3Destination, String> {
        let grant = BinlogCredentialGrant::of(&service.name, node_id, s3_source);
        if let Some(cached) = self
            .binlog_archive_credentials
            .lock()
            .await
            .get(&service.id)
            .filter(|cached| cached.grant == grant && Instant::now() < cached.reuse_until)
        {
            return Ok(cached.destination.clone());
        }
        let scoped = self
            .manager
            .mariadb_binlog_archive_destination(service, node_id, s3_source)
            .await
            .map_err(|error| error.to_string())?;
        let mut cache = self.binlog_archive_credentials.lock().await;
        match binlog_credential_reuse_window(&scoped.scope, Utc::now()) {
            Some(window) => {
                cache.insert(
                    service.id,
                    BinlogArchiveCredential {
                        grant,
                        reuse_until: Instant::now() + window,
                        destination: scoped.destination.clone(),
                    },
                );
            }
            None => {
                cache.remove(&service.id);
            }
        }
        Ok(scoped.destination)
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
        // Cluster services need a fan-out probe — the standalone
        // ExternalService::health_probe path can't reach a multi-host
        // cluster (it falls through to localhost:5432). Route through the
        // manager's cluster-aware probe instead.
        if service.topology == "cluster" {
            let result = self.manager.probe_cluster(service).await;
            return (result.status, result.response_time_ms, result.error_message);
        }

        match self.manager.probe_service_health(service).await {
            Ok(result) => (result.status, result.response_time_ms, result.error_message),
            Err(e) => (
                HealthProbeStatus::Down,
                None,
                Some(format!("health_probe raised an error: {}", e)),
            ),
        }
    }

    /// Resolve the project a service belongs to, for scoping its alarm.
    /// External services have no `project_id` column of their own — it's
    /// only known via the `project_services` join table.
    async fn resolve_project_id(&self, service_id: i32) -> Option<i32> {
        project_services::Entity::find()
            .filter(project_services::Column::ServiceId.eq(service_id))
            .one(self.db.as_ref())
            .await
            .ok()
            .flatten()
            .map(|ps| ps.project_id)
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

        let project_id = self.resolve_project_id(service.id).await;
        let request = FireAlarmRequest {
            project_id,
            environment_id: None,
            deployment_id: None,
            container_id: None,
            service_id: Some(service.id),
            alarm_type: AlarmType::ExternalServiceDown,
            severity: DOWN_ALERT_SEVERITY,
            title,
            message,
            metadata: Some(serde_json::json!({
                "service_name": service.name,
                "service_type": service.service_type,
            })),
        };

        match self.alarm_service.fire_alarm(request).await {
            Ok(Some(_)) => info!(
                "Sent health-check down alert for service {} ({})",
                service.id, service.name
            ),
            Ok(None) => debug!(
                "Down alert for service {} suppressed by cooldown/silence",
                service.id
            ),
            Err(e) => error!(
                "Failed to fire down-alert alarm for service {}: {}",
                service.id, e
            ),
        }
    }

    async fn send_recovered_alert(&self, service: &external_services::Model) -> bool {
        let project_id = self.resolve_project_id(service.id).await;
        if let Err(e) = self
            .alarm_service
            .resolve_alarms_by_scope(
                project_id,
                None,
                None,
                None,
                Some(service.id),
                AlarmType::ExternalServiceDown,
            )
            .await
        {
            error!(
                "Failed to resolve down-alert alarm(s) for recovered service {}: {}",
                service.id, e
            );
            false
        } else {
            info!(
                "Service {} ({}) recovered — resolved its down alarm(s)",
                service.id, service.name
            );
            true
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

/// What the health monitor does with a service, given its lifecycle
/// `external_services.status`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum LifecycleProbe {
    /// The service should be answering: probe it and record the result.
    Probe,
    /// Not expected to answer (still provisioning, or deliberately stopped):
    /// skip it without recording a check, a failure, or downtime.
    NotApplicable,
    /// Expected to exist but in a terminal bad state (e.g. `failed`, or a
    /// status this build does not know): record it as down.
    Down,
}

/// One service's stats baselines (`container_name` → previous tick).
type ServiceStatsBaselines = Arc<Mutex<HashMap<String, ContainerStatsBaseline>>>;

/// The health monitor's stats baselines, one entry per sampled service.
///
/// Each service has its own lock, held for the whole of that service's
/// sampling pass. The outer lock is only held to look the entry up, never
/// across Docker calls. Entries are never taken out of the map while a
/// service is sampled, so a sampler whose future is dropped cannot lose them.
#[derive(Default)]
struct StatsBaselines {
    services: Mutex<HashMap<i32, ServiceStatsBaselines>>,
}

impl StatsBaselines {
    /// The baselines of `service_id`, created empty on first use.
    async fn for_service(&self, service_id: i32) -> ServiceStatsBaselines {
        self.services
            .lock()
            .await
            .entry(service_id)
            .or_default()
            .clone()
    }

    /// Keep only the services in `sampled`.
    async fn retain(&self, sampled: &std::collections::HashSet<i32>) {
        self.services
            .lock()
            .await
            .retain(|id, _| sampled.contains(id));
    }
}

/// Whether the health loop samples this service's container stats: a
/// running service with metrics enabled (the same per-service flag the
/// engine-metrics scraper uses), on this host's Docker or on a worker,
/// whose agent samples it.
fn samples_container_metrics(service: &external_services::Model) -> bool {
    service.status == "running" && service.metrics_enabled
}

/// The metric points for one tick of a service's containers. All `Gauge`,
/// `source_kind = database`, `source_id = external_services.id`, one series
/// per container (`role` / `container_name` labels):
/// - `container.cpu_percent`: cores used × 100 (Docker's convention, 100 ==
///   one core), averaged since the previous tick. Absent on the first tick
///   per container (no rate baseline yet).
/// - `container.cpu_utilization_percent`: the same usage as a percentage of
///   the cores the container may use, its CPU limit or else every host core
///   (100 == saturated). What CPU alert rules threshold. Absent while the
///   limit is unknown.
/// - `container.cpu_limit_cores`: the applied CPU limit in cores, `0` when
///   uncapped, so a tile can read "used / limit cores". Written on every tick
///   where the limit is known, uncapped included: written only while capped,
///   the last capped value would stay the "latest" for the store's whole
///   latest-values window after a cap was removed.
/// - `container.memory_used_bytes`: RSS excluding page cache.
/// - `container.memory_percent`: usage relative to the container's memory
///   limit (host RAM when no limit is set, as on the live stats endpoint).
///
/// The names and meanings match what deployment containers record.
///
/// A service with more than one container (a cluster) also gets an
/// unlabelled `cpu_limit_cores` and `cpu_utilization_percent`, and
/// `container.cluster_cpu_percent` (cores used × 100): its busiest
/// container's reading, all three from one tick; see `busiest_member`. The
/// store reads the series with the fewest label keys for latest values and
/// charts, so that set is what tiles, charts and alerts see. Without it, the
/// latest usage and limit could each come from a different member and pair
/// into a reading that belongs to no container.
///
/// The busiest member's cores go under their own name rather than an
/// unlabelled `container.cpu_percent`: that is the one CPU metric with
/// history from before these were recorded, all of it labelled, and a chart
/// follows the fewest-label series, so an unlabelled one would hide that
/// history until it ages out.
fn container_metric_points(
    service: &external_services::Model,
    members: &[PolledContainerStats],
    now: chrono::DateTime<Utc>,
) -> Vec<MetricPoint> {
    let mut points = Vec::with_capacity(members.len() * 5);

    for member in members {
        let sample = &member.sample;
        let mut labels = HashMap::new();
        labels.insert("role".to_string(), sample.role.clone());
        labels.insert("container_name".to_string(), sample.container_name.clone());

        let make_point = |name: &str, value: f64| MetricPoint {
            time: now,
            source_kind: SourceKind::Database,
            source_id: service.id,
            name: name.to_string(),
            value,
            kind: MetricKind::Gauge,
            engine: Some(service.service_type.clone()),
            environment: None,
            node_id: service.node_id,
            labels: labels.clone(),
        };

        if let Some(cpu) = sample.cpu_percent {
            points.push(make_point("container.cpu_percent", cpu));
        }
        if let Some(utilization) = member.cpu_utilization_percent() {
            points.push(make_point("container.cpu_utilization_percent", utilization));
        }
        if let Some(limit) = member.cpu_limit {
            let cores = limit.cores().unwrap_or(0.0);
            points.push(make_point("container.cpu_limit_cores", cores));
        }
        if let Some(mem) = sample.memory_usage_bytes {
            points.push(make_point("container.memory_used_bytes", mem as f64));
        }
        if let Some(mem_pct) = sample.memory_percent {
            points.push(make_point("container.memory_percent", mem_pct));
        }
    }

    if let Some(busiest) = busiest_member(members) {
        let make_point = |name: &str, value: f64| MetricPoint {
            time: now,
            source_kind: SourceKind::Database,
            source_id: service.id,
            name: name.to_string(),
            value,
            kind: MetricKind::Gauge,
            engine: Some(service.service_type.clone()),
            environment: None,
            node_id: service.node_id,
            labels: HashMap::new(),
        };
        points.push(make_point(
            "container.cluster_cpu_percent",
            busiest.cpu_percent,
        ));
        points.push(make_point("container.cpu_limit_cores", busiest.limit_cores));
        points.push(make_point(
            "container.cpu_utilization_percent",
            busiest.utilization_percent,
        ));
    }

    points
}

/// The service-level CPU reading of a cluster: one container's own values,
/// from one tick.
#[derive(Debug, PartialEq)]
struct BusiestMemberCpu {
    /// Cores used × 100 by that container.
    cpu_percent: f64,
    /// Its CPU limit in cores; 0 when uncapped.
    limit_cores: f64,
    /// Its share of the cores it may use.
    utilization_percent: f64,
}

/// For a service with more than one container, the reading of the container
/// using the largest share of the cores it may use, among the containers that
/// reported a rate and a known limit this tick.
///
/// - One container's values, never a sum: usage and limit always pair up,
///   and a threshold keeps its per-container meaning however many members
///   the service has.
/// - The busiest, so one saturated member (a primary taking all writes)
///   still crosses an alert threshold.
/// - From whichever members reported: a stopped or unreadable member is left
///   out rather than holding the whole reading back, so the set is written
///   on every tick any member reports and never goes stale as "latest"
///   while the service runs degraded.
///
/// `None` for a single container (its own series already pair up) and on a
/// tick where no member reported both a rate and a limit.
fn busiest_member(members: &[PolledContainerStats]) -> Option<BusiestMemberCpu> {
    if members.len() < 2 {
        return None;
    }
    members
        .iter()
        .filter_map(|member| {
            Some(BusiestMemberCpu {
                cpu_percent: member.sample.cpu_percent?,
                limit_cores: member.cpu_limit?.cores().unwrap_or(0.0),
                utilization_percent: member.cpu_utilization_percent()?,
            })
        })
        .max_by(|a, b| a.utilization_percent.total_cmp(&b.utilization_percent))
}

fn lifecycle_probe(status: &str) -> LifecycleProbe {
    match status {
        "running" => LifecycleProbe::Probe,
        "pending" | "creating" | "starting" | "restarting" | "stopping" | "stopped" => {
            LifecycleProbe::NotApplicable
        }
        _ => LifecycleProbe::Down,
    }
}

// ── Tests ────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests;
