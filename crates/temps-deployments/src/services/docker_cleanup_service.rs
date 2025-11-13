//! Docker Cleanup Service
//!
//! Manages nightly cleanup of unused Docker images and build caches to save disk space.
//! Runs as a background task scheduled at 2 AM UTC daily.

use chrono::Timelike as _;
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
        // Use bollard to interact with Docker daemon
        use bollard::query_parameters::PruneImagesOptions;
        use bollard::Docker;

        let docker = Docker::connect_with_unix_defaults()
            .map_err(|e| format!("Failed to connect to Docker daemon: {}", e))?;

        match docker.prune_images(None::<PruneImagesOptions>).await {
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
        // Docker builder prune removes build cache
        // Unfortunately, bollard doesn't have a direct prune_builder_cache method,
        // so we'll use docker CLI as a workaround
        use std::process::Command;

        // Calculate max_unused filter for docker builder prune
        // Docker uses "duration" format (e.g., "168h" for 7 days)
        let duration = format!("{}h", max_unused_days * 24);

        let output = Command::new("docker")
            .args(&[
                "builder",
                "prune",
                "-f",
                "--keep-state",
                "--filter",
                &format!("unused-for={}", duration),
            ])
            .output()
            .map_err(|e| format!("Failed to execute docker builder prune: {}", e))?;

        if output.status.success() {
            String::from_utf8(output.stdout)
                .map_err(|e| format!("Failed to parse docker builder prune output: {}", e))
        } else {
            let stderr = String::from_utf8_lossy(&output.stderr);
            Err(format!("Docker builder prune failed: {}", stderr))
        }
    }
}

/// Docker cleanup service that runs nightly
pub struct DockerCleanupService {
    docker_client: Arc<dyn DockerClient>,
    /// Hour of day (UTC) to run cleanup (default: 2 AM)
    cleanup_hour: u32,
    /// Maximum number of days build cache can be unused before deletion (default: 7)
    max_cache_age_days: i64,
}

impl DockerCleanupService {
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

    /// Calculate seconds until the next scheduled cleanup
    fn seconds_until_next_cleanup(&self) -> u64 {
        let now = chrono::Utc::now();

        // Calculate target time (today at cleanup_hour)
        let target_time = now
            .with_hour(self.cleanup_hour)
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

        // Cleanup unused images
        match self.docker_client.prune_images(true).await {
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
        match self
            .docker_client
            .prune_builder_cache(self.max_cache_age_days)
            .await
        {
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

        info!("🧹 Nightly Docker cleanup completed");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Clone)]
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

    #[test]
    fn test_cleanup_hour_calculation() {
        let service = DockerCleanupService::new(Arc::new(DefaultDockerClient));
        let seconds = service.seconds_until_next_cleanup();

        // Should be positive and less than 24 hours
        assert!(seconds > 0);
        assert!(seconds <= 24 * 3600);
    }

    #[test]
    fn test_custom_cleanup_hour() {
        let service = DockerCleanupService::new(Arc::new(DefaultDockerClient)).with_cleanup_hour(3);

        assert_eq!(service.cleanup_hour, 3);
    }

    #[test]
    fn test_custom_cache_age() {
        let service =
            DockerCleanupService::new(Arc::new(DefaultDockerClient)).with_max_cache_age_days(14);

        assert_eq!(service.max_cache_age_days, 14);
    }

    #[tokio::test]
    async fn test_cleanup_service_with_mock() {
        let mock = MockDockerClient {
            prune_images_result: Ok(PruneStats {
                images_deleted: 5,
                space_reclaimed_mb: 1024,
            }),
            prune_cache_result: Ok("Cache cleanup completed".to_string()),
        };

        let service = DockerCleanupService::new(Arc::new(mock));

        // Test cleanup runs without error
        service.perform_cleanup().await;
    }
}
