// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! In-memory active surveys per project, for `/api/_temps/surveys`.
//!
//! The endpoint is called by page loads of sites that enable surveys, so it
//! may not query the database per request (CLAUDE.md hot-path rules). Same
//! design as the feature-flag delivery cache:
//!
//! - **Reads are lock-free**: one `ArcSwap` load and a hash lookup.
//! - **Freshness**: an entry is reused for [`TTL`]; every survey write drops
//!   the project's entry, so a single process sees it on the next request and
//!   split topologies converge within the TTL.
//! - **Load is bounded**: at most [`MAX_CONCURRENT_LOADS`] projects load at
//!   once; a stale entry is refreshed by one request while the rest are served
//!   the previous set.
//! - **Memory is bounded**: at most [`MAX_ENTRIES`] projects, each holding
//!   only its active surveys (at most [`MAX_ACTIVE_SURVEYS`]).

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use arc_swap::ArcSwap;
use tokio::sync::Semaphore;

use crate::selection::DeliveredSurvey;

pub const TTL: Duration = Duration::from_secs(5);
pub const MAX_CONCURRENT_LOADS: usize = 4;
pub const MAX_ENTRIES: usize = 4_096;
/// Active surveys delivered per project. More are refused at activation.
pub const MAX_ACTIVE_SURVEYS: usize = 50;

#[derive(Debug)]
pub struct ProjectSurveys {
    pub project_id: i32,
    /// Active surveys, oldest first.
    pub surveys: Vec<DeliveredSurvey>,
    loaded_at: Instant,
    refreshing: AtomicBool,
}

impl ProjectSurveys {
    pub fn new(project_id: i32, surveys: Vec<DeliveredSurvey>) -> Self {
        Self {
            project_id,
            surveys,
            loaded_at: Instant::now(),
            refreshing: AtomicBool::new(false),
        }
    }

    fn is_fresh(&self) -> bool {
        self.loaded_at.elapsed() < TTL
    }
}

pub enum Lookup {
    /// A usable entry; `refresh` is true when it is stale and this caller won
    /// the right to reload it.
    Hit {
        entry: Arc<ProjectSurveys>,
        refresh: bool,
    },
    Miss,
}

pub struct SurveyDeliveryCache {
    entries: ArcSwap<HashMap<i32, Arc<ProjectSurveys>>>,
    loads: Semaphore,
    /// Bumped by every invalidation, so a load that read the database before
    /// a write cannot cache what it read after the write dropped it.
    generation: AtomicU64,
}

impl Default for SurveyDeliveryCache {
    fn default() -> Self {
        Self::new()
    }
}

impl SurveyDeliveryCache {
    pub fn new() -> Self {
        Self {
            entries: ArcSwap::from_pointee(HashMap::new()),
            loads: Semaphore::new(MAX_CONCURRENT_LOADS),
            generation: AtomicU64::new(0),
        }
    }

    pub fn lookup(&self, project_id: i32) -> Lookup {
        let entries = self.entries.load();
        let Some(entry) = entries.get(&project_id) else {
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

    pub fn load_permit(&self) -> &Semaphore {
        &self.loads
    }

    pub fn generation(&self) -> u64 {
        self.generation.load(Ordering::Acquire)
    }

    /// Cache a fresh load unless an invalidation happened since
    /// `loaded_at_generation`. Returns the entry either way.
    pub fn insert(&self, entry: ProjectSurveys, loaded_at_generation: u64) -> Arc<ProjectSurveys> {
        let entry = Arc::new(entry);
        let project_id = entry.project_id;
        self.entries.rcu(|current| {
            if self.generation.load(Ordering::Acquire) != loaded_at_generation {
                return HashMap::clone(current);
            }
            let mut next: HashMap<i32, Arc<ProjectSurveys>> = HashMap::clone(current);
            if next.len() >= MAX_ENTRIES && !next.contains_key(&project_id) {
                next.retain(|_, existing| existing.is_fresh());
                if next.len() >= MAX_ENTRIES {
                    next.clear();
                }
            }
            next.insert(project_id, entry.clone());
            next
        });
        entry
    }

    pub fn release_refresh(&self, entry: &ProjectSurveys) {
        entry.refreshing.store(false, Ordering::Release);
    }

    pub fn invalidate_project(&self, project_id: i32) {
        self.generation.fetch_add(1, Ordering::AcqRel);
        self.entries.rcu(|current| {
            let mut next: HashMap<i32, Arc<ProjectSurveys>> = HashMap::clone(current);
            next.remove(&project_id);
            next
        });
    }

    #[cfg(test)]
    pub fn cached_projects(&self) -> usize {
        self.entries.load().len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_cached_project_is_a_fresh_hit() {
        let cache = SurveyDeliveryCache::new();
        assert!(matches!(cache.lookup(1), Lookup::Miss));
        let generation = cache.generation();
        cache.insert(ProjectSurveys::new(1, Vec::new()), generation);
        assert!(matches!(
            cache.lookup(1),
            Lookup::Hit { refresh: false, .. }
        ));
    }

    #[test]
    fn invalidation_drops_only_that_project() {
        let cache = SurveyDeliveryCache::new();
        let generation = cache.generation();
        cache.insert(ProjectSurveys::new(1, Vec::new()), generation);
        cache.insert(ProjectSurveys::new(2, Vec::new()), generation);
        cache.invalidate_project(1);
        assert!(matches!(cache.lookup(1), Lookup::Miss));
        assert!(matches!(cache.lookup(2), Lookup::Hit { .. }));
    }

    #[test]
    fn a_load_that_raced_an_invalidation_is_not_cached() {
        let cache = SurveyDeliveryCache::new();
        let before = cache.generation();
        cache.invalidate_project(1);
        let served = cache.insert(ProjectSurveys::new(1, Vec::new()), before);
        assert_eq!(served.project_id, 1);
        assert!(matches!(cache.lookup(1), Lookup::Miss));
    }

    #[test]
    fn a_stale_entry_is_refreshed_by_one_caller() {
        let cache = SurveyDeliveryCache::new();
        let generation = cache.generation();
        let stale = ProjectSurveys {
            project_id: 1,
            surveys: Vec::new(),
            loaded_at: Instant::now()
                .checked_sub(TTL + Duration::from_millis(1))
                .unwrap(),
            refreshing: AtomicBool::new(false),
        };
        cache.insert(stale, generation);
        assert!(matches!(cache.lookup(1), Lookup::Hit { refresh: true, .. }));
        assert!(matches!(
            cache.lookup(1),
            Lookup::Hit { refresh: false, .. }
        ));
        if let Lookup::Hit { entry, .. } = cache.lookup(1) {
            cache.release_refresh(&entry);
        }
        assert!(matches!(cache.lookup(1), Lookup::Hit { refresh: true, .. }));
    }

    #[test]
    fn the_cache_is_bounded() {
        let cache = SurveyDeliveryCache::new();
        let generation = cache.generation();
        for project in 0..(MAX_ENTRIES as i32 + 10) {
            cache.insert(ProjectSurveys::new(project, Vec::new()), generation);
        }
        assert!(cache.cached_projects() <= MAX_ENTRIES);
    }
}
