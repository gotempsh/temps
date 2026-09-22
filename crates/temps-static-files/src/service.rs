// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! File Service
//!
//! Service for reading files from the static directory

use std::path::{Component, Path};
use std::sync::Arc;
use temps_file_store::s3_config::{resolve_static_storage_backend, StaticStorageBackend};
use temps_file_store::s3_store::S3FileStore;
use temps_file_store::FileStore;
use tokio::fs;
use tracing::debug;

fn is_safe_relative_path(file_path: &str) -> bool {
    !Path::new(file_path).components().any(|component| {
        matches!(
            component,
            Component::ParentDir | Component::RootDir | Component::Prefix(_)
        )
    })
}

#[derive(Clone)]
pub struct FileService {
    config_service: Arc<temps_config::ConfigService>,
    durable_store: Option<Arc<dyn FileStore>>,
}

impl FileService {
    pub fn new(config_service: Arc<temps_config::ConfigService>) -> Self {
        Self {
            config_service,
            durable_store: None,
        }
    }

    pub fn from_config(
        config_service: Arc<temps_config::ConfigService>,
    ) -> Result<Self, temps_file_store::s3_config::StaticStorageConfigError> {
        let durable_store = match resolve_static_storage_backend()? {
            StaticStorageBackend::Filesystem => None,
            StaticStorageBackend::S3(config) => {
                Some(Arc::new(S3FileStore::new(config)) as Arc<dyn FileStore>)
            }
        };
        Ok(Self {
            config_service,
            durable_store,
        })
    }

    pub fn with_store(
        config_service: Arc<temps_config::ConfigService>,
        durable_store: Arc<dyn FileStore>,
    ) -> Self {
        Self {
            config_service,
            durable_store: Some(durable_store),
        }
    }

    /// Read a file from the static directory
    ///
    /// # Arguments
    /// * `file_path` - Relative path from static_dir (e.g., "screenshots/project/env/file.png")
    ///
    /// # Security
    /// - Path traversal is prevented by canonicalizing the path
    /// - Only files within static_dir can be accessed
    pub async fn get_file(&self, file_path: &str) -> Result<Vec<u8>, std::io::Error> {
        if !is_safe_relative_path(file_path) {
            return Err(std::io::Error::new(
                std::io::ErrorKind::PermissionDenied,
                format!("Access denied for static file path '{file_path}'"),
            ));
        }

        if let Some(store) = &self.durable_store {
            return store
                .get(file_path)
                .await
                .map(|bytes| bytes.to_vec())
                .map_err(|error| match error {
                    temps_file_store::FileStoreError::NotFound { .. } => std::io::Error::new(
                        std::io::ErrorKind::NotFound,
                        format!("Static file '{file_path}' was not found in durable storage"),
                    ),
                    other => std::io::Error::other(format!(
                        "Failed to read static file '{file_path}' from durable storage: {other}"
                    )),
                });
        }

        let static_dir = self.config_service.static_dir();
        let requested_path = static_dir.join(file_path);

        // Canonicalize to prevent path traversal attacks
        let canonical_path = requested_path.canonicalize()?;
        let canonical_static_dir = static_dir.canonicalize()?;

        // Ensure the requested file is within static_dir
        if !canonical_path.starts_with(&canonical_static_dir) {
            return Err(std::io::Error::new(
                std::io::ErrorKind::PermissionDenied,
                "Access denied: path outside static directory",
            ));
        }

        debug!(
            "Reading file: {} (canonical: {})",
            requested_path.display(),
            canonical_path.display()
        );

        fs::read(canonical_path).await
    }
}

#[cfg(test)]
mod tests {
    use super::is_safe_relative_path;

    #[test]
    fn durable_paths_accept_nested_screenshot_keys() {
        assert!(is_safe_relative_path(
            "screenshots/deployment-42-20260922.png"
        ));
    }

    #[test]
    fn durable_paths_reject_traversal_and_absolute_paths() {
        assert!(!is_safe_relative_path("../cloud-link/state.json"));
        assert!(!is_safe_relative_path("screenshots/../../secret"));
        assert!(!is_safe_relative_path("/etc/passwd"));
    }
}
