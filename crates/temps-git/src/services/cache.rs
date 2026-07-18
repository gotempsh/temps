use chrono::{DateTime, Duration as ChronoDuration, Utc};
use std::collections::{HashMap, VecDeque};
use std::hash::Hash;
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::sync::{Mutex, RwLock};

const DEFAULT_CACHE_MAX_ENTRIES: usize = 10_000;
const COMMIT_LOOKUP_REQUESTS_PER_MINUTE: usize = 60;
const COMMIT_LOOKUP_MAX_PRINCIPALS: usize = 10_000;

/// Cache entry with expiration time
#[derive(Clone, Debug)]
struct CacheEntry<T> {
    value: T,
    expires_at: DateTime<Utc>,
}

impl<T> CacheEntry<T> {
    fn new(value: T, ttl_minutes: i64) -> Self {
        Self {
            value,
            expires_at: Utc::now() + ChronoDuration::minutes(ttl_minutes),
        }
    }

    fn is_expired(&self) -> bool {
        Utc::now() > self.expires_at
    }
}

/// Generic time-based cache for any serializable data
pub struct GitProviderCache<K, V>
where
    K: Eq + Hash + Clone,
    V: Clone,
{
    cache: Arc<RwLock<HashMap<K, CacheEntry<V>>>>,
    default_ttl_minutes: i64,
    max_entries: usize,
}

impl<K, V> GitProviderCache<K, V>
where
    K: Eq + Hash + Clone,
    V: Clone,
{
    /// Create a new cache with the given default TTL in minutes
    pub fn new(default_ttl_minutes: i64) -> Self {
        Self::new_with_capacity(default_ttl_minutes, DEFAULT_CACHE_MAX_ENTRIES)
    }

    /// Create a cache with an explicit entry bound.
    pub fn new_with_capacity(default_ttl_minutes: i64, max_entries: usize) -> Self {
        Self {
            cache: Arc::new(RwLock::new(HashMap::new())),
            default_ttl_minutes,
            max_entries: max_entries.max(1),
        }
    }

    /// Get a value from the cache if it exists and is not expired
    pub async fn get(&self, key: &K) -> Option<V> {
        let cache = self.cache.read().await;

        if let Some(entry) = cache.get(key) {
            if !entry.is_expired() {
                return Some(entry.value.clone());
            }
        }

        None
    }

    /// Set a value in the cache with the default TTL
    pub async fn set(&self, key: K, value: V) {
        self.set_with_ttl(key, value, self.default_ttl_minutes)
            .await;
    }

    /// Set a value in the cache with a custom TTL
    pub async fn set_with_ttl(&self, key: K, value: V, ttl_minutes: i64) {
        let mut cache = self.cache.write().await;
        if cache.len() >= self.max_entries && !cache.contains_key(&key) {
            cache.retain(|_, entry| !entry.is_expired());
        }
        if cache.len() >= self.max_entries && !cache.contains_key(&key) {
            if let Some(eviction_key) = cache.keys().next().cloned() {
                cache.remove(&eviction_key);
            }
        }
        cache.insert(key, CacheEntry::new(value, ttl_minutes));
    }

    /// Invalidate (remove) a specific cache entry
    pub async fn invalidate(&self, key: &K) {
        let mut cache = self.cache.write().await;
        cache.remove(key);
    }

    /// Clear all cache entries
    pub async fn clear(&self) {
        let mut cache = self.cache.write().await;
        cache.clear();
    }

    /// Remove all expired entries from the cache
    pub async fn cleanup_expired(&self) {
        let mut cache = self.cache.write().await;
        cache.retain(|_, entry| !entry.is_expired());
    }

    /// Get the number of entries in the cache (including expired)
    pub async fn len(&self) -> usize {
        let cache = self.cache.read().await;
        cache.len()
    }

    /// Check if the cache is empty
    pub async fn is_empty(&self) -> bool {
        let cache = self.cache.read().await;
        cache.is_empty()
    }
}

/// Cache key for repository branches
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct BranchCacheKey {
    pub connection_id: i32,
    pub owner: String,
    pub repo: String,
}

impl BranchCacheKey {
    pub fn new(connection_id: i32, owner: String, repo: String) -> Self {
        Self {
            connection_id,
            owner,
            repo,
        }
    }
}

/// Cache key for repository tags
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct TagCacheKey {
    pub connection_id: i32,
    pub owner: String,
    pub repo: String,
}

impl TagCacheKey {
    pub fn new(connection_id: i32, owner: String, repo: String) -> Self {
        Self {
            connection_id,
            owner,
            repo,
        }
    }
}

/// Cache key for immutable commit metadata lookups
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct CommitCacheKey {
    pub connection_id: i32,
    pub owner: String,
    pub repo: String,
    pub commit_ref: String,
}

#[derive(Debug)]
struct CommitLookupWindow {
    requests: VecDeque<Instant>,
    last_seen: Instant,
}

/// Bounded, per-principal sliding-window limiter for provider commit lookups.
pub struct CommitLookupRateLimiter {
    windows: Mutex<HashMap<String, CommitLookupWindow>>,
    max_requests: usize,
    window: Duration,
    max_principals: usize,
}

impl CommitLookupRateLimiter {
    pub fn new(max_requests: usize, window: Duration, max_principals: usize) -> Self {
        Self {
            windows: Mutex::new(HashMap::new()),
            max_requests: max_requests.max(1),
            window,
            max_principals: max_principals.max(1),
        }
    }

    /// Record an upstream lookup, returning the retry delay when the limit is reached.
    pub async fn check(&self, principal: &str) -> Result<(), u64> {
        let now = Instant::now();
        let cutoff = now.checked_sub(self.window).unwrap_or(now);
        let mut windows = self.windows.lock().await;

        windows.retain(|_, entry| entry.last_seen >= cutoff);
        if windows.len() >= self.max_principals && !windows.contains_key(principal) {
            if let Some(oldest) = windows
                .iter()
                .min_by_key(|(_, entry)| entry.last_seen)
                .map(|(key, _)| key.clone())
            {
                windows.remove(&oldest);
            }
        }

        let entry = windows
            .entry(principal.to_string())
            .or_insert_with(|| CommitLookupWindow {
                requests: VecDeque::new(),
                last_seen: now,
            });
        entry.last_seen = now;
        while entry
            .requests
            .front()
            .is_some_and(|request| *request < cutoff)
        {
            entry.requests.pop_front();
        }

        if entry.requests.len() >= self.max_requests {
            let retry_after = entry
                .requests
                .front()
                .map(|request| self.window.saturating_sub(now.duration_since(*request)))
                .unwrap_or(self.window);
            return Err(retry_after.as_secs().max(1));
        }

        entry.requests.push_back(now);
        Ok(())
    }
}

impl CommitCacheKey {
    pub fn new(connection_id: i32, owner: String, repo: String, commit_ref: String) -> Self {
        Self {
            connection_id,
            owner,
            repo,
            commit_ref,
        }
    }
}

/// Cache key for public repository branches (no connection_id required)
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct PublicBranchCacheKey {
    pub provider: String,
    pub owner: String,
    pub repo: String,
}

impl PublicBranchCacheKey {
    pub fn new(provider: String, owner: String, repo: String) -> Self {
        Self {
            provider,
            owner,
            repo,
        }
    }
}

/// Cache key for public repository preset detection
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct PublicPresetCacheKey {
    pub provider: String,
    pub owner: String,
    pub repo: String,
    pub branch: String,
}

impl PublicPresetCacheKey {
    pub fn new(provider: String, owner: String, repo: String, branch: String) -> Self {
        Self {
            provider,
            owner,
            repo,
            branch,
        }
    }
}

/// Preset info for public repository cache
#[derive(Debug, Clone)]
pub struct CachedPresetInfo {
    pub path: String,
    pub preset: String,
    pub preset_label: String,
    pub exposed_port: Option<i32>,
    pub icon_url: Option<String>,
    pub project_type: String,
    pub compose_files: Option<Vec<String>>,
}

/// Aggregated cache manager for all Git provider caches
pub struct GitProviderCacheManager {
    /// Cache for repository branches (60 minutes TTL)
    pub branches: GitProviderCache<BranchCacheKey, Vec<crate::services::git_provider::Branch>>,

    /// Cache for repository tags (60 minutes TTL)
    pub tags: GitProviderCache<TagCacheKey, Vec<crate::services::git_provider::GitProviderTag>>,

    /// Cache for immutable commit metadata and negative lookups (30 minutes TTL)
    pub commits: GitProviderCache<CommitCacheKey, Option<crate::services::git_provider::Commit>>,

    /// Per-user or API-key limiter for cache-miss provider lookups.
    pub commit_lookup_rate_limiter: CommitLookupRateLimiter,

    /// Cache for public repository branches (15 minutes TTL - shorter due to rate limits)
    pub public_branches:
        GitProviderCache<PublicBranchCacheKey, Vec<crate::services::git_provider::Branch>>,

    /// Cache for public repository presets (30 minutes TTL)
    pub public_presets: GitProviderCache<PublicPresetCacheKey, Vec<CachedPresetInfo>>,
}

impl GitProviderCacheManager {
    pub fn new() -> Self {
        Self {
            branches: GitProviderCache::new(60), // 60 minutes for branches
            tags: GitProviderCache::new(60),     // 60 minutes for tags
            commits: GitProviderCache::new(30),  // 30 minutes for commits
            commit_lookup_rate_limiter: CommitLookupRateLimiter::new(
                COMMIT_LOOKUP_REQUESTS_PER_MINUTE,
                Duration::from_secs(60),
                COMMIT_LOOKUP_MAX_PRINCIPALS,
            ),
            public_branches: GitProviderCache::new(15), // 15 minutes for public branches (rate limit aware)
            public_presets: GitProviderCache::new(30),  // 30 minutes for public presets
        }
    }

    /// Cleanup expired entries from all caches
    pub async fn cleanup_all_expired(&self) {
        self.branches.cleanup_expired().await;
        self.tags.cleanup_expired().await;
        self.commits.cleanup_expired().await;
        self.public_branches.cleanup_expired().await;
        self.public_presets.cleanup_expired().await;
    }

    /// Clear all caches
    pub async fn clear_all(&self) {
        self.branches.clear().await;
        self.tags.clear().await;
        self.commits.clear().await;
        self.public_branches.clear().await;
        self.public_presets.clear().await;
    }
}

impl Default for GitProviderCacheManager {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_cache_basic_operations() {
        let cache: GitProviderCache<String, String> = GitProviderCache::new(1);

        // Test set and get
        cache.set("key1".to_string(), "value1".to_string()).await;
        assert_eq!(
            cache.get(&"key1".to_string()).await,
            Some("value1".to_string())
        );

        // Test non-existent key
        assert_eq!(cache.get(&"nonexistent".to_string()).await, None);

        // Test invalidate
        cache.invalidate(&"key1".to_string()).await;
        assert_eq!(cache.get(&"key1".to_string()).await, None);
    }

    #[tokio::test]
    async fn test_cache_expiration() {
        let cache: GitProviderCache<String, String> = GitProviderCache::new(1);

        // Set with very short TTL (negative means already expired)
        cache
            .set_with_ttl("key1".to_string(), "value1".to_string(), -1)
            .await;

        // Should return None because it's expired
        assert_eq!(cache.get(&"key1".to_string()).await, None);
    }

    #[tokio::test]
    async fn test_cache_cleanup() {
        let cache: GitProviderCache<String, String> = GitProviderCache::new(1);

        // Add expired and valid entries
        cache
            .set_with_ttl("expired".to_string(), "value".to_string(), -1)
            .await;
        cache.set("valid".to_string(), "value".to_string()).await;

        assert_eq!(cache.len().await, 2);

        // Cleanup should remove expired entries
        cache.cleanup_expired().await;
        assert_eq!(cache.len().await, 1);
        assert_eq!(
            cache.get(&"valid".to_string()).await,
            Some("value".to_string())
        );
    }

    #[tokio::test]
    async fn cache_never_exceeds_its_entry_bound() {
        let cache: GitProviderCache<String, String> = GitProviderCache::new_with_capacity(1, 2);

        cache.set("one".to_string(), "1".to_string()).await;
        cache.set("two".to_string(), "2".to_string()).await;
        cache.set("three".to_string(), "3".to_string()).await;

        assert_eq!(cache.len().await, 2);
        assert_eq!(cache.get(&"three".to_string()).await.as_deref(), Some("3"));
    }

    #[tokio::test]
    async fn commit_cache_distinguishes_negative_result_from_cache_miss() {
        let cache: GitProviderCache<String, Option<crate::services::git_provider::Commit>> =
            GitProviderCache::new(1);

        cache.set("missing".to_string(), None).await;

        assert!(cache.get(&"unknown".to_string()).await.is_none());
        assert!(matches!(
            cache.get(&"missing".to_string()).await,
            Some(None)
        ));
    }

    #[tokio::test]
    async fn commit_lookup_rate_limiter_is_scoped_by_principal() {
        let limiter = CommitLookupRateLimiter::new(1, Duration::from_secs(60), 10);

        assert!(limiter.check("user:1").await.is_ok());
        assert!(limiter.check("user:1").await.is_err());
        assert!(limiter.check("api-key:1").await.is_ok());
    }

    #[tokio::test]
    async fn commit_lookup_rate_limiter_bounds_principal_memory() {
        let limiter = CommitLookupRateLimiter::new(1, Duration::from_secs(60), 2);

        assert!(limiter.check("user:1").await.is_ok());
        assert!(limiter.check("user:2").await.is_ok());
        assert!(limiter.check("user:3").await.is_ok());

        assert!(limiter.windows.lock().await.len() <= 2);
    }

    #[tokio::test]
    async fn test_branch_cache_key() {
        let key1 = BranchCacheKey::new(1, "owner".to_string(), "repo".to_string());
        let key2 = BranchCacheKey::new(1, "owner".to_string(), "repo".to_string());
        let key3 = BranchCacheKey::new(2, "owner".to_string(), "repo".to_string());

        assert_eq!(key1, key2);
        assert_ne!(key1, key3);
    }

    #[tokio::test]
    async fn test_cache_manager() {
        let manager = GitProviderCacheManager::new();

        // Test that all caches are initialized
        assert!(manager.branches.is_empty().await);
        assert!(manager.tags.is_empty().await);
        assert!(manager.commits.is_empty().await);

        // Test clear_all
        let key = BranchCacheKey::new(1, "owner".to_string(), "repo".to_string());
        manager.branches.set(key.clone(), vec![]).await;
        manager.clear_all().await;
        assert!(manager.branches.is_empty().await);
    }
}
