// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Chunk writer service (ADR-046 §1, §4, §6a, §8, §8a.2): owns the whole seal
//! pipeline for a container's head buffer so the ordering invariant "object
//! written → manifest committed → WAL generation removed" lives in one function and
//! readers never observe a gap.
//!
//! This is the v2 writer. It replaces the v1 `ChunkWriterService` that wrote
//! legacy single-frame `.ndjson.zst` objects; those chunks are still read via
//! `format_version = 1` in the [`crate::store::chunk_store::ChunkStore`], but
//! nothing writes them anymore.
//!
//! ## Seal pipeline ordering
//!
//! 1. Under the buffer mutex: sync/rotate the active WAL into an immutable
//!    generation, then move the current lines into `sealing`. New appends use
//!    a fresh active WAL. Release the mutex before object/manifest IO.
//! 2. Encode with [`ChunkEncoder`].
//! 3. Content-addressed `storage_key` from the encoded generation — a
//!    crash-and-replay re-seal writes the same key (ADR-046 §8a.2).
//! 4. `storage.write_chunk`, retried 3× with backoff (250ms/1s/4s). On
//!    persistent failure: log, count, drop the lines from memory, but leave
//!    the WAL untouched — the lines are still recoverable at next start.
//! 5. Build the [`ChunkMeta`] and `manifests.insert` (idempotent on
//!    `storage_key`), retried the same way. On persistent failure: log and
//!    leave the WAL intact (the object exists; a reconcile sweep re-adopts it
//!    from its footer).
//! 6. Line index (ADR-047 §4): hand `(seq, labels, lines)` to the
//!    [`LineIndexSink`], retried the same way; on success mark the manifest
//!    `indexed_at`. Failure is logged and counted, never propagated — the
//!    reindexer re-derives the rows from the chunk.
//! 7. Write-through: if a cache is configured, seed it with the sealed
//!    chunk's footer so the node that sealed it never needs to re-fetch its
//!    own index.
//! 8. Remove only the committed WAL generation, then clear `sealing` under
//!    the buffer mutex. New active WAL records are untouched.
//!
//! Readers see `sealing ++ lines` via [`HeadSource::snapshot`] until step 8,
//! so a line is visible from the head or from the manifest at every instant
//! — never neither. Because the manifest only becomes visible to readers
//! once step 5 completes and `sealing` is cleared strictly after that, there
//! is a narrow window where a line could be read twice (once from `sealing`,
//! once from the freshly committed manifest) if a query races the exact
//! commit instant. That is accepted for v1 of this engine: the planner's
//! stop rule tolerates it, and a duplicate of identical timestamp+message
//! within one page is a cosmetic, extremely rare edge case.
//!
//! // FOLLOW-UP: de-duplicate in `snapshots()`/`snapshot()` by excluding
//! // `sealing` lines whose `ts` is `<=` the just-committed manifest's
//! // `ended_at`, once a manifest has been committed for this buffer. Left
//! // undone for v1 because it requires threading the manifest's `ended_at`
//! // back into the buffer across the same mutex boundary the seal already
//! // crosses twice; the overlap window it would close is on the order of a
//! // single async task hop.

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use async_trait::async_trait;
use bytes::Bytes;
use chrono::{DateTime, Utc};
use sha2::{Digest, Sha256};
use tokio::sync::Mutex;
use tracing::{error, warn};
use uuid::Uuid;

use temps_core::retry::RetryConfig;

use crate::chunk::cache::{CacheTier, ChunkCache};
use crate::chunk::format::{ChunkEncoder, ChunkIdentity};
use crate::chunk::wal::{WalDir, WalGeneration};
use crate::chunk::{
    level_bit, ChunkLabels, DEFAULT_HEAD_MAX_BYTES, FLUSH_AGE_SECS, MAX_FLUSH_AGE_SECS,
    MIN_FLUSH_BYTES,
};
use crate::error::{LogAggregatorError, RetryClass};
use crate::index::{IndexOutcome, LineIndexSink, NoLineIndex};
use crate::storage::traits::build_storage_key_v2;
use crate::storage::LogStorage;
use crate::store::chunk_store::{HeadSnapshot, HeadSource, HeadSummary};
use crate::store::manifest::ManifestRepo;
use crate::types::{ChunkMeta, LogLevel, LogLine};

type PurgeGuard = tokio::sync::OwnedRwLockWriteGuard<Option<DateTime<Utc>>>;

type ProjectPurgeGate = Arc<tokio::sync::RwLock<Option<DateTime<Utc>>>>;

/// Estimated per-line overhead (framing, columnar arrays) added to `msg.len()`
/// when tracking a head buffer's uncompressed byte estimate.
const LINE_OVERHEAD_BYTES: usize = 64;

/// A frozen (immutable) run of lines is capped at this many lines...
const SEGMENT_MAX_LINES: usize = 1024;

/// ...or this many uncompressed message bytes, whichever comes first. Once a
/// segment is frozen it becomes an `Arc<Vec<LogLine>>` that [`HeadSnapshot`]
/// clones by reference, not by content — this is what keeps a query's read of
/// a head buffer O(#segments) instead of O(#lines).
const SEGMENT_MAX_BYTES: usize = 256 * 1024;

/// Backoff schedule for storage/manifest retries inside [`ChunkWriterService::seal`].
const RETRY_DELAYS: [Duration; 3] = [
    Duration::from_millis(250),
    Duration::from_secs(1),
    Duration::from_secs(4),
];

// ── Manifest dependency (testable) ─────────────────────────────────────────

/// The writer's only dependency on the manifest store — narrow enough to fake
/// in tests without a live Postgres.
#[async_trait]
pub trait ManifestSink: Send + Sync {
    async fn insert(&self, meta: &ChunkMeta) -> Result<i64, LogAggregatorError>;

    /// Record that chunk `seq` is fully present in the line index
    /// (ADR-047 §4). Chunks never marked are what the reindexer picks up.
    async fn mark_indexed(&self, _seq: i64) -> Result<(), LogAggregatorError> {
        Ok(())
    }
}

#[async_trait]
impl ManifestSink for ManifestRepo {
    async fn insert(&self, meta: &ChunkMeta) -> Result<i64, LogAggregatorError> {
        ManifestRepo::insert(self, meta).await
    }

    async fn mark_indexed(&self, seq: i64) -> Result<(), LogAggregatorError> {
        ManifestRepo::mark_indexed(self, seq).await
    }
}

// ── Head buffer ─────────────────────────────────────────────────────────

/// Retry thresholds, exposed as constructor params so tests can shrink them
/// well below the ADR-046 §1 defaults.
#[derive(Debug, Clone, Copy)]
struct Thresholds {
    head_max_bytes: usize,
    flush_age_secs: i64,
    min_flush_bytes: usize,
    max_flush_age_secs: i64,
}

impl Default for Thresholds {
    fn default() -> Self {
        Self {
            head_max_bytes: DEFAULT_HEAD_MAX_BYTES,
            flush_age_secs: FLUSH_AGE_SECS,
            min_flush_bytes: MIN_FLUSH_BYTES,
            max_flush_age_secs: MAX_FLUSH_AGE_SECS,
        }
    }
}

/// Running summary of a run of lines, updated in O(1) per line so a
/// [`HeadSummary`] never has to walk the lines it describes.
#[derive(Debug, Clone, Copy, Default)]
struct BufferStats {
    started_at: Option<DateTime<Utc>>,
    ended_at: Option<DateTime<Utc>>,
    level_mask: u16,
    line_count: u32,
    level_counts: [u32; crate::chunk::LEVEL_COUNT],
}

impl BufferStats {
    fn record(&mut self, line: &LogLine) {
        self.started_at = Some(self.started_at.map_or(line.ts, |t| t.min(line.ts)));
        self.ended_at = Some(self.ended_at.map_or(line.ts, |t| t.max(line.ts)));
        self.level_mask |= level_bit(line.level);
        self.line_count += 1;
        self.level_counts[crate::chunk::level_to_u8(line.level) as usize] += 1;
    }

    fn merge(&self, other: &BufferStats) -> BufferStats {
        BufferStats {
            started_at: match (self.started_at, other.started_at) {
                (Some(a), Some(b)) => Some(a.min(b)),
                (a, b) => a.or(b),
            },
            ended_at: match (self.ended_at, other.ended_at) {
                (Some(a), Some(b)) => Some(a.max(b)),
                (a, b) => a.or(b),
            },
            level_mask: self.level_mask | other.level_mask,
            line_count: self.line_count + other.line_count,
            level_counts: std::array::from_fn(|i| self.level_counts[i] + other.level_counts[i]),
        }
    }
}

/// Per-container write buffer. Guarded by [`ChunkWriterService::buffers`].
///
/// Lines are kept in frozen, immutable segments (`Arc<Vec<LogLine>>`) plus a
/// small mutable `active` tail, so a reader ([`HeadSource::snapshot`]) clones
/// `O(#segments)` `Arc` pointers plus the bounded `active` tail — never the
/// whole buffer's content — and [`HeadSource::summaries`] touches no lines at
/// all (see [`BufferStats`]).
struct HeadBuffer {
    identity: ChunkIdentity,
    /// Frozen segments not yet part of an in-flight seal, oldest first.
    segments: Vec<Arc<Vec<LogLine>>>,
    /// The unfrozen tail; at most [`SEGMENT_MAX_LINES`] lines /
    /// [`SEGMENT_MAX_BYTES`] message bytes.
    active: Vec<LogLine>,
    active_bytes: usize,
    /// Total uncompressed estimate since the last seal — drives
    /// [`Self::should_seal`]. Reset to 0 when a seal starts.
    bytes: usize,
    opened_at: Instant,
    /// `segments` (+ the then-frozen `active`) moved out while a seal is in
    /// flight; visible to readers via [`HeadSource`] until the seal
    /// completes or fails.
    sealing: Option<Vec<Arc<Vec<LogLine>>>>,
    /// Stats of `segments` + `active` (the *current* cycle, i.e. not yet
    /// handed to a seal).
    stats: BufferStats,
    /// Stats of `sealing`, frozen at the moment the seal started. `None`
    /// when nothing is sealing.
    sealing_stats: Option<BufferStats>,
    wal: Option<WalHandle>,
    recovered_generation: Option<WalGeneration>,
    /// Notified whenever `sealing` transitions from `Some` back to `None`
    /// (both on success and on every failure path via [`drop_sealing`]).
    /// Callers that need to wait for an in-flight seal to finish — notably
    /// [`ChunkWriterService::seal_inner`] and
    /// [`ChunkWriterService::remove_container`] — subscribe to this before
    /// releasing the buffer lock and then `.await` the notification, so a
    /// cancellation of an outer `write_line` task can never leave
    /// `sealing = Some(...)` permanently stuck.
    sealing_notify: Arc<tokio::sync::Notify>,
}

/// The writer's own thin wrapper around [`crate::chunk::wal::StreamWal`] so a
/// missing WAL directory (no `wal_dir` configured) and a per-stream WAL file
/// are both `Option<WalHandle>` uniformly.
struct WalHandle {
    stream: crate::chunk::wal::StreamWal,
}

impl HeadBuffer {
    fn new(identity: ChunkIdentity, wal: Option<crate::chunk::wal::StreamWal>) -> Self {
        Self {
            identity,
            segments: Vec::new(),
            active: Vec::new(),
            active_bytes: 0,
            bytes: 0,
            opened_at: Instant::now(),
            sealing: None,
            stats: BufferStats::default(),
            sealing_stats: None,
            wal: wal.map(|stream| WalHandle { stream }),
            recovered_generation: None,
            sealing_notify: Arc::new(tokio::sync::Notify::new()),
        }
    }

    fn seal_completion(&self) -> tokio::sync::futures::OwnedNotified {
        self.sealing_notify.clone().notified_owned()
    }

    fn is_empty(&self) -> bool {
        self.segments.is_empty() && self.active.is_empty()
    }

    /// Append one line, updating the running summary and freezing `active`
    /// into a new segment once it crosses [`SEGMENT_MAX_LINES`] /
    /// [`SEGMENT_MAX_BYTES`].
    fn push_line(&mut self, line: LogLine) {
        self.stats.record(&line);
        let msg_len = line.msg.len();
        self.bytes += msg_len + LINE_OVERHEAD_BYTES;
        self.active_bytes += msg_len;
        self.active.push(line);
        if self.active.len() >= SEGMENT_MAX_LINES || self.active_bytes >= SEGMENT_MAX_BYTES {
            self.freeze_active();
        }
    }

    /// Move `active` into a new frozen segment, if non-empty.
    fn freeze_active(&mut self) {
        if self.active.is_empty() {
            return;
        }
        self.segments
            .push(Arc::new(std::mem::take(&mut self.active)));
        self.active_bytes = 0;
    }

    /// [`crate::chunk::MIN_FLUSH_BYTES`]/[`crate::chunk::FLUSH_AGE_SECS`]/
    /// [`crate::chunk::MAX_FLUSH_AGE_SECS`] flush policy (ADR-046 §1),
    /// against the (possibly test-shrunk) thresholds.
    fn should_seal(&self, t: &Thresholds, head_max_bytes: usize) -> bool {
        if self.is_empty() {
            return false;
        }
        if self.bytes >= head_max_bytes {
            return true;
        }
        let age_secs = self.opened_at.elapsed().as_secs() as i64;
        if age_secs >= t.flush_age_secs && self.bytes >= t.min_flush_bytes {
            return true;
        }
        if age_secs >= t.max_flush_age_secs {
            return true;
        }
        false
    }

    /// Cheap snapshot for [`HeadSource::snapshot`]: `O(#segments)` `Arc`
    /// clones plus one clone of the bounded `active` tail. `None` when the
    /// buffer is entirely empty (nothing sealing, no segments, no active
    /// lines).
    fn build_snapshot(&self) -> Option<HeadSnapshot> {
        let mut segments: Vec<Arc<Vec<LogLine>>> = Vec::with_capacity(
            self.sealing.as_ref().map_or(0, |s| s.len()) + self.segments.len() + 1,
        );
        if let Some(sealing) = &self.sealing {
            segments.extend(sealing.iter().cloned());
        }
        segments.extend(self.segments.iter().cloned());
        if !self.active.is_empty() {
            segments.push(Arc::new(self.active.clone()));
        }
        if segments.is_empty() {
            return None;
        }
        Some(HeadSnapshot {
            identity: self.identity.clone(),
            segments,
        })
    }

    /// `O(1)` label summary from tracked stats — never touches a line.
    /// Combines `sealing_stats` (if a seal is in flight) with the current
    /// cycle's `stats`, since `sealing` lines are still visible to readers
    /// (and therefore must still count toward the prefilter) until the seal
    /// completes.
    fn summary_labels(&self) -> Option<ChunkLabels> {
        let combined = match &self.sealing_stats {
            Some(sealing) => sealing.merge(&self.stats),
            None => self.stats,
        };
        if combined.line_count == 0 {
            return None;
        }
        Some(ChunkLabels {
            project_id: self.identity.project_id,
            external_service_id: self.identity.external_service_id,
            env: self.identity.env.clone(),
            service: self.identity.service.clone(),
            container_id: self.identity.container_id.clone(),
            deploy_id: self.identity.deploy_id,
            node_id: self.identity.node_id,
            node_name: self.identity.node_name.clone(),
            started_at: combined.started_at?,
            ended_at: combined.ended_at?,
            line_count: combined.line_count,
            level_mask: combined.level_mask,
            level_counts: combined.level_counts,
        })
    }
}

fn identity_from_line(line: &LogLine) -> ChunkIdentity {
    ChunkIdentity {
        project_id: line.project_id,
        external_service_id: line.external_service_id,
        env: line.env.clone(),
        service: line.service.clone(),
        container_id: line.container_id.clone(),
        deploy_id: line.deploy_id,
        node_id: line.node_id,
        node_name: line.node_name.clone(),
    }
}

// ── The service ─────────────────────────────────────────────────────────

/// Owns per-container head buffers, the local WAL, and the whole seal
/// pipeline (encode → object write → manifest insert → cache write-through →
/// WAL generation cleanup). See the module docs for the ordering guarantee.
pub struct ChunkWriterService {
    storage: Arc<dyn LogStorage>,
    manifests: Arc<dyn ManifestSink>,
    /// ADR-047 line index, written right after the manifest commit.
    line_index: Arc<dyn LineIndexSink>,
    cache: Option<ChunkCache>,
    wal_dir: Option<WalDir>,
    buffers: Mutex<HashMap<String, HeadBuffer>>,
    project_gates: Mutex<HashMap<i32, ProjectPurgeGate>>,
    thresholds: Thresholds,
    wait_timeout: Duration,
    /// Per-container unsealed buffer cap (Settings → Monitoring → container
    /// logs); a setting, so it can change while the writer runs.
    head_max_bytes: AtomicUsize,
    /// Skip building a bloom for the next seals (ADR-046 §8 shedding). No
    /// policy wires this yet; it exists so a future memory/CPU-pressure
    /// signal has somewhere to land.
    shed_bloom: AtomicBool,
    /// Chunks whose object write or manifest insert failed permanently and
    /// were dropped from memory (WAL-recoverable). Metric-ish; not yet wired
    /// to a real metrics sink.
    dropped_chunks: AtomicU64,
    /// Chunks sealed but not indexed because the line index write failed.
    unindexed_chunks: AtomicU64,
    recovery_ready: tokio::sync::watch::Sender<bool>,
    recovery_started: AtomicBool,
    #[cfg(test)]
    recovery_failures: AtomicU64,
}

impl ChunkWriterService {
    /// Open the writer: recovers any WAL left over from a previous process
    /// (sealing every recovered stream immediately) before returning.
    pub async fn open(
        storage: Arc<dyn LogStorage>,
        manifests: Arc<dyn ManifestSink>,
        wal_dir: Option<std::path::PathBuf>,
        cache: Option<ChunkCache>,
    ) -> Result<Arc<Self>, LogAggregatorError> {
        Self::open_with_index(
            storage,
            manifests,
            wal_dir,
            cache,
            Arc::new(NoLineIndex::new("line index not configured")),
        )
        .await
    }

    /// [`Self::open`] with a line index sink (ADR-047 §4). The plugin passes
    /// the ClickHouse sink when configured; `open` uses [`NoLineIndex`].
    pub async fn open_with_index(
        storage: Arc<dyn LogStorage>,
        manifests: Arc<dyn ManifestSink>,
        wal_dir: Option<std::path::PathBuf>,
        cache: Option<ChunkCache>,
        line_index: Arc<dyn LineIndexSink>,
    ) -> Result<Arc<Self>, LogAggregatorError> {
        Self::open_with_thresholds(
            storage,
            manifests,
            wal_dir,
            cache,
            line_index,
            Thresholds::default(),
        )
        .await
    }

    async fn open_with_thresholds(
        storage: Arc<dyn LogStorage>,
        manifests: Arc<dyn ManifestSink>,
        wal_dir: Option<std::path::PathBuf>,
        cache: Option<ChunkCache>,
        line_index: Arc<dyn LineIndexSink>,
        thresholds: Thresholds,
    ) -> Result<Arc<Self>, LogAggregatorError> {
        let service = Self::open_deferred_with_thresholds(
            storage, manifests, wal_dir, cache, line_index, thresholds,
        )
        .await?;
        service.recover_wal().await?;
        service.recovery_ready.send_replace(true);
        Ok(service)
    }

    /// Construct without replaying WAL. Call `start_background_recovery` from
    /// the long-lived plugin runtime; writes and purge stay gated until replay
    /// succeeds, while readers and console initialization remain available.
    pub async fn open_deferred_with_index(
        storage: Arc<dyn LogStorage>,
        manifests: Arc<dyn ManifestSink>,
        wal_dir: Option<std::path::PathBuf>,
        cache: Option<ChunkCache>,
        line_index: Arc<dyn LineIndexSink>,
    ) -> Result<Arc<Self>, LogAggregatorError> {
        Self::open_deferred_with_thresholds(
            storage,
            manifests,
            wal_dir,
            cache,
            line_index,
            Thresholds::default(),
        )
        .await
    }

    async fn open_deferred_with_thresholds(
        storage: Arc<dyn LogStorage>,
        manifests: Arc<dyn ManifestSink>,
        wal_dir: Option<std::path::PathBuf>,
        cache: Option<ChunkCache>,
        line_index: Arc<dyn LineIndexSink>,
        thresholds: Thresholds,
    ) -> Result<Arc<Self>, LogAggregatorError> {
        let wal_dir = match wal_dir {
            Some(root) => Some(WalDir::open(root).await?),
            None => None,
        };

        let recovery_ready = tokio::sync::watch::channel(wal_dir.is_none()).0;
        let service = Arc::new(Self {
            storage,
            manifests,
            line_index,
            cache,
            wal_dir,
            buffers: Mutex::new(HashMap::new()),
            project_gates: Mutex::new(HashMap::new()),
            head_max_bytes: AtomicUsize::new(thresholds.head_max_bytes),
            thresholds,
            wait_timeout: Duration::from_secs(30),
            shed_bloom: AtomicBool::new(false),
            dropped_chunks: AtomicU64::new(0),
            unindexed_chunks: AtomicU64::new(0),
            recovery_ready,
            recovery_started: AtomicBool::new(false),
            #[cfg(test)]
            recovery_failures: AtomicU64::new(0),
        });

        Ok(service)
    }

    async fn recover_wal(&self) -> Result<(), LogAggregatorError> {
        if let Some(wal_dir) = self.wal_dir.as_ref() {
            let mut recovered = wal_dir
                .recover_batched(
                    self.head_max_bytes(),
                    !self.shed_bloom.load(Ordering::Relaxed),
                )
                .await?;
            let mut batches = 0u64;
            while let Some(stream) = recovered.next().await? {
                self.recover_stream(stream).await?;
                batches += 1;
                if batches.is_multiple_of(100) {
                    tracing::info!(batches, "Background log WAL recovery progressing");
                }
            }
        }
        Ok(())
    }

    /// Schedule once, without making console readiness depend on recovery.
    /// Failed passes retain WAL and retry; no collector or destructive log
    /// maintenance may run until an entire pass succeeds.
    pub fn start_background_recovery(self: &Arc<Self>) {
        if *self.recovery_ready.borrow() || self.recovery_started.swap(true, Ordering::AcqRel) {
            return;
        }
        let writer = self.clone();
        tokio::spawn(async move {
            let started = Instant::now();
            let retry = RetryConfig::default()
                .with_base_delay(Duration::from_secs(30))
                .with_max_delay(Duration::from_secs(600));
            let mut retry_attempt = 0u32;
            tracing::info!("Background log WAL recovery started; console startup continues");
            loop {
                let result = async {
                    writer.recover_wal().await?;
                    if let Some(wal_dir) = &writer.wal_dir {
                        wal_dir.ensure_recovery_complete().await?;
                    }
                    Ok::<(), LogAggregatorError>(())
                }
                .await;
                match result {
                    Ok(()) => {
                        writer.recovery_ready.send_replace(true);
                        tracing::info!(
                            elapsed_seconds = started.elapsed().as_secs(),
                            "Background log WAL recovery completed; log collection resumed"
                        );
                        break;
                    }
                    Err(error) => {
                        #[cfg(test)]
                        writer.recovery_failures.fetch_add(1, Ordering::Relaxed);
                        match error.retry_class() {
                            RetryClass::Transient => {
                                // RetryConfig performs `1 << attempt`; clamp
                                // before calling it so a service left down for
                                // months cannot overflow the shift.
                                let delay = retry.compute_delay(retry_attempt.min(31));
                                retry_attempt = retry_attempt.saturating_add(1);
                                tracing::error!(
                                    %error,
                                    retry_seconds = delay.as_secs(),
                                    "Background log WAL recovery hit a transient failure; WAL retained, log collection paused; retrying"
                                );
                                tokio::time::sleep(delay).await;
                            }
                            RetryClass::RepairRequired => {
                                tracing::error!(
                                    %error,
                                    retry_seconds = 600,
                                    "Background log WAL recovery requires operator repair; WAL retained and log collection remains paused; retrying in case the WAL is repaired in place"
                                );
                                tokio::time::sleep(Duration::from_secs(600)).await;
                            }
                            RetryClass::Permanent => {
                                tracing::error!(
                                    %error,
                                    "Background log WAL recovery cannot continue with the current data or configuration; WAL retained and log collection remains paused; correct the reported error and restart"
                                );
                                break;
                            }
                        }
                    }
                }
            }
        });
    }

    /// Background log workers wait here, independently of console startup.
    pub async fn wait_for_recovery(&self) {
        let mut ready = self.recovery_ready.subscribe();
        // This service owns the sender, so it cannot close while borrowed.
        let _ = ready.wait_for(|ready| *ready).await;
    }

    async fn ensure_recovered(&self, target: String) -> Result<(), LogAggregatorError> {
        tokio::time::timeout(self.wait_timeout, self.wait_for_recovery())
            .await
            .map_err(|_| LogAggregatorError::OperationTimedOut {
                operation: "wait for log WAL recovery",
                target,
            })
    }

    /// Test-only constructor with shrunk thresholds so flush-policy tests
    /// don't need to wait real minutes.
    #[cfg(test)]
    #[allow(clippy::too_many_arguments)]
    async fn open_for_test(
        storage: Arc<dyn LogStorage>,
        manifests: Arc<dyn ManifestSink>,
        wal_dir: Option<std::path::PathBuf>,
        cache: Option<ChunkCache>,
        head_max_bytes: usize,
        flush_age_secs: i64,
        min_flush_bytes: usize,
        max_flush_age_secs: i64,
    ) -> Result<Arc<Self>, LogAggregatorError> {
        Self::open_with_thresholds(
            storage,
            manifests,
            wal_dir,
            cache,
            Arc::new(NoLineIndex::default()),
            Thresholds {
                head_max_bytes,
                flush_age_secs,
                min_flush_bytes,
                max_flush_age_secs,
            },
        )
        .await
    }

    /// Skip bloom construction on every subsequent seal (ADR-046 §8, first
    /// shed step: "skip bloom construction for the chunk"). No caller wires
    /// this to a pressure signal yet.
    pub fn set_shed_bloom(&self, shed: bool) {
        self.shed_bloom.store(shed, Ordering::Relaxed);
    }

    /// Count of chunks dropped from memory after a permanent object-write or
    /// manifest-insert failure (WAL-recoverable). Exposed for tests/metrics.
    pub fn dropped_chunks(&self) -> u64 {
        self.dropped_chunks.load(Ordering::Relaxed)
    }

    /// Count of sealed chunks whose line-index write failed (ADR-047 §4);
    /// they remain `indexed_at IS NULL` for the reindexer.
    pub fn unindexed_chunks(&self) -> u64 {
        self.unindexed_chunks.load(Ordering::Relaxed)
    }

    /// The configured line index sink (for the capabilities endpoint).
    pub fn line_index(&self) -> &Arc<dyn LineIndexSink> {
        &self.line_index
    }

    /// Re-seal a stream recovered from the WAL at startup. Identity is taken
    /// from the first recovered line.
    async fn recover_stream(
        &self,
        stream: crate::chunk::wal::RecoveredStream,
    ) -> Result<(), LogAggregatorError> {
        let Some(first) = stream.lines.first() else {
            return Ok(());
        };
        let identity = identity_from_line(first);
        let wal = match &self.wal_dir {
            Some(wal_dir) => Some(wal_dir.stream(&stream.container_id).await?),
            None => None,
        };
        let mut buffer = HeadBuffer::new(identity, wal);
        buffer.recovered_generation = stream.generation;
        for line in stream.lines {
            buffer.push_line(line);
        }

        {
            let mut buffers = self.buffers.lock().await;
            buffers.insert(stream.container_id.clone(), buffer);
        }
        // Startup must stop on persistence failure. Continuing to a final
        // batch could remove the source generation while an earlier batch is
        // still only present in memory.
        self.seal_inner(&stream.container_id, true).await
    }

    /// Append one line to its container's head buffer, sealing immediately
    /// if the buffer just crossed `head_max_bytes`.
    ///
    /// Takes `self: &Arc<Self>` so threshold-triggered seals can be spawned as
    /// detached [`tokio::spawn`] tasks.  This is the cancellation-safety fix:
    /// if the caller's task is aborted while a threshold seal is in flight, the
    /// detached task continues to completion and clears `buffer.sealing`,
    /// instead of leaving it permanently stuck `Some(...)`.
    pub async fn write_line(self: &Arc<Self>, line: LogLine) -> Result<(), LogAggregatorError> {
        if !*self.recovery_ready.borrow() {
            self.ensure_recovered(format!("container {}", line.container_id))
                .await?;
        }
        let project_gate = {
            let mut gates = self.project_gates.lock().await;
            gates
                .entry(line.project_id)
                .or_insert_with(|| Arc::new(tokio::sync::RwLock::new(None)))
                .clone()
        };
        let ingest_guard = project_gate.read_owned().await;
        if ingest_guard.is_some_and(|purged_before| line.ts < purged_before) {
            return Ok(());
        }
        let container_id = line.container_id.clone();
        let should_seal = {
            let mut buffers = self.buffers.lock().await;
            if !buffers.contains_key(&container_id) {
                let wal = match &self.wal_dir {
                    Some(wal_dir) => Some(wal_dir.stream(&container_id).await?),
                    None => None,
                };
                buffers.insert(
                    container_id.clone(),
                    HeadBuffer::new(identity_from_line(&line), wal),
                );
            }
            // Safe: just inserted above if absent.
            let buffer =
                buffers
                    .get_mut(&container_id)
                    .ok_or_else(|| LogAggregatorError::Validation {
                        message: format!("head buffer for '{container_id}' vanished mid-write"),
                    })?;
            if let Some(wal) = buffer.wal.as_mut() {
                wal.stream.append(&line).await?;
            }
            buffer.push_line(line);
            buffer.bytes >= self.head_max_bytes()
        };

        if should_seal {
            // Transfer the existing read permit: reacquiring behind a queued
            // purge writer deadlocks Tokio's fair RwLock. The detached seal
            // retains this permit even if the ingest caller is cancelled.
            let writer = self.clone();
            let seal_container_id = container_id.clone();
            self.await_task(
                "threshold seal",
                container_id,
                tokio::spawn(async move {
                    let _ingest_guard = ingest_guard;
                    writer.seal_inner(&seal_container_id, false).await
                }),
            )
            .await?;
        }
        Ok(())
    }

    /// Current per-container unsealed buffer cap in bytes.
    pub fn head_max_bytes(&self) -> usize {
        self.head_max_bytes.load(Ordering::Relaxed)
    }

    /// Change the per-container buffer cap at runtime. Buffers already over
    /// a lowered cap seal on the next line or flush tick.
    pub fn set_head_max_bytes(&self, bytes: usize) {
        self.head_max_bytes.store(bytes.max(1), Ordering::Relaxed);
    }

    /// Seal every head buffer whose flush policy (ADR-046 §1) says it's due.
    pub async fn flush_expired(self: &Arc<Self>) {
        if !*self.recovery_ready.borrow() {
            return;
        }
        let head_max_bytes = self.head_max_bytes();
        let due: Vec<String> = {
            let buffers = self.buffers.lock().await;
            buffers
                .iter()
                .filter(|(_, b)| {
                    b.sealing.is_none() && b.should_seal(&self.thresholds, head_max_bytes)
                })
                .map(|(id, _)| id.clone())
                .collect()
        };
        for container_id in due {
            if let Err(e) = self.seal(&container_id).await {
                warn!(container_id = %container_id, error = %e, "flush_expired: seal failed");
            }
        }
    }

    /// Seal every non-empty head buffer — called on graceful shutdown.
    pub async fn flush_all(self: &Arc<Self>) {
        if !*self.recovery_ready.borrow() {
            return;
        }
        let ids: Vec<String> = {
            let buffers = self.buffers.lock().await;
            buffers
                .iter()
                .filter(|(_, b)| !b.is_empty())
                .map(|(id, _)| id.clone())
                .collect()
        };
        for container_id in ids {
            if let Err(e) = self.seal(&container_id).await {
                warn!(container_id = %container_id, error = %e, "flush_all: seal failed");
            }
        }
    }

    /// Seal every visible head before a destructive operation selects chunk
    /// manifests. Unlike the shutdown helper, failures are propagated: a
    /// purge must never report success while matching lines remain searchable
    /// in an unsealed head buffer.
    pub async fn lock_project_for_purge(
        &self,
        project_id: i32,
    ) -> Result<PurgeGuard, LogAggregatorError> {
        self.ensure_recovered(format!("project {project_id}"))
            .await?;
        let gate = {
            let mut gates = self.project_gates.lock().await;
            gates
                .entry(project_id)
                .or_insert_with(|| Arc::new(tokio::sync::RwLock::new(None)))
                .clone()
        };
        tokio::time::timeout(self.wait_timeout, gate.write_owned())
            .await
            .map_err(|_| LogAggregatorError::OperationTimedOut {
                operation: "acquire purge barrier",
                target: format!("project {project_id}"),
            })
    }

    /// Keep the exclusive barrier in the detached task until every pending
    /// manifest write finishes, even when the request times out or is cancelled.
    pub async fn prepare_project_for_purge(
        self: &Arc<Self>,
        project_id: i32,
    ) -> Result<PurgeGuard, LogAggregatorError> {
        let writer = self.clone();
        self.await_task(
            "prepare purge",
            format!("project {project_id}"),
            tokio::spawn(async move {
                let guard = writer.lock_project_for_purge(project_id).await?;
                writer.flush_project_for_purge(project_id).await?;
                Ok(guard)
            }),
        )
        .await
    }

    async fn await_task<T>(
        &self,
        operation: &'static str,
        target: String,
        task: tokio::task::JoinHandle<Result<T, LogAggregatorError>>,
    ) -> Result<T, LogAggregatorError> {
        tokio::time::timeout(self.wait_timeout, task)
            .await
            .map_err(|_| LogAggregatorError::OperationTimedOut {
                operation,
                target: target.clone(),
            })?
            .map_err(|error| LogAggregatorError::Validation {
                message: format!("{operation} task for {target} failed: {error}"),
            })?
    }

    /// Seal every head buffer for `project_id` that holds lines not yet
    /// committed to the manifest — either because they are still in
    /// `segments`/`active` (`!buffer.is_empty()`) **or** because a seal is
    /// already in flight (`buffer.sealing.is_some()`).
    ///
    /// The in-flight case matters: when the background flush ticker kicks off
    /// a seal, `segments` is moved into `sealing` and `is_empty()` returns
    /// `true`, but the manifest row doesn't exist yet — a purge that ran right
    /// then would skip those lines entirely.  Including buffers where
    /// `sealing.is_some()` and then calling `seal_inner` (which waits for the
    /// in-flight seal via `sealing_notify` before doing its own work) ensures
    /// we block until every pre-existing seal has committed, then reseal any
    /// lines that arrived in `active` during that wait.
    ///
    /// Failures are propagated: a purge must never report success while
    /// matching lines remain searchable in an unsealed head buffer.
    async fn flush_project_for_purge(&self, project_id: i32) -> Result<(), LogAggregatorError> {
        let ids: Vec<String> = {
            let buffers = self.buffers.lock().await;
            buffers
                .iter()
                .filter(|(_, buffer)| {
                    buffer.identity.project_id == project_id
                        && (!buffer.is_empty() || buffer.sealing.is_some())
                })
                .map(|(id, _)| id.clone())
                .collect()
        };
        for container_id in ids {
            self.seal_inner(&container_id, true).await?;
        }
        Ok(())
    }

    /// Seal then drop a container's buffer and WAL file (call when a
    /// container stops).
    ///
    /// Takes `self: &Arc<Self>` for symmetry with [`Self::write_line`].  When
    /// called after a streaming task has been `.abort()`ed, `seal_inner` will
    /// wait for the detached threshold-seal task (if any) to finish before
    /// starting a new seal, so no lines are silently dropped.
    pub async fn remove_container(
        self: &Arc<Self>,
        container_id: &str,
    ) -> Result<(), LogAggregatorError> {
        self.ensure_recovered(format!("container {container_id}"))
            .await?;
        let writer = self.clone();
        let id = container_id.to_owned();
        self.await_task(
            "remove container",
            id.clone(),
            tokio::spawn(async move {
                // Await the underlying seal, not its timeout wrapper: removal must
                // never discard the buffer/WAL while a detached seal still uses it.
                let project_id = {
                    let buffers = writer.buffers.lock().await;
                    let Some(buffer) = buffers.get(&id) else {
                        return Ok(());
                    };
                    buffer.identity.project_id
                };
                let _guard = writer.lock_project_for_purge(project_id).await?;
                writer.seal_inner(&id, true).await?;
                writer.buffers.lock().await.remove(&id);
                if let Some(wal_dir) = &writer.wal_dir {
                    wal_dir.remove(&id).await?;
                }
                Ok(())
            }),
        )
        .await
    }

    /// Flush and fsync every open WAL file. The plugin calls this on a 1 s
    /// ticker so at most ~1 s of ingest is unsynced at any time.
    pub async fn sync_wals(&self) {
        if !*self.recovery_ready.borrow() {
            return;
        }
        let mut buffers = self.buffers.lock().await;
        for (container_id, buffer) in buffers.iter_mut() {
            if let Some(wal) = buffer.wal.as_mut() {
                if let Err(e) = wal.stream.sync().await {
                    warn!(container_id = %container_id, error = %e, "wal sync failed");
                }
            }
        }
    }

    /// Clear `sealing`/`sealing_stats` without truncating the WAL — used on
    /// every failure path so the lines remain recoverable from disk (and
    /// stop counting toward the buffer's [`HeadSummary`], since they are
    /// dropped from memory).
    ///
    /// Notifies [`HeadBuffer::sealing_notify`] after clearing so that any
    /// waiter in [`Self::seal_inner`] or [`Self::remove_container`] is
    /// unblocked and can re-evaluate.
    async fn drop_sealing(&self, container_id: &str) {
        let notify = {
            let mut buffers = self.buffers.lock().await;
            if let Some(buffer) = buffers.get_mut(container_id) {
                buffer.sealing = None;
                buffer.sealing_stats = None;
                Some(buffer.sealing_notify.clone())
            } else {
                None
            }
        };
        if let Some(n) = notify {
            n.notify_waiters();
        }
    }

    /// The seal pipeline. See the module docs for the step-by-step ordering
    /// guarantee. Never returns an error for a storage/manifest failure that
    /// was already retried and logged — those are shed (ADR-021), not
    /// propagated, so a single bad object store doesn't take down ingest.
    /// Only encode failures (a bug, not an operational fault) and WAL IO
    /// errors propagate.
    async fn seal(self: &Arc<Self>, container_id: &str) -> Result<(), LogAggregatorError> {
        let writer = self.clone();
        let id = container_id.to_owned();
        self.await_task(
            "seal",
            id.clone(),
            tokio::spawn(async move { writer.seal_guarded(&id).await }),
        )
        .await
    }

    async fn seal_guarded(&self, container_id: &str) -> Result<(), LogAggregatorError> {
        self.ensure_recovered(format!("container {container_id}"))
            .await?;
        let project_id = {
            let buffers = self.buffers.lock().await;
            let Some(buffer) = buffers.get(container_id) else {
                return Ok(());
            };
            buffer.identity.project_id
        };
        let project_gate = {
            let mut gates = self.project_gates.lock().await;
            gates
                .entry(project_id)
                .or_insert_with(|| Arc::new(tokio::sync::RwLock::new(None)))
                .clone()
        };
        let _seal_guard = project_gate.read().await;
        self.seal_inner(container_id, false).await
    }

    async fn seal_inner(
        &self,
        container_id: &str,
        fail_on_persistence_error: bool,
    ) -> Result<(), LogAggregatorError> {
        // Cancellation-safety loop: if a prior `write_line` threshold-seal was
        // spawned as a detached task and is still in flight, we must wait for
        // it to finish before we can take the `sealing` slot ourselves.
        // Waiting here (rather than returning early) is what prevents
        // `remove_container` from silently no-oping while lines are stranded
        // in `sealing` by an aborted caller.
        let (identity, sealing_segments, generation) = loop {
            let maybe_notify = {
                let mut buffers = self.buffers.lock().await;
                let Some(buffer) = buffers.get_mut(container_id) else {
                    return Ok(());
                };
                if buffer.sealing.is_some() {
                    // A seal is already in flight. Subscribe to the
                    // completion notifier while holding the lock. Creating
                    // the future here observes notify_waiters even before its
                    // first poll; cloning the Arc alone does not subscribe.
                    Some(buffer.seal_completion())
                } else {
                    // Freeze any unfrozen tail so every line about to be sealed
                    // is captured in an immutable segment before we release the
                    // lock.
                    buffer.freeze_active();
                    if buffer.segments.is_empty() {
                        return Ok(());
                    }
                    // Persist the immutable generation before publishing any
                    // object. Concurrent appends will use the fresh active WAL.
                    let generation = if buffer.recovered_generation.is_some() {
                        buffer.recovered_generation.take()
                    } else if let Some(wal) = buffer.wal.as_mut() {
                        Some(
                            wal.stream
                                .rotate(!self.shed_bloom.load(Ordering::Relaxed))
                                .await?,
                        )
                    } else {
                        None
                    };
                    let sealing_segments = std::mem::take(&mut buffer.segments);
                    buffer.bytes = 0;
                    buffer.opened_at = Instant::now();
                    buffer.sealing = Some(sealing_segments.clone());
                    buffer.sealing_stats = Some(buffer.stats);
                    buffer.stats = BufferStats::default();
                    break (buffer.identity.clone(), sealing_segments, generation);
                }
            };
            // Lock is released here before awaiting.
            if let Some(notify) = maybe_notify {
                notify.await;
            }
        };

        let with_bloom = generation.as_ref().map_or_else(
            || !self.shed_bloom.load(Ordering::Relaxed),
            |generation| generation.with_bloom,
        );
        let encoded = match encode_segments(identity, &sealing_segments, with_bloom) {
            Ok(e) => e,
            Err(e) => {
                error!(
                    container_id = container_id,
                    error = %e,
                    "chunk encode failed; dropping buffered lines, WAL retained for recovery"
                );
                self.drop_sealing(container_id).await;
                return Err(e);
            }
        };

        let labels = &encoded.footer.labels;
        // Bind object identity to its complete encoded contents, not merely
        // its first timestamp: different generations may start at the same ts.
        let content_hash = match generation
            .as_ref()
            .and_then(WalGeneration::recovery_identity)
        {
            Some(recovery_identity) => {
                let mut digest = Sha256::new();
                digest.update(b"wal-recovery-v1\0");
                digest.update(recovery_identity.as_bytes());
                digest.update(b"\0");
                digest.update(&encoded.bytes);
                hex::encode(digest.finalize())
            }
            None => hex::encode(Sha256::digest(&encoded.bytes)),
        };
        let storage_key = build_storage_key_v2(
            labels.project_id,
            labels.external_service_id,
            &labels.env,
            &labels.service,
            labels.started_at,
            &labels.container_id,
            Some(&content_hash),
        );

        let write_result = retry_log_operation_with_backoff(|| {
            self.storage.write_chunk(&storage_key, &encoded.bytes)
        })
        .await;
        let compressed_size = match write_result {
            Ok(size) => size,
            Err(e) => {
                error!(
                    container_id = container_id,
                    storage_key = %storage_key,
                    error = %e,
                    "chunk object write failed after retries; dropping buffered lines, WAL retained"
                );
                self.dropped_chunks.fetch_add(1, Ordering::Relaxed);
                self.drop_sealing(container_id).await;
                if fail_on_persistence_error {
                    return Err(e);
                }
                return Ok(());
            }
        };

        let meta = build_chunk_meta(&encoded, &storage_key, compressed_size);
        let manifests = &self.manifests;
        let seq = match retry_log_operation_with_backoff(|| manifests.insert(&meta)).await {
            Ok(seq) => seq,
            Err(e) => {
                error!(
                    container_id = container_id,
                    storage_key = %storage_key,
                    error = %e,
                    "manifest insert failed after retries; object is on disk, WAL retained for \
                     the reconcile sweep to re-adopt it"
                );
                self.dropped_chunks.fetch_add(1, Ordering::Relaxed);
                self.drop_sealing(container_id).await;
                if fail_on_persistence_error {
                    return Err(e);
                }
                return Ok(());
            }
        };

        // ADR-047 §4: index the lines now that the chunk is committed. An
        // index failure never blocks sealing — the manifest simply stays
        // unmarked and the reindexer retries it from the chunk later.
        let line_index = &self.line_index;
        match retry_log_operation_with_backoff(|| {
            line_index.index_chunk(seq, labels, &sealing_segments)
        })
        .await
        {
            Ok(IndexOutcome::Indexed) => {
                if let Err(e) = manifests.mark_indexed(seq).await {
                    warn!(seq, error = %e, "could not mark chunk as indexed; reindexer will redo it");
                }
            }
            Ok(IndexOutcome::Skipped) => {}
            Err(e) => {
                warn!(
                    container_id = container_id,
                    seq,
                    error = %e,
                    "line index write failed after retries; chunk sealed, left for the reindexer"
                );
                self.unindexed_chunks.fetch_add(1, Ordering::Relaxed);
            }
        }

        if let Some(cache) = &self.cache {
            let footer_offset = encoded.trailer.footer_offset();
            let footer_end = footer_offset + encoded.trailer.footer_len();
            let tier = if encoded.trailer.bloom_len > 16 * 1024 {
                CacheTier::Bloom
            } else {
                CacheTier::Index
            };
            let footer_bytes =
                Bytes::copy_from_slice(&encoded.bytes[footer_offset as usize..footer_end as usize]);
            cache
                .put(&storage_key, footer_offset, footer_end, footer_bytes, tier)
                .await;
        }

        // Cleanup is independent of the active WAL and runs without the
        // global buffer mutex, so slow filesystem I/O cannot stall other heads.
        let wal_result = match generation {
            Some(generation) => generation.remove().await,
            None => Ok(()),
        };
        let notify = {
            let mut buffers = self.buffers.lock().await;
            if let Some(buffer) = buffers.get_mut(container_id) {
                buffer.sealing = None;
                buffer.sealing_stats = None;
                Some(buffer.sealing_notify.clone())
            } else {
                None
            }
        };
        // Completion is a state transition, including WAL failure. Always
        // wake subscribers before propagating the generation cleanup error.
        if let Some(notify) = notify {
            notify.notify_waiters();
        }
        wal_result
    }
}

#[async_trait]
impl HeadSource for ChunkWriterService {
    /// `O(#containers)`, and never touches a line — every container's
    /// summary comes from tracked [`BufferStats`], not from its content.
    async fn summaries(&self) -> Vec<HeadSummary> {
        let buffers = self.buffers.lock().await;
        buffers
            .values()
            .filter_map(|b| b.summary_labels().map(|labels| HeadSummary { labels }))
            .collect()
    }

    /// `O(#segments)` `Arc` clones plus one clone of the bounded `active`
    /// tail — never the whole buffer's lines.
    async fn snapshot(&self, container_id: &str) -> Option<HeadSnapshot> {
        let buffers = self.buffers.lock().await;
        buffers.get(container_id)?.build_snapshot()
    }
}

// ── Free functions ─────────────────────────────────────────────────────

fn encode_segments(
    identity: ChunkIdentity,
    segments: &[Arc<Vec<LogLine>>],
    with_bloom: bool,
) -> Result<crate::chunk::format::EncodedChunk, LogAggregatorError> {
    let mut encoder = ChunkEncoder::new(identity);
    for segment in segments {
        for line in segment.iter() {
            encoder.push(line)?;
        }
    }
    encoder.finish(with_bloom)
}

fn build_chunk_meta(
    encoded: &crate::chunk::format::EncodedChunk,
    storage_key: &str,
    compressed_size: u64,
) -> ChunkMeta {
    let labels = &encoded.footer.labels;
    let has_errors =
        (labels.level_mask & (level_bit(LogLevel::Warn) | level_bit(LogLevel::Error))) != 0;
    ChunkMeta {
        id: Uuid::new_v4(),
        project_id: labels.project_id,
        external_service_id: labels.external_service_id,
        env: labels.env.clone(),
        service: labels.service.clone(),
        container_id: labels.container_id.clone(),
        deploy_id: labels.deploy_id,
        node_id: labels.node_id,
        node_name: labels.node_name.clone(),
        started_at: labels.started_at,
        ended_at: labels.ended_at,
        storage_key: storage_key.to_string(),
        line_count: labels.line_count as i32,
        compressed_size_bytes: compressed_size as i32,
        has_errors,
        line_offsets: Vec::new(),
        format_version: crate::chunk::FORMAT_VERSION,
        level_mask: labels.level_mask,
        level_counts: labels.level_counts.to_vec(),
        footer_offset: Some(encoded.trailer.footer_offset()),
        footer_len: Some(encoded.trailer.footer_len() as u32),
        bloom_len: encoded.trailer.bloom_len,
    }
}

/// Retry `f` up to `RETRY_DELAYS.len()` additional times (so
/// `1 + RETRY_DELAYS.len()` attempts total) with the ADR-046 backoff
/// schedule, returning the last error if every attempt fails.
pub(crate) async fn retry_with_backoff<T, E, F, Fut>(mut f: F) -> Result<T, E>
where
    F: FnMut() -> Fut,
    Fut: std::future::Future<Output = Result<T, E>>,
{
    let mut attempt = 0usize;
    loop {
        match f().await {
            Ok(v) => return Ok(v),
            Err(e) => {
                if attempt >= RETRY_DELAYS.len() {
                    return Err(e);
                }
                tokio::time::sleep(RETRY_DELAYS[attempt]).await;
                attempt += 1;
            }
        }
    }
}

async fn retry_log_operation_with_backoff<T, F, Fut>(mut f: F) -> Result<T, LogAggregatorError>
where
    F: FnMut() -> Fut,
    Fut: std::future::Future<Output = Result<T, LogAggregatorError>>,
{
    let mut attempt = 0usize;
    loop {
        match f().await {
            Ok(value) => return Ok(value),
            Err(error) => {
                if error.retry_class() != RetryClass::Transient || attempt >= RETRY_DELAYS.len() {
                    return Err(error);
                }
                tokio::time::sleep(RETRY_DELAYS[attempt]).await;
                attempt += 1;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::chunk::format::{decode_block, decode_footer, decode_trailer};
    use crate::storage::FilesystemStorage;
    use crate::types::{LogLevel, LogStream};
    use chrono::Utc;
    use tokio::sync::Mutex as StdAsyncMutex;

    async fn seed_background_wal(root: &std::path::Path) {
        let wal_dir = WalDir::open(root.to_path_buf()).await.unwrap();
        let mut wal = wal_dir.stream("background-container").await.unwrap();
        wal.append(&make_line(
            "background-container",
            LogLevel::Info,
            "before restart",
        ))
        .await
        .unwrap();
        wal.sync().await.unwrap();
    }

    #[tokio::test]
    async fn background_recovery_returns_before_commit_and_protects_writes_and_purge() {
        let tmp = tempfile::tempdir().unwrap();
        let wal_root = tmp.path().join("wal");
        seed_background_wal(&wal_root).await;
        let sink = Arc::new(GatedSink::default());
        let mut writer = tokio::time::timeout(
            Duration::from_secs(1),
            ChunkWriterService::open_deferred_with_index(
                Arc::new(FilesystemStorage::new(tmp.path().join("objects")).unwrap()),
                sink.clone(),
                Some(wal_root.clone()),
                None,
                Arc::new(NoLineIndex::default()),
            ),
        )
        .await
        .expect("console registration must not wait for manifest insertion")
        .unwrap();
        Arc::get_mut(&mut writer).unwrap().wait_timeout = Duration::from_millis(20);
        // Exercise the production plugin initialization, not just the writer:
        // it must return even while the recovery manifest sink is blocked.
        use temps_core::plugin::{ServiceRegistrationContext, TempsPlugin};
        let context = ServiceRegistrationContext::new();
        let db = Arc::new(
            sea_orm::MockDatabase::new(sea_orm::DatabaseBackend::Postgres).into_connection(),
        );
        let docker = Arc::new(temps_core::DockerHandle::disabled(
            "control-plane",
            "test has no daemon",
        ));
        let metadata = Arc::new(crate::services::LogMetadataService::new(db.clone()));
        let storage: Arc<dyn LogStorage> =
            Arc::new(FilesystemStorage::new(tmp.path().join("objects")).unwrap());
        let index: Arc<dyn crate::index::LineIndex> = Arc::new(NoLineIndex::default());
        context.register_service(db.clone());
        context.register_service(docker.clone());
        context.register_service(writer.clone());
        context.register_service(metadata.clone());
        context.register_service(storage);
        context.register_service(index);
        context.register_service(Arc::new(ChunkCache::open(None, 1024).await.unwrap()));
        context.register_service(Arc::new(crate::services::CollectorService::new(
            docker,
            writer.clone(),
            metadata.clone(),
            100,
        )));
        context.register_service(Arc::new(
            crate::services::RetentionService::new(Arc::new(ManifestRepo::new(db)), metadata)
                .with_chunk_writer(writer.clone()),
        ));
        let plugin =
            crate::plugin::LogAggregatorPlugin::new(crate::types::StorageConfig::Filesystem {
                base_path: tmp.path().join("objects"),
            });
        tokio::time::timeout(
            Duration::from_secs(1),
            plugin.initialize_plugin_services(&context.create_plugin_context()),
        )
        .await
        .expect("console plugin initialization must not await WAL replay")
        .unwrap();
        writer.start_background_recovery(); // Must not launch a second replay.
        tokio::time::timeout(Duration::from_secs(2), sink.entered.notified())
            .await
            .unwrap();
        assert!(!*writer.recovery_ready.borrow());
        assert!(sink.rows.all().await.is_empty());
        assert!(matches!(
            writer
                .write_line(make_line(
                    "background-container",
                    LogLevel::Info,
                    "during recovery"
                ))
                .await,
            Err(LogAggregatorError::OperationTimedOut {
                operation: "wait for log WAL recovery",
                ..
            })
        ));
        assert!(matches!(
            writer.prepare_project_for_purge(1).await,
            Err(LogAggregatorError::OperationTimedOut { .. })
        ));
        assert!(matches!(
            writer.remove_container("background-container").await,
            Err(LogAggregatorError::OperationTimedOut { .. })
        ));
        writer.flush_all().await;
        writer.flush_expired().await;
        assert!(has_recovery_wal(&wal_root).await);
        sink.release.notify_one();
        tokio::time::timeout(Duration::from_secs(2), writer.wait_for_recovery())
            .await
            .unwrap();
        assert_eq!(sink.rows.all().await.len(), 1);
        assert_eq!(sink.rows.all().await[0].line_count, 1);
        assert!(!has_recovery_wal(&wal_root).await);
        writer
            .write_line(make_line(
                "background-container",
                LogLevel::Info,
                "after recovery",
            ))
            .await
            .unwrap();
        let flush_writer = writer.clone();
        let flush = tokio::spawn(async move { flush_writer.flush_all().await });
        sink.entered.notified().await;
        sink.release.notify_one();
        flush.await.unwrap();
        // The intentionally short public-operation timeout can return while
        // the cancellation-safe detached seal is still committing.
        tokio::time::timeout(Duration::from_secs(2), async {
            while sink.rows.all().await.len() != 2 {
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("new lines must not be replaced by recovery");
    }

    #[derive(Default)]
    struct RetryRecoverySink {
        fail: AtomicBool,
        attempts: AtomicU64,
        rows: VecSink,
    }

    #[async_trait]
    impl ManifestSink for RetryRecoverySink {
        async fn insert(&self, meta: &ChunkMeta) -> Result<i64, LogAggregatorError> {
            self.attempts.fetch_add(1, Ordering::Relaxed);
            if self.fail.load(Ordering::Relaxed) {
                return Err(LogAggregatorError::Database(
                    sea_orm::DbErr::ConnectionAcquire(
                        sea_orm::error::ConnAcquireErr::ConnectionClosed,
                    ),
                ));
            }
            self.rows.insert(meta).await
        }
    }

    #[derive(Default)]
    struct PermanentRecoverySink {
        attempts: AtomicU64,
    }

    #[async_trait]
    impl ManifestSink for PermanentRecoverySink {
        async fn insert(&self, _meta: &ChunkMeta) -> Result<i64, LogAggregatorError> {
            self.attempts.fetch_add(1, Ordering::Relaxed);
            Err(LogAggregatorError::Validation {
                message: "injected invalid recovered manifest".into(),
            })
        }
    }

    #[tokio::test]
    async fn background_recovery_retries_failed_pass_without_losing_wal() {
        let tmp = tempfile::tempdir().unwrap();
        let wal_root = tmp.path().join("wal");
        seed_background_wal(&wal_root).await;
        let sink = Arc::new(RetryRecoverySink::default());
        sink.fail.store(true, Ordering::Relaxed);
        let writer = ChunkWriterService::open_deferred_with_index(
            Arc::new(FilesystemStorage::new(tmp.path().join("objects")).unwrap()),
            sink.clone(),
            Some(wal_root.clone()),
            None,
            Arc::new(NoLineIndex::default()),
        )
        .await
        .unwrap();
        writer.start_background_recovery();
        tokio::time::timeout(Duration::from_secs(10), async {
            while writer.dropped_chunks() == 0 {
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .expect("failed recovery pass should finish its bounded insert retries");
        assert!(!*writer.recovery_ready.borrow());
        assert!(has_recovery_wal(&wal_root).await);
        sink.fail.store(false, Ordering::Relaxed);
        tokio::time::pause();
        tokio::time::advance(Duration::from_secs(31)).await;
        tokio::time::resume();
        tokio::time::timeout(Duration::from_secs(3), writer.wait_for_recovery())
            .await
            .unwrap();
        assert_eq!(sink.rows.all().await.len(), 1);
        assert!(!has_recovery_wal(&wal_root).await);
    }

    #[tokio::test(start_paused = true)]
    async fn background_recovery_stops_after_one_permanent_failure_and_retains_wal() {
        let tmp = tempfile::tempdir().unwrap();
        let wal_root = tmp.path().join("wal");
        seed_background_wal(&wal_root).await;
        let sink = Arc::new(PermanentRecoverySink::default());
        let writer = ChunkWriterService::open_deferred_with_index(
            Arc::new(FilesystemStorage::new(tmp.path().join("objects")).unwrap()),
            sink.clone(),
            Some(wal_root.clone()),
            None,
            Arc::new(NoLineIndex::default()),
        )
        .await
        .unwrap();

        writer.start_background_recovery();
        tokio::time::timeout(Duration::from_secs(2), async {
            while sink.attempts.load(Ordering::Relaxed) == 0 {
                tokio::time::sleep(Duration::from_millis(1)).await;
            }
        })
        .await
        .expect("permanent recovery failure should be observed");
        tokio::time::timeout(Duration::from_secs(2), async {
            while Arc::strong_count(&writer) != 1 {
                tokio::time::sleep(Duration::from_millis(1)).await;
            }
        })
        .await
        .expect("terminal recovery task should exit and release its writer clone");
        // Cross both the former 30-second general retry and the former
        // 600-second damaged-WAL retry. This catches regressions in either
        // the inner persistence retry or the outer recovery-pass loop.
        tokio::time::advance(Duration::from_secs(601)).await;
        for _ in 0..10 {
            tokio::task::yield_now().await;
        }

        assert_eq!(sink.attempts.load(Ordering::Relaxed), 1);
        assert!(!*writer.recovery_ready.borrow());
        assert!(has_recovery_wal(&wal_root).await);
    }

    #[tokio::test]
    async fn background_recovery_retained_generation_does_not_enable_purge() {
        let tmp = tempfile::tempdir().unwrap();
        let wal_root = tmp.path().join("wal");
        seed_background_wal(&wal_root).await;
        // A truncated header leaves its valid prefix replayable on disk.
        let wal_dir = WalDir::open(wal_root.clone()).await.unwrap();
        let mut entries = tokio::fs::read_dir(&wal_root).await.unwrap();
        let path = entries.next_entry().await.unwrap().unwrap().path();
        use tokio::io::AsyncWriteExt;
        tokio::fs::OpenOptions::new()
            .append(true)
            .open(&path)
            .await
            .unwrap()
            .write_all(&[1, 2])
            .await
            .unwrap();
        let mut writer = ChunkWriterService::open_deferred_with_index(
            Arc::new(FilesystemStorage::new(tmp.path().join("objects")).unwrap()),
            Arc::new(VecSink::default()),
            Some(wal_root.clone()),
            None,
            Arc::new(NoLineIndex::default()),
        )
        .await
        .unwrap();
        Arc::get_mut(&mut writer).unwrap().wait_timeout = Duration::from_millis(20);
        writer.recover_wal().await.unwrap();
        assert!(matches!(
            wal_dir.ensure_recovery_complete().await,
            Err(LogAggregatorError::WalRecoveryIncomplete { .. })
        ));
        writer.start_background_recovery();
        assert!(matches!(
            writer.prepare_project_for_purge(1).await,
            Err(LogAggregatorError::OperationTimedOut { .. })
        ));
        assert!(!*writer.recovery_ready.borrow());
        assert!(has_recovery_wal(&wal_root).await);
    }

    #[tokio::test]
    async fn background_recovery_resumes_after_retained_wal_is_repaired_in_place() {
        let tmp = tempfile::tempdir().unwrap();
        let wal_root = tmp.path().join("wal");
        seed_background_wal(&wal_root).await;
        let mut entries = tokio::fs::read_dir(&wal_root).await.unwrap();
        let path = entries.next_entry().await.unwrap().unwrap().path();
        let valid_len = tokio::fs::metadata(&path).await.unwrap().len();
        use tokio::io::AsyncWriteExt;
        tokio::fs::OpenOptions::new()
            .append(true)
            .open(&path)
            .await
            .unwrap()
            .write_all(&[1, 2])
            .await
            .unwrap();

        let sink = Arc::new(VecSink::default());
        let writer = ChunkWriterService::open_deferred_with_index(
            Arc::new(FilesystemStorage::new(tmp.path().join("objects")).unwrap()),
            sink.clone(),
            Some(wal_root.clone()),
            None,
            Arc::new(NoLineIndex::default()),
        )
        .await
        .unwrap();
        writer.start_background_recovery();

        tokio::time::timeout(Duration::from_secs(2), async {
            while writer.recovery_failures.load(Ordering::Relaxed) == 0 {
                tokio::time::sleep(Duration::from_millis(1)).await;
            }
        })
        .await
        .expect("initial retained-WAL pass should enter the repair polling state");
        // Let the recovery task enter its 600-second sleep before freezing
        // time, so repairing below cannot race the initial completeness check.
        tokio::time::sleep(Duration::from_millis(10)).await;
        assert!(!*writer.recovery_ready.borrow());
        assert!(has_recovery_wal(&wal_root).await);
        assert_eq!(sink.all().await.len(), 1);

        tokio::time::pause();

        let mut retained_entries = tokio::fs::read_dir(&wal_root).await.unwrap();
        let mut retained_path = None;
        while let Some(entry) = retained_entries.next_entry().await.unwrap() {
            if entry.path().extension().and_then(|value| value.to_str()) == Some("recovery-wal") {
                retained_path = Some(entry.path());
                break;
            }
        }
        let retained_path = retained_path.expect("damaged generation should remain retained");
        tokio::fs::OpenOptions::new()
            .write(true)
            .open(&retained_path)
            .await
            .unwrap()
            .set_len(valid_len)
            .await
            .unwrap();
        for _ in 0..10 {
            tokio::task::yield_now().await;
        }
        assert!(
            !*writer.recovery_ready.borrow(),
            "repair-required recovery should remain paused until its polling interval"
        );

        tokio::time::advance(Duration::from_secs(600)).await;
        tokio::time::resume();
        tokio::time::timeout(Duration::from_secs(2), writer.wait_for_recovery())
            .await
            .expect("in-place WAL repair should be detected on the next poll");
        assert_eq!(sink.all().await.len(), 1);
        assert!(!has_recovery_wal(&wal_root).await);
    }

    #[tokio::test]
    async fn background_recovery_without_wal_is_ready_immediately() {
        let tmp = tempfile::tempdir().unwrap();
        let writer = ChunkWriterService::open_deferred_with_index(
            Arc::new(FilesystemStorage::new(tmp.path().join("objects")).unwrap()),
            Arc::new(VecSink::default()),
            None,
            None,
            Arc::new(NoLineIndex::default()),
        )
        .await
        .unwrap();
        writer.start_background_recovery();
        assert!(*writer.recovery_ready.borrow());
        writer
            .write_line(make_line("no-wal", LogLevel::Info, "live"))
            .await
            .unwrap();
    }

    /// In-memory [`ManifestSink`] for tests.
    #[derive(Default)]
    struct VecSink {
        rows: StdAsyncMutex<Vec<ChunkMeta>>,
    }

    impl VecSink {
        async fn all(&self) -> Vec<ChunkMeta> {
            self.rows.lock().await.clone()
        }
    }

    async fn has_recovery_wal(root: &std::path::Path) -> bool {
        let mut entries = tokio::fs::read_dir(root).await.unwrap();
        while let Some(entry) = entries.next_entry().await.unwrap() {
            if entry
                .path()
                .extension()
                .and_then(|extension| extension.to_str())
                == Some("recovery-wal")
            {
                return true;
            }
        }
        false
    }

    #[async_trait]
    impl ManifestSink for VecSink {
        async fn insert(&self, meta: &ChunkMeta) -> Result<i64, LogAggregatorError> {
            let mut rows = self.rows.lock().await;
            if let Some(index) = rows
                .iter()
                .position(|row| row.storage_key == meta.storage_key)
            {
                return Ok(index as i64 + 1);
            }
            rows.push(meta.clone());
            Ok(rows.len() as i64)
        }
    }

    #[derive(Default)]
    struct FailAfterFirstCommitSink {
        rows: VecSink,
        failing: AtomicBool,
    }

    #[async_trait]
    impl ManifestSink for FailAfterFirstCommitSink {
        async fn insert(&self, meta: &ChunkMeta) -> Result<i64, LogAggregatorError> {
            if self.failing.load(Ordering::Relaxed) && !self.rows.all().await.is_empty() {
                return Err(LogAggregatorError::Validation {
                    message: "injected recovery failure after first commit".to_string(),
                });
            }
            self.rows.insert(meta).await
        }
    }

    /// A sink whose `insert` always fails — for the storage/manifest failure
    /// test paths.
    struct FailingSink;

    #[async_trait]
    impl ManifestSink for FailingSink {
        async fn insert(&self, _meta: &ChunkMeta) -> Result<i64, LogAggregatorError> {
            Err(LogAggregatorError::Validation {
                message: "manifest insert always fails in this test".to_string(),
            })
        }
    }

    struct BlockingSink {
        entered: tokio::sync::Notify,
        release: tokio::sync::Notify,
    }

    #[async_trait]
    impl ManifestSink for BlockingSink {
        async fn insert(&self, _meta: &ChunkMeta) -> Result<i64, LogAggregatorError> {
            self.entered.notify_one();
            self.release.notified().await;
            Ok(1)
        }
    }

    /// A sink that pauses at the manifest insert point and records what was
    /// inserted. Used to gate a seal mid-flight: `entered` fires when the
    /// seal reaches the manifest insert; `release` unblocks it; `rows` holds
    /// every chunk that was committed.
    #[derive(Default)]
    struct GatedSink {
        rows: Arc<VecSink>,
        entered: tokio::sync::Notify,
        release: tokio::sync::Notify,
    }

    #[async_trait]
    impl ManifestSink for GatedSink {
        async fn insert(&self, meta: &ChunkMeta) -> Result<i64, LogAggregatorError> {
            self.entered.notify_one();
            self.release.notified().await;
            self.rows.insert(meta).await
        }
    }

    /// A storage backend whose `write_chunk` always fails.
    struct FailingStorage;

    #[async_trait]
    impl LogStorage for FailingStorage {
        async fn write_chunk(&self, _key: &str, _data: &[u8]) -> Result<u64, LogAggregatorError> {
            Err(LogAggregatorError::StorageConfiguration {
                message: "storage always fails in this test".to_string(),
            })
        }
        async fn read_chunk(&self, _key: &str) -> Result<Vec<u8>, LogAggregatorError> {
            Err(LogAggregatorError::StorageConfiguration {
                message: "not implemented".to_string(),
            })
        }
        async fn read_chunk_range(
            &self,
            _key: &str,
            _start: u64,
            _end: Option<u64>,
        ) -> Result<Vec<u8>, LogAggregatorError> {
            Err(LogAggregatorError::StorageConfiguration {
                message: "not implemented".to_string(),
            })
        }
        async fn list_chunks(&self, _prefix: &str) -> Result<Vec<String>, LogAggregatorError> {
            Ok(Vec::new())
        }
        async fn delete_chunk(&self, _key: &str) -> Result<(), LogAggregatorError> {
            Ok(())
        }
        async fn chunk_exists(&self, _key: &str) -> Result<bool, LogAggregatorError> {
            Ok(false)
        }
    }

    fn make_line(container_id: &str, level: LogLevel, msg: &str) -> LogLine {
        LogLine {
            ts: Utc::now(),
            stream: LogStream::Stdout,
            level,
            msg: msg.to_string(),
            fields: None,
            container_id: container_id.to_string(),
            service: "web".to_string(),
            env: "1".to_string(),
            project_id: 1,
            external_service_id: None,
            deploy_id: None,
            node_id: None,
            node_name: None,
        }
    }

    async fn writer_with_defaults() -> (Arc<ChunkWriterService>, Arc<VecSink>, tempfile::TempDir) {
        let tmp = tempfile::tempdir().unwrap();
        let storage: Arc<dyn LogStorage> =
            Arc::new(FilesystemStorage::new(tmp.path().join("objects")).unwrap());
        let sink = Arc::new(VecSink::default());
        let writer = ChunkWriterService::open(
            storage,
            sink.clone() as Arc<dyn ManifestSink>,
            Some(tmp.path().join("wal")),
            None,
        )
        .await
        .unwrap();
        (writer, sink, tmp)
    }

    async fn generation_paths(root: &std::path::Path) -> Vec<std::path::PathBuf> {
        let mut files = tokio::fs::read_dir(root).await.unwrap();
        let mut paths = Vec::new();
        while let Some(file) = files.next_entry().await.unwrap() {
            if file.path().extension().and_then(|s| s.to_str()) == Some("sealed-wal") {
                paths.push(file.path());
            }
        }
        paths
    }

    async fn assert_manifest_decodes(
        storage: &dyn LogStorage,
        meta: &ChunkMeta,
        expected: &[&str],
    ) {
        let object = storage.read_chunk(&meta.storage_key).await.unwrap();
        assert_eq!(object.len(), meta.compressed_size_bytes as usize);
        let trailer = decode_trailer(&object).unwrap();
        assert_eq!(Some(trailer.footer_offset()), meta.footer_offset);
        assert_eq!(Some(trailer.footer_len() as u32), meta.footer_len);
        let start = meta.footer_offset.unwrap() as usize;
        let footer = decode_footer(
            &object[start..start + meta.footer_len.unwrap() as usize],
            &trailer,
        )
        .unwrap();
        let messages: Vec<String> = footer
            .blocks
            .iter()
            .flat_map(|block| {
                decode_block(
                    &object[block.offset as usize..block.offset as usize + block.len as usize],
                    block,
                    &Default::default(),
                )
                .unwrap()
                .into_iter()
                .map(|line| line.message)
            })
            .collect();
        assert_eq!(messages, expected);
    }

    #[tokio::test]
    async fn startup_recovery_keeps_identical_batches_distinct() {
        let tmp = tempfile::tempdir().unwrap();
        let wal_root = tmp.path().join("wal");
        let wal_dir = WalDir::open(wal_root.clone()).await.unwrap();
        let mut wal = wal_dir.stream("identical-batches").await.unwrap();
        let line = make_line(
            "identical-batches",
            LogLevel::Info,
            &format!("same-{}", "x".repeat(2048)),
        );
        for _ in 0..30 {
            wal.append(&line).await.unwrap();
        }
        wal.sync().await.unwrap();
        drop(wal);

        let sink = Arc::new(VecSink::default());
        let _writer = ChunkWriterService::open_for_test(
            Arc::new(FilesystemStorage::new(tmp.path().join("objects")).unwrap()),
            sink.clone(),
            Some(wal_root),
            None,
            8 * 1024,
            300,
            64 * 1024,
            1_800,
        )
        .await
        .unwrap();
        let rows = sink.all().await;
        assert!(rows.len() > 2, "fixture must span at least three batches");
        assert_eq!(
            rows.iter()
                .map(|row| row.line_count as usize)
                .sum::<usize>(),
            30,
            "identical encoded batches need distinct stable recovery identities"
        );
    }

    #[tokio::test]
    async fn partial_startup_recovery_retries_without_loss_or_duplicates() {
        let tmp = tempfile::tempdir().unwrap();
        let wal_root = tmp.path().join("wal");
        let wal_dir = WalDir::open(wal_root.clone()).await.unwrap();
        let mut wal = wal_dir.stream("recovery-retry").await.unwrap();
        let line = make_line(
            "recovery-retry",
            LogLevel::Info,
            &format!("same-{}", "z".repeat(2048)),
        );
        for _ in 0..30 {
            wal.append(&line).await.unwrap();
        }
        wal.sync().await.unwrap();
        drop(wal);

        let sink = Arc::new(FailAfterFirstCommitSink::default());
        sink.failing.store(true, Ordering::Relaxed);
        let first = ChunkWriterService::open_for_test(
            Arc::new(FilesystemStorage::new(tmp.path().join("objects")).unwrap()),
            sink.clone(),
            Some(wal_root.clone()),
            None,
            8 * 1024,
            300,
            64 * 1024,
            1_800,
        )
        .await;
        assert!(first.is_err(), "second recovery batch must fail startup");
        assert_eq!(sink.rows.all().await.len(), 1, "first batch committed");
        assert!(has_recovery_wal(&wal_root).await);

        sink.failing.store(false, Ordering::Relaxed);
        let _recovered = ChunkWriterService::open_for_test(
            Arc::new(FilesystemStorage::new(tmp.path().join("objects")).unwrap()),
            sink.clone(),
            Some(wal_root.clone()),
            None,
            8 * 1024,
            300,
            64 * 1024,
            1_800,
        )
        .await
        .unwrap();
        let rows = sink.rows.all().await;
        assert_eq!(
            rows.iter()
                .map(|row| row.line_count as usize)
                .sum::<usize>(),
            30
        );
        assert!(!has_recovery_wal(&wal_root).await);
    }

    #[tokio::test]
    async fn restart_preserves_committed_chunk_and_replays_only_its_tail() {
        let tmp = tempfile::tempdir().unwrap();
        let storage: Arc<dyn LogStorage> =
            Arc::new(FilesystemStorage::new(tmp.path().join("objects")).unwrap());
        let sink = Arc::new(GatedSink::default());
        let wal_root = tmp.path().join("wal");
        let writer =
            ChunkWriterService::open(storage.clone(), sink.clone(), Some(wal_root.clone()), None)
                .await
                .unwrap();
        let first = make_line("restart-tail", LogLevel::Info, "committed first generation");
        let mut tail = make_line(
            "restart-tail",
            LogLevel::Info,
            "uncommitted second generation with the same timestamp",
        );
        tail.ts = first.ts; // timestamp identity alone cannot distinguish generations
        writer.write_line(first).await.unwrap();
        let sealing_writer = writer.clone();
        let sealing = tokio::spawn(async move { sealing_writer.seal("restart-tail").await });
        sink.entered.notified().await;
        writer.write_line(tail).await.unwrap();
        writer.sync_wals().await;
        sink.release.notify_one();
        sealing.await.unwrap().unwrap();
        let committed = sink.rows.all().await[0].clone();
        let original_bytes = storage.read_chunk(&committed.storage_key).await.unwrap();
        drop(writer); // restart using the same manifests, WAL and object store
        let recovered = ChunkWriterService::open(
            storage.clone(),
            sink.rows.clone(),
            Some(wal_root.clone()),
            None,
        )
        .await
        .unwrap();
        assert_eq!(
            storage.read_chunk(&committed.storage_key).await.unwrap(),
            original_bytes,
            "replay must never replace an already committed object with A+B"
        );
        let rows = sink.rows.all().await;
        assert_eq!(
            rows.len(),
            2,
            "manifest conflict must not hide the replayed tail"
        );
        assert_manifest_decodes(storage.as_ref(), &rows[0], &["committed first generation"]).await;
        assert_manifest_decodes(
            storage.as_ref(),
            &rows[1],
            &["uncommitted second generation with the same timestamp"],
        )
        .await;
        assert!(generation_paths(&wal_root).await.is_empty());
        drop(recovered);
        let _again = ChunkWriterService::open(storage, sink.rows.clone(), Some(wal_root), None)
            .await
            .unwrap();
        assert_eq!(
            sink.rows.all().await.len(),
            2,
            "completed generations must not replay again"
        );
    }

    #[tokio::test]
    async fn restart_after_manifest_commit_replays_each_generation_idempotently() {
        let tmp = tempfile::tempdir().unwrap();
        let storage: Arc<dyn LogStorage> =
            Arc::new(FilesystemStorage::new(tmp.path().join("objects")).unwrap());
        let sink = Arc::new(GatedSink::default());
        let wal_root = tmp.path().join("wal");
        let writer =
            ChunkWriterService::open(storage.clone(), sink.clone(), Some(wal_root.clone()), None)
                .await
                .unwrap();
        writer.set_shed_bloom(true); // replay's default differs; the generation persists this mode
        writer
            .write_line(make_line("commit-crash", LogLevel::Info, "A"))
            .await
            .unwrap();
        let sealing_writer = writer.clone();
        let sealing = tokio::spawn(async move { sealing_writer.seal("commit-crash").await });
        sink.entered.notified().await;
        let generation = generation_paths(&wal_root).await.remove(0);
        let saved = tmp.path().join("saved-generation");
        tokio::fs::rename(&generation, &saved).await.unwrap();
        tokio::fs::create_dir(&generation).await.unwrap(); // fail cleanup after manifest commit
        writer
            .write_line(make_line("commit-crash", LogLevel::Info, "B"))
            .await
            .unwrap();
        writer.sync_wals().await;
        sink.release.notify_one();
        assert!(sealing.await.unwrap().is_err());
        let committed = sink.rows.all().await[0].clone();
        let original_bytes = storage.read_chunk(&committed.storage_key).await.unwrap();
        drop(writer);
        tokio::fs::remove_dir(&generation).await.unwrap();
        tokio::fs::rename(saved, generation).await.unwrap(); // crash image retains A and active B
        let _recovered = ChunkWriterService::open(
            storage.clone(),
            sink.rows.clone(),
            Some(wal_root.clone()),
            None,
        )
        .await
        .unwrap();
        let rows = sink.rows.all().await;
        assert_eq!(
            rows.len(),
            2,
            "exact generation replay must hit ON CONFLICT while B gets its own manifest"
        );
        assert_eq!(
            rows[0].storage_key, committed.storage_key,
            "a committed immutable generation must retain its original object identity"
        );
        assert_eq!(
            storage.read_chunk(&committed.storage_key).await.unwrap(),
            original_bytes
        );
        assert_manifest_decodes(storage.as_ref(), &rows[0], &["A"]).await;
        assert_manifest_decodes(storage.as_ref(), &rows[1], &["B"]).await;
        assert!(generation_paths(&wal_root).await.is_empty());
    }

    #[tokio::test]
    async fn continuous_tail_ingest_cleans_each_committed_wal_generation() {
        let tmp = tempfile::tempdir().unwrap();
        let sink = Arc::new(GatedSink::default());
        let wal_root = tmp.path().join("wal");
        let writer = ChunkWriterService::open(
            Arc::new(FilesystemStorage::new(tmp.path().join("objects")).unwrap()),
            sink.clone(),
            Some(wal_root.clone()),
            None,
        )
        .await
        .unwrap();
        writer
            .write_line(make_line("continuous-tail", LogLevel::Info, "first"))
            .await
            .unwrap();
        for _ in 0..4 {
            let sealing_writer = writer.clone();
            let sealing = tokio::spawn(async move { sealing_writer.seal("continuous-tail").await });
            sink.entered.notified().await;
            assert_eq!(generation_paths(&wal_root).await.len(), 1);
            writer
                .write_line(make_line("continuous-tail", LogLevel::Info, "next"))
                .await
                .unwrap();
            writer.sync_wals().await;
            sink.release.notify_one();
            sealing.await.unwrap().unwrap();
            assert!(generation_paths(&wal_root).await.is_empty());
            let active = WalDir::open(wal_root.clone())
                .await
                .unwrap()
                .recover()
                .await
                .unwrap();
            assert_eq!(active.len(), 1);
            assert_eq!(
                active[0].lines.len(),
                1,
                "committed prefixes must not accumulate on disk"
            );
        }
    }

    struct ProjectGatedSink {
        blocked: GatedSink,
        other: VecSink,
    }

    #[async_trait]
    impl ManifestSink for ProjectGatedSink {
        async fn insert(&self, meta: &ChunkMeta) -> Result<i64, LogAggregatorError> {
            if meta.project_id == 1 {
                self.blocked.insert(meta).await
            } else {
                self.other.insert(meta).await
            }
        }
    }

    #[tokio::test]
    async fn slow_project_seal_does_not_block_other_project_purge_or_removal() {
        let tmp = tempfile::tempdir().unwrap();
        let sink = Arc::new(ProjectGatedSink {
            blocked: GatedSink::default(),
            other: VecSink::default(),
        });
        let writer = ChunkWriterService::open(
            Arc::new(FilesystemStorage::new(tmp.path().join("objects")).unwrap()),
            sink.clone(),
            Some(tmp.path().join("wal")),
            None,
        )
        .await
        .unwrap();
        writer
            .write_line(make_line("slow-project", LogLevel::Info, "A"))
            .await
            .unwrap();
        let sealing_writer = writer.clone();
        let sealing =
            tokio::spawn(async move { sealing_writer.prepare_project_for_purge(1).await });
        sink.blocked.entered.notified().await;
        tokio::time::timeout(Duration::from_secs(1), async {
            for (id, project) in [("purge-other", 2), ("remove-other", 3)] {
                let mut line = make_line(id, LogLevel::Info, "independent");
                line.project_id = project;
                writer.write_line(line).await.unwrap();
            }
            drop(writer.prepare_project_for_purge(2).await.unwrap());
            writer.remove_container("remove-other").await.unwrap();
        })
        .await
        .expect("unrelated operations must finish while project 1 stays paused");
        assert_eq!(sink.other.all().await.len(), 2);
        assert!(!sealing.is_finished());
        sink.blocked.release.notify_one();
        drop(sealing.await.unwrap().unwrap());
    }

    #[tokio::test]
    async fn threshold_seal_does_not_reacquire_a_read_permit_behind_purge() {
        let (writer, sink, _tmp) = writer_with_defaults().await;
        writer.set_head_max_bytes(1);
        let buffers = writer.buffers.lock().await;
        let ingest = writer.write_line(make_line("threshold-purge", LogLevel::Info, "line"));
        tokio::pin!(ingest);
        assert!(futures::poll!(&mut ingest).is_pending()); // owns the read permit
        let purge = writer.lock_project_for_purge(1);
        tokio::pin!(purge);
        assert!(futures::poll!(&mut purge).is_pending()); // writer queued first
        drop(buffers);
        tokio::time::timeout(Duration::from_secs(2), async {
            let (ingest, purge) = tokio::join!(ingest, purge);
            ingest.unwrap();
            drop(purge.unwrap());
        })
        .await
        .expect("threshold seal and purge must both complete");
        assert_eq!(sink.all().await.len(), 1);
    }

    #[tokio::test]
    async fn seal_completion_is_observed_before_waiter_first_poll() {
        let buffer = HeadBuffer::new(
            identity_from_line(&make_line("notify", LogLevel::Info, "line")),
            None,
        );
        let first = buffer.seal_completion();
        let second = buffer.seal_completion();
        buffer.sealing_notify.notify_waiters();
        tokio::time::timeout(Duration::from_millis(100), async {
            tokio::join!(first, second);
        })
        .await
        .expect("all unpolled subscribers must observe completion");
    }

    #[tokio::test]
    async fn wal_generation_cleanup_error_wakes_all_seal_waiters() {
        let tmp = tempfile::tempdir().unwrap();
        let sink = Arc::new(GatedSink::default());
        let writer = ChunkWriterService::open(
            Arc::new(FilesystemStorage::new(tmp.path().join("objects")).unwrap()),
            sink.clone(),
            Some(tmp.path().join("wal")),
            None,
        )
        .await
        .unwrap();
        writer
            .write_line(make_line("wal-error", LogLevel::Info, "line"))
            .await
            .unwrap();
        let seal_writer = writer.clone();
        let seal = tokio::spawn(async move { seal_writer.seal("wal-error").await });
        sink.entered.notified().await;
        let (first, second) = {
            let mut buffers = writer.buffers.lock().await;
            let buffer = buffers.get_mut("wal-error").unwrap();
            (buffer.seal_completion(), buffer.seal_completion())
        };
        let path = generation_paths(&tmp.path().join("wal")).await.remove(0);
        tokio::fs::remove_file(&path).await.unwrap();
        tokio::fs::create_dir(&path).await.unwrap();
        sink.release.notify_one();
        assert!(matches!(
            seal.await.unwrap(),
            Err(LogAggregatorError::Io(_))
        ));
        tokio::time::timeout(Duration::from_millis(100), async {
            tokio::join!(first, second);
        })
        .await
        .expect("WAL errors must wake every waiter");
        assert!(writer
            .buffers
            .lock()
            .await
            .get("wal-error")
            .unwrap()
            .sealing
            .is_none());
    }

    #[tokio::test]
    async fn timed_out_purge_keeps_its_barrier_until_detached_io_finishes() {
        let tmp = tempfile::tempdir().unwrap();
        let sink = Arc::new(GatedSink::default());
        let mut writer = ChunkWriterService::open(
            Arc::new(FilesystemStorage::new(tmp.path().join("objects")).unwrap()),
            sink.clone(),
            None,
            None,
        )
        .await
        .unwrap();
        Arc::get_mut(&mut writer).unwrap().wait_timeout = Duration::from_millis(100);
        writer
            .write_line(make_line("purge-timeout", LogLevel::Info, "line"))
            .await
            .unwrap();
        let purge_writer = writer.clone();
        let purge = tokio::spawn(async move { purge_writer.prepare_project_for_purge(1).await });
        sink.entered.notified().await;
        assert!(matches!(
            purge.await.unwrap(),
            Err(LogAggregatorError::OperationTimedOut { .. })
        ));
        assert!(matches!(
            writer.lock_project_for_purge(1).await,
            Err(LogAggregatorError::OperationTimedOut { .. })
        ));
        let mut other = make_line("other-project", LogLevel::Info, "unrelated");
        other.project_id = 2;
        tokio::time::timeout(Duration::from_millis(100), writer.write_line(other))
            .await
            .unwrap()
            .unwrap();
        drop(
            writer
                .prepare_project_for_purge(3)
                .await
                .expect("unrelated purge must complete while project 1 is sealing"),
        );
        sink.release.notify_one();
        drop(writer.lock_project_for_purge(1).await.unwrap());
        assert_eq!(sink.rows.all().await.len(), 1);
    }

    #[tokio::test]
    async fn removal_timeout_preserves_the_wal_until_seal_completes() {
        let tmp = tempfile::tempdir().unwrap();
        let sink = Arc::new(GatedSink::default());
        let mut writer = ChunkWriterService::open(
            Arc::new(FilesystemStorage::new(tmp.path().join("objects")).unwrap()),
            sink.clone(),
            Some(tmp.path().join("wal")),
            None,
        )
        .await
        .unwrap();
        Arc::get_mut(&mut writer).unwrap().wait_timeout = Duration::from_millis(100);
        writer
            .write_line(make_line("remove-timeout", LogLevel::Info, "line"))
            .await
            .unwrap();
        let remove_writer = writer.clone();
        let remove =
            tokio::spawn(async move { remove_writer.remove_container("remove-timeout").await });
        sink.entered.notified().await;
        assert!(matches!(
            remove.await.unwrap(),
            Err(LogAggregatorError::OperationTimedOut { .. })
        ));
        assert!(tmp.path().join("wal/remove-timeout.wal").exists());
        assert!(writer.snapshot("remove-timeout").await.is_some());
        sink.release.notify_one();
        drop(writer.lock_project_for_purge(1).await.unwrap());
        assert!(!tmp.path().join("wal/remove-timeout.wal").exists());
        assert!(writer.snapshot("remove-timeout").await.is_none());
    }

    #[tokio::test]
    async fn seal_preserves_wal_for_lines_appended_during_io() {
        let tmp = tempfile::tempdir().unwrap();
        let sink = Arc::new(GatedSink::default());
        let wal_root = tmp.path().join("wal");
        let writer = ChunkWriterService::open(
            Arc::new(FilesystemStorage::new(tmp.path().join("objects")).unwrap()),
            sink.clone(),
            Some(wal_root.clone()),
            None,
        )
        .await
        .unwrap();
        writer
            .write_line(make_line("wal-tail", LogLevel::Info, "first"))
            .await
            .unwrap();
        let seal_writer = writer.clone();
        let seal = tokio::spawn(async move { seal_writer.seal("wal-tail").await });
        sink.entered.notified().await;
        writer
            .write_line(make_line("wal-tail", LogLevel::Info, "tail"))
            .await
            .unwrap();
        writer.sync_wals().await;
        sink.release.notify_one();
        seal.await.unwrap().unwrap();
        let recovered = WalDir::open(wal_root)
            .await
            .unwrap()
            .recover()
            .await
            .unwrap();
        assert!(recovered[0].lines.iter().any(|line| line.msg == "tail"));
    }

    #[tokio::test]
    async fn lines_below_threshold_are_visible_via_head_source_without_a_manifest() {
        let (writer, sink, _tmp) = writer_with_defaults().await;
        writer
            .write_line(make_line("cnt1", LogLevel::Info, "hello"))
            .await
            .unwrap();

        let snap = writer.snapshot("cnt1").await.expect("head has lines");
        assert_eq!(snap.len(), 1);
        assert_eq!(snap.get(0).unwrap().msg, "hello");
        assert!(
            sink.all().await.is_empty(),
            "no seal should have happened yet"
        );
    }

    #[tokio::test]
    async fn snapshot_freezes_many_lines_into_multiple_segments() {
        let (writer, sink, _tmp) = writer_with_defaults().await;
        for i in 0..5_000 {
            writer
                .write_line(make_line("cnt-seg", LogLevel::Info, &format!("line {i}")))
                .await
                .unwrap();
        }

        let snap = writer.snapshot("cnt-seg").await.expect("head has lines");
        assert_eq!(snap.len(), 5_000, "no lines lost across segment freezes");
        assert!(
            snap.segments.len() >= 4,
            "5000 lines / {SEGMENT_MAX_LINES} per segment must freeze into several segments, \
             got {} segments",
            snap.segments.len()
        );
        // Global indices must stay contiguous and in order across segments.
        for (expected, (i, line)) in snap.iter().enumerate() {
            assert_eq!(i, expected);
            assert_eq!(line.msg, format!("line {expected}"));
        }

        let summaries = writer.summaries().await;
        let summary = summaries
            .iter()
            .find(|s| s.labels.container_id == "cnt-seg")
            .expect("summary for cnt-seg");
        assert_eq!(summary.labels.line_count, 5_000);
        assert!(sink.all().await.is_empty(), "nothing sealed yet");
    }

    #[tokio::test]
    async fn flush_all_seals_exactly_one_manifest_that_decodes_back_to_the_same_lines() {
        let (writer, sink, _tmp) = writer_with_defaults().await;
        for i in 0..10 {
            writer
                .write_line(make_line("cnt1", LogLevel::Info, &format!("line {i}")))
                .await
                .unwrap();
        }
        writer.flush_all().await;

        let rows = sink.all().await;
        assert_eq!(rows.len(), 1);
        let meta = &rows[0];
        assert_eq!(meta.format_version, 2);
        assert_eq!(meta.line_count, 10);
        assert!(meta.footer_offset.is_some());
        assert!(meta.footer_len.is_some());

        // No lines left in the head after a clean seal.
        assert!(writer.snapshot("cnt1").await.is_none());
    }

    #[tokio::test]
    async fn sealed_object_decodes_to_the_same_lines_in_order() {
        let tmp = tempfile::tempdir().unwrap();
        let storage_root = tmp.path().join("objects");
        let storage: Arc<dyn LogStorage> =
            Arc::new(FilesystemStorage::new(storage_root.clone()).unwrap());
        let sink = Arc::new(VecSink::default());
        let writer = ChunkWriterService::open(
            storage.clone(),
            sink.clone() as Arc<dyn ManifestSink>,
            Some(tmp.path().join("wal")),
            None,
        )
        .await
        .unwrap();

        let mut expected = Vec::new();
        for i in 0..25 {
            let msg = format!("message number {i}");
            writer
                .write_line(make_line("cnt-decode", LogLevel::Info, &msg))
                .await
                .unwrap();
            expected.push(msg);
        }
        writer.flush_all().await;

        let rows = sink.all().await;
        assert_eq!(rows.len(), 1);
        let meta = &rows[0];

        let object = storage.read_chunk(&meta.storage_key).await.unwrap();
        let trailer = decode_trailer(&object).unwrap();
        let footer_bytes = &object[trailer.footer_offset() as usize
            ..(trailer.footer_offset() + trailer.footer_len()) as usize];
        let footer = decode_footer(footer_bytes, &trailer).unwrap();
        assert_eq!(footer.labels.line_count, 25);

        let mut got = Vec::new();
        for block in &footer.blocks {
            let bytes = &object[block.offset as usize..block.offset as usize + block.len as usize];
            let lines = decode_block(bytes, block, &Default::default()).unwrap();
            got.extend(lines.into_iter().map(|l| l.message));
        }
        assert_eq!(got, expected);
    }

    #[tokio::test]
    async fn size_threshold_triggers_an_immediate_seal() {
        let tmp = tempfile::tempdir().unwrap();
        let storage: Arc<dyn LogStorage> =
            Arc::new(FilesystemStorage::new(tmp.path().join("objects")).unwrap());
        let sink = Arc::new(VecSink::default());
        let writer = ChunkWriterService::open_for_test(
            storage,
            sink.clone() as Arc<dyn ManifestSink>,
            Some(tmp.path().join("wal")),
            None,
            256, // tiny head_max_bytes
            FLUSH_AGE_SECS,
            MIN_FLUSH_BYTES,
            MAX_FLUSH_AGE_SECS,
        )
        .await
        .unwrap();

        // Each line is well under 256 bytes; enough of them cross the cap.
        for i in 0..20 {
            writer
                .write_line(make_line("cnt-size", LogLevel::Info, &format!("line {i}")))
                .await
                .unwrap();
        }

        assert!(
            !sink.all().await.is_empty(),
            "size threshold should have sealed at least one chunk without an explicit flush"
        );
    }

    #[tokio::test]
    async fn small_idle_buffer_waits_for_max_flush_age_not_flush_age() {
        let tmp = tempfile::tempdir().unwrap();
        let storage: Arc<dyn LogStorage> =
            Arc::new(FilesystemStorage::new(tmp.path().join("objects")).unwrap());
        let sink = Arc::new(VecSink::default());
        // flush_age is effectively "immediately" but min_flush_bytes is huge,
        // so only max_flush_age can trigger a seal for a tiny buffer.
        let writer = ChunkWriterService::open_for_test(
            storage,
            sink.clone() as Arc<dyn ManifestSink>,
            Some(tmp.path().join("wal")),
            None,
            DEFAULT_HEAD_MAX_BYTES,
            0,         // flush_age_secs: elapsed immediately
            1_000_000, // min_flush_bytes: our tiny buffer never reaches this
            1,         // max_flush_age_secs: 1s
        )
        .await
        .unwrap();

        writer
            .write_line(make_line("cnt-idle", LogLevel::Info, "tiny"))
            .await
            .unwrap();

        writer.flush_expired().await;
        assert!(
            sink.all().await.is_empty(),
            "small buffer must not flush at FLUSH_AGE_SECS alone"
        );

        tokio::time::sleep(Duration::from_millis(1100)).await;
        writer.flush_expired().await;
        assert_eq!(
            sink.all().await.len(),
            1,
            "small buffer must flush once MAX_FLUSH_AGE_SECS elapses"
        );
    }

    #[tokio::test]
    async fn wal_recovery_seals_lines_written_before_a_crash() {
        let tmp = tempfile::tempdir().unwrap();
        let storage_root = tmp.path().join("objects");
        let wal_root = tmp.path().join("wal");
        let storage: Arc<dyn LogStorage> =
            Arc::new(FilesystemStorage::new(storage_root.clone()).unwrap());
        let sink = Arc::new(VecSink::default());

        {
            let writer = ChunkWriterService::open(
                storage.clone(),
                sink.clone() as Arc<dyn ManifestSink>,
                Some(wal_root.clone()),
                None,
            )
            .await
            .unwrap();
            writer
                .write_line(make_line("cnt-crash", LogLevel::Error, "before crash"))
                .await
                .unwrap();
            // In production the plugin's 1s ticker calls this; a crash after
            // that tick (but before the next flush) is exactly the window
            // the WAL protects. Simulate the tick, then drop without an
            // explicit chunk flush to simulate the crash itself.
            writer.sync_wals().await;
        }

        assert!(
            sink.all().await.is_empty(),
            "nothing should be sealed before recovery runs"
        );

        let sink2 = Arc::new(VecSink::default());
        let _writer2 = ChunkWriterService::open(
            storage,
            sink2.clone() as Arc<dyn ManifestSink>,
            Some(wal_root),
            None,
        )
        .await
        .unwrap();

        let rows = sink2.all().await;
        assert_eq!(
            rows.len(),
            1,
            "recovery must seal the lost lines into one manifest"
        );
        assert_eq!(rows[0].line_count, 1);
        assert_eq!(rows[0].container_id, "cnt-crash");
    }

    #[tokio::test]
    async fn storage_failure_drops_lines_but_keeps_the_wal_and_writes_no_manifest() {
        let tmp = tempfile::tempdir().unwrap();
        let storage: Arc<dyn LogStorage> = Arc::new(FailingStorage);
        let sink = Arc::new(VecSink::default());
        let writer = ChunkWriterService::open(
            storage,
            sink.clone() as Arc<dyn ManifestSink>,
            Some(tmp.path().join("wal")),
            None,
        )
        .await
        .unwrap();

        writer
            .write_line(make_line("cnt-fail", LogLevel::Error, "will not persist"))
            .await
            .unwrap();
        // Simulate the plugin's 1s WAL-sync ticker, which in production would
        // have fired well before the retry schedule below completes.
        writer.sync_wals().await;
        writer.flush_all().await;

        assert!(
            sink.all().await.is_empty(),
            "no manifest on storage failure"
        );
        assert!(
            writer.snapshot("cnt-fail").await.is_none(),
            "lines must be dropped from memory after a permanent storage failure"
        );
        assert_eq!(writer.dropped_chunks(), 1);

        // The WAL file must still hold the record: reopening a writer over
        // the same WAL dir recovers it (proves its generation was retained).
        let storage2: Arc<dyn LogStorage> =
            Arc::new(FilesystemStorage::new(tmp.path().join("objects")).unwrap());
        let sink2 = Arc::new(VecSink::default());
        let _writer2 = ChunkWriterService::open(
            storage2,
            sink2.clone() as Arc<dyn ManifestSink>,
            Some(tmp.path().join("wal")),
            None,
        )
        .await
        .unwrap();
        assert_eq!(
            sink2.all().await.len(),
            1,
            "WAL must still hold the line so a working writer can recover it"
        );
    }

    #[tokio::test]
    async fn purge_flush_fails_closed_when_storage_cannot_commit_the_head() {
        let storage: Arc<dyn LogStorage> = Arc::new(FailingStorage);
        let sink = Arc::new(VecSink::default());
        let writer =
            ChunkWriterService::open(storage, sink.clone() as Arc<dyn ManifestSink>, None, None)
                .await
                .unwrap();
        writer
            .write_line(make_line("cnt-purge-fail", LogLevel::Error, "sensitive"))
            .await
            .unwrap();

        let error = writer
            .flush_project_for_purge(1)
            .await
            .expect_err("purge must fail when its head cannot become a manifest");
        assert!(matches!(
            error,
            LogAggregatorError::StorageConfiguration { .. }
        ));
        assert!(sink.all().await.is_empty());
    }

    #[tokio::test]
    async fn project_purge_gate_blocks_concurrent_ingest_until_the_boundary_closes() {
        let (writer, _sink, _tmp) = writer_with_defaults().await;
        let mut purge_guard = writer.lock_project_for_purge(1).await.unwrap();
        let ingest_writer = writer.clone();
        let ingest = tokio::spawn(async move {
            ingest_writer
                .write_line(make_line(
                    "cnt-race",
                    LogLevel::Info,
                    "arrived during purge",
                ))
                .await
        });

        tokio::time::sleep(std::time::Duration::from_millis(25)).await;
        assert!(
            !ingest.is_finished(),
            "same-project ingest must wait until purge releases its write barrier"
        );
        *purge_guard = Some(Utc::now() + chrono::Duration::seconds(1));
        drop(purge_guard);
        ingest.await.expect("ingest task").expect("ingest succeeds");
        assert!(
            writer.snapshot("cnt-race").await.is_none(),
            "a delayed line older than the committed purge boundary must not reappear"
        );
    }

    #[tokio::test]
    async fn background_seal_cannot_cross_an_active_project_purge_boundary() {
        let (writer, sink, _tmp) = writer_with_defaults().await;
        writer
            .write_line(make_line("cnt-background", LogLevel::Info, "before purge"))
            .await
            .unwrap();
        let purge_guard = writer.lock_project_for_purge(1).await.unwrap();
        let flush_writer = writer.clone();
        let flush = tokio::spawn(async move {
            flush_writer.flush_all().await;
        });

        tokio::time::sleep(std::time::Duration::from_millis(25)).await;
        assert!(
            !flush.is_finished(),
            "timer/shutdown sealing must participate in the project purge barrier"
        );
        drop(purge_guard);
        flush.await.expect("background flush task");
        assert_eq!(sink.all().await.len(), 1);
    }

    #[tokio::test]
    async fn purge_waits_for_an_inflight_background_manifest_commit() {
        let tmp = tempfile::tempdir().unwrap();
        let storage: Arc<dyn LogStorage> =
            Arc::new(FilesystemStorage::new(tmp.path().join("objects")).unwrap());
        let sink = Arc::new(BlockingSink {
            entered: tokio::sync::Notify::new(),
            release: tokio::sync::Notify::new(),
        });
        let writer =
            ChunkWriterService::open(storage, sink.clone() as Arc<dyn ManifestSink>, None, None)
                .await
                .unwrap();
        writer
            .write_line(make_line("cnt-inflight", LogLevel::Info, "before purge"))
            .await
            .unwrap();

        let entered = sink.entered.notified();
        let flush_writer = writer.clone();
        let flush = tokio::spawn(async move { flush_writer.flush_all().await });
        entered.await;
        let purge_writer = writer.clone();
        let purge = tokio::spawn(async move { purge_writer.lock_project_for_purge(1).await });
        tokio::time::sleep(std::time::Duration::from_millis(25)).await;
        assert!(
            !purge.is_finished(),
            "purge must wait until an in-flight manifest commit finishes"
        );

        sink.release.notify_waiters();
        flush.await.expect("background flush task");
        let guard = purge.await.expect("purge lock task");
        drop(guard);
    }

    /// Regression: when a background seal is in flight (`sealing.is_some()`),
    /// `is_empty()` returns `true` because `segments` has been moved into
    /// `sealing`. Before this fix, `flush_project_for_purge` only checked
    /// `!buffer.is_empty()` and would skip the container entirely, letting the
    /// purge proceed before the in-flight seal committed its manifest row.
    ///
    /// The fix: also include `sealing.is_some()` in the filter so `seal_inner`
    /// is called for the container; it then waits (via `sealing_notify`) for
    /// the existing seal to finish before doing its own work.
    #[tokio::test]
    async fn flush_project_for_purge_waits_for_in_flight_background_seal() {
        tokio::time::timeout(std::time::Duration::from_secs(3), async {
            let tmp = tempfile::tempdir().unwrap();
            let sink = Arc::new(GatedSink::default());
            let writer = ChunkWriterService::open(
                Arc::new(FilesystemStorage::new(tmp.path().join("objects")).unwrap()),
                sink.clone() as Arc<dyn ManifestSink>,
                None,
                None,
            )
            .await
            .unwrap();
            writer
                .write_line(make_line("cnt-bg-seal", LogLevel::Info, "will be sealed"))
                .await
                .unwrap();

            // Start a background seal; wait until it enters the manifest insert
            // so `sealing.is_some()` and `is_empty()` are both true.
            let bg_writer = writer.clone();
            let bg_flush = tokio::spawn(async move { bg_writer.flush_all().await });
            sink.entered.notified().await;

            // Now call flush_project_for_purge — it must wait for the in-flight
            // seal to finish, not skip the container and return immediately.
            let purge_writer = writer.clone();
            let purge_flush =
                tokio::spawn(async move { purge_writer.flush_project_for_purge(1).await });

            // Give the purge flush task a moment to start and hit the wait.
            tokio::time::sleep(std::time::Duration::from_millis(25)).await;
            assert!(
                !purge_flush.is_finished(),
                "flush_project_for_purge must wait for the in-flight seal, not return early"
            );

            // Release the in-flight seal.
            sink.release.notify_one();
            bg_flush.await.expect("background flush");

            // Now flush_project_for_purge must complete.
            purge_flush
                .await
                .expect("purge flush task")
                .expect("flush_project_for_purge ok");

            // The manifest must have a row for the originally-in-flight lines.
            assert_eq!(
                sink.rows.all().await.len(),
                1,
                "the in-flight seal's lines must be committed before flush_project_for_purge returns"
            );
        })
        .await
        .expect("flush_project_for_purge_waits_for_in_flight_background_seal timed out");
    }

    #[tokio::test]
    async fn manifest_failure_drops_lines_but_keeps_the_wal() {
        let tmp = tempfile::tempdir().unwrap();
        let storage: Arc<dyn LogStorage> =
            Arc::new(FilesystemStorage::new(tmp.path().join("objects")).unwrap());
        let sink = Arc::new(FailingSink);
        let writer = ChunkWriterService::open(
            storage,
            sink as Arc<dyn ManifestSink>,
            Some(tmp.path().join("wal")),
            None,
        )
        .await
        .unwrap();

        writer
            .write_line(make_line(
                "cnt-manifest-fail",
                LogLevel::Error,
                "orphan object",
            ))
            .await
            .unwrap();
        // Simulate the plugin's 1s WAL-sync ticker, which in production would
        // have fired well before the retry schedule below completes.
        writer.sync_wals().await;
        writer.flush_all().await;

        assert!(
            writer.snapshot("cnt-manifest-fail").await.is_none(),
            "lines must be dropped from memory after a permanent manifest failure"
        );

        // Reopening over the same WAL dir with a working sink recovers it.
        let storage2: Arc<dyn LogStorage> =
            Arc::new(FilesystemStorage::new(tmp.path().join("objects")).unwrap());
        let sink2 = Arc::new(VecSink::default());
        let _writer2 = ChunkWriterService::open(
            storage2,
            sink2.clone() as Arc<dyn ManifestSink>,
            Some(tmp.path().join("wal")),
            None,
        )
        .await
        .unwrap();
        assert_eq!(sink2.all().await.len(), 1);
    }

    #[tokio::test]
    async fn sealing_the_same_lines_twice_yields_the_same_storage_key() {
        let tmp = tempfile::tempdir().unwrap();
        let storage: Arc<dyn LogStorage> =
            Arc::new(FilesystemStorage::new(tmp.path().join("objects")).unwrap());

        let ts = Utc::now();
        let mut line = make_line("cnt-det", LogLevel::Info, "deterministic");
        line.ts = ts;

        let sink1 = Arc::new(VecSink::default());
        let writer1 = ChunkWriterService::open(
            storage.clone(),
            sink1.clone() as Arc<dyn ManifestSink>,
            Some(tmp.path().join("wal-1")),
            None,
        )
        .await
        .unwrap();
        writer1.write_line(line.clone()).await.unwrap();
        writer1.flush_all().await;

        let sink2 = Arc::new(VecSink::default());
        let writer2 = ChunkWriterService::open(
            storage,
            sink2.clone() as Arc<dyn ManifestSink>,
            Some(tmp.path().join("wal-2")),
            None,
        )
        .await
        .unwrap();
        writer2.write_line(line).await.unwrap();
        writer2.flush_all().await;

        let key1 = sink1.all().await[0].storage_key.clone();
        let key2 = sink2.all().await[0].storage_key.clone();
        assert_eq!(key1, key2, "same content must produce the same storage key");
    }

    /// Regression: a collector calls `.abort()` on its per-container streaming
    /// task and then calls `remove_container`. Before this fix, if the abort
    /// landed while `write_line`'s threshold-triggered seal was awaited inline,
    /// `buffer.sealing` was left stuck forever — the seal pipeline was killed
    /// mid-flight with no path left to clear it, so every subsequent call to
    /// `seal_inner`/`remove_container` would wait on `sealing_notify` forever.
    ///
    /// The fix: `write_line` spawns the threshold seal as an independent
    /// `tokio::spawn` task and then awaits the `JoinHandle`. If the caller is
    /// aborted, only the `JoinHandle` await is dropped; the spawned task itself
    /// continues running to completion in the runtime, clears `sealing`, and
    /// fires `sealing_notify`. A subsequent `remove_container` call then
    /// unblocks, seals the data, and returns normally.
    ///
    /// To confirm the test exercises the real fix: temporarily comment out the
    /// `notify.notified().await` wait in `seal_inner` (or the `n.notify_waiters()`
    /// call in `drop_sealing`) and the test will hang in `remove_container`,
    /// proving the `sealing_notify` mechanism is load-bearing.
    #[tokio::test]
    async fn write_line_threshold_seal_survives_the_caller_being_aborted() {
        tokio::time::timeout(std::time::Duration::from_secs(3), async {
            let tmp = tempfile::tempdir().unwrap();
            let sink = Arc::new(GatedSink::default());
            let writer = ChunkWriterService::open(
                Arc::new(FilesystemStorage::new(tmp.path().join("objects")).unwrap()),
                sink.clone() as Arc<dyn ManifestSink>,
                None,
                None,
            )
            .await
            .unwrap();
            // A threshold of 1 byte means the first write_line call will
            // immediately try to seal its buffer.
            writer.set_head_max_bytes(1);

            // Simulate a collector's per-container streaming task: write one
            // line that crosses the byte threshold (triggering a seal) and get
            // aborted mid-seal, exactly as `CollectorService::stop_streaming`
            // does before calling `remove_container`.
            let ingest_writer = writer.clone();
            let ingest_task = tokio::spawn(async move {
                ingest_writer
                    .write_line(make_line(
                        "aborted-mid-seal",
                        LogLevel::Info,
                        "trigger seal",
                    ))
                    .await
            });

            // Wait for the seal to enter the manifest insert (proving the seal
            // task is live inside `GatedSink::insert`), then abort the caller.
            sink.entered.notified().await;
            ingest_task.abort();
            assert!(
                ingest_task.await.unwrap_err().is_cancelled(),
                "ingest task must be cancelled"
            );

            // The detached seal task is still running inside `GatedSink::insert`,
            // waiting on `release`. Release it — the task should clear `sealing`
            // and fire `sealing_notify` as part of its normal completion path.
            sink.release.notify_one();

            // `remove_container` must not hang waiting on a stuck `sealing`
            // slot. Without the fix it would block here forever because the
            // abort killed the only task that could have cleared `sealing`.
            writer
                .remove_container("aborted-mid-seal")
                .await
                .expect("remove_container must converge after caller abort");

            assert_eq!(
                sink.rows.all().await.len(),
                1,
                "sealed chunk must have been committed despite caller abort"
            );
            assert!(
                writer.snapshot("aborted-mid-seal").await.is_none(),
                "buffer must be removed after remove_container"
            );
        })
        .await
        .expect("write_line_threshold_seal_survives_the_caller_being_aborted timed out");
    }

    #[tokio::test]
    async fn remove_container_seals_and_drops_the_wal_file() {
        let tmp = tempfile::tempdir().unwrap();
        let storage: Arc<dyn LogStorage> =
            Arc::new(FilesystemStorage::new(tmp.path().join("objects")).unwrap());
        let sink = Arc::new(VecSink::default());
        let wal_dir = tmp.path().join("wal");
        let writer = ChunkWriterService::open(
            storage,
            sink.clone() as Arc<dyn ManifestSink>,
            Some(wal_dir.clone()),
            None,
        )
        .await
        .unwrap();

        writer
            .write_line(make_line("cnt-remove", LogLevel::Info, "bye"))
            .await
            .unwrap();
        writer.remove_container("cnt-remove").await.unwrap();

        assert_eq!(sink.all().await.len(), 1);
        assert!(writer.snapshot("cnt-remove").await.is_none());
        assert!(!wal_dir.join("cnt-remove.wal").exists());
    }
}
