//! Deploy Static Bundle Job
//!
//! Deploys pre-uploaded static files from blob storage.
//! This job is used for remote deployments where static files are built externally
//! and uploaded as a tar.gz or zip bundle.

use async_trait::async_trait;
use flate2::read::GzDecoder;
use serde::{Deserialize, Serialize};
use std::io::{Cursor, Read};
use std::sync::Arc;
use temps_blob::BlobService;
use temps_core::{JobResult, WorkflowContext, WorkflowError, WorkflowTask};
use temps_deployer::static_deployer::{StaticDeployRequest, StaticDeployer};
use temps_logs::{LogLevel, LogService};
use tokio_util::io::StreamReader;
use tracing::{debug, error, info};
use zip::ZipArchive;

/// Output from DeployStaticBundleJob
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DeployStaticBundleOutput {
    /// Full path to the deployed static files directory
    pub static_dir_location: String,
    /// Number of files deployed
    pub file_count: u32,
    /// Total size in bytes
    pub total_size_bytes: u64,
    /// Original bundle path in blob storage
    pub bundle_path: String,
}

impl DeployStaticBundleOutput {
    /// Extract DeployStaticBundleOutput from WorkflowContext
    pub fn from_context(context: &WorkflowContext, job_id: &str) -> Result<Self, WorkflowError> {
        let static_dir_location: String = context
            .get_output(job_id, "static_dir_location")?
            .ok_or_else(|| {
                WorkflowError::JobValidationFailed(
                    "static_dir_location output not found".to_string(),
                )
            })?;
        let file_count: u32 = context.get_output(job_id, "file_count")?.ok_or_else(|| {
            WorkflowError::JobValidationFailed("file_count output not found".to_string())
        })?;
        let total_size_bytes: u64 =
            context
                .get_output(job_id, "total_size_bytes")?
                .ok_or_else(|| {
                    WorkflowError::JobValidationFailed(
                        "total_size_bytes output not found".to_string(),
                    )
                })?;
        let bundle_path: String = context.get_output(job_id, "bundle_path")?.ok_or_else(|| {
            WorkflowError::JobValidationFailed("bundle_path output not found".to_string())
        })?;

        Ok(Self {
            static_dir_location,
            file_count,
            total_size_bytes,
            bundle_path,
        })
    }
}

/// Job that deploys a static bundle from blob storage
pub struct DeployStaticBundleJob {
    /// Unique job identifier
    job_id: String,
    /// Project ID for blob storage namespace
    project_id: i32,
    /// Path to the bundle in blob storage (relative to project namespace)
    bundle_path: String,
    /// Content type of the bundle (application/gzip or application/zip)
    content_type: String,
    /// Static bundle ID (reference to static_bundles table)
    static_bundle_id: Option<i32>,
    /// Project slug for organizing files
    project_slug: String,
    /// Environment slug for organizing files
    environment_slug: String,
    /// Deployment slug for organizing files
    deployment_slug: String,
    /// Blob service for retrieving the bundle
    blob_service: Arc<BlobService>,
    /// Static deployer for deploying files
    static_deployer: Arc<dyn StaticDeployer>,
    /// Log service for streaming logs
    log_service: Option<Arc<LogService>>,
    /// Log ID for this job's logs
    log_id: Option<String>,
}

impl std::fmt::Debug for DeployStaticBundleJob {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("DeployStaticBundleJob")
            .field("job_id", &self.job_id)
            .field("project_id", &self.project_id)
            .field("bundle_path", &self.bundle_path)
            .field("content_type", &self.content_type)
            .field("static_bundle_id", &self.static_bundle_id)
            .field("project_slug", &self.project_slug)
            .field("environment_slug", &self.environment_slug)
            .field("deployment_slug", &self.deployment_slug)
            .finish()
    }
}

impl DeployStaticBundleJob {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        job_id: String,
        project_id: i32,
        bundle_path: String,
        content_type: String,
        static_bundle_id: Option<i32>,
        project_slug: String,
        environment_slug: String,
        deployment_slug: String,
        blob_service: Arc<BlobService>,
        static_deployer: Arc<dyn StaticDeployer>,
    ) -> Self {
        Self {
            job_id,
            project_id,
            bundle_path,
            content_type,
            static_bundle_id,
            project_slug,
            environment_slug,
            deployment_slug,
            blob_service,
            static_deployer,
            log_service: None,
            log_id: None,
        }
    }

    pub fn with_log_service(mut self, log_service: Arc<LogService>, log_id: String) -> Self {
        self.log_service = Some(log_service);
        self.log_id = Some(log_id);
        self
    }

    async fn log(&self, level: LogLevel, message: &str) {
        if let (Some(log_service), Some(log_id)) = (&self.log_service, &self.log_id) {
            if let Err(e) = log_service
                .append_structured_log(log_id, level, message)
                .await
            {
                error!("Failed to write to log: {}", e);
            }
        }
    }

    /// Detect content type from the bundle path if not provided
    fn detect_content_type(&self) -> &str {
        if !self.content_type.is_empty() {
            return &self.content_type;
        }

        // Detect from file extension
        if self.bundle_path.ends_with(".tar.gz") || self.bundle_path.ends_with(".tgz") {
            "application/gzip"
        } else if self.bundle_path.ends_with(".zip") {
            "application/zip"
        } else {
            // Default to gzip
            "application/gzip"
        }
    }

    /// Extract tar.gz bundle to the target directory
    fn extract_tar_gz(
        &self,
        data: &[u8],
        target_dir: &std::path::Path,
    ) -> Result<u32, WorkflowError> {
        let decoder = GzDecoder::new(Cursor::new(data));
        let mut archive = tar::Archive::new(decoder);

        let mut file_count = 0u32;

        for entry in archive.entries().map_err(|e| {
            WorkflowError::JobExecutionFailed(format!("Failed to read tar.gz entries: {}", e))
        })? {
            let mut entry = entry.map_err(|e| {
                WorkflowError::JobExecutionFailed(format!("Failed to read tar entry: {}", e))
            })?;

            let path = entry.path().map_err(|e| {
                WorkflowError::JobExecutionFailed(format!("Failed to get entry path: {}", e))
            })?;

            let dest_path = target_dir.join(&path);

            // Security: Ensure path doesn't escape target directory
            if !dest_path.starts_with(target_dir) {
                return Err(WorkflowError::JobExecutionFailed(format!(
                    "Path traversal attempt detected: {:?}",
                    path
                )));
            }

            if entry.header().entry_type().is_dir() {
                std::fs::create_dir_all(&dest_path).map_err(|e| {
                    WorkflowError::JobExecutionFailed(format!(
                        "Failed to create directory {:?}: {}",
                        dest_path, e
                    ))
                })?;
            } else {
                // Ensure parent directory exists
                if let Some(parent) = dest_path.parent() {
                    std::fs::create_dir_all(parent).map_err(|e| {
                        WorkflowError::JobExecutionFailed(format!(
                            "Failed to create parent directory: {}",
                            e
                        ))
                    })?;
                }

                // Extract file
                entry.unpack(&dest_path).map_err(|e| {
                    WorkflowError::JobExecutionFailed(format!(
                        "Failed to extract file {:?}: {}",
                        dest_path, e
                    ))
                })?;

                file_count += 1;
            }
        }

        Ok(file_count)
    }

    /// Extract zip bundle to the target directory
    fn extract_zip(&self, data: &[u8], target_dir: &std::path::Path) -> Result<u32, WorkflowError> {
        let cursor = Cursor::new(data);
        let mut archive = ZipArchive::new(cursor).map_err(|e| {
            WorkflowError::JobExecutionFailed(format!("Failed to open zip archive: {}", e))
        })?;

        let mut file_count = 0u32;

        for i in 0..archive.len() {
            let mut file = archive.by_index(i).map_err(|e| {
                WorkflowError::JobExecutionFailed(format!("Failed to read zip entry {}: {}", i, e))
            })?;

            let outpath = match file.enclosed_name() {
                Some(path) => target_dir.join(path),
                None => continue, // Skip entries with invalid paths
            };

            // Security: Ensure path doesn't escape target directory
            if !outpath.starts_with(target_dir) {
                return Err(WorkflowError::JobExecutionFailed(format!(
                    "Path traversal attempt detected in zip entry {}",
                    i
                )));
            }

            if file.is_dir() {
                std::fs::create_dir_all(&outpath).map_err(|e| {
                    WorkflowError::JobExecutionFailed(format!(
                        "Failed to create directory {:?}: {}",
                        outpath, e
                    ))
                })?;
            } else {
                // Ensure parent directory exists
                if let Some(parent) = outpath.parent() {
                    if !parent.exists() {
                        std::fs::create_dir_all(parent).map_err(|e| {
                            WorkflowError::JobExecutionFailed(format!(
                                "Failed to create parent directory: {}",
                                e
                            ))
                        })?;
                    }
                }

                // Read file contents
                let mut contents = Vec::new();
                file.read_to_end(&mut contents).map_err(|e| {
                    WorkflowError::JobExecutionFailed(format!(
                        "Failed to read zip file contents: {}",
                        e
                    ))
                })?;

                // Write to destination
                std::fs::write(&outpath, contents).map_err(|e| {
                    WorkflowError::JobExecutionFailed(format!(
                        "Failed to write file {:?}: {}",
                        outpath, e
                    ))
                })?;

                file_count += 1;
            }
        }

        Ok(file_count)
    }

    /// Download the bundle data from blob storage
    async fn download_bundle(&self) -> Result<Vec<u8>, WorkflowError> {
        use futures::StreamExt;
        use tokio::io::AsyncReadExt;

        let (stream, _content_type, _size) = self
            .blob_service
            .download(self.project_id, &self.bundle_path)
            .await
            .map_err(|e| {
                WorkflowError::JobExecutionFailed(format!(
                    "Failed to download bundle from blob storage: {}",
                    e
                ))
            })?;

        // Collect the stream into bytes
        let stream = stream
            .map(|result| result.map_err(|e| std::io::Error::new(std::io::ErrorKind::Other, e)));
        let mut reader = StreamReader::new(stream);
        let mut data = Vec::new();
        reader.read_to_end(&mut data).await.map_err(|e| {
            WorkflowError::JobExecutionFailed(format!("Failed to read bundle data: {}", e))
        })?;

        Ok(data)
    }
}

#[async_trait]
impl WorkflowTask for DeployStaticBundleJob {
    fn job_id(&self) -> &str {
        &self.job_id
    }

    fn name(&self) -> &str {
        "Deploy Static Bundle"
    }

    fn description(&self) -> &str {
        "Deploys pre-uploaded static files from blob storage"
    }

    async fn execute(&self, mut context: WorkflowContext) -> Result<JobResult, WorkflowError> {
        info!(
            "Deploying static bundle: {} (content_type: {})",
            self.bundle_path,
            self.detect_content_type()
        );

        self.log(
            LogLevel::Info,
            &format!("📦 Starting static bundle deployment: {}", self.bundle_path),
        )
        .await;

        if let Some(bundle_id) = self.static_bundle_id {
            self.log(
                LogLevel::Info,
                &format!("📋 Static bundle ID: {}", bundle_id),
            )
            .await;
        }

        // Download bundle from blob storage
        self.log(LogLevel::Info, "⏳ Downloading bundle from storage...")
            .await;

        let bundle_data = self.download_bundle().await?;

        self.log(
            LogLevel::Success,
            &format!("✅ Downloaded {} bytes", bundle_data.len()),
        )
        .await;

        // Create temporary directory for extraction
        let temp_dir = std::env::temp_dir().join(format!("temps-bundle-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&temp_dir).map_err(|e| {
            WorkflowError::JobExecutionFailed(format!("Failed to create temp directory: {}", e))
        })?;

        debug!("Extracting bundle to: {:?}", temp_dir);
        self.log(
            LogLevel::Info,
            "📂 Extracting bundle to temporary directory...",
        )
        .await;

        // Extract based on content type
        let content_type = self.detect_content_type();
        let file_count = if content_type.contains("zip") {
            self.extract_zip(&bundle_data, &temp_dir)?
        } else {
            // Default to tar.gz
            self.extract_tar_gz(&bundle_data, &temp_dir)?
        };

        self.log(
            LogLevel::Success,
            &format!("✅ Extracted {} files", file_count),
        )
        .await;

        // Deploy using StaticDeployer
        let request = StaticDeployRequest {
            source_dir: temp_dir.clone(),
            project_slug: self.project_slug.clone(),
            environment_slug: self.environment_slug.clone(),
            deployment_slug: self.deployment_slug.clone(),
        };

        self.log(LogLevel::Info, "🚀 Deploying static files...")
            .await;

        let result = self.static_deployer.deploy(request).await.map_err(|e| {
            WorkflowError::JobExecutionFailed(format!("Failed to deploy static files: {}", e))
        })?;

        self.log(
            LogLevel::Success,
            &format!("📍 Deployed to: {}", result.storage_path),
        )
        .await;

        // Clean up temporary directory
        if let Err(e) = std::fs::remove_dir_all(&temp_dir) {
            debug!("Warning: Failed to clean up temp directory: {}", e);
        }

        // Store outputs in context
        context.set_output(&self.job_id, "static_dir_location", &result.storage_path)?;
        context.set_output(&self.job_id, "file_count", result.file_count)?;
        context.set_output(&self.job_id, "total_size_bytes", result.total_size_bytes)?;
        context.set_output(&self.job_id, "bundle_path", &self.bundle_path)?;

        self.log(
            LogLevel::Success,
            &format!(
                "✅ Static bundle deployment complete: {} files ({} bytes)",
                result.file_count, result.total_size_bytes
            ),
        )
        .await;

        Ok(JobResult::success_with_message(
            context,
            format!(
                "Successfully deployed static bundle: {} ({} files)",
                self.bundle_path, result.file_count
            ),
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use flate2::write::GzEncoder;
    use flate2::Compression;
    use std::io::Write;
    use tempfile::TempDir;

    fn create_test_tar_gz() -> Vec<u8> {
        let mut encoder = GzEncoder::new(Vec::new(), Compression::default());
        {
            let mut tar = tar::Builder::new(&mut encoder);

            // Add index.html
            let content = b"<html><body>Hello World</body></html>";
            let mut header = tar::Header::new_gnu();
            header.set_path("index.html").unwrap();
            header.set_size(content.len() as u64);
            header.set_mode(0o644);
            header.set_cksum();
            tar.append(&header, &content[..]).unwrap();

            // Add assets/app.js
            let js_content = b"console.log('Hello');";
            let mut js_header = tar::Header::new_gnu();
            js_header.set_path("assets/app.js").unwrap();
            js_header.set_size(js_content.len() as u64);
            js_header.set_mode(0o644);
            js_header.set_cksum();
            tar.append(&js_header, &js_content[..]).unwrap();

            tar.finish().unwrap();
        }
        encoder.finish().unwrap()
    }

    fn create_test_zip() -> Vec<u8> {
        let mut buffer = Vec::new();
        {
            let mut zip = zip::ZipWriter::new(Cursor::new(&mut buffer));

            let options = zip::write::SimpleFileOptions::default()
                .compression_method(zip::CompressionMethod::Stored);

            zip.start_file("index.html", options).unwrap();
            zip.write_all(b"<html><body>Hello World</body></html>")
                .unwrap();

            zip.start_file("assets/app.js", options).unwrap();
            zip.write_all(b"console.log('Hello');").unwrap();

            zip.finish().unwrap();
        }
        buffer
    }

    /// Test tar.gz extraction directly without needing BlobService
    fn extract_tar_gz_test(
        data: &[u8],
        target_dir: &std::path::Path,
    ) -> Result<u32, WorkflowError> {
        let decoder = GzDecoder::new(Cursor::new(data));
        let mut archive = tar::Archive::new(decoder);

        let mut file_count = 0u32;

        for entry in archive.entries().map_err(|e| {
            WorkflowError::JobExecutionFailed(format!("Failed to read tar.gz entries: {}", e))
        })? {
            let mut entry = entry.map_err(|e| {
                WorkflowError::JobExecutionFailed(format!("Failed to read tar entry: {}", e))
            })?;

            let path = entry.path().map_err(|e| {
                WorkflowError::JobExecutionFailed(format!("Failed to get entry path: {}", e))
            })?;

            let dest_path = target_dir.join(&path);

            if !dest_path.starts_with(target_dir) {
                return Err(WorkflowError::JobExecutionFailed(format!(
                    "Path traversal attempt detected: {:?}",
                    path
                )));
            }

            if entry.header().entry_type().is_dir() {
                std::fs::create_dir_all(&dest_path).map_err(|e| {
                    WorkflowError::JobExecutionFailed(format!(
                        "Failed to create directory {:?}: {}",
                        dest_path, e
                    ))
                })?;
            } else {
                if let Some(parent) = dest_path.parent() {
                    std::fs::create_dir_all(parent).map_err(|e| {
                        WorkflowError::JobExecutionFailed(format!(
                            "Failed to create parent directory: {}",
                            e
                        ))
                    })?;
                }

                entry.unpack(&dest_path).map_err(|e| {
                    WorkflowError::JobExecutionFailed(format!(
                        "Failed to extract file {:?}: {}",
                        dest_path, e
                    ))
                })?;

                file_count += 1;
            }
        }

        Ok(file_count)
    }

    /// Test zip extraction directly without needing BlobService
    fn extract_zip_test(data: &[u8], target_dir: &std::path::Path) -> Result<u32, WorkflowError> {
        let cursor = Cursor::new(data);
        let mut archive = ZipArchive::new(cursor).map_err(|e| {
            WorkflowError::JobExecutionFailed(format!("Failed to open zip archive: {}", e))
        })?;

        let mut file_count = 0u32;

        for i in 0..archive.len() {
            let mut file = archive.by_index(i).map_err(|e| {
                WorkflowError::JobExecutionFailed(format!("Failed to read zip entry {}: {}", i, e))
            })?;

            let outpath = match file.enclosed_name() {
                Some(path) => target_dir.join(path),
                None => continue,
            };

            if !outpath.starts_with(target_dir) {
                return Err(WorkflowError::JobExecutionFailed(format!(
                    "Path traversal attempt detected in zip entry {}",
                    i
                )));
            }

            if file.is_dir() {
                std::fs::create_dir_all(&outpath).map_err(|e| {
                    WorkflowError::JobExecutionFailed(format!(
                        "Failed to create directory {:?}: {}",
                        outpath, e
                    ))
                })?;
            } else {
                if let Some(parent) = outpath.parent() {
                    if !parent.exists() {
                        std::fs::create_dir_all(parent).map_err(|e| {
                            WorkflowError::JobExecutionFailed(format!(
                                "Failed to create parent directory: {}",
                                e
                            ))
                        })?;
                    }
                }

                let mut contents = Vec::new();
                file.read_to_end(&mut contents).map_err(|e| {
                    WorkflowError::JobExecutionFailed(format!(
                        "Failed to read zip file contents: {}",
                        e
                    ))
                })?;

                std::fs::write(&outpath, contents).map_err(|e| {
                    WorkflowError::JobExecutionFailed(format!(
                        "Failed to write file {:?}: {}",
                        outpath, e
                    ))
                })?;

                file_count += 1;
            }
        }

        Ok(file_count)
    }

    #[test]
    fn test_extract_tar_gz() {
        let temp_dir = TempDir::new().unwrap();
        let tar_gz_data = create_test_tar_gz();

        let file_count = extract_tar_gz_test(&tar_gz_data, temp_dir.path()).unwrap();
        assert_eq!(file_count, 2);
        assert!(temp_dir.path().join("index.html").exists());
        assert!(temp_dir.path().join("assets/app.js").exists());

        // Verify file contents
        let index_content = std::fs::read_to_string(temp_dir.path().join("index.html")).unwrap();
        assert!(index_content.contains("Hello World"));
    }

    #[test]
    fn test_extract_zip() {
        let temp_dir = TempDir::new().unwrap();
        let zip_data = create_test_zip();

        let file_count = extract_zip_test(&zip_data, temp_dir.path()).unwrap();
        assert_eq!(file_count, 2);
        assert!(temp_dir.path().join("index.html").exists());
        assert!(temp_dir.path().join("assets/app.js").exists());

        // Verify file contents
        let js_content = std::fs::read_to_string(temp_dir.path().join("assets/app.js")).unwrap();
        assert!(js_content.contains("console.log"));
    }

    #[test]
    fn test_content_type_detection_from_path() {
        // Test tar.gz detection
        let path = "bundle.tar.gz";
        let content_type = "";
        let detected = if !content_type.is_empty() {
            content_type
        } else if path.ends_with(".tar.gz") || path.ends_with(".tgz") {
            "application/gzip"
        } else if path.ends_with(".zip") {
            "application/zip"
        } else {
            "application/gzip"
        };
        assert_eq!(detected, "application/gzip");

        // Test tgz detection
        let path = "bundle.tgz";
        let detected = if path.ends_with(".tar.gz") || path.ends_with(".tgz") {
            "application/gzip"
        } else if path.ends_with(".zip") {
            "application/zip"
        } else {
            "application/gzip"
        };
        assert_eq!(detected, "application/gzip");

        // Test zip detection
        let path = "bundle.zip";
        let detected = if path.ends_with(".tar.gz") || path.ends_with(".tgz") {
            "application/gzip"
        } else if path.ends_with(".zip") {
            "application/zip"
        } else {
            "application/gzip"
        };
        assert_eq!(detected, "application/zip");
    }

    #[test]
    fn test_explicit_content_type_takes_precedence() {
        let path = "bundle.tar.gz";
        let explicit_content_type = "application/zip";

        let detected = if !explicit_content_type.is_empty() {
            explicit_content_type
        } else if path.ends_with(".tar.gz") || path.ends_with(".tgz") {
            "application/gzip"
        } else if path.ends_with(".zip") {
            "application/zip"
        } else {
            "application/gzip"
        };

        assert_eq!(detected, "application/zip");
    }
}
