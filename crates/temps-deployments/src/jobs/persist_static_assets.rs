// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Persist Static Assets Job
//!
//! Extracts immutable static assets (JS chunks, CSS, fonts, images) from the built
//! Docker image and persists them to a path-keyed file store. This ensures old clients
//! can still fetch chunks from previous deployments after a new deployment goes live.
//!
//! The proxy looks up assets by URL path in the file store — no database table needed.

use async_trait::async_trait;
use bytes::Bytes;
use sea_orm::{ActiveModelTrait, Set};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use temps_core::static_files::{MAX_PUBLIC_STATIC_ASSET_BYTES, MAX_STATIC_PATH_COMPONENTS};
use temps_core::{JobResult, WorkflowContext, WorkflowError, WorkflowTask};
use temps_database::DbConnection;
use temps_deployer::ImageBuilder;
use temps_entities::static_asset_cache;
use temps_file_store::{FileStore, FileStoreError};
use temps_logs::{LogLevel, LogService};
use tracing::{debug, info, warn};

use crate::jobs::ImageOutput;

/// Output from PersistStaticAssetsJob
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PersistStaticAssetsOutput {
    pub assets_persisted: u32,
    pub total_size_bytes: u64,
}

/// Job that extracts immutable static assets from the built image and
/// persists them to a path-keyed file store for stale-chunk fallback serving.
pub struct PersistStaticAssetsJob {
    job_id: String,
    deployment_id: i32,
    project_id: i32,
    environment_id: i32,
    build_job_id: String,
    /// Paths inside the container to search for static assets
    search_paths: Vec<String>,
    /// Path rewrite rules applied to stored file paths.
    /// Each tuple is (from, to) — e.g., (".next", "_next") rewrites
    /// container paths like `/.next/static/...` to `/_next/static/...`
    /// so they match browser request URLs.
    path_rewrites: Vec<(String, String)>,
    image_builder: Arc<dyn ImageBuilder>,
    /// Legacy chunks directory (backward compat, will be removed).
    chunks_dir: PathBuf,
    /// Content-addressable blob store for persisting assets.
    file_store: Option<Arc<dyn FileStore>>,
    /// Database connection for storing URL→hash mappings.
    db: Option<Arc<DbConnection>>,
    log_id: Option<String>,
    log_service: Option<Arc<LogService>>,
}

impl std::fmt::Debug for PersistStaticAssetsJob {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PersistStaticAssetsJob")
            .field("job_id", &self.job_id)
            .field("deployment_id", &self.deployment_id)
            .field("build_job_id", &self.build_job_id)
            .field("search_paths", &self.search_paths)
            .field("path_rewrites", &self.path_rewrites)
            .field("chunks_dir", &self.chunks_dir)
            .finish()
    }
}

/// File extensions to persist as static assets.
const STATIC_ASSET_EXTENSIONS: &[&str] = &[
    "js", "css", "woff", "woff2", "ttf", "eot", "png", "jpg", "jpeg", "gif", "svg", "webp", "avif",
    "ico", "json", "txt", "xml",
];

#[derive(Debug, thiserror::Error)]
enum PersistStaticAssetError {
    #[error("Failed to inspect static asset '{path}': {source}")]
    Metadata {
        path: String,
        #[source]
        source: std::io::Error,
    },
    #[error("Failed to read static asset '{path}': {source}")]
    Read {
        path: String,
        #[source]
        source: std::io::Error,
    },
    #[error("Failed to store static asset '{path}' in CAS: {source}")]
    Store {
        path: String,
        #[source]
        source: FileStoreError,
    },
}

struct PersistedStaticAsset {
    content_hash: Option<String>,
    size_bytes: u64,
}

impl PersistStaticAssetsJob {
    async fn persist_asset_blob(
        file_store: Option<&dyn FileStore>,
        asset_path: &Path,
    ) -> Result<Option<PersistedStaticAsset>, PersistStaticAssetError> {
        let metadata = tokio::fs::metadata(asset_path).await.map_err(|source| {
            PersistStaticAssetError::Metadata {
                path: asset_path.display().to_string(),
                source,
            }
        })?;
        if !metadata.is_file() || metadata.len() > MAX_PUBLIC_STATIC_ASSET_BYTES {
            return Ok(None);
        }

        let file_bytes =
            tokio::fs::read(asset_path)
                .await
                .map_err(|source| PersistStaticAssetError::Read {
                    path: asset_path.display().to_string(),
                    source,
                })?;
        let size_bytes = file_bytes.len() as u64;
        if size_bytes != metadata.len() || size_bytes > MAX_PUBLIC_STATIC_ASSET_BYTES {
            return Ok(None);
        }

        let content_hash = if let Some(file_store) = file_store {
            Some(
                file_store
                    .put_blob(Bytes::from(file_bytes))
                    .await
                    .map_err(|source| PersistStaticAssetError::Store {
                        path: asset_path.display().to_string(),
                        source,
                    })?,
            )
        } else {
            None
        };
        Ok(Some(PersistedStaticAsset {
            content_hash,
            size_bytes,
        }))
    }

    #[allow(clippy::too_many_arguments)]
    pub fn new(
        job_id: String,
        deployment_id: i32,
        project_id: i32,
        environment_id: i32,
        build_job_id: String,
        search_paths: Vec<String>,
        path_rewrites: Vec<(String, String)>,
        image_builder: Arc<dyn ImageBuilder>,
        chunks_dir: PathBuf,
    ) -> Self {
        Self {
            job_id,
            deployment_id,
            project_id,
            environment_id,
            build_job_id,
            search_paths,
            path_rewrites,
            image_builder,
            chunks_dir,
            file_store: None,
            db: None,
            log_id: None,
            log_service: None,
        }
    }

    pub fn with_file_store(mut self, file_store: Arc<dyn FileStore>) -> Self {
        self.file_store = Some(file_store);
        self
    }

    pub fn with_db(mut self, db: Arc<DbConnection>) -> Self {
        self.db = Some(db);
        self
    }

    pub fn with_log_id(mut self, log_id: String) -> Self {
        self.log_id = Some(log_id);
        self
    }

    pub fn with_log_service(mut self, log_service: Arc<LogService>) -> Self {
        self.log_service = Some(log_service);
        self
    }

    async fn log(&self, message: String) -> Result<(), WorkflowError> {
        let level = Self::detect_log_level(&message);

        if let (Some(log_id), Some(log_service)) = (&self.log_id, &self.log_service) {
            log_service
                .append_structured_log(log_id, level, message)
                .await
                .map_err(|e| {
                    WorkflowError::JobExecutionFailed(format!("Failed to write log: {}", e))
                })?;
        }

        Ok(())
    }

    fn detect_log_level(message: &str) -> LogLevel {
        if message.contains("✅") || message.contains("Complete") || message.contains("success") {
            LogLevel::Success
        } else if message.contains("❌") || message.contains("Failed") || message.contains("Error")
        {
            LogLevel::Error
        } else if message.contains("⚠️") || message.contains("Warning") {
            LogLevel::Warning
        } else {
            LogLevel::Info
        }
    }

    /// Check if a file is a static asset we should persist (by extension).
    fn is_static_asset(path: &Path) -> bool {
        path.extension()
            .and_then(|ext| ext.to_str())
            .map(|ext| STATIC_ASSET_EXTENSIONS.contains(&ext.to_lowercase().as_str()))
            .unwrap_or(false)
    }

    /// Recursively find all static asset files in a directory, excluding .map files.
    fn find_static_assets(dir: &Path) -> Vec<PathBuf> {
        let mut assets = Vec::new();
        Self::walk_dir_for_assets(dir, 0, &mut assets);
        assets
    }

    fn walk_dir_for_assets(dir: &Path, depth: usize, assets: &mut Vec<PathBuf>) {
        if depth >= MAX_STATIC_PATH_COMPONENTS {
            return;
        }
        let entries = match std::fs::read_dir(dir) {
            Ok(e) => e,
            Err(_) => return,
        };

        for entry in entries.flatten() {
            let path = entry.path();
            let Ok(metadata) = std::fs::symlink_metadata(&path) else {
                continue;
            };
            if metadata.file_type().is_symlink() {
                continue;
            }
            if metadata.is_dir() {
                Self::walk_dir_for_assets(&path, depth + 1, assets);
            } else if metadata.is_file() && Self::is_static_asset(&path) {
                // Exclude source map files — handled by CaptureSourceMapsJob
                if path.extension().and_then(|e| e.to_str()) != Some("map") {
                    assets.push(path);
                }
            }
        }
    }

    /// Apply path rewrites to a relative path.
    /// e.g., ".next/static/chunks/main.js" → "_next/static/chunks/main.js"
    fn apply_rewrites(&self, path: &str) -> String {
        let mut result = path.to_string();
        for (from, to) in &self.path_rewrites {
            result = result.replace(from.as_str(), to.as_str());
        }
        result
    }
}

#[async_trait]
impl WorkflowTask for PersistStaticAssetsJob {
    fn job_id(&self) -> &str {
        &self.job_id
    }

    fn name(&self) -> &str {
        "Persist Static Assets"
    }

    fn description(&self) -> &str {
        "Extract immutable static assets for stale-chunk fallback"
    }

    fn depends_on(&self) -> Vec<String> {
        vec![self.build_job_id.clone()]
    }

    async fn execute(&self, context: WorkflowContext) -> Result<JobResult, WorkflowError> {
        info!(
            "Starting static asset persistence for deployment {}",
            self.deployment_id
        );

        self.log("📦 Persisting static assets for stale-chunk fallback...".to_string())
            .await?;

        // Get image tag from build job output
        let image_output = ImageOutput::from_context(&context, &self.build_job_id)?;
        let image_tag = &image_output.image_tag;

        // Detect the image's WORKDIR to resolve relative search paths
        let workdir = match self.image_builder.inspect_image(image_tag).await {
            Ok(info) => {
                let wd = info.working_dir.unwrap_or_else(|| "/app".to_string());
                self.log(format!("Detected image WORKDIR: {}", wd)).await?;
                wd
            }
            Err(e) => {
                warn!("Could not inspect image, defaulting WORKDIR to /app: {}", e);
                self.log(format!(
                    "⚠️ Could not inspect image WORKDIR, defaulting to /app: {}",
                    e
                ))
                .await?;
                "/app".to_string()
            }
        };

        // Clean up old chunk directories before creating the new one.
        // Keep only the most recent 2 directories (by name, which is the deployment ID).
        // This bounds disk usage even during rapid successive deploys.
        if let Some(env_dir) = self.chunks_dir.parent() {
            if env_dir.exists() {
                if let Ok(entries) = std::fs::read_dir(env_dir) {
                    let mut dirs: Vec<_> = entries
                        .filter_map(|e| e.ok())
                        .filter(|e| e.file_type().map(|t| t.is_dir()).unwrap_or(false))
                        .collect();
                    // Sort descending by name (highest deployment ID first = newest)
                    dirs.sort_by_key(|b| std::cmp::Reverse(b.file_name()));

                    // Remove all but the 2 most recent
                    for dir in dirs.iter().skip(2) {
                        if let Err(e) = std::fs::remove_dir_all(dir.path()) {
                            warn!(
                                "Failed to remove old chunk dir {}: {}",
                                dir.path().display(),
                                e
                            );
                        } else {
                            debug!("Cleaned up old chunk dir: {}", dir.path().display());
                        }
                    }
                }
            }
        }

        // Ensure chunks directory exists
        tokio::fs::create_dir_all(&self.chunks_dir)
            .await
            .map_err(|e| {
                WorkflowError::JobExecutionFailed(format!(
                    "Failed to create chunks directory {}: {}",
                    self.chunks_dir.display(),
                    e
                ))
            })?;

        let mut total_assets: u32 = 0;
        let mut total_size: u64 = 0;

        for search_path in &self.search_paths {
            // Resolve search path: if relative, prepend WORKDIR
            let absolute_search_path = if search_path.starts_with('/') {
                search_path.clone()
            } else {
                format!(
                    "{}/{}",
                    workdir.trim_end_matches('/'),
                    search_path.trim_start_matches('/')
                )
            };

            // Acquire first so reverse drop order removes the extracted
            // directory before releasing capacity to another extraction.
            let _extraction_permit = super::acquire_archive_extraction_permit().await?;

            // RAII ownership guarantees cleanup on every continue/return path;
            // tempfile creates owner-only directories on Unix.
            let temp_dir = match tempfile::Builder::new()
                .prefix("temps-persist-assets-")
                .tempdir()
            {
                Ok(temp_dir) => temp_dir,
                Err(error) => {
                    warn!("Failed to create secure temp directory: {}", error);
                    self.log(format!(
                        "⚠️ Failed to create secure temp directory: {error}"
                    ))
                    .await?;
                    continue;
                }
            };

            // Extract files from the container image
            self.log(format!(
                "📂 Extracting static assets from: {}",
                absolute_search_path
            ))
            .await?;

            let extraction_result = self
                .image_builder
                .extract_from_image(image_tag, &absolute_search_path, temp_dir.path())
                .await;
            match extraction_result {
                Ok(()) => {
                    debug!(
                        "Extracted files from {} to {}",
                        absolute_search_path,
                        temp_dir.path().display()
                    );
                }
                Err(e) => {
                    warn!(
                        "Could not extract from '{}': {} (skipping)",
                        absolute_search_path, e
                    );
                    self.log(format!(
                        "⚠️ Path '{}' not found in image (skipping): {}",
                        absolute_search_path, e
                    ))
                    .await?;
                    continue;
                }
            }

            // Find all static asset files (excluding .map files)
            let asset_files = Self::find_static_assets(temp_dir.path());

            if asset_files.is_empty() {
                self.log(format!(
                    "No static assets found in {}",
                    absolute_search_path
                ))
                .await?;
                continue;
            }

            self.log(format!(
                "Found {} static asset(s) in {}",
                asset_files.len(),
                absolute_search_path
            ))
            .await?;

            // Persist each asset: store blob in CAS + insert DB row for URL→hash mapping
            for asset_path in &asset_files {
                let relative_path = asset_path
                    .strip_prefix(temp_dir.path())
                    .unwrap_or(asset_path);

                let container_relative = format!(
                    "{}/{}",
                    search_path.trim_start_matches('/'),
                    relative_path.display()
                );
                let url_path = self.apply_rewrites(&container_relative);

                let persisted = match Self::persist_asset_blob(
                    self.file_store.as_deref(),
                    asset_path,
                )
                .await
                {
                    Ok(Some(persisted)) => persisted,
                    Ok(None) => {
                        warn!(
                            asset_path = %asset_path.display(),
                            maximum_size_bytes = MAX_PUBLIC_STATIC_ASSET_BYTES,
                            "Skipping static asset because it is oversized, not a file, or changed while reading"
                        );
                        continue;
                    }
                    Err(error) => {
                        warn!(asset_path = %asset_path.display(), reason = %error, "Failed to persist static asset");
                        continue;
                    }
                };

                // Insert URL→hash mapping in database
                if let (Some(hash), Some(db)) = (&persisted.content_hash, &self.db) {
                    let record = static_asset_cache::ActiveModel {
                        url_path: Set(url_path.clone()),
                        content_hash: Set(hash.clone()),
                        project_id: Set(self.project_id),
                        environment_id: Set(self.environment_id),
                        deployment_id: Set(self.deployment_id),
                        size_bytes: Set(persisted.size_bytes as i64),
                        created_at: Set(chrono::Utc::now()),
                        ..Default::default()
                    };
                    if let Err(e) = record.insert(db.as_ref()).await {
                        warn!(
                            "Failed to insert static_asset_cache row for {}: {}",
                            url_path, e
                        );
                    }
                }

                total_assets += 1;
                total_size += persisted.size_bytes;
            }
        }

        if total_assets > 0 {
            self.log(format!(
                "✅ Persisted {} static asset(s) ({} bytes) for deployment {}",
                total_assets, total_size, self.deployment_id
            ))
            .await?;
        } else {
            self.log("No static assets found to persist".to_string())
                .await?;
        }

        info!(
            "Static asset persistence completed for deployment {}: {} assets ({} bytes)",
            self.deployment_id, total_assets, total_size
        );

        let mut updated_context = context.clone();
        updated_context.set_output(&self.job_id, "assets_persisted", total_assets)?;
        updated_context.set_output(&self.job_id, "total_size_bytes", total_size)?;

        Ok(JobResult::success(updated_context))
    }

    async fn validate_prerequisites(&self, context: &WorkflowContext) -> Result<(), WorkflowError> {
        // Verify build job output exists
        let _image_output = ImageOutput::from_context(context, &self.build_job_id)?;

        if self.search_paths.is_empty() {
            return Err(WorkflowError::JobValidationFailed(
                "search_paths cannot be empty".to_string(),
            ));
        }

        Ok(())
    }

    async fn cleanup(&self, _context: &WorkflowContext) -> Result<(), WorkflowError> {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use temps_file_store::OpenedBlob;

    #[test]
    fn extracted_static_asset_temp_directory_is_removed_on_scope_exit() {
        let extraction_path;
        {
            let temp_dir = tempfile::Builder::new()
                .prefix("temps-persist-assets-test-")
                .tempdir()
                .expect("create extraction directory");
            extraction_path = temp_dir.path().to_path_buf();
            std::fs::write(temp_dir.path().join("app.js"), b"temporary asset")
                .expect("write temporary asset");
        }
        assert!(!extraction_path.exists());
    }

    #[test]
    fn test_detect_log_level() {
        assert!(matches!(
            PersistStaticAssetsJob::detect_log_level("✅ Done"),
            LogLevel::Success
        ));
        assert!(matches!(
            PersistStaticAssetsJob::detect_log_level("❌ Failed"),
            LogLevel::Error
        ));
        assert!(matches!(
            PersistStaticAssetsJob::detect_log_level("⚠️ Warning"),
            LogLevel::Warning
        ));
        assert!(matches!(
            PersistStaticAssetsJob::detect_log_level("Processing..."),
            LogLevel::Info
        ));
    }

    #[test]
    fn test_is_static_asset() {
        assert!(PersistStaticAssetsJob::is_static_asset(Path::new(
            "main-abc123.js"
        )));
        assert!(PersistStaticAssetsJob::is_static_asset(Path::new(
            "styles.css"
        )));
        assert!(PersistStaticAssetsJob::is_static_asset(Path::new(
            "font.woff2"
        )));
        assert!(PersistStaticAssetsJob::is_static_asset(Path::new(
            "logo.svg"
        )));
        assert!(PersistStaticAssetsJob::is_static_asset(Path::new(
            "image.webp"
        )));

        // .map files are NOT static assets (handled by source maps job)
        assert!(!PersistStaticAssetsJob::is_static_asset(Path::new(
            "main.js.map"
        )));
        // Unknown extensions
        assert!(!PersistStaticAssetsJob::is_static_asset(Path::new(
            "README.md"
        )));
        assert!(!PersistStaticAssetsJob::is_static_asset(Path::new(
            "server.ts"
        )));
    }

    #[test]
    fn test_apply_rewrites_nextjs() {
        let job = PersistStaticAssetsJob::new(
            "test".to_string(),
            1,
            1,
            1,
            "build".to_string(),
            vec![],
            vec![(".next".to_string(), "_next".to_string())],
            Arc::new(MockImageBuilder),
            PathBuf::from("/tmp/chunks"),
        );

        assert_eq!(
            job.apply_rewrites(".next/static/chunks/main-abc123.js"),
            "_next/static/chunks/main-abc123.js"
        );
    }

    #[test]
    fn test_apply_rewrites_vite_strip_dist() {
        let job = PersistStaticAssetsJob::new(
            "test".to_string(),
            1,
            1,
            1,
            "build".to_string(),
            vec![],
            vec![("dist/".to_string(), String::new())],
            Arc::new(MockImageBuilder),
            PathBuf::from("/tmp/chunks"),
        );

        assert_eq!(
            job.apply_rewrites("dist/assets/index-abc123.js"),
            "assets/index-abc123.js"
        );
    }

    /// Simulate the full path resolution for a Next.js project.
    /// Verifies that the stored file path matches what a browser would request.
    ///
    /// Next.js container layout:
    ///   /app/.next/static/chunks/main-abc123.js
    ///   /app/.next/static/css/layout-xyz789.css
    ///
    /// Browser requests:
    ///   /_next/static/chunks/main-abc123.js
    ///   /_next/static/css/layout-xyz789.css
    #[test]
    fn test_nextjs_fixture_path_resolution() {
        let temp_dir = std::env::temp_dir().join("temps-test-nextjs-fixture");
        let chunks_dir = temp_dir.join("chunks/1/1/100");
        let _ = std::fs::remove_dir_all(&temp_dir);

        // Simulate extracted files from .next/static (as extract_from_image would produce)
        let extracted_dir = temp_dir.join("extracted");
        std::fs::create_dir_all(extracted_dir.join("chunks")).unwrap();
        std::fs::create_dir_all(extracted_dir.join("css")).unwrap();
        std::fs::write(extracted_dir.join("chunks/main-abc123.js"), "js").unwrap();
        std::fs::write(extracted_dir.join("css/layout-xyz789.css"), "css").unwrap();
        std::fs::write(extracted_dir.join("chunks/main-abc123.js.map"), "map").unwrap();

        // search_path = ".next/static", rewrites = [(".next", "_next")]
        let search_path = ".next/static";
        let job = PersistStaticAssetsJob::new(
            "test".to_string(),
            100,
            1,
            1,
            "build".to_string(),
            vec![search_path.to_string()],
            vec![(".next".to_string(), "_next".to_string())],
            Arc::new(MockImageBuilder),
            chunks_dir.clone(),
        );

        // Simulate the path computation from execute()
        let assets = PersistStaticAssetsJob::find_static_assets(&extracted_dir);
        std::fs::create_dir_all(&chunks_dir).unwrap();

        for asset_path in &assets {
            let relative_path = asset_path.strip_prefix(&extracted_dir).unwrap();
            let container_relative = format!(
                "{}/{}",
                search_path.trim_start_matches('/'),
                relative_path.display()
            );
            let url_path = job.apply_rewrites(&container_relative);
            let target = chunks_dir.join(&url_path);
            std::fs::create_dir_all(target.parent().unwrap()).unwrap();
            std::fs::copy(asset_path, &target).unwrap();
        }

        // Verify: browser URL paths must match stored file paths
        // Browser requests /_next/static/chunks/main-abc123.js
        // Proxy strips leading / and looks for: chunks_dir/_next/static/chunks/main-abc123.js
        assert!(
            chunks_dir
                .join("_next/static/chunks/main-abc123.js")
                .exists(),
            "Next.js JS chunk must be accessible at _next/static/chunks/main-abc123.js"
        );
        assert!(
            chunks_dir
                .join("_next/static/css/layout-xyz789.css")
                .exists(),
            "Next.js CSS must be accessible at _next/static/css/layout-xyz789.css"
        );
        // .map files should NOT be persisted
        assert!(
            !chunks_dir
                .join("_next/static/chunks/main-abc123.js.map")
                .exists(),
            "Source maps should not be persisted"
        );

        let _ = std::fs::remove_dir_all(&temp_dir);
    }

    /// Simulate the full path resolution for a Vite project.
    /// Verifies that the stored file path matches what a browser would request.
    ///
    /// Vite container layout:
    ///   /app/dist/assets/index-abc123.js
    ///   /app/dist/assets/style-def456.css
    ///   /app/dist/index.html
    ///
    /// Browser requests:
    ///   /assets/index-abc123.js
    ///   /assets/style-def456.css
    #[test]
    fn test_vite_fixture_path_resolution() {
        let temp_dir = std::env::temp_dir().join("temps-test-vite-fixture");
        let chunks_dir = temp_dir.join("chunks/2/3/200");
        let _ = std::fs::remove_dir_all(&temp_dir);

        // Simulate extracted files from dist/assets
        let extracted_dir = temp_dir.join("extracted");
        std::fs::create_dir_all(&extracted_dir).unwrap();
        std::fs::write(extracted_dir.join("index-abc123.js"), "js").unwrap();
        std::fs::write(extracted_dir.join("style-def456.css"), "css").unwrap();

        // search_path = "dist/assets", rewrites = [("dist/", "")]
        let search_path = "dist/assets";
        let job = PersistStaticAssetsJob::new(
            "test".to_string(),
            200,
            1,
            1,
            "build".to_string(),
            vec![search_path.to_string()],
            vec![("dist/".to_string(), String::new())],
            Arc::new(MockImageBuilder),
            chunks_dir.clone(),
        );

        let assets = PersistStaticAssetsJob::find_static_assets(&extracted_dir);
        std::fs::create_dir_all(&chunks_dir).unwrap();

        for asset_path in &assets {
            let relative_path = asset_path.strip_prefix(&extracted_dir).unwrap();
            let container_relative = format!(
                "{}/{}",
                search_path.trim_start_matches('/'),
                relative_path.display()
            );
            let url_path = job.apply_rewrites(&container_relative);
            let target = chunks_dir.join(&url_path);
            std::fs::create_dir_all(target.parent().unwrap()).unwrap();
            std::fs::copy(asset_path, &target).unwrap();
        }

        // Browser requests /assets/index-abc123.js
        // Proxy strips leading / and looks for: chunks_dir/assets/index-abc123.js
        assert!(
            chunks_dir.join("assets/index-abc123.js").exists(),
            "Vite JS must be accessible at assets/index-abc123.js"
        );
        assert!(
            chunks_dir.join("assets/style-def456.css").exists(),
            "Vite CSS must be accessible at assets/style-def456.css"
        );

        let _ = std::fs::remove_dir_all(&temp_dir);
    }

    /// Test that multiple deployments coexist without conflicts.
    /// Each deployment gets its own subdirectory.
    #[test]
    fn test_multiple_deployments_coexist() {
        let temp_dir = std::env::temp_dir().join("temps-test-multi-deploy");
        let env_chunks = temp_dir.join("chunks/1/1");
        let _ = std::fs::remove_dir_all(&temp_dir);

        // Deploy v1: chunks/1/1/100/_next/static/chunks/main-aaa.js
        let v1_dir = env_chunks.join("100/_next/static/chunks");
        std::fs::create_dir_all(&v1_dir).unwrap();
        std::fs::write(v1_dir.join("main-aaa.js"), "v1").unwrap();

        // Deploy v2: chunks/1/1/101/_next/static/chunks/main-bbb.js
        let v2_dir = env_chunks.join("101/_next/static/chunks");
        std::fs::create_dir_all(&v2_dir).unwrap();
        std::fs::write(v2_dir.join("main-bbb.js"), "v2").unwrap();

        // Both deployments' chunks accessible
        assert!(env_chunks
            .join("100/_next/static/chunks/main-aaa.js")
            .exists());
        assert!(env_chunks
            .join("101/_next/static/chunks/main-bbb.js")
            .exists());

        // Cleanup of v1 doesn't affect v2
        std::fs::remove_dir_all(env_chunks.join("100")).unwrap();
        assert!(!env_chunks.join("100").exists());
        assert!(env_chunks
            .join("101/_next/static/chunks/main-bbb.js")
            .exists());

        let _ = std::fs::remove_dir_all(&temp_dir);
    }

    #[test]
    fn test_find_static_assets() {
        let temp_dir = std::env::temp_dir().join("temps-test-persist-assets");
        let _ = std::fs::remove_dir_all(&temp_dir);
        std::fs::create_dir_all(temp_dir.join("chunks")).unwrap();

        // Create test files
        std::fs::write(temp_dir.join("chunks/main.js"), "console.log()").unwrap();
        std::fs::write(temp_dir.join("chunks/style.css"), "body{}").unwrap();
        std::fs::write(temp_dir.join("chunks/main.js.map"), "{}").unwrap();
        std::fs::write(temp_dir.join("chunks/readme.md"), "# Hi").unwrap();

        let assets = PersistStaticAssetsJob::find_static_assets(&temp_dir);

        assert_eq!(assets.len(), 2); // .js and .css, NOT .map or .md
        assert!(assets.iter().any(|p| p.ends_with("main.js")));
        assert!(assets.iter().any(|p| p.ends_with("style.css")));

        let _ = std::fs::remove_dir_all(&temp_dir);
    }

    #[test]
    fn static_asset_walk_stops_at_shared_path_depth_limit() {
        let temp_dir = tempfile::TempDir::new().expect("temporary extraction root");
        let mut maximum_parent = temp_dir.path().to_path_buf();
        for index in 0..(MAX_STATIC_PATH_COMPONENTS - 1) {
            maximum_parent.push(format!("d{index}"));
        }
        std::fs::create_dir_all(&maximum_parent).expect("create maximum-depth directory");
        let accepted = maximum_parent.join("accepted.js");
        std::fs::write(&accepted, "accepted").expect("write maximum-depth asset");

        let too_deep_parent = maximum_parent.join("too-deep");
        std::fs::create_dir(&too_deep_parent).expect("create over-depth directory");
        let rejected = too_deep_parent.join("rejected.js");
        std::fs::write(&rejected, "rejected").expect("write over-depth asset");

        let assets = PersistStaticAssetsJob::find_static_assets(temp_dir.path());

        assert!(assets.contains(&accepted));
        assert!(!assets.contains(&rejected));
    }

    #[cfg(unix)]
    #[test]
    fn static_asset_walk_does_not_follow_directory_symlinks() {
        use std::os::unix::fs::symlink;

        let temp_dir = tempfile::TempDir::new().expect("temporary extraction root");
        let outside = tempfile::TempDir::new().expect("outside directory");
        std::fs::write(outside.path().join("private.js"), "private").expect("write outside asset");
        symlink(outside.path(), temp_dir.path().join("linked")).expect("create directory symlink");

        let assets = PersistStaticAssetsJob::find_static_assets(temp_dir.path());

        assert!(assets.is_empty());
    }

    struct CountingFileStore {
        put_blob_calls: AtomicUsize,
    }

    #[async_trait]
    impl FileStore for CountingFileStore {
        async fn put_blob(&self, _data: Bytes) -> Result<String, FileStoreError> {
            self.put_blob_calls.fetch_add(1, Ordering::Relaxed);
            Ok("a".repeat(64))
        }

        async fn get_blob(&self, _hash: &str) -> Result<Bytes, FileStoreError> {
            Err(FileStoreError::Backend("unused test operation".to_string()))
        }

        async fn open_blob(&self, _hash: &str) -> Result<OpenedBlob, FileStoreError> {
            Err(FileStoreError::Backend("unused test operation".to_string()))
        }

        async fn blob_exists(&self, _hash: &str) -> Result<bool, FileStoreError> {
            Err(FileStoreError::Backend("unused test operation".to_string()))
        }

        async fn delete_blob(&self, _hash: &str) -> Result<bool, FileStoreError> {
            Err(FileStoreError::Backend("unused test operation".to_string()))
        }

        async fn put(&self, _path: &str, _data: Bytes) -> Result<u64, FileStoreError> {
            Err(FileStoreError::Backend("unused test operation".to_string()))
        }

        async fn get(&self, _path: &str) -> Result<Bytes, FileStoreError> {
            Err(FileStoreError::Backend("unused test operation".to_string()))
        }

        async fn exists(&self, _path: &str) -> Result<bool, FileStoreError> {
            Err(FileStoreError::Backend("unused test operation".to_string()))
        }
    }

    #[tokio::test]
    async fn oversized_asset_is_rejected_before_put_blob() {
        let temporary = tempfile::tempdir().expect("temporary asset directory");
        let oversized = temporary.path().join("oversized.js");
        let file = std::fs::File::create(&oversized).expect("create sparse oversized asset");
        file.set_len(MAX_PUBLIC_STATIC_ASSET_BYTES + 1)
            .expect("size sparse oversized asset");
        drop(file);
        let store = CountingFileStore {
            put_blob_calls: AtomicUsize::new(0),
        };

        let result = PersistStaticAssetsJob::persist_asset_blob(Some(&store), &oversized)
            .await
            .expect("oversized assets are skipped without IO errors");

        assert!(result.is_none());
        assert_eq!(store.put_blob_calls.load(Ordering::Relaxed), 0);
    }

    // Minimal mock for tests that don't need real image extraction
    struct MockImageBuilder;

    #[async_trait]
    impl ImageBuilder for MockImageBuilder {
        async fn build_image(
            &self,
            _request: temps_deployer::BuildRequest,
        ) -> Result<temps_deployer::BuildResult, temps_deployer::BuilderError> {
            unimplemented!()
        }
        async fn import_image(
            &self,
            _image_path: PathBuf,
            _tag: &str,
        ) -> Result<String, temps_deployer::BuilderError> {
            unimplemented!()
        }
        async fn extract_from_image(
            &self,
            _image_name: &str,
            _source_path: &str,
            _destination_path: &Path,
        ) -> Result<(), temps_deployer::BuilderError> {
            unimplemented!()
        }
        async fn list_images(&self) -> Result<Vec<String>, temps_deployer::BuilderError> {
            unimplemented!()
        }
        async fn remove_image(
            &self,
            _image_name: &str,
        ) -> Result<(), temps_deployer::BuilderError> {
            unimplemented!()
        }
        async fn build_image_with_callback(
            &self,
            _request: temps_deployer::BuildRequestWithCallback,
        ) -> Result<temps_deployer::BuildResult, temps_deployer::BuilderError> {
            unimplemented!()
        }
        async fn inspect_image(
            &self,
            _image_name: &str,
        ) -> Result<temps_deployer::ImageInfo, temps_deployer::BuilderError> {
            Ok(temps_deployer::ImageInfo {
                id: "sha256:mock".to_string(),
                architecture: "amd64".to_string(),
                os: "linux".to_string(),
                platform: "linux/amd64".to_string(),
                size_bytes: 0,
                tags: vec![],
                created: None,
                working_dir: Some("/app".to_string()),
            })
        }
        async fn save_image(
            &self,
            _image_name: &str,
            _output_path: &Path,
        ) -> Result<(), temps_deployer::BuilderError> {
            Ok(())
        }
        fn get_native_platform(&self) -> String {
            "linux/amd64".to_string()
        }
    }
}
