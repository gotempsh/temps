// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! LRU cache layer wrapping `FsFileStore`.
//!
//! Files are keyed by `{domain}/{url_path}` to prevent collisions between
//! different deployed apps. Tracks metadata (size, access time, immutability)
//! in memory for fast LRU eviction.

use bytes::Bytes;
use chrono::{DateTime, Utc};
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::RwLock;
use temps_file_store::fs_store::FsFileStore;
use temps_file_store::FileStore;
use tracing::{debug, info, warn};

use crate::{CacheStats, EdgeError};

/// TTL for non-immutable content (HTML pages, etc.)
const DEFAULT_TTL_SECS: i64 = 60;

/// Metadata about a cached entry.
#[derive(Debug, Clone)]
struct CacheEntry {
    size_bytes: u64,
    last_accessed: DateTime<Utc>,
    is_immutable: bool,
    ttl_expires: Option<DateTime<Utc>>,
}

/// LRU cache wrapping `FsFileStore` with eviction and stats.
pub struct EdgeCache {
    store: FsFileStore,
    index: RwLock<HashMap<String, CacheEntry>>,
    total_bytes: AtomicU64,
    max_bytes: u64,
    hits: AtomicU64,
    misses: AtomicU64,
}

impl EdgeCache {
    /// Create a new cache at the given directory with a max size in bytes.
    pub fn new(cache_dir: &PathBuf, max_bytes: u64) -> Self {
        Self {
            store: FsFileStore::new(cache_dir),
            index: RwLock::new(HashMap::new()),
            total_bytes: AtomicU64::new(0),
            max_bytes,
            hits: AtomicU64::new(0),
            misses: AtomicU64::new(0),
        }
    }

    /// Build the cache key: `{domain}/{url_path}`.
    fn cache_key(domain: &str, url_path: &str) -> String {
        let clean_path = url_path.trim_start_matches('/');
        format!("{}/{}", domain, clean_path)
    }

    /// Attempt to get a cached asset. Returns `None` on miss or TTL expiry.
    pub async fn get(&self, domain: &str, url_path: &str) -> Option<Bytes> {
        let key = Self::cache_key(domain, url_path);

        // Check if entry exists and is not expired
        {
            let index = self.index.read().unwrap();
            match index.get(&key) {
                Some(entry) => {
                    if let Some(expires) = entry.ttl_expires {
                        if Utc::now() > expires {
                            // Expired — treat as miss, will be cleaned up by eviction
                            self.misses.fetch_add(1, Ordering::Relaxed);
                            return None;
                        }
                    }
                }
                None => {
                    self.misses.fetch_add(1, Ordering::Relaxed);
                    return None;
                }
            }
        }

        // Read from disk
        match self.store.get(&key).await {
            Ok(data) => {
                self.hits.fetch_add(1, Ordering::Relaxed);
                // Update last accessed time
                {
                    let mut index = self.index.write().unwrap();
                    if let Some(entry) = index.get_mut(&key) {
                        entry.last_accessed = Utc::now();
                    }
                }
                Some(data)
            }
            Err(_) => {
                // File disappeared from disk — remove from index
                {
                    let mut index = self.index.write().unwrap();
                    if let Some(entry) = index.remove(&key) {
                        self.total_bytes
                            .fetch_sub(entry.size_bytes, Ordering::Relaxed);
                    }
                }
                self.misses.fetch_add(1, Ordering::Relaxed);
                None
            }
        }
    }

    /// Store an asset in the cache.
    pub async fn put(
        &self,
        domain: &str,
        url_path: &str,
        data: Bytes,
        is_immutable: bool,
    ) -> Result<(), EdgeError> {
        let key = Self::cache_key(domain, url_path);
        let size = data.len() as u64;

        self.store
            .put(&key, data)
            .await
            .map_err(|e| EdgeError::Cache {
                path: key.clone(),
                reason: format!("{}", e),
            })?;

        let ttl_expires = if is_immutable {
            None
        } else {
            Some(Utc::now() + chrono::Duration::seconds(DEFAULT_TTL_SECS))
        };

        let entry = CacheEntry {
            size_bytes: size,
            last_accessed: Utc::now(),
            is_immutable,
            ttl_expires,
        };

        {
            let mut index = self.index.write().unwrap();
            // If overwriting, subtract old size
            if let Some(old) = index.insert(key.clone(), entry) {
                self.total_bytes
                    .fetch_sub(old.size_bytes, Ordering::Relaxed);
            }
        }
        self.total_bytes.fetch_add(size, Ordering::Relaxed);

        debug!(
            "Cached {} ({} bytes, immutable={})",
            key, size, is_immutable
        );
        Ok(())
    }

    /// Evict entries until total size is below 80% of max.
    /// Evicts expired entries first, then LRU (non-immutable before immutable).
    pub async fn evict_if_needed(&self) {
        let current = self.total_bytes.load(Ordering::Relaxed);
        let threshold = (self.max_bytes as f64 * 0.9) as u64;

        if current <= threshold {
            return;
        }

        let target = (self.max_bytes as f64 * 0.8) as u64;
        let mut to_remove: Vec<(String, u64)> = Vec::new();
        let now = Utc::now();

        {
            let index = self.index.read().unwrap();

            // Phase 1: collect expired entries
            for (key, entry) in index.iter() {
                if let Some(expires) = entry.ttl_expires {
                    if now > expires {
                        to_remove.push((key.clone(), entry.size_bytes));
                    }
                }
            }

            // Phase 2: if still over target, sort by LRU (non-immutable first)
            let mut freed: u64 = to_remove.iter().map(|(_, s)| s).sum();
            if current.saturating_sub(freed) > target {
                let mut candidates: Vec<_> = index
                    .iter()
                    .filter(|(k, _)| !to_remove.iter().any(|(rk, _)| rk == *k))
                    .map(|(k, e)| (k.clone(), e.clone()))
                    .collect();

                // Sort: non-immutable first, then by oldest access time
                candidates.sort_by(|(_, a), (_, b)| {
                    a.is_immutable
                        .cmp(&b.is_immutable)
                        .then(a.last_accessed.cmp(&b.last_accessed))
                });

                for (key, entry) in candidates {
                    if current.saturating_sub(freed) <= target {
                        break;
                    }
                    freed += entry.size_bytes;
                    to_remove.push((key, entry.size_bytes));
                }
            }
        }

        if to_remove.is_empty() {
            return;
        }

        let count = to_remove.len();
        let mut freed_total: u64 = 0;

        for (key, size) in &to_remove {
            // Remove from disk (best-effort)
            if let Err(e) = self.remove_from_disk(key).await {
                warn!("Failed to remove cached file {}: {}", key, e);
                continue;
            }
            freed_total += size;
        }

        // Remove from index
        {
            let mut index = self.index.write().unwrap();
            for (key, _) in &to_remove {
                index.remove(key);
            }
        }

        self.total_bytes.fetch_sub(freed_total, Ordering::Relaxed);
        info!("Evicted {} entries, freed {} bytes", count, freed_total);
    }

    /// Remove a file from the underlying store's disk.
    async fn remove_from_disk(&self, key: &str) -> Result<(), EdgeError> {
        // FsFileStore doesn't expose delete, so we remove the file directly
        let clean: PathBuf = key
            .trim_start_matches('/')
            .split('/')
            .filter(|seg| !seg.is_empty() && *seg != ".." && *seg != ".")
            .collect();
        let blob_path = self.cache_dir().join(clean);
        if blob_path.exists() {
            tokio::fs::remove_file(&blob_path)
                .await
                .map_err(|e| EdgeError::Cache {
                    path: key.to_string(),
                    reason: format!("Failed to delete: {}", e),
                })?;
        }
        Ok(())
    }

    /// Get the cache root directory (same as FsFileStore root).
    fn cache_dir(&self) -> PathBuf {
        // FsFileStore stores at root — we reconstruct from the same path
        // The store was created with the cache_dir, so its root is cache_dir
        self.store.root().to_path_buf()
    }

    /// Current cache statistics.
    pub fn stats(&self) -> CacheStats {
        let hits = self.hits.load(Ordering::Relaxed);
        let misses = self.misses.load(Ordering::Relaxed);
        let total = hits + misses;
        let hit_rate = if total > 0 {
            hits as f64 / total as f64
        } else {
            0.0
        };

        CacheStats {
            hit_count: hits,
            miss_count: misses,
            hit_rate,
            disk_usage_bytes: self.total_bytes.load(Ordering::Relaxed),
            entry_count: self.index.read().unwrap().len() as u64,
        }
    }

    /// Invalidate all entries for a specific domain.
    pub async fn invalidate_domain(&self, domain: &str) {
        let prefix = format!("{}/", domain);
        let to_remove: Vec<(String, u64)>;
        {
            let index = self.index.read().unwrap();
            to_remove = index
                .iter()
                .filter(|(k, _)| k.starts_with(&prefix))
                .map(|(k, e)| (k.clone(), e.size_bytes))
                .collect();
        }

        let mut freed: u64 = 0;
        for (key, size) in &to_remove {
            if self.remove_from_disk(key).await.is_ok() {
                freed += size;
            }
        }

        {
            let mut index = self.index.write().unwrap();
            for (key, _) in &to_remove {
                index.remove(key);
            }
        }
        self.total_bytes.fetch_sub(freed, Ordering::Relaxed);

        if !to_remove.is_empty() {
            info!(
                "Invalidated {} entries for domain {}, freed {} bytes",
                to_remove.len(),
                domain,
                freed
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_cache() -> (tempfile::TempDir, EdgeCache) {
        let dir = tempfile::tempdir().unwrap();
        let cache = EdgeCache::new(&dir.path().join("cache"), 1024 * 1024); // 1MB
        (dir, cache)
    }

    #[tokio::test]
    async fn test_put_and_get() {
        let (_dir, cache) = temp_cache();
        let data = Bytes::from("hello world");

        cache
            .put("example.com", "assets/main.js", data.clone(), true)
            .await
            .unwrap();

        let result = cache.get("example.com", "assets/main.js").await;
        assert_eq!(result, Some(data));
    }

    #[tokio::test]
    async fn test_cache_miss() {
        let (_dir, cache) = temp_cache();
        let result = cache.get("example.com", "nope.js").await;
        assert!(result.is_none());
    }

    #[tokio::test]
    async fn test_domain_isolation() {
        let (_dir, cache) = temp_cache();

        cache
            .put("a.com", "main.js", Bytes::from("app-a"), true)
            .await
            .unwrap();
        cache
            .put("b.com", "main.js", Bytes::from("app-b"), true)
            .await
            .unwrap();

        assert_eq!(
            cache.get("a.com", "main.js").await,
            Some(Bytes::from("app-a"))
        );
        assert_eq!(
            cache.get("b.com", "main.js").await,
            Some(Bytes::from("app-b"))
        );
    }

    #[tokio::test]
    async fn test_stats() {
        let (_dir, cache) = temp_cache();

        cache
            .put("x.com", "a.js", Bytes::from("data"), true)
            .await
            .unwrap();

        // One hit
        cache.get("x.com", "a.js").await;
        // One miss
        cache.get("x.com", "b.js").await;

        let stats = cache.stats();
        assert_eq!(stats.hit_count, 1);
        assert_eq!(stats.miss_count, 1);
        assert_eq!(stats.entry_count, 1);
        assert_eq!(stats.disk_usage_bytes, 4); // "data" = 4 bytes
    }

    #[tokio::test]
    async fn test_invalidate_domain() {
        let (_dir, cache) = temp_cache();

        cache
            .put("a.com", "x.js", Bytes::from("1"), true)
            .await
            .unwrap();
        cache
            .put("a.com", "y.js", Bytes::from("2"), true)
            .await
            .unwrap();
        cache
            .put("b.com", "z.js", Bytes::from("3"), true)
            .await
            .unwrap();

        cache.invalidate_domain("a.com").await;

        assert!(cache.get("a.com", "x.js").await.is_none());
        assert!(cache.get("a.com", "y.js").await.is_none());
        assert!(cache.get("b.com", "z.js").await.is_some()); // untouched
    }

    #[tokio::test]
    async fn test_eviction() {
        let dir = tempfile::tempdir().unwrap();
        // Tiny cache: 100 bytes
        let cache = EdgeCache::new(&dir.path().join("cache"), 100);

        // Put 60 bytes
        cache
            .put("a.com", "big.js", Bytes::from(vec![b'x'; 60]), true)
            .await
            .unwrap();

        // Put 50 more bytes — now at 110, over 90% threshold (90)
        cache
            .put("a.com", "big2.js", Bytes::from(vec![b'y'; 50]), true)
            .await
            .unwrap();

        cache.evict_if_needed().await;

        // After eviction, should be below 80 bytes
        let stats = cache.stats();
        assert!(stats.disk_usage_bytes <= 80);
    }
}
