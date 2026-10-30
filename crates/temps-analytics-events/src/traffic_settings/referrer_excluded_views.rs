// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Recent page views a referrer rule or the spam list excluded, for Speed
//! samples that cannot be matched against the referrer themselves.
//!
//! Current SDKs send the measured page's referrer with every Speed sample, so
//! the Speed handler applies the referrer rules to it directly. SDKs released
//! before that send none, and stay bundled in deployed apps long after a
//! server upgrade. Their samples follow the page view they measure instead:
//! event ingest notes each referrer-excluded page view here, and the Speed
//! handler looks a sample's session and path up when it carries no referrer.
//!
//! Event ingest and Speed ingest are served by the same process, so this is
//! an in-memory map rather than a database query per sample. It is bounded by
//! entry count and age; an entry that is evicted or that another instance
//! holds only means an old-SDK sample is counted, as it was before.

use std::hash::{DefaultHasher, Hash, Hasher};
use std::time::Duration;

use moka::future::Cache;

/// How long after a page view its Speed samples can still follow it. Late
/// metrics (CLS, INP) arrive when the page is hidden, which is usually within
/// minutes; a page left open longer than this is counted.
const PAGE_VIEW_TTL: Duration = Duration::from_secs(30 * 60);

/// At most this many excluded page views are remembered (8 bytes each plus
/// the map's overhead, a few MB at most). Only excluded page views are
/// stored, which are a small share of traffic.
const MAX_PAGE_VIEWS: u64 = 100_000;

pub struct ReferrerExcludedPageViews {
    /// Keyed by a hash of `(project_id, session_id, pathname)`, so a lookup
    /// allocates nothing. A 64-bit collision could at worst exclude one
    /// sample.
    views: Cache<u64, ()>,
}

impl Default for ReferrerExcludedPageViews {
    fn default() -> Self {
        Self::new()
    }
}

impl ReferrerExcludedPageViews {
    pub fn new() -> Self {
        Self {
            views: Cache::builder()
                .max_capacity(MAX_PAGE_VIEWS)
                .time_to_live(PAGE_VIEW_TTL)
                .build(),
        }
    }

    fn key(project_id: i32, session_id: &str, pathname: &str) -> u64 {
        let mut hasher = DefaultHasher::new();
        (project_id, session_id, pathname).hash(&mut hasher);
        hasher.finish()
    }

    /// Record a page view of `pathname` in `session_id`: remembered when a
    /// referrer rule or the spam list excluded it, forgotten otherwise, so the
    /// session's latest view of the path decides, as a reload from another
    /// source should.
    pub async fn note_page_view(
        &self,
        project_id: i32,
        session_id: &str,
        pathname: &str,
        excluded_by_referrer: bool,
    ) {
        let key = Self::key(project_id, session_id, pathname);
        if excluded_by_referrer {
            self.views.insert(key, ()).await;
        } else {
            self.views.invalidate(&key).await;
        }
    }

    /// Whether the session's latest view of `pathname` was excluded by its
    /// referrer.
    pub fn contains(&self, project_id: i32, session_id: &str, pathname: &str) -> bool {
        self.views
            .contains_key(&Self::key(project_id, session_id, pathname))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn the_latest_view_of_a_path_in_a_session_decides() {
        let views = ReferrerExcludedPageViews::new();
        views.note_page_view(1, "session", "/pricing", true).await;
        assert!(views.contains(1, "session", "/pricing"));

        // Other sessions, paths and projects are not affected.
        assert!(!views.contains(1, "other-session", "/pricing"));
        assert!(!views.contains(1, "session", "/docs"));
        assert!(!views.contains(2, "session", "/pricing"));

        // The visitor comes back to the page from a counted source.
        views.note_page_view(1, "session", "/pricing", false).await;
        assert!(!views.contains(1, "session", "/pricing"));
    }
}
