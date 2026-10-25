// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Per-container log ingestion budget.
//!
//! Without a budget one misconfigured container (a managed service printing
//! metric dumps to stdout, say) can produce nearly every line a node ingests,
//! and every one of them is buffered, sealed into a chunk and written to the
//! line index. Docker's `max-size`/`max-file` bound the host's disk but not
//! what reaches this pipeline.
//!
//! Each collected container gets a token bucket for lines/s and one for
//! bytes/s, refilled at the configured rate and holding up to
//! [`BURST_SECONDS`] of it so a startup burst or a stack trace passes intact.
//! A line that finds either bucket empty is not stored.
//!
//! ## Event time, not arrival time
//!
//! The buckets run on each line's Docker timestamp, which the daemon stamps
//! when the container writes the line (the container cannot set it). What is
//! limited is therefore the rate a container *produced* lines at, not the
//! rate they reach us: after a restart, a reconnect or a first discovery the
//! collector replays a backlog as fast as Docker serves it, and a container
//! that stayed within its budget while producing that backlog keeps all of
//! it. A noisy container's backlog is held to the budget it broke.
//!
//! A bucket outlives its stream. When a stream ends its state is parked in
//! the registry for [`THROTTLED_VISIBLE_FOR`] and picked up by the next
//! stream of the same container, so a crash-looping container does not get a
//! fresh burst with every restart, and stays listed while it loops.
//!
//! ## The loss is never silent
//!
//! - Lines are counted in [`STATS_WINDOW`]-long windows aligned on event
//!   time. A window with drops ends with one synthetic `WARN` line in the
//!   container's own log, stamped with the window's last line, saying how
//!   many lines were dropped and why. It is written when the next window
//!   starts, when the stream has been quiet for a whole window, or when the
//!   stream ends ([`IngestLimiter::flush`]).
//! - Entering and leaving the throttled state is logged once each.
//! - [`IngestBudget::throttled_sources`] lists every container that dropped
//!   lines recently, with its offered rate and drop counts, for the console.
//!
//! ## Cost
//!
//! The bucket ([`IngestLimiter`]) is owned by the container's stream task, so
//! admitting a line takes no lock: two relaxed atomic loads for the limits
//! and arithmetic on the line's timestamp. The shared registry behind a mutex
//! is touched only when a stream starts or ends and when the console asks
//! for a snapshot. Memory is one small entry per streamed container, plus one
//! per container whose stream ended within the last [`THROTTLED_VISIBLE_FOR`].

use std::collections::HashMap;
use std::sync::atomic::{AtomicI64, AtomicU32, AtomicU64, AtomicU8, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use chrono::{DateTime, Utc};
use tracing::{error, info, warn};

use crate::types::{ContainerContext, LogLevel, LogLine, LogStream};

/// How many seconds of budget a bucket holds when full.
pub const BURST_SECONDS: u64 = 10;

/// Window over which the offered rate is measured and dropped lines are
/// summarised into one notice line.
pub const STATS_WINDOW: Duration = Duration::from_secs(5);
const WINDOW_MS: i64 = STATS_WINDOW.as_millis() as i64;

/// A container stays listed as throttled for this long after its last drop,
/// and an ended stream's bucket is kept this long for the next one.
pub const THROTTLED_VISIBLE_FOR: Duration = Duration::from_secs(60);
const VISIBLE_MS: i64 = THROTTLED_VISIBLE_FOR.as_millis() as i64;

/// Event time going backwards by more than this (the daemon's clock was set
/// back) re-anchors the bucket instead of waiting for time to catch up.
/// Smaller steps back (stdout and stderr interleaving) just earn no refill.
const MAX_REWIND_MS: i64 = BURST_SECONDS as i64 * 1000;

/// Which budget a dropped line exceeded.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, utoipa::ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum IngestDropReason {
    /// The container exceeded its lines-per-second budget.
    LinesPerSec,
    /// The container exceeded its bytes-per-second budget.
    BytesPerSec,
}

impl IngestDropReason {
    fn code(self) -> u8 {
        match self {
            Self::LinesPerSec => 1,
            Self::BytesPerSec => 2,
        }
    }

    fn from_code(code: u8) -> Option<Self> {
        match code {
            1 => Some(Self::LinesPerSec),
            2 => Some(Self::BytesPerSec),
            _ => None,
        }
    }
}

impl std::fmt::Display for IngestDropReason {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::LinesPerSec => write!(f, "lines_per_sec"),
            Self::BytesPerSec => write!(f, "bytes_per_sec"),
        }
    }
}

/// Whether a line may be stored.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Admission {
    Admit,
    Drop(IngestDropReason),
}

/// Where a stream's lines come from: the enrichment context plus the worker
/// node for remote containers.
#[derive(Debug, Clone)]
pub struct IngestSource {
    pub ctx: ContainerContext,
    pub node_id: Option<i32>,
    pub node_name: Option<String>,
}

/// The configured limits. `0` disables that dimension.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct IngestLimits {
    pub lines_per_sec: u32,
    pub bytes_per_sec: u64,
}

impl IngestLimits {
    pub const UNLIMITED: Self = Self {
        lines_per_sec: 0,
        bytes_per_sec: 0,
    };

    /// Limits from the `container_logs` settings, which store bytes in KiB.
    pub fn from_settings(logs: &temps_core::ContainerLogSettings) -> Self {
        Self {
            lines_per_sec: logs.ingest_lines_per_sec,
            bytes_per_sec: u64::from(logs.ingest_kib_per_sec) * 1024,
        }
    }
}

/// Counters for one container, shared between its stream task (writer) and
/// snapshots (readers). Wall-clock times: they say when we saw it.
struct SourceStats {
    source: IngestSource,
    /// Lines and bytes offered per second over the last completed window.
    offered_lines_per_sec: AtomicU64,
    offered_bytes_per_sec: AtomicU64,
    /// When that window was closed (unix ms); `0` before the first one.
    window_ended_at_ms: AtomicI64,
    dropped_lines: AtomicU64,
    dropped_bytes: AtomicU64,
    /// [`IngestDropReason::code`] of the most recent drop.
    last_reason: AtomicU8,
    /// Unix ms of the first drop in the latest window with drops; `0` = never.
    last_dropped_at_ms: AtomicI64,
    /// Unix ms the current throttled episode began; `0` = not throttled.
    throttled_since_ms: AtomicI64,
}

impl SourceStats {
    fn new(source: IngestSource) -> Self {
        Self {
            source,
            offered_lines_per_sec: AtomicU64::new(0),
            offered_bytes_per_sec: AtomicU64::new(0),
            window_ended_at_ms: AtomicI64::new(0),
            dropped_lines: AtomicU64::new(0),
            dropped_bytes: AtomicU64::new(0),
            last_reason: AtomicU8::new(0),
            last_dropped_at_ms: AtomicI64::new(0),
            throttled_since_ms: AtomicI64::new(0),
        }
    }
}

/// A container that is dropping lines because it exceeds its budget.
#[derive(Debug, Clone, serde::Serialize, utoipa::ToSchema)]
pub struct ThrottledLogSource {
    pub container_id: String,
    /// Service label, or the external service's name.
    pub service: String,
    /// `None` for external-service containers.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub project_id: Option<i32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub external_service_id: Option<i32>,
    pub env: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub deploy_id: Option<i32>,
    /// Worker node, for containers collected from a remote node.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub node_id: Option<i32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub node_name: Option<String>,
    /// What the container is producing, measured over the last few seconds;
    /// `0` once it has gone quiet.
    pub offered_lines_per_sec: u64,
    pub offered_bytes_per_sec: u64,
    /// Totals since temps started collecting the container.
    pub dropped_lines: u64,
    pub dropped_bytes: u64,
    /// Budget the most recent dropped line exceeded.
    pub reason: IngestDropReason,
    #[schema(value_type = String, format = DateTime)]
    pub throttled_since: DateTime<Utc>,
    #[schema(value_type = String, format = DateTime)]
    pub last_dropped_at: DateTime<Utc>,
}

/// One container in the registry.
struct Entry {
    stats: Arc<SourceStats>,
    /// Bumped by every new limiter, so only the latest one parks its bucket.
    generation: u64,
    /// The bucket of an ended stream and when it ended (unix ms); `None`
    /// while a stream is running.
    parked: Option<(Bucket, i64)>,
}

/// Instance-wide budget and the registry of streamed containers.
pub struct IngestBudget {
    lines_per_sec: AtomicU32,
    bytes_per_sec: AtomicU64,
    sources: Mutex<HashMap<String, Entry>>,
}

impl IngestBudget {
    pub fn new(limits: IngestLimits) -> Self {
        Self {
            lines_per_sec: AtomicU32::new(limits.lines_per_sec),
            bytes_per_sec: AtomicU64::new(limits.bytes_per_sec),
            sources: Mutex::new(HashMap::new()),
        }
    }

    pub fn limits(&self) -> IngestLimits {
        IngestLimits {
            lines_per_sec: self.lines_per_sec.load(Ordering::Relaxed),
            bytes_per_sec: self.bytes_per_sec.load(Ordering::Relaxed),
        }
    }

    /// Apply new limits; running streams pick them up on their next line.
    pub fn set_limits(&self, limits: IngestLimits) {
        self.lines_per_sec
            .store(limits.lines_per_sec, Ordering::Relaxed);
        self.bytes_per_sec
            .store(limits.bytes_per_sec, Ordering::Relaxed);
    }

    /// A limiter for one container's stream. It resumes the bucket of the
    /// container's previous stream if that ended recently, registers the
    /// container for [`Self::throttled_sources`], and parks its bucket for
    /// the next stream when dropped.
    pub fn limiter(self: &Arc<Self>, source: IngestSource) -> IngestLimiter {
        let container_id = source.ctx.container_id.clone();
        let limits = self.limits();
        let mut sources = match self.sources.lock() {
            Ok(sources) => sources,
            Err(e) => {
                error!(
                    container_id = %container_id,
                    error = %e,
                    "log ingestion budget registry is poisoned; container will not be listed when throttled"
                );
                return IngestLimiter {
                    budget: self.clone(),
                    stats: Arc::new(SourceStats::new(source)),
                    generation: 0,
                    bucket: Bucket::full(limits),
                };
            }
        };
        evict_parked(&mut sources, Utc::now().timestamp_millis());
        let entry = sources.entry(container_id).or_insert_with(|| Entry {
            stats: Arc::new(SourceStats::new(source)),
            generation: 0,
            parked: None,
        });
        entry.generation += 1;
        let bucket = match entry.parked.take() {
            Some((bucket, _)) => bucket,
            None => Bucket::full(limits),
        };
        IngestLimiter {
            budget: self.clone(),
            stats: entry.stats.clone(),
            generation: entry.generation,
            bucket,
        }
    }

    fn park(&self, stats: &Arc<SourceStats>, generation: u64, bucket: Bucket) {
        let Ok(mut sources) = self.sources.lock() else {
            return;
        };
        let now_ms = Utc::now().timestamp_millis();
        // A newer stream for the same container may already own the entry;
        // its bucket is the live one.
        if let Some(entry) = sources.get_mut(&stats.source.ctx.container_id) {
            if entry.generation == generation && Arc::ptr_eq(&entry.stats, stats) {
                entry.parked = Some((bucket, now_ms));
            }
        }
        evict_parked(&mut sources, now_ms);
    }

    /// Containers that dropped lines within [`THROTTLED_VISIBLE_FOR`],
    /// noisiest first, including ones whose stream has just ended.
    pub fn throttled_sources(&self) -> Vec<ThrottledLogSource> {
        self.throttled_sources_at(Utc::now())
    }

    fn throttled_sources_at(&self, now: DateTime<Utc>) -> Vec<ThrottledLogSource> {
        let now_ms = now.timestamp_millis();
        let stats: Vec<Arc<SourceStats>> = match self.sources.lock() {
            Ok(mut sources) => {
                evict_parked(&mut sources, now_ms);
                sources.values().map(|entry| entry.stats.clone()).collect()
            }
            Err(e) => {
                error!(error = %e, "log ingestion budget registry is poisoned; reporting no throttled containers");
                return Vec::new();
            }
        };
        let stale_ms = 2 * WINDOW_MS;

        let mut throttled: Vec<ThrottledLogSource> = stats
            .iter()
            .filter_map(|stats| {
                let last_dropped_ms = stats.last_dropped_at_ms.load(Ordering::Relaxed);
                if last_dropped_ms == 0 || now_ms - last_dropped_ms > VISIBLE_MS {
                    return None;
                }
                let reason =
                    IngestDropReason::from_code(stats.last_reason.load(Ordering::Relaxed))?;
                let window_ended_ms = stats.window_ended_at_ms.load(Ordering::Relaxed);
                let current = window_ended_ms != 0 && now_ms - window_ended_ms <= stale_ms;
                let (offered_lines, offered_bytes) = if current {
                    (
                        stats.offered_lines_per_sec.load(Ordering::Relaxed),
                        stats.offered_bytes_per_sec.load(Ordering::Relaxed),
                    )
                } else {
                    (0, 0)
                };
                let since_ms = match stats.throttled_since_ms.load(Ordering::Relaxed) {
                    0 => last_dropped_ms,
                    since => since,
                };
                let ctx = &stats.source.ctx;
                Some(ThrottledLogSource {
                    container_id: ctx.container_id.clone(),
                    service: ctx.service.clone(),
                    project_id: ctx.external_service_id.is_none().then_some(ctx.project_id),
                    external_service_id: ctx.external_service_id,
                    env: ctx.env.clone(),
                    deploy_id: ctx.deploy_id,
                    node_id: stats.source.node_id,
                    node_name: stats.source.node_name.clone(),
                    offered_lines_per_sec: offered_lines,
                    offered_bytes_per_sec: offered_bytes,
                    dropped_lines: stats.dropped_lines.load(Ordering::Relaxed),
                    dropped_bytes: stats.dropped_bytes.load(Ordering::Relaxed),
                    reason,
                    throttled_since: from_millis(since_ms),
                    last_dropped_at: from_millis(last_dropped_ms),
                })
            })
            .collect();
        throttled.sort_by(|a, b| {
            b.offered_lines_per_sec
                .cmp(&a.offered_lines_per_sec)
                .then(b.dropped_lines.cmp(&a.dropped_lines))
                .then(a.container_id.cmp(&b.container_id))
        });
        throttled
    }
}

/// Forget containers whose stream ended more than [`THROTTLED_VISIBLE_FOR`]
/// ago: their listing has expired and a bucket idle that long is full.
fn evict_parked(sources: &mut HashMap<String, Entry>, now_ms: i64) {
    sources.retain(|_, entry| match entry.parked {
        Some((_, parked_at_ms)) => now_ms - parked_at_ms <= VISIBLE_MS,
        None => true,
    });
}

fn capacity(rate: u64) -> f64 {
    (rate * BURST_SECONDS) as f64
}

fn from_millis(ms: i64) -> DateTime<Utc> {
    DateTime::from_timestamp_millis(ms).unwrap_or_default()
}

/// Drops summarised for one window, waiting to be written as a log line.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct DropNotice {
    dropped_lines: u64,
    dropped_bytes: u64,
    window_secs: u64,
    reason: IngestDropReason,
    limits: IngestLimits,
    /// Event time of the window's last line; the notice is stamped with it
    /// so it sits right after the gap it explains.
    at: DateTime<Utc>,
}

/// Lines counted in one event-time window.
#[derive(Debug, Clone, Copy)]
struct Window {
    /// Event time divided by [`WINDOW_MS`].
    index: i64,
    lines: u64,
    bytes: u64,
    dropped_lines: u64,
    dropped_bytes: u64,
    last_ts: Option<DateTime<Utc>>,
    /// Last drop reason in this window.
    reason: Option<IngestDropReason>,
}

impl Window {
    fn new(index: i64) -> Self {
        Self {
            index,
            lines: 0,
            bytes: 0,
            dropped_lines: 0,
            dropped_bytes: 0,
            last_ts: None,
            reason: None,
        }
    }
}

/// A container's token buckets and open window. Plain data, so an ended
/// stream can park it for the container's next stream.
#[derive(Debug, Clone, Copy)]
struct Bucket {
    /// Limits the buckets were last sized for.
    limits: IngestLimits,
    line_tokens: f64,
    byte_tokens: f64,
    /// Event time (unix ms) of the last refill; `None` before the first line.
    refilled_at_ms: Option<i64>,
    window: Option<Window>,
    pending_notice: Option<DropNotice>,
}

impl Bucket {
    fn full(limits: IngestLimits) -> Self {
        Self {
            limits,
            line_tokens: capacity(u64::from(limits.lines_per_sec)),
            byte_tokens: capacity(limits.bytes_per_sec),
            refilled_at_ms: None,
            window: None,
            pending_notice: None,
        }
    }
}

/// One container's limiter. Owned by its stream task, never shared.
pub struct IngestLimiter {
    budget: Arc<IngestBudget>,
    stats: Arc<SourceStats>,
    generation: u64,
    bucket: Bucket,
}

impl IngestLimiter {
    /// Decide whether a line of `bytes` bytes, stamped `ts` by Docker, may be
    /// stored. A dropped line consumes nothing.
    pub fn admit(&mut self, bytes: usize, ts: DateTime<Utc>) -> Admission {
        let ts_ms = ts.timestamp_millis();
        self.advance_window(ts_ms);
        self.refill(ts_ms);

        let bytes = bytes as u64;
        let limits = self.bucket.limits;
        let lines_limited = limits.lines_per_sec > 0;
        let bytes_limited = limits.bytes_per_sec > 0;
        // A line larger than the whole byte bucket passes when the bucket is
        // full and leaves it in debt, so it is delayed, not lost forever.
        let bytes_needed = (bytes as f64).min(capacity(limits.bytes_per_sec));
        let reason = if lines_limited && self.bucket.line_tokens < 1.0 {
            Some(IngestDropReason::LinesPerSec)
        } else if bytes_limited && self.bucket.byte_tokens < bytes_needed {
            Some(IngestDropReason::BytesPerSec)
        } else {
            None
        };

        if let Some(window) = self.bucket.window.as_mut() {
            window.lines += 1;
            window.bytes += bytes;
            window.last_ts = Some(window.last_ts.map_or(ts, |last| last.max(ts)));
        }

        match reason {
            None => {
                if lines_limited {
                    self.bucket.line_tokens -= 1.0;
                }
                if bytes_limited {
                    self.bucket.byte_tokens -= bytes as f64;
                }
                Admission::Admit
            }
            Some(reason) => {
                self.record_drop(bytes, reason);
                Admission::Drop(reason)
            }
        }
    }

    /// The summary line for the last window that dropped lines, once. Write
    /// it alongside the stream's lines so the gap is explained where it is
    /// seen; it is not counted against the budget.
    pub fn take_drop_notice(&mut self) -> Option<LogLine> {
        let notice = self.bucket.pending_notice.take()?;
        let source = &self.stats.source;
        let ctx = &source.ctx;
        Some(LogLine {
            ts: notice.at,
            stream: LogStream::Stderr,
            level: LogLevel::Warn,
            msg: notice_message(&notice),
            fields: Some(serde_json::json!({
                "temps.logs_dropped": notice.dropped_lines,
                "temps.logs_dropped_bytes": notice.dropped_bytes,
                "temps.logs_dropped_reason": notice.reason.to_string(),
            })),
            container_id: ctx.container_id.clone(),
            service: ctx.service.clone(),
            env: ctx.env.clone(),
            project_id: ctx.project_id,
            external_service_id: ctx.external_service_id,
            deploy_id: ctx.deploy_id,
            node_id: source.node_id,
            node_name: source.node_name.clone(),
        })
    }

    /// Close the open window now instead of with the next line, and return
    /// its notice if it dropped lines. Call it when the stream has been
    /// quiet for a [`STATS_WINDOW`] or has ended, so a gap is explained
    /// without waiting for a line that may never come.
    pub fn flush(&mut self) -> Option<LogLine> {
        self.close_window();
        self.take_drop_notice()
    }

    fn advance_window(&mut self, ts_ms: i64) {
        let index = ts_ms.div_euclid(WINDOW_MS);
        match self.bucket.window {
            // Same window, or a line slightly out of order: count it here.
            Some(window)
                if index <= window.index && window.index - index <= MAX_REWIND_MS / WINDOW_MS => {}
            Some(_) => {
                self.close_window();
                self.bucket.window = Some(Window::new(index));
            }
            None => self.bucket.window = Some(Window::new(index)),
        }
    }

    fn refill(&mut self, ts_ms: i64) {
        let limits = self.budget.limits();
        let bucket = &mut self.bucket;
        if limits != bucket.limits {
            // A changed budget takes effect at once. A lowered one cannot
            // leave more than its own burst in the bucket; one that was off
            // starts full, as a new stream would.
            let lines_cap = capacity(u64::from(limits.lines_per_sec));
            let bytes_cap = capacity(limits.bytes_per_sec);
            bucket.line_tokens = if bucket.limits.lines_per_sec == 0 {
                lines_cap
            } else {
                bucket.line_tokens.min(lines_cap)
            };
            bucket.byte_tokens = if bucket.limits.bytes_per_sec == 0 {
                bytes_cap
            } else {
                bucket.byte_tokens.min(bytes_cap)
            };
            bucket.limits = limits;
        }
        let elapsed_ms = match bucket.refilled_at_ms {
            None => {
                bucket.refilled_at_ms = Some(ts_ms);
                return;
            }
            Some(previous) => ts_ms - previous,
        };
        if elapsed_ms <= 0 {
            if elapsed_ms < -MAX_REWIND_MS {
                bucket.refilled_at_ms = Some(ts_ms);
            }
            return;
        }
        bucket.refilled_at_ms = Some(ts_ms);
        let elapsed = elapsed_ms as f64 / 1000.0;
        bucket.line_tokens = (bucket.line_tokens + elapsed * f64::from(limits.lines_per_sec))
            .min(capacity(u64::from(limits.lines_per_sec)));
        bucket.byte_tokens = (bucket.byte_tokens + elapsed * limits.bytes_per_sec as f64)
            .min(capacity(limits.bytes_per_sec));
    }

    fn record_drop(&mut self, bytes: u64, reason: IngestDropReason) {
        let stats = &self.stats;
        stats.dropped_lines.fetch_add(1, Ordering::Relaxed);
        stats.dropped_bytes.fetch_add(bytes, Ordering::Relaxed);
        stats.last_reason.store(reason.code(), Ordering::Relaxed);
        let Some(window) = self.bucket.window.as_mut() else {
            return;
        };
        if window.dropped_lines == 0 {
            // First drop of this window: stamp it so the container shows as
            // throttled right away, not only when the window closes.
            let now_ms = Utc::now().timestamp_millis();
            let previous_drop_ms = stats.last_dropped_at_ms.swap(now_ms, Ordering::Relaxed);
            let since_ms = stats.throttled_since_ms.load(Ordering::Relaxed);
            if since_ms == 0 || now_ms - previous_drop_ms > VISIBLE_MS {
                stats.throttled_since_ms.store(now_ms, Ordering::Relaxed);
                let ctx = &stats.source.ctx;
                warn!(
                    container_id = %ctx.container_id,
                    project_id = ctx.project_id,
                    external_service_id = ?ctx.external_service_id,
                    service = %ctx.service,
                    node_id = ?stats.source.node_id,
                    reason = %reason,
                    lines_per_sec_budget = self.bucket.limits.lines_per_sec,
                    bytes_per_sec_budget = self.bucket.limits.bytes_per_sec,
                    "Container exceeds its log ingestion budget; dropping lines"
                );
            }
        }
        window.dropped_lines += 1;
        window.dropped_bytes += bytes;
        window.reason = Some(reason);
    }

    fn close_window(&mut self) {
        let Some(window) = self.bucket.window.take() else {
            return;
        };
        if window.lines == 0 {
            return;
        }
        let stats = &self.stats;
        let secs = STATS_WINDOW.as_secs_f64();
        stats.offered_lines_per_sec.store(
            (window.lines as f64 / secs).round() as u64,
            Ordering::Relaxed,
        );
        stats.offered_bytes_per_sec.store(
            (window.bytes as f64 / secs).round() as u64,
            Ordering::Relaxed,
        );
        stats
            .window_ended_at_ms
            .store(Utc::now().timestamp_millis(), Ordering::Relaxed);

        match window.reason.zip(window.last_ts) {
            Some((reason, at)) => {
                let notice = DropNotice {
                    dropped_lines: window.dropped_lines,
                    dropped_bytes: window.dropped_bytes,
                    window_secs: STATS_WINDOW.as_secs(),
                    reason,
                    limits: self.bucket.limits,
                    at,
                };
                // A notice not yet written (the caller skipped one) is folded
                // in rather than lost.
                self.bucket.pending_notice = Some(match self.bucket.pending_notice.take() {
                    Some(earlier) => DropNotice {
                        dropped_lines: earlier.dropped_lines + notice.dropped_lines,
                        dropped_bytes: earlier.dropped_bytes + notice.dropped_bytes,
                        window_secs: earlier.window_secs + notice.window_secs,
                        ..notice
                    },
                    None => notice,
                });
            }
            None => {
                if stats.throttled_since_ms.swap(0, Ordering::Relaxed) != 0 {
                    let ctx = &stats.source.ctx;
                    info!(
                        container_id = %ctx.container_id,
                        project_id = ctx.project_id,
                        external_service_id = ?ctx.external_service_id,
                        service = %ctx.service,
                        dropped_lines_total = stats.dropped_lines.load(Ordering::Relaxed),
                        dropped_bytes_total = stats.dropped_bytes.load(Ordering::Relaxed),
                        "Container is back within its log ingestion budget"
                    );
                }
            }
        }
    }
}

impl Drop for IngestLimiter {
    fn drop(&mut self) {
        self.budget.park(&self.stats, self.generation, self.bucket);
    }
}

fn notice_message(notice: &DropNotice) -> String {
    let budget = match notice.reason {
        IngestDropReason::LinesPerSec => format!("{} lines/s", notice.limits.lines_per_sec),
        IngestDropReason::BytesPerSec => {
            format!("{} KiB/s", notice.limits.bytes_per_sec / 1024)
        }
    };
    format!(
        "temps: dropped {} log lines ({} bytes) in the {}s up to this line: this container \
         exceeded its log ingestion budget of {}. Raise it under Settings > Monitoring > \
         Container logs, or make the container log less.",
        notice.dropped_lines, notice.dropped_bytes, notice.window_secs, budget
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn source(container_id: &str) -> IngestSource {
        IngestSource {
            ctx: ContainerContext {
                project_id: 3,
                external_service_id: None,
                env: "5".into(),
                service: "web".into(),
                container_id: container_id.into(),
                deploy_id: Some(9),
            },
            node_id: None,
            node_name: None,
        }
    }

    fn budget(lines_per_sec: u32, bytes_per_sec: u64) -> Arc<IngestBudget> {
        Arc::new(IngestBudget::new(IngestLimits {
            lines_per_sec,
            bytes_per_sec,
        }))
    }

    /// An event time on a window boundary, so window counts are exact.
    fn t0() -> DateTime<Utc> {
        from_millis(1_790_000_000_000)
    }

    fn at(start: DateTime<Utc>, offset: Duration) -> DateTime<Utc> {
        start + chrono::Duration::from_std(offset).unwrap_or_default()
    }

    /// Offer `n` lines 1µs apart: a burst no refill can soften.
    fn burst(limiter: &mut IngestLimiter, start: DateTime<Utc>, n: u64, bytes: usize) -> u64 {
        (0..n)
            .filter(|i| {
                limiter.admit(bytes, at(start, Duration::from_micros(*i))) == Admission::Admit
            })
            .count() as u64
    }

    /// Offer lines produced at `rate` lines/s of `bytes` bytes each for
    /// `secs` seconds from `start` (event time), all at once as a replay
    /// would, and return how many were admitted.
    fn offer(
        limiter: &mut IngestLimiter,
        start: DateTime<Utc>,
        rate: u64,
        secs: u64,
        bytes: usize,
    ) -> u64 {
        let step = Duration::from_secs(1) / rate as u32;
        let mut admitted = 0;
        for i in 0..rate * secs {
            if limiter.admit(bytes, at(start, step * i as u32)) == Admission::Admit {
                admitted += 1;
            }
        }
        admitted
    }

    // The acceptance case: a container logging 1,000 lines/s against a
    // 100 lines/s budget stores no more than the budget plus one burst.
    #[test]
    fn noisy_container_cannot_exceed_its_line_budget() {
        let budget = budget(100, 0);
        let mut limiter = budget.limiter(source("noisy"));

        let secs = 60;
        let admitted = offer(&mut limiter, t0(), 1_000, secs, 80);

        let ceiling = 100 * secs + 100 * BURST_SECONDS;
        assert!(
            admitted <= ceiling,
            "admitted {admitted} lines, more than the {ceiling} the budget allows"
        );
        assert!(
            admitted >= 100 * secs,
            "admitted only {admitted} lines; the budget should still be used in full"
        );
    }

    #[test]
    fn noisy_container_cannot_exceed_its_byte_budget() {
        let budget = budget(0, 10 * 1024);
        let mut limiter = budget.limiter(source("noisy"));

        let secs = 30;
        let admitted = offer(&mut limiter, t0(), 200, secs, 1024);

        let ceiling = (10 * secs) + 10 * BURST_SECONDS;
        assert!(admitted <= ceiling, "admitted {admitted} 1 KiB lines");
        let throttled = budget.throttled_sources();
        assert_eq!(
            throttled.first().map(|t| t.reason),
            Some(IngestDropReason::BytesPerSec),
            "an exhausted byte bucket must name the byte budget"
        );
    }

    // After a restart or a reconnect the collector replays a backlog as fast
    // as Docker serves it. A container that stayed within its budget while
    // producing that backlog must keep every line of it.
    #[test]
    fn a_replayed_backlog_within_budget_is_stored_in_full() {
        let budget = budget(100, 64 * 1024);
        let mut limiter = budget.limiter(source("healthy"));

        // Ten minutes at 80 lines/s of 200 bytes, offered back to back.
        let admitted = offer(&mut limiter, t0(), 80, 600, 200);

        assert_eq!(admitted, 80 * 600, "a healthy backlog lost lines");
        assert!(limiter.flush().is_none());
        assert!(budget.throttled_sources().is_empty());
    }

    // The same replay of a noisy container's backlog is held to the budget
    // it broke while producing it.
    #[test]
    fn a_replayed_noisy_backlog_is_still_held_to_the_budget() {
        let budget = budget(100, 0);
        let mut limiter = budget.limiter(source("noisy"));
        let admitted = offer(&mut limiter, t0(), 1_000, 120, 80);
        assert!(admitted <= 100 * 120 + 100 * BURST_SECONDS);
    }

    // A container within its budget, including a burst on startup, loses
    // nothing and is never listed.
    #[test]
    fn quiet_container_and_startup_burst_are_never_dropped() {
        let budget = budget(100, 1024 * 1024);
        let mut limiter = budget.limiter(source("quiet"));

        for i in 0..900u64 {
            assert_eq!(
                limiter.admit(200, at(t0(), Duration::from_micros(i))),
                Admission::Admit,
                "startup burst line {i} was dropped"
            );
        }
        let later = at(t0(), Duration::from_secs(20));
        assert_eq!(offer(&mut limiter, later, 50, 30, 200), 50 * 30);
        assert!(limiter.flush().is_none());
        assert!(budget.throttled_sources().is_empty());
    }

    #[test]
    fn zero_limits_mean_unlimited() {
        let budget = budget(0, 0);
        let mut limiter = budget.limiter(source("any"));
        assert_eq!(offer(&mut limiter, t0(), 5_000, 2, 4096), 10_000);
    }

    // A line bigger than the whole byte bucket would otherwise never fit.
    // It passes when the bucket is full and pays for itself afterwards.
    #[test]
    fn a_line_larger_than_the_byte_bucket_is_delayed_not_lost() {
        // 1 KiB/s holds 10 KiB; Docker splits lines at 16 KiB.
        let budget = budget(0, 1024);
        let mut limiter = budget.limiter(source("big"));
        let line = 16 * 1024;

        assert_eq!(limiter.admit(line, t0()), Admission::Admit);
        assert_eq!(
            limiter.admit(line, at(t0(), Duration::from_secs(1))),
            Admission::Drop(IngestDropReason::BytesPerSec),
            "the debt must be paid before the next one"
        );
        // 6 KiB of debt, then a full 10 KiB bucket: 16 s at 1 KiB/s.
        assert_eq!(
            limiter.admit(line, at(t0(), Duration::from_secs(17))),
            Admission::Admit
        );
    }

    // The operator can see which container is throttled and how hard it is
    // pushing, and the container's own log explains the gap.
    #[test]
    fn throttled_container_is_reported_and_its_log_explains_the_gap() {
        let budget = budget(100, 0);
        let mut noisy = budget.limiter(source("noisy"));
        let mut quiet = budget.limiter(source("quiet"));

        offer(&mut noisy, t0(), 1_000, 6, 80);
        offer(&mut quiet, t0(), 10, 6, 80);

        let throttled = budget.throttled_sources();
        assert_eq!(
            throttled.len(),
            1,
            "only the noisy container: {throttled:?}"
        );
        let entry = &throttled[0];
        assert_eq!(entry.container_id, "noisy");
        assert_eq!(entry.project_id, Some(3));
        assert_eq!(entry.reason, IngestDropReason::LinesPerSec);
        assert_eq!(
            entry.offered_lines_per_sec, 1_000,
            "offered rate should reflect the 1,000 lines/s produced"
        );
        assert!(entry.dropped_lines > 0);
        assert_eq!(entry.dropped_bytes, entry.dropped_lines * 80);

        let notice = noisy
            .take_drop_notice()
            .expect("a window with drops must leave a notice line");
        assert_eq!(notice.level, LogLevel::Warn);
        assert_eq!(notice.container_id, "noisy");
        assert_eq!(notice.deploy_id, Some(9));
        assert!(notice.msg.contains("100 lines/s"), "{}", notice.msg);
        assert!(
            notice.msg.contains("in the 5s up to this line"),
            "{}",
            notice.msg
        );
        let fields = notice.fields.expect("notice carries structured counts");
        assert!(fields["temps.logs_dropped"].as_u64().unwrap_or(0) > 0);
        assert_eq!(fields["temps.logs_dropped_reason"], "lines_per_sec");
        assert!(
            noisy.take_drop_notice().is_none(),
            "a notice is written once"
        );
    }

    // A notice belongs where the gap is: stamped with the last line of the
    // window it summarises, and its rate measured over that window only,
    // however long the container stays quiet afterwards.
    #[test]
    fn a_notice_is_stamped_with_its_window_not_the_next_line() {
        let budget = budget(10, 0);
        let mut limiter = budget.limiter(source("bursty"));
        // 500 lines at once: the 100-line burst passes.
        assert_eq!(burst(&mut limiter, t0(), 500, 80), 100);
        let last_offered = at(t0(), Duration::from_micros(499));

        // An hour of silence, then one line.
        let hour_later = at(t0(), Duration::from_secs(3_600));
        assert_eq!(limiter.admit(80, hour_later), Admission::Admit);

        let notice = limiter.take_drop_notice().expect("the burst dropped lines");
        assert_eq!(notice.ts, last_offered);
        assert!(notice.ts < hour_later);
        assert!(
            notice.msg.contains("dropped 400 log lines"),
            "{}",
            notice.msg
        );
        assert!(
            notice.msg.contains("in the 5s up to this line"),
            "{}",
            notice.msg
        );
        assert_eq!(
            budget.throttled_sources()[0].offered_lines_per_sec,
            100,
            "500 lines in a 5s window, not spread over the hour"
        );
    }

    // Without a next line the window's notice comes from flush(), which the
    // collectors call when a stream goes quiet or ends.
    #[test]
    fn flush_explains_a_gap_when_no_line_follows() {
        let budget = budget(10, 0);
        let mut limiter = budget.limiter(source("noisy"));
        burst(&mut limiter, t0(), 500, 80);

        let notice = limiter.flush().expect("flush closes the open window");
        assert!(
            notice.msg.contains("dropped 400 log lines"),
            "{}",
            notice.msg
        );
        assert!(limiter.flush().is_none(), "and only once");
    }

    // Enabling a limit that was off must not start from an empty bucket and
    // drop the first lines of every container.
    #[test]
    fn enabling_a_limit_starts_with_a_full_bucket() {
        let budget = budget(0, 0);
        let mut limiter = budget.limiter(source("web"));
        offer(&mut limiter, t0(), 1_000, 1, 10);

        budget.set_limits(IngestLimits {
            lines_per_sec: 100,
            bytes_per_sec: 64 * 1024,
        });
        let later = at(t0(), Duration::from_secs(1));
        assert_eq!(
            offer(&mut limiter, later, 500, 1, 10),
            500,
            "a burst within the new limit's capacity passes"
        );
    }

    // A budget saved in settings applies to streams already running.
    #[test]
    fn lowering_the_budget_applies_to_running_streams() {
        let budget = budget(0, 0);
        let mut limiter = budget.limiter(source("noisy"));
        assert_eq!(offer(&mut limiter, t0(), 1_000, 1, 10), 1_000);

        budget.set_limits(IngestLimits {
            lines_per_sec: 10,
            bytes_per_sec: 0,
        });
        let later = at(t0(), Duration::from_secs(1));
        let admitted = offer(&mut limiter, later, 1_000, 10, 10);
        assert!(
            admitted <= 10 * 10 + 10 * BURST_SECONDS,
            "admitted {admitted} after lowering the budget to 10 lines/s"
        );
    }

    // A crash-looping container gets a new stream per restart. It must not
    // get a fresh burst each time, and stays listed between streams.
    #[test]
    fn a_restarted_stream_resumes_its_containers_bucket() {
        let budget = budget(10, 0);
        let mut first = budget.limiter(source("looping"));
        offer(&mut first, t0(), 1_000, 1, 80);
        drop(first);

        let throttled = budget.throttled_sources();
        assert_eq!(throttled.len(), 1, "still listed after its stream ended");
        let dropped_before = throttled[0].dropped_lines;

        let mut second = budget.limiter(source("looping"));
        let restart = at(t0(), Duration::from_secs(1));
        let admitted = offer(&mut second, restart, 1_000, 1, 80);
        assert!(
            admitted <= 20,
            "admitted {admitted} lines: a restart must not refill the burst"
        );
        assert!(budget.throttled_sources()[0].dropped_lines > dropped_before);
    }

    #[test]
    fn an_ended_stream_is_forgotten_once_its_listing_expires() {
        let budget = budget(1, 0);
        let mut limiter = budget.limiter(source("noisy"));
        offer(&mut limiter, t0(), 100, 1, 10);
        drop(limiter);
        assert_eq!(budget.throttled_sources().len(), 1);

        let later = Utc::now()
            + chrono::Duration::from_std(THROTTLED_VISIBLE_FOR).unwrap_or_default()
            + chrono::Duration::seconds(1);
        assert!(budget.throttled_sources_at(later).is_empty());
        let sources = budget.sources.lock().map(|s| s.len()).unwrap_or(usize::MAX);
        assert_eq!(sources, 0, "the parked bucket must be evicted");
    }

    // Stopping and restarting a stream for the same container must not let
    // the old limiter park over the new one's bucket.
    #[test]
    fn a_replaced_stream_does_not_park_over_its_successor() {
        let budget = budget(1, 0);
        let old = budget.limiter(source("cid"));
        let mut new = budget.limiter(source("cid"));
        drop(old);

        offer(&mut new, t0(), 100, 1, 10);
        assert_eq!(budget.throttled_sources().len(), 1);
        let parked = budget
            .sources
            .lock()
            .map(|s| s.get("cid").is_some_and(|e| e.parked.is_some()))
            .unwrap_or(true);
        assert!(!parked, "the running stream's entry must not be parked");
    }

    #[test]
    fn throttled_listing_expires_after_the_container_calms_down() {
        let budget = budget(1, 0);
        let mut limiter = budget.limiter(source("noisy"));
        offer(&mut limiter, t0(), 100, 1, 10);

        let later = Utc::now()
            + chrono::Duration::from_std(THROTTLED_VISIBLE_FOR).unwrap_or_default()
            + chrono::Duration::seconds(1);
        assert!(budget.throttled_sources_at(later).is_empty());
    }

    // Docker's clock set back an hour must not stall the container until
    // event time catches up.
    #[test]
    fn a_clock_set_back_reanchors_the_bucket() {
        let budget = budget(10, 0);
        let mut limiter = budget.limiter(source("web"));
        offer(&mut limiter, t0(), 10, 5, 10);

        let rewound = t0() - chrono::Duration::hours(1);
        offer(&mut limiter, rewound, 1, 1, 10);
        let admitted = offer(
            &mut limiter,
            at(rewound, Duration::from_secs(2)),
            10,
            60,
            10,
        );
        assert!(admitted >= 590, "admitted only {admitted} after the rewind");
    }

    #[test]
    fn limits_from_settings_convert_kib_to_bytes() {
        let logs = temps_core::ContainerLogSettings {
            ingest_lines_per_sec: 300,
            ingest_kib_per_sec: 128,
            ..Default::default()
        };
        assert_eq!(
            IngestLimits::from_settings(&logs),
            IngestLimits {
                lines_per_sec: 300,
                bytes_per_sec: 128 * 1024,
            }
        );
    }
}
