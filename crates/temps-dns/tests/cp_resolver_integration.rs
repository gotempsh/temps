// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Real-Postgres integration test for the control-plane DNS resolver (ADR-024).
//!
//! Seeds one `*.temps.local` A record into a real `service_endpoints` table
//! (testcontainers Postgres), starts the actual control-plane resolver through
//! the public [`temps_dns::start_control_plane_resolver`] entry point, and
//! resolves the name with an in-process hickory DNS client over a real UDP
//! socket — the same path a container's stub resolver takes, minus the Docker
//! hop (the container hop is covered by the live cluster e2e). This exercises
//! the whole feature end to end: DB feeder -> `ZoneStore` -> Hickory server.
//!
//! Uses an ephemeral loopback DNS port so real UDP verification runs without
//! root privileges. Skips gracefully only when Docker/Postgres is unavailable.

use std::net::{IpAddr, SocketAddr};
use std::time::Duration;

use hickory_resolver::config::{
    NameServerConfig, ResolveHosts, ResolverConfig as ClientResolverConfig, ResolverOpts,
};
use hickory_resolver::net::runtime::TokioRuntimeProvider;
use hickory_resolver::TokioResolver;

use temps_database::test_utils::TestDatabase;
use temps_dns::services::{DnsRegistry, EndpointDraft, OwnerKind, RecordType};

const TEST_FQDN: &str = "itest-app.temps.local";
const TEST_IP: &str = "10.123.45.67";
/// A hickory stub resolver pointed straight at our resolver's UDP socket —
/// the same shape as `temps-dns-resolver`'s own end-to-end client.
fn dns_client(resolver_addr: SocketAddr) -> TokioResolver {
    let mut cfg = ClientResolverConfig::default();
    let mut name_server = NameServerConfig::udp(resolver_addr.ip());
    if let Some(conn) = name_server.connections.first_mut() {
        conn.port = resolver_addr.port();
    }
    cfg.add_name_server(name_server);
    let mut opts = ResolverOpts::default();
    // Hard failures only — never fall through to the system resolver.
    opts.use_hosts_file = ResolveHosts::Never;
    opts.attempts = 1;
    opts.timeout = Duration::from_secs(2);
    // No client-side cache: the propagation assertion below must observe the
    // resolver's live zone, not a TTL-cached first answer.
    opts.cache_size = 0;
    TokioResolver::builder_with_config(cfg, TokioRuntimeProvider::default())
        .with_options(opts)
        .build()
        .expect("build hickory test client")
}

#[tokio::test]
async fn cp_resolver_serves_zone_from_real_db() {
    // --- Real Postgres (skip if Docker/testcontainers unavailable) ---
    let test_db = match TestDatabase::with_migrations().await {
        Ok(db) => db,
        Err(error) if std::env::var_os("TEMPS_TEST_DATABASE_URL").is_some() => {
            panic!("Explicit DNS test database failed: {error}");
        }
        Err(_) => {
            println!("Docker/Postgres unavailable, skipping cp_resolver integration test");
            return;
        }
    };
    let db = test_db.connection_arc();

    // --- Seed one authoritative A record into the real service_endpoints table ---
    let registry = DnsRegistry::new(db.clone());
    let draft = EndpointDraft {
        fqdn: TEST_FQDN.into(),
        record_type: RecordType::A,
        target_ip: Some(TEST_IP.into()),
        target_port: Some(8080),
        ttl: 10,
        owner_kind: OwnerKind::Deployment,
        owner_id: 999,
        node_id: None,
    };
    registry
        .replace_endpoints_for_owner(OwnerKind::Deployment, 999, &[draft])
        .await
        .expect("seed service_endpoint");

    // Exercise the same DB feeder and DNS listener on an unprivileged port.
    let socket = std::net::UdpSocket::bind("127.0.0.1:0").expect("allocate DNS port");
    let resolver_addr = socket.local_addr().expect("DNS address");
    drop(socket);
    let snapshot_dir =
        std::env::temp_dir().join(format!("temps-cp-dns-itest-{}", uuid::Uuid::new_v4()));
    let mut config =
        temps_dns_resolver::ResolverConfig::new_local_feed(0, resolver_addr.ip(), snapshot_dir);
    config.listen_addrs = vec![resolver_addr];
    config.upstream_resolvers.clear();
    let slot = temps_dns::start_control_plane_resolver_with_config(db.clone(), config)
        .await
        .expect("control-plane DNS must bind ephemeral loopback port");
    assert_eq!(*slot.read().unwrap(), Some(resolver_addr.ip()));
    let client = dns_client(resolver_addr);

    // The DB feeder polls ~1s; retry the lookup until the zone is populated.
    let mut resolved: Option<Vec<IpAddr>> = None;
    for _ in 0..30 {
        if let Ok(answer) = client.lookup_ip(format!("{TEST_FQDN}.")).await {
            let ips: Vec<IpAddr> = answer.iter().collect();
            if !ips.is_empty() {
                resolved = Some(ips);
                break;
            }
        }
        tokio::time::sleep(Duration::from_millis(200)).await;
    }

    let ips = resolved.unwrap_or_else(|| panic!("resolver never served {TEST_FQDN} from the DB"));
    assert_eq!(ips.len(), 1, "expected exactly one A record");
    assert_eq!(ips[0].to_string(), TEST_IP);

    // An unknown in-zone name must be NXDOMAIN — served from our authority,
    // never forwarded upstream and never the seeded IP.
    let err = client
        .lookup_ip("nope.itest.temps.local.")
        .await
        .expect_err("unknown in-zone name must not resolve");
    let msg = err.to_string().to_lowercase();
    assert!(
        msg.contains("no record") || msg.contains("nxdomain") || msg.contains("not found"),
        "expected NXDOMAIN-style error, got: {msg}"
    );

    // --- The feeder must pick up a live change (generation bump) ---
    // Repoint the same owner's record to a new IP. `replace_endpoints_for_owner`
    // advances the zone generation, so the feeder re-reads the DB and replaces
    // the in-memory zone within a poll cycle (client cache is disabled, so each
    // lookup hits the resolver fresh). This guards the feeder's change-detection
    // path, which the initial load alone does not exercise.
    const TEST_IP_2: &str = "10.222.33.44";
    let updated = EndpointDraft {
        fqdn: TEST_FQDN.into(),
        record_type: RecordType::A,
        target_ip: Some(TEST_IP_2.into()),
        target_port: Some(8080),
        ttl: 10,
        owner_kind: OwnerKind::Deployment,
        owner_id: 999,
        node_id: None,
    };
    registry
        .replace_endpoints_for_owner(OwnerKind::Deployment, 999, &[updated])
        .await
        .expect("update service_endpoint");

    let mut reresolved: Option<Vec<IpAddr>> = None;
    for _ in 0..30 {
        if let Ok(answer) = client.lookup_ip(format!("{TEST_FQDN}.")).await {
            let ips: Vec<IpAddr> = answer.iter().collect();
            if ips.iter().any(|ip| ip.to_string() == TEST_IP_2) {
                reresolved = Some(ips);
                break;
            }
        }
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
    let ips =
        reresolved.unwrap_or_else(|| panic!("feeder did not pick up the updated {TEST_FQDN}"));
    assert_eq!(
        ips.len(),
        1,
        "the old record must be replaced, not appended"
    );
    assert_eq!(ips[0].to_string(), TEST_IP_2);
}
