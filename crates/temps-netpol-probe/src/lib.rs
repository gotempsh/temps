// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! The per-node network policy runtime as one small binary (ADR-055 §9).
//!
//! `serve` runs the two hot-path pieces a node puts in front of
//! restricted workloads, built from one [`ServeConfig`]:
//!
//! - the internal resolver ([`temps_dns_resolver::ZoneAuthority`]) with the
//!   policy snapshot, so a restricted source resolves only what its
//!   policy names and never an external name;
//! - the egress proxy ([`temps_workload_netpol::EgressProxy`]), which
//!   identifies the workload by source address and applies hostname and
//!   CIDR rules plus the public-address check.
//!
//! `sink` is the other end of the proofs: it accepts TCP on the given
//! ports (answering a minimal HTTP response) and logs every UDP datagram
//! with its source address, which is what the anti-spoofing proof reads.
//!
//! The binary is cross-built for Linux (musl, static) so it runs in a bare
//! container on the Docker VM; the proofs in `tests/` drive it.

use std::net::{IpAddr, SocketAddr};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use arc_swap::ArcSwap;
use serde::{Deserialize, Serialize};
use temps_dns_resolver::upstream::UpstreamResolver;
use temps_dns_resolver::{ZoneAuthority, ZoneRecord, ZoneStore};
use temps_workload::WorkloadRef;
use temps_workload_netpol::dns::{Alias, NamedWorkload};
use temps_workload_netpol::{
    DnsPolicy, EgressPolicy, EgressProxy, EgressProxyConfig, EgressTable, IdentityMap,
    RelayUpstream, WorkloadPolicy,
};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, UdpSocket};
use tracing::{info, warn};

#[derive(Debug, thiserror::Error)]
pub enum ProbeError {
    #[error("reading config {path}: {reason}")]
    Config { path: String, reason: String },
    #[error("binding {addr}: {reason}")]
    Bind { addr: String, reason: String },
    #[error("egress proxy: {reason}")]
    Egress { reason: String },
    #[error("usage: {usage}")]
    Usage { usage: String },
}

/// Everything `serve` needs, as the control plane would push it.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ServeConfig {
    pub dns_listen: Option<SocketAddr>,
    #[serde(default)]
    pub upstream: Vec<SocketAddr>,
    pub proxy_listen: Option<SocketAddr>,
    #[serde(default)]
    pub max_connections: Option<usize>,
    #[serde(default)]
    pub relay: Option<String>,
    #[serde(default)]
    pub records: Vec<ZoneRecord>,
    #[serde(default)]
    pub policies: Vec<WorkloadPolicy>,
    #[serde(default)]
    pub identities: Vec<(WorkloadRef, IpAddr)>,
    #[serde(default)]
    pub names: Vec<NamedWorkload>,
    #[serde(default)]
    pub aliases: Vec<Alias>,
    #[serde(default)]
    pub egress: Vec<EgressPolicy>,
    /// Seconds between stats lines on stdout. `None`: never.
    #[serde(default)]
    pub stats_every_secs: Option<u64>,
}

impl ServeConfig {
    pub fn identity_map(&self) -> IdentityMap {
        let mut map = IdentityMap::new();
        for (workload, addr) in &self.identities {
            map.insert(*workload, *addr);
        }
        map
    }
}

pub fn read_config(path: &PathBuf) -> Result<ServeConfig, ProbeError> {
    let bytes = std::fs::read(path).map_err(|e| ProbeError::Config {
        path: path.display().to_string(),
        reason: e.to_string(),
    })?;
    serde_json::from_slice(&bytes).map_err(|e| ProbeError::Config {
        path: path.display().to_string(),
        reason: e.to_string(),
    })
}

/// Runs the resolver and the proxy until one of them stops.
pub async fn serve(config: ServeConfig) -> Result<(), ProbeError> {
    let identities = config.identity_map();
    let dns = Arc::new(ArcSwap::from_pointee(DnsPolicy::build(
        &config.policies,
        &identities,
        &config.names,
        &config.aliases,
    )));
    let table = Arc::new(ArcSwap::from_pointee(EgressTable::build(
        &config.egress,
        &identities,
    )));

    let mut tasks = tokio::task::JoinSet::new();
    if let Some(addr) = config.dns_listen {
        let zone = Arc::new(ZoneStore::new(PathBuf::from("/dev/null")));
        zone.replace(1, config.records.clone());
        let mut authority = ZoneAuthority::new(zone).with_policy(dns.clone());
        if let Some(upstream) = UpstreamResolver::new(&config.upstream) {
            authority = authority.with_upstream(Arc::new(upstream));
        }
        let socket = UdpSocket::bind(addr).await.map_err(|e| ProbeError::Bind {
            addr: addr.to_string(),
            reason: e.to_string(),
        })?;
        let mut server = hickory_server::server::Server::new(authority);
        server.register_socket(socket);
        info!(%addr, restricted_sources = dns.load().restricted_sources(), "resolver listening");
        tasks.spawn(async move {
            if let Err(e) = server.block_until_done().await {
                warn!(error = %e, "resolver stopped");
            }
        });
    }
    if let Some(addr) = config.proxy_listen {
        let mut proxy_config = EgressProxyConfig::new(addr);
        if let Some(max) = config.max_connections {
            proxy_config.max_connections = max;
        }
        if let Some(relay) = &config.relay {
            proxy_config.relay =
                Some(RelayUpstream::parse(relay).map_err(|e| ProbeError::Egress {
                    reason: e.to_string(),
                })?);
        }
        let proxy =
            EgressProxy::new(proxy_config, table.clone()).map_err(|e| ProbeError::Egress {
                reason: e.to_string(),
            })?;
        let listener = proxy.bind().await.map_err(|e| ProbeError::Bind {
            addr: addr.to_string(),
            reason: e.to_string(),
        })?;
        if let Some(every) = config.stats_every_secs.filter(|s| *s > 0) {
            let stats = proxy.clone();
            tasks.spawn(async move {
                loop {
                    tokio::time::sleep(Duration::from_secs(every)).await;
                    if let Ok(line) = serde_json::to_string(&stats.stats().snapshot()) {
                        println!("stats {line}");
                    }
                }
            });
        }
        tasks.spawn(async move {
            if let Err(e) = proxy.serve(listener).await {
                warn!(error = %e, "egress proxy stopped");
            }
        });
    }
    // Ends when the first task does: either listener failing ends the run.
    let _ = tasks.join_next().await;
    Ok(())
}

const SINK_RESPONSE: &[u8] =
    b"HTTP/1.1 200 OK\r\nContent-Length: 3\r\nConnection: close\r\n\r\nok\n";

/// Accepts TCP on `tcp_ports`, logs UDP datagrams on `udp_port`.
pub async fn sink(udp_port: Option<u16>, tcp_ports: Vec<u16>) -> Result<(), ProbeError> {
    let mut tasks = tokio::task::JoinSet::new();
    for port in tcp_ports {
        let listener =
            TcpListener::bind(("0.0.0.0", port))
                .await
                .map_err(|e| ProbeError::Bind {
                    addr: format!("0.0.0.0:{port}"),
                    reason: e.to_string(),
                })?;
        tasks.spawn(async move {
            while let Ok((mut stream, peer)) = listener.accept().await {
                println!("tcp {port} {}", peer.ip());
                tokio::spawn(async move {
                    let mut buf = [0u8; 1024];
                    let _ = tokio::time::timeout(Duration::from_millis(500), stream.read(&mut buf))
                        .await;
                    let _ = stream.write_all(SINK_RESPONSE).await;
                    let _ = stream.shutdown().await;
                });
            }
        });
    }
    if let Some(port) = udp_port {
        let socket = UdpSocket::bind(("0.0.0.0", port))
            .await
            .map_err(|e| ProbeError::Bind {
                addr: format!("0.0.0.0:{port}"),
                reason: e.to_string(),
            })?;
        tasks.spawn(async move {
            let mut buf = [0u8; 512];
            while let Ok((n, peer)) = socket.recv_from(&mut buf).await {
                let text = String::from_utf8_lossy(&buf[..n]);
                println!("udp {port} {} {}", peer.ip(), text.trim());
            }
        });
    }
    let _ = tasks.join_next().await;
    Ok(())
}

/// The address [`public_dns`] answers every `A` query with
/// (TEST-NET-3, never routed).
pub const PUBLIC_DNS_ANSWER: std::net::Ipv4Addr = std::net::Ipv4Addr::new(203, 0, 113, 80);

/// A stand-in for a public resolver: answers every `A` query with
/// [`PUBLIC_DNS_ANSWER`] and logs who asked. The proofs use it as the
/// "external" resolver, so they need no Internet access: an answer from it
/// means the name was resolved outside the internal zone.
pub async fn public_dns(port: u16) -> Result<(), ProbeError> {
    use hickory_proto::op::{Message, MessageType, ResponseCode};
    use hickory_proto::rr::{rdata::A, RData, Record, RecordType};

    let socket = UdpSocket::bind(("0.0.0.0", port))
        .await
        .map_err(|e| ProbeError::Bind {
            addr: format!("0.0.0.0:{port}"),
            reason: e.to_string(),
        })?;
    let mut buf = [0u8; 1500];
    while let Ok((n, peer)) = socket.recv_from(&mut buf).await {
        let Ok(request) = Message::from_vec(&buf[..n]) else {
            continue;
        };
        let mut response = Message::response(request.metadata.id, request.metadata.op_code);
        response.metadata.message_type = MessageType::Response;
        response.metadata.recursion_desired = request.metadata.recursion_desired;
        response.metadata.recursion_available = true;
        response.metadata.response_code = ResponseCode::NoError;
        for query in &request.queries {
            response.add_query(query.clone());
            println!(
                "dns {} {} {:?}",
                peer.ip(),
                query.name(),
                query.query_type()
            );
            if query.query_type() == RecordType::A {
                response.add_answer(Record::from_rdata(
                    query.name().clone(),
                    5,
                    RData::A(A(PUBLIC_DNS_ANSWER)),
                ));
            }
        }
        if let Ok(bytes) = response.to_vec() {
            let _ = socket.send_to(&bytes, peer).await;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn configs_round_trip() {
        let json = r#"{
            "dns_listen": "0.0.0.0:53",
            "proxy_listen": "0.0.0.0:3128",
            "identities": [[{"kind": "sandbox", "id": 1}, "10.237.1.10"]],
            "egress": [{"subject": {"kind": "sandbox", "id": 1}, "rules": [
                {"id": 1, "target": {"host": "*.example.test"}, "action": "allow"}
            ]}]
        }"#;
        let config: ServeConfig = serde_json::from_str(json).unwrap();
        let identities = config.identity_map();
        let table = EgressTable::build(&config.egress, &identities);
        assert_eq!(table.len(), 1);
        assert!(serde_json::from_str::<ServeConfig>(r#"{"egress": [{"subject": {"kind": "sandbox", "id": 1}, "rules": [{"id": 1, "target": {"host": "a b"}, "action": "allow"}]}]}"#).is_err());
    }
}
