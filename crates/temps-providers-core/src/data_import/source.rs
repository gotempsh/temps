// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Source connection strings: parsing, the SSRF guard, DNS pinning, masking
//! and secret scrubbing. Shared by every engine.
//!
//! Connection strings are parsed here rather than with a generic URL parser
//! because database URLs are not ordinary URLs: MongoDB and libpq both accept
//! several comma-separated hosts in the authority, which general-purpose
//! parsers reject.
//!
//! ## Why pin
//!
//! The source host is resolved and every address checked
//! ([`pin_source_hosts`]) when the import is requested, but the dump tool
//! resolves it again later, inside the helper container. A hostname whose
//! answer changes in between (DNS rebinding) would slip past the check. The
//! runner writes the validated address into the helper's `/etc/hosts`, so the
//! tool dials exactly what was checked while TLS still sees the real name.

use std::net::IpAddr;

use percent_encoding::percent_decode_str;
use temps_core::url_validation::{resolve_and_validate_domain, validate_outbound_ip};

use super::DataImportError;

/// Longest source connection string accepted.
const MAX_SOURCE_LEN: usize = 4096;
/// Longest option value accepted.
const MAX_OPTION_VALUE_LEN: usize = 256;
/// Longest source database name accepted. 64 covers PostgreSQL (63),
/// MariaDB (64) and MongoDB (63).
const MAX_DATABASE_LEN: usize = 64;

/// What one engine accepts in a source connection string.
#[derive(Debug, Clone, Copy)]
pub struct SourceUrlRules<'a> {
    /// Accepted schemes, lowercase.
    pub schemes: &'a [&'a str],
    /// Port used when the URL names none.
    pub default_port: u16,
    /// Accepted query options, in their canonical spelling.
    pub allowed_options: &'a [&'a str],
    /// Compare option names case-insensitively (MongoDB does).
    pub options_case_insensitive: bool,
    /// Most hosts the authority may list.
    pub max_hosts: usize,
    /// Database used when the URL names none (Redis: logical DB `0`).
    /// `None` makes the database part of the URL required.
    pub default_database: Option<&'a str>,
}

/// One `host:port` the source connection string names.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SourceEndpoint {
    /// Host as written: a lowercase DNS name or an IP literal (no brackets).
    pub host: String,
    pub port: u16,
}

impl SourceEndpoint {
    /// `host:port`, bracketing IPv6 literals.
    pub fn authority(&self) -> String {
        if self.host.contains(':') {
            format!("[{}]:{}", self.host, self.port)
        } else {
            format!("{}:{}", self.host, self.port)
        }
    }

    fn ip_literal(&self) -> Option<IpAddr> {
        self.host.parse::<IpAddr>().ok()
    }
}

/// A parsed, validated source connection string.
///
/// Holds the raw string because the dump tool needs it, but never prints it:
/// `Debug` is redacted and [`ImportSource::masked`] is the only form that may
/// be stored, logged or returned.
#[derive(Clone)]
pub struct ImportSource {
    raw: String,
    scheme: String,
    username: Option<String>,
    password: Option<String>,
    endpoints: Vec<SourceEndpoint>,
    database: String,
    options: Vec<(String, String)>,
}

impl std::fmt::Debug for ImportSource {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ImportSource")
            .field("source", &self.masked())
            .finish()
    }
}

impl ImportSource {
    /// The connection string exactly as the caller gave it. Secret.
    pub fn raw(&self) -> &str {
        &self.raw
    }

    pub fn scheme(&self) -> &str {
        &self.scheme
    }

    /// Decoded user name, if the URL has one.
    pub fn username(&self) -> Option<&str> {
        self.username.as_deref()
    }

    /// Decoded password, if the URL has one. Secret.
    pub fn password(&self) -> Option<&str> {
        self.password.as_deref()
    }

    pub fn endpoints(&self) -> &[SourceEndpoint] {
        &self.endpoints
    }

    /// Database the data is read from.
    pub fn database(&self) -> &str {
        &self.database
    }

    /// Query options, names in their canonical spelling.
    pub fn options(&self) -> &[(String, String)] {
        &self.options
    }

    pub fn option(&self, name: &str) -> Option<&str> {
        self.options
            .iter()
            .find(|(key, _)| key == name)
            .map(|(_, value)| value.as_str())
    }

    /// The connection string with user and password replaced by `***`. The
    /// only form of the source that may be stored, logged or returned.
    pub fn masked(&self) -> String {
        let userinfo = match (&self.username, &self.password) {
            (_, Some(_)) => "***:***@",
            (Some(_), None) => "***@",
            (None, None) => "",
        };
        let hosts = self
            .endpoints
            .iter()
            .map(SourceEndpoint::authority)
            .collect::<Vec<_>>()
            .join(",");
        let query = if self.options.is_empty() {
            String::new()
        } else {
            format!(
                "?{}",
                self.options
                    .iter()
                    .map(|(key, value)| format!("{key}={value}"))
                    .collect::<Vec<_>>()
                    .join("&")
            )
        };
        format!(
            "{}://{}{}/{}{}",
            self.scheme, userinfo, hosts, self.database, query
        )
    }

    /// Every form in which a secret of this source could show up in a client
    /// tool's output: the raw string and the password, encoded and decoded.
    pub fn secrets(&self) -> Vec<String> {
        let mut secrets = vec![self.raw.clone()];
        if let Some(password) = &self.password {
            secrets.push(password.clone());
            secrets.push(percent_encode_userinfo(password));
        }
        secrets
    }
}

/// Parse a source connection string under `rules`. Never echoes the input
/// back in an error: it may hold a password.
pub fn parse_source_url(
    raw: &str,
    rules: &SourceUrlRules<'_>,
) -> Result<ImportSource, DataImportError> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return Err(invalid("the source connection string is empty"));
    }
    if trimmed.len() > MAX_SOURCE_LEN {
        return Err(invalid(format!(
            "the source connection string is longer than {MAX_SOURCE_LEN} characters"
        )));
    }
    if trimmed.chars().any(|c| c.is_control() || c.is_whitespace()) {
        return Err(invalid(
            "the source connection string contains spaces or control characters; \
             percent-encode them",
        ));
    }

    let (scheme, rest) = trimmed
        .split_once("://")
        .ok_or_else(|| invalid("expected <scheme>://<user>:<password>@<host>:<port>/<database>"))?;
    let scheme = scheme.to_ascii_lowercase();
    if !rules.schemes.contains(&scheme.as_str()) {
        return Err(invalid(format!(
            "scheme '{}' is not supported here; use {}",
            scheme,
            rules
                .schemes
                .iter()
                .map(|s| format!("{s}://"))
                .collect::<Vec<_>>()
                .join(" or ")
        )));
    }
    if rest.contains('#') {
        return Err(invalid(
            "the source connection string must not contain a '#' fragment; percent-encode '#' \
             in passwords as %23",
        ));
    }

    // The authority ends at the first '/' or '?'. A password holding an
    // unencoded '/', '?' or '@' would move that boundary; refuse instead of
    // guessing.
    let authority_end = rest.find(['/', '?']).unwrap_or(rest.len());
    let (authority, after_authority) = rest.split_at(authority_end);
    if after_authority.contains('@') {
        return Err(invalid(
            "the user name or password contains an unencoded '/', '?' or '@'; percent-encode \
             special characters in credentials",
        ));
    }

    let (userinfo, hostlist) = match authority.rsplit_once('@') {
        Some((userinfo, hostlist)) => (Some(userinfo), hostlist),
        None => (None, authority),
    };
    let (username, password) = match userinfo {
        None => (None, None),
        Some(userinfo) => {
            let (user, pass) = match userinfo.split_once(':') {
                Some((user, pass)) => (user, Some(pass)),
                None => (userinfo, None),
            };
            let user = decode_component(user, "user name")?;
            let pass = pass.map(|p| decode_component(p, "password")).transpose()?;
            (Some(user).filter(|u| !u.is_empty()), pass)
        }
    };

    let endpoints = parse_hosts(hostlist, rules)?;

    let (path, query) = match after_authority.split_once('?') {
        Some((path, query)) => (path, Some(query)),
        None => (after_authority, None),
    };
    let mut database = decode_component(path.trim_start_matches('/'), "database name")?;
    if database.is_empty() {
        if let Some(default) = rules.default_database {
            database = default.to_string();
        }
    }
    validate_source_database(&database)?;

    let options = match query {
        Some(query) => parse_options(query, rules)?,
        None => Vec::new(),
    };

    Ok(ImportSource {
        raw: trimmed.to_string(),
        scheme,
        username,
        password,
        endpoints,
        database,
        options,
    })
}

fn invalid(reason: impl Into<String>) -> DataImportError {
    DataImportError::invalid_source(reason)
}

fn decode_component(value: &str, what: &str) -> Result<String, DataImportError> {
    percent_decode_str(value)
        .decode_utf8()
        .map(|decoded| decoded.into_owned())
        .map_err(|_| {
            DataImportError::invalid_source(format!(
                "the {what} is not valid percent-encoded UTF-8"
            ))
        })
}

fn parse_hosts(
    hostlist: &str,
    rules: &SourceUrlRules<'_>,
) -> Result<Vec<SourceEndpoint>, DataImportError> {
    if hostlist.is_empty() {
        return Err(invalid("the source connection string names no host"));
    }
    let mut endpoints = Vec::new();
    for entry in hostlist.split(',') {
        let (host, port) = if let Some(bracketed) = entry.strip_prefix('[') {
            let (host, tail) = bracketed
                .split_once(']')
                .ok_or_else(|| invalid("an IPv6 host is missing its closing ']'"))?;
            if host.parse::<std::net::Ipv6Addr>().is_err() {
                return Err(invalid(format!("'{host}' is not a valid IPv6 address")));
            }
            let port = match tail.strip_prefix(':') {
                Some(port) => Some(port),
                None if tail.is_empty() => None,
                None => return Err(invalid(format!("unexpected characters after '[{host}]'"))),
            };
            (host.to_string(), port)
        } else {
            match entry.rsplit_once(':') {
                Some((host, port)) => (host.to_ascii_lowercase(), Some(port)),
                None => (entry.to_ascii_lowercase(), None),
            }
        };
        let port = match port {
            Some(port) => port
                .parse::<u16>()
                .ok()
                .filter(|p| *p != 0)
                .ok_or_else(|| {
                    invalid(format!("'{port}' is not a valid port for host '{host}'"))
                })?,
            None => rules.default_port,
        };
        if host.is_empty() {
            return Err(invalid("the source connection string has an empty host"));
        }
        let is_ip = host.parse::<IpAddr>().is_ok();
        let is_name = host.len() <= 253
            && host.split('.').all(|label| {
                !label.is_empty()
                    && label.len() <= 63
                    && label
                        .chars()
                        .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
            });
        if !is_ip && !is_name {
            return Err(invalid(format!(
                "'{host}' is not a valid host name or address"
            )));
        }
        endpoints.push(SourceEndpoint { host, port });
    }
    if endpoints.len() > rules.max_hosts {
        return Err(invalid(format!(
            "the source connection string lists {} hosts; at most {} are accepted",
            endpoints.len(),
            rules.max_hosts
        )));
    }
    Ok(endpoints)
}

/// The source database name is handed to dump tools and, for MongoDB, used
/// in namespace patterns: keep it to a plain identifier.
fn validate_source_database(database: &str) -> Result<(), DataImportError> {
    if database.is_empty() {
        return Err(DataImportError::invalid_source(
            "the source connection string must name the database to import \
             (…@host:port/<database>)",
        ));
    }
    if database.len() > MAX_DATABASE_LEN {
        return Err(DataImportError::invalid_source(format!(
            "the source database name is longer than {MAX_DATABASE_LEN} characters"
        )));
    }
    if !database
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '-' | '.'))
    {
        return Err(DataImportError::invalid_source(format!(
            "source database name '{database}' may only contain letters, digits, '_', '-' and '.'"
        )));
    }
    Ok(())
}

fn parse_options(
    query: &str,
    rules: &SourceUrlRules<'_>,
) -> Result<Vec<(String, String)>, DataImportError> {
    let mut options: Vec<(String, String)> = Vec::new();
    for pair in query.split('&').filter(|p| !p.is_empty()) {
        let (key, value) = pair.split_once('=').unwrap_or((pair, ""));
        let key = decode_component(key, "option name")?;
        let value = decode_component(value, "option value")?;
        let canonical = rules
            .allowed_options
            .iter()
            .find(|allowed| {
                if rules.options_case_insensitive {
                    allowed.eq_ignore_ascii_case(&key)
                } else {
                    **allowed == key
                }
            })
            .ok_or_else(|| {
                invalid(format!(
                    "option '{}' is not allowed; accepted options are: {}",
                    key,
                    rules.allowed_options.join(", ")
                ))
            })?;
        if options.iter().any(|(existing, _)| existing == canonical) {
            return Err(invalid(format!(
                "option '{canonical}' is given more than once"
            )));
        }
        if value.len() > MAX_OPTION_VALUE_LEN || value.chars().any(|c| c.is_control()) {
            return Err(invalid(format!(
                "option '{canonical}' has an invalid value"
            )));
        }
        options.push((canonical.to_string(), value));
    }
    Ok(options)
}

/// A validated address the helper container must use for a host name.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PinnedHost {
    pub host: String,
    pub ip: IpAddr,
}

impl PinnedHost {
    /// Docker `ExtraHosts` entry (`host:ip`).
    pub fn extra_host_entry(&self) -> String {
        format!("{}:{}", self.host, self.ip)
    }
}

/// SSRF guard: every host of the source must be public. Literal addresses are
/// checked as they are; names are resolved, every resolved address checked,
/// and one validated address returned per name so the helper can be pinned
/// to it (see the module documentation).
pub async fn pin_source_hosts(source: &ImportSource) -> Result<Vec<PinnedHost>, DataImportError> {
    let mut pinned = Vec::new();
    for endpoint in source.endpoints() {
        let refused = |reason: String| {
            DataImportError::invalid_source(format!(
                "host '{}' cannot be used as an import source: {} — the source must be a \
                 publicly reachable database server or inside a trusted private network",
                endpoint.host, reason
            ))
        };
        match endpoint.ip_literal() {
            Some(ip) => validate_outbound_ip(ip).map_err(|e| refused(e.to_string()))?,
            None => {
                if (endpoint.host == "localhost" || endpoint.host.ends_with(".localhost"))
                    && validate_outbound_ip(IpAddr::V4(std::net::Ipv4Addr::LOCALHOST)).is_err()
                {
                    return Err(refused("it is a loopback name".to_string()));
                }
                let addresses = resolve_and_validate_domain(&endpoint.host, endpoint.port)
                    .await
                    .map_err(|e| refused(e.to_string()))?;
                // Prefer IPv4: Docker bridge networks often have no IPv6
                // route, and every resolved address was validated anyway.
                let ip = addresses
                    .iter()
                    .map(|a| a.ip())
                    .find(IpAddr::is_ipv4)
                    .or_else(|| addresses.first().map(|a| a.ip()))
                    .ok_or_else(|| refused("it resolves to no address".to_string()))?;
                if !pinned.iter().any(|p: &PinnedHost| p.host == endpoint.host) {
                    pinned.push(PinnedHost {
                        host: endpoint.host.clone(),
                        ip,
                    });
                }
            }
        }
    }
    Ok(pinned)
}

/// Percent-encode a URL userinfo component. Everything outside the RFC 3986
/// unreserved set is encoded, including '%' itself.
pub fn percent_encode_userinfo(raw: &str) -> String {
    let mut encoded = String::with_capacity(raw.len());
    for byte in raw.bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'.' | b'_' | b'~' => {
                encoded.push(byte as char)
            }
            _ => encoded.push_str(&format!("%{byte:02X}")),
        }
    }
    encoded
}

/// Replace every secret in `message` with `***`. Longest secrets first, so a
/// connection string is removed whole before the password inside it.
pub fn scrub_secrets(message: &str, secrets: &[String]) -> String {
    let mut needles: Vec<&str> = secrets
        .iter()
        .map(String::as_str)
        // Very short "secrets" would shred ordinary words.
        .filter(|s| s.len() >= 3)
        .collect();
    needles.sort_by_key(|needle| std::cmp::Reverse(needle.len()));
    needles.dedup();
    let mut scrubbed = message.to_string();
    for needle in needles {
        scrubbed = scrubbed.replace(needle, "***");
    }
    scrubbed
}

#[cfg(test)]
mod tests {
    use super::*;

    const PG_RULES: SourceUrlRules<'static> = SourceUrlRules {
        schemes: &["postgres", "postgresql"],
        default_port: 5432,
        allowed_options: &["sslmode", "connect_timeout"],
        options_case_insensitive: false,
        max_hosts: 4,
        default_database: None,
    };

    const MONGO_RULES: SourceUrlRules<'static> = SourceUrlRules {
        schemes: &["mongodb"],
        default_port: 27017,
        allowed_options: &["authSource", "replicaSet", "tls"],
        options_case_insensitive: true,
        max_hosts: 8,
        default_database: None,
    };

    #[test]
    fn parses_a_complete_url_and_decodes_credentials() {
        let source = parse_source_url(
            "postgresql://app%40corp:p%2Fss%25w0rd@DB.Example.com:6543/shop?sslmode=require",
            &PG_RULES,
        )
        .expect("parse");
        assert_eq!(source.scheme(), "postgresql");
        assert_eq!(source.username(), Some("app@corp"));
        assert_eq!(source.password(), Some("p/ss%w0rd"));
        assert_eq!(
            source.endpoints(),
            &[SourceEndpoint {
                host: "db.example.com".to_string(),
                port: 6543
            }]
        );
        assert_eq!(source.database(), "shop");
        assert_eq!(source.option("sslmode"), Some("require"));
    }

    #[test]
    fn default_port_applies_when_none_is_given() {
        let source =
            parse_source_url("postgres://u:p@db.example.com/shop", &PG_RULES).expect("parse");
        assert_eq!(source.endpoints()[0].port, 5432);
    }

    #[test]
    fn parses_several_hosts_including_ipv6() {
        let source = parse_source_url(
            "mongodb://u:p@a.example.com:27018,[2001:db8::5]:27019,b.example.com/app?replicaSet=rs0",
            &MONGO_RULES,
        )
        .expect("parse");
        let authorities: Vec<String> = source
            .endpoints()
            .iter()
            .map(SourceEndpoint::authority)
            .collect();
        assert_eq!(
            authorities,
            vec![
                "a.example.com:27018",
                "[2001:db8::5]:27019",
                "b.example.com:27017"
            ]
        );
    }

    #[test]
    fn option_names_follow_the_engine_case_rule() {
        let mongo = parse_source_url("mongodb://h.example.com/app?authsource=admin", &MONGO_RULES)
            .expect("mongo options are case-insensitive");
        assert_eq!(mongo.option("authSource"), Some("admin"));

        let error = parse_source_url("postgres://h.example.com/app?SSLMODE=require", &PG_RULES)
            .expect_err("libpq options are case-sensitive");
        assert!(
            error.to_string().contains("'SSLMODE' is not allowed"),
            "{error}"
        );
    }

    #[test]
    fn refuses_options_outside_the_allowlist() {
        // libpq honours host/hostaddr in the query string; accepting them would
        // let a URL whose host passed the SSRF guard connect somewhere else.
        for query in [
            "host=127.0.0.1",
            "hostaddr=10.0.0.1",
            "password=x",
            "dbname=other",
        ] {
            let error =
                parse_source_url(&format!("postgres://h.example.com/app?{query}"), &PG_RULES)
                    .expect_err(query);
            assert!(
                matches!(error, DataImportError::InvalidSource { .. }),
                "{query}"
            );
        }
    }

    #[test]
    fn refuses_duplicate_options() {
        let error = parse_source_url(
            "postgres://h.example.com/app?sslmode=require&sslmode=disable",
            &PG_RULES,
        )
        .expect_err("duplicate");
        assert!(error.to_string().contains("more than once"), "{error}");
    }

    #[test]
    fn refuses_malformed_input_without_echoing_the_password() {
        let cases = [
            "",
            "db.example.com:5432/app",
            "mysql://u:hunter2@h.example.com/app",
            "postgres://u:hunter2@h.example.com/",
            "postgres://u:hunter2@/app",
            "postgres://u:hunter2@h.example.com:99999/app",
            "postgres://u:hun/ter2@h.example.com/app",
            "postgres://u:hunter2@h.example.com/app#frag",
            "postgres://u:hunter2@h ex.com/app",
            "postgres://u:hunter2@h.example.com/a;b",
            "postgres://u:hunter2@[::1/app",
        ];
        for case in cases {
            let error = parse_source_url(case, &PG_RULES).expect_err(case);
            assert!(
                matches!(error, DataImportError::InvalidSource { .. }),
                "{case}: {error}"
            );
            assert!(!error.to_string().contains("hunter2"), "{case}: {error}");
        }
    }

    #[test]
    fn an_engine_default_database_fills_a_missing_path() {
        let rules = SourceUrlRules {
            schemes: &["redis"],
            default_port: 6379,
            allowed_options: &[],
            options_case_insensitive: false,
            max_hosts: 1,
            default_database: Some("0"),
        };
        for url in [
            "redis://:pw@cache.example.com",
            "redis://:pw@cache.example.com/",
        ] {
            assert_eq!(parse_source_url(url, &rules).expect(url).database(), "0");
        }
        assert_eq!(
            parse_source_url("redis://cache.example.com/3", &rules)
                .expect("explicit")
                .database(),
            "3"
        );
    }

    #[test]
    fn refuses_too_many_hosts() {
        let error = parse_source_url("postgres://a.io,b.io,c.io,d.io,e.io/app", &PG_RULES)
            .expect_err("five hosts");
        assert!(error.to_string().contains("at most 4"), "{error}");
    }

    #[test]
    fn masked_form_hides_credentials_and_keeps_everything_else() {
        let source = parse_source_url(
            "postgres://admin:s3cr%3At@db.example.com:5433/shop?sslmode=require",
            &PG_RULES,
        )
        .expect("parse");
        let masked = source.masked();
        assert_eq!(
            masked,
            "postgres://***:***@db.example.com:5433/shop?sslmode=require"
        );
        assert!(!format!("{source:?}").contains("s3cr"));
    }

    #[test]
    fn secrets_cover_the_raw_string_and_both_password_encodings() {
        let source = parse_source_url("postgres://u:a%2Fb%40c@db.example.com/app", &PG_RULES)
            .expect("parse");
        let secrets = source.secrets();
        assert!(secrets.contains(&"postgres://u:a%2Fb%40c@db.example.com/app".to_string()));
        assert!(secrets.contains(&"a/b@c".to_string()));
        assert!(secrets.contains(&"a%2Fb%40c".to_string()));
    }

    #[test]
    fn scrub_removes_whole_urls_before_their_passwords() {
        let secrets = vec![
            "postgres://u:hunter2@db.example.com/app".to_string(),
            "hunter2".to_string(),
            "x".to_string(),
        ];
        let message = "pg_dump: connection to postgres://u:hunter2@db.example.com/app failed; \
                       password hunter2 rejected; x marks the spot";
        let scrubbed = scrub_secrets(message, &secrets);
        assert!(!scrubbed.contains("hunter2"), "{scrubbed}");
        assert!(scrubbed.contains("connection to *** failed"), "{scrubbed}");
        // One-character secrets are ignored rather than shredding the text.
        assert!(scrubbed.contains("x marks"), "{scrubbed}");
    }

    #[tokio::test]
    async fn ssrf_guard_refuses_private_loopback_and_metadata_sources() {
        for url in [
            "postgres://u:p@127.0.0.1/app",
            "postgres://u:p@10.1.2.3/app",
            "postgres://u:p@192.168.1.10/app",
            "postgres://u:p@169.254.169.254/app",
            "postgres://u:p@[::1]/app",
            "postgres://u:p@localhost/app",
            "postgres://u:p@db.localhost/app",
        ] {
            let source = parse_source_url(url, &PG_RULES).expect(url);
            let error = pin_source_hosts(&source).await.expect_err(url);
            assert!(
                matches!(error, DataImportError::InvalidSource { .. }),
                "{url}"
            );
            assert!(
                error.to_string().contains("publicly reachable"),
                "{url}: {error}"
            );
        }
    }

    #[tokio::test]
    async fn public_ip_literals_need_no_pin() {
        let source = parse_source_url("postgres://u:p@203.0.113.10/app", &PG_RULES).expect("parse");
        // TEST-NET-3 is documentation space; whether it is "public" is the
        // validator's call — either way a literal is never pinned.
        if let Ok(pins) = pin_source_hosts(&source).await {
            assert!(pins.is_empty());
        }
    }

    #[test]
    fn pinned_host_entries_use_docker_extra_hosts_syntax() {
        let pin = PinnedHost {
            host: "db.example.com".to_string(),
            ip: "198.51.100.7".parse().expect("ip"),
        };
        assert_eq!(pin.extra_host_entry(), "db.example.com:198.51.100.7");
    }
}
