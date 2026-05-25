//! Deploy Static Bundle Job
//!
//! Deploys pre-uploaded static files from local storage.
//! This job is used for remote deployments where static files are built externally
//! and uploaded as a tar.gz or zip bundle.

use async_trait::async_trait;
use flate2::read::GzDecoder;
use serde::{Deserialize, Serialize};
use std::io::{Cursor, Read};
use std::path::{Component, Path, PathBuf};
use std::sync::Arc;
use tar::EntryType;
use temps_core::{JobResult, WorkflowContext, WorkflowError, WorkflowTask};
use temps_deployer::static_deployer::{StaticDeployRequest, StaticDeployer};
use temps_logs::{LogLevel, LogService};
use tracing::{debug, error, info, warn};
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

/// Job that deploys a static bundle from local storage
pub struct DeployStaticBundleJob {
    /// Unique job identifier
    job_id: String,
    /// Project ID (for logging/tracking purposes)
    project_id: i32,
    /// Path to the bundle in local storage (relative to data_dir)
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
    /// Data directory for reading the bundle
    data_dir: PathBuf,
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

/// Validate an archive entry path by walking its components.
///
/// Rejects paths with `..` (ParentDir) or absolute roots (RootDir / Prefix)
/// before the caller joins the path with a target directory.  Lexical
/// `starts_with` alone is insufficient because `target.join("../foo")`
/// can still resolve outside the target on the host filesystem.
///
/// Fix #17 / #18 path-traversal guard.
fn validate_archive_entry_path(path: &Path) -> Result<(), WorkflowError> {
    for component in path.components() {
        match component {
            Component::ParentDir => {
                return Err(WorkflowError::InvalidArchiveEntry {
                    path: path.display().to_string(),
                    reason: "contains parent directory reference (..)".into(),
                });
            }
            Component::RootDir | Component::Prefix(_) => {
                return Err(WorkflowError::InvalidArchiveEntry {
                    path: path.display().to_string(),
                    reason: "absolute path entries not allowed".into(),
                });
            }
            Component::Normal(_) | Component::CurDir => {} // safe
        }
    }
    Ok(())
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
        data_dir: PathBuf,
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
            data_dir,
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

    /// Detect content type - prioritize file extension over provided content_type
    /// to avoid mismatches (e.g., tar.gz file with wrong content-type header)
    fn detect_content_type(&self) -> &str {
        // ALWAYS check file extension first - it's more reliable than content_type header
        // which can be wrong due to multipart form handling or client misconfiguration
        if self.bundle_path.ends_with(".tar.gz") || self.bundle_path.ends_with(".tgz") {
            return "application/gzip";
        }
        if self.bundle_path.ends_with(".zip") {
            return "application/zip";
        }

        // Fall back to provided content type if extension is unknown
        if !self.content_type.is_empty() {
            return &self.content_type;
        }

        // Default to gzip
        "application/gzip"
    }

    // ── Decompression-bomb limits (Fix #27) ──────────────────────────────────
    // 2 GiB aggregate: generous for any realistic static-site bundle while still
    // bounding memory/disk against a crafted archive.
    // 500 MiB per entry: prevents a single huge file from dominating.
    /// Maximum total decompressed bytes across all entries (Fix #27).
    pub const MAX_EXTRACTED_BYTES: u64 = 2 * 1024 * 1024 * 1024; // 2 GiB
    /// Maximum decompressed bytes for a single archive entry (Fix #27).
    pub const MAX_ENTRY_BYTES: u64 = 500 * 1024 * 1024; // 500 MiB

    /// Extract tar.gz bundle to the target directory.
    ///
    /// Security hardening:
    ///  - Fix #17: validates every entry path via component walk before joining.
    ///  - Fix #19: skips symlink and hardlink entries (they can escape the sandbox).
    ///  - Fix #27: rejects entries whose declared size exceeds per-entry or
    ///    aggregate limits, and wraps the decoder in `io::Take` as a backstop.
    fn extract_tar_gz(
        &self,
        data: &[u8],
        target_dir: &std::path::Path,
    ) -> Result<u32, WorkflowError> {
        // Fix #27: io::Take backstop so any read past the aggregate limit
        // returns EOF and tar parsing fails before we accumulate unbounded data.
        let decoder = GzDecoder::new(Cursor::new(data));
        let limited = decoder.take(Self::MAX_EXTRACTED_BYTES);
        let mut archive = tar::Archive::new(limited);

        let mut file_count = 0u32;
        let mut extracted_total: u64 = 0;

        for entry in archive.entries().map_err(|e| {
            WorkflowError::JobExecutionFailed(format!("Failed to read tar.gz entries: {}", e))
        })? {
            let mut entry = entry.map_err(|e| {
                WorkflowError::JobExecutionFailed(format!("Failed to read tar entry: {}", e))
            })?;

            let path = entry.path().map_err(|e| {
                WorkflowError::JobExecutionFailed(format!("Failed to get entry path: {}", e))
            })?;
            let path = path.into_owned();

            // Fix #17: component-walk validation BEFORE joining with target_dir.
            // Lexical starts_with is insufficient: join("../foo") resolves outside.
            validate_archive_entry_path(&path)?;

            let dest_path = target_dir.join(&path);

            // Defense-in-depth: keep the starts_with check as well.
            if !dest_path.starts_with(target_dir) {
                return Err(WorkflowError::InvalidArchiveEntry {
                    path: path.display().to_string(),
                    reason: "resolved path escapes extraction directory".into(),
                });
            }

            // Fix #19: skip symlinks and hardlinks.  Symlinks can point outside
            // target_dir; static-file serving would follow them to arbitrary paths.
            match entry.header().entry_type() {
                EntryType::Regular | EntryType::GNUSparse | EntryType::Continuous => {
                    // Regular file — proceed to size check and extraction.
                }
                EntryType::Directory => {
                    std::fs::create_dir_all(&dest_path).map_err(|e| {
                        WorkflowError::JobExecutionFailed(format!(
                            "Failed to create directory {:?}: {}",
                            dest_path, e
                        ))
                    })?;
                    continue;
                }
                EntryType::Symlink | EntryType::Link => {
                    warn!(
                        path = %path.display(),
                        "Skipping symlink/hardlink entry in deployment bundle (security)"
                    );
                    continue;
                }
                other => {
                    warn!(
                        path = %path.display(),
                        entry_type = ?other,
                        "Skipping non-regular tar entry in deployment bundle"
                    );
                    continue;
                }
            }

            // Fix #27: per-entry size check on declared header size.
            let entry_size = entry.header().size().unwrap_or(0);
            if entry_size > Self::MAX_ENTRY_BYTES {
                return Err(WorkflowError::ArchiveTooLarge {
                    limit_bytes: Self::MAX_ENTRY_BYTES,
                });
            }
            extracted_total = extracted_total.saturating_add(entry_size);
            if extracted_total > Self::MAX_EXTRACTED_BYTES {
                return Err(WorkflowError::ArchiveTooLarge {
                    limit_bytes: Self::MAX_EXTRACTED_BYTES,
                });
            }

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

        Ok(file_count)
    }

    /// Extract zip bundle to the target directory.
    ///
    /// Security hardening:
    ///  - Fix #18: validates every entry path via component walk before joining.
    ///  - Fix #27: rejects entries whose declared uncompressed size exceeds
    ///    per-entry or aggregate limits.
    fn extract_zip(&self, data: &[u8], target_dir: &std::path::Path) -> Result<u32, WorkflowError> {
        let cursor = Cursor::new(data);
        let mut archive = ZipArchive::new(cursor).map_err(|e| {
            WorkflowError::JobExecutionFailed(format!("Failed to open zip archive: {}", e))
        })?;

        let mut file_count = 0u32;
        let mut extracted_total: u64 = 0;

        for i in 0..archive.len() {
            let mut file = archive.by_index(i).map_err(|e| {
                WorkflowError::JobExecutionFailed(format!("Failed to read zip entry {}: {}", i, e))
            })?;

            // Fix #18: `enclosed_name()` strips leading `/` and `..` segments,
            // but the returned path still needs component-walk validation since
            // the zip crate's stripping may not cover all cases.
            let entry_path = match file.enclosed_name() {
                Some(path) => path.to_path_buf(),
                None => continue, // zip crate already rejected this as unsafe
            };

            validate_archive_entry_path(&entry_path)?;

            let outpath = target_dir.join(&entry_path);

            // Defense-in-depth: keep the starts_with check.
            if !outpath.starts_with(target_dir) {
                return Err(WorkflowError::InvalidArchiveEntry {
                    path: entry_path.display().to_string(),
                    reason: "resolved path escapes extraction directory".into(),
                });
            }

            if file.is_dir() {
                std::fs::create_dir_all(&outpath).map_err(|e| {
                    WorkflowError::JobExecutionFailed(format!(
                        "Failed to create directory {:?}: {}",
                        outpath, e
                    ))
                })?;
            } else {
                // Fix #27: check declared uncompressed size from central directory
                // before decompressing — this fires before any data is read.
                let entry_size = file.size();
                if entry_size > Self::MAX_ENTRY_BYTES {
                    return Err(WorkflowError::ArchiveTooLarge {
                        limit_bytes: Self::MAX_ENTRY_BYTES,
                    });
                }
                extracted_total = extracted_total.saturating_add(entry_size);
                if extracted_total > Self::MAX_EXTRACTED_BYTES {
                    return Err(WorkflowError::ArchiveTooLarge {
                        limit_bytes: Self::MAX_EXTRACTED_BYTES,
                    });
                }

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

    /// Read the bundle data from local storage.
    ///
    /// Canonicalizes both the data_dir root and the resolved bundle path and
    /// verifies the result is still inside data_dir (defense-in-depth against
    /// a tampered bundle_path in the database).
    async fn download_bundle(&self) -> Result<Vec<u8>, WorkflowError> {
        let local_path = self.data_dir.join(&self.bundle_path);

        debug!("Reading bundle from local storage: {:?}", local_path);

        // Canonicalize the data_dir root (must already exist).
        let canonical_root =
            self.data_dir
                .canonicalize()
                .map_err(|e| WorkflowError::InvalidBundlePath {
                    path: self.data_dir.display().to_string(),
                    reason: format!("data_dir canonicalization failed: {e}"),
                })?;

        // Canonicalize the resolved bundle path (file must exist).
        let canonical =
            local_path
                .canonicalize()
                .map_err(|e| WorkflowError::InvalidBundlePath {
                    path: local_path.display().to_string(),
                    reason: format!("canonicalization failed: {e}"),
                })?;

        // Reject any path that escapes the data directory.
        if !canonical.starts_with(&canonical_root) {
            return Err(WorkflowError::InvalidBundlePath {
                path: canonical.display().to_string(),
                reason: format!("escapes data_dir {}", canonical_root.display()),
            });
        }

        tokio::fs::read(&canonical).await.map_err(|e| {
            WorkflowError::JobExecutionFailed(format!(
                "Failed to read bundle from local storage at {canonical:?}: {e}"
            ))
        })
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
            &format!("Starting static bundle deployment: {}", self.bundle_path),
        )
        .await;

        if let Some(bundle_id) = self.static_bundle_id {
            self.log(LogLevel::Info, &format!("Static bundle ID: {}", bundle_id))
                .await;
        }

        self.log(LogLevel::Info, "Reading bundle from local storage...")
            .await;

        let bundle_data = self.download_bundle().await?;

        self.log(
            LogLevel::Success,
            &format!("Read {} bytes from local storage", bundle_data.len()),
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
            "Extracting bundle to temporary directory...",
        )
        .await;

        // Extract based on content type.
        // Note: Must check for exact "application/zip" since "application/gzip" also contains "zip".
        let content_type = self.detect_content_type();
        let file_count = if content_type == "application/zip" {
            self.extract_zip(&bundle_data, &temp_dir)?
        } else {
            // Default to tar.gz (application/gzip, application/x-gzip, etc.)
            self.extract_tar_gz(&bundle_data, &temp_dir)?
        };

        self.log(
            LogLevel::Success,
            &format!("Extracted {} files", file_count),
        )
        .await;

        // Deploy using StaticDeployer
        let request = StaticDeployRequest {
            source_dir: temp_dir.clone(),
            project_slug: self.project_slug.clone(),
            environment_slug: self.environment_slug.clone(),
            deployment_slug: self.deployment_slug.clone(),
        };

        self.log(LogLevel::Info, "Deploying static files...").await;

        let result = self.static_deployer.deploy(request).await.map_err(|e| {
            WorkflowError::JobExecutionFailed(format!("Failed to deploy static files: {}", e))
        })?;

        self.log(
            LogLevel::Success,
            &format!("Deployed to: {}", result.storage_path),
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
                "Static bundle deployment complete: {} files ({} bytes)",
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

    // ── Archive builders ──────────────────────────────────────────────────────

    /// Build a raw (uncompressed) POSIX ustar tar archive with a single entry.
    ///
    /// Writes header bytes verbatim — bypassing the tar crate's `set_path` /
    /// `set_link_name` validators that also reject `..` components.  This lets
    /// tests produce malicious archives that the reader (production code) must
    /// then reject.
    ///
    /// Header layout (512-byte POSIX ustar block):
    ///   [0..100]   filename (NUL-terminated)
    ///   [100..108] mode (octal ASCII)
    ///   [124..136] size (octal ASCII, space-terminated)
    ///   [136..148] mtime
    ///   [148..156] checksum (recomputed after filling)
    ///   [156]      typeflag: b'0' regular, b'2' symlink, b'1' hardlink
    ///   [157..257] linkname
    ///   [257..265] magic "ustar  \0"
    fn build_raw_tar_entry(
        entry_path: &str,
        typeflag: u8,
        link_target: &str,
        data: &[u8],
    ) -> Vec<u8> {
        const BLOCK: usize = 512;
        let mut header = [0u8; BLOCK];

        let name = entry_path.as_bytes();
        header[..name.len().min(100)].copy_from_slice(&name[..name.len().min(100)]);
        header[100..108].copy_from_slice(b"0000644\0");
        let size_str = format!("{:011o} ", data.len());
        header[124..136].copy_from_slice(size_str.as_bytes());
        header[136..148].copy_from_slice(b"00000000000 ");
        header[156] = typeflag;
        let link = link_target.as_bytes();
        header[157..157 + link.len().min(100)].copy_from_slice(&link[..link.len().min(100)]);
        header[257..265].copy_from_slice(b"ustar  \0");

        // Compute checksum with field as spaces, then write.
        header[148..156].copy_from_slice(b"        ");
        let cksum: u32 = header.iter().map(|&b| b as u32).sum();
        let ck = format!("{:06o}\0 ", cksum);
        header[148..156].copy_from_slice(ck.as_bytes());

        let mut out = Vec::new();
        out.extend_from_slice(&header);
        out.extend_from_slice(data);
        let pad = (BLOCK - data.len() % BLOCK) % BLOCK;
        out.extend_from_slice(&vec![0u8; pad]);
        out.extend_from_slice(&[0u8; BLOCK * 2]); // end-of-archive
        out
    }

    /// Build a gzip-compressed tar with a single entry at the given raw path.
    ///
    /// Uses raw byte-level construction so `..` paths survive into the archive.
    fn build_tar_gz_with_path(entry_path: &str, content: &[u8]) -> Vec<u8> {
        let raw = build_raw_tar_entry(entry_path, b'0', "", content);
        let mut encoder = GzEncoder::new(Vec::new(), Compression::default());
        encoder.write_all(&raw).unwrap();
        encoder.finish().unwrap()
    }

    /// Build a gzip-compressed tar with a symlink entry.
    ///
    /// Uses raw byte-level construction so `..` link targets survive.
    fn build_tar_gz_with_symlink(link_name: &str, link_target: &str) -> Vec<u8> {
        let raw = build_raw_tar_entry(link_name, b'2', link_target, &[]);
        let mut encoder = GzEncoder::new(Vec::new(), Compression::default());
        encoder.write_all(&raw).unwrap();
        encoder.finish().unwrap()
    }

    /// Build a gzip-compressed tar with one entry whose header declares
    /// `claimed_size` bytes but carries no actual data.
    ///
    /// The archive is structurally malformed (header size > actual data), but
    /// the per-entry size check fires on the *declared* header value before any
    /// data is read, so this is sufficient for fix-#27 unit tests.
    fn build_tar_gz_with_claimed_size(entry_name: &str, claimed_size: u64) -> Vec<u8> {
        const BLOCK: usize = 512;
        let mut header = [0u8; BLOCK];
        let name = entry_name.as_bytes();
        header[..name.len().min(100)].copy_from_slice(&name[..name.len().min(100)]);
        header[100..108].copy_from_slice(b"0000644\0");
        let size_str = format!("{:011o} ", claimed_size);
        header[124..136].copy_from_slice(size_str.as_bytes());
        header[136..148].copy_from_slice(b"00000000000 ");
        header[156] = b'0';
        header[257..265].copy_from_slice(b"ustar  \0");
        header[148..156].copy_from_slice(b"        ");
        let cksum: u32 = header.iter().map(|&b| b as u32).sum();
        let ck = format!("{:06o}\0 ", cksum);
        header[148..156].copy_from_slice(ck.as_bytes());
        // Header only, no data blocks.
        let mut raw = header.to_vec();
        raw.extend_from_slice(&[0u8; BLOCK * 2]);
        let mut encoder = GzEncoder::new(Vec::new(), Compression::default());
        encoder.write_all(&raw).unwrap();
        encoder.finish().unwrap()
    }

    /// Build a zip with a single entry at the given raw path.
    fn build_zip_with_path(entry_path: &str, content: &[u8]) -> Vec<u8> {
        let mut buffer = Vec::new();
        let mut zip = zip::ZipWriter::new(Cursor::new(&mut buffer));
        let options = zip::write::SimpleFileOptions::default()
            .compression_method(zip::CompressionMethod::Stored);
        zip.start_file(entry_path, options).unwrap();
        zip.write_all(content).unwrap();
        zip.finish().unwrap(); // finish() consumes zip
        buffer
    }

    /// Build a valid two-file tar.gz for happy-path tests.
    fn create_test_tar_gz() -> Vec<u8> {
        let mut encoder = GzEncoder::new(Vec::new(), Compression::default());
        {
            let mut tar = tar::Builder::new(&mut encoder);

            let content = b"<html><body>Hello World</body></html>";
            let mut header = tar::Header::new_gnu();
            header.set_path("index.html").unwrap();
            header.set_size(content.len() as u64);
            header.set_mode(0o644);
            header.set_cksum();
            tar.append(&header, &content[..]).unwrap();

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

    /// Build a valid two-file zip for happy-path tests.
    fn create_test_zip() -> Vec<u8> {
        let mut buffer = Vec::new();
        let mut zip = zip::ZipWriter::new(Cursor::new(&mut buffer));
        let options = zip::write::SimpleFileOptions::default()
            .compression_method(zip::CompressionMethod::Stored);
        zip.start_file("index.html", options).unwrap();
        zip.write_all(b"<html><body>Hello World</body></html>")
            .unwrap();
        zip.start_file("assets/app.js", options).unwrap();
        zip.write_all(b"console.log('Hello');").unwrap();
        zip.finish().unwrap(); // finish() consumes zip
        buffer
    }

    fn make_job_for_extract() -> DeployStaticBundleJob {
        use temps_deployer::static_deployer::FilesystemStaticDeployer;
        let tmp = std::env::temp_dir();
        DeployStaticBundleJob {
            job_id: "test-job".to_string(),
            project_id: 1,
            bundle_path: "bundle.tar.gz".to_string(),
            content_type: "application/gzip".to_string(),
            static_bundle_id: None,
            project_slug: "proj".to_string(),
            environment_slug: "env".to_string(),
            deployment_slug: "dep".to_string(),
            data_dir: tmp.clone(),
            static_deployer: Arc::new(FilesystemStaticDeployer::new(tmp)),
            log_service: None,
            log_id: None,
        }
    }

    fn make_job_in_dir(data_dir: &std::path::Path, bundle_path: &str) -> DeployStaticBundleJob {
        use temps_deployer::static_deployer::FilesystemStaticDeployer;
        DeployStaticBundleJob {
            job_id: "test-job".to_string(),
            project_id: 1,
            bundle_path: bundle_path.to_string(),
            content_type: "application/gzip".to_string(),
            static_bundle_id: None,
            project_slug: "proj".to_string(),
            environment_slug: "env".to_string(),
            deployment_slug: "dep".to_string(),
            data_dir: data_dir.to_path_buf(),
            static_deployer: Arc::new(FilesystemStaticDeployer::new(data_dir.to_path_buf())),
            log_service: None,
            log_id: None,
        }
    }

    // ── Happy-path tests ──────────────────────────────────────────────────────

    #[test]
    fn test_extract_tar_gz() {
        let temp_dir = TempDir::new().unwrap();
        let job = make_job_for_extract();
        let file_count = job
            .extract_tar_gz(&create_test_tar_gz(), temp_dir.path())
            .unwrap();
        assert_eq!(file_count, 2);
        assert!(temp_dir.path().join("index.html").exists());
        assert!(temp_dir.path().join("assets/app.js").exists());
        let idx = std::fs::read_to_string(temp_dir.path().join("index.html")).unwrap();
        assert!(idx.contains("Hello World"));
    }

    #[test]
    fn test_extract_zip() {
        let temp_dir = TempDir::new().unwrap();
        let job = make_job_for_extract();
        let file_count = job
            .extract_zip(&create_test_zip(), temp_dir.path())
            .unwrap();
        assert_eq!(file_count, 2);
        assert!(temp_dir.path().join("index.html").exists());
        assert!(temp_dir.path().join("assets/app.js").exists());
        let js = std::fs::read_to_string(temp_dir.path().join("assets/app.js")).unwrap();
        assert!(js.contains("console.log"));
    }

    // ── Fix #17: tar zip-slip via parent-dir component ────────────────────────

    #[test]
    fn test_tar_rejects_parent_dir_traversal() {
        let temp_dir = TempDir::new().unwrap();
        let job = make_job_for_extract();
        let data = build_tar_gz_with_path("../escape.txt", b"pwned");
        let err = job.extract_tar_gz(&data, temp_dir.path()).unwrap_err();
        assert!(
            matches!(err, WorkflowError::InvalidArchiveEntry { .. }),
            "expected InvalidArchiveEntry for '..', got {:?}",
            err
        );
        assert!(!temp_dir
            .path()
            .parent()
            .unwrap()
            .join("escape.txt")
            .exists());
    }

    #[test]
    fn test_tar_rejects_absolute_path() {
        let temp_dir = TempDir::new().unwrap();
        let job = make_job_for_extract();
        let data = build_tar_gz_with_path("/etc/passwd", b"pwned");
        let err = job.extract_tar_gz(&data, temp_dir.path()).unwrap_err();
        assert!(
            matches!(err, WorkflowError::InvalidArchiveEntry { .. }),
            "expected InvalidArchiveEntry for absolute path, got {:?}",
            err
        );
    }

    // ── Fix #18: zip zip-slip via parent-dir component ────────────────────────

    #[test]
    fn test_zip_rejects_or_skips_parent_dir_traversal() {
        let temp_dir = TempDir::new().unwrap();
        let job = make_job_for_extract();
        let data = build_zip_with_path("subdir/../../escape.txt", b"pwned");
        let result = job.extract_zip(&data, temp_dir.path());
        // Either InvalidArchiveEntry or silently skipped (Ok(0))
        match result {
            Ok(n) => assert_eq!(n, 0, "dangerous zip entry must be skipped"),
            Err(WorkflowError::InvalidArchiveEntry { .. }) => {}
            Err(e) => panic!("unexpected error: {:?}", e),
        }
        assert!(!temp_dir
            .path()
            .parent()
            .unwrap()
            .join("escape.txt")
            .exists());
    }

    #[test]
    fn test_zip_rejects_or_skips_absolute_path() {
        let temp_dir = TempDir::new().unwrap();
        let job = make_job_for_extract();
        let data = build_zip_with_path("/etc/passwd", b"pwned");
        let result = job.extract_zip(&data, temp_dir.path());
        match result {
            Ok(n) => assert_eq!(n, 0, "absolute zip entry must be skipped"),
            Err(WorkflowError::InvalidArchiveEntry { .. }) => {}
            Err(e) => panic!("unexpected error: {:?}", e),
        }
    }

    // ── Fix #19: symlink entries skipped ─────────────────────────────────────

    #[test]
    fn test_tar_skips_symlink_entry() {
        let temp_dir = TempDir::new().unwrap();
        let job = make_job_for_extract();
        let data = build_tar_gz_with_symlink("evil_link", "../../../etc/shadow");
        let file_count = job.extract_tar_gz(&data, temp_dir.path()).unwrap();
        assert_eq!(file_count, 0, "symlink entry must be skipped");
        assert!(!temp_dir.path().join("evil_link").exists());
    }

    // ── Fix #27: decompression-bomb detection ────────────────────────────────

    #[test]
    fn test_tar_rejects_oversized_single_entry() {
        // A single entry whose declared header size > MAX_ENTRY_BYTES must be
        // rejected before any content is decompressed.
        let temp_dir = TempDir::new().unwrap();
        let job = make_job_for_extract();
        let huge: u64 = DeployStaticBundleJob::MAX_ENTRY_BYTES + 1;
        let data = build_tar_gz_with_claimed_size("huge.bin", huge);
        let err = job.extract_tar_gz(&data, temp_dir.path()).unwrap_err();
        assert!(
            matches!(err, WorkflowError::ArchiveTooLarge { .. }),
            "expected ArchiveTooLarge, got {:?}",
            err
        );
    }

    #[test]
    fn test_zip_rejects_oversized_single_entry() {
        // A zip entry whose declared uncompressed size > MAX_ENTRY_BYTES must
        // be rejected.  We write the content with Deflate so the zip file
        // is small on disk but file.size() returns the full uncompressed size.
        let temp_dir = TempDir::new().unwrap();
        let job = make_job_for_extract();

        let chunk = vec![0u8; 1024 * 1024]; // 1 MiB of zeros (compresses well)
        let total_chunks = (DeployStaticBundleJob::MAX_ENTRY_BYTES / (1024 * 1024)) as usize + 1;
        let mut buffer = Vec::new();
        {
            let mut zip = zip::ZipWriter::new(Cursor::new(&mut buffer));
            let opts = zip::write::SimpleFileOptions::default()
                .compression_method(zip::CompressionMethod::Deflated);
            zip.start_file("bomb.bin", opts).unwrap();
            for _ in 0..total_chunks {
                zip.write_all(&chunk).unwrap();
            }
            zip.finish().unwrap();
        }

        let err = job.extract_zip(&buffer, temp_dir.path()).unwrap_err();
        assert!(
            matches!(err, WorkflowError::ArchiveTooLarge { .. }),
            "expected ArchiveTooLarge for zip single entry, got {:?}",
            err
        );
    }

    // ── validate_archive_entry_path unit tests ────────────────────────────────

    #[test]
    fn test_validate_path_rejects_parent_dir() {
        let p = Path::new("../etc/passwd");
        assert!(matches!(
            validate_archive_entry_path(p),
            Err(WorkflowError::InvalidArchiveEntry { .. })
        ));
    }

    #[test]
    fn test_validate_path_rejects_root_dir() {
        let p = Path::new("/etc/passwd");
        assert!(matches!(
            validate_archive_entry_path(p),
            Err(WorkflowError::InvalidArchiveEntry { .. })
        ));
    }

    #[test]
    fn test_validate_path_accepts_normal_relative_paths() {
        assert!(validate_archive_entry_path(Path::new("index.html")).is_ok());
        assert!(validate_archive_entry_path(Path::new("assets/app.js")).is_ok());
        assert!(validate_archive_entry_path(Path::new("./assets/app.js")).is_ok());
        assert!(validate_archive_entry_path(Path::new("a/b/c/d.txt")).is_ok());
    }

    // ── Content-type detection ────────────────────────────────────────────────

    #[test]
    fn test_content_type_detection_from_path() {
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

        let path = "bundle.tgz";
        let detected = if path.ends_with(".tar.gz") || path.ends_with(".tgz") {
            "application/gzip"
        } else if path.ends_with(".zip") {
            "application/zip"
        } else {
            "application/gzip"
        };
        assert_eq!(detected, "application/gzip");

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
    fn test_file_extension_takes_precedence_over_content_type() {
        let path = "bundle.tar.gz";
        let explicit_content_type = "application/zip";
        let detected = if path.ends_with(".tar.gz") || path.ends_with(".tgz") {
            "application/gzip"
        } else if path.ends_with(".zip") {
            "application/zip"
        } else if !explicit_content_type.is_empty() {
            explicit_content_type
        } else {
            "application/gzip"
        };
        assert_eq!(detected, "application/gzip");
    }

    #[test]
    fn test_content_type_used_when_extension_unknown() {
        let path = "bundle.dat";
        let explicit_content_type = "application/zip";
        let detected = if path.ends_with(".tar.gz") || path.ends_with(".tgz") {
            "application/gzip"
        } else if path.ends_with(".zip") {
            "application/zip"
        } else if !explicit_content_type.is_empty() {
            explicit_content_type
        } else {
            "application/gzip"
        };
        assert_eq!(detected, "application/zip");
    }

    // ── download_bundle path-traversal defense ────────────────────────────────

    #[tokio::test]
    async fn test_download_bundle_rejects_path_traversal() {
        let root = TempDir::new().unwrap();
        let data_dir = root.path().join("data");
        let outside_dir = root.path().join("outside");
        std::fs::create_dir_all(&data_dir).unwrap();
        std::fs::create_dir_all(&outside_dir).unwrap();

        let sensitive = outside_dir.join("secret.txt");
        std::fs::write(&sensitive, b"super secret").unwrap();

        let traversal = "../outside/secret.txt";
        let job = make_job_in_dir(&data_dir, traversal);

        let result = job.download_bundle().await;
        assert!(result.is_err());
        let err = result.unwrap_err();
        assert!(
            matches!(err, WorkflowError::InvalidBundlePath { .. }),
            "expected InvalidBundlePath, got: {err:?}"
        );
    }

    #[tokio::test]
    async fn test_download_bundle_accepts_valid_path() {
        let root = TempDir::new().unwrap();
        let data_dir = root.path().join("data");
        let bundles_dir = data_dir.join("static-bundles");
        std::fs::create_dir_all(&bundles_dir).unwrap();

        let bundle_id = uuid::Uuid::new_v4();
        let bundle_filename = format!("{bundle_id}.tar.gz");
        let bundle_file = bundles_dir.join(&bundle_filename);
        let expected_bytes: Vec<u8> = vec![1, 2, 3, 4];
        std::fs::write(&bundle_file, &expected_bytes).unwrap();

        let bundle_path = format!("static-bundles/{bundle_filename}");
        let job = make_job_in_dir(&data_dir, &bundle_path);

        let result = job.download_bundle().await;
        assert!(result.is_ok(), "expected Ok, got: {result:?}");
        assert_eq!(result.unwrap(), expected_bytes);
    }

    #[tokio::test]
    async fn test_download_bundle_missing_file_returns_error() {
        let root = TempDir::new().unwrap();
        let data_dir = root.path().join("data");
        std::fs::create_dir_all(&data_dir).unwrap();

        let job = make_job_in_dir(&data_dir, "static-bundles/nonexistent.tar.gz");
        let result = job.download_bundle().await;
        assert!(result.is_err());
        let err = result.unwrap_err();
        assert!(
            matches!(err, WorkflowError::InvalidBundlePath { .. }),
            "expected InvalidBundlePath (canonicalization failed), got: {err:?}"
        );
    }
}
