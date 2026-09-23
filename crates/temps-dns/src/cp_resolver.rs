// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Control-plane DNS resolver bootstrap (ADR-024).
//!
//! Workers feed their `*.temps.local` resolver from the proxy-owned HTTP sync
//! service. The control plane runs the same resolver on its app-bridge gateway,
//! fed directly from `service_endpoints` with a persisted last-good snapshot.
//!
//! Both `temps proxy` and combined `temps serve` own this listener independently
//! of console initialization. Cluster DNS remains opt-in. Bootstrap failures
//! are retried in the background without blocking HTTP serving; the deployer
//! injects the resolver address only after the fixed TXT readiness probe passes.

use std::net::{IpAddr, SocketAddr};
use std::path::PathBuf;
use std::sync::{
    atomic::{AtomicU16, Ordering},
    OnceLock,
};
use std::sync::{Arc, RwLock};
use std::time::Duration;

use hickory_proto::{
    op::{Message, MessageType, OpCode, Query, ResponseCode},
    rr::{DNSClass, Name, RData, RecordType},
};
use sea_orm::DatabaseConnection;
use temps_dns_resolver::{ResolverConfig, ResolverHandle, ZoneRecord};
use temps_entities::service_endpoints;
use tracing::{info, warn};

use crate::services::DnsRegistry;

/// Shared slot the deployer reads to write the resolver IP into every
/// container's `/etc/resolv.conf` (`HostConfig.dns`). Same shape as the
/// agent's `overlay_dns_slot`.
pub type OverlayDnsSlot = Arc<RwLock<Option<IpAddr>>>;

/// How often the control-plane feeder re-reads the zone from the DB. Matches
/// the worker long-poll cadence; reads are cheap (local Postgres).
const FEED_INTERVAL: Duration = Duration::from_secs(1);

pub async fn probe_control_plane_resolver(address: SocketAddr) -> bool {
    let bind_address = if address.is_ipv6() {
        "[::]:0"
    } else {
        "0.0.0.0:0"
    };
    let socket = match tokio::net::UdpSocket::bind(bind_address).await {
        Ok(socket) => socket,
        Err(_) => return false,
    };
    if socket.connect(address).await.is_err() {
        return false;
    }
    static NEXT_ID: OnceLock<AtomicU16> = OnceLock::new();
    let seed = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |duration| {
            (duration.as_nanos() as u16) ^ std::process::id() as u16
        });
    let id = NEXT_ID
        .get_or_init(|| AtomicU16::new(seed))
        .fetch_add(1, Ordering::Relaxed);
    let mut marker_name = match Name::from_ascii(temps_dns_resolver::RESOLVER_MARKER_NAME) {
        Ok(name) => name,
        Err(_) => return false,
    };
    // Wire-decoded names are absolute; compare against an absolute question.
    marker_name.set_fqdn(true);
    let mut query = Message::new(id, MessageType::Query, OpCode::Query);
    query.add_query(Query::query(marker_name.clone(), RecordType::TXT));
    let wire = match query.to_vec() {
        Ok(wire) => wire,
        Err(_) => return false,
    };
    if socket.send(&wire).await.is_err() {
        return false;
    }
    let mut response = [0u8; 512];
    let received =
        match tokio::time::timeout(Duration::from_secs(1), socket.recv(&mut response)).await {
            Ok(Ok(received)) => received,
            _ => return false,
        };
    let Ok(response) = Message::from_vec(&response[..received]) else {
        return false;
    };
    let metadata = response.metadata;
    let question_matches = response.queries.as_slice().first().is_some_and(|question| {
        response.queries.len() == 1
            && question.name() == &marker_name
            && question.query_type() == RecordType::TXT
            && question.query_class() == DNSClass::IN
    });
    let answer_matches = response.answers.as_slice().first().is_some_and(|answer| {
        response.answers.len() == 1
            && answer.name == marker_name
            && answer.record_type() == RecordType::TXT
            && answer.dns_class == DNSClass::IN
            && matches!(&answer.data, RData::TXT(txt) if txt.txt_data.len() == 1 && txt.txt_data[0].as_ref() == temps_dns_resolver::RESOLVER_MARKER_VALUE.as_bytes())
    });
    metadata.id == id
        && metadata.message_type == MessageType::Response
        && metadata.op_code == OpCode::Query
        && metadata.authoritative
        && !metadata.truncation
        && metadata.response_code == ResponseCode::NoError
        && response.authorities.is_empty()
        && response.additionals.is_empty()
        && question_matches
        && answer_matches
}

/// `service_endpoints::Model` is field-identical to the resolver's wire
/// `ZoneRecord` (the same row the HTTP sync serialises for workers).
fn to_zone_record(m: service_endpoints::Model) -> ZoneRecord {
    ZoneRecord {
        id: m.id,
        fqdn: m.fqdn,
        record_type: m.record_type,
        target_ip: m.target_ip,
        target_port: m.target_port,
        ttl: m.ttl,
        owner_kind: m.owner_kind,
        owner_id: m.owner_id,
        node_id: m.node_id,
        generation: m.generation,
    }
}

/// Start the control-plane DNS resolver bound on `bridge_gateway:53`, fed
/// directly from `service_endpoints`. Returns the populated DNS slot for the
/// deployer to wire into containers, or `None` if the resolver could not start
/// (best-effort — the caller then continues without DNS injection, exactly as
/// the control plane does today).
pub async fn start_control_plane_resolver(
    db: Arc<DatabaseConnection>,
    bridge_gateway: IpAddr,
    snapshot_dir: PathBuf,
) -> Option<OverlayDnsSlot> {
    // node_id 0 is conventional for the control plane; the DB-direct feeder
    // ignores it (it serves the full zone, not a per-node slice). `new_local_feed`
    // binds ONLY the bridge gateway (not 127.0.0.53, which systemd-resolved owns)
    // and disables the HTTP sync loop.
    let config = ResolverConfig::new_local_feed(0, bridge_gateway, snapshot_dir);
    start_control_plane_resolver_with_config(db, config).await
}

/// Configurable entrypoint used by integration tests and alternate proxy
/// compositions. Production uses [`start_control_plane_resolver`], which pins
/// the Docker bridge address and DNS port 53.
pub async fn start_control_plane_resolver_with_config(
    db: Arc<DatabaseConnection>,
    config: ResolverConfig,
) -> Option<OverlayDnsSlot> {
    let listen_address = *config.listen_addrs.first()?;
    let bridge_gateway = listen_address.ip();
    let handle = match ResolverHandle::start(config).await {
        Ok(h) => h,
        Err(e) => {
            warn!(
                error = %e,
                bridge = %bridge_gateway,
                "control-plane DNS resolver unavailable; containers will use Docker embedded DNS"
            );
            return None;
        }
    };
    if !probe_control_plane_resolver(listen_address).await {
        warn!(address = %listen_address, "Control-plane DNS readiness probe failed; stopping resolver before retry");
        handle.shutdown().await;
        return None;
    }
    info!(
        bridge = %bridge_gateway,
        "control-plane DNS resolver listening (*.temps.local served locally)"
    );

    let registry = DnsRegistry::new(db);
    let zone = handle.zone.clone();

    // The feeder task OWNS `handle` so the Hickory server tasks stay alive for
    // the life of the process. It refreshes the in-memory zone from the DB
    // whenever the generation advances.
    tokio::spawn(async move {
        let _handle = handle; // keep the server alive
        let mut last_generation: i64 = -1;
        loop {
            match registry.get_full_zone().await {
                Ok(snapshot) => {
                    if snapshot.generation != last_generation {
                        let records: Vec<ZoneRecord> =
                            snapshot.records.into_iter().map(to_zone_record).collect();
                        zone.replace(snapshot.generation, records);
                        last_generation = snapshot.generation;
                    }
                }
                Err(e) => {
                    warn!(
                        error = %e,
                        "control-plane DNS zone refresh failed; serving last good zone"
                    );
                }
            }
            tokio::time::sleep(FEED_INTERVAL).await;
        }
    });

    Some(Arc::new(RwLock::new(Some(bridge_gateway))))
}

#[cfg(test)]
mod tests {
    use super::*;
    use temps_entities::service_endpoints;

    fn model(
        record_type: &str,
        target_ip: Option<&str>,
        target_port: Option<i32>,
    ) -> service_endpoints::Model {
        let now = chrono::Utc::now();
        service_endpoints::Model {
            id: 7,
            fqdn: "api.production.temps.local".into(),
            record_type: record_type.into(),
            target_ip: target_ip.map(str::to_string),
            target_port,
            ttl: 45,
            owner_kind: "service_member".into(),
            owner_id: 31,
            node_id: Some(2),
            generation: 99,
            created_at: now,
            updated_at: now,
        }
    }

    #[test]
    fn to_zone_record_copies_every_dns_field() {
        let m = model("A", Some("172.20.3.9"), Some(8080));
        let z = to_zone_record(m.clone());
        // Every field the resolver serves must survive the mapping verbatim.
        assert_eq!(z.id, m.id);
        assert_eq!(z.fqdn, m.fqdn);
        assert_eq!(z.record_type, m.record_type);
        assert_eq!(z.target_ip, m.target_ip);
        assert_eq!(z.target_port, m.target_port);
        assert_eq!(z.ttl, m.ttl);
        assert_eq!(z.owner_kind, m.owner_kind);
        assert_eq!(z.owner_id, m.owner_id);
        assert_eq!(z.node_id, m.node_id);
        assert_eq!(z.generation, m.generation);
        // And the result is consumable by the resolver's own accessors.
        assert_eq!(z.ip().unwrap().unwrap().to_string(), "172.20.3.9");
    }

    #[test]
    fn to_zone_record_preserves_none_and_cname_shape() {
        let z = to_zone_record(model("CNAME", Some("target.temps.local"), None));
        assert_eq!(z.record_type, "CNAME");
        assert_eq!(z.target_port, None);
        assert_eq!(z.cname_target(), Some("target.temps.local"));
        assert!(z.ip().unwrap().is_none(), "CNAME has no IP");
    }
}
