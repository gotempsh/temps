//! Content-addressable filesystem blob store.
//!
//! Layout (git-style double-prefix sharding):
//! ```text
//! {root}/
//!   blobs/{hash[0..2]}/{hash[2..4]}/{hash}
//!   .tmp/
//! ```
//!
//! With 65,536 prefix buckets (256×256), even 1M blobs averages ~15 files per directory.

use async_trait::async_trait;
use bytes::Bytes;
use sha2::{Digest, Sha256};
use std::path::PathBuf;
use tracing::debug;

use crate::{FileStore, FileStoreError};

pub struct FsFileStore {
    root: PathBuf,
}

impl FsFileStore {
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self { root: root.into() }
    }

    pub fn root(&self) -> &std::path::Path {
        &self.root
    }

    /// SHA-256 hex digest.
    pub fn content_hash(data: &[u8]) -> String {
        let mut hasher = Sha256::new();
        hasher.update(data);
        hex::encode(hasher.finalize())
    }

    /// Git-style double-prefix sharding: `blobs/{hash[0..2]}/{hash[2..4]}/{hash}`
    fn blob_path(&self, hash: &str) -> PathBuf {
        let h = hash.trim();
        let p1 = &h[..2.min(h.len())];
        let p2 = if h.len() >= 4 { &h[2..4] } else { "00" };
        self.root.join("blobs").join(p1).join(p2).join(h)
    }

    /// Path-based cache: `cache/{sanitized_path}` (for edge caching, not CAS)
    fn cache_path(&self, url_path: &str) -> PathBuf {
        let clean: PathBuf = url_path
            .trim_start_matches('/')
            .split('/')
            .filter(|seg| !seg.is_empty() && *seg != ".." && *seg != ".")
            .collect();
        self.root.join("cache").join(clean)
    }

    fn tmp_dir(&self) -> PathBuf {
        self.root.join(".tmp")
    }

    async fn atomic_write(
        &self,
        target: &std::path::Path,
        data: &[u8],
    ) -> Result<(), FileStoreError> {
        if let Some(parent) = target.parent() {
            tokio::fs::create_dir_all(parent)
                .await
                .map_err(|e| FileStoreError::Io {
                    path: target.display().to_string(),
                    reason: format!("mkdir: {}", e),
                })?;
        }

        let tmp = self.tmp_dir();
        tokio::fs::create_dir_all(&tmp)
            .await
            .map_err(|e| FileStoreError::Io {
                path: tmp.display().to_string(),
                reason: format!("mkdir tmp: {}", e),
            })?;

        let tmp_file = tmp.join(uuid::Uuid::new_v4().to_string());
        tokio::fs::write(&tmp_file, data)
            .await
            .map_err(|e| FileStoreError::Io {
                path: tmp_file.display().to_string(),
                reason: format!("write tmp: {}", e),
            })?;

        if let Err(e) = tokio::fs::rename(&tmp_file, target).await {
            let _ = tokio::fs::remove_file(&tmp_file).await;
            return Err(FileStoreError::Io {
                path: target.display().to_string(),
                reason: format!("rename: {}", e),
            });
        }

        Ok(())
    }
}

#[async_trait]
impl FileStore for FsFileStore {
    async fn put_blob(&self, data: Bytes) -> Result<String, FileStoreError> {
        let hash = Self::content_hash(&data);
        let blob = self.blob_path(&hash);

        if !blob.exists() {
            self.atomic_write(&blob, &data).await?;
            debug!("CAS: stored blob {} ({} bytes)", &hash[..8], data.len());
        } else {
            debug!("CAS: dedup hit {} ({} bytes saved)", &hash[..8], data.len());
        }

        Ok(hash)
    }

    async fn get_blob(&self, hash: &str) -> Result<Bytes, FileStoreError> {
        let blob = self.blob_path(hash);
        let data = tokio::fs::read(&blob).await.map_err(|e| {
            if e.kind() == std::io::ErrorKind::NotFound {
                FileStoreError::NotFound {
                    path: hash.to_string(),
                }
            } else {
                FileStoreError::Io {
                    path: hash.to_string(),
                    reason: format!("read blob: {}", e),
                }
            }
        })?;
        Ok(Bytes::from(data))
    }

    async fn blob_exists(&self, hash: &str) -> Result<bool, FileStoreError> {
        Ok(self.blob_path(hash).exists())
    }

    async fn delete_blob(&self, hash: &str) -> Result<bool, FileStoreError> {
        let blob = self.blob_path(hash);
        match tokio::fs::remove_file(&blob).await {
            Ok(()) => Ok(true),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(false),
            Err(e) => Err(FileStoreError::Io {
                path: hash.to_string(),
                reason: format!("delete blob: {}", e),
            }),
        }
    }

    // Path-based key-value methods (for edge caching, not CAS)

    async fn put(&self, path: &str, data: Bytes) -> Result<u64, FileStoreError> {
        let size = data.len() as u64;
        let file_path = self.cache_path(path);
        self.atomic_write(&file_path, &data).await?;
        Ok(size)
    }

    async fn get(&self, path: &str) -> Result<Bytes, FileStoreError> {
        let file_path = self.cache_path(path);
        let data = tokio::fs::read(&file_path).await.map_err(|e| {
            if e.kind() == std::io::ErrorKind::NotFound {
                FileStoreError::NotFound {
                    path: path.to_string(),
                }
            } else {
                FileStoreError::Io {
                    path: path.to_string(),
                    reason: format!("read: {}", e),
                }
            }
        })?;
        Ok(Bytes::from(data))
    }

    async fn exists(&self, path: &str) -> Result<bool, FileStoreError> {
        Ok(self.cache_path(path).exists())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_store() -> (tempfile::TempDir, FsFileStore) {
        let dir = tempfile::tempdir().unwrap();
        let store = FsFileStore::new(dir.path().join("cas"));
        (dir, store)
    }

    #[tokio::test]
    async fn test_put_and_get_blob() {
        let (_dir, store) = temp_store();
        let data = Bytes::from("hello world");

        let hash = store.put_blob(data.clone()).await.unwrap();
        assert_eq!(hash.len(), 64);

        let retrieved = store.get_blob(&hash).await.unwrap();
        assert_eq!(retrieved, data);
    }

    #[tokio::test]
    async fn test_deduplication() {
        let (_dir, store) = temp_store();
        let data = Bytes::from("shared vendor chunk");

        let h1 = store.put_blob(data.clone()).await.unwrap();
        let h2 = store.put_blob(data.clone()).await.unwrap();
        assert_eq!(h1, h2);
    }

    #[tokio::test]
    async fn test_blob_exists() {
        let (_dir, store) = temp_store();
        let hash = store.put_blob(Bytes::from("test")).await.unwrap();

        assert!(store.blob_exists(&hash).await.unwrap());
        assert!(!store.blob_exists("nonexistent").await.unwrap());
    }

    #[tokio::test]
    async fn test_delete_blob() {
        let (_dir, store) = temp_store();
        let hash = store.put_blob(Bytes::from("delete me")).await.unwrap();

        assert!(store.delete_blob(&hash).await.unwrap());
        assert!(!store.blob_exists(&hash).await.unwrap());
        assert!(!store.delete_blob(&hash).await.unwrap()); // already gone
    }

    #[tokio::test]
    async fn test_get_not_found() {
        let (_dir, store) = temp_store();
        let result = store.get_blob("nonexistent").await;
        assert!(matches!(result, Err(FileStoreError::NotFound { .. })));
    }

    #[tokio::test]
    async fn test_double_prefix_sharding() {
        let (_dir, store) = temp_store();
        let hash = store.put_blob(Bytes::from("shard test")).await.unwrap();

        // Verify blob is at blobs/{hash[0..2]}/{hash[2..4]}/{hash}
        let blob = store.blob_path(&hash);
        let components: Vec<_> = blob.components().collect();
        let len = components.len();
        // .../{p1}/{p2}/{hash}
        assert_eq!(
            components[len - 3].as_os_str().to_str().unwrap(),
            &hash[..2]
        );
        assert_eq!(
            components[len - 2].as_os_str().to_str().unwrap(),
            &hash[2..4]
        );
        assert_eq!(components[len - 1].as_os_str().to_str().unwrap(), &hash);
        assert!(blob.exists());
    }

    #[tokio::test]
    async fn test_content_hash_deterministic() {
        let h1 = FsFileStore::content_hash(b"hello");
        let h2 = FsFileStore::content_hash(b"hello");
        let h3 = FsFileStore::content_hash(b"world");
        assert_eq!(h1, h2);
        assert_ne!(h1, h3);
        assert_eq!(h1.len(), 64);
    }
}
