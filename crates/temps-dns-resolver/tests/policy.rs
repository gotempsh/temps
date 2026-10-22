// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Policy-aware answers (ADR-055 §9): a restricted source resolves the
//! names its policy allows, and the group short names of those, and
//! nothing else — not other workloads' names, and never a forwarded
//! external name. An open source keeps today's behaviour.
//!
//! The client asks from 127.0.0.1, which the policy snapshot maps to a
//! restricted sandbox; swapping the snapshot to open shows the same query
//! from the same address being answered again, so the difference is the
//! policy and nothing else.

use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use arc_swap::ArcSwap;
use hickory_proto::rr::RecordType;
use hickory_resolver::config::{
    NameServerConfig, ResolveHosts, ResolverConfig as ClientResolverConfig, ResolverOpts,
};
use hickory_resolver::net::runtime::TokioRuntimeProvider;
use hickory_resolver::TokioResolver;
use hickory_server::server::Server;
use temps_dns_resolver::{ZoneAuthority, ZoneRecord, ZoneStore};
use temps_workload::{WorkloadKind, WorkloadRef};
use temps_workload_netpol::dns::{group_short_name, Alias, NamedWorkload};
use temps_workload_netpol::{
    Action, Direction, DnsPolicy, IdentityMap, L4Protocol, Origin, Peer, PortRange, Profile, Rule,
    WorkloadPolicy,
};
use tokio::net::UdpSocket;

fn record(id: i64, fqdn: &str, ip: &str) -> ZoneRecord {
    ZoneRecord {
        id,
        fqdn: fqdn.into(),
        record_type: "A".into(),
        target_ip: Some(ip.into()),
        target_port: None,
        ttl: 5,
        owner_kind: "service".into(),
        owner_id: id,
        node_id: None,
        generation: 1,
    }
}

fn client_for(server: SocketAddr) -> TokioResolver {
    let mut cfg = ClientResolverConfig::default();
    let mut name_server = NameServerConfig::udp(server.ip());
    if let Some(conn) = name_server.connections.first_mut() {
        conn.port = server.port();
    }
    cfg.add_name_server(name_server);
    let mut opts = ResolverOpts::default();
    opts.use_hosts_file = ResolveHosts::Never;
    opts.attempts = 1;
    opts.timeout = Duration::from_secs(2);
    opts.cache_size = 0;
    TokioResolver::builder_with_config(cfg, TokioRuntimeProvider::default())
        .with_options(opts)
        .build()
        .expect("failed to build test DNS client")
}

fn sandbox() -> WorkloadRef {
    WorkloadRef::new(WorkloadKind::Sandbox, 1)
}

fn linked_db() -> WorkloadRef {
    WorkloadRef::new(WorkloadKind::Service, 10)
}

fn unlinked_cache() -> WorkloadRef {
    WorkloadRef::new(WorkloadKind::Service, 11)
}

fn restricted_snapshot() -> DnsPolicy {
    let policy = WorkloadPolicy {
        subject: sandbox(),
        egress: Profile::Restricted,
        ingress: Profile::Open,
        rules: vec![Rule {
            id: 1,
            direction: Direction::Egress,
            peer: Peer::Workload(linked_db()),
            ports: Some(PortRange::single(5432)),
            protocol: L4Protocol::Tcp,
            action: Action::Allow,
            origin: Origin::Link,
        }],
    };
    let identities = IdentityMap::new().with(sandbox(), IpAddr::V4(Ipv4Addr::LOCALHOST));
    DnsPolicy::build(
        &[policy],
        &identities,
        &[
            NamedWorkload {
                fqdn: "db.shop.temps.local".into(),
                owner: linked_db(),
            },
            NamedWorkload {
                fqdn: "cache.shop.temps.local".into(),
                owner: unlinked_cache(),
            },
        ],
        &[
            Alias {
                alias: group_short_name("db", "shop"),
                canonical: "db.shop.temps.local".into(),
            },
            Alias {
                alias: group_short_name("cache", "shop"),
                canonical: "cache.shop.temps.local".into(),
            },
        ],
    )
}

async fn start(policy: Arc<ArcSwap<DnsPolicy>>) -> SocketAddr {
    let zone = Arc::new(ZoneStore::new(PathBuf::from("/dev/null")));
    zone.replace(
        1,
        vec![
            record(10, "db.shop.temps.local", "10.237.1.10"),
            record(11, "cache.shop.temps.local", "10.237.1.11"),
        ],
    );
    let authority = ZoneAuthority::new(zone).with_policy(policy);
    let socket = UdpSocket::bind("127.0.0.1:0").await.expect("bind");
    let addr = socket.local_addr().expect("local addr");
    let mut server = Server::new(authority);
    server.register_socket(socket);
    tokio::spawn(async move {
        let _ = server.block_until_done().await;
    });
    addr
}

async fn a_records(client: &TokioResolver, name: &str) -> Result<Vec<IpAddr>, String> {
    client
        .lookup(name, RecordType::A)
        .await
        .map(|lookup| {
            lookup
                .answers()
                .iter()
                .filter_map(|r| match &r.data {
                    hickory_proto::rr::RData::A(a) => Some(IpAddr::V4(a.0)),
                    _ => None,
                })
                .collect()
        })
        .map_err(|e| e.to_string())
}

#[tokio::test]
async fn a_restricted_source_resolves_only_what_its_policy_names() {
    let policy = Arc::new(ArcSwap::from_pointee(restricted_snapshot()));
    let addr = start(policy.clone()).await;
    let client = client_for(addr);

    assert_eq!(
        a_records(&client, "db.shop.temps.local.").await,
        Ok(vec!["10.237.1.10".parse().unwrap()]),
        "the linked service resolves"
    );
    assert_eq!(
        a_records(&client, "db.shop.grp.temps.local.").await,
        Ok(vec!["10.237.1.10".parse().unwrap()]),
        "and so does its group short name"
    );
    let unlinked = a_records(&client, "cache.shop.temps.local.").await;
    assert!(
        unlinked.is_err(),
        "an unlinked service is NXDOMAIN: {unlinked:?}"
    );
    let unlinked_alias = a_records(&client, "cache.shop.grp.temps.local.").await;
    assert!(unlinked_alias.is_err(), "{unlinked_alias:?}");

    // No upstream is configured here; with or without one, a restricted
    // source never reaches the forwarding branch (the code returns before
    // it), which the Docker proof also checks on the wire.
    let external = a_records(&client, "example.com.").await;
    let error = external.expect_err("an external name is not resolved");
    assert!(
        error.to_lowercase().contains("nxdomain") || error.contains("no record"),
        "{error}"
    );

    // Open the policy: the same source, the same server.
    policy.store(Arc::new(DnsPolicy::open()));
    assert_eq!(
        a_records(&client, "cache.shop.temps.local.").await,
        Ok(vec!["10.237.1.11".parse().unwrap()]),
        "unrestricted sources keep today's answers"
    );
    let alias_open = a_records(&client, "cache.shop.grp.temps.local.").await;
    assert!(
        alias_open.is_err(),
        "the open snapshot has no aliases: {alias_open:?}"
    );
}
