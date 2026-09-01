// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Capture Source Maps Job
//!
//! Extracts source map files (.map) from the built Docker image and uploads them
//! to the source map storage for error symbolication. This runs as a post-deployment
//! job so it does not block the deployment pipeline.

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use std::path::Path;
use std::sync::Arc;
use temps_core::{JobResult, WorkflowContext, WorkflowError, WorkflowTask};
use temps_deployer::ImageBuilder;
use temps_error_tracking::services::{SourceMapService, MAX_SOURCE_MAP_BYTES};
use temps_logs::{LogLevel, LogService};
use thiserror::Error;
use tokio::io::AsyncReadExt;
use tracing::{debug, info, warn};

use crate::jobs::ImageOutput;

/// Output from CaptureSourceMapsJob
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CaptureSourceMapsOutput {
    pub source_maps_captured: u32,
    pub total_size_bytes: u64,
    pub release: String,
}

#[derive(Debug, Error)]
enum SourceMapFileReadError {
    #[error("Failed to {operation} source map '{path}': {source}")]
    Io {
        path: String,
        operation: &'static str,
        #[source]
        source: std::io::Error,
    },
    #[error("Source map '{path}' is {size_bytes} bytes, exceeding the {limit_bytes} byte limit")]
    TooLarge {
        path: String,
        size_bytes: u64,
        limit_bytes: usize,
    },
}

async fn read_source_map_bounded(path: &Path) -> Result<Vec<u8>, SourceMapFileReadError> {
    let path_display = path.display().to_string();
    let file = tokio::fs::File::open(path)
        .await
        .map_err(|source| SourceMapFileReadError::Io {
            path: path_display.clone(),
            operation: "open",
            source,
        })?;
    let metadata = file
        .metadata()
        .await
        .map_err(|source| SourceMapFileReadError::Io {
            path: path_display.clone(),
            operation: "inspect",
            source,
        })?;
    if metadata.len() > MAX_SOURCE_MAP_BYTES as u64 {
        return Err(SourceMapFileReadError::TooLarge {
            path: path_display,
            size_bytes: metadata.len(),
            limit_bytes: MAX_SOURCE_MAP_BYTES,
        });
    }

    let mut data = Vec::with_capacity(metadata.len() as usize);
    let mut bounded_file = file.take(MAX_SOURCE_MAP_BYTES as u64 + 1);
    bounded_file
        .read_to_end(&mut data)
        .await
        .map_err(|source| SourceMapFileReadError::Io {
            path: path.display().to_string(),
            operation: "read",
            source,
        })?;
    if data.len() > MAX_SOURCE_MAP_BYTES {
        return Err(SourceMapFileReadError::TooLarge {
            path: path.display().to_string(),
            size_bytes: data.len() as u64,
            limit_bytes: MAX_SOURCE_MAP_BYTES,
        });
    }
    Ok(data)
}

/// Job that extracts source maps from the built image and stores them
/// for stack trace symbolication.
pub struct CaptureSourceMapsJob {
    job_id: String,
    deployment_id: i32,
    project_id: i32,
    release: String,
    build_job_id: String,
    /// Paths inside the container to search for source maps
    search_paths: Vec<String>,
    /// Path rewrite rules applied to stored file paths.
    /// Each tuple is (from, to) — e.g., (".next", "_next") rewrites
    /// container paths like `/.next/static/...` to `/_next/static/...`
    /// so they match what browsers report in stack traces.
    path_rewrites: Vec<(String, String)>,
    image_builder: Arc<dyn ImageBuilder>,
    source_map_service: Arc<SourceMapService>,
    log_id: Option<String>,
    log_service: Option<Arc<LogService>>,
}

impl std::fmt::Debug for CaptureSourceMapsJob {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CaptureSourceMapsJob")
            .field("job_id", &self.job_id)
            .field("deployment_id", &self.deployment_id)
            .field("project_id", &self.project_id)
            .field("release", &self.release)
            .field("build_job_id", &self.build_job_id)
            .field("search_paths", &self.search_paths)
            .field("path_rewrites", &self.path_rewrites)
            .finish()
    }
}

impl CaptureSourceMapsJob {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        job_id: String,
        deployment_id: i32,
        project_id: i32,
        release: String,
        build_job_id: String,
        search_paths: Vec<String>,
        path_rewrites: Vec<(String, String)>,
        image_builder: Arc<dyn ImageBuilder>,
        source_map_service: Arc<SourceMapService>,
    ) -> Self {
        Self {
            job_id,
            deployment_id,
            project_id,
            release,
            build_job_id,
            search_paths,
            path_rewrites,
            image_builder,
            source_map_service,
            log_id: None,
            log_service: None,
        }
    }

    pub fn with_log_id(mut self, log_id: String) -> Self {
        self.log_id = Some(log_id);
        self
    }

    pub fn with_log_service(mut self, log_service: Arc<LogService>) -> Self {
        self.log_service = Some(log_service);
        self
    }

    /// Write log message to job-specific log file
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

    /// Recursively find all .map files in a directory
    async fn find_source_maps(&self, dir: &Path) -> Vec<std::path::PathBuf> {
        let mut maps = Vec::new();
        self.walk_dir_for_maps(dir, &mut maps).await;
        maps
    }

    fn walk_dir_for_maps_sync(dir: &Path, maps: &mut Vec<std::path::PathBuf>) {
        let entries = match std::fs::read_dir(dir) {
            Ok(e) => e,
            Err(_) => return,
        };

        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                Self::walk_dir_for_maps_sync(&path, maps);
            } else if let Some(ext) = path.extension() {
                if ext == "map" {
                    maps.push(path);
                }
            }
        }
    }

    async fn walk_dir_for_maps(&self, dir: &Path, maps: &mut Vec<std::path::PathBuf>) {
        Self::walk_dir_for_maps_sync(dir, maps);
    }
}

#[async_trait]
impl WorkflowTask for CaptureSourceMapsJob {
    fn job_id(&self) -> &str {
        &self.job_id
    }

    fn name(&self) -> &str {
        "Capture Source Maps"
    }

    fn description(&self) -> &str {
        "Extract source maps from build output for error symbolication"
    }

    async fn execute(&self, context: WorkflowContext) -> Result<JobResult, WorkflowError> {
        info!(
            "Starting source map capture for deployment {} (project: {}, release: {})",
            self.deployment_id, self.project_id, self.release
        );

        self.log("🗺️ Starting source map capture...".to_string())
            .await?;

        // Get image tag from build job output
        let image_output = ImageOutput::from_context(&context, &self.build_job_id)?;
        let image_tag = &image_output.image_tag;

        self.log(format!("Extracting source maps from image: {}", image_tag))
            .await?;

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

        let mut total_maps_captured: u32 = 0;
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
            // Acquire first so reverse drop order removes the private
            // directory before releasing capacity to another extraction.
            let _extraction_permit = super::acquire_archive_extraction_permit().await?;

            // Source maps are private artifacts. Keep their extraction in an
            // owner-only RAII directory that is removed on every exit path.
            let temp_dir = match tempfile::Builder::new()
                .prefix("temps-sourcemaps-")
                .tempdir()
            {
                Ok(temp_dir) => temp_dir,
                Err(error) => {
                    warn!(
                        "Failed to create secure temp directory for source map extraction: {}",
                        error
                    );
                    self.log(format!(
                        "⚠️ Failed to create secure temp directory: {error}"
                    ))
                    .await?;
                    continue;
                }
            };

            // Extract files from the container image
            self.log(format!(
                "📂 Extracting from container path: {}",
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
                    // Non-fatal: the path might not exist in the image
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

            // Find all .map files
            let map_files = self.find_source_maps(temp_dir.path()).await;

            if map_files.is_empty() {
                self.log(format!(
                    "No source map files found in {}",
                    absolute_search_path
                ))
                .await?;
                continue;
            }

            self.log(format!(
                "Found {} source map file(s) in {}",
                map_files.len(),
                absolute_search_path
            ))
            .await?;

            // Upload each source map
            for map_path in &map_files {
                // Compute the file path relative to the search path
                let relative_path = map_path.strip_prefix(temp_dir.path()).unwrap_or(map_path);

                // Build the ~ prefixed path for storage
                // search_path is relative (e.g., ".next/static") and gets path rewrites applied
                // e.g., search_path=".next/static" + relative="chunks/main-abc123.js.map"
                // after rewrite (.next -> _next): "~/_next/static/chunks/main-abc123.js.map"
                let mut container_relative = format!(
                    "{}/{}",
                    search_path.trim_start_matches('/'),
                    relative_path.display()
                );

                // Apply path rewrites so stored paths match what browsers report.
                // For Next.js: ".next" -> "_next" because Next.js serves .next/static
                // as /_next/static in the browser.
                for (from, to) in &self.path_rewrites {
                    container_relative = container_relative.replace(from.as_str(), to.as_str());
                }

                let file_path = format!("~/{}", container_relative.trim_start_matches('/'));

                // The stored file_path should point to the JS file, not the .map file
                // e.g., "~/static/chunks/main-abc123.js" (strip .map suffix)
                let js_file_path = file_path
                    .strip_suffix(".map")
                    .unwrap_or(&file_path)
                    .to_string();

                let source_map_data = match read_source_map_bounded(map_path).await {
                    Ok(data) => data,
                    Err(e) => {
                        warn!(
                            "Failed to read source map file {}: {}",
                            map_path.display(),
                            e
                        );
                        continue;
                    }
                };

                let file_size = source_map_data.len() as u64;

                match self
                    .source_map_service
                    .upload(
                        self.project_id,
                        &self.release,
                        &js_file_path,
                        source_map_data,
                        None,
                    )
                    .await
                {
                    Ok(info) => {
                        debug!(
                            "Uploaded source map: {} ({} bytes)",
                            info.file_path, info.size_bytes
                        );
                        total_maps_captured += 1;
                        total_size += file_size;
                    }
                    Err(e) => {
                        // Non-fatal: log warning and continue with other files
                        warn!("Failed to upload source map '{}': {}", js_file_path, e);
                        self.log(format!("⚠️ Failed to upload {}: {}", js_file_path, e))
                            .await?;
                    }
                }
            }
        }

        if total_maps_captured > 0 {
            self.log(format!(
                "✅ Captured {} source map(s) for release '{}' ({} bytes total)",
                total_maps_captured, self.release, total_size
            ))
            .await?;
        } else {
            self.log("No source maps found in the build output".to_string())
                .await?;
        }

        info!(
            "Source map capture completed for deployment {}: {} maps captured ({} bytes)",
            self.deployment_id, total_maps_captured, total_size
        );

        // Clean up source maps from old releases no longer tied to active deployments
        match self
            .source_map_service
            .delete_stale_source_maps(self.project_id)
            .await
        {
            Ok(deleted) if deleted > 0 => {
                self.log(format!(
                    "Cleaned up {} stale source map(s) from previous deployments",
                    deleted
                ))
                .await?;
                info!(
                    "Cleaned up {} stale source map(s) for project {}",
                    deleted, self.project_id
                );
            }
            Ok(_) => {}
            Err(e) => {
                // Non-fatal: log warning and continue
                warn!(
                    "Failed to clean up stale source maps for project {}: {}",
                    self.project_id, e
                );
            }
        }

        // Set output in context
        let mut updated_context = context.clone();
        updated_context.set_output(&self.job_id, "source_maps_captured", total_maps_captured)?;
        updated_context.set_output(&self.job_id, "total_size_bytes", total_size)?;
        updated_context.set_output(&self.job_id, "release", &self.release)?;

        Ok(JobResult::success(updated_context))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn oversized_sparse_source_map_is_rejected_before_reading() {
        let temp_dir = tempfile::TempDir::new().expect("create temp dir");
        let map_path = temp_dir.path().join("private.js.map");
        let file = tokio::fs::File::create(&map_path)
            .await
            .expect("create sparse source map");
        file.set_len(MAX_SOURCE_MAP_BYTES as u64 + 1)
            .await
            .expect("set sparse source map length");

        let error = read_source_map_bounded(&map_path)
            .await
            .expect_err("oversized source map must be rejected from metadata");
        assert!(matches!(
            error,
            SourceMapFileReadError::TooLarge {
                size_bytes,
                limit_bytes: MAX_SOURCE_MAP_BYTES,
                ..
            } if size_bytes == MAX_SOURCE_MAP_BYTES as u64 + 1
        ));
    }

    #[test]
    fn private_source_map_temp_directory_is_removed_on_scope_exit() {
        let extraction_path;
        {
            let temp_dir = tempfile::Builder::new()
                .prefix("temps-sourcemaps-test-")
                .tempdir()
                .expect("create private extraction directory");
            extraction_path = temp_dir.path().to_path_buf();
            std::fs::write(
                temp_dir.path().join("private.js.map"),
                b"private source map",
            )
            .expect("write private test artifact");
        }
        assert!(!extraction_path.exists());
    }

    #[test]
    fn test_detect_log_level() {
        assert!(matches!(
            CaptureSourceMapsJob::detect_log_level("✅ Done"),
            LogLevel::Success
        ));
        assert!(matches!(
            CaptureSourceMapsJob::detect_log_level("❌ Failed to upload"),
            LogLevel::Error
        ));
        assert!(matches!(
            CaptureSourceMapsJob::detect_log_level("⚠️ Warning: file skipped"),
            LogLevel::Warning
        ));
        assert!(matches!(
            CaptureSourceMapsJob::detect_log_level("Processing files..."),
            LogLevel::Info
        ));
    }

    /// Helper to simulate the path computation logic used in execute().
    /// search_path is now relative (e.g., ".next/static"), matching what
    /// workflow_planner.rs provides. The job prepends the WORKDIR at runtime.
    fn compute_file_path(
        search_path: &str,
        relative: &str,
        path_rewrites: &[(String, String)],
    ) -> String {
        let mut container_relative =
            format!("{}/{}", search_path.trim_start_matches('/'), relative);
        for (from, to) in path_rewrites {
            container_relative = container_relative.replace(from.as_str(), to.as_str());
        }
        let file_path = format!("~/{}", container_relative.trim_start_matches('/'));
        file_path
            .strip_suffix(".map")
            .unwrap_or(&file_path)
            .to_string()
    }

    /// Helper to simulate the absolute path resolution used in execute()
    fn resolve_absolute_path(workdir: &str, search_path: &str) -> String {
        if search_path.starts_with('/') {
            search_path.to_string()
        } else {
            format!(
                "{}/{}",
                workdir.trim_end_matches('/'),
                search_path.trim_start_matches('/')
            )
        }
    }

    #[test]
    fn test_nextjs_file_path_with_rewrite() {
        // Next.js: .next -> _next so paths match browser stack traces
        // search_path is now relative (no /app prefix)
        let rewrites = vec![(".next".to_string(), "_next".to_string())];

        assert_eq!(
            compute_file_path(".next/static", "chunks/main-abc123.js.map", &rewrites),
            "~/_next/static/chunks/main-abc123.js"
        );
    }

    #[test]
    fn test_nextjs_server_path_with_rewrite() {
        let rewrites = vec![(".next".to_string(), "_next".to_string())];

        assert_eq!(
            compute_file_path(".next/server", "app/page.js.map", &rewrites),
            "~/_next/server/app/page.js"
        );
    }

    #[test]
    fn test_vite_file_path_no_rewrite() {
        let rewrites: Vec<(String, String)> = vec![];

        assert_eq!(
            compute_file_path("dist/assets", "index-abc123.js.map", &rewrites),
            "~/dist/assets/index-abc123.js"
        );
    }

    #[test]
    fn test_generic_file_path_no_rewrite() {
        let rewrites: Vec<(String, String)> = vec![];

        assert_eq!(
            compute_file_path("dist", "bundle.js.map", &rewrites),
            "~/dist/bundle.js"
        );
    }

    #[test]
    fn test_no_map_extension() {
        let rewrites: Vec<(String, String)> = vec![];

        assert_eq!(
            compute_file_path("dist", "bundle.js", &rewrites),
            "~/dist/bundle.js"
        );
    }

    #[test]
    fn test_resolve_absolute_path_with_standard_workdir() {
        assert_eq!(
            resolve_absolute_path("/app", ".next/static"),
            "/app/.next/static"
        );
    }

    #[test]
    fn test_resolve_absolute_path_with_custom_workdir() {
        assert_eq!(
            resolve_absolute_path("/task_abc123_my_project", ".next/static"),
            "/task_abc123_my_project/.next/static"
        );
    }

    #[test]
    fn test_resolve_absolute_path_already_absolute() {
        // If search_path is already absolute, use it as-is
        assert_eq!(
            resolve_absolute_path("/app", "/custom/path/.next/static"),
            "/custom/path/.next/static"
        );
    }

    #[test]
    fn test_resolve_absolute_path_trailing_slash() {
        assert_eq!(
            resolve_absolute_path("/app/", ".next/server"),
            "/app/.next/server"
        );
    }
}
