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

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;
use tokio::time::timeout;
use tracing::debug;

use super::ProxyConfig;

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
    if config.mx_hosts.is_empty() {
        return SmtpProbe {
            error: Some("no MX hosts to probe".to_string()),
            ..Default::default()
        };
    }

    let mut last_error = None;
    for host in config.mx_hosts {
        match probe_single_host(host, &config).await {
            Ok(probe) => return probe,
            Err(e) => {
                debug!("SMTP probe via {host} failed: {e}");
                last_error = Some(e);
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
async fn probe_single_host(host: &str, config: &SmtpProbeConfig<'_>) -> Result<SmtpProbe, String> {
    let addr = format!("{host}:25");
    let mut stream = connect(host, &addr, config).await?;

    // Greeting.
    let greeting = read_reply(&mut stream, config.timeout).await?;
    if !greeting.starts_with('2') {
        return Err(format!("server greeting was not 2xx: {greeting}"));
    }

    // EHLO.
    send(
        &mut stream,
        &format!("EHLO {}\r\n", config.hello_name),
        config.timeout,
    )
    .await?;
    let _ = read_reply(&mut stream, config.timeout).await?;

    // MAIL FROM — envelope sender.
    send(
        &mut stream,
        &format!("MAIL FROM:<{}>\r\n", config.from_email),
        config.timeout,
    )
    .await?;
    let mail_reply = read_reply(&mut stream, config.timeout).await?;
    if !mail_reply.starts_with('2') {
        // Connected, but the server won't take our envelope sender — we can't
        // determine deliverability. Reachable, but Unknown.
        let _ = send(&mut stream, "QUIT\r\n", config.timeout).await;
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

    let _ = send(&mut stream, "QUIT\r\n", config.timeout).await;

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
) -> Result<RcptOutcome, String> {
    send(stream, &format!("RCPT TO:<{address}>\r\n",), op_timeout).await?;
    let reply = read_reply(stream, op_timeout).await?;
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
/// direct (non-proxied) path, the Temps server itself performs the DNS
/// resolution and TCP connect, so a domain whose MX record points at an
/// internal/private/link-local/cloud-metadata address would otherwise let an
/// authenticated user make this server probe its own internal network on
/// port 25. The proxied path is exempt: that proxy is operator-configured,
/// not attacker-controlled, and resolution there happens proxy-side.
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
async fn connect(host: &str, addr: &str, config: &SmtpProbeConfig<'_>) -> Result<SmtpStream, String> {
    match config.proxy {
        Some(proxy) => {
            let proxy_addr = format!("{}:{}", proxy.host, proxy.port);
            let connect = async {
                match (&proxy.username, &proxy.password) {
                    (Some(user), Some(pass)) => {
                        tokio_socks::tcp::Socks5Stream::connect_with_password(
                            proxy_addr.as_str(),
                            addr,
                            user.as_str(),
                            pass.as_str(),
                        )
                        .await
                    }
                    _ => tokio_socks::tcp::Socks5Stream::connect(proxy_addr.as_str(), addr).await,
                }
            };
            timeout(config.timeout, connect)
                .await
                .map_err(|_| format!("SOCKS5 connect to {addr} timed out"))?
                .map(SmtpStream::Proxied)
                .map_err(|e| format!("SOCKS5 connect to {addr} failed: {e}"))
        }
        None => {
            let resolved: Vec<std::net::SocketAddr> = timeout(
                config.timeout,
                tokio::net::lookup_host(addr),
            )
            .await
            .map_err(|_| format!("DNS resolution of {addr} timed out"))?
            .map_err(|e| format!("Failed to resolve {addr}: {e}"))?
            .collect();

            if resolved.is_empty() {
                return Err(format!("No addresses found for {addr}"));
            }

            // Reject the whole result if any resolved address is
            // blocked, not just skip it — an attacker's DNS response
            // mixing a public and a private IP shouldn't get a free
            // pick of which one validation "sees".
            for socket_addr in &resolved {
                let validation = match socket_addr.ip() {
                    std::net::IpAddr::V4(ip) => temps_core::url_validation::validate_ipv4(&ip),
                    std::net::IpAddr::V6(ip) => temps_core::url_validation::validate_ipv6(&ip),
                };
                if let Err(e) = validation {
                    return Err(format!("Refusing to probe {host}: {e}"));
                }
            }

            timeout(config.timeout, TcpStream::connect(resolved.as_slice()))
                .await
                .map_err(|_| format!("TCP connect to {addr} timed out"))?
                .map(SmtpStream::Direct)
                .map_err(|e| format!("TCP connect to {addr} failed: {e}"))
        }
    }
}

/// Write a command, bounded by the operation timeout.
async fn send(stream: &mut SmtpStream, cmd: &str, op_timeout: Duration) -> Result<(), String> {
    timeout(op_timeout, stream.write_all(cmd.as_bytes()))
        .await
        .map_err(|_| "SMTP write timed out".to_string())?
        .map_err(|e| format!("SMTP write failed: {e}"))
}

/// Read one SMTP reply. Handles multi-line replies (`250-…` continuation
/// lines) by reading until a line whose 4th byte is a space. Returns the
/// final line, which carries the authoritative status code.
async fn read_reply(stream: &mut SmtpStream, op_timeout: Duration) -> Result<String, String> {
    let mut buf = Vec::with_capacity(512);
    let mut chunk = [0u8; 512];

    loop {
        let n = timeout(op_timeout, stream.read(&mut chunk))
            .await
            .map_err(|_| "SMTP read timed out".to_string())?
            .map_err(|e| format!("SMTP read failed: {e}"))?;
        if n == 0 {
            return Err("SMTP connection closed by server".to_string());
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
            return Err("SMTP reply exceeded 64 KiB".to_string());
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
        assert_eq!(last_complete_line(b"250 HEL"), None);
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
            proxy: None,
        }
    }

    #[tokio::test]
    async fn test_connect_rejects_private_ip_host() {
        let config = test_config();
        match connect("10.0.0.5", "10.0.0.5:25", &config).await {
            Ok(_) => panic!("must refuse an RFC 1918 private host"),
            Err(e) => assert!(e.contains("Refusing to probe"), "got: {e}"),
        }
    }

    #[tokio::test]
    async fn test_connect_rejects_loopback_host() {
        let config = test_config();
        match connect("127.0.0.1", "127.0.0.1:25", &config).await {
            Ok(_) => panic!("must refuse a loopback host"),
            Err(e) => assert!(e.contains("Refusing to probe"), "got: {e}"),
        }
    }

    #[tokio::test]
    async fn test_connect_rejects_cloud_metadata_host() {
        let config = test_config();
        match connect("169.254.169.254", "169.254.169.254:25", &config).await {
            Ok(_) => panic!("must refuse the cloud metadata address"),
            Err(e) => assert!(e.contains("Refusing to probe"), "got: {e}"),
        }
    }

    #[tokio::test]
    async fn test_probe_no_mx_hosts() {
        let probe = probe_mailbox(SmtpProbeConfig {
            mx_hosts: &[],
            to_email: "test@example.com",
            from_email: "noreply@temps.sh",
            hello_name: "temps.sh",
            timeout: Duration::from_secs(1),
            proxy: None,
        })
        .await;
        assert!(!probe.can_connect);
        assert!(probe.error.is_some());
    }
}
