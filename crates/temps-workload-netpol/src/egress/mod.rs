// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! The per-node egress proxy (ADR-055 §9).
//!
//! One bounded proxy per node replaces the Node.js sidecar each sandbox
//! gets today. It speaks the same protocol the sidecar does, so a sandbox
//! needs nothing but a different `HTTP(S)_PROXY`:
//!
//! - `CONNECT host:port` tunnels (HTTPS), and absolute-form `http://` requests;
//! - the model and MCP relays under `/.temps/...`, forwarded to the control
//!   plane, and the git relay, which fails closed exactly as the sidecar's
//!   does until the control plane issues a relay credential.
//!
//! What it decides, per connection:
//!
//! 1. **Who is asking**, from the source address alone, through the same
//!    identity map the nftables reconciler uses ([`EgressTable`]). A spoofed
//!    source never gets here: the anti-spoofing binding
//!    ([`crate::binding`]) drops it at the bridge port. An unknown source is
//!    refused.
//! 2. **Whether the destination is allowed**: deny rules first (hostname or
//!    CIDR), then an allow rule must match the hostname, and the port must
//!    be one the matching rule allows (80 and 443 when it names none).
//! 3. **Today's public-address check**: every address the name resolves to
//!    must be public, unless an allow CIDR rule names it. One private answer
//!    refuses the request, which closes the DNS-rebinding fallback, and the
//!    proxy connects to the address it checked rather than resolving twice.
//!
//! **Bounded by construction.** A fixed connection limit (a semaphore that
//! is tried, never awaited) and a fixed buffer pool. At saturation a new
//! connection is answered `503` and closed, counted in
//! [`EgressStats::refused_saturated`] and logged at most once a second;
//! nothing is queued. Memory is `max_connections * 2 * buffer_size` plus a
//! small per-connection constant.
//!
//! **Hot path.** No locks: the rule table is an `ArcSwap` snapshot,
//! counters are atomics, buffers come from a lock-free queue.
//!
//! **Recording** ([`record`]), for workloads whose policy opts in: one
//! [`ConnectionRecord`] per connection when it ends, handed to a bounded
//! channel with `try_send`. A workload that has not opted in costs one
//! branch per connection; a full channel drops the record and counts it,
//! and never slows the connection.

pub mod record;

use std::collections::HashMap;
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr};
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use arc_swap::ArcSwap;
use crossbeam_queue::ArrayQueue;
use serde::{Deserialize, Serialize};
use temps_workload::WorkloadRef;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::Semaphore;
use tracing::{debug, info, warn};

use crate::identity::IdentityMap;
use crate::rule::{Action, Cidr, PortRange};

pub use record::{
    recording_channel, ConnectionKind, ConnectionOutcome, ConnectionRecord, Delivered,
    EgressBatcher, EgressReason, EgressRecordBatch, EgressRecordSink, EgressRecordStream,
    EgressRecorder, EgressRecordingCapability, RecordSinkError, RecordingUnavailable, RelayKind,
};

/// Ports a rule allows when it names none: today's sidecar allows exactly
/// these two.
pub const DEFAULT_PORTS: [u16; 2] = [80, 443];

/// The longest request head the proxy reads.
pub const MAX_HEAD: usize = 8 * 1024;

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum EgressConfigError {
    #[error(
        "'{pattern}' is not a hostname pattern: expected 'example.com', '*.example.com' or '*'"
    )]
    InvalidPattern { pattern: String },
    #[error("relay upstream '{url}' is not supported: {reason}")]
    InvalidRelay { url: String, reason: String },
    #[error("egress proxy limits are invalid: {reason}")]
    InvalidLimits { reason: String },
}

#[derive(Debug, thiserror::Error)]
pub enum EgressError {
    #[error("egress proxy could not bind {addr}: {reason}")]
    Bind { addr: SocketAddr, reason: String },
    #[error("egress proxy on {addr} stopped accepting: {reason}")]
    Accept { addr: SocketAddr, reason: String },
}

/// A hostname a rule matches: exact, every subdomain (`*.example.com`,
/// not the apex), or every name (`*`).
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct HostPattern(Box<str>);

impl HostPattern {
    pub fn parse(pattern: &str) -> Result<Self, EgressConfigError> {
        let trimmed = pattern.trim().to_ascii_lowercase();
        if trimmed == "*" {
            return Ok(Self(trimmed.into()));
        }
        // One trailing dot (an absolute name) is the same name.
        let normalized = trimmed.strip_suffix('.').unwrap_or(&trimmed).to_string();
        let invalid = || EgressConfigError::InvalidPattern {
            pattern: pattern.to_string(),
        };
        let host = normalized.strip_prefix("*.").unwrap_or(&normalized);
        let label_ok = |label: &str| {
            !label.is_empty()
                && label.len() <= 63
                && label
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
                && !label.starts_with('-')
                && !label.ends_with('-')
        };
        if host.is_empty() || host.len() > 253 || !host.split('.').all(label_ok) {
            return Err(invalid());
        }
        Ok(Self(normalized.into()))
    }

    /// `host` must already be lower case without a trailing dot.
    pub fn matches(&self, host: &str) -> bool {
        let pattern = &*self.0;
        if pattern == "*" {
            return true;
        }
        match pattern.strip_prefix("*.") {
            Some(suffix) => host
                .strip_suffix(suffix)
                .is_some_and(|rest| rest.len() > 1 && rest.ends_with('.')),
            None => host == pattern,
        }
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl TryFrom<String> for HostPattern {
    type Error = EgressConfigError;
    fn try_from(value: String) -> Result<Self, Self::Error> {
        Self::parse(&value)
    }
}

impl From<HostPattern> for String {
    fn from(value: HostPattern) -> Self {
        value.0.into()
    }
}

/// What an egress rule names.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EgressTarget {
    Host(HostPattern),
    Cidr(Cidr),
}

/// One hostname or CIDR rule the proxy enforces.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct EgressRule {
    pub id: i64,
    pub target: EgressTarget,
    /// `None`: [`DEFAULT_PORTS`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ports: Option<PortRange>,
    pub action: Action,
}

impl EgressRule {
    fn port_matches(&self, port: u16) -> bool {
        match self.ports {
            Some(range) => range.start <= port && port <= range.end,
            None => DEFAULT_PORTS.contains(&port),
        }
    }
}

/// The proxy rules of one workload.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EgressPolicy {
    pub subject: WorkloadRef,
    #[serde(default)]
    pub rules: Vec<EgressRule>,
    /// Record every outbound connection of this workload ([`record`]).
    /// Set by the control plane from the workload's own opt-in; absent from
    /// an older control plane, which means off.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub record: bool,
    /// Never connect to a non-public address, even inside an allowed CIDR:
    /// a sandbox's own egress policy refuses private, loopback, link-local
    /// and metadata destinations whatever its rules name.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub public_only: bool,
}

/// Why a request was refused. Stable strings, for logs and metrics.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Refusal {
    UnknownSource,
    Denied,
    NotAllowed,
    Port,
    Private,
    Unresolved,
}

impl Refusal {
    pub fn as_str(self) -> &'static str {
        match self {
            Refusal::UnknownSource => "unknown_source",
            Refusal::Denied => "denied_by_rule",
            Refusal::NotAllowed => "no_allow_rule",
            Refusal::Port => "port_not_allowed",
            Refusal::Private => "private_address",
            Refusal::Unresolved => "did_not_resolve",
        }
    }
}

/// How [`CompiledRules::check_host`] admitted a destination, which decides
/// what [`CompiledRules::check_addresses`] must then prove.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Admission {
    /// An allow rule named the destination (a host pattern, or a CIDR that
    /// contains the literal) on this port: every address must be public or
    /// inside an allowed CIDR.
    ByRule,
    /// No rule named the hostname, but the workload has CIDR allows: every
    /// resolved address must fall inside an allowed CIDR on this port.
    ByAddress,
}

/// Rules of one workload, compiled for lookup.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CompiledRules {
    pub workload: WorkloadRef,
    /// Whether this workload's connections are recorded.
    pub record: bool,
    /// See [`EgressPolicy::public_only`].
    pub public_only: bool,
    deny: Vec<EgressRule>,
    allow: Vec<EgressRule>,
}

impl CompiledRules {
    pub fn new(policy: &EgressPolicy) -> Self {
        let mut rules = policy.rules.clone();
        rules.sort_by_key(|r| r.id);
        let (deny, allow) = rules.into_iter().partition(|r| r.action == Action::Deny);
        Self {
            workload: policy.subject,
            record: policy.record,
            public_only: policy.public_only,
            deny,
            allow,
        }
    }

    /// Whether the name may be asked for at all, before resolving it.
    /// `host` is lower case; it may be an address literal.
    pub fn check_host(&self, host: &str, port: u16) -> Result<Admission, Refusal> {
        let literal: Option<IpAddr> = host.parse().ok();
        let matches = |rule: &EgressRule| match (&rule.target, literal) {
            (EgressTarget::Host(pattern), None) => pattern.matches(host),
            (EgressTarget::Host(pattern), Some(_)) => pattern.as_str() == "*",
            (EgressTarget::Cidr(cidr), Some(ip)) => cidr.contains(ip),
            (EgressTarget::Cidr(_), None) => false,
        };
        if self.deny.iter().any(|r| r.port_matches(port) && matches(r)) {
            return Err(Refusal::Denied);
        }
        let allowed: Vec<&EgressRule> = self.allow.iter().filter(|r| matches(r)).collect();
        if allowed.is_empty() {
            // A literal was already compared against every CIDR allow above,
            // so nothing admits it. A hostname may still resolve into an
            // allowed CIDR; that is decided per address after resolution,
            // and only a CIDR match (never "is public") admits it then.
            let has_cidr_allow = self
                .allow
                .iter()
                .any(|r| matches!(r.target, EgressTarget::Cidr(_)));
            if literal.is_some() || !has_cidr_allow {
                return Err(Refusal::NotAllowed);
            }
            return Ok(Admission::ByAddress);
        }
        if !allowed.iter().any(|r| r.port_matches(port)) {
            return Err(Refusal::Port);
        }
        Ok(Admission::ByRule)
    }

    /// Whether the resolved addresses may be connected to: `Ok` when
    /// [`CompiledRules::admissible_addresses`] leaves at least one.
    pub fn check_addresses(
        &self,
        addrs: &[IpAddr],
        port: u16,
        admission: Admission,
    ) -> Result<(), Refusal> {
        self.admissible_addresses(addrs, port, admission)
            .map(|_| ())
    }

    /// The resolved addresses that may be connected to, in their order.
    ///
    /// - With `public_only`, one non-public answer refuses the whole name
    ///   (the DNS-rebinding fallback), as does an answer inside a denied
    ///   CIDR, whatever the admission.
    /// - [`Admission::ByRule`]: an allow rule named the destination, so
    ///   every answer must be public or inside an allowed CIDR, and all of
    ///   them are admissible.
    /// - [`Admission::ByAddress`]: no rule named the hostname, so only the
    ///   answers inside an allowed CIDR on this port are admissible, and the
    ///   name is refused only when none is. This is the sandbox sidecar's
    ///   decision too: it connects to the first answer a CIDR rule allows,
    ///   so recording a sandbox never changes what it may reach.
    ///
    /// The proxy connects to an address from this list, never to one it did
    /// not check.
    pub fn admissible_addresses(
        &self,
        addrs: &[IpAddr],
        port: u16,
        admission: Admission,
    ) -> Result<Vec<IpAddr>, Refusal> {
        if addrs.is_empty() {
            return Err(Refusal::Unresolved);
        }
        let mut admissible = Vec::with_capacity(addrs.len());
        for &addr in addrs {
            let addr = unmap(addr);
            if self.public_only && !is_public(addr) {
                return Err(Refusal::Private);
            }
            let in_cidr = |r: &&EgressRule| {
                matches!(&r.target, EgressTarget::Cidr(c) if c.contains(addr))
                    && r.port_matches(port)
            };
            if self.deny.iter().any(|r| in_cidr(&r)) {
                return Err(Refusal::Denied);
            }
            if self.allow.iter().any(|r| in_cidr(&r)) {
                admissible.push(addr);
                continue;
            }
            match admission {
                Admission::ByAddress => {}
                Admission::ByRule if is_public(addr) => admissible.push(addr),
                Admission::ByRule => return Err(Refusal::Private),
            }
        }
        if admissible.is_empty() {
            return Err(Refusal::NotAllowed);
        }
        Ok(admissible)
    }
}

/// Source address → the rules of the workload sending from it.
#[derive(Debug, Clone, Default)]
pub struct EgressTable {
    sources: HashMap<IpAddr, Arc<CompiledRules>>,
}

impl EgressTable {
    pub fn build(policies: &[EgressPolicy], identities: &IdentityMap) -> Self {
        let mut sources = HashMap::new();
        for policy in policies {
            let compiled = Arc::new(CompiledRules::new(policy));
            for addr in identities.addresses(&policy.subject) {
                sources.insert(addr, compiled.clone());
            }
        }
        Self { sources }
    }

    pub fn lookup(&self, source: IpAddr) -> Option<&Arc<CompiledRules>> {
        self.sources.get(&unmap(source))
    }

    pub fn len(&self) -> usize {
        self.sources.len()
    }

    pub fn is_empty(&self) -> bool {
        self.sources.is_empty()
    }
}

fn unmap(addr: IpAddr) -> IpAddr {
    match addr {
        IpAddr::V6(v6) => v6
            .to_ipv4_mapped()
            .map(IpAddr::V4)
            .unwrap_or(IpAddr::V6(v6)),
        v4 => v4,
    }
}

/// Today's sidecar rule, address for address: private, shared, loopback,
/// link-local, documentation, benchmarking, multicast and reserved IPv4
/// blocks are refused; for IPv6 only global unicast (`2000::/3`) outside
/// the documentation and benchmarking blocks is public.
pub fn is_public(addr: IpAddr) -> bool {
    match unmap(addr) {
        IpAddr::V4(v4) => is_public_v4(v4),
        IpAddr::V6(v6) => is_public_v6(v6),
    }
}

fn is_public_v4(v4: Ipv4Addr) -> bool {
    let [a, b, c, _] = v4.octets();
    !(a == 0
        || a == 10
        || a == 127
        || (a == 100 && (64..=127).contains(&b))
        || (a == 169 && b == 254)
        || (a == 172 && (16..=31).contains(&b))
        || (a == 192 && b == 0 && c == 0)
        || (a == 192 && b == 0 && c == 2)
        || (a == 192 && b == 88 && c == 99)
        || (a == 192 && b == 168)
        || (a == 198 && (b == 18 || b == 19))
        || (a == 198 && b == 51 && c == 100)
        || (a == 203 && b == 0 && c == 113)
        || a >= 224)
}

fn is_public_v6(v6: Ipv6Addr) -> bool {
    let s = v6.segments();
    (s[0] & 0xe000) == 0x2000
        && !(s[0] == 0x2001 && s[1] == 0x0002)
        && !(s[0] == 0x2001 && s[1] == 0x0db8)
}

/// Where the model and MCP relays go: the control plane, over plain HTTP
/// (node 0 relays to its own console listener). A TLS upstream for worker
/// nodes is not supported yet and is refused when the proxy is built.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RelayUpstream {
    /// `host:port` to connect to.
    pub authority: String,
    /// Path prefix of the control plane, without a trailing slash.
    pub base_path: String,
}

impl RelayUpstream {
    pub fn parse(url: &str) -> Result<Self, EgressConfigError> {
        let invalid = |reason: &str| EgressConfigError::InvalidRelay {
            url: url.to_string(),
            reason: reason.to_string(),
        };
        let rest = url
            .strip_prefix("http://")
            .ok_or_else(|| invalid("only http:// upstreams are supported"))?;
        let (authority, path) = match rest.find('/') {
            Some(i) => (&rest[..i], &rest[i..]),
            None => (rest, ""),
        };
        if authority.is_empty() || authority.contains('@') {
            return Err(invalid(
                "expected http://host[:port][/path] without credentials",
            ));
        }
        let authority = if authority.contains(':') && !authority.ends_with(']') {
            authority.to_string()
        } else {
            format!("{authority}:80")
        };
        Ok(Self {
            authority,
            base_path: path.trim_end_matches('/').to_string(),
        })
    }
}

#[derive(Debug, Clone)]
pub struct EgressProxyConfig {
    pub listen: SocketAddr,
    pub max_connections: usize,
    pub buffer_size: usize,
    pub head_timeout: Duration,
    pub connect_timeout: Duration,
    pub idle_timeout: Duration,
    pub relay: Option<RelayUpstream>,
}

impl EgressProxyConfig {
    /// Sized for the 1 vCPU / 2 GB target: 512 connections × 2 × 16 KiB =
    /// 16 MiB of buffers at saturation.
    pub fn new(listen: SocketAddr) -> Self {
        Self {
            listen,
            max_connections: 512,
            buffer_size: 16 * 1024,
            head_timeout: Duration::from_secs(10),
            connect_timeout: Duration::from_secs(10),
            idle_timeout: Duration::from_secs(60),
            relay: None,
        }
    }

    fn validate(&self) -> Result<(), EgressConfigError> {
        if self.max_connections == 0 || self.max_connections > 65_536 {
            return Err(EgressConfigError::InvalidLimits {
                reason: format!(
                    "max_connections is {}; expected 1..=65536",
                    self.max_connections
                ),
            });
        }
        if self.buffer_size < MAX_HEAD || self.buffer_size > 1024 * 1024 {
            return Err(EgressConfigError::InvalidLimits {
                reason: format!(
                    "buffer_size is {}; expected {MAX_HEAD}..=1048576",
                    self.buffer_size
                ),
            });
        }
        Ok(())
    }
}

/// Counters, all monotonic except `active`.
#[derive(Debug, Default)]
pub struct EgressStats {
    pub accepted: AtomicU64,
    pub active: AtomicU64,
    pub tunnels: AtomicU64,
    pub forwarded: AtomicU64,
    pub relayed: AtomicU64,
    pub refused_saturated: AtomicU64,
    pub refused_unknown_source: AtomicU64,
    pub refused_policy: AtomicU64,
    pub refused_private: AtomicU64,
    pub malformed: AtomicU64,
    pub upstream_failed: AtomicU64,
    pub bytes_up: AtomicU64,
    pub bytes_down: AtomicU64,
    /// Connection records queued for the control plane.
    pub records_emitted: AtomicU64,
    /// Connection records dropped because the recording channel was full.
    pub records_dropped: AtomicU64,
    /// Repeated refusals merged into an earlier record of the same batch.
    pub records_collapsed: AtomicU64,
    /// Records the sink accepted.
    pub records_delivered: AtomicU64,
    /// Records lost because their batch could not be delivered.
    pub records_undelivered: AtomicU64,
    /// Records the sink accepted but discarded ([`Delivered::discarded`]).
    pub records_discarded: AtomicU64,
    last_saturation_log: AtomicU64,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize)]
pub struct EgressStatsSnapshot {
    pub accepted: u64,
    pub active: u64,
    pub tunnels: u64,
    pub forwarded: u64,
    pub relayed: u64,
    pub refused_saturated: u64,
    pub refused_unknown_source: u64,
    pub refused_policy: u64,
    pub refused_private: u64,
    pub malformed: u64,
    pub upstream_failed: u64,
    pub bytes_up: u64,
    pub bytes_down: u64,
    pub records_emitted: u64,
    pub records_dropped: u64,
    pub records_collapsed: u64,
    pub records_delivered: u64,
    pub records_undelivered: u64,
    pub records_discarded: u64,
}

impl EgressStats {
    pub fn snapshot(&self) -> EgressStatsSnapshot {
        let get = |c: &AtomicU64| c.load(Ordering::Relaxed);
        EgressStatsSnapshot {
            accepted: get(&self.accepted),
            active: get(&self.active),
            tunnels: get(&self.tunnels),
            forwarded: get(&self.forwarded),
            relayed: get(&self.relayed),
            refused_saturated: get(&self.refused_saturated),
            refused_unknown_source: get(&self.refused_unknown_source),
            refused_policy: get(&self.refused_policy),
            refused_private: get(&self.refused_private),
            malformed: get(&self.malformed),
            upstream_failed: get(&self.upstream_failed),
            bytes_up: get(&self.bytes_up),
            bytes_down: get(&self.bytes_down),
            records_emitted: get(&self.records_emitted),
            records_dropped: get(&self.records_dropped),
            records_collapsed: get(&self.records_collapsed),
            records_delivered: get(&self.records_delivered),
            records_discarded: get(&self.records_discarded),
            records_undelivered: get(&self.records_undelivered),
        }
    }
}

/// A fixed set of buffers, allocated on first use and never beyond
/// `capacity`.
pub struct BufferPool {
    free: ArrayQueue<Box<[u8]>>,
    allocated: AtomicUsize,
    capacity: usize,
    size: usize,
}

impl BufferPool {
    pub fn new(capacity: usize, size: usize) -> Arc<Self> {
        Arc::new(Self {
            free: ArrayQueue::new(capacity.max(1)),
            allocated: AtomicUsize::new(0),
            capacity,
            size,
        })
    }

    /// A buffer, or `None` when every buffer is in use.
    pub fn get(self: &Arc<Self>) -> Option<PooledBuffer> {
        if let Some(buf) = self.free.pop() {
            return Some(PooledBuffer {
                buf: Some(buf),
                pool: self.clone(),
            });
        }
        let mut current = self.allocated.load(Ordering::Relaxed);
        loop {
            if current >= self.capacity {
                return None;
            }
            match self.allocated.compare_exchange_weak(
                current,
                current + 1,
                Ordering::AcqRel,
                Ordering::Relaxed,
            ) {
                Ok(_) => break,
                Err(seen) => current = seen,
            }
        }
        Some(PooledBuffer {
            buf: Some(vec![0u8; self.size].into_boxed_slice()),
            pool: self.clone(),
        })
    }

    pub fn allocated(&self) -> usize {
        self.allocated.load(Ordering::Relaxed)
    }
}

pub struct PooledBuffer {
    buf: Option<Box<[u8]>>,
    pool: Arc<BufferPool>,
}

impl std::ops::Deref for PooledBuffer {
    type Target = [u8];
    fn deref(&self) -> &[u8] {
        self.buf.as_deref().unwrap_or(&[])
    }
}

impl std::ops::DerefMut for PooledBuffer {
    fn deref_mut(&mut self) -> &mut [u8] {
        self.buf.as_deref_mut().unwrap_or(&mut [])
    }
}

impl Drop for PooledBuffer {
    fn drop(&mut self) {
        if let Some(buf) = self.buf.take() {
            // The queue holds `capacity` entries, so this cannot fail; if
            // it ever did the buffer is simply freed.
            let _ = self.pool.free.push(buf);
        }
    }
}

/// The running proxy's handle: its counters, its rule table and the
/// address it listens on.
pub struct EgressProxy {
    config: EgressProxyConfig,
    table: Arc<ArcSwap<EgressTable>>,
    stats: Arc<EgressStats>,
    limit: Arc<Semaphore>,
    pool: Arc<BufferPool>,
    /// Where records of opted-in workloads go; `None`: nothing is recorded,
    /// whatever the policies say.
    recorder: Option<EgressRecorder>,
    /// Asked once before a caller the table does not know is refused.
    unknown_source: std::sync::OnceLock<Arc<dyn UnknownSource>>,
    /// How many unknown callers may wait for [`UnknownSource`] at once
    /// ([`unknown_waits`]). A waiting caller holds a connection slot, so
    /// past this the rest are refused at once, and a burst of unknown
    /// callers can never take the slots known workloads need.
    unknown_waits: Arc<Semaphore>,
}

/// The unknown callers that may wait at once out of `max_connections`: one
/// in sixteen, at least one.
pub fn unknown_waits(max_connections: usize) -> usize {
    (max_connections / 16).max(1)
}

/// The longest a connection from a caller the table does not know waits for
/// [`UnknownSource::unknown_source`] before it is refused.
pub const UNKNOWN_SOURCE_WAIT: Duration = Duration::from_secs(3);

/// What the proxy asks before refusing a caller its table does not hold: a
/// workload that has just started may be reaching the proxy before the
/// table that names it was rebuilt. The proxy looks the caller up again
/// afterwards and refuses only if it is still unknown.
#[async_trait::async_trait]
pub trait UnknownSource: Send + Sync {
    /// Brings the table up to date if a new workload may hold `source`.
    /// Bounded by [`UNKNOWN_SOURCE_WAIT`]; an implementation must also bound
    /// how often it does real work, since any caller can trigger it.
    async fn unknown_source(&self, source: IpAddr);
}

impl EgressProxy {
    pub fn new(
        config: EgressProxyConfig,
        table: Arc<ArcSwap<EgressTable>>,
    ) -> Result<Arc<Self>, EgressConfigError> {
        Self::with_recorder(config, table, None)
    }

    /// A proxy that records the connections of workloads whose policy opts
    /// in, into `recorder` (see [`recording_channel`]). The caller runs an
    /// [`EgressBatcher`] on the other end, with this proxy's
    /// [`EgressProxy::stats`].
    pub fn with_recorder(
        config: EgressProxyConfig,
        table: Arc<ArcSwap<EgressTable>>,
        recorder: Option<EgressRecorder>,
    ) -> Result<Arc<Self>, EgressConfigError> {
        config.validate()?;
        Ok(Arc::new(Self {
            limit: Arc::new(Semaphore::new(config.max_connections)),
            pool: BufferPool::new(config.max_connections * 2, config.buffer_size),
            unknown_waits: Arc::new(Semaphore::new(unknown_waits(config.max_connections))),
            config,
            table,
            stats: Arc::new(EgressStats::default()),
            recorder,
            unknown_source: std::sync::OnceLock::new(),
        }))
    }

    /// Sets what is asked before an unknown caller is refused. Set once.
    pub fn set_unknown_source(&self, handler: Arc<dyn UnknownSource>) {
        let _ = self.unknown_source.set(handler);
    }

    /// Whether this proxy was built with a recorder.
    pub fn records(&self) -> bool {
        self.recorder.is_some()
    }

    pub fn stats(&self) -> &Arc<EgressStats> {
        &self.stats
    }

    pub fn table(&self) -> &Arc<ArcSwap<EgressTable>> {
        &self.table
    }

    pub fn buffers_allocated(&self) -> usize {
        self.pool.allocated()
    }

    /// Binds the listener; [`EgressProxy::serve`] then runs it.
    pub async fn bind(&self) -> Result<TcpListener, EgressError> {
        TcpListener::bind(self.config.listen)
            .await
            .map_err(|e| EgressError::Bind {
                addr: self.config.listen,
                reason: e.to_string(),
            })
    }

    /// Accepts until the listener fails. Each connection runs on its own
    /// task, bounded by the connection limit.
    pub async fn serve(self: Arc<Self>, listener: TcpListener) -> Result<(), EgressError> {
        let addr = listener.local_addr().unwrap_or(self.config.listen);
        info!(%addr, max_connections = self.config.max_connections, "egress proxy listening");
        loop {
            let (stream, peer) = match listener.accept().await {
                Ok(accepted) => accepted,
                Err(e) if is_transient_accept_error(&e) => {
                    warn!(%addr, error = %e, "egress proxy accept failed; retrying");
                    tokio::time::sleep(Duration::from_millis(50)).await;
                    continue;
                }
                Err(e) => {
                    return Err(EgressError::Accept {
                        addr,
                        reason: e.to_string(),
                    })
                }
            };
            self.stats.accepted.fetch_add(1, Ordering::Relaxed);
            let Ok(permit) = self.limit.clone().try_acquire_owned() else {
                self.refuse_saturated(stream, peer);
                continue;
            };
            let proxy = self.clone();
            tokio::spawn(async move {
                proxy.stats.active.fetch_add(1, Ordering::Relaxed);
                proxy.handle(stream, peer, permit).await;
                proxy.stats.active.fetch_sub(1, Ordering::Relaxed);
            });
        }
    }

    /// No workload owns this source, so there is nobody to attribute a
    /// record to and no opt-in to honour: counted, never recorded.
    async fn refuse_unknown(&self, mut client: TcpStream, source: IpAddr) {
        self.stats
            .refused_unknown_source
            .fetch_add(1, Ordering::Relaxed);
        debug!(%source, reason = Refusal::UnknownSource.as_str(), "egress proxy refused");
        let _ = client.write_all(FORBIDDEN).await;
    }

    fn refuse_saturated(&self, stream: TcpStream, peer: SocketAddr) {
        let refused = self.stats.refused_saturated.fetch_add(1, Ordering::Relaxed) + 1;
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or_default();
        let last = self.stats.last_saturation_log.load(Ordering::Relaxed);
        if now > last
            && self
                .stats
                .last_saturation_log
                .compare_exchange(last, now, Ordering::Relaxed, Ordering::Relaxed)
                .is_ok()
        {
            warn!(
                %peer,
                limit = self.config.max_connections,
                refused_total = refused,
                reason = "connection_limit",
                "egress proxy at its connection limit: refusing new connections"
            );
        }
        // Saturation is the one refusal taken before the connection has a
        // task, so its record is taken here. Only on this already-refusing
        // path, and lock-free: one snapshot load and one map lookup.
        if let Some(recorder) = &self.recorder {
            if let Some(rules) = self.table.load().lookup(peer.ip()) {
                if rules.record {
                    let now = record::unix_millis();
                    recorder.record(
                        Ended::refused(None, EgressReason::Saturated)
                            .into_record(rules.workload, now, Duration::ZERO)
                            .from_source(unmap(peer.ip())),
                        &self.stats,
                    );
                }
            }
        }
        // Answered inline, without a task: a just-accepted socket has an
        // empty send buffer, so this ~100-byte write completes at once (the
        // timeout bounds it if it ever did not). Tokio's readiness is not
        // primed yet, so `try_write` would report `WouldBlock`.
        if let Ok(std_stream) = stream.into_std() {
            use std::io::Write;
            let _ = std_stream.set_nonblocking(false);
            let _ = std_stream.set_write_timeout(Some(Duration::from_millis(50)));
            let mut std_stream = std_stream;
            let _ = std_stream.write_all(SATURATED);
        }
    }

    /// Serves one accepted connection, holding `permit` (its connection
    /// slot) for as long as it is served.
    async fn handle(
        &self,
        client: TcpStream,
        peer: SocketAddr,
        permit: tokio::sync::OwnedSemaphorePermit,
    ) {
        let source = unmap(peer.ip());
        let mut rules = self.table.load().lookup(source).cloned();
        // Held until the connection is served; given up while waiting.
        let mut slot = Some(permit);
        if rules.is_none() {
            if let Some(handler) = self.unknown_source.get() {
                // A workload that has just started can be here before the
                // table naming it: ask once, briefly, then look again. The
                // wait holds a discovery slot instead of a connection slot,
                // so callers waiting can never take the slots known
                // workloads need; with every discovery slot taken the caller
                // is told to retry (503), as at the connection limit: it may
                // well be a workload that discovery would have found.
                let Ok(waiting) = self.unknown_waits.clone().try_acquire_owned() else {
                    drop(slot);
                    self.refuse_saturated(client, peer);
                    return;
                };
                drop(slot.take());
                let _ =
                    tokio::time::timeout(UNKNOWN_SOURCE_WAIT, handler.unknown_source(source)).await;
                drop(waiting);
                rules = self.table.load().lookup(source).cloned();
                if rules.is_none() {
                    self.refuse_unknown(client, source).await;
                    return;
                }
                let Ok(permit) = self.limit.clone().try_acquire_owned() else {
                    self.refuse_saturated(client, peer);
                    return;
                };
                slot = Some(permit);
            }
        }
        let _slot = slot;
        let Some(rules) = rules else {
            self.refuse_unknown(client, source).await;
            return;
        };
        // The one branch recording costs a workload that has not opted in.
        let Some(recorder) = self.recorder.as_ref().filter(|_| rules.record) else {
            self.serve_connection(client, source, &rules).await;
            return;
        };
        let started = Instant::now();
        let started_at_ms = record::unix_millis();
        let ended = self.serve_connection(client, source, &rules).await;
        recorder.record(
            ended
                .into_record(rules.workload, started_at_ms, started.elapsed())
                .from_source(source),
            &self.stats,
        );
    }

    async fn serve_connection(
        &self,
        mut client: TcpStream,
        source: IpAddr,
        rules: &CompiledRules,
    ) -> Ended {
        let (Some(mut up_buf), Some(mut down_buf)) = (self.pool.get(), self.pool.get()) else {
            // Cannot happen while the semaphore holds (two buffers per
            // permit), but never allocate past the pool.
            self.stats.refused_saturated.fetch_add(1, Ordering::Relaxed);
            let _ = client.write_all(UNAVAILABLE).await;
            return Ended::refused(None, EgressReason::Saturated);
        };
        let read = tokio::time::timeout(
            self.config.head_timeout,
            read_head(&mut client, &mut up_buf),
        )
        .await;
        let (head_len, filled) = match read {
            Ok(Ok(Some(lengths))) => lengths,
            _ => {
                self.stats.malformed.fetch_add(1, Ordering::Relaxed);
                let _ = client.write_all(BAD_REQUEST).await;
                return Ended::refused(None, EgressReason::MalformedRequest);
            }
        };
        let request = match parse_request(&up_buf[..head_len]) {
            Some(request) => request,
            None => {
                self.stats.malformed.fetch_add(1, Ordering::Relaxed);
                let _ = client.write_all(BAD_REQUEST).await;
                return Ended::refused(None, EgressReason::MalformedRequest);
            }
        };
        // Body bytes that arrived with the head, sent on before the splice.
        let early = filled.saturating_sub(head_len) as u64;
        match request.kind {
            RequestKind::Connect { ref host, port } => {
                let (mut upstream, peer) = match self.open(rules, source, host, port).await {
                    Ok(opened) => opened,
                    Err(OpenError::Refused(refusal)) => {
                        let _ = client.write_all(FORBIDDEN).await;
                        return Ended::refused(Some(request), refusal.into());
                    }
                    Err(OpenError::Unreachable) => {
                        self.stats.upstream_failed.fetch_add(1, Ordering::Relaxed);
                        let _ = client.write_all(BAD_GATEWAY).await;
                        return Ended::failed(Some(request));
                    }
                };
                if client.write_all(ESTABLISHED).await.is_err() {
                    return Ended::allowed(request, Some(peer), 0, 0);
                }
                if filled > head_len && upstream.write_all(&up_buf[head_len..filled]).await.is_err()
                {
                    return Ended::allowed(request, Some(peer), 0, 0);
                }
                self.stats.tunnels.fetch_add(1, Ordering::Relaxed);
                let (sent, received) = self.splice(client, upstream, up_buf, down_buf).await;
                Ended::allowed(request, Some(peer), early + sent, received)
            }
            RequestKind::Forward {
                ref host,
                port,
                ref path,
            } => {
                let (mut upstream, peer) = match self.open(rules, source, host, port).await {
                    Ok(opened) => opened,
                    Err(OpenError::Refused(refusal)) => {
                        let _ = client.write_all(FORBIDDEN).await;
                        return Ended::refused(Some(request), refusal.into());
                    }
                    Err(OpenError::Unreachable) => {
                        self.stats.upstream_failed.fetch_add(1, Ordering::Relaxed);
                        let _ = client.write_all(BAD_GATEWAY).await;
                        return Ended::failed(Some(request));
                    }
                };
                let authority = if port == 80 {
                    host.clone()
                } else {
                    format!("{host}:{port}")
                };
                let head = rewrite_head(&request, path, &authority, &mut down_buf);
                let Some(head_len_out) = head else {
                    self.stats.malformed.fetch_add(1, Ordering::Relaxed);
                    let _ = client.write_all(BAD_REQUEST).await;
                    return Ended::refused(Some(request), EgressReason::MalformedRequest);
                };
                if upstream.write_all(&down_buf[..head_len_out]).await.is_err()
                    || (filled > head_len
                        && upstream.write_all(&up_buf[head_len..filled]).await.is_err())
                {
                    return Ended::allowed(request, Some(peer), 0, 0);
                }
                self.stats.forwarded.fetch_add(1, Ordering::Relaxed);
                let (sent, received) = self.splice(client, upstream, up_buf, down_buf).await;
                // The request head the proxy forwarded counts as sent.
                let head = head_len_out as u64;
                Ended::allowed(request, Some(peer), head + early + sent, received)
            }
            RequestKind::Relay { ref target } => {
                let Some(relay) = self.config.relay.as_ref() else {
                    self.stats.refused_policy.fetch_add(1, Ordering::Relaxed);
                    let _ = client.write_all(FORBIDDEN).await;
                    return Ended::refused(Some(request), EgressReason::RelayUnavailable);
                };
                let connect = tokio::time::timeout(
                    self.config.connect_timeout,
                    TcpStream::connect(relay.authority.as_str()),
                )
                .await;
                let Ok(Ok(mut upstream)) = connect else {
                    self.stats.upstream_failed.fetch_add(1, Ordering::Relaxed);
                    let _ = client.write_all(BAD_GATEWAY).await;
                    return Ended::failed(Some(request));
                };
                let path = format!("{}{target}", relay.base_path);
                let Some(head_len_out) =
                    rewrite_head(&request, &path, &relay.authority, &mut down_buf)
                else {
                    self.stats.malformed.fetch_add(1, Ordering::Relaxed);
                    let _ = client.write_all(BAD_REQUEST).await;
                    return Ended::refused(Some(request), EgressReason::MalformedRequest);
                };
                if upstream.write_all(&down_buf[..head_len_out]).await.is_err()
                    || (filled > head_len
                        && upstream.write_all(&up_buf[head_len..filled]).await.is_err())
                {
                    return Ended::allowed(request, None, 0, 0);
                }
                self.stats.relayed.fetch_add(1, Ordering::Relaxed);
                let (sent, received) = self.splice(client, upstream, up_buf, down_buf).await;
                let head = head_len_out as u64;
                Ended::allowed(request, None, head + early + sent, received)
            }
            RequestKind::Refused(reason) => {
                self.stats.refused_policy.fetch_add(1, Ordering::Relaxed);
                let _ = client.write_all(FORBIDDEN).await;
                Ended::refused(Some(request), reason)
            }
        }
    }

    /// Checks and connects, returning the stream and the address it
    /// connected to. A refusal is already counted and logged.
    async fn open(
        &self,
        rules: &CompiledRules,
        source: IpAddr,
        host: &str,
        port: u16,
    ) -> Result<(TcpStream, IpAddr), OpenError> {
        let refuse = |refusal: Refusal| {
            match refusal {
                Refusal::Private => self.stats.refused_private.fetch_add(1, Ordering::Relaxed),
                _ => self.stats.refused_policy.fetch_add(1, Ordering::Relaxed),
            };
            debug!(
                %source,
                workload = %rules.workload,
                host,
                port,
                reason = refusal.as_str(),
                "egress proxy refused"
            );
            OpenError::Refused(refusal)
        };
        let admission = rules.check_host(host, port).map_err(refuse)?;
        let addrs: Vec<IpAddr> = match host.parse::<IpAddr>() {
            Ok(ip) => vec![ip],
            Err(_) => {
                let lookup = tokio::time::timeout(
                    self.config.connect_timeout,
                    tokio::net::lookup_host((host, port)),
                )
                .await;
                match lookup {
                    Ok(Ok(found)) => found.map(|a| a.ip()).collect(),
                    _ => Vec::new(),
                }
            }
        };
        let addrs = rules
            .admissible_addresses(&addrs, port, admission)
            .map_err(refuse)?;
        let target = addrs
            .iter()
            .find(|a| a.is_ipv4())
            .or_else(|| addrs.first())
            .copied()
            .ok_or_else(|| refuse(Refusal::Unresolved))?;
        let connect = tokio::time::timeout(
            self.config.connect_timeout,
            TcpStream::connect(SocketAddr::new(target, port)),
        )
        .await;
        match connect {
            Ok(Ok(stream)) => Ok((stream, unmap(target))),
            _ => Err(OpenError::Unreachable),
        }
    }

    /// Copies both ways until both sides are done. Returns the bytes sent
    /// (workload to destination) and received.
    async fn splice(
        &self,
        client: TcpStream,
        upstream: TcpStream,
        mut up_buf: PooledBuffer,
        mut down_buf: PooledBuffer,
    ) -> (u64, u64) {
        let (mut client_read, mut client_write) = client.into_split();
        let (mut upstream_read, mut upstream_write) = upstream.into_split();
        let idle = self.config.idle_timeout;
        let up = pump(&mut client_read, &mut upstream_write, &mut up_buf, idle);
        let down = pump(&mut upstream_read, &mut client_write, &mut down_buf, idle);
        let (sent, received) = tokio::join!(up, down);
        self.stats.bytes_up.fetch_add(sent, Ordering::Relaxed);
        self.stats.bytes_down.fetch_add(received, Ordering::Relaxed);
        (sent, received)
    }
}

/// Why [`EgressProxy::open`] did not return a stream.
enum OpenError {
    Refused(Refusal),
    Unreachable,
}

/// How a connection ended, from what `serve_connection` already holds.
/// Building it moves values the connection was done with; only an opted-in
/// workload's turns into a [`ConnectionRecord`].
struct Ended {
    request: Option<ParsedRequest>,
    outcome: ConnectionOutcome,
    reason: Option<EgressReason>,
    peer: Option<IpAddr>,
    bytes_sent: u64,
    bytes_received: u64,
}

impl Ended {
    fn refused(request: Option<ParsedRequest>, reason: EgressReason) -> Self {
        Self {
            request,
            outcome: ConnectionOutcome::Refused,
            reason: Some(reason),
            peer: None,
            bytes_sent: 0,
            bytes_received: 0,
        }
    }

    fn failed(request: Option<ParsedRequest>) -> Self {
        Self {
            outcome: ConnectionOutcome::Failed,
            reason: Some(EgressReason::UpstreamUnreachable),
            ..Self::refused(request, EgressReason::UpstreamUnreachable)
        }
    }

    fn allowed(
        request: ParsedRequest,
        peer: Option<IpAddr>,
        bytes_sent: u64,
        bytes_received: u64,
    ) -> Self {
        Self {
            request: Some(request),
            outcome: ConnectionOutcome::Allowed,
            reason: None,
            peer,
            bytes_sent,
            bytes_received,
        }
    }

    fn into_record(
        self,
        workload: WorkloadRef,
        started_at_ms: u64,
        elapsed: Duration,
    ) -> ConnectionRecord {
        let mut kind = ConnectionKind::Unknown;
        let (mut host, mut port, mut method, mut path, mut relay) = (None, None, None, None, None);
        if let Some(request) = self.request {
            match request.kind {
                RequestKind::Connect { host: h, port: p } => {
                    kind = ConnectionKind::Tunnel;
                    host = Some(recorded_host(h));
                    port = Some(p);
                }
                RequestKind::Forward {
                    host: h,
                    port: p,
                    path: target,
                } => {
                    kind = ConnectionKind::Forward;
                    host = Some(recorded_host(h));
                    port = Some(p);
                    method = Some(record::recorded_method(&request.method));
                    path = Some(record::recorded_path(&target));
                }
                RequestKind::Relay { target } => {
                    kind = ConnectionKind::Relay;
                    relay = relay_kind(&target);
                }
                RequestKind::Refused(_) => {}
            }
        }
        ConnectionRecord {
            workload,
            started_at_ms,
            last_started_at_ms: started_at_ms,
            duration_ms: elapsed.as_millis().min(u128::from(u64::MAX)) as u64,
            kind,
            outcome: self.outcome,
            reason: self.reason,
            host,
            port,
            peer: self.peer,
            method,
            path,
            relay,
            bytes_sent: self.bytes_sent,
            bytes_received: self.bytes_received,
            count: 1,
            source: None,
            recorded_at: None,
        }
    }
}

/// A host as recorded: at most [`record::MAX_RECORDED_HOST`] bytes.
fn recorded_host(mut host: String) -> String {
    if host.len() > record::MAX_RECORDED_HOST {
        let mut cut = record::MAX_RECORDED_HOST;
        while !host.is_char_boundary(cut) {
            cut -= 1;
        }
        host.truncate(cut);
    }
    host
}

/// Which relay a rewritten control-plane path goes to.
fn relay_kind(target: &str) -> Option<RelayKind> {
    if target.starts_with("/api/ai/sandbox-models/") {
        Some(RelayKind::Model)
    } else if target.starts_with("/api/ai/sandbox-tools/") {
        Some(RelayKind::Mcp)
    } else {
        None
    }
}

fn is_transient_accept_error(e: &std::io::Error) -> bool {
    matches!(
        e.kind(),
        std::io::ErrorKind::ConnectionAborted
            | std::io::ErrorKind::ConnectionReset
            | std::io::ErrorKind::Interrupted
            | std::io::ErrorKind::WouldBlock
    ) || e.raw_os_error() == Some(24) // EMFILE: back off instead of dying
}

/// Copies until EOF, an error or `idle` without traffic, then shuts the
/// writer down so the other side sees the end. Returns bytes copied.
async fn pump<R, W>(reader: &mut R, writer: &mut W, buf: &mut [u8], idle: Duration) -> u64
where
    R: AsyncRead + Unpin,
    W: AsyncWrite + Unpin,
{
    let mut total = 0u64;
    loop {
        let read = tokio::time::timeout(idle, reader.read(buf)).await;
        let n = match read {
            Ok(Ok(0)) | Ok(Err(_)) | Err(_) => break,
            Ok(Ok(n)) => n,
        };
        if writer.write_all(&buf[..n]).await.is_err() {
            break;
        }
        total += n as u64;
    }
    let _ = writer.shutdown().await;
    total
}

/// Reads until the end of the request head. Returns the head length and
/// the bytes read in total (the head plus any body bytes after it).
async fn read_head(
    stream: &mut TcpStream,
    buf: &mut [u8],
) -> std::io::Result<Option<(usize, usize)>> {
    let limit = buf.len().min(MAX_HEAD);
    let mut filled = 0;
    loop {
        if filled == limit {
            return Ok(None);
        }
        let n = stream.read(&mut buf[filled..limit]).await?;
        if n == 0 {
            return Ok(None);
        }
        let start = filled.saturating_sub(3);
        filled += n;
        if let Some(end) = find_head_end(&buf[start..filled]) {
            return Ok(Some((start + end, filled)));
        }
    }
}

fn find_head_end(bytes: &[u8]) -> Option<usize> {
    bytes
        .windows(4)
        .position(|w| w == b"\r\n\r\n")
        .map(|i| i + 4)
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum RequestKind {
    Connect {
        host: String,
        port: u16,
    },
    Forward {
        host: String,
        port: u16,
        path: String,
    },
    Relay {
        target: String,
    },
    /// Refused before it reaches a destination, for this reason.
    Refused(EgressReason),
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct ParsedRequest {
    method: String,
    kind: RequestKind,
    /// Headers to pass on, without hop-by-hop and proxy headers.
    headers: Vec<(String, Vec<u8>)>,
}

fn parse_request(head: &[u8]) -> Option<ParsedRequest> {
    let mut headers = [httparse::EMPTY_HEADER; 64];
    let mut request = httparse::Request::new(&mut headers);
    match request.parse(head) {
        Ok(httparse::Status::Complete(_)) => {}
        _ => return None,
    }
    let method = request.method?.to_string();
    let target = request.path?;
    let kind = if method == "CONNECT" {
        let (host, port) = split_authority(target, 443)?;
        RequestKind::Connect { host, port }
    } else if let Some(rest) = target.strip_prefix("http://") {
        // The authority ends at the first `/`, `?` or `#` (RFC 3986 3.2), so
        // a query on a host-only URL (`http://host?token=…`) is never read
        // as part of the host, where it would be recorded as the
        // destination. The fragment is never sent.
        let (authority, rest) = match rest.find(['/', '?', '#']) {
            Some(i) => (&rest[..i], &rest[i..]),
            None => (rest, ""),
        };
        let rest = rest.split('#').next().unwrap_or_default();
        let path = if rest.starts_with('/') {
            rest.to_string()
        } else {
            format!("/{rest}")
        };
        if authority.contains('@') {
            RequestKind::Refused(EgressReason::CredentialsInUrl)
        } else {
            let (host, port) = split_authority(authority, 80)?;
            RequestKind::Forward { host, port, path }
        }
    } else if target.starts_with("/.temps/") {
        match relay_target(&method, target) {
            Some(target) => RequestKind::Relay { target },
            None => RequestKind::Refused(EgressReason::RelayNotAllowed),
        }
    } else {
        RequestKind::Refused(EgressReason::NotAProxyRequest)
    };
    let headers = request
        .headers
        .iter()
        .filter(|h| {
            let name = h.name.to_ascii_lowercase();
            !matches!(
                name.as_str(),
                "proxy-authorization" | "proxy-connection" | "connection" | "keep-alive" | "host"
            )
        })
        .map(|h| (h.name.to_string(), h.value.to_vec()))
        .collect();
    Some(ParsedRequest {
        method,
        kind,
        headers,
    })
}

/// `host[:port]` or `[v6]:port`, lower-cased.
fn split_authority(authority: &str, default_port: u16) -> Option<(String, u16)> {
    let (host, port) = if let Some(rest) = authority.strip_prefix('[') {
        let (host, after) = rest.split_once(']')?;
        let port = match after.strip_prefix(':') {
            Some(p) => p.parse().ok()?,
            None if after.is_empty() => default_port,
            None => return None,
        };
        (host, port)
    } else {
        match authority.rsplit_once(':') {
            Some((host, port)) => (host, port.parse().ok()?),
            None => (authority, default_port),
        }
    };
    // A hostname or an address literal, nothing else: anything that would
    // make a credential or a query part of the host is not a destination.
    if host.is_empty()
        || port == 0
        || !host
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'-' | b'_' | b':'))
    {
        return None;
    }
    Some((host.trim_end_matches('.').to_ascii_lowercase(), port))
}

fn is_token(segment: &str) -> bool {
    segment.len() == 32
        && segment
            .bytes()
            .all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))
}

/// The control-plane path a relay request goes to, or `None` when the
/// path is not one the sidecar accepts. Mirrors the sidecar exactly: the
/// model relay (`POST v1/messages[/count_tokens]`, `responses`,
/// `chat/completions`; `GET models`), the MCP relay (`POST`), and the git
/// relay, which is refused until a relay credential exists.
pub fn relay_target(method: &str, target: &str) -> Option<String> {
    let (path, query) = match target.split_once('?') {
        Some((p, q)) => (p, Some(q)),
        None => (target, None),
    };
    if let Some(rest) = path.strip_prefix("/.temps/model-relay/") {
        let (token, operation) = rest.split_once('/')?;
        if !is_token(token) {
            return None;
        }
        let allowed = match (method, operation) {
            ("POST", "v1/messages" | "v1/messages/count_tokens") => {
                query.is_none() || query == Some("beta=true")
            }
            ("POST", "responses" | "chat/completions") => query.is_none(),
            ("GET", "models") => match query {
                None => true,
                Some(q) => q.strip_prefix("client_version=").is_some_and(|v| {
                    !v.is_empty()
                        && v.len() <= 64
                        && v.bytes().all(|b| {
                            b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'+' | b'-')
                        })
                }),
            },
            _ => false,
        };
        if !allowed {
            return None;
        }
        let query = query.map(|q| format!("?{q}")).unwrap_or_default();
        return Some(format!("/api/ai/sandbox-models/{token}/{operation}{query}"));
    }
    if let Some(token) = path.strip_prefix("/.temps/mcp/") {
        if method == "POST" && is_token(token) && query.is_none() {
            return Some(format!("/api/ai/sandbox-tools/{token}/mcp"));
        }
        return None;
    }
    // `/.temps/git/...` fails closed, as in the sidecar.
    None
}

/// Writes `METHOD path HTTP/1.1`, the kept headers, `Host` and
/// `Connection: close` into `out`. `None` when it does not fit.
fn rewrite_head(request: &ParsedRequest, path: &str, host: &str, out: &mut [u8]) -> Option<usize> {
    let mut len = 0;
    let mut push = |bytes: &[u8]| -> Option<()> {
        let end = len + bytes.len();
        if end > out.len() {
            return None;
        }
        out[len..end].copy_from_slice(bytes);
        len = end;
        Some(())
    };
    push(request.method.as_bytes())?;
    push(b" ")?;
    push(path.as_bytes())?;
    push(b" HTTP/1.1\r\nHost: ")?;
    push(host.as_bytes())?;
    push(b"\r\n")?;
    for (name, value) in &request.headers {
        push(name.as_bytes())?;
        push(b": ")?;
        push(value)?;
        push(b"\r\n")?;
    }
    push(b"Connection: close\r\n\r\n")?;
    Some(len)
}

const ESTABLISHED: &[u8] = b"HTTP/1.1 200 Connection Established\r\n\r\n";
const FORBIDDEN: &[u8] =
    b"HTTP/1.1 403 Forbidden\r\nConnection: close\r\nContent-Type: text/plain\r\nContent-Length: 19\r\n\r\ndestination denied\n";
const BAD_REQUEST: &[u8] =
    b"HTTP/1.1 400 Bad Request\r\nConnection: close\r\nContent-Length: 0\r\n\r\n";
const BAD_GATEWAY: &[u8] =
    b"HTTP/1.1 502 Bad Gateway\r\nConnection: close\r\nContent-Length: 0\r\n\r\n";
const SATURATED: &[u8] = b"HTTP/1.1 503 Service Unavailable\r\nConnection: close\r\nContent-Length: 23\r\n\r\negress proxy saturated\n";
const UNAVAILABLE: &[u8] =
    b"HTTP/1.1 503 Service Unavailable\r\nConnection: close\r\nContent-Length: 0\r\n\r\n";

#[cfg(test)]
mod tests;
