// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Tiered local-disk cache of immutable chunk byte ranges (ADR-046 §6).
//!
//! Chunk objects are immutable, so caching a `(storage_key, byte_range)` pair
//! is trivial: it never goes stale. The cache is split into three priority
//! tiers so the planner can pin the cheap, high-value bytes (footers) and let
//! the expensive, low-value bytes (raw blocks) evict first:
//!
//! * [`CacheTier::Index`] — labels + block index tier of the footer. Small
//!   (~2 KB/chunk) and read on every planned query; pinned preferentially.
//! * [`CacheTier::Bloom`] — bloom filter tier of the footer (~80 KB/chunk);
//!   read only for free-text queries.
//! * [`CacheTier::Block`] — decompressed-on-demand data blocks; the bulk of
//!   the bytes, evicted first.
//!
//! In the filesystem storage backend the cache is unnecessary (reads are
//! already local); passing `dir: None` to [`ChunkCache::open`] yields a
//! no-op cache where every `get` misses and `put` does nothing.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use bytes::Bytes;
use sha2::{Digest, Sha256};
use tokio::io::AsyncWriteExt;

use crate::error::LogAggregatorError;

/// Priority tier of a cached byte range. Eviction order is [`CacheTier::Block`]
/// first, then [`CacheTier::Bloom`], then [`CacheTier::Index`] (index entries
/// are only evicted when the cache is still over capacity after clearing
/// every block and bloom entry).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum CacheTier {
    Index,
    Bloom,
    Block,
}

impl CacheTier {
    /// Single-character suffix used in the on-disk file name.
    fn suffix(self) -> char {
        match self {
            CacheTier::Index => 'i',
            CacheTier::Bloom => 'b',
            CacheTier::Block => 'k',
        }
    }

    fn from_suffix(c: char) -> Option<CacheTier> {
        match c {
            'i' => Some(CacheTier::Index),
            'b' => Some(CacheTier::Bloom),
            'k' => Some(CacheTier::Block),
            _ => None,
        }
    }

    /// Eviction priority: higher is evicted first. Block goes first, then
    /// Bloom, then Index (only once Block and Bloom are both empty).
    fn eviction_rank(self) -> u8 {
        match self {
            CacheTier::Block => 2,
            CacheTier::Bloom => 1,
            CacheTier::Index => 0,
        }
    }
}

/// Snapshot of cache occupancy and hit/miss counters.
#[derive(Debug, Clone, Default)]
pub struct CacheStats {
    pub index_bytes: u64,
    pub index_entries: u64,
    pub bloom_bytes: u64,
    pub bloom_entries: u64,
    pub block_bytes: u64,
    pub block_entries: u64,
    pub hits: u64,
    pub misses: u64,
}

impl CacheStats {
    pub fn total_bytes(&self) -> u64 {
        self.index_bytes + self.bloom_bytes + self.block_bytes
    }
}

#[derive(Debug, Clone)]
struct Entry {
    size: u64,
    tier: CacheTier,
    last_access: u64,
}

struct Inner {
    dir: Option<PathBuf>,
    /// Capacity; a setting, so it can change while the cache is open.
    max_bytes: AtomicU64,
    entries: Mutex<HashMap<String, Entry>>,
    access_counter: AtomicU64,
    hits: AtomicU64,
    misses: AtomicU64,
}

/// Cheaply-cloneable handle to a tiered on-disk cache of chunk byte ranges.
#[derive(Clone)]
pub struct ChunkCache {
    inner: Arc<Inner>,
}

/// The on-disk file name for one cache entry, without the `<xx>/` shard
/// prefix: `<sha256hex(key)>-<start>-<end>.<tier-letter>`.
fn entry_file_name(key: &str, start: u64, end: u64, tier: CacheTier) -> String {
    let digest = Sha256::digest(key.as_bytes());
    let hex_digest = hex::encode(digest);
    format!("{hex_digest}-{start}-{end}.{}", tier.suffix())
}

/// The map key used for in-memory bookkeeping: shard-independent, unique per
/// `(key, range, tier)`.
fn entry_map_key(key: &str, start: u64, end: u64, tier: CacheTier) -> String {
    entry_file_name(key, start, end, tier)
}

/// Path of a cache entry under `dir`, given its file name.
fn shard_path(dir: &Path, file_name: &str) -> PathBuf {
    let shard = &file_name[..2.min(file_name.len())];
    dir.join(shard).join(file_name)
}

impl ChunkCache {
    /// Open (creating if necessary) a tiered cache rooted at `dir`, rebuilding
    /// its in-memory bookkeeping by scanning the directory. `dir: None`
    /// yields a no-op cache (every `get` misses, `put` is a no-op) — used for
    /// the filesystem storage backend where caching buys nothing.
    pub async fn open(dir: Option<PathBuf>, max_bytes: u64) -> Result<Self, LogAggregatorError> {
        let entries = Mutex::new(HashMap::new());
        let inner = Inner {
            dir: dir.clone(),
            max_bytes: AtomicU64::new(max_bytes),
            entries,
            access_counter: AtomicU64::new(0),
            hits: AtomicU64::new(0),
            misses: AtomicU64::new(0),
        };
        let cache = ChunkCache {
            inner: Arc::new(inner),
        };
        if let Some(dir) = dir {
            tokio::fs::create_dir_all(&dir).await?;
            cache.rebuild(&dir).await?;
        }
        Ok(cache)
    }

    /// Scan `dir` for existing shard directories and entries, populating the
    /// in-memory bookkeeping. Files whose name does not parse as a valid
    /// cache entry are deleted.
    async fn rebuild(&self, dir: &Path) -> Result<(), LogAggregatorError> {
        let mut top = tokio::fs::read_dir(dir).await?;
        while let Some(shard_entry) = top.next_entry().await? {
            let shard_path = shard_entry.path();
            if !shard_entry.file_type().await?.is_dir() {
                continue;
            }
            let mut shard_iter = match tokio::fs::read_dir(&shard_path).await {
                Ok(iter) => iter,
                Err(_) => continue,
            };
            while let Some(file_entry) = shard_iter.next_entry().await? {
                let path = file_entry.path();
                let file_name = match path.file_name().and_then(|n| n.to_str()) {
                    Some(n) => n.to_string(),
                    None => {
                        let _ = tokio::fs::remove_file(&path).await;
                        continue;
                    }
                };
                match parse_entry_file_name(&file_name) {
                    Some(tier) => {
                        let meta = match tokio::fs::metadata(&path).await {
                            Ok(m) => m,
                            Err(_) => continue,
                        };
                        let size = meta.len();
                        let counter = self.inner.access_counter.fetch_add(1, Ordering::Relaxed);
                        let mut entries = self.lock_entries();
                        entries.insert(
                            file_name,
                            Entry {
                                size,
                                tier,
                                last_access: counter,
                            },
                        );
                    }
                    None => {
                        let _ = tokio::fs::remove_file(&path).await;
                    }
                }
            }
        }
        Ok(())
    }

    fn lock_entries(&self) -> std::sync::MutexGuard<'_, HashMap<String, Entry>> {
        // Never held across an await point; a poisoned mutex means a prior
        // panic already corrupted state we can't recover, so surface it.
        match self.inner.entries.lock() {
            Ok(guard) => guard,
            Err(poisoned) => poisoned.into_inner(),
        }
    }

    /// Fetch a cached byte range. `None` on a miss (never cached, evicted, or
    /// this is a no-op cache).
    pub async fn get(&self, key: &str, start: u64, end: u64) -> Option<Bytes> {
        let dir = self.inner.dir.as_ref()?;
        // Try every tier: the caller doesn't know which tier a range was
        // stored under (index vs bloom share the footer's byte space in
        // practice, but callers pass whichever tier they cache under).
        for tier in [CacheTier::Index, CacheTier::Bloom, CacheTier::Block] {
            let map_key = entry_map_key(key, start, end, tier);
            let present = {
                let entries = self.lock_entries();
                entries.contains_key(&map_key)
            };
            if !present {
                continue;
            }
            let path = shard_path(dir, &map_key);
            match tokio::fs::read(&path).await {
                Ok(data) => {
                    let counter = self.inner.access_counter.fetch_add(1, Ordering::Relaxed);
                    let mut entries = self.lock_entries();
                    if let Some(e) = entries.get_mut(&map_key) {
                        e.last_access = counter;
                    }
                    drop(entries);
                    self.inner.hits.fetch_add(1, Ordering::Relaxed);
                    return Some(Bytes::from(data));
                }
                Err(_) => {
                    // File vanished under us (e.g. concurrent eviction);
                    // drop the stale bookkeeping and keep trying other tiers.
                    let mut entries = self.lock_entries();
                    entries.remove(&map_key);
                }
            }
        }
        self.inner.misses.fetch_add(1, Ordering::Relaxed);
        None
    }

    /// Store a byte range under the given tier. A no-op for the no-op cache,
    /// for an already-present identical entry, or when the item alone is
    /// larger than the cache's capacity.
    pub async fn put(&self, key: &str, start: u64, end: u64, data: Bytes, tier: CacheTier) {
        let Some(dir) = self.inner.dir.clone() else {
            return;
        };
        let size = data.len() as u64;
        if size > self.max_bytes() {
            // Oversized single item: never store it.
            return;
        }
        let file_name = entry_file_name(key, start, end, tier);
        let map_key = file_name.clone();

        // Idempotent: if already present, just bump last_access.
        {
            let entries = self.lock_entries();
            if entries.contains_key(&map_key) {
                drop(entries);
                let counter = self.inner.access_counter.fetch_add(1, Ordering::Relaxed);
                let mut entries = self.lock_entries();
                if let Some(e) = entries.get_mut(&map_key) {
                    e.last_access = counter;
                }
                return;
            }
        }

        // Evict until there is room for `size` more bytes.
        self.evict_for(size).await;

        let final_path = shard_path(&dir, &file_name);
        let Some(shard_dir) = final_path.parent() else {
            return;
        };
        if tokio::fs::create_dir_all(shard_dir).await.is_err() {
            return;
        }

        let tmp_path = shard_dir.join(format!("{file_name}.tmp-{}", uuid_like()));
        let write_result: std::io::Result<()> = async {
            let mut f = tokio::fs::File::create(&tmp_path).await?;
            f.write_all(&data).await?;
            f.flush().await?;
            tokio::fs::rename(&tmp_path, &final_path).await?;
            Ok(())
        }
        .await;

        if write_result.is_err() {
            let _ = tokio::fs::remove_file(&tmp_path).await;
            return;
        }

        let counter = self.inner.access_counter.fetch_add(1, Ordering::Relaxed);
        let mut entries = self.lock_entries();
        entries.insert(
            map_key,
            Entry {
                size,
                tier,
                last_access: counter,
            },
        );
    }

    /// Current capacity in bytes.
    pub fn max_bytes(&self) -> u64 {
        self.inner.max_bytes.load(Ordering::Relaxed)
    }

    /// Change the capacity at runtime (Settings → Monitoring → container
    /// logs). Shrinking evicts immediately, lowest tier first, until the
    /// cache fits; growing simply allows more to be kept.
    pub async fn set_max_bytes(&self, max_bytes: u64) {
        if self.inner.max_bytes.swap(max_bytes, Ordering::Relaxed) != max_bytes {
            self.evict_for(0).await;
        }
    }

    /// Evict entries (Block tier first, then Bloom, then Index) until adding
    /// `incoming` bytes would not exceed `max_bytes`.
    async fn evict_for(&self, incoming: u64) {
        let Some(dir) = self.inner.dir.clone() else {
            return;
        };
        loop {
            let current_total: u64 = {
                let entries = self.lock_entries();
                entries.values().map(|e| e.size).sum()
            };
            if current_total + incoming <= self.max_bytes() {
                return;
            }
            // Pick the LRU entry within the lowest-priority non-empty tier.
            let victim = {
                let entries = self.lock_entries();
                // Sort key: (eviction_rank, reversed last_access) so that
                // max_by_key picks the highest-priority tier and, within
                // that tier, the oldest (least-recently-used) entry.
                entries
                    .iter()
                    .max_by_key(|(_, e)| (e.tier.eviction_rank(), u64::MAX - e.last_access))
                    .map(|(k, v)| (k.clone(), v.size))
            };
            let Some((victim_key, _victim_size)) = victim else {
                // Nothing left to evict; give up (the incoming item just
                // won't fit, but callers already checked size > max_bytes).
                return;
            };
            let path = shard_path(&dir, &victim_key);
            let _ = tokio::fs::remove_file(&path).await;
            let mut entries = self.lock_entries();
            entries.remove(&victim_key);
        }
    }

    /// Snapshot of current occupancy and hit/miss counters.
    pub fn stats(&self) -> CacheStats {
        let entries = self.lock_entries();
        let mut stats = CacheStats {
            hits: self.inner.hits.load(Ordering::Relaxed),
            misses: self.inner.misses.load(Ordering::Relaxed),
            ..Default::default()
        };
        for entry in entries.values() {
            match entry.tier {
                CacheTier::Index => {
                    stats.index_bytes += entry.size;
                    stats.index_entries += 1;
                }
                CacheTier::Bloom => {
                    stats.bloom_bytes += entry.size;
                    stats.bloom_entries += 1;
                }
                CacheTier::Block => {
                    stats.block_bytes += entry.size;
                    stats.block_entries += 1;
                }
            }
        }
        stats
    }

    /// Remove every cached entry from disk and memory.
    pub async fn clear(&self) {
        let Some(dir) = self.inner.dir.clone() else {
            return;
        };
        let keys: Vec<String> = {
            let entries = self.lock_entries();
            entries.keys().cloned().collect()
        };
        for key in keys {
            let path = shard_path(&dir, &key);
            let _ = tokio::fs::remove_file(&path).await;
        }
        let mut entries = self.lock_entries();
        entries.clear();
    }
}

/// Parse `<sha256hex>-<start>-<end>.<tier>` back into a [`CacheTier`],
/// validating shape without needing the full digest/range back.
fn parse_entry_file_name(name: &str) -> Option<CacheTier> {
    let (stem, ext) = name.rsplit_once('.')?;
    if ext.len() != 1 {
        return None;
    }
    let tier = CacheTier::from_suffix(ext.chars().next()?)?;
    let mut parts = stem.rsplitn(3, '-');
    let end = parts.next()?;
    let start = parts.next()?;
    let digest = parts.next()?;
    if digest.len() != 64 || !digest.bytes().all(|b| b.is_ascii_hexdigit()) {
        return None;
    }
    start.parse::<u64>().ok()?;
    end.parse::<u64>().ok()?;
    Some(tier)
}

/// A short random-ish suffix for temp file names, without pulling in a UUID
/// dependency just for this. Not for security purposes — collisions are
/// harmless (the loser's rename simply fails and is retried by the next
/// `put`).
fn uuid_like() -> u64 {
    use std::time::{SystemTime, UNIX_EPOCH};
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos() as u64)
        .unwrap_or(0)
        ^ (std::process::id() as u64) << 32
}

#[cfg(test)]
mod tests {
    use super::*;

    fn bytes_of(s: &str) -> Bytes {
        Bytes::from(s.as_bytes().to_vec())
    }

    #[tokio::test]
    async fn hit_and_miss() {
        let dir = tempfile::tempdir().expect("tempdir");
        let cache = ChunkCache::open(Some(dir.path().to_path_buf()), 1024 * 1024)
            .await
            .expect("open");

        assert!(cache.get("chunk-a", 0, 100).await.is_none());

        cache
            .put("chunk-a", 0, 100, bytes_of("hello world"), CacheTier::Block)
            .await;

        let got = cache.get("chunk-a", 0, 100).await;
        assert_eq!(got, Some(bytes_of("hello world")));

        let stats = cache.stats();
        assert_eq!(stats.hits, 1);
        assert_eq!(stats.misses, 1);
        assert_eq!(stats.block_entries, 1);
    }

    #[tokio::test]
    async fn no_op_mode() {
        let cache = ChunkCache::open(None, 1024 * 1024).await.expect("open");
        cache
            .put("chunk-a", 0, 100, bytes_of("data"), CacheTier::Index)
            .await;
        assert!(cache.get("chunk-a", 0, 100).await.is_none());
        let stats = cache.stats();
        assert_eq!(stats.total_bytes(), 0);
    }

    #[tokio::test]
    async fn oversized_item_skipped() {
        let dir = tempfile::tempdir().expect("tempdir");
        let cache = ChunkCache::open(Some(dir.path().to_path_buf()), 8)
            .await
            .expect("open");
        cache
            .put(
                "chunk-a",
                0,
                100,
                bytes_of("this is way more than 8 bytes"),
                CacheTier::Block,
            )
            .await;
        assert!(cache.get("chunk-a", 0, 100).await.is_none());
        assert_eq!(cache.stats().total_bytes(), 0);
    }

    #[tokio::test]
    async fn eviction_order_respects_tiers() {
        let dir = tempfile::tempdir().expect("tempdir");
        // Capacity for exactly ~3 ten-byte entries.
        let cache = ChunkCache::open(Some(dir.path().to_path_buf()), 30)
            .await
            .expect("open");

        cache
            .put("k-index", 0, 10, bytes_of("0123456789"), CacheTier::Index)
            .await;
        cache
            .put("k-bloom", 0, 10, bytes_of("0123456789"), CacheTier::Bloom)
            .await;
        cache
            .put("k-block", 0, 10, bytes_of("0123456789"), CacheTier::Block)
            .await;

        // All three fit exactly. Adding a 4th of any tier forces an eviction;
        // Block should go first even though it's the newest.
        cache
            .put("k-block2", 0, 10, bytes_of("0123456789"), CacheTier::Block)
            .await;

        let stats = cache.stats();
        assert_eq!(stats.index_entries, 1, "index tier must survive");
        assert_eq!(stats.bloom_entries, 1, "bloom tier must survive");
        assert_eq!(
            stats.block_entries, 1,
            "only the newest block entry survives"
        );
        assert!(cache.get("k-block", 0, 10).await.is_none());
        assert!(cache.get("k-block2", 0, 10).await.is_some());
        assert!(cache.get("k-index", 0, 10).await.is_some());
        assert!(cache.get("k-bloom", 0, 10).await.is_some());
    }

    #[tokio::test]
    async fn eviction_falls_through_to_bloom_then_index_when_only_one_tier_present() {
        let dir = tempfile::tempdir().expect("tempdir");
        let cache = ChunkCache::open(Some(dir.path().to_path_buf()), 20)
            .await
            .expect("open");

        // Only Index-tier entries exist; once full, further puts must still
        // evict *something* (falling through past empty Block/Bloom tiers).
        cache
            .put("k1", 0, 10, bytes_of("0123456789"), CacheTier::Index)
            .await;
        cache
            .put("k2", 0, 10, bytes_of("0123456789"), CacheTier::Index)
            .await;
        cache
            .put("k3", 0, 10, bytes_of("0123456789"), CacheTier::Index)
            .await;

        let stats = cache.stats();
        assert_eq!(stats.index_entries, 2);
        assert!(cache.get("k1", 0, 10).await.is_none(), "oldest evicted");
        assert!(cache.get("k2", 0, 10).await.is_some());
        assert!(cache.get("k3", 0, 10).await.is_some());
    }

    #[tokio::test]
    async fn rebuild_after_reopen() {
        let dir = tempfile::tempdir().expect("tempdir");
        {
            let cache = ChunkCache::open(Some(dir.path().to_path_buf()), 1024 * 1024)
                .await
                .expect("open");
            cache
                .put("chunk-a", 0, 100, bytes_of("hello"), CacheTier::Block)
                .await;
            cache
                .put("chunk-b", 10, 50, bytes_of("world"), CacheTier::Index)
                .await;
        }

        // Write a garbage file that shouldn't parse as an entry.
        let garbage_shard = dir.path().join("zz");
        tokio::fs::create_dir_all(&garbage_shard)
            .await
            .expect("mkdir");
        tokio::fs::write(garbage_shard.join("not-an-entry.txt"), b"junk")
            .await
            .expect("write");

        let reopened = ChunkCache::open(Some(dir.path().to_path_buf()), 1024 * 1024)
            .await
            .expect("reopen");

        let stats = reopened.stats();
        assert_eq!(stats.block_entries, 1);
        assert_eq!(stats.index_entries, 1);

        assert_eq!(
            reopened.get("chunk-a", 0, 100).await,
            Some(bytes_of("hello"))
        );
        assert_eq!(
            reopened.get("chunk-b", 10, 50).await,
            Some(bytes_of("world"))
        );

        // Garbage file must have been deleted during rebuild.
        assert!(!garbage_shard.join("not-an-entry.txt").exists());
    }

    #[tokio::test]
    async fn idempotent_double_put() {
        let dir = tempfile::tempdir().expect("tempdir");
        let cache = ChunkCache::open(Some(dir.path().to_path_buf()), 1024 * 1024)
            .await
            .expect("open");
        cache
            .put("chunk-a", 0, 100, bytes_of("hello"), CacheTier::Block)
            .await;
        cache
            .put("chunk-a", 0, 100, bytes_of("hello"), CacheTier::Block)
            .await;
        let stats = cache.stats();
        assert_eq!(stats.block_entries, 1);
    }

    #[tokio::test]
    async fn clear_removes_everything() {
        let dir = tempfile::tempdir().expect("tempdir");
        let cache = ChunkCache::open(Some(dir.path().to_path_buf()), 1024 * 1024)
            .await
            .expect("open");
        cache
            .put("chunk-a", 0, 100, bytes_of("hello"), CacheTier::Block)
            .await;
        cache.clear().await;
        let stats = cache.stats();
        assert_eq!(stats.total_bytes(), 0);
        assert!(cache.get("chunk-a", 0, 100).await.is_none());
    }
}
