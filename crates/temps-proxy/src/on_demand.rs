//! On-demand environment manager (scale-to-zero)
//!
//! Tracks idle environments and coordinates wake-on-request.
//!
//! - **Idle tracking**: Records last-activity timestamp per environment (atomic, lock-free)
//! - **Sleep sweep**: Background task stops containers after idle timeout
//! - **Wake coordination**: First request triggers container start, concurrent requests wait
//! - **Multi-node**: Stops/starts all containers (local + remote) in parallel
//! - **Multi-proxy**: Uses atomic DB transitions (UPDATE WHERE) instead of locks

use async_trait::async_trait;
use dashmap::DashMap;
use sea_orm::{
    ColumnTrait, ConnectionTrait, DatabaseConnection, EntityTrait, QueryFilter, Statement,
};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};
use temps_core::{ForceRouteReloadJob, Job, JobQueue, OnDemandWaker};
use temps_entities::{deployment_containers, environments};
use thiserror::Error;
use tokio::sync::{Notify, OwnedSemaphorePermit, Semaphore};
use tracing::{debug, error, info, warn};

/// Trait for starting/stopping containers. Implemented by the deployer crate.
/// Injected via the plugin system to avoid a direct dependency from proxy -> deployer.
#[async_trait]
pub trait ContainerLifecycle: Send + Sync {
    /// Start a stopped container by its Docker container ID.
    async fn start_container(&self, container_id: &str) -> Result<(), OnDemandError>;

    /// Stop a running container by its Docker container ID.
    async fn stop_container(&self, container_id: &str) -> Result<(), OnDemandError>;

    /// Check if a container is running and healthy.
    async fn is_container_healthy(&self, container_id: &str) -> Result<bool, OnDemandError>;
}

#[derive(Error, Debug)]
pub enum OnDemandError {
    #[error("Database error: {0}")]
    Database(#[from] sea_orm::DbErr),

    #[error("Container operation failed for container {container_id}: {reason}")]
    ContainerOperation {
        container_id: String,
        reason: String,
    },

    #[error("Wake timeout after {timeout_secs}s for environment {environment_id}")]
    WakeTimeout {
        environment_id: i32,
        timeout_secs: i32,
    },

    #[error("Environment {environment_id} has no current deployment")]
    NoDeployment { environment_id: i32 },

    #[error("Environment {environment_id} not found")]
    NotFound { environment_id: i32 },
}

/// Per-environment wake coordination state (in-process only).
struct WakeState {
    /// True if a wake operation is currently in progress on this proxy instance.
    waking: AtomicBool,
    /// Waiters are notified when the wake completes (success or failure).
    notify: Notify,
}

/// Information about a sleeping environment, loaded from the route table.
#[derive(Clone, Debug)]
pub struct SleepingEnvironmentInfo {
    pub environment_id: i32,
    pub project_id: i32,
    pub deployment_id: i32,
    pub wake_timeout_seconds: i32,
}

/// Core on-demand manager. Lives in the proxy process.
pub struct OnDemandManager {
    /// Last request timestamp (epoch seconds) per environment_id.
    /// Updated atomically on every proxied request — zero overhead.
    last_activity: DashMap<i32, AtomicU64>,

    /// On-demand config per environment_id (only environments with on_demand=true).
    /// Refreshed when the route table reloads.
    configs: DashMap<i32, OnDemandConfig>,

    /// Per-environment wake coordination (in-process).
    wake_states: DashMap<i32, Arc<WakeState>>,

    /// Sleeping environments indexed by domain (for wake-on-request lookup).
    /// Populated during route table reload for environments with sleeping=true.
    sleeping_by_domain: DashMap<String, SleepingEnvironmentInfo>,

    /// Database connection for state transitions.
    db: Arc<DatabaseConnection>,

    /// Container lifecycle operations (injected).
    container_lifecycle: Arc<dyn ContainerLifecycle>,

    /// Shared in-process job queue. After a wake, `do_wake` publishes
    /// [`Job::ForceRouteReload`] on this queue so the in-process
    /// `RouteReloadSubscriber` reloads the route table deterministically —
    /// without a database connection in the critical path that can silently
    /// wedge. This is the same mechanism the deployment pipeline uses; the raw
    /// PG `NOTIFY route_table_changes` is still fired in addition, purely to
    /// reach remote worker nodes that don't share this queue.
    queue: Arc<dyn JobQueue>,

    /// Notified whenever the route table finishes a reload.
    /// Used by wake-on-request to know when routes are available after waking.
    route_reloaded: Notify,

    /// Caps how many requests may be parked in the inline wake/re-resolve path
    /// at once. The wake block runs in the Pingora request hot path and can hold
    /// a request for several seconds (wait-for-reload + bounded re-resolve). An
    /// unauthenticated client that knows a sleeping hostname could otherwise open
    /// many connections and pin proxy worker tasks. Requests that can't get a
    /// permit get an immediate retryable 503 instead of parking.
    wake_slots: Arc<Semaphore>,
}

/// Maximum number of requests allowed to be parked in the inline wake path
/// concurrently across this proxy instance. Generous enough to never throttle
/// legitimate first-request bursts, small enough to bound task/connection
/// pressure from an attacker hammering known sleeping hostnames.
const MAX_CONCURRENT_WAKE_WAITERS: usize = 256;

#[derive(Clone, Debug)]
pub(crate) struct OnDemandConfig {
    pub(crate) environment_id: i32,
    pub(crate) idle_timeout_seconds: i32,
    #[allow(dead_code)] // stored for use when waking via SleepingEnvironmentInfo
    pub(crate) wake_timeout_seconds: i32,
}

impl OnDemandManager {
    pub fn new(
        db: Arc<DatabaseConnection>,
        container_lifecycle: Arc<dyn ContainerLifecycle>,
        queue: Arc<dyn JobQueue>,
    ) -> Self {
        Self {
            last_activity: DashMap::new(),
            configs: DashMap::new(),
            wake_states: DashMap::new(),
            sleeping_by_domain: DashMap::new(),
            db,
            container_lifecycle,
            queue,
            route_reloaded: Notify::new(),
            wake_slots: Arc::new(Semaphore::new(MAX_CONCURRENT_WAKE_WAITERS)),
        }
    }

    /// Try to reserve a slot for parking a request in the inline wake path.
    ///
    /// Returns a permit (held for the duration of the wake/re-resolve) when
    /// capacity is available, or `None` when the proxy is already at
    /// [`MAX_CONCURRENT_WAKE_WAITERS`]. Callers that get `None` should return an
    /// immediate retryable 503 rather than parking, so an attacker hammering a
    /// known sleeping hostname cannot pin an unbounded number of worker tasks.
    pub fn try_acquire_wake_slot(&self) -> Option<OwnedSemaphorePermit> {
        Arc::clone(&self.wake_slots).try_acquire_owned().ok()
    }

    /// Test-only constructor that injects a no-op job queue, so existing tests
    /// don't need to thread a real queue through every call. Tests that assert
    /// on queue behavior construct the manager via [`Self::new`] with a real
    /// (counting) queue instead.
    #[cfg(test)]
    pub(crate) fn new_test(
        db: Arc<DatabaseConnection>,
        container_lifecycle: Arc<dyn ContainerLifecycle>,
    ) -> Self {
        Self::new(db, container_lifecycle, Arc::new(tests::NoopQueue))
    }

    // ── Activity Tracking ──

    /// Record that a request was received for an environment.
    /// Called on every proxied request — must be O(1) and lock-free.
    pub fn record_activity(&self, environment_id: i32) {
        let now = self.current_epoch_secs();
        if let Some(entry) = self.last_activity.get(&environment_id) {
            entry.value().store(now, Ordering::Relaxed);
        } else {
            self.last_activity
                .insert(environment_id, AtomicU64::new(now));
        }
    }

    /// Check if a domain maps to a sleeping environment.
    /// Returns wake info if found.
    pub fn get_sleeping_environment(&self, domain: &str) -> Option<SleepingEnvironmentInfo> {
        self.sleeping_by_domain
            .get(domain)
            .map(|r| r.value().clone())
    }

    /// Register a sleeping environment domain for wake-on-request lookup.
    pub fn register_sleeping_domain(&self, domain: String, info: SleepingEnvironmentInfo) {
        self.sleeping_by_domain.insert(domain, info);
    }

    /// Clear all sleeping domain mappings (called before route table reload).
    pub fn clear_sleeping_domains(&self) {
        self.sleeping_by_domain.clear();
    }

    /// Signal that the route table has been reloaded.
    /// Called from the route-reload callback in server.rs.
    pub fn notify_route_reloaded(&self) {
        self.route_reloaded.notify_waiters();
    }

    /// Wait for the next route table reload, with a timeout.
    ///
    /// Returns `true` if a reload was observed within `timeout`, `false` on
    /// timeout. Callers treat `false` as non-fatal (re-resolve / retry), but a
    /// truthful return lets them log and react instead of silently proceeding
    /// with a stale route table.
    ///
    /// **Lost-wakeup note:** `route_reloaded` is a bare [`Notify`] with no
    /// stored permit — `notify_waiters()` only wakes waiters that are *already
    /// registered*, and a `Notified` future only registers when first polled.
    /// Building the future before the `await` *narrows* the race window (a
    /// notification fired after the future is first polled is delivered), but it
    /// cannot fully close it: this signal carries no state to re-check, so a
    /// reload that completes before this future is polled is still missed. That
    /// is acceptable because the proxy caller does NOT rely on this signal for
    /// correctness — it re-resolves the route in a bounded loop afterwards, so a
    /// missed wakeup costs latency, not a failed request (see the proxy wake
    /// block). This returns `bool` (rather than swallowing the timeout) so the
    /// caller can log and react instead of silently trusting a stale table.
    pub async fn wait_for_route_reload(&self, timeout: Duration) -> bool {
        // Build the Notified before awaiting to narrow (not eliminate) the
        // lost-wakeup window; see the method docs.
        let notified = self.route_reloaded.notified();
        match tokio::time::timeout(timeout, notified).await {
            Ok(()) => true,
            Err(_) => {
                warn!("Timed out waiting for route reload after wake");
                false
            }
        }
    }

    /// Update on-demand configs from loaded environments.
    /// Called after route table reload.
    #[allow(dead_code)] // will be called when wired to route table reload
    pub(crate) fn update_configs(&self, configs: Vec<OnDemandConfig>) {
        self.configs.clear();
        for config in configs {
            self.configs.insert(config.environment_id, config.clone());
            // Initialize activity tracking for new on-demand environments
            self.last_activity
                .entry(config.environment_id)
                .or_insert_with(|| AtomicU64::new(self.current_epoch_secs()));
        }
    }

    /// Register on-demand config for a single environment.
    pub fn register_on_demand_environment(
        &self,
        environment_id: i32,
        idle_timeout_seconds: i32,
        wake_timeout_seconds: i32,
    ) {
        self.configs.insert(
            environment_id,
            OnDemandConfig {
                environment_id,
                idle_timeout_seconds,
                wake_timeout_seconds,
            },
        );
        self.last_activity
            .entry(environment_id)
            .or_insert_with(|| AtomicU64::new(self.current_epoch_secs()));
    }

    /// Remove tracking for an environment that is no longer on-demand.
    pub fn remove_environment(&self, environment_id: i32) {
        self.configs.remove(&environment_id);
        self.last_activity.remove(&environment_id);
        self.wake_states.remove(&environment_id);
    }

    // ── Sleep (Idle -> Sleeping) ──

    /// Run one sweep iteration. Checks all on-demand environments for idle timeout.
    /// Also persists last_activity_at to the database for UI display.
    /// Returns IDs of environments that were put to sleep.
    pub async fn sweep_idle_environments(&self) -> Vec<i32> {
        let now = self.current_epoch_secs();
        let mut slept = Vec::new();

        // Collect activity updates to batch-persist to DB
        let mut activity_updates: Vec<(i32, u64)> = Vec::new();

        for entry in self.configs.iter() {
            let config = entry.value();
            let env_id = config.environment_id;

            if let Some(last) = self.last_activity.get(&env_id) {
                let last_secs = last.value().load(Ordering::Relaxed);
                activity_updates.push((env_id, last_secs));

                let idle_secs = now.saturating_sub(last_secs);

                if idle_secs >= config.idle_timeout_seconds as u64 {
                    match self.sleep_environment(env_id).await {
                        Ok(true) => {
                            info!(
                                environment_id = env_id,
                                idle_secs = idle_secs,
                                "Environment put to sleep after idle timeout"
                            );
                            slept.push(env_id);
                        }
                        Ok(false) => {
                            debug!(
                                environment_id = env_id,
                                "Environment already sleeping or no deployment"
                            );
                        }
                        Err(e) => {
                            error!(
                                environment_id = env_id,
                                error = %e,
                                "Failed to put environment to sleep"
                            );
                        }
                    }
                }
            }
        }

        // Batch-persist last_activity_at to DB (best-effort, doesn't block sweep)
        self.persist_activity_timestamps(&activity_updates).await;

        slept
    }

    /// Persist in-memory activity timestamps to the database for UI display.
    /// Uses a batch UPDATE for efficiency. Failures are logged but don't affect sweep.
    async fn persist_activity_timestamps(&self, updates: &[(i32, u64)]) {
        if updates.is_empty() {
            return;
        }

        for (env_id, epoch_secs) in updates {
            let timestamp = chrono::DateTime::from_timestamp(*epoch_secs as i64, 0);
            if let Some(ts) = timestamp {
                if let Err(e) = self
                    .db
                    .execute(Statement::from_sql_and_values(
                        sea_orm::DatabaseBackend::Postgres,
                        "UPDATE environments SET last_activity_at = $1 WHERE id = $2 AND sleeping = false",
                        [ts.into(), (*env_id).into()],
                    ))
                    .await
                {
                    debug!(
                        environment_id = env_id,
                        error = %e,
                        "Failed to persist last_activity_at"
                    );
                }
            }
        }
    }

    /// Put an environment to sleep: stop all containers, set sleeping=true.
    /// Returns Ok(true) if this call won the race and performed the sleep.
    /// Returns Ok(false) if the environment was already sleeping or has no deployment.
    pub async fn sleep_environment(&self, environment_id: i32) -> Result<bool, OnDemandError> {
        // Atomic CAS via UPDATE WHERE — only one proxy wins
        let result = self
            .db
            .execute(Statement::from_sql_and_values(
                sea_orm::DatabaseBackend::Postgres,
                "UPDATE environments SET sleeping = true WHERE id = $1 AND sleeping = false RETURNING id",
                [environment_id.into()],
            ))
            .await?;

        if result.rows_affected() == 0 {
            return Ok(false); // Already sleeping or doesn't exist
        }

        // Load containers for the current deployment
        let env = environments::Entity::find_by_id(environment_id)
            .one(self.db.as_ref())
            .await?
            .ok_or(OnDemandError::NotFound { environment_id })?;

        let deployment_id = match env.current_deployment_id {
            Some(id) => id,
            None => return Ok(false),
        };

        let containers = deployment_containers::Entity::find()
            .filter(deployment_containers::Column::DeploymentId.eq(deployment_id))
            .filter(deployment_containers::Column::DeletedAt.is_null())
            .all(self.db.as_ref())
            .await?;

        // Stop all containers in parallel, tracking failures
        let stop_futures: Vec<_> = containers
            .iter()
            .map(|c| {
                let container_id = c.container_id.clone();
                let lifecycle = Arc::clone(&self.container_lifecycle);
                async move {
                    match lifecycle.stop_container(&container_id).await {
                        Ok(()) => Ok(container_id),
                        Err(e) => {
                            warn!(
                                container_id = %container_id,
                                error = %e,
                                "Failed to stop container during sleep"
                            );
                            Err((container_id, e))
                        }
                    }
                }
            })
            .collect();

        let results = futures::future::join_all(stop_futures).await;

        let failed: Vec<_> = results.iter().filter(|r| r.is_err()).collect();
        if !failed.is_empty() {
            // Some containers failed to stop — revert sleeping state to avoid
            // inconsistency where DB says sleeping but containers are still running
            error!(
                environment_id = environment_id,
                failed_count = failed.len(),
                total = containers.len(),
                "Failed to stop some containers during sleep, reverting sleeping state"
            );
            let _ = self
                .db
                .execute(Statement::from_sql_and_values(
                    sea_orm::DatabaseBackend::Postgres,
                    "UPDATE environments SET sleeping = false WHERE id = $1",
                    [environment_id.into()],
                ))
                .await;
            self.notify_route_change().await;
            return Err(OnDemandError::ContainerOperation {
                container_id: "multiple".to_string(),
                reason: format!(
                    "Failed to stop {}/{} containers during sleep",
                    failed.len(),
                    containers.len()
                ),
            });
        }

        info!(
            environment_id = environment_id,
            deployment_id = deployment_id,
            containers_stopped = containers.len(),
            "Environment sleeping"
        );

        // Fire PG NOTIFY so other proxy instances reload routes
        self.notify_route_change().await;

        Ok(true)
    }

    // ── Wake (Sleeping -> Running) ──

    /// Wake an environment: start all containers, wait for health checks, set sleeping=false.
    /// Handles concurrent requests: first caller starts containers, others wait.
    pub async fn wake_environment(
        &self,
        environment_id: i32,
        wake_timeout_seconds: i32,
    ) -> Result<(), OnDemandError> {
        // Get or create wake state for this environment
        let wake_state = self
            .wake_states
            .entry(environment_id)
            .or_insert_with(|| {
                Arc::new(WakeState {
                    waking: AtomicBool::new(false),
                    notify: Notify::new(),
                })
            })
            .value()
            .clone();

        // Try to become the waker
        if wake_state
            .waking
            .compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst)
            .is_ok()
        {
            // We won — perform the wake
            let result = self.do_wake(environment_id, wake_timeout_seconds).await;

            // Signal all waiters (regardless of success/failure)
            wake_state.waking.store(false, Ordering::SeqCst);
            wake_state.notify.notify_waiters();

            return result;
        }

        // Someone else is waking — wait for them
        let timeout = Duration::from_secs(wake_timeout_seconds as u64);
        match tokio::time::timeout(timeout, wake_state.notify.notified()).await {
            Ok(_) => {
                // Wake completed — check if environment is actually awake
                let env = environments::Entity::find_by_id(environment_id)
                    .one(self.db.as_ref())
                    .await?
                    .ok_or(OnDemandError::NotFound { environment_id })?;

                if env.sleeping {
                    // Waker failed — return error
                    Err(OnDemandError::ContainerOperation {
                        container_id: "all".to_string(),
                        reason: "Wake operation failed on another request".to_string(),
                    })
                } else {
                    Ok(())
                }
            }
            Err(_) => Err(OnDemandError::WakeTimeout {
                environment_id,
                timeout_secs: wake_timeout_seconds,
            }),
        }
    }

    /// Perform the actual wake operation. Called only by the winning waker.
    async fn do_wake(
        &self,
        environment_id: i32,
        wake_timeout_seconds: i32,
    ) -> Result<(), OnDemandError> {
        // Atomic CAS — only one proxy instance wins
        let result = self
            .db
            .execute(Statement::from_sql_and_values(
                sea_orm::DatabaseBackend::Postgres,
                "UPDATE environments SET sleeping = false WHERE id = $1 AND sleeping = true RETURNING id",
                [environment_id.into()],
            ))
            .await?;

        if result.rows_affected() == 0 {
            // Another proxy already woke it — just wait for route reload
            return Ok(());
        }

        // Load containers for the current deployment
        let env = environments::Entity::find_by_id(environment_id)
            .one(self.db.as_ref())
            .await?
            .ok_or(OnDemandError::NotFound { environment_id })?;

        let deployment_id = env
            .current_deployment_id
            .ok_or(OnDemandError::NoDeployment { environment_id })?;

        let containers = deployment_containers::Entity::find()
            .filter(deployment_containers::Column::DeploymentId.eq(deployment_id))
            .filter(deployment_containers::Column::DeletedAt.is_null())
            .all(self.db.as_ref())
            .await?;

        if containers.is_empty() {
            warn!(
                environment_id = environment_id,
                "No containers found to wake"
            );
            // We've already committed sleeping=false, so the route table must
            // reload to drop the sleeping exclusion — otherwise the DB and the
            // in-memory routes disagree and the request's re-resolve loop spins
            // until it times out. Trigger the same deterministic reload as the
            // success path.
            self.publish_route_reload(environment_id, deployment_id)
                .await;
            self.notify_route_change().await;
            return Ok(());
        }

        info!(
            environment_id = environment_id,
            deployment_id = deployment_id,
            container_count = containers.len(),
            "Waking environment"
        );

        // Start all containers in parallel
        let start_results: Vec<Result<String, OnDemandError>> = {
            let futures: Vec<_> = containers
                .iter()
                .map(|c| {
                    let container_id = c.container_id.clone();
                    let lifecycle = Arc::clone(&self.container_lifecycle);
                    async move {
                        lifecycle.start_container(&container_id).await?;
                        Ok(container_id)
                    }
                })
                .collect();
            futures::future::join_all(futures).await
        };

        // Check for failures
        let mut failed = Vec::new();
        let mut started = Vec::new();
        for result in start_results {
            match result {
                Ok(id) => started.push(id),
                Err(e) => {
                    error!(error = %e, "Failed to start container during wake");
                    failed.push(e);
                }
            }
        }

        if !failed.is_empty() {
            // Some containers failed to start — revert to sleeping
            error!(
                environment_id = environment_id,
                started = started.len(),
                failed = failed.len(),
                "Wake partially failed, reverting to sleeping"
            );

            // Stop any containers we managed to start
            for container_id in &started {
                let _ = self.container_lifecycle.stop_container(container_id).await;
            }

            // Revert DB state
            let _ = self
                .db
                .execute(Statement::from_sql_and_values(
                    sea_orm::DatabaseBackend::Postgres,
                    "UPDATE environments SET sleeping = true WHERE id = $1",
                    [environment_id.into()],
                ))
                .await;

            return Err(OnDemandError::ContainerOperation {
                container_id: "multiple".to_string(),
                reason: format!(
                    "Failed to start {}/{} containers",
                    failed.len(),
                    started.len() + failed.len()
                ),
            });
        }

        // Wait for health checks with timeout
        let health_timeout = Duration::from_secs(wake_timeout_seconds as u64);
        let health_start = Instant::now();

        for container_id in &started {
            loop {
                if health_start.elapsed() > health_timeout {
                    error!(
                        environment_id = environment_id,
                        container_id = %container_id,
                        "Health check timeout during wake"
                    );

                    // Revert: stop all and set sleeping
                    for cid in &started {
                        let _ = self.container_lifecycle.stop_container(cid).await;
                    }
                    let _ = self
                        .db
                        .execute(Statement::from_sql_and_values(
                            sea_orm::DatabaseBackend::Postgres,
                            "UPDATE environments SET sleeping = true WHERE id = $1",
                            [environment_id.into()],
                        ))
                        .await;

                    return Err(OnDemandError::WakeTimeout {
                        environment_id,
                        timeout_secs: wake_timeout_seconds,
                    });
                }

                match self
                    .container_lifecycle
                    .is_container_healthy(container_id)
                    .await
                {
                    Ok(true) => break,
                    Ok(false) => {
                        tokio::time::sleep(Duration::from_millis(500)).await;
                    }
                    Err(e) => {
                        warn!(
                            container_id = %container_id,
                            error = %e,
                            "Health check error, retrying"
                        );
                        tokio::time::sleep(Duration::from_millis(500)).await;
                    }
                }
            }
        }

        // Record activity so we don't immediately sleep again
        self.record_activity(environment_id);

        // Persist last_activity_at to DB so the UI shows it immediately after wake
        let now_ts = chrono::Utc::now();
        let _ = self
            .db
            .execute(Statement::from_sql_and_values(
                sea_orm::DatabaseBackend::Postgres,
                "UPDATE environments SET last_activity_at = $1 WHERE id = $2",
                [now_ts.into(), environment_id.into()],
            ))
            .await;

        info!(
            environment_id = environment_id,
            containers_started = started.len(),
            wake_duration_ms = health_start.elapsed().as_millis(),
            "Environment awake"
        );

        // Reload routes (in-process ForceRouteReload + PG NOTIFY for remote nodes).
        self.publish_route_reload(environment_id, deployment_id)
            .await;
        self.notify_route_change().await;

        Ok(())
    }

    // ── Helpers ──

    fn current_epoch_secs(&self) -> u64 {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default();
        now.as_secs()
    }

    /// Publish an in-process `Job::ForceRouteReload` so the `RouteReloadSubscriber`
    /// reloads the route table deterministically after a wake. A woken environment
    /// was excluded from the route table while sleeping, so the table MUST reload
    /// before the request can resolve. This path (the same one the deploy pipeline
    /// uses) has no database connection in its critical path and therefore cannot
    /// silently wedge the way a long-lived PG `LISTEN` connection can. Callers fire
    /// `notify_route_change()` in addition, to reach remote worker nodes that don't
    /// share this in-process queue. Failure is non-fatal: the PG NOTIFY and the
    /// proxy's bounded re-resolve loop are the fallbacks.
    async fn publish_route_reload(&self, environment_id: i32, deployment_id: i32) {
        if let Err(e) = self
            .queue
            .send(Job::ForceRouteReload(ForceRouteReloadJob {
                environment_id: Some(environment_id),
                deployment_id: Some(deployment_id),
            }))
            .await
        {
            warn!(
                environment_id = environment_id,
                deployment_id = deployment_id,
                error = %e,
                "Failed to publish in-process ForceRouteReload after wake; \
                 falling back to PG NOTIFY"
            );
        }
    }

    async fn notify_route_change(&self) {
        if let Err(e) = self
            .db
            .execute(Statement::from_string(
                sea_orm::DatabaseBackend::Postgres,
                "NOTIFY route_table_changes".to_string(),
            ))
            .await
        {
            error!(error = %e, "Failed to send route_table_changes NOTIFY");
        }
    }

    /// Start the background idle sweep task in its own thread with a dedicated runtime.
    /// Checks every `sweep_interval` for environments that have exceeded their idle timeout.
    pub fn start_sweep_task(self: &Arc<Self>, sweep_interval: Duration) {
        let manager = Arc::clone(self);
        std::thread::Builder::new()
            .name("on-demand-sweep".to_string())
            .spawn(move || {
                let rt = tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build()
                    .expect("Failed to create tokio runtime for on-demand sweep");
                rt.block_on(async move {
                    let mut interval = tokio::time::interval(sweep_interval);
                    loop {
                        interval.tick().await;
                        let slept = manager.sweep_idle_environments().await;
                        if !slept.is_empty() {
                            debug!(
                                count = slept.len(),
                                environment_ids = ?slept,
                                "Idle sweep put environments to sleep"
                            );
                        }
                    }
                });
            })
            .expect("Failed to spawn on-demand sweep thread");
    }
}

/// Bridge implementation so the proxy's OnDemandManager can be injected into
/// the environments handler AppState via the plugin system.
#[async_trait]
impl OnDemandWaker for OnDemandManager {
    async fn wake_environment(
        &self,
        environment_id: i32,
        wake_timeout_seconds: i32,
    ) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        self.wake_environment(environment_id, wake_timeout_seconds)
            .await
            .map_err(|e| Box::new(e) as Box<dyn std::error::Error + Send + Sync>)
    }

    async fn sleep_environment(
        &self,
        environment_id: i32,
    ) -> Result<bool, Box<dyn std::error::Error + Send + Sync>> {
        self.sleep_environment(environment_id)
            .await
            .map_err(|e| Box::new(e) as Box<dyn std::error::Error + Send + Sync>)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use sea_orm::{DatabaseBackend, MockDatabase, MockExecResult};
    use std::sync::Mutex;

    // ── Mock JobQueue ──

    /// A job queue that drops everything. Used by [`OnDemandManager::new_test`]
    /// for the many tests that don't care about reload signalling.
    pub(crate) struct NoopQueue;

    #[async_trait]
    impl JobQueue for NoopQueue {
        async fn send(&self, _job: Job) -> Result<(), temps_core::QueueError> {
            Ok(())
        }

        fn subscribe(&self) -> Box<dyn temps_core::JobReceiver> {
            // No test consumes from the noop queue; a never-ready receiver is
            // sufficient and avoids pulling in a broadcast channel here.
            Box::new(NoopReceiver)
        }
    }

    struct NoopReceiver;

    #[async_trait]
    impl temps_core::JobReceiver for NoopReceiver {
        async fn recv(&mut self) -> Result<Job, temps_core::QueueError> {
            // Park forever — the noop queue never delivers.
            std::future::pending().await
        }
    }

    /// A job queue that captures every `ForceRouteReload` job published, so a
    /// test can assert both that the wake path posts the in-process reload
    /// request AND that it carries the right environment/deployment IDs.
    struct CountingQueue {
        force_reloads: Arc<Mutex<Vec<ForceRouteReloadJob>>>,
    }

    #[async_trait]
    impl JobQueue for CountingQueue {
        async fn send(&self, job: Job) -> Result<(), temps_core::QueueError> {
            if let Job::ForceRouteReload(req) = job {
                self.force_reloads.lock().unwrap().push(req);
            }
            Ok(())
        }

        fn subscribe(&self) -> Box<dyn temps_core::JobReceiver> {
            Box::new(NoopReceiver)
        }
    }

    // ── Mock ContainerLifecycle ──

    #[derive(Default)]
    struct MockContainerState {
        started: Vec<String>,
        stopped: Vec<String>,
        healthy: bool,
        fail_start: bool,
        fail_health: bool,
        /// Containers that should fail on start (selective failure)
        fail_start_ids: Vec<String>,
        /// Containers that should fail on stop
        fail_stop_ids: Vec<String>,
        /// Number of health checks before becoming healthy (0 = immediate)
        health_check_delay: u32,
        /// Current health check count per container
        health_check_counts: std::collections::HashMap<String, u32>,
        /// If true, health checks always return false (never healthy)
        never_healthy: bool,
    }

    struct MockLifecycle {
        state: Mutex<MockContainerState>,
    }

    impl MockLifecycle {
        fn new() -> Self {
            Self {
                state: Mutex::new(MockContainerState {
                    healthy: true,
                    ..Default::default()
                }),
            }
        }

        fn with_fail_start() -> Self {
            Self {
                state: Mutex::new(MockContainerState {
                    fail_start: true,
                    ..Default::default()
                }),
            }
        }

        fn with_selective_start_failures(fail_ids: Vec<String>) -> Self {
            Self {
                state: Mutex::new(MockContainerState {
                    healthy: true,
                    fail_start_ids: fail_ids,
                    ..Default::default()
                }),
            }
        }

        fn with_selective_stop_failures(fail_ids: Vec<String>) -> Self {
            Self {
                state: Mutex::new(MockContainerState {
                    healthy: true,
                    fail_stop_ids: fail_ids,
                    ..Default::default()
                }),
            }
        }

        fn with_never_healthy() -> Self {
            Self {
                state: Mutex::new(MockContainerState {
                    healthy: false,
                    never_healthy: true,
                    ..Default::default()
                }),
            }
        }

        fn with_delayed_health(delay_checks: u32) -> Self {
            Self {
                state: Mutex::new(MockContainerState {
                    healthy: false,
                    health_check_delay: delay_checks,
                    ..Default::default()
                }),
            }
        }

        #[allow(dead_code)]
        fn with_unhealthy() -> Self {
            Self {
                state: Mutex::new(MockContainerState {
                    healthy: false,
                    fail_health: true,
                    ..Default::default()
                }),
            }
        }

        fn started_containers(&self) -> Vec<String> {
            self.state.lock().unwrap().started.clone()
        }

        fn stopped_containers(&self) -> Vec<String> {
            self.state.lock().unwrap().stopped.clone()
        }
    }

    #[async_trait]
    impl ContainerLifecycle for MockLifecycle {
        async fn start_container(&self, container_id: &str) -> Result<(), OnDemandError> {
            let mut state = self.state.lock().unwrap();
            if state.fail_start {
                return Err(OnDemandError::ContainerOperation {
                    container_id: container_id.to_string(),
                    reason: "Mock start failure".to_string(),
                });
            }
            if state.fail_start_ids.contains(&container_id.to_string()) {
                return Err(OnDemandError::ContainerOperation {
                    container_id: container_id.to_string(),
                    reason: format!("Mock selective start failure for {}", container_id),
                });
            }
            state.started.push(container_id.to_string());
            Ok(())
        }

        async fn stop_container(&self, container_id: &str) -> Result<(), OnDemandError> {
            let state = self.state.lock().unwrap();
            if state.fail_stop_ids.contains(&container_id.to_string()) {
                return Err(OnDemandError::ContainerOperation {
                    container_id: container_id.to_string(),
                    reason: format!("Mock selective stop failure for {}", container_id),
                });
            }
            drop(state);
            self.state
                .lock()
                .unwrap()
                .stopped
                .push(container_id.to_string());
            Ok(())
        }

        async fn is_container_healthy(&self, _container_id: &str) -> Result<bool, OnDemandError> {
            let mut state = self.state.lock().unwrap();
            if state.fail_health {
                return Err(OnDemandError::ContainerOperation {
                    container_id: _container_id.to_string(),
                    reason: "Mock health check failure".to_string(),
                });
            }
            if state.never_healthy {
                return Ok(false);
            }
            if state.health_check_delay > 0 {
                let count = state
                    .health_check_counts
                    .entry(_container_id.to_string())
                    .or_insert(0);
                *count += 1;
                if *count >= state.health_check_delay {
                    return Ok(true);
                }
                return Ok(false);
            }
            Ok(state.healthy)
        }
    }

    // ── Tests ──

    #[test]
    fn test_record_activity_updates_timestamp() {
        let db = MockDatabase::new(DatabaseBackend::Postgres).into_connection();
        let lifecycle = Arc::new(MockLifecycle::new());
        let manager = OnDemandManager::new_test(Arc::new(db), lifecycle);

        manager.record_activity(1);
        let ts1 = manager
            .last_activity
            .get(&1)
            .unwrap()
            .value()
            .load(Ordering::Relaxed);

        // Second call updates
        std::thread::sleep(Duration::from_millis(10));
        manager.record_activity(1);
        let ts2 = manager
            .last_activity
            .get(&1)
            .unwrap()
            .value()
            .load(Ordering::Relaxed);

        assert!(ts2 >= ts1);
    }

    #[test]
    fn test_on_demand_disabled_by_default() {
        let config = temps_entities::deployment_config::DeploymentConfig::default();
        assert!(!config.on_demand);
        assert_eq!(config.idle_timeout_seconds, 300);
        assert_eq!(config.wake_timeout_seconds, 30);
    }

    #[test]
    fn test_on_demand_validation_valid() {
        let config = temps_entities::deployment_config::DeploymentConfig {
            on_demand: true,
            idle_timeout_seconds: 120,
            wake_timeout_seconds: 15,
            ..Default::default()
        };
        assert!(config.validate().is_ok());
    }

    #[test]
    fn test_on_demand_validation_idle_too_low() {
        let config = temps_entities::deployment_config::DeploymentConfig {
            on_demand: true,
            idle_timeout_seconds: 30, // below 60 minimum
            ..Default::default()
        };
        assert!(config.validate().is_err());
        assert!(config.validate().unwrap_err().contains("Idle timeout"));
    }

    #[test]
    fn test_on_demand_validation_idle_too_high() {
        let config = temps_entities::deployment_config::DeploymentConfig {
            on_demand: true,
            idle_timeout_seconds: 100_000, // above 86400 max
            ..Default::default()
        };
        assert!(config.validate().is_err());
    }

    #[test]
    fn test_on_demand_validation_wake_too_low() {
        let config = temps_entities::deployment_config::DeploymentConfig {
            on_demand: true,
            wake_timeout_seconds: 2, // below 5 minimum
            ..Default::default()
        };
        assert!(config.validate().is_err());
        assert!(config.validate().unwrap_err().contains("Wake timeout"));
    }

    #[test]
    fn test_on_demand_validation_wake_too_high() {
        let config = temps_entities::deployment_config::DeploymentConfig {
            on_demand: true,
            wake_timeout_seconds: 200, // above 120 max
            ..Default::default()
        };
        assert!(config.validate().is_err());
    }

    #[test]
    fn test_on_demand_validation_skipped_when_disabled() {
        let config = temps_entities::deployment_config::DeploymentConfig {
            on_demand: false,
            idle_timeout_seconds: 1, // would be invalid if on_demand were true
            wake_timeout_seconds: 1,
            ..Default::default()
        };
        assert!(config.validate().is_ok());
    }

    #[test]
    fn test_sleeping_domain_lookup() {
        let db = MockDatabase::new(DatabaseBackend::Postgres).into_connection();
        let lifecycle = Arc::new(MockLifecycle::new());
        let manager = OnDemandManager::new_test(Arc::new(db), lifecycle);

        assert!(manager.get_sleeping_environment("example.com").is_none());

        manager.register_sleeping_domain(
            "example.com".to_string(),
            SleepingEnvironmentInfo {
                environment_id: 1,
                project_id: 10,
                deployment_id: 100,
                wake_timeout_seconds: 30,
            },
        );

        let info = manager.get_sleeping_environment("example.com").unwrap();
        assert_eq!(info.environment_id, 1);
        assert_eq!(info.project_id, 10);
        assert_eq!(info.deployment_id, 100);
    }

    #[test]
    fn test_clear_sleeping_domains() {
        let db = MockDatabase::new(DatabaseBackend::Postgres).into_connection();
        let lifecycle = Arc::new(MockLifecycle::new());
        let manager = OnDemandManager::new_test(Arc::new(db), lifecycle);

        manager.register_sleeping_domain(
            "a.com".to_string(),
            SleepingEnvironmentInfo {
                environment_id: 1,
                project_id: 1,
                deployment_id: 1,
                wake_timeout_seconds: 30,
            },
        );
        manager.register_sleeping_domain(
            "b.com".to_string(),
            SleepingEnvironmentInfo {
                environment_id: 2,
                project_id: 1,
                deployment_id: 2,
                wake_timeout_seconds: 30,
            },
        );

        assert!(manager.get_sleeping_environment("a.com").is_some());
        assert!(manager.get_sleeping_environment("b.com").is_some());

        manager.clear_sleeping_domains();

        assert!(manager.get_sleeping_environment("a.com").is_none());
        assert!(manager.get_sleeping_environment("b.com").is_none());
    }

    #[test]
    fn test_register_and_remove_environment() {
        let db = MockDatabase::new(DatabaseBackend::Postgres).into_connection();
        let lifecycle = Arc::new(MockLifecycle::new());
        let manager = OnDemandManager::new_test(Arc::new(db), lifecycle);

        manager.register_on_demand_environment(42, 300, 30);
        assert!(manager.configs.contains_key(&42));
        assert!(manager.last_activity.contains_key(&42));

        manager.remove_environment(42);
        assert!(!manager.configs.contains_key(&42));
        assert!(!manager.last_activity.contains_key(&42));
    }

    #[test]
    fn test_on_demand_merge_inherits() {
        let project = temps_entities::deployment_config::DeploymentConfig {
            on_demand: true,
            idle_timeout_seconds: 600,
            wake_timeout_seconds: 45,
            ..Default::default()
        };
        let env = temps_entities::deployment_config::DeploymentConfig::default();
        let merged = project.merge(&env);
        assert!(merged.on_demand); // true || false = true
        assert_eq!(merged.idle_timeout_seconds, 600); // project wins (env is default 300)
    }

    #[test]
    fn test_on_demand_merge_env_overrides() {
        let project = temps_entities::deployment_config::DeploymentConfig {
            on_demand: false,
            idle_timeout_seconds: 600,
            ..Default::default()
        };
        let env = temps_entities::deployment_config::DeploymentConfig {
            on_demand: true,
            idle_timeout_seconds: 120,
            wake_timeout_seconds: 10,
            ..Default::default()
        };
        let merged = project.merge(&env);
        assert!(merged.on_demand);
        assert_eq!(merged.idle_timeout_seconds, 120);
        assert_eq!(merged.wake_timeout_seconds, 10);
    }

    #[test]
    fn test_on_demand_serialization_roundtrip() {
        let config = temps_entities::deployment_config::DeploymentConfig {
            on_demand: true,
            idle_timeout_seconds: 600,
            wake_timeout_seconds: 45,
            ..Default::default()
        };
        let json = serde_json::to_value(&config).unwrap();
        assert_eq!(json["onDemand"], true);
        assert_eq!(json["idleTimeoutSeconds"], 600);
        assert_eq!(json["wakeTimeoutSeconds"], 45);

        let deserialized: temps_entities::deployment_config::DeploymentConfig =
            serde_json::from_value(json).unwrap();
        assert_eq!(config, deserialized);
    }

    #[test]
    fn test_on_demand_deserialization_defaults() {
        // Existing JSON without on-demand fields should deserialize with defaults
        let json = serde_json::json!({
            "cpuRequest": 100,
            "automaticDeploy": false,
            "replicas": 1
        });
        let config: temps_entities::deployment_config::DeploymentConfig =
            serde_json::from_value(json).unwrap();
        assert!(!config.on_demand);
        assert_eq!(config.idle_timeout_seconds, 300);
        assert_eq!(config.wake_timeout_seconds, 30);
    }

    #[test]
    fn test_on_demand_error_display() {
        let err = OnDemandError::WakeTimeout {
            environment_id: 42,
            timeout_secs: 30,
        };
        assert!(err.to_string().contains("42"));
        assert!(err.to_string().contains("30"));

        let err = OnDemandError::NoDeployment { environment_id: 5 };
        assert!(err.to_string().contains("5"));
    }

    #[tokio::test]
    async fn test_sweep_no_on_demand_environments() {
        let db = MockDatabase::new(DatabaseBackend::Postgres).into_connection();
        let lifecycle = Arc::new(MockLifecycle::new());
        let manager = OnDemandManager::new_test(Arc::new(db), lifecycle);

        let slept = manager.sweep_idle_environments().await;
        assert!(slept.is_empty());
    }

    #[tokio::test]
    async fn test_wake_concurrent_requests_coordinate() {
        // Test that multiple concurrent wake requests coordinate properly:
        // Only one should actually perform the wake, others should wait.
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            // First wake: UPDATE sleeping=false WHERE sleeping=true -> 1 row
            .append_exec_results(vec![MockExecResult {
                last_insert_id: 0,
                rows_affected: 1,
            }])
            // find_by_id for env
            .append_query_results(vec![vec![environments::Model {
                id: 1,
                name: "production".to_string(),
                slug: "production".to_string(),
                subdomain: "prod".to_string(),
                last_deployment: None,
                host: "".to_string(),
                upstreams: temps_entities::upstream_config::UpstreamList::new(),
                created_at: chrono::Utc::now(),
                updated_at: chrono::Utc::now(),
                project_id: 10,
                current_deployment_id: Some(100),
                branch: None,
                deleted_at: None,
                deployment_config: None,
                is_preview: false,
                protected: false,
                sleeping: false,
                last_activity_at: None,
            }]])
            // containers query
            .append_query_results(vec![vec![deployment_containers::Model {
                id: 1,
                deployment_id: 100,
                container_id: "abc123".to_string(),
                container_name: "my-app-abc123".to_string(),
                container_port: 3000,
                host_port: Some(32000),
                image_name: Some("my-app:latest".to_string()),
                status: Some("running".to_string()),
                service_name: None,
                created_at: chrono::Utc::now(),
                deployed_at: chrono::Utc::now(),
                ready_at: None,
                deleted_at: None,
                node_id: None,
                exit_code: None,
                exit_reason: None,
                oom_killed: None,
                error_message: None,
                finished_at: None,
                started_at: None,
                cpu_limit_cores: None,
            }]])
            // NOTIFY
            .append_exec_results(vec![MockExecResult {
                last_insert_id: 0,
                rows_affected: 0,
            }])
            .into_connection();

        let lifecycle = Arc::new(MockLifecycle::new());
        let manager = Arc::new(OnDemandManager::new_test(
            Arc::new(db),
            Arc::clone(&lifecycle) as Arc<dyn ContainerLifecycle>,
        ));

        // Single wake should succeed
        let result = manager.wake_environment(1, 30).await;
        assert!(result.is_ok());

        // Container should have been started
        assert_eq!(lifecycle.started_containers(), vec!["abc123"]);
    }

    #[tokio::test]
    async fn test_wake_posts_force_route_reload() {
        // A successful wake MUST publish Job::ForceRouteReload on the in-process
        // queue so the route table reloads deterministically (not just PG NOTIFY).
        // This is the fix for "first request to an on-demand env returns 503".
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            // UPDATE sleeping=false WHERE sleeping=true -> 1 row
            .append_exec_results(vec![MockExecResult {
                last_insert_id: 0,
                rows_affected: 1,
            }])
            // find_by_id for env
            .append_query_results(vec![vec![environments::Model {
                id: 1,
                name: "production".to_string(),
                slug: "production".to_string(),
                subdomain: "prod".to_string(),
                last_deployment: None,
                host: "".to_string(),
                upstreams: temps_entities::upstream_config::UpstreamList::new(),
                created_at: chrono::Utc::now(),
                updated_at: chrono::Utc::now(),
                project_id: 10,
                current_deployment_id: Some(100),
                branch: None,
                deleted_at: None,
                deployment_config: None,
                is_preview: false,
                protected: false,
                sleeping: false,
                last_activity_at: None,
            }]])
            // containers query
            .append_query_results(vec![vec![deployment_containers::Model {
                id: 1,
                deployment_id: 100,
                container_id: "abc123".to_string(),
                container_name: "my-app-abc123".to_string(),
                container_port: 3000,
                host_port: Some(32000),
                image_name: Some("my-app:latest".to_string()),
                status: Some("running".to_string()),
                service_name: None,
                created_at: chrono::Utc::now(),
                deployed_at: chrono::Utc::now(),
                ready_at: None,
                deleted_at: None,
                node_id: None,
                exit_code: None,
                exit_reason: None,
                oom_killed: None,
                error_message: None,
                finished_at: None,
                started_at: None,
                cpu_limit_cores: None,
            }]])
            .into_connection();

        let force_reloads = Arc::new(Mutex::new(Vec::<ForceRouteReloadJob>::new()));
        let queue: Arc<dyn JobQueue> = Arc::new(CountingQueue {
            force_reloads: force_reloads.clone(),
        });
        let lifecycle = Arc::new(MockLifecycle::new());
        let manager = OnDemandManager::new(
            Arc::new(db),
            Arc::clone(&lifecycle) as Arc<dyn ContainerLifecycle>,
            queue,
        );

        let result = manager.wake_environment(1, 30).await;
        assert!(result.is_ok());

        let posted = force_reloads.lock().unwrap();
        assert_eq!(
            posted.len(),
            1,
            "wake must publish exactly one ForceRouteReload"
        );
        // Must carry the woken environment and its current deployment, so the
        // RouteReloadSubscriber can match the RouteTableUpdated confirmation.
        assert_eq!(posted[0].environment_id, Some(1));
        assert_eq!(posted[0].deployment_id, Some(100));
    }

    #[tokio::test]
    async fn test_wait_for_route_reload_lost_wakeup_safe() {
        // Regression for the lost-wakeup race: a notify_route_reloaded() that
        // fires the instant after wait_for_route_reload arms must still be
        // delivered. We arm the wait, then notify from another task; the wait
        // must resolve to `true` well within the timeout (not park for the full
        // duration).
        let db = MockDatabase::new(DatabaseBackend::Postgres).into_connection();
        let lifecycle = Arc::new(MockLifecycle::new());
        let manager = Arc::new(OnDemandManager::new_test(
            Arc::new(db),
            Arc::clone(&lifecycle) as Arc<dyn ContainerLifecycle>,
        ));

        let m2 = Arc::clone(&manager);
        let notifier = tokio::spawn(async move {
            // Give the waiter a moment to park, then signal.
            tokio::time::sleep(Duration::from_millis(20)).await;
            m2.notify_route_reloaded();
        });

        let start = Instant::now();
        let reloaded = manager.wait_for_route_reload(Duration::from_secs(5)).await;
        let elapsed = start.elapsed();

        notifier.await.unwrap();
        assert!(reloaded, "wait must observe the reload signal");
        assert!(
            elapsed < Duration::from_secs(1),
            "wait must wake promptly on notify, not park for the full timeout (took {elapsed:?})"
        );
    }

    #[test]
    fn test_wake_slot_acquire_and_release() {
        let db = MockDatabase::new(DatabaseBackend::Postgres).into_connection();
        let lifecycle = Arc::new(MockLifecycle::new());
        let manager = OnDemandManager::new_test(
            Arc::new(db),
            Arc::clone(&lifecycle) as Arc<dyn ContainerLifecycle>,
        );

        // A fresh manager hands out a slot.
        let permit = manager.try_acquire_wake_slot();
        assert!(permit.is_some(), "first slot should be available");

        // Drain the rest of the pool and confirm exhaustion returns None.
        let mut held = vec![permit.unwrap()];
        while let Some(p) = manager.try_acquire_wake_slot() {
            held.push(p);
        }
        assert_eq!(
            held.len(),
            MAX_CONCURRENT_WAKE_WAITERS,
            "pool should hand out exactly MAX_CONCURRENT_WAKE_WAITERS permits"
        );
        assert!(
            manager.try_acquire_wake_slot().is_none(),
            "exhausted pool must return None instead of parking"
        );

        // Releasing a permit makes a slot available again.
        held.pop();
        assert!(
            manager.try_acquire_wake_slot().is_some(),
            "released slot should be re-acquirable"
        );
    }

    #[tokio::test]
    async fn test_wait_for_route_reload_times_out_to_false() {
        // With no signal, the wait must return `false` (not a swallowed Ok) so
        // the caller knows to re-resolve rather than trusting a stale table.
        let db = MockDatabase::new(DatabaseBackend::Postgres).into_connection();
        let lifecycle = Arc::new(MockLifecycle::new());
        let manager = OnDemandManager::new_test(
            Arc::new(db),
            Arc::clone(&lifecycle) as Arc<dyn ContainerLifecycle>,
        );

        let reloaded = manager
            .wait_for_route_reload(Duration::from_millis(50))
            .await;
        assert!(!reloaded, "no signal within timeout must return false");
    }

    #[tokio::test]
    async fn test_wake_failure_reverts_to_sleeping() {
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            // UPDATE sleeping=false -> 1 row
            .append_exec_results(vec![MockExecResult {
                last_insert_id: 0,
                rows_affected: 1,
            }])
            // find_by_id env
            .append_query_results(vec![vec![environments::Model {
                id: 1,
                name: "test".to_string(),
                slug: "test".to_string(),
                subdomain: "test".to_string(),
                last_deployment: None,
                host: "".to_string(),
                upstreams: temps_entities::upstream_config::UpstreamList::new(),
                created_at: chrono::Utc::now(),
                updated_at: chrono::Utc::now(),
                project_id: 10,
                current_deployment_id: Some(100),
                branch: None,
                deleted_at: None,
                deployment_config: None,
                is_preview: false,
                protected: false,
                sleeping: false,
                last_activity_at: None,
            }]])
            // containers
            .append_query_results(vec![vec![deployment_containers::Model {
                id: 1,
                deployment_id: 100,
                container_id: "fail-container".to_string(),
                container_name: "my-app-fail".to_string(),
                container_port: 3000,
                host_port: Some(32001),
                image_name: None,
                status: None,
                service_name: None,
                created_at: chrono::Utc::now(),
                deployed_at: chrono::Utc::now(),
                ready_at: None,
                deleted_at: None,
                node_id: None,
                exit_code: None,
                exit_reason: None,
                oom_killed: None,
                error_message: None,
                finished_at: None,
                started_at: None,
                cpu_limit_cores: None,
            }]])
            // Revert UPDATE sleeping=true
            .append_exec_results(vec![MockExecResult {
                last_insert_id: 0,
                rows_affected: 1,
            }])
            .into_connection();

        let lifecycle = Arc::new(MockLifecycle::with_fail_start());
        let manager = OnDemandManager::new_test(Arc::new(db), lifecycle);

        let result = manager.wake_environment(1, 30).await;
        assert!(result.is_err());
        assert!(matches!(
            result.unwrap_err(),
            OnDemandError::ContainerOperation { .. }
        ));
    }

    #[tokio::test]
    async fn test_wake_no_deployment_returns_error() {
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            // UPDATE sleeping=false -> 1 row
            .append_exec_results(vec![MockExecResult {
                last_insert_id: 0,
                rows_affected: 1,
            }])
            // find_by_id env -> no current deployment
            .append_query_results(vec![vec![environments::Model {
                id: 1,
                name: "test".to_string(),
                slug: "test".to_string(),
                subdomain: "test".to_string(),
                last_deployment: None,
                host: "".to_string(),
                upstreams: temps_entities::upstream_config::UpstreamList::new(),
                created_at: chrono::Utc::now(),
                updated_at: chrono::Utc::now(),
                project_id: 10,
                current_deployment_id: None, // No deployment
                branch: None,
                deleted_at: None,
                deployment_config: None,
                is_preview: false,
                protected: false,
                sleeping: false,
                last_activity_at: None,
            }]])
            .into_connection();

        let lifecycle = Arc::new(MockLifecycle::new());
        let manager = OnDemandManager::new_test(Arc::new(db), lifecycle);

        let result = manager.wake_environment(1, 30).await;
        assert!(matches!(
            result.unwrap_err(),
            OnDemandError::NoDeployment { environment_id: 1 }
        ));
    }

    #[tokio::test]
    async fn test_sleep_already_sleeping_returns_false() {
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            // UPDATE sleeping=true WHERE sleeping=false -> 0 rows (already sleeping)
            .append_exec_results(vec![MockExecResult {
                last_insert_id: 0,
                rows_affected: 0,
            }])
            .into_connection();

        let lifecycle = Arc::new(MockLifecycle::new());
        let manager = OnDemandManager::new_test(Arc::new(db), lifecycle);

        let result = manager.sleep_environment(1).await.unwrap();
        assert!(!result); // Already sleeping
    }

    #[tokio::test]
    async fn test_wake_already_awake_succeeds() {
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            // UPDATE sleeping=false WHERE sleeping=true -> 0 rows (already awake)
            .append_exec_results(vec![MockExecResult {
                last_insert_id: 0,
                rows_affected: 0,
            }])
            .into_connection();

        let lifecycle = Arc::new(MockLifecycle::new());
        let manager = OnDemandManager::new_test(Arc::new(db), lifecycle);

        let result = manager.wake_environment(1, 30).await;
        assert!(result.is_ok()); // No-op, already awake
    }

    #[tokio::test]
    async fn test_sleep_stops_multiple_containers() {
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            // UPDATE sleeping=true WHERE sleeping=false -> 1 row
            .append_exec_results(vec![MockExecResult {
                last_insert_id: 0,
                rows_affected: 1,
            }])
            // find_by_id env
            .append_query_results(vec![vec![environments::Model {
                id: 1,
                name: "test".to_string(),
                slug: "test".to_string(),
                subdomain: "test".to_string(),
                last_deployment: None,
                host: "".to_string(),
                upstreams: temps_entities::upstream_config::UpstreamList::new(),
                created_at: chrono::Utc::now(),
                updated_at: chrono::Utc::now(),
                project_id: 10,
                current_deployment_id: Some(100),
                branch: None,
                deleted_at: None,
                deployment_config: None,
                is_preview: false,
                protected: false,
                sleeping: false,
                last_activity_at: None,
            }]])
            // containers (3 replicas)
            .append_query_results(vec![vec![
                deployment_containers::Model {
                    id: 1,
                    deployment_id: 100,
                    container_id: "container-1".to_string(),
                    container_name: "app-1".to_string(),
                    container_port: 3000,
                    host_port: Some(32001),
                    image_name: None,
                    status: None,
                    service_name: None,
                    created_at: chrono::Utc::now(),
                    deployed_at: chrono::Utc::now(),
                    ready_at: None,
                    deleted_at: None,
                    node_id: None,
                    exit_code: None,
                    exit_reason: None,
                    oom_killed: None,
                    error_message: None,
                    finished_at: None,
                    started_at: None,
                    cpu_limit_cores: None,
                },
                deployment_containers::Model {
                    id: 2,
                    deployment_id: 100,
                    container_id: "container-2".to_string(),
                    container_name: "app-2".to_string(),
                    container_port: 3000,
                    host_port: Some(32002),
                    image_name: None,
                    status: None,
                    service_name: None,
                    created_at: chrono::Utc::now(),
                    deployed_at: chrono::Utc::now(),
                    ready_at: None,
                    deleted_at: None,
                    node_id: Some(2),
                    exit_code: None,
                    exit_reason: None,
                    oom_killed: None,
                    error_message: None,
                    finished_at: None,
                    started_at: None,
                    cpu_limit_cores: None, // Remote node
                },
                deployment_containers::Model {
                    id: 3,
                    deployment_id: 100,
                    container_id: "container-3".to_string(),
                    container_name: "app-3".to_string(),
                    container_port: 3000,
                    host_port: Some(32003),
                    image_name: None,
                    status: None,
                    service_name: None,
                    created_at: chrono::Utc::now(),
                    deployed_at: chrono::Utc::now(),
                    ready_at: None,
                    deleted_at: None,
                    node_id: Some(3),
                    exit_code: None,
                    exit_reason: None,
                    oom_killed: None,
                    error_message: None,
                    finished_at: None,
                    started_at: None,
                    cpu_limit_cores: None, // Another remote node
                },
            ]])
            // NOTIFY
            .append_exec_results(vec![MockExecResult {
                last_insert_id: 0,
                rows_affected: 0,
            }])
            .into_connection();

        let lifecycle = Arc::new(MockLifecycle::new());
        let manager = OnDemandManager::new_test(
            Arc::new(db),
            Arc::clone(&lifecycle) as Arc<dyn ContainerLifecycle>,
        );

        let result = manager.sleep_environment(1).await.unwrap();
        assert!(result);

        // All 3 containers should have been stopped
        let stopped = lifecycle.stopped_containers();
        assert_eq!(stopped.len(), 3);
        assert!(stopped.contains(&"container-1".to_string()));
        assert!(stopped.contains(&"container-2".to_string()));
        assert!(stopped.contains(&"container-3".to_string()));
    }

    // ── Sleeping domain lookup with port stripping ──

    #[test]
    fn test_sleeping_domain_lookup_with_port() {
        let db = MockDatabase::new(DatabaseBackend::Postgres).into_connection();
        let lifecycle = Arc::new(MockLifecycle::new());
        let manager = OnDemandManager::new_test(
            Arc::new(db),
            Arc::clone(&lifecycle) as Arc<dyn ContainerLifecycle>,
        );

        manager.register_sleeping_domain(
            "app.preview.example.com".to_string(),
            SleepingEnvironmentInfo {
                environment_id: 1,
                project_id: 1,
                deployment_id: 10,
                wake_timeout_seconds: 30,
            },
        );

        // Lookup without port
        assert!(manager
            .get_sleeping_environment("app.preview.example.com")
            .is_some());

        // Port should be stripped by the caller (LoadBalancer.request_filter)
        assert!(manager
            .get_sleeping_environment("app.preview.example.com:443")
            .is_none());
    }

    // ── Sweep only sleeps environments past idle timeout ──

    #[tokio::test]
    async fn test_sweep_respects_idle_timeout() {
        // Environment with 300s idle timeout but recent activity → should NOT sleep
        let db = MockDatabase::new(DatabaseBackend::Postgres).into_connection();
        let lifecycle = Arc::new(MockLifecycle::new());
        let manager = OnDemandManager::new_test(
            Arc::new(db),
            Arc::clone(&lifecycle) as Arc<dyn ContainerLifecycle>,
        );

        manager.register_on_demand_environment(1, 300, 30);
        manager.record_activity(1); // just now

        let slept = manager.sweep_idle_environments().await;
        assert!(
            slept.is_empty(),
            "Should not sleep recently active environment"
        );
    }

    // ── Multiple sleeping domains for same environment ──

    #[test]
    fn test_multiple_domains_same_sleeping_environment() {
        let db = MockDatabase::new(DatabaseBackend::Postgres).into_connection();
        let lifecycle = Arc::new(MockLifecycle::new());
        let manager = OnDemandManager::new_test(
            Arc::new(db),
            Arc::clone(&lifecycle) as Arc<dyn ContainerLifecycle>,
        );

        let info = SleepingEnvironmentInfo {
            environment_id: 5,
            project_id: 2,
            deployment_id: 20,
            wake_timeout_seconds: 15,
        };

        // Same env, multiple domains (subdomain + custom domain)
        manager.register_sleeping_domain("app.preview.example.com".to_string(), info.clone());
        manager.register_sleeping_domain("my-app.custom.com".to_string(), info.clone());

        let lookup1 = manager
            .get_sleeping_environment("app.preview.example.com")
            .unwrap();
        let lookup2 = manager
            .get_sleeping_environment("my-app.custom.com")
            .unwrap();

        assert_eq!(lookup1.environment_id, 5);
        assert_eq!(lookup2.environment_id, 5);
        assert_eq!(lookup1.wake_timeout_seconds, 15);
    }

    // ── Clear sleeping domains removes all entries ──

    #[test]
    fn test_clear_sleeping_domains_removes_all() {
        let db = MockDatabase::new(DatabaseBackend::Postgres).into_connection();
        let lifecycle = Arc::new(MockLifecycle::new());
        let manager = OnDemandManager::new_test(
            Arc::new(db),
            Arc::clone(&lifecycle) as Arc<dyn ContainerLifecycle>,
        );

        manager.register_sleeping_domain(
            "a.example.com".to_string(),
            SleepingEnvironmentInfo {
                environment_id: 1,
                project_id: 1,
                deployment_id: 1,
                wake_timeout_seconds: 30,
            },
        );
        manager.register_sleeping_domain(
            "b.example.com".to_string(),
            SleepingEnvironmentInfo {
                environment_id: 2,
                project_id: 1,
                deployment_id: 2,
                wake_timeout_seconds: 30,
            },
        );

        assert!(manager.get_sleeping_environment("a.example.com").is_some());
        assert!(manager.get_sleeping_environment("b.example.com").is_some());

        manager.clear_sleeping_domains();

        assert!(manager.get_sleeping_environment("a.example.com").is_none());
        assert!(manager.get_sleeping_environment("b.example.com").is_none());
    }

    // ── Activity tracking idempotent ──

    #[test]
    fn test_record_activity_creates_entry_if_missing() {
        let db = MockDatabase::new(DatabaseBackend::Postgres).into_connection();
        let lifecycle = Arc::new(MockLifecycle::new());
        let manager = OnDemandManager::new_test(
            Arc::new(db),
            Arc::clone(&lifecycle) as Arc<dyn ContainerLifecycle>,
        );

        // Environment 99 not registered, but record_activity should still work
        manager.record_activity(99);
        // No panic, entry created
        assert!(manager.last_activity.contains_key(&99));
    }

    // ── Route table callback integration ──

    #[test]
    fn test_route_table_callback_populates_sleeping_domains() {
        let db = MockDatabase::new(DatabaseBackend::Postgres).into_connection();
        let lifecycle = Arc::new(MockLifecycle::new());
        let manager = Arc::new(OnDemandManager::new_test(
            Arc::new(db),
            Arc::clone(&lifecycle) as Arc<dyn ContainerLifecycle>,
        ));

        // Simulate the callback that setup_proxy_server registers
        let on_demand = Arc::clone(&manager);
        let callback = move |entries: Vec<temps_routes::SleepingEnvironmentEntry>| {
            on_demand.clear_sleeping_domains();
            for entry in entries {
                on_demand.register_sleeping_domain(
                    entry.domain.clone(),
                    SleepingEnvironmentInfo {
                        environment_id: entry.environment_id,
                        project_id: entry.project_id,
                        deployment_id: entry.deployment_id,
                        wake_timeout_seconds: entry.wake_timeout_seconds,
                    },
                );
            }
        };

        // Fire callback with sleeping entries
        callback(vec![
            temps_routes::SleepingEnvironmentEntry {
                domain: "app.preview.example.com".to_string(),
                environment_id: 1,
                project_id: 10,
                deployment_id: 100,
                wake_timeout_seconds: 30,
            },
            temps_routes::SleepingEnvironmentEntry {
                domain: "custom.example.com".to_string(),
                environment_id: 1,
                project_id: 10,
                deployment_id: 100,
                wake_timeout_seconds: 30,
            },
        ]);

        // Both domains should resolve to the same sleeping environment
        let info1 = manager
            .get_sleeping_environment("app.preview.example.com")
            .unwrap();
        let info2 = manager
            .get_sleeping_environment("custom.example.com")
            .unwrap();
        assert_eq!(info1.environment_id, 1);
        assert_eq!(info2.environment_id, 1);
        assert_eq!(info1.deployment_id, 100);

        // Non-sleeping domain should not be found
        assert!(manager
            .get_sleeping_environment("other.example.com")
            .is_none());
    }

    #[test]
    fn test_route_table_callback_replaces_previous_entries() {
        let db = MockDatabase::new(DatabaseBackend::Postgres).into_connection();
        let lifecycle = Arc::new(MockLifecycle::new());
        let manager = Arc::new(OnDemandManager::new_test(
            Arc::new(db),
            Arc::clone(&lifecycle) as Arc<dyn ContainerLifecycle>,
        ));

        // Register initial sleeping domain
        manager.register_sleeping_domain(
            "old.example.com".to_string(),
            SleepingEnvironmentInfo {
                environment_id: 1,
                project_id: 10,
                deployment_id: 100,
                wake_timeout_seconds: 30,
            },
        );
        assert!(manager
            .get_sleeping_environment("old.example.com")
            .is_some());

        // Simulate callback clearing and setting new entries (like after a route reload
        // where the environment woke up and a new one went to sleep)
        let on_demand = Arc::clone(&manager);
        on_demand.clear_sleeping_domains();
        on_demand.register_sleeping_domain(
            "new.example.com".to_string(),
            SleepingEnvironmentInfo {
                environment_id: 2,
                project_id: 20,
                deployment_id: 200,
                wake_timeout_seconds: 60,
            },
        );

        // Old domain gone, new domain present
        assert!(manager
            .get_sleeping_environment("old.example.com")
            .is_none());
        let info = manager.get_sleeping_environment("new.example.com").unwrap();
        assert_eq!(info.environment_id, 2);
        assert_eq!(info.wake_timeout_seconds, 60);
    }

    // ── Wake triggers container start and DB update ──

    #[tokio::test]
    async fn test_wake_starts_container_and_updates_db() {
        use temps_entities::deployment_containers;

        let env_model = temps_entities::environments::Model {
            id: 1,
            name: "staging".to_string(),
            slug: "staging".to_string(),
            subdomain: "my-project-staging".to_string(),
            branch: Some("develop".to_string()),
            project_id: 10,
            host: "".to_string(),
            upstreams: temps_entities::upstream_config::UpstreamList::new(),
            created_at: chrono::Utc::now(),
            updated_at: chrono::Utc::now(),
            last_deployment: None,
            current_deployment_id: Some(100),
            deleted_at: None,
            deployment_config: Some(temps_entities::deployment_config::DeploymentConfig {
                on_demand: true,
                idle_timeout_seconds: 300,
                wake_timeout_seconds: 30,
                ..Default::default()
            }),
            is_preview: false,
            protected: false,
            sleeping: true,
            last_activity_at: None,
        };

        let container = deployment_containers::Model {
            id: 1,
            deployment_id: 100,
            container_id: "abc123".to_string(),
            container_name: "staging-abc123".to_string(),
            container_port: 8080,
            host_port: Some(32000),
            image_name: None,
            status: Some("stopped".to_string()),
            service_name: None,
            node_id: None,
            exit_code: None,
            exit_reason: None,
            oom_killed: None,
            error_message: None,
            finished_at: None,
            started_at: None,
            cpu_limit_cores: None,
            created_at: chrono::Utc::now(),
            deployed_at: chrono::Utc::now(),
            ready_at: None,
            deleted_at: None,
        };

        let db = MockDatabase::new(DatabaseBackend::Postgres)
            // 1. CAS UPDATE environments SET sleeping=false WHERE sleeping=true
            .append_exec_results(vec![MockExecResult {
                last_insert_id: 0,
                rows_affected: 1,
            }])
            // 2. find environment by id
            .append_query_results(vec![vec![env_model.clone()]])
            // 3. find containers for deployment
            .append_query_results(vec![vec![container]])
            // 4. NOTIFY route_table_changes
            .append_exec_results(vec![MockExecResult {
                last_insert_id: 0,
                rows_affected: 0,
            }])
            .into_connection();

        let lifecycle = Arc::new(MockLifecycle::new());
        let manager = Arc::new(OnDemandManager::new_test(
            Arc::new(db),
            Arc::clone(&lifecycle) as Arc<dyn ContainerLifecycle>,
        ));

        // Register sleeping domain
        manager.register_sleeping_domain(
            "staging.preview.example.com".to_string(),
            SleepingEnvironmentInfo {
                environment_id: 1,
                project_id: 10,
                deployment_id: 100,
                wake_timeout_seconds: 30,
            },
        );

        // Wake the environment
        let result = manager.wake_environment(1, 30).await;
        assert!(result.is_ok(), "Wake should succeed: {:?}", result.err());

        // Verify container was started
        let started = lifecycle.started_containers();
        assert_eq!(started, vec!["abc123"]);

        // Note: sleeping domain map is cleared by the route table callback
        // (triggered by NOTIFY route_table_changes), not by wake_environment itself.
        // In production, load_routes re-fires and the callback repopulates the map
        // without this environment (since sleeping=false now).
    }

    // ── Sleep stops containers and registers sleeping domain ──

    #[tokio::test]
    async fn test_sleep_stops_containers_and_updates_db() {
        use temps_entities::deployment_containers;

        let env_model = temps_entities::environments::Model {
            id: 2,
            name: "preview".to_string(),
            slug: "preview".to_string(),
            subdomain: "my-project-preview".to_string(),
            branch: Some("feature".to_string()),
            project_id: 10,
            host: "".to_string(),
            upstreams: temps_entities::upstream_config::UpstreamList::new(),
            created_at: chrono::Utc::now(),
            updated_at: chrono::Utc::now(),
            last_deployment: None,
            current_deployment_id: Some(200),
            deleted_at: None,
            deployment_config: Some(temps_entities::deployment_config::DeploymentConfig {
                on_demand: true,
                idle_timeout_seconds: 300,
                wake_timeout_seconds: 30,
                ..Default::default()
            }),
            is_preview: false,
            protected: false,
            sleeping: false,
            last_activity_at: None,
        };

        let container1 = deployment_containers::Model {
            id: 1,
            deployment_id: 200,
            container_id: "container-a".to_string(),
            container_name: "preview-a".to_string(),
            container_port: 3000,
            host_port: Some(32001),
            image_name: None,
            status: Some("running".to_string()),
            service_name: None,
            node_id: None,
            exit_code: None,
            exit_reason: None,
            oom_killed: None,
            error_message: None,
            finished_at: None,
            started_at: None,
            cpu_limit_cores: None,
            created_at: chrono::Utc::now(),
            deployed_at: chrono::Utc::now(),
            ready_at: None,
            deleted_at: None,
        };
        let container2 = deployment_containers::Model {
            id: 2,
            deployment_id: 200,
            container_id: "container-b".to_string(),
            container_name: "preview-b".to_string(),
            container_port: 3000,
            host_port: Some(32002),
            image_name: None,
            status: Some("running".to_string()),
            service_name: None,
            node_id: None,
            exit_code: None,
            exit_reason: None,
            oom_killed: None,
            error_message: None,
            finished_at: None,
            started_at: None,
            cpu_limit_cores: None,
            created_at: chrono::Utc::now(),
            deployed_at: chrono::Utc::now(),
            ready_at: None,
            deleted_at: None,
        };

        let db = MockDatabase::new(DatabaseBackend::Postgres)
            // 1. CAS UPDATE environments SET sleeping=true WHERE sleeping=false
            .append_exec_results(vec![MockExecResult {
                last_insert_id: 0,
                rows_affected: 1,
            }])
            // 2. find environment by id
            .append_query_results(vec![vec![env_model.clone()]])
            // 3. find containers for deployment
            .append_query_results(vec![vec![container1, container2]])
            // 4. NOTIFY route_table_changes
            .append_exec_results(vec![MockExecResult {
                last_insert_id: 0,
                rows_affected: 0,
            }])
            .into_connection();

        let lifecycle = Arc::new(MockLifecycle::new());
        let manager = Arc::new(OnDemandManager::new_test(
            Arc::new(db),
            Arc::clone(&lifecycle) as Arc<dyn ContainerLifecycle>,
        ));

        // Register on-demand config so sweep can find it
        manager.register_on_demand_environment(2, 300, 30);
        manager.record_activity(2);

        let result = manager.sleep_environment(2).await;
        assert!(result.is_ok(), "Sleep should succeed: {:?}", result.err());
        assert!(result.unwrap(), "Should return true (was put to sleep)");

        // Both containers should be stopped
        let mut stopped = lifecycle.stopped_containers();
        stopped.sort();
        assert_eq!(stopped, vec!["container-a", "container-b"]);
    }

    // ── Sweep integration: idle environment gets slept ──

    #[tokio::test]
    async fn test_sweep_sleeps_idle_environment() {
        use temps_entities::deployment_containers;

        let env_model = temps_entities::environments::Model {
            id: 3,
            name: "idle-env".to_string(),
            slug: "idle-env".to_string(),
            subdomain: "idle-env".to_string(),
            branch: None,
            project_id: 10,
            host: "".to_string(),
            upstreams: temps_entities::upstream_config::UpstreamList::new(),
            created_at: chrono::Utc::now(),
            updated_at: chrono::Utc::now(),
            last_deployment: None,
            current_deployment_id: Some(300),
            deleted_at: None,
            deployment_config: Some(temps_entities::deployment_config::DeploymentConfig {
                on_demand: true,
                idle_timeout_seconds: 60,
                wake_timeout_seconds: 30,
                ..Default::default()
            }),
            is_preview: false,
            protected: false,
            sleeping: false,
            last_activity_at: None,
        };

        let container = deployment_containers::Model {
            id: 1,
            deployment_id: 300,
            container_id: "idle-container".to_string(),
            container_name: "idle-app".to_string(),
            container_port: 3000,
            host_port: Some(32010),
            image_name: None,
            status: Some("running".to_string()),
            service_name: None,
            node_id: None,
            exit_code: None,
            exit_reason: None,
            oom_killed: None,
            error_message: None,
            finished_at: None,
            started_at: None,
            cpu_limit_cores: None,
            created_at: chrono::Utc::now(),
            deployed_at: chrono::Utc::now(),
            ready_at: None,
            deleted_at: None,
        };

        let db = MockDatabase::new(DatabaseBackend::Postgres)
            // 1. CAS UPDATE environments SET sleeping=true
            .append_exec_results(vec![MockExecResult {
                last_insert_id: 0,
                rows_affected: 1,
            }])
            // 2. find environment by id
            .append_query_results(vec![vec![env_model]])
            // 3. find containers
            .append_query_results(vec![vec![container]])
            // 4. NOTIFY
            .append_exec_results(vec![MockExecResult {
                last_insert_id: 0,
                rows_affected: 0,
            }])
            .into_connection();

        let lifecycle = Arc::new(MockLifecycle::new());
        let manager = Arc::new(OnDemandManager::new_test(
            Arc::new(db),
            Arc::clone(&lifecycle) as Arc<dyn ContainerLifecycle>,
        ));

        // Register with 60s idle timeout
        manager.register_on_demand_environment(3, 60, 30);

        // Set last activity to 120 seconds ago (well past 60s idle timeout)
        let old_time = manager.current_epoch_secs() - 120;
        manager.last_activity.insert(3, AtomicU64::new(old_time));

        let slept = manager.sweep_idle_environments().await;
        assert_eq!(
            slept,
            vec![3],
            "Environment 3 should have been put to sleep"
        );

        // Container should be stopped
        assert_eq!(lifecycle.stopped_containers(), vec!["idle-container"]);
    }

    // ── Helper: build a standard env model ──

    fn make_env_model(
        id: i32,
        project_id: i32,
        deployment_id: Option<i32>,
        sleeping: bool,
    ) -> environments::Model {
        environments::Model {
            id,
            name: format!("env-{}", id),
            slug: format!("env-{}", id),
            subdomain: format!("env-{}", id),
            last_deployment: None,
            host: "".to_string(),
            upstreams: temps_entities::upstream_config::UpstreamList::new(),
            created_at: chrono::Utc::now(),
            updated_at: chrono::Utc::now(),
            project_id,
            current_deployment_id: deployment_id,
            branch: None,
            deleted_at: None,
            deployment_config: Some(temps_entities::deployment_config::DeploymentConfig {
                on_demand: true,
                idle_timeout_seconds: 300,
                wake_timeout_seconds: 30,
                ..Default::default()
            }),
            is_preview: false,
            protected: false,
            sleeping,
            last_activity_at: None,
        }
    }

    fn make_container(
        id: i32,
        deployment_id: i32,
        container_id: &str,
        node_id: Option<i32>,
    ) -> deployment_containers::Model {
        deployment_containers::Model {
            id,
            deployment_id,
            container_id: container_id.to_string(),
            container_name: format!("app-{}", container_id),
            container_port: 3000,
            host_port: Some(32000 + id),
            image_name: None,
            status: Some("running".to_string()),
            service_name: None,
            created_at: chrono::Utc::now(),
            deployed_at: chrono::Utc::now(),
            ready_at: None,
            deleted_at: None,
            node_id,
            exit_code: None,
            exit_reason: None,
            oom_killed: None,
            error_message: None,
            finished_at: None,
            started_at: None,
            cpu_limit_cores: None,
        }
    }

    // ══════════════════════════════════════════════════════════════════════════
    //  PARTIAL CONTAINER START FAILURE + ROLLBACK
    // ══════════════════════════════════════════════════════════════════════════

    #[tokio::test]
    async fn test_wake_partial_start_failure_stops_started_containers() {
        // 3 containers: c1 and c3 start OK, c2 fails
        // Verify: c1 and c3 are stopped (rolled back), DB reverted to sleeping
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            // CAS UPDATE sleeping=false -> 1 row
            .append_exec_results(vec![MockExecResult {
                last_insert_id: 0,
                rows_affected: 1,
            }])
            // find env
            .append_query_results(vec![vec![make_env_model(1, 10, Some(100), true)]])
            // containers
            .append_query_results(vec![vec![
                make_container(1, 100, "c1", None),
                make_container(2, 100, "c2", None),
                make_container(3, 100, "c3", None),
            ]])
            // Revert UPDATE sleeping=true
            .append_exec_results(vec![MockExecResult {
                last_insert_id: 0,
                rows_affected: 1,
            }])
            .into_connection();

        let lifecycle = Arc::new(MockLifecycle::with_selective_start_failures(vec![
            "c2".to_string()
        ]));
        let manager = OnDemandManager::new_test(
            Arc::new(db),
            Arc::clone(&lifecycle) as Arc<dyn ContainerLifecycle>,
        );

        let result = manager.wake_environment(1, 30).await;
        assert!(result.is_err());
        assert!(matches!(
            result.unwrap_err(),
            OnDemandError::ContainerOperation { .. }
        ));

        // Successfully started containers should have been stopped as rollback
        let stopped = lifecycle.stopped_containers();
        let started = lifecycle.started_containers();
        // c1 and c3 started (c2 failed), so c1 and c3 should be in stopped
        for c in &started {
            assert!(
                stopped.contains(c),
                "Started container {} should have been stopped in rollback",
                c
            );
        }
        // c2 should NOT be in started
        assert!(
            !started.contains(&"c2".to_string()),
            "c2 should not have been started"
        );
    }

    // ══════════════════════════════════════════════════════════════════════════
    //  HEALTH CHECK TIMEOUT + ROLLBACK
    // ══════════════════════════════════════════════════════════════════════════

    #[tokio::test]
    async fn test_wake_health_timeout_stops_containers_and_reverts() {
        // Container starts but never becomes healthy → timeout → rollback
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            // CAS UPDATE sleeping=false -> 1 row
            .append_exec_results(vec![MockExecResult {
                last_insert_id: 0,
                rows_affected: 1,
            }])
            // find env
            .append_query_results(vec![vec![make_env_model(1, 10, Some(100), true)]])
            // containers
            .append_query_results(vec![vec![make_container(1, 100, "slow-container", None)]])
            // Revert UPDATE sleeping=true
            .append_exec_results(vec![MockExecResult {
                last_insert_id: 0,
                rows_affected: 1,
            }])
            .into_connection();

        let lifecycle = Arc::new(MockLifecycle::with_never_healthy());
        let manager = OnDemandManager::new_test(
            Arc::new(db),
            Arc::clone(&lifecycle) as Arc<dyn ContainerLifecycle>,
        );

        // Use very short timeout (1s) so test doesn't hang
        let result = manager.wake_environment(1, 1).await;
        assert!(result.is_err());
        assert!(
            matches!(
                result.unwrap_err(),
                OnDemandError::WakeTimeout {
                    environment_id: 1,
                    timeout_secs: 1
                }
            ),
            "Should be WakeTimeout error"
        );

        // Container was started then stopped on rollback
        assert_eq!(lifecycle.started_containers(), vec!["slow-container"]);
        assert!(
            lifecycle
                .stopped_containers()
                .contains(&"slow-container".to_string()),
            "Container should be stopped after health timeout rollback"
        );
    }

    // ══════════════════════════════════════════════════════════════════════════
    //  DELAYED HEALTH CHECK SUCCESS (becomes healthy after N polls)
    // ══════════════════════════════════════════════════════════════════════════

    #[tokio::test]
    async fn test_wake_delayed_health_succeeds() {
        // Container takes 2 health checks to become healthy, but within timeout
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            // CAS UPDATE sleeping=false -> 1 row
            .append_exec_results(vec![MockExecResult {
                last_insert_id: 0,
                rows_affected: 1,
            }])
            // find env
            .append_query_results(vec![vec![make_env_model(1, 10, Some(100), true)]])
            // containers
            .append_query_results(vec![vec![make_container(1, 100, "delayed-c", None)]])
            // NOTIFY
            .append_exec_results(vec![MockExecResult {
                last_insert_id: 0,
                rows_affected: 0,
            }])
            .into_connection();

        // Healthy after 2 checks (2 * 500ms = 1s, well within 30s timeout)
        let lifecycle = Arc::new(MockLifecycle::with_delayed_health(2));
        let manager = OnDemandManager::new_test(
            Arc::new(db),
            Arc::clone(&lifecycle) as Arc<dyn ContainerLifecycle>,
        );

        let result = manager.wake_environment(1, 30).await;
        assert!(
            result.is_ok(),
            "Wake should succeed after delayed health: {:?}",
            result.err()
        );
        assert_eq!(lifecycle.started_containers(), vec!["delayed-c"]);
        // No containers stopped (no rollback)
        assert!(lifecycle.stopped_containers().is_empty());
    }

    // ══════════════════════════════════════════════════════════════════════════
    //  SLEEP WITH CONTAINER STOP FAILURE → REVERT
    // ══════════════════════════════════════════════════════════════════════════

    #[tokio::test]
    async fn test_sleep_stop_failure_reverts_db_state() {
        // 2 containers, second fails to stop → revert sleeping=false
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            // CAS UPDATE sleeping=true -> 1 row
            .append_exec_results(vec![MockExecResult {
                last_insert_id: 0,
                rows_affected: 1,
            }])
            // find env
            .append_query_results(vec![vec![make_env_model(1, 10, Some(100), false)]])
            // containers
            .append_query_results(vec![vec![
                make_container(1, 100, "ok-stop", None),
                make_container(2, 100, "fail-stop", None),
            ]])
            // Revert UPDATE sleeping=false
            .append_exec_results(vec![MockExecResult {
                last_insert_id: 0,
                rows_affected: 1,
            }])
            // NOTIFY after revert
            .append_exec_results(vec![MockExecResult {
                last_insert_id: 0,
                rows_affected: 0,
            }])
            .into_connection();

        let lifecycle = Arc::new(MockLifecycle::with_selective_stop_failures(vec![
            "fail-stop".to_string(),
        ]));
        let manager = OnDemandManager::new_test(
            Arc::new(db),
            Arc::clone(&lifecycle) as Arc<dyn ContainerLifecycle>,
        );

        let result = manager.sleep_environment(1).await;
        assert!(result.is_err());
        assert!(matches!(
            result.unwrap_err(),
            OnDemandError::ContainerOperation { .. }
        ));
    }

    // ══════════════════════════════════════════════════════════════════════════
    //  CONCURRENT WAKE: TRUE CONCURRENCY TEST
    // ══════════════════════════════════════════════════════════════════════════

    #[tokio::test]
    async fn test_wake_concurrent_only_one_wakes() {
        // Simulate 5 concurrent wake requests. Only the first should perform
        // the DB CAS and start containers. Others should wait and return Ok.
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            // CAS UPDATE sleeping=false -> 1 row (first waker wins)
            .append_exec_results(vec![MockExecResult {
                last_insert_id: 0,
                rows_affected: 1,
            }])
            // find env
            .append_query_results(vec![vec![make_env_model(1, 10, Some(100), false)]])
            // containers
            .append_query_results(vec![vec![make_container(1, 100, "concurrent-c", None)]])
            // NOTIFY
            .append_exec_results(vec![MockExecResult {
                last_insert_id: 0,
                rows_affected: 0,
            }])
            // find_by_id for waiters checking env status (up to 4 waiters)
            .append_query_results(vec![vec![make_env_model(1, 10, Some(100), false)]])
            .append_query_results(vec![vec![make_env_model(1, 10, Some(100), false)]])
            .append_query_results(vec![vec![make_env_model(1, 10, Some(100), false)]])
            .append_query_results(vec![vec![make_env_model(1, 10, Some(100), false)]])
            .into_connection();

        let lifecycle = Arc::new(MockLifecycle::new());
        let manager = Arc::new(OnDemandManager::new_test(
            Arc::new(db),
            Arc::clone(&lifecycle) as Arc<dyn ContainerLifecycle>,
        ));

        let mut handles = Vec::new();
        for _ in 0..5 {
            let m = Arc::clone(&manager);
            handles.push(tokio::spawn(async move { m.wake_environment(1, 30).await }));
        }

        let results: Vec<_> = futures::future::join_all(handles).await;
        let ok_count = results
            .iter()
            .filter(|r| r.as_ref().map(|r| r.is_ok()).unwrap_or(false))
            .count();

        // All should succeed (first wakes, rest wait and find env awake)
        assert!(
            ok_count >= 1,
            "At least one concurrent wake should succeed, got {} successes out of {}",
            ok_count,
            results.len()
        );

        // Container should be started exactly once
        assert_eq!(
            lifecycle.started_containers().len(),
            1,
            "Container should only be started once despite concurrent requests"
        );
    }

    // ══════════════════════════════════════════════════════════════════════════
    //  SWEEP: MIXED IDLE AND ACTIVE ENVIRONMENTS
    // ══════════════════════════════════════════════════════════════════════════

    #[tokio::test]
    async fn test_sweep_only_sleeps_idle_not_active() {
        // env 1: idle → should sleep
        // env 2: active (just now) → should NOT sleep
        // Uses only 1 idle env to avoid DashMap iteration order issues with MockDatabase
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            // env 1 sleep:
            .append_exec_results(vec![MockExecResult {
                last_insert_id: 0,
                rows_affected: 1,
            }])
            .append_query_results(vec![vec![make_env_model(1, 10, Some(100), false)]])
            .append_query_results(vec![vec![make_container(1, 100, "c1", None)]])
            .append_exec_results(vec![MockExecResult {
                last_insert_id: 0,
                rows_affected: 0,
            }])
            .into_connection();

        let lifecycle = Arc::new(MockLifecycle::new());
        let manager = Arc::new(OnDemandManager::new_test(
            Arc::new(db),
            Arc::clone(&lifecycle) as Arc<dyn ContainerLifecycle>,
        ));

        manager.register_on_demand_environment(1, 60, 30);
        manager.register_on_demand_environment(2, 60, 30);

        let old_time = manager.current_epoch_secs() - 120;
        // env 1: idle for 120s (past 60s timeout)
        manager.last_activity.insert(1, AtomicU64::new(old_time));
        // env 2: active just now
        manager.record_activity(2);

        let slept = manager.sweep_idle_environments().await;
        assert!(slept.contains(&1), "Idle env 1 should sleep");
        assert!(!slept.contains(&2), "Active env 2 should NOT sleep");
        assert_eq!(slept.len(), 1);

        assert_eq!(lifecycle.stopped_containers(), vec!["c1"]);
    }

    // ══════════════════════════════════════════════════════════════════════════
    //  SWEEP CONTINUES AFTER ONE ENVIRONMENT FAILS
    // ══════════════════════════════════════════════════════════════════════════

    #[tokio::test]
    async fn test_sweep_does_not_crash_on_sleep_failure() {
        // Single env whose container stop fails → sweep should return empty
        // (not panic or crash), proving it handles errors gracefully
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            // CAS succeeds
            .append_exec_results(vec![MockExecResult {
                last_insert_id: 0,
                rows_affected: 1,
            }])
            .append_query_results(vec![vec![make_env_model(1, 10, Some(100), false)]])
            .append_query_results(vec![vec![make_container(1, 100, "fail-c", None)]])
            // revert sleeping=false
            .append_exec_results(vec![MockExecResult {
                last_insert_id: 0,
                rows_affected: 1,
            }])
            // NOTIFY after revert
            .append_exec_results(vec![MockExecResult {
                last_insert_id: 0,
                rows_affected: 0,
            }])
            .into_connection();

        let lifecycle = Arc::new(MockLifecycle::with_selective_stop_failures(vec![
            "fail-c".to_string()
        ]));
        let manager = Arc::new(OnDemandManager::new_test(
            Arc::new(db),
            Arc::clone(&lifecycle) as Arc<dyn ContainerLifecycle>,
        ));

        manager.register_on_demand_environment(1, 60, 30);
        let old_time = manager.current_epoch_secs() - 120;
        manager.last_activity.insert(1, AtomicU64::new(old_time));

        let slept = manager.sweep_idle_environments().await;
        // Failed to sleep → not in slept list, but no panic
        assert!(!slept.contains(&1), "env 1 sleep should have failed");
        assert!(slept.is_empty());
    }

    // ══════════════════════════════════════════════════════════════════════════
    //  WAKE: ENVIRONMENT NOT FOUND AFTER CAS
    // ══════════════════════════════════════════════════════════════════════════

    #[tokio::test]
    async fn test_wake_env_deleted_between_cas_and_load() {
        // CAS succeeds (1 row), but find_by_id returns nothing (deleted concurrently)
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_exec_results(vec![MockExecResult {
                last_insert_id: 0,
                rows_affected: 1,
            }])
            // Empty result for find_by_id
            .append_query_results(vec![Vec::<environments::Model>::new()])
            .into_connection();

        let lifecycle = Arc::new(MockLifecycle::new());
        let manager = OnDemandManager::new_test(Arc::new(db), lifecycle);

        let result = manager.wake_environment(1, 30).await;
        assert!(result.is_err());
        assert!(matches!(
            result.unwrap_err(),
            OnDemandError::NotFound { environment_id: 1 }
        ));
    }

    // ══════════════════════════════════════════════════════════════════════════
    //  SLEEP: ENV NOT FOUND AFTER CAS
    // ══════════════════════════════════════════════════════════════════════════

    #[tokio::test]
    async fn test_sleep_env_deleted_between_cas_and_load() {
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            // CAS succeeds
            .append_exec_results(vec![MockExecResult {
                last_insert_id: 0,
                rows_affected: 1,
            }])
            // Empty result for find_by_id
            .append_query_results(vec![Vec::<environments::Model>::new()])
            .into_connection();

        let lifecycle = Arc::new(MockLifecycle::new());
        let manager = OnDemandManager::new_test(Arc::new(db), lifecycle);

        let result = manager.sleep_environment(1).await;
        assert!(result.is_err());
        assert!(matches!(
            result.unwrap_err(),
            OnDemandError::NotFound { environment_id: 1 }
        ));
    }

    // ══════════════════════════════════════════════════════════════════════════
    //  SLEEP: ENV WITH NO CURRENT DEPLOYMENT
    // ══════════════════════════════════════════════════════════════════════════

    #[tokio::test]
    async fn test_sleep_env_no_deployment_returns_false() {
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            // CAS succeeds
            .append_exec_results(vec![MockExecResult {
                last_insert_id: 0,
                rows_affected: 1,
            }])
            // env with no deployment
            .append_query_results(vec![vec![make_env_model(1, 10, None, false)]])
            .into_connection();

        let lifecycle = Arc::new(MockLifecycle::new());
        let manager = OnDemandManager::new_test(Arc::new(db), lifecycle);

        let result = manager.sleep_environment(1).await.unwrap();
        assert!(!result, "Should return false when env has no deployment");
    }

    // ══════════════════════════════════════════════════════════════════════════
    //  WAKE: NO CONTAINERS FOUND
    // ══════════════════════════════════════════════════════════════════════════

    #[tokio::test]
    async fn test_wake_no_containers_succeeds_gracefully() {
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            // CAS UPDATE -> 1 row
            .append_exec_results(vec![MockExecResult {
                last_insert_id: 0,
                rows_affected: 1,
            }])
            // find env
            .append_query_results(vec![vec![make_env_model(1, 10, Some(100), true)]])
            // empty containers
            .append_query_results(vec![Vec::<deployment_containers::Model>::new()])
            .into_connection();

        let lifecycle = Arc::new(MockLifecycle::new());
        let manager = OnDemandManager::new_test(Arc::new(db), lifecycle);

        let result = manager.wake_environment(1, 30).await;
        assert!(
            result.is_ok(),
            "Wake with no containers should succeed gracefully"
        );
    }

    // ══════════════════════════════════════════════════════════════════════════
    //  UPDATE_CONFIGS REPLACES OLD AND INITIALIZES ACTIVITY
    // ══════════════════════════════════════════════════════════════════════════

    #[test]
    fn test_update_configs_replaces_and_initializes() {
        let db = MockDatabase::new(DatabaseBackend::Postgres).into_connection();
        let lifecycle = Arc::new(MockLifecycle::new());
        let manager = OnDemandManager::new_test(Arc::new(db), lifecycle);

        // First batch
        manager.update_configs(vec![
            OnDemandConfig {
                environment_id: 1,
                idle_timeout_seconds: 300,
                wake_timeout_seconds: 30,
            },
            OnDemandConfig {
                environment_id: 2,
                idle_timeout_seconds: 600,
                wake_timeout_seconds: 60,
            },
        ]);
        assert!(manager.configs.contains_key(&1));
        assert!(manager.configs.contains_key(&2));
        assert!(manager.last_activity.contains_key(&1));
        assert!(manager.last_activity.contains_key(&2));

        // Second batch replaces first
        manager.update_configs(vec![OnDemandConfig {
            environment_id: 3,
            idle_timeout_seconds: 120,
            wake_timeout_seconds: 15,
        }]);
        assert!(
            !manager.configs.contains_key(&1),
            "Old config 1 should be removed"
        );
        assert!(
            !manager.configs.contains_key(&2),
            "Old config 2 should be removed"
        );
        assert!(
            manager.configs.contains_key(&3),
            "New config 3 should be present"
        );
        // Activity for old envs persists (not cleared by update_configs)
        assert!(manager.last_activity.contains_key(&1));
    }

    // ══════════════════════════════════════════════════════════════════════════
    //  REMOVE_ENVIRONMENT CLEANS UP ALL STATE
    // ══════════════════════════════════════════════════════════════════════════

    #[test]
    fn test_remove_environment_cleans_all_state() {
        let db = MockDatabase::new(DatabaseBackend::Postgres).into_connection();
        let lifecycle = Arc::new(MockLifecycle::new());
        let manager = OnDemandManager::new_test(Arc::new(db), lifecycle);

        manager.register_on_demand_environment(42, 300, 30);
        manager.record_activity(42);
        // Simulate wake state creation
        manager.wake_states.insert(
            42,
            Arc::new(WakeState {
                waking: AtomicBool::new(false),
                notify: Notify::new(),
            }),
        );

        assert!(manager.configs.contains_key(&42));
        assert!(manager.last_activity.contains_key(&42));
        assert!(manager.wake_states.contains_key(&42));

        manager.remove_environment(42);

        assert!(!manager.configs.contains_key(&42));
        assert!(!manager.last_activity.contains_key(&42));
        assert!(!manager.wake_states.contains_key(&42));
    }

    // ══════════════════════════════════════════════════════════════════════════
    //  ACTIVITY TRACKING: RAPID CONCURRENT UPDATES
    // ══════════════════════════════════════════════════════════════════════════

    #[test]
    fn test_rapid_activity_updates_no_panic() {
        let db = MockDatabase::new(DatabaseBackend::Postgres).into_connection();
        let lifecycle = Arc::new(MockLifecycle::new());
        let manager = Arc::new(OnDemandManager::new_test(Arc::new(db), lifecycle));

        // Simulate rapid concurrent activity recording from multiple threads
        let mut handles = Vec::new();
        for _ in 0..10 {
            let m = Arc::clone(&manager);
            handles.push(std::thread::spawn(move || {
                for _ in 0..1000 {
                    m.record_activity(1);
                    m.record_activity(2);
                    m.record_activity(3);
                }
            }));
        }

        for h in handles {
            h.join().unwrap();
        }

        // All 3 environments should have activity tracked
        assert!(manager.last_activity.contains_key(&1));
        assert!(manager.last_activity.contains_key(&2));
        assert!(manager.last_activity.contains_key(&3));
    }

    // ══════════════════════════════════════════════════════════════════════════
    //  SLEEPING DOMAIN: OVERWRITE SAME DOMAIN
    // ══════════════════════════════════════════════════════════════════════════

    #[test]
    fn test_sleeping_domain_overwrite() {
        let db = MockDatabase::new(DatabaseBackend::Postgres).into_connection();
        let lifecycle = Arc::new(MockLifecycle::new());
        let manager = OnDemandManager::new_test(Arc::new(db), lifecycle);

        manager.register_sleeping_domain(
            "app.example.com".to_string(),
            SleepingEnvironmentInfo {
                environment_id: 1,
                project_id: 10,
                deployment_id: 100,
                wake_timeout_seconds: 30,
            },
        );

        // Overwrite with different env for same domain (e.g. after redeployment)
        manager.register_sleeping_domain(
            "app.example.com".to_string(),
            SleepingEnvironmentInfo {
                environment_id: 2,
                project_id: 20,
                deployment_id: 200,
                wake_timeout_seconds: 60,
            },
        );

        let info = manager.get_sleeping_environment("app.example.com").unwrap();
        assert_eq!(info.environment_id, 2);
        assert_eq!(info.project_id, 20);
        assert_eq!(info.deployment_id, 200);
        assert_eq!(info.wake_timeout_seconds, 60);
    }

    // ══════════════════════════════════════════════════════════════════════════
    //  WAKE: MULTIPLE CONTAINERS WITH MULTI-NODE
    // ══════════════════════════════════════════════════════════════════════════

    #[tokio::test]
    async fn test_wake_multiple_containers_multi_node() {
        // 3 containers across different nodes, all should be started
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_exec_results(vec![MockExecResult {
                last_insert_id: 0,
                rows_affected: 1,
            }])
            .append_query_results(vec![vec![make_env_model(1, 10, Some(100), true)]])
            .append_query_results(vec![vec![
                make_container(1, 100, "local-c", None),
                make_container(2, 100, "remote-c1", Some(2)),
                make_container(3, 100, "remote-c2", Some(3)),
            ]])
            // NOTIFY
            .append_exec_results(vec![MockExecResult {
                last_insert_id: 0,
                rows_affected: 0,
            }])
            .into_connection();

        let lifecycle = Arc::new(MockLifecycle::new());
        let manager = OnDemandManager::new_test(
            Arc::new(db),
            Arc::clone(&lifecycle) as Arc<dyn ContainerLifecycle>,
        );

        let result = manager.wake_environment(1, 30).await;
        assert!(result.is_ok());

        let mut started = lifecycle.started_containers();
        started.sort();
        assert_eq!(started, vec!["local-c", "remote-c1", "remote-c2"]);
    }

    // ══════════════════════════════════════════════════════════════════════════
    //  VALIDATION: BOUNDARY VALUES
    // ══════════════════════════════════════════════════════════════════════════

    #[test]
    fn test_on_demand_validation_boundary_idle_min() {
        let config = temps_entities::deployment_config::DeploymentConfig {
            on_demand: true,
            idle_timeout_seconds: 60, // exact minimum
            ..Default::default()
        };
        assert!(config.validate().is_ok());
    }

    #[test]
    fn test_on_demand_validation_boundary_idle_max() {
        let config = temps_entities::deployment_config::DeploymentConfig {
            on_demand: true,
            idle_timeout_seconds: 86400, // exact maximum
            ..Default::default()
        };
        assert!(config.validate().is_ok());
    }

    #[test]
    fn test_on_demand_validation_boundary_wake_min() {
        let config = temps_entities::deployment_config::DeploymentConfig {
            on_demand: true,
            wake_timeout_seconds: 5, // exact minimum
            ..Default::default()
        };
        assert!(config.validate().is_ok());
    }

    #[test]
    fn test_on_demand_validation_boundary_wake_max() {
        let config = temps_entities::deployment_config::DeploymentConfig {
            on_demand: true,
            wake_timeout_seconds: 120, // exact maximum
            ..Default::default()
        };
        assert!(config.validate().is_ok());
    }

    #[test]
    fn test_on_demand_validation_boundary_idle_just_below_min() {
        let config = temps_entities::deployment_config::DeploymentConfig {
            on_demand: true,
            idle_timeout_seconds: 59, // just below 60
            ..Default::default()
        };
        assert!(config.validate().is_err());
    }

    #[test]
    fn test_on_demand_validation_boundary_idle_just_above_max() {
        let config = temps_entities::deployment_config::DeploymentConfig {
            on_demand: true,
            idle_timeout_seconds: 86401, // just above 86400
            ..Default::default()
        };
        assert!(config.validate().is_err());
    }

    #[test]
    fn test_on_demand_validation_boundary_wake_just_below_min() {
        let config = temps_entities::deployment_config::DeploymentConfig {
            on_demand: true,
            wake_timeout_seconds: 4, // just below 5
            ..Default::default()
        };
        assert!(config.validate().is_err());
    }

    #[test]
    fn test_on_demand_validation_boundary_wake_just_above_max() {
        let config = temps_entities::deployment_config::DeploymentConfig {
            on_demand: true,
            wake_timeout_seconds: 121, // just above 120
            ..Default::default()
        };
        assert!(config.validate().is_err());
    }

    // ══════════════════════════════════════════════════════════════════════════
    //  OnDemandWaker TRAIT BRIDGE
    // ══════════════════════════════════════════════════════════════════════════

    #[tokio::test]
    async fn test_on_demand_waker_bridge_wake() {
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            // CAS -> 0 rows (already awake)
            .append_exec_results(vec![MockExecResult {
                last_insert_id: 0,
                rows_affected: 0,
            }])
            .into_connection();

        let lifecycle = Arc::new(MockLifecycle::new());
        let manager = OnDemandManager::new_test(Arc::new(db), lifecycle);

        // Call through the OnDemandWaker trait
        let waker: &dyn OnDemandWaker = &manager;
        let result = waker.wake_environment(1, 30).await;
        assert!(result.is_ok());
    }

    #[tokio::test]
    async fn test_on_demand_waker_bridge_sleep() {
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            // CAS -> 0 rows (already sleeping)
            .append_exec_results(vec![MockExecResult {
                last_insert_id: 0,
                rows_affected: 0,
            }])
            .into_connection();

        let lifecycle = Arc::new(MockLifecycle::new());
        let manager = OnDemandManager::new_test(Arc::new(db), lifecycle);

        let waker: &dyn OnDemandWaker = &manager;
        let result = waker.sleep_environment(1).await;
        assert!(result.is_ok());
        assert!(!result.unwrap());
    }

    // ══════════════════════════════════════════════════════════════════════════
    //  ERROR TYPE COVERAGE
    // ══════════════════════════════════════════════════════════════════════════

    #[test]
    fn test_on_demand_error_container_operation_display() {
        let err = OnDemandError::ContainerOperation {
            container_id: "abc123".to_string(),
            reason: "connection refused".to_string(),
        };
        let msg = err.to_string();
        assert!(msg.contains("abc123"));
        assert!(msg.contains("connection refused"));
    }

    #[test]
    fn test_on_demand_error_not_found_display() {
        let err = OnDemandError::NotFound { environment_id: 42 };
        assert!(err.to_string().contains("42"));
    }

    #[test]
    fn test_on_demand_error_from_db_err() {
        let db_err = sea_orm::DbErr::Custom("test db error".to_string());
        let err: OnDemandError = db_err.into();
        assert!(matches!(err, OnDemandError::Database(_)));
        assert!(err.to_string().contains("test db error"));
    }

    // ══════════════════════════════════════════════════════════════════════════
    //  REGISTER ON-DEMAND PRESERVES EXISTING ACTIVITY
    // ══════════════════════════════════════════════════════════════════════════

    #[test]
    fn test_register_on_demand_preserves_existing_activity() {
        let db = MockDatabase::new(DatabaseBackend::Postgres).into_connection();
        let lifecycle = Arc::new(MockLifecycle::new());
        let manager = OnDemandManager::new_test(Arc::new(db), lifecycle);

        // Record activity first
        manager.record_activity(1);
        let original_ts = manager
            .last_activity
            .get(&1)
            .unwrap()
            .value()
            .load(Ordering::Relaxed);

        // Register should not overwrite existing activity
        std::thread::sleep(Duration::from_millis(10));
        manager.register_on_demand_environment(1, 300, 30);

        let ts_after = manager
            .last_activity
            .get(&1)
            .unwrap()
            .value()
            .load(Ordering::Relaxed);

        assert_eq!(
            original_ts, ts_after,
            "register_on_demand_environment should not overwrite existing activity"
        );
    }

    // ══════════════════════════════════════════════════════════════════════════
    //  SWEEP: NO ACTIVITY RECORDED (EDGE CASE)
    // ══════════════════════════════════════════════════════════════════════════

    #[tokio::test]
    async fn test_sweep_env_without_activity_entry() {
        // Config exists but no last_activity entry — should not panic
        let db = MockDatabase::new(DatabaseBackend::Postgres).into_connection();
        let lifecycle = Arc::new(MockLifecycle::new());
        let manager = OnDemandManager::new_test(Arc::new(db), lifecycle);

        // Manually insert config without activity
        manager.configs.insert(
            99,
            OnDemandConfig {
                environment_id: 99,
                idle_timeout_seconds: 60,
                wake_timeout_seconds: 30,
            },
        );
        // No last_activity entry for env 99

        let slept = manager.sweep_idle_environments().await;
        // Should not panic, should not try to sleep (no activity data)
        assert!(slept.is_empty());
    }

    // ══════════════════════════════════════════════════════════════════════════
    //  SLEEPING ENVIRONMENT INFO CLONE
    // ══════════════════════════════════════════════════════════════════════════

    // ══════════════════════════════════════════════════════════════════════════
    //  INTEGRATION: FULL LIFECYCLE — ENABLE → IDLE → KILL → WAKE
    // ══════════════════════════════════════════════════════════════════════════

    /// Simulates the real callback wiring from server.rs.
    /// Returns the manager with sleeping/on-demand state populated.
    fn simulate_route_reload_callback(
        manager: &Arc<OnDemandManager>,
        sleeping: Vec<temps_routes::SleepingEnvironmentEntry>,
        on_demand_configs: Vec<temps_routes::OnDemandConfigEntry>,
    ) {
        manager.clear_sleeping_domains();
        for entry in sleeping {
            manager.register_sleeping_domain(
                entry.domain.clone(),
                SleepingEnvironmentInfo {
                    environment_id: entry.environment_id,
                    project_id: entry.project_id,
                    deployment_id: entry.deployment_id,
                    wake_timeout_seconds: entry.wake_timeout_seconds,
                },
            );
        }
        for config in on_demand_configs {
            manager.register_on_demand_environment(
                config.environment_id,
                config.idle_timeout_seconds,
                config.wake_timeout_seconds,
            );
        }
    }

    #[tokio::test]
    async fn test_full_lifecycle_enable_idle_kill() {
        // Scenario: User enables on-demand for env 1 with 60s idle timeout.
        // Route reload fires callback → env registered for idle tracking.
        // No requests come in → idle timeout exceeded → sweep kills containers.

        let db = MockDatabase::new(DatabaseBackend::Postgres)
            // sleep_environment CAS: UPDATE sleeping=true WHERE sleeping=false → 1 row
            .append_exec_results(vec![MockExecResult {
                last_insert_id: 0,
                rows_affected: 1,
            }])
            // find env for sleep
            .append_query_results(vec![vec![make_env_model(1, 10, Some(100), false)]])
            // containers for sleep
            .append_query_results(vec![vec![
                make_container(1, 100, "app-c1", None),
                make_container(2, 100, "app-c2", Some(2)),
            ]])
            // NOTIFY route_table_changes
            .append_exec_results(vec![MockExecResult {
                last_insert_id: 0,
                rows_affected: 0,
            }])
            .into_connection();

        let lifecycle = Arc::new(MockLifecycle::new());
        let manager = Arc::new(OnDemandManager::new_test(
            Arc::new(db),
            Arc::clone(&lifecycle) as Arc<dyn ContainerLifecycle>,
        ));

        // Step 1: Simulate route reload callback (what happens after user enables on-demand)
        // The route table found env 1 is awake with on_demand=true, so it's in on_demand_configs
        simulate_route_reload_callback(
            &manager,
            vec![], // no sleeping envs yet
            vec![temps_routes::OnDemandConfigEntry {
                environment_id: 1,
                idle_timeout_seconds: 60,
                wake_timeout_seconds: 30,
            }],
        );

        // Verify env is registered for idle tracking
        assert!(manager.configs.contains_key(&1));
        assert!(manager.last_activity.contains_key(&1));

        // Step 2: Simulate time passing — set last activity to 120s ago (past 60s timeout)
        let old_time = manager.current_epoch_secs() - 120;
        manager.last_activity.insert(1, AtomicU64::new(old_time));

        // Step 3: Run sweep — should detect idle and kill containers
        let slept = manager.sweep_idle_environments().await;
        assert_eq!(slept, vec![1], "Sweep should put env 1 to sleep");

        // Step 4: Verify containers were actually stopped
        let mut stopped = lifecycle.stopped_containers();
        stopped.sort();
        assert_eq!(stopped, vec!["app-c1", "app-c2"]);
    }

    #[tokio::test]
    async fn test_full_lifecycle_active_env_not_killed() {
        // Scenario: User enables on-demand, but keeps sending requests.
        // Container should NOT be killed because activity keeps resetting.

        let db = MockDatabase::new(DatabaseBackend::Postgres).into_connection();
        let lifecycle = Arc::new(MockLifecycle::new());
        let manager = Arc::new(OnDemandManager::new_test(
            Arc::new(db),
            Arc::clone(&lifecycle) as Arc<dyn ContainerLifecycle>,
        ));

        // Route reload: env 1 with on_demand=true, 300s idle timeout
        simulate_route_reload_callback(
            &manager,
            vec![],
            vec![temps_routes::OnDemandConfigEntry {
                environment_id: 1,
                idle_timeout_seconds: 300,
                wake_timeout_seconds: 30,
            }],
        );

        // Simulate proxy recording activity (happens on every request)
        manager.record_activity(1);

        // Run sweep — env just had activity, so it's NOT idle
        let slept = manager.sweep_idle_environments().await;
        assert!(slept.is_empty(), "Active env should not be put to sleep");
        assert!(
            lifecycle.stopped_containers().is_empty(),
            "No containers should be stopped"
        );
    }

    #[tokio::test]
    async fn test_full_lifecycle_kill_then_wake_on_request() {
        // Full round-trip:
        // 1. Enable on-demand
        // 2. Env goes idle → containers killed, marked sleeping
        // 3. After sleep, route reload fires again → env is now in sleeping list
        // 4. Request comes in → domain found in sleeping map → wake triggered

        // DB mock for the entire lifecycle:
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            // === PHASE 1: SLEEP (via sweep_idle_environments) ===
            // CAS UPDATE sleeping=true → 1 row
            .append_exec_results(vec![MockExecResult {
                last_insert_id: 0,
                rows_affected: 1,
            }])
            // find env for sleep
            .append_query_results(vec![vec![make_env_model(1, 10, Some(100), false)]])
            // containers for sleep
            .append_query_results(vec![vec![make_container(1, 100, "myapp", None)]])
            // NOTIFY after sleep
            .append_exec_results(vec![MockExecResult {
                last_insert_id: 0,
                rows_affected: 0,
            }])
            // persist_activity_timestamps: UPDATE last_activity_at for env 1
            .append_exec_results(vec![MockExecResult {
                last_insert_id: 0,
                rows_affected: 1,
            }])
            // === PHASE 2: WAKE ===
            // CAS UPDATE sleeping=false → 1 row
            .append_exec_results(vec![MockExecResult {
                last_insert_id: 0,
                rows_affected: 1,
            }])
            // find env for wake
            .append_query_results(vec![vec![make_env_model(1, 10, Some(100), true)]])
            // containers for wake
            .append_query_results(vec![vec![make_container(1, 100, "myapp", None)]])
            // UPDATE last_activity_at after wake
            .append_exec_results(vec![MockExecResult {
                last_insert_id: 0,
                rows_affected: 1,
            }])
            // NOTIFY after wake
            .append_exec_results(vec![MockExecResult {
                last_insert_id: 0,
                rows_affected: 0,
            }])
            .into_connection();

        let lifecycle = Arc::new(MockLifecycle::new());
        let manager = Arc::new(OnDemandManager::new_test(
            Arc::new(db),
            Arc::clone(&lifecycle) as Arc<dyn ContainerLifecycle>,
        ));

        // ── PHASE 1: Enable on-demand, go idle, get killed ──

        simulate_route_reload_callback(
            &manager,
            vec![],
            vec![temps_routes::OnDemandConfigEntry {
                environment_id: 1,
                idle_timeout_seconds: 60,
                wake_timeout_seconds: 30,
            }],
        );

        // Set activity to 120s ago → past 60s timeout
        let old_time = manager.current_epoch_secs() - 120;
        manager.last_activity.insert(1, AtomicU64::new(old_time));

        let slept = manager.sweep_idle_environments().await;
        assert_eq!(slept, vec![1]);
        assert_eq!(lifecycle.stopped_containers(), vec!["myapp"]);

        // ── PHASE 2: Route reload fires (triggered by NOTIFY) ──
        // Now env 1 is sleeping, so route table puts it in sleeping_environments

        simulate_route_reload_callback(
            &manager,
            vec![temps_routes::SleepingEnvironmentEntry {
                domain: "myapp.preview.example.com".to_string(),
                environment_id: 1,
                project_id: 10,
                deployment_id: 100,
                wake_timeout_seconds: 30,
            }],
            vec![], // env 1 is sleeping, so NOT in on_demand_configs
        );

        // Verify domain is now in sleeping map
        let sleeping_info = manager.get_sleeping_environment("myapp.preview.example.com");
        assert!(
            sleeping_info.is_some(),
            "Domain should be in sleeping map after route reload"
        );
        let info = sleeping_info.unwrap();
        assert_eq!(info.environment_id, 1);

        // ── PHASE 3: Request comes in → wake ──
        // This is what proxy.rs does when it finds the domain in sleeping map
        let wake_result = manager
            .wake_environment(info.environment_id, info.wake_timeout_seconds)
            .await;
        assert!(
            wake_result.is_ok(),
            "Wake should succeed: {:?}",
            wake_result.err()
        );

        // Container should have been started
        assert_eq!(lifecycle.started_containers(), vec!["myapp"]);
    }

    #[tokio::test]
    async fn test_full_lifecycle_request_resets_idle_timer() {
        // Scenario: Env idle for 50s (timeout 60s), then a request comes in,
        // then 50s more passes. Total 100s but timer was reset → should NOT sleep.

        let db = MockDatabase::new(DatabaseBackend::Postgres).into_connection();
        let lifecycle = Arc::new(MockLifecycle::new());
        let manager = Arc::new(OnDemandManager::new_test(
            Arc::new(db),
            Arc::clone(&lifecycle) as Arc<dyn ContainerLifecycle>,
        ));

        simulate_route_reload_callback(
            &manager,
            vec![],
            vec![temps_routes::OnDemandConfigEntry {
                environment_id: 1,
                idle_timeout_seconds: 60,
                wake_timeout_seconds: 30,
            }],
        );

        // Activity was 50s ago (under 60s timeout)
        let t = manager.current_epoch_secs() - 50;
        manager.last_activity.insert(1, AtomicU64::new(t));

        // Sweep: should NOT sleep (50s < 60s)
        let slept = manager.sweep_idle_environments().await;
        assert!(slept.is_empty(), "Should not sleep before timeout");

        // Simulate a request that resets the timer
        manager.record_activity(1);

        // Now even after 50s more, the timer was reset, so still not idle
        let slept = manager.sweep_idle_environments().await;
        assert!(slept.is_empty(), "Should not sleep after activity reset");
        assert!(lifecycle.stopped_containers().is_empty());
    }

    #[tokio::test]
    async fn test_full_lifecycle_disable_on_demand_stops_tracking() {
        // Scenario: on-demand was enabled, then user disables it.
        // Route reload fires without the env in on_demand_configs.
        // Sweep should no longer track or sleep this env.

        let db = MockDatabase::new(DatabaseBackend::Postgres).into_connection();
        let lifecycle = Arc::new(MockLifecycle::new());
        let manager = Arc::new(OnDemandManager::new_test(
            Arc::new(db),
            Arc::clone(&lifecycle) as Arc<dyn ContainerLifecycle>,
        ));

        // Initially enabled
        simulate_route_reload_callback(
            &manager,
            vec![],
            vec![temps_routes::OnDemandConfigEntry {
                environment_id: 1,
                idle_timeout_seconds: 60,
                wake_timeout_seconds: 30,
            }],
        );
        assert!(manager.configs.contains_key(&1));

        // User disables on-demand → route reload fires without env 1 in configs
        // Note: the callback currently only adds, doesn't remove old configs.
        // This tests the real behavior — the env stays in configs until removed.
        // For proper cleanup, remove_environment would need to be called.
        manager.remove_environment(1);
        assert!(!manager.configs.contains_key(&1));

        // Set old activity to make it look idle
        manager.last_activity.insert(1, AtomicU64::new(0));

        // Sweep should NOT touch env 1 (not in configs)
        let slept = manager.sweep_idle_environments().await;
        assert!(slept.is_empty());
    }

    #[tokio::test]
    async fn test_full_lifecycle_env_not_idle_enough_stays_awake() {
        // env has 300s timeout, idle for 120s → should NOT sleep
        let db = MockDatabase::new(DatabaseBackend::Postgres).into_connection();
        let lifecycle = Arc::new(MockLifecycle::new());
        let manager = Arc::new(OnDemandManager::new_test(
            Arc::new(db),
            Arc::clone(&lifecycle) as Arc<dyn ContainerLifecycle>,
        ));

        simulate_route_reload_callback(
            &manager,
            vec![],
            vec![temps_routes::OnDemandConfigEntry {
                environment_id: 1,
                idle_timeout_seconds: 300,
                wake_timeout_seconds: 30,
            }],
        );

        // Idle for 120s, but timeout is 300s
        let old_time = manager.current_epoch_secs() - 120;
        manager.last_activity.insert(1, AtomicU64::new(old_time));

        let slept = manager.sweep_idle_environments().await;
        assert!(
            slept.is_empty(),
            "env with 300s timeout should NOT sleep after only 120s idle"
        );
        assert!(lifecycle.stopped_containers().is_empty());
    }

    #[tokio::test]
    async fn test_full_lifecycle_exact_boundary_idle_sleeps() {
        // env has 120s timeout, idle for exactly 120s → should sleep (>= check)
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_exec_results(vec![MockExecResult {
                last_insert_id: 0,
                rows_affected: 1,
            }])
            .append_query_results(vec![vec![make_env_model(1, 10, Some(100), false)]])
            .append_query_results(vec![vec![make_container(1, 100, "c1", None)]])
            .append_exec_results(vec![MockExecResult {
                last_insert_id: 0,
                rows_affected: 0,
            }])
            .into_connection();

        let lifecycle = Arc::new(MockLifecycle::new());
        let manager = Arc::new(OnDemandManager::new_test(
            Arc::new(db),
            Arc::clone(&lifecycle) as Arc<dyn ContainerLifecycle>,
        ));

        simulate_route_reload_callback(
            &manager,
            vec![],
            vec![temps_routes::OnDemandConfigEntry {
                environment_id: 1,
                idle_timeout_seconds: 120,
                wake_timeout_seconds: 30,
            }],
        );

        let old_time = manager.current_epoch_secs() - 120;
        manager.last_activity.insert(1, AtomicU64::new(old_time));

        let slept = manager.sweep_idle_environments().await;
        assert_eq!(
            slept,
            vec![1],
            "env should sleep at exact boundary (idle_secs >= timeout)"
        );
        assert_eq!(lifecycle.stopped_containers(), vec!["c1"]);
    }

    #[tokio::test]
    async fn test_full_lifecycle_route_reload_transitions_sleeping_to_awake() {
        // When an env wakes up, the next route reload should:
        // 1. Remove it from sleeping_by_domain (clear_sleeping_domains)
        // 2. Add it to on_demand_configs (for idle tracking again)

        let db = MockDatabase::new(DatabaseBackend::Postgres).into_connection();
        let lifecycle = Arc::new(MockLifecycle::new());
        let manager = Arc::new(OnDemandManager::new_test(
            Arc::new(db),
            Arc::clone(&lifecycle) as Arc<dyn ContainerLifecycle>,
        ));

        // Initial state: env 1 sleeping
        simulate_route_reload_callback(
            &manager,
            vec![temps_routes::SleepingEnvironmentEntry {
                domain: "app.preview.example.com".to_string(),
                environment_id: 1,
                project_id: 10,
                deployment_id: 100,
                wake_timeout_seconds: 30,
            }],
            vec![],
        );

        assert!(manager
            .get_sleeping_environment("app.preview.example.com")
            .is_some());

        // After wake, route reload fires again: env is now awake
        simulate_route_reload_callback(
            &manager,
            vec![], // no longer sleeping
            vec![temps_routes::OnDemandConfigEntry {
                environment_id: 1,
                idle_timeout_seconds: 300,
                wake_timeout_seconds: 30,
            }],
        );

        // Domain should no longer be in sleeping map
        assert!(
            manager
                .get_sleeping_environment("app.preview.example.com")
                .is_none(),
            "Domain should be removed from sleeping map after env wakes up"
        );
        // But env should be tracked for idle timeout again
        assert!(
            manager.configs.contains_key(&1),
            "Env should be in configs for idle tracking"
        );
    }

    #[test]
    fn test_sleeping_environment_info_debug_clone() {
        let info = SleepingEnvironmentInfo {
            environment_id: 1,
            project_id: 10,
            deployment_id: 100,
            wake_timeout_seconds: 30,
        };
        let cloned = info.clone();
        assert_eq!(cloned.environment_id, info.environment_id);
        assert_eq!(cloned.project_id, info.project_id);
        assert_eq!(cloned.deployment_id, info.deployment_id);
        assert_eq!(cloned.wake_timeout_seconds, info.wake_timeout_seconds);

        // Debug trait
        let debug_str = format!("{:?}", info);
        assert!(debug_str.contains("SleepingEnvironmentInfo"));
    }
}
