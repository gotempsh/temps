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

type DeploymentImageReference = (i32, i32, i32, Option<String>, chrono::DateTime<chrono::Utc>);

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
    pub fn with_image_retention(mut self, settings: &temps_core::ImageRetentionSettings) -> Self {
        self.image_retention_enabled = settings.enabled;
        self.default_image_retention_hours = settings.effective_default_hours();
        self
    }

    fn is_temps_managed_image(image_name: &str) -> bool {
        image_name.starts_with("temps-") && !image_name.contains('/')
    }

    fn record_image_retention_eligibility(
        candidates: &mut HashMap<String, bool>,
        image_name: &str,
        referenced_at: chrono::DateTime<chrono::Utc>,
        cutoff: chrono::DateTime<chrono::Utc>,
    ) {
        if !Self::is_temps_managed_image(image_name) {
            return;
        }

        let reference_is_expired = referenced_at < cutoff;
        candidates
            .entry(image_name.to_string())
            .and_modify(|eligible| *eligible &= reference_is_expired)
            .or_insert(reference_is_expired);
    }

    /// Mark an image as permanently protected, whatever its age.
    ///
    /// Used for images we cannot rebuild (uploaded tarballs, external
    /// registry pulls) and for the image each environment is currently
    /// serving. `insert` rather than `and_modify` so protection wins
    /// regardless of the order references are visited in.
    fn protect_image(candidates: &mut HashMap<String, bool>, image_name: &str) {
        if !Self::is_temps_managed_image(image_name) {
            return;
        }
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
    /// most `max_deployment_images_per_run` candidates, oldest first. Only
    /// Temps-managed local tags are considered; registry images are left to Docker's
    /// normal cache policy. Docker removal is non-forced, so an image still referenced
    /// by any container is retained as a final safety net.
    async fn prune_old_deployment_images(&self) {
        use sea_orm::{ColumnTrait, EntityTrait, QueryFilter, QuerySelect};
        use temps_entities::{deployments, environments, projects};

        if !self.image_retention_enabled {
            debug!("Deployment image retention is disabled; skipping");
            return;
        }

        // Per-project retention overrides. Small table, fetched once so the
        // deployment scan below does not need to join (or carry) project rows.
        let retention_by_project: HashMap<i32, i64> = match projects::Entity::find()
            .select_only()
            .column(projects::Column::Id)
            .column(projects::Column::ImageRetentionHours)
            .into_tuple::<(i32, Option<i32>)>()
            .all(self.db.as_ref())
            .await
        {
            Ok(rows) => rows
                .into_iter()
                .map(|(id, hours)| {
                    (
                        id,
                        hours
                            .map(i64::from)
                            .unwrap_or(self.default_image_retention_hours),
                    )
                })
                .collect(),
            Err(e) => {
                error!("Failed to query project retention overrides: {}", e);
                return;
            }
        };

        // Only the five columns the policy actually needs. Selecting the full
        // model here would pull `deployment_config`, `context_vars`,
        // `commit_json` and `metadata` for every deployment ever created.
        let mut deployment_rows: Vec<DeploymentImageReference> = match deployments::Entity::find()
            .select_only()
            .column(deployments::Column::Id)
            .column(deployments::Column::ProjectId)
            .column(deployments::Column::EnvironmentId)
            .column(deployments::Column::ImageName)
            .column(deployments::Column::CreatedAt)
            .filter(deployments::Column::ImageName.is_not_null())
            .into_tuple()
            .all(self.db.as_ref())
            .await
        {
            Ok(rows) => rows,
            Err(e) => {
                error!(
                    "Failed to query deployment images for retention cleanup: {}",
                    e
                );
                return;
            }
        };

        if deployment_rows.is_empty() {
            debug!("No deployment images recorded; nothing to prune");
            return;
        }

        let now = chrono::Utc::now();
        let mut candidates: HashMap<String, bool> = HashMap::new();
        let mut oldest_reference_by_image = HashMap::new();
        for (_, project_id, _, image_name, created_at) in &deployment_rows {
            let Some(image_name) = image_name.as_deref() else {
                continue;
            };
            let retention_hours = retention_by_project
                .get(project_id)
                .copied()
                .unwrap_or(self.default_image_retention_hours);
            let cutoff = now - chrono::Duration::hours(retention_hours);

            Self::record_image_retention_eligibility(
                &mut candidates,
                image_name,
                *created_at,
                cutoff,
            );
            if Self::is_temps_managed_image(image_name) {
                oldest_reference_by_image
                    .entry(image_name.to_string())
                    .and_modify(|oldest: &mut chrono::DateTime<chrono::Utc>| {
                        *oldest = (*oldest).min(*created_at);
                    })
                    .or_insert(*created_at);
            }
        }

        if candidates.is_empty() {
            debug!("No Temps-managed deployment images to consider");
            return;
        }

        // Preserve main's rollback floor from #645: regardless of the age
        // policy, keep the newest N deployment images for every
        // project+environment. Sort newest-first before counting so every
        // image referenced by a recent deployment is protected.
        deployment_rows.sort_by_key(|row| std::cmp::Reverse(row.4));
        let mut recent_by_scope: HashMap<(i32, i32), u64> = HashMap::new();
        for (_, project_id, environment_id, image_name, _) in &deployment_rows {
            let seen = recent_by_scope
                .entry((*project_id, *environment_id))
                .or_default();
            if *seen < self.keep_recent_deployment_images {
                if let Some(image_name) = image_name.as_deref() {
                    Self::protect_image(&mut candidates, image_name);
                }
            }
            *seen += 1;
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

        // Protect whatever each environment is currently serving.
        match environments::Entity::find()
            .select_only()
            .column(environments::Column::CurrentDeploymentId)
            .filter(environments::Column::CurrentDeploymentId.is_not_null())
            .into_tuple::<Option<i32>>()
            .all(self.db.as_ref())
            .await
        {
            Ok(current_ids) => {
                let active: std::collections::HashSet<i32> =
                    current_ids.into_iter().flatten().collect();
                for (deployment_id, _, _, image_name, _) in &deployment_rows {
                    if !active.contains(deployment_id) {
                        continue;
                    }
                    if let Some(image_name) = image_name.as_deref() {
                        Self::protect_image(&mut candidates, image_name);
                    }
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
                    let oldest = oldest_reference_by_image.get(&name).copied().unwrap_or(now);
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

    /// Image names that must never be pruned because Temps cannot rebuild them:
    /// tarballs uploaded via the image-upload API and external registry images.
    ///
    /// Matched on the deployment's own provenance rather than on the tag text,
    /// because the upload endpoint lets the caller supply an arbitrary `tag`.
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
                        warn!("Failed to delete orphaned blob {}: {}", &hash[..8], e);
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
    }

    impl RecordingDockerClient {
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

    #[test]
    fn test_newer_reference_preserves_reused_image() {
        let now = chrono::Utc::now();
        let cutoff = now - chrono::Duration::hours(48);
        let mut candidates = HashMap::new();

        DockerCleanupService::record_image_retention_eligibility(
            &mut candidates,
            "temps-demo:42",
            now - chrono::Duration::hours(72),
            cutoff,
        );
        DockerCleanupService::record_image_retention_eligibility(
            &mut candidates,
            "temps-demo:42",
            now - chrono::Duration::hours(1),
            cutoff,
        );

        assert_eq!(candidates.get("temps-demo:42"), Some(&false));
    }

    #[test]
    fn test_only_temps_managed_local_images_become_candidates() {
        let now = chrono::Utc::now();
        let cutoff = now - chrono::Duration::hours(48);
        let mut candidates = HashMap::new();

        DockerCleanupService::record_image_retention_eligibility(
            &mut candidates,
            "ghcr.io/example/temps-demo:42",
            now - chrono::Duration::hours(72),
            cutoff,
        );
        DockerCleanupService::record_image_retention_eligibility(
            &mut candidates,
            "nginx:latest",
            now - chrono::Duration::hours(72),
            cutoff,
        );
        DockerCleanupService::record_image_retention_eligibility(
            &mut candidates,
            "temps-demo:42",
            now - chrono::Duration::hours(72),
            cutoff,
        );

        assert_eq!(candidates.len(), 1);
        assert_eq!(candidates.get("temps-demo:42"), Some(&true));
    }

    /// An uploaded image has no source to rebuild from. Pruning it is data
    /// loss: rollback and promotion both hard-fail with "image no longer
    /// exists locally" and there is no way to get it back.
    #[test]
    fn test_protection_beats_expiry_regardless_of_order() {
        let now = chrono::Utc::now();
        let cutoff = now - chrono::Duration::hours(336);
        let uploaded = "temps-demo-prod:upload-1750000000";

        // Protect first, then record an expired reference.
        let mut protect_first = HashMap::new();
        DockerCleanupService::protect_image(&mut protect_first, uploaded);
        DockerCleanupService::record_image_retention_eligibility(
            &mut protect_first,
            uploaded,
            now - chrono::Duration::days(400),
            cutoff,
        );
        assert_eq!(protect_first.get(uploaded), Some(&false));

        // Record an expired reference first, then protect.
        let mut record_first = HashMap::new();
        DockerCleanupService::record_image_retention_eligibility(
            &mut record_first,
            uploaded,
            now - chrono::Duration::days(400),
            cutoff,
        );
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

    #[test]
    fn test_protect_ignores_non_temps_images() {
        let mut candidates = HashMap::new();
        DockerCleanupService::protect_image(&mut candidates, "ghcr.io/example/app:1");
        DockerCleanupService::protect_image(&mut candidates, "nginx:latest");
        assert!(
            candidates.is_empty(),
            "external images are never candidates, so they need no protection entry"
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
            removed: Default::default(),
            refuse: vec!["temps-a:1".to_string()],
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
    /// unique `temps-{slug}:{deployment_id}` tag per deployment, so the
    /// single-tag case tested here is the shape that actually ships.
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
}
