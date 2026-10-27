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
use std::sync::{Arc, RwLock};
use std::time::Duration;

use sea_orm::DatabaseConnection;
use temps_dns_resolver::{DnsPolicySlot, ResolverConfig, ResolverHandle, ZoneRecord};
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

/// Owns both the listener and its database feeder so a gateway change can
/// stop the old listener before binding the replacement.
pub struct ControlPlaneResolver {
    handle: Option<ResolverHandle>,
    feeder: tokio::task::JoinHandle<()>,
    gateway: IpAddr,
}

impl Drop for ControlPlaneResolver {
    fn drop(&mut self) {
        self.feeder.abort();
        // ResolverHandle::drop aborts the DNS server and releases its sockets.
    }
}

impl ControlPlaneResolver {
    pub fn gateway(&self) -> IpAddr {
        self.gateway
    }

    pub async fn shutdown(mut self) {
        self.feeder.abort();
        let _ = (&mut self.feeder).await;
        if let Some(handle) = self.handle.take() {
            handle.shutdown().await;
        }
    }
}

/// Whether a resolver answers its readiness marker at `address`. See
/// [`temps_dns_resolver::probe::probe_resolver`].
pub async fn probe_control_plane_resolver(address: SocketAddr) -> bool {
    temps_dns_resolver::probe::probe_resolver(address).await
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
/// directly from `service_endpoints`. The caller owns the returned listener
/// and must shut it down before binding a replacement gateway.
///
/// `policy` is node 0's network policy view, kept current by its enforcer;
/// with it, restricted workloads only resolve the names their rules allow.
pub async fn start_control_plane_resolver(
    db: Arc<DatabaseConnection>,
    bridge_gateway: IpAddr,
    snapshot_dir: PathBuf,
    policy: Option<DnsPolicySlot>,
) -> Option<ControlPlaneResolver> {
    // node_id 0 is conventional for the control plane; the DB-direct feeder
    // ignores it (it serves the full zone, not a per-node slice). `new_local_feed`
    // binds ONLY the bridge gateway (not 127.0.0.53, which systemd-resolved owns)
    // and disables the HTTP sync loop.
    let config = ResolverConfig::new_local_feed(0, bridge_gateway, snapshot_dir);
    start_resolver(db, config, policy).await
}

/// Configurable entrypoint used by integration tests and alternate proxy
/// compositions. Production uses [`start_control_plane_resolver`], which pins
/// the Docker bridge address and DNS port 53.
pub async fn start_control_plane_resolver_with_config(
    db: Arc<DatabaseConnection>,
    config: ResolverConfig,
) -> Option<ControlPlaneResolver> {
    start_resolver(db, config, None).await
}

async fn start_resolver(
    db: Arc<DatabaseConnection>,
    config: ResolverConfig,
    policy: Option<DnsPolicySlot>,
) -> Option<ControlPlaneResolver> {
    let listen_address = *config.listen_addrs.first()?;
    let bridge_gateway = listen_address.ip();
    let handle = match ResolverHandle::start_with_policy(config, policy).await {
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

    // Keep ownership with the caller so gateway reconciliation can shut down
    // both the feeder and the bound DNS listener.
    let feeder = tokio::spawn(async move {
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

    Some(ControlPlaneResolver {
        handle: Some(handle),
        feeder,
        gateway: bridge_gateway,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use sea_orm::{DatabaseBackend, MockDatabase};
    use temps_entities::service_endpoints;

    #[tokio::test]
    async fn managed_resolver_shutdown_releases_udp_and_tcp_listeners() {
        let reservation = std::net::TcpListener::bind("127.0.0.1:0").expect("reserve test port");
        let address = reservation.local_addr().expect("test address");
        drop(reservation);
        let db = Arc::new(MockDatabase::new(DatabaseBackend::Postgres).into_connection());
        let mut config = ResolverConfig::new_local_feed(
            0,
            address.ip(),
            std::env::temp_dir().join(format!("temps-cp-dns-shutdown-{}", uuid::Uuid::new_v4())),
        );
        config.listen_addrs = vec![address];
        config.upstream_resolvers.clear();

        let first = start_control_plane_resolver_with_config(db.clone(), config.clone())
            .await
            .expect("first resolver starts");
        assert!(
            start_control_plane_resolver_with_config(db.clone(), config.clone())
                .await
                .is_none(),
            "second resolver cannot bind occupied port"
        );
        first.shutdown().await;
        let restarted = start_control_plane_resolver_with_config(db, config)
            .await
            .expect("resolver restarts after shutdown");
        restarted.shutdown().await;
    }

    /// The resolver must answer from the very slot it was started with,
    /// which is the one the node's enforcer writes: a restriction stored
    /// there after start changes the next answer.
    #[tokio::test]
    async fn the_resolver_answers_from_the_policy_slot_it_was_given() {
        use hickory_resolver::config::{
            NameServerConfig, ResolveHosts, ResolverConfig as ClientConfig, ResolverOpts,
        };
        use hickory_resolver::net::runtime::TokioRuntimeProvider;
        use temps_workload::{WorkloadKind, WorkloadRef};
        use temps_workload_netpol::{DnsPolicy, IdentityMap, Profile, WorkloadPolicy};

        let reservation = std::net::UdpSocket::bind("127.0.0.1:0").expect("reserve test port");
        let address = reservation.local_addr().expect("test address");
        drop(reservation);
        let db = Arc::new(MockDatabase::new(DatabaseBackend::Postgres).into_connection());
        let mut config = ResolverConfig::new_local_feed(
            0,
            address.ip(),
            std::env::temp_dir().join(format!("temps-cp-dns-policy-{}", uuid::Uuid::new_v4())),
        );
        config.listen_addrs = vec![address];
        config.upstream_resolvers.clear();

        let slot = temps_dns_resolver::open_dns_policy_slot();
        let resolver = start_resolver(db, config, Some(slot.clone()))
            .await
            .expect("resolver starts");
        // The mock database has no rows, so the feeder never replaces this.
        let handle = resolver.handle.as_ref().expect("running resolver");
        handle.zone.replace(
            1,
            vec![to_zone_record(model("A", Some("10.202.0.9"), None))],
        );

        let mut client_config = ClientConfig::default();
        let mut name_server = NameServerConfig::udp(address.ip());
        if let Some(connection) = name_server.connections.first_mut() {
            connection.port = address.port();
        }
        client_config.add_name_server(name_server);
        let mut options = ResolverOpts::default();
        options.use_hosts_file = ResolveHosts::Never;
        options.attempts = 1;
        options.timeout = std::time::Duration::from_secs(2);
        options.cache_size = 0;
        let client = hickory_resolver::TokioResolver::builder_with_config(
            client_config,
            TokioRuntimeProvider::default(),
        )
        .with_options(options)
        .build()
        .expect("test DNS client");

        let open = client.lookup_ip("api.production.temps.local.").await;
        assert!(open.is_ok(), "an open source resolves the name: {open:?}");

        // The client asks from 127.0.0.1: make that a restricted workload
        // with no rules, so it may resolve nothing.
        let sandbox = WorkloadRef::new(WorkloadKind::Sandbox, 1);
        slot.store(Arc::new(DnsPolicy::build(
            &[WorkloadPolicy {
                subject: sandbox,
                egress: Profile::Restricted,
                ingress: Profile::Open,
                rules: vec![],
            }],
            &IdentityMap::new().with(sandbox, std::net::Ipv4Addr::LOCALHOST.into()),
            &[],
            &[],
        )));
        let restricted = client.lookup_ip("api.production.temps.local.").await;
        assert!(
            restricted.is_err(),
            "a restricted source with no rules gets no answer: {restricted:?}"
        );
        resolver.shutdown().await;
    }

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
