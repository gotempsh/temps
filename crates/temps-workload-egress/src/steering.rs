// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Sending a workload's HTTPS through the recorder with proxy variables.
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
//! **Only HTTPS is steered.** Plain HTTP stays direct: a container calls its
//! siblings by names only its Docker network resolves (`http://inventory`),
//! often named nowhere but in its own code, and no `NO_PROXY` entry can say
//! "every name without a dot" to every client. The recorder, on the host,
//! could neither resolve such a name nor reach a private address, so a
//! steered plain-HTTP call to a sibling would break. A sibling called over
//! HTTPS by such a name is rare (it needs a certificate for it); one named
//! in the workload's variables stays direct through `NO_PROXY`.
//!
//! **A workload's own proxy settings win.** When the workload, or the
//! image it runs, already goes through a proxy of its own (`HTTPS_PROXY`,
//! `HTTP_PROXY`, or `ALL_PROXY`, which clients use when no scheme-specific
//! variable is set) nothing is steered: a steered `HTTPS_PROXY` would take
//! precedence over its HTTPS route, and the `NO_PROXY` steering writes
//! applies to every scheme, so it would send plain HTTP its proxy carried
//! (to a private service only that proxy reaches) direct. Its own
//! `NO_PROXY` entries are kept in the merged list when it is steered.
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

/// The proxy variable steering sets, in both cases: tools disagree on which
/// they read.
pub const STEERED_VARIABLES: (&str, &str) = ("HTTPS_PROXY", "https_proxy");

/// Variables that already route a workload through a proxy of its own, in
/// both cases: with any of them set nothing is steered.
pub const OWN_PROXY_ROUTE: [(&str, &str); 3] = [
    ("HTTPS_PROXY", "https_proxy"),
    ("HTTP_PROXY", "http_proxy"),
    ("ALL_PROXY", "all_proxy"),
];

/// What [`proxy_variables`] reads from an image's own environment: what
/// routes it, and what it keeps direct. Nothing else of an image's `ENV` is
/// read, since its values are whatever the Dockerfile baked in.
pub const ROUTING_VARIABLES: [&str; 8] = [
    "HTTPS_PROXY",
    "https_proxy",
    "ALL_PROXY",
    "all_proxy",
    "HTTP_PROXY",
    "http_proxy",
    "NO_PROXY",
    "no_proxy",
];

/// The routing variables of an image's `ENV` entries (`NAME=value`), for
/// [`proxy_variables`] to treat as the workload's own: a container takes
/// its image's `ENV` unless its own variables replace it.
pub fn image_routing_variables<'a>(
    env: impl IntoIterator<Item = &'a str>,
) -> HashMap<String, String> {
    env.into_iter()
        .filter_map(|entry| entry.split_once('='))
        .filter(|(name, _)| ROUTING_VARIABLES.contains(name))
        .map(|(name, value)| (name.to_string(), value.to_string()))
        .collect()
}

/// The entries of an image's `ENV` (`NAME=value`) that point at internal
/// hosts (`INVENTORY_URL=https://inventory`, `DB_HOST=postgres-main`), for
/// [`proxy_variables_with_image`]: a container reaches them as its image
/// says unless its own variables replace them, so they stay direct too.
/// Read the way the workload's own variables are: never a credential, and
/// only hosts that are internal ([`is_internal`]), so an image cannot
/// exempt a public destination from recording.
pub fn image_host_variables<'a>(env: impl IntoIterator<Item = &'a str>) -> HashMap<String, String> {
    env.into_iter()
        .filter_map(|entry| entry.split_once('='))
        .filter(|(name, _)| !ROUTING_VARIABLES.contains(name))
        .filter(|(name, value)| !direct_hosts_in(name, value).is_empty())
        .map(|(name, value)| (name.to_string(), value.to_string()))
        .collect()
}

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
    /// with a merged list. Empty when nothing is steered.
    pub variables: BTreeMap<String, String>,
    /// The variables that already route the workload through a proxy of
    /// its own, left alone: nothing is steered then.
    pub kept: Vec<String>,
    /// The variables naming a single-label host put in `NO_PROXY`
    /// (`INVENTORY_URL=https://inventory`, a container or alias only the
    /// workload's network resolves): their names, never their values,
    /// which may be secret. Clients read such an entry as a domain suffix
    /// too, so HTTPS to any host under it (`*.inventory`, or `*.dev` for a
    /// service named `dev`) also goes direct and is not recorded; the
    /// deploy log names these variables. Kept direct anyway: through the
    /// recorder, which cannot resolve them, the workload's calls would fail.
    pub bare_name_variables: Vec<String>,
}

impl ProxyVariables {
    /// Whether anything is steered: the HTTPS proxy variable was set.
    pub fn steers(&self) -> bool {
        self.variables.contains_key(STEERED_VARIABLES.0)
    }
}

/// The variables that send `existing`'s workload's HTTPS through the
/// recorder at `proxy`, keeping `direct` (the platform's own hosts) and
/// everything internal direct. `existing` is what the container starts
/// with: its own variables over its image's routing variables
/// ([`image_routing_variables`]).
pub fn proxy_variables(
    proxy: SocketAddr,
    existing: &HashMap<String, String>,
    direct: &[String],
) -> ProxyVariables {
    proxy_variables_with_image(proxy, existing, &HashMap::new(), direct)
}

/// [`proxy_variables`], also keeping direct the internal hosts the image's
/// `ENV` names (`image`, from [`image_host_variables`]) that the
/// workload's own variables do not replace: a variable `existing` sets is
/// the one the container sees, so the image's value of it is not read.
pub fn proxy_variables_with_image(
    proxy: SocketAddr,
    existing: &HashMap<String, String>,
    image: &HashMap<String, String>,
    direct: &[String],
) -> ProxyVariables {
    let set = |name: &str| existing.get(name).is_some_and(|value| !value.is_empty());
    let mut steering = ProxyVariables::default();
    for (upper, lower) in OWN_PROXY_ROUTE {
        if set(upper) || set(lower) {
            steering.kept.push(upper.to_string());
        }
    }
    if !steering.kept.is_empty() {
        return steering;
    }
    let url = format!("http://{proxy}");
    let (upper, lower) = STEERED_VARIABLES;
    steering.variables.insert(upper.to_string(), url.clone());
    steering.variables.insert(lower.to_string(), url);

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
    // What the container sees: its own variables, then the image's that
    // it does not replace.
    let effective = existing.iter().chain(
        image
            .iter()
            .filter(|(name, _)| !existing.contains_key(name.as_str())),
    );
    let mut internal: Vec<String> = Vec::new();
    let mut bare_name_variables: Vec<String> = Vec::new();
    for (name, value) in effective {
        let hosts = direct_hosts_in(name, value);
        if hosts.iter().any(|host| is_bare_name(host)) {
            bare_name_variables.push(name.clone());
        }
        internal.extend(hosts);
    }
    internal.sort();
    internal.iter().map(String::as_str).for_each(&mut add);
    bare_name_variables.sort();
    bare_name_variables.dedup();
    steering.bare_name_variables = bare_name_variables;

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

/// A single-label name, such as a container name: not an address, not
/// `localhost`, no dot.
fn is_bare_name(host: &str) -> bool {
    host.parse::<IpAddr>().is_err() && host != "localhost" && !host.contains('.')
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

/// The internal hosts `name=value` names that must stay direct. A dotted
/// internal name or a private address always does. A single-label name
/// (`inventory`) only when it is the host of an `https://` or `wss://` URL:
/// only HTTPS goes through the recorder, and clients read a single-label
/// `NO_PROXY` entry as a domain suffix too (`dev` would exempt every
/// `*.dev` host), so a name a database driver or a plain-HTTP call uses
/// (`DB_HOST=dev`, `postgres://postgres-main`, `http://inventory`) is
/// never put there.
fn direct_hosts_in(name: &str, value: &str) -> Vec<String> {
    let https = https_hosts_in(value);
    hosts_in(name, value)
        .into_iter()
        .filter(|host| is_internal(host))
        .filter(|host| !is_bare_name(host) || https.contains(host))
        .collect()
}

/// The hosts of the `https://` and `wss://` URLs in `value`.
fn https_hosts_in(value: &str) -> Vec<String> {
    let lower = value.to_ascii_lowercase();
    let mut hosts = Vec::new();
    for (at, _) in lower.match_indices("://") {
        let scheme = &lower[..at];
        if !(scheme.ends_with("https") || scheme.ends_with("wss")) {
            continue;
        }
        let url = &value[at..];
        let end = url[3..]
            .find(|c: char| matches!(c, '/' | '?' | '#') || c.is_whitespace())
            .map_or(url.len(), |end| end + 3);
        hosts.extend(url_hosts_in(&url[..end]));
    }
    hosts
}

/// The hosts a variable names: every host of a URL in its value (a
/// connection string may list several), or the value itself when the
/// variable is a host (`REDIS_HOST=redis-cache`).
fn hosts_in(name: &str, value: &str) -> Vec<String> {
    // A credential is never read, whatever its value looks like: a password
    // such as `prefix://secret` would otherwise be taken for a URL and its
    // "host" copied into `NO_PROXY`.
    if is_credential(name) {
        return Vec::new();
    }
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
    if is_credential(name) {
        return false;
    }
    let upper = name.to_ascii_uppercase();
    let words: Vec<&str> = upper
        .split(['_', '-', '.'])
        .filter(|w| !w.is_empty())
        .collect();
    matches!(words.last(), Some(&"HOST") | Some(&"HOSTNAME")) || upper.ends_with("HOST")
}

/// Last words that make a variable a locator, whatever else its name
/// says: `AUTH_URL`, `TOKEN_ENDPOINT` and `KEY_SERVER_HOST` hold where a
/// service is, not a secret.
const LOCATOR_WORDS: [&str; 9] = [
    "URL", "URI", "ENDPOINT", "HOST", "HOSTNAME", "ADDR", "ADDRESS", "SERVER", "DSN",
];

/// Whether `name` holds a credential, whose value is never read: a word of
/// it says credential (`DATABASE_PASSWORD`, `HOSTING_API_TOKEN`,
/// `HOST_KEY`) and its last word does not make it a locator (`AUTH_URL`,
/// `OAUTH_TOKEN_URL`, whose hosts are read like any URL's; a URL's
/// credentials are never taken as its host).
fn is_credential(name: &str) -> bool {
    let upper = name.to_ascii_uppercase();
    let words: Vec<&str> = upper
        .split(['_', '-', '.'])
        .filter(|w| !w.is_empty())
        .collect();
    let locator = words
        .last()
        .is_some_and(|last| LOCATOR_WORDS.contains(last));
    !locator && words.iter().any(|word| CREDENTIAL_WORDS.contains(word))
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
            ("REDIS_HOST", "redis.internal"),
            ("PGHOST", "postgres-main.temps.local"),
            ("DB_HOSTNAME", "10.20.0.8"),
        ]);
        let steering = proxy_variables(proxy(), &existing, &[]);
        let entries = no_proxy(&steering);
        for secret in ["abcdef0123456789secret", "hunter2value", "keyvalue99"] {
            assert!(!entries.contains(&secret), "{secret} leaked: {entries:?}");
        }
        for host in ["redis.internal", "postgres-main.temps.local", "10.20.0.8"] {
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

    /// HTTPS goes through the recorder, in both cases; plain HTTP stays
    /// direct, so a call to a sibling by a name only its Docker network
    /// resolves (`http://inventory`) never reaches a recorder that cannot.
    #[test]
    fn a_workload_is_given_the_recorder_for_https_in_both_cases() {
        let steering = proxy_variables(proxy(), &HashMap::new(), &[]);
        assert!(steering.steers());
        for name in ["HTTPS_PROXY", "https_proxy"] {
            assert_eq!(
                steering.variables[name], "http://172.18.0.1:13128",
                "{name}"
            );
        }
        for name in ["HTTP_PROXY", "http_proxy", "ALL_PROXY", "all_proxy"] {
            assert!(!steering.variables.contains_key(name), "{name} is not set");
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
            ("INVENTORY_URL", "https://inventory:8443/v1"),
            ("PAYMENTS_API", "https://api.example.com/v1"),
            ("PUBLIC_HOST", "www.example.com"),
            ("NODE_ENV", "production"),
        ]);
        let steering = proxy_variables(proxy(), &existing, &[]);
        let direct = no_proxy(&steering);
        for host in ["10.20.0.7", "cache.temps.local", "inventory"] {
            assert!(direct.contains(&host), "{host} in {direct:?}");
        }
        // A single-label name only an HTTPS URL needs direct: a database
        // driver's (`postgres://postgres-main`, `REDIS_HOST=redis-cache`)
        // never goes through the proxy, and in NO_PROXY it would exempt
        // every host under it as a suffix.
        for not_needed in ["postgres-main", "mongo-a", "mongo-b", "redis-cache"] {
            assert!(!direct.contains(&not_needed), "{not_needed} in {direct:?}");
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

    /// A workload whose HTTPS already goes through a proxy of its own,
    /// by `HTTPS_PROXY` or by `ALL_PROXY` (which a scheme-specific variable
    /// would take precedence over), is not steered at all. Its own
    /// `NO_PROXY` entries survive the merge when it is.
    #[test]
    fn a_workloads_own_proxy_settings_win() {
        for (name, kept) in [
            ("https_proxy", "HTTPS_PROXY"),
            ("HTTPS_PROXY", "HTTPS_PROXY"),
            ("ALL_PROXY", "ALL_PROXY"),
            ("all_proxy", "ALL_PROXY"),
            // NO_PROXY applies to every scheme: steering would send plain
            // HTTP this proxy carries direct.
            ("http_proxy", "HTTP_PROXY"),
            ("HTTP_PROXY", "HTTP_PROXY"),
        ] {
            let existing = variables(&[(name, "http://corp-proxy.example.com:8080")]);
            let steering = proxy_variables(proxy(), &existing, &[]);
            assert_eq!(steering.kept, [kept], "{name}");
            assert!(!steering.steers(), "{name}");
            assert!(steering.variables.is_empty(), "{name}: {steering:?}");
        }

        // Steered, the workload's own NO_PROXY entries survive the merge.
        let existing = variables(&[("NO_PROXY", "metrics.example.com, LOCALHOST")]);
        let steering = proxy_variables(proxy(), &existing, &[]);
        assert!(steering.steers() && steering.kept.is_empty());
        let direct = no_proxy(&steering);
        assert_eq!(&direct[..2], ["metrics.example.com", "localhost"]);
        assert_eq!(
            direct.iter().filter(|entry| **entry == "localhost").count(),
            1,
            "entries are not repeated"
        );
    }

    /// An image's own routing variables count as the workload's: an image
    /// that sets `HTTPS_PROXY` is not steered, and nothing else of its
    /// `ENV` is read.
    #[test]
    fn an_images_own_routing_variables_count() {
        let image = image_routing_variables([
            "PATH=/usr/local/bin:/usr/bin",
            "HTTPS_PROXY=http://image-proxy.example.com:8080",
            "API_TOKEN=abcdef0123456789",
            "no_proxy=metrics.example.com",
            "MALFORMED",
        ]);
        assert_eq!(
            image,
            variables(&[
                ("HTTPS_PROXY", "http://image-proxy.example.com:8080"),
                ("no_proxy", "metrics.example.com"),
            ])
        );
        let steering = proxy_variables(proxy(), &image, &[]);
        assert_eq!(steering.kept, ["HTTPS_PROXY"]);
        assert!(!steering.steers());
    }

    /// An internal host named only in the image's `ENV` stays direct as
    /// well: a call to `https://inventory` that worked before recording
    /// keeps working. A public host or a credential in it is never read,
    /// and an image variable the deployment replaces is not read either:
    /// the container never sees its value.
    #[test]
    fn internal_hosts_the_image_names_stay_direct_unless_replaced() {
        let image = image_host_variables([
            "INVENTORY_URL=https://inventory",
            "DB_HOST=postgres-main.temps.local",
            "API_URL=https://dev",
            "PAYMENTS_API=https://api.example.com",
            "REDIS_HOST_PASSWORD=hunter2value",
            "PATH=/usr/local/bin:/usr/bin",
        ]);
        let mut names: Vec<&str> = image.keys().map(String::as_str).collect();
        names.sort_unstable();
        assert_eq!(names, ["API_URL", "DB_HOST", "INVENTORY_URL"]);
        // The deployment replaces API_URL with a public host.
        let existing = variables(&[("API_URL", "https://api.example.dev")]);
        let steering = proxy_variables_with_image(proxy(), &existing, &image, &[]);
        let direct = no_proxy(&steering);
        for host in ["inventory", "postgres-main.temps.local"] {
            assert!(direct.contains(&host), "{host}: {direct:?}");
        }
        for recorded in ["dev", "api.example.dev", "api.example.com"] {
            assert!(!direct.contains(&recorded), "{recorded}: {direct:?}");
        }
    }

    /// A single-label host a database driver or plain HTTP uses
    /// (`DB_HOST=dev`) is never put in `NO_PROXY`, where clients would also
    /// read it as a suffix and send every `*.dev` HTTPS call around the
    /// recorder. One an HTTPS URL names is (the call would fail through
    /// the recorder), and its variable is reported by name, never value.
    #[test]
    fn single_label_hosts_are_kept_direct_only_for_https() {
        let existing = variables(&[
            ("DB_HOST", "dev"),
            ("CACHE_URL", "http://cache"),
            ("CACHE_HOSTNAME", "cache.temps.local"),
            ("S3_ENDPOINT", "http://10.20.0.7:9000"),
        ]);
        let image = variables(&[("INVENTORY_URL", "https://inventory")]);
        let steering = proxy_variables_with_image(proxy(), &existing, &image, &[]);
        let direct = no_proxy(&steering);
        assert!(!direct.contains(&"dev"), "{direct:?}");
        assert!(!direct.contains(&"cache"), "{direct:?}");
        assert!(direct.contains(&"inventory"), "{direct:?}");
        assert_eq!(steering.bare_name_variables, ["INVENTORY_URL"]);
        assert_eq!(
            https_hosts_in("https://inventory:8443/x, http://other"),
            ["inventory"]
        );
        assert_eq!(https_hosts_in("WSS://Feed/live"), ["feed"]);
        assert!(proxy_variables(proxy(), &HashMap::new(), &[])
            .bare_name_variables
            .is_empty());
    }

    /// A locator whose name mentions a credential word is still read: the
    /// internal auth service an `AUTH_URL` names stays direct, so turning
    /// recording on does not break it.
    #[test]
    fn a_locator_named_after_auth_or_tokens_is_read() {
        let existing = variables(&[
            ("AUTH_URL", "https://auth-service"),
            ("OAUTH_TOKEN_URL", "https://idp.internal/token"),
            ("AUTH_SERVER_HOST", "auth-primary.internal"),
        ]);
        let steering = proxy_variables(proxy(), &existing, &[]);
        let direct = no_proxy(&steering);
        for host in ["auth-service", "idp.internal", "auth-primary.internal"] {
            assert!(direct.contains(&host), "{host}: {direct:?}");
        }
        for credential in [
            "HOST_KEY",
            "DATABASE_PASSWORD",
            "HOSTING_API_TOKEN",
            "AUTH_SECRET",
        ] {
            assert!(is_credential(credential), "{credential}");
        }
        for locator in ["AUTH_URL", "TOKEN_ENDPOINT", "REDIS_HOST", "KEYCLOAK_URL"] {
            assert!(!is_credential(locator), "{locator}");
        }
    }

    /// A credential is never read as a URL, whatever its value looks like:
    /// a password `prefix://secret` puts nothing in `NO_PROXY` and names no
    /// variable for the deploy log.
    #[test]
    fn a_url_shaped_credential_is_never_read() {
        let existing = variables(&[("DATABASE_PASSWORD", "prefix://privatepassphrase")]);
        let steering = proxy_variables(proxy(), &existing, &[]);
        assert!(
            !steering.variables["NO_PROXY"].contains("privatepassphrase"),
            "{steering:?}"
        );
        assert!(steering.bare_name_variables.is_empty());
        assert!(image_host_variables(["API_TOKEN=x://privatepassphrase"]).is_empty());
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
