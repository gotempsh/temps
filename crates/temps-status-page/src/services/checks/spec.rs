// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! What a monitor outside Temps checks, and the rules its settings follow.

use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

use super::error::CheckConfigError;

/// Longest target string accepted.
pub const MAX_TARGET_CHARS: usize = 2048;
/// Default and ceiling of one probe's time limit.
pub const DEFAULT_TIMEOUT_MS: u32 = 10_000;
pub const MAX_TIMEOUT_MS: u32 = 30_000;
pub const MIN_TIMEOUT_MS: u32 = 500;
/// How much of an HTTP body a keyword check reads. The body is never stored.
pub const MAX_KEYWORD_BODY_BYTES: usize = 256 * 1024;
pub const MAX_KEYWORD_CHARS: usize = 200;
pub const MAX_EXPECTED_STATUSES: usize = 10;
pub const MAX_HEADERS: usize = 20;
pub const MAX_HEADER_NAME_CHARS: usize = 100;
pub const MAX_HEADER_VALUE_CHARS: usize = 4096;
pub const MAX_REQUEST_BODY_BYTES: usize = 64 * 1024;
pub const MAX_DNS_EXPECTED: usize = 10;
pub const MAX_DNS_VALUE_CHARS: usize = 255;
pub const DEFAULT_TLS_WARN_DAYS: u32 = 14;
pub const MAX_TLS_WARN_DAYS: u32 = 90;

/// What a monitor checks. `Environment` is the probe of a Temps
/// environment; the others check an explicit target.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum MonitorCheckKind {
    Environment,
    Http,
    Tcp,
    Tls,
    Dns,
}

impl MonitorCheckKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Environment => "environment",
            Self::Http => "http",
            Self::Tcp => "tcp",
            Self::Tls => "tls",
            Self::Dns => "dns",
        }
    }

    /// The stored value; unknown values read as `Environment`, which the
    /// database constraint makes impossible for a row with a target.
    pub fn from_stored(value: &str) -> Self {
        match value {
            "http" => Self::Http,
            "tcp" => Self::Tcp,
            "tls" => Self::Tls,
            "dns" => Self::Dns,
            _ => Self::Environment,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "UPPERCASE")]
pub enum HttpMethod {
    #[default]
    Get,
    Head,
    Post,
    Put,
    Patch,
    Delete,
    Options,
}

impl HttpMethod {
    pub fn as_reqwest(self) -> reqwest::Method {
        match self {
            Self::Get => reqwest::Method::GET,
            Self::Head => reqwest::Method::HEAD,
            Self::Post => reqwest::Method::POST,
            Self::Put => reqwest::Method::PUT,
            Self::Patch => reqwest::Method::PATCH,
            Self::Delete => reqwest::Method::DELETE,
            Self::Options => reqwest::Method::OPTIONS,
        }
    }
}

/// Text that must, or must not, appear in the response body.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct KeywordAssertion {
    /// Case-sensitive, at most 200 characters. Only the first 256 KiB of
    /// the body is searched, and the body is never stored.
    pub text: String,
    /// `true`: the check fails unless the text is there. `false`: it fails
    /// when the text is there (e.g. "maintenance mode").
    #[serde(default = "default_true")]
    pub present: bool,
}

fn default_true() -> bool {
    true
}

#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize, ToSchema)]
pub struct HttpCheck {
    #[serde(default)]
    pub method: HttpMethod,
    /// Status codes that count as up: exact codes ("204") or classes
    /// ("2xx"). Empty: any 2xx or 3xx.
    #[serde(default)]
    pub expected_status: Vec<String>,
    #[serde(default)]
    pub keyword: Option<KeywordAssertion>,
    /// Slower than this many milliseconds is degraded, not up.
    #[serde(default)]
    pub degraded_after_ms: Option<u32>,
    /// Time limit of one request, 500 to 30,000 ms (default 10,000).
    #[serde(default)]
    pub timeout_ms: Option<u32>,
}

#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize, ToSchema)]
pub struct TcpCheck {
    /// Slower than this many milliseconds to connect is degraded.
    #[serde(default)]
    pub degraded_after_ms: Option<u32>,
    #[serde(default)]
    pub timeout_ms: Option<u32>,
}

#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize, ToSchema)]
pub struct TlsCheck {
    /// A certificate expiring within this many days is degraded
    /// (default 14, at most 90). An expired or untrusted one is down.
    #[serde(default)]
    pub warn_days: Option<u32>,
    #[serde(default)]
    pub timeout_ms: Option<u32>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "UPPERCASE")]
pub enum DnsCheckRecordType {
    A,
    Aaaa,
    Cname,
    Mx,
    Txt,
    Ns,
}

impl DnsCheckRecordType {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::A => "A",
            Self::Aaaa => "AAAA",
            Self::Cname => "CNAME",
            Self::Mx => "MX",
            Self::Txt => "TXT",
            Self::Ns => "NS",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct DnsCheck {
    pub record_type: DnsCheckRecordType,
    /// Up when any answer equals one of these (case-insensitive, trailing
    /// dot ignored). Empty: up when the name has any record of the type.
    #[serde(default)]
    pub expected: Vec<String>,
    #[serde(default)]
    pub timeout_ms: Option<u32>,
}

/// A target monitor's check, one shape per kind.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum CheckSpec {
    Http(HttpCheck),
    Tcp(TcpCheck),
    Tls(TlsCheck),
    Dns(DnsCheck),
}

impl CheckSpec {
    pub fn kind(&self) -> MonitorCheckKind {
        match self {
            Self::Http(_) => MonitorCheckKind::Http,
            Self::Tcp(_) => MonitorCheckKind::Tcp,
            Self::Tls(_) => MonitorCheckKind::Tls,
            Self::Dns(_) => MonitorCheckKind::Dns,
        }
    }

    pub fn timeout(&self) -> std::time::Duration {
        let ms = match self {
            Self::Http(c) => c.timeout_ms,
            Self::Tcp(c) => c.timeout_ms,
            Self::Tls(c) => c.timeout_ms,
            Self::Dns(c) => c.timeout_ms,
        };
        std::time::Duration::from_millis(u64::from(
            ms.unwrap_or(DEFAULT_TIMEOUT_MS)
                .clamp(MIN_TIMEOUT_MS, MAX_TIMEOUT_MS),
        ))
    }
}

/// What is stored in `status_monitors.check_spec`: the spec plus what the
/// encrypted request holds, so a response can describe it without
/// decrypting anything.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StoredCheck {
    pub spec: CheckSpec,
    #[serde(default)]
    pub header_names: Vec<String>,
    #[serde(default)]
    pub has_body: bool,
}

/// One HTTP request header. The value is stored encrypted and never
/// returned.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct CheckRequestHeader {
    pub name: String,
    pub value: String,
}

/// The secret part of an HTTP check: headers and body, encrypted at rest.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize, ToSchema)]
pub struct CheckRequest {
    #[serde(default)]
    pub headers: Vec<CheckRequestHeader>,
    #[serde(default)]
    pub body: Option<String>,
}

impl CheckRequest {
    pub fn is_empty(&self) -> bool {
        self.headers.is_empty() && self.body.as_deref().is_none_or(str::is_empty)
    }
}

/// A validated target, ready to probe.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Target {
    Url(reqwest::Url),
    HostPort { host: String, port: u16 },
    Name(String),
}

impl Target {
    /// The canonical string stored in `status_monitors.target`.
    pub fn stored(&self) -> String {
        match self {
            Self::Url(url) => url.to_string(),
            Self::HostPort { host, port } if host.contains(':') => format!("[{host}]:{port}"),
            Self::HostPort { host, port } => format!("{host}:{port}"),
            Self::Name(name) => name.clone(),
        }
    }
}

fn invalid(field: &'static str, reason: impl Into<String>) -> CheckConfigError {
    CheckConfigError::Invalid {
        field,
        reason: reason.into(),
    }
}

fn validate_hostname(field: &'static str, host: &str) -> Result<String, CheckConfigError> {
    let host = host.trim().trim_end_matches('.').to_ascii_lowercase();
    if host.is_empty() {
        return Err(invalid(field, "the host is empty"));
    }
    if host.parse::<std::net::IpAddr>().is_ok() {
        return Ok(host);
    }
    if host.len() > 253 {
        return Err(invalid(
            field,
            "the host name is longer than 253 characters",
        ));
    }
    let labels_ok = host.split('.').all(|label| {
        !label.is_empty()
            && label.len() <= 63
            && !label.starts_with('-')
            && !label.ends_with('-')
            && label
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
    });
    if !labels_ok {
        return Err(invalid(
            field,
            format!("'{host}' is not a valid host name (international names: use the xn-- form)"),
        ));
    }
    Ok(host)
}

/// `host:port`, `[v6]:port`, or `host` with `default_port`.
fn parse_host_port(raw: &str, default_port: Option<u16>) -> Result<Target, CheckConfigError> {
    let raw = raw.trim();
    let (host, port) = if let Some(rest) = raw.strip_prefix('[') {
        let (host, after) = rest
            .split_once(']')
            .ok_or_else(|| invalid("target", "an IPv6 address needs a closing ]"))?;
        let port = after.strip_prefix(':');
        (host.to_string(), port.map(str::to_string))
    } else if raw.matches(':').count() == 1 {
        let (host, port) = raw.split_once(':').unwrap_or((raw, ""));
        (host.to_string(), Some(port.to_string()))
    } else if raw.contains(':') {
        return Err(invalid(
            "target",
            "write an IPv6 address in brackets, like [2001:db8::1]:443",
        ));
    } else {
        (raw.to_string(), None)
    };
    let port = match port {
        Some(port) => port
            .parse::<u16>()
            .ok()
            .filter(|p| *p > 0)
            .ok_or_else(|| invalid("target", format!("'{port}' is not a port (1-65535)")))?,
        None => {
            default_port.ok_or_else(|| invalid("target", "add a port, like db.example.com:5432"))?
        }
    };
    Ok(Target::HostPort {
        host: validate_hostname("target", &host)?,
        port,
    })
}

/// Parse and validate `raw` as the target of a `kind` check.
pub fn parse_target(kind: MonitorCheckKind, raw: &str) -> Result<Target, CheckConfigError> {
    let raw = raw.trim();
    if raw.is_empty() {
        return Err(invalid("target", "the target is empty"));
    }
    if raw.chars().count() > MAX_TARGET_CHARS {
        return Err(invalid(
            "target",
            "the target is longer than 2048 characters",
        ));
    }
    if raw.chars().any(char::is_control) {
        return Err(invalid("target", "the target contains control characters"));
    }
    match kind {
        MonitorCheckKind::Environment => Err(invalid(
            "target",
            "an environment monitor checks its environment, not a target",
        )),
        MonitorCheckKind::Http => {
            let url = reqwest::Url::parse(raw)
                .map_err(|e| invalid("target", format!("'{raw}' is not a URL: {e}")))?;
            if !matches!(url.scheme(), "http" | "https") {
                return Err(invalid(
                    "target",
                    "the URL must start with http:// or https://",
                ));
            }
            if url.host_str().is_none_or(str::is_empty) {
                return Err(invalid("target", "the URL has no host"));
            }
            if !url.username().is_empty() || url.password().is_some() {
                return Err(invalid(
                    "target",
                    "put credentials in a request header, not in the URL",
                ));
            }
            if url.fragment().is_some() {
                return Err(invalid("target", "the URL cannot have a #fragment"));
            }
            Ok(Target::Url(url))
        }
        MonitorCheckKind::Tcp => parse_host_port(raw, None),
        MonitorCheckKind::Tls => parse_host_port(raw, Some(443)),
        MonitorCheckKind::Dns => {
            if raw.contains("://") || raw.contains('/') || raw.contains(':') {
                return Err(invalid(
                    "target",
                    "a DNS check takes a host name, like example.com",
                ));
            }
            let name = validate_hostname("target", raw)?;
            if name.parse::<std::net::IpAddr>().is_ok() {
                return Err(invalid(
                    "target",
                    "a DNS check takes a host name, not an address",
                ));
            }
            Ok(Target::Name(name))
        }
    }
}

fn validate_timeout(timeout_ms: Option<u32>) -> Result<(), CheckConfigError> {
    match timeout_ms {
        Some(ms) if !(MIN_TIMEOUT_MS..=MAX_TIMEOUT_MS).contains(&ms) => Err(invalid(
            "timeout_ms",
            format!("use {MIN_TIMEOUT_MS} to {MAX_TIMEOUT_MS} milliseconds"),
        )),
        _ => Ok(()),
    }
}

fn validate_degraded(
    degraded_after_ms: Option<u32>,
    timeout_ms: Option<u32>,
) -> Result<(), CheckConfigError> {
    let Some(ms) = degraded_after_ms else {
        return Ok(());
    };
    let timeout = timeout_ms.unwrap_or(DEFAULT_TIMEOUT_MS);
    if ms == 0 || ms >= timeout {
        return Err(invalid(
            "degraded_after_ms",
            format!("use 1 to {} milliseconds, below the timeout", timeout - 1),
        ));
    }
    Ok(())
}

/// Whether `pattern` ("204", "2xx") is a valid expected status.
pub fn parse_status_pattern(pattern: &str) -> Option<StatusPattern> {
    let p = pattern.trim().to_ascii_lowercase();
    if let Some(class) = p.strip_suffix("xx") {
        let digit = class.parse::<u16>().ok().filter(|d| (1..=5).contains(d))?;
        return (class.len() == 1).then_some(StatusPattern::Class(digit));
    }
    p.parse::<u16>()
        .ok()
        .filter(|code| (100..=599).contains(code))
        .map(StatusPattern::Exact)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StatusPattern {
    Exact(u16),
    Class(u16),
}

impl StatusPattern {
    pub fn matches(self, status: u16) -> bool {
        match self {
            Self::Exact(code) => code == status,
            Self::Class(class) => status / 100 == class,
        }
    }
}

/// Normalise and validate `spec` (and, for HTTP, its `request`).
pub fn validate_spec(
    spec: CheckSpec,
    request: &CheckRequest,
) -> Result<CheckSpec, CheckConfigError> {
    if !matches!(spec, CheckSpec::Http(_)) && !request.is_empty() {
        return Err(invalid(
            "request",
            "only an HTTP check sends headers or a body",
        ));
    }
    match spec {
        CheckSpec::Http(mut http) => {
            validate_timeout(http.timeout_ms)?;
            validate_degraded(http.degraded_after_ms, http.timeout_ms)?;
            if http.expected_status.len() > MAX_EXPECTED_STATUSES {
                return Err(invalid(
                    "expected_status",
                    "at most 10 status codes or classes",
                ));
            }
            let mut normalized = Vec::with_capacity(http.expected_status.len());
            for pattern in &http.expected_status {
                if parse_status_pattern(pattern).is_none() {
                    return Err(invalid(
                        "expected_status",
                        format!("'{pattern}' is not a status code (100-599) or class (2xx)"),
                    ));
                }
                let value = pattern.trim().to_ascii_lowercase();
                if !normalized.contains(&value) {
                    normalized.push(value);
                }
            }
            http.expected_status = normalized;
            if let Some(keyword) = &http.keyword {
                let chars = keyword.text.chars().count();
                if chars == 0 || chars > MAX_KEYWORD_CHARS {
                    return Err(invalid("keyword", "the keyword is 1 to 200 characters"));
                }
                if http.method == HttpMethod::Head {
                    return Err(invalid("keyword", "a HEAD response has no body to search"));
                }
            }
            validate_request(request)?;
            Ok(CheckSpec::Http(http))
        }
        CheckSpec::Tcp(tcp) => {
            validate_timeout(tcp.timeout_ms)?;
            validate_degraded(tcp.degraded_after_ms, tcp.timeout_ms)?;
            Ok(CheckSpec::Tcp(tcp))
        }
        CheckSpec::Tls(tls) => {
            validate_timeout(tls.timeout_ms)?;
            if let Some(days) = tls.warn_days {
                if days == 0 || days > MAX_TLS_WARN_DAYS {
                    return Err(invalid("warn_days", "use 1 to 90 days"));
                }
            }
            Ok(CheckSpec::Tls(tls))
        }
        CheckSpec::Dns(mut dns) => {
            validate_timeout(dns.timeout_ms)?;
            if dns.expected.len() > MAX_DNS_EXPECTED {
                return Err(invalid("expected", "at most 10 expected values"));
            }
            let mut normalized = Vec::with_capacity(dns.expected.len());
            for value in &dns.expected {
                let value = normalize_dns_value(value);
                if value.is_empty() || value.chars().count() > MAX_DNS_VALUE_CHARS {
                    return Err(invalid(
                        "expected",
                        "each expected value is 1 to 255 characters",
                    ));
                }
                if value.chars().any(char::is_control) {
                    return Err(invalid(
                        "expected",
                        "expected values cannot contain control characters",
                    ));
                }
                match dns.record_type {
                    DnsCheckRecordType::A if value.parse::<std::net::Ipv4Addr>().is_err() => {
                        return Err(invalid(
                            "expected",
                            format!("'{value}' is not an IPv4 address"),
                        ));
                    }
                    DnsCheckRecordType::Aaaa if value.parse::<std::net::Ipv6Addr>().is_err() => {
                        return Err(invalid(
                            "expected",
                            format!("'{value}' is not an IPv6 address"),
                        ));
                    }
                    _ => {}
                }
                if !normalized.contains(&value) {
                    normalized.push(value);
                }
            }
            dns.expected = normalized;
            Ok(CheckSpec::Dns(dns))
        }
    }
}

/// How DNS answers and expectations are compared.
pub fn normalize_dns_value(value: &str) -> String {
    let value = value.trim().trim_end_matches('.');
    match value.parse::<std::net::IpAddr>() {
        Ok(ip) => ip.to_string(),
        Err(_) => value.to_ascii_lowercase(),
    }
}

/// Headers the HTTP client sets itself, or that would change where or how
/// the request goes.
const RESERVED_HEADERS: &[&str] = &[
    "host",
    "content-length",
    "transfer-encoding",
    "connection",
    "upgrade",
    "te",
    "trailer",
    "keep-alive",
    "proxy-connection",
    "proxy-authorization",
];

fn validate_request(request: &CheckRequest) -> Result<(), CheckConfigError> {
    if request.headers.len() > MAX_HEADERS {
        return Err(invalid("request.headers", "at most 20 headers"));
    }
    let mut seen = Vec::new();
    for header in &request.headers {
        let name = header.name.trim().to_ascii_lowercase();
        if name.is_empty() || name.len() > MAX_HEADER_NAME_CHARS {
            return Err(invalid(
                "request.headers",
                "a header name is 1 to 100 characters",
            ));
        }
        if reqwest::header::HeaderName::from_bytes(name.as_bytes()).is_err() {
            return Err(invalid(
                "request.headers",
                format!("'{}' is not a header name", header.name),
            ));
        }
        if RESERVED_HEADERS.contains(&name.as_str()) {
            return Err(invalid(
                "request.headers",
                format!("the {name} header is set by Temps and cannot be overridden"),
            ));
        }
        if seen.contains(&name) {
            return Err(invalid(
                "request.headers",
                format!("the {name} header is set twice"),
            ));
        }
        if header.value.len() > MAX_HEADER_VALUE_CHARS
            || reqwest::header::HeaderValue::from_str(&header.value).is_err()
        {
            return Err(invalid(
                "request.headers",
                format!("the value of {name} is not a valid header value (at most 4096 visible characters)"),
            ));
        }
        seen.push(name);
    }
    if request
        .body
        .as_ref()
        .is_some_and(|body| body.len() > MAX_REQUEST_BODY_BYTES)
    {
        return Err(invalid(
            "request.body",
            "the request body is at most 64 KiB",
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn targets_are_parsed_per_kind() {
        assert_eq!(
            parse_target(MonitorCheckKind::Http, " https://api.example.com/health ")
                .expect("http")
                .stored(),
            "https://api.example.com/health"
        );
        assert_eq!(
            parse_target(MonitorCheckKind::Tcp, "DB.Example.com:5432")
                .expect("tcp")
                .stored(),
            "db.example.com:5432"
        );
        assert_eq!(
            parse_target(MonitorCheckKind::Tls, "example.com")
                .expect("tls")
                .stored(),
            "example.com:443"
        );
        assert_eq!(
            parse_target(MonitorCheckKind::Tcp, "[2001:db8::1]:22")
                .expect("v6")
                .stored(),
            "[2001:db8::1]:22"
        );
        assert_eq!(
            parse_target(MonitorCheckKind::Dns, "example.com.")
                .expect("dns")
                .stored(),
            "example.com"
        );
        for (kind, raw, reason) in [
            (MonitorCheckKind::Http, "ftp://example.com", "http://"),
            (
                MonitorCheckKind::Http,
                "https://u:p@example.com",
                "credentials",
            ),
            (MonitorCheckKind::Http, "https://example.com/#x", "fragment"),
            (MonitorCheckKind::Tcp, "example.com", "add a port"),
            (MonitorCheckKind::Tcp, "example.com:0", "not a port"),
            (MonitorCheckKind::Tcp, "2001:db8::1:22", "brackets"),
            (MonitorCheckKind::Tcp, "bad_host!:22", "not a valid host"),
            (MonitorCheckKind::Dns, "https://example.com", "host name"),
            (MonitorCheckKind::Dns, "192.0.2.1", "not an address"),
            (MonitorCheckKind::Environment, "x", "environment"),
            (MonitorCheckKind::Http, "", "empty"),
        ] {
            let err = parse_target(kind, raw).expect_err(raw);
            assert!(err.to_string().contains(reason), "{raw}: {err}");
        }
    }

    #[test]
    fn http_specs_are_normalized_and_bounded() {
        let spec = validate_spec(
            CheckSpec::Http(HttpCheck {
                expected_status: vec!["2XX".into(), "204".into(), "2xx".into()],
                ..Default::default()
            }),
            &CheckRequest::default(),
        )
        .expect("valid");
        let CheckSpec::Http(http) = spec else {
            panic!("http")
        };
        assert_eq!(http.expected_status, vec!["2xx", "204"]);

        let bad = |http: HttpCheck, request: CheckRequest| {
            validate_spec(CheckSpec::Http(http), &request)
                .expect_err("invalid")
                .to_string()
        };
        assert!(bad(
            HttpCheck {
                expected_status: vec!["700".into()],
                ..Default::default()
            },
            CheckRequest::default()
        )
        .contains("700"));
        assert!(bad(
            HttpCheck {
                timeout_ms: Some(100),
                ..Default::default()
            },
            CheckRequest::default()
        )
        .contains("timeout_ms"));
        assert!(bad(
            HttpCheck {
                degraded_after_ms: Some(10_000),
                ..Default::default()
            },
            CheckRequest::default()
        )
        .contains("below the timeout"));
        assert!(bad(
            HttpCheck {
                method: HttpMethod::Head,
                keyword: Some(KeywordAssertion {
                    text: "ok".into(),
                    present: true
                }),
                ..Default::default()
            },
            CheckRequest::default()
        )
        .contains("HEAD"));
        let header = |name: &str| CheckRequest {
            headers: vec![CheckRequestHeader {
                name: name.into(),
                value: "x".into(),
            }],
            body: None,
        };
        assert!(bad(HttpCheck::default(), header("Host")).contains("set by Temps"));
        assert!(bad(HttpCheck::default(), header("bad header")).contains("not a header name"));
        assert!(validate_spec(
            CheckSpec::Http(HttpCheck::default()),
            &header("Authorization")
        )
        .is_ok());
    }

    #[test]
    fn only_http_sends_a_request() {
        let request = CheckRequest {
            headers: vec![],
            body: Some("x".into()),
        };
        let err = validate_spec(CheckSpec::Tcp(TcpCheck::default()), &request).expect_err("tcp");
        assert!(err.to_string().contains("only an HTTP check"));
    }

    #[test]
    fn dns_expectations_match_the_record_type() {
        let ok = validate_spec(
            CheckSpec::Dns(DnsCheck {
                record_type: DnsCheckRecordType::Cname,
                expected: vec!["Edge.Example.NET.".into()],
                timeout_ms: None,
            }),
            &CheckRequest::default(),
        )
        .expect("valid");
        assert_eq!(
            ok,
            CheckSpec::Dns(DnsCheck {
                record_type: DnsCheckRecordType::Cname,
                expected: vec!["edge.example.net".into()],
                timeout_ms: None
            })
        );
        let err = validate_spec(
            CheckSpec::Dns(DnsCheck {
                record_type: DnsCheckRecordType::A,
                expected: vec!["example.com".into()],
                timeout_ms: None,
            }),
            &CheckRequest::default(),
        )
        .expect_err("not an address");
        assert!(err.to_string().contains("IPv4"));
    }

    #[test]
    fn status_patterns_match() {
        assert!(parse_status_pattern("2xx").expect("class").matches(204));
        assert!(!parse_status_pattern("2xx").expect("class").matches(301));
        assert!(parse_status_pattern("404").expect("exact").matches(404));
        assert_eq!(parse_status_pattern("6xx"), None);
        assert_eq!(parse_status_pattern("22xx"), None);
    }

    #[test]
    fn specs_round_trip_as_tagged_json() {
        let spec = CheckSpec::Tls(TlsCheck {
            warn_days: Some(30),
            timeout_ms: None,
        });
        let json = serde_json::to_value(&spec).expect("json");
        assert_eq!(json["kind"], "tls");
        assert_eq!(
            serde_json::from_value::<CheckSpec>(json).expect("back"),
            spec
        );
        assert_eq!(MonitorCheckKind::from_stored("dns"), MonitorCheckKind::Dns);
        assert_eq!(
            MonitorCheckKind::from_stored("web"),
            MonitorCheckKind::Environment
        );
    }
}
