// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Coalesce many small metric writes into few large inserts.
//!
//! Half a dozen producers share one metrics store — the scraper, the node
//! and proxy samplers, container health, the service health monitor, OTLP
//! ingest — and each calls `write_batch` on its own short cadence with a few
//! dozen points. Against ClickHouse every call was its own insert, i.e. its
//! own new part: ~200k inserts a day of ~44 rows each, and the merges and
//! part removals that follow. [`BufferedMetricsStore`] sits in front of the
//! real store: `write_batch` hands the points to a bounded channel and
//! returns, and one background task writes the accumulated points every
//! [`BufferConfig::flush_interval`] or as soon as
//! [`BufferConfig::max_rows`] are pending.
//!
//! Bounds: the channel holds at most [`BufferConfig::channel_capacity`]
//! batches and the flusher at most `max_rows` plus one batch, so memory is
//! fixed regardless of the store's health. When the channel is full (the
//! store is down or slower than the producers) a batch is dropped and the
//! producer gets [`MetricsError::WriteBufferFull`] — never a blocked
//! producer. A flush that does not finish within
//! [`BufferConfig::write_timeout`] is abandoned and its points dropped, so a
//! store that hangs (rather than errors) cannot stall the flusher forever and
//! turn every later write into a dropped batch. Every lost point — channel
//! overflow, failed flush or timed-out flush — is counted in
//! [`BufferedMetricsStore::dropped_points`]. Reads go straight to the inner store, so a point becomes
//! visible up to one flush interval after it is written.

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use tokio::sync::mpsc;
use tracing::{debug, warn};

use super::{
    LabelledMetric, LatestByLabelQuery, LatestQuery, MetricPoint, MetricsStore, RangeQuery,
    SourceKind,
};
use crate::error::MetricsError;

/// Flush policy for a [`BufferedMetricsStore`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BufferConfig {
    /// Longest a point waits before it is written.
    pub flush_interval: Duration,
    /// Pending points that trigger an early flush.
    pub max_rows: usize,
    /// `write_batch` calls that may be queued before batches are dropped.
    pub channel_capacity: usize,
    /// Longest one flush may take before its points are dropped.
    pub write_timeout: Duration,
}

impl Default for BufferConfig {
    fn default() -> Self {
        Self {
            flush_interval: Duration::from_secs(5),
            max_rows: 10_000,
            channel_capacity: 1_024,
            write_timeout: Duration::from_secs(30),
        }
    }
}

/// A [`MetricsStore`] that batches writes to `inner` in the background.
pub struct BufferedMetricsStore {
    inner: Arc<dyn MetricsStore>,
    sender: mpsc::Sender<Vec<MetricPoint>>,
    capacity: usize,
    dropped_points: Arc<AtomicU64>,
}

impl BufferedMetricsStore {
    /// Wrap `inner` and start the flusher on the current tokio runtime. The
    /// flusher writes whatever is pending and exits once every clone of the
    /// returned store has been dropped.
    pub fn spawn(inner: Arc<dyn MetricsStore>, config: BufferConfig) -> Self {
        let capacity = config.channel_capacity.max(1);
        let (sender, receiver) = mpsc::channel(capacity);
        let dropped_points = Arc::new(AtomicU64::new(0));
        tokio::spawn(run_flusher(
            inner.clone(),
            receiver,
            config,
            dropped_points.clone(),
        ));
        Self {
            inner,
            sender,
            capacity,
            dropped_points,
        }
    }

    /// `inner` behind a write buffer when called inside a tokio runtime (the
    /// flusher needs one), otherwise `inner` unchanged — writes then go
    /// straight to the store as before, which is correct, just chattier.
    pub fn wrap(inner: Arc<dyn MetricsStore>, config: BufferConfig) -> Arc<dyn MetricsStore> {
        if tokio::runtime::Handle::try_current().is_err() {
            warn!("no tokio runtime; metrics writes will not be buffered");
            return inner;
        }
        Arc::new(Self::spawn(inner, config))
    }

    /// Points lost since start: the buffer was full, or a flush failed or
    /// timed out.
    pub fn dropped_points(&self) -> u64 {
        self.dropped_points.load(Ordering::Relaxed)
    }
}

async fn run_flusher(
    inner: Arc<dyn MetricsStore>,
    mut receiver: mpsc::Receiver<Vec<MetricPoint>>,
    config: BufferConfig,
    dropped_points: Arc<AtomicU64>,
) {
    let flusher = Flusher {
        inner,
        timeout: config.write_timeout,
        dropped_points,
    };
    let max_rows = config.max_rows.max(1);
    let mut pending: Vec<MetricPoint> = Vec::new();
    let mut ticker = tokio::time::interval(config.flush_interval);
    ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    // The first tick completes immediately; nothing is pending yet.
    ticker.tick().await;

    loop {
        tokio::select! {
            batch = receiver.recv() => match batch {
                Some(points) => {
                    pending.extend(points);
                    if pending.len() >= max_rows {
                        flusher.flush(&mut pending).await;
                    }
                }
                None => {
                    flusher.flush(&mut pending).await;
                    debug!("metrics write buffer closed; flusher exiting");
                    return;
                }
            },
            _ = ticker.tick() => flusher.flush(&mut pending).await,
        }
    }
}

struct Flusher {
    inner: Arc<dyn MetricsStore>,
    timeout: Duration,
    dropped_points: Arc<AtomicU64>,
}

impl Flusher {
    async fn flush(&self, pending: &mut Vec<MetricPoint>) {
        if pending.is_empty() {
            return;
        }
        let points = std::mem::take(pending);
        let count = points.len();
        // Fail open like every producer did before: a store outage loses these
        // points, it never backs up into the producers.
        let failure = match tokio::time::timeout(self.timeout, self.inner.write_batch(points)).await
        {
            Ok(Ok(())) => return,
            Ok(Err(e)) => e.to_string(),
            Err(_) => format!("write did not finish within {:?}", self.timeout),
        };
        let dropped_total = self
            .dropped_points
            .fetch_add(count as u64, Ordering::Relaxed)
            + count as u64;
        warn!(
            points = count,
            dropped_total,
            error = %failure,
            "metrics write buffer: flush failed; points dropped"
        );
    }
}

#[async_trait]
impl MetricsStore for BufferedMetricsStore {
    async fn write_batch(&self, points: Vec<MetricPoint>) -> Result<(), MetricsError> {
        if points.is_empty() {
            return Ok(());
        }
        let count = points.len();
        match self.sender.try_send(points) {
            Ok(()) => Ok(()),
            Err(mpsc::error::TrySendError::Full(_)) | Err(mpsc::error::TrySendError::Closed(_)) => {
                let dropped_total = self
                    .dropped_points
                    .fetch_add(count as u64, Ordering::Relaxed)
                    + count as u64;
                Err(MetricsError::WriteBufferFull {
                    capacity: self.capacity,
                    points: count,
                    dropped_total,
                })
            }
        }
    }

    async fn query_range(
        &self,
        filter: RangeQuery,
    ) -> Result<Vec<(DateTime<Utc>, f64)>, MetricsError> {
        self.inner.query_range(filter).await
    }

    async fn query_latest(
        &self,
        filter: LatestQuery,
    ) -> Result<HashMap<String, f64>, MetricsError> {
        self.inner.query_latest(filter).await
    }

    async fn query_latest_by_label(
        &self,
        filter: LatestByLabelQuery,
    ) -> Result<Vec<LabelledMetric>, MetricsError> {
        self.inner.query_latest_by_label(filter).await
    }

    async fn latest_timestamp(
        &self,
        source_kind: SourceKind,
        source_id: i32,
    ) -> Result<Option<DateTime<Utc>>, MetricsError> {
        self.inner.latest_timestamp(source_kind, source_id).await
    }

    async fn prune(&self, older_than: DateTime<Utc>) -> Result<u64, MetricsError> {
        self.inner.prune(older_than).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::MetricKind;
    use std::sync::Mutex;

    /// Records every `write_batch` call's size; optionally blocks forever so
    /// the channel can be filled.
    #[derive(Default)]
    struct RecordingStore {
        writes: Mutex<Vec<usize>>,
        stall: bool,
        /// Stall only on the first `write_batch` call.
        stall_once: std::sync::atomic::AtomicBool,
    }

    #[async_trait]
    impl MetricsStore for RecordingStore {
        async fn write_batch(&self, points: Vec<MetricPoint>) -> Result<(), MetricsError> {
            if self.stall || self.stall_once.swap(false, Ordering::SeqCst) {
                std::future::pending::<()>().await;
            }
            self.writes.lock().unwrap().push(points.len());
            Ok(())
        }
        async fn query_range(
            &self,
            _filter: RangeQuery,
        ) -> Result<Vec<(DateTime<Utc>, f64)>, MetricsError> {
            Ok(Vec::new())
        }
        async fn query_latest(
            &self,
            _filter: LatestQuery,
        ) -> Result<HashMap<String, f64>, MetricsError> {
            Ok(HashMap::new())
        }
        async fn query_latest_by_label(
            &self,
            _filter: LatestByLabelQuery,
        ) -> Result<Vec<LabelledMetric>, MetricsError> {
            Ok(Vec::new())
        }
        async fn latest_timestamp(
            &self,
            _source_kind: SourceKind,
            _source_id: i32,
        ) -> Result<Option<DateTime<Utc>>, MetricsError> {
            Ok(None)
        }
        async fn prune(&self, _older_than: DateTime<Utc>) -> Result<u64, MetricsError> {
            Ok(0)
        }
    }

    fn points(n: usize) -> Vec<MetricPoint> {
        (0..n)
            .map(|i| MetricPoint {
                time: Utc::now(),
                source_kind: SourceKind::Node,
                source_id: i as i32,
                name: "node.cpu_percent".to_string(),
                value: 1.0,
                kind: MetricKind::Gauge,
                engine: None,
                environment: None,
                node_id: None,
                labels: HashMap::new(),
            })
            .collect()
    }

    #[tokio::test]
    async fn many_small_writes_become_one_insert_per_interval() {
        let inner = Arc::new(RecordingStore::default());
        let config = BufferConfig {
            flush_interval: Duration::from_millis(200),
            ..BufferConfig::default()
        };
        let store = BufferedMetricsStore::spawn(inner.clone(), config);
        for _ in 0..50 {
            store.write_batch(points(3)).await.unwrap();
        }
        assert!(
            inner.writes.lock().unwrap().is_empty(),
            "nothing is written before the interval"
        );
        tokio::time::sleep(Duration::from_millis(400)).await;
        assert_eq!(inner.writes.lock().unwrap().as_slice(), &[150]);
    }

    #[tokio::test]
    async fn reaching_max_rows_flushes_before_the_interval() {
        let inner = Arc::new(RecordingStore::default());
        let config = BufferConfig {
            max_rows: 10,
            ..BufferConfig::default()
        };
        let store = BufferedMetricsStore::spawn(inner.clone(), config);
        store.write_batch(points(6)).await.unwrap();
        store.write_batch(points(6)).await.unwrap();
        // Let the flusher run without advancing to the interval.
        tokio::time::sleep(Duration::from_millis(10)).await;
        assert_eq!(inner.writes.lock().unwrap().as_slice(), &[12]);
    }

    #[tokio::test]
    async fn dropping_the_store_flushes_what_is_pending() {
        let inner = Arc::new(RecordingStore::default());
        let store = BufferedMetricsStore::spawn(inner.clone(), BufferConfig::default());
        store.write_batch(points(4)).await.unwrap();
        drop(store);
        tokio::time::sleep(Duration::from_millis(10)).await;
        assert_eq!(inner.writes.lock().unwrap().as_slice(), &[4]);
    }

    #[tokio::test]
    async fn a_stalled_store_drops_batches_instead_of_blocking_producers() {
        let inner = Arc::new(RecordingStore {
            stall: true,
            ..Default::default()
        });
        let config = BufferConfig {
            max_rows: 1,
            channel_capacity: 2,
            ..BufferConfig::default()
        };
        let store = BufferedMetricsStore::spawn(inner, config);
        // The first batch is taken by the flusher, which then stalls; two
        // more fill the channel; the next one has nowhere to go.
        store.write_batch(points(1)).await.unwrap();
        tokio::time::sleep(Duration::from_millis(10)).await;
        store.write_batch(points(1)).await.unwrap();
        store.write_batch(points(1)).await.unwrap();
        let err = store.write_batch(points(5)).await.unwrap_err();
        assert!(matches!(
            err,
            MetricsError::WriteBufferFull {
                capacity: 2,
                points: 5,
                dropped_total: 5
            }
        ));
        assert_eq!(store.dropped_points(), 5);
    }

    /// A store that hangs instead of failing must not wedge the flusher: the
    /// hung flush is abandoned (and its points counted as dropped) and the
    /// next flush goes through.
    #[tokio::test]
    async fn a_hung_flush_times_out_and_the_flusher_recovers() {
        let inner = Arc::new(RecordingStore {
            stall_once: std::sync::atomic::AtomicBool::new(true),
            ..Default::default()
        });
        let config = BufferConfig {
            max_rows: 1,
            write_timeout: Duration::from_millis(50),
            ..BufferConfig::default()
        };
        let store = BufferedMetricsStore::spawn(inner.clone(), config);
        store.write_batch(points(3)).await.unwrap();
        tokio::time::sleep(Duration::from_millis(150)).await;
        assert_eq!(store.dropped_points(), 3, "the hung flush is counted");
        assert!(inner.writes.lock().unwrap().is_empty());

        store.write_batch(points(2)).await.unwrap();
        tokio::time::sleep(Duration::from_millis(50)).await;
        assert_eq!(inner.writes.lock().unwrap().as_slice(), &[2]);
        assert_eq!(store.dropped_points(), 3);
    }

    #[test]
    fn wrap_without_a_runtime_returns_the_inner_store() {
        let inner: Arc<dyn MetricsStore> = Arc::new(RecordingStore::default());
        let wrapped = BufferedMetricsStore::wrap(inner.clone(), BufferConfig::default());
        assert!(Arc::ptr_eq(&inner, &wrapped));
    }

    #[tokio::test]
    async fn empty_writes_are_a_no_op() {
        let inner = Arc::new(RecordingStore::default());
        let store = BufferedMetricsStore::spawn(inner.clone(), BufferConfig::default());
        store.write_batch(Vec::new()).await.unwrap();
        drop(store);
        tokio::time::sleep(Duration::from_millis(10)).await;
        assert!(inner.writes.lock().unwrap().is_empty());
    }
}
