// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Log chunk format v2 (ADR-046): the shared contract between the writer,
//! the reader/planner, the compactor and the reconcile sweep.
//!
//! A chunk is one immutable object holding the lines of **one container** in
//! **time order**. It is self-describing: everything the `log_chunks` manifest
//! row says about it is also in the file, so the manifest can be rebuilt by
//! walking the bucket.
//!
//! ```text
//! ┌──────────────── body ────────────────┐┌──────── footer (zstd skippable frame) ────────┐
//! │ block 0 │ block 1 │ … │ block n-1     ││ hdr │ labels │ block index │ bloom │ trailer │
//! └──────────────────────────────────────┘└───────────────────────────────────────────────┘
//! ```
//!
//! The whole object is a **valid zstd stream** (`.zst`): the blocks are
//! ordinary frames and the footer is wrapped in a *skippable frame*
//! ([`SKIPPABLE_MAGIC`] + length, [`SKIPPABLE_HEADER_LEN`] bytes) that any
//! zstd decoder silently ignores. `zstd -dc chunk.zst` therefore yields the
//! concatenated block bodies; our reader locates the footer via the trailer.
//!
//! * **Block** — one independent zstd frame (level 3) over a *columnar* body
//!   of ≈[`TARGET_BLOCK_BYTES`] uncompressed: zigzag-varint timestamp
//!   deltas, `level[]`, `stream[]`, varint message lengths + bytes, varint
//!   fields lengths + bytes (see [`format`]). Delta timestamps compress 2×
//!   better than absolute nanoseconds (measured, see ADR-046 §1); a level or
//!   time filter scans a few KB of arrays without touching messages.
//! * **Labels** — JSON [`ChunkLabels`]: the stream identity and time bounds.
//! * **Block index** — JSON `Vec<`[`BlockMeta`]`>`.
//! * **Bloom** — [`bloom::Bloom`] over lowercased message tokens *and* every
//!   3-gram of each token, so both whole-word and substring needles prune.
//!   Sized per chunk from the distinct-entry count (≈1 % false positives).
//!   Length `0` means "no bloom; scan, never prune" (built under shed).
//! * **Trailer** — fixed [`TRAILER_LEN`] bytes, little-endian:
//!   `labels_off u64, labels_len u32, index_off u64, index_len u32,
//!   bloom_off u64, bloom_len u32, crc32(labels‖index‖bloom) u32,
//!   version u16 = 2, pad u16, magic u32 =`[`MAGIC`]. The manifest row stores
//!   `footer_offset = labels_off` and `footer_len` so a reader fetches the
//!   whole footer (index tier + bloom tier) with one range-GET; the trailer
//!   alone lets a bucket walk find the footer without Postgres.
//!
//! `line_id` for a sealed line is `manifest.seq << 20 | line_index` where
//! `line_index` counts from 0 across the whole chunk (see
//! [`crate::store::LogLineKey`]).
//!
//! Format v1 (`.ndjson.zst`, one zstd frame of NDJSON) remains readable via
//! [`format::decode_v1`]; its manifest rows carry `format_version = 1`. v1
//! and v2 objects share the `.zst` suffix; a bucket walk tells them apart by
//! the `.ndjson.zst` suffix and, authoritatively, by the trailer magic.

pub mod bloom;
pub mod cache;
pub mod format;
pub mod wal;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::types::LogLevel;

/// Magic at the very end of every v2 chunk: ASCII `TLC2`, little-endian.
pub const MAGIC: u32 = 0x3243_4C54;

/// Current chunk format version.
pub const FORMAT_VERSION: u16 = 2;

/// Fixed size of the trailer in bytes.
pub const TRAILER_LEN: usize = 48;

/// Magic of the zstd skippable frame that wraps the footer
/// (`0x184D2A50`, the first of the sixteen `0x184D2A5?` values the zstd
/// spec reserves for user data).
pub const SKIPPABLE_MAGIC: u32 = 0x184D_2A50;

/// Size of a skippable frame header: magic `u32` + payload length `u32`.
pub const SKIPPABLE_HEADER_LEN: usize = 8;

/// Uncompressed bytes per block the writer aims for. 1 MiB blocks measured
/// 7–21 % smaller objects, but a tail query decodes the newest block of
/// every live container, and that tripled "last 500" latency (14 → 42 ms
/// at 40 containers). 256 KiB keeps the per-container tail cost ≈0.3 ms.
pub const TARGET_BLOCK_BYTES: usize = 256 * 1024;

/// Writer seals a stream's head buffer at this many uncompressed bytes.
pub const DEFAULT_HEAD_MAX_BYTES: usize = 8 * 1024 * 1024;

/// Writer seals a head buffer that is at least this old and at least
/// [`MIN_FLUSH_BYTES`] large.
pub const FLUSH_AGE_SECS: i64 = 5 * 60;

/// A head buffer smaller than this waits for [`MAX_FLUSH_AGE_SECS`] instead
/// of [`FLUSH_AGE_SECS`], so idle containers do not cost a PUT every 5 min.
pub const MIN_FLUSH_BYTES: usize = 64 * 1024;

/// Hard age limit: any non-empty head buffer is sealed after this.
pub const MAX_FLUSH_AGE_SECS: i64 = 30 * 60;

/// Compactor never produces a chunk larger than this (uncompressed).
pub const MAX_COMPACTED_BYTES: usize = 64 * 1024 * 1024;

/// File extension of v2 chunk objects — a plain `.zst`, because the object
/// is a conforming zstd stream (footer in a skippable frame).
pub const V2_EXTENSION: &str = "zst";

/// Suffix of legacy v1 objects. Both formats end in `.zst`; this is the
/// only key-level way to tell them apart (the trailer magic is definitive).
pub const V1_SUFFIX: &str = ".ndjson.zst";

/// Bit set in a `level_mask` for each [`LogLevel`].
pub fn level_bit(level: LogLevel) -> u16 {
    match level {
        LogLevel::Trace => 1 << 0,
        LogLevel::Debug => 1 << 1,
        LogLevel::Info => 1 << 2,
        LogLevel::Warn => 1 << 3,
        LogLevel::Error => 1 << 4,
    }
}

/// `level_mask` with every bit set — used for v1 rows (levels unknown) so
/// they are never pruned.
pub const LEVEL_MASK_ALL: u16 = 0b1_1111;

/// Mask for a set of wanted levels; empty set means "all".
pub fn level_mask_for(levels: &[LogLevel]) -> u16 {
    if levels.is_empty() {
        LEVEL_MASK_ALL
    } else {
        levels.iter().map(|l| level_bit(*l)).fold(0, |a, b| a | b)
    }
}

/// Number of distinct [`LogLevel`]s, i.e. the length of `level_counts`.
pub const LEVEL_COUNT: usize = 5;

/// Encode a [`LogLevel`] as the single byte stored in a block's `level[]`.
pub fn level_to_u8(level: LogLevel) -> u8 {
    match level {
        LogLevel::Trace => 0,
        LogLevel::Debug => 1,
        LogLevel::Info => 2,
        LogLevel::Warn => 3,
        LogLevel::Error => 4,
    }
}

/// Inverse of [`level_to_u8`]; unknown bytes decode as `Info`.
pub fn level_from_u8(b: u8) -> LogLevel {
    match b {
        0 => LogLevel::Trace,
        1 => LogLevel::Debug,
        3 => LogLevel::Warn,
        4 => LogLevel::Error,
        _ => LogLevel::Info,
    }
}

/// Stream identity and bounds of a chunk, stored in the footer's labels
/// section and mirrored in the `log_chunks` manifest row.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChunkLabels {
    /// `0` sentinel for external-service chunks.
    pub project_id: i32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub external_service_id: Option<i32>,
    pub env: String,
    pub service: String,
    pub container_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub deploy_id: Option<i32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub node_id: Option<i32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub node_name: Option<String>,
    /// Event time of the oldest line.
    pub started_at: DateTime<Utc>,
    /// Event time of the newest line.
    pub ended_at: DateTime<Utc>,
    pub line_count: u32,
    /// OR of [`level_bit`] over every line.
    pub level_mask: u16,
    /// Lines per level, indexed by [`level_to_u8`] (Trace..Error). Exact
    /// per-level facet counts come from summing this over manifests.
    #[serde(default)]
    pub level_counts: [u32; 5],
}

/// One entry of the block index.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BlockMeta {
    /// Byte offset of the zstd frame from the start of the object.
    pub offset: u64,
    /// Compressed length of the frame.
    pub len: u32,
    /// Length of the columnar body once decompressed.
    pub uncompressed_len: u32,
    pub first_ts: DateTime<Utc>,
    pub last_ts: DateTime<Utc>,
    pub line_count: u32,
    /// Chunk-wide index of this block's first line (`line_index` base).
    pub first_line_index: u32,
    /// OR of [`level_bit`] over the block's lines.
    pub level_mask: u16,
    /// Lines per level in this block, indexed by [`level_to_u8`]. Lets a
    /// histogram be exact at block granularity (≈ seconds of logs) from
    /// the cached footer alone, without decoding the block.
    #[serde(default)]
    pub level_counts: [u32; LEVEL_COUNT],
}

impl BlockMeta {
    /// True when the block may hold lines inside `[start, end]` with one of
    /// the wanted levels. Both checks are cheap manifest arithmetic.
    pub fn may_match(&self, start: DateTime<Utc>, end: DateTime<Utc>, level_mask: u16) -> bool {
        self.last_ts >= start && self.first_ts <= end && (self.level_mask & level_mask) != 0
    }
}

/// The decoded footer of a v2 chunk.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChunkFooter {
    pub labels: ChunkLabels,
    pub blocks: Vec<BlockMeta>,
    /// `None` when the chunk was written without a bloom (never prune).
    pub bloom: Option<bloom::Bloom>,
}

/// Byte extents of the footer sections, as read from the trailer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Trailer {
    pub labels_off: u64,
    pub labels_len: u32,
    pub index_off: u64,
    pub index_len: u32,
    pub bloom_off: u64,
    pub bloom_len: u32,
    pub crc32: u32,
    pub version: u16,
}

impl Trailer {
    /// Offset of the first footer byte — what the manifest stores as
    /// `footer_offset`.
    pub fn footer_offset(&self) -> u64 {
        self.labels_off
    }

    /// Total footer length including the trailer — the manifest's
    /// `footer_len`. One range-GET of `[footer_offset, footer_offset +
    /// footer_len)` yields everything needed to decode the footer.
    pub fn footer_len(&self) -> u64 {
        (self.bloom_off + u64::from(self.bloom_len) + TRAILER_LEN as u64) - self.labels_off
    }
}

/// One decoded line, positioned within its chunk.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DecodedLine {
    pub ts: DateTime<Utc>,
    pub level: LogLevel,
    pub stream: crate::types::LogStream,
    pub message: String,
    pub fields: Option<serde_json::Value>,
    /// Chunk-wide line index (`first_line_index + position in block`).
    pub line_index: u32,
}
