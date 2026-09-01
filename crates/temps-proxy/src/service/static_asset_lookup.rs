// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

use moka::future::Cache;
use sea_orm::{ColumnTrait, EntityTrait, QueryFilter};
use sha2::{Digest, Sha256};
use std::collections::HashSet;
use std::sync::Arc;
use std::time::Duration;
use temps_core::static_files::MAX_STATIC_ASSET_URL_PATH_BYTES;
use temps_database::DbConnection;
use temps_entities::{deployments, static_asset_cache};
use tokio::sync::Semaphore;
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
/// Each entry stores a fixed-size SHA-256 URL key plus a validated 64-byte
/// content hash and declared size. 50 000 entries therefore remain a real,
/// explicit memory ceiling independent of attacker-controlled URL length.
const ASSET_STORE_CACHE_MAX_CAPACITY: u64 = 50_000;

/// Maximum number of uncached CAS metadata queries admitted by this proxy.
///
/// Cache hits bypass this limit. Cache misses shed immediately when all slots
/// are occupied so attacker-controlled unique paths cannot queue unbounded DB
/// work or retain one waiting task per request.
const MAX_CONCURRENT_ASSET_STORE_DB_LOOKUPS: usize = 32;

#[derive(Debug)]
struct AssetStoreLookupSaturated;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StaticAssetMetadata {
    pub content_hash: String,
    pub size_bytes: i64,
}

#[derive(Clone, Copy, Debug, Hash, PartialEq, Eq)]
struct StaticAssetCacheKey {
    project_id: i32,
    environment_id: i32,
    deployment_id: i32,
    url_path_digest: [u8; 32],
}

#[derive(Clone, Copy, Debug, Hash, PartialEq, Eq)]
struct LegacyAssetOriginCacheKey {
    project_id: i32,
    source_deployment_id: i32,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LegacyAssetOrigin {
    pub deployment_id: i32,
    pub environment_id: i32,
    pub slug: String,
}

impl StaticAssetCacheKey {
    fn new(project_id: i32, environment_id: i32, deployment_id: i32, url_path: &str) -> Self {
        let digest = Sha256::digest(url_path.as_bytes());
        let mut url_path_digest = [0_u8; 32];
        url_path_digest.copy_from_slice(&digest);
        Self {
            project_id,
            environment_id,
            deployment_id,
            url_path_digest,
        }
    }
}

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
/// - **Key**: project/environment/deployment IDs plus a fixed SHA-256 URL digest
///   — binds the asset to the exact route context without retaining raw URLs.
/// - **Value**: optional content hash and declared size metadata when a matching
///   row was found; `None` when no row exists. **Caching `None` (the miss case)
///   is the critical path**: container deployments serve most assets upstream,
///   so the vast majority of lookups find nothing.
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
    /// `(project_id, environment_id, deployment_id, url_path) → asset metadata`.
    cache: Cache<StaticAssetCacheKey, Option<StaticAssetMetadata>>,
    /// Legacy reuse rows omitted the source slug. Cache the resolved immutable
    /// build origin so compatibility never adds an uncached query per asset.
    legacy_origin_cache: Cache<LegacyAssetOriginCacheKey, Option<LegacyAssetOrigin>>,
    db_lookup_slots: Arc<Semaphore>,
}

impl StaticAssetLookup {
    /// Create a new [`StaticAssetLookup`] with the production 60-second TTL.
    pub fn new(db: Arc<DbConnection>) -> Self {
        Self::new_with_ttl(db, ASSET_STORE_CACHE_TTL)
    }

    /// Internal constructor that accepts an explicit TTL. Used in tests to
    /// shorten the TTL to observable durations without long `sleep` calls.
    fn new_with_ttl(db: Arc<DbConnection>, ttl: Duration) -> Self {
        Self::new_with_ttl_and_limit(db, ttl, MAX_CONCURRENT_ASSET_STORE_DB_LOOKUPS)
    }

    fn new_with_ttl_and_limit(
        db: Arc<DbConnection>,
        ttl: Duration,
        max_concurrent_db_lookups: usize,
    ) -> Self {
        let cache = Cache::builder()
            .max_capacity(ASSET_STORE_CACHE_MAX_CAPACITY)
            .time_to_live(ttl)
            .build();
        let legacy_origin_cache = Cache::builder()
            .max_capacity(ASSET_STORE_CACHE_MAX_CAPACITY)
            .time_to_live(ttl)
            .build();
        Self {
            db,
            cache,
            legacy_origin_cache,
            db_lookup_slots: Arc::new(Semaphore::new(max_concurrent_db_lookups)),
        }
    }

    /// Resolve the immutable build behind pre-source-slug promotion and
    /// rollback metadata. Results (including missing/cyclic metadata) are
    /// cached and project-scoped to prevent cross-project ID traversal.
    pub async fn resolve_legacy_asset_origin(
        &self,
        project_id: i32,
        source_deployment_id: i32,
    ) -> Option<LegacyAssetOrigin> {
        let key = LegacyAssetOriginCacheKey {
            project_id,
            source_deployment_id,
        };
        if let Some(cached) = self.legacy_origin_cache.get(&key).await {
            return cached;
        }

        let db = self.db.clone();
        let db_lookup_slots = self.db_lookup_slots.clone();
        self.legacy_origin_cache
            .try_get_with(key, async move {
                let _permit = db_lookup_slots
                    .try_acquire_owned()
                    .map_err(|_| AssetStoreLookupSaturated)?;
                let mut current_id = source_deployment_id;
                let mut visited = HashSet::new();

                loop {
                    if !visited.insert(current_id) {
                        return Ok::<Option<LegacyAssetOrigin>, AssetStoreLookupSaturated>(None);
                    }
                    let Some(deployment) = deployments::Entity::find_by_id(current_id)
                        .filter(deployments::Column::ProjectId.eq(project_id))
                        .one(db.as_ref())
                        .await
                        .ok()
                        .flatten()
                    else {
                        return Ok::<Option<LegacyAssetOrigin>, AssetStoreLookupSaturated>(None);
                    };

                    let context = deployment.context_vars.as_ref();
                    let nested_source_id = context
                        .and_then(|value| value.get("source_deployment_id"))
                        .and_then(serde_json::Value::as_i64)
                        .and_then(|value| i32::try_from(value).ok());
                    let complete_source_environment_id = context
                        .and_then(|value| value.get("source_environment_id"))
                        .and_then(serde_json::Value::as_i64)
                        .and_then(|value| i32::try_from(value).ok());
                    let complete_source_slug = context
                        .and_then(|value| value.get("source_deployment_slug"))
                        .and_then(serde_json::Value::as_str);

                    match (
                        nested_source_id,
                        complete_source_environment_id,
                        complete_source_slug,
                    ) {
                        (Some(deployment_id), Some(environment_id), Some(slug)) => {
                            return Ok::<Option<LegacyAssetOrigin>, AssetStoreLookupSaturated>(
                                Some(LegacyAssetOrigin {
                                    deployment_id,
                                    environment_id,
                                    slug: slug.to_string(),
                                }),
                            );
                        }
                        (Some(next_id), _, _) => current_id = next_id,
                        (None, _, _) => {
                            return Ok::<Option<LegacyAssetOrigin>, AssetStoreLookupSaturated>(
                                Some(LegacyAssetOrigin {
                                    deployment_id: deployment.id,
                                    environment_id: deployment.environment_id,
                                    slug: deployment.slug,
                                }),
                            );
                        }
                    }
                }
            })
            .await
            .ok()
            .flatten()
    }

    /// Return bounded-serving metadata for the exact routed deployment, or
    /// `None` when no matching row exists in `static_asset_cache`.
    ///
    /// Results are served from the in-memory cache when available. Both
    /// Hits and `None` are cached so the common miss case never
    /// amplifies into Postgres load.
    pub async fn get_asset_metadata(
        &self,
        project_id: i32,
        environment_id: i32,
        deployment_id: i32,
        url_path: &str,
    ) -> Option<StaticAssetMetadata> {
        if url_path.len() > MAX_STATIC_ASSET_URL_PATH_BYTES {
            return None;
        }
        let key = StaticAssetCacheKey::new(project_id, environment_id, deployment_id, url_path);

        // Fast path: a previous lookup already resolved this key (hit or miss).
        if let Some(cached) = self.cache.get(&key).await {
            debug!(project_id, "static-asset cache hit (skipping DB)");
            return cached;
        }

        let db = self.db.clone();
        let db_lookup_slots = self.db_lookup_slots.clone();
        let bounded_url_path = url_path.to_owned();
        let loaded = self
            .cache
            .try_get_with(key, async move {
                // Never await a permit: shedding is what bounds both queued work
                // and memory when unique cache misses arrive concurrently.
                let _permit = db_lookup_slots
                    .try_acquire_owned()
                    .map_err(|_| AssetStoreLookupSaturated)?;

                let result = static_asset_cache::Entity::find()
                    .filter(static_asset_cache::Column::ProjectId.eq(project_id))
                    .filter(static_asset_cache::Column::EnvironmentId.eq(environment_id))
                    .filter(static_asset_cache::Column::DeploymentId.eq(deployment_id))
                    .filter(static_asset_cache::Column::UrlPath.eq(&bounded_url_path))
                    .one(db.as_ref())
                    .await
                    .ok()
                    .flatten()
                    .and_then(|entry| {
                        (entry.content_hash.len() == 64
                            && entry
                                .content_hash
                                .bytes()
                                .all(|byte| byte.is_ascii_hexdigit()))
                        .then_some(StaticAssetMetadata {
                            content_hash: entry.content_hash,
                            size_bytes: entry.size_bytes,
                        })
                    });

                Ok::<Option<StaticAssetMetadata>, AssetStoreLookupSaturated>(result)
            })
            .await;

        match loaded {
            Ok(result) => {
                debug!(
                    project_id,
                    found = result.is_some(),
                    "static-asset DB lookup complete; caching result (including None)"
                );
                result
            }
            Err(_) => {
                // `try_get_with` never caches loader errors. A later request can
                // retry once a slot is free instead of inheriting a negative TTL.
                debug!(
                    project_id,
                    "static-asset DB lookup shed at concurrency limit"
                );
                None
            }
        }
    }

    #[cfg(test)]
    async fn get_content_hash(
        &self,
        project_id: i32,
        environment_id: i32,
        deployment_id: i32,
        url_path: &str,
    ) -> Option<String> {
        self.get_asset_metadata(project_id, environment_id, deployment_id, url_path)
            .await
            .map(|asset| asset.content_hash)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Utc;
    use sea_orm::{DatabaseBackend, MockDatabase};

    fn valid_hash(character: char) -> String {
        character.to_string().repeat(64)
    }

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

    fn deployment_model(
        id: i32,
        project_id: i32,
        environment_id: i32,
        slug: &str,
        context_vars: Option<serde_json::Value>,
    ) -> deployments::Model {
        deployments::Model {
            id,
            project_id,
            environment_id,
            created_at: Utc::now(),
            updated_at: Utc::now(),
            slug: slug.to_string(),
            state: "deployed".to_string(),
            metadata: None,
            deploying_at: None,
            ready_at: None,
            started_at: None,
            finished_at: None,
            context_vars,
            branch_ref: None,
            tag_ref: None,
            commit_sha: None,
            commit_message: None,
            commit_author: None,
            commit_json: None,
            cancelled_reason: None,
            static_dir_location: None,
            screenshot_location: None,
            image_name: None,
            deployment_config: None,
            promoted_from_deployment_id: None,
        }
    }

    #[tokio::test]
    async fn legacy_reuse_origin_walks_to_original_build_and_is_cached() {
        let legacy_promotion = deployment_model(
            20,
            7,
            3,
            "legacy-promotion",
            Some(serde_json::json!({
                "trigger": "promotion",
                "source_deployment_id": 10,
                "source_environment_id": 2,
            })),
        );
        let original = deployment_model(10, 7, 2, "original-build", None);
        let db = Arc::new(
            MockDatabase::new(DatabaseBackend::Postgres)
                .append_query_results(vec![vec![legacy_promotion], vec![original]])
                .into_connection(),
        );
        let lookup = StaticAssetLookup::new(db.clone());

        let origin = lookup
            .resolve_legacy_asset_origin(7, 20)
            .await
            .expect("legacy origin should resolve");
        assert_eq!(
            origin,
            LegacyAssetOrigin {
                deployment_id: 10,
                environment_id: 2,
                slug: "original-build".to_string(),
            }
        );
        assert_eq!(
            lookup.resolve_legacy_asset_origin(7, 20).await,
            Some(origin)
        );

        drop(lookup);
        let db = Arc::try_unwrap(db).expect("lookup releases database");
        assert_eq!(db.into_transaction_log().len(), 2);
    }

    #[tokio::test]
    async fn legacy_reuse_origin_fails_closed_when_source_is_outside_project() {
        let db = Arc::new(
            MockDatabase::new(DatabaseBackend::Postgres)
                .append_query_results(vec![Vec::<deployments::Model>::new()])
                .into_connection(),
        );
        let lookup = StaticAssetLookup::new(db.clone());

        assert!(lookup.resolve_legacy_asset_origin(7, 99).await.is_none());

        drop(lookup);
        let db = Arc::try_unwrap(db).expect("lookup releases database");
        let transactions = db.into_transaction_log();
        assert_eq!(transactions.len(), 1);
        assert!(transactions[0].statements()[0]
            .to_string()
            .contains("\"project_id\" = 7"));
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
            .get_content_hash(42, 4, 9, "_next/static/chunks/main-abc.js")
            .await;
        assert!(first.is_none(), "first call should return None (no row)");

        // Second call: negative cache hit → 0 DB queries.
        // If this makes a DB query, the MockDatabase empty queue panics.
        let second = lookup
            .get_content_hash(42, 4, 9, "_next/static/chunks/main-abc.js")
            .await;
        assert!(
            second.is_none(),
            "second call from negative cache should return None without DB access"
        );
    }

    #[tokio::test]
    async fn concurrent_same_key_misses_are_coalesced_into_one_db_query() {
        let expected_hash = valid_hash('c');
        let model = asset_model(42, "assets/coalesced.js", &expected_hash, 9);
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results(vec![vec![model]])
            .into_connection();
        let lookup = StaticAssetLookup::new(Arc::new(db));

        let (first, second) = tokio::join!(
            lookup.get_content_hash(42, 1, 9, "assets/coalesced.js"),
            lookup.get_content_hash(42, 1, 9, "assets/coalesced.js")
        );

        assert_eq!(first.as_deref(), Some(expected_hash.as_str()));
        assert_eq!(second.as_deref(), Some(expected_hash.as_str()));
    }

    #[tokio::test]
    async fn exhausted_lookup_slots_shed_without_querying_or_negative_caching() {
        let expected_hash = valid_hash('d');
        let model = asset_model(8, "assets/retry.js", &expected_hash, 13);
        let db = Arc::new(
            MockDatabase::new(DatabaseBackend::Postgres)
                .append_query_results(vec![vec![model]])
                .into_connection(),
        );
        let lookup =
            StaticAssetLookup::new_with_ttl_and_limit(db.clone(), ASSET_STORE_CACHE_TTL, 1);
        let permit = lookup
            .db_lookup_slots
            .clone()
            .try_acquire_owned()
            .expect("test reserves the sole DB lookup slot");

        let shed = lookup.get_content_hash(8, 1, 13, "assets/retry.js").await;
        assert!(shed.is_none());

        drop(permit);
        let retried = lookup.get_content_hash(8, 1, 13, "assets/retry.js").await;
        assert_eq!(retried.as_deref(), Some(expected_hash.as_str()));

        drop(lookup);
        let db = Arc::try_unwrap(db).expect("lookup releases database");
        assert_eq!(db.into_transaction_log().len(), 1);
    }

    /// A hit result is cached — repeated lookups for the same key return the
    /// cached hash without a second DB query.
    ///
    /// The MockDatabase queue contains exactly ONE result row. A second DB call
    /// would panic, proving the cache is serving the second request.
    #[tokio::test]
    async fn test_hit_path_returns_cached_hash_without_second_query() {
        let expected_hash = valid_hash('a');
        let model = asset_model(7, "_next/static/chunks/page-abc.js", &expected_hash, 99);

        let db = MockDatabase::new(DatabaseBackend::Postgres)
            // Single result: the first call returns this row.
            .append_query_results(vec![vec![model]])
            .into_connection();

        let lookup = StaticAssetLookup::new(Arc::new(db));

        // First call: cache miss → 1 DB query → hash cached.
        let first = lookup
            .get_content_hash(7, 3, 99, "_next/static/chunks/page-abc.js")
            .await;
        assert_eq!(
            first.as_deref(),
            Some(expected_hash.as_str()),
            "first call should return the content hash"
        );

        // Second call: positive cache hit → 0 DB queries → same hash returned.
        let second = lookup
            .get_content_hash(7, 3, 99, "_next/static/chunks/page-abc.js")
            .await;
        assert_eq!(
            second.as_deref(),
            Some(expected_hash.as_str()),
            "second call should return the cached hash without hitting DB"
        );
    }

    #[tokio::test]
    async fn asset_metadata_includes_declared_size_for_pre_read_bounding() {
        let expected_hash = valid_hash('b');
        let model = asset_model(7, "assets/app.js", &expected_hash, 99);
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results(vec![vec![model]])
            .into_connection();
        let lookup = StaticAssetLookup::new(Arc::new(db));

        let metadata = lookup
            .get_asset_metadata(7, 1, 99, "assets/app.js")
            .await
            .expect("asset metadata");
        assert_eq!(metadata.content_hash, expected_hash);
        assert_eq!(metadata.size_bytes, 1024);
    }

    /// Different `(project_id, url_path)` keys are independent: caching one
    /// key does not interfere with another. Each key goes to the DB exactly once.
    #[tokio::test]
    async fn test_different_keys_are_independent() {
        let hash_a = valid_hash('a');
        let hash_b = valid_hash('b');

        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results(vec![
                vec![asset_model(1, "assets/app.js", &hash_a, 10)], // key (1, "assets/app.js")
                vec![asset_model(2, "assets/app.js", &hash_b, 20)], // key (2, "assets/app.js")
            ])
            .into_connection();

        let lookup = StaticAssetLookup::new(Arc::new(db));

        let res_a = lookup.get_content_hash(1, 1, 10, "assets/app.js").await;
        assert_eq!(res_a.as_deref(), Some(hash_a.as_str()));

        let res_b = lookup.get_content_hash(2, 1, 20, "assets/app.js").await;
        assert_eq!(res_b.as_deref(), Some(hash_b.as_str()));

        // Both keys cached — no more DB queries allowed.
        let res_a2 = lookup.get_content_hash(1, 1, 10, "assets/app.js").await;
        let res_b2 = lookup.get_content_hash(2, 1, 20, "assets/app.js").await;
        assert_eq!(res_a2.as_deref(), Some(hash_a.as_str()));
        assert_eq!(res_b2.as_deref(), Some(hash_b.as_str()));
    }

    #[tokio::test]
    async fn test_same_project_path_isolated_by_environment_and_deployment() {
        let hash_a = valid_hash('a');
        let hash_b = valid_hash('b');
        let mut env_a = asset_model(1, "assets/app.js", &hash_a, 10);
        env_a.environment_id = 11;
        let mut env_b = asset_model(1, "assets/app.js", &hash_b, 20);
        env_b.environment_id = 22;

        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results(vec![vec![env_a], vec![env_b]])
            .into_connection();
        let lookup = StaticAssetLookup::new(Arc::new(db));

        assert_eq!(
            lookup
                .get_content_hash(1, 11, 10, "assets/app.js")
                .await
                .as_deref(),
            Some(hash_a.as_str())
        );
        assert_eq!(
            lookup
                .get_content_hash(1, 22, 20, "assets/app.js")
                .await
                .as_deref(),
            Some(hash_b.as_str())
        );
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

        let first = lookup.get_content_hash(5, 2, 8, "static/vendor.js").await;
        assert!(first.is_none());

        // Wait for the negative cache entry to expire.
        tokio::time::sleep(Duration::from_millis(10)).await;

        // Second call: entry expired → DB queried again (consumes the second queued result).
        let second = lookup.get_content_hash(5, 2, 8, "static/vendor.js").await;
        assert!(second.is_none());
    }

    #[tokio::test]
    async fn oversized_url_path_never_queries_or_enters_the_cache() {
        let db = Arc::new(MockDatabase::new(DatabaseBackend::Postgres).into_connection());
        let lookup = StaticAssetLookup::new(db.clone());
        let oversized = "a".repeat(MAX_STATIC_ASSET_URL_PATH_BYTES + 1);

        assert!(lookup
            .get_asset_metadata(1, 2, 3, &oversized)
            .await
            .is_none());
        assert_eq!(lookup.cache.entry_count(), 0);

        drop(lookup);
        let db = Arc::try_unwrap(db).expect("lookup releases database");
        assert!(db.into_transaction_log().is_empty());
    }

    #[test]
    fn cache_key_has_fixed_size_independent_of_url_length() {
        let short = StaticAssetCacheKey::new(1, 2, 3, "a.js");
        let long = StaticAssetCacheKey::new(1, 2, 3, &"a".repeat(MAX_STATIC_ASSET_URL_PATH_BYTES));

        assert_ne!(short.url_path_digest, long.url_path_digest);
        assert!(std::mem::size_of::<StaticAssetCacheKey>() <= 48);
        assert!(!std::mem::needs_drop::<StaticAssetCacheKey>());
    }
}
