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
//! `ResolverHandle::shutdown()` notifies all child tasks and awaits them.

use std::sync::{Arc, RwLock};
use std::time::Duration;

use hickory_server::server::Server;
use tokio::net::{TcpListener, UdpSocket};
use tokio::sync::Notify;
use tokio::task::JoinHandle;
use tracing::{info, warn};

use crate::authority::ZoneAuthority;
use crate::config::ResolverConfig;
use crate::error::ResolverError;
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

pub struct ResolverHandle {
    pub zone: Arc<ZoneStore>,
    shutdown: Arc<Notify>,
    sync_task: JoinHandle<()>,
    server_task: JoinHandle<()>,
    sync_status: Arc<RwLock<SyncStatus>>,
}

impl ResolverHandle {
    /// Boot the resolver. Returns once UDP + TCP sockets are bound and the
    /// sync loop is running. The first sync round may not have completed
    /// yet — but the disk snapshot (if any) is already serving.
    pub async fn start(config: ResolverConfig) -> Result<Self, ResolverError> {
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
        let mut server = Server::new(authority);

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
            tasks_alive: !self.sync_task.is_finished() && !self.server_task.is_finished(),
            sync: self
                .sync_status
                .read()
                .map(|s| s.clone())
                .unwrap_or_default(),
            record_count: snapshot.records().len(),
            zone_generation: snapshot.generation(),
        }
    }

    /// Notify both background tasks and wait for them to exit. Idempotent —
    /// calling twice is harmless (the second `notify_waiters` finds no
    /// waiters).
    pub async fn shutdown(self) {
        self.shutdown.notify_waiters();
        // Don't propagate JoinError — we're shutting down anyway.
        let _ = self.sync_task.await;
        let _ = self.server_task.await;
    }
}
