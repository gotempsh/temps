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
//! Every plain HTTP request is judged and sent on its own: a browser keeps a
//! proxy connection alive and sends requests for different sites over it, so
//! each one gets its own upstream connection to the address checked for its
//! own host. A `CONNECT` tunnel (HTTPS, WebSocket) is judged once, when it is
//! opened, and then carries one stream to one destination.
//!
//! One proxy runs per capture, on a random loopback port, and stops with it.
//! Resource use is bounded: at most [`MAX_CONNECTIONS`] browser connections
//! and tunnels at once (further ones wait for a free slot), request heads of
//! at most [`MAX_HEAD_BYTES`], and deadlines on reading a head, on DNS
//! lookups and on connecting.

use std::convert::Infallible;
use std::future::Future;
use std::io;
use std::net::{IpAddr, SocketAddr};
use std::pin::Pin;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use bytes::Bytes;
use http_body_util::{combinators::BoxBody, BodyExt, Empty};
use hyper::body::Incoming;
use hyper::header::{HeaderMap, HeaderName, HeaderValue, CONNECTION, HOST, UPGRADE};
use hyper::{Method, Request, Response, StatusCode, Uri};
use hyper_util::rt::{TokioIo, TokioTimer};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::{OwnedSemaphorePermit, Semaphore};
use tokio::task::{JoinHandle, JoinSet};
use tracing::{debug, warn};

use crate::request_policy::RequestPolicy;

/// Browser connections and tunnels served at once. A screenshot page needs a
/// few dozen at most.
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
    /// Start a proxy on a random loopback port. Every request it refuses is
    /// counted in `refused`.
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
            tasks: Mutex::new(JoinSet::new()),
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
        // Connection tasks live in a `JoinSet` owned by this task, and every
        // tunnel and upstream connection in `Shared::tasks`, which only
        // connection tasks keep alive: aborting this task stops them all.
        self.task.abort();
    }
}

struct Shared {
    policy: RequestPolicy,
    lookup: Lookup,
    refused: Arc<AtomicUsize>,
    /// Tunnels and upstream connections. Tasks in here never hold `Shared`.
    tasks: Mutex<JoinSet<()>>,
}

impl Shared {
    fn spawn(&self, task: impl Future<Output = ()> + Send + 'static) {
        let mut tasks = self.tasks.lock().unwrap_or_else(PoisonError::into_inner);
        while tasks.try_join_next().is_some() {}
        tasks.spawn(task);
    }

    fn refuse(&self, status: StatusCode, reason: &str) -> Response<ProxyBody> {
        self.refused.fetch_add(1, Ordering::Relaxed);
        warn!("Blocked a request the page being captured made: {reason}");
        respond(status)
    }
}

/// The slot a browser connection holds, handed on to a tunnel it opens.
type Slot = Arc<Mutex<Option<OwnedSemaphorePermit>>>;
type ProxyBody = BoxBody<Bytes, hyper::Error>;

async fn accept_loop(listener: TcpListener, shared: Arc<Shared>) {
    let slots = Arc::new(Semaphore::new(MAX_CONNECTIONS));
    let mut connections = JoinSet::new();
    loop {
        while connections.try_join_next().is_some() {}
        let Ok(permit) = slots.clone().acquire_owned().await else {
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
            let slot: Slot = Arc::new(Mutex::new(Some(permit)));
            let service = hyper::service::service_fn({
                let slot = slot.clone();
                move |request| {
                    let shared = shared.clone();
                    let slot = slot.clone();
                    async move { Ok::<_, Infallible>(handle(request, &shared, &slot).await) }
                }
            });
            let served = hyper::server::conn::http1::Builder::new()
                .timer(TokioTimer::new())
                .header_read_timeout(HEAD_TIMEOUT)
                .max_buf_size(MAX_HEAD_BYTES)
                .serve_connection(TokioIo::new(client), service)
                .with_upgrades()
                .await;
            if let Err(error) = served {
                debug!("Screenshot proxy connection from {peer} ended: {error}");
            }
        });
    }
}

async fn handle(request: Request<Incoming>, shared: &Shared, slot: &Slot) -> Response<ProxyBody> {
    if request.method() == Method::CONNECT {
        tunnel(request, shared, slot).await
    } else {
        forward(request, shared, slot).await
    }
}

/// Open a `CONNECT` tunnel to a checked address of the requested authority.
async fn tunnel(
    mut request: Request<Incoming>,
    shared: &Shared,
    slot: &Slot,
) -> Response<ProxyBody> {
    let (host, port) = match connect_target(request.uri()) {
        Ok(target) => target,
        Err(reason) => return shared.refuse(StatusCode::BAD_REQUEST, &reason),
    };
    let Some(upstream) = connect_checked(shared, &host, port).await else {
        return respond(StatusCode::BAD_GATEWAY);
    };
    let upstream = match upstream {
        Ok(stream) => stream,
        Err(refusal) => return refusal,
    };
    // The tunnel outlives this connection's HTTP handling, so it takes over
    // the connection's slot.
    let permit = slot.lock().unwrap_or_else(PoisonError::into_inner).take();
    let upgrade = hyper::upgrade::on(&mut request);
    shared.spawn(async move {
        let _permit = permit;
        match upgrade.await {
            Ok(upgraded) => {
                let mut client = TokioIo::new(upgraded);
                let mut upstream = upstream;
                let _ = tokio::io::copy_bidirectional(&mut client, &mut upstream).await;
            }
            Err(error) => debug!("Screenshot proxy tunnel was not established: {error}"),
        }
    });
    respond(StatusCode::OK)
}

/// Send one plain HTTP request to a checked address of its own host.
async fn forward(
    mut request: Request<Incoming>,
    shared: &Shared,
    slot: &Slot,
) -> Response<ProxyBody> {
    let (host, port) = match forward_target(request.uri()) {
        Ok(target) => target,
        Err(reason) => return shared.refuse(StatusCode::BAD_REQUEST, &reason),
    };
    let upstream = match connect_checked(shared, &host, port).await {
        None => return respond(StatusCode::BAD_GATEWAY),
        Some(Err(refusal)) => return refusal,
        Some(Ok(stream)) => stream,
    };
    let (mut sender, connection) =
        match hyper::client::conn::http1::handshake(TokioIo::new(upstream)).await {
            Ok(parts) => parts,
            Err(error) => {
                debug!("Screenshot proxy could not talk HTTP to {host}:{port}: {error}");
                return respond(StatusCode::BAD_GATEWAY);
            }
        };
    shared.spawn(async move {
        if let Err(error) = connection.with_upgrades().await {
            debug!("Screenshot proxy upstream connection ended: {error}");
        }
    });

    // Proxy form (`GET http://host/path`) to origin form (`GET /path`).
    if !request.headers().contains_key(HOST) {
        if let Some(authority) = request.uri().authority() {
            if let Ok(value) = HeaderValue::from_str(authority.as_str()) {
                request.headers_mut().insert(HOST, value);
            }
        }
    }
    let origin = request
        .uri()
        .path_and_query()
        .map_or("/", |path| path.as_str())
        .parse::<Uri>()
        .unwrap_or_else(|_| Uri::from_static("/"));
    *request.uri_mut() = origin;
    let upgrading = request.headers().contains_key(UPGRADE);
    strip_hop_by_hop(request.headers_mut(), upgrading);
    let client_upgrade = upgrading.then(|| hyper::upgrade::on(&mut request));

    let mut response = match sender.send_request(request).await {
        Ok(response) => response,
        Err(error) => {
            debug!("Screenshot proxy request to {host}:{port} failed: {error}");
            return respond(StatusCode::BAD_GATEWAY);
        }
    };
    if response.status() == StatusCode::SWITCHING_PROTOCOLS {
        if let Some(client_upgrade) = client_upgrade {
            let upstream_upgrade = hyper::upgrade::on(&mut response);
            // Like a `CONNECT` tunnel, the upgraded stream outlives this
            // connection's HTTP handling, so it takes over the slot.
            let permit = slot.lock().unwrap_or_else(PoisonError::into_inner).take();
            shared.spawn(async move {
                let _permit = permit;
                if let (Ok(client), Ok(upstream)) = (client_upgrade.await, upstream_upgrade.await) {
                    let _ = tokio::io::copy_bidirectional(
                        &mut TokioIo::new(client),
                        &mut TokioIo::new(upstream),
                    )
                    .await;
                }
            });
            return response.map(BodyExt::boxed);
        }
    }
    strip_hop_by_hop(response.headers_mut(), false);
    response.map(BodyExt::boxed)
}

/// Resolve and check `host:port`, then connect to one of the checked
/// addresses. `None` when none of them answers; `Some(Err)` when the policy
/// refuses the destination.
async fn connect_checked(
    shared: &Shared,
    host: &str,
    port: u16,
) -> Option<Result<TcpStream, Response<ProxyBody>>> {
    let addresses = match shared.policy.resolve(host, port, &shared.lookup).await {
        Ok(addresses) => addresses,
        Err(reason) => {
            let reason = format!("{host}:{port}: {reason}");
            return Some(Err(shared.refuse(StatusCode::FORBIDDEN, &reason)));
        }
    };
    connect_any(&addresses).await.map(Ok)
}

/// The destination of a `CONNECT`: the authority exactly as written. Its port
/// is required and kept as given, including the default ports.
fn connect_target(uri: &Uri) -> Result<(String, u16), String> {
    let authority = uri
        .authority()
        .ok_or_else(|| format!("CONNECT target '{uri}' is not host:port"))?;
    if uri.scheme().is_some() || authority.as_str().contains('@') {
        return Err(format!("malformed CONNECT target '{uri}'"));
    }
    let port = authority
        .port_u16()
        .ok_or_else(|| format!("CONNECT target '{uri}' has no port"))?;
    Ok((bare_host(authority.host()), port))
}

/// The destination of a plain request, which must use the proxy form.
fn forward_target(uri: &Uri) -> Result<(String, u16), String> {
    if uri.scheme_str() != Some("http") {
        return Err(format!("expected an absolute http:// URL, got '{uri}'"));
    }
    let authority = uri
        .authority()
        .ok_or_else(|| format!("'{uri}' has no host"))?;
    if authority.as_str().contains('@') {
        return Err(format!("'{uri}' carries credentials"));
    }
    Ok((
        bare_host(authority.host()),
        authority.port_u16().unwrap_or(80),
    ))
}

/// A host as a name or a bare IP address: no IPv6 brackets, lower case.
fn bare_host(host: &str) -> String {
    host.trim_start_matches('[')
        .trim_end_matches(']')
        .to_ascii_lowercase()
}

/// Remove the headers that describe one connection rather than the message,
/// including any the `Connection` header names. `keep_upgrade` keeps the
/// `Connection` and `Upgrade` headers of a protocol upgrade.
fn strip_hop_by_hop(headers: &mut HeaderMap, keep_upgrade: bool) {
    let named: Vec<HeaderName> = headers
        .get_all(CONNECTION)
        .iter()
        .filter_map(|value| value.to_str().ok())
        .flat_map(|value| value.split(','))
        .filter_map(|token| HeaderName::from_bytes(token.trim().as_bytes()).ok())
        .filter(|name| !(keep_upgrade && *name == UPGRADE))
        .collect();
    for name in named {
        headers.remove(name);
    }
    for name in [
        "keep-alive",
        "proxy-connection",
        "proxy-authenticate",
        "proxy-authorization",
        "te",
        "trailer",
    ] {
        headers.remove(name);
    }
    if !keep_upgrade {
        headers.remove(CONNECTION);
        headers.remove(UPGRADE);
    }
}

fn respond(status: StatusCode) -> Response<ProxyBody> {
    let mut response = Response::new(
        Empty::<Bytes>::new()
            .map_err(|never| match never {})
            .boxed(),
    );
    *response.status_mut() = status;
    response
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
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

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

    /// Read one response: its head, then as many body bytes as it declares.
    async fn read_response(stream: &mut TcpStream) -> String {
        let mut buffer = Vec::new();
        let mut chunk = [0u8; 4096];
        loop {
            if let Some(end) = buffer.windows(4).position(|w| w == b"\r\n\r\n") {
                let head = String::from_utf8_lossy(&buffer[..end]).to_ascii_lowercase();
                let length = head
                    .lines()
                    .find_map(|line| line.strip_prefix("content-length:"))
                    .and_then(|value| value.trim().parse::<usize>().ok())
                    .unwrap_or(0);
                if buffer.len() >= end + 4 + length {
                    return String::from_utf8_lossy(&buffer[..end + 4 + length]).into_owned();
                }
            }
            match tokio::time::timeout(Duration::from_secs(5), stream.read(&mut chunk)).await {
                Ok(Ok(read)) if read > 0 => buffer.extend_from_slice(&chunk[..read]),
                _ => return String::from_utf8_lossy(&buffer).into_owned(),
            }
        }
    }

    /// Send `request` on a new connection and read one response.
    async fn send(proxy: &PinningProxy, request: &str) -> String {
        let mut stream = TcpStream::connect(proxy.address())
            .await
            .expect("connect proxy");
        stream.write_all(request.as_bytes()).await.expect("send");
        read_response(&mut stream).await
    }

    /// Send `request` and read everything until the proxy closes: a tunnel.
    async fn send_until_closed(proxy: &PinningProxy, request: &str) -> String {
        let mut stream = TcpStream::connect(proxy.address())
            .await
            .expect("connect proxy");
        stream.write_all(request.as_bytes()).await.expect("send");
        let mut response = Vec::new();
        let _ =
            tokio::time::timeout(Duration::from_secs(5), stream.read_to_end(&mut response)).await;
        String::from_utf8_lossy(&response).into_owned()
    }

    /// A resolver that answers `host` with exactly `address`, whatever port
    /// was asked for.
    fn exact_lookup(host: &'static str, address: SocketAddr) -> Lookup {
        Arc::new(move |asked: String, _port: u16| {
            Box::pin(async move {
                if asked == host {
                    Ok(vec![address])
                } else {
                    Err(io::Error::new(io::ErrorKind::NotFound, "no such name"))
                }
            })
        })
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

        let response = send_until_closed(
            &proxy,
            &format!(
                "CONNECT 127.0.0.1:{} HTTP/1.1\r\n\r\nGET / HTTP/1.1\r\nhost: x\r\n\r\n",
                app.port()
            ),
        )
        .await;
        assert!(response.starts_with("HTTP/1.1 200 OK\r\n"), "{response}");
        assert!(
            response.contains("\r\n\r\nHTTP/1.1 200 OK\r\n"),
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

    /// A browser keeps a proxy connection alive and sends requests for other
    /// sites over it. Each request is judged and routed on its own: a later
    /// request for a refused name is refused, rather than being passed on to
    /// the server the first request went to, and the connection keeps
    /// working.
    #[tokio::test]
    async fn each_request_on_a_kept_alive_connection_is_routed_by_its_own_host() {
        let (app, app_hits, heads) = upstream("page").await;
        let (internal, internal_hits, _) = upstream("internal").await;
        let target = format!("http://app.example.test:{}/", app.port());
        let policy = RequestPolicy::for_target(&target).expect("policy");
        let (lookup, _) = fixed_lookup(HashMap::from([
            ("app.example.test", vec![app]),
            ("rebind.example.net", vec![internal]),
        ]));
        let refused = Arc::new(AtomicUsize::new(0));
        let proxy = PinningProxy::start(policy, lookup, refused.clone())
            .await
            .expect("start proxy");

        let (app_port, internal_port) = (app.port(), internal.port());
        let mut stream = TcpStream::connect(proxy.address())
            .await
            .expect("connect proxy");
        for (request, expected) in [
            (
                format!(
                    "GET http://app.example.test:{app_port}/one HTTP/1.1\r\n\
                     host: app.example.test:{app_port}\r\n\r\n"
                ),
                "HTTP/1.1 200",
            ),
            (
                format!(
                    "GET http://rebind.example.net:{internal_port}/secret HTTP/1.1\r\n\
                     host: rebind.example.net:{internal_port}\r\ncookie: session=other-site\r\n\r\n"
                ),
                "HTTP/1.1 403",
            ),
            (
                format!(
                    "GET http://app.example.test:{app_port}/two HTTP/1.1\r\n\
                     host: app.example.test:{app_port}\r\n\r\n"
                ),
                "HTTP/1.1 200",
            ),
        ] {
            stream.write_all(request.as_bytes()).await.expect("send");
            let response = read_response(&mut stream).await;
            assert!(response.starts_with(expected), "{request}: {response}");
        }

        assert_eq!(internal_hits.load(Ordering::SeqCst), 0);
        assert_eq!(app_hits.load(Ordering::SeqCst), 2);
        let heads = heads.lock().expect("heads").clone();
        assert!(heads[0].starts_with("GET /one "), "{heads:?}");
        assert!(heads[1].starts_with("GET /two "), "{heads:?}");
        assert!(
            heads.iter().all(|head| !head.contains("other-site")),
            "a request meant for another site must not reach this one: {heads:?}"
        );
        assert_eq!(refused.load(Ordering::SeqCst), 1);
    }

    /// `CONNECT host:80` names its port explicitly; it is kept, not treated
    /// as missing because 80 is HTTP's default.
    #[tokio::test]
    async fn a_tunnel_to_port_80_is_opened() {
        let (app, app_hits, _) = upstream("on port 80").await;
        let policy = RequestPolicy::for_target("https://app.example.test:80/").expect("policy");
        let proxy = PinningProxy::start(
            policy,
            exact_lookup("app.example.test", app),
            Arc::new(AtomicUsize::new(0)),
        )
        .await
        .expect("start proxy");

        let response = send_until_closed(
            &proxy,
            "CONNECT app.example.test:80 HTTP/1.1\r\nhost: app.example.test:80\r\n\r\n\
             GET / HTTP/1.1\r\nhost: app.example.test\r\n\r\n",
        )
        .await;
        assert!(response.starts_with("HTTP/1.1 200"), "{response}");
        assert!(response.ends_with("on port 80"), "{response}");
        assert_eq!(app_hits.load(Ordering::SeqCst), 1);
    }

    /// An upgraded stream (a WebSocket) outlives the HTTP connection it came
    /// from, so it keeps that connection's slot: with every slot held by
    /// open upgrades, a further connection waits until one of them closes.
    #[tokio::test]
    async fn upgraded_streams_count_against_the_connection_limit() {
        let listener = TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind upstream");
        let app = listener.local_addr().expect("upstream address");
        tokio::spawn(async move {
            while let Ok((mut socket, _)) = listener.accept().await {
                tokio::spawn(async move {
                    let mut buffer = vec![0u8; 4096];
                    let read = socket.read(&mut buffer).await.unwrap_or(0);
                    let response = if buffer[..read].starts_with(b"GET /socket ") {
                        "HTTP/1.1 101 Switching Protocols\r\nconnection: upgrade\r\n\
                         upgrade: websocket\r\n\r\n"
                    } else {
                        "HTTP/1.1 200 OK\r\ncontent-length: 2\r\nconnection: close\r\n\r\nok"
                    };
                    if socket.write_all(response.as_bytes()).await.is_ok() {
                        // Hold the upgraded stream open until the peer closes.
                        let _ = socket.read(&mut buffer).await;
                    }
                });
            }
        });
        let port = app.port();
        let policy =
            RequestPolicy::for_target(&format!("http://app.example.test:{port}/")).expect("policy");
        let proxy = PinningProxy::start(
            policy,
            exact_lookup("app.example.test", app),
            Arc::new(AtomicUsize::new(0)),
        )
        .await
        .expect("start proxy");

        let upgrade = format!(
            "GET http://app.example.test:{port}/socket HTTP/1.1\r\nhost: app.example.test:{port}\r\n\
             connection: upgrade\r\nupgrade: websocket\r\n\r\n"
        );
        let mut sockets = Vec::new();
        for _ in 0..MAX_CONNECTIONS {
            let mut stream = TcpStream::connect(proxy.address())
                .await
                .expect("connect proxy");
            stream.write_all(upgrade.as_bytes()).await.expect("send");
            let response = read_response(&mut stream).await;
            assert!(response.starts_with("HTTP/1.1 101"), "{response}");
            sockets.push(stream);
        }

        let plain = format!(
            "GET http://app.example.test:{port}/page HTTP/1.1\r\nhost: app.example.test:{port}\r\n\r\n"
        );
        let mut waiting = TcpStream::connect(proxy.address())
            .await
            .expect("connect proxy");
        waiting.write_all(plain.as_bytes()).await.expect("send");
        let mut byte = [0u8; 1];
        assert!(
            tokio::time::timeout(Duration::from_millis(500), waiting.read(&mut byte))
                .await
                .is_err(),
            "a connection beyond the limit must wait while every slot is held by an upgrade"
        );

        drop(sockets.pop());
        let response = read_response(&mut waiting).await;
        assert!(response.starts_with("HTTP/1.1 200"), "{response}");
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
        assert!(
            response.starts_with("HTTP/1.1 431"),
            "an oversized head is refused: {response}"
        );
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
