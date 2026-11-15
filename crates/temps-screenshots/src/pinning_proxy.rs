// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! The proxy every connection of a local capture goes through.
//!
//! The request filter in [`crate::local_provider`] judges a URL, but Chromium
//! resolves the hostname itself, later and on its own. A name can resolve to
//! a public address when it is judged and to an internal one when Chromium
//! connects, so a check that only looks at the name constrains nothing.
//!
//! Chromium is therefore launched with this proxy as its only route out. For
//! each request the proxy resolves the name **once**, judges every address
//! with [`RequestPolicy::resolve`], and connects to one of exactly those
//! addresses. The address that was checked is the address that is dialled.
//!
//! One proxy runs per capture, on a random loopback port, and stops with it.
//! Resource use is bounded: at most [`MAX_CONNECTIONS`] connections at once
//! (further ones wait for a free slot), request heads of at most
//! [`MAX_HEAD_BYTES`], and timeouts on reading the head and on connecting.

use std::future::Future;
use std::io;
use std::net::{IpAddr, SocketAddr};
use std::pin::Pin;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Duration;

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::Semaphore;
use tokio::task::{JoinHandle, JoinSet};
use tracing::{debug, warn};

use crate::request_policy::RequestPolicy;

/// Connections served at once. A screenshot page needs a few dozen at most.
pub const MAX_CONNECTIONS: usize = 64;
/// Largest request line plus headers accepted from the browser.
pub const MAX_HEAD_BYTES: usize = 16 * 1024;
const HEAD_TIMEOUT: Duration = Duration::from_secs(10);
const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);

/// The result of one DNS lookup.
pub type LookupFuture = Pin<Box<dyn Future<Output = io::Result<Vec<SocketAddr>>> + Send>>;
/// Resolves `host` and `port` to socket addresses. Production uses the system
/// resolver; tests substitute answers a real resolver would give an attacker.
pub type Lookup = Arc<dyn Fn(String, u16) -> LookupFuture + Send + Sync>;

/// The system resolver.
pub fn system_lookup() -> Lookup {
    Arc::new(|host: String, port: u16| {
        Box::pin(async move {
            Ok(tokio::net::lookup_host((host.as_str(), port))
                .await?
                .collect())
        })
    })
}

/// A running proxy. Dropping it stops the proxy and every connection it holds.
pub struct PinningProxy {
    address: SocketAddr,
    task: JoinHandle<()>,
}

impl PinningProxy {
    /// Start a proxy on a random loopback port. Every connection it refuses
    /// is counted in `refused`.
    pub async fn start(
        policy: RequestPolicy,
        lookup: Lookup,
        refused: Arc<AtomicUsize>,
    ) -> io::Result<Self> {
        let listener = TcpListener::bind((IpAddr::from([127, 0, 0, 1]), 0)).await?;
        let address = listener.local_addr()?;
        let shared = Arc::new(Shared {
            policy,
            lookup,
            refused,
        });
        let task = tokio::spawn(accept_loop(listener, shared));
        Ok(Self { address, task })
    }

    /// Where the browser should send its requests.
    pub fn address(&self) -> SocketAddr {
        self.address
    }

    /// The value for Chromium's `--proxy-server`.
    pub fn proxy_server(&self) -> String {
        format!("http://{}", self.address)
    }
}

impl Drop for PinningProxy {
    fn drop(&mut self) {
        // The connection tasks live in a `JoinSet` owned by this task, so
        // aborting it aborts them too.
        self.task.abort();
    }
}

struct Shared {
    policy: RequestPolicy,
    lookup: Lookup,
    refused: Arc<AtomicUsize>,
}

async fn accept_loop(listener: TcpListener, shared: Arc<Shared>) {
    let slots = Arc::new(Semaphore::new(MAX_CONNECTIONS));
    let mut connections = JoinSet::new();
    loop {
        while connections.try_join_next().is_some() {}
        let Ok(slot) = slots.clone().acquire_owned().await else {
            return;
        };
        let (client, peer) = match listener.accept().await {
            Ok(accepted) => accepted,
            Err(error) => {
                warn!("Screenshot proxy stopped accepting connections: {error}");
                return;
            }
        };
        let shared = shared.clone();
        connections.spawn(async move {
            let _slot = slot;
            if let Err(error) = serve(client, &shared).await {
                debug!("Screenshot proxy connection from {peer} ended: {error}");
            }
        });
    }
}

/// What the browser asked the proxy to do.
#[derive(Debug, PartialEq, Eq)]
struct ProxyRequest {
    host: String,
    port: u16,
    /// `None` for a `CONNECT` tunnel; otherwise the head to send upstream,
    /// rewritten from the proxy form (`GET http://host/path`) to the origin
    /// form (`GET /path`).
    forward_head: Option<Vec<u8>>,
}

async fn serve(mut client: TcpStream, shared: &Shared) -> io::Result<()> {
    let (head, rest) = match tokio::time::timeout(HEAD_TIMEOUT, read_head(&mut client)).await {
        Ok(result) => result?,
        Err(_) => return Ok(()),
    };
    let request = match parse_head(&head) {
        Ok(request) => request,
        Err(reason) => return refuse(&mut client, shared, "400 Bad Request", &reason).await,
    };

    let addresses = match shared
        .policy
        .resolve(&request.host, request.port, &shared.lookup)
        .await
    {
        Ok(addresses) => addresses,
        Err(reason) => {
            let reason = format!("{}:{}: {reason}", request.host, request.port);
            return refuse(&mut client, shared, "403 Forbidden", &reason).await;
        }
    };
    let Some(mut upstream) = connect_any(&addresses).await else {
        let _ = client
            .write_all(
                b"HTTP/1.1 502 Bad Gateway\r\ncontent-length: 0\r\nconnection: close\r\n\r\n",
            )
            .await;
        return Ok(());
    };

    match request.forward_head {
        None => {
            client
                .write_all(b"HTTP/1.1 200 Connection Established\r\n\r\n")
                .await?
        }
        Some(head) => upstream.write_all(&head).await?,
    }
    upstream.write_all(&rest).await?;
    tokio::io::copy_bidirectional(&mut client, &mut upstream).await?;
    Ok(())
}

async fn refuse(
    client: &mut TcpStream,
    shared: &Shared,
    status: &str,
    reason: &str,
) -> io::Result<()> {
    shared.refused.fetch_add(1, Ordering::Relaxed);
    warn!("Blocked a connection the page being captured tried to open: {reason}");
    client
        .write_all(
            format!("HTTP/1.1 {status}\r\ncontent-length: 0\r\nconnection: close\r\n\r\n")
                .as_bytes(),
        )
        .await
}

/// Read up to the end of the request head. Returns the head and whatever was
/// read past it (the start of a body or of a tunnelled stream).
async fn read_head(client: &mut TcpStream) -> io::Result<(Vec<u8>, Vec<u8>)> {
    let mut buffer = Vec::with_capacity(2048);
    let mut chunk = [0u8; 2048];
    loop {
        let read = client.read(&mut chunk).await?;
        if read == 0 {
            return Err(io::Error::new(
                io::ErrorKind::UnexpectedEof,
                "closed before the request head ended",
            ));
        }
        let searched_from = buffer.len().saturating_sub(3);
        buffer.extend_from_slice(&chunk[..read]);
        if let Some(end) = find_head_end(&buffer[searched_from..]) {
            let end = searched_from + end;
            let rest = buffer.split_off(end);
            return Ok((buffer, rest));
        }
        if buffer.len() > MAX_HEAD_BYTES {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!("request head exceeds {MAX_HEAD_BYTES} bytes"),
            ));
        }
    }
}

/// The index just past the blank line that ends a request head.
fn find_head_end(bytes: &[u8]) -> Option<usize> {
    bytes
        .windows(4)
        .position(|window| window == b"\r\n\r\n")
        .map(|start| start + 4)
}

fn parse_head(head: &[u8]) -> Result<ProxyRequest, String> {
    let text = std::str::from_utf8(head).map_err(|_| "the request head is not UTF-8")?;
    let mut lines = text.split("\r\n");
    let request_line = lines.next().unwrap_or_default();
    let mut parts = request_line.split(' ');
    let (Some(method), Some(target), Some(version), None) =
        (parts.next(), parts.next(), parts.next(), parts.next())
    else {
        return Err(format!("malformed request line '{request_line}'"));
    };
    if !version.starts_with("HTTP/1.") {
        return Err(format!("unsupported protocol '{version}'"));
    }

    if method.eq_ignore_ascii_case("CONNECT") {
        let url = url::Url::parse(&format!("http://{target}/"))
            .map_err(|error| format!("malformed CONNECT target '{target}': {error}"))?;
        let host = host_of(&url).ok_or_else(|| format!("CONNECT target '{target}' has no host"))?;
        let port = url
            .port()
            .ok_or_else(|| format!("CONNECT target '{target}' has no port"))?;
        if url.path() != "/" || url.username() != "" || url.password().is_some() {
            return Err(format!("malformed CONNECT target '{target}'"));
        }
        return Ok(ProxyRequest {
            host,
            port,
            forward_head: None,
        });
    }

    let url = url::Url::parse(target)
        .map_err(|error| format!("expected an absolute URL, got '{target}': {error}"))?;
    if !matches!(url.scheme(), "http" | "ws") {
        return Err(format!("scheme '{}' cannot be proxied", url.scheme()));
    }
    let host = host_of(&url).ok_or_else(|| format!("'{target}' has no host"))?;
    let port = url
        .port_or_known_default()
        .ok_or_else(|| format!("'{target}' has no port"))?;
    let path = match url.query() {
        Some(query) => format!("{}?{query}", url.path()),
        None => url.path().to_string(),
    };

    let mut forward = format!("{method} {path} {version}\r\n");
    for line in lines.filter(|line| !line.is_empty()) {
        let name = line.split(':').next().unwrap_or_default().trim();
        // Meant for this proxy, not for the server.
        if name.eq_ignore_ascii_case("proxy-connection")
            || name.eq_ignore_ascii_case("proxy-authorization")
        {
            continue;
        }
        forward.push_str(line);
        forward.push_str("\r\n");
    }
    forward.push_str("\r\n");
    Ok(ProxyRequest {
        host,
        port,
        forward_head: Some(forward.into_bytes()),
    })
}

/// The host of `url` as a name or a bare IP address (no IPv6 brackets).
fn host_of(url: &url::Url) -> Option<String> {
    match url.host()? {
        url::Host::Domain(domain) => Some(domain.to_ascii_lowercase()),
        url::Host::Ipv4(ip) => Some(ip.to_string()),
        url::Host::Ipv6(ip) => Some(ip.to_string()),
    }
}

/// Connect to the first of `addresses` that answers, and only to those.
async fn connect_any(addresses: &[SocketAddr]) -> Option<TcpStream> {
    for address in addresses {
        match tokio::time::timeout(CONNECT_TIMEOUT, TcpStream::connect(address)).await {
            Ok(Ok(stream)) => return Some(stream),
            Ok(Err(error)) => debug!("Screenshot proxy could not reach {address}: {error}"),
            Err(_) => debug!("Screenshot proxy timed out connecting to {address}"),
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;
    use std::sync::Mutex;

    /// A resolver with fixed answers, recording every lookup it is asked for.
    fn fixed_lookup(
        answers: HashMap<&'static str, Vec<SocketAddr>>,
    ) -> (Lookup, Arc<Mutex<Vec<String>>>) {
        let asked = Arc::new(Mutex::new(Vec::new()));
        let record = asked.clone();
        let lookup: Lookup = Arc::new(move |host: String, port: u16| {
            record.lock().expect("lookup log").push(host.clone());
            let answer = answers.get(host.as_str()).map(|addresses| {
                addresses
                    .iter()
                    .map(|address| SocketAddr::new(address.ip(), port))
                    .collect::<Vec<_>>()
            });
            Box::pin(async move {
                answer.ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "no such name"))
            })
        });
        (lookup, asked)
    }

    /// A server that answers every connection with `body` and counts them.
    async fn upstream(
        body: &'static str,
    ) -> (SocketAddr, Arc<AtomicUsize>, Arc<Mutex<Vec<String>>>) {
        let listener = TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind upstream");
        let address = listener.local_addr().expect("upstream address");
        let hits = Arc::new(AtomicUsize::new(0));
        let heads = Arc::new(Mutex::new(Vec::new()));
        let (counter, log) = (hits.clone(), heads.clone());
        tokio::spawn(async move {
            while let Ok((mut socket, _)) = listener.accept().await {
                counter.fetch_add(1, Ordering::SeqCst);
                let mut buffer = vec![0u8; 4096];
                let read = socket.read(&mut buffer).await.unwrap_or(0);
                log.lock()
                    .expect("head log")
                    .push(String::from_utf8_lossy(&buffer[..read]).into_owned());
                let response = format!(
                    "HTTP/1.1 200 OK\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",
                    body.len()
                );
                let _ = socket.write_all(response.as_bytes()).await;
            }
        });
        (address, hits, heads)
    }

    async fn send(proxy: &PinningProxy, request: &str) -> String {
        let mut stream = TcpStream::connect(proxy.address())
            .await
            .expect("connect proxy");
        stream.write_all(request.as_bytes()).await.expect("send");
        let mut response = Vec::new();
        let _ =
            tokio::time::timeout(Duration::from_secs(5), stream.read_to_end(&mut response)).await;
        String::from_utf8_lossy(&response).into_owned()
    }

    /// The DNS-rebinding case: a name the request filter lets through, because
    /// nothing about the name itself is internal, answers with a loopback
    /// address when the connection is made. The proxy judges the answer it
    /// dials, so nothing reaches the internal service.
    #[tokio::test]
    async fn a_name_that_resolves_to_an_internal_address_at_connect_time_is_not_dialled() {
        let (internal, internal_hits, _) = upstream("internal").await;
        let policy = RequestPolicy::for_target("http://app.example.test:8080/").expect("policy");
        assert_eq!(
            policy.decide(&format!("http://rebind.example.net:{}/", internal.port())),
            crate::request_policy::RequestDecision::Allow,
            "the name alone carries nothing to block"
        );

        let (lookup, _) = fixed_lookup(HashMap::from([("rebind.example.net", vec![internal])]));
        let refused = Arc::new(AtomicUsize::new(0));
        let proxy = PinningProxy::start(policy, lookup, refused.clone())
            .await
            .expect("start proxy");

        let port = internal.port();
        let plain = send(
            &proxy,
            &format!("GET http://rebind.example.net:{port}/secret HTTP/1.1\r\nhost: rebind.example.net:{port}\r\n\r\n"),
        )
        .await;
        assert!(plain.starts_with("HTTP/1.1 403"), "{plain}");
        let tunnel = send(
            &proxy,
            &format!("CONNECT rebind.example.net:{port} HTTP/1.1\r\nhost: rebind.example.net:{port}\r\n\r\n"),
        )
        .await;
        assert!(tunnel.starts_with("HTTP/1.1 403"), "{tunnel}");

        assert_eq!(internal_hits.load(Ordering::SeqCst), 0);
        assert_eq!(refused.load(Ordering::SeqCst), 2);
    }

    /// A name whose answer mixes a public and an internal address is refused
    /// whole, rather than dialling whichever address answers first.
    #[tokio::test]
    async fn one_internal_address_in_the_answer_refuses_the_name() {
        let (internal, internal_hits, _) = upstream("internal").await;
        let policy = RequestPolicy::for_target("http://app.example.test:8080/").expect("policy");
        let public: SocketAddr = "8.8.8.8:80".parse().expect("address");
        let (lookup, _) = fixed_lookup(HashMap::from([(
            "mixed.example.net",
            vec![internal, public],
        )]));
        let proxy = PinningProxy::start(policy, lookup, Arc::new(AtomicUsize::new(0)))
            .await
            .expect("start proxy");
        let response = send(
            &proxy,
            &format!(
                "CONNECT mixed.example.net:{} HTTP/1.1\r\n\r\n",
                internal.port()
            ),
        )
        .await;
        assert!(response.starts_with("HTTP/1.1 403"), "{response}");
        assert_eq!(internal_hits.load(Ordering::SeqCst), 0);
    }

    /// The captured app's own address is reached whatever it resolves to, and
    /// the request reaches it in origin form with its headers, minus the ones
    /// addressed to the proxy.
    #[tokio::test]
    async fn the_target_is_reached_through_the_address_that_was_resolved() {
        let (app, app_hits, heads) = upstream("page").await;
        let target = format!("http://app.example.test:{}/", app.port());
        let policy = RequestPolicy::for_target(&target).expect("policy");
        let (lookup, asked) = fixed_lookup(HashMap::from([("app.example.test", vec![app])]));
        let proxy = PinningProxy::start(policy, lookup, Arc::new(AtomicUsize::new(0)))
            .await
            .expect("start proxy");

        let port = app.port();
        let response = send(
            &proxy,
            &format!(
                "GET http://app.example.test:{port}/a?b=c HTTP/1.1\r\nhost: app.example.test:{port}\r\n\
                 proxy-connection: keep-alive\r\naccept: */*\r\n\r\n"
            ),
        )
        .await;
        assert!(response.starts_with("HTTP/1.1 200"), "{response}");
        assert!(response.ends_with("page"), "{response}");
        assert_eq!(app_hits.load(Ordering::SeqCst), 1);
        assert_eq!(
            asked.lock().expect("lookups").as_slice(),
            ["app.example.test"]
        );

        let head = heads.lock().expect("heads")[0].clone();
        assert!(head.starts_with("GET /a?b=c HTTP/1.1\r\n"), "{head}");
        assert!(
            head.contains(&format!("host: app.example.test:{port}\r\n")),
            "{head}"
        );
        assert!(head.contains("accept: */*\r\n"), "{head}");
        assert!(
            !head.to_ascii_lowercase().contains("proxy-connection"),
            "{head}"
        );
    }

    /// A tunnel to the target carries bytes both ways once established.
    #[tokio::test]
    async fn a_tunnel_to_an_allowed_address_carries_the_stream() {
        let (app, app_hits, heads) = upstream("tunnelled").await;
        let policy = RequestPolicy::for_target(&format!("https://127.0.0.1:{}/", app.port()))
            .expect("policy");
        let (lookup, asked) = fixed_lookup(HashMap::new());
        let proxy = PinningProxy::start(policy, lookup, Arc::new(AtomicUsize::new(0)))
            .await
            .expect("start proxy");

        let response = send(
            &proxy,
            &format!(
                "CONNECT 127.0.0.1:{} HTTP/1.1\r\n\r\nGET / HTTP/1.1\r\nhost: x\r\n\r\n",
                app.port()
            ),
        )
        .await;
        assert!(
            response.starts_with("HTTP/1.1 200 Connection Established\r\n\r\nHTTP/1.1 200 OK"),
            "{response}"
        );
        assert!(response.ends_with("tunnelled"), "{response}");
        assert_eq!(app_hits.load(Ordering::SeqCst), 1);
        assert!(heads.lock().expect("heads")[0].starts_with("GET / HTTP/1.1"));
        assert!(
            asked.lock().expect("lookups").is_empty(),
            "an IP literal needs no lookup"
        );
    }

    #[tokio::test]
    async fn malformed_and_oversized_requests_are_refused() {
        let policy = RequestPolicy::for_target("http://app.example.test:8080/").expect("policy");
        let (lookup, asked) = fixed_lookup(HashMap::new());
        let proxy = PinningProxy::start(policy, lookup, Arc::new(AtomicUsize::new(0)))
            .await
            .expect("start proxy");

        for request in [
            "GET /relative HTTP/1.1\r\n\r\n",
            "GET ftp://8.8.8.8/ HTTP/1.1\r\n\r\n",
            "CONNECT 8.8.8.8 HTTP/1.1\r\n\r\n",
            "GET http://8.8.8.8/ HTTP/2\r\n\r\n",
        ] {
            let response = send(&proxy, request).await;
            assert!(
                response.starts_with("HTTP/1.1 400"),
                "{request}: {response}"
            );
        }

        let oversized = format!(
            "GET http://8.8.8.8/ HTTP/1.1\r\nx: {}\r\n\r\n",
            "a".repeat(MAX_HEAD_BYTES)
        );
        let response = send(&proxy, &oversized).await;
        assert!(response.is_empty(), "the connection is dropped: {response}");
        assert!(asked.lock().expect("lookups").is_empty());
    }

    #[tokio::test]
    async fn dropping_the_proxy_stops_it() {
        let policy = RequestPolicy::for_target("http://app.example.test:8080/").expect("policy");
        let proxy = PinningProxy::start(policy, system_lookup(), Arc::new(AtomicUsize::new(0)))
            .await
            .expect("start proxy");
        let address = proxy.address();
        drop(proxy);
        tokio::task::yield_now().await;
        let mut refused = false;
        for _ in 0..50 {
            if TcpStream::connect(address).await.is_err() {
                refused = true;
                break;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        assert!(refused, "the listener should be closed");
    }
}
