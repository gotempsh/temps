// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

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

use crate::{FileStore, FileStoreError, OpenedBlob};

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
    fn blob_path(&self, hash: &str) -> Result<PathBuf, FileStoreError> {
        validate_content_hash(hash)?;
        Ok(self
            .root
            .join("blobs")
            .join(&hash[..2])
            .join(&hash[2..4])
            .join(hash))
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
        let blob = self.blob_path(&hash)?;

        if !blob.exists() {
            self.atomic_write(&blob, &data).await?;
            debug!(
                "CAS: stored blob {} ({} bytes)",
                hash.get(..8).unwrap_or(hash.as_str()),
                data.len()
            );
        } else {
            debug!(
                "CAS: dedup hit {} ({} bytes saved)",
                hash.get(..8).unwrap_or(hash.as_str()),
                data.len()
            );
        }

        Ok(hash)
    }

    async fn get_blob(&self, hash: &str) -> Result<Bytes, FileStoreError> {
        let blob = self.blob_path(hash)?;
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

    async fn open_blob(&self, hash: &str) -> Result<OpenedBlob, FileStoreError> {
        let blob = self.blob_path(hash)?;
        let file = tokio::fs::File::open(&blob).await.map_err(|error| {
            if error.kind() == std::io::ErrorKind::NotFound {
                FileStoreError::NotFound {
                    path: hash.to_string(),
                }
            } else {
                FileStoreError::Io {
                    path: hash.to_string(),
                    reason: format!("open blob: {error}"),
                }
            }
        })?;
        let metadata = file.metadata().await.map_err(|error| FileStoreError::Io {
            path: hash.to_string(),
            reason: format!("read opened blob metadata: {error}"),
        })?;
        if !metadata.is_file() {
            return Err(FileStoreError::Io {
                path: hash.to_string(),
                reason: "opened blob is not a regular file".to_string(),
            });
        }
        Ok(OpenedBlob {
            reader: Box::new(file),
            size_bytes: metadata.len(),
        })
    }

    async fn blob_exists(&self, hash: &str) -> Result<bool, FileStoreError> {
        let blob = self.blob_path(hash)?;
        tokio::fs::try_exists(&blob)
            .await
            .map_err(|error| FileStoreError::Io {
                path: hash.to_string(),
                reason: format!("check blob existence: {error}"),
            })
    }

    async fn delete_blob(&self, hash: &str) -> Result<bool, FileStoreError> {
        let blob = self.blob_path(hash)?;
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

fn validate_content_hash(hash: &str) -> Result<(), FileStoreError> {
    if hash.len() == 64 && hash.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return Ok(());
    }
    Err(FileStoreError::InvalidHash { length: hash.len() })
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::AsyncReadExt;

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

        let mut opened = store.open_blob(&hash).await.unwrap();
        assert_eq!(opened.size_bytes, data.len() as u64);
        let mut streamed = Vec::new();
        opened.reader.read_to_end(&mut streamed).await.unwrap();
        assert_eq!(streamed, data);
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
        assert!(!store.blob_exists(&"0".repeat(64)).await.unwrap());
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
        let result = store.get_blob(&"0".repeat(64)).await;
        assert!(matches!(result, Err(FileStoreError::NotFound { .. })));
    }

    #[tokio::test]
    async fn invalid_hashes_are_rejected_by_every_cas_blob_operation() {
        let (_dir, store) = temp_store();
        let invalid_hashes = [
            "/absolute/path".to_string(),
            format!("../{}", "a".repeat(61)),
            "abcd".to_string(),
            "z".repeat(64),
            "secret".repeat(10_000),
        ];

        for hash in invalid_hashes {
            assert!(matches!(
                store.get_blob(&hash).await,
                Err(FileStoreError::InvalidHash { .. })
            ));
            assert!(matches!(
                store.open_blob(&hash).await,
                Err(FileStoreError::InvalidHash { .. })
            ));
            assert!(matches!(
                store.blob_exists(&hash).await,
                Err(FileStoreError::InvalidHash { .. })
            ));
            assert!(matches!(
                store.delete_blob(&hash).await,
                Err(FileStoreError::InvalidHash { .. })
            ));
        }
    }

    #[test]
    fn invalid_hash_errors_do_not_echo_untrusted_values() {
        let untrusted = "private-value".repeat(1_000);
        let error = validate_content_hash(&untrusted).expect_err("hash must be rejected");
        let rendered = error.to_string();

        assert!(rendered.contains(&untrusted.len().to_string()));
        assert!(!rendered.contains("private-value"));
    }

    #[tokio::test]
    async fn test_double_prefix_sharding() {
        let (_dir, store) = temp_store();
        let hash = store.put_blob(Bytes::from("shard test")).await.unwrap();

        // Verify blob is at blobs/{hash[0..2]}/{hash[2..4]}/{hash}
        let blob = store.blob_path(&hash).unwrap();
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
