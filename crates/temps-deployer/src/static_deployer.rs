//! Static File Deployer
//!
//! Handles deployment of static files (Vite, React, etc.) to organized filesystem storage

use async_trait::async_trait;
use chrono::Utc;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use thiserror::Error;
use tokio::fs;
use tracing::debug;

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

    /// Recursively copy directory contents
    fn copy_dir_recursive<'a>(
        source: &'a PathBuf,
        dest: &'a PathBuf,
    ) -> std::pin::Pin<
        Box<dyn std::future::Future<Output = Result<(u32, u64), StaticDeployError>> + Send + 'a>,
    > {
        Box::pin(async move {
            let mut file_count = 0u32;
            let mut total_size = 0u64;

            // Ensure destination directory exists
            fs::create_dir_all(dest).await?;

            let mut entries = fs::read_dir(source).await.map_err(|e| {
                StaticDeployError::IoError(std::io::Error::new(
                    std::io::ErrorKind::NotFound,
                    format!("Failed to read source directory: {}", e),
                ))
            })?;

            while let Some(entry) = entries.next_entry().await? {
                let source_path = entry.path();
                let file_name = entry.file_name();
                let dest_path = dest.join(&file_name);

                let metadata = entry.metadata().await?;

                if metadata.is_dir() {
                    // Recurse into subdirectory
                    let (sub_count, sub_size) =
                        Self::copy_dir_recursive(&source_path, &dest_path).await?;
                    file_count += sub_count;
                    total_size += sub_size;
                } else if metadata.is_file() {
                    // Copy file using read + write for reliability
                    let content = fs::read(&source_path).await.map_err(|e| {
                        StaticDeployError::IoError(std::io::Error::new(
                            e.kind(),
                            format!("Failed to read file {}: {}", source_path.display(), e),
                        ))
                    })?;

                    fs::write(&dest_path, content).await.map_err(|e| {
                        StaticDeployError::IoError(std::io::Error::new(
                            e.kind(),
                            format!("Failed to write file {}: {}", dest_path.display(), e),
                        ))
                    })?;

                    file_count += 1;
                    total_size += metadata.len();

                    debug!(
                        "Copied file: {} -> {} ({} bytes)",
                        source_path.display(),
                        dest_path.display(),
                        metadata.len()
                    );
                }
            }

            Ok((file_count, total_size))
        })
    }

    /// Recursively list files in a directory
    fn list_files_recursive<'a>(
        path: &'a PathBuf,
        base_path: &'a PathBuf,
    ) -> std::pin::Pin<
        Box<dyn std::future::Future<Output = Result<Vec<FileInfo>, StaticDeployError>> + Send + 'a>,
    > {
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

        // Copy files recursively
        let (file_count, total_size) =
            Self::copy_dir_recursive(&request.source_dir, &storage_path).await?;

        debug!(
            "Deployed {} files ({} bytes) to {}",
            file_count,
            total_size,
            storage_path.display()
        );

        // Security: Store ONLY the relative path (without base_dir prefix)
        // This ensures the proxy always joins with the configured base directory,
        // preventing potential security issues from absolute paths in the database
        let relative_storage_path = storage_path
            .strip_prefix(&self.base_dir)
            .map_err(|e| {
                StaticDeployError::InvalidPath(format!(
                    "Storage path does not start with base_dir: {}",
                    e
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
            StaticDeployError::DeploymentFailed(format!("Deployment not found: {}", deployment_slug))
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
            .map(|t| chrono::DateTime::from(t))
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
        let deployment_info =
            self.get_deployment(project_slug, environment_slug, deployment_slug)
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
        let deployment_info =
            self.get_deployment(project_slug, environment_slug, deployment_slug)
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
        let files = deployer.list_files("test", "prod", "deploy-1").await.unwrap();

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
}
