// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Chunk writer service (ADR-046 §1, §4, §6a, §8, §8a.2): owns the whole seal
//! pipeline for a container's head buffer so the ordering invariant "object
//! written → manifest committed → WAL truncated" lives in one function and
//! readers never observe a gap.
//!
//! This is the v2 writer. It replaces the v1 `ChunkWriterService` that wrote
//! legacy single-frame `.ndjson.zst` objects; those chunks are still read via
//! `format_version = 1` in the [`crate::store::chunk_store::ChunkStore`], but
//! nothing writes them anymore.
//!
//! ## Seal pipeline ordering
//!
//! 1. Under the buffer mutex: move `lines` into `sealing` (leave `lines`
//!    empty so ingest continues). Release the mutex before any IO.
//! 2. Encode with [`ChunkEncoder`].
//! 3. Deterministic `storage_key` from `(container_id, first_ts)` — a
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
//! 8. Under the buffer mutex: clear `sealing`, then truncate the WAL.
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
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use async_trait::async_trait;
use bytes::Bytes;
use chrono::{DateTime, Utc};
use tokio::sync::Mutex;
use tracing::{error, warn};
use uuid::Uuid;

use crate::chunk::cache::{CacheTier, ChunkCache};
use crate::chunk::format::{ChunkEncoder, ChunkIdentity};
use crate::chunk::wal::WalDir;
use crate::chunk::{
    level_bit, ChunkLabels, DEFAULT_HEAD_MAX_BYTES, FLUSH_AGE_SECS, MAX_FLUSH_AGE_SECS,
    MIN_FLUSH_BYTES,
};
use crate::error::LogAggregatorError;
use crate::index::{IndexOutcome, LineIndexSink, NoLineIndex};
use crate::storage::traits::build_storage_key_v2;
use crate::storage::LogStorage;
use crate::store::chunk_store::{HeadSnapshot, HeadSource, HeadSummary};
use crate::store::manifest::ManifestRepo;
use crate::types::{ChunkMeta, LogLevel, LogLine};

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

/// Environment override for [`DEFAULT_HEAD_MAX_BYTES`].
const HEAD_MAX_BYTES_ENV: &str = "TEMPS_LOG_HEAD_BYTES";

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
            head_max_bytes: head_max_bytes_from_env(),
            flush_age_secs: FLUSH_AGE_SECS,
            min_flush_bytes: MIN_FLUSH_BYTES,
            max_flush_age_secs: MAX_FLUSH_AGE_SECS,
        }
    }
}

fn head_max_bytes_from_env() -> usize {
    std::env::var(HEAD_MAX_BYTES_ENV)
        .ok()
        .and_then(|v| v.parse::<usize>().ok())
        .filter(|v| *v > 0)
        .unwrap_or(DEFAULT_HEAD_MAX_BYTES)
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
        }
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
    fn should_seal(&self, t: &Thresholds) -> bool {
        if self.is_empty() {
            return false;
        }
        if self.bytes >= t.head_max_bytes {
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
/// WAL truncate). See the module docs for the ordering guarantee.
pub struct ChunkWriterService {
    storage: Arc<dyn LogStorage>,
    manifests: Arc<dyn ManifestSink>,
    /// ADR-047 line index, written right after the manifest commit.
    line_index: Arc<dyn LineIndexSink>,
    cache: Option<ChunkCache>,
    wal_dir: Option<WalDir>,
    buffers: Mutex<HashMap<String, HeadBuffer>>,
    thresholds: Thresholds,
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
        let wal_dir = match wal_dir {
            Some(root) => Some(WalDir::open(root).await?),
            None => None,
        };

        let service = Arc::new(Self {
            storage,
            manifests,
            line_index,
            cache,
            wal_dir,
            buffers: Mutex::new(HashMap::new()),
            thresholds,
            shed_bloom: AtomicBool::new(false),
            dropped_chunks: AtomicU64::new(0),
            unindexed_chunks: AtomicU64::new(0),
        });

        if let Some(wal_dir) = service.wal_dir.as_ref() {
            let recovered = wal_dir.recover().await?;
            for stream in recovered {
                service.recover_stream(stream).await?;
            }
        }

        Ok(service)
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
        for line in stream.lines {
            buffer.push_line(line);
        }

        {
            let mut buffers = self.buffers.lock().await;
            buffers.insert(stream.container_id.clone(), buffer);
        }
        self.seal(&stream.container_id).await
    }

    /// Append one line to its container's head buffer, sealing immediately
    /// if the buffer just crossed `head_max_bytes`.
    pub async fn write_line(&self, line: LogLine) -> Result<(), LogAggregatorError> {
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
            buffer.bytes >= self.thresholds.head_max_bytes
        };

        if should_seal {
            self.seal(&container_id).await?;
        }
        Ok(())
    }

    /// Seal every head buffer whose flush policy (ADR-046 §1) says it's due.
    pub async fn flush_expired(&self) {
        let due: Vec<String> = {
            let buffers = self.buffers.lock().await;
            buffers
                .iter()
                .filter(|(_, b)| b.sealing.is_none() && b.should_seal(&self.thresholds))
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
    pub async fn flush_all(&self) {
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

    /// Seal then drop a container's buffer and WAL file (call when a
    /// container stops).
    pub async fn remove_container(&self, container_id: &str) -> Result<(), LogAggregatorError> {
        self.seal(container_id).await?;
        {
            let mut buffers = self.buffers.lock().await;
            buffers.remove(container_id);
        }
        if let Some(wal_dir) = &self.wal_dir {
            wal_dir.remove(container_id).await?;
        }
        Ok(())
    }

    /// Flush and fsync every open WAL file. The plugin calls this on a 1 s
    /// ticker so at most ~1 s of ingest is unsynced at any time.
    pub async fn sync_wals(&self) {
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
    async fn drop_sealing(&self, container_id: &str) {
        let mut buffers = self.buffers.lock().await;
        if let Some(buffer) = buffers.get_mut(container_id) {
            buffer.sealing = None;
            buffer.sealing_stats = None;
        }
    }

    /// The seal pipeline. See the module docs for the step-by-step ordering
    /// guarantee. Never returns an error for a storage/manifest failure that
    /// was already retried and logged — those are shed (ADR-021), not
    /// propagated, so a single bad object store doesn't take down ingest.
    /// Only encode failures (a bug, not an operational fault) and WAL IO
    /// errors propagate.
    async fn seal(&self, container_id: &str) -> Result<(), LogAggregatorError> {
        let (identity, sealing_segments) = {
            let mut buffers = self.buffers.lock().await;
            let Some(buffer) = buffers.get_mut(container_id) else {
                return Ok(());
            };
            if buffer.sealing.is_some() {
                // A seal for this container is already in flight (or a prior
                // seal failed without clearing `sealing`, which cannot
                // happen — every failure path clears it). Skip rather than
                // double-seal.
                return Ok(());
            }
            // Freeze any unfrozen tail so every line about to be sealed is
            // captured in an immutable segment before we release the lock.
            buffer.freeze_active();
            if buffer.segments.is_empty() {
                return Ok(());
            }
            let sealing_segments = std::mem::take(&mut buffer.segments);
            buffer.bytes = 0;
            buffer.opened_at = Instant::now();
            buffer.sealing = Some(sealing_segments.clone());
            buffer.sealing_stats = Some(buffer.stats);
            buffer.stats = BufferStats::default();
            (buffer.identity.clone(), sealing_segments)
        };

        let with_bloom = !self.shed_bloom.load(Ordering::Relaxed);
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
        let storage_key = build_storage_key_v2(
            labels.project_id,
            labels.external_service_id,
            &labels.env,
            &labels.service,
            labels.started_at,
            &labels.container_id,
            None,
        );

        let write_result =
            retry_with_backoff(|| self.storage.write_chunk(&storage_key, &encoded.bytes)).await;
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
                return Ok(());
            }
        };

        let meta = build_chunk_meta(&encoded, &storage_key, compressed_size);
        let manifests = &self.manifests;
        let seq = match retry_with_backoff(|| manifests.insert(&meta)).await {
            Ok(seq) => seq,
            Err(e) => {
                error!(
                    container_id = container_id,
                    storage_key = %storage_key,
                    error = %e,
                    "manifest insert failed after retries; object is on disk, WAL retained for \
                     the reconcile sweep to re-adopt it"
                );
                self.drop_sealing(container_id).await;
                return Ok(());
            }
        };

        // ADR-047 §4: index the lines now that the chunk is committed. An
        // index failure never blocks sealing — the manifest simply stays
        // unmarked and the reindexer retries it from the chunk later.
        let line_index = &self.line_index;
        match retry_with_backoff(|| line_index.index_chunk(seq, labels, &sealing_segments)).await {
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

        {
            let mut buffers = self.buffers.lock().await;
            if let Some(buffer) = buffers.get_mut(container_id) {
                buffer.sealing = None;
                buffer.sealing_stats = None;
                if let Some(wal) = buffer.wal.as_mut() {
                    wal.stream.truncate().await?;
                }
            }
        }

        Ok(())
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
async fn retry_with_backoff<T, E, F, Fut>(mut f: F) -> Result<T, E>
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::chunk::format::{decode_block, decode_footer, decode_trailer};
    use crate::storage::FilesystemStorage;
    use crate::types::{LogLevel, LogStream};
    use chrono::Utc;
    use tokio::sync::Mutex as StdAsyncMutex;

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

    #[async_trait]
    impl ManifestSink for VecSink {
        async fn insert(&self, meta: &ChunkMeta) -> Result<i64, LogAggregatorError> {
            let mut rows = self.rows.lock().await;
            rows.push(meta.clone());
            Ok(rows.len() as i64)
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
        // the same WAL dir recovers it (proves it wasn't truncated).
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
