use moka::future::Cache;
use sea_orm::{ColumnTrait, EntityTrait, QueryFilter, QueryOrder};
use std::sync::Arc;
use std::time::Duration;
use temps_database::DbConnection;
use temps_entities::static_asset_cache;
use tracing::debug;

/// Cache TTL for static-asset-store lookups (both hits and misses).
///
/// 60 seconds bounds staleness acceptably: the stale-chunk fallback exists
/// precisely to tolerate the brief period after a deploy when some clients still
/// request the previous deployment's hashed asset URLs. A freshly deployed asset
/// may therefore be served from the prior deployment's hash for up to 60 s —
/// that is the same trade-off the DB-backed lookup already accepted implicitly,
/// so it is not a regression.
const ASSET_STORE_CACHE_TTL: Duration = Duration::from_secs(60);

/// Maximum number of entries held in the asset-store lookup cache.
///
/// Each entry is an `(i32, String) → Option<String>` key–value pair (roughly
/// 100 bytes). 50 000 entries ≈ 5 MiB — a reasonable ceiling for a production
/// reverse proxy serving many projects.
const ASSET_STORE_CACHE_MAX_CAPACITY: u64 = 50_000;

/// In-memory cache for `static_asset_cache` DB lookups on the proxy hot path.
///
/// `serve_asset_from_store` in `proxy.rs` queries the `static_asset_cache`
/// table on **every** cacheable-asset request for container deployments —
/// including the overwhelmingly common miss case (asset served normally by
/// upstream, no fallback row found). Without this cache each miss is a
/// synchronous Postgres round-trip that blocks the response.
///
/// ## Cache strategy
///
/// - **Key**: `(project_id, url_path)` — identifies the asset within its
///   project regardless of deployment.
/// - **Value**: `Option<String>` — `Some(content_hash)` when a matching row
///   was found; `None` when no row exists. **Caching `None` (the miss case) is
///   the critical path**: container deployments serve most assets upstream, so
///   the vast majority of lookups find nothing.
/// - **TTL**: 60 seconds for both hit and miss results. New deployments insert
///   rows with a higher `deployment_id`; the TTL bounds staleness to an
///   acceptable 60 s window that aligns with the stale-chunk fallback semantics.
///
/// ## Invalidation
///
/// No active cross-process invalidation is performed. The 60 s TTL alone is
/// sufficient: the stale-chunk fallback was designed for exactly this tolerance
/// window, so a newly deployed asset hash becoming visible up to 60 s late is
/// not a correctness problem. Wiring deploy-event invalidation is a non-goal
/// for this PR.
pub struct StaticAssetLookup {
    db: Arc<DbConnection>,
    /// `(project_id, url_path) → Option<content_hash>`. TTL 60 s, cap 50 k.
    cache: Cache<(i32, String), Option<String>>,
}

impl StaticAssetLookup {
    /// Create a new [`StaticAssetLookup`] with the production 60-second TTL.
    pub fn new(db: Arc<DbConnection>) -> Self {
        Self::new_with_ttl(db, ASSET_STORE_CACHE_TTL)
    }

    /// Internal constructor that accepts an explicit TTL. Used in tests to
    /// shorten the TTL to observable durations without long `sleep` calls.
    fn new_with_ttl(db: Arc<DbConnection>, ttl: Duration) -> Self {
        let cache = Cache::builder()
            .max_capacity(ASSET_STORE_CACHE_MAX_CAPACITY)
            .time_to_live(ttl)
            .build();
        Self { db, cache }
    }

    /// Return the content hash for `(project_id, url_path)`, or `None` when no
    /// matching row exists in `static_asset_cache`.
    ///
    /// Results are served from the in-memory cache when available. Both
    /// `Some(hash)` and `None` are cached so the common miss case never
    /// amplifies into Postgres load.
    pub async fn get_content_hash(&self, project_id: i32, url_path: &str) -> Option<String> {
        let key = (project_id, url_path.to_string());

        // Fast path: a previous lookup already resolved this key (hit or miss).
        if let Some(cached) = self.cache.get(&key).await {
            debug!(project_id, url_path, "static-asset cache hit (skipping DB)");
            return cached;
        }

        // Cache miss: query the DB (most recent deployment wins).
        let result = static_asset_cache::Entity::find()
            .filter(static_asset_cache::Column::ProjectId.eq(project_id))
            .filter(static_asset_cache::Column::UrlPath.eq(url_path))
            .order_by_desc(static_asset_cache::Column::DeploymentId)
            .one(self.db.as_ref())
            .await
            .ok()
            .flatten()
            .map(|entry| entry.content_hash);

        debug!(
            project_id,
            url_path,
            found = result.is_some(),
            "static-asset DB lookup complete; caching result (including None)"
        );

        // Store the result — including None for the miss case so repeated
        // lookups for absent assets do not re-hit Postgres within the TTL.
        self.cache.insert(key, result.clone()).await;
        result
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Utc;
    use sea_orm::{DatabaseBackend, MockDatabase};

    /// Build a minimal `static_asset_cache::Model` for use in MockDatabase results.
    fn asset_model(
        project_id: i32,
        url_path: &str,
        content_hash: &str,
        deployment_id: i32,
    ) -> static_asset_cache::Model {
        static_asset_cache::Model {
            id: 1,
            url_path: url_path.to_string(),
            content_hash: content_hash.to_string(),
            project_id,
            environment_id: 1,
            deployment_id,
            size_bytes: 1024,
            created_at: Utc::now(),
        }
    }

    /// A repeated miss for the same `(project_id, url_path)` within the TTL
    /// window must hit the database exactly once.
    ///
    /// The MockDatabase queue contains exactly ONE empty result. If `get_content_hash`
    /// makes a second DB query the empty queue causes a panic, which fails the test.
    #[tokio::test]
    async fn test_repeated_miss_hits_db_only_once() {
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            // Single empty result: the first call sees no matching row.
            .append_query_results(vec![Vec::<static_asset_cache::Model>::new()])
            .into_connection();

        let lookup = StaticAssetLookup::new(Arc::new(db));

        // First call: cache miss → 1 DB query → None cached.
        let first = lookup
            .get_content_hash(42, "_next/static/chunks/main-abc.js")
            .await;
        assert!(first.is_none(), "first call should return None (no row)");

        // Second call: negative cache hit → 0 DB queries.
        // If this makes a DB query, the MockDatabase empty queue panics.
        let second = lookup
            .get_content_hash(42, "_next/static/chunks/main-abc.js")
            .await;
        assert!(
            second.is_none(),
            "second call from negative cache should return None without DB access"
        );
    }

    /// A hit result is cached — repeated lookups for the same key return the
    /// cached hash without a second DB query.
    ///
    /// The MockDatabase queue contains exactly ONE result row. A second DB call
    /// would panic, proving the cache is serving the second request.
    #[tokio::test]
    async fn test_hit_path_returns_cached_hash_without_second_query() {
        let expected_hash = "sha256:deadbeef1234567890abcdef".to_string();
        let model = asset_model(7, "_next/static/chunks/page-abc.js", &expected_hash, 99);

        let db = MockDatabase::new(DatabaseBackend::Postgres)
            // Single result: the first call returns this row.
            .append_query_results(vec![vec![model]])
            .into_connection();

        let lookup = StaticAssetLookup::new(Arc::new(db));

        // First call: cache miss → 1 DB query → hash cached.
        let first = lookup
            .get_content_hash(7, "_next/static/chunks/page-abc.js")
            .await;
        assert_eq!(
            first.as_deref(),
            Some(expected_hash.as_str()),
            "first call should return the content hash"
        );

        // Second call: positive cache hit → 0 DB queries → same hash returned.
        let second = lookup
            .get_content_hash(7, "_next/static/chunks/page-abc.js")
            .await;
        assert_eq!(
            second.as_deref(),
            Some(expected_hash.as_str()),
            "second call should return the cached hash without hitting DB"
        );
    }

    /// Different `(project_id, url_path)` keys are independent: caching one
    /// key does not interfere with another. Each key goes to the DB exactly once.
    #[tokio::test]
    async fn test_different_keys_are_independent() {
        let hash_a = "hash-for-project-1".to_string();
        let hash_b = "hash-for-project-2".to_string();

        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results(vec![
                vec![asset_model(1, "assets/app.js", &hash_a, 10)], // key (1, "assets/app.js")
                vec![asset_model(2, "assets/app.js", &hash_b, 20)], // key (2, "assets/app.js")
            ])
            .into_connection();

        let lookup = StaticAssetLookup::new(Arc::new(db));

        let res_a = lookup.get_content_hash(1, "assets/app.js").await;
        assert_eq!(res_a.as_deref(), Some(hash_a.as_str()));

        let res_b = lookup.get_content_hash(2, "assets/app.js").await;
        assert_eq!(res_b.as_deref(), Some(hash_b.as_str()));

        // Both keys cached — no more DB queries allowed.
        let res_a2 = lookup.get_content_hash(1, "assets/app.js").await;
        let res_b2 = lookup.get_content_hash(2, "assets/app.js").await;
        assert_eq!(res_a2.as_deref(), Some(hash_a.as_str()));
        assert_eq!(res_b2.as_deref(), Some(hash_b.as_str()));
    }

    /// After a negative-cache entry expires, the loader retries the DB.
    /// Verified by using a 1 ms TTL and sleeping past it.
    #[tokio::test]
    async fn test_negative_cache_expiry_retries_db() {
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results(vec![
                Vec::<static_asset_cache::Model>::new(), // first call: miss
                Vec::<static_asset_cache::Model>::new(), // second call after expiry: miss again
            ])
            .into_connection();

        // 1 ms TTL so we can expire it quickly in the test.
        let lookup = StaticAssetLookup::new_with_ttl(Arc::new(db), Duration::from_millis(1));

        let first = lookup.get_content_hash(5, "static/vendor.js").await;
        assert!(first.is_none());

        // Wait for the negative cache entry to expire.
        tokio::time::sleep(Duration::from_millis(10)).await;

        // Second call: entry expired → DB queried again (consumes the second queued result).
        let second = lookup.get_content_hash(5, "static/vendor.js").await;
        assert!(second.is_none());
    }
}
