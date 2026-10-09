// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Screenshot Service
//!
//! Main service that manages screenshot providers and configuration

use bytes::Bytes;
use std::path::PathBuf;
use std::sync::Arc;
use tokio::fs;
use tracing::{debug, error, info, warn};

use temps_config::ConfigService;
use temps_file_store::s3_config::StaticStorageBackend;
use temps_file_store::{s3_store::S3FileStore, FileStore};

use crate::error::{ScreenshotError, ScreenshotResult};
use crate::local_provider::LocalScreenshotProvider;
use crate::noop_provider::NoopScreenshotProvider;
use crate::provider::ScreenshotProvider;
use crate::remote_provider::RemoteScreenshotProvider;

/// Screenshot service that manages providers and storage
pub struct ScreenshotService {
    config_service: Arc<ConfigService>,
    provider: Arc<dyn ScreenshotProvider>,
    durable_store: Option<Arc<dyn FileStore>>,
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

        // Spawn a background task so the availability check never blocks plugin
        // registration. The check is purely diagnostic — the provider was already
        // selected above — so delaying the warning until shortly after startup is
        // indistinguishable from surfacing it inline, except that it no longer
        // stalls `initialize_plugins()` (and therefore the proxy's ready signal)
        // when the check is slow (e.g. headless Chrome is absent and the 10 s
        // launch timeout fires).
        {
            let provider_check = Arc::clone(&provider);
            tokio::spawn(async move {
                if let Err(e) = provider_check.check_availability().await {
                    warn!(
                        "Screenshot provider '{}' may not be available: {}",
                        provider_check.provider_name(),
                        e
                    );
                }
            });
        }

        let instance_id = config_service
            .stateless_instance_id()
            .await
            .map_err(|error| {
                ScreenshotError::ConfigError(format!(
                    "Failed to read persisted installation mode: {error}"
                ))
            })?;
        let stateless =
            temps_file_store::s3_config::resolve_stateless_storage_for(instance_id.as_deref())
                .map_err(|error| {
                    ScreenshotError::ConfigError(format!(
                        "Failed to resolve screenshot storage: {error}"
                    ))
                })?;
        let durable_store =
            match temps_file_store::s3_config::resolve_static_storage_backend_for(&stateless)
                .map_err(|error| {
                    ScreenshotError::ConfigError(format!(
                        "Failed to resolve screenshot storage: {error}"
                    ))
                })? {
                StaticStorageBackend::Filesystem => None,
                StaticStorageBackend::S3(config) => {
                    Some(Arc::new(S3FileStore::new(config)) as Arc<dyn FileStore>)
                }
            };

        Ok(Self {
            config_service,
            provider,
            durable_store,
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
            durable_store: None,
        }
    }

    /// Create a screenshot service with explicit durable storage.
    ///
    /// This keeps tests and embedders independent from process environment
    /// while exercising the same write path used in stateless mode.
    pub fn with_provider_and_store(
        config_service: Arc<ConfigService>,
        provider: Arc<dyn ScreenshotProvider>,
        durable_store: Arc<dyn FileStore>,
    ) -> Self {
        Self {
            config_service,
            provider,
            durable_store: Some(durable_store),
        }
    }

    /// Capture a screenshot and save it to the static files directory
    pub async fn capture_and_save(&self, url: &str, filename: &str) -> ScreenshotResult<PathBuf> {
        debug!("Capturing screenshot of {} and saving as {}", url, filename);

        // Capture screenshot
        let image_data = self.provider.capture_screenshot(url).await?;
        // Never store (and let a caller report) something that is not an image:
        // a provider can answer with an empty body or an HTML error page.
        validate_image_bytes(&image_data).map_err(|reason| ScreenshotError::InvalidImage {
            url: url.to_string(),
            reason,
        })?;

        if let Some(store) = &self.durable_store {
            store
                .put(filename, Bytes::from(image_data))
                .await
                .map_err(|error| ScreenshotError::Storage {
                    path: filename.to_string(),
                    reason: error.to_string(),
                })?;
            info!(
                path = filename,
                "Screenshot saved to durable object storage"
            );
            // Callers persist only the relative filename in Postgres. Returning
            // its conventional local-shaped path preserves the existing API;
            // no local file is created in stateless mode.
            return Ok(self.config_service.static_dir().join(filename));
        }

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

    /// Check whether the provider is available, returning the reason if it is not
    pub async fn check_provider_availability(&self) -> ScreenshotResult<()> {
        self.provider.check_availability().await
    }
}

/// Smallest byte count accepted as a real screenshot. A valid 1x1 PNG is
/// ~67 bytes; anything shorter is a truncated or placeholder response.
const MIN_IMAGE_BYTES: usize = 64;

/// Check that `bytes` is a PNG, JPEG or WebP image rather than an empty body or
/// an error page, returning the detected format.
///
/// Providers report success as soon as they get *some* bytes back; this is the
/// one place that proves a capture actually produced an image before it is
/// stored and recorded on a deployment.
pub fn validate_image_bytes(bytes: &[u8]) -> Result<&'static str, String> {
    if bytes.is_empty() {
        return Err("the provider returned no data".to_string());
    }
    if bytes.len() < MIN_IMAGE_BYTES {
        return Err(format!(
            "the provider returned only {} bytes (expected at least {})",
            bytes.len(),
            MIN_IMAGE_BYTES
        ));
    }
    if bytes.starts_with(&[0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A]) {
        return Ok("png");
    }
    if bytes.starts_with(&[0xFF, 0xD8, 0xFF]) {
        return Ok("jpeg");
    }
    if bytes.len() >= 12 && &bytes[0..4] == b"RIFF" && &bytes[8..12] == b"WEBP" {
        return Ok("webp");
    }
    let preview: String = String::from_utf8_lossy(&bytes[..bytes.len().min(32)])
        .chars()
        .map(|c| if c.is_control() { ' ' } else { c })
        .collect();
    Err(format!(
        "the provider returned {} bytes that are not a PNG, JPEG or WebP image (starts with \"{}\")",
        bytes.len(),
        preview.trim()
    ))
}

#[cfg(test)]
mod image_validation_tests {
    use super::*;

    fn png(len: usize) -> Vec<u8> {
        let mut bytes = vec![0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A];
        bytes.resize(len, 0);
        bytes
    }

    #[test]
    fn accepts_png_jpeg_and_webp() {
        assert_eq!(validate_image_bytes(&png(128)), Ok("png"));

        let mut jpeg = vec![0xFF, 0xD8, 0xFF, 0xE0];
        jpeg.resize(128, 0);
        assert_eq!(validate_image_bytes(&jpeg), Ok("jpeg"));

        let mut webp = b"RIFF\0\0\0\0WEBPVP8 ".to_vec();
        webp.resize(128, 0);
        assert_eq!(validate_image_bytes(&webp), Ok("webp"));
    }

    #[test]
    fn rejects_empty_and_truncated_data() {
        assert!(validate_image_bytes(&[]).unwrap_err().contains("no data"));
        assert!(validate_image_bytes(&png(16))
            .unwrap_err()
            .contains("only 16 bytes"));
    }

    #[test]
    fn rejects_html_error_page_and_names_what_came_back() {
        let html = b"<!DOCTYPE html><html><body>502 Bad Gateway</body></html>".repeat(2);
        let reason = validate_image_bytes(&html).unwrap_err();
        assert!(reason.contains("not a PNG, JPEG or WebP image"), "{reason}");
        assert!(reason.contains("<!DOCTYPE html>"), "{reason}");
    }
}
