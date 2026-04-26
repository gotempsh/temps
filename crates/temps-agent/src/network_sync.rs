//! Multi-host network sync — polls the control plane for our compute_cidr
//! allocation and the peer list, then drives `temps_network::NetworkManager`
//! accordingly.
//!
//! The sync loop is *additive*: if the control plane returns `alloc: null`
//! (single-host cluster, or this node hasn't been allocated yet) we simply
//! do nothing and keep retrying. Multi-host bootstrap failures NEVER stop
//! the agent from doing its existing work — the worst case is "this node
//! cannot reach other nodes by overlay IP", same as today.
//!
//! The `temps join` CLI surface is not modified. The agent picks up the
//! overlay automatically when the control plane has decided to allocate
//! one for this node.

use std::net::IpAddr;
use std::str::FromStr;
use std::time::Duration;

use ipnet::Ipv4Net;
use serde::Deserialize;
use temps_network::{NetworkConfig, NetworkManager, NodeAlloc, Peer};
use tracing::{debug, error, info, warn};
use uuid::Uuid;

use crate::AgentConfig;

/// Wire types — match the server's `handlers::network::PeerListResponse`.
/// We re-declare them here rather than depending on `temps-deployments`
/// because that crate transitively pulls in sea-orm and we don't want it
/// in the worker build.
#[derive(Debug, Clone, Deserialize)]
struct WirePeerListResponse {
    #[serde(default)]
    alloc: Option<WireAlloc>,
    #[serde(default)]
    peers: Vec<WirePeer>,
}

#[derive(Debug, Clone, Deserialize)]
struct WireAlloc {
    node_id: String,
    compute_cidr: String,
    bridge_address: String,
    underlay_address: String,
}

#[derive(Debug, Clone, Deserialize)]
struct WirePeer {
    node_id: String,
    compute_cidr: String,
    underlay_address: String,
}

/// Default polling interval. Kept generous because the cost of a 30s lag
/// is just "a new peer becomes reachable a few seconds later" — no user
/// impact.
const POLL_INTERVAL: Duration = Duration::from_secs(30);

/// Backoff window after a transient failure (network blip, control plane
/// briefly down, etc.).
const BACKOFF_INTERVAL: Duration = Duration::from_secs(5);

/// Spawn the network-sync background task. Returns immediately; the task
/// owns its own retry loop and never blocks server startup.
pub fn spawn(config: &AgentConfig) {
    let cfg = config.clone();
    tokio::spawn(async move {
        if let Err(e) = run(cfg).await {
            // The loop is designed to retry forever; reaching this branch
            // means the loop itself unwound, which only happens on
            // unrecoverable invariant violations.
            error!("network sync loop exited unexpectedly: {}", e);
        }
    });
}

async fn run(config: AgentConfig) -> Result<(), SyncError> {
    info!(
        node_id = config.node_id,
        control_plane = %config.control_plane_url,
        "network sync loop started"
    );

    // Strict TLS — this carries the same secrets as heartbeat.
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(10))
        .danger_accept_invalid_certs(false)
        .build()
        .map_err(|e| SyncError::ClientBuild(e.to_string()))?;

    let url = format!(
        "{}/api/internal/nodes/{}/network/peers",
        config.control_plane_url.trim_end_matches('/'),
        config.node_id
    );

    let net_config = NetworkConfig::default();
    let manager = match NetworkManager::new(net_config) {
        Ok(m) => m,
        Err(e) => {
            // Static config validation failed — should be impossible since
            // we use Default. Report and exit; agent keeps working.
            return Err(SyncError::ManagerConstruct(e.to_string()));
        }
    };

    let mut bootstrapped = false;

    loop {
        match poll_once(&client, &url, &config.token).await {
            Ok(Some(payload)) => {
                if let Err(e) = apply(&manager, payload, &mut bootstrapped).await {
                    warn!(error = %e, "network sync apply failed; will retry");
                    tokio::time::sleep(BACKOFF_INTERVAL).await;
                    continue;
                }
            }
            Ok(None) => {
                // No allocation yet — single-host mode for this node.
                debug!("network sync: no compute_cidr allocated yet");
            }
            Err(e) => {
                warn!(error = %e, "network sync poll failed; will retry");
                tokio::time::sleep(BACKOFF_INTERVAL).await;
                continue;
            }
        }

        tokio::time::sleep(POLL_INTERVAL).await;
    }
}

async fn poll_once(
    client: &reqwest::Client,
    url: &str,
    token: &str,
) -> Result<Option<WirePeerListResponse>, SyncError> {
    let resp = client
        .get(url)
        .bearer_auth(token)
        .send()
        .await
        .map_err(|e| SyncError::Http(e.to_string()))?;

    let status = resp.status();
    if !status.is_success() {
        let body = resp.text().await.unwrap_or_default();
        return Err(SyncError::HttpStatus { status, body });
    }

    let payload: WirePeerListResponse = resp
        .json()
        .await
        .map_err(|e| SyncError::Parse(e.to_string()))?;

    if payload.alloc.is_none() {
        return Ok(None);
    }
    Ok(Some(payload))
}

async fn apply(
    manager: &NetworkManager,
    payload: WirePeerListResponse,
    bootstrapped: &mut bool,
) -> Result<(), SyncError> {
    let Some(alloc_wire) = payload.alloc else {
        return Ok(());
    };
    let alloc = parse_alloc(&alloc_wire)?;
    let peers: Result<Vec<Peer>, _> = payload.peers.iter().map(parse_peer).collect();
    let peers = peers?;

    if !*bootstrapped {
        info!(
            cidr = %alloc.compute_cidr,
            peers = peers.len(),
            "bringing up multi-host overlay"
        );
        manager
            .bootstrap(alloc, peers)
            .await
            .map_err(|e| SyncError::Bootstrap(e.to_string()))?;
        *bootstrapped = true;
    } else {
        let changed = manager
            .reconcile_peers(peers)
            .await
            .map_err(|e| SyncError::Reconcile(e.to_string()))?;
        if changed {
            info!("multi-host peer list updated");
        }
    }
    Ok(())
}

fn parse_alloc(w: &WireAlloc) -> Result<NodeAlloc, SyncError> {
    Ok(NodeAlloc {
        node_id: Uuid::parse_str(&w.node_id)
            .map_err(|e| SyncError::WireParse(format!("alloc.node_id: {}", e)))?,
        compute_cidr: Ipv4Net::from_str(&w.compute_cidr)
            .map_err(|e| SyncError::WireParse(format!("alloc.compute_cidr: {}", e)))?,
        bridge_address: IpAddr::from_str(&w.bridge_address)
            .map_err(|e| SyncError::WireParse(format!("alloc.bridge_address: {}", e)))?,
        underlay_address: IpAddr::from_str(&w.underlay_address)
            .map_err(|e| SyncError::WireParse(format!("alloc.underlay_address: {}", e)))?,
    })
}

fn parse_peer(w: &WirePeer) -> Result<Peer, SyncError> {
    Ok(Peer {
        node_id: Uuid::parse_str(&w.node_id)
            .map_err(|e| SyncError::WireParse(format!("peer.node_id: {}", e)))?,
        compute_cidr: Ipv4Net::from_str(&w.compute_cidr)
            .map_err(|e| SyncError::WireParse(format!("peer.compute_cidr: {}", e)))?,
        underlay_address: IpAddr::from_str(&w.underlay_address)
            .map_err(|e| SyncError::WireParse(format!("peer.underlay_address: {}", e)))?,
    })
}

#[derive(Debug, thiserror::Error)]
enum SyncError {
    #[error("failed to build http client: {0}")]
    ClientBuild(String),

    #[error("failed to construct NetworkManager: {0}")]
    ManagerConstruct(String),

    #[error("http error: {0}")]
    Http(String),

    #[error("control plane returned {status}: {body}")]
    HttpStatus {
        status: reqwest::StatusCode,
        body: String,
    },

    #[error("failed to parse peer list response: {0}")]
    Parse(String),

    #[error("malformed wire payload: {0}")]
    WireParse(String),

    #[error("bootstrap failed: {0}")]
    Bootstrap(String),

    #[error("reconcile failed: {0}")]
    Reconcile(String),
}

#[cfg(test)]
mod tests {
    use super::*;

    fn wire_alloc() -> WireAlloc {
        WireAlloc {
            node_id: "00000000-0000-0000-0000-00000000002a".into(),
            compute_cidr: "172.20.5.0/24".into(),
            bridge_address: "172.20.5.1".into(),
            underlay_address: "10.0.0.5".into(),
        }
    }

    fn wire_peer() -> WirePeer {
        WirePeer {
            node_id: "00000000-0000-0000-0000-000000000007".into(),
            compute_cidr: "172.20.6.0/24".into(),
            underlay_address: "10.0.0.6".into(),
        }
    }

    #[test]
    fn parse_alloc_ok() {
        let a = parse_alloc(&wire_alloc()).unwrap();
        assert_eq!(a.compute_cidr.to_string(), "172.20.5.0/24");
        assert_eq!(a.bridge_address.to_string(), "172.20.5.1");
        assert_eq!(a.underlay_address.to_string(), "10.0.0.5");
    }

    #[test]
    fn parse_alloc_rejects_bad_cidr() {
        let mut w = wire_alloc();
        w.compute_cidr = "not-a-cidr".into();
        let err = parse_alloc(&w).unwrap_err();
        assert!(matches!(err, SyncError::WireParse(_)));
    }

    #[test]
    fn parse_alloc_rejects_bad_uuid() {
        let mut w = wire_alloc();
        w.node_id = "not-a-uuid".into();
        let err = parse_alloc(&w).unwrap_err();
        assert!(matches!(err, SyncError::WireParse(_)));
    }

    #[test]
    fn parse_peer_ok() {
        let p = parse_peer(&wire_peer()).unwrap();
        assert_eq!(p.compute_cidr.to_string(), "172.20.6.0/24");
        assert_eq!(p.underlay_address.to_string(), "10.0.0.6");
    }

    #[test]
    fn deserialize_response_with_null_alloc() {
        // Server returns no `alloc` field at all when serde skip_serializing_if
        // is configured; verify our deserializer treats that as None.
        let json = r#"{"peers": []}"#;
        let resp: WirePeerListResponse = serde_json::from_str(json).unwrap();
        assert!(resp.alloc.is_none());
        assert!(resp.peers.is_empty());
    }

    #[test]
    fn deserialize_response_with_alloc_and_peers() {
        let json = r#"{
            "alloc": {
                "node_id": "00000000-0000-0000-0000-00000000002a",
                "compute_cidr": "172.20.5.0/24",
                "bridge_address": "172.20.5.1",
                "underlay_address": "10.0.0.5"
            },
            "peers": [{
                "node_id": "00000000-0000-0000-0000-000000000007",
                "compute_cidr": "172.20.6.0/24",
                "underlay_address": "10.0.0.6"
            }]
        }"#;
        let resp: WirePeerListResponse = serde_json::from_str(json).unwrap();
        assert!(resp.alloc.is_some());
        assert_eq!(resp.peers.len(), 1);
    }
}
