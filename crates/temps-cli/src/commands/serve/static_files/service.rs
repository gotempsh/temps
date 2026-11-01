// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! File Service
//!
//! Service for reading files from the static directory

use std::path::{Component, Path};
use std::sync::Arc;
use temps_file_store::s3_config::StaticStorageBackend;
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
pub(super) struct FileService {
    config_service: Arc<temps_config::ConfigService>,
    durable_store: Option<Arc<dyn FileStore>>,
}

impl FileService {
    #[cfg(test)]
    pub(super) fn new(config_service: Arc<temps_config::ConfigService>) -> Self {
        Self {
            config_service,
            durable_store: None,
        }
    }

    pub(super) fn from_config(
        config_service: Arc<temps_config::ConfigService>,
        stateless: &temps_file_store::s3_config::StatelessStorage,
    ) -> Result<Self, temps_file_store::s3_config::StaticStorageConfigError> {
        let durable_store =
            match temps_file_store::s3_config::resolve_static_storage_backend_for(stateless)? {
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

    #[cfg(test)]
    pub(super) fn with_store(
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
    pub(super) async fn get_file(&self, file_path: &str) -> Result<Vec<u8>, std::io::Error> {
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

/// A `ConfigService` whose data directory (and so static directory) is
/// `data_dir`, backed by a mock database the file paths never touch.
#[cfg(test)]
pub(super) fn config_service_for_tests(data_dir: &Path) -> Arc<temps_config::ConfigService> {
    let mut config = temps_config::ServerConfig::new(
        "127.0.0.1:3000".to_string(),
        "postgresql://test".to_string(),
        None,
        Some("127.0.0.1:8000".to_string()),
    )
    .expect("valid test server config");
    config.data_dir = data_dir.to_path_buf();
    let db =
        Arc::new(sea_orm::MockDatabase::new(sea_orm::DatabaseBackend::Postgres).into_connection());
    Arc::new(temps_config::ConfigService::new(Arc::new(config), db))
}

#[cfg(test)]
mod tests {
    use std::io::ErrorKind;

    use bytes::Bytes;
    use temps_file_store::fs_store::FsFileStore;

    use super::*;

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

    #[tokio::test]
    async fn durable_store_serves_files_and_reports_missing_ones() {
        let dir = tempfile::tempdir().expect("temp dir");
        let store = FsFileStore::new(dir.path());
        store
            .put(
                "screenshots/deployment-42.png",
                Bytes::from_static(b"png-bytes"),
            )
            .await
            .expect("seed screenshot");
        let service =
            FileService::with_store(config_service_for_tests(dir.path()), Arc::new(store));

        let content = service
            .get_file("screenshots/deployment-42.png")
            .await
            .expect("stored file is served");
        assert_eq!(content, b"png-bytes");

        let missing = service
            .get_file("screenshots/deployment-43.png")
            .await
            .expect_err("missing file");
        assert_eq!(missing.kind(), ErrorKind::NotFound);
        assert!(
            missing.to_string().contains("deployment-43.png"),
            "{missing}"
        );

        let traversal = service
            .get_file("../cloud-link/state.json")
            .await
            .expect_err("traversal is refused before the store is read");
        assert_eq!(traversal.kind(), ErrorKind::PermissionDenied);
    }

    #[tokio::test]
    async fn filesystem_reads_stay_inside_the_static_directory() {
        let data_dir = tempfile::tempdir().expect("temp dir");
        let config_service = config_service_for_tests(data_dir.path());
        let static_dir = config_service.static_dir();
        std::fs::create_dir_all(static_dir.join("logs")).expect("static dir");
        std::fs::write(static_dir.join("logs/build.log"), b"build ok").expect("seed log");
        let service = FileService::new(config_service);

        let content = service
            .get_file("logs/build.log")
            .await
            .expect("file inside the static dir is served");
        assert_eq!(content, b"build ok");

        let missing = service
            .get_file("logs/missing.log")
            .await
            .expect_err("missing file");
        assert_eq!(missing.kind(), ErrorKind::NotFound);

        #[cfg(unix)]
        {
            let outside = data_dir.path().join("secret.txt");
            std::fs::write(&outside, b"secret").expect("seed outside file");
            std::os::unix::fs::symlink(&outside, static_dir.join("escape"))
                .expect("symlink out of the static dir");

            let escape = service
                .get_file("escape")
                .await
                .expect_err("a symlink out of the static dir is refused");
            assert_eq!(escape.kind(), ErrorKind::PermissionDenied);
        }
    }
}
