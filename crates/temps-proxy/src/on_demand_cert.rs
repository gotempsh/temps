//! On-demand HTTP-01 TLS certificate manager (ADR-018).
//!
//! When on-demand TLS is enabled, the proxy's `certificate_callback` finds no
//! cert for an SNI and — instead of silently failing the handshake — asks this
//! manager to provision one in the background. The first request to a brand-new
//! hostname still fails the TLS handshake (Option B, ADR §1: fail-fast, issue in
//! background); within a few seconds the cert is issued and the client's retry
//! succeeds.
//!
//! Structurally this mirrors [`crate::on_demand::OnDemandManager`] (scale-to-
//! zero): a hot-path trigger publishes a job onto a bounded in-process channel, a
//! background task consumes it, and an in-process [`DashMap`] caches per-host
//! state so the hot path never touches the DB.
//!
//! ## The gate (`try_enqueue`)
//!
//! Every enqueue decision is made by a sequence of O(1), I/O-free checks
//! (ADR §2). A hostname is enqueued for issuance only if ALL hold:
//!   1. it is a **direct** subdomain of the configured on-demand zone;
//!   2. it has a route in the proxy's in-memory route table AND that route is
//!      **cert-eligible** (stable env/console host, NOT an ephemeral
//!      per-deployment hostname — see [`temps_routes::RouteInfo::cert_eligible`]);
//!   3. it is not already `pending`/`issuing` (in-flight dedup);
//!   4. it is not inside an active failure backoff window;
//!   5. it is under the global hourly cap, and — WHEN a peer IP is supplied —
//!      the per-IP novelty cap.
//!
//! NOTE on the per-IP cap: the sole production trigger is the TLS SNI callback,
//! which cannot observe the client IP (OpenSSL does not expose it there), so
//! `try_enqueue` is currently called with `peer_ip = None` and the per-IP check
//! does not engage. The real random-SNI flood defenses are therefore checks 1-2
//! (zone + cert-eligible route), which reject before ANY state mutation, plus
//! the global hourly cap (check 5). The per-IP limiter remains implemented and
//! tested so an HTTP-path trigger (where `session.client_addr()` IS available)
//! can activate it without re-adding the logic; it is defense-in-depth, not the
//! primary control. (ADR-018 security review, MEDIUM/LOW.)
//!
//! The actual ACME flow lives in `temps-domains`
//! (`DomainService::provision_on_demand`); this module only triggers it via the
//! injected [`OnDemandCertProvisioner`] trait, so the proxy crate stays
//! decoupled from the ACME client (same anti-coupling pattern as
//! [`crate::on_demand::ContainerLifecycle`]).

use async_trait::async_trait;
use dashmap::DashMap;
use std::net::IpAddr;
use std::sync::atomic::{AtomicU32, AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};
use temps_routes::CachedPeerTable;
use thiserror::Error;
use tokio::sync::{mpsc, Semaphore};
use tracing::{debug, error, info, warn};

/// Provisions a certificate for a single hostname via the full ACME HTTP-01
/// flow. Implemented in the binary by adapting `temps-domains::DomainService`
/// (`provision_on_demand`), so this crate does not depend on the ACME client.
///
/// Mirrors [`crate::on_demand::ContainerLifecycle`]: a narrow trait injected by
/// the wiring layer, keeping the manager unit-testable with a fake.
#[async_trait]
pub trait OnDemandCertProvisioner: Send + Sync {
    /// Run the ACME HTTP-01 flow for `hostname` using `email` as the ACME
    /// account contact. Returns the full error chain (already flattened across
    /// `source()` levels by the implementation) on failure, plus whether the
    /// failure was a Let's Encrypt rate limit and the resulting backoff deadline
    /// (epoch seconds) the implementation persisted, so the in-process negative
    /// cache can mirror the DB without a round-trip.
    async fn provision(&self, hostname: &str, email: &str) -> Result<(), CertProvisionFailure>;
}

/// Failure detail returned by [`OnDemandCertProvisioner::provision`]. Carries
/// enough for the manager to update its in-process negative cache to match the
/// backoff the orchestration layer already persisted on the `domains` row.
#[derive(Debug, Clone)]
pub struct CertProvisionFailure {
    /// Full `Display` chain of the underlying error (all `source()` levels).
    pub error_chain: String,
    /// Coarse category mirrored from the audit row (`rate_limited`, `timeout`, …).
    pub category: String,
    /// Backoff deadline (epoch seconds) the orchestration layer computed and
    /// persisted, used to seed the in-process negative cache. `None` falls back
    /// to the manager's own exponential ladder.
    pub backoff_until_epoch: Option<u64>,
}

#[derive(Error, Debug)]
pub enum OnDemandCertError {
    #[error("on-demand cert job channel is closed")]
    ChannelClosed,
}

/// In-process per-host state cache (ADR §3). The authoritative state is the
/// `domains` row; this is the hot-path cache the gate reads to dedup in-flight
/// issuance and enforce the negative-cache backoff without a DB lookup.
#[derive(Clone, Debug)]
pub enum OnDemandCertState {
    /// Job enqueued, not yet picked up by the consumer.
    Pending,
    /// Consumer is actively running the ACME flow.
    Issuing,
    /// Last attempt failed; do not retry until `backoff_until_epoch` (epoch secs).
    Failed { backoff_until_epoch: u64 },
}

/// Why an enqueue attempt did or did not result in a queued issuance job. Maps
/// 1:1 onto the `on_demand_cert_attempts.outcome` "skipped_*" values for the
/// rejection cases, so the caller (Layer 4) can record an audit row for skips.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum EnqueueOutcome {
    /// Job was published onto the issuance channel.
    Enqueued,
    /// Hostname is not a direct subdomain of the on-demand zone (or no zone).
    SkippedGate,
    /// No cert-eligible route exists for this hostname (ephemeral or unknown).
    SkippedNoRoute,
    /// Already pending or issuing — deduplicated.
    SkippedDuplicate,
    /// Inside an active failure backoff window.
    SkippedBackoff,
    /// Per-IP novelty cap or global hourly cap exhausted.
    SkippedRateLimit,
    /// On-demand TLS is disabled, or the issuance channel is full/closed.
    SkippedDisabled,
}

/// A unit of work for the background issuance consumer.
#[derive(Clone, Debug)]
struct OnDemandCertJob {
    hostname: String,
}

/// Tunables for the manager, sourced from `OnDemandTlsSettings` at startup.
#[derive(Clone, Debug)]
pub struct OnDemandCertConfig {
    /// On-demand zone suffix (e.g. `1.2.3.4.sslip.io`). A hostname passes the
    /// gate's first check iff it is a direct subdomain of this zone.
    pub zone: String,
    /// ACME account contact email passed through to the provisioner.
    pub email: String,
    /// Concurrent issuance semaphore size (ADR §4 Layer 1).
    pub max_concurrent: u32,
    /// Global per-hour issuance cap (ADR §4 Layer 3).
    pub hourly_cap: u32,
    /// The instance's console host (`console.<zone>`), when on a sslip.io-style
    /// install. The console is served as a fall-through, not a `CachedPeerTable`
    /// route, so the gate's cert-eligible-route check (check 2) would otherwise
    /// reject it. It is nonetheless a stable, low-cardinality host that should
    /// get on-demand HTTPS — so the gate exempts exactly this one hostname from
    /// the route check. `None` (custom-domain installs, or no derivable console
    /// host) means no exemption: the console must get its cert another way.
    pub console_host: Option<String>,
}

/// Max novel hostnames a single source IP may trigger per minute (ADR §4
/// random-SNI flood mitigation, final layer).
const MAX_NOVEL_HOSTNAMES_PER_IP_PER_MINUTE: u32 = 5;

/// Per-IP novelty window length, in seconds.
const PER_IP_WINDOW_SECS: u64 = 60;

/// Global hourly-cap window length, in seconds.
const HOURLY_WINDOW_SECS: u64 = 3600;

/// Hard ceiling on a single issuance attempt. A hung Let's Encrypt endpoint (or
/// the network to it) must never permanently hold a concurrency permit: with
/// `max_concurrent` slots, that many hangs would wedge the feature until process
/// restart. On timeout we abort the task, record a `timeout` failure with
/// backoff, and free the permit. (ADR-018 security review, MEDIUM.)
const ISSUANCE_TIMEOUT_SECS: u64 = 120;

/// Bounded issuance job channel capacity. Generous enough to absorb a burst of
/// distinct stable hostnames after a cold start, small enough to bound memory;
/// when full, `try_enqueue` returns `SkippedDisabled` rather than blocking the
/// Pingora hot path.
const JOB_CHANNEL_CAPACITY: usize = 256;

/// Per-IP novelty counter with a rolling window.
struct IpNoveltyCounter {
    window_start_epoch: AtomicU64,
    count: AtomicU32,
}

/// On-demand certificate manager. Lives in the proxy process.
pub struct OnDemandCertManager {
    config: OnDemandCertConfig,

    /// Hot-path per-host state cache (authoritative state is the `domains` row).
    state: DashMap<String, OnDemandCertState>,

    /// Bounded channel into the background issuance consumer.
    job_tx: mpsc::Sender<OnDemandCertJob>,

    /// Proxy in-memory route table, for the gate's cert-eligible route check.
    route_table: Arc<CachedPeerTable>,

    /// Per-source-IP novelty limiter (ADR §4 random-SNI flood final layer).
    ip_novelty: DashMap<IpAddr, IpNoveltyCounter>,

    /// Global hourly issuance counter window-start (epoch seconds).
    hourly_window_start: AtomicU64,
    /// Issuances counted in the current hourly window.
    hourly_count: AtomicU32,
}

impl std::fmt::Debug for OnDemandCertManager {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // Channels, the semaphore, and DashMaps aren't usefully Debug-printable;
        // surface the tunables and live state size so `ProxyConfig`'s derived
        // Debug stays meaningful without dumping internal machinery.
        f.debug_struct("OnDemandCertManager")
            .field("zone", &self.config.zone)
            .field("max_concurrent", &self.config.max_concurrent)
            .field("hourly_cap", &self.config.hourly_cap)
            .field("tracked_hosts", &self.state.len())
            .finish_non_exhaustive()
    }
}

impl OnDemandCertManager {
    /// Build the manager and its background consumer task. The consumer is
    /// spawned onto the current Tokio runtime; callers wire this up at proxy
    /// startup. Returns an `Arc` so the same instance backs both the TLS
    /// callback (`try_enqueue`) and any future surfaces.
    pub fn new(
        config: OnDemandCertConfig,
        route_table: Arc<CachedPeerTable>,
        provisioner: Arc<dyn OnDemandCertProvisioner>,
    ) -> Arc<Self> {
        let (job_tx, job_rx) = mpsc::channel(JOB_CHANNEL_CAPACITY);

        let manager = Arc::new(Self {
            config,
            state: DashMap::new(),
            job_tx,
            route_table,
            ip_novelty: DashMap::new(),
            hourly_window_start: AtomicU64::new(now_epoch_secs()),
            hourly_count: AtomicU32::new(0),
        });

        manager.clone().spawn_consumer(job_rx, provisioner);
        manager
    }

    fn current_epoch_secs(&self) -> u64 {
        now_epoch_secs()
    }

    /// The gate (ADR §2 + §4). Pure, O(1), no DB I/O — safe in the Pingora TLS
    /// callback hot path. Publishes an issuance job and returns
    /// [`EnqueueOutcome::Enqueued`] only when every check passes; otherwise it
    /// returns the specific skip reason so the caller can record an audit row.
    pub fn try_enqueue(&self, sni: &str, peer_ip: Option<IpAddr>) -> EnqueueOutcome {
        let hostname = sni.trim().to_ascii_lowercase();

        // Check 1 — direct subdomain of the configured zone.
        if !self.is_direct_subdomain_of_zone(&hostname) {
            debug!(sni = %hostname, "on-demand cert gate: out of zone");
            return EnqueueOutcome::SkippedGate;
        }

        // Check 2 — a cert-eligible route exists (stable, not ephemeral).
        // Resolve across all lookup strategies (TLS/SNI, HTTP-host, legacy) so
        // stable env hostnames stored in the legacy routes map are seen.
        //
        // Exemption: the console host (`console.<zone>`) is served as a
        // fall-through, not a `CachedPeerTable` route, so it has no route entry
        // to satisfy this check. It is nonetheless a stable, single host that
        // should get on-demand HTTPS, so it bypasses the route check. Everything
        // else still requires a cert-eligible route, which is what keeps a
        // random-SNI flood from creating issuance jobs.
        if !self.is_console_host(&hostname) {
            match self.route_table.resolve_route_for_sni(&hostname) {
                Some(route) if route.cert_eligible => {}
                _ => {
                    debug!(sni = %hostname, "on-demand cert gate: no cert-eligible route");
                    return EnqueueOutcome::SkippedNoRoute;
                }
            }
        }

        // Checks 3 & 4 — in-flight dedup and active backoff window. Read-only
        // here; the authoritative pending-mark is done atomically below via the
        // DashMap `entry` API so two concurrent callbacks for the same SNI can't
        // both enqueue.
        if let Some(entry) = self.state.get(&hostname) {
            match entry.value() {
                OnDemandCertState::Pending | OnDemandCertState::Issuing => {
                    debug!(sni = %hostname, "on-demand cert gate: already in flight");
                    return EnqueueOutcome::SkippedDuplicate;
                }
                OnDemandCertState::Failed {
                    backoff_until_epoch,
                } => {
                    if *backoff_until_epoch > self.current_epoch_secs() {
                        debug!(sni = %hostname, "on-demand cert gate: in backoff");
                        return EnqueueOutcome::SkippedBackoff;
                    }
                    // Backoff elapsed — fall through and re-enqueue.
                }
            }
        }

        // Check 5a — per-IP novelty limit.
        if let Some(ip) = peer_ip {
            if !self.allow_ip_novelty(ip) {
                warn!(sni = %hostname, peer_ip = %ip, "on-demand cert gate: per-IP novelty cap");
                return EnqueueOutcome::SkippedRateLimit;
            }
        }

        // Check 5b — global hourly cap.
        if !self.allow_hourly_issuance() {
            warn!(sni = %hostname, "on-demand cert gate: global hourly cap reached");
            return EnqueueOutcome::SkippedRateLimit;
        }

        // Atomically claim the pending slot. The DashMap `entry` write lock
        // serializes concurrent callbacks for the same SNI: exactly one caller
        // transitions the entry to `Pending` and proceeds to enqueue; a racing
        // caller that finds it already `Pending`/`Issuing` dedups, and a `Failed`
        // entry still inside its backoff window is rejected. (Backoff is
        // re-checked here under the lock to close the read-check→claim race.) A
        // `Failed` entry whose backoff has elapsed, or no entry at all, is
        // claimed fresh.
        let now = self.current_epoch_secs();
        let claim = match self.state.entry(hostname.clone()) {
            dashmap::mapref::entry::Entry::Occupied(mut occ) => match occ.get() {
                OnDemandCertState::Pending | OnDemandCertState::Issuing => {
                    Err(EnqueueOutcome::SkippedDuplicate)
                }
                OnDemandCertState::Failed {
                    backoff_until_epoch,
                } if *backoff_until_epoch > now => Err(EnqueueOutcome::SkippedBackoff),
                OnDemandCertState::Failed { .. } => {
                    occ.insert(OnDemandCertState::Pending);
                    Ok(())
                }
            },
            dashmap::mapref::entry::Entry::Vacant(vac) => {
                vac.insert(OnDemandCertState::Pending);
                Ok(())
            }
        };
        if let Err(outcome) = claim {
            debug!(sni = %hostname, ?outcome, "on-demand cert gate: dedup/backoff under lock");
            return outcome;
        }

        match self.job_tx.try_send(OnDemandCertJob {
            hostname: hostname.clone(),
        }) {
            Ok(()) => {
                info!(sni = %hostname, "on-demand cert: issuance enqueued");
                EnqueueOutcome::Enqueued
            }
            Err(mpsc::error::TrySendError::Full(_)) => {
                // Channel saturated — undo the pending marker and shed load.
                self.state.remove(&hostname);
                warn!(sni = %hostname, "on-demand cert: job channel full, shedding");
                EnqueueOutcome::SkippedDisabled
            }
            Err(mpsc::error::TrySendError::Closed(_)) => {
                self.state.remove(&hostname);
                error!(sni = %hostname, "on-demand cert: job channel closed");
                EnqueueOutcome::SkippedDisabled
            }
        }
    }

    /// Check 1: `hostname` is a direct (single-label) subdomain of `zone`.
    /// `myapp.<zone>` passes; `<zone>` itself and `deep.sub.<zone>` do not.
    fn is_direct_subdomain_of_zone(&self, hostname: &str) -> bool {
        let zone = self.config.zone.trim().trim_end_matches('.');
        if zone.is_empty() {
            return false;
        }
        let zone = zone.to_ascii_lowercase();
        let suffix = format!(".{}", zone);
        let Some(label) = hostname.strip_suffix(&suffix) else {
            return false;
        };
        // Exactly one non-empty label before the zone (no nested subdomains).
        !label.is_empty() && !label.contains('.')
    }

    /// True iff `hostname` is the configured console host. Used to exempt the
    /// console (a fall-through, not a route) from the cert-eligible-route check
    /// while still requiring it to be in-zone (check 1 runs first). The compare
    /// is case-insensitive against the already-lowercased gate input.
    fn is_console_host(&self, hostname: &str) -> bool {
        self.config
            .console_host
            .as_deref()
            .map(|c| c.trim().trim_end_matches('.').to_ascii_lowercase())
            .filter(|c| !c.is_empty())
            .is_some_and(|c| c == hostname)
    }

    /// ADR §4 final layer: bound how many novel hostnames a single source IP
    /// can trigger per [`PER_IP_WINDOW_SECS`]. Returns `true` if allowed.
    fn allow_ip_novelty(&self, ip: IpAddr) -> bool {
        let now = self.current_epoch_secs();
        let entry = self
            .ip_novelty
            .entry(ip)
            .or_insert_with(|| IpNoveltyCounter {
                window_start_epoch: AtomicU64::new(now),
                count: AtomicU32::new(0),
            });
        let counter = entry.value();
        let window_start = counter.window_start_epoch.load(Ordering::Relaxed);
        if now.saturating_sub(window_start) >= PER_IP_WINDOW_SECS {
            // Window rolled over — reset.
            counter.window_start_epoch.store(now, Ordering::Relaxed);
            counter.count.store(1, Ordering::Relaxed);
            return true;
        }
        let prev = counter.count.fetch_add(1, Ordering::Relaxed);
        prev < MAX_NOVEL_HOSTNAMES_PER_IP_PER_MINUTE
    }

    /// ADR §4 Layer 3: global hourly issuance cap. Returns `true` if a slot is
    /// available in the current window (and consumes it).
    fn allow_hourly_issuance(&self) -> bool {
        let now = self.current_epoch_secs();
        let window_start = self.hourly_window_start.load(Ordering::Relaxed);
        if now.saturating_sub(window_start) >= HOURLY_WINDOW_SECS {
            self.hourly_window_start.store(now, Ordering::Relaxed);
            self.hourly_count.store(1, Ordering::Relaxed);
            return true;
        }
        let prev = self.hourly_count.fetch_add(1, Ordering::Relaxed);
        if prev >= self.config.hourly_cap {
            // Over cap — back the counter out so it doesn't overflow forever.
            self.hourly_count.fetch_sub(1, Ordering::Relaxed);
            return false;
        }
        true
    }

    /// Seed the in-process state cache from the DB at startup (ADR §3). Rows in
    /// `pending`/`issuing`/`failed` within the last 24h are mirrored so a fresh
    /// proxy doesn't re-enqueue everything during the cold-cache window.
    pub fn seed_state(&self, entries: Vec<(String, OnDemandCertState)>) {
        for (hostname, state) in entries {
            self.state.insert(hostname.to_ascii_lowercase(), state);
        }
    }

    /// Current cached state for a hostname (used by the HTTP-side 503 surface in
    /// Layer 4 and by tests).
    pub fn state_of(&self, hostname: &str) -> Option<OnDemandCertState> {
        self.state
            .get(&hostname.trim().to_ascii_lowercase())
            .map(|e| e.value().clone())
    }

    /// Background consumer (ADR §7). Pulls jobs, bounds concurrency with the
    /// semaphore, and runs each issuance in a panic-isolated task so a panic in
    /// `DomainService`/the ACME client is recorded as a failure and never
    /// reaches a Pingora worker thread (ADR §Risks).
    fn spawn_consumer(
        self: Arc<Self>,
        mut job_rx: mpsc::Receiver<OnDemandCertJob>,
        provisioner: Arc<dyn OnDemandCertProvisioner>,
    ) {
        let semaphore = Arc::new(Semaphore::new(self.config.max_concurrent.max(1) as usize));
        tokio::spawn(async move {
            while let Some(job) = job_rx.recv().await {
                let permit = match Arc::clone(&semaphore).acquire_owned().await {
                    Ok(p) => p,
                    Err(_) => {
                        error!("on-demand cert semaphore closed; stopping consumer");
                        break;
                    }
                };
                let manager = Arc::clone(&self);
                let provisioner = Arc::clone(&provisioner);
                tokio::spawn(async move {
                    // Held for the duration of the issuance; dropped on return.
                    let _permit = permit;
                    manager.run_issuance(job.hostname, provisioner).await;
                });
            }
            debug!("on-demand cert job channel drained; consumer exiting");
        });
    }

    /// Run one issuance, catching panics from the provisioner so they become a
    /// recorded failure instead of unwinding into a worker thread.
    async fn run_issuance(&self, hostname: String, provisioner: Arc<dyn OnDemandCertProvisioner>) {
        self.state
            .insert(hostname.clone(), OnDemandCertState::Issuing);

        let email = self.config.email.clone();
        let host_for_task = hostname.clone();
        let handle =
            tokio::spawn(async move { provisioner.provision(&host_for_task, &email).await });

        // Bound the whole attempt: a stuck LE/network call must not hold its
        // concurrency permit forever. On Elapsed we abort the task and fall
        // through to a recorded `timeout` failure.
        let timeout = std::time::Duration::from_secs(ISSUANCE_TIMEOUT_SECS);
        let outcome = match tokio::time::timeout(timeout, handle).await {
            Ok(joined) => joined,
            Err(_elapsed) => {
                let backoff = self.next_backoff_epoch(&hostname);
                self.state.insert(
                    hostname.clone(),
                    OnDemandCertState::Failed {
                        backoff_until_epoch: backoff,
                    },
                );
                error!(
                    sni = %hostname,
                    timeout_secs = ISSUANCE_TIMEOUT_SECS,
                    backoff_until = backoff,
                    "on-demand cert: issuance timed out; recorded as failed (permit released)"
                );
                return;
            }
        };

        match outcome {
            Ok(Ok(())) => {
                // Success: the cert is now `active` on the `domains` row and the
                // SNI loader will pick it up. Drop the cache entry so a future
                // renewal-time miss can re-enter the state machine cleanly.
                self.state.remove(&hostname);
                info!(sni = %hostname, "on-demand cert: issued");
            }
            Ok(Err(failure)) => {
                let backoff = failure
                    .backoff_until_epoch
                    .unwrap_or_else(|| self.next_backoff_epoch(&hostname));
                self.state.insert(
                    hostname.clone(),
                    OnDemandCertState::Failed {
                        backoff_until_epoch: backoff,
                    },
                );
                warn!(
                    sni = %hostname,
                    category = %failure.category,
                    error_chain = %failure.error_chain,
                    backoff_until = backoff,
                    "on-demand cert: issuance failed"
                );
            }
            Err(join_err) => {
                // The issuance task panicked (or was cancelled). Record a
                // failure with a self-computed backoff; the orchestration layer
                // already wrote any partial audit/DB state it reached.
                let backoff = self.next_backoff_epoch(&hostname);
                self.state.insert(
                    hostname.clone(),
                    OnDemandCertState::Failed {
                        backoff_until_epoch: backoff,
                    },
                );
                error!(
                    sni = %hostname,
                    error = %join_err,
                    backoff_until = backoff,
                    "on-demand cert: issuance task panicked; recorded as failed"
                );
            }
        }
    }

    /// Conservative in-process backoff (first rung of the ADR §4 ladder, 5m)
    /// used ONLY when the provisioner did not supply a deadline — i.e. the
    /// issuance task panicked or was cancelled before `temps-domains` could
    /// persist its authoritative exponential backoff (5m → 15m → 1h → 4h → 24h)
    /// on the `domains` row. On the next request after this window the gate
    /// re-enqueues, the provisioner runs again, and the DB ladder (which IS
    /// authoritative) climbs from where it left off. Keeping this a fixed short
    /// delay avoids duplicating the ladder's state here without it.
    fn next_backoff_epoch(&self, _hostname: &str) -> u64 {
        const FIRST_RUNG_SECS: u64 = 300;
        self.current_epoch_secs() + FIRST_RUNG_SECS
    }
}

fn now_epoch_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use sea_orm::{DatabaseBackend, MockDatabase};
    use std::net::Ipv4Addr;
    use std::sync::atomic::AtomicUsize;
    use std::sync::Mutex;
    use temps_routes::{BackendEntry, BackendType, CachedPeerTable, RouteInfo};

    /// A provisioner that records calls and returns a configurable result.
    struct FakeProvisioner {
        calls: Arc<Mutex<Vec<String>>>,
        succeed: bool,
    }

    #[async_trait]
    impl OnDemandCertProvisioner for FakeProvisioner {
        async fn provision(
            &self,
            hostname: &str,
            _email: &str,
        ) -> Result<(), CertProvisionFailure> {
            self.calls.lock().unwrap().push(hostname.to_string());
            if self.succeed {
                Ok(())
            } else {
                Err(CertProvisionFailure {
                    error_chain: "boom".to_string(),
                    category: "internal".to_string(),
                    backoff_until_epoch: None,
                })
            }
        }
    }

    fn empty_route_table() -> Arc<CachedPeerTable> {
        let db = MockDatabase::new(DatabaseBackend::Postgres).into_connection();
        Arc::new(CachedPeerTable::new(Arc::new(db)))
    }

    fn route(cert_eligible: bool) -> RouteInfo {
        RouteInfo {
            backend: BackendType::Upstream {
                backends: vec![BackendEntry {
                    address: "127.0.0.1:3000".to_string(),
                    container_id: None,
                    container_name: None,
                }],
                round_robin_counter: Arc::new(AtomicUsize::new(0)),
            },
            redirect_to: None,
            status_code: None,
            project: None,
            environment: None,
            deployment: None,
            cert_eligible,
        }
    }

    fn manager_with(
        zone: &str,
        route_table: Arc<CachedPeerTable>,
        provisioner: Arc<dyn OnDemandCertProvisioner>,
    ) -> Arc<OnDemandCertManager> {
        manager_with_console(zone, None, route_table, provisioner)
    }

    fn manager_with_console(
        zone: &str,
        console_host: Option<&str>,
        route_table: Arc<CachedPeerTable>,
        provisioner: Arc<dyn OnDemandCertProvisioner>,
    ) -> Arc<OnDemandCertManager> {
        OnDemandCertManager::new(
            OnDemandCertConfig {
                zone: zone.to_string(),
                email: "ops@example.com".to_string(),
                max_concurrent: 3,
                hourly_cap: 10,
                console_host: console_host.map(|c| c.to_string()),
            },
            route_table,
            provisioner,
        )
    }

    fn fake(succeed: bool) -> (Arc<dyn OnDemandCertProvisioner>, Arc<Mutex<Vec<String>>>) {
        let calls = Arc::new(Mutex::new(Vec::new()));
        let p = Arc::new(FakeProvisioner {
            calls: Arc::clone(&calls),
            succeed,
        });
        (p, calls)
    }

    fn ip() -> Option<IpAddr> {
        Some(IpAddr::V4(Ipv4Addr::new(203, 0, 113, 7)))
    }

    #[tokio::test]
    async fn test_gate_rejects_out_of_zone() {
        let (p, _) = fake(true);
        let rt = empty_route_table();
        rt.insert_route_for_test("myapp.other.example.com", route(true));
        let mgr = manager_with("1.2.3.4.sslip.io", rt, p);

        let outcome = mgr.try_enqueue("myapp.other.example.com", ip());
        assert_eq!(outcome, EnqueueOutcome::SkippedGate);
    }

    #[tokio::test]
    async fn test_gate_rejects_nested_subdomain() {
        let (p, _) = fake(true);
        let rt = empty_route_table();
        rt.insert_route_for_test("deep.sub.1.2.3.4.sslip.io", route(true));
        let mgr = manager_with("1.2.3.4.sslip.io", rt, p);

        // Direct-subdomain rule: nested labels before the zone must be rejected.
        let outcome = mgr.try_enqueue("deep.sub.1.2.3.4.sslip.io", ip());
        assert_eq!(outcome, EnqueueOutcome::SkippedGate);
    }

    #[tokio::test]
    async fn test_gate_rejects_ephemeral_non_cert_eligible() {
        let (p, _) = fake(true);
        let rt = empty_route_table();
        // In-zone host with a route, but the route is an ephemeral
        // per-deployment hostname (cert_eligible = false).
        rt.insert_route_for_test("myapp-prod-42.1.2.3.4.sslip.io", route(false));
        let mgr = manager_with("1.2.3.4.sslip.io", rt, p);

        let outcome = mgr.try_enqueue("myapp-prod-42.1.2.3.4.sslip.io", ip());
        assert_eq!(outcome, EnqueueOutcome::SkippedNoRoute);
    }

    #[tokio::test]
    async fn test_gate_rejects_in_zone_with_no_route() {
        let (p, _) = fake(true);
        let rt = empty_route_table();
        let mgr = manager_with("1.2.3.4.sslip.io", rt, p);

        let outcome = mgr.try_enqueue("ghost.1.2.3.4.sslip.io", ip());
        assert_eq!(outcome, EnqueueOutcome::SkippedNoRoute);
    }

    #[tokio::test]
    async fn test_gate_accepts_console_host_without_route() {
        let (p, calls) = fake(true);
        // No route at all for the console host — it is served as a fall-through,
        // not a CachedPeerTable route. The exemption must let it through anyway.
        let rt = empty_route_table();
        let mgr = manager_with_console(
            "1.2.3.4.sslip.io",
            Some("console.1.2.3.4.sslip.io"),
            rt,
            Arc::clone(&p),
        );

        let outcome = mgr.try_enqueue("console.1.2.3.4.sslip.io", ip());
        assert_eq!(outcome, EnqueueOutcome::Enqueued);

        // Give the background consumer a moment to drain the job.
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        assert_eq!(
            calls.lock().unwrap().as_slice(),
            &["console.1.2.3.4.sslip.io".to_string()]
        );
    }

    #[tokio::test]
    async fn test_console_exemption_is_exact_host_only() {
        let (p, _) = fake(true);
        let rt = empty_route_table();
        // The console host is `console.<zone>`; a *different* routeless in-zone
        // host must NOT inherit the exemption (it would defeat the route check).
        let mgr = manager_with_console("1.2.3.4.sslip.io", Some("console.1.2.3.4.sslip.io"), rt, p);

        let outcome = mgr.try_enqueue("ghost.1.2.3.4.sslip.io", ip());
        assert_eq!(outcome, EnqueueOutcome::SkippedNoRoute);
    }

    #[tokio::test]
    async fn test_console_exemption_still_requires_in_zone() {
        let (p, _) = fake(true);
        let rt = empty_route_table();
        // A console host configured outside the zone still fails check 1 (which
        // runs before the exemption), so it can never trigger issuance. This
        // guards against a misconfigured console_host bypassing the zone gate.
        let mgr =
            manager_with_console("1.2.3.4.sslip.io", Some("console.other.example.com"), rt, p);

        let outcome = mgr.try_enqueue("console.other.example.com", ip());
        assert_eq!(outcome, EnqueueOutcome::SkippedGate);
    }

    #[tokio::test]
    async fn test_gate_accepts_stable_in_zone_routed_host() {
        let (p, calls) = fake(true);
        let rt = empty_route_table();
        rt.insert_route_for_test("myapp.1.2.3.4.sslip.io", route(true));
        let mgr = manager_with("1.2.3.4.sslip.io", rt, Arc::clone(&p));

        let outcome = mgr.try_enqueue("myapp.1.2.3.4.sslip.io", ip());
        assert_eq!(outcome, EnqueueOutcome::Enqueued);

        // Let the background consumer run the issuance.
        for _ in 0..50 {
            if !calls.lock().unwrap().is_empty() {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
        assert_eq!(
            calls.lock().unwrap().as_slice(),
            &["myapp.1.2.3.4.sslip.io"]
        );
    }

    #[tokio::test]
    async fn test_gate_dedups_in_flight() {
        let (p, _) = fake(true);
        let rt = empty_route_table();
        rt.insert_route_for_test("myapp.1.2.3.4.sslip.io", route(true));
        let mgr = manager_with("1.2.3.4.sslip.io", rt, p);

        // First enqueue marks pending.
        assert_eq!(
            mgr.try_enqueue("myapp.1.2.3.4.sslip.io", ip()),
            EnqueueOutcome::Enqueued
        );
        // Immediate second enqueue must dedup against the pending/issuing marker.
        let second = mgr.try_enqueue("myapp.1.2.3.4.sslip.io", ip());
        assert_eq!(second, EnqueueOutcome::SkippedDuplicate);
    }

    #[tokio::test]
    async fn test_gate_rejects_during_backoff() {
        let (p, _) = fake(true);
        let rt = empty_route_table();
        rt.insert_route_for_test("myapp.1.2.3.4.sslip.io", route(true));
        let mgr = manager_with("1.2.3.4.sslip.io", rt, p);

        // Seed an active backoff window in the future.
        let future = now_epoch_secs() + 3600;
        mgr.seed_state(vec![(
            "myapp.1.2.3.4.sslip.io".to_string(),
            OnDemandCertState::Failed {
                backoff_until_epoch: future,
            },
        )]);

        let outcome = mgr.try_enqueue("myapp.1.2.3.4.sslip.io", ip());
        assert_eq!(outcome, EnqueueOutcome::SkippedBackoff);
    }

    #[tokio::test]
    async fn test_gate_reenqueues_after_backoff_elapsed() {
        let (p, _) = fake(true);
        let rt = empty_route_table();
        rt.insert_route_for_test("myapp.1.2.3.4.sslip.io", route(true));
        let mgr = manager_with("1.2.3.4.sslip.io", rt, p);

        // Backoff already elapsed (in the past) — gate must allow a retry.
        let past = now_epoch_secs().saturating_sub(10);
        mgr.seed_state(vec![(
            "myapp.1.2.3.4.sslip.io".to_string(),
            OnDemandCertState::Failed {
                backoff_until_epoch: past,
            },
        )]);

        let outcome = mgr.try_enqueue("myapp.1.2.3.4.sslip.io", ip());
        assert_eq!(outcome, EnqueueOutcome::Enqueued);
    }

    #[tokio::test]
    async fn test_no_zone_rejects_all() {
        let (p, _) = fake(true);
        let rt = empty_route_table();
        rt.insert_route_for_test("myapp.1.2.3.4.sslip.io", route(true));
        let mgr = manager_with("", rt, p);

        assert_eq!(
            mgr.try_enqueue("myapp.1.2.3.4.sslip.io", ip()),
            EnqueueOutcome::SkippedGate
        );
    }

    #[tokio::test]
    async fn test_per_ip_novelty_cap() {
        let (p, _) = fake(true);
        let rt = empty_route_table();
        // Enough distinct cert-eligible hosts to exceed the per-IP cap.
        for i in 0..(MAX_NOVEL_HOSTNAMES_PER_IP_PER_MINUTE + 3) {
            rt.insert_route_for_test(&format!("app{i}.1.2.3.4.sslip.io"), route(true));
        }
        let mgr = manager_with("1.2.3.4.sslip.io", rt, p);

        let mut rate_limited = 0;
        for i in 0..(MAX_NOVEL_HOSTNAMES_PER_IP_PER_MINUTE + 3) {
            let outcome = mgr.try_enqueue(&format!("app{i}.1.2.3.4.sslip.io"), ip());
            if outcome == EnqueueOutcome::SkippedRateLimit {
                rate_limited += 1;
            }
        }
        assert!(rate_limited >= 1, "per-IP novelty cap should reject excess");
    }

    #[tokio::test]
    async fn test_failure_records_backoff_in_cache() {
        let (p, calls) = fake(false); // provisioner fails
        let rt = empty_route_table();
        rt.insert_route_for_test("myapp.1.2.3.4.sslip.io", route(true));
        let mgr = manager_with("1.2.3.4.sslip.io", rt, Arc::clone(&p));

        assert_eq!(
            mgr.try_enqueue("myapp.1.2.3.4.sslip.io", ip()),
            EnqueueOutcome::Enqueued
        );

        // Wait for the consumer to fail and record backoff.
        let mut got_backoff = false;
        for _ in 0..100 {
            if let Some(OnDemandCertState::Failed { .. }) = mgr.state_of("myapp.1.2.3.4.sslip.io") {
                got_backoff = true;
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
        assert!(got_backoff, "failed issuance must record a backoff state");
        assert_eq!(calls.lock().unwrap().len(), 1);

        // A subsequent enqueue is now in backoff.
        assert_eq!(
            mgr.try_enqueue("myapp.1.2.3.4.sslip.io", ip()),
            EnqueueOutcome::SkippedBackoff
        );
    }

    #[tokio::test]
    async fn test_is_direct_subdomain_of_zone() {
        let (p, _) = fake(true);
        let rt = empty_route_table();
        let mgr = manager_with("1.2.3.4.sslip.io", rt, p);

        assert!(mgr.is_direct_subdomain_of_zone("myapp.1.2.3.4.sslip.io"));
        assert!(mgr.is_direct_subdomain_of_zone("MyApp.1.2.3.4.sslip.io")); // case-insensitive
        assert!(!mgr.is_direct_subdomain_of_zone("1.2.3.4.sslip.io")); // zone itself
        assert!(!mgr.is_direct_subdomain_of_zone("deep.sub.1.2.3.4.sslip.io")); // nested
        assert!(!mgr.is_direct_subdomain_of_zone("myapp.other.com")); // out of zone
        assert!(!mgr.is_direct_subdomain_of_zone(".1.2.3.4.sslip.io")); // empty label
    }
}
