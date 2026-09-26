// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Exercise worker synchronization over HTTP with no console or plugin registry.
use std::sync::Arc;
use std::time::Duration;

use hickory_resolver::config::{
    NameServerConfig, ResolveHosts, ResolverConfig as ClientConfig, ResolverOpts,
};
use hickory_resolver::net::runtime::TokioRuntimeProvider;
use hickory_resolver::TokioResolver;

use reqwest::StatusCode;
use sea_orm::{ConnectionTrait, DatabaseBackend, Statement};
use sha2::{Digest, Sha256};
use temps_database::test_utils::TestDatabase;
use temps_dns::handlers::dns_sync::DnsChangesResponse;
use temps_dns::services::{DnsRegistry, EndpointDraft, OwnerKind, RecordType};

#[tokio::test]
async fn worker_dns_sync_works_without_console_and_preserves_node_auth() {
    let test_db = match TestDatabase::with_migrations().await {
        Ok(db) => db,
        Err(error) if std::env::var_os("TEMPS_TEST_DATABASE_URL").is_some() => {
            panic!("Explicit DNS test database failed: {error}");
        }
        Err(error) => {
            eprintln!("Postgres unavailable; skipping DNS sync integration: {error}");
            return;
        }
    };
    let db = test_db.connection_arc();
    let mut node_ids = Vec::new();
    for (name, token) in [
        ("dns-worker-a", "fixture-token-a"),
        ("dns-worker-b", "fixture-token-b"),
    ] {
        let hash = hex::encode(Sha256::digest(token.as_bytes()));
        let row = db.query_one(Statement::from_sql_and_values(
            DatabaseBackend::Postgres,
            "INSERT INTO nodes (name, token_hash, address, private_address, role, status, labels, capacity) VALUES ($1,$2,'127.0.0.1','10.0.0.1','worker','active','{}','{}') RETURNING id",
            [name.into(), hash.into()],
        )).await.expect("seed worker").expect("worker id");
        node_ids.push(row.try_get::<i32>("", "id").expect("read worker id"));
    }
    let registry = Arc::new(DnsRegistry::new(db.clone()));
    let draft = |ip: &str| EndpointDraft {
        fqdn: "proxy-independent.temps.local".into(),
        record_type: RecordType::A,
        target_ip: Some(ip.into()),
        target_port: Some(8080),
        ttl: 10,
        owner_kind: OwnerKind::Deployment,
        owner_id: 999,
        node_id: Some(node_ids[0]),
    };
    registry
        .replace_endpoints_for_owner(OwnerKind::Deployment, 999, &[draft("10.0.0.2")])
        .await
        .expect("seed zone");
    let router = temps_dns::proxy_dns_sync_router(db.clone());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind sync API");
    let base = format!("http://{}", listener.local_addr().expect("listen address"));
    let server =
        tokio::spawn(async move { axum::serve(listener, router).await.expect("serve sync API") });
    let client = reqwest::Client::new();
    let changes = format!("{base}/api/internal/nodes/{}/dns/changes", node_ids[0]);
    for token in [None, Some("wrong-token"), Some("fixture-token-b")] {
        let request = client.get(&changes);
        let request = match token {
            Some(token) => request.bearer_auth(token),
            None => request,
        };
        assert_eq!(
            request.send().await.expect("unauthorized request").status(),
            StatusCode::UNAUTHORIZED
        );
    }
    let response = client
        .get(&changes)
        .bearer_auth("fixture-token-a")
        .send()
        .await
        .expect("fetch zone");
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(
        response
            .headers()
            .get("cache-control")
            .expect("private DNS response"),
        "no-store"
    );
    let first: DnsChangesResponse = response.json().await.expect("decode zone");
    // Heartbeat liveness is not credential revocation. With the console down,
    // a worker cannot reactivate itself through the heartbeat endpoint, but
    // its valid node token must still permit independent DNS synchronization.
    db.execute(Statement::from_sql_and_values(
        DatabaseBackend::Postgres,
        "UPDATE nodes SET status='offline' WHERE id=$1",
        [node_ids[0].into()],
    ))
    .await
    .expect("mark worker offline");
    assert_eq!(
        client
            .get(&changes)
            .bearer_auth("fixture-token-a")
            .send()
            .await
            .expect("offline worker sync")
            .status(),
        StatusCode::OK
    );
    assert!(first.full_snapshot);
    assert!(first
        .records
        .iter()
        .any(|r| r.target_ip.as_deref() == Some("10.0.0.2")));
    let ack = format!("{base}/api/internal/nodes/{}/dns/ack", node_ids[0]);
    assert_eq!(
        client
            .post(&changes)
            .bearer_auth("fixture-token-a")
            .send()
            .await
            .expect("wrong method")
            .status(),
        StatusCode::METHOD_NOT_ALLOWED
    );
    assert_eq!(
        client
            .get(format!("{changes}?since=invalid"))
            .bearer_auth("fixture-token-a")
            .send()
            .await
            .expect("invalid generation")
            .status(),
        StatusCode::BAD_REQUEST
    );
    assert_eq!(
        client
            .post(&ack)
            .bearer_auth("fixture-token-a")
            .header("content-type", "application/json")
            .body(" ".repeat(32 * 1024))
            .send()
            .await
            .expect("oversized ack")
            .status(),
        StatusCode::PAYLOAD_TOO_LARGE
    );
    assert_eq!(
        client
            .post(&ack)
            .bearer_auth("fixture-token-a")
            .header("content-type", "application/json")
            .body(format!(
                "{{\"applied_generation\":{}}}",
                first.generation + 100
            ))
            .send()
            .await
            .expect("future ack")
            .status(),
        StatusCode::BAD_REQUEST
    );
    let body = format!("{{\"applied_generation\":{}}}", first.generation);
    assert_eq!(
        client
            .post(&ack)
            .header("content-type", "application/json")
            .body(body.clone())
            .send()
            .await
            .expect("unauthenticated ack")
            .status(),
        StatusCode::UNAUTHORIZED
    );
    assert_eq!(
        client
            .post(&ack)
            .bearer_auth("fixture-token-a")
            .header("content-type", "application/json")
            .body(body)
            .send()
            .await
            .expect("ack")
            .status(),
        StatusCode::OK
    );
    registry
        .replace_endpoints_for_owner(OwnerKind::Deployment, 999, &[draft("10.0.0.3")])
        .await
        .expect("update zone while console absent");
    let updated: DnsChangesResponse = client
        .get(&changes)
        .query(&[("since", first.generation)])
        .bearer_auth("fixture-token-a")
        .send()
        .await
        .expect("fetch updated zone")
        .error_for_status()
        .expect("updated zone status")
        .json()
        .await
        .expect("decode updated zone");
    assert!(updated.generation > first.generation);
    assert!(updated
        .records
        .iter()
        .any(|r| r.target_ip.as_deref() == Some("10.0.0.3")));
    assert!(!updated
        .records
        .iter()
        .any(|r| r.target_ip.as_deref() == Some("10.0.0.2")));
    assert_eq!(
        client
            .get(format!("{base}/api/projects"))
            .send()
            .await
            .expect("unrelated route")
            .status(),
        StatusCode::NOT_FOUND
    );
    // A real worker resolver consumes the proxy-owned API and answers UDP,
    // even though no console process has existed during this test.
    let snapshot_dir =
        std::env::temp_dir().join(format!("temps-proxy-dns-{}", uuid::Uuid::new_v4()));
    let socket = std::net::UdpSocket::bind("127.0.0.1:0").expect("choose DNS port");
    let dns_address = socket.local_addr().expect("DNS port");
    drop(socket);
    let mut config = temps_dns_resolver::ResolverConfig::new(
        node_ids[0],
        "fixture-token-a".into(),
        base,
        dns_address.ip(),
        snapshot_dir.clone(),
    );
    config.listen_addrs = vec![dns_address];
    config.upstream_resolvers.clear();
    config.poll_interval = Duration::from_millis(100);
    let worker = temps_dns_resolver::ResolverHandle::start(config)
        .await
        .expect("start worker DNS");
    assert!(
        temps_dns::probe_control_plane_resolver(dns_address).await,
        "an empty-zone Temps resolver proves readiness before synchronization"
    );
    // The marker is fixed, case-insensitive, uncached, and reserved for TXT.
    for query_type in [
        hickory_proto::rr::RecordType::TXT,
        hickory_proto::rr::RecordType::A,
    ] {
        use hickory_proto::{
            op::{Message, MessageType, OpCode, Query, ResponseCode},
            rr::{Name, RData},
        };
        let socket = tokio::net::UdpSocket::bind("127.0.0.1:0").await.unwrap();
        socket.connect(dns_address).await.unwrap();
        let mut question = Message::new(99, MessageType::Query, OpCode::Query);
        question.add_query(Query::query(
            Name::from_ascii("_TeMpS-ReSoLvEr.temps.local.").unwrap(),
            query_type,
        ));
        socket.send(&question.to_vec().unwrap()).await.unwrap();
        let mut bytes = [0; 512];
        let count = tokio::time::timeout(Duration::from_secs(1), socket.recv(&mut bytes))
            .await
            .unwrap()
            .unwrap();
        let answer = Message::from_vec(&bytes[..count]).unwrap();
        assert!(answer.metadata.authoritative);
        assert_eq!(answer.metadata.response_code, ResponseCode::NoError);
        if query_type == hickory_proto::rr::RecordType::TXT {
            assert_eq!(answer.answers.len(), 1);
            assert_eq!(answer.answers[0].ttl, 0);
            assert!(
                matches!(&answer.answers[0].data, RData::TXT(txt) if txt.txt_data.len() == 1 && txt.txt_data[0].as_ref() == temps_dns_resolver::RESOLVER_MARKER_VALUE.as_bytes())
            );
        } else {
            assert!(answer.answers.is_empty());
        }
    }
    let mut client_config = ClientConfig::default();
    let mut nameserver = NameServerConfig::udp(dns_address.ip());
    nameserver
        .connections
        .first_mut()
        .expect("UDP connection")
        .port = dns_address.port();
    client_config.add_name_server(nameserver);
    let mut options = ResolverOpts::default();
    options.cache_size = 0;
    options.use_hosts_file = ResolveHosts::Never;
    options.attempts = 1;
    options.timeout = Duration::from_secs(1);
    let dns = TokioResolver::builder_with_config(client_config, TokioRuntimeProvider::default())
        .with_options(options)
        .build()
        .expect("DNS client");
    tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            if let Ok(answer) = dns.lookup_ip("proxy-independent.temps.local.").await {
                if answer.iter().any(|ip| ip.to_string() == "10.0.0.3") {
                    break;
                }
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    })
    .await
    .expect("worker resolves live zone without console");
    server.abort();
    let answer = dns
        .lookup_ip("proxy-independent.temps.local.")
        .await
        .expect("serve retained zone after sync API loss");
    assert!(answer.iter().any(|ip| ip.to_string() == "10.0.0.3"));
    worker.shutdown().await;
    assert!(
        !temps_dns::probe_control_plane_resolver(dns_address).await,
        "stopped resolver must not remain eligible for container DNS injection"
    );
    std::fs::remove_dir_all(snapshot_dir).expect("remove DNS fixture snapshot");
}

#[tokio::test]
async fn arbitrary_tcp_or_udp_listener_does_not_prove_temps_dns_readiness() {
    let tcp = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind fake TCP service");
    let address = tcp.local_addr().expect("fake service address");
    assert!(
        !temps_dns::probe_control_plane_resolver(address).await,
        "a TCP listener alone cannot justify DNS injection"
    );
    let udp = tokio::net::UdpSocket::bind(address)
        .await
        .expect("bind fake UDP service");
    let fake = tokio::spawn(async move {
        let mut buffer = [0; 512];
        let (length, peer) = udp.recv_from(&mut buffer).await.expect("receive DNS query");
        udp.send_to(&buffer[..length], peer)
            .await
            .expect("echo query without authoritative answer");
    });
    assert!(
        !temps_dns::probe_control_plane_resolver(address).await,
        "echoing a matching DNS question is not an authoritative readiness response"
    );
    fake.await.expect("fake service task");
}

#[tokio::test]
async fn readiness_rejects_mismatched_or_non_authoritative_dns_answers() {
    use hickory_proto::{
        op::{Message, MessageType, ResponseCode},
        rr::{rdata::TXT, Name, RData, Record},
    };
    for invalid in [
        "id",
        "name",
        "value",
        "authority",
        "rcode",
        "truncated",
        "question",
        "extra",
    ] {
        let socket = tokio::net::UdpSocket::bind("127.0.0.1:0").await.unwrap();
        let address = socket.local_addr().unwrap();
        let fake = tokio::spawn(async move {
            let mut bytes = [0; 512];
            let (size, peer) = socket.recv_from(&mut bytes).await.unwrap();
            let mut response = Message::from_vec(&bytes[..size]).unwrap();
            response.metadata.message_type = MessageType::Response;
            response.metadata.authoritative = true;
            let name = Name::from_ascii(if invalid == "name" {
                "unrelated.temps.local"
            } else {
                temps_dns_resolver::RESOLVER_MARKER_NAME
            })
            .unwrap();
            let value = if invalid == "value" {
                "unrelated-service"
            } else {
                temps_dns_resolver::RESOLVER_MARKER_VALUE
            };
            response.add_answer(Record::from_rdata(
                name,
                0,
                RData::TXT(TXT::new(vec![value.into()])),
            ));
            match invalid {
                "id" => response.metadata.id = response.metadata.id.wrapping_add(1),
                "authority" => response.metadata.authoritative = false,
                "rcode" => response.metadata.response_code = ResponseCode::ServFail,
                "truncated" => response.metadata.truncation = true,
                "question" => response.queries.clear(),
                "extra" => response.answers.push(response.answers[0].clone()),
                _ => {}
            }
            socket
                .send_to(&response.to_vec().unwrap(), peer)
                .await
                .unwrap();
        });
        assert!(
            !temps_dns::probe_control_plane_resolver(address).await,
            "accepted invalid {invalid} response"
        );
        fake.await.unwrap();
    }
}
