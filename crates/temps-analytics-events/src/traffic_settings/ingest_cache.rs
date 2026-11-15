// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! The per-project cache the ingest path reads traffic settings and filters
//! through.
//!
//! Two properties matter on a busy instance:
//!
//! - **One load per miss.** When an entry expires under load, every event for
//!   that project misses at once; concurrent misses for the same project share
//!   a single database read instead of each issuing their own.
//! - **A write is not undone by a load already in flight.** A load that read
//!   the table before a write committed would otherwise cache the old value
//!   *after* the write invalidated the entry, and keep applying it for a full
//!   TTL. Entries are keyed by a generation counter as well as the project,
//!   and every write bumps it: a load started before the write can only fill
//!   the old generation's slot, which no later caller reads or joins. A load
//!   that sees the counter change while it ran also skips caching, so that
//!   slot rarely holds anything.
//!
//! The counter is shared by every project, so a write re-keys them all and
//! each reloads once on its next event. Writes are rare admin actions; the
//! entries left behind are never read again, age out within the TTL and
//! count toward the weight bound until then.

use std::fmt;
use std::future::Future;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Duration;

use moka::future::Cache;
use sea_orm::DbErr;

pub(super) struct IngestCache<V> {
    /// Keyed by `(project_id, generation)`.
    cache: Cache<(i32, u64), V>,
    generation: AtomicU64,
}

enum Miss<V> {
    Failed(DbErr),
    /// A write landed while the load ran: the value is served, not cached.
    Raced(V),
}

/// A cache load that failed. Carries the database error for logging; failures
/// are never cached, so the next event retries.
pub(super) struct IngestLoadError<V>(Arc<Miss<V>>);

impl<V> fmt::Display for IngestLoadError<V> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self.0.as_ref() {
            Miss::Failed(e) => write!(f, "{e}"),
            Miss::Raced(_) => write!(f, "load raced a write"),
        }
    }
}

impl<V: Clone + Send + Sync + 'static> IngestCache<V> {
    /// Entries live for `ttl`. `max_weight` bounds the sum of `weigh` over the
    /// cached values, so a cache of unevenly sized values is bounded by size
    /// rather than by entry count; weigh every entry 1 to bound the count.
    pub(super) fn new(ttl: Duration, max_weight: u64, weigh: fn(&V) -> u32) -> Self {
        Self {
            cache: Cache::builder()
                .max_capacity(max_weight)
                .weigher(move |_key: &(i32, u64), value: &V| weigh(value))
                .time_to_live(ttl)
                .build(),
            generation: AtomicU64::new(0),
        }
    }

    /// The cached value for `project_id`, or the result of `load`, shared with
    /// every concurrent caller missing on the same project.
    pub(super) async fn get_or_load<F, Fut>(
        &self,
        project_id: i32,
        load: F,
    ) -> Result<V, IngestLoadError<V>>
    where
        F: FnOnce() -> Fut,
        Fut: Future<Output = Result<V, DbErr>>,
    {
        let generation = &self.generation;
        let before = generation.load(Ordering::Acquire);
        let result = self
            .cache
            .try_get_with((project_id, before), async move {
                let value = load().await.map_err(Miss::Failed)?;
                if generation.load(Ordering::Acquire) != before {
                    return Err(Miss::Raced(value));
                }
                Ok(value)
            })
            .await;
        match result {
            Ok(value) => Ok(value),
            Err(miss) => match miss.as_ref() {
                Miss::Raced(value) => Ok(value.clone()),
                Miss::Failed(_) => Err(IngestLoadError(miss)),
            },
        }
    }

    /// Move every caller to a new generation after a write, so none reads or
    /// joins a load of what was there before, and drop `project_id`'s
    /// previous entry right away.
    pub(super) async fn invalidate(&self, project_id: i32) {
        let previous = self.generation.fetch_add(1, Ordering::AcqRel);
        self.cache.invalidate(&(project_id, previous)).await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::AtomicUsize;

    fn cache() -> IngestCache<u32> {
        IngestCache::new(Duration::from_secs(30), 100, |_| 1)
    }

    #[tokio::test]
    async fn a_loaded_value_is_cached() {
        let cache = cache();
        let loads = AtomicUsize::new(0);
        for _ in 0..3 {
            let value = cache
                .get_or_load(1, || async {
                    loads.fetch_add(1, Ordering::SeqCst);
                    Ok(7)
                })
                .await;
            assert_eq!(value.ok(), Some(7));
        }
        assert_eq!(loads.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn concurrent_misses_share_one_load() {
        let cache = Arc::new(cache());
        let loads = Arc::new(AtomicUsize::new(0));
        let gate = Arc::new(tokio::sync::Notify::new());
        let mut tasks = Vec::new();
        for _ in 0..16 {
            let (cache, loads, gate) = (cache.clone(), loads.clone(), gate.clone());
            tasks.push(tokio::spawn(async move {
                cache
                    .get_or_load(1, || async move {
                        loads.fetch_add(1, Ordering::SeqCst);
                        gate.notified().await;
                        Ok(7)
                    })
                    .await
                    .ok()
            }));
        }
        // Let every task reach the cache before the one load completes.
        tokio::time::sleep(Duration::from_millis(50)).await;
        gate.notify_waiters();
        for task in tasks {
            assert_eq!(task.await.ok().flatten(), Some(7));
        }
        assert_eq!(loads.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn a_failed_load_is_reported_and_not_cached() {
        let cache = cache();
        let failed = cache
            .get_or_load(1, || async { Err(DbErr::Custom("db down".into())) })
            .await;
        let Err(e) = failed else {
            panic!("a failed load must surface as an error");
        };
        assert!(e.to_string().contains("db down"), "{e}");

        let retried = cache.get_or_load(1, || async { Ok(7) }).await;
        assert_eq!(retried.ok(), Some(7));
    }

    #[tokio::test]
    async fn a_load_that_raced_a_write_is_served_but_not_cached() {
        let cache = cache();
        let raced = cache
            .get_or_load(1, || async {
                // The write commits and invalidates while this load runs.
                cache.invalidate(1).await;
                Ok(1)
            })
            .await;
        assert_eq!(raced.ok(), Some(1), "the caller still gets what was read");

        let next = cache.get_or_load(1, || async { Ok(2) }).await;
        assert_eq!(next.ok(), Some(2), "the pre-write value must not be cached");
    }

    #[tokio::test]
    async fn a_caller_after_a_write_does_not_join_a_load_started_before_it() {
        let cache = Arc::new(cache());
        // One-shot channels rather than a sleep and a `Notify`: the write
        // must land after the load has started, and the release must not be
        // lost if the load is not waiting yet.
        let (started_tx, started_rx) = tokio::sync::oneshot::channel::<()>();
        let (release_tx, release_rx) = tokio::sync::oneshot::channel::<()>();
        let before_write = {
            let cache = cache.clone();
            tokio::spawn(async move {
                cache
                    .get_or_load(1, || async move {
                        let _ = started_tx.send(());
                        let _ = release_rx.await;
                        Ok(1)
                    })
                    .await
                    .ok()
            })
        };
        started_rx
            .await
            .expect("the pre-write load must start before the write");
        cache.invalidate(1).await;

        let after_write = cache.get_or_load(1, || async { Ok(2) }).await;
        assert_eq!(after_write.ok(), Some(2), "joined the pre-write load");

        let _ = release_tx.send(());
        assert_eq!(before_write.await.ok().flatten(), Some(1));
        let next = cache.get_or_load(1, || async { Ok(3) }).await;
        assert_eq!(
            next.ok(),
            Some(2),
            "the post-write value is what stays cached"
        );
    }

    #[tokio::test]
    async fn the_bound_is_on_total_weight_not_entry_count() {
        // Each value weighs itself: two 60s cannot both fit in 100.
        let cache: IngestCache<u32> = IngestCache::new(Duration::from_secs(30), 100, |v| *v);
        for project in 0..2 {
            let _ = cache.get_or_load(project, || async { Ok(60) }).await;
        }
        cache.cache.run_pending_tasks().await;
        assert!(
            cache.cache.weighted_size() <= 100,
            "weighted size {} exceeds the bound",
            cache.cache.weighted_size()
        );
        assert_eq!(cache.cache.entry_count(), 1);
    }

    #[tokio::test]
    async fn invalidate_drops_the_entry() {
        let cache = cache();
        assert_eq!(cache.get_or_load(1, || async { Ok(1) }).await.ok(), Some(1));
        cache.invalidate(1).await;
        assert_eq!(cache.get_or_load(1, || async { Ok(2) }).await.ok(), Some(2));
    }
}
