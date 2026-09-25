// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Docker log collector service
//!
//! Opens a streaming log connection to the Docker daemon for each running container.
//! Enriches every log line with project_id, env, service, deploy_id, container_id
//! from Docker labels set by temps.sh at container creation.
//!
//! Resilience:
//! - On stream error: exponential backoff (1s → 30s cap), reconnects using the
//!   timestamp of the last successfully received line to avoid gaps.
//! - On container gone (404): gives up immediately instead of retrying forever.
//! - On a transient failure deciding whether to collect a container (e.g. the
//!   ownership lookup hits a database blip): retries in the background, since
//!   discovery sees each container only once.
//! - Max consecutive failures threshold: after 20 consecutive errors the streaming
//!   task for that container exits to avoid wasting resources.

use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use bollard::query_parameters::LogsOptionsBuilder;
use bollard::Docker;

use futures_util::StreamExt;
use tokio::sync::broadcast;
use tokio::sync::{Mutex, OwnedMutexGuard};
use tokio::task::JoinHandle;
use tracing::{debug, error, info, warn};

use crate::error::{LogAggregatorError, RetryClass};
use crate::parser::{parse_docker_timestamp, parse_log_line};
use crate::services::{ChunkWriterService, LogMetadataService};
use crate::types::{ContainerContext, LogLine, LogStream};

/// Docker label keys set by temps.sh at container creation
const LABEL_PROJECT_ID: &str = "sh.temps.project_id";
const LABEL_ENV: &str = "sh.temps.environment";
const LABEL_SERVICE: &str = "sh.temps.service";
const LABEL_DEPLOY_ID: &str = "sh.temps.deploy_id";

/// Maximum consecutive stream errors before giving up on a container.
/// At 30s max backoff this is roughly 10 minutes of retrying.
const MAX_CONSECUTIVE_ERRORS: u32 = 20;

/// Backoff bounds for retrying a container whose collection could not be
/// decided yet (see [`CollectorService::start_streaming_with_retry`]).
const START_RETRY_INITIAL: std::time::Duration = std::time::Duration::from_secs(1);
const START_RETRY_MAX: std::time::Duration = std::time::Duration::from_secs(30);

/// A still-failing start retry is logged on its first failure and then every
/// this many attempts (~5 minutes at the backoff cap), not every 30 seconds.
const START_RETRY_LOG_EVERY: u32 = 10;

/// State of a streaming task for a single container
struct StreamTask {
    handle: JoinHandle<()>,
}

/// Service that manages Docker container log streaming.
///
/// For each running container, it opens a `follow: true` streaming connection
/// to the Docker daemon. Tracks the last seen timestamp per container so
/// reconnections resume without gaps.
pub struct CollectorService {
    docker: Arc<temps_core::DockerHandle>,
    chunk_writer: Arc<ChunkWriterService>,
    metadata_service: Arc<LogMetadataService>,
    /// DB handle used to resolve an imported external service's
    /// `temps.service_name` label to its `external_services.id`, and to verify
    /// a project-labelled container names a deployment this instance owns.
    /// `None` in tests that don't exercise either.
    db: Option<Arc<sea_orm::DatabaseConnection>>,
    /// Broadcast channel for live tail subscribers
    tail_tx: broadcast::Sender<LogLine>,
    /// Active streaming tasks per container_id
    active_streams: Mutex<HashMap<String, StreamTask>>,
    /// Containers whose start is being retried in the background. Removing
    /// an id (on stop) cancels its retry.
    pending_starts: Mutex<HashSet<String>>,
    /// One lock per container, held across a whole start or stop so the two
    /// never interleave: a stop that arrives while a (retried) start is
    /// inspecting the container waits, then tears down whatever that start
    /// installed, instead of finding nothing to cancel and leaving a stream
    /// behind. Entries are dropped once nobody holds or waits on them.
    lifecycle_locks: std::sync::Mutex<HashMap<String, Arc<Mutex<()>>>>,
}

/// Held for the duration of one start or stop of a container; see
/// [`CollectorService::lifecycle_locks`].
struct ContainerLifecycleGuard<'a> {
    locks: &'a std::sync::Mutex<HashMap<String, Arc<Mutex<()>>>>,
    container_id: String,
    lock: Arc<Mutex<()>>,
    guard: Option<OwnedMutexGuard<()>>,
}

impl Drop for ContainerLifecycleGuard<'_> {
    fn drop(&mut self) {
        drop(self.guard.take());
        // Clones are only made under this map lock, so a count of two (the
        // map's and ours) means nobody else holds or waits on it.
        let mut locks = self
            .locks
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if Arc::strong_count(&self.lock) == 2
            && locks
                .get(&self.container_id)
                .is_some_and(|lock| Arc::ptr_eq(lock, &self.lock))
        {
            locks.remove(&self.container_id);
        }
    }
}

impl CollectorService {
    pub fn new(
        docker: Arc<temps_core::DockerHandle>,
        chunk_writer: Arc<ChunkWriterService>,
        metadata_service: Arc<LogMetadataService>,
        tail_capacity: usize,
    ) -> Self {
        let (tail_tx, _) = broadcast::channel(tail_capacity);
        Self {
            docker,
            chunk_writer,
            metadata_service,
            db: None,
            tail_tx,
            active_streams: Mutex::new(HashMap::new()),
            pending_starts: Mutex::new(HashSet::new()),
            lifecycle_locks: std::sync::Mutex::new(HashMap::new()),
        }
    }

    /// Attach a DB handle so external-service containers (labelled
    /// `temps.service_type`/`temps.service_name`) resolve to their
    /// `external_services.id` and get first-class log history.
    pub fn with_db(mut self, db: Arc<sea_orm::DatabaseConnection>) -> Self {
        self.db = Some(db);
        self
    }

    /// Get a broadcast receiver for live tail subscriptions.
    pub fn subscribe_tail(&self) -> broadcast::Receiver<LogLine> {
        self.tail_tx.subscribe()
    }

    /// Get the broadcast sender for creating a TailService.
    pub fn tail_sender(&self) -> broadcast::Sender<LogLine> {
        self.tail_tx.clone()
    }

    /// Start streaming logs for a container.
    ///
    /// Extracts context from Docker labels. If the container has no temps.sh labels,
    /// it is silently skipped.
    pub async fn start_streaming(&self, container_id: &str) -> Result<(), LogAggregatorError> {
        let _lifecycle = self.lock_container(container_id).await;
        self.start_streaming_locked(container_id).await
    }

    /// Wait for exclusive start/stop rights over one container.
    async fn lock_container(&self, container_id: &str) -> ContainerLifecycleGuard<'_> {
        let lock = self
            .lifecycle_locks
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .entry(container_id.to_string())
            .or_default()
            .clone();
        let guard = Arc::clone(&lock).lock_owned().await;
        ContainerLifecycleGuard {
            locks: &self.lifecycle_locks,
            container_id: container_id.to_string(),
            lock,
            guard: Some(guard),
        }
    }

    /// [`Self::start_streaming`] for a caller already holding the
    /// container's lifecycle lock.
    async fn start_streaming_locked(&self, container_id: &str) -> Result<(), LogAggregatorError> {
        // Check if already streaming
        {
            let streams = self.active_streams.lock().await;
            if streams.contains_key(container_id) {
                debug!(container_id = container_id, "Already streaming, skipping");
                return Ok(());
            }
        }

        // Inspect container for labels
        let ctx = self.extract_context(container_id).await?;
        let ctx = match ctx {
            Some(c) => c,
            None => {
                debug!(
                    container_id = container_id,
                    "Container is not running or carries no labels this instance collects, skipping"
                );
                return Ok(());
            }
        };

        // Query the DB for the latest chunk end timestamp for this container.
        // On server restart, this prevents replaying the entire container history.
        //
        // We add +1 second because Docker's `since` parameter is second-resolution
        // AND inclusive — passing the raw `ended_at.timestamp()` would re-serve every
        // line stamped in that same second (the boot-time burst in crash-loop
        // scenarios was producing 10x duplicate rows per restart). Trading <1s of
        // coverage for zero duplicates is the right call; the dedup layer in
        // archive_search is the backstop for anything that still slips through.
        let resume_after = self
            .metadata_service
            .get_latest_chunk_end_for_container(container_id)
            .await
            .unwrap_or(None)
            .map(|ts| ts.timestamp().saturating_add(1))
            .unwrap_or(0);

        if resume_after > 0 {
            info!(
                container_id = container_id,
                resume_after_ts = resume_after,
                "Resuming log stream from last known position"
            );
        }

        // Resolve the daemon at streaming start — returns a typed error on a
        // control-plane process instead of spawning a task that immediately
        // fails with a connection error.
        let docker: Arc<Docker> = self
            .docker
            .require()
            .map_err(LogAggregatorError::DockerUnavailable)?;
        let chunk_writer = self.chunk_writer.clone();
        let tail_tx = self.tail_tx.clone();
        let container_id_owned = container_id.to_string();

        let handle = tokio::spawn(async move {
            Self::stream_container_logs(
                docker,
                chunk_writer,
                tail_tx,
                container_id_owned.clone(),
                ctx,
                resume_after,
            )
            .await;
        });

        let mut streams = self.active_streams.lock().await;
        streams.insert(container_id.to_string(), StreamTask { handle });

        info!(container_id = container_id, "Started log streaming");
        Ok(())
    }

    /// Start streaming a container, retrying in the background while the
    /// decision to collect it fails transiently.
    ///
    /// Discovery sees a container once — in the startup scan or on its
    /// Docker `start` event — so a failed [`Self::start_streaming`] would be
    /// final: a database blip during the ownership lookup would leave a
    /// running container's logs uncollected until it or temps restarted.
    /// A transient failure is instead retried (1s → 30s backoff) until the
    /// container streams, turns out not to be collectable, disappears, or is
    /// stopped. Returns `Ok` once a retry is scheduled; permanent failures
    /// are returned as before.
    pub async fn start_streaming_with_retry(
        self: &Arc<Self>,
        container_id: &str,
    ) -> Result<(), LogAggregatorError> {
        let lifecycle = self.lock_container(container_id).await;
        let error = match self.start_streaming_locked(container_id).await {
            Err(error) if Self::start_is_retryable(&error) => error,
            other => return other,
        };
        let scheduled = self
            .pending_starts
            .lock()
            .await
            .insert(container_id.to_string());
        drop(lifecycle);
        if !scheduled {
            // A retry for this container is already running.
            return Ok(());
        }
        warn!(
            container_id = container_id,
            error = %error,
            "Could not start collecting logs for container; retrying in the background"
        );
        let collector = Arc::clone(self);
        let container_id = container_id.to_string();
        tokio::spawn(async move { collector.retry_start(container_id).await });
        Ok(())
    }

    /// Whether a [`Self::start_streaming`] failure may clear on its own.
    /// A missing Docker daemon never will on this process, however its
    /// recovery classification reads.
    fn start_is_retryable(error: &LogAggregatorError) -> bool {
        !matches!(error, LogAggregatorError::DockerUnavailable(_))
            && error.retry_class() == RetryClass::Transient
    }

    async fn retry_start(self: Arc<Self>, container_id: String) {
        let mut delay = START_RETRY_INITIAL;
        let mut attempt: u32 = 0;
        loop {
            tokio::time::sleep(delay).await;
            // Held across the pending check AND the start, so a stop either
            // lands first (and this retry ends here) or waits and then tears
            // down the stream this attempt installs.
            let _lifecycle = self.lock_container(&container_id).await;
            if !self.pending_starts.lock().await.contains(&container_id) {
                debug!(
                    container_id = %container_id,
                    "Container stopped; abandoning log collection start retry"
                );
                return;
            }
            attempt += 1;
            match self.start_streaming_locked(&container_id).await {
                Err(error) if Self::start_is_retryable(&error) => {
                    if attempt.is_multiple_of(START_RETRY_LOG_EVERY) {
                        warn!(
                            container_id = %container_id,
                            attempt,
                            error = %error,
                            "Still cannot start collecting logs for container; retrying"
                        );
                    }
                    delay = std::cmp::min(delay * 2, START_RETRY_MAX);
                }
                result => {
                    self.pending_starts.lock().await.remove(&container_id);
                    match result {
                        Ok(()) => info!(
                            container_id = %container_id,
                            attempt,
                            "Started collecting logs for container after retrying"
                        ),
                        Err(error) => warn!(
                            container_id = %container_id,
                            attempt,
                            error = %error,
                            "Gave up starting log collection for container"
                        ),
                    }
                    return;
                }
            }
        }
    }

    /// Stop streaming logs for a container and flush remaining buffer.
    pub async fn stop_streaming(&self, container_id: &str) {
        let _lifecycle = self.lock_container(container_id).await;
        self.pending_starts.lock().await.remove(container_id);
        let task = {
            let mut streams = self.active_streams.lock().await;
            streams.remove(container_id)
        };

        if let Some(task) = task {
            task.handle.abort();
            // Flush (seal + commit the manifest) remaining lines; the writer
            // owns the whole seal pipeline now, so there is nothing left for
            // the caller to insert.
            if let Err(e) = self.chunk_writer.remove_container(container_id).await {
                warn!(
                    container_id = container_id,
                    error = %e,
                    "Failed to flush remaining lines on stop"
                );
            }
            info!(container_id = container_id, "Stopped log streaming");
        }
    }

    /// Stop all active streams.
    pub async fn stop_all(&self) {
        let container_ids: Vec<String> = {
            let streams = self.active_streams.lock().await;
            streams.keys().cloned().collect()
        };

        for container_id in container_ids {
            self.stop_streaming(&container_id).await;
        }
    }

    /// Get the list of currently streaming container IDs.
    pub async fn active_containers(&self) -> Vec<String> {
        let streams = self.active_streams.lock().await;
        streams.keys().cloned().collect()
    }

    /// Extract container context from Docker labels.
    async fn extract_context(
        &self,
        container_id: &str,
    ) -> Result<Option<ContainerContext>, LogAggregatorError> {
        let docker = self
            .docker
            .require()
            .map_err(LogAggregatorError::DockerUnavailable)?;
        let inspect = docker
            .inspect_container(
                container_id,
                None::<bollard::query_parameters::InspectContainerOptions>,
            )
            .await
            .map_err(|e| {
                if e.to_string().contains("404") || e.to_string().contains("No such container") {
                    LogAggregatorError::ContainerNotFound {
                        container_id: container_id.to_string(),
                    }
                } else {
                    LogAggregatorError::DockerStreamFailed {
                        container_id: container_id.to_string(),
                        reason: format!("Failed to inspect container: {}", e),
                    }
                }
            })?;

        // Discovery can act on a stale view: the startup scan listed the
        // container as running, or a start is retried, and it has exited
        // since. Its stop was already handled, so a stream opened now would
        // never be torn down and would make a later start skip it.
        if inspect.state.as_ref().and_then(|state| state.running) == Some(false) {
            return Ok(None);
        }

        // Real Docker container name (e.g. "legacy-postgres"), used to resolve
        // imported external-service containers that carry no temps.* labels.
        let container_name = inspect
            .name
            .as_deref()
            .map(|n| n.trim_start_matches('/').to_string());

        // Empty map when the container has no labels at all (e.g. an imported
        // pre-existing container) — still fall through to the external-service
        // resolution below rather than bailing out.
        let empty_labels = std::collections::HashMap::new();
        let labels = inspect
            .config
            .as_ref()
            .and_then(|c| c.labels.as_ref())
            .unwrap_or(&empty_labels);

        // Deployment/application containers carry `sh.temps.project_id`.
        // Everything else is a candidate external-service container: either
        // Temps-created (carries `temps.service_type`/`temps.service_name`
        // labels) or IMPORTED (no temps.* labels — resolved by matching the
        // real container name against `external_services.container_name`).
        if !labels.contains_key(LABEL_PROJECT_ID) {
            return self
                .extract_external_service_context(container_id, container_name.as_deref(), labels)
                .await;
        }

        let project_id = match labels.get(LABEL_PROJECT_ID) {
            Some(id) => match id.parse::<i32>() {
                Ok(pid) => pid,
                Err(_) => return Ok(None),
            },
            None => return Ok(None),
        };

        let env = labels
            .get(LABEL_ENV)
            .cloned()
            .unwrap_or_else(|| "default".to_string());
        let service = labels
            .get(LABEL_SERVICE)
            .cloned()
            .unwrap_or_else(|| "unknown".to_string());
        let deploy_id = labels
            .get(LABEL_DEPLOY_ID)
            .and_then(|id| id.parse::<i32>().ok());

        if !self
            .owns_project_container(container_id, project_id, &env, deploy_id)
            .await?
        {
            debug!(
                container_id,
                project_id,
                ?deploy_id,
                "Skipping container whose sh.temps.* labels name a deployment this instance does not own"
            );
            return Ok(None);
        }

        Ok(Some(ContainerContext {
            project_id,
            external_service_id: None,
            env,
            service,
            container_id: container_id.to_string(),
            deploy_id,
        }))
    }

    /// Whether a `sh.temps.project_id`-labelled container belongs to THIS
    /// instance. Labels are just strings on a Docker daemon that other Temps
    /// instances, test suites and user workloads share, so a label naming a
    /// project is not proof of ownership: the deployment it names must exist
    /// here, under the same project and environment. Without that check, a
    /// second instance's (or a test's) containers are collected under project
    /// ids this instance has never heard of.
    ///
    /// Containers without a `sh.temps.deploy_id` label only need their project
    /// to exist. With no DB handle (unit tests) nothing can be verified, so the
    /// labels are trusted as before.
    async fn owns_project_container(
        &self,
        container_id: &str,
        project_id: i32,
        env: &str,
        deploy_id: Option<i32>,
    ) -> Result<bool, LogAggregatorError> {
        use sea_orm::{ColumnTrait, EntityTrait, QueryFilter, QuerySelect};

        let Some(db) = self.db.as_ref() else {
            return Ok(true);
        };
        let lookup_failed =
            |source: sea_orm::DbErr| LogAggregatorError::ContainerContextLookupFailed {
                container_id: container_id.to_string(),
                source,
            };

        let found = match deploy_id {
            Some(deploy_id) => {
                use temps_entities::deployments::{Column, Entity};
                let mut query = Entity::find()
                    .select_only()
                    .column(Column::Id)
                    .filter(Column::Id.eq(deploy_id))
                    .filter(Column::ProjectId.eq(project_id));
                if let Ok(environment_id) = env.parse::<i32>() {
                    query = query.filter(Column::EnvironmentId.eq(environment_id));
                }
                query
                    .into_tuple::<i32>()
                    .one(db.as_ref())
                    .await
                    .map_err(lookup_failed)?
            }
            None => {
                use temps_entities::projects::{Column, Entity};
                Entity::find()
                    .select_only()
                    .column(Column::Id)
                    .filter(Column::Id.eq(project_id))
                    .into_tuple::<i32>()
                    .one(db.as_ref())
                    .await
                    .map_err(lookup_failed)?
            }
        };
        Ok(found.is_some())
    }

    /// Resolve an external-service container to its owning `external_services`
    /// row, returning a context keyed on `external_service_id` (with the
    /// `project_id = 0` sentinel, since a service isn't owned by one project).
    ///
    /// Three resolution paths, in order:
    ///  1. **Created standalone** services carry a `temps.service_name` label
    ///     → look the service up by name.
    ///  2. **Imported standalone** services' pre-existing containers have no
    ///     temps.* labels (Docker labels are immutable) → match the real
    ///     container name against the plaintext `external_services.container_name`
    ///     column stamped at import time.
    ///  3. **Cluster members** (monitor/primary/replica) carry deployment-style
    ///     `sh.temps.service.*` labels the standalone paths don't recognise, and
    ///     their per-member names live in `service_members`, not on the service
    ///     row. Match the real container name against `service_members.container_name`
    ///     → its `service_id` ties every member to the one external service, so
    ///     the whole cluster's logs aggregate under one `/storage/{id}/logs`.
    ///     The member container name becomes `service` so members stay
    ///     distinguishable within that scope.
    ///
    /// Returns `None` (container skipped) when none resolve, no DB handle is
    /// attached, or the container has no name.
    async fn extract_external_service_context(
        &self,
        container_id: &str,
        container_name: Option<&str>,
        labels: &std::collections::HashMap<String, String>,
    ) -> Result<Option<ContainerContext>, LogAggregatorError> {
        use sea_orm::{ColumnTrait, EntityTrait, QueryFilter};

        let Some(db) = self.db.as_ref() else {
            return Ok(None);
        };
        let prefix = temps_core::DOCKER_LABEL_PREFIX; // "temps."

        // Path 1: created standalone service — resolve by the temps.service_name label.
        if let Some(name) = labels.get(&format!("{prefix}service_name")) {
            let svc = temps_entities::external_services::Entity::find()
                .filter(temps_entities::external_services::Column::Name.eq(name.clone()))
                .one(db.as_ref())
                .await
                .map_err(|source| LogAggregatorError::ContainerContextLookupFailed {
                    container_id: container_id.to_string(),
                    source,
                })?;
            return Ok(svc.map(|svc| ContainerContext {
                project_id: 0,
                external_service_id: Some(svc.id),
                env: "default".to_string(),
                service: name.clone(),
                container_id: container_id.to_string(),
                deploy_id: None,
            }));
        }

        // Paths 2 & 3 both key off the real container name.
        let Some(cname) = container_name else {
            return Ok(None);
        };

        // Path 2: imported standalone service — real container name matches the
        // plaintext external_services.container_name stamped at import.
        if let Some(svc) = temps_entities::external_services::Entity::find()
            .filter(temps_entities::external_services::Column::ContainerName.eq(cname))
            .one(db.as_ref())
            .await
            .map_err(|source| LogAggregatorError::ContainerContextLookupFailed {
                container_id: container_id.to_string(),
                source,
            })?
        {
            return Ok(Some(ContainerContext {
                project_id: 0,
                external_service_id: Some(svc.id),
                env: "default".to_string(),
                service: svc.name,
                container_id: container_id.to_string(),
                deploy_id: None,
            }));
        }

        // Path 3: cluster member — real container name matches a
        // service_members row; its service_id is the owning external service.
        if let Some(member) = temps_entities::service_members::Entity::find()
            .filter(temps_entities::service_members::Column::ContainerName.eq(cname))
            .one(db.as_ref())
            .await
            .map_err(|source| LogAggregatorError::ContainerContextLookupFailed {
                container_id: container_id.to_string(),
                source,
            })?
        {
            return Ok(Some(ContainerContext {
                project_id: 0,
                external_service_id: Some(member.service_id),
                env: "default".to_string(),
                service: cname.to_string(),
                container_id: container_id.to_string(),
                deploy_id: None,
            }));
        }

        Ok(None)
    }

    /// Returns true if the error string indicates the container no longer exists.
    fn is_container_gone(error_msg: &str) -> bool {
        error_msg.contains("404")
            || error_msg.contains("No such container")
            || error_msg.contains("is not running")
            || error_msg.contains("removal in progress")
    }

    /// Build a `follow: true` log options set, resuming from `since_ts`.
    fn build_log_options(since_ts: i64) -> bollard::query_parameters::LogsOptions {
        LogsOptionsBuilder::new()
            .follow(true)
            .stdout(true)
            .stderr(true)
            .timestamps(true)
            .since(since_ts as i32)
            .build()
    }

    /// Internal: stream logs from a Docker container via the daemon API.
    ///
    /// On error: retries with exponential backoff, using the timestamp of the
    /// last successfully received line as `since` so reconnections don't create
    /// gaps (at worst, the same second is re-fetched — but not the full history).
    ///
    /// Gives up when:
    /// - The container is gone (404 / not running)
    /// - MAX_CONSECUTIVE_ERRORS consecutive failures are hit
    /// - The stream returns None (container stopped normally)
    async fn stream_container_logs(
        docker: Arc<Docker>,
        chunk_writer: Arc<ChunkWriterService>,
        tail_tx: broadcast::Sender<LogLine>,
        container_id: String,
        ctx: ContainerContext,
        resume_after: i64,
    ) {
        // Track the timestamp of the last successfully received line.
        // On reconnect we use this so we only re-fetch at most one second of overlap
        // instead of the entire history. On fresh startup, resume_after comes from
        // the DB (latest chunk ended_at), preventing replay of already-collected logs.
        let mut last_seen_ts: i64 = resume_after;
        let mut consecutive_errors: u32 = 0;
        let mut retry_delay = std::time::Duration::from_secs(1);
        let max_retry_delay = std::time::Duration::from_secs(30);

        let options = Self::build_log_options(last_seen_ts);
        let mut stream = docker.logs(&container_id, Some(options));

        loop {
            match stream.next().await {
                Some(Ok(output)) => {
                    // Reset error state on success
                    consecutive_errors = 0;
                    retry_delay = std::time::Duration::from_secs(1);

                    let (stream_type, raw) = match output {
                        bollard::container::LogOutput::StdOut { message } => (
                            LogStream::Stdout,
                            String::from_utf8_lossy(&message).to_string(),
                        ),
                        bollard::container::LogOutput::StdErr { message } => (
                            LogStream::Stderr,
                            String::from_utf8_lossy(&message).to_string(),
                        ),
                        _ => continue,
                    };

                    let (ts, msg) = parse_docker_timestamp(&raw);
                    let line = parse_log_line(msg, ts, stream_type, &ctx);

                    // Update last seen timestamp for reconnection
                    last_seen_ts = line.ts.timestamp();

                    // Send to live tail subscribers (ignore errors if no subscribers)
                    let _ = tail_tx.send(line.clone());

                    // Buffer the line. The writer owns sealing (object write +
                    // manifest commit) itself now — nothing left to do here.
                    if let Err(e) = chunk_writer.write_line(line).await {
                        error!(
                            container_id = container_id,
                            error = %e,
                            "Failed to write log line to chunk buffer"
                        );
                    }
                }
                Some(Err(e)) => {
                    let err_msg = e.to_string();

                    // If the container no longer exists, stop immediately
                    if Self::is_container_gone(&err_msg) {
                        info!(
                            container_id = container_id,
                            error = %e,
                            "Container is gone, stopping log stream"
                        );
                        break;
                    }

                    consecutive_errors += 1;
                    if consecutive_errors >= MAX_CONSECUTIVE_ERRORS {
                        error!(
                            container_id = container_id,
                            consecutive_errors = consecutive_errors,
                            "Max consecutive errors reached, giving up on container"
                        );
                        break;
                    }

                    warn!(
                        container_id = container_id,
                        error = %e,
                        retry_delay_secs = retry_delay.as_secs(),
                        consecutive_errors = consecutive_errors,
                        last_seen_ts = last_seen_ts,
                        "Docker log stream error, retrying"
                    );
                    tokio::time::sleep(retry_delay).await;
                    retry_delay = std::cmp::min(retry_delay * 2, max_retry_delay);

                    // Reconnect one second past the last line we successfully received.
                    // Docker's `since` parameter is integer seconds and is inclusive, so
                    // passing `last_seen_ts` would re-serve every line stamped at exactly
                    // that second — producing visible duplicates in the UI. Trading a
                    // worst-case <1s gap for no duplicates is the right default here;
                    // the dedup layer in archive_search catches anything that slips through.
                    let resume_from = last_seen_ts.saturating_add(1);
                    let reconnect_options = Self::build_log_options(resume_from);
                    stream = docker.logs(&container_id, Some(reconnect_options));
                }
                None => {
                    // Stream ended normally — the container stopped.
                    info!(
                        container_id = container_id,
                        "Docker log stream ended (container stopped)"
                    );
                    break;
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::storage::{FilesystemStorage, LogStorage};
    use sea_orm::{DatabaseBackend, MockDatabase};

    /// A [`crate::services::ManifestSink`] that never gets exercised by these
    /// tests (they only touch `extract_external_service_context`, never the
    /// chunk-writer seal path); it exists only so `ChunkWriterService::open`
    /// has something to hold.
    struct NoopManifestSink;

    #[async_trait::async_trait]
    impl crate::services::ManifestSink for NoopManifestSink {
        async fn insert(&self, _meta: &crate::types::ChunkMeta) -> Result<i64, LogAggregatorError> {
            Ok(0)
        }
    }

    /// Build a CollectorService backed by a MockDatabase. `extract_external_service_context`
    /// only touches `self.db`, so the Docker handle is never dialed — but `new`
    /// requires one, so we use a disabled handle (no daemon connection needed).
    async fn collector_with_db(db: Arc<sea_orm::DatabaseConnection>) -> CollectorService {
        let handle = Arc::new(temps_core::DockerHandle::disabled(
            "test",
            "no docker needed for db-only tests".to_string(),
        ));
        let tmp = tempfile::tempdir().unwrap();
        let storage: Arc<dyn LogStorage> =
            Arc::new(FilesystemStorage::new(tmp.path().to_path_buf()).unwrap());
        let chunk_writer =
            ChunkWriterService::open(storage, Arc::new(NoopManifestSink), None, None)
                .await
                .unwrap();
        let metadata = Arc::new(LogMetadataService::new(db.clone()));
        CollectorService::new(handle, chunk_writer, metadata, 16).with_db(db)
    }

    fn member(service_id: i32, container_name: &str) -> temps_entities::service_members::Model {
        temps_entities::service_members::Model {
            id: 1,
            service_id,
            node_id: None,
            role: "primary".into(),
            container_id: Some("abc123".into()),
            container_name: container_name.into(),
            hostname: None,
            port: Some(5432),
            compute_ip: None,
            status: "running".into(),
            ordinal: 1,
            config: None,
            provisioning_step: None,
            provisioning_error: None,
            created_at: chrono::Utc::now(),
            updated_at: chrono::Utc::now(),
        }
    }

    // A cluster member carries neither the standalone `temps.service_name` label
    // (Path 1) nor an `external_services.container_name` match (Path 2); it must
    // resolve via `service_members.container_name` → its owning service (Path 3),
    // so every member's logs aggregate under the one external service.
    #[tokio::test]
    async fn test_cluster_member_resolves_via_service_members() {
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            // Path 2: external_services by container_name → miss.
            .append_query_results(vec![Vec::<temps_entities::external_services::Model>::new()])
            // Path 3: service_members by container_name → the member.
            .append_query_results(vec![vec![member(7, "postgres-mydb-1")]])
            .into_connection();
        let collector = collector_with_db(Arc::new(db)).await;

        let labels = HashMap::new(); // no temps.service_name label
        let ctx = collector
            .extract_external_service_context("cid", Some("postgres-mydb-1"), &labels)
            .await
            .expect("resolution must not error")
            .expect("cluster member should resolve to its owning external service");

        assert_eq!(ctx.external_service_id, Some(7));
        assert_eq!(
            ctx.project_id, 0,
            "external-service chunks use the 0 sentinel"
        );
        assert_eq!(
            ctx.service, "postgres-mydb-1",
            "member container name is kept as `service` for per-member distinction"
        );
    }

    // A container that matches no standalone service and no cluster member is
    // skipped (None), not misattributed.
    #[tokio::test]
    async fn test_unknown_container_resolves_to_none() {
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results(vec![Vec::<temps_entities::external_services::Model>::new()])
            .append_query_results(vec![Vec::<temps_entities::service_members::Model>::new()])
            .into_connection();
        let collector = collector_with_db(Arc::new(db)).await;

        let labels = HashMap::new();
        let ctx = collector
            .extract_external_service_context("cid", Some("some-random-container"), &labels)
            .await
            .expect("resolution must not error");
        assert!(ctx.is_none(), "unresolved container must be skipped");
    }

    fn id_row(id: i32) -> std::collections::BTreeMap<&'static str, sea_orm::Value> {
        std::collections::BTreeMap::from([("id", sea_orm::Value::Int(Some(id)))])
    }

    // The container's labels name a deployment that exists here under the same
    // project and environment: it is ours, so it is collected.
    #[tokio::test]
    async fn test_owned_deployment_container_is_collected() {
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results(vec![vec![id_row(7)]])
            .into_connection();
        let collector = collector_with_db(Arc::new(db)).await;

        let owned = collector
            .owns_project_container("cid", 1, "1", Some(7))
            .await
            .expect("lookup must not error");
        assert!(owned);
    }

    // Another Temps instance (or a test suite) on the same Docker daemon labels
    // its containers `sh.temps.project_id=42`. No such deployment exists here,
    // so its logs must not be collected under a project this instance lacks.
    #[tokio::test]
    async fn test_foreign_deployment_container_is_skipped() {
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results(vec![
                Vec::<std::collections::BTreeMap<&str, sea_orm::Value>>::new(),
            ])
            .into_connection();
        let collector = collector_with_db(Arc::new(db)).await;

        let owned = collector
            .owns_project_container("cid", 42, "7", Some(3))
            .await
            .expect("lookup must not error");
        assert!(
            !owned,
            "a deployment this instance does not own must be skipped"
        );
    }

    // Without a deploy label the project itself must exist here.
    #[tokio::test]
    async fn test_unknown_project_without_deploy_label_is_skipped() {
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results(vec![
                Vec::<std::collections::BTreeMap<&str, sea_orm::Value>>::new(),
            ])
            .into_connection();
        let collector = collector_with_db(Arc::new(db)).await;

        let owned = collector
            .owns_project_container("cid", 2, "default", None)
            .await
            .expect("lookup must not error");
        assert!(!owned);
    }

    // A database blip while checking ownership must not read as "not ours":
    // it surfaces as a lookup failure that discovery retries, since the
    // container is seen only once and would otherwise never be collected.
    #[tokio::test]
    async fn test_ownership_lookup_connection_failure_is_retried() {
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_errors(vec![sea_orm::DbErr::Conn(sea_orm::RuntimeErr::Internal(
                "connection reset".into(),
            ))])
            .into_connection();
        let collector = collector_with_db(Arc::new(db)).await;

        let error = collector
            .owns_project_container("cid", 1, "1", Some(7))
            .await
            .expect_err("a failed lookup must not decide ownership");
        assert!(
            matches!(
                error,
                LogAggregatorError::ContainerContextLookupFailed { .. }
            ),
            "unexpected error: {error}"
        );
        assert!(CollectorService::start_is_retryable(&error));
    }

    // The same holds for the external-service lookups on unlabelled containers.
    #[tokio::test]
    async fn test_external_service_lookup_connection_failure_is_retried() {
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_errors(vec![sea_orm::DbErr::ConnectionAcquire(
                sea_orm::ConnAcquireErr::Timeout,
            )])
            .into_connection();
        let collector = collector_with_db(Arc::new(db)).await;

        let error = collector
            .extract_external_service_context("cid", Some("legacy-postgres"), &HashMap::new())
            .await
            .expect_err("a failed lookup must not skip the container");
        assert!(CollectorService::start_is_retryable(&error));
    }

    // Failures that cannot clear on their own are not retried: the container
    // is gone, this process has no Docker daemon, or the lookup itself is
    // malformed.
    #[test]
    fn test_permanent_start_failures_are_not_retried() {
        let permanent = [
            LogAggregatorError::ContainerNotFound {
                container_id: "cid".into(),
            },
            LogAggregatorError::DockerUnavailable(
                temps_core::DockerHandle::disabled("test", "no daemon")
                    .require()
                    .expect_err("a disabled handle has no daemon"),
            ),
            LogAggregatorError::ContainerContextLookupFailed {
                container_id: "cid".into(),
                source: sea_orm::DbErr::Custom("bad query".into()),
            },
        ];
        for error in &permanent {
            assert!(
                !CollectorService::start_is_retryable(error),
                "must not retry: {error}"
            );
        }
    }

    // A container that stops while its start is being retried must not be
    // picked up later: stopping cancels the retry.
    #[tokio::test(start_paused = true)]
    async fn test_stopping_a_container_cancels_its_start_retry() {
        let db = MockDatabase::new(DatabaseBackend::Postgres).into_connection();
        let collector = Arc::new(collector_with_db(Arc::new(db)).await);
        collector.pending_starts.lock().await.insert("cid".into());

        collector.stop_streaming("cid").await;
        assert!(collector.pending_starts.lock().await.is_empty());

        // The retry wakes, finds itself cancelled and ends without touching
        // Docker (which is disabled here and would fail permanently).
        tokio::time::timeout(
            std::time::Duration::from_secs(5),
            Arc::clone(&collector).retry_start("cid".into()),
        )
        .await
        .expect("a cancelled retry must end");
        assert!(collector.active_containers().await.is_empty());
    }

    // End to end against a real daemon: a running deployment container whose
    // ownership lookup fails once (database blip) is still collected, because
    // discovery retries it instead of dropping it for good.
    #[tokio::test]
    async fn test_container_is_collected_after_ownership_lookup_recovers() {
        let Some(docker) = local_docker().await else {
            return;
        };
        let id = labelled_container(&docker, "while true; do echo tick; sleep 1; done").await;

        // Ownership lookup fails, then succeeds on retry; the resume-point
        // lookup that follows finds no earlier chunk.
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_errors(vec![sea_orm::DbErr::Conn(sea_orm::RuntimeErr::Internal(
                "connection reset".into(),
            ))])
            .append_query_results(vec![vec![id_row(7)]])
            .append_query_results(vec![
                Vec::<std::collections::BTreeMap<&str, sea_orm::Value>>::new(),
            ])
            .into_connection();
        let (collector, _tmp) = docker_collector(docker.clone(), db).await;

        let scheduled = collector.start_streaming_with_retry(&id).await;
        let started_at_first = collector.active_containers().await.contains(&id);
        let mut streaming = false;
        for _ in 0..50 {
            if collector.active_containers().await.contains(&id) {
                streaming = true;
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        }
        collector.stop_streaming(&id).await;
        remove_container(&docker, &id).await;

        scheduled.expect("a transient failure schedules a retry instead of failing");
        assert!(!started_at_first, "the first attempt must have failed");
        assert!(streaming, "the retry must start collecting the container");
        assert!(collector.pending_starts.lock().await.is_empty());
    }

    /// A reachable local Docker daemon with `alpine:3` present, or `None`
    /// (the caller skips) on machines without one.
    async fn local_docker() -> Option<Arc<bollard::Docker>> {
        let docker = match bollard::Docker::connect_with_local_defaults() {
            Ok(docker) if docker.ping().await.is_ok() => Arc::new(docker),
            _ => {
                eprintln!("Skipping: Docker unavailable");
                return None;
            }
        };
        if docker.inspect_image("alpine:3").await.is_err() {
            eprintln!("Skipping: alpine:3 not present locally");
            return None;
        }
        Some(docker)
    }

    /// Start an `alpine:3` container labelled as deployment 7 of project 1.
    async fn labelled_container(docker: &bollard::Docker, script: &str) -> String {
        use bollard::models::ContainerCreateBody;
        use bollard::query_parameters::{CreateContainerOptions, StartContainerOptions};

        let labels = HashMap::from([
            (LABEL_PROJECT_ID.to_string(), "1".to_string()),
            (LABEL_ENV.to_string(), "1".to_string()),
            (LABEL_SERVICE.to_string(), "web".to_string()),
            (LABEL_DEPLOY_ID.to_string(), "7".to_string()),
        ]);
        let id = docker
            .create_container(
                None::<CreateContainerOptions>,
                ContainerCreateBody {
                    image: Some("alpine:3".to_string()),
                    cmd: Some(vec!["sh".into(), "-c".into(), script.into()]),
                    labels: Some(labels),
                    ..Default::default()
                },
            )
            .await
            .expect("create test container")
            .id;
        docker
            .start_container(&id, None::<StartContainerOptions>)
            .await
            .expect("start test container");
        id
    }

    async fn remove_container(docker: &bollard::Docker, id: &str) {
        let _ = docker
            .remove_container(
                id,
                Some(bollard::query_parameters::RemoveContainerOptions {
                    force: true,
                    ..Default::default()
                }),
            )
            .await;
    }

    async fn docker_collector(
        docker: Arc<bollard::Docker>,
        db: sea_orm::DatabaseConnection,
    ) -> (Arc<CollectorService>, tempfile::TempDir) {
        let db = Arc::new(db);
        let tmp = tempfile::tempdir().unwrap();
        let storage: Arc<dyn LogStorage> =
            Arc::new(FilesystemStorage::new(tmp.path().to_path_buf()).unwrap());
        let chunk_writer =
            ChunkWriterService::open(storage, Arc::new(NoopManifestSink), None, None)
                .await
                .unwrap();
        let collector = Arc::new(
            CollectorService::new(
                Arc::new(temps_core::DockerHandle::available(docker)),
                chunk_writer,
                Arc::new(LogMetadataService::new(db.clone())),
                16,
            )
            .with_db(db),
        );
        (collector, tmp)
    }

    // A stop that arrives while a start for the same container is in flight
    // (e.g. a retry still inspecting it) must wait for that start and then
    // tear down the stream it installed — not find nothing to cancel and
    // leave a stream behind that makes the next start event skip collection.
    #[tokio::test]
    async fn test_stop_during_an_in_flight_start_tears_down_what_it_installed() {
        let db = MockDatabase::new(DatabaseBackend::Postgres).into_connection();
        let collector = Arc::new(collector_with_db(Arc::new(db)).await);
        collector.pending_starts.lock().await.insert("cid".into());

        // The retry has passed its pending check and is mid-start.
        let in_flight_start = collector.lock_container("cid").await;
        let stopping = tokio::spawn({
            let collector = Arc::clone(&collector);
            async move { collector.stop_streaming("cid").await }
        });
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        assert!(
            !stopping.is_finished(),
            "stop must wait for the in-flight start"
        );
        assert!(collector.pending_starts.lock().await.contains("cid"));

        // The start completes by installing its stream.
        collector.active_streams.lock().await.insert(
            "cid".into(),
            StreamTask {
                handle: tokio::spawn(std::future::pending::<()>()),
            },
        );
        drop(in_flight_start);
        stopping.await.unwrap();

        assert!(collector.active_containers().await.is_empty());
        assert!(collector.pending_starts.lock().await.is_empty());
        assert!(
            collector.lifecycle_locks.lock().unwrap().is_empty(),
            "an idle container's lock must not be kept"
        );
    }

    // Discovery can act on a stale view (the startup scan listed it, or a
    // retry fires) after the container exited and its stop was handled. Even
    // with ownership confirmed, nothing is streamed for it.
    #[tokio::test]
    async fn test_exited_container_is_not_streamed() {
        let Some(docker) = local_docker().await else {
            return;
        };
        let id = labelled_container(&docker, "true").await;
        for _ in 0..50 {
            let running = docker
                .inspect_container(
                    &id,
                    None::<bollard::query_parameters::InspectContainerOptions>,
                )
                .await
                .ok()
                .and_then(|inspect| inspect.state.and_then(|state| state.running));
            if running == Some(false) {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        }
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results(vec![vec![id_row(7)]])
            .append_query_results(vec![
                Vec::<std::collections::BTreeMap<&str, sea_orm::Value>>::new(),
            ])
            .into_connection();
        let (collector, _tmp) = docker_collector(docker.clone(), db).await;

        let result = collector.start_streaming(&id).await;
        let streaming = collector.active_containers().await.contains(&id);
        remove_container(&docker, &id).await;

        result.expect("an exited container is skipped, not an error");
        assert!(!streaming, "an exited container must not be streamed");
    }
}
