//! End-to-end integration test for [`ResolverHandle`].
//!
//! Stands up:
//!   1. A wiremock control plane that serves `dns/changes` + `dns/ack`.
//!   2. A `ResolverHandle` listening on a random ephemeral port.
//!   3. A `hickory-resolver` client pointed at that port.
//!
//! Verifies the full happy path:
//!   - resolver hydrates from the sync API,
//!   - DNS A queries return the right IP and TTL,
//!   - the resolver shuts down cleanly,
//!   - and a subsequent `start` from the same snapshot dir serves the
//!     persisted records *before* the first sync (proves disk persistence).

use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::path::PathBuf;
use std::time::Duration;

use hickory_resolver::config::{
    NameServerConfig, ResolveHosts, ResolverConfig as ClientResolverConfig, ResolverOpts,
};
use hickory_resolver::net::runtime::TokioRuntimeProvider;
use hickory_resolver::TokioResolver;
use tempfile::tempdir;
use temps_dns_resolver::{ResolverConfig, ResolverHandle};
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

/// Build a resolver-side config bound to a random localhost port. Returns
/// (config, listen socket address) so the test can point the client at it.
fn make_resolver_config(
    control_plane_url: String,
    snapshot_dir: PathBuf,
    listen_port: u16,
) -> (ResolverConfig, SocketAddr) {
    let listen = SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), listen_port);
    let cfg = ResolverConfig {
        node_id: 1,
        node_token: "test-token".into(),
        control_plane_url,
        listen_addrs: vec![listen],
        snapshot_dir,
        poll_interval: Duration::from_millis(100),
        initial_backoff: Duration::from_millis(50),
        max_backoff: Duration::from_millis(500),
        http_timeout: Duration::from_secs(2),
        // No upstream forwarder in tests — the integration suite only
        // exercises in-zone (`*.temps.local`) lookups.
        upstream_resolvers: vec![],
        disable_sync: false,
    };
    (cfg, listen)
}

/// Pick a free localhost UDP port. Bind, read the assigned port, drop
/// before returning so the resolver can re-bind. There is a tiny race
/// window — for an integration test on developer machines this is fine.
fn random_port() -> u16 {
    let sock = std::net::UdpSocket::bind("127.0.0.1:0").expect("bind random port");
    sock.local_addr().unwrap().port()
}

fn client_for(server: SocketAddr) -> TokioResolver {
    let mut cfg = ClientResolverConfig::default();
    // The test resolver listens on a random localhost UDP port; point a
    // UDP-only nameserver at exactly that address.
    let mut name_server = NameServerConfig::udp(server.ip());
    if let Some(conn) = name_server.connections.first_mut() {
        conn.port = server.port();
    }
    cfg.add_name_server(name_server);
    let mut opts = ResolverOpts::default();
    // Don't fall through to the system resolver — we want hard failures
    // when our resolver can't answer.
    opts.use_hosts_file = ResolveHosts::Never;
    opts.attempts = 1;
    opts.timeout = Duration::from_secs(2);
    TokioResolver::builder_with_config(cfg, TokioRuntimeProvider::default())
        .with_options(opts)
        .build()
        .expect("failed to build test DNS client")
}

async fn install_changes_mock(server: &MockServer, body: serde_json::Value) {
    Mock::given(method("GET"))
        .and(path("/internal/nodes/1/dns/changes"))
        .respond_with(ResponseTemplate::new(200).set_body_json(body))
        .mount(server)
        .await;
    Mock::given(method("POST"))
        .and(path("/internal/nodes/1/dns/ack"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "node_id": 1, "applied_generation": 1, "server_generation": 1
        })))
        .mount(server)
        .await;
}

#[tokio::test]
async fn resolver_serves_records_synced_from_control_plane() {
    let server = MockServer::start().await;
    install_changes_mock(
        &server,
        serde_json::json!({
            "generation": 1,
            "full_snapshot": true,
            "records": [{
                "id": 1,
                "fqdn": "pg-orders-0.pg-orders.temps.local",
                "record_type": "A",
                "target_ip": "172.20.5.10",
                "target_port": 5432,
                "ttl": 30,
                "owner_kind": "service_member",
                "owner_id": 1,
                "node_id": 1,
                "generation": 1
            }],
            "removed_ids": []
        }),
    )
    .await;

    let dir = tempdir().unwrap();
    let port = random_port();
    let (cfg, listen) = make_resolver_config(server.uri(), dir.path().to_path_buf(), port);
    let handle = ResolverHandle::start(cfg).await.expect("resolver starts");

    // Wait for first sync to complete.
    tokio::time::timeout(Duration::from_secs(2), async {
        loop {
            if handle.zone.generation() >= 1 {
                return;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .expect("sync did not converge in time");

    let client = client_for(listen);
    let answer = client
        .lookup_ip("pg-orders-0.pg-orders.temps.local.")
        .await
        .expect("dig succeeds");
    let ips: Vec<IpAddr> = answer.iter().collect();
    assert_eq!(ips.len(), 1);
    assert_eq!(ips[0].to_string(), "172.20.5.10");

    handle.shutdown().await;
}

#[tokio::test]
async fn resolver_returns_nxdomain_for_unknown_name() {
    let server = MockServer::start().await;
    install_changes_mock(
        &server,
        serde_json::json!({
            "generation": 1,
            "full_snapshot": true,
            "records": [],
            "removed_ids": []
        }),
    )
    .await;

    let dir = tempdir().unwrap();
    let port = random_port();
    let (cfg, listen) = make_resolver_config(server.uri(), dir.path().to_path_buf(), port);
    let handle = ResolverHandle::start(cfg).await.expect("resolver starts");

    let client = client_for(listen);
    let err = client
        .lookup_ip("nonexistent.temps.local.")
        .await
        .unwrap_err();
    let msg = err.to_string().to_lowercase();
    assert!(
        msg.contains("no record") || msg.contains("nxdomain") || msg.contains("not found"),
        "expected NXDOMAIN-style error, got: {msg}"
    );

    handle.shutdown().await;
}

#[tokio::test]
async fn resolver_serves_from_disk_snapshot_before_first_sync() {
    // First boot: hydrate from sync, write disk snapshot, shut down.
    let server = MockServer::start().await;
    install_changes_mock(
        &server,
        serde_json::json!({
            "generation": 5,
            "full_snapshot": true,
            "records": [{
                "id": 42,
                "fqdn": "warm.temps.local",
                "record_type": "A",
                "target_ip": "10.0.0.42",
                "target_port": null,
                "ttl": 30,
                "owner_kind": "static",
                "owner_id": 42,
                "node_id": null,
                "generation": 5
            }],
            "removed_ids": []
        }),
    )
    .await;

    let dir = tempdir().unwrap();
    let port_a = random_port();
    let (cfg_a, _listen_a) = make_resolver_config(server.uri(), dir.path().to_path_buf(), port_a);
    let handle_a = ResolverHandle::start(cfg_a).await.expect("first boot");
    tokio::time::timeout(Duration::from_secs(2), async {
        while handle_a.zone.generation() < 5 {
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .expect("first sync converged");
    handle_a.shutdown().await;

    // Second boot: control plane unreachable on purpose. Resolver must
    // still answer from the on-disk snapshot before the first sync round.
    let cold_server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/internal/nodes/1/dns/changes"))
        .respond_with(ResponseTemplate::new(503))
        .mount(&cold_server)
        .await;

    let port_b = random_port();
    let (cfg_b, listen_b) =
        make_resolver_config(cold_server.uri(), dir.path().to_path_buf(), port_b);
    let handle_b = ResolverHandle::start(cfg_b).await.expect("second boot");

    // Disk snapshot must already be loaded.
    assert_eq!(handle_b.zone.generation(), 5);

    let client = client_for(listen_b);
    let answer = client.lookup_ip("warm.temps.local.").await.expect("dig");
    let ips: Vec<IpAddr> = answer.iter().collect();
    assert_eq!(ips.len(), 1);
    assert_eq!(ips[0].to_string(), "10.0.0.42");

    handle_b.shutdown().await;
}
