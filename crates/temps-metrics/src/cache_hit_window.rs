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
//! a negative delta. That step sees each counter alone, so after
//! `pg_stat_reset()` a counter whose count since the reset already exceeds its
//! old value looks as if it kept growing, and the interval would mix both sides
//! of the reset (9,900 hits since the reset next to 100 of the 10,100 reads
//! reads as 99% instead of 49.5%). The collector therefore also reports each
//! database's `stats_reset` time ([`PG_STATS_RESET`]), and the interval in
//! which it changes is dropped. [`CacheHitWindows::derive`] then keeps each series' deltas
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
//! Neither is written until the series has been tracked for a whole window.
//! The windows live in memory: after a Temps restart, an HA failover or an
//! outage longer than the window they refill from empty, and the first
//! scrapes' sums cover a minute, not 15. Written, such a ratio would be
//! judged against the volume stored before the reset (the store's latest
//! value looks back 15 minutes) and could resolve or open an alarm, and a
//! partial volume would read as a nearly idle database. While a window
//! refills, the latest values stay the last whole window's, then none.
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
//! An interval spans from the source's previous successful scrape to this
//! one, and counts only while it lies entirely inside the window. A failed
//! scrape leaves the counter baselines alone, so the first delta after an
//! outage covers the whole outage. When that is longer than the window, the
//! delta cannot honestly be called "the last 15 minutes": the series reports
//! nothing until the window holds intervals again. A shorter gap's delta
//! counts while it lies inside the window; once it leaves, the rest covers
//! only the minutes since the gap, and the series reports nothing until the
//! window is whole again (minutes, not 15, would read as a nearly idle
//! database to the alarm's volume gate). A configured scrape
//! interval could cause the same on every scrape, so it is capped at
//! `temps_config::MAX_SCRAPE_INTERVAL_SECS` (5 minutes): rejected above that
//! when the settings are saved, and clamped with a warning by the scraper for
//! a value stored before the cap existed.
//!
//! Memory is bounded: one entry per scrape per series, and at the minimum
//! 10 s scrape interval a 15-minute window holds 90 entries of 32 bytes.
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

/// `pg_stat_database.stats_reset` of a database as Unix seconds, 0 while it
/// was never reset, emitted by the collector as a gauge and consumed here.
/// Never persisted.
pub const PG_STATS_RESET: &str = "pg.stats_reset";

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
    /// When the source's previous successful scrape ran: the delta's
    /// baseline. Equal to `end` when that is unknown.
    start: DateTime<Utc>,
    /// When the scrape that closed this interval ran.
    end: DateTime<Utc>,
    hit: f64,
    read: f64,
}

/// Trailing per-interval deltas of one `(source, labels)` series.
#[derive(Debug, Default)]
struct Series {
    intervals: VecDeque<Interval>,
    /// Start of the first interval since the window was last empty: from
    /// then on every interval was recorded, so the window is whole once this
    /// is a full window old.
    tracked_since: Option<DateTime<Utc>>,
    /// Length of the interval dropped last at the window's start, the one
    /// just before the first interval kept: a scrape, or a gap.
    dropped_before: Option<Duration>,
}

/// A series' window after recording an interval.
#[derive(Debug, Clone, Copy, PartialEq)]
struct WindowSum {
    hits: f64,
    accesses: f64,
    /// Whether the intervals cover the whole window, short of at most the
    /// scrape dropped at its start or a tenth of the window, rather than only
    /// the time since it started refilling or since a gap.
    full: bool,
}

impl Series {
    /// Record an interval and drop every interval no longer entirely inside
    /// the window ending at `interval.end`. Returns the sums over the window,
    /// or `None` when nothing is left in it (the new interval itself started
    /// before the window).
    fn push(&mut self, interval: Interval, window: Duration) -> Option<WindowSum> {
        // Scrapes of one service never run concurrently, so time only goes
        // backwards if the clock was stepped. Restart the window rather than
        // keep intervals that may now be in the future.
        if self
            .intervals
            .back()
            .is_some_and(|last| last.end > interval.end)
        {
            self.intervals.clear();
        }
        if self.intervals.is_empty() {
            self.tracked_since = Some(interval.start);
            self.dropped_before = None;
        }
        self.intervals.push_back(interval);
        let cutoff = interval.end - window;
        while self
            .intervals
            .front()
            .is_some_and(|i| i.start < cutoff || i.end <= cutoff)
        {
            if let Some(dropped) = self.intervals.pop_front() {
                self.dropped_before = Some(dropped.end - dropped.start);
            }
        }
        if self.intervals.is_empty() {
            self.tracked_since = None;
            self.dropped_before = None;
            return None;
        }
        let (hits, accesses) = self.intervals.iter().fold((0.0, 0.0), |(hits, total), i| {
            (hits + i.hit, total + i.hit + i.read)
        });
        // The interval that straddled the cutoff was dropped, so the window
        // begins less than that interval after its start. A normal scrape is
        // the configured interval plus a cycle that varies by seconds, at
        // most half as long again as the current one; a missed scrape is at
        // least twice as long, and after a gap shorter than the window the
        // dropped interval is the gap and the rest may cover only the minutes
        // since. Then the window must reach back to within a tenth of its
        // length, whatever the scrape interval.
        let current = interval.end - interval.start;
        let dropped_a_scrape = self
            .dropped_before
            .is_some_and(|dropped| dropped * 2 <= current * 3);
        let uncovered = self
            .intervals
            .front()
            .map_or(window, |first| first.start - cutoff);
        Some(WindowSum {
            hits,
            accesses,
            full: self
                .tracked_since
                .is_some_and(|since| interval.end - since >= window)
                && (dropped_a_scrape || uncovered <= window / 10),
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
    /// Time of each source's latest successful scrape: the start of its next
    /// interval, since the scraper's counter baselines date from then.
    last_scrape: HashMap<i32, DateTime<Utc>>,
    /// Last [`PG_STATS_RESET`] seen for each database series.
    stats_reset: HashMap<SeriesKey, f64>,
}

impl Default for CacheHitWindows {
    /// Windows of [`CACHE_HIT_WINDOW`], the length the console labels.
    fn default() -> Self {
        Self::new(CACHE_HIT_WINDOW)
    }
}

impl CacheHitWindows {
    /// Empty windows of length `window`; every series starts refilling.
    pub fn new(window: Duration) -> Self {
        Self {
            window,
            series: HashMap::new(),
            last_scrape: HashMap::new(),
            stats_reset: HashMap::new(),
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
    /// instance-wide one. Neither does a database whose [`PG_STATS_RESET`]
    /// changed since the previous scrape, whose deltas straddle the reset.
    /// Counters without a [`PG_DATABASE_LABEL`] are dropped: the
    /// instance-wide series is always derived here.
    ///
    /// Call it for every successful scrape of the source, including one that
    /// only recorded baselines, so the next interval's start is known.
    pub fn derive(&mut self, source_id: i32, points: Vec<MetricPoint>) -> Vec<MetricPoint> {
        let previous = self.last_scrape.get(&source_id).copied();
        if let Some(scraped_at) = points.iter().map(|p| p.time).max() {
            self.last_scrape.insert(source_id, scraped_at);
        }
        let mut out = Vec::with_capacity(points.len());
        // Per label key: the hit point (kept as the template for the gauges'
        // time, labels and source fields) and the read delta.
        let mut pairs: HashMap<String, (Option<MetricPoint>, Option<f64>)> = HashMap::new();
        let mut resets: HashMap<String, f64> = HashMap::new();
        for pt in points {
            if pt.kind == MetricKind::Counter && pt.name == PG_BLKS_HIT {
                let key = labels_key(&pt.labels);
                pairs.entry(key).or_default().0 = Some(pt);
            } else if pt.kind == MetricKind::Counter && pt.name == PG_BLKS_READ {
                pairs.entry(labels_key(&pt.labels)).or_default().1 = Some(pt.value);
            } else if pt.name == PG_STATS_RESET {
                resets.insert(labels_key(&pt.labels), pt.value);
            } else {
                out.push(pt);
            }
        }
        // Record every database's reset time, also on a scrape that only set
        // counter baselines (none of its counters reached here), so a reset
        // right after it is still caught.
        let mut reset_since_last_scrape: HashSet<String> = HashSet::new();
        for (key, reset) in &resets {
            let before = self.stats_reset.insert((source_id, key.clone()), *reset);
            if before.is_some_and(|before| before != *reset) {
                reset_since_last_scrape.insert(key.clone());
            }
        }
        self.stats_reset
            .retain(|(sid, key), _| *sid != source_id || resets.contains_key(key));
        if pairs.is_empty() {
            return out;
        }

        let mut seen: HashSet<String> = HashSet::with_capacity(pairs.len() + 1);
        // This scrape's instance-wide interval: the sum of the databases'.
        let mut instance: Option<(MetricPoint, Interval)> = None;
        for (key, pair) in pairs {
            seen.insert(key.clone());
            let stats_were_reset = reset_since_last_scrape.contains(&key);
            let (Some(template), Some(read)) = pair else {
                continue;
            };
            if !template.labels.contains_key(PG_DATABASE_LABEL) {
                continue;
            }
            if stats_were_reset {
                // The intervals before the reset still count, in this
                // database's window and in the instance-wide one, which this
                // scrape may otherwise never reach: keep both.
                let mut labels = template.labels.clone();
                labels.remove(PG_DATABASE_LABEL);
                seen.insert(labels_key(&labels));
                continue;
            }
            let interval = Interval {
                start: previous.map_or(template.time, |p| p.min(template.time)),
                end: template.time,
                // Deltas are non-negative by the scraper's contract; clamp so
                // a violation can never yield a ratio outside 0..=1.
                hit: template.value.max(0.0),
                read: read.max(0.0),
            };
            match &mut instance {
                Some((_, sum)) => {
                    sum.start = sum.start.min(interval.start);
                    sum.end = sum.end.max(interval.end);
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
        let Some(sum) = self
            .series
            .entry((source_id, key))
            .or_default()
            .push(interval, self.window)
        else {
            return;
        };
        if !sum.full {
            return;
        }
        if sum.accesses > 0.0 {
            out.push(gauge(template, PG_CACHE_HIT_RATIO, sum.hits / sum.accesses));
        }
        out.push(gauge(template, PG_CACHE_BLOCKS_ACCESSED, sum.accesses));
    }

    /// Forget every window of `source_id`, e.g. after an HA failover, when the
    /// new primary's counters belong to a different server.
    pub fn reset_source(&mut self, source_id: i32) {
        self.series.retain(|(sid, _), _| *sid != source_id);
        self.last_scrape.remove(&source_id);
        self.stats_reset.retain(|(sid, _), _| *sid != source_id);
    }

    /// Keep only the windows of `active` sources.
    pub fn retain_sources(&mut self, active: &HashSet<i32>) {
        self.series.retain(|(sid, _), _| active.contains(sid));
        self.last_scrape.retain(|sid, _| active.contains(sid));
        self.stats_reset.retain(|(sid, _), _| active.contains(sid));
    }
}

/// A gauge named `name` with the time, source and labels of `template`.
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

    /// One database's deltas plus its `stats_reset` time.
    fn deltas_since(
        hit: f64,
        read: f64,
        time: DateTime<Utc>,
        db: &str,
        stats_reset: f64,
    ) -> Vec<MetricPoint> {
        let mut points = deltas(hit, read, time, db);
        let mut reset = counter(PG_STATS_RESET, stats_reset, time, Some(db));
        reset.kind = MetricKind::Gauge;
        points.push(reset);
        points
    }

    /// Track `dbs` of source 1 for the 16 minutes before `at(0)`, without
    /// block access, so their windows are whole from `at(0)` on.
    fn warm_up(windows: &mut CacheHitWindows, dbs: &[&str]) {
        for minute in -16..0 {
            let batch = dbs
                .iter()
                .flat_map(|db| deltas(0.0, 0.0, at(minute), db))
                .collect();
            windows.derive(1, batch);
        }
    }

    #[test]
    fn a_series_reports_nothing_until_its_window_is_whole() {
        // As after a restart, a failover or a long outage: the window refills
        // from empty. Its first minutes are not "the last 15 minutes".
        let mut windows = CacheHitWindows::default();
        windows.derive(1, deltas(90.0, 10.0, at(0), "app"));
        for minute in 1..15 {
            let out = windows.derive(1, deltas(90.0, 10.0, at(minute), "app"));
            assert!(out.is_empty(), "minute {minute}: {out:?}");
        }
        let out = windows.derive(1, deltas(90.0, 10.0, at(15), "app"));
        for db in [Some("app"), None] {
            assert_eq!(value(&out, PG_CACHE_HIT_RATIO, db), Some(0.9));
            assert_eq!(value(&out, PG_CACHE_BLOCKS_ACCESSED, db), Some(1_500.0));
        }
    }

    #[test]
    fn a_stats_reset_right_after_a_baseline_only_scrape_is_still_caught() {
        let mut windows = CacheHitWindows::default();
        warm_up(&mut windows, &["app"]);
        // The scrape that set the counter baselines: only the reset time
        // reaches the window.
        let mut reset = counter(PG_STATS_RESET, 0.0, at(0), Some("app"));
        reset.kind = MetricKind::Gauge;
        assert!(windows.derive(1, vec![reset]).is_empty());

        let out = windows.derive(1, deltas_since(9_900.0, 100.0, at(1), "app", 1_000.0));
        assert_eq!(value(&out, PG_CACHE_HIT_RATIO, Some("app")), None);
        assert_eq!(value(&out, PG_CACHE_HIT_RATIO, None), None);

        let out = windows.derive(1, deltas_since(10_000.0, 10_000.0, at(2), "app", 1_000.0));
        assert_eq!(value(&out, PG_CACHE_HIT_RATIO, Some("app")), Some(0.5));
    }

    #[test]
    fn the_interval_spanning_a_stats_reset_is_dropped() {
        let mut windows = CacheHitWindows::default();
        warm_up(&mut windows, &["app"]);
        let out = windows.derive(1, deltas_since(900.0, 100.0, at(0), "app", 0.0));
        assert_eq!(value(&out, PG_CACHE_HIT_RATIO, Some("app")), Some(0.9));
        assert_eq!(value(&out, PG_STATS_RESET, Some("app")), None);

        // `pg_stat_reset()` between the scrapes: the per-counter delta step
        // saw hits go backwards but reads keep growing, so these deltas mix
        // both sides of the reset.
        let out = windows.derive(1, deltas_since(9_900.0, 100.0, at(1), "app", 1_000.0));
        assert_eq!(value(&out, PG_CACHE_HIT_RATIO, Some("app")), None);
        assert_eq!(value(&out, PG_CACHE_HIT_RATIO, None), None);
        assert_eq!(value(&out, PG_CACHE_BLOCKS_ACCESSED, None), None);

        // After it, deltas are clean again and count with the earlier ones.
        let out = windows.derive(1, deltas_since(50.0, 50.0, at(2), "app", 1_000.0));
        let ratio = value(&out, PG_CACHE_HIT_RATIO, Some("app")).unwrap();
        assert!((ratio - 950.0 / 1_100.0).abs() < 1e-12, "{ratio}");
        assert_eq!(value(&out, PG_CACHE_BLOCKS_ACCESSED, None), Some(1_100.0));
    }

    #[test]
    fn ratio_is_weighted_over_the_window_and_counters_are_not_persisted() {
        let mut windows = CacheHitWindows::default();
        warm_up(&mut windows, &["app"]);
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
        warm_up(&mut windows, &["app"]);
        let out = windows.derive(1, deltas(0.0, 0.0, at(0), "app"));
        for db in [Some("app"), None] {
            assert_eq!(value(&out, PG_CACHE_HIT_RATIO, db), None);
            assert_eq!(value(&out, PG_CACHE_BLOCKS_ACCESSED, db), Some(0.0));
        }
    }

    #[test]
    fn the_instance_series_is_the_traffic_weighted_sum_of_the_databases() {
        let mut windows = CacheHitWindows::default();
        warm_up(&mut windows, &["app", "bulk"]);
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
        warm_up(&mut windows, &["app"]);
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
    fn a_delta_spanning_more_than_the_window_is_not_reported_as_the_window() {
        let mut windows = CacheHitWindows::default();
        windows.derive(1, deltas(990.0, 10.0, at(0), "app"));
        windows.derive(1, deltas(990.0, 10.0, at(1), "app"));
        // Scrapes fail for 40 minutes; the first good one carries the whole
        // outage's accesses, which mostly missed the cache.
        let out = windows.derive(1, deltas(1_000.0, 39_000.0, at(41), "app"));
        assert!(out.is_empty(), "nothing is claimed for the window: {out:?}");

        // The window refills from minute 41 and reports again once whole.
        let out = windows.derive(1, deltas(990.0, 10.0, at(42), "app"));
        assert!(out.is_empty(), "the window is refilling: {out:?}");
        for minute in 43..56 {
            windows.derive(1, deltas(0.0, 0.0, at(minute), "app"));
        }
        let out = windows.derive(1, deltas(0.0, 0.0, at(56), "app"));
        assert_eq!(value(&out, PG_CACHE_HIT_RATIO, None), Some(0.99));
        assert_eq!(value(&out, PG_CACHE_BLOCKS_ACCESSED, None), Some(1000.0));
    }

    #[test]
    fn a_shorter_gap_counts_until_its_start_leaves_the_window() {
        let mut windows = CacheHitWindows::default();
        warm_up(&mut windows, &["app"]);
        windows.derive(1, deltas(0.0, 0.0, at(0), "app"));
        // Ten minutes without a scrape: the delta spans minutes 0 to 10.
        let out = windows.derive(1, deltas(0.0, 1_000.0, at(10), "app"));
        assert_eq!(value(&out, PG_CACHE_HIT_RATIO, None), Some(0.0));
        let out = windows.derive(1, deltas(1_000.0, 0.0, at(15), "app"));
        assert_eq!(value(&out, PG_CACHE_HIT_RATIO, None), Some(0.5));
        // At minute 16 the window starts after minute 0, so the long interval
        // is no longer entirely inside it, and what is left covers minutes 10
        // to 16: not the window.
        let out = windows.derive(1, deltas(1_000.0, 0.0, at(16), "app"));
        assert!(
            out.is_empty(),
            "minutes 10 to 16 are not the window: {out:?}"
        );
    }

    #[test]
    fn a_window_left_with_only_the_minutes_after_a_gap_is_not_whole() {
        // A database doing 1,000 block accesses a minute, then unreachable
        // for 13 minutes: the first scrape after it carries the whole gap.
        let mut windows = CacheHitWindows::default();
        warm_up(&mut windows, &["app"]);
        windows.derive(1, deltas(900.0, 100.0, at(0), "app"));
        windows.derive(1, deltas(11_700.0, 1_300.0, at(13), "app"));
        windows.derive(1, deltas(900.0, 100.0, at(14), "app"));
        // The gap's interval still lies inside the window: whole.
        let out = windows.derive(1, deltas(900.0, 100.0, at(15), "app"));
        assert_eq!(value(&out, PG_CACHE_BLOCKS_ACCESSED, None), Some(15_000.0));

        // Once it leaves, the window holds minutes 13 to 16, then 13 to 26:
        // a fraction of the volume, which the alarm would read as idle.
        for minute in 16..27 {
            let out = windows.derive(1, deltas(900.0, 100.0, at(minute), "app"));
            assert!(out.is_empty(), "minute {minute}: {out:?}");
        }
        // From minute 27 the window starts one scrape before minute 13.
        let out = windows.derive(1, deltas(900.0, 100.0, at(27), "app"));
        for db in [Some("app"), None] {
            assert_eq!(value(&out, PG_CACHE_HIT_RATIO, db), Some(0.9));
            assert_eq!(value(&out, PG_CACHE_BLOCKS_ACCESSED, db), Some(14_000.0));
        }
    }

    /// Scrape every `interval_ms` plus a cycle that varies by up to the 5 s
    /// collector timeout, without a failed scrape, and assert every scrape
    /// from the window's first whole one on serves the ratio.
    fn assert_jittered_scrapes_keep_the_window_whole(interval_ms: i64, scrapes: usize) {
        let mut windows = CacheHitWindows::default();
        let start = at(0);
        let mut elapsed_ms: i64 = 0;
        let mut seed: u64 = 0x5eed;
        windows.derive(1, deltas(90.0, 10.0, start, "app"));
        for _ in 0..scrapes {
            // A fixed linear congruential sequence: reproducible jitter.
            seed = seed.wrapping_mul(6_364_136_223_846_793_005).wrapping_add(1);
            elapsed_ms += interval_ms + ((seed >> 33) % 5_000) as i64;
            let time = start + Duration::milliseconds(elapsed_ms);
            let out = windows.derive(1, deltas(90.0, 10.0, time, "app"));
            if elapsed_ms >= 900_000 {
                assert_eq!(
                    value(&out, PG_CACHE_HIT_RATIO, None),
                    Some(0.9),
                    "{interval_ms} ms interval, {elapsed_ms} ms: {out:?}"
                );
            }
        }
    }

    #[test]
    fn uneven_scrape_intervals_without_a_gap_keep_the_window_whole() {
        assert_jittered_scrapes_keep_the_window_whole(30_000, 1_000);
    }

    #[test]
    fn uneven_long_scrape_intervals_without_a_gap_keep_the_window_whole() {
        // Above a tenth of the window, the interval left at the window's
        // start is all the slack there is: jitter alone must not exceed it.
        for interval_ms in [120_000, 300_000] {
            assert_jittered_scrapes_keep_the_window_whole(interval_ms, 2_000);
        }
    }

    #[test]
    fn a_missed_long_scrape_still_leaves_the_window_partial() {
        // At a 5-minute interval one failed scrape is a 10-minute delta; once
        // it leaves, the rest covers the last 5 to 10 minutes only.
        let mut windows = CacheHitWindows::default();
        for minute in [0, 5, 10, 15, 20] {
            windows.derive(1, deltas(90.0, 10.0, at(minute), "app"));
        }
        windows.derive(1, deltas(180.0, 20.0, at(30), "app"));
        let out = windows.derive(1, deltas(90.0, 10.0, at(35), "app"));
        assert_eq!(value(&out, PG_CACHE_BLOCKS_ACCESSED, None), Some(300.0));
        let out = windows.derive(1, deltas(90.0, 10.0, at(40), "app"));
        assert!(
            out.is_empty(),
            "minutes 30 to 40 are not the window: {out:?}"
        );
        let out = windows.derive(1, deltas(90.0, 10.0, at(45), "app"));
        assert_eq!(value(&out, PG_CACHE_BLOCKS_ACCESSED, None), Some(300.0));
    }

    #[test]
    fn a_clock_step_backwards_restarts_the_window() {
        let mut windows = CacheHitWindows::default();
        windows.derive(1, deltas(0.0, 100.0, at(10), "app"));
        let out = windows.derive(1, deltas(0.0, 0.0, at(5), "app"));
        assert!(out.is_empty(), "the window refills from minute 5: {out:?}");
        windows.derive(1, deltas(100.0, 0.0, at(6), "app"));
        for minute in 7..20 {
            windows.derive(1, deltas(0.0, 0.0, at(minute), "app"));
        }
        // Only what came after the step: the 100 reads before it are gone.
        let out = windows.derive(1, deltas(0.0, 0.0, at(20), "app"));
        assert_eq!(value(&out, PG_CACHE_HIT_RATIO, None), Some(1.0));
    }
}
