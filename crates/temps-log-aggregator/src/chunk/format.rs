// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Byte-level encoder/decoder for the v2 chunk format (ADR-046 §1), plus a
//! decoder for legacy v1 (`.ndjson.zst`) objects.
//!
//! See [`crate::chunk`] for the overall layout. This module owns:
//!
//! * The **block body** layout — a columnar record of `ts[]`, `level[]`,
//!   `stream[]`, message bytes and fields bytes, each block compressed as
//!   one independent zstd frame.
//! * [`ChunkEncoder`] — appends [`LogLine`]s, seals blocks at
//!   [`crate::chunk::TARGET_BLOCK_BYTES`], and produces a complete
//!   [`EncodedChunk`] on [`ChunkEncoder::finish`].
//! * [`decode_trailer`] / [`decode_footer`] / [`decode_block`] — the reader
//!   side, each taking exactly the byte range the planner would range-GET.
//! * [`decode_v1`] — whole-object decode of the legacy NDJSON+zstd format.

use chrono::{DateTime, TimeZone, Utc};

use crate::chunk::{
    bloom::{Bloom, BloomBuilder},
    level_from_u8, level_to_u8, BlockMeta, ChunkFooter, ChunkLabels, DecodedLine, Trailer, MAGIC,
    TARGET_BLOCK_BYTES, TRAILER_LEN,
};
use crate::error::LogAggregatorError;
use crate::types::{LogLevel, LogLine, LogStream};

/// Identity of the stream a [`ChunkEncoder`] is sealing — everything that
/// goes into [`ChunkLabels`] except the derived time bounds/counts.
#[derive(Debug, Clone)]
pub struct ChunkIdentity {
    pub project_id: i32,
    pub external_service_id: Option<i32>,
    pub env: String,
    pub service: String,
    pub container_id: String,
    pub deploy_id: Option<i32>,
    pub node_id: Option<i32>,
    pub node_name: Option<String>,
}

/// Output of [`ChunkEncoder::finish`]: the encoded object bytes plus the
/// structured footer/trailer the caller needs for the manifest row.
#[derive(Debug)]
pub struct EncodedChunk {
    pub bytes: Vec<u8>,
    pub footer: ChunkFooter,
    pub trailer: Trailer,
    /// Sum of uncompressed block bytes — informational (not stored on disk).
    pub uncompressed_bytes: usize,
}

/// Accumulates lines into ≈[`TARGET_BLOCK_BYTES`] blocks and encodes a
/// complete v2 chunk object on [`ChunkEncoder::finish`].
pub struct ChunkEncoder {
    identity: ChunkIdentity,
    body: Vec<u8>,
    block_metas: Vec<BlockMeta>,
    bloom_builder: BloomBuilder,
    current: CurrentBlock,
    total_lines: u32,
    min_ts: Option<DateTime<Utc>>,
    max_ts: Option<DateTime<Utc>>,
    level_mask: u16,
    level_counts: [u32; crate::chunk::LEVEL_COUNT],
}

/// The in-progress (unsealed) block, kept as parallel columns so sealing is
/// just "serialize the columns and compress".
#[derive(Default)]
struct CurrentBlock {
    ts: Vec<i64>,
    level: Vec<u8>,
    stream: Vec<u8>,
    msg_offsets: Vec<u32>,
    msg_bytes: Vec<u8>,
    fields_offsets: Vec<u32>,
    fields_bytes: Vec<u8>,
    level_mask: u16,
    level_counts: [u32; crate::chunk::LEVEL_COUNT],
    min_ts: Option<DateTime<Utc>>,
    max_ts: Option<DateTime<Utc>>,
}

impl CurrentBlock {
    fn new() -> Self {
        let mut b = Self::default();
        b.msg_offsets.push(0);
        b.fields_offsets.push(0);
        b
    }

    fn line_count(&self) -> usize {
        self.ts.len()
    }

    fn is_empty(&self) -> bool {
        self.ts.is_empty()
    }

    /// Estimated uncompressed size of the columnar body if sealed right now
    /// (varints assumed at their typical width: ~4 bytes per timestamp
    /// delta, ~1–2 per length).
    fn estimated_bytes(&self) -> usize {
        let line_count = self.line_count();
        4 // line_count
            + line_count * 4 // ts deltas
            + line_count // level
            + line_count // stream
            + line_count * 2 // msg lengths
            + self.msg_bytes.len()
            + line_count // fields lengths
            + self.fields_bytes.len()
    }

    fn push(&mut self, line: &LogLine) -> Result<(), LogAggregatorError> {
        let ts_nanos = ts_nanos(line.ts);
        self.ts.push(ts_nanos);
        self.level.push(level_to_u8(line.level));
        self.stream.push(stream_to_u8(line.stream));

        self.msg_bytes.extend_from_slice(line.msg.as_bytes());
        self.msg_offsets.push(self.msg_bytes.len() as u32);

        if let Some(fields) = &line.fields {
            let encoded = serde_json::to_vec(fields)?;
            self.fields_bytes.extend_from_slice(&encoded);
        }
        self.fields_offsets.push(self.fields_bytes.len() as u32);

        self.level_mask |= crate::chunk::level_bit(line.level);
        self.level_counts[level_to_u8(line.level) as usize] += 1;
        self.min_ts = Some(match self.min_ts {
            Some(existing) if existing <= line.ts => existing,
            _ => line.ts,
        });
        self.max_ts = Some(match self.max_ts {
            Some(existing) if existing >= line.ts => existing,
            _ => line.ts,
        });
        Ok(())
    }

    /// Serialize the columnar body (uncompressed):
    ///
    /// ```text
    /// line_count u32 LE
    /// ts        : zigzag varint delta from the previous line (first from 0)
    /// level     : u8 × line_count
    /// stream    : u8 × line_count
    /// msg_len   : varint × line_count
    /// msg_bytes : concatenated
    /// fields_len: varint × line_count (0 = no fields)
    /// fields_bytes: concatenated JSON objects
    /// ```
    ///
    /// Deltas instead of absolute nanoseconds and lengths instead of
    /// offsets halve the compressed size on real logs (absolute `i64`
    /// timestamps are almost incompressible).
    fn serialize(&self) -> Vec<u8> {
        let line_count = self.line_count() as u32;
        let mut buf = Vec::with_capacity(self.estimated_bytes());
        buf.extend_from_slice(&line_count.to_le_bytes());
        let mut prev = 0i64;
        for &ts in &self.ts {
            put_varint(&mut buf, zigzag(ts.wrapping_sub(prev)));
            prev = ts;
        }
        buf.extend_from_slice(&self.level);
        buf.extend_from_slice(&self.stream);
        for w in self.msg_offsets.windows(2) {
            put_varint(&mut buf, u64::from(w[1] - w[0]));
        }
        buf.extend_from_slice(&self.msg_bytes);
        for w in self.fields_offsets.windows(2) {
            put_varint(&mut buf, u64::from(w[1] - w[0]));
        }
        buf.extend_from_slice(&self.fields_bytes);
        buf
    }
}

fn put_varint(buf: &mut Vec<u8>, mut v: u64) {
    while v >= 0x80 {
        buf.push((v as u8) | 0x80);
        v >>= 7;
    }
    buf.push(v as u8);
}

/// Reads one LEB128 varint at `*cursor`, advancing it. Errors on truncation
/// or on more than 10 bytes (a `u64` never needs more).
fn get_varint(buf: &[u8], cursor: &mut usize) -> Result<u64, LogAggregatorError> {
    let mut v = 0u64;
    let mut shift = 0u32;
    loop {
        let b = *buf
            .get(*cursor)
            .ok_or_else(|| LogAggregatorError::ChunkFormat {
                reason: format!("block body truncated inside a varint at byte {cursor}"),
            })?;
        *cursor += 1;
        v |= u64::from(b & 0x7f) << shift;
        if b & 0x80 == 0 {
            return Ok(v);
        }
        shift += 7;
        if shift > 63 {
            return Err(LogAggregatorError::ChunkFormat {
                reason: format!("varint longer than 10 bytes at byte {cursor}"),
            });
        }
    }
}

fn zigzag(v: i64) -> u64 {
    ((v << 1) ^ (v >> 63)) as u64
}

fn unzigzag(v: u64) -> i64 {
    ((v >> 1) as i64) ^ -((v & 1) as i64)
}

fn stream_to_u8(stream: LogStream) -> u8 {
    match stream {
        LogStream::Stdout => 0,
        LogStream::Stderr => 1,
    }
}

fn stream_from_u8(b: u8) -> LogStream {
    match b {
        1 => LogStream::Stderr,
        _ => LogStream::Stdout,
    }
}

/// Nanosecond Unix timestamp for `ts`, falling back to `millis * 1_000_000`
/// when `timestamp_nanos_opt` overflows (dates far outside ~1677–2262).
fn ts_nanos(ts: DateTime<Utc>) -> i64 {
    ts.timestamp_nanos_opt()
        .unwrap_or_else(|| ts.timestamp_millis().saturating_mul(1_000_000))
}

fn ts_from_nanos(nanos: i64) -> DateTime<Utc> {
    let secs = nanos.div_euclid(1_000_000_000);
    let subsec_nanos = nanos.rem_euclid(1_000_000_000) as u32;
    Utc.timestamp_opt(secs, subsec_nanos)
        .single()
        .unwrap_or_else(Utc::now)
}

impl ChunkEncoder {
    pub fn new(identity: ChunkIdentity) -> Self {
        Self {
            identity,
            body: Vec::new(),
            block_metas: Vec::new(),
            bloom_builder: BloomBuilder::new(),
            current: CurrentBlock::new(),
            total_lines: 0,
            min_ts: None,
            max_ts: None,
            level_mask: 0,
            level_counts: [0; crate::chunk::LEVEL_COUNT],
        }
    }

    /// Appends one line to the current block. Lines are expected in time
    /// order but are never reordered even if they are not: [`BlockMeta`]
    /// bounds are computed as min/max, so out-of-order input stays correct,
    /// just less selective for pruning.
    pub fn push(&mut self, line: &LogLine) -> Result<(), LogAggregatorError> {
        self.bloom_builder.insert_message(&line.msg);
        self.current.push(line)?;
        self.total_lines += 1;
        self.level_mask |= crate::chunk::level_bit(line.level);
        self.level_counts[crate::chunk::level_to_u8(line.level) as usize] += 1;
        self.min_ts = Some(match self.min_ts {
            Some(existing) if existing <= line.ts => existing,
            _ => line.ts,
        });
        self.max_ts = Some(match self.max_ts {
            Some(existing) if existing >= line.ts => existing,
            _ => line.ts,
        });

        if self.current.estimated_bytes() >= TARGET_BLOCK_BYTES {
            self.seal_current_block()?;
        }
        Ok(())
    }

    fn seal_current_block(&mut self) -> Result<(), LogAggregatorError> {
        if self.current.is_empty() {
            return Ok(());
        }
        let first_line_index = self
            .block_metas
            .last()
            .map_or(0, |m| m.first_line_index + m.line_count);
        let uncompressed = self.current.serialize();
        let compressed = zstd::encode_all(uncompressed.as_slice(), 3).map_err(|e| {
            LogAggregatorError::CompressionFailed {
                chunk_id: uuid::Uuid::nil(),
                reason: e.to_string(),
            }
        })?;
        let offset = self.body.len() as u64;
        let len = compressed.len() as u32;
        let uncompressed_len = uncompressed.len() as u32;
        let line_count = self.current.line_count() as u32;
        let first_ts = self.current.min_ts.unwrap_or_else(Utc::now);
        let last_ts = self.current.max_ts.unwrap_or_else(Utc::now);
        let level_mask = self.current.level_mask;
        let level_counts = self.current.level_counts;

        self.body.extend_from_slice(&compressed);
        self.block_metas.push(BlockMeta {
            offset,
            len,
            uncompressed_len,
            first_ts,
            last_ts,
            line_count,
            first_line_index,
            level_mask,
            level_counts,
        });
        self.current = CurrentBlock::new();
        Ok(())
    }

    /// Seals the last block and writes labels, block index, bloom and
    /// trailer. Errors if no lines were ever pushed.
    pub fn finish(mut self, with_bloom: bool) -> Result<EncodedChunk, LogAggregatorError> {
        self.seal_current_block()?;

        if self.total_lines == 0 {
            return Err(LogAggregatorError::ChunkFormat {
                reason: "cannot encode an empty chunk (no lines were pushed)".to_string(),
            });
        }

        let started_at = self.min_ts.ok_or_else(|| LogAggregatorError::ChunkFormat {
            reason: "chunk has lines but no minimum timestamp; this is a bug".to_string(),
        })?;
        let ended_at = self.max_ts.ok_or_else(|| LogAggregatorError::ChunkFormat {
            reason: "chunk has lines but no maximum timestamp; this is a bug".to_string(),
        })?;

        let labels = ChunkLabels {
            project_id: self.identity.project_id,
            external_service_id: self.identity.external_service_id,
            env: self.identity.env,
            service: self.identity.service,
            container_id: self.identity.container_id,
            deploy_id: self.identity.deploy_id,
            node_id: self.identity.node_id,
            node_name: self.identity.node_name,
            started_at,
            ended_at,
            line_count: self.total_lines,
            level_mask: self.level_mask,
            level_counts: self.level_counts,
        };

        let labels_bytes = serde_json::to_vec(&labels)?;
        let index_bytes = serde_json::to_vec(&self.block_metas)?;
        let bloom = if with_bloom {
            Some(self.bloom_builder.build())
        } else {
            None
        };
        let bloom_bytes = bloom.as_ref().map(Bloom::to_bytes).unwrap_or_default();

        let mut crc = crc32fast::Hasher::new();
        crc.update(&labels_bytes);
        crc.update(&index_bytes);
        crc.update(&bloom_bytes);
        let crc32 = crc.finalize();

        // The footer rides inside a zstd *skippable frame* so the object as a
        // whole is a conforming zstd stream (`zstd -dc` decodes the blocks
        // and ignores the footer). Trailer offsets are absolute file offsets
        // and therefore already account for the 8-byte frame header.
        let payload_len = labels_bytes.len() + index_bytes.len() + bloom_bytes.len() + TRAILER_LEN;
        let payload_len_u32 =
            u32::try_from(payload_len).map_err(|_| LogAggregatorError::ChunkFormat {
                reason: format!("footer of {payload_len} bytes exceeds the skippable-frame limit"),
            })?;

        let labels_off = (self.body.len() + crate::chunk::SKIPPABLE_HEADER_LEN) as u64;
        let labels_len = labels_bytes.len() as u32;
        let index_off = labels_off + u64::from(labels_len);
        let index_len = index_bytes.len() as u32;
        let bloom_off = index_off + u64::from(index_len);
        let bloom_len = bloom_bytes.len() as u32;

        let trailer = Trailer {
            labels_off,
            labels_len,
            index_off,
            index_len,
            bloom_off,
            bloom_len,
            crc32,
            version: crate::chunk::FORMAT_VERSION,
        };

        let mut bytes = self.body;
        bytes.extend_from_slice(&crate::chunk::SKIPPABLE_MAGIC.to_le_bytes());
        bytes.extend_from_slice(&payload_len_u32.to_le_bytes());
        bytes.extend_from_slice(&labels_bytes);
        bytes.extend_from_slice(&index_bytes);
        bytes.extend_from_slice(&bloom_bytes);
        bytes.extend_from_slice(&encode_trailer(&trailer));

        let footer = ChunkFooter {
            labels,
            blocks: self.block_metas,
            bloom,
        };

        Ok(EncodedChunk {
            bytes,
            footer,
            trailer,
            uncompressed_bytes: 0,
        })
    }
}

fn encode_trailer(t: &Trailer) -> [u8; TRAILER_LEN] {
    let mut buf = [0u8; TRAILER_LEN];
    buf[0..8].copy_from_slice(&t.labels_off.to_le_bytes());
    buf[8..12].copy_from_slice(&t.labels_len.to_le_bytes());
    buf[12..20].copy_from_slice(&t.index_off.to_le_bytes());
    buf[20..24].copy_from_slice(&t.index_len.to_le_bytes());
    buf[24..32].copy_from_slice(&t.bloom_off.to_le_bytes());
    buf[32..36].copy_from_slice(&t.bloom_len.to_le_bytes());
    buf[36..40].copy_from_slice(&t.crc32.to_le_bytes());
    buf[40..42].copy_from_slice(&t.version.to_le_bytes());
    buf[42..44].copy_from_slice(&0u16.to_le_bytes());
    buf[44..48].copy_from_slice(&MAGIC.to_le_bytes());
    buf
}

/// Decodes the fixed trailer from the last [`TRAILER_LEN`] bytes of `tail`.
/// `tail` may be longer (e.g. the last 512 bytes of a speculative tail
/// read); only the final [`TRAILER_LEN`] bytes are used.
pub fn decode_trailer(tail: &[u8]) -> Result<Trailer, LogAggregatorError> {
    if tail.len() < TRAILER_LEN {
        return Err(LogAggregatorError::ChunkFormat {
            reason: format!(
                "trailer buffer too short: {} bytes, need at least {TRAILER_LEN}",
                tail.len()
            ),
        });
    }
    let t = &tail[tail.len() - TRAILER_LEN..];

    let magic = u32::from_le_bytes(t[44..48].try_into().unwrap_or_default());
    if magic != MAGIC {
        return Err(LogAggregatorError::ChunkFormat {
            reason: format!("bad chunk magic: expected {MAGIC:#x}, got {magic:#x}"),
        });
    }
    let version = u16::from_le_bytes(t[40..42].try_into().unwrap_or_default());
    if version != crate::chunk::FORMAT_VERSION {
        return Err(LogAggregatorError::ChunkFormat {
            reason: format!(
                "unsupported chunk format version {version}, expected {}",
                crate::chunk::FORMAT_VERSION
            ),
        });
    }

    Ok(Trailer {
        labels_off: u64::from_le_bytes(t[0..8].try_into().unwrap_or_default()),
        labels_len: u32::from_le_bytes(t[8..12].try_into().unwrap_or_default()),
        index_off: u64::from_le_bytes(t[12..20].try_into().unwrap_or_default()),
        index_len: u32::from_le_bytes(t[20..24].try_into().unwrap_or_default()),
        bloom_off: u64::from_le_bytes(t[24..32].try_into().unwrap_or_default()),
        bloom_len: u32::from_le_bytes(t[32..36].try_into().unwrap_or_default()),
        crc32: u32::from_le_bytes(t[36..40].try_into().unwrap_or_default()),
        version,
    })
}

/// Decodes the footer from `footer_bytes`, the exact range
/// `[trailer.footer_offset(), +trailer.footer_len())` (so `footer_bytes`
/// ends with the trailer). Verifies the crc32 over labels‖index‖bloom.
pub fn decode_footer(
    footer_bytes: &[u8],
    trailer: &Trailer,
) -> Result<ChunkFooter, LogAggregatorError> {
    if footer_bytes.len() < TRAILER_LEN {
        return Err(LogAggregatorError::ChunkFormat {
            reason: format!(
                "footer buffer too short: {} bytes, need at least {TRAILER_LEN} for the trailer",
                footer_bytes.len()
            ),
        });
    }

    let labels_len = trailer.labels_len as usize;
    let index_len = trailer.index_len as usize;
    let bloom_len = trailer.bloom_len as usize;

    if footer_bytes.len() < labels_len + index_len + bloom_len + TRAILER_LEN {
        return Err(LogAggregatorError::ChunkFormat {
            reason: format!(
                "footer buffer too short for declared sections: got {} bytes, need {}",
                footer_bytes.len(),
                labels_len + index_len + bloom_len + TRAILER_LEN
            ),
        });
    }

    let labels_bytes = &footer_bytes[0..labels_len];
    let index_bytes = &footer_bytes[labels_len..labels_len + index_len];
    let bloom_bytes = &footer_bytes[labels_len + index_len..labels_len + index_len + bloom_len];

    let mut crc = crc32fast::Hasher::new();
    crc.update(labels_bytes);
    crc.update(index_bytes);
    crc.update(bloom_bytes);
    let computed = crc.finalize();
    if computed != trailer.crc32 {
        return Err(LogAggregatorError::ChunkFormat {
            reason: format!(
                "footer crc32 mismatch: computed {computed:#x}, trailer says {:#x}",
                trailer.crc32
            ),
        });
    }

    let labels: ChunkLabels = serde_json::from_slice(labels_bytes)?;
    let blocks: Vec<BlockMeta> = serde_json::from_slice(index_bytes)?;
    let bloom = if bloom_len == 0 {
        None
    } else {
        Some(Bloom::from_bytes(bloom_bytes)?)
    };

    Ok(ChunkFooter {
        labels,
        blocks,
        bloom,
    })
}

/// Time/level predicate applied while scanning a decompressed block, before
/// message/fields bytes are materialised.
#[derive(Debug, Clone, Default)]
pub struct BlockFilter {
    pub start: Option<DateTime<Utc>>,
    pub end: Option<DateTime<Utc>>,
    pub level_mask: u16,
}

impl BlockFilter {
    fn matches(&self, ts: DateTime<Utc>, level: LogLevel) -> bool {
        if let Some(start) = self.start {
            if ts < start {
                return false;
            }
        }
        if let Some(end) = self.end {
            if ts > end {
                return false;
            }
        }
        if self.level_mask != 0 && (self.level_mask & crate::chunk::level_bit(level)) == 0 {
            return false;
        }
        true
    }
}

/// Decompresses `compressed` (one block's zstd frame) and returns the lines
/// passing `filter`, in file order. Walks `ts[]`/`level[]` first and only
/// decodes message/fields bytes for lines that pass.
pub fn decode_block(
    compressed: &[u8],
    meta: &BlockMeta,
    filter: &BlockFilter,
) -> Result<Vec<DecodedLine>, LogAggregatorError> {
    let body =
        zstd::decode_all(compressed).map_err(|e| LogAggregatorError::DecompressionFailed {
            chunk_id: uuid::Uuid::nil(),
            reason: e.to_string(),
        })?;

    if body.len() < 4 {
        return Err(LogAggregatorError::ChunkFormat {
            reason: format!("block body too short: {} bytes", body.len()),
        });
    }
    let line_count = u32::from_le_bytes(body[0..4].try_into().unwrap_or_default()) as usize;
    if line_count > body.len() {
        // Every line costs at least one byte in each column; a count this
        // large cannot be genuine and would otherwise drive huge allocations.
        return Err(LogAggregatorError::ChunkFormat {
            reason: format!("block claims {line_count} lines in {} bytes", body.len()),
        });
    }
    let mut cursor = 4usize;

    let mut ts_nanos = Vec::with_capacity(line_count);
    let mut prev = 0i64;
    for _ in 0..line_count {
        prev = prev.wrapping_add(unzigzag(get_varint(&body, &mut cursor)?));
        ts_nanos.push(prev);
    }

    let level_start = cursor;
    let level_end = span_end(&body, level_start, line_count)?;
    cursor = level_end;

    let stream_start = cursor;
    let stream_end = span_end(&body, stream_start, line_count)?;
    cursor = stream_end;

    // Prefix sums of the varint lengths give the same `offsets[i..=i+1]`
    // slicing the old fixed-width layout had. Every partial sum is bounded
    // by the body length as it is built, so a corrupt length can neither
    // overflow the offset table nor alias another column.
    let msg_offsets = length_prefix_sums(&body, &mut cursor, line_count)?;
    let msg_bytes_start = cursor;
    let msg_bytes_end = span_end(&body, msg_bytes_start, msg_offsets[line_count])?;
    cursor = msg_bytes_end;

    let fields_offsets = length_prefix_sums(&body, &mut cursor, line_count)?;
    let fields_bytes_start = cursor;
    span_end(&body, fields_bytes_start, fields_offsets[line_count])?;

    let mut out = Vec::new();
    for i in 0..line_count {
        let ts = ts_from_nanos(ts_nanos[i]);
        let level = level_from_u8(body[level_start + i]);

        if !filter.matches(ts, level) {
            continue;
        }

        let stream = stream_from_u8(body[stream_start + i]);

        let msg_from = msg_bytes_start + msg_offsets[i];
        let msg_to = msg_bytes_start + msg_offsets[i + 1];
        let message = String::from_utf8_lossy(&body[msg_from..msg_to]).into_owned();

        let fields_from = fields_bytes_start + fields_offsets[i];
        let fields_to = fields_bytes_start + fields_offsets[i + 1];
        let fields = if fields_from == fields_to {
            None
        } else {
            Some(serde_json::from_slice(&body[fields_from..fields_to])?)
        };

        out.push(DecodedLine {
            ts,
            level,
            stream,
            message,
            fields,
            line_index: meta.first_line_index + i as u32,
        });
    }

    Ok(out)
}

/// `start + len`, rejected (never wrapped) when it would run past `buf`.
fn span_end(buf: &[u8], start: usize, len: usize) -> Result<usize, LogAggregatorError> {
    let end = start
        .checked_add(len)
        .ok_or_else(|| LogAggregatorError::ChunkFormat {
            reason: format!("block body span overflows: start {start} + len {len}"),
        })?;
    require_len(buf, end)?;
    Ok(end)
}

/// Reads `n` varint lengths and returns their `n + 1` prefix sums. Each
/// running total must stay within `buf.len()`: a length that cannot fit in
/// the remaining body is a format error, not a wrapped offset.
fn length_prefix_sums(
    buf: &[u8],
    cursor: &mut usize,
    n: usize,
) -> Result<Vec<usize>, LogAggregatorError> {
    let mut sums = Vec::with_capacity(n + 1);
    sums.push(0usize);
    let mut total = 0usize;
    for _ in 0..n {
        let len = get_varint(buf, cursor)?;
        let len = usize::try_from(len)
            .ok()
            .and_then(|len| total.checked_add(len))
            .filter(|&t| t <= buf.len())
            .ok_or_else(|| LogAggregatorError::ChunkFormat {
                reason: format!(
                    "block column length {len} exceeds body of {} bytes",
                    buf.len()
                ),
            })?;
        total = len;
        sums.push(total);
    }
    Ok(sums)
}

fn require_len(buf: &[u8], needed: usize) -> Result<(), LogAggregatorError> {
    if buf.len() < needed {
        return Err(LogAggregatorError::ChunkFormat {
            reason: format!(
                "block body truncated: needed at least {needed} bytes, got {}",
                buf.len()
            ),
        });
    }
    Ok(())
}

/// Decompresses a legacy `.ndjson.zst` object (one zstd frame of newline
/// delimited [`LogLine`] JSON) and parses every non-blank line.
pub fn decode_v1(compressed: &[u8]) -> Result<Vec<DecodedLine>, LogAggregatorError> {
    let body =
        zstd::decode_all(compressed).map_err(|e| LogAggregatorError::DecompressionFailed {
            chunk_id: uuid::Uuid::nil(),
            reason: e.to_string(),
        })?;
    let text = String::from_utf8_lossy(&body);

    let mut out = Vec::new();
    for (line_number, raw) in text.lines().enumerate() {
        if raw.trim().is_empty() {
            continue;
        }
        let line: LogLine =
            serde_json::from_str(raw).map_err(|e| LogAggregatorError::ChunkFormat {
                reason: format!("v1 chunk malformed at line {}: {e}", line_number + 1),
            })?;
        out.push(DecodedLine {
            ts: line.ts,
            level: line.level,
            stream: line.stream,
            message: line.msg,
            fields: line.fields,
            line_index: line_number as u32,
        });
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Duration;

    fn identity() -> ChunkIdentity {
        ChunkIdentity {
            project_id: 1,
            external_service_id: None,
            env: "1".to_string(),
            service: "web".to_string(),
            container_id: "container-abc".to_string(),
            deploy_id: Some(42),
            node_id: None,
            node_name: None,
        }
    }

    fn line(ts: DateTime<Utc>, level: LogLevel, msg: &str) -> LogLine {
        LogLine {
            ts,
            stream: LogStream::Stdout,
            level,
            msg: msg.to_string(),
            fields: None,
            container_id: "container-abc".to_string(),
            service: "web".to_string(),
            env: "1".to_string(),
            project_id: 1,
            external_service_id: None,
            deploy_id: Some(42),
            node_id: None,
            node_name: None,
        }
    }

    #[test]
    fn round_trip_single_line() {
        let mut encoder = ChunkEncoder::new(identity());
        let ts = Utc::now();
        encoder
            .push(&line(ts, LogLevel::Info, "hello world"))
            .unwrap();
        let encoded = encoder.finish(true).unwrap();

        let trailer = decode_trailer(&encoded.bytes).unwrap();
        let footer_bytes = &encoded.bytes[trailer.footer_offset() as usize
            ..(trailer.footer_offset() + trailer.footer_len()) as usize];
        let footer = decode_footer(footer_bytes, &trailer).unwrap();

        assert_eq!(footer.labels.line_count, 1);
        assert_eq!(footer.blocks.len(), 1);
        assert!(footer.bloom.is_some());

        let block_bytes = &encoded.bytes[footer.blocks[0].offset as usize
            ..footer.blocks[0].offset as usize + footer.blocks[0].len as usize];
        let lines = decode_block(block_bytes, &footer.blocks[0], &BlockFilter::default()).unwrap();
        assert_eq!(lines.len(), 1);
        assert_eq!(lines[0].message, "hello world");
        assert_eq!(lines[0].line_index, 0);
    }

    #[test]
    fn round_trip_many_blocks_contiguous_line_index() {
        let mut encoder = ChunkEncoder::new(identity());
        let base = Utc::now();
        let n = 50_000;
        // Long-ish messages so we cross TARGET_BLOCK_BYTES multiple times.
        for i in 0..n {
            let ts = base + Duration::milliseconds(i as i64);
            let level = match i % 5 {
                0 => LogLevel::Error,
                1 => LogLevel::Warn,
                2 => LogLevel::Debug,
                3 => LogLevel::Trace,
                _ => LogLevel::Info,
            };
            let msg = format!(
                "line number {i} with some padding text to grow the block xxxxxxxxxxxxxxxxxxxx"
            );
            encoder.push(&line(ts, level, &msg)).unwrap();
        }
        let encoded = encoder.finish(true).unwrap();

        let trailer = decode_trailer(&encoded.bytes).unwrap();
        let footer_bytes = &encoded.bytes[trailer.footer_offset() as usize
            ..(trailer.footer_offset() + trailer.footer_len()) as usize];
        let footer = decode_footer(footer_bytes, &trailer).unwrap();

        assert_eq!(footer.labels.line_count, n as u32);
        assert!(footer.blocks.len() > 1, "expected multiple blocks");
        assert_eq!(footer.labels.level_mask, crate::chunk::LEVEL_MASK_ALL);

        let mut expected_index = 0u32;
        let mut total_decoded = 0u32;
        let mut prev_last_ts: Option<DateTime<Utc>> = None;
        for meta in &footer.blocks {
            assert_eq!(meta.first_line_index, expected_index);
            let block_bytes =
                &encoded.bytes[meta.offset as usize..meta.offset as usize + meta.len as usize];
            let lines = decode_block(block_bytes, meta, &BlockFilter::default()).unwrap();
            assert_eq!(lines.len(), meta.line_count as usize);
            for (i, decoded) in lines.iter().enumerate() {
                assert_eq!(decoded.line_index, expected_index + i as u32);
            }
            assert_eq!(meta.first_ts, lines.first().unwrap().ts);
            assert_eq!(meta.last_ts, lines.last().unwrap().ts);
            if let Some(prev) = prev_last_ts {
                assert!(meta.first_ts >= prev);
            }
            prev_last_ts = Some(meta.last_ts);
            expected_index += meta.line_count;
            total_decoded += lines.len() as u32;
        }
        assert_eq!(total_decoded, n as u32);
    }

    #[test]
    fn block_filter_by_level_and_time_keeps_line_index() {
        let mut encoder = ChunkEncoder::new(identity());
        let base = Utc::now();
        for i in 0..2000 {
            let ts = base + Duration::seconds(i as i64);
            let level = if i % 10 == 0 {
                LogLevel::Error
            } else {
                LogLevel::Info
            };
            encoder.push(&line(ts, level, &format!("msg {i}"))).unwrap();
        }
        let encoded = encoder.finish(false).unwrap();
        let trailer = decode_trailer(&encoded.bytes).unwrap();
        let footer_bytes = &encoded.bytes[trailer.footer_offset() as usize
            ..(trailer.footer_offset() + trailer.footer_len()) as usize];
        let footer = decode_footer(footer_bytes, &trailer).unwrap();
        assert!(footer.bloom.is_none());
        assert_eq!(trailer.bloom_len, 0);

        let filter = BlockFilter {
            start: None,
            end: None,
            level_mask: crate::chunk::level_mask_for(&[LogLevel::Error]),
        };

        let mut matched = 0;
        for meta in &footer.blocks {
            let block_bytes =
                &encoded.bytes[meta.offset as usize..meta.offset as usize + meta.len as usize];
            let lines = decode_block(block_bytes, meta, &filter).unwrap();
            for l in &lines {
                assert_eq!(l.level, LogLevel::Error);
                assert_eq!(l.line_index % 10, 0);
            }
            matched += lines.len();
        }
        assert_eq!(matched, 200);
    }

    /// The object must be a conforming zstd stream: a stock decoder decodes
    /// every block body back to back and skips the footer frame.
    #[test]
    fn whole_object_is_a_valid_zstd_stream() {
        let mut encoder = ChunkEncoder::new(identity());
        let base = Utc::now();
        for i in 0..3000 {
            let ts = base + Duration::milliseconds(i);
            encoder
                .push(&line(ts, LogLevel::Info, &format!("plain zstd line {i}")))
                .unwrap();
        }
        let encoded = encoder.finish(true).unwrap();
        assert!(encoded.bytes.len() > crate::chunk::SKIPPABLE_HEADER_LEN + TRAILER_LEN);

        let decoded = zstd::decode_all(encoded.bytes.as_slice())
            .expect("stock zstd must accept the object (footer is a skippable frame)");

        let mut expected = Vec::new();
        for meta in &encoded.footer.blocks {
            let frame =
                &encoded.bytes[meta.offset as usize..meta.offset as usize + meta.len as usize];
            let body = zstd::decode_all(frame).unwrap();
            assert_eq!(body.len(), meta.uncompressed_len as usize);
            expected.extend(body);
        }
        assert_eq!(decoded, expected);

        // The skippable frame header sits right after the last block and
        // declares exactly the footer payload.
        let hdr = (encoded.trailer.labels_off as usize) - crate::chunk::SKIPPABLE_HEADER_LEN;
        let magic = u32::from_le_bytes(encoded.bytes[hdr..hdr + 4].try_into().unwrap());
        let len = u32::from_le_bytes(encoded.bytes[hdr + 4..hdr + 8].try_into().unwrap());
        assert_eq!(magic, crate::chunk::SKIPPABLE_MAGIC);
        assert_eq!(hdr + 8 + len as usize, encoded.bytes.len());
        assert_eq!(len as u64, encoded.trailer.footer_len());
    }

    #[test]
    fn varint_and_zigzag_round_trip() {
        for &v in &[
            0i64,
            1,
            -1,
            63,
            -64,
            1 << 40,
            -(1 << 40),
            i64::MAX,
            i64::MIN,
        ] {
            let mut buf = Vec::new();
            put_varint(&mut buf, zigzag(v));
            let mut c = 0;
            assert_eq!(unzigzag(get_varint(&buf, &mut c).unwrap()), v);
            assert_eq!(c, buf.len());
        }
        // Truncated varint is an error, not a panic.
        let mut c = 0;
        assert!(get_varint(&[0x80, 0x80], &mut c).is_err());
    }

    #[test]
    fn out_of_order_timestamps_survive_delta_encoding() {
        let mut encoder = ChunkEncoder::new(identity());
        let base = Utc::now();
        let offsets = [5i64, -3, 10, -10, 0, 7];
        for (i, off) in offsets.iter().enumerate() {
            encoder
                .push(&line(
                    base + Duration::seconds(*off),
                    LogLevel::Info,
                    &format!("l{i}"),
                ))
                .unwrap();
        }
        let encoded = encoder.finish(false).unwrap();
        let meta = &encoded.footer.blocks[0];
        let block = &encoded.bytes[meta.offset as usize..meta.offset as usize + meta.len as usize];
        let lines = decode_block(block, meta, &BlockFilter::default()).unwrap();
        for (l, off) in lines.iter().zip(offsets) {
            assert_eq!(l.ts, base + Duration::seconds(off));
        }
        assert_eq!(meta.first_ts, base + Duration::seconds(-10));
        assert_eq!(meta.last_ts, base + Duration::seconds(10));
    }

    #[test]
    fn block_level_counts_sum_to_chunk_level_counts() {
        let mut encoder = ChunkEncoder::new(identity());
        let base = Utc::now();
        for i in 0..30_000 {
            let level = match i % 7 {
                0 => LogLevel::Error,
                1 | 2 => LogLevel::Warn,
                3 => LogLevel::Debug,
                _ => LogLevel::Info,
            };
            encoder
                .push(&line(
                    base + Duration::milliseconds(i),
                    level,
                    &format!("padding padding padding padding line {i}"),
                ))
                .unwrap();
        }
        let encoded = encoder.finish(false).unwrap();
        assert!(encoded.footer.blocks.len() > 1);
        let mut sum = [0u32; crate::chunk::LEVEL_COUNT];
        for b in &encoded.footer.blocks {
            assert_eq!(b.level_counts.iter().sum::<u32>(), b.line_count);
            for (acc, c) in sum.iter_mut().zip(b.level_counts) {
                *acc += c;
            }
        }
        assert_eq!(sum, encoded.footer.labels.level_counts);
        assert_eq!(sum[4], 30_000 / 7 + 1);
    }

    #[test]
    fn corrupt_crc_is_rejected() {
        let mut encoder = ChunkEncoder::new(identity());
        encoder
            .push(&line(Utc::now(), LogLevel::Info, "hi"))
            .unwrap();
        let encoded = encoder.finish(true).unwrap();
        let trailer = decode_trailer(&encoded.bytes).unwrap();
        let mut footer_bytes = encoded.bytes[trailer.footer_offset() as usize
            ..(trailer.footer_offset() + trailer.footer_len()) as usize]
            .to_vec();
        // Flip a byte inside the labels section.
        footer_bytes[0] ^= 0xFF;
        assert!(decode_footer(&footer_bytes, &trailer).is_err());
    }

    /// Hand-built block bodies with hostile column lengths: a message
    /// length near `u64::MAX` and a sum of lengths that overflows `usize`
    /// must both be reported as format errors, never wrapped into offsets
    /// that alias another column or panic on slicing.
    #[test]
    fn malformed_column_lengths_are_rejected_without_overflow() {
        let meta = |uncompressed_len: u32| BlockMeta {
            offset: 0,
            len: 0,
            uncompressed_len,
            first_ts: Utc::now(),
            last_ts: Utc::now(),
            line_count: 2,
            first_line_index: 0,
            level_mask: u16::MAX,
            level_counts: [0; crate::chunk::LEVEL_COUNT],
        };
        let filter = BlockFilter::default();

        // line_count = 2, two zero ts deltas, two levels, two streams, then
        // message lengths.
        let mut body = 2u32.to_le_bytes().to_vec();
        body.extend_from_slice(&[0, 0]); // ts deltas
        body.extend_from_slice(&[2, 2]); // levels
        body.extend_from_slice(&[0, 0]); // streams
        let prefix = body.clone();

        // One absurd length.
        put_varint(&mut body, u64::MAX);
        put_varint(&mut body, 0);
        let compressed = zstd::encode_all(body.as_slice(), 1).unwrap();
        let err = decode_block(&compressed, &meta(body.len() as u32), &filter).unwrap_err();
        assert!(
            matches!(err, LogAggregatorError::ChunkFormat { .. }),
            "{err}"
        );

        // Two lengths whose sum wraps usize but each of which is "large".
        let mut body = prefix.clone();
        put_varint(&mut body, (usize::MAX / 2 + 1) as u64);
        put_varint(&mut body, (usize::MAX / 2 + 1) as u64);
        let compressed = zstd::encode_all(body.as_slice(), 1).unwrap();
        let err = decode_block(&compressed, &meta(body.len() as u32), &filter).unwrap_err();
        assert!(
            matches!(err, LogAggregatorError::ChunkFormat { .. }),
            "{err}"
        );

        // A well-formed body of the same shape still decodes.
        let mut body = prefix;
        put_varint(&mut body, 1);
        put_varint(&mut body, 1);
        body.extend_from_slice(b"ab");
        put_varint(&mut body, 0);
        put_varint(&mut body, 0);
        let compressed = zstd::encode_all(body.as_slice(), 1).unwrap();
        let lines = decode_block(&compressed, &meta(body.len() as u32), &filter).unwrap();
        assert_eq!(lines.len(), 2);
        assert_eq!(lines[1].message, "b");
    }

    #[test]
    fn wrong_magic_is_rejected() {
        let mut encoder = ChunkEncoder::new(identity());
        encoder
            .push(&line(Utc::now(), LogLevel::Info, "hi"))
            .unwrap();
        let encoded = encoder.finish(true).unwrap();
        let mut bytes = encoded.bytes.clone();
        let len = bytes.len();
        bytes[len - 1] ^= 0xFF;
        assert!(decode_trailer(&bytes).is_err());
    }

    #[test]
    fn footer_offset_and_len_slice_correctly() {
        let mut encoder = ChunkEncoder::new(identity());
        for i in 0..10 {
            encoder
                .push(&line(Utc::now(), LogLevel::Info, &format!("m{i}")))
                .unwrap();
        }
        let encoded = encoder.finish(true).unwrap();
        let trailer = decode_trailer(&encoded.bytes).unwrap();
        let start = trailer.footer_offset() as usize;
        let end = start + trailer.footer_len() as usize;
        assert_eq!(end, encoded.bytes.len());
        let footer_bytes = &encoded.bytes[start..end];
        decode_footer(footer_bytes, &trailer).expect("footer should decode");
    }

    #[test]
    fn finish_false_yields_no_bloom() {
        let mut encoder = ChunkEncoder::new(identity());
        encoder
            .push(&line(Utc::now(), LogLevel::Info, "hi"))
            .unwrap();
        let encoded = encoder.finish(false).unwrap();
        assert!(encoded.footer.bloom.is_none());
        assert_eq!(encoded.trailer.bloom_len, 0);
    }

    #[test]
    fn encoding_empty_chunk_errors() {
        let encoder = ChunkEncoder::new(identity());
        assert!(encoder.finish(true).is_err());
    }

    #[test]
    fn bloom_answers_query_entries_for_contained_message() {
        let mut encoder = ChunkEncoder::new(identity());
        encoder
            .push(&line(
                Utc::now(),
                LogLevel::Info,
                "connection refused by upstream",
            ))
            .unwrap();
        let encoded = encoder.finish(true).unwrap();
        let bloom = encoded.footer.bloom.expect("bloom should be built");
        let entries = crate::chunk::bloom::query_entries("connection refused");
        assert!(bloom.contains_all(&entries));
    }

    #[test]
    fn decode_v1_reads_hand_built_ndjson() {
        let l1 = line(Utc::now(), LogLevel::Info, "first line");
        let l2 = line(Utc::now(), LogLevel::Error, "second line");
        let ndjson = format!(
            "{}\n{}\n",
            serde_json::to_string(&l1).unwrap(),
            serde_json::to_string(&l2).unwrap()
        );
        let compressed = zstd::encode_all(ndjson.as_bytes(), 3).unwrap();
        let decoded = decode_v1(&compressed).unwrap();
        assert_eq!(decoded.len(), 2);
        assert_eq!(decoded[0].message, "first line");
        assert_eq!(decoded[0].line_index, 0);
        assert_eq!(decoded[1].message, "second line");
        assert_eq!(decoded[1].line_index, 1);
        assert_eq!(decoded[1].level, LogLevel::Error);
    }

    #[test]
    fn decode_v1_skips_blank_lines_and_errors_on_malformed() {
        let l1 = line(Utc::now(), LogLevel::Info, "ok line");
        let ndjson = format!("{}\n\nnot json\n", serde_json::to_string(&l1).unwrap());
        let compressed = zstd::encode_all(ndjson.as_bytes(), 3).unwrap();
        let err = decode_v1(&compressed).unwrap_err();
        let msg = err.to_string();
        assert!(
            msg.contains("line 3"),
            "error should name the line number: {msg}"
        );
    }
}
