// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Recording the outbound connections of workloads that opted in.
//!
//! The egress proxy is the one place every outbound connection of a
//! restricted workload passes through without the workload's cooperation,
//! so it is where a record of what the workload actually called is taken.
//! Recording is off unless the control plane marks a workload's
//! [`EgressPolicy`](super::EgressPolicy) with `record`.
//!
//! **One record per connection, when it ends**, allowed or not:
//! [`ConnectionRecord`]. It carries what the proxy saw and nothing it did
//! not: the destination as asked (`CONNECT host:port`, or the host, port,
//! method and path of a plain-HTTP forward, never its query string), how it
//! ended, the bytes each way and how long it took. Never headers, bodies or
//! query strings, and never a relay's token. The workload is the one the
//! source address belongs to, so a workload cannot attribute a record to
//! anyone else, and cannot suppress one either: it does not see the proxy.
//!
//! **Off the connection path.** The proxy hands a finished record to a
//! bounded channel with `try_send` ([`EgressRecorder`]). A full channel
//! drops the record and counts it in [`EgressStats::records_dropped`]; the
//! connection is never slowed or failed by recording. A background
//! [`EgressBatcher`] drains the channel and delivers batches to an
//! [`EgressRecordSink`] (the control plane), flushing at
//! [`MAX_BATCH_RECORDS`] records or [`MAX_BATCH_AGE`], whichever comes
//! first.
//!
//! **Repeats collapse.** A workload retrying a refused destination in a
//! loop would otherwise produce one record per attempt. Within a batch,
//! refused and failed connections with the same workload, kind,
//! destination and reason become one record whose `count` says how many
//! there were, so a retry loop costs one record per [`MAX_BATCH_AGE`].
//!
//! **Memory bound.** At most [`RECORD_CHANNEL_CAPACITY`] queued records,
//! one batch being assembled and one being delivered, each at most
//! [`MAX_BATCH_RECORDS`]. A record is a fixed struct plus a host (at most
//! 253 bytes), a method ([`MAX_RECORDED_METHOD`]) and a path
//! ([`MAX_RECORDED_PATH`]): under 1 KiB, so about 3 MiB in the worst case
//! and a few hundred KiB in practice.

use std::collections::HashMap;
use std::net::IpAddr;
use std::sync::atomic::Ordering;
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use temps_workload::WorkloadRef;
use tokio::sync::mpsc;
use tokio::time::Instant;
use tracing::{debug, warn};

use super::EgressStats;

/// The instrumentation scope of platform-observed egress records, so a
/// reader can tell them from records a workload sends about itself.
pub const RECORD_SCOPE: &str = "temps.egress";

/// Records queued between the proxy and the batcher.
pub const RECORD_CHANNEL_CAPACITY: usize = 2048;

/// The most records in one delivered batch.
pub const MAX_BATCH_RECORDS: usize = 512;

/// The longest a record waits before its batch is delivered, and the window
/// within which repeated refusals collapse.
pub const MAX_BATCH_AGE: Duration = Duration::from_secs(5);

/// The longest path recorded for a plain-HTTP forward; longer paths are cut
/// at a character boundary and end in `…`.
pub const MAX_RECORDED_PATH: usize = 256;

/// The longest method recorded; a longer one is recorded as `_OTHER`, as
/// the OpenTelemetry HTTP conventions do for unknown methods.
pub const MAX_RECORDED_METHOD: usize = 16;

/// The longest destination host a record carries (a DNS name's limit).
pub const MAX_RECORDED_HOST: usize = 253;

/// How the workload asked to go out.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ConnectionKind {
    /// `CONNECT host:port`: an HTTPS (or any TLS) tunnel. Host and port are
    /// all the proxy can see.
    Tunnel,
    /// An absolute-form `http://` request.
    Forward,
    /// A Temps relay under `/.temps/...` (models, MCP tools).
    Relay,
    /// The request was refused before its kind was known: not HTTP, not a
    /// proxy request, or a relay path that does not exist.
    Unknown,
}

impl ConnectionKind {
    pub fn as_str(self) -> &'static str {
        match self {
            ConnectionKind::Tunnel => "tunnel",
            ConnectionKind::Forward => "forward",
            ConnectionKind::Relay => "relay",
            ConnectionKind::Unknown => "unknown",
        }
    }
}

/// How a connection ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ConnectionOutcome {
    /// The proxy connected it to its destination.
    Allowed,
    /// The proxy refused it; `reason` says why.
    Refused,
    /// It was allowed, but the destination did not answer.
    Failed,
}

impl ConnectionOutcome {
    pub fn as_str(self) -> &'static str {
        match self {
            ConnectionOutcome::Allowed => "allowed",
            ConnectionOutcome::Refused => "refused",
            ConnectionOutcome::Failed => "failed",
        }
    }
}

/// Why a connection was refused or failed. Stable strings: they are what an
/// operator searches the logs for.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EgressReason {
    /// A deny rule names the destination.
    DeniedByRule,
    /// No allow rule names the destination.
    NoAllowRule,
    /// An allow rule names the destination, but not on this port.
    PortNotAllowed,
    /// The name resolved to a private, loopback, link-local or metadata
    /// address no rule allows.
    PrivateAddress,
    /// The name did not resolve.
    DidNotResolve,
    /// The proxy was at its connection limit.
    Saturated,
    /// The request head was not HTTP, too long, or not sent in time.
    MalformedRequest,
    /// The request was not a proxy request (`CONNECT`, absolute-form
    /// `http://`, or a Temps relay).
    NotAProxyRequest,
    /// The URL carried credentials (`http://user:pass@host`).
    CredentialsInUrl,
    /// A `/.temps/...` path that is not a relay this proxy serves (the git
    /// relay fails closed until a relay credential exists).
    RelayNotAllowed,
    /// No relay upstream is configured on this node.
    RelayUnavailable,
    /// The destination (or the relay upstream) did not accept the
    /// connection in time.
    UpstreamUnreachable,
}

impl EgressReason {
    pub fn as_str(self) -> &'static str {
        match self {
            EgressReason::DeniedByRule => "denied_by_rule",
            EgressReason::NoAllowRule => "no_allow_rule",
            EgressReason::PortNotAllowed => "port_not_allowed",
            EgressReason::PrivateAddress => "private_address",
            EgressReason::DidNotResolve => "did_not_resolve",
            EgressReason::Saturated => "saturated",
            EgressReason::MalformedRequest => "malformed_request",
            EgressReason::NotAProxyRequest => "not_a_proxy_request",
            EgressReason::CredentialsInUrl => "credentials_in_url",
            EgressReason::RelayNotAllowed => "relay_not_allowed",
            EgressReason::RelayUnavailable => "relay_unavailable",
            EgressReason::UpstreamUnreachable => "upstream_unreachable",
        }
    }
}

impl From<super::Refusal> for EgressReason {
    fn from(refusal: super::Refusal) -> Self {
        use super::Refusal;
        match refusal {
            // An unknown source has no workload, so nothing is recorded for
            // it; the mapping exists only to keep this match total.
            Refusal::UnknownSource => EgressReason::NotAProxyRequest,
            Refusal::Denied => EgressReason::DeniedByRule,
            Refusal::NotAllowed => EgressReason::NoAllowRule,
            Refusal::Port => EgressReason::PortNotAllowed,
            Refusal::Private => EgressReason::PrivateAddress,
            Refusal::Unresolved => EgressReason::DidNotResolve,
        }
    }
}

/// Which Temps relay a relayed connection went to. The relay token in the
/// path is a credential and is never recorded.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RelayKind {
    Model,
    Mcp,
}

impl RelayKind {
    pub fn as_str(self) -> &'static str {
        match self {
            RelayKind::Model => "model",
            RelayKind::Mcp => "mcp",
        }
    }
}

fn one() -> u32 {
    1
}

/// One outbound connection, as the proxy saw it when it ended. With
/// `count > 1`, that many refused (or failed) connections to the same
/// destination for the same reason, collapsed: `started_at_ms` is the
/// first one, `last_started_at_ms` the last.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ConnectionRecord {
    /// The workload the source address belongs to, from the proxy's table.
    pub workload: WorkloadRef,
    /// When the connection was accepted, in Unix milliseconds.
    pub started_at_ms: u64,
    /// When the last collapsed repeat was accepted; equal to
    /// `started_at_ms` for a single connection.
    #[serde(default)]
    pub last_started_at_ms: u64,
    pub duration_ms: u64,
    pub kind: ConnectionKind,
    pub outcome: ConnectionOutcome,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<EgressReason>,
    /// The destination host as the workload asked for it (lower case), or
    /// an address literal. `None` when the request never named one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub host: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub port: Option<u16>,
    /// The address the proxy connected to, for an allowed connection.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub peer: Option<IpAddr>,
    /// A plain-HTTP forward's method.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub method: Option<String>,
    /// A plain-HTTP forward's path, without its query string or fragment.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub relay: Option<RelayKind>,
    /// Bytes from the workload to the destination.
    #[serde(default)]
    pub bytes_sent: u64,
    /// Bytes from the destination to the workload.
    #[serde(default)]
    pub bytes_received: u64,
    #[serde(default = "one")]
    pub count: u32,
    /// The address the connection came from, which the workload was looked
    /// up by. Kept so whoever stores the record can check that the address
    /// still belongs to that workload: an address can pass to another
    /// workload before the table follows. Never stored with the record.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source: Option<IpAddr>,
    /// When this node's proxy made the record, on the monotonic clock: after
    /// every connection it covers was accepted. What a refresh started
    /// later than this saw is what the address belonged to then, whatever
    /// the wall clock did. Set by [`EgressRecorder::record`]; never sent.
    #[serde(skip)]
    pub recorded_at: Option<Instant>,
}

impl ConnectionRecord {
    /// This record, taken from a connection that came from `source`.
    pub fn from_source(mut self, source: IpAddr) -> Self {
        self.source = Some(source);
        self
    }
}

/// What a node delivers to the control plane: the body of
/// `POST /internal/nodes/{node_id}/egress-records`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct EgressRecordBatch {
    pub records: Vec<ConnectionRecord>,
    /// Records this node dropped since its last delivered batch (channel
    /// full, or a delivery that failed), so the control plane can say that
    /// the record is incomplete.
    #[serde(default)]
    pub dropped: u64,
}

/// Unix milliseconds now; 0 if the clock is before the epoch.
pub fn unix_millis() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or_default()
}

/// `path` without its query string or fragment, cut to
/// [`MAX_RECORDED_PATH`] bytes at a character boundary.
pub fn recorded_path(path: &str) -> String {
    let end = path.find(['?', '#']).unwrap_or(path.len());
    let path = &path[..end];
    if path.len() <= MAX_RECORDED_PATH {
        return path.to_string();
    }
    let mut cut = MAX_RECORDED_PATH;
    while !path.is_char_boundary(cut) {
        cut -= 1;
    }
    format!("{}…", &path[..cut])
}

/// The method as recorded: upper-case ASCII token, or `_OTHER`.
pub fn recorded_method(method: &str) -> String {
    if method.is_empty()
        || method.len() > MAX_RECORDED_METHOD
        || !method.bytes().all(|b| b.is_ascii_alphabetic() || b == b'-')
    {
        return "_OTHER".to_string();
    }
    method.to_ascii_uppercase()
}

/// The proxy's end of the recording channel. Cloning it is cheap; sending
/// never waits.
#[derive(Debug, Clone)]
pub struct EgressRecorder {
    tx: mpsc::Sender<ConnectionRecord>,
}

/// The batcher's end of the recording channel.
#[derive(Debug)]
pub struct EgressRecordStream {
    rx: mpsc::Receiver<ConnectionRecord>,
}

impl EgressRecordStream {
    /// The next record; `None` once every recorder is dropped.
    pub async fn recv(&mut self) -> Option<ConnectionRecord> {
        self.rx.recv().await
    }

    /// A record if one is queued, without waiting.
    pub fn try_recv(&mut self) -> Option<ConnectionRecord> {
        self.rx.try_recv().ok()
    }
}

/// A bounded recording channel of `capacity` records.
pub fn recording_channel(capacity: usize) -> (EgressRecorder, EgressRecordStream) {
    let (tx, rx) = mpsc::channel(capacity.max(1));
    (EgressRecorder { tx }, EgressRecordStream { rx })
}

impl EgressRecorder {
    /// Queues `record` without waiting. A full (or closed) channel drops it
    /// and counts the drop; the caller's connection is unaffected.
    pub fn record(&self, mut record: ConnectionRecord, stats: &EgressStats) {
        record.recorded_at.get_or_insert_with(Instant::now);
        match self.tx.try_send(record) {
            Ok(()) => {
                stats.records_emitted.fetch_add(1, Ordering::Relaxed);
            }
            Err(_) => {
                stats.records_dropped.fetch_add(1, Ordering::Relaxed);
            }
        }
    }
}

#[derive(Debug, thiserror::Error)]
pub enum RecordSinkError {
    #[error("delivering {records} egress records to {destination} failed: {reason}")]
    Delivery {
        destination: String,
        records: usize,
        reason: String,
    },
}

/// Where batches go: the control plane, over the node API on a worker, or
/// straight into the ingest service on node 0.
#[async_trait]
pub trait EgressRecordSink: Send + Sync {
    async fn deliver(&self, batch: EgressRecordBatch) -> Result<Delivered, RecordSinkError>;
}

/// What a sink did with a batch it accepted.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Delivered {
    /// Records of the batch the sink accepted but did not pass on, for a
    /// reason of its own (node 0 discards a record whose source address it
    /// cannot confirm, or that ingest no longer stores). Counted apart from
    /// the delivered ones.
    pub discarded: u64,
    /// Records the sink took but has not passed on yet. The sink counts them
    /// itself ([`EgressStats::records_delivered`] or
    /// [`EgressStats::records_discarded`]) once it has.
    pub deferred: u64,
}

/// What a refused or failed record collapses on.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
struct CollapseKey {
    workload: WorkloadRef,
    source: Option<IpAddr>,
    kind: ConnectionKind,
    outcome: ConnectionOutcome,
    reason: Option<EgressReason>,
    host: Option<String>,
    port: Option<u16>,
    method: Option<String>,
    path: Option<String>,
}

impl CollapseKey {
    fn of(record: &ConnectionRecord) -> Option<Self> {
        (record.outcome != ConnectionOutcome::Allowed).then(|| Self {
            workload: record.workload,
            source: record.source,
            kind: record.kind,
            outcome: record.outcome,
            reason: record.reason,
            host: record.host.clone(),
            port: record.port,
            method: record.method.clone(),
            path: record.path.clone(),
        })
    }
}

/// The batch being assembled.
#[derive(Debug, Default)]
pub struct PendingBatch {
    records: Vec<ConnectionRecord>,
    collapsed: HashMap<CollapseKey, usize>,
    opened: Option<Instant>,
}

impl PendingBatch {
    /// Adds `record`, merging it into an earlier refusal it repeats.
    /// Returns whether it was merged.
    pub fn push(&mut self, record: ConnectionRecord) -> bool {
        if self.opened.is_none() {
            self.opened = Some(Instant::now());
        }
        if let Some(key) = CollapseKey::of(&record) {
            if let Some(&index) = self.collapsed.get(&key) {
                if let Some(first) = self.records.get_mut(index) {
                    first.count = first.count.saturating_add(record.count.max(1));
                    first.last_started_at_ms = first
                        .last_started_at_ms
                        .max(record.last_started_at_ms.max(record.started_at_ms));
                    first.duration_ms = first.duration_ms.max(record.duration_ms);
                    first.recorded_at = first.recorded_at.max(record.recorded_at);
                    return true;
                }
            }
            self.collapsed.insert(key, self.records.len());
        }
        self.records.push(record);
        false
    }

    pub fn len(&self) -> usize {
        self.records.len()
    }

    pub fn is_empty(&self) -> bool {
        self.records.is_empty()
    }

    /// When the batch must be delivered by.
    pub fn deadline(&self, max_age: Duration) -> Option<Instant> {
        self.opened.map(|opened| opened + max_age)
    }

    pub fn take(&mut self) -> Vec<ConnectionRecord> {
        self.collapsed.clear();
        self.opened = None;
        std::mem::take(&mut self.records)
    }
}

/// Drains the recording channel into batches for a sink.
#[derive(Debug, Clone)]
pub struct EgressBatcher {
    pub max_records: usize,
    pub max_age: Duration,
}

impl Default for EgressBatcher {
    fn default() -> Self {
        Self {
            max_records: MAX_BATCH_RECORDS,
            max_age: MAX_BATCH_AGE,
        }
    }
}

impl EgressBatcher {
    /// Runs until every [`EgressRecorder`] is dropped, delivering what is
    /// left before it returns. One batch is delivered at a time; while it
    /// is, new records wait in the channel (and are dropped and counted if
    /// it fills). A failed delivery is logged and its records counted in
    /// [`EgressStats::records_undelivered`]; it is not retried, so a down
    /// control plane costs at most one batch per [`MAX_BATCH_AGE`], never
    /// unbounded memory.
    pub async fn run(
        self,
        mut stream: EgressRecordStream,
        sink: Arc<dyn EgressRecordSink>,
        stats: Arc<EgressStats>,
    ) {
        let max_records = self.max_records.max(1);
        let mut pending = PendingBatch::default();
        let mut reported_dropped = 0u64;
        loop {
            let deadline = pending.deadline(self.max_age);
            // The age is checked before the channel on every turn: a stream
            // that is never empty (identical refusals, collapsed into one
            // row) must not keep a full-age batch from being delivered.
            if deadline.is_some_and(|deadline| tokio::time::Instant::now() >= deadline) {
                self.flush(&mut pending, &sink, &stats, &mut reported_dropped)
                    .await;
                continue;
            }
            let received = match deadline {
                Some(deadline) => {
                    tokio::select! {
                        biased;
                        record = stream.rx.recv() => Some(record),
                        _ = tokio::time::sleep_until(deadline) => None,
                    }
                }
                None => Some(stream.rx.recv().await),
            };
            match received {
                Some(Some(record)) => {
                    if pending.push(record) {
                        stats.records_collapsed.fetch_add(1, Ordering::Relaxed);
                    }
                    if pending.len() >= max_records {
                        self.flush(&mut pending, &sink, &stats, &mut reported_dropped)
                            .await;
                    }
                }
                Some(None) => {
                    self.flush(&mut pending, &sink, &stats, &mut reported_dropped)
                        .await;
                    debug!("egress recording channel closed; batcher stopped");
                    return;
                }
                None => {
                    self.flush(&mut pending, &sink, &stats, &mut reported_dropped)
                        .await;
                }
            }
        }
    }

    async fn flush(
        &self,
        pending: &mut PendingBatch,
        sink: &Arc<dyn EgressRecordSink>,
        stats: &EgressStats,
        reported_dropped: &mut u64,
    ) {
        if pending.is_empty() {
            return;
        }
        let records = pending.take();
        let count = records.len() as u64;
        let lost = stats.records_dropped.load(Ordering::Relaxed)
            + stats.records_undelivered.load(Ordering::Relaxed);
        let batch = EgressRecordBatch {
            records,
            dropped: lost.saturating_sub(*reported_dropped),
        };
        match sink.deliver(batch).await {
            Ok(delivered) => {
                *reported_dropped = lost;
                let discarded = delivered.discarded.min(count);
                let deferred = delivered.deferred.min(count - discarded);
                stats
                    .records_delivered
                    .fetch_add(count - discarded - deferred, Ordering::Relaxed);
                stats
                    .records_discarded
                    .fetch_add(discarded, Ordering::Relaxed);
            }
            Err(error) => {
                stats
                    .records_undelivered
                    .fetch_add(count, Ordering::Relaxed);
                warn!(
                    records = count,
                    error = %error,
                    "egress records not delivered; they are dropped and counted"
                );
            }
        }
    }
}

/// Why a node cannot record outbound connections.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "code", rename_all = "snake_case")]
pub enum RecordingUnavailable {
    /// Recording is taken by the Linux per-node egress proxy; this node
    /// runs another operating system.
    UnsupportedPlatform { os: String },
    /// The node is Linux, but its per-node egress proxy is not carrying
    /// sandbox traffic (ADR-055 phase 7 wires it in), so there would be
    /// nothing to record.
    ProxyNotServing,
}

impl RecordingUnavailable {
    /// One sentence for an operator, naming what is missing.
    pub fn describe(&self) -> String {
        match self {
            RecordingUnavailable::UnsupportedPlatform { os } => format!(
                "the node runs {os}; outbound connections are recorded by the per-node \
                 egress proxy, which records only on Linux nodes"
            ),
            RecordingUnavailable::ProxyNotServing => "the node's per-node egress proxy is not \
                 carrying sandbox traffic yet (sandboxes still egress through their own \
                 sidecar proxy, ADR-055 phase 7), so there is nothing to record"
                .to_string(),
        }
    }
}

/// Whether a node can record outbound connections, as it reports about
/// itself on every network-policy pass.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum EgressRecordingCapability {
    Available,
    Unavailable { reason: RecordingUnavailable },
}

impl EgressRecordingCapability {
    /// What a node running on `os` reports, given whether its egress proxy
    /// serves sandbox traffic with a recorder attached.
    pub fn for_platform(os: &str, proxy_records: bool) -> Self {
        if os != "linux" {
            return Self::Unavailable {
                reason: RecordingUnavailable::UnsupportedPlatform { os: os.to_string() },
            };
        }
        if !proxy_records {
            return Self::Unavailable {
                reason: RecordingUnavailable::ProxyNotServing,
            };
        }
        Self::Available
    }

    /// [`Self::for_platform`] for the platform this binary runs on.
    pub fn detect(proxy_records: bool) -> Self {
        Self::for_platform(std::env::consts::OS, proxy_records)
    }

    pub fn is_available(&self) -> bool {
        matches!(self, Self::Available)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;
    use temps_workload::WorkloadKind;

    fn sandbox(id: i64) -> WorkloadRef {
        WorkloadRef::new(WorkloadKind::Sandbox, id)
    }

    fn refused(id: i64, host: &str, reason: EgressReason, at: u64) -> ConnectionRecord {
        ConnectionRecord {
            workload: sandbox(id),
            started_at_ms: at,
            last_started_at_ms: at,
            duration_ms: 1,
            kind: ConnectionKind::Tunnel,
            outcome: ConnectionOutcome::Refused,
            reason: Some(reason),
            host: Some(host.to_string()),
            port: Some(443),
            peer: None,
            method: None,
            path: None,
            relay: None,
            bytes_sent: 0,
            bytes_received: 0,
            count: 1,
            source: None,
            recorded_at: None,
        }
    }

    fn allowed(id: i64, host: &str, at: u64) -> ConnectionRecord {
        ConnectionRecord {
            outcome: ConnectionOutcome::Allowed,
            reason: None,
            ..refused(id, host, EgressReason::NoAllowRule, at)
        }
    }

    #[derive(Default)]
    struct Collect {
        batches: Mutex<Vec<EgressRecordBatch>>,
        fail: bool,
        /// Records of each batch it says it discarded.
        discard: u64,
    }

    #[async_trait]
    impl EgressRecordSink for Collect {
        async fn deliver(&self, batch: EgressRecordBatch) -> Result<Delivered, RecordSinkError> {
            let records = batch.records.len();
            self.batches.lock().unwrap().push(batch);
            if self.fail {
                return Err(RecordSinkError::Delivery {
                    destination: "test".into(),
                    records,
                    reason: "refused".into(),
                });
            }
            Ok(Delivered {
                discarded: self.discard,
                ..Default::default()
            })
        }
    }

    #[test]
    fn repeated_refusals_collapse_into_one_record_with_a_count() {
        let mut batch = PendingBatch::default();
        assert!(!batch.push(refused(
            1,
            "blocked.example.com",
            EgressReason::NoAllowRule,
            10
        )));
        for at in 11..=60 {
            assert!(batch.push(refused(
                1,
                "blocked.example.com",
                EgressReason::NoAllowRule,
                at
            )));
        }
        // A different destination, reason or sandbox is its own record.
        assert!(!batch.push(refused(
            1,
            "other.example.com",
            EgressReason::NoAllowRule,
            61
        )));
        assert!(!batch.push(refused(
            1,
            "blocked.example.com",
            EgressReason::PortNotAllowed,
            62
        )));
        assert!(!batch.push(refused(
            2,
            "blocked.example.com",
            EgressReason::NoAllowRule,
            63
        )));
        let records = batch.take();
        assert_eq!(records.len(), 4);
        assert_eq!(records[0].count, 51);
        assert_eq!(records[0].started_at_ms, 10);
        assert_eq!(records[0].last_started_at_ms, 60);
        assert!(records[1..].iter().all(|r| r.count == 1));
        assert!(batch.is_empty());
        // The window ends with the batch: the next repeat starts a new record.
        assert!(!batch.push(refused(
            1,
            "blocked.example.com",
            EgressReason::NoAllowRule,
            70
        )));
    }

    #[test]
    fn allowed_connections_never_collapse() {
        let mut batch = PendingBatch::default();
        for at in 0..5 {
            assert!(!batch.push(allowed(1, "api.example.com", at)));
        }
        assert_eq!(batch.len(), 5);
    }

    #[test]
    fn paths_lose_their_query_and_are_bounded() {
        assert_eq!(recorded_path("/v1/items?token=secret"), "/v1/items");
        assert_eq!(recorded_path("/a#frag"), "/a");
        assert_eq!(recorded_path("/"), "/");
        let long = format!("/{}", "é".repeat(400));
        let cut = recorded_path(&long);
        assert!(cut.len() <= MAX_RECORDED_PATH + '…'.len_utf8());
        assert!(cut.ends_with('…'));
        assert_eq!(recorded_method("get"), "GET");
        assert_eq!(recorded_method("X".repeat(40).as_str()), "_OTHER");
        assert_eq!(recorded_method("GE T"), "_OTHER");
    }

    #[test]
    fn a_full_channel_drops_and_counts_without_waiting() {
        let stats = EgressStats::default();
        let (recorder, _stream) = recording_channel(2);
        for at in 0..5 {
            recorder.record(allowed(1, "api.example.com", at), &stats);
        }
        assert_eq!(stats.records_emitted.load(Ordering::Relaxed), 2);
        assert_eq!(stats.records_dropped.load(Ordering::Relaxed), 3);
    }

    #[test]
    fn a_closed_channel_drops_and_counts() {
        let stats = EgressStats::default();
        let (recorder, stream) = recording_channel(2);
        drop(stream);
        recorder.record(allowed(1, "api.example.com", 0), &stats);
        assert_eq!(stats.records_dropped.load(Ordering::Relaxed), 1);
    }

    #[tokio::test(start_paused = true)]
    async fn a_batch_is_delivered_when_it_is_full() {
        let stats = Arc::new(EgressStats::default());
        let sink = Arc::new(Collect::default());
        let (recorder, stream) = recording_channel(64);
        let batcher = EgressBatcher {
            max_records: 3,
            max_age: Duration::from_secs(3600),
        };
        let task = tokio::spawn(batcher.run(stream, sink.clone(), stats.clone()));
        for at in 0..7 {
            recorder.record(allowed(1, "api.example.com", at), &stats);
        }
        // Two full batches go at once; the seventh record waits for its age.
        for _ in 0..100 {
            tokio::task::yield_now().await;
        }
        {
            let batches = sink.batches.lock().unwrap();
            assert_eq!(batches.len(), 2);
            assert!(batches.iter().all(|b| b.records.len() == 3));
        }
        drop(recorder);
        task.await.unwrap();
        let batches = sink.batches.lock().unwrap();
        assert_eq!(batches.len(), 3, "the rest is delivered on shutdown");
        assert_eq!(batches[2].records.len(), 1);
        assert_eq!(stats.records_delivered.load(Ordering::Relaxed), 7);
    }

    #[tokio::test(start_paused = true)]
    async fn a_batch_is_delivered_when_it_is_old_enough() {
        let stats = Arc::new(EgressStats::default());
        let sink = Arc::new(Collect::default());
        let (recorder, stream) = recording_channel(64);
        let batcher = EgressBatcher {
            max_records: 100,
            max_age: Duration::from_secs(5),
        };
        tokio::spawn(batcher.run(stream, sink.clone(), stats.clone()));
        recorder.record(allowed(1, "api.example.com", 0), &stats);
        tokio::time::sleep(Duration::from_secs(4)).await;
        assert!(
            sink.batches.lock().unwrap().is_empty(),
            "not old enough yet"
        );
        tokio::time::sleep(Duration::from_secs(2)).await;
        let batches = sink.batches.lock().unwrap();
        assert_eq!(batches.len(), 1);
        assert_eq!(batches[0].records.len(), 1);
    }

    #[tokio::test(start_paused = true)]
    async fn a_batch_past_its_age_goes_before_more_records_are_taken() {
        // A channel that is never empty (identical refusals collapse, so the
        // batch never fills) must not keep a full-age batch waiting.
        let stats = Arc::new(EgressStats::default());
        let sink = Arc::new(Collect::default());
        let (recorder, stream) = recording_channel(128);
        let batcher = EgressBatcher {
            max_records: 1000,
            max_age: Duration::from_secs(5),
        };
        let task = tokio::spawn(batcher.run(stream, sink.clone(), stats.clone()));
        recorder.record(allowed(1, "first.example.com", 0), &stats);
        for _ in 0..10 {
            tokio::task::yield_now().await;
        }
        // Waiting now; the channel fills while the deadline passes.
        for at in 1..64 {
            recorder.record(allowed(1, &format!("h{at}.example.com"), at), &stats);
        }
        tokio::time::advance(Duration::from_secs(6)).await;
        for _ in 0..10 {
            tokio::task::yield_now().await;
        }
        drop(recorder);
        task.await.unwrap();
        let batches = sink.batches.lock().unwrap();
        assert!(
            batches[0].records.len() <= 2,
            "the aged batch went at once, got {} records",
            batches[0].records.len()
        );
        let total: usize = batches.iter().map(|b| b.records.len()).sum();
        assert_eq!(total, 64);
    }

    /// What the sink accepted but discarded is not counted as delivered.
    #[tokio::test(start_paused = true)]
    async fn records_the_sink_discards_are_not_counted_as_delivered() {
        let stats = Arc::new(EgressStats::default());
        let sink = Arc::new(Collect {
            discard: 1,
            ..Default::default()
        });
        let (recorder, stream) = recording_channel(64);
        let batcher = EgressBatcher {
            max_records: 100,
            max_age: Duration::from_secs(5),
        };
        let task = tokio::spawn(batcher.run(stream, sink.clone(), stats.clone()));
        recorder.record(allowed(1, "api.example.com", 0), &stats);
        recorder.record(allowed(1, "api.example.com", 1), &stats);
        drop(recorder);
        task.await.unwrap();
        let stats = stats.snapshot();
        assert_eq!(stats.records_delivered, 1);
        assert_eq!(stats.records_discarded, 1);
    }

    #[tokio::test(start_paused = true)]
    async fn a_failed_delivery_is_counted_and_reported_with_the_next_batch() {
        let stats = Arc::new(EgressStats::default());
        let sink = Arc::new(Collect {
            fail: true,
            ..Default::default()
        });
        let (recorder, stream) = recording_channel(64);
        let batcher = EgressBatcher {
            max_records: 2,
            max_age: Duration::from_secs(5),
        };
        let task = tokio::spawn(batcher.run(stream, sink.clone(), stats.clone()));
        recorder.record(allowed(1, "api.example.com", 0), &stats);
        recorder.record(allowed(1, "api.example.com", 1), &stats);
        recorder.record(allowed(1, "api.example.com", 2), &stats);
        recorder.record(allowed(1, "api.example.com", 3), &stats);
        drop(recorder);
        task.await.unwrap();
        assert_eq!(stats.records_undelivered.load(Ordering::Relaxed), 4);
        let batches = sink.batches.lock().unwrap();
        assert_eq!(batches[0].dropped, 0);
        assert_eq!(batches[1].dropped, 2, "the first batch's loss is reported");
    }

    #[tokio::test(start_paused = true)]
    async fn refusals_in_a_retry_loop_cost_one_record_per_window() {
        let stats = Arc::new(EgressStats::default());
        let sink = Arc::new(Collect::default());
        let (recorder, stream) = recording_channel(4096);
        let batcher = EgressBatcher::default();
        let task = tokio::spawn(batcher.run(stream, sink.clone(), stats.clone()));
        for at in 0..2000 {
            recorder.record(
                refused(1, "blocked.example.com", EgressReason::NoAllowRule, at),
                &stats,
            );
        }
        drop(recorder);
        task.await.unwrap();
        let batches = sink.batches.lock().unwrap();
        assert_eq!(batches.len(), 1);
        assert_eq!(batches[0].records.len(), 1);
        assert_eq!(batches[0].records[0].count, 2000);
        assert_eq!(stats.records_collapsed.load(Ordering::Relaxed), 1999);
    }

    #[test]
    fn only_linux_nodes_with_a_serving_proxy_can_record() {
        assert!(EgressRecordingCapability::for_platform("linux", true).is_available());
        assert_eq!(
            EgressRecordingCapability::for_platform("linux", false),
            EgressRecordingCapability::Unavailable {
                reason: RecordingUnavailable::ProxyNotServing
            }
        );
        let mac = EgressRecordingCapability::for_platform("macos", true);
        assert_eq!(
            mac,
            EgressRecordingCapability::Unavailable {
                reason: RecordingUnavailable::UnsupportedPlatform { os: "macos".into() }
            }
        );
        let EgressRecordingCapability::Unavailable { reason } = mac else {
            unreachable!()
        };
        assert!(reason.describe().contains("macos"));
        let json =
            serde_json::to_string(&EgressRecordingCapability::for_platform("windows", false))
                .unwrap();
        assert_eq!(
            json,
            r#"{"status":"unavailable","reason":{"code":"unsupported_platform","os":"windows"}}"#
        );
    }

    #[test]
    fn records_round_trip_and_default_their_count() {
        let record = refused(1, "blocked.example.com", EgressReason::NoAllowRule, 5);
        let json = serde_json::to_string(&record).unwrap();
        assert_eq!(
            serde_json::from_str::<ConnectionRecord>(&json).unwrap(),
            record
        );
        let minimal: ConnectionRecord = serde_json::from_str(
            r#"{"workload":{"kind":"sandbox","id":1},"started_at_ms":1,"duration_ms":0,"kind":"tunnel","outcome":"allowed"}"#,
        )
        .unwrap();
        assert_eq!(minimal.count, 1);
    }
}
