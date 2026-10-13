// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Docker Cleanup Service
//!
//! Manages nightly cleanup of unused Docker images and build caches to save disk space.
//! Runs as a background task scheduled at 2 AM UTC daily.

use chrono::Timelike as _;
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use tokio::time::{sleep, Duration};
use tracing::{debug, error, info, warn};

/// Trait for Docker operations (mockable for testing)
#[async_trait::async_trait]
pub trait DockerClient: Send + Sync {
    /// Remove unused Docker images
    async fn prune_images(&self, force: bool) -> Result<PruneStats, String>;

    /// Remove unused Docker build cache
    async fn prune_builder_cache(&self, max_unused_days: i64) -> Result<String, String>;

    /// Remove the named images, returning a per-image outcome in the same
    /// order. Takes the whole batch (rather than one image per call) so the
    /// implementation opens a single Docker connection for the entire nightly
    /// pass instead of one per image.
    async fn remove_images(&self, image_names: &[String]) -> Vec<ImageRemovalOutcome>;

    /// Every `repo:tag` currently present on the local daemon. The retention
    /// pass only considers images that still exist here: once an image is
    /// removed its deployment rows keep naming it, so without this filter the
    /// oldest-first candidate window would refill with already-removed tags
    /// every night and never reach newer ones.
    async fn list_image_tags(&self) -> Result<Vec<String>, DockerImageListError>;
}

/// Why the local image listing used by the retention pass failed. Keeps the
/// underlying Docker error as its source instead of flattening it to text.
#[derive(Debug, thiserror::Error)]
pub enum DockerImageListError {
    #[error("Failed to connect to Docker daemon to list local images: {source}")]
    Connect {
        #[source]
        source: bollard::errors::Error,
    },

    #[error("Failed to list local Docker images (all=false): {source}")]
    List {
        #[source]
        source: bollard::errors::Error,
    },
}

/// Result of attempting to remove one image during the retention pass.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ImageRemovalOutcome {
    pub image_name: String,
    /// `None` on success, `Some(reason)` when Docker refused (most commonly
    /// because a container still references the image, which is the intended
    /// safety net rather than a real failure).
    pub error: Option<String>,
}

/// Statistics from Docker prune operations
#[derive(Debug, Clone)]
pub struct PruneStats {
    pub images_deleted: u64,
    pub space_reclaimed_mb: u64,
}

/// Default Docker client implementation using the Docker daemon
#[derive(Clone)]
pub struct DefaultDockerClient;

#[async_trait::async_trait]
impl DockerClient for DefaultDockerClient {
    async fn prune_images(&self, _force: bool) -> Result<PruneStats, String> {
        use bollard::query_parameters::PruneImagesOptionsBuilder;
        use bollard::Docker;
        use std::collections::HashMap;

        let docker = Docker::connect_with_unix_defaults()
            .map_err(|e| format!("Failed to connect to Docker daemon: {}", e))?;

        // Only prune images older than 7 days (168 hours)
        let mut filters: HashMap<String, Vec<String>> = HashMap::new();
        filters.insert("until".to_string(), vec!["168h".to_string()]);
        // Also only prune dangling images (not tagged)
        filters.insert("dangling".to_string(), vec!["true".to_string()]);

        let options = PruneImagesOptionsBuilder::default()
            .filters(&filters)
            .build();

        match docker.prune_images(Some(options)).await {
            Ok(result) => {
                let space_mb = result.space_reclaimed.unwrap_or(0) / (1024 * 1024);
                let count = result.images_deleted.map(|v| v.len()).unwrap_or(0) as u64;
                Ok(PruneStats {
                    images_deleted: count,
                    space_reclaimed_mb: space_mb as u64,
                })
            }
            Err(e) => Err(format!("Failed to prune images: {}", e)),
        }
    }

    async fn remove_images(&self, image_names: &[String]) -> Vec<ImageRemovalOutcome> {
        use bollard::Docker;

        // One connection for the whole batch.
        let docker = match Docker::connect_with_unix_defaults() {
            Ok(docker) => docker,
            Err(e) => {
                let reason = format!("Failed to connect to Docker daemon: {}", e);
                return image_names
                    .iter()
                    .map(|name| ImageRemovalOutcome {
                        image_name: name.clone(),
                        error: Some(reason.clone()),
                    })
                    .collect();
            }
        };

        let mut outcomes = Vec::with_capacity(image_names.len());
        for image_name in image_names {
            // Non-forced: Docker refuses while any container (running or
            // stopped) still references the image. That is deliberate — it is
            // the last line of defence behind the active-deployment guard.
            let result = docker
                .remove_image(
                    image_name,
                    Some(bollard::query_parameters::RemoveImageOptions {
                        force: false,
                        ..Default::default()
                    }),
                    None,
                )
                .await;

            outcomes.push(ImageRemovalOutcome {
                image_name: image_name.clone(),
                error: result
                    .err()
                    .map(|e| format!("Failed to remove image '{}': {}", image_name, e)),
            });
        }

        outcomes
    }

    async fn list_image_tags(&self) -> Result<Vec<String>, DockerImageListError> {
        use bollard::Docker;

        let docker = Docker::connect_with_unix_defaults()
            .map_err(|source| DockerImageListError::Connect { source })?;
        let images = docker
            .list_images(Some(bollard::query_parameters::ListImagesOptions {
                all: false,
                ..Default::default()
            }))
            .await
            .map_err(|source| DockerImageListError::List { source })?;

        Ok(images
            .into_iter()
            .flat_map(|image| image.repo_tags)
            .filter(|tag| tag != "<none>:<none>")
            .collect())
    }

    async fn prune_builder_cache(&self, max_unused_days: i64) -> Result<String, String> {
        use bollard::query_parameters::PruneBuildOptionsBuilder;
        use bollard::Docker;
        use std::collections::HashMap;

        let docker = Docker::connect_with_unix_defaults()
            .map_err(|e| format!("Failed to connect to Docker daemon: {}", e))?;

        // Calculate duration filter (e.g., "168h" for 7 days)
        let duration = format!("{}h", max_unused_days * 24);

        // Build filters with "until" to prune cache older than the specified duration
        let mut filters: HashMap<String, Vec<String>> = HashMap::new();
        filters.insert("until".to_string(), vec![duration]);

        let options = PruneBuildOptionsBuilder::default()
            // Without `all`, the Build/prune API only removes cache marked
            // "dangling" (not shared by any remaining build lineage) --
            // the same distinction as `docker image prune` vs `-a`. Most
            // build cache (e.g. per-build COPY/RUN layers with unique
            // content) is never dangling, so leaving this unset meant the
            // nightly cleanup reclaimed almost none of it regardless of
            // the `until` age filter, letting the cache grow unbounded.
            .all(true)
            .filters(&filters)
            .build();

        match docker.prune_build(Some(options)).await {
            Ok(result) => {
                let space_mb = result.space_reclaimed.unwrap_or(0) / (1024 * 1024);
                let caches_deleted = result.caches_deleted.map(|v| v.len()).unwrap_or(0);

                if caches_deleted > 0 || space_mb > 0 {
                    Ok(format!(
                        "removed {} build cache entries, freed {} MB",
                        caches_deleted, space_mb
                    ))
                } else {
                    Ok(String::new())
                }
            }
            Err(e) => Err(format!("Failed to prune build cache: {}", e)),
        }
    }
}

/// Calculate seconds until the next occurrence of `cleanup_hour` (UTC).
/// Shared by `DockerCleanupService` (console) and `DockerOnlyCleanupScheduler`
/// (worker agents) so both run on the same nightly cadence.
fn seconds_until_next_cleanup(cleanup_hour: u32) -> u64 {
    let now = chrono::Utc::now();

    // Calculate target time (today at cleanup_hour). `with_hour` etc. only
    // return None for an out-of-range component; every caller already
    // constrains `cleanup_hour` to 0..24 via `% 24`, but this runs on every
    // scheduler tick for the life of the process (console and every worker
    // agent), so a future caller skipping that guard must not crash the
    // spawned task -- retry in 24h instead of panicking.
    let target_time = match now
        .with_hour(cleanup_hour)
        .and_then(|t| t.with_minute(0))
        .and_then(|t| t.with_second(0))
    {
        Some(t) => t,
        None => {
            error!(
                cleanup_hour,
                "Invalid cleanup hour; docker cleanup scheduler will retry in 24h"
            );
            return 24 * 3600;
        }
    };

    let next_cleanup = if target_time > now {
        // Cleanup time hasn't passed today
        target_time
    } else {
        // Cleanup time already passed today, schedule for tomorrow
        target_time + chrono::Duration::days(1)
    };

    let duration = next_cleanup - now;
    duration.num_seconds().max(0) as u64
}

/// Prune unused Docker images and stale build cache via `docker_client`.
/// Shared by `DockerCleanupService` (console, which also cleans up the
/// DB-backed static asset cache) and `DockerOnlyCleanupScheduler` (worker
/// agents, which have no database connection).
async fn perform_docker_prune(docker_client: &Arc<dyn DockerClient>, max_cache_age_days: i64) {
    // Cleanup unused images
    match docker_client.prune_images(true).await {
        Ok(stats) => {
            if stats.images_deleted > 0 {
                info!(
                    "✅ Removed {} unused Docker images, freed {} MB",
                    stats.images_deleted, stats.space_reclaimed_mb
                );
            } else {
                info!("✅ No unused Docker images to remove");
            }
        }
        Err(e) => {
            error!("❌ Failed to prune Docker images: {}", e);
        }
    }

    // Cleanup old build cache
    match docker_client.prune_builder_cache(max_cache_age_days).await {
        Ok(output) => {
            // Parse output for statistics
            if output.contains("freed") || output.contains("removed") {
                info!("✅ Docker build cache cleanup completed: {}", output.trim());
            } else if output.is_empty() {
                info!("✅ No old Docker build cache to remove");
            } else {
                debug!("Docker build cache cleanup output: {}", output);
            }
        }
        Err(e) => {
            // Builder prune might not be available in all Docker versions
            warn!(
                "⚠️ Failed to prune Docker builder cache (may not be available): {}",
                e
            );
        }
    }
}

/// Lightweight Docker-only cleanup scheduler for hosts without a database
/// connection — namely worker agent nodes (`temps agent`), which are a
/// separate process from the console and never register the plugin system
/// that wires up `DockerCleanupService`. Worker nodes still build images
/// and accumulate build cache locally, so they need the same nightly
/// image + build-cache prune; they just skip the DB-backed static asset
/// cache and chunk cleanup steps, which are console-only concerns.
pub struct DockerOnlyCleanupScheduler {
    docker_client: Arc<dyn DockerClient>,
    /// Hour of day (UTC) to run cleanup (default: 2 AM)
    cleanup_hour: u32,
    /// Maximum number of days build cache can be unused before deletion (default: 7)
    max_cache_age_days: i64,
}

impl DockerOnlyCleanupScheduler {
    pub fn new(docker_client: Arc<dyn DockerClient>) -> Self {
        Self {
            docker_client,
            cleanup_hour: 2, // 2 AM UTC
            max_cache_age_days: 7,
        }
    }

    pub fn with_cleanup_hour(mut self, hour: u32) -> Self {
        self.cleanup_hour = hour % 24;
        self
    }

    pub fn with_max_cache_age_days(mut self, days: i64) -> Self {
        self.max_cache_age_days = days;
        self
    }

    /// Start the cleanup scheduler (blocking, should be spawned in a tokio task).
    pub async fn start_cleanup_scheduler(&self) {
        info!(
            "Docker cleanup scheduler started (agent node, cleanup hour: {}:00 UTC)",
            self.cleanup_hour
        );

        loop {
            let seconds_until_cleanup = seconds_until_next_cleanup(self.cleanup_hour);
            let hours = seconds_until_cleanup / 3600;
            let minutes = (seconds_until_cleanup % 3600) / 60;

            debug!(
                "Next Docker cleanup scheduled in {} hours {} minutes",
                hours, minutes
            );

            sleep(Duration::from_secs(seconds_until_cleanup)).await;

            info!("🧹 Starting nightly Docker cleanup (agent node)");
            perform_docker_prune(&self.docker_client, self.max_cache_age_days).await;
            info!("Nightly Docker cleanup completed (agent node)");

            // Sleep for 1 minute to avoid running cleanup multiple times in the same minute
            sleep(Duration::from_secs(60)).await;
        }
    }
}

/// Docker cleanup service that runs nightly
pub struct DockerCleanupService {
    docker_client: Arc<dyn DockerClient>,
    db: Arc<temps_database::DbConnection>,
    file_store: Arc<dyn temps_file_store::FileStore>,
    /// Hour of day (UTC) to run cleanup (default: 2 AM)
    cleanup_hour: u32,
    /// Maximum number of days build cache can be unused before deletion (default: 7)
    max_cache_age_days: i64,
    /// Base directory for static files (for persisted chunks cleanup)
    static_dir: Option<PathBuf>,
    /// Maximum age of persisted chunk directories in hours (default: 24)
    max_chunk_age_hours: u64,
    /// Maximum age of static asset cache entries in days (default: 7)
    max_asset_cache_age_days: i64,
    /// Number of most-recent deployment images to always keep per
    /// project+environment, regardless of age (default: 5)
    keep_recent_deployment_images: u64,
    /// Maximum number of stale deployment images to remove in a single
    /// nightly run (default: 500). Bounds memory and Docker API calls on
    /// installs with very large deployment histories; any remainder is
    /// picked up on the following night's run.
    max_deployment_images_per_run: u64,
    /// System-wide default: how many hours to keep a built deployment image before
    /// it is eligible for removal. Projects can override this via their
    /// `image_retention_hours` column. Sourced from
    /// `AppSettings.image_retention.default_hours` at startup.
    default_image_retention_hours: i64,
    /// When false, the retention pass is skipped entirely and built images are
    /// kept forever. Sourced from `AppSettings.image_retention.enabled`.
    image_retention_enabled: bool,
    /// When set, `prune_old_deployment_images` re-reads `image_retention`
    /// from this service at the start of every run instead of relying on the
    /// `image_retention_enabled`/`default_image_retention_hours` snapshot
    /// taken at construction. Without this, an operator disabling retention
    /// via the settings UI would have no effect until the process restarts —
    /// `ConfigService::get_settings` is TTL-cached (5s, NOTIFY-backed), so
    /// this costs nothing on a job that runs once nightly. `None` in tests
    /// that construct the service directly, which fall back to the
    /// constructor/builder-supplied values.
    config_service: Option<Arc<temps_config::ConfigService>>,
    /// (enabled, default_hours) from the most recent successful read —
    /// either the constructor/builder snapshot, or the latest live read via
    /// `config_service`. Used as the fallback on a *failed* live read
    /// instead of always reverting to the boot-time snapshot: an operator
    /// who disabled retention, followed by a transient settings-read error,
    /// must not have deletions silently resume under the stale boot policy.
    last_known_image_retention: std::sync::Mutex<(bool, i64)>,
}

impl DockerCleanupService {
    pub fn new(
        docker_client: Arc<dyn DockerClient>,
        db: Arc<temps_database::DbConnection>,
        file_store: Arc<dyn temps_file_store::FileStore>,
    ) -> Self {
        Self {
            docker_client,
            db,
            file_store,
            cleanup_hour: 2, // 2 AM UTC
            max_cache_age_days: 7,
            static_dir: None,
            max_chunk_age_hours: 24,
            max_asset_cache_age_days: 7,
            keep_recent_deployment_images: 5,
            max_deployment_images_per_run: 500,
            // Mirrors ImageRetentionSettings::default(). Deleting an image
            // makes rollback to that deployment impossible, so this is a
            // rollback window (14 days), not a cache TTL.
            default_image_retention_hours: 336,
            image_retention_enabled: true,
            config_service: None,
            last_known_image_retention: std::sync::Mutex::new((true, 336)),
        }
    }

    pub fn with_static_dir(mut self, static_dir: PathBuf) -> Self {
        self.static_dir = Some(static_dir);
        self
    }

    pub fn with_cleanup_hour(mut self, hour: u32) -> Self {
        self.cleanup_hour = hour % 24;
        self
    }

    pub fn with_max_cache_age_days(mut self, days: i64) -> Self {
        self.max_cache_age_days = days;
        self
    }

    pub fn with_max_asset_cache_age_days(mut self, days: i64) -> Self {
        self.max_asset_cache_age_days = days;
        self
    }

    pub fn with_keep_recent_deployment_images(mut self, count: u64) -> Self {
        self.keep_recent_deployment_images = count;
        self
    }

    pub fn with_max_deployment_images_per_run(mut self, count: u64) -> Self {
        self.max_deployment_images_per_run = count;
        self
    }

    /// Apply the operator-configured retention policy from `AppSettings`.
    /// Used as the boot-time value and, until the first live read completes,
    /// as the `last_known_image_retention` fallback.
    pub fn with_image_retention(mut self, settings: &temps_core::ImageRetentionSettings) -> Self {
        self.image_retention_enabled = settings.enabled;
        self.default_image_retention_hours = settings.effective_default_hours();
        self.last_known_image_retention = std::sync::Mutex::new((
            self.image_retention_enabled,
            self.default_image_retention_hours,
        ));
        self
    }

    /// Re-read `image_retention` from this service at the start of every
    /// nightly run instead of only once at construction, so an operator
    /// disabling retention takes effect on the very next run rather than
    /// requiring a restart.
    pub fn with_config_service(mut self, config_service: Arc<temps_config::ConfigService>) -> Self {
        self.config_service = Some(config_service);
        self
    }

    /// Current retention policy: the fresh settings-row value when a
    /// `ConfigService` is wired up, falling back to `last_known_image_retention`
    /// otherwise (all existing tests, before the first live read) or on a
    /// read error. Deliberately the *last successful* read rather than the
    /// boot-time snapshot: an operator who disabled retention and then hit a
    /// transient settings-read error must not have deletions silently resume
    /// under a stale "enabled" policy from startup.
    async fn current_image_retention(&self) -> (bool, i64) {
        let Some(config_service) = &self.config_service else {
            return *self
                .last_known_image_retention
                .lock()
                .expect("last_known_image_retention mutex poisoned");
        };
        match config_service.get_settings().await {
            Ok(settings) => {
                let fresh = (
                    settings.image_retention.enabled,
                    settings.image_retention.effective_default_hours(),
                );
                *self
                    .last_known_image_retention
                    .lock()
                    .expect("last_known_image_retention mutex poisoned") = fresh;
                fresh
            }
            Err(e) => {
                let last_known = *self
                    .last_known_image_retention
                    .lock()
                    .expect("last_known_image_retention mutex poisoned");
                error!(
                    error = %e,
                    "Could not read image retention settings; using last-known values"
                );
                last_known
            }
        }
    }

    /// Mark an image as permanently protected, whatever its age.
    ///
    /// Used for images we cannot rebuild (uploaded tarballs, external
    /// registry pulls) and for the image each environment is currently
    /// serving. `insert` rather than `and_modify` so protection wins
    /// regardless of the order references are visited in. No name filter:
    /// which images are candidates at all is decided by
    /// `expired_image_candidates` from the builder's own records, so a
    /// protection entry for any other name is simply never consulted.
    fn protect_image(candidates: &mut HashMap<String, bool>, image_name: &str) {
        candidates.insert(image_name.to_string(), false);
    }

    /// Calculate seconds until the next scheduled cleanup
    fn seconds_until_next_cleanup(&self) -> u64 {
        seconds_until_next_cleanup(self.cleanup_hour)
    }

    /// Start the cleanup scheduler (blocking, should be spawned in tokio task)
    pub async fn start_cleanup_scheduler(&self) {
        info!(
            "Docker cleanup scheduler started (cleanup hour: {}:00 UTC)",
            self.cleanup_hour
        );

        loop {
            let seconds_until_cleanup = self.seconds_until_next_cleanup();
            let hours = seconds_until_cleanup / 3600;
            let minutes = (seconds_until_cleanup % 3600) / 60;

            debug!(
                "Next Docker cleanup scheduled in {} hours {} minutes",
                hours, minutes
            );

            sleep(Duration::from_secs(seconds_until_cleanup)).await;

            // Run cleanup
            self.perform_cleanup().await;

            // Sleep for 1 minute to avoid running cleanup multiple times in the same minute
            sleep(Duration::from_secs(60)).await;
        }
    }

    /// Remove deployment images that are older than their project's retention period.
    ///
    /// An image is eligible only when **every** deployment row that references it is
    /// older than its owning project's retention period, so an image reused by a newer
    /// rollback or promotion survives. On top of that expiry rule three categories are
    /// protected unconditionally, whatever their age:
    ///
    /// 1. **Images we cannot rebuild** — uploaded tarballs (`POST .../deployments/upload`)
    ///    and external registry pulls. There is no source to build them from again, so
    ///    removing one is data loss, not disk reclamation.
    /// 2. **The image each environment is currently serving**
    ///    (`environments.current_deployment_id`), so a live service can never lose the
    ///    image it is running even if it has not been redeployed in months.
    /// 3. **Images belonging to deployments on other nodes** — this pass only talks to
    ///    the local Docker daemon, so it only considers deployments whose containers
    ///    live on the control plane.
    ///
    /// The newest `keep_recent_deployment_images` deployment rows in each
    /// project+environment are retained as a rollback floor, and each pass removes at
    /// most `max_deployment_images_per_run` candidates, oldest first. Only tags the
    /// Temps image builder recorded producing, and which still exist on the local
    /// daemon, are considered; registry and uploaded images are left alone. Docker
    /// removal is non-forced, so an image still referenced by any container is
    /// retained as a final safety net.
    async fn prune_old_deployment_images(&self) {
        let (image_retention_enabled, default_image_retention_hours) =
            self.current_image_retention().await;
        if !image_retention_enabled {
            debug!("Deployment image retention is disabled; skipping");
            return;
        }

        let local_image_tags = match self.docker_client.list_image_tags().await {
            Ok(tags) => tags,
            Err(e) => {
                error!(
                    error = %e,
                    "Could not list local Docker images; skipping image retention this run"
                );
                return;
            }
        };
        if local_image_tags.is_empty() {
            debug!("No tagged local Docker images; skipping image retention");
            return;
        }

        // Eligibility (and the oldest reference per image, used to order
        // removal) is computed inside Postgres via GROUP BY/HAVING rather
        // than by pulling every deployment row into the process, and capped
        // at `max_deployment_images_per_run` — see the doc comment on
        // `expired_image_candidates` for why this table's row count doesn't
        // shrink to "how many images exist".
        let (mut candidates, oldest_reference_by_image) = match self
            .expired_image_candidates(default_image_retention_hours, &local_image_tags)
            .await
        {
            Ok(result) => result,
            Err(e) => {
                error!(
                    error = %e,
                    "Could not compute deployment image retention eligibility; \
                     skipping image retention this run"
                );
                return;
            }
        };

        if candidates.is_empty() {
            debug!("No expired Temps-built deployment images to consider");
            return;
        }

        // Preserve main's rollback floor from #645: regardless of the age
        // policy, keep the newest N deployment images for every
        // project+environment. Computed via a window function so the result
        // is bounded by (project, environment) pairs × N, not by total
        // deployment history.
        match self
            .recently_protected_image_names(self.keep_recent_deployment_images)
            .await
        {
            Ok(names) => {
                for name in &names {
                    Self::protect_image(&mut candidates, name);
                }
            }
            Err(e) => {
                error!(
                    error = %e,
                    "Could not determine recently-deployed images; skipping image \
                     retention this run to avoid removing a rollback target"
                );
                return;
            }
        }

        // Protect images that cannot be rebuilt from source.
        match self.unrebuildable_image_names().await {
            Ok(names) => {
                for name in &names {
                    Self::protect_image(&mut candidates, name);
                }
                debug!(
                    protected = names.len(),
                    "Protected non-rebuildable deployment images from retention"
                );
            }
            Err(e) => {
                // Failing open here would delete uploaded images. Abort the
                // whole pass instead and say so — a skipped night costs disk,
                // a wrong deletion costs the user their deployment.
                error!(
                    error = %e,
                    "Could not determine which deployment images are rebuildable; \
                     skipping image retention this run to avoid deleting an \
                     unrecoverable image"
                );
                return;
            }
        }

        // Protect whatever each environment is currently serving. Bounded by
        // environment count via a direct join rather than a scan of
        // deployment history.
        match self.actively_served_image_names().await {
            Ok(names) => {
                for name in &names {
                    Self::protect_image(&mut candidates, name);
                }
            }
            Err(e) => {
                error!(
                    error = %e,
                    "Could not determine active deployments; skipping image \
                     retention this run to avoid removing a live image"
                );
                return;
            }
        }

        // Protect images whose containers live on a worker node — this pass
        // only speaks to the local Docker daemon, so a remote image is not
        // ours to remove and a failed removal here would be pure log noise.
        match self.remote_node_image_names().await {
            Ok(names) => {
                for name in &names {
                    Self::protect_image(&mut candidates, name);
                }
                if !names.is_empty() {
                    debug!(
                        remote = names.len(),
                        "Skipping deployment images owned by worker nodes"
                    );
                }
            }
            Err(e) => {
                error!(
                    error = %e,
                    "Could not determine which deployment images are node-local; \
                     skipping image retention this run"
                );
                return;
            }
        }

        let mut expired: Vec<(String, chrono::DateTime<chrono::Utc>)> = candidates
            .into_iter()
            .filter_map(|(name, eligible)| {
                eligible.then(|| {
                    let oldest = oldest_reference_by_image
                        .get(&name)
                        .copied()
                        .unwrap_or_else(chrono::Utc::now);
                    (name, oldest)
                })
            })
            .collect();

        // Preserve main's bounded nightly work from #645. Oldest candidates
        // go first; the remainder is picked up on later runs.
        expired.sort_by_key(|(_, oldest)| *oldest);
        expired.truncate(self.max_deployment_images_per_run.min(usize::MAX as u64) as usize);
        let expired: Vec<String> = expired.into_iter().map(|(name, _)| name).collect();

        if expired.is_empty() {
            debug!("No expired deployment images to remove");
            return;
        }

        let outcomes = self.docker_client.remove_images(&expired).await;
        let removed = outcomes.iter().filter(|o| o.error.is_none()).count();
        let failed = outcomes.len() - removed;

        for outcome in outcomes.iter().filter(|o| o.error.is_some()) {
            // Most commonly "image is being used by container" — the intended
            // safety net rather than a real failure. Logged individually so an
            // operator debugging disk usage can see exactly what was retained.
            warn!(
                image_name = %outcome.image_name,
                error = %outcome.error.as_deref().unwrap_or_default(),
                "Could not remove expired deployment image"
            );
        }

        // Report retained-vs-removed explicitly. Reporting only successes made
        // a run where every removal failed look identical to a run with
        // nothing to do.
        if failed > 0 {
            info!(
                "🧹 Deployment image retention: removed {}, retained {} (still referenced or already gone)",
                removed, failed
            );
        } else {
            info!("✅ Removed {} expired deployment images", removed);
        }
    }

    /// Per-image eligibility and oldest reference, computed inside Postgres.
    ///
    /// **What is a candidate.** Only a tag the Temps image builder recorded
    /// producing: the `image_tag` output a successful `BuildImageJob`
    /// persists to `deployment_jobs.outputs`. Eligibility is never inferred
    /// from the tag text. The builder tags `<deployment slug>:latest`, so an
    /// earlier `image_name LIKE 'temps-%'` filter matched only projects whose
    /// slug happened to start with `temps-` and left every other project's
    /// images on disk forever. Provenance from the build job also covers a
    /// build that succeeded ahead of a deploy that failed: the image is on
    /// disk, but `deployments.image_name` is only written on completion and
    /// stays NULL. External pulls and uploads never run a `BuildImageJob`,
    /// so they can't become candidates this way. Candidates are further
    /// restricted to `local_image_tags`, the tags that still exist on this
    /// daemon, so an image removed on an earlier night (whose deployment rows
    /// still name it) does not take up a slot in the `LIMIT` window again.
    ///
    /// **When it is eligible.** A built image's references are the
    /// deployment whose build produced it plus every deployment that records
    /// it in `image_name` (rollbacks and promotions reuse the tag). The image
    /// is eligible only when **every** reference is older than its owning
    /// project's retention window (`BOOL_AND`, with the per-project override
    /// applied via `COALESCE`), so an image reused by a newer rollback
    /// survives, and only when **no** reference is
    ///
    /// - still in flight (a deployment that built its image and is still
    ///   rolling out has no `image_name` yet, and nothing else protects it),
    /// - unrebuildable (upload trigger, external image metadata, or a
    ///   non-git project, the same predicate as `unrebuildable_image_names`),
    /// - or running containers on a worker node (`remote_node_image_names`).
    ///
    /// Each protective flag is `COALESCE`d to `TRUE`, so a NULL column
    /// protects the image rather than releasing it.
    ///
    /// The permanent protections are applied here, before `LIMIT`, and not
    /// only downstream via `protect_image`. An install with
    /// `max_deployment_images_per_run` or more old, permanently protected
    /// images would otherwise fill the whole `ORDER BY MIN(created_at) ASC
    /// LIMIT` window with rows that `protect_image` then throws away, and
    /// every run would reclaim nothing instead of picking up the remainder
    /// on the next run. Recency-based protections (keep-recent-N,
    /// actively-served) don't need this: they cover the *newest*
    /// deployments, so they never compete for the oldest-first slots.
    /// `HAVING` drops non-expired groups before they leave Postgres, and
    /// `LIMIT` gives the candidate scan the same bound as the removal batch.
    async fn expired_image_candidates(
        &self,
        default_hours: i64,
        local_image_tags: &[String],
    ) -> Result<
        (
            HashMap<String, bool>,
            HashMap<String, chrono::DateTime<chrono::Utc>>,
        ),
        sea_orm::DbErr,
    > {
        use sea_orm::{ActiveEnum, ConnectionTrait, Statement};

        let limit = i64::try_from(self.max_deployment_images_per_run).unwrap_or(i64::MAX);
        let build_succeeded = temps_entities::types::JobStatus::Success.to_value();
        let local_image_tags = serde_json::Value::from(local_image_tags.to_vec());
        let rows = self
            .db
            .as_ref()
            .query_all(Statement::from_sql_and_values(
                sea_orm::DatabaseBackend::Postgres,
                r#"
                WITH built AS (
                    SELECT DISTINCT
                        dj.deployment_id,
                        dj.outputs ->> 'image_tag' AS image_name
                    FROM deployment_jobs dj
                    WHERE dj.job_type = 'BuildImageJob'
                      AND dj.status = $3
                      AND dj.outputs ->> 'image_tag' IS NOT NULL
                ),
                local_images AS (
                    SELECT jsonb_array_elements_text($4::jsonb) AS image_name
                ),
                candidate_images AS (
                    SELECT DISTINCT b.image_name
                    FROM built b
                    JOIN local_images li ON li.image_name = b.image_name
                    WHERE b.image_name NOT LIKE '%/%'
                ),
                refs AS (
                    SELECT b.image_name, b.deployment_id
                    FROM built b
                    JOIN candidate_images ci ON ci.image_name = b.image_name
                    UNION
                    SELECT ci.image_name, d.id AS deployment_id
                    FROM candidate_images ci
                    JOIN deployments d ON d.image_name = ci.image_name
                ),
                ref_rows AS (
                    SELECT
                        r.image_name,
                        d.created_at,
                        d.created_at < NOW() - (
                            COALESCE(p.image_retention_hours, $1) * INTERVAL '1 hour'
                        ) AS expired,
                        COALESCE(
                            d.state IN (
                                'pending', 'creating', 'running',
                                'in_progress', 'built', 'paused'
                            ),
                            TRUE
                        ) AS in_flight,
                        COALESCE(
                            COALESCE(d.context_vars ->> 'trigger', '') = 'image_upload'
                         OR d.metadata ->> 'externalImageRef' IS NOT NULL
                         OR d.metadata ->> 'externalImageId' IS NOT NULL
                         OR p.source_type <> 'git',
                            TRUE
                        ) AS unrebuildable,
                        EXISTS (
                            SELECT 1
                            FROM deployment_containers dcr
                            WHERE dcr.deployment_id = d.id
                              AND dcr.node_id IS NOT NULL
                        ) AS on_remote_node
                    FROM refs r
                    JOIN deployments d ON d.id = r.deployment_id
                    JOIN projects p ON p.id = d.project_id
                )
                SELECT
                    image_name,
                    MIN(created_at) AS oldest_reference
                FROM ref_rows
                GROUP BY image_name
                HAVING BOOL_AND(expired)
                   AND NOT BOOL_OR(in_flight)
                   AND NOT BOOL_OR(unrebuildable)
                   AND NOT BOOL_OR(on_remote_node)
                ORDER BY MIN(created_at) ASC
                LIMIT $2
                "#,
                vec![
                    default_hours.into(),
                    limit.into(),
                    build_succeeded.into(),
                    local_image_tags.into(),
                ],
            ))
            .await?;

        let mut candidates = HashMap::new();
        let mut oldest_reference_by_image = HashMap::new();
        for row in rows {
            let image_name: String = row.try_get("", "image_name")?;
            let oldest_reference: chrono::DateTime<chrono::Utc> =
                row.try_get("", "oldest_reference")?;
            oldest_reference_by_image.insert(image_name.clone(), oldest_reference);
            // Every row that survives the query's HAVING clause is, by
            // definition, fully expired — `protect_image` (called for the
            // recent/unrebuildable/active/remote-node sets below) is what
            // flips an entry back to `false`.
            candidates.insert(image_name, true);
        }

        Ok((candidates, oldest_reference_by_image))
    }

    /// Image names among the newest `keep_recent` deployments for each
    /// project+environment scope, via a window function. Bounded by
    /// (project, environment) pairs × `keep_recent`, not total deployment
    /// history.
    async fn recently_protected_image_names(
        &self,
        keep_recent: u64,
    ) -> Result<Vec<String>, sea_orm::DbErr> {
        use sea_orm::{ConnectionTrait, Statement};

        let rows = self
            .db
            .as_ref()
            .query_all(Statement::from_sql_and_values(
                sea_orm::DatabaseBackend::Postgres,
                r#"
                SELECT image_name
                FROM (
                    SELECT
                        image_name,
                        ROW_NUMBER() OVER (
                            PARTITION BY project_id, environment_id
                            ORDER BY created_at DESC
                        ) AS rn
                    FROM deployments
                    WHERE image_name IS NOT NULL
                ) ranked
                WHERE rn <= $1
                "#,
                // `as i64` would silently wrap a value above i64::MAX negative,
                // making `rn <= $1` match zero rows and emptying the
                // rollback-floor protection right before an irreversible
                // delete — every other error path in this function fails
                // closed (aborts the run), so this must too.
                vec![i64::try_from(keep_recent).unwrap_or(i64::MAX).into()],
            ))
            .await?;

        rows.into_iter()
            .map(|row| row.try_get::<String>("", "image_name"))
            .collect()
    }

    /// Image names for whatever each environment is currently serving.
    /// Bounded by environment count via a direct join, not deployment
    /// history size.
    async fn actively_served_image_names(&self) -> Result<Vec<String>, sea_orm::DbErr> {
        use sea_orm::{ConnectionTrait, Statement};

        let rows = self
            .db
            .as_ref()
            .query_all(Statement::from_string(
                sea_orm::DatabaseBackend::Postgres,
                r#"
                SELECT DISTINCT d.image_name
                FROM environments e
                JOIN deployments d ON d.id = e.current_deployment_id
                WHERE e.current_deployment_id IS NOT NULL
                  AND d.image_name IS NOT NULL
                "#,
            ))
            .await?;

        rows.into_iter()
            .map(|row| row.try_get::<String>("", "image_name"))
            .collect()
    }

    /// Image names that must never be pruned because Temps cannot rebuild them:
    /// tarballs uploaded via the image-upload API and external registry images.
    ///
    /// Matched on the deployment's own provenance rather than on the tag text,
    /// because the upload endpoint lets the caller supply an arbitrary `tag`.
    ///
    /// `source_type <> 'git'` is deliberately broader than strictly necessary:
    /// a `Manual`-source project (`SourceType::Manual`, "accepts any
    /// deployment method") *can* deploy from a rebuildable git-built image,
    /// but the project row doesn't distinguish that case from a genuinely
    /// unrebuildable one, so every image from a non-`Git` project is
    /// protected. This costs disk on `Manual` projects that happen to be
    /// rebuildable; the alternative — trying to infer rebuildability from
    /// something other than the source_type column — risks a false negative
    /// that permanently deletes an image with no way back. Over-retention is
    /// the safe direction here; under-retention is not.
    async fn unrebuildable_image_names(&self) -> Result<Vec<String>, sea_orm::DbErr> {
        use sea_orm::{ConnectionTrait, Statement};

        // No user input is interpolated — this is a fixed predicate over JSON
        // columns that Sea-ORM's query builder cannot express directly.
        let rows = self
            .db
            .as_ref()
            .query_all(Statement::from_string(
                sea_orm::DatabaseBackend::Postgres,
                r#"
                SELECT DISTINCT d.image_name
                FROM deployments d
                JOIN projects p ON p.id = d.project_id
                WHERE d.image_name IS NOT NULL
                  AND (
                        d.context_vars ->> 'trigger' = 'image_upload'
                     OR d.metadata ->> 'externalImageRef' IS NOT NULL
                     OR d.metadata ->> 'externalImageId' IS NOT NULL
                     OR p.source_type <> 'git'
                  )
                "#,
            ))
            .await?;

        rows.into_iter()
            .map(|row| row.try_get::<String>("", "image_name"))
            .collect()
    }

    /// Image names whose deployment containers are recorded against a worker
    /// node rather than the control plane.
    async fn remote_node_image_names(&self) -> Result<Vec<String>, sea_orm::DbErr> {
        use sea_orm::{ConnectionTrait, Statement};

        let rows = self
            .db
            .as_ref()
            .query_all(Statement::from_string(
                sea_orm::DatabaseBackend::Postgres,
                r#"
                SELECT DISTINCT d.image_name
                FROM deployments d
                JOIN deployment_containers dc ON dc.deployment_id = d.id
                WHERE d.image_name IS NOT NULL
                  AND dc.node_id IS NOT NULL
                "#,
            ))
            .await?;

        rows.into_iter()
            .map(|row| row.try_get::<String>("", "image_name"))
            .collect()
    }

    /// Perform the actual cleanup
    async fn perform_cleanup(&self) {
        info!("🧹 Starting nightly Docker cleanup");

        perform_docker_prune(&self.docker_client, self.max_cache_age_days).await;

        // Remove old deployment images per project retention policy
        self.prune_old_deployment_images().await;

        // Cleanup old persisted static asset chunks
        if let Some(ref static_dir) = self.static_dir {
            let chunks_base = static_dir.join("chunks");
            if chunks_base.exists() {
                let (dirs_deleted, bytes_reclaimed) =
                    Self::cleanup_stale_chunks(&chunks_base, self.max_chunk_age_hours).await;
                if dirs_deleted > 0 {
                    info!(
                        "Removed {} stale chunk directories, freed {} MB",
                        dirs_deleted,
                        bytes_reclaimed / (1024 * 1024)
                    );
                } else {
                    debug!("No stale chunk directories to remove");
                }
            }
        }

        // Cleanup stale static asset cache entries and orphaned CAS blobs
        self.cleanup_stale_asset_cache().await;

        info!("Nightly cleanup completed");
    }

    /// Delete static_asset_cache rows older than `max_asset_cache_age_days`
    /// and garbage-collect CAS blobs no longer referenced by any row.
    async fn cleanup_stale_asset_cache(&self) {
        use sea_orm::{ColumnTrait, ConnectionTrait, EntityTrait, PaginatorTrait, QueryFilter};
        use temps_entities::static_asset_cache;

        let cutoff = chrono::Utc::now() - chrono::Duration::days(self.max_asset_cache_age_days);

        // 1. Find hashes that will become orphaned after deletion
        let stale_rows = match static_asset_cache::Entity::find()
            .filter(static_asset_cache::Column::CreatedAt.lt(cutoff))
            .all(self.db.as_ref())
            .await
        {
            Ok(rows) => rows,
            Err(e) => {
                error!("Failed to query stale static asset cache rows: {}", e);
                return;
            }
        };

        if stale_rows.is_empty() {
            debug!("No stale static asset cache entries to clean up");
            return;
        }

        let stale_hashes: std::collections::HashSet<String> =
            stale_rows.iter().map(|r| r.content_hash.clone()).collect();
        let stale_count = stale_rows.len();

        // 2. Delete stale rows (parameterized query to prevent SQL injection)
        let delete_result = self
            .db
            .as_ref()
            .execute(sea_orm::Statement::from_sql_and_values(
                sea_orm::DatabaseBackend::Postgres,
                "DELETE FROM static_asset_cache WHERE created_at < $1",
                [cutoff.into()],
            ))
            .await;

        match delete_result {
            Ok(result) => {
                info!(
                    "🧹 Deleted {} stale static asset cache entries (older than {} days)",
                    result.rows_affected(),
                    self.max_asset_cache_age_days
                );
            }
            Err(e) => {
                error!("Failed to delete stale static asset cache entries: {}", e);
                return;
            }
        }

        // 3. Garbage-collect orphaned blobs (hashes no longer referenced)
        let mut blobs_deleted = 0u64;
        for hash in &stale_hashes {
            // Check if any remaining row still references this hash
            let still_referenced = static_asset_cache::Entity::find()
                .filter(static_asset_cache::Column::ContentHash.eq(hash.as_str()))
                .count(self.db.as_ref())
                .await
                .unwrap_or(1); // If query fails, assume referenced (safe)

            if still_referenced == 0 {
                match self.file_store.delete_blob(hash).await {
                    Ok(true) => {
                        blobs_deleted += 1;
                    }
                    Ok(false) => {} // Already gone
                    Err(e) => {
                        let hash_prefix: String = hash.chars().take(8).collect();
                        warn!("Failed to delete orphaned blob {}: {}", hash_prefix, e);
                    }
                }
            }
        }

        if blobs_deleted > 0 {
            info!(
                "🧹 Garbage-collected {} orphaned CAS blobs (from {} stale entries)",
                blobs_deleted, stale_count
            );
        }
    }

    /// Remove persisted chunk directories older than `max_age_hours`.
    async fn cleanup_stale_chunks(chunks_base: &std::path::Path, max_age_hours: u64) -> (u64, u64) {
        let max_age = Duration::from_secs(max_age_hours * 3600);
        let mut dirs_deleted = 0u64;
        let mut bytes_reclaimed = 0u64;

        // Walk: chunks/{project_id}/{environment_id}/{deployment_id}/
        let project_dirs = match std::fs::read_dir(chunks_base) {
            Ok(entries) => entries,
            Err(e) => {
                warn!("Failed to read chunks directory: {}", e);
                return (0, 0);
            }
        };

        for project_entry in project_dirs.flatten() {
            if !project_entry.path().is_dir() {
                continue;
            }

            let env_dirs = match std::fs::read_dir(project_entry.path()) {
                Ok(entries) => entries,
                Err(_) => continue,
            };

            for env_entry in env_dirs.flatten() {
                if !env_entry.path().is_dir() {
                    continue;
                }

                let deploy_dirs = match std::fs::read_dir(env_entry.path()) {
                    Ok(entries) => entries,
                    Err(_) => continue,
                };

                for deploy_entry in deploy_dirs.flatten() {
                    let deploy_path = deploy_entry.path();
                    if !deploy_path.is_dir() {
                        continue;
                    }

                    let age = deploy_entry
                        .metadata()
                        .ok()
                        .and_then(|m| m.modified().ok())
                        .and_then(|t| t.elapsed().ok());

                    if let Some(age) = age {
                        if age > max_age {
                            let size = Self::dir_size_sync(&deploy_path);
                            match std::fs::remove_dir_all(&deploy_path) {
                                Ok(()) => {
                                    dirs_deleted += 1;
                                    bytes_reclaimed += size;
                                    debug!(
                                        "Removed stale chunk dir: {} (age: {}h)",
                                        deploy_path.display(),
                                        age.as_secs() / 3600,
                                    );
                                }
                                Err(e) => {
                                    warn!(
                                        "Failed to remove chunk dir {}: {}",
                                        deploy_path.display(),
                                        e
                                    );
                                }
                            }
                        }
                    }
                }

                // Remove empty environment directory
                if std::fs::read_dir(env_entry.path())
                    .map(|mut e| e.next().is_none())
                    .unwrap_or(false)
                {
                    let _ = std::fs::remove_dir(env_entry.path());
                }
            }

            // Remove empty project directory
            if std::fs::read_dir(project_entry.path())
                .map(|mut e| e.next().is_none())
                .unwrap_or(false)
            {
                let _ = std::fs::remove_dir(project_entry.path());
            }
        }

        (dirs_deleted, bytes_reclaimed)
    }

    fn dir_size_sync(path: &std::path::Path) -> u64 {
        let mut total = 0u64;
        if let Ok(entries) = std::fs::read_dir(path) {
            for entry in entries.flatten() {
                let p = entry.path();
                if p.is_dir() {
                    total += Self::dir_size_sync(&p);
                } else if let Ok(meta) = entry.metadata() {
                    total += meta.len();
                }
            }
        }
        total
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Clone)]
    #[allow(dead_code)]
    struct MockDockerClient {
        prune_images_result: Result<PruneStats, String>,
        prune_cache_result: Result<String, String>,
    }

    #[async_trait::async_trait]
    impl DockerClient for MockDockerClient {
        async fn prune_images(&self, _force: bool) -> Result<PruneStats, String> {
            self.prune_images_result.clone()
        }

        async fn prune_builder_cache(&self, _max_unused_days: i64) -> Result<String, String> {
            self.prune_cache_result.clone()
        }

        async fn remove_images(&self, image_names: &[String]) -> Vec<ImageRemovalOutcome> {
            image_names
                .iter()
                .map(|name| ImageRemovalOutcome {
                    image_name: name.clone(),
                    error: None,
                })
                .collect()
        }

        async fn list_image_tags(&self) -> Result<Vec<String>, DockerImageListError> {
            Ok(Vec::new())
        }
    }

    /// Docker client that records every image it was asked to remove, so tests
    /// can assert on *which* images the policy selected rather than only that
    /// the call did not panic.
    #[derive(Default)]
    struct RecordingDockerClient {
        removed: std::sync::Mutex<Vec<String>>,
        /// Image names the fake daemon refuses to remove (simulating "image is
        /// being used by container").
        refuse: Vec<String>,
        /// Tags the fake daemon reports as present locally.
        local_images: Vec<String>,
    }

    impl RecordingDockerClient {
        fn with_local_images(tags: &[&str]) -> Self {
            Self {
                local_images: tags.iter().map(|tag| tag.to_string()).collect(),
                ..Default::default()
            }
        }

        fn removed_sorted(&self) -> Vec<String> {
            let mut v = self
                .removed
                .lock()
                .expect("recording mock mutex poisoned")
                .clone();
            v.sort();
            v
        }
    }

    #[async_trait::async_trait]
    impl DockerClient for RecordingDockerClient {
        async fn prune_images(&self, _force: bool) -> Result<PruneStats, String> {
            Ok(PruneStats {
                images_deleted: 0,
                space_reclaimed_mb: 0,
            })
        }

        async fn prune_builder_cache(&self, _max_unused_days: i64) -> Result<String, String> {
            Ok(String::new())
        }

        async fn remove_images(&self, image_names: &[String]) -> Vec<ImageRemovalOutcome> {
            self.removed
                .lock()
                .expect("recording mock mutex poisoned")
                .extend(image_names.iter().cloned());
            image_names
                .iter()
                .map(|name| ImageRemovalOutcome {
                    image_name: name.clone(),
                    error: self
                        .refuse
                        .contains(name)
                        .then(|| format!("conflict: image {} is being used", name)),
                })
                .collect()
        }

        async fn list_image_tags(&self) -> Result<Vec<String>, DockerImageListError> {
            Ok(self.local_images.clone())
        }
    }

    /// Docker client whose image listing fails, to prove the pass fails
    /// closed rather than guessing which images exist.
    struct UnlistableDockerClient {
        removed: std::sync::Mutex<Vec<String>>,
    }

    #[async_trait::async_trait]
    impl DockerClient for UnlistableDockerClient {
        async fn prune_images(&self, _force: bool) -> Result<PruneStats, String> {
            Ok(PruneStats {
                images_deleted: 0,
                space_reclaimed_mb: 0,
            })
        }

        async fn prune_builder_cache(&self, _max_unused_days: i64) -> Result<String, String> {
            Ok(String::new())
        }

        async fn remove_images(&self, image_names: &[String]) -> Vec<ImageRemovalOutcome> {
            self.removed
                .lock()
                .expect("unlistable mock mutex poisoned")
                .extend(image_names.iter().cloned());
            Vec::new()
        }

        async fn list_image_tags(&self) -> Result<Vec<String>, DockerImageListError> {
            Err(DockerImageListError::List {
                source: bollard::errors::Error::DockerResponseServerError {
                    status_code: 500,
                    message: "daemon unreachable".to_string(),
                },
            })
        }
    }

    fn mock_db() -> Arc<sea_orm::DatabaseConnection> {
        Arc::new(sea_orm::MockDatabase::new(sea_orm::DatabaseBackend::Postgres).into_connection())
    }

    fn mock_file_store() -> Arc<dyn temps_file_store::FileStore> {
        Arc::new(temps_file_store::fs_store::FsFileStore::new(
            std::path::PathBuf::from("/tmp/temps-test-cleanup"),
        ))
    }

    #[test]
    fn test_cleanup_hour_calculation() {
        let service =
            DockerCleanupService::new(Arc::new(DefaultDockerClient), mock_db(), mock_file_store());
        let seconds = service.seconds_until_next_cleanup();

        // Should be positive and less than 24 hours
        assert!(seconds > 0);
        assert!(seconds <= 24 * 3600);
    }

    #[test]
    fn test_seconds_until_next_cleanup_invalid_hour_retries_instead_of_panicking() {
        // 25 is out of chrono's 0..24 range for `with_hour`; every real
        // caller guards with `% 24`, but this must degrade to a 24h retry
        // rather than panic if that guard is ever skipped.
        assert_eq!(seconds_until_next_cleanup(25), 24 * 3600);
    }

    #[test]
    fn test_custom_cleanup_hour() {
        let service =
            DockerCleanupService::new(Arc::new(DefaultDockerClient), mock_db(), mock_file_store())
                .with_cleanup_hour(3);

        assert_eq!(service.cleanup_hour, 3);
    }

    #[test]
    fn test_custom_cache_age() {
        let service =
            DockerCleanupService::new(Arc::new(DefaultDockerClient), mock_db(), mock_file_store())
                .with_max_cache_age_days(14);

        assert_eq!(service.max_cache_age_days, 14);
    }

    #[test]
    fn test_docker_only_scheduler_defaults() {
        let scheduler = DockerOnlyCleanupScheduler::new(Arc::new(DefaultDockerClient));

        assert_eq!(scheduler.cleanup_hour, 2);
        assert_eq!(scheduler.max_cache_age_days, 7);
    }

    #[test]
    fn test_docker_only_scheduler_custom_cleanup_hour() {
        let scheduler =
            DockerOnlyCleanupScheduler::new(Arc::new(DefaultDockerClient)).with_cleanup_hour(3);

        assert_eq!(scheduler.cleanup_hour, 3);
    }

    #[test]
    fn test_docker_only_scheduler_custom_cache_age() {
        let scheduler = DockerOnlyCleanupScheduler::new(Arc::new(DefaultDockerClient))
            .with_max_cache_age_days(14);

        assert_eq!(scheduler.max_cache_age_days, 14);
    }

    #[tokio::test]
    async fn test_perform_docker_prune_reports_success() {
        let client: Arc<dyn DockerClient> = Arc::new(MockDockerClient {
            prune_images_result: Ok(PruneStats {
                images_deleted: 2,
                space_reclaimed_mb: 128,
            }),
            prune_cache_result: Ok("removed 1 build cache entries, freed 64 MB".to_string()),
        });

        // Exercises the shared free function directly — success is "did
        // not panic and produced no error path" since prune_images/
        // prune_builder_cache results are only logged, not returned.
        perform_docker_prune(&client, 7).await;
    }

    #[tokio::test]
    async fn test_perform_docker_prune_handles_errors_gracefully() {
        let client: Arc<dyn DockerClient> = Arc::new(MockDockerClient {
            prune_images_result: Err("daemon unreachable".to_string()),
            prune_cache_result: Err("builder prune not supported".to_string()),
        });

        // Both prune calls fail; the shared helper must not panic — errors
        // are logged and cleanup continues (e.g. images prune failing must
        // not skip the build-cache prune).
        perform_docker_prune(&client, 7).await;
    }

    #[test]
    fn test_default_image_retention_hours_is_a_rollback_window() {
        let service =
            DockerCleanupService::new(Arc::new(DefaultDockerClient), mock_db(), mock_file_store());
        // 14 days. A short default (e.g. 48h) silently destroys the rollback
        // history of any project that does not deploy over a long weekend.
        assert_eq!(service.default_image_retention_hours, 336);
        assert!(service.image_retention_enabled);
        assert_eq!(service.keep_recent_deployment_images, 5);
        assert_eq!(service.max_deployment_images_per_run, 500);
        assert_eq!(
            service.default_image_retention_hours,
            temps_core::ImageRetentionSettings::default().default_hours,
            "service default must track the settings default"
        );
    }

    #[test]
    fn test_cleanup_limits_are_configurable() {
        let service =
            DockerCleanupService::new(Arc::new(DefaultDockerClient), mock_db(), mock_file_store())
                .with_keep_recent_deployment_images(10)
                .with_max_deployment_images_per_run(50);

        assert_eq!(service.keep_recent_deployment_images, 10);
        assert_eq!(service.max_deployment_images_per_run, 50);
    }

    #[test]
    fn test_operator_settings_override_retention() {
        let settings = temps_core::ImageRetentionSettings {
            enabled: false,
            default_hours: 72,
        };
        let service =
            DockerCleanupService::new(Arc::new(DefaultDockerClient), mock_db(), mock_file_store())
                .with_image_retention(&settings);

        assert_eq!(service.default_image_retention_hours, 72);
        assert!(!service.image_retention_enabled);
    }

    #[test]
    fn test_out_of_range_operator_setting_is_clamped() {
        // A hand-edited settings row must not be able to produce a cutoff that
        // deletes images the moment they are built.
        let zero = temps_core::ImageRetentionSettings {
            enabled: true,
            default_hours: 0,
        };
        let huge = temps_core::ImageRetentionSettings {
            enabled: true,
            default_hours: 999_999,
        };

        assert_eq!(zero.effective_default_hours(), 1);
        assert_eq!(huge.effective_default_hours(), 8760);
    }

    /// An uploaded image has no source to rebuild from. Pruning it is data
    /// loss: rollback and promotion both hard-fail with "image no longer
    /// exists locally" and there is no way to get it back.
    ///
    /// Eligibility itself (newer-reference-preserves-reused-image, the
    /// build-provenance filter) is now computed inside Postgres by
    /// `expired_image_candidates` and exercised end-to-end in
    /// `test_prune_old_deployment_images_end_to_end` below; this test only
    /// covers `protect_image`'s order-independence against an
    /// already-recorded entry, which is still pure Rust.
    #[test]
    fn test_protection_beats_expiry_regardless_of_order() {
        let uploaded = "temps-demo-prod:upload-1750000000";

        // Protect first, then simulate an already-recorded expired entry.
        let mut protect_first = HashMap::new();
        DockerCleanupService::protect_image(&mut protect_first, uploaded);
        protect_first
            .entry(uploaded.to_string())
            .and_modify(|eligible| *eligible &= true)
            .or_insert(true);
        assert_eq!(protect_first.get(uploaded), Some(&false));

        // Simulate an already-recorded expired entry first, then protect.
        let mut record_first = HashMap::new();
        record_first.insert(uploaded.to_string(), true);
        assert_eq!(
            record_first.get(uploaded),
            Some(&true),
            "precondition: an old upload looks eligible on age alone"
        );
        DockerCleanupService::protect_image(&mut record_first, uploaded);
        assert_eq!(
            record_first.get(uploaded),
            Some(&false),
            "protection must win over an already-recorded expiry"
        );
    }

    /// Protection used to skip any name without a `temps-` prefix, which
    /// would have left a builder-produced `<slug>:latest` tag unprotected
    /// once such tags became candidates. It must apply to any name now.
    #[test]
    fn test_protect_applies_to_builder_tags_without_temps_prefix() {
        let mut candidates = HashMap::new();
        candidates.insert("my-app-12:latest".to_string(), true);
        DockerCleanupService::protect_image(&mut candidates, "my-app-12:latest");
        assert_eq!(candidates.get("my-app-12:latest"), Some(&false));
    }

    /// The candidate query only considers images present on the local
    /// daemon, so an image listing failure must abort the pass rather than
    /// guess. The mock DB has no queued results, so reaching any query would
    /// also fail.
    #[tokio::test]
    async fn test_image_listing_failure_skips_retention() {
        let docker = Arc::new(UnlistableDockerClient {
            removed: Default::default(),
        });
        let service = DockerCleanupService::new(docker.clone(), mock_db(), mock_file_store())
            .with_image_retention(&temps_core::ImageRetentionSettings {
                enabled: true,
                default_hours: 1,
            });

        service.prune_old_deployment_images().await;

        assert!(
            docker
                .removed
                .lock()
                .expect("unlistable mock mutex poisoned")
                .is_empty(),
            "no image may be removed when the local image list is unavailable"
        );
    }

    #[tokio::test]
    async fn test_retention_disabled_removes_nothing() {
        let docker = Arc::new(RecordingDockerClient::default());
        let service = DockerCleanupService::new(docker.clone(), mock_db(), mock_file_store())
            .with_image_retention(&temps_core::ImageRetentionSettings {
                enabled: false,
                default_hours: 1,
            });

        service.prune_old_deployment_images().await;

        assert!(
            docker.removed_sorted().is_empty(),
            "no Docker call may be made when retention is disabled"
        );
    }

    /// A run where every removal is refused must not report success. Before
    /// this, `total_removed == 0` logged "nothing to remove", which reads to an
    /// operator as "cleanup is healthy" when it is in fact doing nothing.
    #[tokio::test]
    async fn test_refused_removals_are_counted_separately() {
        let docker = Arc::new(RecordingDockerClient {
            refuse: vec!["temps-a:1".to_string()],
            ..Default::default()
        });

        let outcomes = docker
            .remove_images(&["temps-a:1".to_string(), "temps-b:1".to_string()])
            .await;

        let removed = outcomes.iter().filter(|o| o.error.is_none()).count();
        let failed = outcomes.len() - removed;
        assert_eq!(removed, 1);
        assert_eq!(failed, 1);
        assert_eq!(
            docker.removed_sorted(),
            vec!["temps-a:1".to_string(), "temps-b:1".to_string()]
        );
    }

    /// End-to-end proof against a real Docker daemon that
    /// `DefaultDockerClient::remove_images` actually removes an unreferenced
    /// image and actually *retains* one that a container still references.
    ///
    /// The retention rule is only as good as the non-forced removal underneath
    /// it, and that behaviour lives in bollard rather than in our code — so it
    /// is worth asserting against the real daemon. Skips gracefully when Docker
    /// is unavailable (per project policy, no `#[ignore]`).
    ///
    /// Both images are *built* rather than tagged from a shared base, because
    /// `remove_image` on a tag that shares an image ID with another tag merely
    /// untags it without consulting container references. Temps builds one
    /// `<deployment slug>:latest` tag per deployment, so the single-tag case
    /// tested here is the shape that actually ships.
    #[tokio::test]
    async fn test_real_docker_removes_unused_and_retains_in_use_image() {
        use bollard::query_parameters::{
            BuildImageOptionsBuilder, CreateContainerOptionsBuilder, CreateImageOptionsBuilder,
            RemoveContainerOptions, RemoveImageOptions,
        };
        use bollard::Docker;
        use futures_util::StreamExt as _;

        let Ok(docker) = Docker::connect_with_unix_defaults() else {
            println!("Docker not available, skipping");
            return;
        };
        if docker.ping().await.is_err() {
            println!("Docker not available, skipping");
            return;
        }

        // Base layer for both test images.
        let mut pull = docker.create_image(
            Some(
                CreateImageOptionsBuilder::default()
                    .from_image("busybox")
                    .tag("latest")
                    .build(),
            ),
            None,
            None,
        );
        while let Some(step) = pull.next().await {
            if step.is_err() {
                println!("Could not pull busybox, skipping");
                return;
            }
        }

        // Each image gets its own ID (distinct ENV) so its tag is the sole
        // reference to it -- matching a real per-deployment build.
        async fn build(docker: &Docker, tag: &str, marker: &str) -> bool {
            let dockerfile = format!("FROM busybox:latest\nENV TEMPS_TEST_MARKER={}\n", marker);
            let mut header = tar::Header::new_gnu();
            header.set_size(dockerfile.len() as u64);
            header.set_mode(0o644);
            header.set_cksum();

            let mut builder = tar::Builder::new(Vec::new());
            if builder
                .append_data(&mut header, "Dockerfile", dockerfile.as_bytes())
                .is_err()
            {
                return false;
            }
            let Ok(context) = builder.into_inner() else {
                return false;
            };

            let mut build = docker.build_image(
                BuildImageOptionsBuilder::default().t(tag).build(),
                None,
                Some(http_body_util::Either::Left(http_body_util::Full::new(
                    bytes::Bytes::from(context),
                ))),
            );
            while let Some(step) = build.next().await {
                if step.is_err() {
                    return false;
                }
            }
            true
        }

        let unused = "temps-retention-test-unused:1";
        let in_use = "temps-retention-test-inuse:1";
        if !build(&docker, unused, "unused").await || !build(&docker, in_use, "inuse").await {
            println!("Could not build test images, skipping");
            let opts = Some(RemoveImageOptions {
                force: true,
                ..Default::default()
            });
            let _ = docker.remove_image(unused, opts.clone(), None).await;
            let _ = docker.remove_image(in_use, opts, None).await;
            return;
        }

        // Hold `in_use` with a container, exactly like a deployed service does.
        let container = docker
            .create_container(
                Some(
                    CreateContainerOptionsBuilder::new()
                        .name("temps-retention-test-holder")
                        .build(),
                ),
                bollard::models::ContainerCreateBody {
                    image: Some(in_use.to_string()),
                    cmd: Some(vec!["true".to_string()]),
                    ..Default::default()
                },
            )
            .await;
        let container_id = match container {
            Ok(c) => c.id,
            Err(e) => {
                println!("Could not create holder container ({e}), skipping");
                let opts = Some(RemoveImageOptions {
                    force: true,
                    ..Default::default()
                });
                let _ = docker.remove_image(unused, opts.clone(), None).await;
                let _ = docker.remove_image(in_use, opts, None).await;
                return;
            }
        };

        let outcomes = DefaultDockerClient
            .remove_images(&[unused.to_string(), in_use.to_string()])
            .await;

        let unused_outcome = outcomes
            .iter()
            .find(|o| o.image_name == unused)
            .expect("outcome reported for the unused image")
            .clone();
        let in_use_outcome = outcomes
            .iter()
            .find(|o| o.image_name == in_use)
            .expect("outcome reported for the in-use image")
            .clone();
        let unused_still_present = docker.inspect_image(unused).await.is_ok();

        // Clean up before asserting so a failing assert cannot leak state.
        let _ = docker
            .remove_container(
                &container_id,
                Some(RemoveContainerOptions {
                    force: true,
                    ..Default::default()
                }),
            )
            .await;
        let _ = docker
            .remove_image(
                in_use,
                Some(RemoveImageOptions {
                    force: true,
                    ..Default::default()
                }),
                None,
            )
            .await;

        assert!(
            unused_outcome.error.is_none(),
            "an unreferenced expired image must actually be removed, got: {:?}",
            unused_outcome.error
        );
        assert!(
            !unused_still_present,
            "the unused image must be gone from the daemon after removal"
        );
        assert!(
            in_use_outcome.error.is_some(),
            "an image still referenced by a container must be retained, not untagged \
             underneath the service running it"
        );
    }

    async fn cleanup_integration_tests_available() -> bool {
        std::env::var_os("TEMPS_TEST_DATABASE_URL").is_some()
            || tokio::process::Command::new("docker")
                .arg("info")
                .output()
                .await
                .map(|output| output.status.success())
                .unwrap_or(false)
    }

    /// Record the persisted output of a successful `BuildImageJob`, which is
    /// what the retention pass treats as proof that Temps built `image_tag`.
    async fn record_build(
        db: &sea_orm::DatabaseConnection,
        deployment_id: i32,
        image_tag: &str,
    ) -> Result<(), sea_orm::DbErr> {
        use sea_orm::{ActiveModelTrait, ActiveValue::Set};
        use temps_entities::{deployment_jobs, types::JobStatus};

        deployment_jobs::ActiveModel {
            deployment_id: Set(deployment_id),
            job_id: Set("build_image".to_string()),
            job_type: Set("BuildImageJob".to_string()),
            name: Set("Build Container Image".to_string()),
            log_id: Set(format!("build-log-{deployment_id}")),
            status: Set(JobStatus::Success),
            outputs: Set(Some(serde_json::json!({ "image_tag": image_tag }))),
            ..Default::default()
        }
        .insert(db)
        .await?;
        Ok(())
    }

    /// A git project with a tight 1-hour retention override and one
    /// environment, so "old" only needs to be a few hours in the past.
    async fn retention_fixture(
        db: &sea_orm::DatabaseConnection,
        slug: &str,
    ) -> Result<
        (
            temps_entities::projects::Model,
            temps_entities::environments::Model,
        ),
        sea_orm::DbErr,
    > {
        use sea_orm::{ActiveModelTrait, ActiveValue::Set};
        use temps_entities::preset::Preset;
        use temps_entities::upstream_config::UpstreamList;
        use temps_entities::{environments, projects};

        let now = chrono::Utc::now();
        let project = projects::ActiveModel {
            name: Set(format!("Project {slug}")),
            slug: Set(slug.to_string()),
            repo_owner: Set("test-owner".to_string()),
            repo_name: Set("test-repo".to_string()),
            preset: Set(Preset::Dockerfile),
            directory: Set("/".to_string()),
            main_branch: Set("main".to_string()),
            created_at: Set(now),
            updated_at: Set(now),
            image_retention_hours: Set(Some(1)),
            ..Default::default()
        }
        .insert(db)
        .await?;

        let environment = environments::ActiveModel {
            project_id: Set(project.id),
            name: Set("Test Environment".to_string()),
            slug: Set("test".to_string()),
            host: Set(format!("{slug}.example.com")),
            upstreams: Set(UpstreamList::default()),
            current_deployment_id: Set(None),
            subdomain: Set(format!("{slug}.example.com")),
            created_at: Set(now),
            updated_at: Set(now),
            ..Default::default()
        }
        .insert(db)
        .await?;

        Ok((project, environment))
    }

    /// End-to-end proof against a real database that eligibility comes from
    /// what the builder recorded, not from a `temps-` name prefix. The
    /// project slug deliberately does not start with `temps-`; before the
    /// fix, nothing here was ever a candidate. Skips gracefully when
    /// Docker/Postgres is unavailable (no `#[ignore]`, per project policy).
    #[tokio::test]
    async fn test_prune_old_deployment_images_end_to_end() -> Result<(), Box<dyn std::error::Error>>
    {
        use chrono::{Duration, Utc};
        use sea_orm::{ActiveModelTrait, ActiveValue::Set, EntityTrait};
        use temps_entities::{deployments, environments};

        if !cleanup_integration_tests_available().await {
            eprintln!("Docker/Postgres unavailable; skipping retention integration test");
            return Ok(());
        }

        let test_db = temps_database::test_utils::TestDatabase::with_migrations().await?;
        let db = test_db.connection_arc();
        let (project, environment) = retention_fixture(db.as_ref(), "retention-app").await?;
        let now = Utc::now();
        let old = now - Duration::hours(72);

        let insert_deployment =
            |slug: &str,
             state: &str,
             image_name: Option<&str>,
             created_at: chrono::DateTime<Utc>| {
                deployments::ActiveModel {
                    project_id: Set(project.id),
                    environment_id: Set(environment.id),
                    slug: Set(slug.to_string()),
                    state: Set(state.to_string()),
                    image_name: Set(image_name.map(str::to_string)),
                    metadata: Set(Some(deployments::DeploymentMetadata::default())),
                    created_at: Set(created_at),
                    updated_at: Set(created_at),
                    ..Default::default()
                }
            };

        // Built, superseded and expired: must be removed even though the
        // slug has no `temps-` prefix.
        let expired = insert_deployment(
            "retention-app-1",
            "stopped",
            Some("retention-app-1:latest"),
            old,
        )
        .insert(db.as_ref())
        .await?;
        record_build(db.as_ref(), expired.id, "retention-app-1:latest").await?;

        // Built by an old deployment, reused by a recent rollback: the
        // recent reference keeps it (BOOL_AND over every reference).
        let reused = insert_deployment(
            "retention-app-2",
            "stopped",
            Some("retention-app-2:latest"),
            old,
        )
        .insert(db.as_ref())
        .await?;
        record_build(db.as_ref(), reused.id, "retention-app-2:latest").await?;
        insert_deployment(
            "retention-app-9",
            "stopped",
            Some("retention-app-2:latest"),
            now,
        )
        .insert(db.as_ref())
        .await?;

        // Build succeeded, deploy failed: `image_name` is never written, but
        // the image is on disk and must still expire.
        let failed = insert_deployment("retention-app-3", "failed", None, old)
            .insert(db.as_ref())
            .await?;
        record_build(db.as_ref(), failed.id, "retention-app-3:latest").await?;

        // Same shape, but the deployment is still rolling out: nothing else
        // protects it yet, so the in-flight guard must.
        let in_flight = insert_deployment("retention-app-4", "running", None, old)
            .insert(db.as_ref())
            .await?;
        record_build(db.as_ref(), in_flight.id, "retention-app-4:latest").await?;

        // Built and expired, but the environment is serving it.
        let active = insert_deployment(
            "retention-app-5",
            "completed",
            Some("retention-app-5:latest"),
            old,
        )
        .insert(db.as_ref())
        .await?;
        record_build(db.as_ref(), active.id, "retention-app-5:latest").await?;
        let mut environment_update: environments::ActiveModel = environment.clone().into();
        environment_update.current_deployment_id = Set(Some(active.id));
        environment_update.update(db.as_ref()).await?;

        // A built tag that an image upload later reused under the same
        // name: the upload cannot be rebuilt, so the tag must survive.
        let built_then_uploaded = insert_deployment(
            "retention-app-6",
            "stopped",
            Some("retention-app-6:latest"),
            old,
        )
        .insert(db.as_ref())
        .await?;
        record_build(
            db.as_ref(),
            built_then_uploaded.id,
            "retention-app-6:latest",
        )
        .await?;
        deployments::ActiveModel {
            context_vars: Set(Some(serde_json::json!({ "trigger": "image_upload" }))),
            ..insert_deployment(
                "retention-app-7",
                "stopped",
                Some("retention-app-6:latest"),
                old,
            )
        }
        .insert(db.as_ref())
        .await?;

        // Built and expired, but already gone from the daemon (removed on an
        // earlier night): must not be requested again.
        let already_removed = insert_deployment(
            "retention-app-8",
            "stopped",
            Some("retention-app-8:latest"),
            old,
        )
        .insert(db.as_ref())
        .await?;
        record_build(db.as_ref(), already_removed.id, "retention-app-8:latest").await?;

        // Images Temps never built, all present locally and expired: an
        // external pull, an external image recorded via metadata, and a
        // name that merely looks like a builder tag. None has a build
        // record, so none may be touched.
        insert_deployment("retention-app-10", "stopped", Some("nginx:latest"), old)
            .insert(db.as_ref())
            .await?;
        deployments::ActiveModel {
            metadata: Set(Some(deployments::DeploymentMetadata {
                external_image_ref: Some("registry.example.com/team/app:1".to_string()),
                ..Default::default()
            })),
            ..insert_deployment(
                "retention-app-11",
                "stopped",
                Some("registry.example.com/team/app:1"),
                old,
            )
        }
        .insert(db.as_ref())
        .await?;
        insert_deployment(
            "retention-app-12",
            "stopped",
            Some("retention-app-12:latest"),
            old,
        )
        .insert(db.as_ref())
        .await?;

        let docker = Arc::new(RecordingDockerClient::with_local_images(&[
            "retention-app-1:latest",
            "retention-app-2:latest",
            "retention-app-3:latest",
            "retention-app-4:latest",
            "retention-app-5:latest",
            "retention-app-6:latest",
            "nginx:latest",
            "registry.example.com/team/app:1",
            "retention-app-12:latest",
        ]));
        let service = DockerCleanupService::new(docker.clone(), db.clone(), mock_file_store())
            .with_keep_recent_deployment_images(0)
            .with_max_deployment_images_per_run(500)
            .with_image_retention(&temps_core::ImageRetentionSettings {
                enabled: true,
                default_hours: 336,
            });

        service.prune_old_deployment_images().await;

        assert_eq!(
            docker.removed_sorted(),
            vec![
                "retention-app-1:latest".to_string(),
                "retention-app-3:latest".to_string(),
            ],
            "only expired, Temps-built, locally present images may be removed; \
             reused, in-flight, active, uploaded, already-removed and non-built \
             images must all survive"
        );

        // Pruning removes the Docker image, never the deployment record.
        let still_present = deployments::Entity::find_by_id(expired.id)
            .one(db.as_ref())
            .await?;
        assert!(
            still_present.is_some(),
            "pruning must never delete the deployment row, only the image"
        );

        Ok(())
    }

    /// The newest `keep_recent_deployment_images` deployments per
    /// project+environment are a rollback floor whatever their age. With
    /// three expired built images and a floor of two, only the oldest may go.
    #[tokio::test]
    async fn test_rollback_floor_protects_newest_built_images(
    ) -> Result<(), Box<dyn std::error::Error>> {
        use chrono::{Duration, Utc};
        use sea_orm::{ActiveModelTrait, ActiveValue::Set};
        use temps_entities::deployments;

        if !cleanup_integration_tests_available().await {
            eprintln!("Docker/Postgres unavailable; skipping rollback floor test");
            return Ok(());
        }

        let test_db = temps_database::test_utils::TestDatabase::with_migrations().await?;
        let db = test_db.connection_arc();
        let (project, environment) = retention_fixture(db.as_ref(), "floor-app").await?;
        let now = Utc::now();

        for (n, age_hours) in [(1, 96), (2, 72), (3, 48)] {
            let created_at = now - Duration::hours(age_hours);
            let tag = format!("floor-app-{n}:latest");
            let deployment = deployments::ActiveModel {
                project_id: Set(project.id),
                environment_id: Set(environment.id),
                slug: Set(format!("floor-app-{n}")),
                state: Set("stopped".to_string()),
                image_name: Set(Some(tag.clone())),
                metadata: Set(Some(deployments::DeploymentMetadata::default())),
                created_at: Set(created_at),
                updated_at: Set(created_at),
                ..Default::default()
            }
            .insert(db.as_ref())
            .await?;
            record_build(db.as_ref(), deployment.id, &tag).await?;
        }

        let docker = Arc::new(RecordingDockerClient::with_local_images(&[
            "floor-app-1:latest",
            "floor-app-2:latest",
            "floor-app-3:latest",
        ]));
        let service = DockerCleanupService::new(docker.clone(), db.clone(), mock_file_store())
            .with_keep_recent_deployment_images(2)
            .with_image_retention(&temps_core::ImageRetentionSettings {
                enabled: true,
                default_hours: 336,
            });

        service.prune_old_deployment_images().await;

        assert_eq!(
            docker.removed_sorted(),
            vec!["floor-app-1:latest".to_string()],
            "the two newest deployments are rollback targets and must keep their images"
        );

        Ok(())
    }

    /// Regression test for a livelock the `ORDER BY ... LIMIT` bound on
    /// `expired_image_candidates` could otherwise introduce: if permanently
    /// protected images (here, built tags later reused by uploads)
    /// outnumber `max_deployment_images_per_run` and are older than a
    /// genuinely removable image, a LIMIT applied *before* protection would
    /// fill the entire candidate window with rows `protect_image`
    /// immediately discards, reclaiming zero bytes forever instead of "the
    /// remainder next run". The protective flags in
    /// `expired_image_candidates` exclude those images on the SQL side, so
    /// the removable image must still surface with a `LIMIT` far smaller
    /// than the protected count.
    #[tokio::test]
    async fn test_permanently_protected_images_do_not_starve_the_limit_window(
    ) -> Result<(), Box<dyn std::error::Error>> {
        use chrono::{Duration, Utc};
        use sea_orm::{ActiveModelTrait, ActiveValue::Set};
        use temps_entities::deployments;

        if !cleanup_integration_tests_available().await {
            eprintln!("Docker/Postgres unavailable; skipping livelock regression test");
            return Ok(());
        }

        let test_db = temps_database::test_utils::TestDatabase::with_migrations().await?;
        let db = test_db.connection_arc();
        let (project, environment) = retention_fixture(db.as_ref(), "livelock-app").await?;
        let now = Utc::now();
        let very_old = now - Duration::hours(200);
        let old = now - Duration::hours(72);

        // Three permanently-protected images, older than the one
        // genuinely-removable image below: enough to fill a LIMIT of 2 on
        // their own if protection isn't applied before the LIMIT.
        let mut local_images = Vec::new();
        for i in 0..3 {
            let tag = format!("livelock-app-{i}:latest");
            let deployment = deployments::ActiveModel {
                project_id: Set(project.id),
                environment_id: Set(environment.id),
                slug: Set(format!("livelock-app-{i}")),
                state: Set("stopped".to_string()),
                image_name: Set(Some(tag.clone())),
                metadata: Set(Some(deployments::DeploymentMetadata {
                    external_image_ref: Some(format!("registry.internal/uploaded-{i}:1")),
                    ..Default::default()
                })),
                created_at: Set(very_old),
                updated_at: Set(very_old),
                ..Default::default()
            }
            .insert(db.as_ref())
            .await?;
            record_build(db.as_ref(), deployment.id, &tag).await?;
            local_images.push(tag);
        }

        // The one image that should actually be removed: newer than the
        // protected ones (so it would lose an oldest-first LIMIT race
        // against them) but still expired against the 1h retention window.
        let removable = deployments::ActiveModel {
            project_id: Set(project.id),
            environment_id: Set(environment.id),
            slug: Set("livelock-app-3".to_string()),
            state: Set("stopped".to_string()),
            image_name: Set(Some("livelock-app-3:latest".to_string())),
            metadata: Set(Some(deployments::DeploymentMetadata::default())),
            created_at: Set(old),
            updated_at: Set(old),
            ..Default::default()
        }
        .insert(db.as_ref())
        .await?;
        record_build(db.as_ref(), removable.id, "livelock-app-3:latest").await?;
        local_images.push("livelock-app-3:latest".to_string());

        let docker = Arc::new(RecordingDockerClient {
            local_images,
            ..Default::default()
        });
        let service = DockerCleanupService::new(docker.clone(), db.clone(), mock_file_store())
            .with_keep_recent_deployment_images(0)
            // Smaller than the protected-image count: if protection were
            // applied after the LIMIT, the 3 protected images alone would
            // exhaust it and the removable image would never be considered.
            .with_max_deployment_images_per_run(2)
            .with_image_retention(&temps_core::ImageRetentionSettings {
                enabled: true,
                default_hours: 336,
            });

        service.prune_old_deployment_images().await;

        assert_eq!(
            docker.removed_sorted(),
            vec!["livelock-app-3:latest".to_string()],
            "a removable image must not be starved out of the LIMIT window by \
             permanently-protected images that will never actually be removed"
        );

        Ok(())
    }

    /// Regression test for the fix that made retention settings live: before
    /// this, `DockerCleanupService` only ever read `AppSettings.image_retention`
    /// once at construction, so an operator disabling retention (or changing
    /// the default window) after boot had no effect until the process
    /// restarted. This proves a settings row written *after* the service was
    /// built is picked up on the very next run, with no rebuild in between.
    #[tokio::test]
    async fn test_disabling_retention_via_settings_takes_effect_without_restart(
    ) -> Result<(), Box<dyn std::error::Error>> {
        use sea_orm::ActiveModelTrait;
        use temps_entities::preset::Preset;
        use temps_entities::upstream_config::UpstreamList;
        use temps_entities::{deployments, environments, projects};

        if !cleanup_integration_tests_available().await {
            eprintln!("Docker/Postgres unavailable; skipping live-settings regression test");
            return Ok(());
        }

        let test_db = temps_database::test_utils::TestDatabase::with_migrations().await?;
        let db = test_db.connection_arc();
        let now = chrono::Utc::now();
        let old = now - chrono::Duration::hours(72);

        let project = projects::ActiveModel {
            name: sea_orm::ActiveValue::Set("Live Settings Test Project".to_string()),
            slug: sea_orm::ActiveValue::Set("live-settings-test-project".to_string()),
            repo_owner: sea_orm::ActiveValue::Set("test-owner".to_string()),
            repo_name: sea_orm::ActiveValue::Set("test-repo".to_string()),
            preset: sea_orm::ActiveValue::Set(Preset::Dockerfile),
            directory: sea_orm::ActiveValue::Set("/".to_string()),
            main_branch: sea_orm::ActiveValue::Set("main".to_string()),
            created_at: sea_orm::ActiveValue::Set(now),
            updated_at: sea_orm::ActiveValue::Set(now),
            ..Default::default()
        }
        .insert(db.as_ref())
        .await?;

        let environment = environments::ActiveModel {
            project_id: sea_orm::ActiveValue::Set(project.id),
            name: sea_orm::ActiveValue::Set("Test Environment".to_string()),
            slug: sea_orm::ActiveValue::Set("test".to_string()),
            host: sea_orm::ActiveValue::Set("live-settings-test.example.com".to_string()),
            upstreams: sea_orm::ActiveValue::Set(UpstreamList::default()),
            current_deployment_id: sea_orm::ActiveValue::Set(None),
            subdomain: sea_orm::ActiveValue::Set("live-settings-test.example.com".to_string()),
            created_at: sea_orm::ActiveValue::Set(now),
            updated_at: sea_orm::ActiveValue::Set(now),
            ..Default::default()
        }
        .insert(db.as_ref())
        .await?;

        let expired = deployments::ActiveModel {
            project_id: sea_orm::ActiveValue::Set(project.id),
            environment_id: sea_orm::ActiveValue::Set(environment.id),
            slug: sea_orm::ActiveValue::Set("live-settings-app-1".to_string()),
            state: sea_orm::ActiveValue::Set("stopped".to_string()),
            image_name: sea_orm::ActiveValue::Set(Some("live-settings-app-1:latest".to_string())),
            metadata: sea_orm::ActiveValue::Set(Some(deployments::DeploymentMetadata::default())),
            created_at: sea_orm::ActiveValue::Set(old),
            updated_at: sea_orm::ActiveValue::Set(old),
            ..Default::default()
        }
        .insert(db.as_ref())
        .await?;
        record_build(db.as_ref(), expired.id, "live-settings-app-1:latest").await?;

        let server_config = Arc::new(
            temps_config::ServerConfig::new(
                "127.0.0.1:3000".to_string(),
                "postgresql://test".to_string(),
                None,
                None,
            )
            .unwrap(),
        );
        let config_service = Arc::new(temps_config::ConfigService::new(server_config, db.clone()));

        // Settings row starts with retention enabled and a 1-hour window, so
        // the seeded deployment above is expired.
        config_service
            .update_settings(temps_core::AppSettings {
                image_retention: temps_core::ImageRetentionSettings {
                    enabled: true,
                    default_hours: 1,
                },
                ..Default::default()
            })
            .await?;

        let docker = Arc::new(RecordingDockerClient::with_local_images(&[
            "live-settings-app-1:latest",
        ]));
        // Constructed with a stale "enabled" snapshot on purpose — this is
        // what the service would have looked like right after boot, before
        // the settings row below is written.
        let service = DockerCleanupService::new(docker.clone(), db.clone(), mock_file_store())
            .with_keep_recent_deployment_images(0)
            .with_image_retention(&temps_core::ImageRetentionSettings {
                enabled: true,
                default_hours: 1,
            })
            .with_config_service(config_service.clone());

        // Operator disables retention via the settings row *after* the
        // service was constructed — no restart, no rebuild.
        config_service
            .update_settings(temps_core::AppSettings {
                image_retention: temps_core::ImageRetentionSettings {
                    enabled: false,
                    default_hours: 1,
                },
                ..Default::default()
            })
            .await?;

        service.prune_old_deployment_images().await;

        assert!(
            docker.removed_sorted().is_empty(),
            "disabling retention after construction must be honored on the very \
             next run, not require a restart"
        );

        Ok(())
    }
}
