// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Screenshot Service
//!
//! Main service that manages screenshot providers and configuration

use arc_swap::ArcSwap;
use bytes::Bytes;
use std::path::PathBuf;
use std::sync::{Arc, LazyLock};
use tokio::fs;
use tokio::sync::Semaphore;
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
    provider: ArcSwap<ProviderSlot>,
    durable_store: Option<Arc<dyn FileStore>>,
    /// Limits how many captures hold image data at once; shared by every
    /// service in the process (see [`CAPTURE_PERMITS`]).
    capture_permits: Arc<Semaphore>,
}

/// The active provider and the settings it was built from.
///
/// `source` is `None` when the provider is pinned by an embedder (see
/// [`ScreenshotService::with_provider`]) and must never be replaced. Otherwise the provider is rebuilt
/// whenever Settings → Screenshots selects a different one, so an operator who
/// follows a "provider unavailable" error to Settings does not also need to
/// restart the server.
struct ProviderSlot {
    source: Option<ProviderSource>,
    provider: Arc<dyn ScreenshotProvider>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum ProviderSource {
    Local,
    Remote {
        url: String,
    },
    /// Screenshots switched off (`noop`), e.g. on a host without Chrome.
    Disabled,
}

/// `screenshots.provider` values that select the no-op provider.
const DISABLED_PROVIDER_VALUES: [&str; 3] = ["noop", "disabled", "none"];

/// Deprecated environment variable replaced by `screenshots.provider`.
const SCREENSHOT_PROVIDER_ENV: &str = "TEMPS_SCREENSHOT_PROVIDER";
const SCREENSHOT_PROVIDER_SETTING: &str = "screenshots.provider";

/// One-release shim: read the deprecated variable once, when the service is
/// built at startup. `temps serve --screenshot-provider` still sets it.
#[allow(
    clippy::disallowed_methods,
    reason = "one-release deprecation shim: read once at startup and persisted into \
              screenshots.provider; never read again at runtime"
)]
fn screenshot_provider_env_value() -> Option<String> {
    std::env::var(SCREENSHOT_PROVIDER_ENV).ok()
}

/// What `TEMPS_SCREENSHOT_PROVIDER` means for the stored `screenshots.provider`.
/// Pure, so the shim's decision is testable without process environment.
fn plan_screenshot_provider_env_migration(
    env_value: Option<&str>,
    stored: &temps_core::ScreenshotSettings,
) -> temps_core::EnvSettingMigration<String> {
    let unset = temps_core::ScreenshotSettings::default().provider;
    temps_core::decide_env_setting_migration(env_value, &stored.provider, &unset, |raw| {
        let normalized = raw.to_ascii_lowercase();
        if DISABLED_PROVIDER_VALUES.contains(&normalized.as_str()) {
            Ok("noop".to_string())
        } else if normalized == "local" || normalized == "remote" {
            Ok(normalized)
        } else {
            Err(format!(
                "unknown provider '{raw}'; expected local, remote, noop, disabled or none"
            ))
        }
    })
}

impl ProviderSource {
    fn from_settings(settings: &temps_core::AppSettings) -> Self {
        let provider = settings.screenshots.provider.to_ascii_lowercase();
        if DISABLED_PROVIDER_VALUES.contains(&provider.as_str()) {
            return Self::Disabled;
        }
        if !settings.screenshots.url.is_empty() && settings.screenshots.provider == "remote" {
            Self::Remote {
                url: settings.screenshots.url.clone(),
            }
        } else {
            Self::Local
        }
    }

    fn build(&self) -> ScreenshotResult<Arc<dyn ScreenshotProvider>> {
        match self {
            Self::Remote { url } => {
                info!("Using remote screenshot provider at {}", url);
                Ok(Arc::new(
                    RemoteScreenshotProvider::new(url.clone(), None).map_err(|e| {
                        error!(
                            "Failed to create remote screenshot provider at {}: {}",
                            url, e
                        );
                        e
                    })?,
                ))
            }
            Self::Local => {
                info!("Using local headless Chrome screenshot provider");
                Ok(Arc::new(LocalScreenshotProvider::new()))
            }
            Self::Disabled => {
                info!(
                    "Screenshots are disabled (screenshots.provider = noop); using the noop \
                     provider"
                );
                Ok(Arc::new(NoopScreenshotProvider::new()))
            }
        }
    }
}

impl ProviderSlot {
    fn pinned(provider: Arc<dyn ScreenshotProvider>) -> Self {
        Self {
            source: None,
            provider,
        }
    }
}

impl ScreenshotService {
    /// Create a new screenshot service.
    ///
    /// The provider follows `screenshots.provider` (`local` — headless Chrome,
    /// the default; `remote` with `screenshots.url`; or `noop`), and is
    /// rebuilt whenever that setting changes. A value left in the deprecated
    /// `TEMPS_SCREENSHOT_PROVIDER` is persisted into the setting once at
    /// startup (see [`Self::migrate_provider_env`]).
    pub async fn new(config_service: Arc<ConfigService>) -> ScreenshotResult<Self> {
        Self::new_with_audit(config_service, None).await
    }

    /// [`Self::new`], audit-logging the one-time migration of
    /// `TEMPS_SCREENSHOT_PROVIDER` the way every other migrated variable is.
    /// The plugin passes the server's audit logger; tests pass `None`.
    pub async fn new_with_audit(
        config_service: Arc<ConfigService>,
        audit: Option<&dyn temps_core::AuditLogger>,
    ) -> ScreenshotResult<Self> {
        let mut settings = config_service
            .get_settings()
            .await
            .map_err(|e| ScreenshotError::ConfigError(format!("Failed to get settings: {}", e)))?;

        settings.screenshots =
            Self::migrate_provider_env(&config_service, settings.screenshots, audit).await;
        let slot = Self::slot_from_settings(&settings)?;
        let provider = Arc::clone(&slot.provider);

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
            provider: ArcSwap::from_pointee(slot),
            durable_store,
            capture_permits: CAPTURE_PERMITS.clone(),
        })
    }

    /// One-release shim for `TEMPS_SCREENSHOT_PROVIDER`: persist it into
    /// `screenshots.provider` when that setting was never changed from its
    /// default, otherwise warn that it is ignored. Returns the settings to
    /// use. If the write fails, the value is honoured for this boot only.
    async fn migrate_provider_env(
        config_service: &ConfigService,
        stored: temps_core::ScreenshotSettings,
        audit: Option<&dyn temps_core::AuditLogger>,
    ) -> temps_core::ScreenshotSettings {
        Self::migrate_provider_value(
            screenshot_provider_env_value(),
            config_service,
            stored,
            audit,
        )
        .await
    }

    /// [`Self::migrate_provider_env`] with the variable's value passed in, so
    /// tests can run it start after start without touching process environment.
    async fn migrate_provider_value(
        env_value: Option<String>,
        config_service: &ConfigService,
        stored: temps_core::ScreenshotSettings,
        audit: Option<&dyn temps_core::AuditLogger>,
    ) -> temps_core::ScreenshotSettings {
        let ledger = config_service.env_migration_ledger_or_empty().await;
        let plan = plan_screenshot_provider_env_migration(env_value.as_deref(), &stored)
            .unless_handled(&ledger, SCREENSHOT_PROVIDER_ENV);
        plan.log(SCREENSHOT_PROVIDER_ENV, SCREENSHOT_PROVIDER_SETTING);
        if plan.is_settled_without_write() {
            config_service
                .mark_env_migrations_handled(&[SCREENSHOT_PROVIDER_ENV])
                .await;
        }
        let Some(provider) = plan.into_persist() else {
            return stored;
        };
        let persisted = provider.clone();
        match config_service
            .update_screenshot_settings(move |screenshots| screenshots.provider = persisted)
            .await
        {
            Ok(saved) => {
                config_service
                    .mark_env_migrations_handled(&[SCREENSHOT_PROVIDER_ENV])
                    .await;
                temps_core::record_env_setting_persisted(
                    audit,
                    SCREENSHOT_PROVIDER_ENV,
                    SCREENSHOT_PROVIDER_SETTING,
                )
                .await;
                saved
            }
            Err(e) => {
                error!(
                    "Failed to persist {} into {}; honouring it for this boot only: {}",
                    SCREENSHOT_PROVIDER_ENV, SCREENSHOT_PROVIDER_SETTING, e
                );
                temps_core::ScreenshotSettings { provider, ..stored }
            }
        }
    }

    /// Build a provider that follows database settings (no env var override).
    fn slot_from_settings(settings: &temps_core::AppSettings) -> ScreenshotResult<ProviderSlot> {
        let source = ProviderSource::from_settings(settings);
        Ok(ProviderSlot {
            provider: source.build()?,
            source: Some(source),
        })
    }

    /// The provider to use now, rebuilt first if Settings → Screenshots has
    /// switched provider or remote URL since it was built. Settings come from
    /// `ConfigService`'s in-memory cache, so this adds no database round trip.
    async fn current_provider(&self) -> ScreenshotResult<Arc<dyn ScreenshotProvider>> {
        let slot = self.provider.load_full();
        let Some(current) = slot.source.as_ref() else {
            return Ok(Arc::clone(&slot.provider));
        };
        let settings = self.config_service.get_settings().await.map_err(|e| {
            ScreenshotError::ConfigError(format!(
                "Failed to read screenshot settings to select a provider: {}",
                e
            ))
        })?;
        let wanted = ProviderSource::from_settings(&settings);
        if &wanted == current {
            return Ok(Arc::clone(&slot.provider));
        }
        info!(
            "Screenshot settings changed provider from {:?} to {:?}; switching without restart",
            current, wanted
        );
        let provider = wanted.build()?;
        self.provider.store(Arc::new(ProviderSlot {
            source: Some(wanted),
            provider: Arc::clone(&provider),
        }));
        Ok(provider)
    }

    /// Create a new screenshot service with a custom provider (useful for testing)
    pub fn with_provider(
        config_service: Arc<ConfigService>,
        provider: Arc<dyn ScreenshotProvider>,
    ) -> Self {
        Self {
            config_service,
            provider: ArcSwap::from_pointee(ProviderSlot::pinned(provider)),
            durable_store: None,
            capture_permits: CAPTURE_PERMITS.clone(),
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
            provider: ArcSwap::from_pointee(ProviderSlot::pinned(provider)),
            durable_store: Some(durable_store),
            capture_permits: CAPTURE_PERMITS.clone(),
        }
    }

    /// Capture a screenshot and save it to the static files directory
    pub async fn capture_and_save(&self, url: &str, filename: &str) -> ScreenshotResult<PathBuf> {
        debug!("Capturing screenshot of {} and saving as {}", url, filename);

        // Held until the image is stored: the fetched bytes, their decode and
        // the write all count against it.
        let _capture_permit = self.acquire_capture_permit(url).await?;

        // Capture screenshot
        let image_data = self
            .current_provider()
            .await?
            .capture_screenshot(url)
            .await?;
        // Never store (and let a caller report) something that is not a
        // readable image: a provider can answer with an empty body, an HTML
        // error page, or a truncated file. Decoding is CPU work, so it runs off
        // the async runtime.
        let image_data = validate_image_bounded(DECODE_PERMITS.clone(), url, image_data).await?;

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
        let _capture_permit = self.acquire_capture_permit(url).await?;
        self.current_provider().await?.capture_screenshot(url).await
    }

    /// Wait for a capture permit before fetching anything, so queued captures
    /// hold no image data while they wait.
    async fn acquire_capture_permit(
        &self,
        url: &str,
    ) -> ScreenshotResult<tokio::sync::OwnedSemaphorePermit> {
        self.capture_permits
            .clone()
            .acquire_owned()
            .await
            .map_err(|e| {
                ScreenshotError::CaptureFailed(format!(
                    "Could not schedule the screenshot of {}: {}",
                    url, e
                ))
            })
    }

    #[cfg(test)]
    fn with_capture_permits(mut self, capture_permits: Arc<Semaphore>) -> Self {
        self.capture_permits = capture_permits;
        self
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

    /// Get the name of the provider used by the most recent capture or check
    pub fn provider_name(&self) -> &'static str {
        self.provider.load().provider.provider_name()
    }

    /// Check if the provider is available
    pub async fn is_provider_available(&self) -> bool {
        match self.current_provider().await {
            Ok(provider) => provider.is_available().await,
            Err(e) => {
                warn!("Screenshot provider could not be selected: {}", e);
                false
            }
        }
    }

    /// Check whether the provider is available, returning the reason if it is not
    pub async fn check_provider_availability(&self) -> ScreenshotResult<()> {
        self.current_provider().await?.check_availability().await
    }
}

/// Most pixel memory a screenshot may need to decode. A 1920px-wide full-page
/// capture reaches it at about 34,000px tall; anything larger is refused rather
/// than risking the memory of a small host.
const MAX_DECODED_IMAGE_BYTES: u64 = 256 * 1024 * 1024;

/// Captures that may fetch, validate and store an image at the same time
/// across the whole instance. Each holds at most a 64 MiB provider response
/// and its decoded bytes, and decoding is limited separately below, so
/// screenshot memory stays around 2 × 112 MiB + 256 MiB however many captures
/// are requested; the rest wait without holding any image data.
static CAPTURE_PERMITS: LazyLock<Arc<Semaphore>> = LazyLock::new(|| Arc::new(Semaphore::new(2)));

/// One screenshot is decoded at a time across the whole instance, so however
/// many captures finish together, validation needs at most
/// `MAX_DECODED_IMAGE_BYTES` of pixel memory.
static DECODE_PERMITS: LazyLock<Arc<Semaphore>> = LazyLock::new(|| Arc::new(Semaphore::new(1)));

/// Run [`validate_image_bytes`] on a blocking thread once a decode permit is
/// free, returning the bytes when they are a readable image.
///
/// The permit is moved into the blocking task, so it is held until the decode
/// actually ends even if the caller stops waiting (a capture timeout).
async fn validate_image_bounded(
    permits: Arc<Semaphore>,
    url: &str,
    image_data: Vec<u8>,
) -> ScreenshotResult<Vec<u8>> {
    let permit = permits.acquire_owned().await.map_err(|e| {
        ScreenshotError::CaptureFailed(format!(
            "Could not schedule validation of the screenshot of {}: {}",
            url, e
        ))
    })?;
    tokio::task::spawn_blocking(move || {
        let _permit = permit;
        validate_image_bytes(&image_data).map(|_| image_data)
    })
    .await
    .map_err(|e| {
        ScreenshotError::CaptureFailed(format!(
            "Validating the screenshot of {} failed to run: {}",
            url, e
        ))
    })?
    .map_err(|reason| ScreenshotError::InvalidImage {
        url: url.to_string(),
        reason,
    })
}

/// Check that `bytes` is a readable PNG, JPEG or WebP image rather than an
/// empty body, an error page or a truncated file, returning its format.
///
/// Providers report success as soon as they get *some* bytes back; this is the
/// one place that proves a capture actually produced an image before it is
/// stored and recorded on a deployment. The whole image is decoded, because a
/// valid header says nothing about whether the pixel data that follows is
/// complete.
pub fn validate_image_bytes(bytes: &[u8]) -> Result<&'static str, String> {
    if bytes.is_empty() {
        return Err("the provider returned no data".to_string());
    }
    let (format, name) = if bytes.starts_with(&[0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A]) {
        (image::ImageFormat::Png, "png")
    } else if bytes.starts_with(&[0xFF, 0xD8, 0xFF]) {
        (image::ImageFormat::Jpeg, "jpeg")
    } else if bytes.len() >= 12 && &bytes[0..4] == b"RIFF" && &bytes[8..12] == b"WEBP" {
        (image::ImageFormat::WebP, "webp")
    } else {
        let preview: String = String::from_utf8_lossy(&bytes[..bytes.len().min(32)])
            .chars()
            .map(|c| if c.is_control() { ' ' } else { c })
            .collect();
        return Err(format!(
            "the provider returned {} bytes that are not a PNG, JPEG or WebP image (starts with \"{}\")",
            bytes.len(),
            preview.trim()
        ));
    };

    let mut reader = image::ImageReader::with_format(std::io::Cursor::new(bytes), format);
    let mut limits = image::Limits::default();
    limits.max_alloc = Some(MAX_DECODED_IMAGE_BYTES);
    reader.limits(limits);
    let decoded = reader.decode().map_err(|e| {
        format!(
            "the provider returned {} bytes that start like a {} image but do not decode: {}",
            bytes.len(),
            name,
            e
        )
    })?;
    if decoded.width() == 0 || decoded.height() == 0 {
        return Err(format!(
            "the provider returned a {} image with no pixels ({}x{})",
            name,
            decoded.width(),
            decoded.height()
        ));
    }
    Ok(name)
}

#[cfg(test)]
mod image_validation_tests {
    use super::*;

    /// A real, fully decodable image in `format`.
    fn encoded(format: image::ImageFormat) -> Vec<u8> {
        let pixels = image::RgbImage::from_fn(64, 48, |x, y| {
            image::Rgb([(x * 4) as u8, (y * 5) as u8, 119])
        });
        let mut bytes = Vec::new();
        pixels
            .write_to(&mut std::io::Cursor::new(&mut bytes), format)
            .expect("encode test image");
        bytes
    }

    #[test]
    fn accepts_real_png_jpeg_and_webp() {
        assert_eq!(
            validate_image_bytes(&encoded(image::ImageFormat::Png)),
            Ok("png")
        );
        assert_eq!(
            validate_image_bytes(&encoded(image::ImageFormat::Jpeg)),
            Ok("jpeg")
        );
        assert_eq!(
            validate_image_bytes(&encoded(image::ImageFormat::WebP)),
            Ok("webp")
        );
    }

    #[test]
    fn rejects_empty_data() {
        assert!(validate_image_bytes(&[]).unwrap_err().contains("no data"));
    }

    #[test]
    fn rejects_a_valid_header_without_image_data() {
        let mut header_only = vec![0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A];
        header_only.resize(128, 0);

        let reason = validate_image_bytes(&header_only).unwrap_err();

        assert!(
            reason.contains("start like a png image but do not decode"),
            "{reason}"
        );
    }

    #[test]
    fn rejects_truncated_images_of_every_format() {
        for format in [
            image::ImageFormat::Png,
            image::ImageFormat::Jpeg,
            image::ImageFormat::WebP,
        ] {
            let full = encoded(format);
            let truncated = &full[..full.len() / 2];

            let reason = validate_image_bytes(truncated)
                .expect_err("half an image must not count as a screenshot");

            assert!(reason.contains("do not decode"), "{format:?}: {reason}");
        }
    }

    #[tokio::test]
    async fn decodes_wait_for_a_shared_permit() {
        let permits = Arc::new(Semaphore::new(1));
        let held = permits.clone().acquire_owned().await.unwrap();
        let png = encoded(image::ImageFormat::Png);

        // Another decode holds the only permit, so this one must wait.
        let waited = tokio::time::timeout(
            std::time::Duration::from_millis(100),
            validate_image_bounded(permits.clone(), "http://app.local", png.clone()),
        )
        .await;
        assert!(waited.is_err(), "a decode started without a free permit");

        drop(held);
        let validated = validate_image_bounded(permits.clone(), "http://app.local", png.clone())
            .await
            .expect("decodes once the permit is free");
        assert_eq!(validated, png);
        assert_eq!(permits.available_permits(), 1, "the permit is returned");
    }

    #[tokio::test]
    async fn bounded_validation_reports_unreadable_images() {
        let err = validate_image_bounded(
            Arc::new(Semaphore::new(1)),
            "http://app.local",
            b"<html>oops</html>".to_vec(),
        )
        .await
        .unwrap_err();

        assert!(
            matches!(err, ScreenshotError::InvalidImage { ref url, .. } if url == "http://app.local"),
            "{err}"
        );
    }

    #[test]
    fn rejects_html_error_page_and_names_what_came_back() {
        let html = b"<!DOCTYPE html><html><body>502 Bad Gateway</body></html>".repeat(2);
        let reason = validate_image_bytes(&html).unwrap_err();
        assert!(reason.contains("not a PNG, JPEG or WebP image"), "{reason}");
        assert!(reason.contains("<!DOCTYPE html>"), "{reason}");
    }
}

#[cfg(test)]
mod provider_selection_tests {
    use super::*;
    use sea_orm::{DatabaseBackend, MockDatabase};

    fn settings_row(provider: &str, url: &str) -> temps_entities::settings::Model {
        temps_entities::settings::Model {
            id: 1,
            data: serde_json::json!({
                "screenshots": { "enabled": true, "provider": provider, "url": url }
            }),
            created_at: chrono::Utc::now(),
            updated_at: chrono::Utc::now(),
        }
    }

    fn config_service(db: MockDatabase) -> Arc<ConfigService> {
        Arc::new(ConfigService::new(
            test_server_config(),
            Arc::new(db.into_connection()),
        ))
    }

    fn test_server_config() -> Arc<temps_config::ServerConfig> {
        Arc::new(temps_config::ServerConfig {
            address: "127.0.0.1:3000".to_string(),
            database_url: "postgres://test".to_string(),
            tls_address: None,
            console_address: "127.0.0.1:0".to_string(),
            console_admin_address: None,
            console_ingress_address: None,
            admin_allowed_ips: Vec::new(),
            admin_allowed_hosts: Vec::new(),
            admin_trust_forwarded_for: false,
            docker_extra_networks: Vec::new(),
            data_dir: PathBuf::from("/tmp/temps-test"),
            auth_secret: "test-secret".to_string(),
            encryption_key: "test-key".to_string(),
            api_base_url: "/api".to_string(),
            postgres_max_connections: None,
            postgres_min_connections: None,
            postgres_connect_timeout_secs: None,
            postgres_acquire_timeout_secs: None,
            postgres_idle_timeout_secs: None,
            postgres_max_lifetime_secs: None,
            clickhouse_url: None,
            clickhouse_database: None,
            clickhouse_user: None,
            clickhouse_password: None,
        })
    }

    #[derive(Default)]
    struct RecordingAudit {
        operations: std::sync::Mutex<Vec<String>>,
    }

    #[async_trait::async_trait]
    impl temps_core::AuditLogger for RecordingAudit {
        async fn create_audit_log(
            &self,
            operation: &dyn temps_core::AuditOperation,
        ) -> temps_core::anyhow::Result<()> {
            self.operations
                .lock()
                .unwrap()
                .push(operation.operation_type());
            Ok(())
        }
    }

    /// The provider variable is persisted once, audited like every other
    /// migrated variable, and never re-applied over an admin's later choice
    /// of the default provider.
    #[tokio::test]
    async fn provider_env_is_migrated_once_with_an_audit_record() {
        let test_db = match temps_database::test_utils::TestDatabase::with_migrations().await {
            Ok(db) => db,
            Err(error)
                if temps_database::test_utils::is_container_runtime_unavailable(
                    &error.to_string(),
                ) =>
            {
                eprintln!(
                    "Docker unavailable; skipping screenshot provider migration test: {error}"
                );
                return;
            }
            Err(error) => panic!("failed to create test database: {error}"),
        };
        let config = ConfigService::new(test_server_config(), test_db.db.clone());
        let audit = RecordingAudit::default();
        let start = |stored: temps_core::ScreenshotSettings| {
            ScreenshotService::migrate_provider_value(
                Some("noop".to_string()),
                &config,
                stored,
                Some(&audit),
            )
        };
        let stored = || async { config.get_settings().await.expect("settings").screenshots };

        let first = start(stored().await).await;
        assert_eq!(first.provider, "noop");
        assert_eq!(stored().await.provider, "noop");
        assert_eq!(
            *audit.operations.lock().unwrap(),
            vec!["SETTING_MIGRATED_FROM_ENV".to_string()]
        );

        // The admin picks the default provider again; later starts keep it.
        config
            .update_screenshot_settings(|screenshots| screenshots.provider = "local".to_string())
            .await
            .expect("admin chooses local");
        let later = start(stored().await).await;
        assert_eq!(later.provider, "local");
        assert_eq!(stored().await.provider, "local");
        assert_eq!(audit.operations.lock().unwrap().len(), 1);
    }

    fn settings_driven(
        config_service: Arc<ConfigService>,
        initial: &temps_core::AppSettings,
    ) -> ScreenshotService {
        ScreenshotService {
            config_service,
            provider: ArcSwap::from_pointee(
                ScreenshotService::slot_from_settings(initial).expect("initial provider"),
            ),
            durable_store: None,
            capture_permits: Arc::new(Semaphore::new(1)),
        }
    }

    #[test]
    fn noop_provider_setting_disables_screenshots() {
        let mut settings = temps_core::AppSettings::default();
        for value in ["noop", "disabled", "none", "NOOP"] {
            settings.screenshots.provider = value.to_string();
            assert_eq!(
                ProviderSource::from_settings(&settings),
                ProviderSource::Disabled
            );
        }
        let provider = ProviderSource::Disabled.build().expect("noop builds");
        assert_eq!(
            provider.provider_name(),
            NoopScreenshotProvider::new().provider_name()
        );
    }

    #[test]
    fn screenshot_provider_env_is_persisted_over_the_default_only() {
        use temps_core::EnvSettingMigration;
        let default = temps_core::ScreenshotSettings::default();
        assert_eq!(
            plan_screenshot_provider_env_migration(Some("disabled"), &default),
            EnvSettingMigration::Persist("noop".to_string())
        );
        assert_eq!(
            plan_screenshot_provider_env_migration(Some("remote"), &default),
            EnvSettingMigration::Persist("remote".to_string())
        );
        assert_eq!(
            plan_screenshot_provider_env_migration(Some("local"), &default),
            EnvSettingMigration::AlreadyPersisted
        );
        assert!(matches!(
            plan_screenshot_provider_env_migration(Some("chrome"), &default),
            EnvSettingMigration::Invalid { .. }
        ));
        assert_eq!(
            plan_screenshot_provider_env_migration(None, &default),
            EnvSettingMigration::EnvUnset
        );

        let remote = temps_core::ScreenshotSettings {
            provider: "remote".to_string(),
            ..Default::default()
        };
        assert_eq!(
            plan_screenshot_provider_env_migration(Some("noop"), &remote),
            EnvSettingMigration::SettingAlreadySet
        );
    }

    #[test]
    fn remote_needs_both_the_provider_and_a_url() {
        let mut settings = temps_core::AppSettings::default();
        settings.screenshots.provider = "remote".to_string();
        assert_eq!(
            ProviderSource::from_settings(&settings),
            ProviderSource::Local
        );

        settings.screenshots.url = "http://shots.internal:9000".to_string();
        assert_eq!(
            ProviderSource::from_settings(&settings),
            ProviderSource::Remote {
                url: "http://shots.internal:9000".to_string()
            }
        );

        settings.screenshots.provider = "local".to_string();
        assert_eq!(
            ProviderSource::from_settings(&settings),
            ProviderSource::Local
        );
    }

    #[tokio::test]
    async fn provider_follows_settings_changes_without_restart() {
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results(vec![vec![settings_row("remote", "http://127.0.0.1:9")]])
            .append_query_results(vec![vec![settings_row("local", "")]]);
        let config = config_service(db);
        let service = settings_driven(config.clone(), &temps_core::AppSettings::default());
        assert_eq!(service.provider_name(), "local-headless-chrome");

        // The operator switched to a remote provider in Settings.
        let provider = service.current_provider().await.expect("provider");
        assert_eq!(provider.provider_name(), "remote-api");
        assert_eq!(service.provider_name(), "remote-api");

        // ...and back to the local browser.
        config.invalidate_settings_cache().await;
        let provider = service.current_provider().await.expect("provider");
        assert_eq!(provider.provider_name(), "local-headless-chrome");
    }

    #[tokio::test]
    async fn unchanged_settings_keep_the_same_provider_instance() {
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results(vec![vec![settings_row("remote", "http://127.0.0.1:9")]]);
        let mut initial = temps_core::AppSettings::default();
        initial.screenshots.provider = "remote".to_string();
        initial.screenshots.url = "http://127.0.0.1:9".to_string();
        let service = settings_driven(config_service(db), &initial);
        let before = Arc::clone(&service.provider.load().provider);

        let after = service.current_provider().await.expect("provider");

        assert!(Arc::ptr_eq(&before, &after));
    }

    /// Returns a fixed image and counts how often it was asked for one.
    struct CountingProvider {
        calls: std::sync::atomic::AtomicUsize,
    }

    #[async_trait::async_trait]
    impl ScreenshotProvider for CountingProvider {
        async fn capture_screenshot(&self, _url: &str) -> ScreenshotResult<Vec<u8>> {
            self.calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            Ok(vec![1, 2, 3])
        }

        fn provider_name(&self) -> &'static str {
            "counting"
        }

        async fn check_availability(&self) -> ScreenshotResult<()> {
            Ok(())
        }
    }

    #[tokio::test]
    async fn queued_captures_fetch_nothing_until_a_permit_is_free() {
        let provider = Arc::new(CountingProvider {
            calls: std::sync::atomic::AtomicUsize::new(0),
        });
        let permits = Arc::new(Semaphore::new(1));
        let service = ScreenshotService::with_provider(
            config_service(MockDatabase::new(DatabaseBackend::Postgres)),
            provider.clone(),
        )
        .with_capture_permits(permits.clone());
        let running = permits.clone().acquire_owned().await.unwrap();

        let waited = tokio::time::timeout(
            std::time::Duration::from_millis(100),
            service.capture("http://app.local"),
        )
        .await;
        assert!(waited.is_err(), "a capture started without a free permit");
        assert_eq!(
            provider.calls.load(std::sync::atomic::Ordering::SeqCst),
            0,
            "a queued capture must not fetch image data"
        );

        drop(running);
        service.capture("http://app.local").await.unwrap();
        assert_eq!(provider.calls.load(std::sync::atomic::Ordering::SeqCst), 1);
        assert_eq!(permits.available_permits(), 1, "the permit is returned");
    }

    #[tokio::test]
    async fn pinned_provider_never_reads_settings() {
        // No query results: reading settings would fail the call.
        let config = config_service(MockDatabase::new(DatabaseBackend::Postgres));
        let service =
            ScreenshotService::with_provider(config, Arc::new(NoopScreenshotProvider::new()));

        let provider = service.current_provider().await.expect("pinned provider");

        assert_eq!(provider.provider_name(), "noop");
    }
}
