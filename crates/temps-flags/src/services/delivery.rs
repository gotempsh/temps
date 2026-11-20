// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! In-memory flag sets per environment, for the evaluation endpoints.
//!
//! `/api/_temps/flags` is called by every page load of every deployed site
//! that reads a flag, and OFREP is called per flag read by remote-evaluating
//! SDKs. Neither may touch the database per request (CLAUDE.md hot-path rules),
//! so both read an environment's flags from here.
//!
//! - **Reads are lock-free**: one `ArcSwap` load and a hash lookup.
//! - **Freshness**: an entry is reused for [`TTL`]. Every write through
//!   [`super::FlagService`] drops the project's entries at once, so on a
//!   single-process install a change is visible on the next request. Other
//!   processes (split console/proxy topologies) converge within the TTL plus
//!   one refresh, because the previous entry is served while it refreshes.
//! - **Load is bounded**: at most [`MAX_CONCURRENT_LOADS`] environments load
//!   at once, and while an entry is being refreshed every other request is
//!   served the previous one instead of piling onto the database.
//! - **Memory is bounded**: at most [`MAX_ENTRIES`] environments are cached;
//!   flags per environment are a small control-plane set.

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use arc_swap::ArcSwap;
use sha2::{Digest, Sha256};
use tokio::sync::Semaphore;

use crate::eval::{CompiledPatterns, FlagSnapshot};

/// How long an environment's flags are reused before they are reloaded.
pub const TTL: Duration = Duration::from_secs(5);
/// Environments loading from the database at the same time, across all
/// requests in this process.
pub const MAX_CONCURRENT_LOADS: usize = 4;
/// Environments held in memory.
pub const MAX_ENTRIES: usize = 4_096;

/// One flag ready to evaluate.
#[derive(Debug)]
pub struct DeliveredFlag {
    pub snapshot: FlagSnapshot,
    /// `matches` patterns compiled once per load, never per request.
    pub patterns: CompiledPatterns,
    /// Whether the flag may be served to browsers.
    pub client_visible: bool,
    /// Share of subjects whose browser records an exposure event.
    pub exposure_sample_rate: f64,
}

/// Every live flag of one environment, sorted by key.
#[derive(Debug)]
pub struct EnvironmentFlags {
    pub project_id: i32,
    pub environment_id: i32,
    pub flags: Vec<DeliveredFlag>,
    /// Entity tag over the client-visible flags' definitions, for OFREP bulk
    /// evaluation caching. Changes whenever any of them changes.
    pub client_etag: String,
    /// The same over every flag, for server-side callers.
    pub server_etag: String,
    loaded_at: Instant,
    refreshing: AtomicBool,
}

impl EnvironmentFlags {
    pub fn new(project_id: i32, environment_id: i32, mut flags: Vec<DeliveredFlag>) -> Self {
        flags.sort_by(|a, b| a.snapshot.key.cmp(&b.snapshot.key));
        let client_etag = etag(flags.iter().filter(|flag| flag.client_visible));
        let server_etag = etag(flags.iter());
        Self {
            project_id,
            environment_id,
            flags,
            client_etag,
            server_etag,
            loaded_at: Instant::now(),
            refreshing: AtomicBool::new(false),
        }
    }

    pub fn get(&self, key: &str) -> Option<&DeliveredFlag> {
        self.flags
            .binary_search_by(|flag| flag.snapshot.key.as_str().cmp(key))
            .ok()
            .map(|index| &self.flags[index])
    }

    fn is_fresh(&self) -> bool {
        self.loaded_at.elapsed() < TTL
    }
}

fn etag<'a>(flags: impl Iterator<Item = &'a DeliveredFlag>) -> String {
    let mut hasher = Sha256::new();
    for flag in flags {
        // serde_json of a typed struct cannot fail; an empty write still
        // yields a stable tag if it somehow did.
        if let Ok(bytes) = serde_json::to_vec(&flag.snapshot) {
            hasher.update(&bytes);
        }
        hasher.update(flag.exposure_sample_rate.to_be_bytes());
        hasher.update([0u8]);
    }
    format!("\"{}\"", hex::encode(&hasher.finalize()[..16]))
}

/// What the cache wants the caller to do.
pub enum Lookup {
    /// A usable entry. `refresh` is true when it is stale and this caller won
    /// the right to reload it; everyone else keeps serving it meanwhile.
    Hit {
        entry: Arc<EnvironmentFlags>,
        refresh: bool,
    },
    /// Nothing cached; load it.
    Miss,
}

pub struct FlagDeliveryCache {
    entries: ArcSwap<HashMap<i32, Arc<EnvironmentFlags>>>,
    loads: Semaphore,
    /// Bumped by every invalidation. A load records it before reading the
    /// database and only caches its result if no invalidation happened
    /// meanwhile, so a load that read the old flags cannot put them back
    /// after a write dropped them.
    generation: AtomicU64,
}

impl Default for FlagDeliveryCache {
    fn default() -> Self {
        Self::new()
    }
}

impl FlagDeliveryCache {
    pub fn new() -> Self {
        Self {
            entries: ArcSwap::from_pointee(HashMap::new()),
            loads: Semaphore::new(MAX_CONCURRENT_LOADS),
            generation: AtomicU64::new(0),
        }
    }

    pub fn lookup(&self, environment_id: i32) -> Lookup {
        let entries = self.entries.load();
        let Some(entry) = entries.get(&environment_id) else {
            return Lookup::Miss;
        };
        if entry.is_fresh() {
            return Lookup::Hit {
                entry: entry.clone(),
                refresh: false,
            };
        }
        let won = entry
            .refreshing
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .is_ok();
        Lookup::Hit {
            entry: entry.clone(),
            refresh: won,
        }
    }

    /// The cached entry, fresh or stale, without claiming its refresh.
    pub fn peek(&self, environment_id: i32) -> Option<Arc<EnvironmentFlags>> {
        self.entries.load().get(&environment_id).cloned()
    }

    /// The load gate. Held across the database read only.
    pub fn load_permit(&self) -> &Semaphore {
        &self.loads
    }

    /// The current invalidation generation. Read it before loading from the
    /// database and pass it to [`Self::insert`].
    pub fn generation(&self) -> u64 {
        self.generation.load(Ordering::Acquire)
    }

    /// Cache a freshly loaded environment, unless an invalidation happened
    /// since `loaded_at_generation` (the data may predate that write). The
    /// entry is returned either way, so the caller can still serve it.
    pub fn insert(
        &self,
        entry: EnvironmentFlags,
        loaded_at_generation: u64,
    ) -> Arc<EnvironmentFlags> {
        let entry = Arc::new(entry);
        let environment_id = entry.environment_id;
        self.entries.rcu(|current| {
            // Checked inside the closure: `rcu` retries it if an invalidation
            // swapped the map in between, and invalidations bump the
            // generation before they swap.
            if self.generation.load(Ordering::Acquire) != loaded_at_generation {
                return HashMap::clone(current);
            }
            let mut next: HashMap<i32, Arc<EnvironmentFlags>> = HashMap::clone(current);
            if next.len() >= MAX_ENTRIES && !next.contains_key(&environment_id) {
                next.retain(|_, existing| existing.is_fresh());
                if next.len() >= MAX_ENTRIES {
                    next.clear();
                }
            }
            next.insert(environment_id, entry.clone());
            next
        });
        entry
    }

    /// Drop every cached environment of a project. Called after every flag
    /// write, so the next read sees it.
    pub fn invalidate_project(&self, project_id: i32) {
        self.generation.fetch_add(1, Ordering::AcqRel);
        self.entries.rcu(|current| {
            let mut next: HashMap<i32, Arc<EnvironmentFlags>> = HashMap::clone(current);
            next.retain(|_, entry| entry.project_id != project_id);
            next
        });
    }

    #[cfg(test)]
    pub fn cached_environments(&self) -> usize {
        self.entries.load().len()
    }
}

/// A won refresh claim on a stale entry. Dropping it releases the claim, on
/// success, error and cancellation alike.
pub struct RefreshClaim {
    entry: Arc<EnvironmentFlags>,
}

impl RefreshClaim {
    pub fn new(entry: Arc<EnvironmentFlags>) -> Self {
        Self { entry }
    }
}

impl Drop for RefreshClaim {
    fn drop(&mut self) {
        self.entry.refreshing.store(false, Ordering::Release);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::eval::FlagValueType;

    fn flag(key: &str, client_visible: bool) -> DeliveredFlag {
        DeliveredFlag {
            snapshot: FlagSnapshot {
                key: key.to_string(),
                value_type: FlagValueType::Bool,
                default_value: serde_json::json!(false),
                enabled: true,
                environment_value: None,
                salt: "s".into(),
                variants: Vec::new(),
                rules: Vec::new(),
            },
            patterns: CompiledPatterns::default(),
            client_visible,
            exposure_sample_rate: 0.0,
        }
    }

    #[test]
    fn a_fresh_entry_is_served_without_a_refresh() {
        let cache = FlagDeliveryCache::new();
        cache.insert(
            EnvironmentFlags::new(1, 10, vec![flag("a", true)]),
            cache.generation(),
        );

        match cache.lookup(10) {
            Lookup::Hit { entry, refresh } => {
                assert!(!refresh);
                assert!(entry.get("a").is_some());
            }
            Lookup::Miss => panic!("expected a hit"),
        }
        assert!(matches!(cache.lookup(11), Lookup::Miss));
    }

    #[test]
    fn only_one_caller_refreshes_a_stale_entry() {
        let cache = FlagDeliveryCache::new();
        let mut entry = EnvironmentFlags::new(1, 10, vec![flag("a", true)]);
        entry.loaded_at = Instant::now() - TTL - Duration::from_secs(1);
        cache.insert(entry, cache.generation());

        let refreshes = (0..10)
            .filter(|_| matches!(cache.lookup(10), Lookup::Hit { refresh: true, .. }))
            .count();
        assert_eq!(refreshes, 1, "everyone else keeps serving the stale entry");
    }

    #[test]
    fn a_released_refresh_can_be_won_again() {
        let cache = FlagDeliveryCache::new();
        let mut entry = EnvironmentFlags::new(1, 10, vec![flag("a", true)]);
        entry.loaded_at = Instant::now() - TTL - Duration::from_secs(1);
        let entry = cache.insert(entry, cache.generation());

        assert!(matches!(
            cache.lookup(10),
            Lookup::Hit { refresh: true, .. }
        ));
        drop(RefreshClaim::new(entry.clone()));
        assert!(matches!(
            cache.lookup(10),
            Lookup::Hit { refresh: true, .. }
        ));
    }

    /// A refresh whose request is cancelled mid-load drops its claim with
    /// it, so the next request refreshes instead of serving stale forever.
    #[tokio::test]
    async fn a_cancelled_refresh_releases_its_claim() {
        let cache = FlagDeliveryCache::new();
        let mut entry = EnvironmentFlags::new(1, 10, vec![flag("a", true)]);
        entry.loaded_at = Instant::now() - TTL - Duration::from_secs(1);
        let entry = cache.insert(entry, cache.generation());
        assert!(matches!(
            cache.lookup(10),
            Lookup::Hit { refresh: true, .. }
        ));

        let pending = tokio::spawn(async move {
            let _claim = RefreshClaim::new(entry);
            std::future::pending::<()>().await;
        });
        tokio::task::yield_now().await;
        pending.abort();
        let _ = pending.await;

        assert!(matches!(
            cache.lookup(10),
            Lookup::Hit { refresh: true, .. }
        ));
    }

    #[test]
    fn peek_never_claims_a_refresh() {
        let cache = FlagDeliveryCache::new();
        let mut entry = EnvironmentFlags::new(1, 10, vec![flag("a", true)]);
        entry.loaded_at = Instant::now() - TTL - Duration::from_secs(1);
        cache.insert(entry, cache.generation());

        assert!(cache.peek(10).is_some());
        assert!(cache.peek(11).is_none());
        assert!(matches!(
            cache.lookup(10),
            Lookup::Hit { refresh: true, .. }
        ));
    }

    #[test]
    fn invalidating_a_project_drops_only_its_environments() {
        let cache = FlagDeliveryCache::new();
        cache.insert(EnvironmentFlags::new(1, 10, vec![]), cache.generation());
        cache.insert(EnvironmentFlags::new(1, 11, vec![]), cache.generation());
        cache.insert(EnvironmentFlags::new(2, 20, vec![]), cache.generation());

        cache.invalidate_project(1);

        assert!(matches!(cache.lookup(10), Lookup::Miss));
        assert!(matches!(cache.lookup(11), Lookup::Miss));
        assert!(matches!(cache.lookup(20), Lookup::Hit { .. }));
    }

    #[test]
    fn the_cache_never_grows_past_its_bound() {
        let cache = FlagDeliveryCache::new();
        for environment_id in 0..(MAX_ENTRIES as i32 + 10) {
            cache.insert(
                EnvironmentFlags::new(1, environment_id, vec![]),
                cache.generation(),
            );
        }
        assert!(cache.cached_environments() <= MAX_ENTRIES);
    }

    /// A load that started before a write must not put the pre-write flags
    /// back after that write invalidated the cache.
    #[test]
    fn a_load_that_raced_an_invalidation_is_served_but_not_cached() {
        let cache = FlagDeliveryCache::new();
        let before_write = cache.generation();
        cache.invalidate_project(1);

        let served = cache.insert(
            EnvironmentFlags::new(1, 10, vec![flag("a", true)]),
            before_write,
        );

        assert!(
            served.get("a").is_some(),
            "the caller still gets its result"
        );
        assert!(
            matches!(cache.lookup(10), Lookup::Miss),
            "but it is not cached"
        );
    }

    /// A refresh discarded by a racing invalidation (of any project) must not
    /// leave the stale entry marked as being refreshed, or nobody would ever
    /// refresh it again.
    #[test]
    fn a_discarded_refresh_leaves_the_entry_refreshable() {
        let cache = FlagDeliveryCache::new();
        let mut entry = EnvironmentFlags::new(1, 10, vec![flag("a", true)]);
        entry.loaded_at = Instant::now() - TTL - Duration::from_secs(1);
        let stale = cache.insert(entry, cache.generation());

        let Lookup::Hit { refresh: true, .. } = cache.lookup(10) else {
            panic!("expected to win the refresh");
        };
        let generation = cache.generation();
        cache.invalidate_project(2);
        cache.insert(EnvironmentFlags::new(1, 10, vec![]), generation);
        drop(RefreshClaim::new(stale));

        assert!(matches!(
            cache.lookup(10),
            Lookup::Hit { refresh: true, .. }
        ));
    }

    #[test]
    fn flags_are_found_by_key_whatever_order_they_were_loaded_in() {
        let entry = EnvironmentFlags::new(1, 10, vec![flag("zeta", true), flag("alpha", false)]);
        assert!(entry.get("alpha").is_some());
        assert!(entry.get("zeta").is_some());
        assert!(entry.get("missing").is_none());
    }

    #[test]
    fn the_client_etag_ignores_server_only_flags() {
        let with_hidden = EnvironmentFlags::new(1, 10, vec![flag("a", true), flag("b", false)]);
        let without = EnvironmentFlags::new(1, 10, vec![flag("a", true)]);
        assert_eq!(with_hidden.client_etag, without.client_etag);
        assert_ne!(with_hidden.server_etag, without.server_etag);
    }
}
