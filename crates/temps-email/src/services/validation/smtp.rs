//! SMTP-level mailbox probing.
//!
//! We open a plain TCP connection to a domain's mail exchanger and run the
//! SMTP envelope handshake up to `RCPT TO` *without ever sending `DATA`* —
//! i.e. we never deliver a message. The server's reply to `RCPT TO` tells us
//! whether the mailbox is deliverable.
//!
//! Catch-all detection: we additionally probe a random, almost-certainly
//! non-existent local-part. If that is also accepted, the domain accepts all
//! addresses and a "deliverable" result for the real address is unreliable.
//!
//! Optional SOCKS5 proxying (`ProxyConfig`) routes the TCP connection through
//! a proxy — necessary because many networks block outbound port 25.

use std::time::Duration;

use thiserror::Error;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;
use tokio::time::timeout;
use tracing::debug;

use super::ProxyConfig;

const MAX_MX_HOSTS_PER_PROBE: usize = 5;

#[derive(Debug, Error)]
enum SmtpConnectError {
    #[error("DNS resolution of {address} timed out")]
    DnsTimeout { address: String },
    #[error("Failed to resolve {address}: {source}")]
    Resolve {
        address: String,
        #[source]
        source: std::io::Error,
    },
    #[error("No addresses found for {address}")]
    NoAddresses { address: String },
    #[error("Refusing to probe {host} at {ip}: {source}")]
    BlockedAddress {
        host: String,
        ip: std::net::IpAddr,
        #[source]
        source: temps_core::url_validation::UrlValidationError,
    },
    #[error("TCP connect to validated addresses for {host} timed out")]
    TcpTimeout { host: String },
    #[error("TCP connect to validated addresses for {host} failed: {source}")]
    TcpConnect {
        host: String,
        #[source]
        source: std::io::Error,
    },
    #[error("SOCKS5 proxy credentials must provide both username and password, or neither")]
    InvalidProxyCredentials,
    #[error("SOCKS5 proxy DNS resolution timed out")]
    ProxyDnsTimeout,
    #[error("Failed to resolve SOCKS5 proxy: {source}")]
    ProxyResolve {
        #[source]
        source: std::io::Error,
    },
    #[error("SOCKS5 proxy resolved without any addresses")]
    ProxyNoAddresses,
    #[error("SOCKS5 connect to validated MX addresses for {host} timed out")]
    SocksTimeout { host: String },
    #[error("SOCKS5 connect to validated MX addresses for {host} failed: {source}")]
    SocksConnect {
        host: String,
        #[source]
        source: tokio_socks::Error,
    },
}

#[derive(Debug, Error)]
enum SmtpProbeError {
    #[error(transparent)]
    Connect(#[from] SmtpConnectError),
    #[error("SMTP greeting from {host} was not successful: {reply}")]
    GreetingRejected { host: String, reply: String },
    #[error("SMTP {operation} timed out")]
    OperationTimeout { operation: &'static str },
    #[error("SMTP {operation} failed: {source}")]
    OperationIo {
        operation: &'static str,
        #[source]
        source: std::io::Error,
    },
    #[error("SMTP connection closed while reading {operation}")]
    ConnectionClosed { operation: &'static str },
    #[error("SMTP reply for {operation} exceeded 64 KiB")]
    ReplyTooLarge { operation: &'static str },
}

/// Result of probing a single mailbox over SMTP.
#[derive(Debug, Clone, Default)]
pub struct SmtpProbe {
    pub can_connect: bool,
    pub is_deliverable: bool,
    pub is_disabled: bool,
    pub has_full_inbox: bool,
    pub is_catch_all: bool,
    pub error: Option<String>,
}

/// Settings for an SMTP probe.
pub struct SmtpProbeConfig<'a> {
    /// MX hosts to try, in preference order.
    pub mx_hosts: &'a [String],
    /// Address being verified.
    pub to_email: &'a str,
    /// Envelope sender used in `MAIL FROM`.
    pub from_email: &'a str,
    /// Name announced in `EHLO`.
    pub hello_name: &'a str,
    /// Per-operation timeout.
    pub timeout: Duration,
    /// Absolute deadline for the complete probe across all MX hosts.
    pub deadline: Duration,
    /// Optional SOCKS5 proxy.
    pub proxy: Option<&'a ProxyConfig>,
}

/// A duplex stream we can run SMTP over — either a direct TCP connection or
/// one tunnelled through a SOCKS5 proxy. Both implement `AsyncRead`/`Write`.
enum SmtpStream {
    Direct(TcpStream),
    Proxied(tokio_socks::tcp::Socks5Stream<TcpStream>),
}

impl SmtpStream {
    async fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        match self {
            SmtpStream::Direct(s) => s.read(buf).await,
            SmtpStream::Proxied(s) => s.read(buf).await,
        }
    }
    async fn write_all(&mut self, buf: &[u8]) -> std::io::Result<()> {
        match self {
            SmtpStream::Direct(s) => s.write_all(buf).await,
            SmtpStream::Proxied(s) => s.write_all(buf).await,
        }
    }
}

/// Probe a mailbox. Tries each MX host until one accepts a TCP connection;
/// the first reachable host decides the result.
pub async fn probe_mailbox(config: SmtpProbeConfig<'_>) -> SmtpProbe {
    let deadline = config.deadline;
    match enforce_deadline(deadline, probe_mailbox_before_deadline(&config)).await {
        Ok(probe) => probe,
        Err(_) => SmtpProbe {
            can_connect: false,
            error: Some(format!(
                "SMTP mailbox probe exceeded its {}ms deadline",
                deadline.as_millis()
            )),
            ..Default::default()
        },
    }
}

async fn enforce_deadline<F>(
    deadline: Duration,
    probe: F,
) -> Result<SmtpProbe, tokio::time::error::Elapsed>
where
    F: std::future::Future<Output = SmtpProbe>,
{
    timeout(deadline, probe).await
}

async fn probe_mailbox_before_deadline(config: &SmtpProbeConfig<'_>) -> SmtpProbe {
    if config.mx_hosts.is_empty() {
        return SmtpProbe {
            error: Some("no MX hosts to probe".to_string()),
            ..Default::default()
        };
    }

    let mut last_error = None;
    for host in config.mx_hosts.iter().take(MAX_MX_HOSTS_PER_PROBE) {
        match probe_single_host(host, config).await {
            Ok(probe) => return probe,
            Err(e) => {
                debug!("SMTP probe via {host} failed: {e}");
                last_error = Some(e.to_string());
            }
        }
    }

    SmtpProbe {
        can_connect: false,
        error: last_error.or_else(|| Some("all MX hosts unreachable".to_string())),
        ..Default::default()
    }
}

/// Run the full SMTP conversation against one MX host. Returns `Err` only
/// when the host could not be reached at all (so the caller can try the next
/// MX); a reachable host that rejects the mailbox is still `Ok`.
async fn probe_single_host(
    host: &str,
    config: &SmtpProbeConfig<'_>,
) -> Result<SmtpProbe, SmtpProbeError> {
    let addr = format!("{host}:25");
    let mut stream = connect(host, &addr, config).await?;

    // Greeting.
    let greeting = read_reply(&mut stream, "server greeting", config.timeout).await?;
    if !greeting.starts_with('2') {
        return Err(SmtpProbeError::GreetingRejected {
            host: host.to_string(),
            reply: greeting,
        });
    }

    // EHLO.
    send(
        &mut stream,
        &format!("EHLO {}\r\n", config.hello_name),
        "EHLO write",
        config.timeout,
    )
    .await?;
    let _ = read_reply(&mut stream, "EHLO reply", config.timeout).await?;

    // MAIL FROM — envelope sender.
    send(
        &mut stream,
        &format!("MAIL FROM:<{}>\r\n", config.from_email),
        "MAIL FROM write",
        config.timeout,
    )
    .await?;
    let mail_reply = read_reply(&mut stream, "MAIL FROM reply", config.timeout).await?;
    if !mail_reply.starts_with('2') {
        // Connected, but the server won't take our envelope sender — we can't
        // determine deliverability. Reachable, but Unknown.
        let _ = send(&mut stream, "QUIT\r\n", "QUIT write", config.timeout).await;
        return Ok(SmtpProbe {
            can_connect: true,
            error: Some(format!("MAIL FROM rejected: {mail_reply}")),
            ..Default::default()
        });
    }

    // RCPT TO — the real address under test.
    let real = rcpt_outcome(&mut stream, config.to_email, config.timeout).await?;

    // Catch-all probe: a random local-part that should not exist.
    let domain = config.to_email.rsplit('@').next().unwrap_or_default();
    let random_addr = format!("temps-probe-{}@{}", random_token(), domain);
    let catch_all = match rcpt_outcome(&mut stream, &random_addr, config.timeout).await {
        Ok(o) => o.deliverable,
        Err(_) => false,
    };

    let _ = send(&mut stream, "QUIT\r\n", "QUIT write", config.timeout).await;

    Ok(SmtpProbe {
        can_connect: true,
        // On a catch-all domain a 250 for the real address is meaningless.
        is_deliverable: real.deliverable && !catch_all,
        is_disabled: real.disabled,
        has_full_inbox: real.full_inbox,
        is_catch_all: catch_all,
        error: None,
    })
}

/// Per-`RCPT TO` interpretation.
struct RcptOutcome {
    deliverable: bool,
    disabled: bool,
    full_inbox: bool,
}

/// Send a single `RCPT TO` and classify the reply.
async fn rcpt_outcome(
    stream: &mut SmtpStream,
    address: &str,
    op_timeout: Duration,
) -> Result<RcptOutcome, SmtpProbeError> {
    send(
        stream,
        &format!("RCPT TO:<{address}>\r\n",),
        "RCPT TO write",
        op_timeout,
    )
    .await?;
    let reply = read_reply(stream, "RCPT TO reply", op_timeout).await?;
    Ok(classify_rcpt_reply(&reply))
}

/// Map an SMTP `RCPT TO` reply to a deliverability outcome.
///
/// - `2xx` → mailbox accepted (deliverable).
/// - `552` / "quota"/"full" wording → mailbox exists but inbox is full.
/// - `5xx` with "disabled"/"suspended" wording → mailbox disabled.
/// - other `5xx` → mailbox does not exist (not deliverable, not disabled).
/// - `4xx` → temporary failure; treated as not-deliverable / Unknown upstream.
fn classify_rcpt_reply(reply: &str) -> RcptOutcome {
    let lower = reply.to_ascii_lowercase();
    if reply.starts_with('2') {
        return RcptOutcome {
            deliverable: true,
            disabled: false,
            full_inbox: false,
        };
    }
    let full_inbox = reply.starts_with("552")
        || lower.contains("quota")
        || lower.contains("inbox is full")
        || lower.contains("mailbox full");
    let disabled = lower.contains("disabled")
        || lower.contains("suspended")
        || lower.contains("inactive")
        || lower.contains("not in use");
    RcptOutcome {
        deliverable: false,
        disabled,
        full_inbox,
    }
}

/// Open the connection — direct or via SOCKS5 — applying the connect timeout.
///
/// `host` is a domain's own MX record, resolved from whatever email address
/// a caller submits for validation — attacker-controlled input. For the
/// Temps resolves the MX host itself, so a domain whose MX record points at an
/// internal/private/link-local/cloud-metadata address would otherwise let an
/// authenticated user make this server probe its own internal network on
/// port 25. The optional SOCKS proxy is operator-configured and cannot be
/// overridden by API callers, but the MX target is still locally validated
/// and passed to the proxy as a pinned IP address.
///
/// The direct path resolves `host` exactly once and connects to that pinned
/// `SocketAddr` — it deliberately does NOT validate via
/// `validate_domain_async(host)` and then separately `TcpStream::connect(addr)`,
/// since that would re-resolve the hostname a second time. Because `host` is
/// attacker-controlled, an attacker's DNS server can answer a public IP for
/// the first (validation) lookup and a private/internal one for the second
/// (connect) lookup — a classic DNS-rebinding TOCTOU that fully defeats the
/// validation. Resolving once and validating the address actually used
/// closes that window.
async fn connect(
    host: &str,
    addr: &str,
    config: &SmtpProbeConfig<'_>,
) -> Result<SmtpStream, SmtpConnectError> {
    let resolved = resolve_addresses(addr, config.timeout).await?;
    validate_external_addresses(host, &resolved)?;

    match config.proxy {
        Some(proxy) => {
            let proxy_addr = format!("{}:{}", proxy.host, proxy.port);
            let credentials = proxy_credentials(proxy)?;
            // Resolve the trusted operator proxy once to avoid a second DNS
            // lookup between selection and connection. Private proxy addresses
            // remain supported because this setting is not exposed by the API.
            let proxy_addresses = resolve_proxy_addresses(&proxy_addr, config.timeout).await?;
            let mut last_error = None;

            for target in &resolved {
                let connect = async {
                    match credentials {
                        Some((user, pass)) => {
                            tokio_socks::tcp::Socks5Stream::connect_with_password(
                                proxy_addresses.as_slice(),
                                *target,
                                user,
                                pass,
                            )
                            .await
                        }
                        None => {
                            tokio_socks::tcp::Socks5Stream::connect(
                                proxy_addresses.as_slice(),
                                *target,
                            )
                            .await
                        }
                    }
                };

                match timeout(config.timeout, connect).await {
                    Ok(Ok(stream)) => return Ok(SmtpStream::Proxied(stream)),
                    Ok(Err(source)) => {
                        last_error = Some(SmtpConnectError::SocksConnect {
                            host: host.to_string(),
                            source,
                        })
                    }
                    Err(_) => {
                        last_error = Some(SmtpConnectError::SocksTimeout {
                            host: host.to_string(),
                        })
                    }
                }
            }

            Err(last_error.unwrap_or(SmtpConnectError::NoAddresses {
                address: addr.to_string(),
            }))
        }
        None => timeout(config.timeout, TcpStream::connect(resolved.as_slice()))
            .await
            .map_err(|_| SmtpConnectError::TcpTimeout {
                host: host.to_string(),
            })?
            .map(SmtpStream::Direct)
            .map_err(|source| SmtpConnectError::TcpConnect {
                host: host.to_string(),
                source,
            }),
    }
}

fn proxy_credentials(proxy: &ProxyConfig) -> Result<Option<(&str, &str)>, SmtpConnectError> {
    match (&proxy.username, &proxy.password) {
        (Some(username), Some(password)) => Ok(Some((username.as_str(), password.as_str()))),
        (None, None) => Ok(None),
        _ => Err(SmtpConnectError::InvalidProxyCredentials),
    }
}

async fn resolve_proxy_addresses(
    proxy_addr: &str,
    op_timeout: Duration,
) -> Result<Vec<std::net::SocketAddr>, SmtpConnectError> {
    let resolved: Vec<std::net::SocketAddr> =
        timeout(op_timeout, tokio::net::lookup_host(proxy_addr))
            .await
            .map_err(|_| SmtpConnectError::ProxyDnsTimeout)?
            .map_err(|source| SmtpConnectError::ProxyResolve { source })?
            .collect();

    if resolved.is_empty() {
        return Err(SmtpConnectError::ProxyNoAddresses);
    }

    Ok(resolved)
}

async fn resolve_addresses(
    addr: &str,
    op_timeout: Duration,
) -> Result<Vec<std::net::SocketAddr>, SmtpConnectError> {
    let resolved: Vec<std::net::SocketAddr> = timeout(op_timeout, tokio::net::lookup_host(addr))
        .await
        .map_err(|_| SmtpConnectError::DnsTimeout {
            address: addr.to_string(),
        })?
        .map_err(|source| SmtpConnectError::Resolve {
            address: addr.to_string(),
            source,
        })?
        .collect();

    if resolved.is_empty() {
        return Err(SmtpConnectError::NoAddresses {
            address: addr.to_string(),
        });
    }

    Ok(resolved)
}

fn validate_external_addresses(
    host: &str,
    resolved: &[std::net::SocketAddr],
) -> Result<(), SmtpConnectError> {
    // Reject the entire DNS result if any address is blocked. A mixed
    // public/private response must never get a free choice of which address
    // validation sees.
    for socket_addr in resolved {
        let validation = match socket_addr.ip() {
            std::net::IpAddr::V4(ip) => temps_core::url_validation::validate_ipv4(&ip),
            std::net::IpAddr::V6(ip) => temps_core::url_validation::validate_ipv6(&ip),
        };
        if let Err(source) = validation {
            return Err(SmtpConnectError::BlockedAddress {
                host: host.to_string(),
                ip: socket_addr.ip(),
                source,
            });
        }
    }

    Ok(())
}

/// Write a command, bounded by the operation timeout.
async fn send(
    stream: &mut SmtpStream,
    cmd: &str,
    operation: &'static str,
    op_timeout: Duration,
) -> Result<(), SmtpProbeError> {
    timeout(op_timeout, stream.write_all(cmd.as_bytes()))
        .await
        .map_err(|_| SmtpProbeError::OperationTimeout { operation })?
        .map_err(|source| SmtpProbeError::OperationIo { operation, source })
}

/// Read one SMTP reply. Handles multi-line replies (`250-…` continuation
/// lines) by reading until a line whose 4th byte is a space. Returns the
/// final line, which carries the authoritative status code.
async fn read_reply(
    stream: &mut SmtpStream,
    operation: &'static str,
    op_timeout: Duration,
) -> Result<String, SmtpProbeError> {
    let mut buf = Vec::with_capacity(512);
    let mut chunk = [0u8; 512];

    loop {
        let n = timeout(op_timeout, stream.read(&mut chunk))
            .await
            .map_err(|_| SmtpProbeError::OperationTimeout { operation })?
            .map_err(|source| SmtpProbeError::OperationIo { operation, source })?;
        if n == 0 {
            return Err(SmtpProbeError::ConnectionClosed { operation });
        }
        buf.extend_from_slice(&chunk[..n]);

        // A complete reply ends with a line of the form "NNN <text>\r\n"
        // (space after the code, not a hyphen).
        if let Some(last_line) = last_complete_line(&buf) {
            if is_final_reply_line(&last_line) {
                return Ok(last_line);
            }
        }
        if buf.len() > 64 * 1024 {
            return Err(SmtpProbeError::ReplyTooLarge { operation });
        }
    }
}

/// Return the last CRLF-terminated line in `buf`, if any.
fn last_complete_line(buf: &[u8]) -> Option<String> {
    let text = String::from_utf8_lossy(buf);
    let trimmed = text.trim_end_matches(['\r', '\n']);
    if !text.ends_with('\n') {
        return None;
    }
    trimmed.rsplit("\r\n").next().map(|s| s.to_string())
}

/// A final SMTP reply line has a space (not `-`) as its 4th character.
fn is_final_reply_line(line: &str) -> bool {
    let b = line.as_bytes();
    b.len() >= 4 && b[0].is_ascii_digit() && b[3] == b' '
}

/// A short random token for the catch-all probe local-part.
fn random_token() -> String {
    // Derive from a UUIDv4 — no extra RNG dependency, plenty of entropy.
    uuid::Uuid::new_v4().simple().to_string()[..16].to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_classify_deliverable() {
        let o = classify_rcpt_reply("250 2.1.5 OK");
        assert!(o.deliverable && !o.disabled && !o.full_inbox);
    }

    #[test]
    fn test_classify_nonexistent() {
        let o = classify_rcpt_reply("550 5.1.1 user unknown");
        assert!(!o.deliverable && !o.disabled);
    }

    #[test]
    fn test_classify_full_inbox() {
        assert!(classify_rcpt_reply("552 mailbox full").full_inbox);
        assert!(classify_rcpt_reply("450 4.2.2 quota exceeded").full_inbox);
    }

    #[test]
    fn test_classify_disabled() {
        assert!(classify_rcpt_reply("550 5.2.1 mailbox disabled").disabled);
        assert!(classify_rcpt_reply("550 account suspended").disabled);
    }

    #[test]
    fn test_final_reply_line() {
        assert!(is_final_reply_line("250 OK"));
        assert!(!is_final_reply_line("250-PIPELINING"));
        assert!(!is_final_reply_line("foo"));
    }

    #[test]
    fn test_last_complete_line_multiline() {
        let buf = b"250-PIPELINING\r\n250-SIZE 1024\r\n250 HELP\r\n";
        assert_eq!(last_complete_line(buf), Some("250 HELP".to_string()));
    }

    #[test]
    fn test_last_complete_line_incomplete() {
        // No trailing newline → reply not yet complete.
        assert_eq!(last_complete_line(b"250 HELP"), None);
    }

    #[test]
    fn test_random_token_unique() {
        assert_ne!(random_token(), random_token());
        assert_eq!(random_token().len(), 16);
    }

    fn test_config() -> SmtpProbeConfig<'static> {
        SmtpProbeConfig {
            mx_hosts: &[],
            to_email: "test@example.com",
            from_email: "noreply@temps.sh",
            hello_name: "temps.sh",
            timeout: Duration::from_secs(2),
            deadline: Duration::from_secs(5),
            proxy: None,
        }
    }

    #[tokio::test]
    async fn test_connect_rejects_private_ip_host() {
        let config = test_config();
        match connect("10.0.0.5", "10.0.0.5:25", &config).await {
            Ok(_) => panic!("must refuse an RFC 1918 private host"),
            Err(e) => assert!(e.to_string().contains("Refusing to probe"), "got: {e}"),
        }
    }

    #[tokio::test]
    async fn test_connect_rejects_loopback_host() {
        let config = test_config();
        match connect("127.0.0.1", "127.0.0.1:25", &config).await {
            Ok(_) => panic!("must refuse a loopback host"),
            Err(e) => assert!(e.to_string().contains("Refusing to probe"), "got: {e}"),
        }
    }

    #[tokio::test]
    async fn test_connect_rejects_cloud_metadata_host() {
        let config = test_config();
        match connect("169.254.169.254", "169.254.169.254:25", &config).await {
            Ok(_) => panic!("must refuse the cloud metadata address"),
            Err(e) => assert!(e.to_string().contains("Refusing to probe"), "got: {e}"),
        }
    }

    #[test]
    fn test_validate_external_addresses_rejects_mixed_dns_answers() {
        let addresses = [
            "1.1.1.1:25".parse().unwrap(),
            "10.0.0.5:25".parse().unwrap(),
        ];

        let error = validate_external_addresses("mx.attacker.test", &addresses)
            .expect_err("a single blocked address must reject the complete DNS result");
        assert!(error.to_string().contains("10.0.0.5"), "got: {error}");
    }

    #[test]
    fn test_validate_external_addresses_rejects_ipv4_mapped_ipv6() {
        for address in [
            "[::ffff:127.0.0.1]:25",
            "[::ffff:10.0.0.5]:25",
            "[::ffff:169.254.169.254]:25",
            "[::ffff:100.64.0.1]:25",
        ] {
            let addresses = [address.parse().unwrap()];
            assert!(
                validate_external_addresses("mx.attacker.test", &addresses).is_err(),
                "must reject mapped address {address}"
            );
        }
    }

    #[test]
    fn test_validate_external_addresses_accepts_public_ipv4_and_ipv6() {
        let addresses = [
            "1.1.1.1:25".parse().unwrap(),
            "[2606:4700:4700::1111]:25".parse().unwrap(),
        ];
        assert!(validate_external_addresses("mx.example.com", &addresses).is_ok());
    }

    #[tokio::test]
    async fn test_operator_proxy_does_not_bypass_target_validation() {
        let proxy = ProxyConfig {
            host: "127.0.0.1".to_string(),
            port: 1080,
            username: None,
            password: None,
        };
        let config = SmtpProbeConfig {
            proxy: Some(&proxy),
            ..test_config()
        };

        let error = match connect("127.0.0.1", "127.0.0.1:25", &config).await {
            Ok(_) => panic!("proxying must not bypass MX target validation"),
            Err(error) => error,
        };
        assert!(
            error.to_string().contains("Refusing to probe"),
            "got: {error}"
        );
    }

    #[test]
    fn test_proxy_credentials_reject_partial_configuration() {
        for (username, password) in [
            (Some("proxy-user".to_string()), None),
            (None, Some("proxy-password".to_string())),
        ] {
            let proxy = ProxyConfig {
                host: "internal-proxy.example".to_string(),
                port: 1080,
                username,
                password,
            };
            let error = proxy_credentials(&proxy)
                .expect_err("partial proxy credentials must not downgrade to anonymous SOCKS");
            let message = error.to_string();
            assert!(message.contains("both username and password"));
            assert!(!message.contains(&proxy.host));
            assert!(!message.contains(&proxy.port.to_string()));
        }
    }

    #[test]
    fn test_proxy_connection_errors_do_not_expose_endpoint() {
        let endpoint = "sensitive-proxy.internal:1080";
        let errors = [
            SmtpConnectError::ProxyDnsTimeout.to_string(),
            SmtpConnectError::ProxyResolve {
                source: std::io::Error::new(std::io::ErrorKind::NotFound, "host not found"),
            }
            .to_string(),
            SmtpConnectError::ProxyNoAddresses.to_string(),
            SmtpConnectError::SocksTimeout {
                host: "mx.example.com".to_string(),
            }
            .to_string(),
        ];
        for error in errors {
            assert!(!error.contains(endpoint));
            assert!(!error.contains("sensitive-proxy"));
            assert!(!error.contains("1080"));
        }
    }

    #[tokio::test]
    async fn test_probe_enforces_absolute_deadline() {
        let result = enforce_deadline(
            Duration::from_millis(1),
            std::future::pending::<SmtpProbe>(),
        )
        .await;

        assert!(
            result.is_err(),
            "pending probe must hit the absolute deadline"
        );
    }

    #[tokio::test]
    async fn test_probe_attempts_at_most_five_mx_hosts() {
        let mx_hosts = (1..=6)
            .map(|suffix| format!("10.0.0.{suffix}"))
            .collect::<Vec<_>>();
        let probe = probe_mailbox(SmtpProbeConfig {
            mx_hosts: &mx_hosts,
            to_email: "test@example.com",
            from_email: "noreply@temps.sh",
            hello_name: "temps.sh",
            timeout: Duration::from_secs(1),
            deadline: Duration::from_secs(2),
            proxy: None,
        })
        .await;

        let error = probe.error.expect("private MX hosts must be rejected");
        assert!(error.contains("10.0.0.5"), "got: {error}");
        assert!(
            !error.contains("10.0.0.6"),
            "sixth MX must not be attempted"
        );
    }

    #[tokio::test]
    async fn test_probe_no_mx_hosts() {
        let probe = probe_mailbox(SmtpProbeConfig {
            mx_hosts: &[],
            to_email: "test@example.com",
            from_email: "noreply@temps.sh",
            hello_name: "temps.sh",
            timeout: Duration::from_secs(1),
            deadline: Duration::from_secs(2),
            proxy: None,
        })
        .await;
        assert!(!probe.can_connect);
        assert!(probe.error.is_some());
    }
}
