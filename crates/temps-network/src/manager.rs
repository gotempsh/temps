//! [`NetworkManager`] — orchestrates the full per-node networking lifecycle.
//!
//! On Linux, it drives `crate::linux` to manipulate the kernel data plane.
//! On non-Linux it returns [`NetworkError::UnsupportedPlatform`] so the same
//! crate can be linked into the workspace and exercised for unit tests.

use crate::config::{NetworkConfig, NodeAlloc, Peer};
use crate::error::NetworkError;
use std::sync::Arc;
use tokio::sync::Mutex;
#[cfg(target_os = "linux")]
use tracing::info;
use tracing::instrument;

/// High-level coordinator for the worker-side network plumbing.
///
/// Construction is cheap and does no I/O; call [`Self::bootstrap`] once on
/// startup, then [`Self::reconcile_peers`] whenever the control plane pushes
/// a new peer list.
///
/// `NetworkManager` is `Send + Sync + Clone` (cheap clone, behaves like
/// `Arc`) so it can be shared across tasks.
#[derive(Clone)]
pub struct NetworkManager {
    inner: Arc<Inner>,
}

impl std::fmt::Debug for NetworkManager {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("NetworkManager")
            .field("bridge_name", &self.inner.config.bridge_name)
            .field(
                "docker_network_name",
                &self.inner.config.docker_network_name,
            )
            .finish_non_exhaustive()
    }
}

struct Inner {
    config: NetworkConfig,
    state: Mutex<State>,
}

#[derive(Default)]
struct State {
    /// `Some` after a successful bootstrap.
    #[cfg_attr(not(target_os = "linux"), allow(dead_code))]
    alloc: Option<NodeAlloc>,
    /// Last peer list we successfully reconciled to.
    #[cfg_attr(not(target_os = "linux"), allow(dead_code))]
    peers: Vec<Peer>,
}

impl NetworkManager {
    /// Construct without doing any I/O. The config is validated immediately.
    pub fn new(config: NetworkConfig) -> crate::Result<Self> {
        config.validate()?;
        Ok(Self {
            inner: Arc::new(Inner {
                config,
                state: Mutex::new(State::default()),
            }),
        })
    }

    /// View the current configuration. Useful for diagnostics endpoints.
    pub fn config(&self) -> &NetworkConfig {
        &self.inner.config
    }

    /// Bring the node onto the overlay.
    ///
    /// Idempotent: calling twice with the same args is safe; calling with
    /// changed args returns [`NetworkError::InterfaceConflict`] rather than
    /// silently mutating state — operators should explicitly tear down
    /// first.
    #[instrument(level = "info", skip_all, fields(node = %alloc.node_id, cidr = %alloc.compute_cidr))]
    pub async fn bootstrap(&self, alloc: NodeAlloc, peers: Vec<Peer>) -> crate::Result<()> {
        self.inner.config.validate_with(&alloc, &peers)?;

        #[cfg(not(target_os = "linux"))]
        {
            let _ = (&alloc, &peers);
            return Err(NetworkError::UnsupportedPlatform {
                target: std::env::consts::OS,
            });
        }

        #[cfg(target_os = "linux")]
        {
            crate::linux::bootstrap(&self.inner.config, &alloc, &peers).await?;
            let mut state = self.inner.state.lock().await;
            state.alloc = Some(alloc);
            state.peers = peers;
            info!("network manager bootstrapped");
            Ok(())
        }
    }

    /// Apply a new peer list, computing the minimum diff against the last
    /// successfully reconciled state. Returns `Ok(true)` when at least one
    /// kernel mutation happened, `Ok(false)` for a no-op.
    #[instrument(level = "info", skip_all, fields(peers = peers.len()))]
    pub async fn reconcile_peers(&self, peers: Vec<Peer>) -> crate::Result<bool> {
        let alloc = {
            let state = self.inner.state.lock().await;
            state.alloc.clone().ok_or(NetworkError::InvalidConfig {
                reason: "reconcile_peers called before bootstrap".into(),
            })?
        };
        self.inner.config.validate_with(&alloc, &peers)?;

        #[cfg(not(target_os = "linux"))]
        {
            let _ = (&alloc, &peers);
            return Err(NetworkError::UnsupportedPlatform {
                target: std::env::consts::OS,
            });
        }

        #[cfg(target_os = "linux")]
        {
            let current = {
                let state = self.inner.state.lock().await;
                state.peers.clone()
            };
            let changed =
                crate::linux::reconcile_peers(&self.inner.config, &alloc, &current, &peers).await?;
            let mut state = self.inner.state.lock().await;
            state.peers = peers;
            Ok(changed)
        }
    }

    /// Tear down everything this manager set up. Idempotent.
    #[instrument(level = "info", skip_all)]
    pub async fn teardown(&self) -> crate::Result<()> {
        #[cfg(not(target_os = "linux"))]
        {
            return Err(NetworkError::UnsupportedPlatform {
                target: std::env::consts::OS,
            });
        }

        #[cfg(target_os = "linux")]
        {
            crate::linux::teardown(&self.inner.config).await?;
            let mut state = self.inner.state.lock().await;
            state.alloc = None;
            state.peers.clear();
            Ok(())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn new_validates_config() {
        let mut cfg = NetworkConfig::default();
        cfg.bridge_name.clear();
        let err = NetworkManager::new(cfg).unwrap_err();
        assert!(matches!(err, NetworkError::InvalidConfig { .. }));
    }

    #[test]
    fn new_accepts_default_config() {
        NetworkManager::new(NetworkConfig::default()).unwrap();
    }

    #[cfg(not(target_os = "linux"))]
    #[tokio::test]
    async fn bootstrap_returns_unsupported_on_non_linux() {
        use crate::config::Peer;
        use ipnet::Ipv4Net;
        use std::net::{IpAddr, Ipv4Addr};
        use std::str::FromStr;
        use uuid::Uuid;

        let m = NetworkManager::new(NetworkConfig::default()).unwrap();
        let alloc = NodeAlloc {
            node_id: Uuid::nil(),
            compute_cidr: Ipv4Net::from_str("172.20.5.0/24").unwrap(),
            bridge_address: IpAddr::V4(Ipv4Addr::new(172, 20, 5, 1)),
            underlay_address: IpAddr::V4(Ipv4Addr::new(10, 0, 0, 1)),
        };
        let peers: Vec<Peer> = vec![];
        let err = m.bootstrap(alloc, peers).await.unwrap_err();
        assert!(matches!(err, NetworkError::UnsupportedPlatform { .. }));
    }
}
