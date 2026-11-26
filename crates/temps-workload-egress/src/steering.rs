// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Sending a workload's HTTP(S) through the recorder with proxy variables.
//!
//! A sandbox has no other way out, so its traffic reaches the recorder
//! whatever it does. A container that reaches the network directly is
//! steered instead: it is started with the standard proxy variables
//! pointing at the recorder. That is opt-in and cooperative: it records
//! what clients that honour those variables send (nearly every HTTP
//! library and tool), not raw sockets, and it takes effect when a
//! container starts.
//!
//! **Only the public internet goes through.** The recorder connects to
//! public addresses only, so everything internal has to stay direct, which
//! is what `NO_PROXY` is for here: loopback, private ranges, internal name
//! suffixes, the names the workload's own variables already point at
//! inside the platform (a linked service's container name), and the
//! platform's own endpoints. What `NO_PROXY` misses is refused by the
//! recorder and shows up, refused, in the workload's activity.
//!
//! **A workload's own proxy settings win.** A variable the workload
//! already sets is left alone, and its `NO_PROXY` entries are kept.
//!
//! Any workload kind whose containers reach the network directly uses
//! this; it is not specific to one.

use std::collections::{BTreeMap, HashMap};
use std::net::{IpAddr, SocketAddr};

/// The label a container carries when it was started with the proxy
/// variables: its value is [`steered_label`], the installation that steered
/// it and the recorder address it was given. The recorder keeps every such
/// container of its own installation in its table, so turning recording off
/// never cuts off a running container.
pub const STEERED_LABEL: &str = "sh.temps.egress-proxy";

/// Which installation's recorder steered a container. Two installations can
/// share a Docker daemon and its networks: a recorder that took the other's
/// listener over must never take the other's containers for its own (their
/// numeric environment ids can be the same). Derived from the installation's
/// encryption key, so it is stable across restarts and differs between
/// installations; it reveals nothing about the key.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct InstallationId([u8; 8]);

impl InstallationId {
    pub fn derive(encryption: &temps_core::EncryptionService) -> Self {
        let key = encryption.derive_subkey("temps.egress-recording.installation");
        let mut id = [0u8; 8];
        id.copy_from_slice(&key[..8]);
        Self(id)
    }

    pub fn from_bytes(id: [u8; 8]) -> Self {
        Self(id)
    }
}

impl std::fmt::Display for InstallationId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.0.iter().try_for_each(|b| write!(f, "{b:02x}"))
    }
}

/// The [`STEERED_LABEL`] value of a container `installation` steered to its
/// recorder at `address`.
pub fn steered_label(installation: InstallationId, address: SocketAddr) -> String {
    format!("{installation}@{address}")
}

/// Whether a [`STEERED_LABEL`] value says `installation` steered it.
pub fn steered_by(label: &str, installation: InstallationId) -> bool {
    label
        .split_once('@')
        .is_some_and(|(id, _)| id == installation.to_string())
}

/// The proxy variables, in both cases: tools disagree on which they read.
pub const PROXY_VARIABLES: [(&str, &str); 2] =
    [("HTTP_PROXY", "http_proxy"), ("HTTPS_PROXY", "https_proxy")];

/// Destinations that never go through the recorder: this host, private and
/// link-local ranges (for the clients that take ranges) and internal name
/// suffixes.
pub const ALWAYS_DIRECT: [&str; 12] = [
    "localhost",
    "127.0.0.1",
    "::1",
    ".local",
    ".internal",
    ".localhost",
    "10.0.0.0/8",
    "172.16.0.0/12",
    "192.168.0.0/16",
    "169.254.0.0/16",
    "100.64.0.0/10",
    "fc00::/7",
];

/// What to add to a workload's variables to steer it.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ProxyVariables {
    /// Variables to set, replacing the workload's `NO_PROXY`/`no_proxy`
    /// with a merged list.
    pub variables: BTreeMap<String, String>,
    /// Proxy variables the workload already sets, left alone: what uses
    /// them is not recorded.
    pub kept: Vec<String>,
}

impl ProxyVariables {
    /// Whether anything is steered: at least one proxy variable was set.
    pub fn steers(&self) -> bool {
        PROXY_VARIABLES
            .iter()
            .any(|(upper, _)| self.variables.contains_key(*upper))
    }
}

/// The variables that send `existing`'s workload through the recorder at
/// `proxy`, keeping `direct` (the platform's own hosts) and everything
/// internal direct.
pub fn proxy_variables(
    proxy: SocketAddr,
    existing: &HashMap<String, String>,
    direct: &[String],
) -> ProxyVariables {
    let set = |name: &str| existing.get(name).is_some_and(|value| !value.is_empty());
    let url = format!("http://{proxy}");
    let mut steering = ProxyVariables::default();
    for (upper, lower) in PROXY_VARIABLES {
        if set(upper) || set(lower) {
            steering.kept.push(upper.to_string());
            continue;
        }
        steering.variables.insert(upper.to_string(), url.clone());
        steering.variables.insert(lower.to_string(), url.clone());
    }
    if !steering.steers() {
        return steering;
    }

    let mut entries: Vec<String> = Vec::new();
    let mut add = |entry: &str| {
        let entry = entry.trim().to_ascii_lowercase();
        if !entry.is_empty() && !entries.contains(&entry) {
            entries.push(entry);
        }
    };
    for name in ["NO_PROXY", "no_proxy"] {
        if let Some(value) = existing.get(name) {
            value.split(',').for_each(&mut add);
        }
    }
    ALWAYS_DIRECT.iter().copied().for_each(&mut add);
    direct.iter().map(String::as_str).for_each(&mut add);
    let mut internal: Vec<String> = existing
        .iter()
        .flat_map(|(name, value)| hosts_in(name, value))
        .filter(|host| is_internal(host))
        .collect();
    internal.sort();
    internal.iter().map(String::as_str).for_each(&mut add);

    let no_proxy = entries.join(",");
    steering
        .variables
        .insert("NO_PROXY".to_string(), no_proxy.clone());
    steering.variables.insert("no_proxy".to_string(), no_proxy);
    steering
}

/// The hosts of the URLs in `values`, for the platform's own endpoints a
/// workload was given (its API, its telemetry intake), which stay direct
/// whatever they resolve to.
pub fn url_hosts<'a>(values: impl IntoIterator<Item = &'a str>) -> Vec<String> {
    let mut hosts: Vec<String> = values.into_iter().flat_map(url_hosts_in).collect();
    hosts.sort();
    hosts.dedup();
    hosts
}

/// A destination that is inside the platform or this host: a name without
/// a dot (a container name), an internal suffix, or an address that is not
/// public.
pub fn is_internal(host: &str) -> bool {
    if let Ok(address) = host.parse::<IpAddr>() {
        return !temps_workload_netpol::egress::is_public(address);
    }
    host == "localhost"
        || !host.contains('.')
        || [".local", ".internal", ".localhost"]
            .iter()
            .any(|suffix| host.ends_with(suffix))
}

/// The hosts a variable names: every host of a URL in its value (a
/// connection string may list several), or the value itself when the
/// variable is a host (`REDIS_HOST=redis-cache`).
fn hosts_in(name: &str, value: &str) -> Vec<String> {
    let hosts = url_hosts_in(value);
    if !hosts.is_empty() || !names_a_host(name) {
        return hosts;
    }
    host_of(value).into_iter().collect()
}

/// Words that make a variable a credential, whatever else its name says.
const CREDENTIAL_WORDS: [&str; 10] = [
    "TOKEN",
    "SECRET",
    "PASSWORD",
    "PASSWD",
    "PASS",
    "KEY",
    "APIKEY",
    "AUTH",
    "CREDENTIAL",
    "CREDENTIALS",
];

/// Whether a variable's value is a host by its name: its last word is
/// `HOST` or `HOSTNAME` (`REDIS_HOST`, `DB_HOSTNAME`), or it ends in `HOST`
/// (`PGHOST`), and no word of it says credential. `HOSTING_API_TOKEN` is
/// not: its value would otherwise be copied verbatim into `NO_PROXY`,
/// which diagnostics print.
fn names_a_host(name: &str) -> bool {
    let upper = name.to_ascii_uppercase();
    let words: Vec<&str> = upper
        .split(['_', '-', '.'])
        .filter(|w| !w.is_empty())
        .collect();
    if words.iter().any(|word| CREDENTIAL_WORDS.contains(word)) {
        return false;
    }
    matches!(words.last(), Some(&"HOST") | Some(&"HOSTNAME")) || upper.ends_with("HOST")
}

/// Every host after a `://` in `value`: `scheme://[user[:pass]@]host[:port]`,
/// with several comma-separated hosts allowed.
fn url_hosts_in(value: &str) -> Vec<String> {
    let mut hosts = Vec::new();
    let mut rest = value;
    while let Some(at) = rest.find("://") {
        let after = &rest[at + 3..];
        let end = after
            .find(|c: char| matches!(c, '/' | '?' | '#') || c.is_whitespace())
            .unwrap_or(after.len());
        let authority = &after[..end];
        let authority = authority
            .rsplit_once('@')
            .map_or(authority, |(_, hosts)| hosts);
        hosts.extend(authority.split(',').filter_map(host_of));
        rest = &after[end..];
    }
    hosts
}

/// `host`, `host:port` or `[v6]:port` as a lower-case host, when it is one.
fn host_of(value: &str) -> Option<String> {
    let value = value.trim();
    let host = match value.strip_prefix('[') {
        Some(bracketed) => bracketed.split_once(']')?.0,
        None => match value.rsplit_once(':') {
            Some((host, port)) if port.parse::<u16>().is_ok() => host,
            _ => value,
        },
    };
    let valid = !host.is_empty()
        && host.len() <= 253
        && (host.parse::<IpAddr>().is_ok()
            || host
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'-' | b'_')));
    valid.then(|| host.to_ascii_lowercase())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn proxy() -> SocketAddr {
        "172.18.0.1:13128".parse().unwrap()
    }

    fn variables(entries: &[(&str, &str)]) -> HashMap<String, String> {
        entries
            .iter()
            .map(|(name, value)| (name.to_string(), value.to_string()))
            .collect()
    }

    fn no_proxy(steering: &ProxyVariables) -> Vec<&str> {
        steering.variables["NO_PROXY"].split(',').collect()
    }

    /// A credential whose name mentions a host is never read as one: its
    /// value would be copied verbatim into `NO_PROXY`, which diagnostics
    /// print. Host variables still are.
    #[test]
    fn credentials_are_never_copied_into_no_proxy() {
        let existing = variables(&[
            ("HOSTING_API_TOKEN", "abcdef0123456789secret"),
            ("REDIS_HOST_PASSWORD", "hunter2value"),
            ("HOST_KEY", "keyvalue99"),
            ("REDIS_HOST", "redis-cache"),
            ("PGHOST", "postgres-main"),
            ("DB_HOSTNAME", "db-primary"),
        ]);
        let steering = proxy_variables(proxy(), &existing, &[]);
        let entries = no_proxy(&steering);
        for secret in ["abcdef0123456789secret", "hunter2value", "keyvalue99"] {
            assert!(!entries.contains(&secret), "{secret} leaked: {entries:?}");
        }
        for host in ["redis-cache", "postgres-main", "db-primary"] {
            assert!(entries.contains(&host), "{host} missing: {entries:?}");
        }
    }

    /// The label names the installation that steered a container, and only
    /// that installation's recorder takes it for its own.
    #[test]
    fn a_steered_label_names_its_installation() {
        let ours = InstallationId::from_bytes([1, 2, 3, 4, 5, 6, 7, 8]);
        let theirs = InstallationId::from_bytes([9; 8]);
        let label = steered_label(ours, proxy());
        assert_eq!(label, "0102030405060708@172.18.0.1:13128");
        assert!(steered_by(&label, ours));
        assert!(!steered_by(&label, theirs));
        assert!(
            !steered_by("172.18.0.1:13128", ours),
            "an unnamed steering is nobody's"
        );
    }

    #[test]
    fn a_workload_is_given_the_recorder_in_both_cases() {
        let steering = proxy_variables(proxy(), &HashMap::new(), &[]);
        assert!(steering.steers());
        for name in ["HTTP_PROXY", "HTTPS_PROXY", "http_proxy", "https_proxy"] {
            assert_eq!(
                steering.variables[name], "http://172.18.0.1:13128",
                "{name}"
            );
        }
        assert_eq!(
            steering.variables["NO_PROXY"],
            steering.variables["no_proxy"]
        );
        assert!(steering.kept.is_empty());
        let direct = no_proxy(&steering);
        for entry in ["localhost", "127.0.0.1", ".internal", "10.0.0.0/8"] {
            assert!(direct.contains(&entry), "{entry} in {direct:?}");
        }
    }

    /// Everything the workload's own variables point at inside the platform
    /// stays direct: a linked service by container name, a private address,
    /// an internal name. A public API it calls is left to the recorder.
    #[test]
    fn what_the_workload_already_reaches_inside_the_platform_stays_direct() {
        let existing = variables(&[
            (
                "DATABASE_URL",
                "postgres://app:s3cret@postgres-main:5432/app?sslmode=disable",
            ),
            (
                "MONGO_URL",
                "mongodb://user:p%40ss@mongo-a:27017,mongo-b:27017/app",
            ),
            ("S3_ENDPOINT", "http://10.20.0.7:9000"),
            ("REDIS_HOST", "redis-cache"),
            ("CACHE_HOSTNAME", "cache.temps.local:6379"),
            ("PAYMENTS_API", "https://api.example.com/v1"),
            ("PUBLIC_HOST", "www.example.com"),
            ("NODE_ENV", "production"),
        ]);
        let steering = proxy_variables(proxy(), &existing, &[]);
        let direct = no_proxy(&steering);
        for host in [
            "postgres-main",
            "mongo-a",
            "mongo-b",
            "10.20.0.7",
            "redis-cache",
            "cache.temps.local",
        ] {
            assert!(direct.contains(&host), "{host} in {direct:?}");
        }
        for recorded in ["api.example.com", "www.example.com", "production", "app"] {
            assert!(!direct.contains(&recorded), "{recorded} in {direct:?}");
        }
        assert!(
            !steering.variables["NO_PROXY"].contains("s3cret"),
            "no credential leaves the connection string"
        );
    }

    #[test]
    fn the_platforms_own_endpoints_stay_direct_wherever_they_are() {
        let hosts = url_hosts([
            "https://console.example.com/api",
            "https://key@console.example.com/42",
            "http://[2001:db8::1]:4318/v1/traces",
            "not a url",
        ]);
        assert_eq!(hosts, ["2001:db8::1", "console.example.com"]);
        let steering = proxy_variables(proxy(), &HashMap::new(), &hosts);
        assert!(no_proxy(&steering).contains(&"console.example.com"));
    }

    /// A workload that already goes through a proxy of its own keeps it,
    /// for that scheme, and its `NO_PROXY` entries survive the merge.
    #[test]
    fn a_workloads_own_proxy_settings_win() {
        let existing = variables(&[
            ("https_proxy", "http://corp-proxy.example.com:8080"),
            ("NO_PROXY", "metrics.example.com, LOCALHOST"),
        ]);
        let steering = proxy_variables(proxy(), &existing, &[]);
        assert_eq!(steering.kept, ["HTTPS_PROXY"]);
        assert!(!steering.variables.contains_key("HTTPS_PROXY"));
        assert!(!steering.variables.contains_key("https_proxy"));
        assert_eq!(steering.variables["HTTP_PROXY"], "http://172.18.0.1:13128");
        let direct = no_proxy(&steering);
        assert_eq!(&direct[..2], ["metrics.example.com", "localhost"]);
        assert_eq!(
            direct.iter().filter(|entry| **entry == "localhost").count(),
            1,
            "entries are not repeated"
        );

        let both = variables(&[("HTTP_PROXY", "http://a:1"), ("HTTPS_PROXY", "http://a:1")]);
        let nothing = proxy_variables(proxy(), &both, &[]);
        assert!(!nothing.steers());
        assert!(nothing.variables.is_empty(), "{nothing:?}");
    }

    #[test]
    fn internal_means_a_container_name_an_internal_suffix_or_a_private_address() {
        for host in [
            "postgres-main",
            "localhost",
            "db.temps.local",
            "host.docker.internal",
            "10.0.0.5",
            "127.0.0.1",
            "169.254.169.254",
            "::1",
        ] {
            assert!(is_internal(host), "{host}");
        }
        for host in ["api.example.com", "93.184.216.34", "2606:4700::1111"] {
            assert!(!is_internal(host), "{host}");
        }
    }
}
