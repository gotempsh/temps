//! [`ResolverHandle`] — the public face of the per-node DNS resolver.
//!
//! `ResolverHandle::start(config)` does the whole job:
//!
//! 1. Build the [`ZoneStore`], hydrate it from `<snapshot_dir>/zone.json`
//!    so the resolver answers from disk *before* the first sync round.
//! 2. Spawn the [`SyncClient`] long-poll loop.
//! 3. Bind UDP + TCP listeners on each `listen_addr`.
//! 4. Run a Hickory `ServerFuture` driving [`ZoneAuthority`].
//!
//! `ResolverHandle::shutdown()` notifies all child tasks and awaits them.

use std::sync::Arc;
use std::time::Duration;

use hickory_server::ServerFuture;
use tokio::net::{TcpListener, UdpSocket};
use tokio::sync::Notify;
use tokio::task::JoinHandle;
use tracing::{info, warn};

use crate::authority::ZoneAuthority;
use crate::config::ResolverConfig;
use crate::error::ResolverError;
use crate::sync_client::SyncClient;
use crate::upstream::UpstreamResolver;
use crate::zone_store::ZoneStore;

/// TCP idle timeout. Hickory closes idle connections after this. 5 s is the
/// hickory examples default; we don't expect high TCP query volume since
/// most DNS traffic is UDP.
const TCP_IDLE_TIMEOUT: Duration = Duration::from_secs(5);

pub struct ResolverHandle {
    pub zone: Arc<ZoneStore>,
    shutdown: Arc<Notify>,
    sync_task: JoinHandle<()>,
    server_task: JoinHandle<()>,
}

impl ResolverHandle {
    /// Boot the resolver. Returns once UDP + TCP sockets are bound and the
    /// sync loop is running. The first sync round may not have completed
    /// yet — but the disk snapshot (if any) is already serving.
    pub async fn start(config: ResolverConfig) -> Result<Self, ResolverError> {
        let zone = Arc::new(ZoneStore::new(config.snapshot_path()));
        zone.load_from_disk();

        let shutdown = Arc::new(Notify::new());

        // ----- Sync loop -----
        let sync_client = SyncClient::new(config.clone(), zone.clone(), shutdown.clone())?;
        let sync_task = tokio::spawn(async move { sync_client.run().await });

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
        let mut server = ServerFuture::new(authority);

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
            server.register_listener(tcp, TCP_IDLE_TIMEOUT);
            info!(%addr, "DNS resolver listening (UDP + TCP)");
        }

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
        })
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
