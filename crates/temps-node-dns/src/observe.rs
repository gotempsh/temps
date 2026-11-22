// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! What a resolver saw: counters and a bounded log of recent queries
//! (ADR-095 §1).
//!
//! The query path pays a few relaxed atomic increments per query and, for
//! the queries worth logging, one `try_send` of a small entry into a bounded
//! channel. A background task drains the channel into a ring of the last
//! [`QUERY_RING_CAPACITY`] entries. A full channel drops the entry and counts
//! the drop; the counters stay exact whatever happens to the log.
//!
//! What is logged:
//! - every `*.temps.local` query, with its name;
//! - a forwarded (outside-zone) query only when it failed, and then without
//!   its name: what applications look up on the internet is not the
//!   platform's business, and such names can carry per-tenant identifiers.
//!
//! The resolver's own readiness probe is neither counted nor logged.
//!
//! [`DnsObserver::snapshot`] hands the counters and the entries newer than a
//! sequence number to whoever reports them: the agent's `/agent/dns/report`
//! on a worker, the proxy process for the control plane's resolver.

use std::collections::VecDeque;
use std::net::IpAddr;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};
use tokio::sync::mpsc;

/// The record type a query asked for, re-exported for callers building
/// [`QueryObservation`]s.
pub use hickory_proto::rr::RecordType;

/// Entries kept per resolver. At ~150 bytes each, ~75 KiB.
pub const QUERY_RING_CAPACITY: usize = 512;

/// Entries in flight between the query path and the ring. Twice the ring:
/// a burst that fills it has already overwritten the ring once.
const QUERY_CHANNEL_CAPACITY: usize = 1024;

/// Version of [`DnsReport`]'s wire shape. A reader refuses other versions.
pub const DNS_REPORT_VERSION: u32 = 1;

/// The longest valid DNS name in presentation form.
pub const MAX_QUERY_NAME_LEN: usize = 253;

/// Upper bounds of the latency histogram, in microseconds. One more bucket
/// counts everything slower than the last bound.
pub const LATENCY_BUCKET_BOUNDS_US: [u64; 5] = [1_000, 5_000, 25_000, 100_000, 500_000];

/// Number of histogram buckets: one per bound, plus overflow.
pub const LATENCY_BUCKETS: usize = LATENCY_BUCKET_BOUNDS_US.len() + 1;

/// Whether a name is in the internal zone or was forwarded upstream.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ZoneClass {
    Internal,
    External,
}

impl ZoneClass {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Internal => "internal",
            Self::External => "external",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "internal" => Some(Self::Internal),
            "external" => Some(Self::External),
            _ => None,
        }
    }

    fn index(self) -> usize {
        match self {
            Self::Internal => 0,
            Self::External => 1,
        }
    }
}

/// How a query was answered.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum QueryOutcome {
    /// NOERROR with at least one answer.
    Noerror,
    /// NOERROR, no answer: the name exists, not with that record type.
    Nodata,
    /// The name does not exist.
    Nxdomain,
    /// The resolver could not answer (broken rows, upstream failure).
    Servfail,
    /// A restricted workload asked for a name its network policy does not
    /// allow; answered NXDOMAIN on the wire (ADR-055 §9).
    Refused,
    /// Any other response code (FORMERR, NOTIMP, REFUSED from upstream).
    Error,
}

impl QueryOutcome {
    pub const ALL: [QueryOutcome; 6] = [
        Self::Noerror,
        Self::Nodata,
        Self::Nxdomain,
        Self::Servfail,
        Self::Refused,
        Self::Error,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Noerror => "noerror",
            Self::Nodata => "nodata",
            Self::Nxdomain => "nxdomain",
            Self::Servfail => "servfail",
            Self::Refused => "refused",
            Self::Error => "error",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|o| o.as_str() == value)
    }

    /// Whether the client got no usable answer because something is wrong,
    /// as opposed to an answer it may not have wanted (NXDOMAIN, NODATA).
    pub fn is_failure(self) -> bool {
        matches!(self, Self::Servfail | Self::Error)
    }

    fn index(self) -> usize {
        match self {
            Self::Noerror => 0,
            Self::Nodata => 1,
            Self::Nxdomain => 2,
            Self::Servfail => 3,
            Self::Refused => 4,
            Self::Error => 5,
        }
    }

    /// Classify a response from its code and answer count. `refused` is the
    /// authority's own policy decision, which goes out as NXDOMAIN.
    pub fn classify(rcode: hickory_proto::op::ResponseCode, answers: u16, refused: bool) -> Self {
        use hickory_proto::op::ResponseCode;
        if refused {
            return Self::Refused;
        }
        match rcode {
            ResponseCode::NoError if answers > 0 => Self::Noerror,
            ResponseCode::NoError => Self::Nodata,
            ResponseCode::NXDomain => Self::Nxdomain,
            ResponseCode::ServFail => Self::Servfail,
            _ => Self::Error,
        }
    }
}

/// One answered query, as the authority hands it to the observer.
#[derive(Debug, Clone, Copy)]
pub struct QueryObservation<'a> {
    /// The name asked, as received.
    pub name: &'a str,
    pub qtype: hickory_proto::rr::RecordType,
    pub zone: ZoneClass,
    pub outcome: QueryOutcome,
    pub latency: Duration,
    pub answers: u16,
    pub source: IpAddr,
}

/// One logged query, as kept in the ring and reported.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DnsQueryEntry {
    /// Increases by one per entry for the life of the observer.
    pub seq: u64,
    /// How long before the report was built the query was answered.
    pub age_ms: u64,
    /// The name asked; `None` for a forwarded name, which is never kept.
    pub name: Option<String>,
    /// The record type asked (`A`, `AAAA`, ...).
    pub qtype: String,
    pub zone: ZoneClass,
    pub outcome: QueryOutcome,
    pub latency_us: u64,
    pub answers: u16,
    /// The address that asked: a container on this node, usually.
    pub source: IpAddr,
}

/// Cumulative counters for one zone class.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ZoneCounters {
    pub noerror: u64,
    pub nodata: u64,
    pub nxdomain: u64,
    pub servfail: u64,
    pub refused: u64,
    pub error: u64,
    /// Sum of query latencies, in microseconds.
    pub latency_us_sum: u64,
    /// Queries per latency bucket ([`LATENCY_BUCKET_BOUNDS_US`], then overflow).
    pub latency_buckets: [u64; LATENCY_BUCKETS],
}

impl ZoneCounters {
    pub fn get(&self, outcome: QueryOutcome) -> u64 {
        match outcome {
            QueryOutcome::Noerror => self.noerror,
            QueryOutcome::Nodata => self.nodata,
            QueryOutcome::Nxdomain => self.nxdomain,
            QueryOutcome::Servfail => self.servfail,
            QueryOutcome::Refused => self.refused,
            QueryOutcome::Error => self.error,
        }
    }

    pub fn total(&self) -> u64 {
        QueryOutcome::ALL
            .into_iter()
            .fold(0u64, |sum, o| sum.saturating_add(self.get(o)))
    }
}

/// Cumulative counters since the observer started.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct DnsCounters {
    pub internal: ZoneCounters,
    pub external: ZoneCounters,
    /// Log entries dropped because the channel to the ring was full.
    pub dropped_log_entries: u64,
}

/// What [`DnsObserver::snapshot`] returns.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DnsObservation {
    pub instance_id: String,
    pub counters: DnsCounters,
    pub queries: Vec<DnsQueryEntry>,
    /// Entries newer than the reader's cursor were overwritten before this
    /// snapshot: the ring wrapped between two reads.
    pub queries_truncated: bool,
}

/// The resolver's state, filled in by whoever holds the resolver handle.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct DnsResolverState {
    /// A resolver is held and was started (cluster DNS on, bind succeeded).
    pub running: bool,
    /// Its server and sync tasks are alive.
    pub tasks_alive: bool,
    /// Where it listens (`ip:port`).
    pub listen_addrs: Vec<String>,
    pub zone_generation: i64,
    pub record_count: u64,
    /// Seconds since the last successful sync with the control plane;
    /// `None` before the first one, or when the zone is fed locally.
    pub last_sync_age_secs: Option<u64>,
    pub consecutive_sync_failures: u32,
    pub last_error: Option<String>,
}

/// `GET /agent/dns/report` (ADR-095 §2), and what the control plane's own
/// resolver produces in-process.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DnsReport {
    pub version: u32,
    /// Random per observer. A different id means the counters restarted.
    pub instance_id: String,
    /// Version of the binary that runs the resolver.
    pub build_version: String,
    pub resolver: DnsResolverState,
    pub counters: DnsCounters,
    pub queries: Vec<DnsQueryEntry>,
    pub queries_truncated: bool,
}

impl DnsReport {
    pub fn assemble(
        observation: DnsObservation,
        build_version: impl Into<String>,
        resolver: DnsResolverState,
    ) -> Self {
        Self {
            version: DNS_REPORT_VERSION,
            instance_id: observation.instance_id,
            build_version: build_version.into(),
            resolver,
            counters: observation.counters,
            queries: observation.queries,
            queries_truncated: observation.queries_truncated,
        }
    }
}

struct StoredEntry {
    seq: u64,
    at: Instant,
    name: Option<String>,
    qtype: hickory_proto::rr::RecordType,
    zone: ZoneClass,
    outcome: QueryOutcome,
    latency_us: u64,
    answers: u16,
    source: IpAddr,
}

struct PendingEntry {
    at: Instant,
    name: Option<String>,
    qtype: hickory_proto::rr::RecordType,
    zone: ZoneClass,
    outcome: QueryOutcome,
    latency_us: u64,
    answers: u16,
    source: IpAddr,
}

#[derive(Default)]
struct Ring {
    entries: VecDeque<StoredEntry>,
    next_seq: u64,
}

impl Ring {
    fn push(&mut self, entry: PendingEntry) {
        self.next_seq += 1;
        if self.entries.len() == QUERY_RING_CAPACITY {
            self.entries.pop_front();
        }
        self.entries.push_back(StoredEntry {
            seq: self.next_seq,
            at: entry.at,
            name: entry.name,
            qtype: entry.qtype,
            zone: entry.zone,
            outcome: entry.outcome,
            latency_us: entry.latency_us,
            answers: entry.answers,
            source: entry.source,
        });
    }
}

#[derive(Default)]
struct AtomicZoneCounters {
    outcomes: [AtomicU64; QueryOutcome::ALL.len()],
    latency_us_sum: AtomicU64,
    latency_buckets: [AtomicU64; LATENCY_BUCKETS],
}

impl AtomicZoneCounters {
    fn load(&self) -> ZoneCounters {
        let outcome = |o: QueryOutcome| self.outcomes[o.index()].load(Ordering::Relaxed);
        let mut latency_buckets = [0u64; LATENCY_BUCKETS];
        for (slot, counter) in latency_buckets.iter_mut().zip(&self.latency_buckets) {
            *slot = counter.load(Ordering::Relaxed);
        }
        ZoneCounters {
            noerror: outcome(QueryOutcome::Noerror),
            nodata: outcome(QueryOutcome::Nodata),
            nxdomain: outcome(QueryOutcome::Nxdomain),
            servfail: outcome(QueryOutcome::Servfail),
            refused: outcome(QueryOutcome::Refused),
            error: outcome(QueryOutcome::Error),
            latency_us_sum: self.latency_us_sum.load(Ordering::Relaxed),
            latency_buckets,
        }
    }
}

fn latency_bucket(latency_us: u64) -> usize {
    LATENCY_BUCKET_BOUNDS_US
        .iter()
        .position(|bound| latency_us <= *bound)
        .unwrap_or(LATENCY_BUCKET_BOUNDS_US.len())
}

/// Counters and recent queries for one resolver. Create it once per
/// process with [`DnsObserver::spawn`] and give it to every resolver that
/// process starts, so a resolver restart keeps its history.
pub struct DnsObserver {
    instance_id: String,
    zones: [AtomicZoneCounters; 2],
    dropped: AtomicU64,
    sender: mpsc::Sender<PendingEntry>,
    ring: Arc<Mutex<Ring>>,
}

impl std::fmt::Debug for DnsObserver {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("DnsObserver")
            .field("instance_id", &self.instance_id)
            .finish_non_exhaustive()
    }
}

fn lock(ring: &Mutex<Ring>) -> MutexGuard<'_, Ring> {
    // A panic while holding the lock leaves a ring that is still valid
    // (every mutation is a single push/pop), so keep using it.
    ring.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
}

impl DnsObserver {
    /// Create an observer and the task that drains its log channel into
    /// the ring. The task ends when the observer is dropped. Must be called
    /// within a Tokio runtime.
    pub fn spawn() -> Arc<Self> {
        let (sender, mut receiver) = mpsc::channel::<PendingEntry>(QUERY_CHANNEL_CAPACITY);
        let ring = Arc::new(Mutex::new(Ring::default()));
        let drain_ring = ring.clone();
        tokio::spawn(async move {
            let mut batch = Vec::with_capacity(64);
            while receiver.recv_many(&mut batch, 64).await > 0 {
                let mut ring = lock(&drain_ring);
                for entry in batch.drain(..) {
                    ring.push(entry);
                }
            }
        });
        Arc::new(Self {
            instance_id: uuid::Uuid::new_v4().simple().to_string(),
            zones: Default::default(),
            dropped: AtomicU64::new(0),
            sender,
            ring,
        })
    }

    pub fn instance_id(&self) -> &str {
        &self.instance_id
    }

    /// Record one answered query. Called on the query path: atomics, and
    /// one `try_send` for a query worth logging.
    pub fn record(&self, query: QueryObservation<'_>) {
        let latency_us = u64::try_from(query.latency.as_micros()).unwrap_or(u64::MAX);
        let zone = &self.zones[query.zone.index()];
        zone.outcomes[query.outcome.index()].fetch_add(1, Ordering::Relaxed);
        zone.latency_us_sum.fetch_add(latency_us, Ordering::Relaxed);
        zone.latency_buckets[latency_bucket(latency_us)].fetch_add(1, Ordering::Relaxed);

        let name = match query.zone {
            ZoneClass::Internal => Some(bounded_name(query.name)),
            // Forwarded names are never kept; a failed forward is logged
            // without one so the failure is still visible.
            ZoneClass::External if query.outcome.is_failure() => None,
            ZoneClass::External => return,
        };
        let entry = PendingEntry {
            at: Instant::now(),
            name,
            qtype: query.qtype,
            zone: query.zone,
            outcome: query.outcome,
            latency_us,
            answers: query.answers,
            source: query.source,
        };
        if self.sender.try_send(entry).is_err() {
            self.dropped.fetch_add(1, Ordering::Relaxed);
        }
    }

    /// The counters, and the logged queries newer than `after`. A cursor
    /// from another instance (or none) reads the whole ring.
    pub fn snapshot(&self, after: Option<u64>, instance: Option<&str>) -> DnsObservation {
        let cursor = match instance {
            Some(id) if id == self.instance_id => after,
            _ => None,
        };
        let now = Instant::now();
        let (queries, queries_truncated) = {
            let ring = lock(&self.ring);
            let truncated = match (cursor, ring.entries.front()) {
                (Some(after), Some(oldest)) => oldest.seq > after.saturating_add(1),
                _ => false,
            };
            let queries = ring
                .entries
                .iter()
                .filter(|e| cursor.is_none_or(|after| e.seq > after))
                .map(|e| DnsQueryEntry {
                    seq: e.seq,
                    age_ms: u64::try_from(now.saturating_duration_since(e.at).as_millis())
                        .unwrap_or(u64::MAX),
                    name: e.name.clone(),
                    qtype: e.qtype.to_string(),
                    zone: e.zone,
                    outcome: e.outcome,
                    latency_us: e.latency_us,
                    answers: e.answers,
                    source: e.source,
                })
                .collect();
            (queries, truncated)
        };
        DnsObservation {
            instance_id: self.instance_id.clone(),
            counters: DnsCounters {
                internal: self.zones[ZoneClass::Internal.index()].load(),
                external: self.zones[ZoneClass::External.index()].load(),
                dropped_log_entries: self.dropped.load(Ordering::Relaxed),
            },
            queries,
            queries_truncated,
        }
    }
}

/// Lowercased, without the trailing dot, cut to the longest valid name.
fn bounded_name(name: &str) -> String {
    let trimmed = name.trim_end_matches('.');
    let mut end = trimmed.len().min(MAX_QUERY_NAME_LEN);
    while !trimmed.is_char_boundary(end) {
        end -= 1;
    }
    trimmed[..end].to_ascii_lowercase()
}

#[cfg(test)]
mod tests {
    use super::*;
    use hickory_proto::op::ResponseCode;
    use hickory_proto::rr::RecordType;

    fn query(name: &str, zone: ZoneClass, outcome: QueryOutcome) -> QueryObservation<'_> {
        QueryObservation {
            name,
            qtype: RecordType::A,
            zone,
            outcome,
            latency: Duration::from_micros(1_500),
            answers: u16::from(outcome == QueryOutcome::Noerror),
            source: "10.237.1.5".parse().expect("ip"),
        }
    }

    async fn settle(observer: &DnsObserver, expected: usize) -> DnsObservation {
        for _ in 0..200 {
            let snapshot = observer.snapshot(None, None);
            if snapshot.queries.len() >= expected {
                return snapshot;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
        observer.snapshot(None, None)
    }

    #[test]
    fn classifies_response_codes() {
        assert_eq!(
            QueryOutcome::classify(ResponseCode::NoError, 2, false),
            QueryOutcome::Noerror
        );
        assert_eq!(
            QueryOutcome::classify(ResponseCode::NoError, 0, false),
            QueryOutcome::Nodata
        );
        assert_eq!(
            QueryOutcome::classify(ResponseCode::NXDomain, 0, false),
            QueryOutcome::Nxdomain
        );
        assert_eq!(
            QueryOutcome::classify(ResponseCode::NXDomain, 0, true),
            QueryOutcome::Refused,
            "a policy refusal goes out as NXDOMAIN but is counted as refused"
        );
        assert_eq!(
            QueryOutcome::classify(ResponseCode::ServFail, 0, false),
            QueryOutcome::Servfail
        );
        assert_eq!(
            QueryOutcome::classify(ResponseCode::FormErr, 0, false),
            QueryOutcome::Error
        );
    }

    #[test]
    fn outcome_names_round_trip() {
        for outcome in QueryOutcome::ALL {
            assert_eq!(QueryOutcome::parse(outcome.as_str()), Some(outcome));
        }
        assert_eq!(QueryOutcome::parse("bogus"), None);
        assert_eq!(ZoneClass::parse("internal"), Some(ZoneClass::Internal));
        assert_eq!(ZoneClass::parse("external"), Some(ZoneClass::External));
        assert_eq!(ZoneClass::parse(""), None);
    }

    #[test]
    fn latency_buckets_are_upper_bounds() {
        assert_eq!(latency_bucket(0), 0);
        assert_eq!(latency_bucket(1_000), 0);
        assert_eq!(latency_bucket(1_001), 1);
        assert_eq!(latency_bucket(500_000), 4);
        assert_eq!(latency_bucket(500_001), 5);
        assert_eq!(latency_bucket(u64::MAX), 5);
    }

    #[tokio::test]
    async fn counts_every_query_and_logs_internal_names() {
        let observer = DnsObserver::spawn();
        observer.record(query(
            "Production.Shop.temps.local.",
            ZoneClass::Internal,
            QueryOutcome::Noerror,
        ));
        observer.record(query(
            "missing.temps.local.",
            ZoneClass::Internal,
            QueryOutcome::Nxdomain,
        ));

        let snapshot = settle(&observer, 2).await;
        assert_eq!(snapshot.counters.internal.noerror, 1);
        assert_eq!(snapshot.counters.internal.nxdomain, 1);
        assert_eq!(snapshot.counters.internal.total(), 2);
        assert_eq!(snapshot.counters.internal.latency_us_sum, 3_000);
        assert_eq!(snapshot.counters.internal.latency_buckets[1], 2);
        let names: Vec<_> = snapshot.queries.iter().map(|q| q.name.clone()).collect();
        assert_eq!(
            names,
            vec![
                Some("production.shop.temps.local".to_string()),
                Some("missing.temps.local".to_string())
            ]
        );
        assert_eq!(snapshot.queries[0].seq, 1);
        assert_eq!(snapshot.queries[1].seq, 2);
        assert_eq!(snapshot.queries[0].qtype, "A");
    }

    #[tokio::test]
    async fn never_keeps_a_forwarded_name() {
        let observer = DnsObserver::spawn();
        observer.record(query(
            "tenant-1234.example.com.",
            ZoneClass::External,
            QueryOutcome::Noerror,
        ));
        observer.record(query(
            "nope.example.com.",
            ZoneClass::External,
            QueryOutcome::Nxdomain,
        ));
        observer.record(query(
            "signed-host.example.net.",
            ZoneClass::External,
            QueryOutcome::Servfail,
        ));

        let snapshot = settle(&observer, 1).await;
        assert_eq!(
            snapshot.counters.external.total(),
            3,
            "all three are counted"
        );
        assert_eq!(
            snapshot.queries.len(),
            1,
            "only the failed forward is logged: {:?}",
            snapshot.queries
        );
        let entry = &snapshot.queries[0];
        assert_eq!(entry.outcome, QueryOutcome::Servfail);
        assert_eq!(entry.zone, ZoneClass::External);
        assert_eq!(entry.name, None, "and without its name");
    }

    #[tokio::test]
    async fn the_ring_keeps_the_latest_entries_and_a_cursor_reads_what_is_new() {
        let observer = DnsObserver::spawn();
        let total = QUERY_RING_CAPACITY + 10;
        for i in 0..total {
            let name = format!("n{i}.temps.local");
            observer.record(query(&name, ZoneClass::Internal, QueryOutcome::Noerror));
            if i % 256 == 255 {
                // Let the drain task keep up: the channel is bounded and the
                // test is about the ring, not about drops.
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        }
        let snapshot = settle(&observer, QUERY_RING_CAPACITY).await;
        // Give the last entries time to arrive.
        tokio::time::sleep(Duration::from_millis(20)).await;
        let snapshot = if snapshot.queries.last().map(|q| q.seq) == Some(total as u64) {
            snapshot
        } else {
            observer.snapshot(None, None)
        };
        assert_eq!(snapshot.queries.len(), QUERY_RING_CAPACITY);
        assert_eq!(snapshot.queries.first().map(|q| q.seq), Some(11));
        assert_eq!(snapshot.queries.last().map(|q| q.seq), Some(total as u64));
        assert_eq!(snapshot.counters.dropped_log_entries, 0);

        let id = observer.instance_id().to_string();
        let newer = observer.snapshot(Some(total as u64 - 3), Some(&id));
        assert_eq!(newer.queries.len(), 3);
        assert!(!newer.queries_truncated);

        let lost = observer.snapshot(Some(2), Some(&id));
        assert!(
            lost.queries_truncated,
            "entries 3..=10 were overwritten before this read"
        );
        assert_eq!(lost.queries.len(), QUERY_RING_CAPACITY);

        let other = observer.snapshot(Some(total as u64), Some("another-instance"));
        assert_eq!(
            other.queries.len(),
            QUERY_RING_CAPACITY,
            "a cursor from another instance reads everything"
        );
        assert!(!other.queries_truncated);
    }

    #[tokio::test]
    async fn a_full_channel_drops_entries_but_not_counts() {
        let observer = DnsObserver::spawn();
        // No await in between: the drain task cannot run on this
        // current-thread runtime, so the channel fills.
        let total = QUERY_CHANNEL_CAPACITY + 50;
        for _ in 0..total {
            observer.record(query(
                "a.temps.local",
                ZoneClass::Internal,
                QueryOutcome::Noerror,
            ));
        }
        let snapshot = observer.snapshot(None, None);
        assert_eq!(snapshot.counters.internal.noerror, total as u64);
        assert_eq!(snapshot.counters.dropped_log_entries, 50);
    }

    #[test]
    fn names_are_bounded() {
        let long = format!("{}.temps.local.", "a".repeat(400));
        assert_eq!(bounded_name(&long).len(), MAX_QUERY_NAME_LEN);
        assert_eq!(bounded_name("X.Temps.Local."), "x.temps.local");
        // A multi-byte character straddling the cut does not split.
        let tricky = format!("{}é.temps.local", "a".repeat(252));
        assert!(bounded_name(&tricky).len() <= MAX_QUERY_NAME_LEN);
    }

    #[test]
    fn report_round_trips_through_json() {
        let report = DnsReport {
            version: DNS_REPORT_VERSION,
            instance_id: "abc".into(),
            build_version: "1.2.3".into(),
            resolver: DnsResolverState {
                running: true,
                tasks_alive: true,
                listen_addrs: vec!["10.237.0.1:53".into()],
                zone_generation: 7,
                record_count: 3,
                last_sync_age_secs: Some(4),
                consecutive_sync_failures: 0,
                last_error: None,
            },
            counters: DnsCounters::default(),
            queries: vec![DnsQueryEntry {
                seq: 1,
                age_ms: 10,
                name: Some("a.temps.local".into()),
                qtype: "A".into(),
                zone: ZoneClass::Internal,
                outcome: QueryOutcome::Nxdomain,
                latency_us: 120,
                answers: 0,
                source: "10.237.1.9".parse().expect("ip"),
            }],
            queries_truncated: false,
        };
        let json = serde_json::to_string(&report).expect("serialize");
        assert!(json.contains("\"outcome\":\"nxdomain\""), "{json}");
        let back: DnsReport = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(back, report);
    }
}
