//! Docker Cleanup Service
//!
//! Manages nightly cleanup of unused Docker images and build caches to save disk space.
//! Runs as a background task scheduled at 2 AM UTC daily.

use chrono::Timelike as _;
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

    // Calculate target time (today at cleanup_hour)
    let target_time = now
        .with_hour(cleanup_hour)
        .and_then(|t| t.with_minute(0))
        .and_then(|t| t.with_second(0))
        .expect("Failed to calculate target cleanup time");

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

    /// Perform the actual cleanup
    async fn perform_cleanup(&self) {
        info!("🧹 Starting nightly Docker cleanup");

        perform_docker_prune(&self.docker_client, self.max_cache_age_days).await;

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
}
