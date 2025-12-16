use chrono::{DateTime, Duration, Utc};
use std::collections::HashMap;
use std::hash::Hash;
use std::sync::Arc;
use tokio::sync::RwLock;

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
            expires_at: Utc::now() + Duration::minutes(ttl_minutes),
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
}

impl<K, V> GitProviderCache<K, V>
where
    K: Eq + Hash + Clone,
    V: Clone,
{
    /// Create a new cache with the given default TTL in minutes
    pub fn new(default_ttl_minutes: i64) -> Self {
        Self {
            cache: Arc::new(RwLock::new(HashMap::new())),
            default_ttl_minutes,
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

/// Cache key for commit existence checks
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct CommitCacheKey {
    pub connection_id: i32,
    pub owner: String,
    pub repo: String,
    pub commit_ref: String,
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
}

/// Aggregated cache manager for all Git provider caches
pub struct GitProviderCacheManager {
    /// Cache for repository branches (60 minutes TTL)
    pub branches: GitProviderCache<BranchCacheKey, Vec<crate::services::git_provider::Branch>>,

    /// Cache for repository tags (60 minutes TTL)
    pub tags: GitProviderCache<TagCacheKey, Vec<crate::services::git_provider::GitProviderTag>>,

    /// Cache for commit existence checks (30 minutes TTL)
    pub commits: GitProviderCache<CommitCacheKey, bool>,

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
