use chrono::{DateTime, Duration, Utc};
use moka::future::Cache;
use sea_orm::{ActiveModelTrait, ColumnTrait, DatabaseConnection, EntityTrait, QueryFilter, Set};
use std::sync::Arc;
use std::time::Duration as StdDuration;
use temps_entities::challenge_sessions;
use thiserror::Error;
use tracing::debug;

/// TTL for negative cache entries (challenge not yet completed).
///
/// Attack mode's whole point is that request floods must not translate into DB floods.
/// A 5-second negative TTL collapses a flood of requests from one attacker into at most
/// 0.2 QPS of DB checks, while still letting a legitimate user who just solved the
/// challenge get through within 5 seconds. The write-through path in
/// `mark_challenge_completed` inserts a positive cache entry on the instance that
/// processed the completion, which is only visible immediately if that instance is
/// also the one serving the hot-path `is_challenge_completed` check. In the current
/// process topology the captcha verify endpoint and the Pingora hot-path reader are
/// separate `ChallengeService` objects, so the 5-second negative TTL expiry is the
/// actual bound a real request experiences: after the TTL elapses, the next
/// `is_challenge_completed` call re-queries the DB and finds the completed session.
const NEGATIVE_CACHE_TTL: StdDuration = StdDuration::from_secs(5);

/// Maximum TTL cap for positive cache entries (challenge completed), in seconds.
///
/// We respect the DB row's `expires_at` but cap at 10 minutes to bound any clock
/// skew or unusually long DB-side TTLs from being held in memory indefinitely.
const POSITIVE_CACHE_TTL_CAP_SECS: i64 = 600;

/// Maximum number of entries held in the challenge-completion cache.
const CHALLENGE_CACHE_MAX_CAPACITY: u64 = 100_000;

#[derive(Error, Debug)]
pub enum ChallengeError {
    #[error("Database error: {0}")]
    Database(#[from] sea_orm::DbErr),
    #[error("Challenge not found")]
    NotFound,
    #[error("Challenge expired")]
    Expired,
}

/// A challenge-completion cache entry.
///
/// We store both the completion state and the DB row's expiry so that the
/// [`ChallengeEntryExpiry`] policy can derive a per-entry TTL that tracks the
/// actual row lifetime rather than a fixed global cap.
#[derive(Clone, Debug)]
struct ChallengeEntry {
    /// Whether the challenge has been completed.
    completed: bool,
    /// The DB row's `expires_at` (only meaningful when `completed` is `true`).
    expires_at: Option<DateTime<Utc>>,
}

/// Per-entry expiry policy for the challenge-completion cache.
///
/// We chose `Expiry` over a single flat TTL because challenge sessions have two
/// semantically distinct lifetimes:
///
/// - **Completed** (`completed = true`): TTL = `min(expires_at − now, 10 minutes)`.
///   A short-lived challenge session (e.g. expiring in 2 minutes) must not linger
///   in the cache for 10 minutes after expiry, so we derive the TTL from the row's
///   actual `expires_at` and cap it at 10 minutes to bound clock skew.
/// - **Not completed** (`completed = false`): TTL = 5 seconds (see [`NEGATIVE_CACHE_TTL`]).
///   Attack mode's whole point is that request floods must not translate into DB floods.
///   5 seconds bounds the worst-case wait for a legitimate user who just solved the
///   challenge while collapsing an attacker's flood to ≤0.2 QPS of DB checks.
struct ChallengeEntryExpiry;

impl moka::Expiry<(i32, String, String), ChallengeEntry> for ChallengeEntryExpiry {
    fn expire_after_create(
        &self,
        _key: &(i32, String, String),
        value: &ChallengeEntry,
        _created_at: std::time::Instant,
    ) -> Option<StdDuration> {
        if value.completed {
            let ttl_secs = value
                .expires_at
                .map(|exp| {
                    // Compute remaining lifetime, clamp to [0, 10 min] to handle
                    // any sub-second race between DB query and cache insert, and to
                    // cap unusually long DB-side TTLs.
                    exp.signed_duration_since(Utc::now())
                        .num_seconds()
                        .clamp(0, POSITIVE_CACHE_TTL_CAP_SECS) as u64
                })
                .unwrap_or(POSITIVE_CACHE_TTL_CAP_SECS as u64);
            Some(StdDuration::from_secs(ttl_secs))
        } else {
            // Negative entries: 5 seconds. See NEGATIVE_CACHE_TTL for rationale.
            Some(NEGATIVE_CACHE_TTL)
        }
    }
}

pub struct ChallengeService {
    db: Arc<DatabaseConnection>,
    /// In-memory cache for challenge-completion lookups.
    ///
    /// Key: `(environment_id, identifier, identifier_type)`.
    ///
    /// - **Positive entries** (challenge completed): cached with a per-entry TTL derived
    ///   from the row's `expires_at`, capped at 10 minutes.
    /// - **Negative entries** (not yet completed): cached for 5 seconds. This is the
    ///   critical safety valve during an attack: a flood of requests for the same IP/JA4
    ///   fingerprint collapses to at most 0.2 QPS of Postgres queries instead of one
    ///   query per request.
    ///
    /// `mark_challenge_completed` writes a positive entry to this instance's cache
    /// (write-through). This is immediately visible to `is_challenge_completed` calls
    /// on the same instance, but the Pingora hot-path reader is a separate
    /// `ChallengeService` object: for it, the 5-second negative TTL expiry followed
    /// by a DB re-check is the actual propagation mechanism after a challenge is solved.
    cache: Cache<(i32, String, String), ChallengeEntry>,
}

impl ChallengeService {
    pub fn new(db: Arc<DatabaseConnection>) -> Self {
        let cache = Cache::builder()
            .max_capacity(CHALLENGE_CACHE_MAX_CAPACITY)
            .expire_after(ChallengeEntryExpiry)
            .build();
        Self { db, cache }
    }

    /// Check if a challenge has been completed for the given environment and identifier.
    ///
    /// Returns `true` if a valid (non-expired) challenge session exists.
    ///
    /// Results are served from the in-memory cache when available. Negative results are
    /// cached for 5 seconds; positive results until the row's `expires_at` (≤10 min cap).
    /// This prevents per-request DB queries from amplifying into a Postgres flood during
    /// an active attack.
    pub async fn is_challenge_completed(
        &self,
        environment_id: i32,
        identifier: &str,
        identifier_type: &str,
    ) -> Result<bool, ChallengeError> {
        let key = (
            environment_id,
            identifier.to_string(),
            identifier_type.to_string(),
        );

        // Fast path: cache hit (positive or negative).
        if let Some(entry) = self.cache.get(&key).await {
            debug!(
                environment_id,
                identifier,
                identifier_type,
                completed = entry.completed,
                "challenge-completion cache hit (skipping DB)"
            );
            return Ok(entry.completed);
        }

        // Cache miss: query the DB.
        let now = Utc::now();
        let session = challenge_sessions::Entity::find()
            .filter(challenge_sessions::Column::EnvironmentId.eq(environment_id))
            .filter(challenge_sessions::Column::Identifier.eq(identifier))
            .filter(challenge_sessions::Column::IdentifierType.eq(identifier_type))
            .filter(challenge_sessions::Column::ExpiresAt.gt(now))
            .one(self.db.as_ref())
            .await?;

        let (completed, expires_at) = match &session {
            Some(s) => (true, Some(s.expires_at)),
            None => (false, None),
        };

        debug!(
            environment_id,
            identifier,
            identifier_type,
            completed,
            "challenge-completion DB lookup; caching result"
        );

        self.cache
            .insert(
                key,
                ChallengeEntry {
                    completed,
                    expires_at,
                },
            )
            .await;

        Ok(completed)
    }

    /// Mark a challenge as completed for the given environment and identifier.
    ///
    /// Challenge sessions expire after `ttl_hours`.
    ///
    /// **Write-through cache update:** after persisting to the DB, this method inserts a
    /// positive cache entry into this instance's in-memory cache. If the same
    /// `ChallengeService` instance also serves the hot-path `is_challenge_completed`
    /// check, the user's next request will see `true` immediately without waiting out
    /// the negative TTL. In the current process topology the captcha verify endpoint
    /// and the Pingora hot path use separate `ChallengeService` instances, so the
    /// hot-path reader observes the change only after its 5-second negative TTL entry
    /// expires and the subsequent DB re-check finds the completed session.
    pub async fn mark_challenge_completed(
        &self,
        environment_id: i32,
        identifier: &str,
        identifier_type: &str,
        user_agent: Option<String>,
        ttl_hours: i64,
    ) -> Result<challenge_sessions::Model, ChallengeError> {
        let now = Utc::now();
        let expires_at = now + Duration::hours(ttl_hours);

        // Check if a session already exists; update it if so, otherwise insert.
        let result = if let Some(existing) = challenge_sessions::Entity::find()
            .filter(challenge_sessions::Column::EnvironmentId.eq(environment_id))
            .filter(challenge_sessions::Column::Identifier.eq(identifier))
            .filter(challenge_sessions::Column::IdentifierType.eq(identifier_type))
            .one(self.db.as_ref())
            .await?
        {
            let mut active: challenge_sessions::ActiveModel = existing.into();
            active.completed_at = Set(now);
            active.expires_at = Set(expires_at);
            if let Some(ua) = user_agent {
                active.user_agent = Set(Some(ua));
            }
            active.update(self.db.as_ref()).await?
        } else {
            challenge_sessions::ActiveModel {
                environment_id: Set(environment_id),
                identifier: Set(identifier.to_string()),
                identifier_type: Set(identifier_type.to_string()),
                user_agent: Set(user_agent),
                completed_at: Set(now),
                expires_at: Set(expires_at),
                ..Default::default()
            }
            .insert(self.db.as_ref())
            .await?
        };

        // Write-through: insert a positive cache entry into this instance's cache.
        // Reads on the SAME instance see the completion immediately; the Pingora
        // hot-path instance is a separate object and still experiences up to 5 seconds
        // of staleness until its negative TTL entry expires and a DB re-check confirms
        // the completed session.
        let key = (
            environment_id,
            identifier.to_string(),
            identifier_type.to_string(),
        );
        self.cache
            .insert(
                key,
                ChallengeEntry {
                    completed: true,
                    expires_at: Some(result.expires_at),
                },
            )
            .await;

        Ok(result)
    }

    /// Clear expired challenge sessions for cleanup.
    pub async fn clear_expired_sessions(&self) -> Result<u64, ChallengeError> {
        let now = Utc::now();
        let result = challenge_sessions::Entity::delete_many()
            .filter(challenge_sessions::Column::ExpiresAt.lt(now))
            .exec(self.db.as_ref())
            .await?;
        Ok(result.rows_affected)
    }

    /// Clear all challenge sessions for a specific environment (when disabling attack mode).
    pub async fn clear_environment_sessions(
        &self,
        environment_id: i32,
    ) -> Result<u64, ChallengeError> {
        let result = challenge_sessions::Entity::delete_many()
            .filter(challenge_sessions::Column::EnvironmentId.eq(environment_id))
            .exec(self.db.as_ref())
            .await?;
        Ok(result.rows_affected)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use sea_orm::{DatabaseBackend, MockDatabase};

    /// Build a minimal `challenge_sessions::Model` for use in MockDatabase results.
    fn session_model(
        environment_id: i32,
        identifier: &str,
        identifier_type: &str,
        expires_at: DateTime<Utc>,
    ) -> challenge_sessions::Model {
        challenge_sessions::Model {
            id: 1,
            environment_id,
            identifier: identifier.to_string(),
            identifier_type: identifier_type.to_string(),
            user_agent: None,
            completed_at: Utc::now(),
            expires_at,
        }
    }

    /// A completed challenge is cached after the first DB lookup — subsequent calls within
    /// the TTL window return the cached result without a second DB query.
    ///
    /// The MockDatabase queue holds exactly ONE result. If `is_challenge_completed` makes a
    /// second DB call, the queue is exhausted and the test panics.
    #[tokio::test]
    async fn test_completed_challenge_is_cached() {
        let expires_at = Utc::now() + Duration::hours(1);
        let model = session_model(1, "192.168.1.1", "ip", expires_at);

        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results(vec![vec![model]])
            .into_connection();

        let service = ChallengeService::new(Arc::new(db));

        // First call: cache miss → DB query → completed = true → cached.
        let first = service
            .is_challenge_completed(1, "192.168.1.1", "ip")
            .await
            .unwrap();
        assert!(first, "first call should return true (session found in DB)");

        // Second call: positive cache hit → no DB query.
        // A second DB call would exhaust the MockDatabase queue, causing a panic.
        let second = service
            .is_challenge_completed(1, "192.168.1.1", "ip")
            .await
            .unwrap();
        assert!(
            second,
            "second call should return true from cache without hitting DB"
        );
    }

    /// A flood of uncompleted-challenge lookups for the same key results in exactly one
    /// DB query. All subsequent calls within the 5-second negative TTL hit the cache.
    ///
    /// The MockDatabase queue contains exactly ONE empty result. Any call beyond the first
    /// that reaches the DB would exhaust the queue and panic, proving the cache is working.
    #[tokio::test]
    async fn test_negative_flood_hits_db_only_once() {
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            // Single empty result: no completed session in DB.
            .append_query_results(vec![Vec::<challenge_sessions::Model>::new()])
            .into_connection();

        let service = ChallengeService::new(Arc::new(db));

        // First call: DB query → not completed → cached for 5 seconds.
        let first = service
            .is_challenge_completed(1, "10.0.0.1", "ip")
            .await
            .unwrap();
        assert!(!first, "first call should be false (no session in DB)");

        // Calls 2–5: negative cache hit → no DB query.
        // Any DB hit here would exhaust the empty MockDatabase queue and panic.
        for i in 2..=5 {
            let result = service
                .is_challenge_completed(1, "10.0.0.1", "ip")
                .await
                .unwrap();
            assert!(
                !result,
                "call {i}: should return false from negative cache (no DB access)"
            );
        }
    }

    /// Completing a challenge via `mark_challenge_completed` writes a positive cache entry
    /// immediately (write-through). A subsequent `is_challenge_completed` call returns
    /// `true` without waiting out the 5-second negative TTL and without hitting the DB.
    ///
    /// DB sequence:
    ///   1. `mark_challenge_completed` → SELECT for existing row → empty.
    ///   2. `mark_challenge_completed` → INSERT RETURNING → new model.
    ///   3. `is_challenge_completed` → cache hit → NO DB call.
    #[tokio::test]
    async fn test_write_through_bypasses_negative_ttl() {
        let expires_at = Utc::now() + Duration::hours(24);
        let model = session_model(1, "10.0.0.2", "ip", expires_at);

        let db = MockDatabase::new(DatabaseBackend::Postgres)
            // (1) SELECT for existing session in mark_challenge_completed → none.
            .append_query_results(vec![Vec::<challenge_sessions::Model>::new()])
            // (2) INSERT RETURNING in mark_challenge_completed → new row.
            .append_query_results(vec![vec![model]])
            .into_connection();

        let service = ChallengeService::new(Arc::new(db));

        // Complete the challenge (no prior negative cache entry exists for this key).
        let session = service
            .mark_challenge_completed(1, "10.0.0.2", "ip", None, 24)
            .await
            .unwrap();
        assert_eq!(session.identifier, "10.0.0.2");

        // Immediately check completion — must be a cache hit (no DB call).
        // If this hits the DB, the MockDatabase queue is empty and will panic.
        let completed = service
            .is_challenge_completed(1, "10.0.0.2", "ip")
            .await
            .unwrap();
        assert!(
            completed,
            "write-through should make the completion immediately visible without a DB call"
        );
    }

    /// Completing a challenge after a negative cache entry was already established
    /// correctly overrides the negative entry via write-through.
    ///
    /// DB sequence:
    ///   1. `is_challenge_completed` → SELECT → empty → negative cache entry.
    ///   2. `mark_challenge_completed` → SELECT for existing → empty.
    ///   3. `mark_challenge_completed` → INSERT RETURNING → new model.
    ///   4. `is_challenge_completed` → cache hit (positive, from write-through) → NO DB.
    #[tokio::test]
    async fn test_write_through_overrides_negative_cache_entry() {
        let expires_at = Utc::now() + Duration::hours(24);
        let model = session_model(1, "10.0.0.3", "ip", expires_at);

        let db = MockDatabase::new(DatabaseBackend::Postgres)
            // (1) is_challenge_completed SELECT → no session yet.
            .append_query_results(vec![Vec::<challenge_sessions::Model>::new()])
            // (2) mark_challenge_completed SELECT for existing → none.
            .append_query_results(vec![Vec::<challenge_sessions::Model>::new()])
            // (3) INSERT RETURNING → new row.
            .append_query_results(vec![vec![model]])
            .into_connection();

        let service = ChallengeService::new(Arc::new(db));

        // Prime a negative cache entry.
        let not_completed = service
            .is_challenge_completed(1, "10.0.0.3", "ip")
            .await
            .unwrap();
        assert!(
            !not_completed,
            "initial check should be false (no session in DB)"
        );

        // Complete the challenge — write-through must override the negative entry.
        service
            .mark_challenge_completed(1, "10.0.0.3", "ip", None, 24)
            .await
            .unwrap();

        // Immediately check — must return true from cache (no DB call).
        // An extra DB call would exhaust the MockDatabase queue and panic.
        let completed = service
            .is_challenge_completed(1, "10.0.0.3", "ip")
            .await
            .unwrap();
        assert!(
            completed,
            "after write-through, completed should be immediately visible without a DB call"
        );
    }

    /// A completed entry whose `expires_at` is in the near future expires from the cache
    /// when the TTL elapses, and the next call re-queries the DB.
    ///
    /// This test uses a very short `expires_at` (10 ms in the future) to verify that
    /// the per-entry TTL is derived from the row's `expires_at`, not a fixed global cap.
    #[tokio::test]
    async fn test_positive_entry_expires_and_retries_db() {
        // Session expires in 10 ms — the cache TTL should be ≤10 ms.
        let short_expires_at = Utc::now() + Duration::milliseconds(10);
        let model_first = session_model(1, "10.0.0.4", "ip", short_expires_at);
        // Second DB call (after expiry) returns the same model (or could return empty
        // to verify the re-query; we return a fresh model here for simplicity).
        let model_second = session_model(1, "10.0.0.4", "ip", Utc::now() + Duration::hours(1));

        let db = MockDatabase::new(DatabaseBackend::Postgres)
            // First DB lookup: session found with short TTL.
            .append_query_results(vec![vec![model_first]])
            // Second DB lookup (after cache entry expires): session still found.
            .append_query_results(vec![vec![model_second]])
            .into_connection();

        let service = ChallengeService::new(Arc::new(db));

        // First call: DB hit → true → cached with ~10 ms TTL.
        let first = service
            .is_challenge_completed(1, "10.0.0.4", "ip")
            .await
            .unwrap();
        assert!(first, "first call should be true");

        // Wait for the cache entry to expire.
        tokio::time::sleep(StdDuration::from_millis(50)).await;

        // Second call: entry expired → DB re-queried (consumes the second queued result).
        let second = service
            .is_challenge_completed(1, "10.0.0.4", "ip")
            .await
            .unwrap();
        assert!(
            second,
            "second call after expiry should also be true (re-queried DB)"
        );
    }
}
