//! Resolver runtime configuration.
//!
//! Built by `temps-agent` at startup from: the node's [`NodeAlloc`]
//! (provides bridge gateway IP for the listen socket), an env var for the
//! disk snapshot directory, and the control-plane URL + node bearer token
//! (for the sync long-poll).

use std::net::{IpAddr, SocketAddr};
use std::path::PathBuf;
use std::time::Duration;

#[derive(Debug, Clone)]
pub struct ResolverConfig {
    /// Database id of this node. Used in the sync URL path and ACK body.
    pub node_id: i32,

    /// Bearer token to authenticate against the control plane's
    /// `/internal/nodes/{id}/dns/...` endpoints. Same token the node uses
    /// for `/network/peers`.
    pub node_token: String,

    /// Base URL of the control plane (e.g. `https://temps.local:3000`),
    /// **without** trailing slash.
    pub control_plane_url: String,

    /// Listen sockets. Typically two:
    ///   - bridge gateway IP (`172.20.X.1:53`) — what containers see.
    ///   - `127.0.0.53:53` — host-local debug.
    pub listen_addrs: Vec<SocketAddr>,

    /// Where to persist the zone snapshot. The resolver writes
    /// `<dir>/zone.json` on every applied generation and reads it on
    /// startup. Default in agent: `/var/lib/temps/dns`.
    pub snapshot_dir: PathBuf,

    /// How often to long-poll for changes. The control plane returns
    /// immediately on changes today (no server-side hold), so this is the
    /// effective propagation cadence for failover. 1 s default.
    pub poll_interval: Duration,

    /// Initial backoff after a sync error. Doubles up to `max_backoff`.
    pub initial_backoff: Duration,

    /// Cap on backoff between failed syncs.
    pub max_backoff: Duration,

    /// HTTP request timeout per sync call. Should be longer than
    /// `poll_interval` once we add server-side long-poll, but for now
    /// keep it short — the control plane returns immediately.
    pub http_timeout: Duration,

    /// Upstream public resolvers used to recursively answer queries that
    /// fall outside our internal `temps.local` zone. Containers point at
    /// us as their *first* nameserver, so without this they would get
    /// NXDOMAIN for everything (e.g. `nslookup google.com` from a worker
    /// container). Defaults to Cloudflare + Google. Empty disables the
    /// forwarder (we fall back to NXDOMAIN like a strict authoritative
    /// server).
    pub upstream_resolvers: Vec<SocketAddr>,

    /// Control-plane / single-node mode. When `true`, the resolver does NOT
    /// spawn the HTTP long-poll [`crate::sync_client::SyncClient`]; the caller
    /// owns the `ZoneStore` and feeds it directly (the control plane reads its
    /// own `service_endpoints` database). `node_token` / `control_plane_url`
    /// are unused in this mode. Defaults to `false` (worker behaviour).
    pub disable_sync: bool,
}

impl ResolverConfig {
    /// Convenience: construct with the agent's typical defaults.
    pub fn new(
        node_id: i32,
        node_token: String,
        control_plane_url: String,
        bridge_gateway: IpAddr,
        snapshot_dir: PathBuf,
    ) -> Self {
        Self {
            node_id,
            node_token,
            control_plane_url,
            listen_addrs: vec![
                SocketAddr::new(bridge_gateway, 53),
                SocketAddr::new("127.0.0.53".parse().expect("static ipv4"), 53),
            ],
            snapshot_dir,
            poll_interval: Duration::from_secs(1),
            initial_backoff: Duration::from_secs(1),
            max_backoff: Duration::from_secs(30),
            http_timeout: Duration::from_secs(10),
            // Cloudflare 1.1.1.1 / 1.0.0.1 + Google 8.8.8.8. Operators
            // who need a private upstream (corporate split-horizon) can
            // override via `with_upstream_resolvers`.
            upstream_resolvers: vec![
                SocketAddr::new("1.1.1.1".parse().expect("static ipv4"), 53),
                SocketAddr::new("1.0.0.1".parse().expect("static ipv4"), 53),
                SocketAddr::new("8.8.8.8".parse().expect("static ipv4"), 53),
            ],
            disable_sync: false,
        }
    }

    /// Control-plane / single-node constructor (ADR-024). Binds **only** on the
    /// app-bridge gateway — NOT `127.0.0.53`, which `systemd-resolved` owns on a
    /// real control-plane host — and disables the HTTP sync loop. The control
    /// plane is the authoritative `service_endpoints` source, so it feeds the
    /// `ZoneStore` directly instead of long-polling itself.
    pub fn new_local_feed(node_id: i32, bridge_gateway: IpAddr, snapshot_dir: PathBuf) -> Self {
        Self {
            node_id,
            node_token: String::new(),
            control_plane_url: String::new(),
            listen_addrs: vec![SocketAddr::new(bridge_gateway, 53)],
            snapshot_dir,
            poll_interval: Duration::from_secs(1),
            initial_backoff: Duration::from_secs(1),
            max_backoff: Duration::from_secs(30),
            http_timeout: Duration::from_secs(10),
            upstream_resolvers: vec![
                SocketAddr::new("1.1.1.1".parse().expect("static ipv4"), 53),
                SocketAddr::new("1.0.0.1".parse().expect("static ipv4"), 53),
                SocketAddr::new("8.8.8.8".parse().expect("static ipv4"), 53),
            ],
            disable_sync: true,
        }
    }

    /// Override the upstream resolver list. Pass an empty `Vec` to
    /// disable forwarding (strict authoritative-only mode).
    pub fn with_upstream_resolvers(mut self, upstreams: Vec<SocketAddr>) -> Self {
        self.upstream_resolvers = upstreams;
        self
    }

    pub fn snapshot_path(&self) -> PathBuf {
        self.snapshot_dir.join("zone.json")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn new_local_feed_binds_only_gateway_and_disables_sync() {
        let gw: IpAddr = "172.19.0.1".parse().unwrap();
        let cfg = ResolverConfig::new_local_feed(0, gw, PathBuf::from("/tmp/dns"));
        assert!(
            cfg.disable_sync,
            "control-plane feed must disable HTTP sync"
        );
        // Binds ONLY the bridge gateway — never 127.0.0.53, which
        // systemd-resolved owns on a real control-plane host (ADR-024).
        assert_eq!(cfg.listen_addrs, vec![SocketAddr::new(gw, 53)]);
        assert_eq!(cfg.node_id, 0);
        assert!(cfg.node_token.is_empty());
        assert!(cfg.control_plane_url.is_empty());
    }

    #[test]
    fn new_keeps_sync_enabled_for_workers() {
        let gw: IpAddr = "172.20.0.1".parse().unwrap();
        let cfg = ResolverConfig::new(
            1,
            "tok".into(),
            "http://cp".into(),
            gw,
            PathBuf::from("/tmp"),
        );
        assert!(
            !cfg.disable_sync,
            "worker config must keep the HTTP sync loop"
        );
        // Workers bind both the gateway and the host-local debug address.
        assert_eq!(cfg.listen_addrs.len(), 2);
    }
}
