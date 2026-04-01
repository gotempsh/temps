//! Filesystem storage backend for log chunks
//!
//! Stores chunks under a configurable base path with the same directory structure
//! as S3: `logs/{project_id}/{service}/{YYYY-MM-DD}/{HH}/{container_id}-{sequence}.ndjson.zst`

use std::path::{Path, PathBuf};

use async_trait::async_trait;
use tracing::debug;

use crate::error::LogAggregatorError;
use crate::storage::traits::LogStorage;

/// Filesystem-based log chunk storage.
///
/// Suitable for self-hosted or development environments. Stores compressed NDJSON
/// chunks with an identical directory structure to the S3 backend.
pub struct FilesystemStorage {
    base_path: PathBuf,
}

impl FilesystemStorage {
    /// Create a new filesystem storage backend.
    ///
    /// The `base_path` directory will be created if it does not exist.
    pub fn new(base_path: PathBuf) -> Result<Self, LogAggregatorError> {
        if !base_path.exists() {
            std::fs::create_dir_all(&base_path).map_err(|e| {
                LogAggregatorError::StorageConfiguration {
                    message: format!(
                        "Failed to create storage directory '{}': {}",
                        base_path.display(),
                        e
                    ),
                }
            })?;
        }
        debug!(
            "Filesystem log storage initialized at: {}",
            base_path.display()
        );
        Ok(Self { base_path })
    }

    /// Resolve a storage key to an absolute filesystem path.
    ///
    /// Rejects keys containing path traversal components (`..`) to prevent
    /// escaping the base directory.
    fn resolve_path(&self, key: &str) -> Result<PathBuf, LogAggregatorError> {
        // Reject path traversal attempts
        if key.contains("..") {
            return Err(LogAggregatorError::StorageConfiguration {
                message: format!("Invalid storage key (path traversal rejected): {}", key),
            });
        }

        let resolved = self.base_path.join(key);

        // Double-check the resolved path stays within base_path
        // (handles edge cases like symlinks if base_path is canonical)
        if !resolved.starts_with(&self.base_path) {
            return Err(LogAggregatorError::StorageConfiguration {
                message: format!(
                    "Storage key '{}' resolves outside base path '{}'",
                    key,
                    self.base_path.display()
                ),
            });
        }

        Ok(resolved)
    }

    /// Ensure the parent directory of a file path exists.
    async fn ensure_parent_dir(path: &Path) -> Result<(), LogAggregatorError> {
        if let Some(parent) = path.parent() {
            tokio::fs::create_dir_all(parent).await.map_err(|e| {
                LogAggregatorError::StorageConfiguration {
                    message: format!("Failed to create directory '{}': {}", parent.display(), e),
                }
            })?;
        }
        Ok(())
    }
}

#[async_trait]
impl LogStorage for FilesystemStorage {
    async fn write_chunk(&self, key: &str, data: &[u8]) -> Result<u64, LogAggregatorError> {
        let path = self.resolve_path(key)?;
        Self::ensure_parent_dir(&path).await?;

        tokio::fs::write(&path, data)
            .await
            .map_err(|e| LogAggregatorError::ChunkWriteFailed {
                chunk_id: uuid::Uuid::nil(),
                project_id: 0,
                service: String::new(),
                reason: format!("Failed to write to '{}': {}", path.display(), e),
            })?;

        debug!(key = key, bytes = data.len(), "Wrote chunk to filesystem");
        Ok(data.len() as u64)
    }

    async fn read_chunk(&self, key: &str) -> Result<Vec<u8>, LogAggregatorError> {
        let path = self.resolve_path(key)?;
        tokio::fs::read(&path).await.map_err(|e| {
            if e.kind() == std::io::ErrorKind::NotFound {
                LogAggregatorError::ChunkNotFound {
                    chunk_id: uuid::Uuid::nil(),
                    storage_key: key.to_string(),
                }
            } else {
                LogAggregatorError::ChunkReadFailed {
                    chunk_id: uuid::Uuid::nil(),
                    storage_key: key.to_string(),
                    reason: e.to_string(),
                }
            }
        })
    }

    async fn read_chunk_range(
        &self,
        key: &str,
        start: u64,
        end: Option<u64>,
    ) -> Result<Vec<u8>, LogAggregatorError> {
        let data = self.read_chunk(key).await?;
        let start = start as usize;
        let end = end.map(|e| e as usize).unwrap_or(data.len());
        let end = std::cmp::min(end, data.len());

        if start >= data.len() {
            return Ok(Vec::new());
        }

        Ok(data[start..end].to_vec())
    }

    async fn list_chunks(&self, prefix: &str) -> Result<Vec<String>, LogAggregatorError> {
        let dir = self.resolve_path(prefix)?;
        if !dir.exists() {
            return Ok(Vec::new());
        }

        let mut keys = Vec::new();
        let mut stack = vec![dir];

        while let Some(current_dir) = stack.pop() {
            let mut entries = tokio::fs::read_dir(&current_dir).await.map_err(|e| {
                LogAggregatorError::ChunkListFailed {
                    project_id: 0,
                    service: String::new(),
                    reason: format!(
                        "Failed to read directory '{}': {}",
                        current_dir.display(),
                        e
                    ),
                }
            })?;

            while let Some(entry) =
                entries
                    .next_entry()
                    .await
                    .map_err(|e| LogAggregatorError::ChunkListFailed {
                        project_id: 0,
                        service: String::new(),
                        reason: format!("Failed to read directory entry: {}", e),
                    })?
            {
                let path = entry.path();
                if path.is_dir() {
                    stack.push(path);
                } else if path.extension().map(|ext| ext == "zst").unwrap_or(false) {
                    // Strip base_path prefix to get the relative storage key
                    if let Ok(relative) = path.strip_prefix(&self.base_path) {
                        keys.push(relative.to_string_lossy().to_string());
                    }
                }
            }
        }

        keys.sort();
        Ok(keys)
    }

    async fn delete_chunk(&self, key: &str) -> Result<(), LogAggregatorError> {
        let path = self.resolve_path(key)?;
        match tokio::fs::remove_file(&path).await {
            Ok(()) => {
                debug!(key = key, "Deleted chunk from filesystem");
                Ok(())
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                // Idempotent: not an error if already deleted
                Ok(())
            }
            Err(e) => Err(LogAggregatorError::ChunkDeleteFailed {
                chunk_id: uuid::Uuid::nil(),
                storage_key: key.to_string(),
                reason: e.to_string(),
            }),
        }
    }

    async fn chunk_exists(&self, key: &str) -> Result<bool, LogAggregatorError> {
        let path = self.resolve_path(key)?;
        Ok(path.exists())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_write_and_read_chunk() {
        let tmp = tempfile::tempdir().unwrap();
        let storage = FilesystemStorage::new(tmp.path().to_path_buf()).unwrap();

        let key = "logs/test-project/web/2026-02-25/14/abc123-000001.ndjson.zst";
        let data = b"compressed test data";

        let written = storage.write_chunk(key, data).await.unwrap();
        assert_eq!(written, data.len() as u64);

        let read_data = storage.read_chunk(key).await.unwrap();
        assert_eq!(read_data, data);
    }

    #[tokio::test]
    async fn test_read_nonexistent_chunk_returns_not_found() {
        let tmp = tempfile::tempdir().unwrap();
        let storage = FilesystemStorage::new(tmp.path().to_path_buf()).unwrap();

        let result = storage.read_chunk("nonexistent/key.ndjson.zst").await;
        assert!(matches!(
            result,
            Err(LogAggregatorError::ChunkNotFound { .. })
        ));
    }

    #[tokio::test]
    async fn test_delete_chunk_idempotent() {
        let tmp = tempfile::tempdir().unwrap();
        let storage = FilesystemStorage::new(tmp.path().to_path_buf()).unwrap();

        // Deleting non-existent key should not error
        storage
            .delete_chunk("nonexistent/key.ndjson.zst")
            .await
            .unwrap();
    }

    #[tokio::test]
    async fn test_write_delete_verify() {
        let tmp = tempfile::tempdir().unwrap();
        let storage = FilesystemStorage::new(tmp.path().to_path_buf()).unwrap();

        let key = "logs/proj/svc/2026-01-01/00/cnt-000001.ndjson.zst";
        storage.write_chunk(key, b"data").await.unwrap();
        assert!(storage.chunk_exists(key).await.unwrap());

        storage.delete_chunk(key).await.unwrap();
        assert!(!storage.chunk_exists(key).await.unwrap());
    }

    #[tokio::test]
    async fn test_list_chunks() {
        let tmp = tempfile::tempdir().unwrap();
        let storage = FilesystemStorage::new(tmp.path().to_path_buf()).unwrap();

        let keys = [
            "logs/proj/web/2026-01-01/00/cnt-000001.ndjson.zst",
            "logs/proj/web/2026-01-01/00/cnt-000002.ndjson.zst",
            "logs/proj/web/2026-01-01/01/cnt-000001.ndjson.zst",
            "logs/proj/api/2026-01-01/00/cnt-000001.ndjson.zst",
        ];

        for key in &keys {
            storage.write_chunk(key, b"data").await.unwrap();
        }

        let web_chunks = storage.list_chunks("logs/proj/web").await.unwrap();
        assert_eq!(web_chunks.len(), 3);

        let all_chunks = storage.list_chunks("logs/proj").await.unwrap();
        assert_eq!(all_chunks.len(), 4);

        let api_chunks = storage.list_chunks("logs/proj/api").await.unwrap();
        assert_eq!(api_chunks.len(), 1);
    }

    #[tokio::test]
    async fn test_read_chunk_range() {
        let tmp = tempfile::tempdir().unwrap();
        let storage = FilesystemStorage::new(tmp.path().to_path_buf()).unwrap();

        let key = "logs/proj/web/2026-01-01/00/cnt-000001.ndjson.zst";
        let data = b"0123456789";
        storage.write_chunk(key, data).await.unwrap();

        let range = storage.read_chunk_range(key, 3, Some(7)).await.unwrap();
        assert_eq!(range, b"3456");

        let range_to_end = storage.read_chunk_range(key, 5, None).await.unwrap();
        assert_eq!(range_to_end, b"56789");
    }

    #[tokio::test]
    async fn test_list_empty_prefix() {
        let tmp = tempfile::tempdir().unwrap();
        let storage = FilesystemStorage::new(tmp.path().to_path_buf()).unwrap();

        let keys = storage
            .list_chunks("logs/nonexistent-project")
            .await
            .unwrap();
        assert!(keys.is_empty());
    }
}
