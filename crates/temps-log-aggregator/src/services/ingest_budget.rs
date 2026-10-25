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
//! A line that finds either bucket empty is not stored. The loss is never
//! silent:
//!
//! - every [`STATS_WINDOW`] with drops ends with one synthetic `WARN` line in
//!   the container's own log saying how many lines were dropped and why
//!   (written with the container's next line, so a window cut short by the
//!   stream ending is counted below but gets no line);
//! - entering and leaving the throttled state is logged once each;
//! - [`IngestBudget::throttled_sources`] lists every container dropping lines
//!   now, with its offered rate and drop counts, for the log explorer.
//!
//! ## Cost
//!
//! The bucket ([`IngestLimiter`]) is owned by the container's stream task, so
//! admitting a line takes no lock: two relaxed atomic loads for the limits, a
//! clock read, and arithmetic. The shared registry behind a mutex is touched
//! only when a stream starts or ends and when the capabilities endpoint asks
//! for a snapshot. Memory is one small entry per streamed container.

use std::collections::HashMap;
use std::sync::atomic::{AtomicI64, AtomicU32, AtomicU64, AtomicU8, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use chrono::{DateTime, Utc};
use tracing::{error, info, warn};

use crate::types::{ContainerContext, LogLevel, LogLine, LogStream};

/// How many seconds of budget a bucket holds when full.
pub const BURST_SECONDS: u64 = 10;

/// Window over which the offered rate is measured and dropped lines are
/// summarised into one notice line.
pub const STATS_WINDOW: Duration = Duration::from_secs(5);

/// A container stays listed as throttled for this long after its last drop.
pub const THROTTLED_VISIBLE_FOR: Duration = Duration::from_secs(60);

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

/// Counters for one streamed container, shared between its stream task
/// (writer) and snapshots (readers).
struct SourceStats {
    source: IngestSource,
    /// Lines and bytes offered per second over the last completed window.
    offered_lines_per_sec: AtomicU64,
    offered_bytes_per_sec: AtomicU64,
    /// When that window ended (unix ms); `0` before the first one.
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
    /// Totals since its log stream started.
    pub dropped_lines: u64,
    pub dropped_bytes: u64,
    /// Budget the most recent dropped line exceeded.
    pub reason: IngestDropReason,
    #[schema(value_type = String, format = DateTime)]
    pub throttled_since: DateTime<Utc>,
    #[schema(value_type = String, format = DateTime)]
    pub last_dropped_at: DateTime<Utc>,
}

/// Instance-wide budget and the registry of streamed containers.
pub struct IngestBudget {
    lines_per_sec: AtomicU32,
    bytes_per_sec: AtomicU64,
    sources: Mutex<HashMap<String, Arc<SourceStats>>>,
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

    /// A limiter for one container's stream. It registers the container for
    /// [`Self::throttled_sources`] and unregisters it when dropped.
    pub fn limiter(self: &Arc<Self>, source: IngestSource) -> IngestLimiter {
        self.limiter_at(source, Instant::now())
    }

    fn limiter_at(self: &Arc<Self>, source: IngestSource, now: Instant) -> IngestLimiter {
        let container_id = source.ctx.container_id.clone();
        let stats = Arc::new(SourceStats::new(source));
        match self.sources.lock() {
            Ok(mut sources) => {
                sources.insert(container_id, stats.clone());
            }
            Err(e) => error!(
                container_id = %container_id,
                error = %e,
                "log ingestion budget registry is poisoned; container will not be listed when throttled"
            ),
        }
        let limits = self.limits();
        IngestLimiter {
            budget: self.clone(),
            stats,
            limits,
            line_tokens: capacity(u64::from(limits.lines_per_sec)),
            byte_tokens: capacity(limits.bytes_per_sec),
            refilled_at: now,
            window_started: now,
            window_lines: 0,
            window_bytes: 0,
            window_dropped_lines: 0,
            window_dropped_bytes: 0,
            pending_notice: None,
        }
    }

    fn unregister(&self, stats: &Arc<SourceStats>) {
        if let Ok(mut sources) = self.sources.lock() {
            let container_id = &stats.source.ctx.container_id;
            // A newer stream for the same container may have replaced this
            // entry; leave that one alone.
            if sources
                .get(container_id)
                .is_some_and(|current| Arc::ptr_eq(current, stats))
            {
                sources.remove(container_id);
            }
        }
    }

    /// Containers that dropped lines within [`THROTTLED_VISIBLE_FOR`],
    /// noisiest first.
    pub fn throttled_sources(&self) -> Vec<ThrottledLogSource> {
        self.throttled_sources_at(Utc::now())
    }

    fn throttled_sources_at(&self, now: DateTime<Utc>) -> Vec<ThrottledLogSource> {
        let stats: Vec<Arc<SourceStats>> = match self.sources.lock() {
            Ok(sources) => sources.values().cloned().collect(),
            Err(e) => {
                error!(error = %e, "log ingestion budget registry is poisoned; reporting no throttled containers");
                return Vec::new();
            }
        };
        let now_ms = now.timestamp_millis();
        let visible_ms = THROTTLED_VISIBLE_FOR.as_millis() as i64;
        let stale_ms = 2 * STATS_WINDOW.as_millis() as i64;

        let mut throttled: Vec<ThrottledLogSource> = stats
            .iter()
            .filter_map(|stats| {
                let last_dropped_ms = stats.last_dropped_at_ms.load(Ordering::Relaxed);
                if last_dropped_ms == 0 || now_ms - last_dropped_ms > visible_ms {
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
}

/// One container's token buckets. Owned by its stream task, never shared.
pub struct IngestLimiter {
    budget: Arc<IngestBudget>,
    stats: Arc<SourceStats>,
    /// Limits the buckets were last sized for.
    limits: IngestLimits,
    line_tokens: f64,
    byte_tokens: f64,
    refilled_at: Instant,
    window_started: Instant,
    window_lines: u64,
    window_bytes: u64,
    window_dropped_lines: u64,
    window_dropped_bytes: u64,
    pending_notice: Option<DropNotice>,
}

impl IngestLimiter {
    /// Decide whether a line of `bytes` bytes may be stored. A dropped line
    /// consumes nothing.
    pub fn admit(&mut self, bytes: usize) -> Admission {
        self.admit_at(bytes, Instant::now())
    }

    fn admit_at(&mut self, bytes: usize, now: Instant) -> Admission {
        if now.duration_since(self.window_started) >= STATS_WINDOW {
            self.close_window(now);
        }
        self.refill(now);

        let bytes = bytes as u64;
        self.window_lines += 1;
        self.window_bytes += bytes;

        let lines_limited = self.limits.lines_per_sec > 0;
        let bytes_limited = self.limits.bytes_per_sec > 0;
        let reason = if lines_limited && self.line_tokens < 1.0 {
            Some(IngestDropReason::LinesPerSec)
        } else if bytes_limited && self.byte_tokens < bytes as f64 {
            Some(IngestDropReason::BytesPerSec)
        } else {
            None
        };

        match reason {
            None => {
                if lines_limited {
                    self.line_tokens -= 1.0;
                }
                if bytes_limited {
                    self.byte_tokens -= bytes as f64;
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
    pub fn take_drop_notice(&mut self, ts: DateTime<Utc>) -> Option<LogLine> {
        let notice = self.pending_notice.take()?;
        let source = &self.stats.source;
        let ctx = &source.ctx;
        Some(LogLine {
            ts,
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

    fn refill(&mut self, now: Instant) {
        let limits = self.budget.limits();
        if limits != self.limits {
            // A changed budget takes effect at once; a lowered one cannot
            // leave more than its own burst in the bucket.
            self.limits = limits;
            self.line_tokens = self
                .line_tokens
                .min(capacity(u64::from(limits.lines_per_sec)));
            self.byte_tokens = self.byte_tokens.min(capacity(limits.bytes_per_sec));
        }
        let elapsed = now.duration_since(self.refilled_at).as_secs_f64();
        self.refilled_at = now;
        if elapsed <= 0.0 {
            return;
        }
        let lines_rate = f64::from(limits.lines_per_sec);
        let bytes_rate = limits.bytes_per_sec as f64;
        self.line_tokens = (self.line_tokens + elapsed * lines_rate)
            .min(capacity(u64::from(limits.lines_per_sec)));
        self.byte_tokens =
            (self.byte_tokens + elapsed * bytes_rate).min(capacity(limits.bytes_per_sec));
    }

    fn record_drop(&mut self, bytes: u64, reason: IngestDropReason) {
        let stats = &self.stats;
        stats.dropped_lines.fetch_add(1, Ordering::Relaxed);
        stats.dropped_bytes.fetch_add(bytes, Ordering::Relaxed);
        stats.last_reason.store(reason.code(), Ordering::Relaxed);
        if self.window_dropped_lines == 0 {
            // First drop of this window: stamp it so the container shows as
            // throttled right away, not only when the window closes.
            let now_ms = Utc::now().timestamp_millis();
            stats.last_dropped_at_ms.store(now_ms, Ordering::Relaxed);
            if stats.throttled_since_ms.load(Ordering::Relaxed) == 0 {
                stats.throttled_since_ms.store(now_ms, Ordering::Relaxed);
                let ctx = &stats.source.ctx;
                warn!(
                    container_id = %ctx.container_id,
                    project_id = ctx.project_id,
                    external_service_id = ?ctx.external_service_id,
                    service = %ctx.service,
                    node_id = ?stats.source.node_id,
                    reason = %reason,
                    lines_per_sec_budget = self.limits.lines_per_sec,
                    bytes_per_sec_budget = self.limits.bytes_per_sec,
                    "Container exceeds its log ingestion budget; dropping lines"
                );
            }
        }
        self.window_dropped_lines += 1;
        self.window_dropped_bytes += bytes;
    }

    fn close_window(&mut self, now: Instant) {
        let elapsed = now.duration_since(self.window_started);
        let secs = elapsed.as_secs_f64().max(1e-3);
        let stats = &self.stats;
        stats.offered_lines_per_sec.store(
            (self.window_lines as f64 / secs).round() as u64,
            Ordering::Relaxed,
        );
        stats.offered_bytes_per_sec.store(
            (self.window_bytes as f64 / secs).round() as u64,
            Ordering::Relaxed,
        );
        stats
            .window_ended_at_ms
            .store(Utc::now().timestamp_millis(), Ordering::Relaxed);

        if self.window_dropped_lines > 0 {
            let reason = IngestDropReason::from_code(stats.last_reason.load(Ordering::Relaxed))
                .unwrap_or(IngestDropReason::LinesPerSec);
            let notice = DropNotice {
                dropped_lines: self.window_dropped_lines,
                dropped_bytes: self.window_dropped_bytes,
                window_secs: elapsed.as_secs().max(1),
                reason,
                limits: self.limits,
            };
            // A notice not yet written (the caller skipped one) is folded in
            // rather than lost.
            self.pending_notice = Some(match self.pending_notice.take() {
                Some(earlier) => DropNotice {
                    dropped_lines: earlier.dropped_lines + notice.dropped_lines,
                    dropped_bytes: earlier.dropped_bytes + notice.dropped_bytes,
                    window_secs: earlier.window_secs + notice.window_secs,
                    ..notice
                },
                None => notice,
            });
        } else if stats.throttled_since_ms.swap(0, Ordering::Relaxed) != 0 {
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

        self.window_started = now;
        self.window_lines = 0;
        self.window_bytes = 0;
        self.window_dropped_lines = 0;
        self.window_dropped_bytes = 0;
    }
}

impl Drop for IngestLimiter {
    fn drop(&mut self) {
        self.budget.unregister(&self.stats);
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
        "temps: dropped {} log lines ({} bytes) in the last {}s: this container exceeded its \
         log ingestion budget of {}. Raise it under Settings > Monitoring > Container logs, or \
         make the container log less.",
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

    /// Offer `rate` lines/s of `bytes` bytes each for `secs` seconds, evenly
    /// spaced, and return how many were admitted.
    fn offer(
        limiter: &mut IngestLimiter,
        start: Instant,
        rate: u64,
        secs: u64,
        bytes: usize,
    ) -> u64 {
        let step = Duration::from_secs(1) / rate as u32;
        let mut admitted = 0;
        for i in 0..rate * secs {
            if limiter.admit_at(bytes, start + step * i as u32) == Admission::Admit {
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
        let start = Instant::now();
        let mut limiter = budget.limiter_at(source("noisy"), start);

        let secs = 60;
        let admitted = offer(&mut limiter, start, 1_000, secs, 80);

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
        let start = Instant::now();
        let mut limiter = budget.limiter_at(source("noisy"), start);

        let secs = 30;
        let admitted = offer(&mut limiter, start, 200, secs, 1024);

        let ceiling = (10 * secs) + 10 * BURST_SECONDS;
        assert!(admitted <= ceiling, "admitted {admitted} 1 KiB lines");
        let throttled = budget.throttled_sources();
        assert_eq!(
            throttled.first().map(|t| t.reason),
            Some(IngestDropReason::BytesPerSec),
            "an exhausted byte bucket must name the byte budget"
        );
    }

    // A container within its budget, including a burst on startup, loses
    // nothing and is never listed.
    #[test]
    fn quiet_container_and_startup_burst_are_never_dropped() {
        let budget = budget(100, 1024 * 1024);
        let start = Instant::now();
        let mut limiter = budget.limiter_at(source("quiet"), start);

        for i in 0..900u32 {
            assert_eq!(
                limiter.admit_at(200, start + Duration::from_micros(u64::from(i))),
                Admission::Admit,
                "startup burst line {i} was dropped"
            );
        }
        let later = start + Duration::from_secs(20);
        assert_eq!(offer(&mut limiter, later, 50, 30, 200), 50 * 30);
        assert!(limiter.take_drop_notice(Utc::now()).is_none());
        assert!(budget.throttled_sources().is_empty());
    }

    #[test]
    fn zero_limits_mean_unlimited() {
        let budget = budget(0, 0);
        let start = Instant::now();
        let mut limiter = budget.limiter_at(source("any"), start);
        assert_eq!(offer(&mut limiter, start, 5_000, 2, 4096), 10_000);
    }

    // The operator can see which container is throttled and how hard it is
    // pushing, and the container's own log explains the gap.
    #[test]
    fn throttled_container_is_reported_and_its_log_explains_the_gap() {
        let budget = budget(100, 0);
        let start = Instant::now();
        let mut noisy = budget.limiter_at(source("noisy"), start);
        let mut quiet = budget.limiter_at(source("quiet"), start);

        offer(&mut noisy, start, 1_000, 6, 80);
        offer(&mut quiet, start, 10, 6, 80);

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
        assert!(
            (900..=1_100).contains(&entry.offered_lines_per_sec),
            "offered rate {} should reflect ~1,000 lines/s",
            entry.offered_lines_per_sec
        );
        assert!(entry.dropped_lines > 0);
        assert_eq!(entry.dropped_bytes, entry.dropped_lines * 80);

        let notice = noisy
            .take_drop_notice(Utc::now())
            .expect("a window with drops must leave a notice line");
        assert_eq!(notice.level, LogLevel::Warn);
        assert_eq!(notice.container_id, "noisy");
        assert_eq!(notice.deploy_id, Some(9));
        assert!(notice.msg.contains("100 lines/s"), "{}", notice.msg);
        let fields = notice.fields.expect("notice carries structured counts");
        assert!(fields["temps.logs_dropped"].as_u64().unwrap_or(0) > 0);
        assert_eq!(fields["temps.logs_dropped_reason"], "lines_per_sec");
        assert!(
            noisy.take_drop_notice(Utc::now()).is_none(),
            "a notice is written once"
        );
    }

    #[test]
    fn container_leaves_the_list_when_its_stream_ends() {
        let budget = budget(1, 0);
        let start = Instant::now();
        let mut limiter = budget.limiter_at(source("noisy"), start);
        offer(&mut limiter, start, 100, 1, 10);
        assert_eq!(budget.throttled_sources().len(), 1);

        drop(limiter);
        assert!(budget.throttled_sources().is_empty());
    }

    // Stopping and restarting a stream for the same container must not let
    // the old limiter's drop unregister the new one.
    #[test]
    fn a_replaced_stream_does_not_unregister_its_successor() {
        let budget = budget(1, 0);
        let start = Instant::now();
        let old = budget.limiter_at(source("cid"), start);
        let mut new = budget.limiter_at(source("cid"), start);
        drop(old);

        offer(&mut new, start, 100, 1, 10);
        assert_eq!(budget.throttled_sources().len(), 1);
    }

    #[test]
    fn throttled_listing_expires_after_the_container_calms_down() {
        let budget = budget(1, 0);
        let start = Instant::now();
        let mut limiter = budget.limiter_at(source("noisy"), start);
        offer(&mut limiter, start, 100, 1, 10);

        let later = Utc::now()
            + chrono::Duration::from_std(THROTTLED_VISIBLE_FOR).unwrap()
            + chrono::Duration::seconds(1);
        assert!(budget.throttled_sources_at(later).is_empty());
    }

    // A budget saved in settings applies to streams already running.
    #[test]
    fn lowering_the_budget_applies_to_running_streams() {
        let budget = budget(0, 0);
        let start = Instant::now();
        let mut limiter = budget.limiter_at(source("noisy"), start);
        assert_eq!(offer(&mut limiter, start, 1_000, 1, 10), 1_000);

        budget.set_limits(IngestLimits {
            lines_per_sec: 10,
            bytes_per_sec: 0,
        });
        let later = start + Duration::from_secs(1);
        let admitted = offer(&mut limiter, later, 1_000, 10, 10);
        assert!(
            admitted <= 10 * 10 + 10 * BURST_SECONDS,
            "admitted {admitted} after lowering the budget to 10 lines/s"
        );
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
