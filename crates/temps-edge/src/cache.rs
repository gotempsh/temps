// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! LRU cache layer wrapping `FsFileStore`.
//!
//! Files are keyed by `{domain}/{url_path}` to prevent collisions between
//! different deployed apps. Tracks metadata (size, access time, immutability)
//! in memory for fast LRU eviction.
//!
//! ## Concurrency
//!
//! A cache hit is the edge's hottest path, so it takes no lock. The index is
//! split into [`SHARD_COUNT`] shards, each an `ArcSwap` snapshot of a
//! `HashMap`: a lookup is one atomic load. Writers (`put`, eviction,
//! invalidation, and the rare "file vanished from disk" cleanup) copy one
//! shard, change it and swap it in with `rcu`, so a write costs
//! `O(entries / SHARD_COUNT)` and never blocks a reader.
//!
//! The access time used for LRU ordering is an atomic on the entry, written
//! at most once per [`ACCESS_TIME_GRANULARITY_MS`]: a hot asset served by
//! every core does not bounce one cache line between them on every hit.
//!
//! `total_bytes` is only ever changed by the size of an entry that a swap
//! actually inserted or removed, so concurrent writers racing on the same key
//! cannot double-count or double-subtract it.

use arc_swap::ArcSwap;
use bytes::Bytes;
use chrono::Utc;
use std::collections::hash_map::RandomState;
use std::collections::HashMap;
use std::hash::BuildHasher;
use std::path::PathBuf;
use std::sync::atomic::{AtomicI64, AtomicU64, Ordering};
use std::sync::Arc;
use temps_file_store::fs_store::FsFileStore;
use temps_file_store::FileStore;
use tracing::{debug, info, warn};

use crate::{CacheStats, EdgeError};

/// TTL for non-immutable content (HTML pages, etc.)
const DEFAULT_TTL_SECS: i64 = 60;

/// Number of independently swapped index shards. A write copies one shard,
/// so this bounds the per-write copy to `1/SHARD_COUNT` of the index.
const SHARD_COUNT: usize = 32;

/// An entry's access time is refreshed at most this often. LRU ordering only
/// needs to be roughly right; writing the atomic on every hit would make a
/// popular asset a cross-core contention point.
const ACCESS_TIME_GRANULARITY_MS: i64 = 1_000;

/// Metadata about a cached entry. Shared between index snapshots by `Arc`,
/// so its only mutable field is atomic.
#[derive(Debug)]
struct CacheEntry {
    size_bytes: u64,
    /// Unix milliseconds of the last (sampled) access.
    last_accessed_ms: AtomicI64,
    is_immutable: bool,
    /// Unix milliseconds after which the entry is stale; `None` = immutable.
    ttl_expires_ms: Option<i64>,
}

impl CacheEntry {
    fn is_expired(&self, now_ms: i64) -> bool {
        matches!(self.ttl_expires_ms, Some(expires) if now_ms > expires)
    }

    /// Record an access, skipping the store when the recorded time is
    /// already within [`ACCESS_TIME_GRANULARITY_MS`] of `now_ms`.
    fn touch(&self, now_ms: i64) {
        let last = self.last_accessed_ms.load(Ordering::Relaxed);
        if now_ms.saturating_sub(last) >= ACCESS_TIME_GRANULARITY_MS {
            self.last_accessed_ms.store(now_ms, Ordering::Relaxed);
        }
    }
}

type ShardMap = HashMap<String, Arc<CacheEntry>>;

/// Whether `key` currently maps to exactly `entry` in `map`.
fn maps_to(map: &ShardMap, key: &str, entry: &Arc<CacheEntry>) -> bool {
    map.get(key)
        .is_some_and(|current| Arc::ptr_eq(current, entry))
}

/// Sharded, copy-on-write index: lock-free reads, `rcu` writes.
struct CacheIndex {
    shards: Vec<ArcSwap<ShardMap>>,
    hasher: RandomState,
}

impl CacheIndex {
    fn new() -> Self {
        Self {
            shards: (0..SHARD_COUNT)
                .map(|_| ArcSwap::from_pointee(ShardMap::new()))
                .collect(),
            hasher: RandomState::new(),
        }
    }

    fn shard_index(&self, key: &str) -> usize {
        (self.hasher.hash_one(key) as usize) % self.shards.len()
    }

    fn shard(&self, key: &str) -> &ArcSwap<ShardMap> {
        &self.shards[self.shard_index(key)]
    }

    /// Lock-free lookup.
    fn get(&self, key: &str) -> Option<Arc<CacheEntry>> {
        self.shard(key).load().get(key).cloned()
    }

    /// Insert `entry` under `key`, returning the entry the swap replaced.
    fn insert(&self, key: &str, entry: Arc<CacheEntry>) -> Option<Arc<CacheEntry>> {
        let previous = self.shard(key).rcu(|current| {
            let mut next = ShardMap::clone(current);
            next.insert(key.to_string(), Arc::clone(&entry));
            next
        });
        previous.get(key).cloned()
    }

    /// Remove every `(key, entry)` whose key still maps to that exact entry
    /// (pointer equality), one swap per affected shard. An entry replaced by
    /// a concurrent `put` in the meantime is left alone. Returns the total
    /// size of the entries actually removed.
    fn remove_exact(&self, victims: &[(String, Arc<CacheEntry>)]) -> (usize, u64) {
        let mut by_shard: Vec<Vec<&(String, Arc<CacheEntry>)>> = vec![Vec::new(); SHARD_COUNT];
        for victim in victims {
            by_shard[self.shard_index(&victim.0)].push(victim);
        }

        let mut removed_count = 0;
        let mut removed_bytes = 0;
        for (shard, shard_victims) in self.shards.iter().zip(by_shard) {
            if shard_victims.is_empty() {
                continue;
            }
            let previous = shard.rcu(|current| {
                let mut next = ShardMap::clone(current);
                for (key, entry) in &shard_victims {
                    if maps_to(current, key, entry) {
                        next.remove(key.as_str());
                    }
                }
                next
            });
            for (key, entry) in &shard_victims {
                if maps_to(&previous, key, entry) {
                    removed_count += 1;
                    removed_bytes += entry.size_bytes;
                }
            }
        }
        (removed_count, removed_bytes)
    }

    /// Every entry, from a consistent snapshot of each shard.
    fn entries(&self) -> Vec<(String, Arc<CacheEntry>)> {
        let mut all = Vec::new();
        for shard in &self.shards {
            let map = shard.load();
            all.extend(map.iter().map(|(k, e)| (k.clone(), Arc::clone(e))));
        }
        all
    }

    fn len(&self) -> usize {
        self.shards.iter().map(|shard| shard.load().len()).sum()
    }
}

/// LRU cache wrapping `FsFileStore` with eviction and stats.
pub struct EdgeCache {
    store: FsFileStore,
    index: CacheIndex,
    total_bytes: AtomicU64,
    max_bytes: u64,
    hits: AtomicU64,
    misses: AtomicU64,
}

fn now_ms() -> i64 {
    Utc::now().timestamp_millis()
}

impl EdgeCache {
    /// Create a new cache at the given directory with a max size in bytes.
    pub fn new(cache_dir: &PathBuf, max_bytes: u64) -> Self {
        Self {
            store: FsFileStore::new(cache_dir),
            index: CacheIndex::new(),
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

    /// Subtract from `total_bytes`, clamping at zero. Every subtraction is
    /// the size of an entry a swap really removed, so the clamp only guards
    /// against a future accounting mistake wrapping the counter to ~2^64 (which
    /// would make the next eviction pass empty the whole cache).
    fn sub_bytes(&self, bytes: u64) {
        // An explicit CAS loop rather than `fetch_update`, which newer
        // toolchains deprecate in favour of `try_update` (absent on older ones).
        let mut current = self.total_bytes.load(Ordering::Relaxed);
        loop {
            match self.total_bytes.compare_exchange_weak(
                current,
                current.saturating_sub(bytes),
                Ordering::Relaxed,
                Ordering::Relaxed,
            ) {
                Ok(_) => break,
                Err(actual) => current = actual,
            }
        }
    }

    /// Attempt to get a cached asset. Returns `None` on miss or TTL expiry.
    ///
    /// Takes no lock: one atomic snapshot load, the disk read, and at most
    /// one relaxed atomic store for the access time.
    pub async fn get(&self, domain: &str, url_path: &str) -> Option<Bytes> {
        let key = Self::cache_key(domain, url_path);

        let entry = match self.index.get(&key) {
            Some(entry) if !entry.is_expired(now_ms()) => entry,
            // Missing, or expired — treat as miss; eviction cleans it up.
            _ => {
                self.misses.fetch_add(1, Ordering::Relaxed);
                return None;
            }
        };

        match self.store.get(&key).await {
            Ok(data) => {
                self.hits.fetch_add(1, Ordering::Relaxed);
                entry.touch(now_ms());
                Some(data)
            }
            Err(_) => {
                // File disappeared from disk — drop this entry from the index,
                // unless a concurrent `put` already replaced it.
                let (_, removed_bytes) = self.index.remove_exact(&[(key, entry)]);
                self.sub_bytes(removed_bytes);
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

        let now = now_ms();
        let ttl_expires_ms = if is_immutable {
            None
        } else {
            Some(now + DEFAULT_TTL_SECS * 1_000)
        };

        let entry = Arc::new(CacheEntry {
            size_bytes: size,
            last_accessed_ms: AtomicI64::new(now),
            is_immutable,
            ttl_expires_ms,
        });

        self.total_bytes.fetch_add(size, Ordering::Relaxed);
        if let Some(replaced) = self.index.insert(&key, entry) {
            self.sub_bytes(replaced.size_bytes);
        }

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
        let now = now_ms();

        // Phase 1: expired entries. Phase 2: the rest, least recently used
        // first (non-immutable before immutable). The access time is read
        // once into the tuple: sorting on a value other threads keep
        // updating would not be a total order.
        let (mut to_remove, mut candidates): (Vec<_>, Vec<_>) = self
            .index
            .entries()
            .into_iter()
            .map(|(key, entry)| {
                let last_accessed = entry.last_accessed_ms.load(Ordering::Relaxed);
                (key, entry, last_accessed)
            })
            .partition(|(_, entry, _)| entry.is_expired(now));

        let mut freed: u64 = to_remove.iter().map(|(_, e, _)| e.size_bytes).sum();
        if current.saturating_sub(freed) > target {
            candidates.sort_by(|(_, a, a_accessed), (_, b, b_accessed)| {
                a.is_immutable
                    .cmp(&b.is_immutable)
                    .then(a_accessed.cmp(b_accessed))
            });
            for candidate in candidates {
                if current.saturating_sub(freed) <= target {
                    break;
                }
                freed += candidate.1.size_bytes;
                to_remove.push(candidate);
            }
        }

        if to_remove.is_empty() {
            return;
        }

        let to_remove = to_remove
            .into_iter()
            .map(|(key, entry, _)| (key, entry))
            .collect();
        let (count, freed_total) = self.remove_entries(to_remove).await;
        info!("Evicted {} entries, freed {} bytes", count, freed_total);
    }

    /// Delete `victims` from disk, then drop from the index those whose
    /// file is gone. Disk first: a `put` racing between the two steps
    /// replaces the entry, and `remove_exact` then leaves the new one in
    /// place. A file that fails to delete keeps its entry (and its bytes),
    /// so the next eviction pass retries it instead of leaking the space.
    async fn remove_entries(&self, victims: Vec<(String, Arc<CacheEntry>)>) -> (usize, u64) {
        let mut deleted = Vec::with_capacity(victims.len());
        for (key, entry) in victims {
            match self.remove_from_disk(&key).await {
                Ok(()) => deleted.push((key, entry)),
                Err(e) => warn!("Failed to remove cached file {}: {}", key, e),
            }
        }
        let (count, freed) = self.index.remove_exact(&deleted);
        self.sub_bytes(freed);
        (count, freed)
    }

    /// Remove a cached file from disk. A file that is already gone is not
    /// an error.
    async fn remove_from_disk(&self, key: &str) -> Result<(), EdgeError> {
        // FsFileStore doesn't expose delete, so we remove the file directly.
        // Mirrors `FsFileStore::cache_path`: cached files live under the
        // store's `cache/` sub-directory, sanitised the same way.
        let clean: PathBuf = key
            .trim_start_matches('/')
            .split('/')
            .filter(|seg| !seg.is_empty() && *seg != ".." && *seg != ".")
            .collect();
        let file_path = self.store.root().join("cache").join(clean);
        match tokio::fs::remove_file(&file_path).await {
            Ok(()) => Ok(()),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(e) => Err(EdgeError::Cache {
                path: key.to_string(),
                reason: format!("Failed to delete {}: {}", file_path.display(), e),
            }),
        }
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
            entry_count: self.index.len() as u64,
        }
    }

    /// Invalidate all entries for a specific domain.
    pub async fn invalidate_domain(&self, domain: &str) {
        let prefix = format!("{}/", domain);
        let to_remove: Vec<_> = self
            .index
            .entries()
            .into_iter()
            .filter(|(key, _)| key.starts_with(&prefix))
            .collect();

        if to_remove.is_empty() {
            return;
        }

        let (count, freed) = self.remove_entries(to_remove).await;
        info!(
            "Invalidated {} entries for domain {}, freed {} bytes",
            count, domain, freed
        );
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

    #[tokio::test]
    async fn test_overwrite_accounts_only_the_latest_size() {
        let (_dir, cache) = temp_cache();
        cache
            .put("a.com", "x.js", Bytes::from(vec![b'a'; 10]), true)
            .await
            .unwrap();
        cache
            .put("a.com", "x.js", Bytes::from(vec![b'b'; 3]), true)
            .await
            .unwrap();

        let stats = cache.stats();
        assert_eq!(stats.entry_count, 1);
        assert_eq!(stats.disk_usage_bytes, 3);
    }

    #[tokio::test]
    async fn test_expired_entry_is_a_miss() {
        let (_dir, cache) = temp_cache();
        cache
            .put("a.com", "index.html", Bytes::from("page"), false)
            .await
            .unwrap();
        // Replace the entry with one whose TTL is already in the past.
        let key = EdgeCache::cache_key("a.com", "index.html");
        cache.index.insert(
            &key,
            Arc::new(CacheEntry {
                size_bytes: 4,
                last_accessed_ms: AtomicI64::new(now_ms()),
                is_immutable: false,
                ttl_expires_ms: Some(now_ms() - 1),
            }),
        );

        assert!(cache.get("a.com", "index.html").await.is_none());
        assert_eq!(cache.stats().miss_count, 1);
    }

    #[test]
    fn test_access_time_is_sampled() {
        let entry = CacheEntry {
            size_bytes: 1,
            last_accessed_ms: AtomicI64::new(10_000),
            is_immutable: true,
            ttl_expires_ms: None,
        };

        // Within the granularity: no store.
        entry.touch(10_000 + ACCESS_TIME_GRANULARITY_MS - 1);
        assert_eq!(entry.last_accessed_ms.load(Ordering::Relaxed), 10_000);

        // Past it: recorded.
        let later = 10_000 + ACCESS_TIME_GRANULARITY_MS;
        entry.touch(later);
        assert_eq!(entry.last_accessed_ms.load(Ordering::Relaxed), later);
    }

    #[tokio::test]
    async fn test_eviction_prefers_least_recently_used() {
        let dir = tempfile::tempdir().unwrap();
        let cache = EdgeCache::new(&dir.path().join("cache"), 100);
        for name in ["a.js", "b.js", "c.js", "d.js"] {
            cache
                .put("x.com", name, Bytes::from(vec![b'x'; 25]), true)
                .await
                .unwrap();
        }
        // Make `c.js` the least recently used, the others recent.
        for (name, accessed) in [
            ("a.js", 4_000),
            ("b.js", 3_000),
            ("c.js", 1),
            ("d.js", 2_000),
        ] {
            let entry = cache
                .index
                .get(&EdgeCache::cache_key("x.com", name))
                .unwrap();
            entry.last_accessed_ms.store(accessed, Ordering::Relaxed);
        }

        // 100 bytes > 90% of 100: evict down to 80% — exactly one entry.
        cache.evict_if_needed().await;

        assert!(cache.get("x.com", "c.js").await.is_none());
        for name in ["a.js", "b.js", "d.js"] {
            assert!(cache.get("x.com", name).await.is_some(), "{name} evicted");
        }
        assert_eq!(cache.stats().disk_usage_bytes, 75);
    }

    /// Regression: eviction used to delete `<root>/<key>` while the store
    /// writes `<root>/cache/<key>`, so evicted files stayed on disk forever.
    #[tokio::test]
    async fn test_invalidate_deletes_the_file_from_disk() {
        let (dir, cache) = temp_cache();
        cache
            .put("a.com", "assets/x.js", Bytes::from("1"), true)
            .await
            .unwrap();
        let on_disk = dir.path().join("cache").join("cache/a.com/assets/x.js");
        assert!(
            on_disk.exists(),
            "precondition: {} exists",
            on_disk.display()
        );

        cache.invalidate_domain("a.com").await;

        assert!(!on_disk.exists(), "{} still on disk", on_disk.display());
        let stats = cache.stats();
        assert_eq!(stats.entry_count, 0);
        assert_eq!(stats.disk_usage_bytes, 0);
    }

    #[tokio::test]
    async fn test_vanished_file_is_dropped_from_the_index() {
        let (dir, cache) = temp_cache();
        cache
            .put("a.com", "x.js", Bytes::from("12345"), true)
            .await
            .unwrap();
        std::fs::remove_file(dir.path().join("cache").join("cache/a.com/x.js")).unwrap();

        assert!(cache.get("a.com", "x.js").await.is_none());
        let stats = cache.stats();
        assert_eq!(stats.entry_count, 0);
        assert_eq!(stats.disk_usage_bytes, 0);
        assert_eq!(stats.miss_count, 1);
    }

    /// Concurrent hits, overwrites and invalidations neither panic nor
    /// corrupt the byte accounting.
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn test_concurrent_access_keeps_accounting_consistent() {
        let dir = tempfile::tempdir().unwrap();
        let cache = Arc::new(EdgeCache::new(&dir.path().join("cache"), 1024 * 1024));

        let mut tasks = Vec::new();
        for worker in 0..8u8 {
            let cache = Arc::clone(&cache);
            tasks.push(tokio::spawn(async move {
                for i in 0..50usize {
                    let path = format!("asset-{}.js", i % 10);
                    let body = Bytes::from(vec![worker; 1 + (i % 7)]);
                    cache.put("c.com", &path, body, true).await.unwrap();
                    let _ = cache.get("c.com", &path).await;
                    if i % 17 == 0 {
                        cache.invalidate_domain("c.com").await;
                    }
                }
            }));
        }
        for task in tasks {
            task.await.unwrap();
        }

        let indexed: u64 = cache
            .index
            .entries()
            .iter()
            .map(|(_, entry)| entry.size_bytes)
            .sum();
        assert_eq!(cache.stats().disk_usage_bytes, indexed);
    }
}
