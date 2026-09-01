// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Static File Deployer
//!
//! Handles deployment of static files (Vite, React, etc.) to organized filesystem storage

use crate::static_ingestion::{MAX_STATIC_ENTRIES, MAX_STATIC_ENTRY_BYTES, MAX_STATIC_TOTAL_BYTES};
use async_trait::async_trait;
use chrono::Utc;
use serde::{Deserialize, Serialize};
use std::path::{Component, Path, PathBuf};
use temps_core::static_files::{
    validate_static_artifact_path, validate_static_dir, MAX_STATIC_PATH_COMPONENTS,
};
use thiserror::Error;
use tokio::fs;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tracing::debug;

const COPY_BUFFER_BYTES: usize = 64 * 1024;

#[derive(Error, Debug)]
pub enum StaticDeployError {
    #[error("Deployment failed: {0}")]
    DeploymentFailed(String),

    #[error("Source directory not found: {0}")]
    SourceNotFound(String),

    #[error("IO error: {0}")]
    IoError(#[from] std::io::Error),

    #[error("Invalid path: {0}")]
    InvalidPath(String),

    #[error("Static deployment resource limit exceeded while processing '{path}': {reason}")]
    ResourceLimitExceeded { path: String, reason: String },

    #[error("Other error: {0}")]
    Other(String),
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StaticDeployRequest {
    /// Source directory containing built static files (e.g., dist/, build/)
    pub source_dir: PathBuf,
    /// Project slug for organizing files
    pub project_slug: String,
    /// Environment slug for organizing files
    pub environment_slug: String,
    /// Deployment slug (unique identifier)
    pub deployment_slug: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StaticDeployResult {
    /// Full path to deployed static files
    pub storage_path: String,
    /// Number of files deployed
    pub file_count: u32,
    /// Total size in bytes
    pub total_size_bytes: u64,
    /// Deployment timestamp
    pub deployed_at: chrono::DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StaticDeploymentInfo {
    pub deployment_slug: String,
    pub storage_path: PathBuf,
    pub deployed_at: chrono::DateTime<Utc>,
    pub file_count: u32,
    pub total_size_bytes: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FileInfo {
    pub path: String,
    pub size_bytes: u64,
    pub is_directory: bool,
}

/// Trait for deploying static files
#[async_trait]
pub trait StaticDeployer: Send + Sync {
    /// Deploy static files from source to organized storage
    async fn deploy(
        &self,
        request: StaticDeployRequest,
    ) -> Result<StaticDeployResult, StaticDeployError>;

    /// Get deployment information
    async fn get_deployment(
        &self,
        project_slug: &str,
        environment_slug: &str,
        deployment_slug: &str,
    ) -> Result<StaticDeploymentInfo, StaticDeployError>;

    /// List files in a deployment
    async fn list_files(
        &self,
        project_slug: &str,
        environment_slug: &str,
        deployment_slug: &str,
    ) -> Result<Vec<FileInfo>, StaticDeployError>;

    /// Remove a deployment
    async fn remove(
        &self,
        project_slug: &str,
        environment_slug: &str,
        deployment_slug: &str,
    ) -> Result<(), StaticDeployError>;
}

/// Filesystem-based static deployer with date-partitioned storage
pub struct FilesystemStaticDeployer {
    /// Base directory for static files (e.g., ~/.temps/static)
    base_dir: PathBuf,
}

// Type aliases to simplify complex async return types
type CopyDirFuture<'a> =
    std::pin::Pin<Box<dyn std::future::Future<Output = Result<(), StaticDeployError>> + Send + 'a>>;

#[derive(Debug, Default)]
struct CopyStats {
    entry_count: u32,
    file_count: u32,
    total_size: u64,
}

type ListFilesFuture<'a> = std::pin::Pin<
    Box<dyn std::future::Future<Output = Result<Vec<FileInfo>, StaticDeployError>> + Send + 'a>,
>;

impl FilesystemStaticDeployer {
    pub fn new(base_dir: PathBuf) -> Self {
        Self { base_dir }
    }

    /// Build the storage path with date partitioning
    /// Format: {base_dir}/projects/{project}/{env}/{year}/{month}/{day}/{deployment}/
    fn build_storage_path(&self, request: &StaticDeployRequest) -> PathBuf {
        let now = Utc::now();
        let year = now.format("%Y").to_string();
        let month = now.format("%m").to_string();
        let day = now.format("%d").to_string();

        self.base_dir
            .join("projects")
            .join(&request.project_slug)
            .join(&request.environment_slug)
            .join(year)
            .join(month)
            .join(day)
            .join(&request.deployment_slug)
    }

    fn validate_storage_component(name: &str, value: &str) -> Result<(), StaticDeployError> {
        if value.is_empty() || value.contains(['/', '\\']) {
            return Err(StaticDeployError::InvalidPath(format!(
                "{name} must be a single non-empty relative path component, got '{value}'"
            )));
        }

        let mut components = Path::new(value).components();
        match (components.next(), components.next()) {
            (Some(Component::Normal(component)), None) if component == value => Ok(()),
            _ => Err(StaticDeployError::InvalidPath(format!(
                "{name} must be a clean relative path component, got '{value}'"
            ))),
        }
    }

    fn validate_storage_identifiers(
        project_slug: &str,
        environment_slug: &str,
        deployment_slug: &str,
    ) -> Result<(), StaticDeployError> {
        Self::validate_storage_component("project_slug", project_slug)?;
        Self::validate_storage_component("environment_slug", environment_slug)?;
        Self::validate_storage_component("deployment_slug", deployment_slug)
    }

    async fn copy_file_bounded(
        source: &Path,
        dest: &Path,
        total_bytes_remaining: u64,
    ) -> Result<u64, StaticDeployError> {
        let mut input = fs::File::open(source).await.map_err(|error| {
            StaticDeployError::IoError(std::io::Error::new(
                error.kind(),
                format!("Failed to open source file {}: {error}", source.display()),
            ))
        })?;
        let mut output = fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(dest)
            .await
            .map_err(|error| {
                StaticDeployError::IoError(std::io::Error::new(
                    error.kind(),
                    format!(
                        "Failed to create destination file {}: {error}",
                        dest.display()
                    ),
                ))
            })?;
        let mut buffer = vec![0; COPY_BUFFER_BYTES];
        let mut copied = 0u64;

        loop {
            let read = input.read(&mut buffer).await.map_err(|error| {
                StaticDeployError::IoError(std::io::Error::new(
                    error.kind(),
                    format!("Failed to read source file {}: {error}", source.display()),
                ))
            })?;
            if read == 0 {
                break;
            }
            let next_size = copied.checked_add(read as u64).ok_or_else(|| {
                StaticDeployError::ResourceLimitExceeded {
                    path: source.display().to_string(),
                    reason: "copied byte count overflowed".to_string(),
                }
            })?;
            if next_size > MAX_STATIC_ENTRY_BYTES {
                return Err(StaticDeployError::ResourceLimitExceeded {
                    path: source.display().to_string(),
                    reason: format!(
                        "file exceeds the {MAX_STATIC_ENTRY_BYTES} byte per-file limit while streaming"
                    ),
                });
            }
            if next_size > total_bytes_remaining {
                return Err(StaticDeployError::ResourceLimitExceeded {
                    path: source.display().to_string(),
                    reason: format!(
                        "deployment exceeds the {MAX_STATIC_TOTAL_BYTES} byte aggregate limit while streaming"
                    ),
                });
            }

            output.write_all(&buffer[..read]).await.map_err(|error| {
                StaticDeployError::IoError(std::io::Error::new(
                    error.kind(),
                    format!(
                        "Failed to write destination file {}: {error}",
                        dest.display()
                    ),
                ))
            })?;
            copied = next_size;
        }

        output.flush().await.map_err(|error| {
            StaticDeployError::IoError(std::io::Error::new(
                error.kind(),
                format!(
                    "Failed to flush destination file {}: {error}",
                    dest.display()
                ),
            ))
        })?;
        Ok(copied)
    }

    /// Recursively copy directory contents
    fn copy_dir_recursive<'a>(
        source_root: &'a Path,
        source: &'a PathBuf,
        dest: &'a PathBuf,
        stats: &'a mut CopyStats,
    ) -> CopyDirFuture<'a> {
        Box::pin(async move {
            // Ensure destination directory exists
            fs::create_dir_all(dest).await?;

            let mut entries = fs::read_dir(source).await.map_err(|e| {
                StaticDeployError::IoError(std::io::Error::new(
                    std::io::ErrorKind::NotFound,
                    format!("Failed to read source directory: {}", e),
                ))
            })?;

            while let Some(entry) = entries.next_entry().await? {
                stats.entry_count = stats.entry_count.checked_add(1).ok_or_else(|| {
                    StaticDeployError::ResourceLimitExceeded {
                        path: source.display().to_string(),
                        reason: "entry count overflowed".to_string(),
                    }
                })?;
                if stats.entry_count > MAX_STATIC_ENTRIES {
                    return Err(StaticDeployError::ResourceLimitExceeded {
                        path: source_root.display().to_string(),
                        reason: format!("deployment exceeds the {MAX_STATIC_ENTRIES} entry limit"),
                    });
                }
                let source_path = entry.path();
                let file_name = entry.file_name();
                let dest_path = dest.join(&file_name);
                let relative_path = source_path.strip_prefix(source_root).map_err(|error| {
                    StaticDeployError::InvalidPath(format!(
                        "Static deployment source entry {} is outside source root {}: {error}",
                        source_path.display(),
                        source_root.display()
                    ))
                })?;
                let component_depth = relative_path.components().count();
                if component_depth > MAX_STATIC_PATH_COMPONENTS {
                    return Err(StaticDeployError::ResourceLimitExceeded {
                        path: relative_path.display().to_string(),
                        reason: format!(
                            "path has {component_depth} components, exceeding the {MAX_STATIC_PATH_COMPONENTS} component depth limit"
                        ),
                    });
                }
                validate_static_artifact_path(relative_path).map_err(|error| {
                    StaticDeployError::InvalidPath(format!(
                        "Static deployment source entry '{}' is not publishable: {error}",
                        relative_path.display()
                    ))
                })?;

                let metadata = fs::symlink_metadata(&source_path).await?;

                if metadata.file_type().is_symlink() {
                    return Err(StaticDeployError::InvalidPath(format!(
                        "Static deployment source contains a symbolic link: {}",
                        source_path.display()
                    )));
                } else if metadata.is_dir() {
                    // Recurse into subdirectory
                    Self::copy_dir_recursive(source_root, &source_path, &dest_path, stats).await?;
                } else if metadata.is_file() {
                    if metadata.len() > MAX_STATIC_ENTRY_BYTES {
                        return Err(StaticDeployError::ResourceLimitExceeded {
                            path: source_path.display().to_string(),
                            reason: format!(
                                "declared file size {} exceeds the {MAX_STATIC_ENTRY_BYTES} byte per-file limit",
                                metadata.len()
                            ),
                        });
                    }
                    let declared_total =
                        stats
                            .total_size
                            .checked_add(metadata.len())
                            .ok_or_else(|| StaticDeployError::ResourceLimitExceeded {
                                path: source_path.display().to_string(),
                                reason: "aggregate byte count overflowed".to_string(),
                            })?;
                    if declared_total > MAX_STATIC_TOTAL_BYTES {
                        return Err(StaticDeployError::ResourceLimitExceeded {
                            path: source_path.display().to_string(),
                            reason: format!(
                                "declared deployment size exceeds the {MAX_STATIC_TOTAL_BYTES} byte aggregate limit"
                            ),
                        });
                    }

                    let remaining = MAX_STATIC_TOTAL_BYTES.saturating_sub(stats.total_size);
                    let copied =
                        Self::copy_file_bounded(&source_path, &dest_path, remaining).await?;

                    stats.file_count = stats.file_count.checked_add(1).ok_or_else(|| {
                        StaticDeployError::ResourceLimitExceeded {
                            path: source.display().to_string(),
                            reason: "file count overflowed".to_string(),
                        }
                    })?;
                    stats.total_size = stats.total_size.checked_add(copied).ok_or_else(|| {
                        StaticDeployError::ResourceLimitExceeded {
                            path: source.display().to_string(),
                            reason: "aggregate byte count overflowed".to_string(),
                        }
                    })?;

                    debug!(
                        "Copied file: {} -> {} ({} bytes)",
                        source_path.display(),
                        dest_path.display(),
                        copied
                    );
                } else {
                    return Err(StaticDeployError::InvalidPath(format!(
                        "Static deployment source contains a non-regular filesystem entry: {}",
                        source_path.display()
                    )));
                }
            }

            Ok(())
        })
    }

    /// Recursively list files in a directory
    fn list_files_recursive<'a>(path: &'a PathBuf, base_path: &'a PathBuf) -> ListFilesFuture<'a> {
        Box::pin(async move {
            let mut files = Vec::new();

            let mut entries = fs::read_dir(path).await?;

            while let Some(entry) = entries.next_entry().await? {
                let entry_path = entry.path();
                let metadata = entry.metadata().await?;

                // Get relative path from base
                let relative_path = entry_path
                    .strip_prefix(base_path)
                    .map_err(|e| StaticDeployError::InvalidPath(e.to_string()))?;

                files.push(FileInfo {
                    path: relative_path.to_string_lossy().to_string(),
                    size_bytes: metadata.len(),
                    is_directory: metadata.is_dir(),
                });

                if metadata.is_dir() {
                    // Recurse into subdirectory
                    let sub_files = Self::list_files_recursive(&entry_path, base_path).await?;
                    files.extend(sub_files);
                }
            }

            Ok(files)
        })
    }
}

#[async_trait]
impl StaticDeployer for FilesystemStaticDeployer {
    async fn deploy(
        &self,
        request: StaticDeployRequest,
    ) -> Result<StaticDeployResult, StaticDeployError> {
        Self::validate_storage_identifiers(
            &request.project_slug,
            &request.environment_slug,
            &request.deployment_slug,
        )?;

        // Verify source directory exists
        if !request.source_dir.exists() {
            return Err(StaticDeployError::SourceNotFound(format!(
                "Source directory not found: {}",
                request.source_dir.display()
            )));
        }

        if !request.source_dir.is_dir() {
            return Err(StaticDeployError::InvalidPath(format!(
                "Source path is not a directory: {}",
                request.source_dir.display()
            )));
        }

        // Build destination path with date partitioning
        let storage_path = self.build_storage_path(&request);

        debug!(
            "Deploying static files from {} to {}",
            request.source_dir.display(),
            storage_path.display()
        );

        match fs::symlink_metadata(&storage_path).await {
            Ok(_) => {
                return Err(StaticDeployError::DeploymentFailed(format!(
                    "Static deployment destination already exists: {}",
                    storage_path.display()
                )));
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => {
                return Err(StaticDeployError::IoError(std::io::Error::new(
                    error.kind(),
                    format!(
                        "Failed to inspect static deployment destination {}: {error}",
                        storage_path.display()
                    ),
                )));
            }
        }

        // Copy files recursively and remove the unpublished partial deployment
        // if any validation, limit, or I/O check fails.
        let mut stats = CopyStats::default();
        if let Err(error) = Self::copy_dir_recursive(
            &request.source_dir,
            &request.source_dir,
            &storage_path,
            &mut stats,
        )
        .await
        {
            if let Err(cleanup_error) = fs::remove_dir_all(&storage_path).await {
                debug!(
                    path = %storage_path.display(),
                    error = %cleanup_error,
                    "Failed to clean partial static deployment"
                );
            }
            return Err(error);
        }

        let file_count = stats.file_count;
        let total_size = stats.total_size;

        debug!(
            "Deployed {} files ({} bytes) to {}",
            file_count,
            total_size,
            storage_path.display()
        );

        // Security: Store ONLY the relative path (without base_dir prefix)
        // This ensures the proxy always joins with the configured base directory,
        // preventing potential security issues from absolute paths in the database
        let relative_storage_path = storage_path.strip_prefix(&self.base_dir).map_err(|e| {
            StaticDeployError::InvalidPath(format!(
                "Storage path does not start with base_dir: {}",
                e
            ))
        })?;
        let relative_storage_path = relative_storage_path.to_str().ok_or_else(|| {
            StaticDeployError::InvalidPath(format!(
                "Generated static storage path is not valid UTF-8: {}",
                relative_storage_path.display()
            ))
        })?;
        let relative_storage_path =
            validate_static_dir(relative_storage_path).map_err(|error| {
                StaticDeployError::InvalidPath(format!(
                    "Generated static storage path '{relative_storage_path}' is invalid: {error}"
                ))
            })?;

        Ok(StaticDeployResult {
            storage_path: relative_storage_path.to_string_lossy().to_string(),
            file_count,
            total_size_bytes: total_size,
            deployed_at: Utc::now(),
        })
    }

    async fn get_deployment(
        &self,
        project_slug: &str,
        environment_slug: &str,
        deployment_slug: &str,
    ) -> Result<StaticDeploymentInfo, StaticDeployError> {
        Self::validate_storage_identifiers(project_slug, environment_slug, deployment_slug)?;

        // Search for deployment across all date partitions
        let project_env_path = self
            .base_dir
            .join("projects")
            .join(project_slug)
            .join(environment_slug);

        if !project_env_path.exists() {
            return Err(StaticDeployError::DeploymentFailed(format!(
                "Project/environment not found: {}/{}",
                project_slug, environment_slug
            )));
        }

        // Search through date partitions (year/month/day)
        let mut deployment_path: Option<PathBuf> = None;
        let mut year_entries = fs::read_dir(&project_env_path).await?;

        while let Some(year_entry) = year_entries.next_entry().await? {
            if !year_entry.metadata().await?.is_dir() {
                continue;
            }

            let mut month_entries = fs::read_dir(year_entry.path()).await?;
            while let Some(month_entry) = month_entries.next_entry().await? {
                if !month_entry.metadata().await?.is_dir() {
                    continue;
                }

                let mut day_entries = fs::read_dir(month_entry.path()).await?;
                while let Some(day_entry) = day_entries.next_entry().await? {
                    if !day_entry.metadata().await?.is_dir() {
                        continue;
                    }

                    let candidate = day_entry.path().join(deployment_slug);
                    if candidate.exists() {
                        deployment_path = Some(candidate);
                        break;
                    }
                }

                if deployment_path.is_some() {
                    break;
                }
            }

            if deployment_path.is_some() {
                break;
            }
        }

        let storage_path = deployment_path.ok_or_else(|| {
            StaticDeployError::DeploymentFailed(format!(
                "Deployment not found: {}",
                deployment_slug
            ))
        })?;

        // Calculate file count and total size
        let files = Self::list_files_recursive(&storage_path, &storage_path).await?;
        let file_count = files.iter().filter(|f| !f.is_directory).count() as u32;
        let total_size_bytes = files
            .iter()
            .filter(|f| !f.is_directory)
            .map(|f| f.size_bytes)
            .sum();

        // Get deployment timestamp from directory metadata
        let metadata = fs::metadata(&storage_path).await?;
        let deployed_at = metadata
            .created()
            .or_else(|_| metadata.modified())
            .map(chrono::DateTime::from)
            .unwrap_or_else(|_| Utc::now());

        Ok(StaticDeploymentInfo {
            deployment_slug: deployment_slug.to_string(),
            storage_path,
            deployed_at,
            file_count,
            total_size_bytes,
        })
    }

    async fn list_files(
        &self,
        project_slug: &str,
        environment_slug: &str,
        deployment_slug: &str,
    ) -> Result<Vec<FileInfo>, StaticDeployError> {
        Self::validate_storage_identifiers(project_slug, environment_slug, deployment_slug)?;
        let deployment_info = self
            .get_deployment(project_slug, environment_slug, deployment_slug)
            .await?;

        Self::list_files_recursive(&deployment_info.storage_path, &deployment_info.storage_path)
            .await
    }

    async fn remove(
        &self,
        project_slug: &str,
        environment_slug: &str,
        deployment_slug: &str,
    ) -> Result<(), StaticDeployError> {
        Self::validate_storage_identifiers(project_slug, environment_slug, deployment_slug)?;
        let deployment_info = self
            .get_deployment(project_slug, environment_slug, deployment_slug)
            .await?;

        fs::remove_dir_all(&deployment_info.storage_path).await?;

        debug!(
            "Removed deployment: {}",
            deployment_info.storage_path.display()
        );

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs as std_fs;
    use std::io::Write;
    use tempfile::TempDir;

    #[tokio::test]
    async fn test_deploy_static_files() {
        let temp_dir = TempDir::new().unwrap();
        let base_dir = temp_dir.path().join("static");
        let source_dir = temp_dir.path().join("source/dist");

        // Create test structure
        std_fs::create_dir_all(&source_dir).unwrap();
        std_fs::create_dir_all(source_dir.join("assets")).unwrap();

        // Create test files
        let mut file1 = std_fs::File::create(source_dir.join("index.html")).unwrap();
        file1.write_all(b"<html>Test</html>").unwrap();
        drop(file1);

        let mut file2 = std_fs::File::create(source_dir.join("assets/app.js")).unwrap();
        file2.write_all(b"console.log('test');").unwrap();
        drop(file2);

        // Deploy
        let deployer = FilesystemStaticDeployer::new(base_dir.clone());
        let request = StaticDeployRequest {
            source_dir,
            project_slug: "my-project".to_string(),
            environment_slug: "production".to_string(),
            deployment_slug: "deploy-123".to_string(),
        };

        let result = deployer.deploy(request).await.unwrap();

        assert_eq!(result.file_count, 2);
        assert!(result.total_size_bytes > 0);
        assert!(result.storage_path.contains("my-project"));
        assert!(result.storage_path.contains("production"));
        assert!(result.storage_path.contains("deploy-123"));

        // Verify path is relative (security requirement)
        let storage_path = PathBuf::from(&result.storage_path);
        assert!(
            storage_path.is_relative(),
            "Storage path should be relative for security: {}",
            result.storage_path
        );
        assert!(
            result.storage_path.starts_with("projects/"),
            "Storage path should start with 'projects/': {}",
            result.storage_path
        );

        // Verify files exist (join with base_dir to get full path)
        let full_path = base_dir.join(&result.storage_path);
        assert!(full_path.join("index.html").exists());
        assert!(full_path.join("assets/app.js").exists());
    }

    #[tokio::test]
    async fn test_get_deployment() {
        let temp_dir = TempDir::new().unwrap();
        let base_dir = temp_dir.path().join("static");
        let source_dir = temp_dir.path().join("source/dist");

        // Create and deploy test files
        std_fs::create_dir_all(&source_dir).unwrap();
        let mut file = std_fs::File::create(source_dir.join("index.html")).unwrap();
        file.write_all(b"<html>Test</html>").unwrap();
        drop(file);

        let deployer = FilesystemStaticDeployer::new(base_dir.clone());
        let request = StaticDeployRequest {
            source_dir,
            project_slug: "test-project".to_string(),
            environment_slug: "staging".to_string(),
            deployment_slug: "deploy-abc".to_string(),
        };

        deployer.deploy(request).await.unwrap();

        // Get deployment info
        let info = deployer
            .get_deployment("test-project", "staging", "deploy-abc")
            .await
            .unwrap();

        assert_eq!(info.deployment_slug, "deploy-abc");
        assert_eq!(info.file_count, 1);
        assert!(info.total_size_bytes > 0);
        assert!(info.storage_path.exists());
    }

    #[tokio::test]
    async fn test_list_files() {
        let temp_dir = TempDir::new().unwrap();
        let base_dir = temp_dir.path().join("static");
        let source_dir = temp_dir.path().join("source/dist");

        // Create test structure
        std_fs::create_dir_all(source_dir.join("assets")).unwrap();
        std_fs::File::create(source_dir.join("index.html"))
            .unwrap()
            .write_all(b"test")
            .unwrap();
        std_fs::File::create(source_dir.join("assets/app.js"))
            .unwrap()
            .write_all(b"test")
            .unwrap();

        let deployer = FilesystemStaticDeployer::new(base_dir);
        let request = StaticDeployRequest {
            source_dir,
            project_slug: "test".to_string(),
            environment_slug: "prod".to_string(),
            deployment_slug: "deploy-1".to_string(),
        };

        deployer.deploy(request).await.unwrap();

        // List files
        let files = deployer
            .list_files("test", "prod", "deploy-1")
            .await
            .unwrap();

        // Should have: index.html, assets/ (dir), assets/app.js
        assert!(files.len() >= 2);
        assert!(files.iter().any(|f| f.path.contains("index.html")));
        assert!(files.iter().any(|f| f.path.contains("app.js")));
    }

    #[tokio::test]
    async fn test_remove_deployment() {
        let temp_dir = TempDir::new().unwrap();
        let base_dir = temp_dir.path().join("static");
        let source_dir = temp_dir.path().join("source/dist");

        std_fs::create_dir_all(&source_dir).unwrap();
        std_fs::File::create(source_dir.join("index.html"))
            .unwrap()
            .write_all(b"test")
            .unwrap();

        // Keep a copy of base_dir for verification
        let base_dir_clone = base_dir.clone();
        let deployer = FilesystemStaticDeployer::new(base_dir);
        let request = StaticDeployRequest {
            source_dir,
            project_slug: "test".to_string(),
            environment_slug: "prod".to_string(),
            deployment_slug: "deploy-remove".to_string(),
        };

        let result = deployer.deploy(request).await.unwrap();

        // storage_path is now relative, join with base_dir to get full path
        let full_storage_path = base_dir_clone.join(&result.storage_path);
        assert!(full_storage_path.exists());

        // Remove deployment
        deployer
            .remove("test", "prod", "deploy-remove")
            .await
            .unwrap();

        // Verify it's gone
        assert!(!full_storage_path.exists());
    }

    #[tokio::test]
    async fn deploy_streams_large_ordinary_files_and_preserves_size() {
        let temp_dir = TempDir::new().unwrap();
        let base_dir = temp_dir.path().join("static");
        let source_dir = temp_dir.path().join("source");
        std_fs::create_dir_all(&source_dir).unwrap();
        let content = vec![b'x'; COPY_BUFFER_BYTES * 3 + 17];
        std_fs::write(source_dir.join("asset.bin"), &content).unwrap();

        let deployer = FilesystemStaticDeployer::new(base_dir.clone());
        let result = deployer
            .deploy(StaticDeployRequest {
                source_dir,
                project_slug: "project".to_string(),
                environment_slug: "production".to_string(),
                deployment_slug: "deploy-stream".to_string(),
            })
            .await
            .unwrap();

        assert_eq!(result.file_count, 1);
        assert_eq!(result.total_size_bytes, content.len() as u64);
        assert_eq!(
            std_fs::read(base_dir.join(result.storage_path).join("asset.bin")).unwrap(),
            content
        );
    }

    #[tokio::test]
    async fn deploy_rejects_sensitive_files_and_cleans_partial_destination() {
        for sensitive_path in [".env", ".git/config", "assets/app.js.map"] {
            let temp_dir = TempDir::new().unwrap();
            let base_dir = temp_dir.path().join("static");
            let source_dir = temp_dir.path().join("source");
            let sensitive_file = source_dir.join(sensitive_path);
            std_fs::create_dir_all(sensitive_file.parent().unwrap()).unwrap();
            std_fs::write(&sensitive_file, b"must not publish").unwrap();
            std_fs::write(source_dir.join("index.html"), b"ordinary").unwrap();

            let deployer = FilesystemStaticDeployer::new(base_dir);
            let request = StaticDeployRequest {
                source_dir,
                project_slug: "project".to_string(),
                environment_slug: "production".to_string(),
                deployment_slug: "deploy-sensitive".to_string(),
            };
            let destination = deployer.build_storage_path(&request);
            let error = deployer.deploy(request).await.unwrap_err();

            assert!(matches!(error, StaticDeployError::InvalidPath(_)));
            assert!(
                !destination.exists(),
                "partial destination remained after rejecting {sensitive_path}"
            );
        }
    }

    #[tokio::test]
    async fn deploy_allows_documented_well_known_content() {
        let temp_dir = TempDir::new().unwrap();
        let base_dir = temp_dir.path().join("static");
        let source_dir = temp_dir.path().join("source");
        std_fs::create_dir_all(source_dir.join(".well-known/acme-challenge")).unwrap();
        std_fs::write(
            source_dir.join(".well-known/security.txt"),
            b"Contact: mailto:test@example.test",
        )
        .unwrap();
        std_fs::write(
            source_dir.join(".well-known/acme-challenge/token"),
            b"challenge",
        )
        .unwrap();

        let deployer = FilesystemStaticDeployer::new(base_dir.clone());
        let result = deployer
            .deploy(StaticDeployRequest {
                source_dir,
                project_slug: "project".to_string(),
                environment_slug: "production".to_string(),
                deployment_slug: "deploy-well-known".to_string(),
            })
            .await
            .unwrap();
        let destination = base_dir.join(result.storage_path);

        assert!(destination.join(".well-known/security.txt").is_file());
        assert!(destination
            .join(".well-known/acme-challenge/token")
            .is_file());
    }

    #[tokio::test]
    async fn deploy_rejects_unclean_storage_identifiers_before_writing() {
        let temp_dir = TempDir::new().unwrap();
        let base_dir = temp_dir.path().join("static");
        let source_dir = temp_dir.path().join("source");
        std_fs::create_dir_all(&source_dir).unwrap();
        std_fs::write(source_dir.join("index.html"), b"ordinary").unwrap();
        let deployer = FilesystemStaticDeployer::new(base_dir.clone());

        for invalid in ["", ".", "..", "../escape", "/absolute", r"nested\escape"] {
            let error = deployer
                .deploy(StaticDeployRequest {
                    source_dir: source_dir.clone(),
                    project_slug: invalid.to_string(),
                    environment_slug: "production".to_string(),
                    deployment_slug: "deploy".to_string(),
                })
                .await
                .unwrap_err();
            assert!(
                matches!(error, StaticDeployError::InvalidPath(_)),
                "identifier {invalid:?} must be rejected"
            );
        }
        assert!(!base_dir.exists());
    }

    #[test]
    fn test_validate_storage_identifiers_unclean_value_in_any_position_returns_invalid_path() {
        // Arrange
        let invalid_values = ["", ".", "..", "../escape", "/absolute", r"nested\escape"];

        // Act / Assert
        for invalid in invalid_values {
            for identifiers in [
                (invalid, "production", "deploy"),
                ("project", invalid, "deploy"),
                ("project", "production", invalid),
            ] {
                let error = FilesystemStaticDeployer::validate_storage_identifiers(
                    identifiers.0,
                    identifiers.1,
                    identifiers.2,
                )
                .expect_err("unclean storage identifier must fail");
                assert!(
                    matches!(error, StaticDeployError::InvalidPath(_)),
                    "identifier tuple {identifiers:?} must be rejected"
                );
            }
        }
    }

    #[tokio::test]
    async fn test_copy_file_bounded_aggregate_limit_crossed_does_not_write_crossing_chunk() {
        // Arrange
        let temp_dir = TempDir::new().unwrap();
        let source = temp_dir.path().join("source.bin");
        let destination = temp_dir.path().join("destination.bin");
        std_fs::write(&source, b"two bytes").unwrap();

        // Act
        let error = FilesystemStaticDeployer::copy_file_bounded(&source, &destination, 1)
            .await
            .expect_err("aggregate limit crossing must fail");

        // Assert
        assert!(matches!(
            error,
            StaticDeployError::ResourceLimitExceeded { .. }
        ));
        assert_eq!(
            std_fs::metadata(destination).unwrap().len(),
            0,
            "the chunk crossing the aggregate limit must not be written"
        );
    }

    #[tokio::test]
    async fn test_copy_dir_recursive_entry_limit_crossed_rejects_small_fixture() {
        // Arrange
        let temp_dir = TempDir::new().unwrap();
        let source = temp_dir.path().join("source");
        let destination = temp_dir.path().join("destination");
        std_fs::create_dir_all(&source).unwrap();
        std_fs::write(source.join("asset.txt"), b"small").unwrap();
        let mut stats = CopyStats {
            entry_count: MAX_STATIC_ENTRIES,
            ..CopyStats::default()
        };

        // Act
        let error = FilesystemStaticDeployer::copy_dir_recursive(
            &source,
            &source,
            &destination,
            &mut stats,
        )
        .await
        .expect_err("entry limit crossing must fail");

        // Assert
        assert!(matches!(
            error,
            StaticDeployError::ResourceLimitExceeded { .. }
        ));
        assert!(!destination.join("asset.txt").exists());
    }

    #[tokio::test]
    async fn test_copy_dir_recursive_total_limit_crossed_rejects_small_fixture() {
        // Arrange
        let temp_dir = TempDir::new().unwrap();
        let source = temp_dir.path().join("source");
        let destination = temp_dir.path().join("destination");
        std_fs::create_dir_all(&source).unwrap();
        std_fs::write(source.join("asset.txt"), b"small").unwrap();
        let mut stats = CopyStats {
            total_size: MAX_STATIC_TOTAL_BYTES,
            ..CopyStats::default()
        };

        // Act
        let error = FilesystemStaticDeployer::copy_dir_recursive(
            &source,
            &source,
            &destination,
            &mut stats,
        )
        .await
        .expect_err("aggregate limit crossing must fail");

        // Assert
        assert!(matches!(
            error,
            StaticDeployError::ResourceLimitExceeded { .. }
        ));
        assert!(!destination.join("asset.txt").exists());
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn test_deploy_special_file_rejected_and_partial_destination_removed() {
        use std::os::unix::net::UnixListener;

        // Arrange
        let temp_dir = TempDir::new().unwrap();
        let base_dir = temp_dir.path().join("static");
        let source_dir = temp_dir.path().join("source");
        std_fs::create_dir_all(&source_dir).unwrap();
        std_fs::write(source_dir.join("index.html"), b"ordinary").unwrap();
        let _socket = match UnixListener::bind(source_dir.join("server.sock")) {
            Ok(socket) => socket,
            Err(error) if error.kind() == std::io::ErrorKind::PermissionDenied => {
                eprintln!("Skipping Unix socket fixture: {error}");
                return;
            }
            Err(error) => panic!("failed to create Unix socket fixture: {error}"),
        };
        let deployer = FilesystemStaticDeployer::new(base_dir);
        let request = StaticDeployRequest {
            source_dir,
            project_slug: "project".to_string(),
            environment_slug: "production".to_string(),
            deployment_slug: "deploy-special".to_string(),
        };
        let destination = deployer.build_storage_path(&request);

        // Act
        let error = deployer
            .deploy(request)
            .await
            .expect_err("special filesystem entry must fail");

        // Assert
        assert!(matches!(error, StaticDeployError::InvalidPath(_)));
        assert!(
            !destination.exists(),
            "partial deployment must be removed after special-file rejection"
        );
    }

    #[tokio::test]
    async fn deploy_rejects_oversized_declared_file_before_copying() {
        let temp_dir = TempDir::new().unwrap();
        let base_dir = temp_dir.path().join("static");
        let source_dir = temp_dir.path().join("source");
        std_fs::create_dir_all(&source_dir).unwrap();
        std_fs::File::create(source_dir.join("large.bin"))
            .unwrap()
            .set_len(MAX_STATIC_ENTRY_BYTES + 1)
            .unwrap();
        let deployer = FilesystemStaticDeployer::new(base_dir);
        let request = StaticDeployRequest {
            source_dir,
            project_slug: "project".to_string(),
            environment_slug: "production".to_string(),
            deployment_slug: "deploy-large".to_string(),
        };
        let destination = deployer.build_storage_path(&request);

        let error = deployer.deploy(request).await.unwrap_err();
        assert!(matches!(
            error,
            StaticDeployError::ResourceLimitExceeded { .. }
        ));
        assert!(!destination.exists());
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn deploy_rejects_symbolic_links() {
        use std::os::unix::fs::symlink;

        let temp_dir = TempDir::new().unwrap();
        let base_dir = temp_dir.path().join("static");
        let source_dir = temp_dir.path().join("source");
        std_fs::create_dir_all(&source_dir).unwrap();
        let outside = temp_dir.path().join("outside.txt");
        std_fs::write(&outside, b"secret").unwrap();
        symlink(&outside, source_dir.join("linked.txt")).unwrap();
        let deployer = FilesystemStaticDeployer::new(base_dir);
        let request = StaticDeployRequest {
            source_dir,
            project_slug: "project".to_string(),
            environment_slug: "production".to_string(),
            deployment_slug: "deploy-link".to_string(),
        };
        let destination = deployer.build_storage_path(&request);

        let error = deployer.deploy(request).await.unwrap_err();
        assert!(matches!(error, StaticDeployError::InvalidPath(_)));
        assert!(!destination.exists());
    }

    fn create_file_at_component_depth(root: &Path, component_depth: usize) {
        assert!(component_depth > 0);
        let mut file_path = root.to_path_buf();
        for _ in 1..component_depth {
            file_path.push("d");
        }
        std_fs::create_dir_all(&file_path).unwrap();
        std_fs::write(file_path.join("asset.txt"), b"small").unwrap();
    }

    #[tokio::test]
    async fn recursive_copy_accepts_path_at_exact_component_depth_limit() {
        let temp_dir = TempDir::new().unwrap();
        let base_dir = temp_dir.path().join("static");
        let source_dir = temp_dir.path().join("source");
        create_file_at_component_depth(&source_dir, MAX_STATIC_PATH_COMPONENTS);
        let deployer = FilesystemStaticDeployer::new(base_dir);

        let result = deployer
            .deploy(StaticDeployRequest {
                source_dir,
                project_slug: "project".to_string(),
                environment_slug: "production".to_string(),
                deployment_slug: "deploy-depth-limit".to_string(),
            })
            .await
            .expect("path at exact depth limit must deploy");

        assert_eq!(result.file_count, 1);
    }

    #[tokio::test]
    async fn recursive_copy_rejects_path_over_component_depth_and_cleans_output() {
        let temp_dir = TempDir::new().unwrap();
        let base_dir = temp_dir.path().join("static");
        let source_dir = temp_dir.path().join("source");
        create_file_at_component_depth(&source_dir, MAX_STATIC_PATH_COMPONENTS + 1);
        let deployer = FilesystemStaticDeployer::new(base_dir);
        let request = StaticDeployRequest {
            source_dir,
            project_slug: "project".to_string(),
            environment_slug: "production".to_string(),
            deployment_slug: "deploy-depth-over".to_string(),
        };
        let destination = deployer.build_storage_path(&request);

        let error = deployer
            .deploy(request)
            .await
            .expect_err("path over depth limit must fail");

        assert!(matches!(
            error,
            StaticDeployError::ResourceLimitExceeded { .. }
        ));
        assert!(!destination.exists());
    }
}
