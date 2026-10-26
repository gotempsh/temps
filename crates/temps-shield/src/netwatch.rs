// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! The exchange with netwatch, the host monitor, where it runs on a node
//! (ADR 066, D12). netwatch is a source and a consumer, never a
//! dependency: without it both halves sit idle and temps decides exactly
//! as it would otherwise.
//!
//! **Bans in.** Every [`POLL_INTERVAL`] the reader asks netwatch's local
//! API (`GET {api_url}/api/bans`) for the bans it currently applies and
//! loads them into an [`Index::local`] as the `netwatch` source, verdict
//! `block`, which the ingress passes to `ShieldEngine::decide` as `local`.
//! A ban netwatch gives an expiry stops matching when it expires, even
//! between polls. When netwatch has not answered for [`STALE_AFTER`] the
//! index is cleared, so bans do not outlive the netwatch that made them. A
//! node without netwatch is normal: failures are logged at debug and the
//! reader backs off.
//!
//! **Request lines out.** The ingress hands each request to a
//! [`RequestSender`], which queues it without blocking (dropping it when
//! the queue is full) for a task that sends it to netwatch's Unix datagram
//! socket. Nobody listening is normal too: lines are dropped. This gives
//! netwatch what it cannot see on the wire, the requests inside TLS.
//!
//! # Wire format
//!
//! One JSON object per datagram, ended by a newline, so a stream reader
//! could take the same bytes. Version 1:
//!
//! ```json
//! {"v":1,"ts_ms":1793000000000,"ip":"203.0.113.7","method":"GET",
//!  "host":"app.example.com","path":"/.env?x=1","user_agent":"curl/8.5.0",
//!  "status":403,"tls":true,"ja4":"t13d1516h2_8daaf6152771_02713d6af862",
//!  "verdict":"block","acted":"block","signature":null}
//! ```
//!
//! | Field | Meaning |
//! |---|---|
//! | `v` | Format version; a receiver ignores versions it does not know |
//! | `ts_ms` | When the request arrived, Unix milliseconds |
//! | `ip` | The client address as the ingress resolved it (behind a trusted forwarder, the visitor's) |
//! | `method`, `host` | As sent |
//! | `path` | The path with its query, as sent |
//! | `user_agent` | Empty when absent |
//! | `status` | The response status |
//! | `tls` | Whether the client connected over TLS |
//! | `ja4` | The client's JA4, when known |
//! | `verdict` | What the lists and signals said: `pass`, `log`, `challenge` or `block` |
//! | `acted` | What Shield did about it (`block`, `challenge`, `error`), `null` when it let the request through |
//! | `signature` | The exploit signature Shield matched, as the CVE or the signature id |
//!
//! `host`, `path` and `user_agent` are cut to
//! [`MAX_FIELD_CHARS`](crate::events::MAX_FIELD_CHARS)
//! characters, so a line stays well under a datagram's size limit. New
//! fields may be added to version 1; a receiver ignores the ones it does
//! not know. Removing or changing the meaning of one takes a new `v`.

use std::io::ErrorKind;
use std::net::IpAddr;
use std::os::unix::net::UnixDatagram;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Weak};
use std::time::Duration;

use arc_swap::ArcSwapOption;
use serde::{Deserialize, Serialize};
use tokio::sync::mpsc;
use tokio::time::Instant;

use crate::engine::{Decision, Request};
use crate::events::bounded;
use crate::index::Index;
use crate::net::Network;
use crate::snapshot::SourceList;
use crate::verdict::{Action, ListVerdict};

/// The source name netwatch's bans carry in verdicts.
pub const SOURCE: &str = "netwatch";
/// The version of the request line this module writes.
pub const WIRE_VERSION: u32 = 1;
/// How often the reader asks netwatch for its bans.
pub const POLL_INTERVAL: Duration = Duration::from_secs(15);
/// How long the bans read last are kept while netwatch does not answer.
pub const STALE_AFTER: Duration = Duration::from_secs(120);
/// netwatch's default API address (its `api_listen`).
pub const DEFAULT_API_URL: &str = "http://127.0.0.1:9180";
/// netwatch's default socket for request lines (its `temps.socket`).
pub const DEFAULT_SOCKET_PATH: &str = "/run/netwatch/requests.sock";
/// Request lines waiting to be sent; more are dropped.
pub const QUEUE_LINES: usize = 4096;

/// Longest wait between attempts while netwatch does not answer.
const MAX_BACKOFF: Duration = Duration::from_secs(120);
const REQUEST_TIMEOUT: Duration = Duration::from_secs(5);
/// After finding nobody on the socket, lines are dropped without trying
/// for this long.
const IDLE_SOCKET_RETRY: Duration = Duration::from_secs(1);

/// Where netwatch is, and whether to talk to it.
#[derive(Clone, PartialEq, Eq, Deserialize)]
#[serde(default)]
pub struct NetwatchConfig {
    pub enabled: bool,
    /// netwatch's API, without the `/api/...` path.
    pub api_url: String,
    /// Sent as a bearer token when set. netwatch's API is loopback-only
    /// and takes none today.
    pub api_token: Option<String>,
    /// netwatch's Unix datagram socket for request lines.
    pub socket_path: PathBuf,
}

impl Default for NetwatchConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            api_url: DEFAULT_API_URL.to_string(),
            api_token: None,
            socket_path: PathBuf::from(DEFAULT_SOCKET_PATH),
        }
    }
}

impl std::fmt::Debug for NetwatchConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("NetwatchConfig")
            .field("enabled", &self.enabled)
            .field("api_url", &self.api_url)
            .field("api_token", &self.api_token.as_ref().map(|_| "***"))
            .field("socket_path", &self.socket_path)
            .finish()
    }
}

/// Why one poll of netwatch's bans failed.
#[derive(Debug, thiserror::Error)]
pub enum NetwatchError {
    #[error("netwatch bans request to {url} failed: {reason}")]
    Request { url: String, reason: String },
    #[error("netwatch bans request to {url} answered {status}")]
    Status { url: String, status: u16 },
    #[error("netwatch bans from {url} are unreadable: {reason}")]
    Decode { url: String, reason: String },
}

/// What [`spawn`] returns: the node's local lists for the ingress, and
/// where to send request lines.
pub struct Netwatch {
    /// Empty until netwatch answers, and again once it has been gone for
    /// [`STALE_AFTER`].
    pub local: Arc<ArcSwapOption<Index>>,
    pub sender: RequestSender,
}

/// Starts the bans reader and the request-line sender on the current
/// Tokio runtime. With `enabled` off nothing is started: the index stays
/// empty and the sender drops everything. The reader stops once `local`
/// is dropped, the sender's task once every [`RequestSender`] is.
pub fn spawn(config: NetwatchConfig) -> Netwatch {
    let local = Arc::new(ArcSwapOption::empty());
    if !config.enabled {
        return Netwatch {
            local,
            sender: RequestSender::disabled(),
        };
    }
    let reader = BansReader::new(&config, Timing::default());
    tokio::spawn(reader.run(Arc::downgrade(&local)));
    let (sender, rx) = RequestSender::channel(QUEUE_LINES);
    tokio::spawn(deliver(rx, config.socket_path, sender.dropped.clone()));
    Netwatch { local, sender }
}

// -- bans --------------------------------------------------------------------

/// One entry of netwatch's `GET /api/bans`. The response carries more
/// (`created`, `reason`, `source`, `dropped`); only these are read.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct Ban {
    /// An address or a CIDR range.
    pub target: String,
    /// Unix seconds; `None` for a ban without an end.
    #[serde(default)]
    pub expires: Option<i64>,
}

/// The `netwatch` list from the bans still in force at `now`; `None` when
/// there are none. Targets that do not parse are skipped. The list goes
/// stale (and only logs) [`STALE_AFTER`] past `fetched_at`, in case the
/// reader stops clearing it.
pub fn bans_list(bans: &[Ban], now: i64, fetched_at: i64) -> Option<SourceList> {
    let networks: Vec<Network> = bans
        .iter()
        .filter(|ban| ban.expires.is_none_or(|expires| expires > now))
        .filter_map(|ban| {
            let network = Network::parse(&ban.target);
            if network.is_none() {
                tracing::debug!(ban = %ban.target, "netwatch ban target is not an address or range; skipped");
            }
            network
        })
        .collect();
    (!networks.is_empty()).then(|| {
        SourceList::new(
            SOURCE,
            ListVerdict::Block,
            fetched_at,
            Some(STALE_AFTER.as_secs()),
            networks,
        )
    })
}

/// The earliest expiry after `now`, when the list must be rebuilt.
fn next_expiry(bans: &[Ban], now: i64) -> Option<i64> {
    bans.iter()
        .filter_map(|ban| ban.expires)
        .filter(|&expires| expires > now)
        .min()
}

#[derive(Debug, Clone, Copy)]
struct Timing {
    poll: Duration,
    stale: Duration,
    max_backoff: Duration,
}

impl Default for Timing {
    fn default() -> Self {
        Self {
            poll: POLL_INTERVAL,
            stale: STALE_AFTER,
            max_backoff: MAX_BACKOFF,
        }
    }
}

struct BansReader {
    client: reqwest::Client,
    url: String,
    token: Option<String>,
    timing: Timing,
}

impl BansReader {
    fn new(config: &NetwatchConfig, timing: Timing) -> Self {
        let client = reqwest::Client::builder()
            .timeout(REQUEST_TIMEOUT)
            .no_proxy()
            .build()
            .unwrap_or_else(|error| {
                tracing::debug!(%error, "netwatch HTTP client fell back to defaults");
                reqwest::Client::new()
            });
        Self {
            client,
            url: format!("{}/api/bans", config.api_url.trim_end_matches('/')),
            token: config.api_token.clone(),
            timing,
        }
    }

    async fn fetch(&self) -> Result<Vec<Ban>, NetwatchError> {
        let mut request = self.client.get(&self.url);
        if let Some(token) = &self.token {
            request = request.bearer_auth(token);
        }
        let response = request.send().await.map_err(|e| NetwatchError::Request {
            url: self.url.clone(),
            reason: e.to_string(),
        })?;
        if !response.status().is_success() {
            return Err(NetwatchError::Status {
                url: self.url.clone(),
                status: response.status().as_u16(),
            });
        }
        response
            .json::<Vec<Ban>>()
            .await
            .map_err(|e| NetwatchError::Decode {
                url: self.url.clone(),
                reason: e.to_string(),
            })
    }

    async fn run(self, local: Weak<ArcSwapOption<Index>>) {
        let mut bans: Vec<Ban> = Vec::new();
        let mut fetched_at = 0;
        let mut last_ok: Option<Instant> = None;
        let mut failures: u32 = 0;
        let mut next_poll = Instant::now();
        loop {
            let Some(slot) = local.upgrade() else { return };
            if Instant::now() >= next_poll {
                match self.fetch().await {
                    Ok(fresh) => {
                        if failures > 0 || last_ok.is_none() {
                            tracing::info!(url = %self.url, bans = fresh.len(), "reading netwatch bans");
                        }
                        bans = fresh;
                        fetched_at = crate::unix_now() as i64;
                        last_ok = Some(Instant::now());
                        failures = 0;
                    }
                    Err(error) => {
                        failures = failures.saturating_add(1);
                        tracing::debug!(%error, failures, "netwatch did not answer");
                        let gone = last_ok.is_none_or(|ok| ok.elapsed() >= self.timing.stale);
                        if gone && (!bans.is_empty() || slot.load().is_some()) {
                            tracing::info!(url = %self.url, "netwatch has not answered for a while; its bans no longer apply");
                            bans.clear();
                        }
                    }
                }
                next_poll = Instant::now() + self.delay(failures);
            }
            let now = crate::unix_now() as i64;
            slot.store(
                bans_list(&bans, now, fetched_at).map(|list| Arc::new(Index::local(&[list]))),
            );
            let mut wake = next_poll;
            if let Some(expires) = next_expiry(&bans, now) {
                let until = Duration::from_secs(expires.saturating_sub(now).max(1) as u64);
                wake = wake.min(Instant::now() + until);
            }
            drop(slot);
            tokio::time::sleep_until(wake).await;
        }
    }

    /// The poll interval, doubled for each failure in a row up to the cap.
    fn delay(&self, failures: u32) -> Duration {
        match failures {
            0 => self.timing.poll,
            n => self
                .timing
                .poll
                .saturating_mul(1 << (n - 1).min(8))
                .min(self.timing.max_backoff.max(self.timing.poll)),
        }
    }
}

// -- request lines -------------------------------------------------------------

/// One request, as netwatch receives it. See the module docs for the
/// wire format.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RequestLine {
    pub ts_ms: i64,
    pub ip: IpAddr,
    pub method: String,
    pub host: String,
    /// The path with its query.
    pub path: String,
    pub user_agent: String,
    pub status: u16,
    pub tls: bool,
    pub ja4: Option<String>,
    pub verdict: String,
    pub acted: Option<String>,
    pub signature: Option<String>,
}

impl RequestLine {
    /// The line for a request the ingress answered with `status`.
    /// `decision` is what Shield decided, `None` where it did not run.
    /// `None` without a client address: netwatch keys everything by it.
    pub fn new(
        request: &Request<'_>,
        host: &str,
        status: u16,
        decision: Option<&Decision>,
        ts_ms: i64,
    ) -> Option<Self> {
        Some(Self {
            ts_ms,
            ip: request.ip?,
            method: bounded(request.method),
            host: bounded(host),
            path: bounded(request.path_and_query),
            user_agent: bounded(request.user_agent),
            status,
            tls: request.tls,
            ja4: request.ja4.map(bounded),
            verdict: decision
                .map_or(Action::Pass, |d| d.verdict.action)
                .as_str()
                .to_string(),
            acted: decision
                .and_then(Decision::header_verdict)
                .map(String::from),
            signature: decision
                .and_then(|d| d.verdict.signature())
                .map(String::from),
        })
    }

    /// The line as sent: versioned JSON and a newline.
    pub fn encode(&self) -> Vec<u8> {
        #[derive(Serialize)]
        struct Wire<'a> {
            v: u32,
            #[serde(flatten)]
            line: &'a RequestLine,
        }
        let mut bytes = serde_json::to_vec(&Wire {
            v: WIRE_VERSION,
            line: self,
        })
        .unwrap_or_default();
        bytes.push(b'\n');
        bytes
    }
}

/// Hands request lines to netwatch without ever waiting. Cheap to clone.
#[derive(Clone)]
pub struct RequestSender {
    tx: Option<mpsc::Sender<RequestLine>>,
    dropped: Arc<AtomicU64>,
}

impl RequestSender {
    /// A sender that drops every line, for a node without netwatch.
    pub fn disabled() -> Self {
        Self {
            tx: None,
            dropped: Arc::new(AtomicU64::new(0)),
        }
    }

    fn channel(capacity: usize) -> (Self, mpsc::Receiver<RequestLine>) {
        let (tx, rx) = mpsc::channel(capacity);
        let sender = Self {
            tx: Some(tx),
            dropped: Arc::new(AtomicU64::new(0)),
        };
        (sender, rx)
    }

    /// Queues `line`, or drops it when the queue is full.
    pub fn send(&self, line: RequestLine) {
        if let Some(tx) = &self.tx {
            if tx.try_send(line).is_err() {
                self.dropped.fetch_add(1, Ordering::Relaxed);
            }
        }
    }

    /// Lines dropped so far: queue full, nobody listening or a send error.
    pub fn dropped(&self) -> u64 {
        self.dropped.load(Ordering::Relaxed)
    }
}

/// Sends queued lines to `path` until every sender is gone.
async fn deliver(mut rx: mpsc::Receiver<RequestLine>, path: PathBuf, dropped: Arc<AtomicU64>) {
    // A plain non-blocking socket: a datagram either goes into the
    // receiver's buffer at once or is dropped, so sending never waits.
    let socket = match UnixDatagram::unbound().and_then(|socket| {
        socket.set_nonblocking(true)?;
        Ok(socket)
    }) {
        Ok(socket) => socket,
        Err(error) => {
            tracing::warn!(%error, "cannot open a socket for netwatch request lines; they are dropped");
            while rx.recv().await.is_some() {
                dropped.fetch_add(1, Ordering::Relaxed);
            }
            return;
        }
    };
    let mut idle_until: Option<Instant> = None;
    let mut listening = false;
    while let Some(line) = rx.recv().await {
        if idle_until.is_some_and(|until| Instant::now() < until) {
            dropped.fetch_add(1, Ordering::Relaxed);
            continue;
        }
        match send_line(&socket, &path, &line) {
            Ok(()) => {
                if !listening {
                    tracing::debug!(path = %path.display(), "sending request lines to netwatch");
                    listening = true;
                }
                idle_until = None;
            }
            Err(error) => {
                dropped.fetch_add(1, Ordering::Relaxed);
                // A full socket buffer only costs this line; nobody
                // listening is worth a pause before trying again.
                if error.kind() != ErrorKind::WouldBlock {
                    if listening {
                        tracing::debug!(path = %path.display(), %error, "netwatch stopped taking request lines");
                    }
                    listening = false;
                    idle_until = Some(Instant::now() + IDLE_SOCKET_RETRY);
                }
            }
        }
    }
}

fn send_line(socket: &UnixDatagram, path: &Path, line: &RequestLine) -> std::io::Result<()> {
    socket.send_to(&line.encode(), path).map(|_| ())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::events::MAX_FIELD_CHARS;
    use std::net::Ipv4Addr;
    use std::sync::Mutex;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::TcpListener;
    use tokio::net::UnixDatagram as TokioDatagram;

    fn ban(target: &str, expires: Option<i64>) -> Ban {
        Ban {
            target: target.to_string(),
            expires,
        }
    }

    fn verdict_for(index: &Index, ip: &str, now: i64) -> Vec<String> {
        index.evaluate(ip.parse().unwrap(), now).reason_strings()
    }

    #[test]
    fn bans_become_a_blocking_list_without_the_expired_ones() {
        let now = 1_800_000_000;
        let bans = [
            ban("203.0.113.7", None),
            ban("198.51.100.0/24", Some(now + 60)),
            ban("192.0.2.1", Some(now - 1)),
            ban("not an address", None),
            ban("2001:db8::/32", None),
        ];
        let list = bans_list(&bans, now, now).unwrap();
        assert_eq!(list.source, SOURCE);
        assert_eq!(list.verdict, ListVerdict::Block);
        let index = Index::local(&[list]);
        let blocked = index.evaluate("203.0.113.7".parse().unwrap(), now);
        assert_eq!(blocked.action, Action::Block);
        assert_eq!(blocked.reason_strings(), ["netwatch (203.0.113.7/32)"]);
        assert_eq!(
            verdict_for(&index, "198.51.100.9", now),
            ["netwatch (198.51.100.0/24)"]
        );
        assert_eq!(
            verdict_for(&index, "2001:db8::1", now),
            ["netwatch (2001:db8::/32)"]
        );
        assert!(verdict_for(&index, "192.0.2.1", now).is_empty());
        assert_eq!(next_expiry(&bans, now), Some(now + 60));
        assert!(bans_list(&[ban("192.0.2.1", Some(now))], now, now).is_none());
        assert!(bans_list(&[], now, now).is_none());
    }

    #[test]
    fn the_api_response_parses_with_its_extra_fields() {
        let body = r#"[
            {"target":"203.0.113.7","created":1793000000,"expires":1793086400,"reason":"port scan","source":"auto","dropped":12},
            {"target":"198.51.100.0/24","created":1793000000,"expires":null,"reason":"manual","source":"manual","dropped":0}
        ]"#;
        let bans: Vec<Ban> = serde_json::from_str(body).unwrap();
        assert_eq!(
            bans,
            [
                ban("203.0.113.7", Some(1_793_086_400)),
                ban("198.51.100.0/24", None)
            ]
        );
    }

    #[test]
    fn backoff_doubles_up_to_the_cap() {
        let reader = BansReader::new(&NetwatchConfig::default(), Timing::default());
        assert_eq!(reader.url, "http://127.0.0.1:9180/api/bans");
        assert_eq!(reader.delay(0), POLL_INTERVAL);
        assert_eq!(reader.delay(1), POLL_INTERVAL);
        assert_eq!(reader.delay(2), POLL_INTERVAL * 2);
        assert_eq!(reader.delay(20), MAX_BACKOFF);
    }

    #[test]
    fn debug_hides_the_token() {
        let config = NetwatchConfig {
            api_token: Some("secret-token".into()),
            ..NetwatchConfig::default()
        };
        assert!(!format!("{config:?}").contains("secret-token"));
    }

    /// A netwatch API that serves `body` on `/api/bans` and records the
    /// `Authorization` header of each request.
    async fn fake_netwatch(
        body: &'static str,
    ) -> (String, tokio::task::JoinHandle<()>, Arc<Mutex<Vec<String>>>) {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        let seen = Arc::new(Mutex::new(Vec::new()));
        let recorded = seen.clone();
        let server = tokio::spawn(async move {
            loop {
                let Ok((mut stream, _)) = listener.accept().await else {
                    return;
                };
                let mut request = vec![0; 4096];
                let n = stream.read(&mut request).await.unwrap_or(0);
                let head = String::from_utf8_lossy(&request[..n]).to_string();
                let auth = head
                    .lines()
                    .find_map(|l| l.strip_prefix("authorization: "))
                    .unwrap_or("")
                    .to_string();
                recorded.lock().unwrap().push(auth);
                let response = if head.starts_with("GET /api/bans ") {
                    format!(
                        "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",
                        body.len()
                    )
                } else {
                    "HTTP/1.1 404 Not Found\r\ncontent-length: 0\r\nconnection: close\r\n\r\n"
                        .to_string()
                };
                let _ = stream.write_all(response.as_bytes()).await;
            }
        });
        (url, server, seen)
    }

    async fn wait_for(local: &ArcSwapOption<Index>, present: bool) -> bool {
        for _ in 0..200 {
            if local.load().is_some() == present {
                return true;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        false
    }

    #[tokio::test]
    async fn the_reader_loads_bans_and_clears_them_once_netwatch_is_gone() {
        let (url, server, seen) =
            fake_netwatch(r#"[{"target":"203.0.113.0/24","expires":null}]"#).await;
        let config = NetwatchConfig {
            api_url: format!("{url}/"),
            api_token: Some("tok".into()),
            ..NetwatchConfig::default()
        };
        let timing = Timing {
            poll: Duration::from_millis(40),
            stale: Duration::from_millis(250),
            max_backoff: Duration::from_millis(40),
        };
        let local = Arc::new(ArcSwapOption::empty());
        let reader = tokio::spawn(BansReader::new(&config, timing).run(Arc::downgrade(&local)));

        assert!(wait_for(&local, true).await, "the bans were never loaded");
        let index = local.load_full().unwrap();
        let now = crate::unix_now() as i64;
        assert_eq!(
            verdict_for(&index, "203.0.113.9", now),
            ["netwatch (203.0.113.0/24)"]
        );
        assert_eq!(seen.lock().unwrap()[0], "Bearer tok");

        // netwatch goes away: the bans hold for the stale limit, then go.
        server.abort();
        let _ = server.await;
        let gone = Instant::now();
        assert!(wait_for(&local, false).await, "stale bans were kept");
        assert!(gone.elapsed() >= Duration::from_millis(200));

        drop(local);
        tokio::time::timeout(Duration::from_secs(2), reader)
            .await
            .expect("the reader stops once the index is dropped")
            .unwrap();
    }

    #[tokio::test]
    async fn the_reader_expires_a_ban_between_polls() {
        let now = crate::unix_now() as i64;
        let body: &'static str = Box::leak(
            format!(r#"[{{"target":"203.0.113.7","expires":{}}}]"#, now + 1).into_boxed_str(),
        );
        let (url, _server, _) = fake_netwatch(body).await;
        let config = NetwatchConfig {
            api_url: url,
            ..NetwatchConfig::default()
        };
        // Polls are far apart: only the expiry can clear the ban.
        let timing = Timing {
            poll: Duration::from_secs(60),
            stale: Duration::from_secs(60),
            max_backoff: Duration::from_secs(60),
        };
        let local = Arc::new(ArcSwapOption::empty());
        tokio::spawn(BansReader::new(&config, timing).run(Arc::downgrade(&local)));
        assert!(wait_for(&local, true).await);
        for _ in 0..300 {
            if local.load().is_none() {
                return;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        panic!("the ban outlived its expiry");
    }

    #[tokio::test]
    async fn without_netwatch_the_index_stays_empty() {
        // A port nothing listens on.
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        drop(listener);
        let config = NetwatchConfig {
            api_url: url,
            ..NetwatchConfig::default()
        };
        let timing = Timing {
            poll: Duration::from_millis(10),
            stale: Duration::from_millis(10),
            max_backoff: Duration::from_millis(20),
        };
        let local = Arc::new(ArcSwapOption::empty());
        tokio::spawn(BansReader::new(&config, timing).run(Arc::downgrade(&local)));
        tokio::time::sleep(Duration::from_millis(100)).await;
        assert!(local.load().is_none());
    }

    #[tokio::test]
    async fn disabled_spawn_starts_nothing() {
        let netwatch = spawn(NetwatchConfig {
            enabled: false,
            ..NetwatchConfig::default()
        });
        assert!(netwatch.local.load().is_none());
        netwatch.sender.send(line("/"));
        assert_eq!(netwatch.sender.dropped(), 0);
    }

    fn line(path: &str) -> RequestLine {
        RequestLine {
            ts_ms: 1_793_000_000_123,
            ip: IpAddr::V4(Ipv4Addr::new(203, 0, 113, 7)),
            method: "GET".into(),
            host: "app.example.com".into(),
            path: path.into(),
            user_agent: "curl/8.5.0".into(),
            status: 404,
            tls: true,
            ja4: Some("t13d1516h2_8daaf6152771_02713d6af862".into()),
            verdict: "log".into(),
            acted: None,
            signature: Some("secrets-probe".into()),
        }
    }

    #[test]
    fn a_line_is_versioned_json_and_a_newline() {
        let bytes = line("/.env?x=1").encode();
        assert_eq!(bytes.last(), Some(&b'\n'));
        let value: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(value["v"], 1);
        assert_eq!(value["ip"], "203.0.113.7");
        assert_eq!(value["path"], "/.env?x=1");
        assert_eq!(value["acted"], serde_json::Value::Null);
        let back: RequestLine = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(back, line("/.env?x=1"));
    }

    #[test]
    fn a_line_is_built_from_the_request_and_bounded() {
        let long = format!("/{}", "a".repeat(2000));
        let request = Request {
            ip: Some("203.0.113.7".parse().unwrap()),
            ja4: None,
            source: "direct",
            user_agent: "curl/8.5.0",
            method: "GET",
            path: "/",
            path_and_query: &long,
            accept: "*/*",
            headers: &[],
            clearance: None,
            bypass_token: None,
            tls: false,
        };
        let line = RequestLine::new(&request, "app.example.com", 200, None, 5).unwrap();
        assert_eq!(line.path.chars().count(), MAX_FIELD_CHARS);
        assert_eq!(line.verdict, "pass");
        assert_eq!(line.acted, None);
        assert!(!line.tls);
        let anonymous = Request {
            ip: None,
            ..request
        };
        assert!(RequestLine::new(&anonymous, "h", 200, None, 5).is_none());
    }

    #[tokio::test]
    async fn lines_reach_a_listening_socket() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("requests.sock");
        let receiver = TokioDatagram::bind(&path).unwrap();
        let (sender, rx) = RequestSender::channel(16);
        tokio::spawn(deliver(rx, path.clone(), sender.dropped.clone()));

        sender.send(line("/.git/HEAD"));
        let mut buf = vec![0; 65536];
        let n = tokio::time::timeout(Duration::from_secs(2), receiver.recv(&mut buf))
            .await
            .expect("no line arrived")
            .unwrap();
        let got: RequestLine = serde_json::from_slice(&buf[..n]).unwrap();
        assert_eq!(got, line("/.git/HEAD"));
        assert_eq!(sender.dropped(), 0);
    }

    #[tokio::test]
    async fn lines_are_dropped_when_nobody_listens_or_the_queue_is_full() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("nobody.sock");
        let (sender, rx) = RequestSender::channel(16);
        tokio::spawn(deliver(rx, path, sender.dropped.clone()));
        sender.send(line("/"));
        for _ in 0..200 {
            if sender.dropped() == 1 {
                break;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
        assert_eq!(sender.dropped(), 1);

        // No task draining the queue: the second line finds it full.
        let (full, _rx) = RequestSender::channel(1);
        full.send(line("/a"));
        full.send(line("/b"));
        assert_eq!(full.dropped(), 1);
    }
}
