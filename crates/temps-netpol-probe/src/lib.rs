// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! The per-node network policy runtime as one small binary (ADR-055 §9).
//!
//! `serve` runs the two hot-path pieces a node puts in front of
//! restricted workloads, built from one [`ServeConfig`]:
//!
//! - the internal resolver ([`temps_node_dns::ZoneAuthority`]) with the
//!   policy snapshot, so a restricted source resolves only what its
//!   policy names and never an external name;
//! - the egress proxy ([`temps_workload_netpol::EgressProxy`]), which
//!   identifies the workload by source address and applies hostname and
//!   CIDR rules plus the public-address check.
//!
//! With `recording` set, the proxy records the connections of workloads
//! whose policy opts in and delivers them, batched, to the control plane's
//! `POST /internal/nodes/{id}/egress-records` with the node token, exactly
//! as a worker's agent would ([`HttpRecordSink`]).
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
use temps_node_dns::upstream::UpstreamResolver;
use temps_node_dns::{ZoneAuthority, ZoneRecord, ZoneStore};
use temps_workload::WorkloadRef;
use temps_workload_netpol::dns::{Alias, NamedWorkload};
use temps_workload_netpol::egress::record::RECORD_CHANNEL_CAPACITY;
use temps_workload_netpol::{
    recording_channel, DnsPolicy, EgressBatcher, EgressPolicy, EgressProxy, EgressProxyConfig,
    EgressRecordBatch, EgressRecordSink, EgressTable, IdentityMap, RecordSinkError, RelayUpstream,
    WorkloadPolicy,
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
    /// Where connection records go. `None`: the proxy has no recorder and
    /// records nothing, whatever the policies say.
    #[serde(default)]
    pub recording: Option<RecordingConfig>,
}

/// The control plane endpoint records are delivered to.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RecordingConfig {
    /// `http://host:port/api/internal/nodes/{id}/egress-records`.
    pub url: String,
    /// The node token.
    pub token: String,
    /// The batch age flush, in milliseconds. Default: the production 5 s.
    #[serde(default)]
    pub max_batch_age_ms: Option<u64>,
    /// Records queued between the proxy and the batcher. Default: the
    /// production capacity.
    #[serde(default)]
    pub channel_capacity: Option<usize>,
}

/// Delivers batches with one plain-HTTP/1.1 `POST` each (the probe runs in a
/// bare container, so it carries no TLS stack; the control plane is
/// reached over the node network). A non-2xx answer is a failed delivery.
pub struct HttpRecordSink {
    authority: String,
    path: String,
    token: String,
}

impl HttpRecordSink {
    pub fn new(url: &str, token: &str) -> Result<Self, ProbeError> {
        let invalid = |reason: &str| ProbeError::Config {
            path: url.to_string(),
            reason: reason.to_string(),
        };
        let rest = url
            .strip_prefix("http://")
            .ok_or_else(|| invalid("recording url must be http://"))?;
        let (authority, path) = rest
            .split_once('/')
            .ok_or_else(|| invalid("recording url has no path"))?;
        Ok(Self {
            authority: authority.to_string(),
            path: format!("/{path}"),
            token: token.to_string(),
        })
    }

    async fn post(&self, body: &[u8]) -> Result<u16, String> {
        let mut stream = tokio::time::timeout(
            Duration::from_secs(5),
            tokio::net::TcpStream::connect(&self.authority),
        )
        .await
        .map_err(|_| "connect timed out".to_string())?
        .map_err(|e| e.to_string())?;
        let head = format!(
            "POST {} HTTP/1.1\r\nHost: {}\r\nAuthorization: Bearer {}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
            self.path,
            self.authority,
            self.token,
            body.len()
        );
        stream
            .write_all(head.as_bytes())
            .await
            .map_err(|e| e.to_string())?;
        stream.write_all(body).await.map_err(|e| e.to_string())?;
        let mut response = Vec::new();
        tokio::time::timeout(Duration::from_secs(10), stream.read_to_end(&mut response))
            .await
            .map_err(|_| "response timed out".to_string())?
            .map_err(|e| e.to_string())?;
        let text = String::from_utf8_lossy(&response);
        let status = text
            .split_whitespace()
            .nth(1)
            .and_then(|code| code.parse::<u16>().ok())
            .ok_or_else(|| format!("unreadable response: {}", text.lines().next().unwrap_or("")))?;
        if !(200..300).contains(&status) {
            let body = text.split_once("\r\n\r\n").map(|(_, b)| b).unwrap_or("");
            println!("records rejected {status} {}", body.trim());
        }
        Ok(status)
    }
}

#[async_trait::async_trait]
impl EgressRecordSink for HttpRecordSink {
    async fn deliver(&self, batch: EgressRecordBatch) -> Result<(), RecordSinkError> {
        let records = batch.records.len();
        let failed = |reason: String| RecordSinkError::Delivery {
            destination: format!("http://{}{}", self.authority, self.path),
            records,
            reason,
        };
        let body = serde_json::to_vec(&batch).map_err(|e| failed(e.to_string()))?;
        match self.post(&body).await {
            Ok(status) if (200..300).contains(&status) => {
                println!("records delivered {records}");
                Ok(())
            }
            Ok(status) => Err(failed(format!("answered {status}"))),
            Err(reason) => Err(failed(reason)),
        }
    }
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
        let (recorder, records) = match &config.recording {
            Some(recording) => {
                let (recorder, stream) = recording_channel(
                    recording
                        .channel_capacity
                        .unwrap_or(RECORD_CHANNEL_CAPACITY),
                );
                (Some(recorder), Some((stream, recording.clone())))
            }
            None => (None, None),
        };
        let proxy =
            EgressProxy::with_recorder(proxy_config, table.clone(), recorder).map_err(|e| {
                ProbeError::Egress {
                    reason: e.to_string(),
                }
            })?;
        if let Some((stream, recording)) = records {
            let sink = Arc::new(HttpRecordSink::new(&recording.url, &recording.token)?);
            let mut batcher = EgressBatcher::default();
            if let Some(ms) = recording.max_batch_age_ms {
                batcher.max_age = Duration::from_millis(ms.max(1));
            }
            info!(url = %recording.url, "egress proxy records opted-in workloads");
            tasks.spawn(batcher.run(stream, sink, proxy.stats().clone()));
        }
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

/// A stand-in for the control plane's record endpoint: accepts one batch
/// per `POST`, answers `200`, and prints `records received <n> dropped <d>
/// total <t>` for each, so the proofs can count what a recording proxy
/// delivered without a control plane on the Docker network.
pub async fn record_sink(port: u16) -> Result<(), ProbeError> {
    use std::sync::atomic::{AtomicU64, Ordering};
    let listener = TcpListener::bind(("0.0.0.0", port))
        .await
        .map_err(|e| ProbeError::Bind {
            addr: format!("0.0.0.0:{port}"),
            reason: e.to_string(),
        })?;
    let total = Arc::new(AtomicU64::new(0));
    while let Ok((mut stream, _)) = listener.accept().await {
        let total = total.clone();
        tokio::spawn(async move {
            let mut seen = Vec::new();
            let mut buf = vec![0u8; 64 * 1024];
            let body = loop {
                let Ok(Ok(n)) =
                    tokio::time::timeout(Duration::from_secs(10), stream.read(&mut buf)).await
                else {
                    return;
                };
                if n == 0 {
                    return;
                }
                seen.extend_from_slice(&buf[..n]);
                let Some(end) = seen.windows(4).position(|w| w == b"\r\n\r\n") else {
                    continue;
                };
                let head = String::from_utf8_lossy(&seen[..end]).to_ascii_lowercase();
                let length = head
                    .lines()
                    .find_map(|l| l.strip_prefix("content-length:"))
                    .and_then(|v| v.trim().parse::<usize>().ok())
                    .unwrap_or(0);
                if seen.len() >= end + 4 + length {
                    break seen[end + 4..end + 4 + length].to_vec();
                }
            };
            let (status, line) = match serde_json::from_slice::<EgressRecordBatch>(&body) {
                Ok(batch) => {
                    let n = batch.records.len() as u64;
                    let t = total.fetch_add(n, Ordering::Relaxed) + n;
                    let repeats: u64 = batch.records.iter().map(|r| u64::from(r.count)).sum();
                    (
                        "200 OK",
                        format!(
                            "records received {n} connections {repeats} dropped {} total {t}",
                            batch.dropped
                        ),
                    )
                }
                Err(e) => ("400 Bad Request", format!("records unreadable: {e}")),
            };
            println!("{line}");
            let _ = stream
                .write_all(
                    format!("HTTP/1.1 {status}\r\nContent-Length: 0\r\nConnection: close\r\n\r\n")
                        .as_bytes(),
                )
                .await;
        });
    }
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
        assert!(config.recording.is_none(), "no recorder unless configured");
        let recording: ServeConfig = serde_json::from_str(
            r#"{"recording": {"url": "http://10.0.0.1:8080/api/internal/nodes/3/egress-records", "token": "t", "max_batch_age_ms": 500},
                "egress": [{"subject": {"kind": "sandbox", "id": 1}, "rules": [], "record": true}]}"#,
        )
        .unwrap();
        assert!(recording.egress[0].record);
        assert_eq!(recording.recording.unwrap().max_batch_age_ms, Some(500));
    }

    #[test]
    fn the_sink_takes_only_plain_http_urls_with_a_path() {
        assert!(HttpRecordSink::new("https://cp.example.test/x", "t").is_err());
        assert!(HttpRecordSink::new("http://cp.example.test", "t").is_err());
        let sink = HttpRecordSink::new(
            "http://10.0.0.1:8080/api/internal/nodes/3/egress-records",
            "t",
        )
        .unwrap();
        assert_eq!(sink.authority, "10.0.0.1:8080");
        assert_eq!(sink.path, "/api/internal/nodes/3/egress-records");
    }

    /// One delivery is one POST with the node token and the batch as JSON;
    /// a non-2xx answer is a failed delivery.
    #[tokio::test]
    async fn the_sink_posts_the_batch_with_the_node_token() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            let mut requests = Vec::new();
            for status in ["200 OK", "403 Forbidden"] {
                let (mut stream, _) = listener.accept().await.unwrap();
                let mut seen = Vec::new();
                let mut buf = [0u8; 4096];
                loop {
                    let n = stream.read(&mut buf).await.unwrap();
                    seen.extend_from_slice(&buf[..n]);
                    let text = String::from_utf8_lossy(&seen).to_string();
                    if let Some((head, body)) = text.split_once("\r\n\r\n") {
                        let length: usize = head
                            .lines()
                            .find_map(|l| l.strip_prefix("Content-Length: "))
                            .unwrap()
                            .parse()
                            .unwrap();
                        if body.len() >= length {
                            break;
                        }
                    }
                }
                stream
                    .write_all(format!("HTTP/1.1 {status}\r\nContent-Length: 0\r\n\r\n").as_bytes())
                    .await
                    .unwrap();
                requests.push(String::from_utf8(seen).unwrap());
            }
            requests
        });
        let sink = HttpRecordSink::new(
            &format!("http://{addr}/api/internal/nodes/3/egress-records"),
            "node-token",
        )
        .unwrap();
        let batch = EgressRecordBatch {
            records: Vec::new(),
            dropped: 2,
        };
        assert!(sink.deliver(batch.clone()).await.is_ok());
        assert!(
            sink.deliver(batch).await.is_err(),
            "a 403 is a failed delivery"
        );
        let requests = server.await.unwrap();
        assert!(requests[0].starts_with("POST /api/internal/nodes/3/egress-records HTTP/1.1\r\n"));
        assert!(requests[0].contains("Authorization: Bearer node-token\r\n"));
        assert!(requests[0].ends_with(r#"{"records":[],"dropped":2}"#));
    }
}
