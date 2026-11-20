// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Running one check against a target outside Temps.
//!
//! Every connection goes to an address that passed the shared outbound
//! policy at probe time (`resolve_and_validate_domain`, the same policy as
//! `PublicProbeTarget`), so a monitor cannot be pointed at loopback, private
//! or metadata addresses, nor rebound to one later. Response bodies are read
//! only for a keyword check, at most 256 KiB, and never stored.

use std::net::{IpAddr, SocketAddr};
use std::sync::{Arc, LazyLock};
use std::time::{Duration, Instant};

use async_trait::async_trait;
use temps_core::url_validation::{
    resolve_and_validate_domain, validate_outbound_ip, UrlValidationError,
};
use tokio::net::TcpStream;

use super::spec::{
    normalize_dns_value, parse_status_pattern, CheckRequest, CheckSpec, DnsCheck, HttpCheck,
    Target, TcpCheck, TlsCheck, DEFAULT_TLS_WARN_DAYS, MAX_KEYWORD_BODY_BYTES,
};
use crate::services::outbound::{pinned_public_client, PublicProbeTarget};

pub const USER_AGENT: &str = "Temps-Status-Monitor/1.0";
/// Answers listed in a DNS failure message.
const MAX_LISTED_ANSWERS: usize = 5;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProbeStatus {
    Operational,
    Degraded,
    Down,
}

impl ProbeStatus {
    /// The value stored in `status_checks.status`.
    pub fn stored(self) -> &'static str {
        match self {
            Self::Operational => "operational",
            Self::Degraded => "degraded",
            Self::Down => "major_outage",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProbeOutcome {
    pub status: ProbeStatus,
    pub response_time_ms: Option<i32>,
    /// Why it is not operational, in words an operator can act on.
    pub message: Option<String>,
}

impl ProbeOutcome {
    fn up(elapsed: Duration) -> Self {
        Self {
            status: ProbeStatus::Operational,
            response_time_ms: Some(millis(elapsed)),
            message: None,
        }
    }

    fn down(elapsed: Option<Duration>, message: impl Into<String>) -> Self {
        Self {
            status: ProbeStatus::Down,
            response_time_ms: elapsed.map(millis),
            message: Some(message.into()),
        }
    }

    fn degraded(elapsed: Duration, message: impl Into<String>) -> Self {
        Self {
            status: ProbeStatus::Degraded,
            response_time_ms: Some(millis(elapsed)),
            message: Some(message.into()),
        }
    }
}

fn millis(elapsed: Duration) -> i32 {
    i32::try_from(elapsed.as_millis()).unwrap_or(i32::MAX)
}

fn seconds(duration: Duration) -> String {
    format!("{:.1} s", duration.as_secs_f64())
}

#[async_trait]
pub trait TargetProber: Send + Sync {
    async fn probe(
        &self,
        spec: &CheckSpec,
        target: &Target,
        request: &CheckRequest,
    ) -> ProbeOutcome;
}

/// Probes over the network.
#[derive(Debug, Default)]
pub struct NetworkProber;

#[async_trait]
impl TargetProber for NetworkProber {
    async fn probe(
        &self,
        spec: &CheckSpec,
        target: &Target,
        request: &CheckRequest,
    ) -> ProbeOutcome {
        let timeout = spec.timeout();
        let probe = async {
            match (spec, target) {
                (CheckSpec::Http(check), Target::Url(url)) => {
                    probe_http(check, url, request, timeout).await
                }
                (CheckSpec::Tcp(check), Target::HostPort { host, port }) => {
                    probe_tcp(check, host, *port, timeout).await
                }
                (CheckSpec::Tls(check), Target::HostPort { host, port }) => {
                    probe_tls(check, host, *port, timeout).await
                }
                (CheckSpec::Dns(check), Target::Name(name)) => {
                    probe_dns(check, name, timeout).await
                }
                _ => ProbeOutcome::down(None, "the stored target does not fit the check kind"),
            }
        };
        within(timeout, probe).await
    }
}

/// `probe`, bounded by the check's `timeout` from start to finish. Each probe
/// times its own connection or request, but resolving the target's address
/// comes first and has no deadline of its own, so a slow resolver would hold
/// up every monitor waiting in the same sweep.
async fn within(
    timeout: Duration,
    probe: impl std::future::Future<Output = ProbeOutcome>,
) -> ProbeOutcome {
    match tokio::time::timeout(timeout, probe).await {
        Ok(outcome) => outcome,
        Err(_) => ProbeOutcome::down(
            Some(timeout),
            format!(
                "no answer within {}, name resolution included",
                seconds(timeout)
            ),
        ),
    }
}

/// Whether the status counts as up under `check`.
pub fn status_is_expected(check: &HttpCheck, status: u16) -> bool {
    if check.expected_status.is_empty() {
        return (200..400).contains(&status);
    }
    check
        .expected_status
        .iter()
        .filter_map(|pattern| parse_status_pattern(pattern))
        .any(|pattern| pattern.matches(status))
}

fn contains(haystack: &[u8], needle: &[u8]) -> bool {
    !needle.is_empty()
        && haystack
            .windows(needle.len())
            .any(|window| window == needle)
}

/// Judge an HTTP answer. `body` is `None` when no keyword is checked.
pub fn judge_http(
    check: &HttpCheck,
    status: u16,
    body: Option<&[u8]>,
    elapsed: Duration,
) -> ProbeOutcome {
    if !status_is_expected(check, status) {
        let expected = if check.expected_status.is_empty() {
            "2xx or 3xx".to_string()
        } else {
            check.expected_status.join(", ")
        };
        return ProbeOutcome::down(Some(elapsed), format!("HTTP {status}, expected {expected}"));
    }
    if let (Some(keyword), Some(body)) = (&check.keyword, body) {
        let found = contains(body, keyword.text.as_bytes());
        if found != keyword.present {
            let message = if keyword.present {
                format!(
                    "HTTP {status}, but \"{}\" is not in the first 256 KiB of the body",
                    keyword.text
                )
            } else {
                format!("HTTP {status}, but the body contains \"{}\"", keyword.text)
            };
            return ProbeOutcome::down(Some(elapsed), message);
        }
    }
    degraded_or_up(check.degraded_after_ms, elapsed, &format!("HTTP {status}"))
}

fn degraded_or_up(threshold_ms: Option<u32>, elapsed: Duration, what: &str) -> ProbeOutcome {
    match threshold_ms {
        Some(ms) if elapsed > Duration::from_millis(u64::from(ms)) => ProbeOutcome::degraded(
            elapsed,
            format!("{what} in {} ms, slower than {ms} ms", elapsed.as_millis()),
        ),
        _ => ProbeOutcome::up(elapsed),
    }
}

async fn probe_http(
    check: &HttpCheck,
    url: &reqwest::Url,
    request: &CheckRequest,
    timeout: Duration,
) -> ProbeOutcome {
    let client = match pinned_public_client(url.as_str(), timeout, USER_AGENT).await {
        Ok(PublicProbeTarget::Pinned(client)) => client,
        Ok(PublicProbeTarget::NonPublic(reason)) => {
            return ProbeOutcome::down(
                None,
                format!("not checked: the address is not public ({reason})"),
            )
        }
        Ok(PublicProbeTarget::Unresolved(reason)) => {
            return ProbeOutcome::down(None, format!("the host did not resolve: {reason}"))
        }
        Err(error) => return ProbeOutcome::down(None, error.to_string()),
    };
    let mut builder = client.request(check.method.as_reqwest(), url.clone());
    for header in &request.headers {
        builder = builder.header(header.name.as_str(), header.value.as_str());
    }
    if let Some(body) = &request.body {
        builder = builder.body(body.clone());
    }
    let started = Instant::now();
    let mut response = match builder.send().await {
        Ok(response) => response,
        Err(error) if error.is_timeout() => {
            return ProbeOutcome::down(
                Some(started.elapsed()),
                format!("no answer within {}", seconds(timeout)),
            )
        }
        Err(error) if error.is_connect() => {
            return ProbeOutcome::down(
                Some(started.elapsed()),
                format!("could not connect: {}", error_chain(&error)),
            )
        }
        Err(error) => {
            return ProbeOutcome::down(
                Some(started.elapsed()),
                format!("request failed: {}", error_chain(&error)),
            )
        }
    };
    let elapsed = started.elapsed();
    let status = response.status().as_u16();
    let body = if check.keyword.is_some() && status_is_expected(check, status) {
        let mut body = Vec::new();
        loop {
            match response.chunk().await {
                Ok(Some(chunk)) => {
                    let room = MAX_KEYWORD_BODY_BYTES - body.len();
                    body.extend_from_slice(&chunk[..chunk.len().min(room)]);
                    if body.len() >= MAX_KEYWORD_BODY_BYTES {
                        break;
                    }
                }
                Ok(None) => break,
                Err(error) => {
                    return ProbeOutcome::down(
                        Some(started.elapsed()),
                        format!(
                            "HTTP {status}, but reading the body failed: {}",
                            error_chain(&error)
                        ),
                    )
                }
            }
        }
        Some(body)
    } else {
        None
    };
    judge_http(check, status, body.as_deref(), elapsed)
}

fn error_chain(error: &(dyn std::error::Error + 'static)) -> String {
    let mut message = error.to_string();
    let mut source = error.source();
    while let Some(inner) = source {
        let text = inner.to_string();
        if !message.contains(&text) {
            message.push_str(": ");
            message.push_str(&text);
        }
        source = inner.source();
    }
    message
}

/// The public addresses `host:port` may be reached at, or why not.
async fn public_addresses(host: &str, port: u16) -> Result<Vec<SocketAddr>, ProbeOutcome> {
    if let Ok(ip) = host.parse::<IpAddr>() {
        return match validate_outbound_ip(ip) {
            Ok(()) => Ok(vec![SocketAddr::new(ip, port)]),
            Err(error) => Err(ProbeOutcome::down(
                None,
                format!("not checked: the address is not public ({error})"),
            )),
        };
    }
    match resolve_and_validate_domain(host, port).await {
        Ok(addresses) if !addresses.is_empty() => Ok(addresses),
        Ok(_) => Err(ProbeOutcome::down(None, format!("{host} has no address"))),
        Err(UrlValidationError::DnsResolutionFailed(reason)) => Err(ProbeOutcome::down(
            None,
            format!("{host} did not resolve: {reason}"),
        )),
        Err(error) => Err(ProbeOutcome::down(
            None,
            format!("not checked: {host} resolves to an address that is not public ({error})"),
        )),
    }
}

/// Connect to the first address that answers before `deadline`.
async fn connect(
    addresses: &[SocketAddr],
    timeout: Duration,
) -> Result<(TcpStream, Duration), ProbeOutcome> {
    let started = Instant::now();
    let mut last_error = None;
    for address in addresses {
        let Some(remaining) = timeout.checked_sub(started.elapsed()) else {
            break;
        };
        match tokio::time::timeout(remaining, TcpStream::connect(address)).await {
            Ok(Ok(stream)) => return Ok((stream, started.elapsed())),
            Ok(Err(error)) => last_error = Some(error.to_string()),
            Err(_) => break,
        }
    }
    Err(ProbeOutcome::down(
        Some(started.elapsed()),
        match last_error {
            Some(error) if started.elapsed() < timeout => format!("could not connect: {error}"),
            _ => format!("no connection within {}", seconds(timeout)),
        },
    ))
}

async fn probe_tcp(check: &TcpCheck, host: &str, port: u16, timeout: Duration) -> ProbeOutcome {
    let addresses = match public_addresses(host, port).await {
        Ok(addresses) => addresses,
        Err(outcome) => return outcome,
    };
    match connect(&addresses, timeout).await {
        Ok((_stream, elapsed)) => degraded_or_up(
            check.degraded_after_ms,
            elapsed,
            &format!("Port {port} answered"),
        ),
        Err(outcome) => outcome,
    }
}

static TLS_CONFIG: LazyLock<Result<Arc<rustls::ClientConfig>, String>> = LazyLock::new(|| {
    let roots = rustls::RootCertStore::from_iter(webpki_roots::TLS_SERVER_ROOTS.iter().cloned());
    rustls::ClientConfig::builder_with_provider(Arc::new(rustls::crypto::ring::default_provider()))
        .with_safe_default_protocol_versions()
        .map(|builder| Arc::new(builder.with_root_certificates(roots).with_no_client_auth()))
        .map_err(|e| e.to_string())
});

/// Judge a certificate that expires at `not_after` (unix seconds).
pub fn judge_certificate(
    check: &TlsCheck,
    not_after: i64,
    now: i64,
    elapsed: Duration,
) -> ProbeOutcome {
    let warn_days = i64::from(check.warn_days.unwrap_or(DEFAULT_TLS_WARN_DAYS));
    let left = not_after - now;
    let days = left.div_euclid(86_400);
    if left <= 0 {
        return ProbeOutcome::down(Some(elapsed), "the certificate has expired");
    }
    if days < warn_days {
        return ProbeOutcome::degraded(
            elapsed,
            format!("the certificate expires in {days} days (warning at {warn_days})"),
        );
    }
    ProbeOutcome::up(elapsed)
}

async fn probe_tls(check: &TlsCheck, host: &str, port: u16, timeout: Duration) -> ProbeOutcome {
    let config = match &*TLS_CONFIG {
        Ok(config) => config.clone(),
        Err(error) => return ProbeOutcome::down(None, format!("TLS is unavailable: {error}")),
    };
    let server_name = match host.parse::<IpAddr>() {
        Ok(ip) => rustls::pki_types::ServerName::IpAddress(ip.into()),
        Err(_) => match rustls::pki_types::ServerName::try_from(host.to_string()) {
            Ok(name) => name,
            Err(error) => {
                return ProbeOutcome::down(
                    None,
                    format!("'{host}' is not a TLS server name: {error}"),
                )
            }
        },
    };
    let addresses = match public_addresses(host, port).await {
        Ok(addresses) => addresses,
        Err(outcome) => return outcome,
    };
    let started = Instant::now();
    let (stream, _) = match connect(&addresses, timeout).await {
        Ok(connected) => connected,
        Err(outcome) => return outcome,
    };
    let remaining = timeout.saturating_sub(started.elapsed());
    let connector = tokio_rustls::TlsConnector::from(config);
    let tls = match tokio::time::timeout(remaining, connector.connect(server_name, stream)).await {
        Ok(Ok(tls)) => tls,
        Ok(Err(error)) => {
            return ProbeOutcome::down(
                Some(started.elapsed()),
                format!("TLS handshake failed: {}", error_chain(&error)),
            )
        }
        Err(_) => {
            return ProbeOutcome::down(
                Some(started.elapsed()),
                format!("no TLS handshake within {}", seconds(timeout)),
            )
        }
    };
    let elapsed = started.elapsed();
    let Some(leaf) = tls
        .get_ref()
        .1
        .peer_certificates()
        .and_then(|certificates| certificates.first())
    else {
        return ProbeOutcome::down(Some(elapsed), "the server sent no certificate");
    };
    match x509_parser::parse_x509_certificate(leaf.as_ref()) {
        Ok((_, certificate)) => judge_certificate(
            check,
            certificate.validity().not_after.timestamp(),
            chrono::Utc::now().timestamp(),
            elapsed,
        ),
        Err(error) => ProbeOutcome::down(
            Some(elapsed),
            format!("the certificate is unreadable: {error}"),
        ),
    }
}

/// Judge DNS `answers` (already normalised) against `check`.
pub fn judge_dns(
    check: &DnsCheck,
    name: &str,
    answers: &[String],
    elapsed: Duration,
) -> ProbeOutcome {
    let kind = check.record_type.as_str();
    if answers.is_empty() {
        return ProbeOutcome::down(Some(elapsed), format!("{name} has no {kind} record"));
    }
    if check.expected.is_empty() || answers.iter().any(|answer| check.expected.contains(answer)) {
        return ProbeOutcome::up(elapsed);
    }
    let mut listed: Vec<&str> = answers
        .iter()
        .take(MAX_LISTED_ANSWERS)
        .map(String::as_str)
        .collect();
    if answers.len() > MAX_LISTED_ANSWERS {
        listed.push("…");
    }
    ProbeOutcome::down(
        Some(elapsed),
        format!(
            "{name} {kind} is {}, expected {}",
            listed.join(", "),
            check.expected.join(", ")
        ),
    )
}

/// An address answer as shown to the operator. A private address is not
/// echoed, so a DNS check cannot be used to map internal names.
fn shown_address(ip: IpAddr) -> String {
    match validate_outbound_ip(ip) {
        Ok(()) => ip.to_string(),
        Err(_) => "a non-public address".to_string(),
    }
}

/// A DNS answer normalised exactly as expectations are on save
/// ([`normalize_dns_value`]), TXT included, so a match never depends on case.
fn dns_answer(data: &hickory_resolver::proto::rr::RData) -> Option<String> {
    use hickory_resolver::proto::rr::RData;
    let value = match data {
        RData::A(a) => a.0.to_string(),
        RData::AAAA(aaaa) => aaaa.0.to_string(),
        RData::CNAME(cname) => cname.0.to_string(),
        RData::NS(ns) => ns.0.to_string(),
        RData::MX(mx) => mx.exchange.to_string(),
        RData::TXT(txt) => txt.to_string(),
        _ => return None,
    };
    Some(normalize_dns_value(&value))
}

async fn probe_dns(check: &DnsCheck, name: &str, timeout: Duration) -> ProbeOutcome {
    use super::spec::DnsCheckRecordType;
    use hickory_resolver::config::ResolveHosts;
    use hickory_resolver::proto::rr::RecordType;

    let mut builder = match hickory_resolver::Resolver::builder_tokio() {
        Ok(builder) => builder,
        Err(error) => {
            return ProbeOutcome::down(
                None,
                format!("the system DNS settings are unreadable: {error}"),
            )
        }
    };
    let options = builder.options_mut();
    options.timeout = timeout;
    options.attempts = 1;
    options.use_hosts_file = ResolveHosts::Never;
    options.cache_size = 0;
    let resolver = match builder.build() {
        Ok(resolver) => resolver,
        Err(error) => {
            return ProbeOutcome::down(None, format!("could not start a DNS resolver: {error}"))
        }
    };
    let record_type = match check.record_type {
        DnsCheckRecordType::A => RecordType::A,
        DnsCheckRecordType::Aaaa => RecordType::AAAA,
        DnsCheckRecordType::Cname => RecordType::CNAME,
        DnsCheckRecordType::Mx => RecordType::MX,
        DnsCheckRecordType::Txt => RecordType::TXT,
        DnsCheckRecordType::Ns => RecordType::NS,
    };
    let started = Instant::now();
    let lookup = tokio::time::timeout(timeout, resolver.lookup(name, record_type)).await;
    let elapsed = started.elapsed();
    let lookup = match lookup {
        Ok(Ok(lookup)) => lookup,
        Ok(Err(error)) if error.is_no_records_found() || error.is_nx_domain() => {
            return judge_dns(check, name, &[], elapsed)
        }
        Ok(Err(error)) => {
            return ProbeOutcome::down(Some(elapsed), format!("DNS lookup failed: {error}"))
        }
        Err(_) => {
            return ProbeOutcome::down(
                Some(elapsed),
                format!("no DNS answer within {}", seconds(timeout)),
            )
        }
    };
    let answers: Vec<String> = lookup
        .answers()
        .iter()
        .filter(|record| record.record_type() == record_type)
        .filter_map(|record| dns_answer(&record.data))
        .collect();
    let outcome = judge_dns(check, name, &answers, elapsed);
    if outcome.status == ProbeStatus::Operational
        || !matches!(
            check.record_type,
            DnsCheckRecordType::A | DnsCheckRecordType::Aaaa
        )
    {
        return outcome;
    }
    // Re-render the mismatch without echoing private addresses.
    let shown: Vec<String> = answers
        .iter()
        .map(|answer| {
            answer
                .parse::<IpAddr>()
                .map(shown_address)
                .unwrap_or_else(|_| answer.clone())
        })
        .collect();
    judge_dns(check, name, &shown, elapsed)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::services::checks::spec::{DnsCheckRecordType, KeywordAssertion};

    const FAST: Duration = Duration::from_millis(120);

    #[test]
    fn http_status_defaults_to_2xx_or_3xx() {
        let check = HttpCheck::default();
        assert_eq!(
            judge_http(&check, 204, None, FAST).status,
            ProbeStatus::Operational
        );
        assert_eq!(
            judge_http(&check, 302, None, FAST).status,
            ProbeStatus::Operational
        );
        let down = judge_http(&check, 503, None, FAST);
        assert_eq!(down.status, ProbeStatus::Down);
        assert_eq!(
            down.message.as_deref(),
            Some("HTTP 503, expected 2xx or 3xx")
        );
    }

    #[test]
    fn http_expected_status_replaces_the_default() {
        let check = HttpCheck {
            expected_status: vec!["401".into()],
            ..Default::default()
        };
        assert_eq!(
            judge_http(&check, 401, None, FAST).status,
            ProbeStatus::Operational
        );
        assert_eq!(
            judge_http(&check, 200, None, FAST).message.as_deref(),
            Some("HTTP 200, expected 401")
        );
    }

    #[test]
    fn keywords_must_or_must_not_appear() {
        let present = HttpCheck {
            keyword: Some(KeywordAssertion {
                text: "\"ok\":true".into(),
                present: true,
            }),
            ..Default::default()
        };
        assert_eq!(
            judge_http(&present, 200, Some(br#"{"ok":true}"#), FAST).status,
            ProbeStatus::Operational
        );
        let missing = judge_http(&present, 200, Some(b"{}"), FAST);
        assert_eq!(missing.status, ProbeStatus::Down);
        assert!(missing
            .message
            .expect("message")
            .contains("is not in the first 256 KiB"));

        let absent = HttpCheck {
            keyword: Some(KeywordAssertion {
                text: "maintenance".into(),
                present: false,
            }),
            ..Default::default()
        };
        assert_eq!(
            judge_http(&absent, 200, Some(b"all good"), FAST).status,
            ProbeStatus::Operational
        );
        assert_eq!(
            judge_http(&absent, 200, Some(b"in maintenance"), FAST).status,
            ProbeStatus::Down
        );
    }

    #[test]
    fn slow_answers_are_degraded() {
        let check = HttpCheck {
            degraded_after_ms: Some(100),
            ..Default::default()
        };
        let slow = judge_http(&check, 200, None, Duration::from_millis(250));
        assert_eq!(slow.status, ProbeStatus::Degraded);
        assert_eq!(slow.response_time_ms, Some(250));
        assert_eq!(
            slow.message.as_deref(),
            Some("HTTP 200 in 250 ms, slower than 100 ms")
        );
        assert_eq!(
            judge_http(&check, 200, None, Duration::from_millis(80)).status,
            ProbeStatus::Operational
        );
    }

    #[test]
    fn certificates_degrade_before_they_expire() {
        let check = TlsCheck {
            warn_days: Some(14),
            timeout_ms: None,
        };
        let now = 1_800_000_000;
        assert_eq!(
            judge_certificate(&check, now + 30 * 86_400, now, FAST).status,
            ProbeStatus::Operational
        );
        let soon = judge_certificate(&check, now + 5 * 86_400 + 10, now, FAST);
        assert_eq!(soon.status, ProbeStatus::Degraded);
        assert_eq!(
            soon.message.as_deref(),
            Some("the certificate expires in 5 days (warning at 14)")
        );
        assert_eq!(
            judge_certificate(&check, now - 1, now, FAST).status,
            ProbeStatus::Down
        );
    }

    #[test]
    fn txt_answers_match_expectations_whatever_their_case() {
        use hickory_resolver::proto::rr::rdata::TXT;
        use hickory_resolver::proto::rr::RData;
        let check = DnsCheck {
            record_type: DnsCheckRecordType::Txt,
            // As saved: expectations are normalised on save.
            expected: vec![normalize_dns_value("site-verification=AbC123")],
            timeout_ms: None,
        };
        let answer = dns_answer(&RData::TXT(TXT::new(vec![
            "site-verification=AbC123".to_string()
        ])))
        .expect("a TXT answer");
        assert_eq!(
            judge_dns(&check, "example.com", &[answer], FAST).status,
            ProbeStatus::Operational
        );
    }

    #[tokio::test]
    async fn a_probe_is_bounded_by_its_timeout_name_resolution_included() {
        let outcome = within(
            Duration::from_millis(20),
            std::future::pending::<ProbeOutcome>(),
        )
        .await;
        assert_eq!(outcome.status, ProbeStatus::Down);
        assert_eq!(outcome.response_time_ms, Some(20));
        assert!(
            outcome
                .message
                .as_deref()
                .is_some_and(|message| message.contains("name resolution included")),
            "{outcome:?}"
        );
    }

    #[test]
    fn dns_answers_are_matched() {
        let check = DnsCheck {
            record_type: DnsCheckRecordType::A,
            expected: vec!["192.0.2.10".into()],
            timeout_ms: None,
        };
        let answers = vec!["192.0.2.10".to_string(), "192.0.2.11".to_string()];
        assert_eq!(
            judge_dns(&check, "example.com", &answers, FAST).status,
            ProbeStatus::Operational
        );
        let wrong = judge_dns(&check, "example.com", &["198.51.100.1".to_string()], FAST);
        assert_eq!(
            wrong.message.as_deref(),
            Some("example.com A is 198.51.100.1, expected 192.0.2.10")
        );
        let none = judge_dns(&check, "example.com", &[], FAST);
        assert_eq!(none.message.as_deref(), Some("example.com has no A record"));
        let any = DnsCheck {
            expected: vec![],
            ..check
        };
        assert_eq!(
            judge_dns(&any, "example.com", &answers, FAST).status,
            ProbeStatus::Operational
        );
    }

    #[test]
    fn private_addresses_are_not_echoed() {
        assert_eq!(
            shown_address("10.0.0.5".parse().expect("ip")),
            "a non-public address"
        );
        assert_eq!(shown_address("8.8.8.8".parse().expect("ip")), "8.8.8.8");
    }

    #[tokio::test]
    async fn non_public_targets_are_never_contacted() {
        let tcp = probe_tcp(
            &TcpCheck::default(),
            "127.0.0.1",
            22,
            Duration::from_secs(1),
        )
        .await;
        assert_eq!(tcp.status, ProbeStatus::Down);
        assert!(tcp.message.expect("message").contains("not public"));
        let tls = probe_tls(
            &TlsCheck::default(),
            "169.254.169.254",
            443,
            Duration::from_secs(1),
        )
        .await;
        assert!(tls.message.expect("message").contains("not public"));
        let url = reqwest::Url::parse("http://127.0.0.1:9/").expect("url");
        let http = probe_http(
            &HttpCheck::default(),
            &url,
            &CheckRequest::default(),
            Duration::from_secs(1),
        )
        .await;
        assert!(
            http.message
                .as_deref()
                .unwrap_or_default()
                .contains("not public"),
            "{http:?}"
        );
    }
}
