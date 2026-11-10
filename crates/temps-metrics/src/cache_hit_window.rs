// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Postgres shared-buffer cache hit ratio over a trailing window.
//!
//! `pg_stat_database.blks_hit` / `blks_read` are lifetime counters: they
//! accumulate from the last statistics reset, and since PostgreSQL 15 they
//! even survive a clean restart. A ratio computed straight from them is
//! dominated by history. After a bulk load that read most of its blocks
//! from disk, the ratio stays low for days, and tuning `shared_buffers` and
//! restarting barely moves it. The console and the "Low cache hit ratio"
//! alarm need the hit rate of the workload running now.
//!
//! The Postgres collector therefore reports the raw counters
//! ([`PG_BLKS_HIT`], [`PG_BLKS_READ`]) as [`MetricKind::Counter`] points.
//! The scraper turns them into per-scrape deltas, and a counter that goes
//! backwards (a stats reset or a crash restart) is treated as a reset, never as
//! a negative delta. [`CacheHitWindows::derive`] then keeps each series' deltas
//! for the last [`CACHE_HIT_WINDOW`] and replaces the counter points with two
//! gauges, which are the only ones persisted:
//!
//! * [`PG_CACHE_HIT_RATIO`]: `Σhit / Σ(hit + read)` over the window, a
//!   weighted ratio (never an average of per-scrape ratios), 0.0 to 1.0.
//!   Omitted when the window saw no block access, because 0/0 is "no data",
//!   not 100%.
//! * [`PG_CACHE_BLOCKS_ACCESSED`]: `Σ(hit + read)` over the window, the
//!   volume behind the ratio. The alarm evaluator gates the ratio rule on it.
//!
//! The collector reports the counters once per database (`datname` label).
//! Each database is its own series, and the instance-wide series (no
//! `datname`) is built here from the sum of the databases' deltas in each
//! scrape, never from a summed counter. A summed counter goes backwards when
//! a database is dropped, and the delta step would read that as a reset:
//! one interval holding the remaining databases' whole lifetime, which is
//! exactly the history this window exists to exclude. Summing deltas
//! instead, a dropped database simply stops contributing, and a database
//! whose own statistics were reset contributes what it did since the reset.
//!
//! Memory is bounded: one entry per scrape per series, and at the minimum
//! 10 s scrape interval a 15-minute window holds 90 entries of 24 bytes.
//! There is one series per database of each monitored service, and a
//! database that disappears from `pg_stat_database` is dropped on that
//! source's next scrape. Nothing here does I/O.

use std::collections::{HashMap, HashSet, VecDeque};

use chrono::{DateTime, Duration, Utc};

use crate::scraper::labels_key;
use crate::store::{MetricKind, MetricPoint};

/// Raw cumulative `pg_stat_database.blks_hit`, emitted by the collector as a
/// counter and consumed here. Never persisted.
pub const PG_BLKS_HIT: &str = "pg.blks_hit";

/// Raw cumulative `pg_stat_database.blks_read`, emitted by the collector as a
/// counter and consumed here. Never persisted.
pub const PG_BLKS_READ: &str = "pg.blks_read";

/// Shared-buffer hit ratio over [`CACHE_HIT_WINDOW`], 0.0 to 1.0.
pub const PG_CACHE_HIT_RATIO: &str = "pg.cache_hit_ratio";

/// Block accesses (hits + reads) over [`CACHE_HIT_WINDOW`].
pub const PG_CACHE_BLOCKS_ACCESSED: &str = "pg.cache_blocks_accessed";

/// Label naming the database of a per-database block counter.
pub const PG_DATABASE_LABEL: &str = "datname";

/// Length of the trailing window the ratio covers. The console labels the
/// value "last 15 min"; keep the two in sync.
pub const CACHE_HIT_WINDOW: Duration = Duration::minutes(15);

/// One scrape interval's block accesses for a series.
#[derive(Debug, Clone, Copy)]
struct Interval {
    /// When the scrape that closed this interval ran.
    time: DateTime<Utc>,
    hit: f64,
    read: f64,
}

/// Trailing per-interval deltas of one `(source, labels)` series.
#[derive(Debug, Default)]
struct Series {
    intervals: VecDeque<Interval>,
}

impl Series {
    /// Record an interval and drop every interval that has left the window
    /// ending at `interval.time`. Returns `(hits, accesses)` over the window.
    fn push(&mut self, interval: Interval, window: Duration) -> (f64, f64) {
        // Scrapes of one service never run concurrently, so time only goes
        // backwards if the clock was stepped. Restart the window rather than
        // keep intervals that may now be in the future.
        if self
            .intervals
            .back()
            .is_some_and(|last| last.time > interval.time)
        {
            self.intervals.clear();
        }
        self.intervals.push_back(interval);
        let cutoff = interval.time - window;
        while self.intervals.front().is_some_and(|i| i.time <= cutoff) {
            self.intervals.pop_front();
        }
        self.intervals.iter().fold((0.0, 0.0), |(hits, total), i| {
            (hits + i.hit, total + i.hit + i.read)
        })
    }
}

/// Key of one series: the source plus its canonical label key (the same
/// [`labels_key`] the scraper's counter baselines use). The instance-wide
/// series has the key of a database's labels without [`PG_DATABASE_LABEL`].
type SeriesKey = (i32, String);

/// Rolling cache-hit windows of every monitored Postgres series.
#[derive(Debug)]
pub struct CacheHitWindows {
    window: Duration,
    series: HashMap<SeriesKey, Series>,
}

impl Default for CacheHitWindows {
    fn default() -> Self {
        Self::new(CACHE_HIT_WINDOW)
    }
}

impl CacheHitWindows {
    pub fn new(window: Duration) -> Self {
        Self {
            window,
            series: HashMap::new(),
        }
    }

    /// Replace the [`PG_BLKS_HIT`] / [`PG_BLKS_READ`] delta points of one
    /// source's scrape with the windowed [`PG_CACHE_HIT_RATIO`] and
    /// [`PG_CACHE_BLOCKS_ACCESSED`] gauges. Every other point passes through
    /// unchanged.
    ///
    /// `points` must already have been through the scraper's delta step, so
    /// the counters are non-negative per-interval deltas. A database whose
    /// hit or read delta is missing this scrape (its baseline was only just
    /// recorded) contributes nothing yet, to its own series or to the
    /// instance-wide one. Counters without a [`PG_DATABASE_LABEL`] are
    /// dropped: the instance-wide series is always derived here.
    pub fn derive(&mut self, source_id: i32, points: Vec<MetricPoint>) -> Vec<MetricPoint> {
        let mut out = Vec::with_capacity(points.len());
        // Per label key: the hit point (kept as the template for the gauges'
        // time, labels and source fields) and the read delta.
        let mut pairs: HashMap<String, (Option<MetricPoint>, Option<f64>)> = HashMap::new();
        for pt in points {
            if pt.kind == MetricKind::Counter && pt.name == PG_BLKS_HIT {
                let key = labels_key(&pt.labels);
                pairs.entry(key).or_default().0 = Some(pt);
            } else if pt.kind == MetricKind::Counter && pt.name == PG_BLKS_READ {
                pairs.entry(labels_key(&pt.labels)).or_default().1 = Some(pt.value);
            } else {
                out.push(pt);
            }
        }
        if pairs.is_empty() {
            return out;
        }

        let mut seen: HashSet<String> = HashSet::with_capacity(pairs.len() + 1);
        // This scrape's instance-wide interval: the sum of the databases'.
        let mut instance: Option<(MetricPoint, Interval)> = None;
        for (key, pair) in pairs {
            seen.insert(key.clone());
            let (Some(template), Some(read)) = pair else {
                continue;
            };
            if !template.labels.contains_key(PG_DATABASE_LABEL) {
                continue;
            }
            let interval = Interval {
                time: template.time,
                // Deltas are non-negative by the scraper's contract; clamp so
                // a violation can never yield a ratio outside 0..=1.
                hit: template.value.max(0.0),
                read: read.max(0.0),
            };
            match &mut instance {
                Some((_, sum)) => {
                    sum.time = sum.time.max(interval.time);
                    sum.hit += interval.hit;
                    sum.read += interval.read;
                }
                None => {
                    let mut aggregate = template.clone();
                    aggregate.labels.remove(PG_DATABASE_LABEL);
                    instance = Some((aggregate, interval));
                }
            }
            self.push_series(source_id, key, &template, interval, &mut out);
        }
        if let Some((template, interval)) = instance {
            let key = labels_key(&template.labels);
            seen.insert(key.clone());
            self.push_series(source_id, key, &template, interval, &mut out);
        }

        // A database dropped since the last scrape no longer reports
        // counters: forget its window so the map tracks only live series.
        self.series
            .retain(|(sid, key), _| *sid != source_id || seen.contains(key));
        out
    }

    /// Record `interval` in a series and emit its windowed gauges.
    fn push_series(
        &mut self,
        source_id: i32,
        key: String,
        template: &MetricPoint,
        interval: Interval,
        out: &mut Vec<MetricPoint>,
    ) {
        let (hits, accesses) = self
            .series
            .entry((source_id, key))
            .or_default()
            .push(interval, self.window);
        if accesses > 0.0 {
            out.push(gauge(template, PG_CACHE_HIT_RATIO, hits / accesses));
        }
        out.push(gauge(template, PG_CACHE_BLOCKS_ACCESSED, accesses));
    }

    /// Forget every window of `source_id`, e.g. after an HA failover, when the
    /// new primary's counters belong to a different server.
    pub fn reset_source(&mut self, source_id: i32) {
        self.series.retain(|(sid, _), _| *sid != source_id);
    }

    /// Keep only the windows of `active` sources.
    pub fn retain_sources(&mut self, active: &HashSet<i32>) {
        self.series.retain(|(sid, _), _| active.contains(sid));
    }
}

fn gauge(template: &MetricPoint, name: &str, value: f64) -> MetricPoint {
    MetricPoint {
        name: name.to_string(),
        value,
        kind: MetricKind::Gauge,
        ..template.clone()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::SourceKind;

    fn at(minute: i64) -> DateTime<Utc> {
        DateTime::<Utc>::from_timestamp(1_760_000_000, 0).unwrap() + Duration::minutes(minute)
    }

    fn counter(name: &str, value: f64, time: DateTime<Utc>, db: Option<&str>) -> MetricPoint {
        let mut labels = HashMap::new();
        if let Some(db) = db {
            labels.insert(PG_DATABASE_LABEL.to_string(), db.to_string());
        }
        MetricPoint {
            time,
            source_kind: SourceKind::Database,
            source_id: 1,
            name: name.to_string(),
            value,
            kind: MetricKind::Counter,
            engine: Some("postgres".into()),
            environment: None,
            node_id: None,
            labels,
        }
    }

    /// One database's per-scrape deltas.
    fn deltas(hit: f64, read: f64, time: DateTime<Utc>, db: &str) -> Vec<MetricPoint> {
        vec![
            counter(PG_BLKS_HIT, hit, time, Some(db)),
            counter(PG_BLKS_READ, read, time, Some(db)),
        ]
    }

    /// `db: None` reads the instance-wide series.
    fn value(points: &[MetricPoint], name: &str, db: Option<&str>) -> Option<f64> {
        points
            .iter()
            .find(|p| p.name == name && p.labels.get(PG_DATABASE_LABEL).map(String::as_str) == db)
            .map(|p| p.value)
    }

    #[test]
    fn ratio_is_weighted_over_the_window_and_counters_are_not_persisted() {
        let mut windows = CacheHitWindows::default();
        windows.derive(1, deltas(900.0, 100.0, at(0), "app"));
        let out = windows.derive(1, deltas(100.0, 900.0, at(1), "app"));

        // (900 + 100) / (1000 + 1000). Equal volumes, so this is also the
        // plain mean of 0.9 and 0.1; the uneven interval below tells them apart.
        assert_eq!(value(&out, PG_CACHE_HIT_RATIO, Some("app")), Some(0.5));
        assert_eq!(
            value(&out, PG_CACHE_BLOCKS_ACCESSED, Some("app")),
            Some(2000.0)
        );
        assert!(out
            .iter()
            .all(|p| p.name != PG_BLKS_HIT && p.name != PG_BLKS_READ));
        assert!(out.iter().all(|p| p.kind == MetricKind::Gauge));

        let out = windows.derive(1, deltas(9_000.0, 0.0, at(2), "app"));
        // 10_000 hits of 11_000 accesses (0.909), not the mean of the three
        // interval ratios (0.667): the busy interval dominates.
        let ratio = value(&out, PG_CACHE_HIT_RATIO, Some("app")).unwrap();
        assert!((ratio - 10_000.0 / 11_000.0).abs() < 1e-12, "{ratio}");
    }

    #[test]
    fn intervals_older_than_the_window_stop_counting() {
        let mut windows = CacheHitWindows::default();
        // A cold bulk load: almost every block read from disk.
        windows.derive(1, deltas(10.0, 990.0, at(0), "app"));
        // Fifteen minutes later that interval has left the window.
        let out = windows.derive(1, deltas(990.0, 10.0, at(15), "app"));
        assert_eq!(value(&out, PG_CACHE_HIT_RATIO, None), Some(0.99));
        assert_eq!(value(&out, PG_CACHE_BLOCKS_ACCESSED, None), Some(1000.0));
    }

    #[test]
    fn an_idle_window_reports_no_ratio_instead_of_dividing_by_zero() {
        let mut windows = CacheHitWindows::default();
        let out = windows.derive(1, deltas(0.0, 0.0, at(0), "app"));
        for db in [Some("app"), None] {
            assert_eq!(value(&out, PG_CACHE_HIT_RATIO, db), None);
            assert_eq!(value(&out, PG_CACHE_BLOCKS_ACCESSED, db), Some(0.0));
        }
    }

    #[test]
    fn the_instance_series_is_the_traffic_weighted_sum_of_the_databases() {
        let mut windows = CacheHitWindows::default();
        let mut batch = deltas(70.0, 30.0, at(0), "app");
        batch.extend(deltas(0.0, 900.0, at(0), "bulk"));
        let out = windows.derive(1, batch);

        assert_eq!(value(&out, PG_CACHE_HIT_RATIO, Some("app")), Some(0.7));
        assert_eq!(value(&out, PG_CACHE_HIT_RATIO, Some("bulk")), Some(0.0));
        // 70 hits of 1000 accesses: the busy database dominates, which is why
        // the console names the scope it shows.
        assert_eq!(value(&out, PG_CACHE_HIT_RATIO, None), Some(0.07));
        assert_eq!(value(&out, PG_CACHE_BLOCKS_ACCESSED, None), Some(1000.0));
        let instance = out
            .iter()
            .find(|p| p.name == PG_CACHE_HIT_RATIO && p.labels.is_empty());
        assert!(instance.is_some(), "the instance series carries no datname");
    }

    #[test]
    fn unlabelled_counters_never_become_a_second_instance_series() {
        let mut windows = CacheHitWindows::default();
        let mut batch = deltas(10.0, 0.0, at(0), "app");
        // A summed counter that "reset" would carry lifetime history.
        batch.push(counter(PG_BLKS_HIT, 0.0, at(0), None));
        batch.push(counter(PG_BLKS_READ, 70_000_000.0, at(0), None));
        let out = windows.derive(1, batch);
        assert_eq!(value(&out, PG_CACHE_HIT_RATIO, None), Some(1.0));
        assert_eq!(value(&out, PG_CACHE_BLOCKS_ACCESSED, None), Some(10.0));
    }

    #[test]
    fn a_database_missing_one_counter_waits_for_both() {
        let mut windows = CacheHitWindows::default();
        let out = windows.derive(1, vec![counter(PG_BLKS_HIT, 5.0, at(0), Some("app"))]);
        assert!(out.is_empty(), "{out:?}");
    }

    #[test]
    fn a_dropped_database_and_a_reset_source_are_forgotten() {
        let mut windows = CacheHitWindows::default();
        let mut batch = deltas(1.0, 1.0, at(0), "app");
        batch.extend(deltas(1.0, 1.0, at(0), "old"));
        windows.derive(1, batch);
        windows.derive(2, deltas(1.0, 1.0, at(0), "app"));
        // Source 1: app, old, instance. Source 2: app, instance.
        assert_eq!(windows.series.len(), 5);

        windows.derive(1, deltas(1.0, 1.0, at(1), "app"));
        assert_eq!(windows.series.len(), 4, "the dropped database is gone");

        windows.reset_source(1);
        assert_eq!(windows.series.len(), 2);
        windows.retain_sources(&HashSet::new());
        assert!(windows.series.is_empty());
    }

    #[test]
    fn a_clock_step_backwards_restarts_the_window() {
        let mut windows = CacheHitWindows::default();
        windows.derive(1, deltas(0.0, 100.0, at(10), "app"));
        let out = windows.derive(1, deltas(100.0, 0.0, at(5), "app"));
        assert_eq!(value(&out, PG_CACHE_HIT_RATIO, None), Some(1.0));
    }
}
