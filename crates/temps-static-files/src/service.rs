//! File Service
//!
//! Service for reading files from the static directory

use std::sync::Arc;
use tokio::fs;
use tracing::debug;

#[derive(Clone)]
pub struct FileService {
    config_service: Arc<temps_config::ConfigService>,
}

impl FileService {
    pub fn new(config_service: Arc<temps_config::ConfigService>) -> Self {
        Self { config_service }
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
