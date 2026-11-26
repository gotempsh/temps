// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

use super::*;
use temps_workload::WorkloadKind;

fn sandbox(id: i64) -> WorkloadRef {
    WorkloadRef::new(WorkloadKind::Sandbox, id)
}

fn host_rule(id: i64, pattern: &str, action: Action) -> EgressRule {
    EgressRule {
        id,
        target: EgressTarget::Host(HostPattern::parse(pattern).unwrap()),
        ports: None,
        action,
    }
}

fn cidr_rule(id: i64, cidr: &str, port: Option<u16>, action: Action) -> EgressRule {
    EgressRule {
        id,
        target: EgressTarget::Cidr(cidr.parse().unwrap()),
        ports: port.map(PortRange::single),
        action,
    }
}

#[test]
fn host_patterns_match_exact_names_subdomains_and_everything() {
    let exact = HostPattern::parse("API.Example.com.").unwrap();
    assert!(exact.matches("api.example.com"));
    assert!(!exact.matches("x.api.example.com"));
    let sub = HostPattern::parse("*.example.com").unwrap();
    assert!(sub.matches("a.example.com"));
    assert!(sub.matches("a.b.example.com"));
    assert!(!sub.matches("example.com"), "not the apex");
    assert!(!sub.matches("badexample.com"));
    assert!(HostPattern::parse("*").unwrap().matches("anything.test"));
    for bad in ["", "*.", "a..b", "exa mple.com", "*.*.com", "-a.com", "a/b"] {
        assert!(HostPattern::parse(bad).is_err(), "{bad}");
    }
}

#[test]
fn deny_beats_allow_and_ports_default_to_web() {
    let rules = CompiledRules::new(&EgressPolicy {
        subject: sandbox(1),
        rules: vec![
            host_rule(1, "*", Action::Allow),
            host_rule(2, "*.blocked.test", Action::Deny),
        ],
        record: false,
        public_only: false,
    });
    assert_eq!(
        rules.check_host("registry.npmjs.org", 443),
        Ok(Admission::ByRule)
    );
    assert_eq!(
        rules.check_host("registry.npmjs.org", 80),
        Ok(Admission::ByRule)
    );
    assert_eq!(
        rules.check_host("registry.npmjs.org", 22),
        Err(Refusal::Port)
    );
    assert_eq!(
        rules.check_host("x.blocked.test", 443),
        Err(Refusal::Denied)
    );
}

#[test]
fn a_restricted_workload_without_rules_reaches_nothing() {
    let rules = CompiledRules::new(&EgressPolicy {
        subject: sandbox(1),
        rules: vec![],
        record: false,
        public_only: false,
    });
    assert_eq!(
        rules.check_host("example.com", 443),
        Err(Refusal::NotAllowed)
    );
    assert_eq!(
        rules.check_host("192.0.2.10", 443),
        Err(Refusal::NotAllowed)
    );
}

#[test]
fn private_answers_are_refused_unless_a_cidr_allows_them() {
    let rules = CompiledRules::new(&EgressPolicy {
        subject: sandbox(1),
        rules: vec![
            host_rule(1, "*", Action::Allow),
            cidr_rule(2, "10.237.9.0/24", Some(8080), Action::Allow),
        ],
        record: false,
        public_only: false,
    });
    let public: IpAddr = "93.184.216.34".parse().unwrap();
    let private: IpAddr = "10.0.0.5".parse().unwrap();
    assert_eq!(
        rules.check_addresses(&[public], 443, Admission::ByRule),
        Ok(())
    );
    assert_eq!(
        rules.check_addresses(&[public, private], 443, Admission::ByRule),
        Err(Refusal::Private),
        "one private answer refuses the whole name (DNS rebinding)"
    );
    assert_eq!(
        rules.check_addresses(&[], 443, Admission::ByRule),
        Err(Refusal::Unresolved)
    );
    let allowed: IpAddr = "10.237.9.4".parse().unwrap();
    assert_eq!(rules.check_host("10.237.9.4", 8080), Ok(Admission::ByRule));
    assert_eq!(
        rules.check_addresses(&[allowed], 8080, Admission::ByRule),
        Ok(())
    );
    assert_eq!(
        rules.check_addresses(&[allowed], 9090, Admission::ByRule),
        Err(Refusal::Private),
        "the CIDR allow names one port"
    );
}

#[test]
fn an_unrelated_cidr_allow_does_not_open_public_literals() {
    let rules = CompiledRules::new(&EgressPolicy {
        subject: sandbox(1),
        rules: vec![cidr_rule(1, "10.20.0.0/16", Some(443), Action::Allow)],
        record: false,
        public_only: false,
    });
    assert_eq!(
        rules.check_host("93.184.216.34", 22),
        Err(Refusal::NotAllowed),
        "a literal outside every allowed CIDR is refused, whatever the port"
    );
    assert_eq!(
        rules.check_host("93.184.216.34", 443),
        Err(Refusal::NotAllowed)
    );
    assert_eq!(
        rules.check_host("10.20.3.4", 22),
        Err(Refusal::Port),
        "the CIDR allow names one port"
    );
    assert_eq!(rules.check_host("10.20.3.4", 443), Ok(Admission::ByRule));

    // A hostname with no host rule is decided by its addresses, and only
    // an allowed CIDR admits them: a public answer is not enough.
    assert_eq!(
        rules.check_host("internal.example", 443),
        Ok(Admission::ByAddress)
    );
    let public: IpAddr = "93.184.216.34".parse().unwrap();
    let inside: IpAddr = "10.20.3.4".parse().unwrap();
    assert_eq!(
        rules.check_addresses(&[public], 443, Admission::ByAddress),
        Err(Refusal::NotAllowed)
    );
    assert_eq!(
        rules.check_addresses(&[inside], 443, Admission::ByAddress),
        Ok(())
    );
    assert_eq!(
        rules.check_addresses(&[inside], 22, Admission::ByAddress),
        Err(Refusal::NotAllowed)
    );
    assert_eq!(
        rules.admissible_addresses(&[public, inside], 443, Admission::ByAddress),
        Ok(vec![inside]),
        "only the answer inside an allowed CIDR is connected to, as the sidecar does"
    );
    assert_eq!(
        rules.admissible_addresses(&[public], 443, Admission::ByAddress),
        Err(Refusal::NotAllowed),
        "no answer inside an allowed CIDR refuses the name"
    );
}

/// A sandbox's own policy (public addresses only) with a CIDR rule, where
/// the name resolves to two public addresses and only one is inside it: the
/// sidecar connects to that one, and so must the proxy that records it.
#[test]
fn recording_admits_what_the_sidecar_admits_for_a_cidr_rule() {
    let rules = CompiledRules::new(&EgressPolicy {
        subject: sandbox(1),
        rules: vec![cidr_rule(1, "93.184.216.0/24", Some(443), Action::Allow)],
        record: true,
        public_only: true,
    });
    let inside: IpAddr = "93.184.216.34".parse().unwrap();
    let outside: IpAddr = "8.8.8.8".parse().unwrap();
    let admission = rules.check_host("svc.example.net", 443).unwrap();
    assert_eq!(admission, Admission::ByAddress);
    for answers in [[inside, outside], [outside, inside]] {
        assert_eq!(
            rules.admissible_addresses(&answers, 443, admission),
            Ok(vec![inside]),
            "{answers:?}"
        );
    }
    // One private answer still refuses the whole name.
    assert_eq!(
        rules.admissible_addresses(&[inside, "10.0.0.5".parse().unwrap()], 443, admission),
        Err(Refusal::Private)
    );
}

#[test]
fn a_cidr_deny_refuses_public_addresses_too() {
    let rules = CompiledRules::new(&EgressPolicy {
        subject: sandbox(1),
        rules: vec![
            host_rule(1, "*", Action::Allow),
            cidr_rule(2, "93.184.216.0/24", None, Action::Deny),
        ],
        record: false,
        public_only: false,
    });
    assert_eq!(
        rules.check_addresses(&["93.184.216.34".parse().unwrap()], 443, Admission::ByRule),
        Err(Refusal::Denied)
    );
}

#[test]
fn the_public_check_matches_the_sidecar() {
    for private in [
        "0.1.2.3",
        "10.1.1.1",
        "127.0.0.1",
        "100.64.0.1",
        "169.254.169.254",
        "172.16.0.1",
        "172.31.255.255",
        "192.0.0.1",
        "192.0.2.1",
        "192.88.99.1",
        "192.168.1.1",
        "198.18.0.1",
        "198.51.100.1",
        "203.0.113.1",
        "224.0.0.1",
        "255.255.255.255",
        "::1",
        "fe80::1",
        "fd00::1",
        "2001:db8::1",
        "2001:2::1",
        "::ffff:10.0.0.1",
    ] {
        assert!(!is_public(private.parse().unwrap()), "{private}");
    }
    for public in [
        "1.1.1.1",
        "172.32.0.1",
        "100.128.0.1",
        "2606:4700::1111",
        "::ffff:1.1.1.1",
    ] {
        assert!(is_public(public.parse().unwrap()), "{public}");
    }
}

/// The authority of an absolute URL ends at the first `/`, `?` or `#`: a
/// query on a host-only URL is a query, forwarded as the request's own and
/// never read as part of the host, which is what gets recorded.
#[test]
fn a_query_on_a_host_only_url_is_never_part_of_the_host() {
    for (target, path) in [
        ("http://example.test?token=abc123", "/?token=abc123"),
        ("http://example.test:8080?token=abc123", "/?token=abc123"),
        ("http://example.test#frag", "/"),
        ("http://example.test/a?b=1#frag", "/a?b=1"),
        ("http://example.test", "/"),
    ] {
        let head = format!("GET {target} HTTP/1.1\r\nHost: example.test\r\n\r\n");
        let parsed = parse_request(head.as_bytes()).unwrap();
        let RequestKind::Forward {
            host, path: got, ..
        } = parsed.kind
        else {
            panic!("{target}: {:?}", parsed.kind);
        };
        assert_eq!(host, "example.test", "{target}");
        assert_eq!(got, path, "{target}");
        assert!(!record::recorded_path(&got).contains("token"), "{target}");
    }
    // Nothing that is not a hostname or an address is ever a host.
    for target in [
        "http://exa mple.test/",
        "http://example.test;x=1/",
        "http://exa%41mple/",
    ] {
        let head = format!("GET {target} HTTP/1.1\r\n\r\n");
        let parsed = parse_request(head.as_bytes());
        assert!(
            !matches!(parsed.map(|p| p.kind), Some(RequestKind::Forward { .. })),
            "{target}"
        );
    }
}

#[test]
fn requests_parse_into_tunnels_forwards_and_relays() {
    let connect =
        parse_request(b"CONNECT Example.com:443 HTTP/1.1\r\nHost: example.com:443\r\n\r\n")
            .unwrap();
    assert_eq!(
        connect.kind,
        RequestKind::Connect {
            host: "example.com".into(),
            port: 443
        }
    );
    let forward = parse_request(
        b"GET http://example.com/a?b=1 HTTP/1.1\r\nHost: example.com\r\nProxy-Authorization: x\r\nAccept: */*\r\n\r\n",
    )
    .unwrap();
    assert_eq!(
        forward.kind,
        RequestKind::Forward {
            host: "example.com".into(),
            port: 80,
            path: "/a?b=1".into()
        }
    );
    assert_eq!(
        forward.headers,
        vec![("Accept".to_string(), b"*/*".to_vec())]
    );
    let v6 = parse_request(b"CONNECT [2606:4700::1111]:443 HTTP/1.1\r\n\r\n").unwrap();
    assert_eq!(
        v6.kind,
        RequestKind::Connect {
            host: "2606:4700::1111".into(),
            port: 443
        }
    );
    assert_eq!(
        parse_request(b"GET http://user:pw@example.com/ HTTP/1.1\r\n\r\n")
            .unwrap()
            .kind,
        RequestKind::Refused(EgressReason::CredentialsInUrl)
    );
    assert_eq!(
        parse_request(b"GET /etc/passwd HTTP/1.1\r\n\r\n")
            .unwrap()
            .kind,
        RequestKind::Refused(EgressReason::NotAProxyRequest)
    );
    assert!(parse_request(b"not http\r\n\r\n").is_none());
}

#[test]
fn relay_paths_match_the_sidecar() {
    let token = "0123456789abcdef0123456789abcdef";
    assert_eq!(
        relay_target(
            "POST",
            &format!("/.temps/model-relay/{token}/v1/messages?beta=true")
        ),
        Some(format!(
            "/api/ai/sandbox-models/{token}/v1/messages?beta=true"
        ))
    );
    assert_eq!(
        relay_target("POST", &format!("/.temps/model-relay/{token}/responses")),
        Some(format!("/api/ai/sandbox-models/{token}/responses"))
    );
    assert_eq!(
        relay_target(
            "GET",
            &format!("/.temps/model-relay/{token}/models?client_version=1.2.3")
        ),
        Some(format!(
            "/api/ai/sandbox-models/{token}/models?client_version=1.2.3"
        ))
    );
    assert_eq!(
        relay_target("POST", &format!("/.temps/mcp/{token}")),
        Some(format!("/api/ai/sandbox-tools/{token}/mcp"))
    );
    for refused in [
        format!("/.temps/model-relay/{token}/responses?x=1"),
        "/.temps/model-relay/short/responses".to_string(),
        format!("/.temps/git/{token}/1/info/refs?service=git-upload-pack"),
        format!("/.temps/mcp/{token}?x"),
    ] {
        assert_eq!(relay_target("POST", &refused), None, "{refused}");
    }
    assert_eq!(
        relay_target("POST", &format!("/.temps/model-relay/{token}/models")),
        None,
        "the model list is GET only"
    );
    assert_eq!(
        relay_target(
            "GET",
            &format!("/.temps/model-relay/{token}/models?client_version=a b")
        ),
        None
    );
}

#[test]
fn relay_upstreams_are_plain_http_origins() {
    assert_eq!(
        RelayUpstream::parse("http://127.0.0.1:3000/").unwrap(),
        RelayUpstream {
            authority: "127.0.0.1:3000".into(),
            base_path: String::new()
        }
    );
    assert_eq!(
        RelayUpstream::parse("http://console.internal/base").unwrap(),
        RelayUpstream {
            authority: "console.internal:80".into(),
            base_path: "/base".into()
        }
    );
    assert!(RelayUpstream::parse("https://cp.example.test").is_err());
    assert!(RelayUpstream::parse("http://u:p@cp.example.test").is_err());
}

#[test]
fn the_pool_never_allocates_past_its_capacity() {
    let pool = BufferPool::new(2, 16);
    let a = pool.get().unwrap();
    let b = pool.get().unwrap();
    assert!(pool.get().is_none());
    drop(a);
    let c = pool.get().unwrap();
    assert_eq!(c.len(), 16);
    assert_eq!(pool.allocated(), 2);
    drop((b, c));
    assert_eq!(pool.allocated(), 2);
}

#[test]
fn heads_are_rewritten_to_origin_form_and_closed() {
    let request = parse_request(b"POST http://example.com/x HTTP/1.1\r\nHost: example.com\r\nConnection: keep-alive\r\nContent-Length: 2\r\n\r\n").unwrap();
    let mut out = [0u8; 256];
    let len = rewrite_head(&request, "/x", "example.com", &mut out).unwrap();
    assert_eq!(
        std::str::from_utf8(&out[..len]).unwrap(),
        "POST /x HTTP/1.1\r\nHost: example.com\r\nContent-Length: 2\r\nConnection: close\r\n\r\n"
    );
    let mut tiny = [0u8; 8];
    assert!(rewrite_head(&request, "/x", "example.com", &mut tiny).is_none());
}

#[test]
fn limits_are_validated() {
    let mut config = EgressProxyConfig::new("127.0.0.1:0".parse().unwrap());
    assert!(config.validate().is_ok());
    config.max_connections = 0;
    assert!(config.validate().is_err());
    config.max_connections = 4;
    config.buffer_size = 100;
    assert!(config.validate().is_err());
}

async fn echo_server() -> SocketAddr {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        while let Ok((mut stream, _)) = listener.accept().await {
            tokio::spawn(async move {
                let mut buf = [0u8; 1024];
                while let Ok(n) = stream.read(&mut buf).await {
                    if n == 0 || stream.write_all(&buf[..n]).await.is_err() {
                        break;
                    }
                }
            });
        }
    });
    addr
}

async fn start(rules: Vec<EgressRule>, max_connections: usize) -> (Arc<EgressProxy>, SocketAddr) {
    let identities = IdentityMap::new().with(sandbox(1), "127.0.0.1".parse().unwrap());
    let table = EgressTable::build(
        &[EgressPolicy {
            subject: sandbox(1),
            rules,
            record: false,
            public_only: false,
        }],
        &identities,
    );
    let mut config = EgressProxyConfig::new("127.0.0.1:0".parse().unwrap());
    config.max_connections = max_connections;
    config.head_timeout = Duration::from_secs(5);
    let proxy = EgressProxy::new(config, Arc::new(ArcSwap::from_pointee(table))).unwrap();
    let listener = proxy.bind().await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(proxy.clone().serve(listener));
    (proxy, addr)
}

async fn status_line(stream: &mut TcpStream) -> String {
    let mut buf = vec![0u8; 512];
    let mut filled = 0;
    loop {
        let n = stream.read(&mut buf[filled..]).await.unwrap_or(0);
        filled += n;
        if n == 0 || find_head_end(&buf[..filled]).is_some() {
            break;
        }
    }
    String::from_utf8_lossy(&buf[..filled])
        .lines()
        .next()
        .unwrap_or_default()
        .to_string()
}

#[tokio::test]
async fn a_cidr_allowed_tunnel_carries_bytes_both_ways() {
    let echo = echo_server().await;
    let (proxy, addr) = start(
        vec![cidr_rule(
            1,
            "127.0.0.1/32",
            Some(echo.port()),
            Action::Allow,
        )],
        4,
    )
    .await;
    let mut client = TcpStream::connect(addr).await.unwrap();
    client
        .write_all(format!("CONNECT 127.0.0.1:{} HTTP/1.1\r\n\r\n", echo.port()).as_bytes())
        .await
        .unwrap();
    assert_eq!(
        status_line(&mut client).await,
        "HTTP/1.1 200 Connection Established"
    );
    client.write_all(b"ping").await.unwrap();
    let mut got = [0u8; 4];
    client.read_exact(&mut got).await.unwrap();
    assert_eq!(&got, b"ping");
    assert_eq!(proxy.stats().snapshot().tunnels, 1);
}

#[tokio::test]
async fn names_resolving_to_private_addresses_are_refused() {
    let echo = echo_server().await;
    let (proxy, addr) = start(
        vec![EgressRule {
            id: 1,
            target: EgressTarget::Host(HostPattern::parse("*").unwrap()),
            ports: Some(PortRange::single(echo.port())),
            action: Action::Allow,
        }],
        4,
    )
    .await;
    let mut client = TcpStream::connect(addr).await.unwrap();
    client
        .write_all(format!("CONNECT localhost:{} HTTP/1.1\r\n\r\n", echo.port()).as_bytes())
        .await
        .unwrap();
    assert_eq!(status_line(&mut client).await, "HTTP/1.1 403 Forbidden");
    assert_eq!(proxy.stats().snapshot().refused_private, 1);
}

#[tokio::test]
async fn an_unknown_source_is_refused() {
    let echo = echo_server().await;
    let mut config = EgressProxyConfig::new("127.0.0.1:0".parse().unwrap());
    config.max_connections = 2;
    let proxy = EgressProxy::new(
        config,
        Arc::new(ArcSwap::from_pointee(EgressTable::default())),
    )
    .unwrap();
    let listener = proxy.bind().await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(proxy.clone().serve(listener));
    let mut client = TcpStream::connect(addr).await.unwrap();
    client
        .write_all(format!("CONNECT 127.0.0.1:{} HTTP/1.1\r\n\r\n", echo.port()).as_bytes())
        .await
        .unwrap();
    assert_eq!(status_line(&mut client).await, "HTTP/1.1 403 Forbidden");
    assert_eq!(proxy.stats().snapshot().refused_unknown_source, 1);
}

/// Learns of sandbox 1 at 127.0.0.1 when asked, as a recorder that has just
/// listed a new workload would.
struct Learns {
    table: Arc<ArcSwap<EgressTable>>,
    asked: std::sync::atomic::AtomicUsize,
    rules: Vec<EgressRule>,
}

#[async_trait::async_trait]
impl UnknownSource for Learns {
    async fn unknown_source(&self, source: IpAddr) {
        self.asked.fetch_add(1, Ordering::Relaxed);
        let identities = IdentityMap::new().with(sandbox(1), source);
        self.table.store(Arc::new(EgressTable::build(
            &[EgressPolicy {
                subject: sandbox(1),
                rules: self.rules.clone(),
                record: false,
                public_only: false,
            }],
            &identities,
        )));
    }
}

#[tokio::test]
async fn an_unknown_source_is_admitted_once_the_handler_learns_of_it() {
    let echo = echo_server().await;
    let mut config = EgressProxyConfig::new("127.0.0.1:0".parse().unwrap());
    config.max_connections = 2;
    let table = Arc::new(ArcSwap::from_pointee(EgressTable::default()));
    let proxy = EgressProxy::new(config, table.clone()).unwrap();
    let learns = Arc::new(Learns {
        table,
        asked: std::sync::atomic::AtomicUsize::new(0),
        rules: vec![cidr_rule(
            1,
            "127.0.0.0/8",
            Some(echo.port()),
            Action::Allow,
        )],
    });
    proxy.set_unknown_source(learns.clone());
    let listener = proxy.bind().await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(proxy.clone().serve(listener));
    let mut client = TcpStream::connect(addr).await.unwrap();
    client
        .write_all(format!("CONNECT 127.0.0.1:{} HTTP/1.1\r\n\r\n", echo.port()).as_bytes())
        .await
        .unwrap();
    assert_eq!(
        status_line(&mut client).await,
        "HTTP/1.1 200 Connection Established"
    );
    assert_eq!(learns.asked.load(Ordering::Relaxed), 1);
    assert_eq!(proxy.stats().snapshot().refused_unknown_source, 0);
}

/// Never answers in time: a slow container list.
struct Stalls;

#[async_trait::async_trait]
impl UnknownSource for Stalls {
    async fn unknown_source(&self, _source: IpAddr) {
        tokio::time::sleep(Duration::from_secs(30)).await;
    }
}

/// A burst of unknown callers waiting for discovery cannot take the slots
/// a known workload needs: a waiting caller holds a discovery slot, not a
/// connection slot, and past the discovery slots the rest are told to retry
/// (503, as at the connection limit), never refused as unknown: discovery
/// might have found them.
#[tokio::test]
async fn unknown_callers_waiting_for_discovery_leave_slots_for_known_workloads() {
    let echo = echo_server().await;
    // One connection slot, one discovery slot.
    let (proxy, addr) = start(
        vec![cidr_rule(
            1,
            "127.0.0.1/32",
            Some(echo.port()),
            Action::Allow,
        )],
        1,
    )
    .await;
    proxy.set_unknown_source(Arc::new(Stalls));
    let unknown_peer = || async {
        let socket = tokio::net::TcpSocket::new_v4().unwrap();
        socket.bind("127.0.0.2:0".parse().unwrap()).unwrap();
        socket.connect(addr).await.unwrap()
    };
    let waiting = unknown_peer().await;
    for _ in 0..50 {
        if proxy.stats().snapshot().active == 1 {
            break;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    tokio::time::sleep(Duration::from_millis(50)).await;
    // The discovery slot is taken: told to retry, at once.
    let mut over = unknown_peer().await;
    let status = tokio::time::timeout(Duration::from_secs(1), status_line(&mut over))
        .await
        .expect("answered at once, not after waiting for discovery");
    assert_eq!(status, "HTTP/1.1 503 Service Unavailable");
    assert_eq!(proxy.stats().snapshot().refused_unknown_source, 0);
    // The only connection slot is free for a known workload meanwhile.
    let mut known = TcpStream::connect(addr).await.unwrap();
    known
        .write_all(format!("CONNECT 127.0.0.1:{} HTTP/1.1\r\n\r\n", echo.port()).as_bytes())
        .await
        .unwrap();
    assert_eq!(
        tokio::time::timeout(Duration::from_secs(1), status_line(&mut known))
            .await
            .expect("served while the unknown caller waits"),
        "HTTP/1.1 200 Connection Established"
    );
    drop(waiting);
}

/// With connection slots to spare but every discovery slot taken, a caller
/// the table does not know is told to retry, not refused as unknown.
#[tokio::test]
async fn a_caller_past_the_discovery_slots_is_told_to_retry() {
    let (proxy, addr) = start(vec![], 4).await;
    proxy.set_unknown_source(Arc::new(Stalls));
    let unknown_peer = || async {
        let socket = tokio::net::TcpSocket::new_v4().unwrap();
        socket.bind("127.0.0.2:0".parse().unwrap()).unwrap();
        socket.connect(addr).await.unwrap()
    };
    let waiting = unknown_peer().await;
    for _ in 0..50 {
        if proxy.stats().snapshot().active == 1 {
            break;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    tokio::time::sleep(Duration::from_millis(50)).await;
    let mut over = unknown_peer().await;
    assert_eq!(
        status_line(&mut over).await,
        "HTTP/1.1 503 Service Unavailable"
    );
    let stats = proxy.stats().snapshot();
    assert_eq!(stats.refused_unknown_source, 0);
    assert_eq!(stats.refused_saturated, 1);
    drop(waiting);
}

#[tokio::test]
async fn at_the_connection_limit_new_connections_get_503_and_are_counted() {
    let (proxy, addr) = start(vec![], 1).await;
    // Holds the only permit: connected, head not sent yet.
    let _held = TcpStream::connect(addr).await.unwrap();
    for _ in 0..50 {
        if proxy.stats().snapshot().active == 1 {
            break;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    let mut refused = TcpStream::connect(addr).await.unwrap();
    assert_eq!(
        status_line(&mut refused).await,
        "HTTP/1.1 503 Service Unavailable"
    );
    let stats = proxy.stats().snapshot();
    assert_eq!(stats.refused_saturated, 1);
    assert_eq!(stats.active, 1);
    assert!(
        proxy.buffers_allocated() <= 2,
        "buffers stay within the pool"
    );
}

// ── Recording ───────────────────────────────────────────────────────

const TOKEN: &str = "0123456789abcdef0123456789abcdef";

/// A proxy whose only workload (sandbox 1, at 127.0.0.1) has `rules` and
/// `record`, recording into a channel of `capacity`.
async fn start_recording(
    rules: Vec<EgressRule>,
    record: bool,
    capacity: usize,
    configure: impl FnOnce(&mut EgressProxyConfig),
) -> (Arc<EgressProxy>, SocketAddr, EgressRecordStream) {
    let identities = IdentityMap::new().with(sandbox(1), "127.0.0.1".parse().unwrap());
    let table = EgressTable::build(
        &[EgressPolicy {
            subject: sandbox(1),
            rules,
            record,
            public_only: false,
        }],
        &identities,
    );
    let mut config = EgressProxyConfig::new("127.0.0.1:0".parse().unwrap());
    config.max_connections = 8;
    config.head_timeout = Duration::from_secs(5);
    config.connect_timeout = Duration::from_secs(2);
    configure(&mut config);
    let (recorder, stream) = recording_channel(capacity);
    let proxy = EgressProxy::with_recorder(
        config,
        Arc::new(ArcSwap::from_pointee(table)),
        Some(recorder),
    )
    .unwrap();
    let listener = proxy.bind().await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(proxy.clone().serve(listener));
    (proxy, addr, stream)
}

async fn next_record(stream: &mut EgressRecordStream) -> ConnectionRecord {
    tokio::time::timeout(Duration::from_secs(5), stream.recv())
        .await
        .expect("a record within 5 s")
        .expect("the channel is open")
}

/// Sends `head`, reads the response head, then (for a tunnel) a ping
/// through it, and closes.
async fn exchange(addr: SocketAddr, head: &str, ping: bool) -> String {
    let mut client = TcpStream::connect(addr).await.unwrap();
    client.write_all(head.as_bytes()).await.unwrap();
    let status = status_line(&mut client).await;
    if ping {
        client.write_all(b"ping").await.unwrap();
        let mut got = [0u8; 4];
        client.read_exact(&mut got).await.unwrap();
        assert_eq!(&got, b"ping");
    }
    let _ = client.shutdown().await;
    let mut rest = Vec::new();
    let _ = tokio::time::timeout(Duration::from_secs(2), client.read_to_end(&mut rest)).await;
    status
}

fn echo_rule(port: u16) -> EgressRule {
    cidr_rule(1, "127.0.0.1/32", Some(port), Action::Allow)
}

#[tokio::test]
async fn an_allowed_tunnel_is_recorded_with_its_destination_and_bytes() {
    let echo = echo_server().await;
    let (proxy, addr, mut records) =
        start_recording(vec![echo_rule(echo.port())], true, 16, |_| {}).await;
    let status = exchange(
        addr,
        &format!("CONNECT 127.0.0.1:{} HTTP/1.1\r\n\r\n", echo.port()),
        true,
    )
    .await;
    assert_eq!(status, "HTTP/1.1 200 Connection Established");
    let record = next_record(&mut records).await;
    assert_eq!(record.workload, sandbox(1));
    assert_eq!(record.kind, ConnectionKind::Tunnel);
    assert_eq!(record.outcome, ConnectionOutcome::Allowed);
    assert_eq!(record.reason, None);
    assert_eq!(record.host.as_deref(), Some("127.0.0.1"));
    assert_eq!(record.port, Some(echo.port()));
    assert_eq!(record.peer, Some("127.0.0.1".parse().unwrap()));
    assert_eq!((record.bytes_sent, record.bytes_received), (4, 4));
    assert_eq!(record.count, 1);
    assert!(record.started_at_ms > 0);
    assert_eq!(
        record.method, None,
        "a tunnel's method is CONNECT, not the request's"
    );
    assert_eq!(proxy.stats().snapshot().records_emitted, 1);
}

#[tokio::test]
async fn a_forward_is_recorded_with_method_and_path_but_never_the_query() {
    let echo = echo_server().await;
    let (_proxy, addr, mut records) =
        start_recording(vec![echo_rule(echo.port())], true, 16, |_| {}).await;
    let mut client = TcpStream::connect(addr).await.unwrap();
    client
        .write_all(
            format!(
                "GET http://127.0.0.1:{}/v1/items?api_key=secret HTTP/1.1\r\nHost: x\r\nAuthorization: Bearer secret\r\n\r\n",
                echo.port()
            )
            .as_bytes(),
        )
        .await
        .unwrap();
    // The echo server returns the rewritten request; read it and close.
    let mut seen = Vec::new();
    let mut buf = [0u8; 512];
    while !seen.windows(4).any(|w| w == b"\r\n\r\n") {
        let n = client.read(&mut buf).await.unwrap();
        assert!(n > 0);
        seen.extend_from_slice(&buf[..n]);
    }
    client.shutdown().await.unwrap();
    let record = next_record(&mut records).await;
    assert_eq!(record.kind, ConnectionKind::Forward);
    assert_eq!(record.outcome, ConnectionOutcome::Allowed);
    assert_eq!(record.method.as_deref(), Some("GET"));
    assert_eq!(record.path.as_deref(), Some("/v1/items"));
    assert_eq!(record.port, Some(echo.port()));
    assert!(record.bytes_received > 0);
    assert!(record.bytes_sent > 0, "the forwarded head counts as sent");
    assert_eq!(
        record.bytes_sent, record.bytes_received,
        "the echo server returns exactly what was forwarded"
    );
    let json = serde_json::to_string(&record).unwrap();
    assert!(
        !json.contains("secret"),
        "no query string or header reaches a record: {json}"
    );
}

#[tokio::test]
async fn a_relay_is_recorded_by_kind_without_its_token() {
    let echo = echo_server().await;
    let (_proxy, addr, mut records) = start_recording(vec![], true, 16, |config| {
        config.relay = Some(RelayUpstream::parse(&format!("http://{echo}")).unwrap());
    })
    .await;
    let mut client = TcpStream::connect(addr).await.unwrap();
    client
        .write_all(
            format!("POST /.temps/mcp/{TOKEN} HTTP/1.1\r\nContent-Length: 0\r\n\r\n").as_bytes(),
        )
        .await
        .unwrap();
    let mut buf = [0u8; 512];
    let n = client.read(&mut buf).await.unwrap();
    assert!(n > 0);
    client.shutdown().await.unwrap();
    let record = next_record(&mut records).await;
    assert_eq!(record.kind, ConnectionKind::Relay);
    assert_eq!(record.relay, Some(RelayKind::Mcp));
    assert_eq!(record.outcome, ConnectionOutcome::Allowed);
    assert_eq!(record.path, None);
    assert!(!serde_json::to_string(&record).unwrap().contains(TOKEN));
}

#[tokio::test]
async fn a_relay_without_an_upstream_is_recorded_as_refused() {
    let (_proxy, addr, mut records) = start_recording(vec![], true, 16, |_| {}).await;
    let status = exchange(
        addr,
        &format!("POST /.temps/mcp/{TOKEN} HTTP/1.1\r\n\r\n"),
        false,
    )
    .await;
    assert_eq!(status, "HTTP/1.1 403 Forbidden");
    let record = next_record(&mut records).await;
    assert_eq!(record.kind, ConnectionKind::Relay);
    assert_eq!(record.reason, Some(EgressReason::RelayUnavailable));
}

#[tokio::test]
async fn a_policy_refusal_is_recorded_with_its_reason() {
    let (proxy, addr, mut records) = start_recording(
        vec![host_rule(1, "api.example.com", Action::Allow)],
        true,
        16,
        |_| {},
    )
    .await;
    let status = exchange(
        addr,
        "CONNECT blocked.example.com:443 HTTP/1.1\r\n\r\n",
        false,
    )
    .await;
    assert_eq!(status, "HTTP/1.1 403 Forbidden");
    let record = next_record(&mut records).await;
    assert_eq!(record.kind, ConnectionKind::Tunnel);
    assert_eq!(record.outcome, ConnectionOutcome::Refused);
    assert_eq!(record.reason, Some(EgressReason::NoAllowRule));
    assert_eq!(record.host.as_deref(), Some("blocked.example.com"));
    assert_eq!(record.port, Some(443));
    assert_eq!(record.peer, None);
    let status = exchange(addr, "CONNECT api.example.com:22 HTTP/1.1\r\n\r\n", false).await;
    assert_eq!(status, "HTTP/1.1 403 Forbidden");
    assert_eq!(
        next_record(&mut records).await.reason,
        Some(EgressReason::PortNotAllowed)
    );
    assert_eq!(proxy.stats().snapshot().refused_policy, 2);
}

#[tokio::test]
async fn a_private_address_refusal_is_recorded() {
    let (_proxy, addr, mut records) =
        start_recording(vec![host_rule(1, "*", Action::Allow)], true, 16, |_| {}).await;
    let status = exchange(addr, "CONNECT localhost:443 HTTP/1.1\r\n\r\n", false).await;
    assert_eq!(status, "HTTP/1.1 403 Forbidden");
    let record = next_record(&mut records).await;
    assert_eq!(record.reason, Some(EgressReason::PrivateAddress));
    assert_eq!(record.host.as_deref(), Some("localhost"));
}

#[tokio::test]
async fn an_unreachable_destination_is_recorded_as_failed() {
    // Bound, then closed: nothing listens on it.
    let port = {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        listener.local_addr().unwrap().port()
    };
    let (_proxy, addr, mut records) =
        start_recording(vec![echo_rule(port)], true, 16, |_| {}).await;
    let status = exchange(
        addr,
        &format!("CONNECT 127.0.0.1:{port} HTTP/1.1\r\n\r\n"),
        false,
    )
    .await;
    assert_eq!(status, "HTTP/1.1 502 Bad Gateway");
    let record = next_record(&mut records).await;
    assert_eq!(record.outcome, ConnectionOutcome::Failed);
    assert_eq!(record.reason, Some(EgressReason::UpstreamUnreachable));
}

#[tokio::test]
async fn malformed_and_non_proxy_requests_are_recorded() {
    let (_proxy, addr, mut records) = start_recording(vec![], true, 16, |_| {}).await;
    let status = exchange(addr, "not http at all\r\n\r\n", false).await;
    assert_eq!(status, "HTTP/1.1 400 Bad Request");
    let record = next_record(&mut records).await;
    assert_eq!(record.kind, ConnectionKind::Unknown);
    assert_eq!(record.reason, Some(EgressReason::MalformedRequest));
    let status = exchange(addr, "GET /etc/passwd HTTP/1.1\r\n\r\n", false).await;
    assert_eq!(status, "HTTP/1.1 403 Forbidden");
    assert_eq!(
        next_record(&mut records).await.reason,
        Some(EgressReason::NotAProxyRequest)
    );
}

#[tokio::test]
async fn an_unknown_source_is_never_recorded() {
    let (recorder, mut records) = recording_channel(16);
    let mut config = EgressProxyConfig::new("127.0.0.1:0".parse().unwrap());
    config.max_connections = 2;
    let proxy = EgressProxy::with_recorder(
        config,
        Arc::new(ArcSwap::from_pointee(EgressTable::default())),
        Some(recorder),
    )
    .unwrap();
    let listener = proxy.bind().await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(proxy.clone().serve(listener));
    let status = exchange(addr, "CONNECT api.example.com:443 HTTP/1.1\r\n\r\n", false).await;
    assert_eq!(status, "HTTP/1.1 403 Forbidden");
    assert_eq!(proxy.stats().snapshot().refused_unknown_source, 1);
    tokio::time::sleep(Duration::from_millis(50)).await;
    assert!(
        records.try_recv().is_none(),
        "no workload owns the source: nothing to attribute a record to"
    );
}

#[tokio::test]
async fn a_saturated_proxy_records_the_refusal_for_an_opted_in_workload() {
    let (proxy, addr, mut records) =
        start_recording(vec![], true, 16, |config| config.max_connections = 1).await;
    // Holds the only permit: connected, head not sent yet.
    let _held = TcpStream::connect(addr).await.unwrap();
    for _ in 0..50 {
        if proxy.stats().snapshot().active == 1 {
            break;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    let mut refused = TcpStream::connect(addr).await.unwrap();
    assert_eq!(
        status_line(&mut refused).await,
        "HTTP/1.1 503 Service Unavailable"
    );
    let record = next_record(&mut records).await;
    assert_eq!(record.workload, sandbox(1));
    assert_eq!(record.outcome, ConnectionOutcome::Refused);
    assert_eq!(record.reason, Some(EgressReason::Saturated));
    assert_eq!(record.host, None, "refused before the head was read");
}

#[tokio::test]
async fn with_recording_off_nothing_is_recorded() {
    let echo = echo_server().await;
    let (proxy, addr, mut records) =
        start_recording(vec![echo_rule(echo.port())], false, 16, |_| {}).await;
    let status = exchange(
        addr,
        &format!("CONNECT 127.0.0.1:{} HTTP/1.1\r\n\r\n", echo.port()),
        true,
    )
    .await;
    assert_eq!(status, "HTTP/1.1 200 Connection Established");
    let status = exchange(
        addr,
        "CONNECT blocked.example.com:443 HTTP/1.1\r\n\r\n",
        false,
    )
    .await;
    assert_eq!(status, "HTTP/1.1 403 Forbidden");
    tokio::time::sleep(Duration::from_millis(50)).await;
    assert!(records.try_recv().is_none());
    let stats = proxy.stats().snapshot();
    assert_eq!(stats.tunnels, 1);
    assert_eq!((stats.records_emitted, stats.records_dropped), (0, 0));
}

#[tokio::test]
async fn a_proxy_without_a_recorder_ignores_the_opt_in() {
    let echo = echo_server().await;
    let identities = IdentityMap::new().with(sandbox(1), "127.0.0.1".parse().unwrap());
    let table = EgressTable::build(
        &[EgressPolicy {
            subject: sandbox(1),
            rules: vec![echo_rule(echo.port())],
            record: true,
            public_only: false,
        }],
        &identities,
    );
    let proxy = EgressProxy::new(
        EgressProxyConfig::new("127.0.0.1:0".parse().unwrap()),
        Arc::new(ArcSwap::from_pointee(table)),
    )
    .unwrap();
    assert!(!proxy.records());
    let listener = proxy.bind().await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(proxy.clone().serve(listener));
    let status = exchange(
        addr,
        &format!("CONNECT 127.0.0.1:{} HTTP/1.1\r\n\r\n", echo.port()),
        true,
    )
    .await;
    assert_eq!(status, "HTTP/1.1 200 Connection Established");
    assert_eq!(proxy.stats().snapshot().records_emitted, 0);
}

#[tokio::test]
async fn a_full_recording_channel_drops_records_and_never_blocks_connections() {
    let echo = echo_server().await;
    // Room for one record and nobody draining it.
    let (proxy, addr, _records) =
        start_recording(vec![echo_rule(echo.port())], true, 1, |_| {}).await;
    for _ in 0..5 {
        let status = tokio::time::timeout(
            Duration::from_secs(5),
            exchange(
                addr,
                &format!("CONNECT 127.0.0.1:{} HTTP/1.1\r\n\r\n", echo.port()),
                true,
            ),
        )
        .await
        .expect("a full channel must not stall the connection");
        assert_eq!(status, "HTTP/1.1 200 Connection Established");
    }
    for _ in 0..50 {
        if proxy.stats().snapshot().records_dropped == 4 {
            break;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    let stats = proxy.stats().snapshot();
    assert_eq!(stats.tunnels, 5);
    assert_eq!(stats.records_emitted, 1);
    assert_eq!(stats.records_dropped, 4);
}

#[test]
fn hosts_in_records_are_bounded() {
    assert_eq!(recorded_host("api.example.com".into()), "api.example.com");
    assert_eq!(
        recorded_host("a".repeat(5000)).len(),
        record::MAX_RECORDED_HOST
    );
}

#[test]
fn a_public_only_policy_refuses_private_addresses_even_inside_an_allowed_cidr() {
    let policy = |public_only| EgressPolicy {
        subject: sandbox(1),
        rules: vec![cidr_rule(1, "10.237.9.0/24", Some(443), Action::Allow)],
        record: false,
        public_only,
    };
    let inside: IpAddr = "10.237.9.4".parse().unwrap();
    let netpol = CompiledRules::new(&policy(false));
    assert_eq!(
        netpol.check_addresses(&[inside], 443, Admission::ByRule),
        Ok(())
    );
    let sandbox_policy = CompiledRules::new(&policy(true));
    assert_eq!(
        sandbox_policy.check_addresses(&[inside], 443, Admission::ByRule),
        Err(Refusal::Private),
        "a sandbox's own policy never reaches a private address"
    );
    let public: IpAddr = "93.184.216.34".parse().unwrap();
    let public_rules = CompiledRules::new(&EgressPolicy {
        rules: vec![host_rule(1, "*", Action::Allow)],
        ..policy(true)
    });
    assert_eq!(
        public_rules.check_addresses(&[public], 443, Admission::ByRule),
        Ok(())
    );
}
