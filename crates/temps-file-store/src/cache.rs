// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! A byte-level, size-bounded cache decorator over any [`FileStore`].
//!
//! # Why this exists
//!
//! `temps-proxy` reads deployment assets (static-site files and CAS blobs)
//! on every single incoming HTTP request. That is safe against local disk
//! because a filesystem read is cheap and local. It is **not** safe against
//! an S3-compatible backend unmodified: every warm request would otherwise
//! pay a network round trip, turning a proxy hot path into one bounded by S3
//! latency. This decorator sits between the proxy and an `Arc<dyn FileStore>`
//! (of either backend) so a warmed key is served straight from memory.
//!
//! # Why no TTL is safe here
//!
//! All three key namespaces this wraps are immutable once written:
//! - CAS blobs (`put_blob`) are keyed by content hash — the same key can
//!   never resolve to different bytes (see `temps-deployments`'
//!   `persist_static_assets` job, which is the only writer).
//! - `path`-keyed entries (`put`/`get`/`open`) are this store's own
//!   sanitized sub-namespace, not currently written by any production
//!   caller.
//! - `open_raw` keys are exact object keys owned by an external writer —
//!   for the proxy's static-site read path, `S3StaticDeployer` — always
//!   written under a fresh, unique deployment slug + date partition (see
//!   `StaticDeployer::deploy`), so an existing key is never overwritten with
//!   different content either.
//!
//! A plain LRU/TinyLFU cache with no expiry is therefore safe — unlike, say,
//! caching container logs, where the same key can grow over time. `delete_blob`
//! and `put` still proactively invalidate their key as defense in depth, in
//! case that invariant is ever violated upstream.
//!
//! # Why size-bounded, not count-bounded
//!
//! These are "big file" caches: a per-project JS bundle or a static HTML
//! page can be single-digit megabytes. A cache bounded only by entry count
//! could still hold enough oversized entries to exhaust memory on a small
//! box. `moka`'s weigher makes capacity a byte budget instead.

use crate::{FileStore, FileStoreError, OpenedBlob};
use async_trait::async_trait;
use bytes::Bytes;
use std::sync::Arc;
use tokio::io::AsyncReadExt;
use tracing::debug;

/// Default byte-cache capacity when `TEMPS_STATIC_BYTE_CACHE_MAX_BYTES` is
/// unset. Applied independently to the CAS and static-site read paths (see
/// `temps-proxy/src/server.rs`), so the worst-case combined footprint is
/// twice this value — documented in the env var's `CLAUDE.md` entry.
pub const DEFAULT_BYTE_CACHE_MAX_BYTES: u64 = 256 * 1024 * 1024;

/// Read `TEMPS_STATIC_BYTE_CACHE_MAX_BYTES`, falling back to
/// [`DEFAULT_BYTE_CACHE_MAX_BYTES`] when unset or not a positive integer.
/// Never fails startup over a malformed value — an operator-tuning knob
/// should degrade to a safe default, not block boot.
pub fn byte_cache_max_bytes_from_env() -> u64 {
    match std::env::var("TEMPS_STATIC_BYTE_CACHE_MAX_BYTES") {
        Ok(value) => match value.trim().parse::<u64>() {
            Ok(bytes) if bytes > 0 => bytes,
            _ => {
                tracing::warn!(
                    value,
                    default_bytes = DEFAULT_BYTE_CACHE_MAX_BYTES,
                    "TEMPS_STATIC_BYTE_CACHE_MAX_BYTES is not a positive integer byte count; \
                     using the default"
                );
                DEFAULT_BYTE_CACHE_MAX_BYTES
            }
        },
        Err(_) => DEFAULT_BYTE_CACHE_MAX_BYTES,
    }
}

pub struct CachingFileStore {
    inner: Arc<dyn FileStore>,
    cache: moka::future::Cache<String, Bytes>,
    /// A single entry is never cached above this size, so one oversized
    /// object cannot evict the entire warm set. Set to 10% of the total
    /// budget (never zero, even for a very small configured cache).
    max_cacheable_entry_bytes: u64,
}

impl CachingFileStore {
    /// `max_total_bytes` is a byte budget, not an entry count — see the
    /// module doc for why that matters for "big file" content like this.
    pub fn new(inner: Arc<dyn FileStore>, max_total_bytes: u64) -> Self {
        let cache = moka::future::Cache::builder()
            .max_capacity(max_total_bytes)
            .weigher(|_key: &String, value: &Bytes| -> u32 {
                u32::try_from(value.len()).unwrap_or(u32::MAX)
            })
            .build();
        Self {
            inner,
            cache,
            max_cacheable_entry_bytes: (max_total_bytes / 10).max(1),
        }
    }

    fn blob_cache_key(hash: &str) -> String {
        format!("blob:{hash}")
    }

    fn path_cache_key(path: &str) -> String {
        format!("path:{path}")
    }

    /// Distinct from [`Self::path_cache_key`]: `open_raw` keys are exact
    /// object keys owned by an external writer (e.g. `S3StaticDeployer`),
    /// not sanitized into the `path:`/`paths/` sub-namespace, so they get
    /// their own cache-key prefix to avoid ever colliding with one.
    fn raw_cache_key(key: &str) -> String {
        format!("raw:{key}")
    }

    /// Buffer a streamed `OpenedBlob` into memory and cache it under `key`,
    /// but only when it fits under `max_cacheable_entry_bytes` — otherwise
    /// return the original streaming reader untouched, so a very large asset
    /// is served with exactly today's bounded, chunked-streaming behavior
    /// instead of being fully buffered per request.
    async fn cache_or_passthrough(
        &self,
        key: String,
        log_path: &str,
        opened: OpenedBlob,
    ) -> Result<OpenedBlob, FileStoreError> {
        if opened.size_bytes > self.max_cacheable_entry_bytes {
            return Ok(opened);
        }

        let mut buffer = Vec::with_capacity(opened.size_bytes as usize);
        let mut reader = opened.reader;
        reader
            .read_to_end(&mut buffer)
            .await
            .map_err(|error| FileStoreError::Io {
                path: log_path.to_string(),
                reason: format!("buffering for byte cache: {error}"),
            })?;
        let bytes = Bytes::from(buffer);
        let size_bytes = bytes.len() as u64;

        // A backend that streamed a size different from what it now produced
        // is not something to memoize — serve what was actually read, but
        // skip the insert so a future request re-fetches from the source of
        // truth instead of trusting a mismatched snapshot.
        if size_bytes == opened.size_bytes {
            self.cache.insert(key, bytes.clone()).await;
        }

        Ok(OpenedBlob {
            reader: Box::new(std::io::Cursor::new(bytes)),
            size_bytes,
        })
    }
}

#[async_trait]
impl FileStore for CachingFileStore {
    async fn put_blob(&self, data: Bytes) -> Result<String, FileStoreError> {
        // Content-addressed: an existing key never changes, so there is
        // nothing to invalidate. The cache is warmed lazily on next read.
        self.inner.put_blob(data).await
    }

    async fn get_blob(&self, hash: &str) -> Result<Bytes, FileStoreError> {
        let key = Self::blob_cache_key(hash);
        if let Some(bytes) = self.cache.get(&key).await {
            debug!(
                hash_prefix = hash.get(..8).unwrap_or(hash),
                "byte cache hit (get_blob)"
            );
            return Ok(bytes);
        }
        let bytes = self.inner.get_blob(hash).await?;
        if bytes.len() as u64 <= self.max_cacheable_entry_bytes {
            self.cache.insert(key, bytes.clone()).await;
        }
        Ok(bytes)
    }

    async fn open_blob(&self, hash: &str) -> Result<OpenedBlob, FileStoreError> {
        let key = Self::blob_cache_key(hash);
        if let Some(bytes) = self.cache.get(&key).await {
            debug!(
                hash_prefix = hash.get(..8).unwrap_or(hash),
                "byte cache hit (open_blob)"
            );
            return Ok(OpenedBlob {
                size_bytes: bytes.len() as u64,
                reader: Box::new(std::io::Cursor::new(bytes)),
            });
        }
        let opened = self.inner.open_blob(hash).await?;
        self.cache_or_passthrough(key, hash, opened).await
    }

    async fn blob_exists(&self, hash: &str) -> Result<bool, FileStoreError> {
        if self.cache.contains_key(&Self::blob_cache_key(hash)) {
            return Ok(true);
        }
        self.inner.blob_exists(hash).await
    }

    async fn delete_blob(&self, hash: &str) -> Result<bool, FileStoreError> {
        let existed = self.inner.delete_blob(hash).await?;
        self.cache.invalidate(&Self::blob_cache_key(hash)).await;
        Ok(existed)
    }

    async fn put(&self, path: &str, data: Bytes) -> Result<u64, FileStoreError> {
        let result = self.inner.put(path, data).await?;
        // Path keys are documented immutable-once-written (see module doc),
        // but a `put` targeting an already-cached key is exactly the
        // scenario that invariant depends on holding — invalidate rather
        // than trust it blindly.
        self.cache.invalidate(&Self::path_cache_key(path)).await;
        Ok(result)
    }

    async fn get(&self, path: &str) -> Result<Bytes, FileStoreError> {
        let key = Self::path_cache_key(path);
        if let Some(bytes) = self.cache.get(&key).await {
            debug!(path, "byte cache hit (get)");
            return Ok(bytes);
        }
        let bytes = self.inner.get(path).await?;
        if bytes.len() as u64 <= self.max_cacheable_entry_bytes {
            self.cache.insert(key, bytes.clone()).await;
        }
        Ok(bytes)
    }

    async fn open(&self, path: &str) -> Result<OpenedBlob, FileStoreError> {
        let key = Self::path_cache_key(path);
        if let Some(bytes) = self.cache.get(&key).await {
            debug!(path, "byte cache hit (open)");
            return Ok(OpenedBlob {
                size_bytes: bytes.len() as u64,
                reader: Box::new(std::io::Cursor::new(bytes)),
            });
        }
        let opened = self.inner.open(path).await?;
        self.cache_or_passthrough(key, path, opened).await
    }

    async fn exists(&self, path: &str) -> Result<bool, FileStoreError> {
        if self.cache.contains_key(&Self::path_cache_key(path)) {
            return Ok(true);
        }
        self.inner.exists(path).await
    }

    async fn open_raw(&self, key: &str) -> Result<OpenedBlob, FileStoreError> {
        let cache_key = Self::raw_cache_key(key);
        if let Some(bytes) = self.cache.get(&cache_key).await {
            debug!(key, "byte cache hit (open_raw)");
            return Ok(OpenedBlob {
                size_bytes: bytes.len() as u64,
                reader: Box::new(std::io::Cursor::new(bytes)),
            });
        }
        let opened = self.inner.open_raw(key).await?;
        self.cache_or_passthrough(cache_key, key, opened).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serial_test::serial;
    use std::collections::HashMap;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use tokio::sync::Mutex;

    /// An in-memory `FileStore` that counts calls to its streaming read
    /// methods, so tests can assert the decorator actually skips the inner
    /// store on a cache hit — not just that it returns the right bytes.
    #[derive(Default)]
    struct CountingStore {
        blobs: Mutex<HashMap<String, Bytes>>,
        paths: Mutex<HashMap<String, Bytes>>,
        raws: Mutex<HashMap<String, Bytes>>,
        open_blob_calls: AtomicUsize,
        open_calls: AtomicUsize,
        open_raw_calls: AtomicUsize,
    }

    impl CountingStore {
        fn new() -> Self {
            Self::default()
        }

        /// `open_raw` keys are written by something outside the `FileStore`
        /// trait (e.g. `S3StaticDeployer`'s own S3 client), so there is no
        /// `put_raw` to seed through — tests insert directly instead.
        async fn seed_raw(&self, key: &str, data: Bytes) {
            self.raws.lock().await.insert(key.to_string(), data);
        }
    }

    #[async_trait]
    impl FileStore for CountingStore {
        async fn put_blob(&self, data: Bytes) -> Result<String, FileStoreError> {
            let hash = crate::fs_store::FsFileStore::content_hash(&data);
            self.blobs.lock().await.insert(hash.clone(), data);
            Ok(hash)
        }

        async fn get_blob(&self, hash: &str) -> Result<Bytes, FileStoreError> {
            self.blobs
                .lock()
                .await
                .get(hash)
                .cloned()
                .ok_or_else(|| FileStoreError::NotFound {
                    path: hash.to_string(),
                })
        }

        async fn open_blob(&self, hash: &str) -> Result<OpenedBlob, FileStoreError> {
            self.open_blob_calls.fetch_add(1, Ordering::SeqCst);
            let data = self.get_blob(hash).await?;
            Ok(OpenedBlob {
                size_bytes: data.len() as u64,
                reader: Box::new(std::io::Cursor::new(data)),
            })
        }

        async fn blob_exists(&self, hash: &str) -> Result<bool, FileStoreError> {
            Ok(self.blobs.lock().await.contains_key(hash))
        }

        async fn delete_blob(&self, hash: &str) -> Result<bool, FileStoreError> {
            Ok(self.blobs.lock().await.remove(hash).is_some())
        }

        async fn put(&self, path: &str, data: Bytes) -> Result<u64, FileStoreError> {
            let size = data.len() as u64;
            self.paths.lock().await.insert(path.to_string(), data);
            Ok(size)
        }

        async fn get(&self, path: &str) -> Result<Bytes, FileStoreError> {
            self.paths
                .lock()
                .await
                .get(path)
                .cloned()
                .ok_or_else(|| FileStoreError::NotFound {
                    path: path.to_string(),
                })
        }

        async fn open(&self, path: &str) -> Result<OpenedBlob, FileStoreError> {
            self.open_calls.fetch_add(1, Ordering::SeqCst);
            let data = self.get(path).await?;
            Ok(OpenedBlob {
                size_bytes: data.len() as u64,
                reader: Box::new(std::io::Cursor::new(data)),
            })
        }

        async fn exists(&self, path: &str) -> Result<bool, FileStoreError> {
            Ok(self.paths.lock().await.contains_key(path))
        }

        async fn open_raw(&self, key: &str) -> Result<OpenedBlob, FileStoreError> {
            self.open_raw_calls.fetch_add(1, Ordering::SeqCst);
            let data = self.raws.lock().await.get(key).cloned().ok_or_else(|| {
                FileStoreError::NotFound {
                    path: key.to_string(),
                }
            })?;
            Ok(OpenedBlob {
                size_bytes: data.len() as u64,
                reader: Box::new(std::io::Cursor::new(data)),
            })
        }
    }

    async fn read_all(mut opened: OpenedBlob) -> Bytes {
        let mut buffer = Vec::new();
        opened.reader.read_to_end(&mut buffer).await.unwrap();
        Bytes::from(buffer)
    }

    #[tokio::test]
    async fn warm_blob_read_never_touches_the_inner_store() {
        let inner = Arc::new(CountingStore::new());
        let cache = CachingFileStore::new(inner.clone(), 1024 * 1024);
        let hash = inner
            .put_blob(Bytes::from_static(b"warm me"))
            .await
            .unwrap();

        let cold = cache.open_blob(&hash).await.unwrap();
        assert_eq!(read_all(cold).await, Bytes::from_static(b"warm me"));
        assert_eq!(inner.open_blob_calls.load(Ordering::SeqCst), 1);

        let warm = cache.open_blob(&hash).await.unwrap();
        assert_eq!(read_all(warm).await, Bytes::from_static(b"warm me"));
        // Still 1: the second open_blob was served entirely from the cache.
        assert_eq!(inner.open_blob_calls.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn warm_path_read_never_touches_the_inner_store() {
        let inner = Arc::new(CountingStore::new());
        let cache = CachingFileStore::new(inner.clone(), 1024 * 1024);
        cache
            .put(
                "projects/site/prod/deploy-1/index.html",
                Bytes::from_static(b"<html/>"),
            )
            .await
            .unwrap();
        // `put` invalidates defensively; the very first read after a write
        // is expected to be a cold fetch from the inner store.
        let cold = cache
            .open("projects/site/prod/deploy-1/index.html")
            .await
            .unwrap();
        assert_eq!(read_all(cold).await, Bytes::from_static(b"<html/>"));
        assert_eq!(inner.open_calls.load(Ordering::SeqCst), 1);

        let warm = cache
            .open("projects/site/prod/deploy-1/index.html")
            .await
            .unwrap();
        assert_eq!(read_all(warm).await, Bytes::from_static(b"<html/>"));
        assert_eq!(inner.open_calls.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn warm_raw_read_never_touches_the_inner_store() {
        // Exercises the exact code path the proxy's S3-backed static-site
        // read path uses (`serve_static_file_from_store` calling
        // `store.open_raw(..)`), as opposed to the `path:`-namespaced
        // `open`/`put` pair above which no production caller uses today.
        let inner = Arc::new(CountingStore::new());
        let cache = CachingFileStore::new(inner.clone(), 1024 * 1024);
        let key = "projects/site/production/2026/01/01/deploy-1/index.html";
        inner.seed_raw(key, Bytes::from_static(b"<html/>")).await;

        let cold = cache.open_raw(key).await.unwrap();
        assert_eq!(read_all(cold).await, Bytes::from_static(b"<html/>"));
        assert_eq!(inner.open_raw_calls.load(Ordering::SeqCst), 1);

        let warm = cache.open_raw(key).await.unwrap();
        assert_eq!(read_all(warm).await, Bytes::from_static(b"<html/>"));
        // Still 1: the second open_raw was served entirely from the cache.
        assert_eq!(inner.open_raw_calls.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn raw_and_path_cache_keys_never_collide_for_the_same_string() {
        // `open` and `open_raw` are different namespaces even when called
        // with the exact same key string — a path-keyed write must never be
        // servable as a raw read or vice versa.
        let inner = Arc::new(CountingStore::new());
        let cache = CachingFileStore::new(inner.clone(), 1024 * 1024);
        let key = "same/key.txt";
        inner.seed_raw(key, Bytes::from_static(b"raw bytes")).await;

        // Warm the raw-key cache entry.
        let raw = cache.open_raw(key).await.unwrap();
        assert_eq!(read_all(raw).await, Bytes::from_static(b"raw bytes"));

        // A path-keyed open for the identical string must still miss (the
        // inner store has nothing under `paths`) rather than incorrectly
        // returning the raw-cached bytes.
        let path_result = cache.open(key).await;
        assert!(matches!(path_result, Err(FileStoreError::NotFound { .. })));
    }

    #[tokio::test]
    async fn oversized_entry_bypasses_the_cache_but_still_serves_correctly() {
        let inner = Arc::new(CountingStore::new());
        // Total budget 100 bytes => max cacheable single entry is 10 bytes.
        let cache = CachingFileStore::new(inner.clone(), 100);
        let hash = inner.put_blob(Bytes::from(vec![b'x'; 50])).await.unwrap();

        let first = cache.open_blob(&hash).await.unwrap();
        assert_eq!(read_all(first).await.len(), 50);
        let second = cache.open_blob(&hash).await.unwrap();
        assert_eq!(read_all(second).await.len(), 50);

        // Never cached: every read hit the inner store.
        assert_eq!(inner.open_blob_calls.load(Ordering::SeqCst), 2);
    }

    #[tokio::test]
    async fn delete_blob_invalidates_a_warmed_cache_entry() {
        let inner = Arc::new(CountingStore::new());
        let cache = CachingFileStore::new(inner.clone(), 1024 * 1024);
        let hash = inner
            .put_blob(Bytes::from_static(b"to delete"))
            .await
            .unwrap();
        let _ = cache.open_blob(&hash).await.unwrap(); // warm the cache
        assert_eq!(inner.open_blob_calls.load(Ordering::SeqCst), 1);

        assert!(cache.delete_blob(&hash).await.unwrap());
        let result = cache.get_blob(&hash).await;
        assert!(matches!(result, Err(FileStoreError::NotFound { .. })));
    }

    #[test]
    #[serial(temps_static_byte_cache_env)]
    fn env_default_applies_when_unset_or_invalid() {
        std::env::remove_var("TEMPS_STATIC_BYTE_CACHE_MAX_BYTES");
        assert_eq!(
            byte_cache_max_bytes_from_env(),
            DEFAULT_BYTE_CACHE_MAX_BYTES
        );

        std::env::set_var("TEMPS_STATIC_BYTE_CACHE_MAX_BYTES", "not-a-number");
        assert_eq!(
            byte_cache_max_bytes_from_env(),
            DEFAULT_BYTE_CACHE_MAX_BYTES
        );

        std::env::set_var("TEMPS_STATIC_BYTE_CACHE_MAX_BYTES", "0");
        assert_eq!(
            byte_cache_max_bytes_from_env(),
            DEFAULT_BYTE_CACHE_MAX_BYTES
        );

        std::env::set_var("TEMPS_STATIC_BYTE_CACHE_MAX_BYTES", "1048576");
        assert_eq!(byte_cache_max_bytes_from_env(), 1_048_576);

        std::env::remove_var("TEMPS_STATIC_BYTE_CACHE_MAX_BYTES");
    }
}
