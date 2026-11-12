// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! [`ResolverHandle`] — the public face of the per-node DNS resolver.
//!
//! `ResolverHandle::start(config)` does the whole job:
//!
//! 1. Build the [`ZoneStore`], hydrate it from `<snapshot_dir>/zone.json`
//!    so the resolver answers from disk *before* the first sync round.
//! 2. Bind UDP + TCP listeners on each `listen_addr`.
//! 3. Spawn the [`SyncClient`] long-poll loop — only once every listener is
//!    bound, so a bind failure returns `Err` without leaving an orphaned,
//!    never-notified sync task running.
//! 4. Run a Hickory `ServerFuture` driving [`ZoneAuthority`].
//!
//! `ResolverHandle::shutdown()` stops all child tasks and awaits them.

use std::net::{IpAddr, SocketAddr};
use std::sync::{Arc, RwLock};
use std::time::{Duration, SystemTime};

use hickory_proto::rr::{Name, RecordType};
use hickory_server::net::runtime::Time;
use hickory_server::server::{Request, RequestHandler, ResponseHandler, ResponseInfo, Server};
use tokio::net::{TcpListener, UdpSocket};
use tokio::sync::Notify;
use tokio::task::JoinHandle;
use tracing::{info, warn};

use crate::authority::ZoneAuthority;
use crate::config::ResolverConfig;
use crate::error::ResolverError;
use crate::observe::{DnsObserver, DnsResolverState};
use crate::resolve::ResolveAnswer;
use crate::sync_client::{SyncClient, SyncStatus};
use crate::upstream::UpstreamResolver;
use crate::zone_store::ZoneStore;

/// TCP idle timeout. Hickory closes idle connections after this. 5 s is the
/// hickory examples default; we don't expect high TCP query volume since
/// most DNS traffic is UDP.
const TCP_IDLE_TIMEOUT: Duration = Duration::from_secs(5);

/// Snapshot of resolver health, for the agent heartbeat and any local
/// troubleshooting surface (see `ResolverHandle::status`).
#[derive(Debug, Clone)]
pub struct ResolverStatus {
    /// `false` means one of the sync or DNS server tasks has exited —
    /// crashed, panicked, or otherwise stopped — while the handle is still
    /// held. A healthy resolver never observes this; it's the signal the
    /// agent's sync loop uses to decide to respawn.
    pub tasks_alive: bool,
    pub sync: SyncStatus,
    /// Number of records currently served (from the last successful sync,
    /// or from the on-disk snapshot if the resolver hasn't synced yet).
    pub record_count: usize,
    pub zone_generation: i64,
}

/// The server owns its handler; the handle keeps the same authority to
/// answer test resolutions in-process ([`ResolverHandle::resolve_as`]).
struct SharedAuthority(Arc<ZoneAuthority>);

#[async_trait::async_trait]
impl RequestHandler for SharedAuthority {
    async fn handle_request<R: ResponseHandler, T: Time>(
        &self,
        request: &Request,
        response_handle: R,
    ) -> ResponseInfo {
        self.0
            .handle_request::<R, T>(request, response_handle)
            .await
    }
}

/// A cheap, cloneable view of a running resolver for whoever reports on
/// it (ADR-095): its state and in-process test resolution, without owning
/// its lifecycle. Reads go to the live zone and sync status.
#[derive(Clone)]
pub struct ResolverView {
    authority: Arc<ZoneAuthority>,
    zone: Arc<ZoneStore>,
    sync_status: Arc<RwLock<SyncStatus>>,
    listen_addrs: Vec<SocketAddr>,
    /// The zone is fed locally (the control plane's resolver), not synced.
    local_feed: bool,
}

impl ResolverView {
    /// The resolver's state, in the shape a DNS report carries.
    /// `tasks_alive` comes from the handle's owner.
    pub fn state(&self, tasks_alive: bool) -> DnsResolverState {
        let snapshot = self.zone.snapshot();
        let sync = self
            .sync_status
            .read()
            .map(|s| s.clone())
            .unwrap_or_default();
        let last_sync_age_secs = if self.local_feed {
            None
        } else {
            sync.last_success_at.map(|at| {
                SystemTime::now()
                    .duration_since(at)
                    .map_or(0, |age| age.as_secs())
            })
        };
        DnsResolverState {
            running: true,
            tasks_alive,
            listen_addrs: self.listen_addrs.iter().map(ToString::to_string).collect(),
            zone_generation: snapshot.generation(),
            record_count: u64::try_from(snapshot.records().len()).unwrap_or(u64::MAX),
            last_sync_age_secs,
            consecutive_sync_failures: sync.consecutive_failures,
            last_error: sync.last_error,
        }
    }

    /// The first address the resolver listens on.
    pub fn listen_addr(&self) -> Option<SocketAddr> {
        self.listen_addrs.first().copied()
    }

    /// Answer a test resolution as this resolver would answer `source`
    /// (ADR-095 §5). Not counted or logged.
    pub async fn resolve_as(
        &self,
        source: IpAddr,
        name: Name,
        qtype: RecordType,
    ) -> Result<ResolveAnswer, ResolverError> {
        self.authority.resolve_as(source, name, qtype).await
    }
}

pub struct ResolverHandle {
    pub zone: Arc<ZoneStore>,
    view: ResolverView,
    shutdown: Arc<Notify>,
    sync_task: JoinHandle<()>,
    server_task: JoinHandle<()>,
    sync_status: Arc<RwLock<SyncStatus>>,
}

impl Drop for ResolverHandle {
    fn drop(&mut self) {
        // Dropping a JoinHandle detaches its task. A cancelled startup future
        // must instead release bound UDP/TCP sockets before a retry binds them.
        self.sync_task.abort();
        self.server_task.abort();
    }
}

impl ResolverHandle {
    /// Boot the resolver. Returns once UDP + TCP sockets are bound and the
    /// sync loop is running. The first sync round may not have completed
    /// yet — but the disk snapshot (if any) is already serving.
    pub async fn start(config: ResolverConfig) -> Result<Self, ResolverError> {
        Self::start_with_policy(config, None).await
    }

    /// [`ResolverHandle::start`], answering restricted workloads from
    /// `policy` (see [`ZoneAuthority::with_policy`]).
    pub async fn start_with_policy(
        config: ResolverConfig,
        policy: Option<crate::DnsPolicySlot>,
    ) -> Result<Self, ResolverError> {
        Self::start_observed(config, policy, None).await
    }

    /// [`ResolverHandle::start_with_policy`], recording every answered
    /// query into `observer` (ADR-095). Pass the same observer to every
    /// restart, so the counters and the query log survive it.
    pub async fn start_observed(
        config: ResolverConfig,
        policy: Option<crate::DnsPolicySlot>,
        observer: Option<Arc<DnsObserver>>,
    ) -> Result<Self, ResolverError> {
        let zone = Arc::new(ZoneStore::new(config.snapshot_path()));
        zone.load_from_disk();

        let shutdown = Arc::new(Notify::new());
        let sync_status = Arc::new(RwLock::new(SyncStatus::default()));

        // ----- Upstream forwarder -----
        // Built once per resolver. `None` means the operator has
        // configured an empty upstream pool — strict authoritative
        // mode, where outside-zone queries fall through to NXDOMAIN.
        let upstream = UpstreamResolver::new(&config.upstream_resolvers).map(Arc::new);
        if let Some(_u) = &upstream {
            info!(
                upstreams = ?config.upstream_resolvers,
                "DNS recursive forwarder enabled"
            );
        } else {
            info!("DNS recursive forwarder disabled (empty upstream list)");
        }

        // ----- DNS server -----
        let mut authority = ZoneAuthority::new(zone.clone());
        if let Some(upstream) = upstream {
            authority = authority.with_upstream(upstream);
        }
        if let Some(policy) = policy {
            authority = authority.with_policy(policy);
        }
        if let Some(observer) = observer {
            authority = authority.with_observer(observer);
        }
        let authority = Arc::new(authority);
        let mut server = Server::new(SharedAuthority(authority.clone()));

        for addr in &config.listen_addrs {
            let udp =
                UdpSocket::bind(addr)
                    .await
                    .map_err(|source| ResolverError::UdpBindFailed {
                        addr: *addr,
                        source,
                    })?;
            server.register_socket(udp);

            let tcp =
                TcpListener::bind(addr)
                    .await
                    .map_err(|source| ResolverError::TcpBindFailed {
                        addr: *addr,
                        source,
                    })?;
            // 65535 = the maximum DNS-over-TCP message size (2-byte length
            // prefix), so a single response never has to be split.
            server.register_listener(tcp, TCP_IDLE_TIMEOUT, u16::MAX as usize);
        }

        // Do not announce individual listeners until every requested UDP/TCP
        // bind has succeeded. Otherwise an error on a later address leaves a
        // misleading "listening" line immediately before startup aborts.
        for addr in &config.listen_addrs {
            info!(%addr, "DNS resolver listening (UDP + TCP)");
        }

        // ----- Sync loop -----
        // Spawned only after every listener above is bound. Before this PR
        // callers only invoked `start()` once per agent lifetime, so an
        // orphaned sync task on a failed bind leaked at most once; the
        // per-tick reconciliation added in ADR-024's self-healing follow-up
        // retries `start()` on every sync tick, which would otherwise spawn
        // one more never-notified poller per retry (the caller's `shutdown`
        // clone is dropped on early return, but the task's own clone keeps
        // it looping forever, hammering the control plane's DNS change feed).
        //
        // Worker nodes long-poll the control plane over HTTP. In control-plane
        // mode (`disable_sync`, ADR-024) the caller owns the `ZoneStore` and
        // feeds it directly from the local `service_endpoints` DB, so we skip
        // the HTTP sync entirely and just park a task on `shutdown` to keep the
        // handle's shape (and `shutdown()` semantics) unchanged. Status stays
        // at its `Default` (never-synced) value in this mode — callers should
        // read `disable_sync`/local-mode context separately rather than treat
        // that as a health signal.
        let sync_task = if config.disable_sync {
            let sd = shutdown.clone();
            tokio::spawn(async move {
                sd.notified().await;
            })
        } else {
            let sync_client = SyncClient::new(
                config.clone(),
                zone.clone(),
                shutdown.clone(),
                sync_status.clone(),
            )?;
            tokio::spawn(async move { sync_client.run().await })
        };

        let shutdown_for_server = shutdown.clone();
        let server_task = tokio::spawn(async move {
            tokio::select! {
                res = server.block_until_done() => {
                    if let Err(e) = res {
                        warn!(error = %e, "DNS server exited with error");
                    }
                }
                _ = shutdown_for_server.notified() => {
                    info!("DNS server shutting down");
                    // Drop `server` to close listeners. ServerFuture has no
                    // explicit shutdown() in 0.25 — drop is the supported path.
                }
            }
        });

        Ok(Self {
            view: ResolverView {
                authority,
                zone: zone.clone(),
                sync_status: sync_status.clone(),
                listen_addrs: config.listen_addrs.clone(),
                local_feed: config.disable_sync,
            },
            zone,
            shutdown,
            sync_task,
            server_task,
            sync_status,
        })
    }

    /// Point-in-time health snapshot. Non-blocking and cheap enough to call
    /// on every agent heartbeat: `JoinHandle::is_finished` doesn't consume
    /// the handle, and the sync status is a clone out of a short-held lock.
    pub fn status(&self) -> ResolverStatus {
        let snapshot = self.zone.snapshot();
        ResolverStatus {
            tasks_alive: self.tasks_alive(),
            sync: self
                .sync_status
                .read()
                .map(|s| s.clone())
                .unwrap_or_default(),
            record_count: snapshot.records().len(),
            zone_generation: snapshot.generation(),
        }
    }

    /// [`ResolverHandle::status`], in the shape a DNS report carries.
    pub fn state(&self) -> DnsResolverState {
        self.view.state(self.tasks_alive())
    }

    fn tasks_alive(&self) -> bool {
        !self.sync_task.is_finished() && !self.server_task.is_finished()
    }

    /// A view for reporting on this resolver (ADR-095).
    pub fn view(&self) -> ResolverView {
        self.view.clone()
    }

    /// Stop both background tasks and wait until their sockets are closed.
    /// Aborting also covers tasks that have not started polling `Notify` yet;
    /// `notify_waiters` alone could lose that notification and wait forever.
    pub async fn shutdown(mut self) {
        self.shutdown.notify_waiters();
        self.sync_task.abort();
        self.server_task.abort();
        // Don't propagate JoinError — cancellation is expected here.
        let _ = (&mut self.sync_task).await;
        let _ = (&mut self.server_task).await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn dropping_handle_releases_bound_listener() {
        let reservation = std::net::TcpListener::bind("127.0.0.1:0").expect("reserve port");
        let address = reservation.local_addr().expect("test address");
        drop(reservation);
        let mut config = ResolverConfig::new_local_feed(
            0,
            address.ip(),
            std::env::temp_dir().join(format!("temps-dns-handle-{}", std::process::id())),
        );
        config.listen_addrs = vec![address];
        config.upstream_resolvers.clear();
        let handle = ResolverHandle::start(config.clone())
            .await
            .expect("first bind");
        drop(handle);

        tokio::time::timeout(Duration::from_secs(2), async {
            loop {
                if let Ok(restarted) = ResolverHandle::start(config.clone()).await {
                    restarted.shutdown().await;
                    break;
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .expect("dropped handle must release listener");
    }
}
