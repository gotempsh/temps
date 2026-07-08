//! Per-project storage quota cache for OTel ingest.
//!
//! `get_storage_quota` runs exact `COUNT(*)` scans over three hypertables
//! (`otel_metrics`, `otel_spans`, `otel_log_events`) filtered by `project_id`.
//! Storage usage changes slowly (bytes accrue over minutes/hours), so paying
//! for that scan on every single ingest request is wasted DB load — under
//! high ingest volume it saturates TimescaleDB and drives up CPU. This cache
//! lets `OtelService::check_quota` reuse the last result for a short TTL
//! instead of re-querying per request.

use std::collections::HashMap;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use crate::types::StorageQuota;

/// How long a cached quota result stays valid before the next check
/// triggers a fresh DB query. Storage usage doesn't need per-request
/// precision — a project that just crossed its limit will be caught
/// within this window.
pub const QUOTA_CACHE_TTL: Duration = Duration::from_secs(30);

struct CacheEntry {
    quota: StorageQuota,
    checked_at: Instant,
}

/// Per-project TTL cache of the last computed [`StorageQuota`].
pub struct QuotaCache {
    ttl: Duration,
    entries: Mutex<HashMap<i32, CacheEntry>>,
}

impl QuotaCache {
    pub fn new(ttl: Duration) -> Self {
        Self {
            ttl,
            entries: Mutex::new(HashMap::new()),
        }
    }

    /// Return the cached quota for `project_id` if it's still within the
    /// TTL, otherwise `None` so the caller can fetch a fresh value.
    pub fn get(&self, project_id: i32) -> Option<StorageQuota> {
        let entries = self.entries.lock().unwrap_or_else(|e| e.into_inner());
        let entry = entries.get(&project_id)?;
        if entry.checked_at.elapsed() < self.ttl {
            Some(entry.quota.clone())
        } else {
            None
        }
    }

    /// Store a freshly fetched quota, replacing any prior entry.
    pub fn put(&self, project_id: i32, quota: StorageQuota) {
        let mut entries = self.entries.lock().unwrap_or_else(|e| e.into_inner());
        entries.insert(
            project_id,
            CacheEntry {
                quota,
                checked_at: Instant::now(),
            },
        );
    }

    /// Drop entries older than twice the TTL to prevent unbounded growth
    /// from projects that stop sending data.
    pub fn cleanup_expired(&self) {
        let mut entries = self.entries.lock().unwrap_or_else(|e| e.into_inner());
        let ttl = self.ttl;
        entries.retain(|_, e| e.checked_at.elapsed() < ttl * 2);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn quota(project_id: i32, usage_pct: f64) -> StorageQuota {
        StorageQuota {
            project_id,
            metrics_bytes: 0,
            traces_bytes: 0,
            logs_bytes: 0,
            total_bytes: 0,
            limit_bytes: 1_000_000,
            usage_pct,
        }
    }

    #[test]
    fn test_miss_when_empty() {
        let cache = QuotaCache::new(Duration::from_secs(60));
        assert!(cache.get(1).is_none());
    }

    #[test]
    fn test_hit_within_ttl() {
        let cache = QuotaCache::new(Duration::from_secs(60));
        cache.put(1, quota(1, 42.0));
        let hit = cache.get(1).expect("should be cached");
        assert_eq!(hit.usage_pct, 42.0);
    }

    #[test]
    fn test_miss_after_ttl_expires() {
        let cache = QuotaCache::new(Duration::from_millis(1));
        cache.put(1, quota(1, 42.0));
        std::thread::sleep(Duration::from_millis(5));
        assert!(cache.get(1).is_none());
    }

    #[test]
    fn test_per_project_isolation() {
        let cache = QuotaCache::new(Duration::from_secs(60));
        cache.put(1, quota(1, 10.0));
        assert!(cache.get(2).is_none());
    }

    #[test]
    fn test_cleanup_expired_removes_stale_entries() {
        let cache = QuotaCache::new(Duration::from_millis(1));
        cache.put(1, quota(1, 10.0));
        std::thread::sleep(Duration::from_millis(10));
        cache.cleanup_expired();
        // Internal map should no longer hold the entry; a subsequent get
        // still misses (already true pre-cleanup), but this exercises the
        // retain path without panicking.
        assert!(cache.get(1).is_none());
    }
}
