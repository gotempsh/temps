// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Background compaction and garbage collection for log chunks (ADR-046
//! §8a.1, §8a.3).
//!
//! **Compaction.** The writer seals chunks for freshness (every few minutes),
//! which leaves idle containers with dozens of tiny objects per day. The
//! compactor merges one container's chunks for one completed UTC day into as
//! few objects as fit under [`MAX_COMPACTED_BYTES`], so the query planner's
//! fan-out stays proportional to containers, not to flush ticks. The swap is:
//! write the merged object → insert its manifest → tombstone the originals.
//! A crash between the last two steps leaves the originals readable alongside
//! the merged chunk (a transient duplicate, never a gap); the next run
//! re-tombstones them because the merged manifest's key is deterministic.
//!
//! **GC.** Nothing deletes an object while a reader might be mid-fetch.
//! Retention and compaction only *tombstone* manifests (`deleted_at`); this
//! service deletes the objects — and then the rows — once a tombstone is
//! older than [`GC_GRACE`].

use std::sync::Arc;
use std::time::Duration;

use chrono::{DateTime, Datelike, TimeZone, Utc};
use tracing::{debug, error, info, warn};

use crate::chunk::cache::{CacheTier, ChunkCache};
use crate::chunk::format::{
    decode_block, decode_footer, decode_trailer, decode_v1, BlockFilter, ChunkEncoder,
    ChunkIdentity,
};
use crate::chunk::{DecodedLine, MAX_COMPACTED_BYTES};
use crate::error::LogAggregatorError;
use crate::index::{IndexOutcome, LineIndexSink, NoLineIndex};
use crate::storage::{traits::build_storage_key_v2, LogStorage};
use crate::store::manifest::{Manifest, ManifestRepo};
use crate::types::{ChunkMeta, LogLevel, LogLine, LogStream};

/// A tombstoned manifest must be at least this old before its object is
/// deleted. Longer than any search budget by a wide margin.
pub const GC_GRACE: Duration = Duration::from_secs(60 * 60);

/// Containers with fewer live chunks than this in a day are left alone.
const MIN_CHUNKS_TO_COMPACT: u32 = 4;

/// Containers processed per compaction run.
const CONTAINERS_PER_RUN: u32 = 200;

/// Tombstones processed per GC run.
const GC_BATCH: u32 = 500;

/// Rough uncompressed-bytes estimate for a manifest whose object we have not
/// read yet: the writer stores compressed size; logs compress ~10×.
fn estimated_raw_bytes(m: &Manifest) -> usize {
    (m.compressed_size_bytes as usize).saturating_mul(10)
}

#[derive(Debug, Default, Clone)]
pub struct CompactionReport {
    pub containers: u64,
    pub chunks_in: u64,
    pub chunks_out: u64,
    pub bytes_in: u64,
    pub bytes_out: u64,
    pub failed: u64,
}

#[derive(Debug, Default, Clone)]
pub struct ReconcileReport {
    pub adopted: u64,
    pub tombstoned: u64,
    pub failed: u64,
}

#[derive(Debug, Default, Clone)]
pub struct GcReport {
    pub objects_deleted: u64,
    pub rows_deleted: u64,
    pub failed: u64,
}

pub struct CompactorService {
    manifests: Arc<ManifestRepo>,
    storage: Arc<dyn LogStorage>,
    cache: Option<ChunkCache>,
    /// ADR-047: rows of chunks this service retires are forgotten in the
    /// line index; merged chunks are indexed by the reindexer (they are
    /// inserted with `indexed_at IS NULL`).
    line_index: Arc<dyn LineIndexSink>,
}

/// Read every line of a chunk from `storage`, oldest first, bypassing the
/// cache (whole-object readers: compaction, reindexing).
pub(crate) async fn read_all_lines(
    storage: &dyn LogStorage,
    m: &Manifest,
) -> Result<Vec<DecodedLine>, LogAggregatorError> {
    if m.format_version < 2 {
        let raw = storage.read_chunk(&m.storage_key).await?;
        return decode_v1(&raw);
    }
    let (Some(off), Some(len)) = (m.footer_offset, m.footer_len) else {
        return Err(LogAggregatorError::ChunkFormat {
            reason: format!("manifest {} has no footer location", m.id),
        });
    };
    let footer_bytes = storage
        .read_chunk_range(&m.storage_key, off, Some(off + u64::from(len)))
        .await?;
    let trailer = decode_trailer(&footer_bytes)?;
    let footer = decode_footer(&footer_bytes, &trailer)?;
    // The body is contiguous: one read for all blocks.
    let body = storage
        .read_chunk_range(&m.storage_key, 0, Some(off))
        .await?;
    let mut out = Vec::with_capacity(m.line_count as usize);
    let unfiltered = BlockFilter::default();
    for meta in &footer.blocks {
        let s = meta.offset as usize;
        let e = s + meta.len as usize;
        let slice = body
            .get(s..e)
            .ok_or_else(|| LogAggregatorError::ChunkFormat {
                reason: format!("block range {s}..{e} outside body of {}", m.storage_key),
            })?;
        out.extend(decode_block(slice, meta, &unfiltered)?);
    }
    Ok(out)
}

/// Rebuild a [`LogLine`] from a decoded line plus its chunk's labels.
pub(crate) fn decoded_to_log_line(labels: &crate::chunk::ChunkLabels, l: DecodedLine) -> LogLine {
    LogLine {
        ts: l.ts,
        stream: l.stream,
        level: l.level,
        msg: l.message,
        fields: l.fields,
        container_id: labels.container_id.clone(),
        service: labels.service.clone(),
        env: labels.env.clone(),
        project_id: labels.project_id,
        external_service_id: labels.external_service_id,
        deploy_id: labels.deploy_id,
        node_id: labels.node_id,
        node_name: labels.node_name.clone(),
    }
}

impl CompactorService {
    pub fn new(
        manifests: Arc<ManifestRepo>,
        storage: Arc<dyn LogStorage>,
        cache: Option<ChunkCache>,
    ) -> Self {
        Self {
            manifests,
            storage,
            cache,
            line_index: Arc::new(NoLineIndex::default()),
        }
    }

    pub fn with_line_index(mut self, line_index: Arc<dyn LineIndexSink>) -> Self {
        self.line_index = line_index;
        self
    }

    /// Forget retired chunks in the line index; logged, never fatal.
    async fn forget_in_index(&self, seqs: &[i64], why: &str) {
        if let Err(e) = self.line_index.forget_chunks(seqs).await {
            warn!(chunks = seqs.len(), why, error = %e, "line index forget failed; rows age out by TTL");
        }
    }

    /// The most recent *completed* UTC day: `[00:00 yesterday, 00:00 today)`.
    fn previous_day(now: DateTime<Utc>) -> (DateTime<Utc>, DateTime<Utc>) {
        let today = Utc
            .with_ymd_and_hms(now.year(), now.month(), now.day(), 0, 0, 0)
            .single()
            .unwrap_or(now);
        (today - chrono::Duration::days(1), today)
    }

    /// Compact every fragmented container for the previous UTC day.
    pub async fn run_once(&self) -> CompactionReport {
        let (start, end) = Self::previous_day(Utc::now());
        self.compact_window(start, end).await
    }

    /// Compact every fragmented container whose chunks end inside `[start, end)`.
    pub async fn compact_window(
        &self,
        start: DateTime<Utc>,
        end: DateTime<Utc>,
    ) -> CompactionReport {
        let mut report = CompactionReport::default();
        let containers = match self
            .manifests
            .fragmented_containers(start, end, MIN_CHUNKS_TO_COMPACT, CONTAINERS_PER_RUN)
            .await
        {
            Ok(c) => c,
            Err(e) => {
                error!(error = %e, "compactor: could not list fragmented containers");
                report.failed += 1;
                return report;
            }
        };
        for container_id in containers {
            match self.compact_container(&container_id, start, end).await {
                Ok(r) => {
                    report.containers += 1;
                    report.chunks_in += r.chunks_in;
                    report.chunks_out += r.chunks_out;
                    report.bytes_in += r.bytes_in;
                    report.bytes_out += r.bytes_out;
                }
                Err(e) => {
                    warn!(container_id, error = %e, "compactor: container skipped");
                    report.failed += 1;
                }
            }
        }
        if report.chunks_in > 0 {
            info!(
                containers = report.containers,
                chunks_in = report.chunks_in,
                chunks_out = report.chunks_out,
                bytes_in = report.bytes_in,
                bytes_out = report.bytes_out,
                failed = report.failed,
                "log chunk compaction completed"
            );
        }
        report
    }

    async fn compact_container(
        &self,
        container_id: &str,
        start: DateTime<Utc>,
        end: DateTime<Utc>,
    ) -> Result<CompactionReport, LogAggregatorError> {
        let chunks = self
            .manifests
            .for_container_window(container_id, start, end)
            .await?;
        let mut report = CompactionReport::default();
        if (chunks.len() as u32) < MIN_CHUNKS_TO_COMPACT {
            return Ok(report);
        }

        // Group consecutive chunks into runs that fit the compaction cap.
        let mut runs: Vec<Vec<Manifest>> = Vec::new();
        let mut current: Vec<Manifest> = Vec::new();
        let mut current_bytes = 0usize;
        for m in chunks {
            let est = estimated_raw_bytes(&m);
            if !current.is_empty() && current_bytes + est > MAX_COMPACTED_BYTES {
                runs.push(std::mem::take(&mut current));
                current_bytes = 0;
            }
            current_bytes += est;
            current.push(m);
        }
        if !current.is_empty() {
            runs.push(current);
        }

        for run in runs.into_iter().filter(|r| r.len() >= 2) {
            let r = self.merge_run(run).await?;
            report.chunks_in += r.chunks_in;
            report.chunks_out += r.chunks_out;
            report.bytes_in += r.bytes_in;
            report.bytes_out += r.bytes_out;
        }
        Ok(report)
    }

    /// Read every line of a chunk, oldest first.
    async fn read_all(&self, m: &Manifest) -> Result<Vec<DecodedLine>, LogAggregatorError> {
        read_all_lines(self.storage.as_ref(), m).await
    }

    async fn merge_run(&self, run: Vec<Manifest>) -> Result<CompactionReport, LogAggregatorError> {
        let first = &run[0];
        let identity = ChunkIdentity {
            project_id: first.labels.project_id,
            external_service_id: first.labels.external_service_id,
            env: first.labels.env.clone(),
            service: first.labels.service.clone(),
            container_id: first.labels.container_id.clone(),
            deploy_id: first.labels.deploy_id,
            node_id: first.labels.node_id,
            node_name: first.labels.node_name.clone(),
        };

        let mut lines: Vec<DecodedLine> = Vec::new();
        let mut bytes_in = 0u64;
        for m in &run {
            lines.extend(self.read_all(m).await?);
            bytes_in += u64::from(m.compressed_size_bytes);
        }
        if lines.is_empty() {
            return Ok(CompactionReport::default());
        }
        // Chunks of one container are already ordered; a stable sort only
        // repairs boundary overlap from out-of-order event times.
        lines.sort_by_key(|l| l.ts);

        let log_lines: Vec<LogLine> = lines
            .into_iter()
            .map(|l| LogLine {
                ts: l.ts,
                stream: l.stream,
                level: l.level,
                msg: l.message,
                fields: l.fields,
                container_id: identity.container_id.clone(),
                service: identity.service.clone(),
                env: identity.env.clone(),
                project_id: identity.project_id,
                external_service_id: identity.external_service_id,
                deploy_id: identity.deploy_id,
                node_id: identity.node_id,
                node_name: identity.node_name.clone(),
            })
            .collect();
        let mut encoder = ChunkEncoder::new(identity.clone());
        for l in &log_lines {
            encoder.push(l)?;
        }
        let encoded = encoder.finish(true)?;
        let labels = &encoded.footer.labels;

        let storage_key = build_storage_key_v2(
            identity.project_id,
            identity.external_service_id,
            &identity.env,
            &identity.service,
            labels.started_at,
            &identity.container_id,
            Some("c"),
        );
        let size = self
            .storage
            .write_chunk(&storage_key, &encoded.bytes)
            .await?;

        let trailer = &encoded.trailer;
        let meta = ChunkMeta {
            id: uuid::Uuid::new_v4(),
            project_id: identity.project_id,
            external_service_id: identity.external_service_id,
            env: identity.env.clone(),
            service: identity.service.clone(),
            container_id: identity.container_id.clone(),
            deploy_id: identity.deploy_id,
            node_id: identity.node_id,
            node_name: identity.node_name.clone(),
            started_at: labels.started_at,
            ended_at: labels.ended_at,
            storage_key: storage_key.clone(),
            line_count: labels.line_count as i32,
            compressed_size_bytes: size as i32,
            has_errors: labels.level_mask
                & (crate::chunk::level_bit(LogLevel::Warn)
                    | crate::chunk::level_bit(LogLevel::Error))
                != 0,
            line_offsets: vec![],
            format_version: crate::chunk::FORMAT_VERSION,
            level_mask: labels.level_mask,
            level_counts: labels.level_counts.to_vec(),
            footer_offset: Some(trailer.footer_offset()),
            footer_len: Some(trailer.footer_len() as u32),
            bloom_len: trailer.bloom_len,
        };
        let seq = self.manifests.insert(&meta).await?;

        // The index must cover the replacement before the sources leave it,
        // or every line of this window vanishes from facets, histograms and
        // attribute search until the reindexer catches up. If the index
        // rejects the rows, abandon this run atomically and leave the
        // sources — and their index rows — exactly as they were for the
        // next pass. The replacement is removed outright (row, then
        // object), not tombstoned: its storage key is deterministic, and a
        // tombstoned row under that key would absorb the retry's insert
        // via `ON CONFLICT DO NOTHING` and hide the merged chunk forever.
        let segments = [Arc::new(log_lines)];
        match self.line_index.index_chunk(seq, labels, &segments).await {
            Ok(IndexOutcome::Indexed) => self.manifests.mark_indexed(seq).await?,
            Ok(IndexOutcome::Skipped) => {}
            Err(e) => {
                warn!(seq, storage_key, error = %e, "line index rejected compacted chunk; keeping sources");
                self.manifests.mark_deleted(&[meta.id]).await?;
                self.manifests.hard_delete(&[meta.id]).await?;
                if let Err(del) = self.storage.delete_chunk(&storage_key).await {
                    // Reconcile treats a manifest-less object as an orphan
                    // and removes it later.
                    warn!(storage_key, error = %del, "could not remove abandoned compacted object");
                }
                return Err(e);
            }
        }
        drop(segments);

        if let Some(cache) = &self.cache {
            let off = trailer.footer_offset();
            let len = trailer.footer_len();
            let footer = encoded.bytes[off as usize..(off + len) as usize].to_vec();
            let tier = if trailer.bloom_len > 16 * 1024 {
                CacheTier::Bloom
            } else {
                CacheTier::Index
            };
            cache
                .put(
                    &storage_key,
                    off,
                    off + len,
                    bytes::Bytes::from(footer),
                    tier,
                )
                .await;
        }

        let ids: Vec<uuid::Uuid> = run.iter().map(|m| m.id).collect();
        self.manifests.mark_deleted(&ids).await?;
        let seqs: Vec<i64> = run.iter().map(|m| m.seq).collect();
        self.forget_in_index(&seqs, "compacted").await;
        debug!(
            container_id = identity.container_id,
            merged = run.len(),
            lines = labels.line_count,
            storage_key,
            "compacted log chunks"
        );
        Ok(CompactionReport {
            containers: 0,
            chunks_in: run.len() as u64,
            chunks_out: 1,
            bytes_in,
            bytes_out: size,
            failed: 0,
        })
    }

    /// Reconcile manifests with the bucket, in both directions (ADR-046
    /// §8a.3). Objects without a manifest — a crash between PUT and commit,
    /// or a restored/older Postgres — are re-adopted from their own footer
    /// (the file is self-describing). Live manifests whose object no longer
    /// exists are tombstoned so searches skip them instead of failing.
    pub async fn reconcile_once(&self) -> ReconcileReport {
        let mut report = ReconcileReport::default();

        // Pass 1: bucket → manifests.
        match self.storage.list_chunks("logs/").await {
            Ok(keys) => {
                // v1 objects have no footer to adopt from and are skipped by
                // their `.ndjson.zst` suffix; anything else ending in `.zst`
                // is a v2 candidate (the trailer magic is checked on adopt).
                let suffix = format!(".{}", crate::chunk::V2_EXTENSION);
                let v2: Vec<String> = keys
                    .into_iter()
                    .filter(|k| k.ends_with(&suffix) && !k.ends_with(crate::chunk::V1_SUFFIX))
                    .collect();
                for batch in v2.chunks(500) {
                    let known = match self.manifests.known_storage_keys(batch).await {
                        Ok(k) => k,
                        Err(e) => {
                            error!(error = %e, "reconcile: manifest lookup failed");
                            report.failed += 1;
                            continue;
                        }
                    };
                    for key in batch.iter().filter(|k| !known.contains(*k)) {
                        match self.adopt(key).await {
                            Ok(()) => report.adopted += 1,
                            Err(e) => {
                                warn!(storage_key = key, error = %e, "reconcile: could not adopt object");
                                report.failed += 1;
                            }
                        }
                    }
                }
            }
            Err(e) => {
                error!(error = %e, "reconcile: bucket listing failed");
                report.failed += 1;
            }
        }

        // Pass 2: manifests → bucket.
        let mut after = 0i64;
        loop {
            let page = match self.manifests.live_after_seq(after, 500).await {
                Ok(p) => p,
                Err(e) => {
                    error!(error = %e, "reconcile: manifest page failed");
                    report.failed += 1;
                    break;
                }
            };
            if page.is_empty() {
                break;
            }
            let mut missing = Vec::new();
            let mut missing_seqs = Vec::new();
            for m in &page {
                after = after.max(m.seq);
                match self.storage.chunk_exists(&m.storage_key).await {
                    Ok(true) => {}
                    Ok(false) => {
                        missing.push(m.id);
                        missing_seqs.push(m.seq);
                    }
                    Err(e) => {
                        warn!(storage_key = m.storage_key, error = %e, "reconcile: exists check failed");
                        report.failed += 1;
                    }
                }
            }
            if !missing.is_empty() {
                match self.manifests.mark_deleted(&missing).await {
                    Ok(n) => {
                        report.tombstoned += n;
                        self.forget_in_index(&missing_seqs, "object missing").await;
                    }
                    Err(e) => {
                        error!(error = %e, "reconcile: tombstoning missing objects failed");
                        report.failed += 1;
                    }
                }
            }
        }
        if report.adopted > 0 || report.tombstoned > 0 || report.failed > 0 {
            info!(
                adopted = report.adopted,
                tombstoned = report.tombstoned,
                failed = report.failed,
                "log chunk reconcile completed"
            );
        }
        report
    }

    /// Insert a manifest for an object from its footer alone.
    async fn adopt(&self, key: &str) -> Result<(), LogAggregatorError> {
        // Adoption is rare, so reading the whole object is fine.
        let bytes = self.storage.read_chunk(key).await?;
        let total = bytes.len();
        if total < crate::chunk::TRAILER_LEN {
            return Err(LogAggregatorError::ChunkFormat {
                reason: format!("{key}: shorter than a trailer"),
            });
        }
        let trailer = decode_trailer(&bytes[total - crate::chunk::TRAILER_LEN..])?;
        let off = trailer.footer_offset() as usize;
        let len = trailer.footer_len() as usize;
        let footer_bytes =
            bytes
                .get(off..off + len)
                .ok_or_else(|| LogAggregatorError::ChunkFormat {
                    reason: format!("{key}: footer range outside object"),
                })?;
        let footer = decode_footer(footer_bytes, &trailer)?;
        let l = &footer.labels;
        let meta = ChunkMeta {
            id: uuid::Uuid::new_v4(),
            project_id: l.project_id,
            external_service_id: l.external_service_id,
            env: l.env.clone(),
            service: l.service.clone(),
            container_id: l.container_id.clone(),
            deploy_id: l.deploy_id,
            node_id: l.node_id,
            node_name: l.node_name.clone(),
            started_at: l.started_at,
            ended_at: l.ended_at,
            storage_key: key.to_string(),
            line_count: l.line_count as i32,
            compressed_size_bytes: total as i32,
            has_errors: l.level_mask
                & (crate::chunk::level_bit(LogLevel::Warn)
                    | crate::chunk::level_bit(LogLevel::Error))
                != 0,
            line_offsets: vec![],
            format_version: trailer.version,
            level_mask: l.level_mask,
            level_counts: l.level_counts.to_vec(),
            footer_offset: Some(trailer.footer_offset()),
            footer_len: Some(trailer.footer_len() as u32),
            bloom_len: trailer.bloom_len,
        };
        self.manifests.insert(&meta).await?;
        debug!(
            storage_key = key,
            lines = l.line_count,
            "reconcile: adopted orphaned chunk"
        );
        Ok(())
    }

    /// Delete objects (then rows) of manifests tombstoned before `now - GC_GRACE`.
    pub async fn gc_once(&self) -> GcReport {
        let cutoff = Utc::now() - chrono::Duration::from_std(GC_GRACE).unwrap_or_default();
        let mut report = GcReport::default();
        let rows = match self.manifests.tombstoned_before(cutoff, GC_BATCH).await {
            Ok(r) => r,
            Err(e) => {
                error!(error = %e, "log chunk gc: could not list tombstones");
                report.failed += 1;
                return report;
            }
        };
        let mut done: Vec<uuid::Uuid> = Vec::with_capacity(rows.len());
        for (id, key) in rows {
            match self.storage.delete_chunk(&key).await {
                Ok(()) => {
                    report.objects_deleted += 1;
                    done.push(id);
                }
                Err(e) => {
                    warn!(storage_key = key, error = %e, "log chunk gc: object delete failed");
                    report.failed += 1;
                }
            }
        }
        if !done.is_empty() {
            match self.manifests.hard_delete(&done).await {
                Ok(n) => report.rows_deleted += n,
                Err(e) => {
                    error!(error = %e, "log chunk gc: row delete failed");
                    report.failed += 1;
                }
            }
        }
        if report.objects_deleted > 0 || report.failed > 0 {
            info!(
                objects = report.objects_deleted,
                rows = report.rows_deleted,
                failed = report.failed,
                "log chunk gc completed"
            );
        }
        report
    }
}

// Keep `LogStream` referenced for readers of this module: the encoder copies
// it through from decoded lines unchanged.
#[allow(dead_code)]
const _: Option<LogStream> = None;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::chunk::cache::ChunkCache;
    use crate::chunk::ChunkLabels;
    use crate::index::IndexOutcome;
    use crate::types::LogLevel;
    use std::sync::Mutex;

    /// Records what the compactor tells the index, in order, and can be
    /// told to reject the next `index_chunk`.
    #[derive(Default)]
    struct RecordingSink {
        events: Mutex<Vec<String>>,
        reject: Mutex<bool>,
    }

    #[async_trait::async_trait]
    impl LineIndexSink for RecordingSink {
        async fn index_chunk(
            &self,
            seq: i64,
            _labels: &ChunkLabels,
            segments: &[Arc<Vec<LogLine>>],
        ) -> Result<IndexOutcome, LogAggregatorError> {
            if *self.reject.lock().unwrap() {
                return Err(LogAggregatorError::LineIndex {
                    reason: "test rejection".into(),
                });
            }
            let lines: usize = segments.iter().map(|s| s.len()).sum();
            self.events
                .lock()
                .unwrap()
                .push(format!("index {seq} ({lines} lines)"));
            Ok(IndexOutcome::Indexed)
        }

        async fn forget_chunks(&self, seqs: &[i64]) -> Result<(), LogAggregatorError> {
            let mut sorted = seqs.to_vec();
            sorted.sort_unstable();
            self.events
                .lock()
                .unwrap()
                .push(format!("forget {sorted:?}"));
            Ok(())
        }
    }

    fn line(container: &str, ts: DateTime<Utc>, i: usize) -> LogLine {
        LogLine {
            ts,
            stream: LogStream::Stdout,
            level: if i.is_multiple_of(10) {
                LogLevel::Error
            } else {
                LogLevel::Info
            },
            msg: format!("line {i}"),
            fields: Some(serde_json::json!({"n": i})),
            container_id: container.to_string(),
            service: "svc".into(),
            env: "prod".into(),
            project_id: 7,
            external_service_id: None,
            deploy_id: Some(1),
            node_id: None,
            node_name: None,
        }
    }

    /// Compaction must hand the merged chunk to the index — and have it
    /// accepted — before the sources are forgotten, so analytics never lose
    /// the window; and when the index rejects the merged chunk, nothing
    /// changes for the sources.
    #[tokio::test]
    #[serial_test::serial]
    async fn compaction_indexes_replacement_before_forgetting_sources() {
        let db = match temps_database::test_utils::TestDatabase::with_migrations().await {
            Ok(db) => db,
            Err(_) => {
                println!("Docker/DB not available, skipping test");
                return;
            }
        };
        let tmp = tempfile::tempdir().unwrap();
        let storage: Arc<dyn LogStorage> =
            Arc::new(crate::storage::FilesystemStorage::new(tmp.path().join("objects")).unwrap());
        let manifests = Arc::new(ManifestRepo::new(db.connection_arc()));
        let cache = ChunkCache::open(Some(tmp.path().join("cache")), 8 * 1024 * 1024)
            .await
            .unwrap();
        let sink = Arc::new(RecordingSink::default());
        let writer = crate::services::ChunkWriterService::open_with_index(
            storage.clone(),
            manifests.clone(),
            Some(tmp.path().join("wal")),
            Some(cache.clone()),
            sink.clone(),
        )
        .await
        .unwrap();

        // Six small sealed chunks of one container inside yesterday's window.
        let container = "compact-me-000000000001";
        let (start, end) = CompactorService::previous_day(Utc::now());
        let mut ts = start + chrono::Duration::hours(1);
        let mut n = 0usize;
        for _ in 0..6 {
            for _ in 0..50 {
                ts += chrono::Duration::milliseconds(3);
                n += 1;
                writer.write_line(line(container, ts, n)).await.unwrap();
            }
            writer.flush_all().await;
        }
        let sources = manifests
            .for_container_window(container, start, end)
            .await
            .unwrap();
        assert_eq!(sources.len(), 6);
        let source_seqs: Vec<i64> = sources.iter().map(|m| m.seq).collect();
        let (live, indexed) = manifests.index_coverage().await.unwrap();
        assert_eq!((live, indexed), (6, 6), "writer indexed every seal");
        sink.events.lock().unwrap().clear();

        let compactor =
            CompactorService::new(manifests.clone(), storage.clone(), Some(cache.clone()))
                .with_line_index(sink.clone());

        // 1. Rejection: atomic no-op for the sources.
        *sink.reject.lock().unwrap() = true;
        let report = compactor.compact_window(start, end).await;
        assert_eq!(report.failed, 1, "{report:?}");
        assert_eq!(report.chunks_out, 0);
        let after = manifests
            .for_container_window(container, start, end)
            .await
            .unwrap();
        assert_eq!(
            after.iter().map(|m| m.seq).collect::<Vec<_>>(),
            source_seqs,
            "sources stay live when the index rejects the merged chunk"
        );
        assert!(
            sink.events.lock().unwrap().is_empty(),
            "nothing forgotten on rejection: {:?}",
            sink.events.lock().unwrap()
        );
        let (live, indexed) = manifests.index_coverage().await.unwrap();
        assert_eq!((live, indexed), (6, 6), "the abandoned replacement is gone");

        // A search result's key from a source chunk, taken before compaction.
        let store = crate::store::chunk_store::ChunkStore::new(
            ManifestRepo::new(db.connection_arc()),
            storage.clone(),
            cache.clone(),
            writer.clone(),
        );
        let scope = crate::store::LogAccessScope::All;
        let mut q = crate::store::LogQuery::for_scope(scope.clone());
        q.start_time = start;
        q.end_time = end;
        q.limit = 10;
        q.text = Some("line 123".into());
        let page = crate::store::LogLineStore::search(&store, &q)
            .await
            .unwrap();
        let stale_key = page.lines[0].key();
        assert_eq!(page.lines[0].message, "line 123");
        assert!(stale_key.chunk_position().is_some());

        // 2. Success: index the replacement, then forget the sources.
        *sink.reject.lock().unwrap() = false;
        let report = compactor.compact_window(start, end).await;
        assert_eq!(
            (report.chunks_in, report.chunks_out, report.failed),
            (6, 1, 0)
        );
        let merged = manifests
            .for_container_window(container, start, end)
            .await
            .unwrap();
        assert_eq!(merged.len(), 1);
        assert_eq!(merged[0].line_count, 300);
        let events = sink.events.lock().unwrap().clone();
        let mut sorted_sources = source_seqs.clone();
        sorted_sources.sort_unstable();
        assert_eq!(
            events,
            vec![
                format!("index {} (300 lines)", merged[0].seq),
                format!("forget {sorted_sources:?}"),
            ],
            "replacement indexed strictly before sources are forgotten"
        );
        let (live, indexed) = manifests.index_coverage().await.unwrap();
        assert_eq!((live, indexed), (1, 1), "merged chunk is marked indexed");

        // 3. The pre-compaction key still resolves: its chunk is gone, so
        //    context relocates the line by (container, timestamp).
        let ctx = crate::store::LogLineStore::context(&store, &scope, &stale_key, 2, 2)
            .await
            .unwrap();
        let messages: Vec<&str> = ctx.iter().map(|l| l.message.as_str()).collect();
        assert_eq!(
            messages,
            ["line 121", "line 122", "line 123", "line 124", "line 125"],
            "context around a compacted-away line id"
        );
        assert_eq!(ctx[2].timestamp, stale_key.timestamp);
        assert!(
            ctx[2].key().chunk_position().map(|(s, _)| s) == Some(merged[0].seq),
            "relocated into the merged chunk"
        );
    }
}
