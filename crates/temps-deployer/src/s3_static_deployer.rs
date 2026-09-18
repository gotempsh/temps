// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! S3-compatible `StaticDeployer`.
//!
//! Deploy-time uploads and admin-facing listing/removal only — never on the
//! proxy's per-request read path (see `temps-proxy/src/static_file_serving.rs`
//! and the `S3FileStore`-backed read path wired in `temps-proxy/src/server.rs`
//! for that). This mirrors [`crate::static_deployer::FilesystemStaticDeployer`]
//! byte for byte in its validation and resource-limit enforcement (symlink
//! rejection, sensitive-path rejection, per-file/aggregate/entry-count
//! limits) — only the destination changes, from a local directory tree to
//! S3 object keys under `{prefix}/{storage_path}/...`.
//!
//! Uses its own `aws_sdk_s3::Client`, built via the shared
//! `temps_file_store::s3_client::build_s3_client` helper, rather than going
//! through the `FileStore` trait: uploads stream directly off the local
//! build-output directory with `ByteStream::from_path`, which keeps memory
//! bounded for files up to `MAX_STATIC_ENTRY_BYTES` (500 MB) without needing
//! a streaming `put` on that trait.

use crate::static_deployer::{
    storage_relative_path, validate_storage_identifiers, FileInfo, StaticDeployError,
    StaticDeployRequest, StaticDeployResult, StaticDeployer, StaticDeploymentInfo,
};
use crate::static_ingestion::{MAX_STATIC_ENTRIES, MAX_STATIC_ENTRY_BYTES, MAX_STATIC_TOTAL_BYTES};
use async_trait::async_trait;
use aws_sdk_s3::primitives::ByteStream;
use aws_sdk_s3::Client as S3Client;
use chrono::Utc;
use std::path::{Path, PathBuf};
use temps_core::static_files::{validate_static_artifact_path, MAX_STATIC_PATH_COMPONENTS};
use temps_file_store::s3_client::build_s3_client;
use temps_file_store::s3_config::S3StorageConfig;
use tokio::fs;
use tracing::debug;

pub struct S3StaticDeployer {
    client: S3Client,
    bucket: String,
    prefix: Option<String>,
}

#[derive(Debug, Default)]
struct UploadStats {
    entry_count: u32,
    file_count: u32,
    total_size: u64,
}

type UploadFuture<'a> =
    std::pin::Pin<Box<dyn std::future::Future<Output = Result<(), StaticDeployError>> + Send + 'a>>;

impl S3StaticDeployer {
    pub fn new(config: S3StorageConfig) -> Self {
        let client = build_s3_client(&config);
        Self {
            client,
            bucket: config.bucket,
            prefix: config.prefix,
        }
    }

    fn full_key(&self, key: &str) -> String {
        match &self.prefix {
            Some(prefix) => format!("{}/{key}", prefix.trim_end_matches('/')),
            None => key.to_string(),
        }
    }

    fn object_key(&self, storage_path: &str, relative_path: &str) -> String {
        self.full_key(&format!("{storage_path}/{relative_path}"))
    }

    /// The exact key prefix (with trailing slash) all of one deployment's
    /// objects share.
    fn deployment_prefix(&self, storage_path: &str) -> String {
        self.full_key(&format!("{storage_path}/"))
    }

    async fn destination_exists(&self, storage_path: &str) -> Result<bool, StaticDeployError> {
        let prefix = self.deployment_prefix(storage_path);
        let response = self
            .client
            .list_objects_v2()
            .bucket(&self.bucket)
            .prefix(&prefix)
            .max_keys(1)
            .send()
            .await
            .map_err(|error| {
                StaticDeployError::Other(format!(
                    "Failed to check S3 destination s3://{}/{prefix}: {error}",
                    self.bucket
                ))
            })?;
        Ok(response.key_count().unwrap_or(0) > 0)
    }

    /// Delete every object under `prefix`, paginating through
    /// `ListObjectsV2`. Best-effort cleanup helpers call this and only log a
    /// failure — an orphaned partial upload under a unique, never-reused
    /// deployment key is a disk-cost nuisance, not a correctness problem.
    async fn delete_all_under_prefix(&self, prefix: &str) -> Result<(), StaticDeployError> {
        let mut continuation_token: Option<String> = None;
        loop {
            let mut request = self
                .client
                .list_objects_v2()
                .bucket(&self.bucket)
                .prefix(prefix);
            if let Some(token) = &continuation_token {
                request = request.continuation_token(token);
            }
            let response = request.send().await.map_err(|error| {
                StaticDeployError::Other(format!(
                    "ListObjectsV2 failed for s3://{}/{prefix}: {error}",
                    self.bucket
                ))
            })?;

            for object in response.contents() {
                let Some(key) = object.key() else { continue };
                self.client
                    .delete_object()
                    .bucket(&self.bucket)
                    .key(key)
                    .send()
                    .await
                    .map_err(|error| {
                        StaticDeployError::Other(format!(
                            "DeleteObject failed for s3://{}/{key}: {error}",
                            self.bucket
                        ))
                    })?;
            }

            if response.is_truncated().unwrap_or(false) {
                continuation_token = response.next_continuation_token().map(str::to_string);
            } else {
                break;
            }
        }
        Ok(())
    }

    async fn remove_partial_deployment_best_effort(&self, storage_path: &str) {
        let prefix = self.deployment_prefix(storage_path);
        if let Err(error) = self.delete_all_under_prefix(&prefix).await {
            debug!(
                prefix = %prefix,
                error = %error,
                "Failed to clean up a partial S3 static deployment"
            );
        }
    }

    /// Recursively validate and upload `source`'s contents under
    /// `storage_path`, enforcing the exact same resource limits as
    /// `FilesystemStaticDeployer::copy_dir_recursive`.
    fn upload_dir_recursive<'a>(
        &'a self,
        source_root: &'a Path,
        source: &'a Path,
        storage_path: &'a str,
        stats: &'a mut UploadStats,
    ) -> UploadFuture<'a> {
        Box::pin(async move {
            let mut entries = fs::read_dir(source).await.map_err(|error| {
                StaticDeployError::IoError(std::io::Error::new(
                    std::io::ErrorKind::NotFound,
                    format!("Failed to read source directory: {error}"),
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
                    self.upload_dir_recursive(source_root, &source_path, storage_path, stats)
                        .await?;
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

                    let relative_key = relative_path.to_str().ok_or_else(|| {
                        StaticDeployError::InvalidPath(format!(
                            "Static deployment source entry '{}' is not valid UTF-8",
                            relative_path.display()
                        ))
                    })?;
                    let object_key = self.object_key(storage_path, relative_key);
                    // Streams the file directly rather than buffering it, so a
                    // 500 MB file (the per-file limit above) never sits fully
                    // in memory during a deploy.
                    let body = ByteStream::from_path(&source_path).await.map_err(|error| {
                        StaticDeployError::IoError(std::io::Error::other(format!(
                            "Failed to open '{}' for upload: {error}",
                            source_path.display()
                        )))
                    })?;
                    self.client
                        .put_object()
                        .bucket(&self.bucket)
                        .key(&object_key)
                        .body(body)
                        .send()
                        .await
                        .map_err(|error| {
                            StaticDeployError::Other(format!(
                                "Failed to upload '{}' to s3://{}/{object_key}: {error}",
                                source_path.display(),
                                self.bucket
                            ))
                        })?;

                    stats.file_count = stats.file_count.checked_add(1).ok_or_else(|| {
                        StaticDeployError::ResourceLimitExceeded {
                            path: source.display().to_string(),
                            reason: "file count overflowed".to_string(),
                        }
                    })?;
                    stats.total_size = declared_total;

                    debug!(
                        "Uploaded {} -> s3://{}/{} ({} bytes)",
                        source_path.display(),
                        self.bucket,
                        object_key,
                        metadata.len()
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

    /// Scan for the storage-path prefix matching `deployment_slug`, then
    /// collect every object under it. Two passes rather than a single
    /// delimiter-based directory walk: the first pass doesn't yet know which
    /// date partition the deployment landed in — an `S3StaticDeployer` never
    /// stores that separately, so recovering it means matching on
    /// `/{deployment_slug}/` inside `projects/{project}/{env}/**`. Neither
    /// pass is on the proxy's hot path (this trait is control-plane only).
    async fn resolve_storage_path_and_files(
        &self,
        project_slug: &str,
        environment_slug: &str,
        deployment_slug: &str,
    ) -> Result<(String, Vec<FileInfo>), StaticDeployError> {
        let scan_prefix = self.full_key(&format!("projects/{project_slug}/{environment_slug}/"));
        let suffix = format!("/{deployment_slug}/");

        let mut storage_path: Option<String> = None;
        let mut continuation_token: Option<String> = None;
        loop {
            let mut request = self
                .client
                .list_objects_v2()
                .bucket(&self.bucket)
                .prefix(&scan_prefix);
            if let Some(token) = &continuation_token {
                request = request.continuation_token(token);
            }
            let response = request.send().await.map_err(|error| {
                StaticDeployError::Other(format!(
                    "ListObjectsV2 failed for s3://{}/{scan_prefix}: {error}",
                    self.bucket
                ))
            })?;

            for object in response.contents() {
                let Some(key) = object.key() else { continue };
                if let Some(index) = key.find(&suffix) {
                    let deployment_root_end = index + suffix.len();
                    let full_deployment_prefix = &key[..deployment_root_end];
                    let relative_to_bucket = match &self.prefix {
                        Some(prefix) => full_deployment_prefix
                            .strip_prefix(&format!("{}/", prefix.trim_end_matches('/')))
                            .unwrap_or(full_deployment_prefix),
                        None => full_deployment_prefix,
                    };
                    storage_path = Some(relative_to_bucket.trim_end_matches('/').to_string());
                    break;
                }
            }

            if storage_path.is_some() || !response.is_truncated().unwrap_or(false) {
                break;
            }
            continuation_token = response.next_continuation_token().map(str::to_string);
        }

        let storage_path = storage_path.ok_or_else(|| {
            StaticDeployError::DeploymentFailed(format!("Deployment not found: {deployment_slug}"))
        })?;

        let exact_prefix = self.deployment_prefix(&storage_path);
        let mut files = Vec::new();
        let mut continuation_token: Option<String> = None;
        loop {
            let mut request = self
                .client
                .list_objects_v2()
                .bucket(&self.bucket)
                .prefix(&exact_prefix);
            if let Some(token) = &continuation_token {
                request = request.continuation_token(token);
            }
            let response = request.send().await.map_err(|error| {
                StaticDeployError::Other(format!(
                    "ListObjectsV2 failed for s3://{}/{exact_prefix}: {error}",
                    self.bucket
                ))
            })?;

            for object in response.contents() {
                let Some(key) = object.key() else { continue };
                let Some(relative) = key.strip_prefix(&exact_prefix) else {
                    continue;
                };
                if relative.is_empty() {
                    continue;
                }
                files.push(FileInfo {
                    path: relative.to_string(),
                    size_bytes: object.size().unwrap_or(0).max(0) as u64,
                    is_directory: false,
                });
            }

            if response.is_truncated().unwrap_or(false) {
                continuation_token = response.next_continuation_token().map(str::to_string);
            } else {
                break;
            }
        }

        Ok((storage_path, files))
    }
}

#[async_trait]
impl StaticDeployer for S3StaticDeployer {
    async fn deploy(
        &self,
        request: StaticDeployRequest,
    ) -> Result<StaticDeployResult, StaticDeployError> {
        validate_storage_identifiers(
            &request.project_slug,
            &request.environment_slug,
            &request.deployment_slug,
        )?;

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

        let storage_path = storage_relative_path(
            &request.project_slug,
            &request.environment_slug,
            &request.deployment_slug,
        );

        if self.destination_exists(&storage_path).await? {
            return Err(StaticDeployError::DeploymentFailed(format!(
                "Static deployment destination already exists: s3://{}/{}",
                self.bucket,
                self.deployment_prefix(&storage_path)
            )));
        }

        debug!(
            "Deploying static files from {} to s3://{}/{}",
            request.source_dir.display(),
            self.bucket,
            storage_path
        );

        let mut stats = UploadStats::default();
        if let Err(error) = self
            .upload_dir_recursive(
                &request.source_dir,
                &request.source_dir,
                &storage_path,
                &mut stats,
            )
            .await
        {
            self.remove_partial_deployment_best_effort(&storage_path)
                .await;
            return Err(error);
        }

        Ok(StaticDeployResult {
            storage_path,
            file_count: stats.file_count,
            total_size_bytes: stats.total_size,
            deployed_at: Utc::now(),
        })
    }

    async fn get_deployment(
        &self,
        project_slug: &str,
        environment_slug: &str,
        deployment_slug: &str,
    ) -> Result<StaticDeploymentInfo, StaticDeployError> {
        validate_storage_identifiers(project_slug, environment_slug, deployment_slug)?;
        let (storage_path, files) = self
            .resolve_storage_path_and_files(project_slug, environment_slug, deployment_slug)
            .await?;

        Ok(StaticDeploymentInfo {
            deployment_slug: deployment_slug.to_string(),
            // Not a real filesystem path for this backend — the logical key
            // prefix under which every object of this deployment lives
            // (`{storage_path}/...` once joined with the configured bucket
            // prefix). No production caller dereferences this as disk state;
            // see the module doc.
            storage_path: PathBuf::from(storage_path),
            deployed_at: Utc::now(),
            file_count: files.len() as u32,
            total_size_bytes: files.iter().map(|file| file.size_bytes).sum(),
        })
    }

    async fn list_files(
        &self,
        project_slug: &str,
        environment_slug: &str,
        deployment_slug: &str,
    ) -> Result<Vec<FileInfo>, StaticDeployError> {
        validate_storage_identifiers(project_slug, environment_slug, deployment_slug)?;
        let (_storage_path, files) = self
            .resolve_storage_path_and_files(project_slug, environment_slug, deployment_slug)
            .await?;
        Ok(files)
    }

    async fn remove(
        &self,
        project_slug: &str,
        environment_slug: &str,
        deployment_slug: &str,
    ) -> Result<(), StaticDeployError> {
        validate_storage_identifiers(project_slug, environment_slug, deployment_slug)?;
        let (storage_path, _files) = self
            .resolve_storage_path_and_files(project_slug, environment_slug, deployment_slug)
            .await?;
        let prefix = self.deployment_prefix(&storage_path);
        self.delete_all_under_prefix(&prefix).await?;
        debug!("Removed S3 deployment: s3://{}/{}", self.bucket, prefix);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    fn sample_config() -> S3StorageConfig {
        S3StorageConfig {
            bucket: "temps-static".to_string(),
            region: "us-east-1".to_string(),
            endpoint: Some("http://127.0.0.1:9000".to_string()),
            access_key_id: "key".to_string(),
            secret_access_key: "secret".to_string(),
            force_path_style: true,
            timeout: Duration::from_secs(10),
            prefix: None,
        }
    }

    #[test]
    fn object_key_and_deployment_prefix_join_storage_path_and_relative_path() {
        let deployer = S3StaticDeployer::new(sample_config());
        assert_eq!(
            deployer.object_key("projects/site/prod/2026/09/18/deploy-1", "assets/app.js"),
            "projects/site/prod/2026/09/18/deploy-1/assets/app.js"
        );
        assert_eq!(
            deployer.deployment_prefix("projects/site/prod/2026/09/18/deploy-1"),
            "projects/site/prod/2026/09/18/deploy-1/"
        );
    }

    #[test]
    fn bucket_prefix_is_applied_ahead_of_the_deployment_key() {
        let mut config = sample_config();
        config.prefix = Some("prod/".to_string());
        let deployer = S3StaticDeployer::new(config);
        assert_eq!(
            deployer.object_key("projects/site/prod/2026/09/18/deploy-1", "index.html"),
            "prod/projects/site/prod/2026/09/18/deploy-1/index.html"
        );
    }

    #[tokio::test]
    async fn deploy_rejects_unclean_storage_identifiers_before_any_network_call() {
        let deployer = S3StaticDeployer::new(sample_config());
        let temp_dir = tempfile::tempdir().unwrap();
        std::fs::write(temp_dir.path().join("index.html"), b"ordinary").unwrap();

        for invalid in ["", ".", "..", "../escape", "/absolute", r"nested\escape"] {
            let error = deployer
                .deploy(StaticDeployRequest {
                    source_dir: temp_dir.path().to_path_buf(),
                    project_slug: invalid.to_string(),
                    environment_slug: "production".to_string(),
                    deployment_slug: "deploy".to_string(),
                })
                .await
                .unwrap_err();
            assert!(
                matches!(error, StaticDeployError::InvalidPath(_)),
                "identifier {invalid:?} must be rejected before any S3 call is attempted"
            );
        }
    }

    #[tokio::test]
    async fn deploy_rejects_missing_source_directory_before_any_network_call() {
        let deployer = S3StaticDeployer::new(sample_config());
        let error = deployer
            .deploy(StaticDeployRequest {
                source_dir: PathBuf::from("/nonexistent/temps-s3-static-deployer-test"),
                project_slug: "project".to_string(),
                environment_slug: "production".to_string(),
                deployment_slug: "deploy".to_string(),
            })
            .await
            .unwrap_err();
        assert!(matches!(error, StaticDeployError::SourceNotFound(_)));
    }

    #[tokio::test]
    async fn get_deployment_and_list_files_reject_unclean_identifiers() {
        let deployer = S3StaticDeployer::new(sample_config());
        let error = deployer
            .get_deployment("../escape", "production", "deploy")
            .await
            .unwrap_err();
        assert!(matches!(error, StaticDeployError::InvalidPath(_)));

        let error = deployer
            .list_files("project", "../escape", "deploy")
            .await
            .unwrap_err();
        assert!(matches!(error, StaticDeployError::InvalidPath(_)));

        let error = deployer
            .remove("project", "production", "../escape")
            .await
            .unwrap_err();
        assert!(matches!(error, StaticDeployError::InvalidPath(_)));
    }
}
