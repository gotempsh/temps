//! Screenshot Service
//!
//! Main service that manages screenshot providers and configuration

use std::path::PathBuf;
use std::sync::Arc;
use tokio::fs;
use tracing::{debug, error, info, warn};

use temps_config::ConfigService;

use crate::error::{ScreenshotError, ScreenshotResult};
use crate::local_provider::LocalScreenshotProvider;
use crate::noop_provider::NoopScreenshotProvider;
use crate::provider::ScreenshotProvider;
use crate::remote_provider::RemoteScreenshotProvider;

/// Screenshot service that manages providers and storage
pub struct ScreenshotService {
    config_service: Arc<ConfigService>,
    provider: Arc<dyn ScreenshotProvider>,
}

impl ScreenshotService {
    /// Create a new screenshot service
    ///
    /// Provider selection priority:
    /// 1. Environment variable `TEMPS_SCREENSHOT_PROVIDER` (values: "noop", "local", "remote")
    /// 2. Settings in database (screenshots.provider)
    /// 3. Default to "local" (headless Chrome)
    pub async fn new(config_service: Arc<ConfigService>) -> ScreenshotResult<Self> {
        let settings = config_service
            .get_settings()
            .await
            .map_err(|e| ScreenshotError::ConfigError(format!("Failed to get settings: {}", e)))?;

        // Check environment variable first (highest priority)
        let env_provider = std::env::var("TEMPS_SCREENSHOT_PROVIDER").ok();

        // Determine which provider to use
        let provider: Arc<dyn ScreenshotProvider> = match env_provider.as_deref() {
            Some("noop") | Some("disabled") | Some("none") => {
                info!(
                    "Using noop screenshot provider (TEMPS_SCREENSHOT_PROVIDER={}). \
                    Screenshots are disabled.",
                    env_provider.as_deref().unwrap_or("noop")
                );
                Arc::new(NoopScreenshotProvider::new())
            }
            Some("remote") => {
                if settings.screenshots.url.is_empty() {
                    return Err(ScreenshotError::ConfigError(
                        "TEMPS_SCREENSHOT_PROVIDER=remote but screenshots.url is not configured"
                            .to_string(),
                    ));
                }
                info!(
                    "Using remote screenshot provider at {} (from TEMPS_SCREENSHOT_PROVIDER)",
                    settings.screenshots.url
                );
                Arc::new(
                    RemoteScreenshotProvider::new(settings.screenshots.url.clone(), None).map_err(
                        |e| {
                            error!("Failed to create remote screenshot provider: {}", e);
                            e
                        },
                    )?,
                )
            }
            Some("local") => {
                info!("Using local headless Chrome screenshot provider (from TEMPS_SCREENSHOT_PROVIDER)");
                Arc::new(LocalScreenshotProvider::new())
            }
            Some(unknown) => {
                warn!(
                    "Unknown TEMPS_SCREENSHOT_PROVIDER value '{}', falling back to settings or default",
                    unknown
                );
                Self::create_provider_from_settings(&settings).await?
            }
            None => {
                // No env var, use settings or default
                Self::create_provider_from_settings(&settings).await?
            }
        };

        // Check if provider is available
        if !provider.is_available().await {
            warn!(
                "Screenshot provider '{}' may not be available",
                provider.provider_name()
            );
        }

        Ok(Self {
            config_service,
            provider,
        })
    }

    /// Create provider based on database settings (fallback when no env var)
    async fn create_provider_from_settings(
        settings: &temps_core::AppSettings,
    ) -> ScreenshotResult<Arc<dyn ScreenshotProvider>> {
        if !settings.screenshots.url.is_empty() && settings.screenshots.provider == "remote" {
            info!(
                "Using remote screenshot provider at {}",
                settings.screenshots.url
            );
            Ok(Arc::new(
                RemoteScreenshotProvider::new(settings.screenshots.url.clone(), None).map_err(
                    |e| {
                        error!("Failed to create remote screenshot provider: {}", e);
                        e
                    },
                )?,
            ))
        } else {
            info!("Using local headless Chrome screenshot provider");
            Ok(Arc::new(LocalScreenshotProvider::new()))
        }
    }

    /// Create a new screenshot service with a custom provider (useful for testing)
    pub fn with_provider(
        config_service: Arc<ConfigService>,
        provider: Arc<dyn ScreenshotProvider>,
    ) -> Self {
        Self {
            config_service,
            provider,
        }
    }

    /// Capture a screenshot and save it to the static files directory
    pub async fn capture_and_save(&self, url: &str, filename: &str) -> ScreenshotResult<PathBuf> {
        debug!("Capturing screenshot of {} and saving as {}", url, filename);

        // Capture screenshot
        let image_data = self.provider.capture_screenshot(url).await?;

        // Get static directory from config
        let static_dir = self.config_service.static_dir();

        // Ensure static directory exists
        fs::create_dir_all(&static_dir).await.map_err(|e| {
            error!("Failed to create static directory: {}", e);
            ScreenshotError::Io(e)
        })?;

        // Create full path
        let file_path = static_dir.join(filename);

        // Ensure parent directory exists
        if let Some(parent) = file_path.parent() {
            fs::create_dir_all(parent).await.map_err(|e| {
                error!("Failed to create screenshot directory: {}", e);
                ScreenshotError::Io(e)
            })?;
        }

        // Save the file
        fs::write(&file_path, &image_data).await.map_err(|e| {
            error!(
                "Failed to write screenshot to {}: {}",
                file_path.display(),
                e
            );
            ScreenshotError::Io(e)
        })?;

        info!(
            "Screenshot saved to {} ({} bytes)",
            file_path.display(),
            image_data.len()
        );

        Ok(file_path)
    }

    /// Capture a screenshot and return the image bytes (without saving)
    pub async fn capture(&self, url: &str) -> ScreenshotResult<Vec<u8>> {
        debug!("Capturing screenshot of {}", url);
        self.provider.capture_screenshot(url).await
    }

    /// Check if screenshots are enabled in configuration
    pub async fn is_enabled(&self) -> bool {
        self.config_service
            .get_settings()
            .await
            .ok()
            .map(|s| s.screenshots.enabled)
            .unwrap_or(false)
    }

    /// Get the name of the current provider
    pub fn provider_name(&self) -> &'static str {
        self.provider.provider_name()
    }

    /// Check if the provider is available
    pub async fn is_provider_available(&self) -> bool {
        self.provider.is_available().await
    }
}
