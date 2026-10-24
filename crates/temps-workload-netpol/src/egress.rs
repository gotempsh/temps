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

use std::collections::HashMap;
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr};
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

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

    /// Whether every resolved address may be connected to.
    pub fn check_addresses(
        &self,
        addrs: &[IpAddr],
        port: u16,
        admission: Admission,
    ) -> Result<(), Refusal> {
        if addrs.is_empty() {
            return Err(Refusal::Unresolved);
        }
        for &addr in addrs {
            let addr = unmap(addr);
            let in_cidr = |r: &&EgressRule| {
                matches!(&r.target, EgressTarget::Cidr(c) if c.contains(addr))
                    && r.port_matches(port)
            };
            if self.deny.iter().any(|r| in_cidr(&r)) {
                return Err(Refusal::Denied);
            }
            if self.allow.iter().any(|r| in_cidr(&r)) {
                continue;
            }
            if admission == Admission::ByAddress {
                return Err(Refusal::NotAllowed);
            }
            if !is_public(addr) {
                return Err(Refusal::Private);
            }
        }
        Ok(())
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
}

impl EgressProxy {
    pub fn new(
        config: EgressProxyConfig,
        table: Arc<ArcSwap<EgressTable>>,
    ) -> Result<Arc<Self>, EgressConfigError> {
        config.validate()?;
        Ok(Arc::new(Self {
            limit: Arc::new(Semaphore::new(config.max_connections)),
            pool: BufferPool::new(config.max_connections * 2, config.buffer_size),
            config,
            table,
            stats: Arc::new(EgressStats::default()),
        }))
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
                proxy.handle(stream, peer).await;
                proxy.stats.active.fetch_sub(1, Ordering::Relaxed);
                drop(permit);
            });
        }
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

    async fn handle(&self, mut client: TcpStream, peer: SocketAddr) {
        let source = unmap(peer.ip());
        let rules = self.table.load().lookup(source).cloned();
        let Some(rules) = rules else {
            self.stats
                .refused_unknown_source
                .fetch_add(1, Ordering::Relaxed);
            debug!(%source, reason = Refusal::UnknownSource.as_str(), "egress proxy refused");
            let _ = client.write_all(FORBIDDEN).await;
            return;
        };
        let (Some(mut up_buf), Some(mut down_buf)) = (self.pool.get(), self.pool.get()) else {
            // Cannot happen while the semaphore holds (two buffers per
            // permit), but never allocate past the pool.
            self.stats.refused_saturated.fetch_add(1, Ordering::Relaxed);
            let _ = client.write_all(UNAVAILABLE).await;
            return;
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
                return;
            }
        };
        let request = match parse_request(&up_buf[..head_len]) {
            Some(request) => request,
            None => {
                self.stats.malformed.fetch_add(1, Ordering::Relaxed);
                let _ = client.write_all(BAD_REQUEST).await;
                return;
            }
        };
        match request.kind {
            RequestKind::Connect { ref host, port } => {
                let Some(upstream) = self.open(&rules, source, host, port).await else {
                    let _ = client.write_all(FORBIDDEN).await;
                    return;
                };
                let Ok(mut upstream) = upstream else {
                    self.stats.upstream_failed.fetch_add(1, Ordering::Relaxed);
                    let _ = client.write_all(BAD_GATEWAY).await;
                    return;
                };
                if client.write_all(ESTABLISHED).await.is_err() {
                    return;
                }
                if filled > head_len && upstream.write_all(&up_buf[head_len..filled]).await.is_err()
                {
                    return;
                }
                self.stats.tunnels.fetch_add(1, Ordering::Relaxed);
                self.splice(client, upstream, up_buf, down_buf).await;
            }
            RequestKind::Forward {
                ref host,
                port,
                ref path,
            } => {
                let Some(upstream) = self.open(&rules, source, host, port).await else {
                    let _ = client.write_all(FORBIDDEN).await;
                    return;
                };
                let Ok(mut upstream) = upstream else {
                    self.stats.upstream_failed.fetch_add(1, Ordering::Relaxed);
                    let _ = client.write_all(BAD_GATEWAY).await;
                    return;
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
                    return;
                };
                if upstream.write_all(&down_buf[..head_len_out]).await.is_err()
                    || (filled > head_len
                        && upstream.write_all(&up_buf[head_len..filled]).await.is_err())
                {
                    return;
                }
                self.stats.forwarded.fetch_add(1, Ordering::Relaxed);
                self.splice(client, upstream, up_buf, down_buf).await;
            }
            RequestKind::Relay { ref target } => {
                let Some(relay) = self.config.relay.as_ref() else {
                    self.stats.refused_policy.fetch_add(1, Ordering::Relaxed);
                    let _ = client.write_all(FORBIDDEN).await;
                    return;
                };
                let connect = tokio::time::timeout(
                    self.config.connect_timeout,
                    TcpStream::connect(relay.authority.as_str()),
                )
                .await;
                let Ok(Ok(mut upstream)) = connect else {
                    self.stats.upstream_failed.fetch_add(1, Ordering::Relaxed);
                    let _ = client.write_all(BAD_GATEWAY).await;
                    return;
                };
                let path = format!("{}{target}", relay.base_path);
                let Some(head_len_out) =
                    rewrite_head(&request, &path, &relay.authority, &mut down_buf)
                else {
                    self.stats.malformed.fetch_add(1, Ordering::Relaxed);
                    let _ = client.write_all(BAD_REQUEST).await;
                    return;
                };
                if upstream.write_all(&down_buf[..head_len_out]).await.is_err()
                    || (filled > head_len
                        && upstream.write_all(&up_buf[head_len..filled]).await.is_err())
                {
                    return;
                }
                self.stats.relayed.fetch_add(1, Ordering::Relaxed);
                self.splice(client, upstream, up_buf, down_buf).await;
            }
            RequestKind::Refused => {
                self.stats.refused_policy.fetch_add(1, Ordering::Relaxed);
                let _ = client.write_all(FORBIDDEN).await;
            }
        }
    }

    /// Checks and connects. `None`: refused by policy (already counted
    /// and logged). `Some(Err)`: the upstream did not answer.
    async fn open(
        &self,
        rules: &CompiledRules,
        source: IpAddr,
        host: &str,
        port: u16,
    ) -> Option<Result<TcpStream, ()>> {
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
        };
        let admission = match rules.check_host(host, port) {
            Ok(admission) => admission,
            Err(refusal) => {
                refuse(refusal);
                return None;
            }
        };
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
        if let Err(refusal) = rules.check_addresses(&addrs, port, admission) {
            refuse(refusal);
            return None;
        }
        let target = addrs
            .iter()
            .find(|a| a.is_ipv4())
            .or_else(|| addrs.first())
            .copied()?;
        let connect = tokio::time::timeout(
            self.config.connect_timeout,
            TcpStream::connect(SocketAddr::new(target, port)),
        )
        .await;
        Some(match connect {
            Ok(Ok(stream)) => Ok(stream),
            _ => Err(()),
        })
    }

    async fn splice(
        &self,
        client: TcpStream,
        upstream: TcpStream,
        mut up_buf: PooledBuffer,
        mut down_buf: PooledBuffer,
    ) {
        let (mut client_read, mut client_write) = client.into_split();
        let (mut upstream_read, mut upstream_write) = upstream.into_split();
        let idle = self.config.idle_timeout;
        let up = pump(&mut client_read, &mut upstream_write, &mut up_buf, idle);
        let down = pump(&mut upstream_read, &mut client_write, &mut down_buf, idle);
        let (sent, received) = tokio::join!(up, down);
        self.stats.bytes_up.fetch_add(sent, Ordering::Relaxed);
        self.stats.bytes_down.fetch_add(received, Ordering::Relaxed);
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
    Refused,
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
        let (authority, path) = match rest.find('/') {
            Some(i) => (&rest[..i], &rest[i..]),
            None => (rest, "/"),
        };
        if authority.contains('@') {
            RequestKind::Refused
        } else {
            let (host, port) = split_authority(authority, 80)?;
            RequestKind::Forward {
                host,
                port,
                path: path.to_string(),
            }
        }
    } else if target.starts_with("/.temps/") {
        match relay_target(&method, target) {
            Some(target) => RequestKind::Relay { target },
            None => RequestKind::Refused,
        }
    } else {
        RequestKind::Refused
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
    if host.is_empty() || port == 0 {
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
mod tests {
    use super::*;
    use temps_workload::WorkloadKind;

    fn sandbox(id: i64) -> WorkloadRef {
        WorkloadRef::new(WorkloadKind::Sandbox, id)
    }

    fn host_rule(id: i64, pattern: &str, action: Action) -> EgressRule {
        EgressRule {
            id,
            target: EgressTarget::Host(HostPattern::parse(pattern).unwrap()),
            ports: None,
            action,
        }
    }

    fn cidr_rule(id: i64, cidr: &str, port: Option<u16>, action: Action) -> EgressRule {
        EgressRule {
            id,
            target: EgressTarget::Cidr(cidr.parse().unwrap()),
            ports: port.map(PortRange::single),
            action,
        }
    }

    #[test]
    fn host_patterns_match_exact_names_subdomains_and_everything() {
        let exact = HostPattern::parse("API.Example.com.").unwrap();
        assert!(exact.matches("api.example.com"));
        assert!(!exact.matches("x.api.example.com"));
        let sub = HostPattern::parse("*.example.com").unwrap();
        assert!(sub.matches("a.example.com"));
        assert!(sub.matches("a.b.example.com"));
        assert!(!sub.matches("example.com"), "not the apex");
        assert!(!sub.matches("badexample.com"));
        assert!(HostPattern::parse("*").unwrap().matches("anything.test"));
        for bad in ["", "*.", "a..b", "exa mple.com", "*.*.com", "-a.com", "a/b"] {
            assert!(HostPattern::parse(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn deny_beats_allow_and_ports_default_to_web() {
        let rules = CompiledRules::new(&EgressPolicy {
            subject: sandbox(1),
            rules: vec![
                host_rule(1, "*", Action::Allow),
                host_rule(2, "*.blocked.test", Action::Deny),
            ],
        });
        assert_eq!(
            rules.check_host("registry.npmjs.org", 443),
            Ok(Admission::ByRule)
        );
        assert_eq!(
            rules.check_host("registry.npmjs.org", 80),
            Ok(Admission::ByRule)
        );
        assert_eq!(
            rules.check_host("registry.npmjs.org", 22),
            Err(Refusal::Port)
        );
        assert_eq!(
            rules.check_host("x.blocked.test", 443),
            Err(Refusal::Denied)
        );
    }

    #[test]
    fn a_restricted_workload_without_rules_reaches_nothing() {
        let rules = CompiledRules::new(&EgressPolicy {
            subject: sandbox(1),
            rules: vec![],
        });
        assert_eq!(
            rules.check_host("example.com", 443),
            Err(Refusal::NotAllowed)
        );
        assert_eq!(
            rules.check_host("192.0.2.10", 443),
            Err(Refusal::NotAllowed)
        );
    }

    #[test]
    fn private_answers_are_refused_unless_a_cidr_allows_them() {
        let rules = CompiledRules::new(&EgressPolicy {
            subject: sandbox(1),
            rules: vec![
                host_rule(1, "*", Action::Allow),
                cidr_rule(2, "10.237.9.0/24", Some(8080), Action::Allow),
            ],
        });
        let public: IpAddr = "93.184.216.34".parse().unwrap();
        let private: IpAddr = "10.0.0.5".parse().unwrap();
        assert_eq!(
            rules.check_addresses(&[public], 443, Admission::ByRule),
            Ok(())
        );
        assert_eq!(
            rules.check_addresses(&[public, private], 443, Admission::ByRule),
            Err(Refusal::Private),
            "one private answer refuses the whole name (DNS rebinding)"
        );
        assert_eq!(
            rules.check_addresses(&[], 443, Admission::ByRule),
            Err(Refusal::Unresolved)
        );
        let allowed: IpAddr = "10.237.9.4".parse().unwrap();
        assert_eq!(rules.check_host("10.237.9.4", 8080), Ok(Admission::ByRule));
        assert_eq!(
            rules.check_addresses(&[allowed], 8080, Admission::ByRule),
            Ok(())
        );
        assert_eq!(
            rules.check_addresses(&[allowed], 9090, Admission::ByRule),
            Err(Refusal::Private),
            "the CIDR allow names one port"
        );
    }

    #[test]
    fn an_unrelated_cidr_allow_does_not_open_public_literals() {
        let rules = CompiledRules::new(&EgressPolicy {
            subject: sandbox(1),
            rules: vec![cidr_rule(1, "10.20.0.0/16", Some(443), Action::Allow)],
        });
        assert_eq!(
            rules.check_host("93.184.216.34", 22),
            Err(Refusal::NotAllowed),
            "a literal outside every allowed CIDR is refused, whatever the port"
        );
        assert_eq!(
            rules.check_host("93.184.216.34", 443),
            Err(Refusal::NotAllowed)
        );
        assert_eq!(
            rules.check_host("10.20.3.4", 22),
            Err(Refusal::Port),
            "the CIDR allow names one port"
        );
        assert_eq!(rules.check_host("10.20.3.4", 443), Ok(Admission::ByRule));

        // A hostname with no host rule is decided by its addresses, and only
        // an allowed CIDR admits them: a public answer is not enough.
        assert_eq!(
            rules.check_host("internal.example", 443),
            Ok(Admission::ByAddress)
        );
        let public: IpAddr = "93.184.216.34".parse().unwrap();
        let inside: IpAddr = "10.20.3.4".parse().unwrap();
        assert_eq!(
            rules.check_addresses(&[public], 443, Admission::ByAddress),
            Err(Refusal::NotAllowed)
        );
        assert_eq!(
            rules.check_addresses(&[inside], 443, Admission::ByAddress),
            Ok(())
        );
        assert_eq!(
            rules.check_addresses(&[inside], 22, Admission::ByAddress),
            Err(Refusal::NotAllowed)
        );
        assert_eq!(
            rules.check_addresses(&[inside, public], 443, Admission::ByAddress),
            Err(Refusal::NotAllowed),
            "one answer outside the allowed CIDRs refuses the whole name"
        );
    }

    #[test]
    fn a_cidr_deny_refuses_public_addresses_too() {
        let rules = CompiledRules::new(&EgressPolicy {
            subject: sandbox(1),
            rules: vec![
                host_rule(1, "*", Action::Allow),
                cidr_rule(2, "93.184.216.0/24", None, Action::Deny),
            ],
        });
        assert_eq!(
            rules.check_addresses(&["93.184.216.34".parse().unwrap()], 443, Admission::ByRule),
            Err(Refusal::Denied)
        );
    }

    #[test]
    fn the_public_check_matches_the_sidecar() {
        for private in [
            "0.1.2.3",
            "10.1.1.1",
            "127.0.0.1",
            "100.64.0.1",
            "169.254.169.254",
            "172.16.0.1",
            "172.31.255.255",
            "192.0.0.1",
            "192.0.2.1",
            "192.88.99.1",
            "192.168.1.1",
            "198.18.0.1",
            "198.51.100.1",
            "203.0.113.1",
            "224.0.0.1",
            "255.255.255.255",
            "::1",
            "fe80::1",
            "fd00::1",
            "2001:db8::1",
            "2001:2::1",
            "::ffff:10.0.0.1",
        ] {
            assert!(!is_public(private.parse().unwrap()), "{private}");
        }
        for public in [
            "1.1.1.1",
            "172.32.0.1",
            "100.128.0.1",
            "2606:4700::1111",
            "::ffff:1.1.1.1",
        ] {
            assert!(is_public(public.parse().unwrap()), "{public}");
        }
    }

    #[test]
    fn requests_parse_into_tunnels_forwards_and_relays() {
        let connect =
            parse_request(b"CONNECT Example.com:443 HTTP/1.1\r\nHost: example.com:443\r\n\r\n")
                .unwrap();
        assert_eq!(
            connect.kind,
            RequestKind::Connect {
                host: "example.com".into(),
                port: 443
            }
        );
        let forward = parse_request(
            b"GET http://example.com/a?b=1 HTTP/1.1\r\nHost: example.com\r\nProxy-Authorization: x\r\nAccept: */*\r\n\r\n",
        )
        .unwrap();
        assert_eq!(
            forward.kind,
            RequestKind::Forward {
                host: "example.com".into(),
                port: 80,
                path: "/a?b=1".into()
            }
        );
        assert_eq!(
            forward.headers,
            vec![("Accept".to_string(), b"*/*".to_vec())]
        );
        let v6 = parse_request(b"CONNECT [2606:4700::1111]:443 HTTP/1.1\r\n\r\n").unwrap();
        assert_eq!(
            v6.kind,
            RequestKind::Connect {
                host: "2606:4700::1111".into(),
                port: 443
            }
        );
        assert_eq!(
            parse_request(b"GET http://user:pw@example.com/ HTTP/1.1\r\n\r\n")
                .unwrap()
                .kind,
            RequestKind::Refused
        );
        assert_eq!(
            parse_request(b"GET /etc/passwd HTTP/1.1\r\n\r\n")
                .unwrap()
                .kind,
            RequestKind::Refused
        );
        assert!(parse_request(b"not http\r\n\r\n").is_none());
    }

    #[test]
    fn relay_paths_match_the_sidecar() {
        let token = "0123456789abcdef0123456789abcdef";
        assert_eq!(
            relay_target(
                "POST",
                &format!("/.temps/model-relay/{token}/v1/messages?beta=true")
            ),
            Some(format!(
                "/api/ai/sandbox-models/{token}/v1/messages?beta=true"
            ))
        );
        assert_eq!(
            relay_target("POST", &format!("/.temps/model-relay/{token}/responses")),
            Some(format!("/api/ai/sandbox-models/{token}/responses"))
        );
        assert_eq!(
            relay_target(
                "GET",
                &format!("/.temps/model-relay/{token}/models?client_version=1.2.3")
            ),
            Some(format!(
                "/api/ai/sandbox-models/{token}/models?client_version=1.2.3"
            ))
        );
        assert_eq!(
            relay_target("POST", &format!("/.temps/mcp/{token}")),
            Some(format!("/api/ai/sandbox-tools/{token}/mcp"))
        );
        for refused in [
            format!("/.temps/model-relay/{token}/responses?x=1"),
            "/.temps/model-relay/short/responses".to_string(),
            format!("/.temps/git/{token}/1/info/refs?service=git-upload-pack"),
            format!("/.temps/mcp/{token}?x"),
        ] {
            assert_eq!(relay_target("POST", &refused), None, "{refused}");
        }
        assert_eq!(
            relay_target("POST", &format!("/.temps/model-relay/{token}/models")),
            None,
            "the model list is GET only"
        );
        assert_eq!(
            relay_target(
                "GET",
                &format!("/.temps/model-relay/{token}/models?client_version=a b")
            ),
            None
        );
    }

    #[test]
    fn relay_upstreams_are_plain_http_origins() {
        assert_eq!(
            RelayUpstream::parse("http://127.0.0.1:3000/").unwrap(),
            RelayUpstream {
                authority: "127.0.0.1:3000".into(),
                base_path: String::new()
            }
        );
        assert_eq!(
            RelayUpstream::parse("http://console.internal/base").unwrap(),
            RelayUpstream {
                authority: "console.internal:80".into(),
                base_path: "/base".into()
            }
        );
        assert!(RelayUpstream::parse("https://cp.example.test").is_err());
        assert!(RelayUpstream::parse("http://u:p@cp.example.test").is_err());
    }

    #[test]
    fn the_pool_never_allocates_past_its_capacity() {
        let pool = BufferPool::new(2, 16);
        let a = pool.get().unwrap();
        let b = pool.get().unwrap();
        assert!(pool.get().is_none());
        drop(a);
        let c = pool.get().unwrap();
        assert_eq!(c.len(), 16);
        assert_eq!(pool.allocated(), 2);
        drop((b, c));
        assert_eq!(pool.allocated(), 2);
    }

    #[test]
    fn heads_are_rewritten_to_origin_form_and_closed() {
        let request = parse_request(b"POST http://example.com/x HTTP/1.1\r\nHost: example.com\r\nConnection: keep-alive\r\nContent-Length: 2\r\n\r\n").unwrap();
        let mut out = [0u8; 256];
        let len = rewrite_head(&request, "/x", "example.com", &mut out).unwrap();
        assert_eq!(
            std::str::from_utf8(&out[..len]).unwrap(),
            "POST /x HTTP/1.1\r\nHost: example.com\r\nContent-Length: 2\r\nConnection: close\r\n\r\n"
        );
        let mut tiny = [0u8; 8];
        assert!(rewrite_head(&request, "/x", "example.com", &mut tiny).is_none());
    }

    #[test]
    fn limits_are_validated() {
        let mut config = EgressProxyConfig::new("127.0.0.1:0".parse().unwrap());
        assert!(config.validate().is_ok());
        config.max_connections = 0;
        assert!(config.validate().is_err());
        config.max_connections = 4;
        config.buffer_size = 100;
        assert!(config.validate().is_err());
    }

    async fn echo_server() -> SocketAddr {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            while let Ok((mut stream, _)) = listener.accept().await {
                tokio::spawn(async move {
                    let mut buf = [0u8; 1024];
                    while let Ok(n) = stream.read(&mut buf).await {
                        if n == 0 || stream.write_all(&buf[..n]).await.is_err() {
                            break;
                        }
                    }
                });
            }
        });
        addr
    }

    async fn start(
        rules: Vec<EgressRule>,
        max_connections: usize,
    ) -> (Arc<EgressProxy>, SocketAddr) {
        let identities = IdentityMap::new().with(sandbox(1), "127.0.0.1".parse().unwrap());
        let table = EgressTable::build(
            &[EgressPolicy {
                subject: sandbox(1),
                rules,
            }],
            &identities,
        );
        let mut config = EgressProxyConfig::new("127.0.0.1:0".parse().unwrap());
        config.max_connections = max_connections;
        config.head_timeout = Duration::from_secs(5);
        let proxy = EgressProxy::new(config, Arc::new(ArcSwap::from_pointee(table))).unwrap();
        let listener = proxy.bind().await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(proxy.clone().serve(listener));
        (proxy, addr)
    }

    async fn status_line(stream: &mut TcpStream) -> String {
        let mut buf = vec![0u8; 512];
        let mut filled = 0;
        loop {
            let n = stream.read(&mut buf[filled..]).await.unwrap_or(0);
            filled += n;
            if n == 0 || find_head_end(&buf[..filled]).is_some() {
                break;
            }
        }
        String::from_utf8_lossy(&buf[..filled])
            .lines()
            .next()
            .unwrap_or_default()
            .to_string()
    }

    #[tokio::test]
    async fn a_cidr_allowed_tunnel_carries_bytes_both_ways() {
        let echo = echo_server().await;
        let (proxy, addr) = start(
            vec![cidr_rule(
                1,
                "127.0.0.1/32",
                Some(echo.port()),
                Action::Allow,
            )],
            4,
        )
        .await;
        let mut client = TcpStream::connect(addr).await.unwrap();
        client
            .write_all(format!("CONNECT 127.0.0.1:{} HTTP/1.1\r\n\r\n", echo.port()).as_bytes())
            .await
            .unwrap();
        assert_eq!(
            status_line(&mut client).await,
            "HTTP/1.1 200 Connection Established"
        );
        client.write_all(b"ping").await.unwrap();
        let mut got = [0u8; 4];
        client.read_exact(&mut got).await.unwrap();
        assert_eq!(&got, b"ping");
        assert_eq!(proxy.stats().snapshot().tunnels, 1);
    }

    #[tokio::test]
    async fn names_resolving_to_private_addresses_are_refused() {
        let echo = echo_server().await;
        let (proxy, addr) = start(
            vec![EgressRule {
                id: 1,
                target: EgressTarget::Host(HostPattern::parse("*").unwrap()),
                ports: Some(PortRange::single(echo.port())),
                action: Action::Allow,
            }],
            4,
        )
        .await;
        let mut client = TcpStream::connect(addr).await.unwrap();
        client
            .write_all(format!("CONNECT localhost:{} HTTP/1.1\r\n\r\n", echo.port()).as_bytes())
            .await
            .unwrap();
        assert_eq!(status_line(&mut client).await, "HTTP/1.1 403 Forbidden");
        assert_eq!(proxy.stats().snapshot().refused_private, 1);
    }

    #[tokio::test]
    async fn an_unknown_source_is_refused() {
        let echo = echo_server().await;
        let mut config = EgressProxyConfig::new("127.0.0.1:0".parse().unwrap());
        config.max_connections = 2;
        let proxy = EgressProxy::new(
            config,
            Arc::new(ArcSwap::from_pointee(EgressTable::default())),
        )
        .unwrap();
        let listener = proxy.bind().await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(proxy.clone().serve(listener));
        let mut client = TcpStream::connect(addr).await.unwrap();
        client
            .write_all(format!("CONNECT 127.0.0.1:{} HTTP/1.1\r\n\r\n", echo.port()).as_bytes())
            .await
            .unwrap();
        assert_eq!(status_line(&mut client).await, "HTTP/1.1 403 Forbidden");
        assert_eq!(proxy.stats().snapshot().refused_unknown_source, 1);
    }

    #[tokio::test]
    async fn at_the_connection_limit_new_connections_get_503_and_are_counted() {
        let (proxy, addr) = start(vec![], 1).await;
        // Holds the only permit: connected, head not sent yet.
        let _held = TcpStream::connect(addr).await.unwrap();
        for _ in 0..50 {
            if proxy.stats().snapshot().active == 1 {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        let mut refused = TcpStream::connect(addr).await.unwrap();
        assert_eq!(
            status_line(&mut refused).await,
            "HTTP/1.1 503 Service Unavailable"
        );
        let stats = proxy.stats().snapshot();
        assert_eq!(stats.refused_saturated, 1);
        assert_eq!(stats.active, 1);
        assert!(
            proxy.buffers_allocated() <= 2,
            "buffers stay within the pool"
        );
    }
}
