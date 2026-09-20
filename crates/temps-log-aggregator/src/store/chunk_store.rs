// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! The chunk-backed [`LogLineStore`] (ADR-046 §3): plan over manifests, read
//! only the blocks a page needs, merge newest-first, stop as soon as the page
//! is provably complete.
//!
//! ## The stop rule
//!
//! Candidates — sealed chunks from `log_chunks` plus the writer's unsealed
//! head buffers — are processed strictly in `ended_at DESC` order. A matching
//! line at time `T` is **final** once every unprocessed candidate has
//! `ended_at < T`: nothing left to read can precede it. The page is emitted
//! as soon as `limit` lines are final. For "last 500 lines, no filter" that
//! is roughly one block per active container.
//!
//! ## Honest partial pages
//!
//! Processing has a time budget and a decompressed-byte budget. When either
//! runs out (checked only *between* chunks, never mid-chunk) the lines that
//! are already final are returned together with a cursor positioned at the
//! horizon `H` — the `ended_at` of the newest *unprocessed* candidate. Every
//! line with `ts > H` from a processed chunk is final and has been emitted;
//! every line with `ts ≤ H` has not been emitted and will be found again from
//! the cursor. So a partial page is a strict prefix of the complete result —
//! no duplicates, no gaps — and *Next* keeps working. A partial page is only
//! returned once `H` is strictly older than the page's window end, which is
//! what guarantees every page makes progress. [`LogPage::scanned_back_to`]
//! carries `H`.
//!
//! ## Resuming
//!
//! A cursor is a [`LogLineKey`]; the next page holds lines strictly older.
//! Candidate chunks are those overlapping `[start, cursor.ts]`; a chunk that
//! spans the cursor is re-read with an in-chunk `key < cursor` filter. At
//! most one chunk per active container can span any instant, so resume cost
//! is bounded regardless of page depth.

use std::cmp::Ordering;
use std::collections::{BTreeSet, BinaryHeap, HashMap};
use std::sync::Arc;
use std::time::{Duration, Instant};

use async_trait::async_trait;
use bytes::Bytes;
use chrono::{DateTime, Utc};
use futures::stream::{self, StreamExt};
use tracing::{debug, warn};

use crate::chunk::bloom;
use crate::chunk::cache::{CacheTier, ChunkCache};
use crate::chunk::format::{decode_block, decode_footer, decode_v1, BlockFilter, ChunkIdentity};
use crate::chunk::{level_bit, level_mask_for, ChunkFooter, ChunkLabels, DecodedLine};
use crate::error::LogAggregatorError;
use crate::index::analytics::AttrPredicate;
use crate::storage::LogStorage;
use crate::store::manifest::{Manifest, ManifestCursor, ManifestRepo};
use crate::store::{
    FacetField, FacetResult, LogAccessScope, LogLineKey, LogLineRecord, LogLineStore, LogPage,
    LogQuery, LogSelection, LogSourceKind, HEAD_LINE_ID_BASE,
};
use crate::types::{ContextLine, LineContext, LogLine, LogSource};

// ── Budgets ─────────────────────────────────────────────────────────────

/// Per-request limits. None of these ever produce an error: hitting one
/// yields an honest partial page with a resumable cursor.
#[derive(Debug, Clone)]
pub struct SearchBudget {
    /// Wall-clock budget for one `search` call.
    pub time: Duration,
    /// Decompressed bytes one `search` call may process. This bounds work,
    /// not memory (blocks are decoded one at a time per in-flight chunk):
    /// at the ~250 MB/s a block scan runs, 1 GiB is ~4 s, comfortably inside
    /// `time`. Measured at 50M lines / 800 chunks, a bloom-pruned needle
    /// search decodes ~60 chunks ≈ 330 MB — a 256 MiB budget cut it off one
    /// page short of the hit every time.
    pub bytes: u64,
    /// Chunks fetched/scanned concurrently.
    pub parallel: usize,
    /// Manifest rows fetched per planner batch.
    pub batch: u32,
}

impl Default for SearchBudget {
    fn default() -> Self {
        Self {
            time: Duration::from_secs(10),
            bytes: 1024 * 1024 * 1024,
            parallel: 16,
            batch: 64,
        }
    }
}

// ── Head source ─────────────────────────────────────────────────────────

/// A cheap, content-free summary of one container's unsealed lines — a
/// `ChunkLabels` the writer tracks incrementally (`O(1)` per line), never
/// computed by walking the lines it describes. This is what
/// [`ChunkStore::head_candidates`] filters on for every query, so it must
/// stay `O(#containers)` regardless of how many unsealed lines exist.
#[derive(Debug, Clone)]
pub struct HeadSummary {
    pub labels: ChunkLabels,
}

/// A snapshot of one container's unsealed lines, oldest first, as immutable
/// segments. Cloning a [`HeadSnapshot`] out of the writer is `O(#segments)`
/// (`Arc` pointer clones) plus one clone of the writer's small in-progress
/// tail — never `O(#lines)`. See [`HeadSnapshot::iter`]/[`Self::iter_rev`]/
/// [`Self::get`] for reading it without flattening into a `Vec`.
#[derive(Debug, Clone)]
pub struct HeadSnapshot {
    pub identity: ChunkIdentity,
    pub segments: Vec<Arc<Vec<LogLine>>>,
}

impl HeadSnapshot {
    /// Total line count across every segment.
    pub fn len(&self) -> usize {
        self.segments.iter().map(|s| s.len()).sum()
    }

    pub fn is_empty(&self) -> bool {
        self.segments.iter().all(|s| s.is_empty())
    }

    /// The line at global index `i` (0 = oldest), or `None` if out of range.
    /// `O(#segments)`.
    pub fn get(&self, i: usize) -> Option<&LogLine> {
        let mut remaining = i;
        for segment in &self.segments {
            if remaining < segment.len() {
                return segment.get(remaining);
            }
            remaining -= segment.len();
        }
        None
    }

    /// Oldest-first `(global_index, line)` pairs.
    pub fn iter(&self) -> impl Iterator<Item = (usize, &LogLine)> {
        let mut offsets = Vec::with_capacity(self.segments.len());
        let mut acc = 0usize;
        for segment in &self.segments {
            offsets.push(acc);
            acc += segment.len();
        }
        self.segments
            .iter()
            .zip(offsets)
            .flat_map(|(segment, offset)| {
                segment
                    .iter()
                    .enumerate()
                    .map(move |(i, l)| (offset + i, l))
            })
    }

    /// Newest-first `(global_index, line)` pairs — what a "last N lines"
    /// scan wants, so it can stop as soon as `limit` matches are found
    /// without ever materialising the whole snapshot.
    pub fn iter_rev(&self) -> impl Iterator<Item = (usize, &LogLine)> {
        let mut offsets = Vec::with_capacity(self.segments.len());
        let mut acc = 0usize;
        for segment in &self.segments {
            offsets.push(acc);
            acc += segment.len();
        }
        self.segments
            .iter()
            .zip(offsets)
            .rev()
            .flat_map(|(segment, offset)| {
                segment
                    .iter()
                    .enumerate()
                    .rev()
                    .map(move |(i, l)| (offset + i, l))
            })
    }
}

/// Compute a [`ChunkLabels`] summary from an already-fetched [`HeadSnapshot`]
/// by walking its lines once. Used only by the rare single-container
/// `context`/`attach_context` paths, never by the per-query prefilter (which
/// uses [`HeadSource::summaries`] and touches no lines at all).
fn head_snapshot_labels(snapshot: &HeadSnapshot) -> Option<ChunkLabels> {
    let mut started_at: Option<DateTime<Utc>> = None;
    let mut ended_at: Option<DateTime<Utc>> = None;
    let mut level_mask = 0u16;
    let mut line_count = 0u32;
    let mut level_counts = [0u32; crate::chunk::LEVEL_COUNT];
    for segment in &snapshot.segments {
        for line in segment.iter() {
            started_at = Some(started_at.map_or(line.ts, |t| t.min(line.ts)));
            ended_at = Some(ended_at.map_or(line.ts, |t| t.max(line.ts)));
            level_mask |= level_bit(line.level);
            level_counts[crate::chunk::level_to_u8(line.level) as usize] += 1;
            line_count += 1;
        }
    }
    let identity = &snapshot.identity;
    Some(ChunkLabels {
        project_id: identity.project_id,
        external_service_id: identity.external_service_id,
        env: identity.env.clone(),
        service: identity.service.clone(),
        container_id: identity.container_id.clone(),
        deploy_id: identity.deploy_id,
        node_id: identity.node_id,
        node_name: identity.node_name.clone(),
        started_at: started_at?,
        ended_at: ended_at?,
        line_count,
        level_mask,
        level_counts,
    })
}

/// The writer's head buffers, seen by the planner as virtual newest chunks.
///
/// This is the *only* interface the reader shares with the writer (ADR-046
/// §8a.5). Implementations must return results consistent with each other
/// and never block ingest to do so.
///
/// [`Self::summaries`] must be `O(#containers)` and must not touch any
/// line — it is called on every `search`/`sources` query. [`Self::snapshot`]
/// may be `O(#lines)` for a *single* container (it is only called for a
/// candidate a query actually needs to scan, or for the rare
/// single-container `context` endpoint).
#[async_trait]
pub trait HeadSource: Send + Sync {
    /// A content-free summary of every container with unsealed lines.
    async fn summaries(&self) -> Vec<HeadSummary>;
    /// Unsealed lines of one container, if any.
    async fn snapshot(&self, container_id: &str) -> Option<HeadSnapshot>;
}

/// A head source with nothing in it — for tests and read-only processes.
pub struct NoHeads;

#[async_trait]
impl HeadSource for NoHeads {
    async fn summaries(&self) -> Vec<HeadSummary> {
        Vec::new()
    }
    async fn snapshot(&self, _container_id: &str) -> Option<HeadSnapshot> {
        None
    }
}

// ── Label matching (shared by heads and manifests) ──────────────────────

/// Does a stream's identity pass the query's label filters and allow-list?
///
/// The manifest SQL applies the same predicates for sealed chunks; heads are
/// filtered here in memory. Fail-closed on scope exactly like the SQL.
pub fn labels_match(labels: &ChunkLabels, query: &LogQuery) -> bool {
    let external = labels.external_service_id;
    let scope_ok = match &query.scope {
        LogAccessScope::All => true,
        LogAccessScope::Allowed {
            project_ids,
            external_service_ids,
        } => match external {
            Some(id) => external_service_ids.contains(&id),
            None => project_ids.contains(&labels.project_id),
        },
    };
    if !scope_ok {
        return false;
    }
    match query.source {
        LogSourceKind::Collected => {}
        LogSourceKind::Application if external.is_some() => return false,
        LogSourceKind::Service if external.is_none() => return false,
        _ => {}
    }
    if let Some(LogSelection {
        project_ids,
        external_service_ids,
    }) = &query.selection
    {
        let selected = match external {
            Some(id) => external_service_ids.contains(&id),
            None => project_ids.contains(&labels.project_id),
        };
        if !selected {
            return false;
        }
    }
    if !query.envs.is_empty() && !query.envs.contains(&labels.env) {
        return false;
    }
    if !query.services.is_empty() && !query.services.contains(&labels.service) {
        return false;
    }
    if !query.container_ids.is_empty() && !query.container_ids.contains(&labels.container_id) {
        return false;
    }
    if !query.node_ids.is_empty() && !labels.node_id.is_some_and(|n| query.node_ids.contains(&n)) {
        return false;
    }
    if let Some(deploy) = query.deploy_id {
        if labels.deploy_id != Some(deploy) {
            return false;
        }
    }
    true
}

// ── Candidates ──────────────────────────────────────────────────────────

/// One unit of planner work: a sealed chunk or a head buffer. A head
/// candidate carries only its (already-computed) [`ChunkLabels`] — the
/// actual lines are fetched lazily in [`ChunkStore::scan`] only for
/// candidates the walk actually visits.
enum Candidate {
    Sealed(Manifest),
    Head(ChunkLabels),
}

impl Candidate {
    fn ended_at(&self) -> DateTime<Utc> {
        match self {
            Candidate::Sealed(m) => m.labels.ended_at,
            Candidate::Head(l) => l.ended_at,
        }
    }
}

/// A match waiting to become final. Ordered newest-first by key.
struct Pending(LogLineRecord);

impl PartialEq for Pending {
    fn eq(&self, other: &Self) -> bool {
        self.0.key() == other.0.key()
    }
}
impl Eq for Pending {}
impl PartialOrd for Pending {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}
impl Ord for Pending {
    fn cmp(&self, other: &Self) -> Ordering {
        self.0.key().cmp(&other.0.key())
    }
}

/// The compiled, per-request form of a query's line-level filters.
struct LineFilter {
    start: DateTime<Utc>,
    end: DateTime<Utc>,
    level_mask: u16,
    /// Lowercased needle, if any.
    needle: Option<String>,
    /// `memmem` searcher over the lowercased needle when it is pure ASCII,
    /// so a line is tested by folding its bytes into a reusable scratch
    /// buffer instead of allocating a lowercased `String` per line.
    finder: Option<memchr::memmem::Finder<'static>>,
    /// Bloom entries the chunk must contain; empty ⇒ never prune.
    needle_entries: Vec<u64>,
    /// Only lines strictly older than this key.
    before: Option<LogLineKey>,
    /// Page size: no single chunk can contribute more than this many lines
    /// to a page, so a scan stops once it holds `limit` matches.
    limit: usize,
    /// Attribute predicates, all of which a line's `fields` must satisfy.
    attrs: Vec<AttrPredicate>,
}

impl LineFilter {
    fn compile(query: &LogQuery) -> Self {
        let needle = query
            .text
            .as_deref()
            .map(str::trim)
            .filter(|t| !t.is_empty())
            .map(str::to_lowercase);
        let needle_entries = needle
            .as_deref()
            .map(bloom::query_entries)
            .unwrap_or_default();
        // A cursor bounds the window from above: nothing newer than it can be
        // on this page, so the manifest overlap test can use it directly.
        let end = query
            .before
            .as_ref()
            .map_or(query.end_time, |b| query.end_time.min(b.timestamp));
        let finder = needle
            .as_deref()
            .filter(|n| n.is_ascii())
            .map(|n| memchr::memmem::Finder::new(n.as_bytes()).into_owned());
        Self {
            start: query.start_time,
            end,
            level_mask: level_mask_for(&query.levels),
            needle,
            finder,
            needle_entries,
            before: query.before.clone(),
            limit: query.limit.max(1) as usize,
            attrs: query.attrs.clone(),
        }
    }

    fn attrs_match(&self, fields: Option<&serde_json::Value>) -> bool {
        self.attrs.iter().all(|p| p.matches(fields))
    }

    fn block_filter(&self) -> BlockFilter {
        BlockFilter {
            start: Some(self.start),
            end: Some(self.end),
            level_mask: self.level_mask,
        }
    }

    fn text_matches(&self, message: &str, scratch: &mut Vec<u8>) -> bool {
        match (&self.finder, &self.needle) {
            (_, None) => true,
            (Some(finder), _) => {
                scratch.clear();
                scratch.extend(message.bytes().map(|b| b.to_ascii_lowercase()));
                finder.find(scratch).is_some()
            }
            (None, Some(n)) => message.to_lowercase().contains(n.as_str()),
        }
    }

    fn key_allowed(&self, key: &LogLineKey) -> bool {
        match &self.before {
            None => true,
            Some(b) => key < b,
        }
    }
}

// ── The store ───────────────────────────────────────────────────────────

/// See the module docs.
pub struct ChunkStore {
    manifests: ManifestRepo,
    storage: Arc<dyn LogStorage>,
    cache: ChunkCache,
    heads: Arc<dyn HeadSource>,
    budget: SearchBudget,
    /// ADR-047 line index, told to forget purged chunks.
    line_index: Arc<dyn crate::index::LineIndexSink>,
}

/// What one chunk read produced.
struct ChunkScan {
    matches: Vec<LogLineRecord>,
    bytes: u64,
}

impl ChunkStore {
    pub fn new(
        manifests: ManifestRepo,
        storage: Arc<dyn LogStorage>,
        cache: ChunkCache,
        heads: Arc<dyn HeadSource>,
    ) -> Self {
        Self {
            manifests,
            storage,
            cache,
            heads,
            budget: SearchBudget::default(),
            line_index: Arc::new(crate::index::NoLineIndex::default()),
        }
    }

    pub fn with_budget(mut self, budget: SearchBudget) -> Self {
        self.budget = budget;
        self
    }

    pub fn with_line_index(mut self, line_index: Arc<dyn crate::index::LineIndexSink>) -> Self {
        self.line_index = line_index;
        self
    }

    // ── Object reads (cache-through) ────────────────────────────────

    async fn read_range(
        &self,
        key: &str,
        start: u64,
        end: u64,
        tier: CacheTier,
    ) -> Result<Bytes, LogAggregatorError> {
        if let Some(hit) = self.cache.get(key, start, end).await {
            return Ok(hit);
        }
        let data = Bytes::from(self.storage.read_chunk_range(key, start, Some(end)).await?);
        self.cache.put(key, start, end, data.clone(), tier).await;
        Ok(data)
    }

    async fn read_whole(&self, key: &str) -> Result<Bytes, LogAggregatorError> {
        if let Some(hit) = self.cache.get(key, 0, u64::MAX).await {
            return Ok(hit);
        }
        let data = Bytes::from(self.storage.read_chunk(key).await?);
        self.cache
            .put(key, 0, u64::MAX, data.clone(), CacheTier::Block)
            .await;
        Ok(data)
    }

    /// Fetch and decode a v2 chunk's footer. Small footers (no bloom, or a
    /// tiny one) are pinned in the index tier; large ones live in the bloom
    /// tier so they evict before indexes but after blocks.
    async fn footer(&self, m: &Manifest) -> Result<ChunkFooter, LogAggregatorError> {
        let (Some(off), Some(len)) = (m.footer_offset, m.footer_len) else {
            return Err(LogAggregatorError::ChunkFormat {
                reason: format!("manifest {} has no footer location", m.id),
            });
        };
        let tier = if m.bloom_len > 16 * 1024 {
            CacheTier::Bloom
        } else {
            CacheTier::Index
        };
        let bytes = self
            .read_range(&m.storage_key, off, off + u64::from(len), tier)
            .await?;
        let trailer = crate::chunk::format::decode_trailer(&bytes)?;
        decode_footer(&bytes, &trailer)
    }

    // ── Scanning one candidate ──────────────────────────────────────

    fn record(labels: &ChunkLabels, line: DecodedLine, line_id: i64) -> LogLineRecord {
        LogLineRecord {
            timestamp: line.ts,
            project_id: if labels.external_service_id.is_some() {
                None
            } else {
                Some(labels.project_id)
            },
            external_service_id: labels.external_service_id,
            env: labels.env.clone(),
            service: labels.service.clone(),
            level: line.level,
            stream: line.stream,
            container_id: labels.container_id.clone(),
            node_id: labels.node_id,
            node_name: labels.node_name.clone(),
            deploy_id: labels.deploy_id,
            message: line.message,
            fields: line.fields,
            line_id,
            context: None,
        }
    }

    fn head_line(line: &LogLine, index: usize) -> DecodedLine {
        DecodedLine {
            ts: line.ts,
            level: line.level,
            stream: line.stream,
            message: line.msg.clone(),
            fields: line.fields.clone(),
            line_index: index as u32,
        }
    }

    fn scan_head(snapshot: &HeadSnapshot, labels: &ChunkLabels, filter: &LineFilter) -> ChunkScan {
        let mut matches = Vec::new();
        let mut bytes = 0u64;
        let mut scratch = Vec::new();
        // Newest first: a head is in arrival order, and once `limit` lines
        // match, nothing older in this stream can be on the page. `iter_rev`
        // walks segments back-to-front so this never flattens the snapshot.
        for (i, line) in snapshot.iter_rev() {
            if matches.len() >= filter.limit {
                break;
            }
            bytes += line.msg.len() as u64;
            if line.ts < filter.start || line.ts > filter.end {
                continue;
            }
            if filter.level_mask & level_bit(line.level) == 0 {
                continue;
            }
            if !filter.text_matches(&line.msg, &mut scratch)
                || !filter.attrs_match(line.fields.as_ref())
            {
                continue;
            }
            let line_id = HEAD_LINE_ID_BASE | i as i64;
            let key = LogLineKey {
                timestamp: line.ts,
                container_id: labels.container_id.clone(),
                line_id,
            };
            if !filter.key_allowed(&key) {
                continue;
            }
            matches.push(Self::record(labels, Self::head_line(line, i), line_id));
        }
        ChunkScan { matches, bytes }
    }

    async fn scan_sealed(
        &self,
        m: &Manifest,
        filter: &LineFilter,
    ) -> Result<ChunkScan, LogAggregatorError> {
        let mut matches: Vec<LogLineRecord> = Vec::new();
        let mut bytes = 0u64;
        let mut scratch: Vec<u8> = Vec::new();
        let mut accept = |line: &DecodedLine| -> Option<i64> {
            if line.ts < filter.start || line.ts > filter.end {
                return None;
            }
            if filter.level_mask & level_bit(line.level) == 0 {
                return None;
            }
            let line_id = LogLineKey::sealed_line_id(m.seq, line.line_index);
            let key = LogLineKey {
                timestamp: line.ts,
                container_id: m.labels.container_id.clone(),
                line_id,
            };
            if !filter.key_allowed(&key)
                || !filter.text_matches(&line.message, &mut scratch)
                || !filter.attrs_match(line.fields.as_ref())
            {
                return None;
            }
            Some(line_id)
        };
        // Keep only the newest `limit` matches of a block/chunk: lines in a
        // block are time-ordered, so the tail of the accepted list is the
        // newest. Blocks are visited newest-first, so once a chunk holds
        // `limit` matches every remaining block is older and can be skipped.
        let keep_newest = |v: &mut Vec<LogLineRecord>, limit: usize| {
            if v.len() > limit {
                v.drain(..v.len() - limit);
            }
        };

        if m.format_version < 2 {
            // Legacy single-frame object: bounded because v1 sealed at 1 MB.
            let raw = self.read_whole(&m.storage_key).await?;
            let lines = decode_v1(&raw)?;
            bytes += lines.iter().map(|l| l.message.len() as u64).sum::<u64>();
            for line in lines {
                if let Some(id) = accept(&line) {
                    matches.push(Self::record(&m.labels, line, id));
                }
            }
            keep_newest(&mut matches, filter.limit);
            return Ok(ChunkScan { matches, bytes });
        }

        let footer = self.footer(m).await?;
        if !filter.needle_entries.is_empty() {
            if let Some(bloom) = &footer.bloom {
                if !bloom.contains_all(&filter.needle_entries) {
                    return Ok(ChunkScan { matches, bytes });
                }
            }
        }
        let block_filter = filter.block_filter();
        // Newest block first so a `limit` that fills early reads the least.
        // `matches` is assembled oldest-first per block; blocks are prepended
        // so the whole vector stays oldest-first for `keep_newest`.
        for meta in footer.blocks.iter().rev() {
            if matches.len() >= filter.limit {
                break;
            }
            if !meta.may_match(filter.start, filter.end, filter.level_mask) {
                continue;
            }
            let compressed = self
                .read_range(
                    &m.storage_key,
                    meta.offset,
                    meta.offset + u64::from(meta.len),
                    CacheTier::Block,
                )
                .await?;
            bytes += u64::from(meta.uncompressed_len);
            let mut block_matches: Vec<LogLineRecord> = Vec::new();
            for line in decode_block(&compressed, meta, &block_filter)? {
                if let Some(id) = accept(&line) {
                    block_matches.push(Self::record(&m.labels, line, id));
                }
            }
            keep_newest(&mut block_matches, filter.limit);
            block_matches.append(&mut matches);
            matches = block_matches;
        }
        keep_newest(&mut matches, filter.limit);
        Ok(ChunkScan { matches, bytes })
    }

    async fn scan(
        &self,
        c: &Candidate,
        filter: &LineFilter,
    ) -> Result<ChunkScan, LogAggregatorError> {
        match c {
            // The snapshot is fetched lazily, here, only for a candidate the
            // walk actually visits. A `None` means the buffer sealed between
            // the prefilter and this scan; its lines are already on their
            // way into (or already in) a manifest row, so an empty scan here
            // is correct — the sealed-chunk walk will find them.
            Candidate::Head(labels) => match self.heads.snapshot(&labels.container_id).await {
                Some(snapshot) => Ok(Self::scan_head(&snapshot, labels, filter)),
                None => Ok(ChunkScan {
                    matches: Vec::new(),
                    bytes: 0,
                }),
            },
            Candidate::Sealed(m) => self.scan_sealed(m, filter).await,
        }
    }

    // ── Candidate stream ────────────────────────────────────────────

    /// Head buffers that pass the label filters and overlap the window, as
    /// virtual chunks, newest `ended_at` first. Built from
    /// [`HeadSource::summaries`] alone — no line is touched here, so this
    /// stays cheap regardless of how many lines are buffered.
    async fn head_candidates(&self, query: &LogQuery, filter: &LineFilter) -> Vec<Candidate> {
        let mut out: Vec<Candidate> = self
            .heads
            .summaries()
            .await
            .into_iter()
            .filter_map(|summary| {
                let labels = summary.labels;
                if !labels_match(&labels, query) {
                    return None;
                }
                if labels.ended_at < filter.start || labels.started_at > filter.end {
                    return None;
                }
                if labels.level_mask & filter.level_mask == 0 {
                    return None;
                }
                Some(Candidate::Head(labels))
            })
            .collect();
        out.sort_by_key(|c| std::cmp::Reverse(c.ended_at()));
        out
    }

    /// Next batch of sealed candidates after `after`, in planner order.
    async fn sealed_batch(
        &self,
        query: &LogQuery,
        filter: &LineFilter,
        after: Option<&ManifestCursor>,
    ) -> Result<Vec<Manifest>, LogAggregatorError> {
        let mut bounded = query.clone();
        bounded.end_time = filter.end;
        self.manifests
            .candidates(&bounded, after, self.budget.batch)
            .await
    }
}

/// Merge-walk state for one `search` call.
struct Walk {
    heads: std::collections::VecDeque<Candidate>,
    sealed: std::collections::VecDeque<Manifest>,
    sealed_exhausted: bool,
    sealed_cursor: Option<ManifestCursor>,
}

impl Walk {
    /// `ended_at` of the newest unprocessed candidate, or `None` when the
    /// walk is complete. Used by the stop rule.
    fn next_ended_at(&self) -> Option<DateTime<Utc>> {
        let h = self.heads.front().map(Candidate::ended_at);
        let s = self.sealed.front().map(|m| m.labels.ended_at);
        match (h, s) {
            (Some(a), Some(b)) => Some(a.max(b)),
            (a, b) => a.or(b),
        }
    }

    /// Pop the next candidate in `ended_at DESC` order across both queues.
    fn pop(&mut self) -> Option<Candidate> {
        let take_head = match (self.heads.front(), self.sealed.front()) {
            (Some(h), Some(s)) => h.ended_at() >= s.labels.ended_at,
            (Some(_), None) => true,
            (None, _) => false,
        };
        if take_head {
            self.heads.pop_front()
        } else {
            self.sealed.pop_front().map(|m| {
                self.sealed_cursor = Some(ManifestCursor {
                    ended_at: m.labels.ended_at,
                    id: m.id,
                });
                Candidate::Sealed(m)
            })
        }
    }
}

/// The cursor that resumes a partial page: everything with `ts ≤ E` is still
/// to be searched. `"\u{FFFF}"` sorts after every real container id.
fn resume_cursor(e: DateTime<Utc>) -> LogLineKey {
    LogLineKey {
        timestamp: e,
        container_id: "\u{FFFF}".to_string(),
        line_id: i64::MAX,
    }
}

#[async_trait]
impl LogLineStore for ChunkStore {
    async fn search(&self, query: &LogQuery) -> Result<LogPage, LogAggregatorError> {
        query.validate()?;
        let started = Instant::now();
        let filter = LineFilter::compile(query);
        let limit = query.limit as usize;

        let mut walk = Walk {
            heads: self.head_candidates(query, &filter).await.into(),
            sealed: std::collections::VecDeque::new(),
            sealed_exhausted: false,
            sealed_cursor: None,
        };
        let mut pending: BinaryHeap<Pending> = BinaryHeap::new();
        let mut finals: Vec<LogLineRecord> = Vec::with_capacity(limit);
        let mut bytes_used = 0u64;
        let mut chunks_read = 0usize;

        loop {
            // Refill the sealed queue so `next_ended_at` can always peek one
            // batch ahead (the stop rule needs the *newest unprocessed*).
            if walk.sealed.is_empty() && !walk.sealed_exhausted {
                let batch = self
                    .sealed_batch(query, &filter, walk.sealed_cursor.as_ref())
                    .await?;
                if batch.len() < self.budget.batch as usize {
                    walk.sealed_exhausted = true;
                }
                walk.sealed.extend(batch);
            }

            // Promote pending matches that no unread candidate can precede.
            let horizon = walk.next_ended_at();
            while let Some(top) = pending.peek() {
                let is_final = match horizon {
                    None => true,
                    Some(h) => top.0.timestamp > h,
                };
                if !is_final {
                    break;
                }
                let Some(Pending(rec)) = pending.pop() else {
                    break;
                };
                finals.push(rec);
                if finals.len() >= limit {
                    break;
                }
            }

            if finals.len() >= limit {
                finals.truncate(limit);
                let next_cursor = finals.last().map(LogLineRecord::key);
                debug!(
                    chunks_read,
                    bytes_used,
                    ms = started.elapsed().as_millis() as u64,
                    "log search page complete"
                );
                return Ok(LogPage {
                    lines: finals,
                    next_cursor,
                    scanned_back_to: None,
                });
            }

            if horizon.is_none() {
                // Nothing left anywhere: complete last page.
                let next_cursor = None;
                debug!(
                    chunks_read,
                    bytes_used,
                    ms = started.elapsed().as_millis() as u64,
                    "log search exhausted window"
                );
                return Ok(LogPage {
                    lines: finals,
                    next_cursor,
                    scanned_back_to: None,
                });
            }

            // Budget check — only between chunks, and only once the walk
            // has made progress: the resume point is the horizon (newest
            // unprocessed `ended_at`), and it must be strictly older than
            // this page's window end or the next page would re-select the
            // same chunks and never advance. Every pending line newer than
            // the horizon has already been promoted above, so what remains
            // pending is exactly what the resumed search will find again.
            let over_budget =
                started.elapsed() >= self.budget.time || bytes_used >= self.budget.bytes;
            if over_budget {
                if let Some(h) = horizon.filter(|h| *h < filter.end) {
                    debug!(chunks_read, bytes_used, scanned_back_to = %h, "log search budget exhausted; returning partial page");
                    return Ok(LogPage {
                        lines: finals,
                        next_cursor: Some(resume_cursor(h)),
                        scanned_back_to: Some(h),
                    });
                }
            }

            // Take up to `parallel` candidates, in order, and scan them
            // concurrently. Finality is re-evaluated after the whole batch,
            // which is conservative and therefore correct.
            let mut batch: Vec<Candidate> = Vec::with_capacity(self.budget.parallel);
            while batch.len() < self.budget.parallel {
                match walk.pop() {
                    Some(c) => batch.push(c),
                    None => break,
                }
            }
            if batch.is_empty() {
                continue;
            }
            let filter_ref = &filter;
            let futures: Vec<_> = batch
                .iter()
                .map(|c| async move { (c.ended_at(), self.scan(c, filter_ref).await) })
                .collect();
            let results: Vec<(DateTime<Utc>, Result<ChunkScan, LogAggregatorError>)> =
                stream::iter(futures)
                    .buffered(self.budget.parallel)
                    .collect()
                    .await;
            for (_ended_at, result) in results {
                chunks_read += 1;
                match result {
                    Ok(scan) => {
                        bytes_used += scan.bytes;
                        pending.extend(scan.matches.into_iter().map(Pending));
                    }
                    Err(LogAggregatorError::ChunkNotFound { storage_key, .. }) => {
                        // Object gone but manifest alive (GC race or lost
                        // object). Skip rather than fail the whole page; the
                        // reconcile sweep tombstones it.
                        warn!(storage_key, "log chunk object missing; skipping");
                    }
                    Err(e) => return Err(e),
                }
            }
        }
    }

    async fn facets(
        &self,
        query: &LogQuery,
        fields: &[FacetField],
    ) -> Result<FacetResult, LogAggregatorError> {
        query.validate()?;
        self.manifests.facets(query, fields).await
    }

    async fn context(
        &self,
        scope: &LogAccessScope,
        key: &LogLineKey,
        before: u32,
        after: u32,
    ) -> Result<Vec<LogLineRecord>, LogAggregatorError> {
        let before = before.min(500) as usize;
        let after = after.min(500) as usize;

        // A `line_id` names a position that can move: a head line's id
        // changes when its buffer seals, and a sealed line's chunk can be
        // compacted into a new one. `(container_id, timestamp)` is the
        // stable part of the key, so when the id's home no longer exists
        // the line is found again by that.
        let (m, all, idx) = match key.chunk_position() {
            None => match self.head_context(scope, key, before, after).await? {
                Some(out) => return Ok(out),
                None => match self.relocate(scope, key).await? {
                    Some(found) => found,
                    None => return Ok(Vec::new()),
                },
            },
            Some((seq, line_index)) => match self.manifests.by_seq(scope, seq).await? {
                Some(m) => {
                    let all = self.decode_all(&m).await?;
                    let idx = all
                        .iter()
                        .position(|l| l.line_index >= line_index)
                        .unwrap_or(all.len().saturating_sub(1));
                    (m, all, idx)
                }
                None => match self.relocate(scope, key).await? {
                    Some(found) => found,
                    None => return Ok(Vec::new()),
                },
            },
        };
        if all.is_empty() {
            return Ok(Vec::new());
        }
        let lo = idx.saturating_sub(before);
        let hi = (idx + after + 1).min(all.len());
        let mut out: Vec<LogLineRecord> = all[lo..hi]
            .iter()
            .cloned()
            .map(|l| {
                let id = LogLineKey::sealed_line_id(m.seq, l.line_index);
                Self::record(&m.labels, l, id)
            })
            .collect();

        // Cross the chunk edge when the window wasn't satisfied inside it.
        let missing_before = before.saturating_sub(idx);
        let missing_after = after.saturating_sub(all.len() - 1 - idx);
        if missing_before > 0 || missing_after > 0 {
            let (older, newer) = self
                .manifests
                .around(scope, &m.labels.container_id, key.timestamp, 1)
                .await?;
            if missing_before > 0 {
                if let Some(prev) = older.iter().find(|p| p.id != m.id) {
                    let lines = self.decode_all(prev).await?;
                    let take = lines.len().saturating_sub(missing_before);
                    let mut head: Vec<LogLineRecord> = lines[take..]
                        .iter()
                        .cloned()
                        .map(|l| {
                            let id = LogLineKey::sealed_line_id(prev.seq, l.line_index);
                            Self::record(&prev.labels, l, id)
                        })
                        .collect();
                    head.append(&mut out);
                    out = head;
                }
            }
            if missing_after > 0 {
                if let Some(next) = newer.iter().find(|n| n.id != m.id) {
                    let lines = self.decode_all(next).await?;
                    out.extend(lines.into_iter().take(missing_after).map(|l| {
                        let id = LogLineKey::sealed_line_id(next.seq, l.line_index);
                        Self::record(&next.labels, l, id)
                    }));
                }
            }
        }
        Ok(out)
    }

    async fn sources(&self, query: &LogQuery) -> Result<Vec<LogSource>, LogAggregatorError> {
        query.validate()?;
        let mut out = self.manifests.sources(query).await?;
        let mut seen: BTreeSet<String> = out.iter().map(|s| s.container_id.clone()).collect();
        for summary in self.heads.summaries().await {
            let labels = summary.labels;
            if !labels_match(&labels, query) || seen.contains(&labels.container_id) {
                continue;
            }
            if labels.ended_at < query.start_time || labels.started_at > query.end_time {
                continue;
            }
            seen.insert(labels.container_id.clone());
            out.push(LogSource {
                container_id: labels.container_id,
                service: labels.service,
                node_id: labels.node_id,
                node_name: labels.node_name,
            });
        }
        Ok(out)
    }

    async fn latest_timestamp_for_container(
        &self,
        container_id: &str,
    ) -> Result<Option<DateTime<Utc>>, LogAggregatorError> {
        let sealed = self.manifests.latest_ended_at(container_id).await?;
        let head = self
            .heads
            .summaries()
            .await
            .into_iter()
            .find(|s| s.labels.container_id == container_id)
            .map(|s| s.labels.ended_at);
        Ok(match (sealed, head) {
            (Some(a), Some(b)) => Some(a.max(b)),
            (a, b) => a.or(b),
        })
    }

    async fn purge_project(
        &self,
        project_id: i32,
        before: DateTime<Utc>,
    ) -> Result<u64, LogAggregatorError> {
        let (lines, seqs) = self.manifests.purge_project(project_id, before).await?;
        if let Err(e) = self.line_index.forget_chunks(&seqs).await {
            tracing::warn!(project_id, chunks = seqs.len(), error = %e, "line index forget after purge failed; rows age out by TTL");
        }
        Ok(lines)
    }

    /// Resolve line-index pointers to full records. Chunks are fetched
    /// concurrently (bounded like a search scan) and only the blocks that
    /// hold a wanted `line_index` are read and decoded — a hit-list spread
    /// across many chunks costs one block per hit, not one chunk decode.
    async fn lines_by_position(
        &self,
        scope: &LogAccessScope,
        positions: &[(i64, u32)],
    ) -> Result<Vec<LogLineRecord>, LogAggregatorError> {
        let mut by_seq: HashMap<i64, BTreeSet<u32>> = HashMap::new();
        for &(seq, line_index) in positions {
            by_seq.entry(seq).or_default().insert(line_index);
        }

        let resolved: Vec<Result<Vec<((i64, u32), LogLineRecord)>, LogAggregatorError>> =
            stream::iter(by_seq)
                .map(|(seq, wanted)| async move {
                    let Some(m) = self.manifests.by_seq(scope, seq).await? else {
                        // Not visible under this scope, or gone — drop
                        // silently (the pointer is from an index row the
                        // caller cannot, or can no longer, resolve).
                        return Ok(Vec::new());
                    };
                    match self.decode_positions(&m, &wanted).await {
                        Ok(lines) => Ok(lines
                            .into_iter()
                            .map(|l| {
                                let id = LogLineKey::sealed_line_id(m.seq, l.line_index);
                                ((seq, l.line_index), Self::record(&m.labels, l, id))
                            })
                            .collect()),
                        Err(LogAggregatorError::ChunkNotFound { storage_key, .. }) => {
                            warn!(
                                storage_key,
                                seq, "log chunk object missing for indexed pointer; skipping"
                            );
                            Ok(Vec::new())
                        }
                        Err(e) => Err(e),
                    }
                })
                .buffer_unordered(self.budget.parallel)
                .collect()
                .await;

        let mut found: HashMap<(i64, u32), LogLineRecord> = HashMap::new();
        for r in resolved {
            found.extend(r?);
        }
        Ok(positions
            .iter()
            .filter_map(|pos| found.get(pos).cloned())
            .collect())
    }

    /// Attach `grep -C`-style neighbours to each match of a page, reading
    /// from the same chunks (neighbours are raw lines, unfiltered).
    async fn attach_context(
        &self,
        scope: &LogAccessScope,
        lines: &mut [LogLineRecord],
        n: u32,
    ) -> Result<(), LogAggregatorError> {
        if n == 0 {
            return Ok(());
        }
        let n = n.min(50);
        // Group by chunk so each chunk is decoded once per page.
        let mut by_chunk: HashMap<Option<i64>, Vec<usize>> = HashMap::new();
        for (i, l) in lines.iter().enumerate() {
            by_chunk
                .entry(l.key().chunk_position().map(|(seq, _)| seq))
                .or_default()
                .push(i);
        }
        for (seq, idxs) in by_chunk {
            let all: Vec<(DecodedLine, i64)> = match seq {
                Some(seq) => {
                    let Some(m) = self.manifests.by_seq(scope, seq).await? else {
                        continue;
                    };
                    self.decode_all(&m)
                        .await?
                        .into_iter()
                        .map(|l| {
                            let id = LogLineKey::sealed_line_id(m.seq, l.line_index);
                            (l, id)
                        })
                        .collect()
                }
                None => {
                    let container = &lines[idxs[0]].container_id;
                    let Some(snap) = self.heads.snapshot(container).await else {
                        continue;
                    };
                    snap.iter()
                        .map(|(i, l)| (Self::head_line(l, i), HEAD_LINE_ID_BASE | i as i64))
                        .collect()
                }
            };
            for i in idxs {
                let target = lines[i].line_id;
                let Some(pos) = all.iter().position(|(_, id)| *id == target) else {
                    continue;
                };
                let lo = pos.saturating_sub(n as usize);
                let hi = (pos + n as usize + 1).min(all.len());
                let ctx = |(l, id): &(DecodedLine, i64), matched: bool| ContextLine {
                    timestamp: l.ts,
                    level: l.level,
                    message: l.message.clone(),
                    fields: l.fields.clone(),
                    line_id: id.to_string(),
                    is_match: matched,
                };
                lines[i].context = Some(LineContext {
                    before: all[lo..pos].iter().map(|x| ctx(x, false)).collect(),
                    after: all[pos + 1..hi].iter().map(|x| ctx(x, false)).collect(),
                });
            }
        }
        Ok(())
    }
}

impl ChunkStore {
    /// Context for a line still in its container's head buffer, or `None`
    /// when the buffer is gone (sealed since the key was issued) or does
    /// not hold that instant.
    async fn head_context(
        &self,
        scope: &LogAccessScope,
        key: &LogLineKey,
        before: usize,
        after: usize,
    ) -> Result<Option<Vec<LogLineRecord>>, LogAggregatorError> {
        let Some(snap) = self.heads.snapshot(&key.container_id).await else {
            return Ok(None);
        };
        let Some(labels) = head_snapshot_labels(&snap) else {
            return Ok(None);
        };
        let mut probe = LogQuery::for_scope(scope.clone());
        probe.container_ids = vec![key.container_id.clone()];
        if !labels_match(&labels, &probe) {
            return Ok(Some(Vec::new()));
        }
        if labels.started_at > key.timestamp {
            // Sealed out from under the key: the line is in a chunk now.
            return Ok(None);
        }
        // Position by timestamp (the stable part of a head key).
        let len = snap.len();
        let idx = snap
            .iter()
            .find(|(_, l)| l.ts >= key.timestamp)
            .map(|(i, _)| i)
            .unwrap_or(len.saturating_sub(1));
        let lo = idx.saturating_sub(before);
        let hi = (idx + after + 1).min(len);
        let mut out = Vec::with_capacity(hi.saturating_sub(lo));
        for i in lo..hi {
            if let Some(l) = snap.get(i) {
                out.push(Self::record(
                    &labels,
                    Self::head_line(l, i),
                    HEAD_LINE_ID_BASE | i as i64,
                ));
            }
        }
        Ok(Some(out))
    }

    /// Find the chunk that now holds `key`'s instant for its container and
    /// the position of the first line at or after that timestamp.
    async fn relocate(
        &self,
        scope: &LogAccessScope,
        key: &LogLineKey,
    ) -> Result<Option<(Manifest, Vec<DecodedLine>, usize)>, LogAggregatorError> {
        let Some(m) = self
            .manifests
            .containing(scope, &key.container_id, key.timestamp)
            .await?
        else {
            return Ok(None);
        };
        let all = self.decode_all(&m).await?;
        let idx = all
            .iter()
            .position(|l| l.ts >= key.timestamp)
            .unwrap_or(all.len().saturating_sub(1));
        Ok(Some((m, all, idx)))
    }

    /// Every line of a chunk, oldest first, unfiltered. Used by `context`.
    async fn decode_all(&self, m: &Manifest) -> Result<Vec<DecodedLine>, LogAggregatorError> {
        if m.format_version < 2 {
            let raw = self.read_whole(&m.storage_key).await?;
            return decode_v1(&raw);
        }
        let footer = self.footer(m).await?;
        let mut out = Vec::with_capacity(m.line_count as usize);
        let unfiltered = BlockFilter::default();
        for meta in &footer.blocks {
            let compressed = self
                .read_range(
                    &m.storage_key,
                    meta.offset,
                    meta.offset + u64::from(meta.len),
                    CacheTier::Block,
                )
                .await?;
            out.extend(decode_block(&compressed, meta, &unfiltered)?);
        }
        Ok(out)
    }

    /// The lines of a chunk at the given `line_index`es, oldest first,
    /// touching only the blocks that contain one of them.
    async fn decode_positions(
        &self,
        m: &Manifest,
        wanted: &BTreeSet<u32>,
    ) -> Result<Vec<DecodedLine>, LogAggregatorError> {
        if m.format_version < 2 {
            let raw = self.read_whole(&m.storage_key).await?;
            return Ok(decode_v1(&raw)?
                .into_iter()
                .filter(|l| wanted.contains(&l.line_index))
                .collect());
        }
        let footer = self.footer(m).await?;
        let unfiltered = BlockFilter::default();
        let mut out = Vec::with_capacity(wanted.len());
        for meta in &footer.blocks {
            let lo = meta.first_line_index;
            let hi = lo + meta.line_count;
            if wanted.range(lo..hi).next().is_none() {
                continue;
            }
            let compressed = self
                .read_range(
                    &m.storage_key,
                    meta.offset,
                    meta.offset + u64::from(meta.len),
                    CacheTier::Block,
                )
                .await?;
            out.extend(
                decode_block(&compressed, meta, &unfiltered)?
                    .into_iter()
                    .filter(|l| wanted.contains(&l.line_index)),
            );
        }
        Ok(out)
    }
}

impl LogQuery {
    /// A query that matches everything the scope allows over all time —
    /// used internally to reuse [`labels_match`] for single-container checks.
    pub(crate) fn for_scope(scope: LogAccessScope) -> Self {
        Self {
            scope,
            start_time: DateTime::<Utc>::MIN_UTC,
            end_time: DateTime::<Utc>::MAX_UTC,
            source: LogSourceKind::Collected,
            selection: None,
            levels: vec![],
            envs: vec![],
            services: vec![],
            container_ids: vec![],
            node_ids: vec![],
            deploy_id: None,
            text: None,
            before: None,
            limit: 1,
            context_lines: 0,
            attrs: Vec::new(),
            chunk_seqs: None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::chunk::LEVEL_MASK_ALL;
    use crate::types::{LogLevel, LogStream};
    use std::time::Duration;

    fn labels(project: i32, external: Option<i32>) -> ChunkLabels {
        ChunkLabels {
            project_id: project,
            external_service_id: external,
            env: "prod".into(),
            service: "api".into(),
            container_id: "c1".into(),
            deploy_id: Some(9),
            node_id: None,
            node_name: None,
            started_at: "2026-01-01T00:00:00Z".parse().unwrap(),
            ended_at: "2026-01-01T01:00:00Z".parse().unwrap(),
            line_count: 1,
            level_mask: LEVEL_MASK_ALL,
            level_counts: [0; 5],
        }
    }

    fn query(scope: LogAccessScope) -> LogQuery {
        LogQuery::for_scope(scope)
    }

    #[test]
    fn empty_allow_list_matches_nothing() {
        let q = query(LogAccessScope::Allowed {
            project_ids: vec![],
            external_service_ids: vec![],
        });
        assert!(!labels_match(&labels(1, None), &q));
        assert!(!labels_match(&labels(0, Some(3)), &q));
    }

    #[test]
    fn allow_list_admits_by_project_or_external_service() {
        let q = query(LogAccessScope::Allowed {
            project_ids: vec![1],
            external_service_ids: vec![3],
        });
        assert!(labels_match(&labels(1, None), &q));
        assert!(!labels_match(&labels(2, None), &q));
        assert!(labels_match(&labels(0, Some(3)), &q));
        assert!(!labels_match(&labels(0, Some(4)), &q));
    }

    #[test]
    fn selection_narrows_within_scope_and_empty_selection_is_nothing() {
        let mut q = query(LogAccessScope::All);
        q.selection = Some(LogSelection {
            project_ids: vec![],
            external_service_ids: vec![],
        });
        assert!(!labels_match(&labels(1, None), &q));
        q.selection = Some(LogSelection {
            project_ids: vec![1],
            external_service_ids: vec![],
        });
        assert!(labels_match(&labels(1, None), &q));
        assert!(!labels_match(&labels(0, Some(3)), &q));
    }

    #[test]
    fn source_kind_and_label_filters_apply() {
        let mut q = query(LogAccessScope::All);
        q.source = LogSourceKind::Application;
        assert!(labels_match(&labels(1, None), &q));
        assert!(!labels_match(&labels(0, Some(3)), &q));
        q.source = LogSourceKind::Service;
        assert!(!labels_match(&labels(1, None), &q));

        let mut q = query(LogAccessScope::All);
        q.envs = vec!["staging".into()];
        assert!(!labels_match(&labels(1, None), &q));
        q.envs = vec!["prod".into()];
        q.deploy_id = Some(9);
        assert!(labels_match(&labels(1, None), &q));
        q.deploy_id = Some(10);
        assert!(!labels_match(&labels(1, None), &q));
    }

    #[test]
    fn resume_cursor_sorts_after_every_real_key_at_that_instant() {
        let e: DateTime<Utc> = "2026-01-01T00:00:00Z".parse().unwrap();
        let cursor = resume_cursor(e);
        let real = LogLineKey {
            timestamp: e,
            container_id: "ffffffffffff".into(),
            line_id: i64::MAX - 1,
        };
        assert!(
            real < cursor,
            "a line at exactly E must be re-found next page"
        );
        let newer = LogLineKey {
            timestamp: e + chrono::Duration::nanoseconds(1),
            container_id: "0".into(),
            line_id: 0,
        };
        assert!(
            newer > cursor,
            "a line after E was final and must not repeat"
        );
    }

    #[test]
    fn line_filter_uses_cursor_to_bound_the_window() {
        let mut q = query(LogAccessScope::All);
        q.start_time = "2026-01-01T00:00:00Z".parse().unwrap();
        q.end_time = "2026-01-02T00:00:00Z".parse().unwrap();
        q.before = Some(LogLineKey {
            timestamp: "2026-01-01T12:00:00Z".parse().unwrap(),
            container_id: "c".into(),
            line_id: 0,
        });
        let f = LineFilter::compile(&q);
        assert_eq!(f.end, q.before.as_ref().unwrap().timestamp);
    }

    #[test]
    fn head_scan_applies_every_filter_and_head_ids() {
        let ts: DateTime<Utc> = "2026-01-01T00:30:00Z".parse().unwrap();
        let mk = |msg: &str, level: LogLevel, offset: i64| LogLine {
            ts: ts + chrono::Duration::seconds(offset),
            stream: LogStream::Stdout,
            level,
            msg: msg.into(),
            fields: None,
            container_id: "c1".into(),
            service: "api".into(),
            env: "prod".into(),
            project_id: 1,
            external_service_id: None,
            deploy_id: Some(9),
            node_id: None,
            node_name: None,
        };
        let identity = ChunkIdentity {
            project_id: 1,
            external_service_id: None,
            env: "prod".into(),
            service: "api".into(),
            container_id: "c1".into(),
            deploy_id: Some(9),
            node_id: None,
            node_name: None,
        };
        let snap = HeadSnapshot {
            identity: identity.clone(),
            segments: vec![Arc::new(vec![
                mk("boot ok", LogLevel::Info, 0),
                mk("Connection refused", LogLevel::Error, 1),
                mk("retrying", LogLevel::Warn, 2),
            ])],
        };
        let labels = head_snapshot_labels(&snap).unwrap();
        let mut q = query(LogAccessScope::All);
        q.start_time = ts - chrono::Duration::hours(1);
        q.end_time = ts + chrono::Duration::hours(1);
        q.levels = vec![LogLevel::Error];
        q.text = Some("REFUSED".into());
        let scan = ChunkStore::scan_head(&snap, &labels, &LineFilter::compile(&q));
        assert_eq!(scan.matches.len(), 1);
        let m = &scan.matches[0];
        assert_eq!(m.message, "Connection refused");
        assert!(m.line_id >= HEAD_LINE_ID_BASE);
        assert_eq!(m.key().chunk_position(), None);
    }

    #[test]
    fn head_snapshot_reads_are_o1_ish_across_segments() {
        // Three segments simulating what the writer freezes at 1024
        // lines/256 KiB: exercise `len`, `get`, `iter`, `iter_rev` and
        // `scan_head`'s reverse walk across segment boundaries.
        let ts: DateTime<Utc> = "2026-01-01T00:00:00Z".parse().unwrap();
        let mk = |msg: String, offset: i64| LogLine {
            ts: ts + chrono::Duration::seconds(offset),
            stream: LogStream::Stdout,
            level: LogLevel::Info,
            msg,
            fields: None,
            container_id: "multi".into(),
            service: "api".into(),
            env: "prod".into(),
            project_id: 1,
            external_service_id: None,
            deploy_id: None,
            node_id: None,
            node_name: None,
        };
        let identity = ChunkIdentity {
            project_id: 1,
            external_service_id: None,
            env: "prod".into(),
            service: "api".into(),
            container_id: "multi".into(),
            deploy_id: None,
            node_id: None,
            node_name: None,
        };

        let mut offset = 0i64;
        let segments: Vec<Arc<Vec<LogLine>>> = (0..3)
            .map(|seg| {
                let lines: Vec<LogLine> = (0..10)
                    .map(|i| {
                        let line = mk(format!("seg{seg}-line{i}"), offset);
                        offset += 1;
                        line
                    })
                    .collect();
                Arc::new(lines)
            })
            .collect();
        let snap = HeadSnapshot { identity, segments };

        assert_eq!(snap.len(), 30);
        assert!(!snap.is_empty());

        // `get` and `iter` agree on every global index, oldest first.
        for (expected_i, (i, line)) in snap.iter().enumerate() {
            assert_eq!(i, expected_i);
            assert_eq!(snap.get(i).unwrap().msg, line.msg);
        }
        assert!(snap.get(30).is_none());

        // `iter_rev` walks newest-first with the same global indices.
        let rev: Vec<(usize, String)> = snap.iter_rev().map(|(i, l)| (i, l.msg.clone())).collect();
        assert_eq!(rev.first().unwrap().0, 29);
        assert_eq!(rev.first().unwrap().1, "seg2-line9");
        assert_eq!(rev.last().unwrap().0, 0);
        assert_eq!(rev.last().unwrap().1, "seg0-line0");
        let mut fwd_order: Vec<usize> = rev.iter().map(|(i, _)| *i).collect();
        fwd_order.reverse();
        assert_eq!(fwd_order, (0..30).collect::<Vec<_>>());

        // `scan_head` with a small limit stops after crossing one segment
        // boundary and returns the newest matches with correct global ids.
        let labels = head_snapshot_labels(&snap).unwrap();
        let mut q = query(LogAccessScope::All);
        q.start_time = ts - chrono::Duration::hours(1);
        q.end_time = ts + chrono::Duration::hours(1);
        q.limit = 12;
        let filter = LineFilter::compile(&q);
        let scan = ChunkStore::scan_head(&snap, &labels, &filter);
        assert_eq!(scan.matches.len(), 12);
        // Newest-first in the scan output.
        assert_eq!(scan.matches[0].message, "seg2-line9");
        assert_eq!(scan.matches[11].message, "seg1-line8");
        for m in &scan.matches {
            assert!(m.line_id >= HEAD_LINE_ID_BASE);
        }
    }

    // ── End-to-end: real writer → real planner → Postgres manifests ────

    fn e2e_line(
        container: &str,
        project: i32,
        external: Option<i32>,
        ts: DateTime<Utc>,
        level: LogLevel,
        msg: &str,
    ) -> LogLine {
        LogLine {
            ts,
            stream: LogStream::Stdout,
            level,
            msg: msg.to_string(),
            // Extracted attributes as the parser would leave them: one
            // string, one number, so attribute predicates have both to bite on.
            fields: Some(serde_json::json!({
                "container": container,
                "n": msg.len(),
            })),
            container_id: container.to_string(),
            service: format!("svc-{container}"),
            env: "prod".into(),
            project_id: project,
            external_service_id: external,
            deploy_id: Some(1),
            node_id: None,
            node_name: None,
        }
    }

    async fn drain(store: &ChunkStore, mut q: LogQuery) -> Vec<LogLineRecord> {
        let mut out = Vec::new();
        loop {
            let page = store.search(&q).await.unwrap();
            assert!(
                page.scanned_back_to.is_none(),
                "budget must not bite in drain"
            );
            let done = page.next_cursor.is_none();
            out.extend(page.lines);
            if done {
                return out;
            }
            q.before = page.next_cursor;
        }
    }

    fn assert_strictly_descending(lines: &[LogLineRecord]) {
        for w in lines.windows(2) {
            assert!(
                w[0].key() > w[1].key(),
                "page order broken: {:?} !> {:?}",
                w[0].key(),
                w[1].key()
            );
        }
    }

    #[tokio::test]
    #[serial_test::serial]
    async fn writer_to_planner_end_to_end() {
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
        let cache = ChunkCache::open(Some(tmp.path().join("cache")), 64 * 1024 * 1024)
            .await
            .unwrap();
        let writer = crate::services::ChunkWriterService::open(
            storage.clone(),
            manifests.clone(),
            Some(tmp.path().join("wal")),
            Some(cache.clone()),
        )
        .await
        .unwrap();

        // Three streams: two application containers in project 7, one
        // external service (database) 5. 5 seals each → fragmented.
        let base: DateTime<Utc> = "2026-03-01T00:00:00Z".parse().unwrap();
        let streams = [
            ("aaaaaaaaaaaa1", 7, None),
            ("bbbbbbbbbbbb2", 7, None),
            ("cccccccccccc3", 0, Some(5)),
        ];
        let mut expected_total = 0usize;
        let mut expected_errors = 0usize;
        let mut ts_counter = 0i64;
        for seal in 0..5 {
            for (container, project, external) in streams {
                for i in 0..400 {
                    let level = match i % 10 {
                        0 => LogLevel::Error,
                        1 | 2 => LogLevel::Warn,
                        _ => LogLevel::Info,
                    };
                    let msg = if seal == 2 && i == 123 {
                        format!("request rid-9f3a7c21 failed on {container}")
                    } else {
                        format!("line {i} of seal {seal} for {container}")
                    };
                    ts_counter += 1;
                    writer
                        .write_line(e2e_line(
                            container,
                            project,
                            external,
                            base + chrono::Duration::milliseconds(ts_counter * 7),
                            level,
                            &msg,
                        ))
                        .await
                        .unwrap();
                    expected_total += 1;
                    if level == LogLevel::Error {
                        expected_errors += 1;
                    }
                }
            }
            writer.flush_all().await;
        }
        // Unsealed head lines on one container: newest of all.
        for i in 0..25 {
            ts_counter += 1;
            writer
                .write_line(e2e_line(
                    "aaaaaaaaaaaa1",
                    7,
                    None,
                    base + chrono::Duration::milliseconds(ts_counter * 7),
                    LogLevel::Info,
                    &format!("head line {i}"),
                ))
                .await
                .unwrap();
        }
        let head_lines = 25usize;
        let window_end = base + chrono::Duration::milliseconds((ts_counter + 10) * 7);

        let store = ChunkStore::new(
            ManifestRepo::new(db.connection_arc()),
            storage.clone(),
            cache.clone(),
            writer.clone(),
        );
        let mut q = LogQuery::for_scope(LogAccessScope::All);
        q.start_time = base - chrono::Duration::hours(1);
        q.end_time = window_end;
        q.limit = 200;

        // 1. Full drain: every line exactly once, newest first, heads first.
        let all = drain(&store, q.clone()).await;
        assert_eq!(all.len(), expected_total + head_lines);
        assert_strictly_descending(&all);
        assert!(
            all[0].line_id >= HEAD_LINE_ID_BASE,
            "newest lines are the unsealed head"
        );
        assert!(all[head_lines].line_id < HEAD_LINE_ID_BASE);
        let mut keys: Vec<LogLineKey> = all.iter().map(LogLineRecord::key).collect();
        keys.sort();
        keys.dedup();
        assert_eq!(keys.len(), all.len(), "no duplicate keys across pages");

        // 2. Level filter: exactly the error lines, level_mask pruning included.
        let mut errors_q = q.clone();
        errors_q.levels = vec![LogLevel::Error];
        let errors = drain(&store, errors_q).await;
        assert_eq!(errors.len(), expected_errors);
        assert!(errors.iter().all(|l| l.level == LogLevel::Error));

        // 3. Free text: the bloom prunes 12 of 15 chunks, 3 lines survive.
        let mut text_q = q.clone();
        text_q.text = Some("RID-9F3A7C21".into());
        let hits = drain(&store, text_q.clone()).await;
        assert_eq!(hits.len(), 3);
        text_q.text = Some("does-not-exist-anywhere".into());
        let page = store.search(&text_q).await.unwrap();
        assert!(
            page.lines.is_empty() && page.next_cursor.is_none() && page.scanned_back_to.is_none()
        );

        // 4. Scope: allow-list is applied to sealed chunks and heads alike.
        let mut scoped = q.clone();
        scoped.scope = LogAccessScope::Allowed {
            project_ids: vec![7],
            external_service_ids: vec![],
        };
        let scoped_lines = drain(&store, scoped.clone()).await;
        assert_eq!(scoped_lines.len(), 2 * 5 * 400 + head_lines);
        assert!(scoped_lines.iter().all(|l| l.external_service_id.is_none()));
        scoped.scope = LogAccessScope::Allowed {
            project_ids: vec![],
            external_service_ids: vec![],
        };
        assert!(drain(&store, scoped.clone()).await.is_empty());
        scoped.scope = LogAccessScope::Allowed {
            project_ids: vec![],
            external_service_ids: vec![5],
        };
        assert_eq!(drain(&store, scoped).await.len(), 5 * 400);

        // 5. Budget exhaustion yields an honest partial page that resumes
        //    into exactly the same total — a strict prefix, no duplicates.
        let tight = ChunkStore::new(
            ManifestRepo::new(db.connection_arc()),
            storage.clone(),
            cache.clone(),
            writer.clone(),
        )
        .with_budget(SearchBudget {
            time: Duration::from_secs(60),
            bytes: 1,
            parallel: 1,
            batch: 4,
        });
        let mut resumed: Vec<LogLineRecord> = Vec::new();
        let mut pq = q.clone();
        pq.limit = 1000;
        let mut partial_pages = 0;
        loop {
            let page = tight.search(&pq).await.unwrap();
            if page.scanned_back_to.is_some() {
                partial_pages += 1;
            }
            let done = page.next_cursor.is_none();
            resumed.extend(page.lines);
            if done {
                break;
            }
            pq.before = page.next_cursor;
        }
        assert!(
            partial_pages > 0,
            "the 1-byte budget must produce partial pages"
        );
        assert_eq!(resumed.len(), all.len());
        assert_strictly_descending(&resumed);
        let resumed_keys: Vec<LogLineKey> = resumed.iter().map(LogLineRecord::key).collect();
        assert_eq!(
            resumed_keys,
            all.iter().map(LogLineRecord::key).collect::<Vec<_>>()
        );

        // 5b. Attribute predicates are enforced per line in both scans —
        //     sealed chunks and the unsealed head — from `fields`, and
        //     `chunk_seqs` restricts the sealed side only.
        let attr = |key: &str, op: crate::index::analytics::AttrOp, value: Option<&str>| {
            crate::index::analytics::AttrPredicate {
                key: key.into(),
                op,
                value: value.map(String::from),
            }
        };
        let mut aq = q.clone();
        aq.attrs = vec![attr(
            "container",
            crate::index::analytics::AttrOp::Eq,
            Some("aaaaaaaaaaaa1"),
        )];
        let a_lines = drain(&store, aq.clone()).await;
        assert_eq!(a_lines.len(), 5 * 400 + head_lines);
        assert!(a_lines.iter().all(|l| l.container_id == "aaaaaaaaaaaa1"));
        assert!(
            a_lines[0].line_id >= HEAD_LINE_ID_BASE,
            "head lines satisfy predicates too"
        );
        aq.attrs
            .push(attr("n", crate::index::analytics::AttrOp::Gt, Some("100")));
        assert!(
            drain(&store, aq.clone()).await.is_empty(),
            "AND of predicates, numeric compare"
        );
        aq.attrs = vec![attr(
            "container",
            crate::index::analytics::AttrOp::Exists,
            None,
        )];
        aq.text = Some("head line".into());
        assert_eq!(drain(&store, aq.clone()).await.len(), head_lines);
        // An empty nomination list scans no sealed chunk but still the heads.
        aq.text = None;
        aq.chunk_seqs = Some(Vec::new());
        let heads_only = drain(&store, aq.clone()).await;
        assert_eq!(heads_only.len(), head_lines);
        assert!(heads_only.iter().all(|l| l.line_id >= HEAD_LINE_ID_BASE));
        // Nominating one real chunk scans exactly that chunk (+ heads).
        let one = manifests
            .candidates(&q, None, 1)
            .await
            .unwrap()
            .into_iter()
            .next()
            .unwrap();
        aq.attrs = vec![attr(
            "container",
            crate::index::analytics::AttrOp::Eq,
            Some(&one.labels.container_id),
        )];
        aq.chunk_seqs = Some(vec![one.seq]);
        let nominated = drain(&store, aq).await;
        let sealed_from_one = nominated
            .iter()
            .filter(|l| l.line_id < HEAD_LINE_ID_BASE)
            .count();
        assert_eq!(sealed_from_one, one.line_count as usize);
        assert!(nominated
            .iter()
            .filter(|l| l.line_id < HEAD_LINE_ID_BASE)
            .all(|l| l.key().chunk_position().map(|(s, _)| s) == Some(one.seq)));

        // 6. Facets and sources come from manifests (+ heads).
        let facets = store
            .facets(
                &q,
                &[FacetField::Service, FacetField::Env, FacetField::Level],
            )
            .await
            .unwrap();
        assert_eq!(facets.fields["service"].len(), 3);
        assert_eq!(facets.fields["env"][0].value, "prod");
        let error_facet = facets.fields["level"]
            .iter()
            .find(|v| v.value.eq_ignore_ascii_case("error"))
            .expect("ERROR facet present");
        assert_eq!(
            error_facet.count as usize, expected_errors,
            "level facets are exact for v2 chunks (per-level counts in the manifest)"
        );
        let sources = store.sources(&q).await.unwrap();
        assert_eq!(sources.len(), 3);

        // 7. Context around a sealed line, crossing nothing.
        let target = &all[head_lines + 50];
        let ctx = store
            .context(&LogAccessScope::All, &target.key(), 3, 2)
            .await
            .unwrap();
        assert_eq!(ctx.len(), 6);
        assert_eq!(ctx[3].line_id, target.line_id);
        for w in ctx.windows(2) {
            assert!(w[0].timestamp <= w[1].timestamp);
        }

        // 8. Compaction merges the 5 fragments per container into 1 and the
        //    search result is unchanged; GC honours the grace period.
        let compactor = crate::services::CompactorService::new(
            manifests.clone(),
            storage.clone(),
            Some(cache.clone()),
        );
        let (_, chunks_before) = manifests.usage().await.unwrap();
        assert_eq!(chunks_before, 15);
        let report = compactor
            .compact_window(base - chrono::Duration::hours(1), window_end)
            .await;
        assert_eq!(report.chunks_in, 15);
        assert_eq!(report.chunks_out, 3);
        let (_, chunks_after) = manifests.usage().await.unwrap();
        assert_eq!(chunks_after, 3);
        let after = drain(&store, q.clone()).await;
        assert_eq!(after.len(), all.len());
        assert_eq!(
            after
                .iter()
                .map(|l| (l.timestamp, l.message.clone()))
                .collect::<Vec<_>>(),
            all.iter()
                .map(|l| (l.timestamp, l.message.clone()))
                .collect::<Vec<_>>()
        );
        let gc = compactor.gc_once().await;
        assert_eq!(
            gc.objects_deleted, 0,
            "tombstones younger than the grace period are kept"
        );

        // 9. Reconcile: a lost manifest row is re-adopted from the object's
        //    own footer; a lost object gets its manifest tombstoned.
        let live = manifests.live_after_seq(0, 100).await.unwrap();
        assert_eq!(live.len(), 3);
        let lost_row = live[0].clone();
        let lost_object = live[1].clone();
        // `hard_delete` only removes tombstones (GC semantics), so simulate
        // a genuinely lost row — e.g. Postgres restored from an older backup.
        use sea_orm::ConnectionTrait as _;
        db.connection_arc()
            .execute(sea_orm::Statement::from_sql_and_values(
                sea_orm::DatabaseBackend::Postgres,
                "DELETE FROM log_chunks WHERE id = $1",
                [lost_row.id.into()],
            ))
            .await
            .unwrap();
        storage
            .delete_chunk(&lost_object.storage_key)
            .await
            .unwrap();
        let r = compactor.reconcile_once().await;
        assert_eq!((r.adopted, r.tombstoned, r.failed), (1, 1, 0));
        let live_after = manifests.live_after_seq(0, 100).await.unwrap();
        assert_eq!(live_after.len(), 2);
        let readopted = live_after
            .iter()
            .find(|m| m.storage_key == lost_row.storage_key)
            .expect("re-adopted from footer");
        assert_eq!(readopted.labels, lost_row.labels);
        assert_eq!(readopted.footer_offset, lost_row.footer_offset);
        assert!(!live_after.iter().any(|m| m.id == lost_object.id));
        let survivors = drain(&store, q.clone()).await;
        assert_eq!(survivors.len(), all.len() - lost_object.line_count as usize);
        let hits_after = drain(&store, {
            let mut t = q.clone();
            t.text = Some("rid-9f3a7c21".into());
            t
        })
        .await;
        // One needle line per container; the lost object took one with it.
        assert_eq!(hits_after.len(), 2);
    }
}
