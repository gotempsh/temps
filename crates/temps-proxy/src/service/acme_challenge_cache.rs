// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! In-memory view of pending ACME HTTP-01 challenges.
//!
//! `/.well-known/acme-challenge/<token>` is unauthenticated and reachable on
//! every host, so it must not cost a database query per request: a stream of
//! random tokens would otherwise turn into the same stream of queries.
//!
//! The proxy keeps a snapshot of every pending challenge
//! (`domains.http_challenge_token` / `http_challenge_key_authorization`).
//! A request whose token is in a recent snapshot is answered from memory with
//! no lock. On a miss the snapshot is reloaded, and that reload is:
//!
//! - **single-flight**: one query at a time, behind an async mutex;
//! - **rate-limited**: no more than one query per [`MIN_RELOAD_INTERVAL`],
//!   however many requests miss;
//! - **never a stale "no"**: a miss is only answered from a snapshot whose
//!   query *started after the request arrived*. A challenge the control plane
//!   stored before Let's Encrypt's validation request reached the proxy is
//!   therefore always found, without any invalidation message from the
//!   process that wrote it (which, in a split topology, is another process).
//!
//! The cost of a flood is bounded to `1 / MIN_RELOAD_INTERVAL` queries per
//! second; the price is that a miss waits for the next reload (at most about
//! `MIN_RELOAD_INTERVAL` plus one query). Let's Encrypt's few validation
//! requests per issuance are not affected by that delay in any way that
//! matters.

use arc_swap::ArcSwapOption;
use async_trait::async_trait;
use sea_orm::{ColumnTrait, EntityTrait, QueryFilter, QueryOrder, QuerySelect};
use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};
use temps_database::DbConnection;
use temps_entities::domains;
use tracing::{debug, error, warn};

/// Minimum spacing between two reload queries.
const MIN_RELOAD_INTERVAL: Duration = Duration::from_millis(500);

/// A snapshot older than this is not trusted even for a hit, so a challenge
/// that was cleared or replaced stops being served within this window.
const MAX_SNAPSHOT_AGE: Duration = Duration::from_secs(30);

/// Upper bound on one reload query.
const RELOAD_TIMEOUT: Duration = Duration::from_secs(5);

/// Most pending challenges held in memory. Pending challenges are normally a
/// handful (tokens are cleared once a certificate is issued); the cap only
/// bounds memory if they pile up. Newest-first, so a fresh issuance is never
/// the one dropped.
const MAX_PENDING_CHALLENGES: u64 = 10_000;

/// Why a challenge lookup could not be answered.
#[derive(Debug, thiserror::Error)]
pub enum AcmeChallengeLookupError {
    #[error("Failed to load pending ACME HTTP-01 challenges for host {host}: {reason}")]
    Database { host: String, reason: String },
    #[error("Loading pending ACME HTTP-01 challenges for host {host} timed out after {timeout:?}")]
    Timeout { host: String, timeout: Duration },
}

/// One pending HTTP-01 challenge.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct PendingChallenge {
    pub domain: String,
    pub token: String,
    pub key_authorization: String,
}

/// Where pending challenges come from: the `domains` table in production, a
/// stub in tests.
#[async_trait]
pub(crate) trait ChallengeSource: Send + Sync + 'static {
    async fn load_pending(&self, limit: u64) -> Result<Vec<PendingChallenge>, sea_orm::DbErr>;
}

struct DbChallengeSource {
    db: Arc<DbConnection>,
}

#[async_trait]
impl ChallengeSource for DbChallengeSource {
    async fn load_pending(&self, limit: u64) -> Result<Vec<PendingChallenge>, sea_orm::DbErr> {
        // Only the three columns needed: the row also carries certificate
        // and key PEMs, which must not be read (or held) here.
        let rows: Vec<(String, String, String)> = domains::Entity::find()
            .select_only()
            .column(domains::Column::Domain)
            .column(domains::Column::HttpChallengeToken)
            .column(domains::Column::HttpChallengeKeyAuthorization)
            .filter(domains::Column::HttpChallengeToken.is_not_null())
            .filter(domains::Column::HttpChallengeKeyAuthorization.is_not_null())
            .order_by_desc(domains::Column::UpdatedAt)
            .limit(limit)
            .into_tuple()
            .all(self.db.as_ref())
            .await?;
        Ok(rows
            .into_iter()
            .map(|(domain, token, key_authorization)| PendingChallenge {
                domain,
                token,
                key_authorization,
            })
            .collect())
    }
}

/// Pending challenges as of `loaded_at`: domain → token → key authorization.
#[derive(Debug)]
struct Snapshot {
    challenges: HashMap<String, HashMap<String, String>>,
    /// When the query that produced this snapshot *started*.
    loaded_at: Instant,
}

impl Snapshot {
    fn get(&self, host: &str, token: &str) -> Option<String> {
        self.challenges.get(host)?.get(token).cloned()
    }
}

/// Bookkeeping for the reload path, guarded by the reload mutex.
#[derive(Debug, Default)]
struct ReloadState {
    last_attempt_started: Option<Instant>,
    /// Start of the most recent attempt, when it failed.
    last_failure_started: Option<Instant>,
}

#[derive(Debug, Clone, Copy)]
struct ReloadPolicy {
    min_interval: Duration,
    max_snapshot_age: Duration,
    timeout: Duration,
}

impl Default for ReloadPolicy {
    fn default() -> Self {
        Self {
            min_interval: MIN_RELOAD_INTERVAL,
            max_snapshot_age: MAX_SNAPSHOT_AGE,
            timeout: RELOAD_TIMEOUT,
        }
    }
}

/// Bounded, self-refreshing cache of pending ACME HTTP-01 challenges.
pub struct AcmeChallengeCache {
    snapshot: ArcSwapOption<Snapshot>,
    reload: tokio::sync::Mutex<ReloadState>,
    source: Arc<dyn ChallengeSource>,
    policy: ReloadPolicy,
}

impl AcmeChallengeCache {
    pub fn new(db: Arc<DbConnection>) -> Self {
        Self::with_source(Arc::new(DbChallengeSource { db }), ReloadPolicy::default())
    }

    fn with_source(source: Arc<dyn ChallengeSource>, policy: ReloadPolicy) -> Self {
        Self {
            snapshot: ArcSwapOption::empty(),
            reload: tokio::sync::Mutex::new(ReloadState::default()),
            source,
            policy,
        }
    }

    /// The key authorization stored for `token` on `host`, if any.
    pub async fn lookup(
        &self,
        host: &str,
        token: &str,
    ) -> Result<Option<String>, AcmeChallengeLookupError> {
        let arrived = Instant::now();

        // Fast path: lock-free hit in a recent snapshot.
        if let Some(snapshot) = self.snapshot.load_full() {
            if arrived.duration_since(snapshot.loaded_at) < self.policy.max_snapshot_age {
                if let Some(key_authorization) = snapshot.get(host, token) {
                    return Ok(Some(key_authorization));
                }
            }
        }

        // Miss (or too old to trust): the answer must come from a query that
        // started after this request arrived.
        let mut state = self.reload.lock().await;

        // Someone else's reload, started after we arrived, already answers us.
        if let Some(snapshot) = self.snapshot.load_full() {
            if snapshot.loaded_at >= arrived {
                return Ok(snapshot.get(host, token));
            }
        }
        // ...or already failed: report that instead of queueing another
        // query behind it.
        if let Some(failed_at) = state.last_failure_started {
            if failed_at >= arrived {
                return Err(AcmeChallengeLookupError::Database {
                    host: host.to_string(),
                    reason: "the reload serving this request failed".to_string(),
                });
            }
        }

        if let Some(last) = state.last_attempt_started {
            let earliest = last + self.policy.min_interval;
            if earliest > Instant::now() {
                tokio::time::sleep_until(earliest.into()).await;
            }
        }

        let started = Instant::now();
        state.last_attempt_started = Some(started);
        let result = tokio::time::timeout(
            self.policy.timeout,
            self.source.load_pending(MAX_PENDING_CHALLENGES),
        )
        .await;

        match result {
            Ok(Ok(pending)) => {
                state.last_failure_started = None;
                if pending.len() as u64 >= MAX_PENDING_CHALLENGES {
                    warn!(
                        limit = MAX_PENDING_CHALLENGES,
                        "ACME challenge cache hit its cap of pending HTTP-01 challenges; \
                         only the most recently updated are served"
                    );
                }
                let mut challenges: HashMap<String, HashMap<String, String>> = HashMap::new();
                for challenge in pending {
                    challenges
                        .entry(challenge.domain)
                        .or_default()
                        .insert(challenge.token, challenge.key_authorization);
                }
                debug!(
                    domains = challenges.len(),
                    "Reloaded pending ACME HTTP-01 challenges"
                );
                let snapshot = Arc::new(Snapshot {
                    challenges,
                    loaded_at: started,
                });
                let answer = snapshot.get(host, token);
                self.snapshot.store(Some(snapshot));
                Ok(answer)
            }
            Ok(Err(e)) => {
                state.last_failure_started = Some(started);
                error!(host, "Database error loading ACME challenges: {}", e);
                Err(AcmeChallengeLookupError::Database {
                    host: host.to_string(),
                    reason: e.to_string(),
                })
            }
            Err(_) => {
                state.last_failure_started = Some(started);
                error!(
                    host,
                    timeout = ?self.policy.timeout,
                    "Loading ACME challenges timed out"
                );
                Err(AcmeChallengeLookupError::Timeout {
                    host: host.to_string(),
                    timeout: self.policy.timeout,
                })
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::sync::Mutex;

    /// Serves whatever `pending` holds at query time and counts queries.
    struct StubSource {
        pending: Mutex<Result<Vec<PendingChallenge>, String>>,
        loads: AtomicU64,
        delay: Duration,
    }

    impl StubSource {
        fn new(pending: Vec<PendingChallenge>) -> Arc<Self> {
            Arc::new(Self {
                pending: Mutex::new(Ok(pending)),
                loads: AtomicU64::new(0),
                delay: Duration::ZERO,
            })
        }

        fn set(&self, pending: Result<Vec<PendingChallenge>, String>) {
            *self.pending.lock().unwrap() = pending;
        }

        fn loads(&self) -> u64 {
            self.loads.load(Ordering::SeqCst)
        }
    }

    #[async_trait]
    impl ChallengeSource for StubSource {
        async fn load_pending(&self, _limit: u64) -> Result<Vec<PendingChallenge>, sea_orm::DbErr> {
            self.loads.fetch_add(1, Ordering::SeqCst);
            if !self.delay.is_zero() {
                tokio::time::sleep(self.delay).await;
            }
            self.pending
                .lock()
                .unwrap()
                .clone()
                .map_err(sea_orm::DbErr::Custom)
        }
    }

    fn challenge(domain: &str, token: &str) -> PendingChallenge {
        PendingChallenge {
            domain: domain.to_string(),
            token: token.to_string(),
            key_authorization: format!("{token}.thumbprint"),
        }
    }

    fn policy() -> ReloadPolicy {
        ReloadPolicy {
            min_interval: Duration::from_millis(20),
            max_snapshot_age: Duration::from_secs(60),
            timeout: Duration::from_secs(5),
        }
    }

    fn cache(source: &Arc<StubSource>, policy: ReloadPolicy) -> AcmeChallengeCache {
        AcmeChallengeCache::with_source(Arc::clone(source) as Arc<dyn ChallengeSource>, policy)
    }

    #[tokio::test]
    async fn hits_are_served_from_memory() {
        let source = StubSource::new(vec![challenge("app.example.test", "tok-1")]);
        let cache = cache(&source, policy());

        for _ in 0..50 {
            let answer = cache.lookup("app.example.test", "tok-1").await.unwrap();
            assert_eq!(answer.as_deref(), Some("tok-1.thumbprint"));
        }
        assert_eq!(source.loads(), 1);
    }

    #[tokio::test]
    async fn token_is_scoped_to_its_host() {
        let source = StubSource::new(vec![challenge("app.example.test", "tok-1")]);
        let cache = cache(&source, policy());

        assert_eq!(
            cache.lookup("other.example.test", "tok-1").await.unwrap(),
            None
        );
    }

    /// A challenge stored after the last reload is found on the next
    /// request: a miss is never answered from a snapshot older than it.
    #[tokio::test]
    async fn challenge_created_after_a_reload_is_found() {
        let source = StubSource::new(vec![challenge("app.example.test", "old")]);
        let cache = cache(&source, policy());
        assert!(cache
            .lookup("app.example.test", "old")
            .await
            .unwrap()
            .is_some());

        source.set(Ok(vec![
            challenge("app.example.test", "old"),
            challenge("app.example.test", "new"),
        ]));
        let answer = cache.lookup("app.example.test", "new").await.unwrap();
        assert_eq!(answer.as_deref(), Some("new.thumbprint"));
        assert_eq!(source.loads(), 2);
    }

    /// Random tokens cannot turn into one query each.
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn a_flood_of_misses_is_rate_limited() {
        let source = Arc::new(StubSource {
            pending: Mutex::new(Ok(Vec::new())),
            loads: AtomicU64::new(0),
            delay: Duration::from_millis(10),
        });
        let cache = Arc::new(cache(
            &source,
            ReloadPolicy {
                min_interval: Duration::from_millis(50),
                ..policy()
            },
        ));

        let started = Instant::now();
        let lookups: Vec<_> = (0..200)
            .map(|i| {
                let cache = Arc::clone(&cache);
                tokio::spawn(async move {
                    cache
                        .lookup("app.example.test", &format!("random-{i}"))
                        .await
                })
            })
            .collect();
        for lookup in lookups {
            assert_eq!(lookup.await.unwrap().unwrap(), None);
        }

        // Every query is at least `min_interval` after the previous one.
        let elapsed = started.elapsed();
        let max_queries = 1 + (elapsed.as_millis() / 50) as u64;
        assert!(
            source.loads() <= max_queries,
            "{} queries in {:?} (at most {} allowed)",
            source.loads(),
            elapsed,
            max_queries
        );
        assert!(
            source.loads() <= 3,
            "200 misses cost {} queries",
            source.loads()
        );
    }

    #[tokio::test]
    async fn database_error_is_reported() {
        let source = StubSource::new(Vec::new());
        source.set(Err("connection refused".to_string()));
        let cache = cache(&source, policy());

        let err = cache.lookup("app.example.test", "tok").await.unwrap_err();
        assert!(matches!(err, AcmeChallengeLookupError::Database { .. }));
        assert!(err.to_string().contains("app.example.test"));
    }

    /// Requests queued behind a failed reload share its error instead of
    /// each running another query against a database that just failed.
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn waiters_share_a_failed_reload() {
        let source = Arc::new(StubSource {
            pending: Mutex::new(Err("connection refused".to_string())),
            loads: AtomicU64::new(0),
            delay: Duration::from_millis(50),
        });
        let cache = Arc::new(cache(&source, policy()));

        let first = {
            let cache = Arc::clone(&cache);
            tokio::spawn(async move { cache.lookup("app.example.test", "a").await })
        };
        // Let the first lookup start its (slow) query.
        tokio::time::sleep(Duration::from_millis(10)).await;
        let waiters: Vec<_> = (0..20)
            .map(|i| {
                let cache = Arc::clone(&cache);
                tokio::spawn(
                    async move { cache.lookup("app.example.test", &format!("w{i}")).await },
                )
            })
            .collect();

        assert!(first.await.unwrap().is_err());
        for waiter in waiters {
            assert!(waiter.await.unwrap().is_err());
        }
        // The waiters arrived before the first query finished, but after it
        // started, so they each need one later query — and share it.
        assert_eq!(source.loads(), 2);
    }

    #[tokio::test]
    async fn timed_out_reload_is_reported() {
        let source = Arc::new(StubSource {
            pending: Mutex::new(Ok(Vec::new())),
            loads: AtomicU64::new(0),
            delay: Duration::from_millis(200),
        });
        let cache = cache(
            &source,
            ReloadPolicy {
                timeout: Duration::from_millis(20),
                ..policy()
            },
        );

        let err = cache.lookup("app.example.test", "tok").await.unwrap_err();
        assert!(matches!(err, AcmeChallengeLookupError::Timeout { .. }));
    }

    /// A cleared challenge stops being served once the snapshot ages out.
    #[tokio::test]
    async fn stale_snapshot_is_not_trusted_for_hits() {
        let source = StubSource::new(vec![challenge("app.example.test", "tok")]);
        let cache = cache(
            &source,
            ReloadPolicy {
                max_snapshot_age: Duration::from_millis(30),
                ..policy()
            },
        );
        assert!(cache
            .lookup("app.example.test", "tok")
            .await
            .unwrap()
            .is_some());

        source.set(Ok(Vec::new()));
        tokio::time::sleep(Duration::from_millis(40)).await;
        assert_eq!(cache.lookup("app.example.test", "tok").await.unwrap(), None);
        assert_eq!(source.loads(), 2);
    }
}
