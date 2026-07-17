//! In-process aggregation of "something went wrong" signals for anonymous
//! telemetry.
//!
//! Self-hosted instances fail where the maintainers can't see them. This
//! module counts three classes of internal errors so a periodic, aggregated
//! `error_summary` telemetry event (see [`crate::telemetry`]) can surface
//! *that* and *where* instances break — without ever collecting the error
//! itself:
//!
//! - `log_error`: ERROR-level tracing events, keyed by target (module path)
//! - `http_5xx`: console-API 5xx responses, keyed by method + route template
//! - `panic`: panics, keyed by sanitized source location (`file:line`)
//!
//! # Privacy contract
//!
//! Every key is a **compile-time identifier of our own code**: a tracing
//! target, an axum route template (`/api/projects/{id}` — never the resolved
//! URL), or a crate-relative source path. Error *messages* are never recorded
//! — in this codebase they deliberately embed IDs, resource names, and paths
//! (see CLAUDE.md's error rules), which makes them radioactive for telemetry.
//! Panic *payloads* are likewise never recorded.
//!
//! # Bounds
//!
//! The counter map is capped at [`MAX_TRACKED_KEYS`] distinct keys; overflow
//! occurrences are still counted (in `overflow`) but not keyed, so a
//! pathological instance can't grow memory or event size. Recording takes a
//! short-lived `Mutex` — acceptable because errors are not a hot path (the
//! 5xx middleware runs per console-API request but only acquires the lock on
//! server-error responses), and the proxy data path never calls into this
//! module.

use std::collections::HashMap;
use std::sync::{LazyLock, Mutex};

/// Maximum number of distinct (category, key) counters kept in memory.
/// Occurrences past the cap are aggregated into [`ErrorSummary::overflow`].
pub const MAX_TRACKED_KEYS: usize = 128;

/// Maximum number of per-key entries included in a drained summary. Keys are
/// sorted by count descending, so these are the most frequent offenders.
pub const MAX_REPORTED_KEYS: usize = 20;

/// Category for ERROR-level tracing events (key = tracing target).
pub const CATEGORY_LOG_ERROR: &str = "log_error";
/// Category for console-API 5xx responses (key = "METHOD /route/{template} STATUS").
pub const CATEGORY_HTTP_5XX: &str = "http_5xx";
/// Category for panics (key = sanitized "file:line").
pub const CATEGORY_PANIC: &str = "panic";

/// Bounded counter store. Prefer the free functions ([`record_log_error`],
/// [`record_http_5xx`], [`record_panic`]) which write to the process-global
/// instance; construct your own only in tests.
#[derive(Default)]
pub struct ErrorCounters {
    inner: Mutex<CountersState>,
}

#[derive(Default)]
struct CountersState {
    counts: HashMap<(&'static str, String), u64>,
    /// Occurrences that arrived after the key cap was reached and whose key
    /// was not already tracked. Counted so truncation is visible, per the
    /// no-silent-caps rule.
    overflow: u64,
}

/// One aggregated counter, as reported in [`ErrorSummary::top`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ErrorCount {
    pub category: &'static str,
    pub key: String,
    pub count: u64,
}

/// Snapshot produced by [`ErrorCounters::drain`]. Counters reset to empty.
#[derive(Debug, Clone)]
pub struct ErrorSummary {
    /// Total recorded occurrences, including overflow.
    pub total: u64,
    /// Occurrences dropped from per-key tracking by [`MAX_TRACKED_KEYS`].
    pub overflow: u64,
    /// Per-category totals, sorted by category name for stable output.
    pub category_totals: Vec<(&'static str, u64)>,
    /// The most frequent keys, sorted by count descending, capped at
    /// [`MAX_REPORTED_KEYS`].
    pub top: Vec<ErrorCount>,
}

impl ErrorCounters {
    /// Count one occurrence of `key` in `category`. Never blocks beyond the
    /// short map insert; never fails.
    pub fn record(&self, category: &'static str, key: impl Into<String>) {
        let mut state = match self.inner.lock() {
            Ok(state) => state,
            // A poisoned lock means a panic mid-insert; losing counters is
            // preferable to propagating the panic out of error accounting.
            Err(poisoned) => poisoned.into_inner(),
        };
        let entry_key = (category, key.into());
        if let Some(count) = state.counts.get_mut(&entry_key) {
            *count += 1;
        } else if state.counts.len() < MAX_TRACKED_KEYS {
            state.counts.insert(entry_key, 1);
        } else {
            state.overflow += 1;
        }
    }

    /// Current count for one (category, key) pair without resetting anything.
    /// Lets callers (primarily tests observing the process-global instance)
    /// assert on specific keys without draining counters that concurrent code
    /// may also be using.
    pub fn count_for(&self, category: &'static str, key: &str) -> u64 {
        let state = match self.inner.lock() {
            Ok(state) => state,
            Err(poisoned) => poisoned.into_inner(),
        };
        state
            .counts
            .get(&(category, key.to_string()))
            .copied()
            .unwrap_or(0)
    }

    /// Take the current counters, resetting them to empty. Returns `None`
    /// when nothing was recorded, so idle instances emit nothing.
    pub fn drain(&self) -> Option<ErrorSummary> {
        let state = {
            let mut state = match self.inner.lock() {
                Ok(state) => state,
                Err(poisoned) => poisoned.into_inner(),
            };
            std::mem::take(&mut *state)
        };

        if state.counts.is_empty() && state.overflow == 0 {
            return None;
        }

        let mut category_totals: HashMap<&'static str, u64> = HashMap::new();
        let mut total = state.overflow;
        for ((category, _), count) in &state.counts {
            *category_totals.entry(category).or_insert(0) += count;
            total += count;
        }
        let mut category_totals: Vec<_> = category_totals.into_iter().collect();
        category_totals.sort_by_key(|(category, _)| *category);

        let mut top: Vec<ErrorCount> = state
            .counts
            .into_iter()
            .map(|((category, key), count)| ErrorCount {
                category,
                key,
                count,
            })
            .collect();
        // Secondary sort on (category, key) keeps equal counts deterministic.
        top.sort_by(|a, b| {
            b.count
                .cmp(&a.count)
                .then_with(|| a.category.cmp(b.category))
                .then_with(|| a.key.cmp(&b.key))
        });
        top.truncate(MAX_REPORTED_KEYS);

        Some(ErrorSummary {
            total,
            overflow: state.overflow,
            category_totals,
            top,
        })
    }
}

static GLOBAL: LazyLock<ErrorCounters> = LazyLock::new(ErrorCounters::default);

/// The process-global counter store, shared by the tracing layer, the console
/// 5xx middleware, and the panic hook, and drained by the telemetry flusher.
pub fn global() -> &'static ErrorCounters {
    &GLOBAL
}

/// Count an ERROR-level tracing event by its target (module path — a
/// compile-time identifier, never the message).
pub fn record_log_error(target: &str) {
    global().record(CATEGORY_LOG_ERROR, target);
}

/// Count a console-API 5xx response by method + route template + status.
/// `route` must be a route TEMPLATE (e.g. from axum's `MatchedPath`), never a
/// concrete request path.
pub fn record_http_5xx(method: &str, route: &str, status: u16) {
    global().record(CATEGORY_HTTP_5XX, format!("{method} {route} {status}"));
}

/// Count a panic by its sanitized source location. The panic payload/message
/// is deliberately ignored.
pub fn record_panic(location: Option<&std::panic::Location<'_>>) {
    let key = match location {
        Some(location) => sanitize_source_path(location.file(), location.line()),
        None => "unknown".to_string(),
    };
    global().record(CATEGORY_PANIC, key);
}

/// Reduce a compile-time source path to a build-machine-independent form.
///
/// Local builds embed absolute paths (`/Users/alice/src/temps/crates/...`)
/// and dependency panics embed registry paths
/// (`~/.cargo/registry/src/index.crates.io-.../tokio-1.40.0/src/...`). Both
/// leak the build user's directory layout, so keep only the meaningful
/// suffix: from `crates/` for workspace code, or the last three path
/// components otherwise.
fn sanitize_source_path(file: &str, line: u32) -> String {
    let normalized = file.replace('\\', "/");
    // Anchor on a path COMPONENT named `crates` (leading slash or path start),
    // not a bare substring — `my-crates/…` must not match. rfind so the
    // workspace-level `crates/` wins if a parent directory is also named
    // `crates` (e.g. a checkout under ~/crates/temps).
    let trimmed = if normalized.starts_with("crates/") {
        normalized.as_str()
    } else if let Some(idx) = normalized.rfind("/crates/") {
        &normalized[idx + 1..]
    } else {
        let mut components: Vec<&str> = normalized.rsplit('/').take(3).collect();
        components.reverse();
        return format!("{}:{line}", components.join("/"));
    };
    format!("{trimmed}:{line}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn record_and_drain_counts_by_category_and_key() {
        let counters = ErrorCounters::default();
        counters.record(CATEGORY_LOG_ERROR, "temps_backup::service");
        counters.record(CATEGORY_LOG_ERROR, "temps_backup::service");
        counters.record(CATEGORY_HTTP_5XX, "GET /api/projects/{id} 500");

        let summary = counters.drain().expect("summary present");
        assert_eq!(summary.total, 3);
        assert_eq!(summary.overflow, 0);
        assert_eq!(
            summary.category_totals,
            vec![(CATEGORY_HTTP_5XX, 1), (CATEGORY_LOG_ERROR, 2)]
        );
        assert_eq!(summary.top[0].key, "temps_backup::service");
        assert_eq!(summary.top[0].count, 2);

        // Drain resets: a second drain is empty.
        assert!(counters.drain().is_none());
    }

    #[test]
    fn drain_on_empty_returns_none() {
        let counters = ErrorCounters::default();
        assert!(counters.drain().is_none());
    }

    #[test]
    fn key_cap_overflows_into_counter_but_existing_keys_still_count() {
        let counters = ErrorCounters::default();
        for i in 0..MAX_TRACKED_KEYS {
            counters.record(CATEGORY_LOG_ERROR, format!("target_{i}"));
        }
        // New key past the cap: dropped into overflow.
        counters.record(CATEGORY_LOG_ERROR, "one_too_many");
        // Existing key past the cap: still counted normally.
        counters.record(CATEGORY_LOG_ERROR, "target_0");

        let summary = counters.drain().expect("summary present");
        assert_eq!(summary.overflow, 1);
        assert_eq!(summary.total, MAX_TRACKED_KEYS as u64 + 2);
        assert_eq!(summary.top.len(), MAX_REPORTED_KEYS);
        assert_eq!(summary.top[0].key, "target_0");
        assert_eq!(summary.top[0].count, 2);
    }

    #[test]
    fn top_is_sorted_desc_and_deterministic_on_ties() {
        let counters = ErrorCounters::default();
        counters.record(CATEGORY_LOG_ERROR, "b_target");
        counters.record(CATEGORY_LOG_ERROR, "a_target");
        counters.record(CATEGORY_PANIC, "crates/x/src/lib.rs:1");
        counters.record(CATEGORY_PANIC, "crates/x/src/lib.rs:1");

        let summary = counters.drain().expect("summary present");
        assert_eq!(summary.top[0].category, CATEGORY_PANIC);
        assert_eq!(summary.top[0].count, 2);
        // Tied counts fall back to category, then key ordering.
        assert_eq!(summary.top[1].key, "a_target");
        assert_eq!(summary.top[2].key, "b_target");
    }

    #[test]
    fn sanitize_source_path_strips_build_machine_prefixes() {
        assert_eq!(
            sanitize_source_path(
                "/Users/alice/projects/temps/crates/temps-backup/src/service.rs",
                42
            ),
            "crates/temps-backup/src/service.rs:42"
        );
        // Dependency path: no crates/ segment — keep the last 3 components.
        assert_eq!(
            sanitize_source_path(
                "/home/alice/.cargo/registry/src/index.crates.io-6f17d22bba15001f/tokio-1.40.0/src/runtime/task.rs",
                7
            ),
            "src/runtime/task.rs:7"
        );
        // Windows separators are normalized.
        assert_eq!(
            sanitize_source_path(r"C:\build\temps\crates\temps-core\src\lib.rs", 3),
            "crates/temps-core/src/lib.rs:3"
        );
        // A directory merely CONTAINING "crates" must not anchor the trim.
        assert_eq!(
            sanitize_source_path("/home/alice/my-crates/temps/src/lib.rs", 9),
            "temps/src/lib.rs:9"
        );
        // Checkout under ~/crates: the rightmost `crates/` component wins, so
        // the home directory is still stripped.
        assert_eq!(
            sanitize_source_path("/home/alice/crates/temps/crates/temps-core/src/lib.rs", 5),
            "crates/temps-core/src/lib.rs:5"
        );
        // Relative build paths (CI) that already start at crates/ pass through.
        assert_eq!(
            sanitize_source_path("crates/temps-core/src/lib.rs", 1),
            "crates/temps-core/src/lib.rs:1"
        );
    }

    #[test]
    fn poisoned_lock_does_not_panic() {
        let counters = std::sync::Arc::new(ErrorCounters::default());
        let poisoner = counters.clone();
        let _ = std::thread::spawn(move || {
            let _guard = poisoner.inner.lock().unwrap();
            panic!("poison the counters lock");
        })
        .join();

        counters.record(CATEGORY_LOG_ERROR, "after_poison");
        let summary = counters.drain().expect("summary present");
        assert_eq!(summary.total, 1);
    }
}
