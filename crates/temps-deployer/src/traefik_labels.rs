// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Traefik-style Docker label parsing for live route discovery.
//!
//! This module is intentionally **pure**: it takes a container's label map
//! (plus the container's exposed ports) and returns the routers Temps should
//! serve. No Docker client, no database, no async — so the adversarial cases
//! (spoofed labels, malformed rules, ambiguous ports) are all covered by plain
//! unit tests. The only dependency is `tracing`, used to explain the drops an
//! operator is most likely to hit by misconfiguration.
//!
//! # v1 label-grammar boundary
//!
//! Temps deliberately implements a **strict subset** of Traefik's rule
//! language. The supported grammar is exactly:
//!
//! ```text
//! rule := hostmatch ( "||" hostmatch )*
//! hostmatch := "Host(" `domain` ( "," `domain` )* ")"
//! ```
//!
//! That is: `Host(`` `a.com` ``)`, `Host(`` `a.com` ``, `` `b.com` ``)` and
//! `Host(`` `a.com` ``) || Host(`` `b.com` ``)`. Domains must be
//! backtick-quoted, exactly as Traefik requires.
//!
//! Everything else — `Path`, `PathPrefix`, `Headers`, `HostRegexp`, `Method`,
//! `&&` conjunctions, negation, priorities, middlewares, weighted services,
//! sticky sessions — is **out of scope**. A rule that is not pure
//! `Host()`/`Host()||Host()` is skipped entirely rather than partially
//! honoured: guessing at a rule we don't understand would route traffic
//! somewhere the operator never asked for, which is strictly worse than not
//! routing it at all.
//!
//! # Ownership boundary
//!
//! Containers carrying `sh.temps.deploy_id` are already Temps-managed and
//! already get routes through the deployment path. They are skipped here
//! unconditionally — a container Temps already owns a route for must never be
//! reinterpretable through (potentially spoofed) Traefik labels, and must
//! never produce a second, conflicting backend entry for the same host.

use std::collections::{BTreeMap, HashSet};

/// Label set by Temps on every container it deploys. Its presence means the
/// container is Temps-managed and must be ignored by Traefik discovery.
pub const TEMPS_DEPLOY_ID_LABEL: &str = "sh.temps.deploy_id";

/// Traefik's opt-in label. Must be the literal string `true`.
pub const TRAEFIK_ENABLE_LABEL: &str = "traefik.enable";

const ROUTER_PREFIX: &str = "traefik.http.routers.";
const SERVICE_PREFIX: &str = "traefik.http.services.";
const HOST_MATCHER: &str = "Host(";

/// Maximum total length of a DNS name, per RFC 1035.
const MAX_HOST_LEN: usize = 253;
/// Maximum length of a single DNS label, per RFC 1035.
const MAX_LABEL_LEN: usize = 63;

/// A router extracted from Traefik labels, before port resolution.
///
/// One entry per `(router, host)` pair: a rule with two hosts produces two
/// `DiscoveredRouter`s sharing a `router_name`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DiscoveredRouter {
    /// Traefik router name (the `<name>` in `traefik.http.routers.<name>.*`).
    pub router_name: String,
    /// Normalized (lowercased, validated) hostname from the `Host()` matcher.
    pub host: String,
    /// Explicit backend port from
    /// `traefik.http.services.<svc>.loadbalancer.server.port`, when present.
    pub service_port: Option<u16>,
    /// Whether the router asked for TLS.
    pub tls: bool,
}

/// A router whose backend port has been resolved to a concrete value.
///
/// Produced by [`resolve_routers`]. Routers whose port cannot be determined
/// unambiguously never reach this stage.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ResolvedRouter {
    /// Traefik router name.
    pub router_name: String,
    /// Normalized hostname.
    pub host: String,
    /// Concrete backend port on the container.
    pub port: u16,
    /// Whether the router asked for TLS.
    pub tls: bool,
}

/// Whether this container was deployed by Temps itself.
///
/// Temps-managed containers already have routes from the deployment path;
/// re-deriving a route from their labels would let a workload override its own
/// routing by setting Traefik labels on itself.
pub fn is_temps_managed(labels: &std::collections::HashMap<String, String>) -> bool {
    labels
        .get(TEMPS_DEPLOY_ID_LABEL)
        .is_some_and(|v| !v.trim().is_empty())
}

/// Whether the container opted in via `traefik.enable=true`.
///
/// Deliberately stricter than Traefik's own `strconv.ParseBool` (which also
/// accepts `1`, `t`, `T`, `TRUE`, `True`): opting a container into Temps'
/// route table is a routing-security decision, so only the canonical literal
/// `true` counts. Anything else — including `True` — is treated as "not
/// enabled" rather than guessed at.
pub fn traefik_enabled(labels: &std::collections::HashMap<String, String>) -> bool {
    labels.get(TRAEFIK_ENABLE_LABEL).map(|v| v.trim()) == Some("true")
}

/// Parse every supported router out of a container's labels.
///
/// Returns an empty vector when the container is Temps-managed, is not
/// `traefik.enable=true`, or carries no parseable `Host()` rule. Results are
/// ordered deterministically by `(router_name, host)`.
pub fn parse_routers(labels: &std::collections::HashMap<String, String>) -> Vec<DiscoveredRouter> {
    if is_temps_managed(labels) || !traefik_enabled(labels) {
        return Vec::new();
    }

    // Traefik lowercases the structural part of label keys, and both
    // `loadbalancer` and `loadBalancer` appear in real-world compose files.
    // Match on a lowercased key index; values are kept verbatim because host
    // normalization is handled separately and deliberately.
    let index: BTreeMap<String, &str> = labels
        .iter()
        .map(|(k, v)| (k.trim().to_ascii_lowercase(), v.as_str()))
        .collect();

    let mut router_names: Vec<&str> = index
        .keys()
        .filter_map(|key| router_name_of(key))
        .collect::<HashSet<_>>()
        .into_iter()
        .collect();
    router_names.sort_unstable();

    let mut out = Vec::new();
    for name in router_names {
        let Some(rule) = index.get(&format!("{ROUTER_PREFIX}{name}.rule")) else {
            // A router with no rule matches nothing in Traefik either.
            continue;
        };
        let Some(hosts) = parse_host_rule(rule) else {
            // Unsupported or malformed rule — skip, never guess.
            continue;
        };

        let tls = tls_enabled(&index, name);

        // `<router>.service` selects the backend service; Traefik defaults the
        // service name to the router name when the label is absent.
        let service = index
            .get(&format!("{ROUTER_PREFIX}{name}.service"))
            .map(|s| s.trim().to_ascii_lowercase())
            .filter(|s| !s.is_empty())
            .unwrap_or_else(|| name.to_string());

        let service_port = index
            .get(&format!(
                "{SERVICE_PREFIX}{service}.loadbalancer.server.port"
            ))
            .and_then(|raw| raw.trim().parse::<u16>().ok())
            .filter(|port| *port != 0);

        for host in hosts {
            out.push(DiscoveredRouter {
                router_name: name.to_string(),
                host,
                service_port,
                tls,
            });
        }
    }

    out.sort_by(|a, b| {
        a.router_name
            .cmp(&b.router_name)
            .then_with(|| a.host.cmp(&b.host))
    });
    out
}

/// Parse routers and resolve each one's backend port.
///
/// Port resolution order:
/// 1. the explicit `loadbalancer.server.port` label, when present — **only if
///    the container actually exposes that port**;
/// 2. otherwise the container's single exposed port.
///
/// A router with no port label and zero or 2+ exposed ports is **ambiguous and
/// dropped** — picking one at random would silently misroute production
/// traffic to (say) a database port.
///
/// # Why the label is validated against `exposed_ports`
///
/// The port label is container-controlled data, and the resolved port ends up
/// in a backend address. On a baremetal install that address is built as
/// `127.0.0.1:<port>`, so an unvalidated label lets any container on the
/// watched network point a hostname it owns at an *arbitrary loopback port on
/// the host* — the Docker daemon's own API, a database bound to localhost, the
/// Temps console. Requiring the labelled port to be one Docker reports for the
/// container keeps the blast radius inside the container's own port surface.
///
/// A labelled port the container does not expose is **dropped**, matching the
/// module's "skip, never guess" policy for rules it cannot honour. Note that
/// exposing a port and *publishing* it are different things: a port that is in
/// `exposed_ports` without a host publication still resolves here (it is a
/// legitimate Docker-network-only backend), and the caller decides separately
/// whether its deployment mode can reach it.
///
/// When two routers claim the same host, the first in `(router_name, host)`
/// order wins and the rest are dropped; a single host can only have one
/// backend in the route table.
pub fn resolve_routers(
    labels: &std::collections::HashMap<String, String>,
    exposed_ports: &[u16],
) -> Vec<ResolvedRouter> {
    let single_exposed = match exposed_ports {
        [only] if *only != 0 => Some(*only),
        _ => None,
    };

    let mut seen: HashSet<String> = HashSet::new();
    let mut out = Vec::new();
    for router in parse_routers(labels) {
        let port = match router.service_port {
            Some(labelled) => {
                if !exposed_ports.contains(&labelled) {
                    // Logged rather than silently dropped: an operator whose
                    // port label is simply wrong (or whose image has no
                    // matching EXPOSE) would otherwise see a labelled
                    // container that is never routed, with nothing to go on.
                    tracing::warn!(
                        router = %router.router_name,
                        host = %router.host,
                        labelled_port = labelled,
                        exposed_ports = ?exposed_ports,
                        "Ignoring Traefik router: its loadbalancer.server.port names a port the \
                         container does not expose. Expose/publish the port on the container, or \
                         correct the label — Temps will not route a host to a port the container \
                         never advertised."
                    );
                    continue;
                }
                labelled
            }
            None => match single_exposed {
                Some(port) => port,
                None => continue,
            },
        };
        if !seen.insert(router.host.clone()) {
            continue;
        }
        out.push(ResolvedRouter {
            router_name: router.router_name,
            host: router.host,
            port,
            tls: router.tls,
        });
    }
    out
}

/// Extract the `<name>` from a `traefik.http.routers.<name>.<something>` key.
///
/// Returns `None` for any key that isn't exactly that shape — including near
/// misses like `traefik.http.router.<name>.rule` (singular) or
/// `traefik.https.routers.<name>.rule`. Traefik router names cannot contain a
/// dot, so `<name>` is the single segment following the prefix and there must
/// be at least one further segment after it.
fn router_name_of(key: &str) -> Option<&str> {
    let rest = key.strip_prefix(ROUTER_PREFIX)?;
    let (name, tail) = rest.split_once('.')?;
    if name.is_empty() || tail.is_empty() {
        return None;
    }
    Some(name)
}

/// Resolve the TLS flag for a router.
///
/// `<router>.tls=false` is authoritative and wins over any `tls.*` sub-key.
/// Otherwise TLS is on when `<router>.tls=true` or when any `<router>.tls.*`
/// sub-key exists (e.g. `tls.certresolver`), matching Traefik's behaviour
/// where declaring a cert resolver implies TLS.
fn tls_enabled(index: &BTreeMap<String, &str>, router: &str) -> bool {
    let tls_key = format!("{ROUTER_PREFIX}{router}.tls");
    if let Some(raw) = index.get(&tls_key) {
        let v = raw.trim();
        if v.eq_ignore_ascii_case("false") {
            return false;
        }
        if v.eq_ignore_ascii_case("true") {
            return true;
        }
    }
    let sub_prefix = format!("{tls_key}.");
    index.keys().any(|k| k.starts_with(&sub_prefix))
}

/// Parse a Traefik rule into the list of hosts it matches.
///
/// Returns `None` when the rule uses anything outside the supported
/// `Host()`/`Host()||Host()` grammar (see the module docs). `None` means
/// "skip this router", never "match everything".
fn parse_host_rule(rule: &str) -> Option<Vec<String>> {
    let rule = rule.trim();
    if rule.is_empty() {
        return None;
    }

    let mut hosts = Vec::new();
    for part in rule.split("||") {
        let part = part.trim();
        // `Host(` … `)` — the matcher name is case-sensitive in Traefik v3,
        // so we require the exact spelling rather than guessing.
        let inner = part
            .strip_prefix(HOST_MATCHER)
            .and_then(|rest| rest.strip_suffix(')'))?;
        if inner.trim().is_empty() {
            return None;
        }
        for raw in inner.split(',') {
            let quoted = raw.trim();
            let unquoted = quoted
                .strip_prefix('`')
                .and_then(|rest| rest.strip_suffix('`'))?;
            hosts.push(normalize_host(unquoted)?);
        }
    }

    if hosts.is_empty() {
        return None;
    }
    hosts.sort();
    hosts.dedup();
    Some(hosts)
}

/// Validate and normalize a hostname from a `Host()` matcher.
///
/// Lowercases (HTTP `Host` matching is case-insensitive) and enforces a
/// conservative LDH charset. Non-ASCII (IDN) names are rejected: DNS and the
/// HTTP `Host` header carry punycode, so an operator must write the
/// `xn--` form in the label — accepting the unicode form would produce a route
/// key that no request could ever match.
fn normalize_host(raw: &str) -> Option<String> {
    let host = raw.trim().trim_end_matches('.').to_ascii_lowercase();
    if host.is_empty() || host.len() > MAX_HOST_LEN {
        return None;
    }
    // Wildcards belong to `HostRegexp`, which we deliberately don't support —
    // a `*` here would otherwise become a literal, never-matching route.
    if host.contains('*') {
        return None;
    }
    for label in host.split('.') {
        if label.is_empty() || label.len() > MAX_LABEL_LEN {
            return None;
        }
        if label.starts_with('-') || label.ends_with('-') {
            return None;
        }
        if !label.chars().all(|c| c.is_ascii_alphanumeric() || c == '-') {
            return None;
        }
    }
    Some(host)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    fn labels(pairs: &[(&str, &str)]) -> HashMap<String, String> {
        pairs
            .iter()
            .map(|(k, v)| ((*k).to_string(), (*v).to_string()))
            .collect()
    }

    fn basic() -> HashMap<String, String> {
        labels(&[
            ("traefik.enable", "true"),
            ("traefik.http.routers.web.rule", "Host(`example.com`)"),
            ("traefik.http.services.web.loadbalancer.server.port", "3000"),
        ])
    }

    // ── enable gate ──────────────────────────────────────────────────

    #[test]
    fn parses_a_minimal_enabled_container() {
        let routers = parse_routers(&basic());
        assert_eq!(
            routers,
            vec![DiscoveredRouter {
                router_name: "web".into(),
                host: "example.com".into(),
                service_port: Some(3000),
                tls: false,
            }]
        );
    }

    #[test]
    fn missing_enable_label_yields_nothing() {
        let mut l = basic();
        l.remove("traefik.enable");
        assert!(parse_routers(&l).is_empty());
    }

    #[test]
    fn enable_false_yields_nothing() {
        let mut l = basic();
        l.insert("traefik.enable".into(), "false".into());
        assert!(parse_routers(&l).is_empty());
    }

    #[test]
    fn enable_must_be_the_literal_true() {
        for value in ["True", "TRUE", "1", "t", "yes", "on", " "] {
            let mut l = basic();
            l.insert("traefik.enable".into(), value.into());
            assert!(
                parse_routers(&l).is_empty(),
                "value {value:?} must not enable discovery"
            );
        }
    }

    #[test]
    fn enable_tolerates_surrounding_whitespace() {
        let mut l = basic();
        l.insert("traefik.enable".into(), "  true  ".into());
        assert_eq!(parse_routers(&l).len(), 1);
    }

    // ── temps ownership ──────────────────────────────────────────────

    #[test]
    fn temps_managed_container_is_skipped_even_with_valid_traefik_labels() {
        let mut l = basic();
        l.insert("sh.temps.deploy_id".into(), "42".into());
        assert!(is_temps_managed(&l));
        assert!(
            parse_routers(&l).is_empty(),
            "a Temps-deployed container must never be re-routed via Traefik labels"
        );
    }

    #[test]
    fn blank_temps_deploy_id_is_not_treated_as_owned() {
        let mut l = basic();
        l.insert("sh.temps.deploy_id".into(), "   ".into());
        assert!(!is_temps_managed(&l));
        assert_eq!(parse_routers(&l).len(), 1);
    }

    #[test]
    fn other_temps_labels_do_not_claim_ownership() {
        let mut l = basic();
        l.insert("sh.temps.project_id".into(), "7".into());
        l.insert("sh.temps.environment".into(), "prod".into());
        l.insert("sh.temps.service".into(), "api".into());
        assert!(!is_temps_managed(&l));
        assert_eq!(parse_routers(&l).len(), 1);
    }

    // ── rule grammar ─────────────────────────────────────────────────

    #[test]
    fn parses_or_of_hosts() {
        let l = labels(&[
            ("traefik.enable", "true"),
            (
                "traefik.http.routers.web.rule",
                "Host(`a.com`) || Host(`b.com`)",
            ),
            ("traefik.http.services.web.loadbalancer.server.port", "8080"),
        ]);
        let hosts: Vec<String> = parse_routers(&l).into_iter().map(|r| r.host).collect();
        assert_eq!(hosts, vec!["a.com".to_string(), "b.com".to_string()]);
    }

    #[test]
    fn parses_comma_separated_hosts_inside_one_matcher() {
        let l = labels(&[
            ("traefik.enable", "true"),
            ("traefik.http.routers.web.rule", "Host(`a.com`, `b.com`)"),
            ("traefik.http.services.web.loadbalancer.server.port", "80"),
        ]);
        let hosts: Vec<String> = parse_routers(&l).into_iter().map(|r| r.host).collect();
        assert_eq!(hosts, vec!["a.com".to_string(), "b.com".to_string()]);
    }

    #[test]
    fn duplicate_hosts_in_one_rule_are_deduped() {
        let l = labels(&[
            ("traefik.enable", "true"),
            (
                "traefik.http.routers.web.rule",
                "Host(`a.com`) || Host(`A.com`)",
            ),
            ("traefik.http.services.web.loadbalancer.server.port", "80"),
        ]);
        assert_eq!(parse_routers(&l).len(), 1);
    }

    #[test]
    fn unsupported_matchers_are_skipped_not_guessed() {
        for rule in [
            "PathPrefix(`/api`)",
            "Path(`/health`)",
            "Headers(`X-Env`, `prod`)",
            "HostRegexp(`{sub:[a-z]+}.example.com`)",
            "Method(`GET`)",
            "ClientIP(`10.0.0.0/8`)",
            "Host(`a.com`) && PathPrefix(`/api`)",
            "Host(`a.com`) || PathPrefix(`/api`)",
            "!Host(`a.com`)",
            "(Host(`a.com`) || Host(`b.com`))",
        ] {
            let mut l = basic();
            l.insert("traefik.http.routers.web.rule".into(), rule.into());
            assert!(
                parse_routers(&l).is_empty(),
                "rule {rule:?} must be skipped, not partially honoured"
            );
        }
    }

    #[test]
    fn malformed_rules_are_skipped_without_panicking() {
        for rule in [
            "",
            "   ",
            "Host(",
            "Host()",
            "Host(``)",
            "Host(`a.com`",
            "Host(a.com)",
            "Host('a.com')",
            "Host(\"a.com\")",
            "host(`a.com`)",
            "HOST(`a.com`)",
            "Host(`a.com`) ||",
            "|| Host(`a.com`)",
            "Host(`a.com`) || Host(``)",
            "Host(`a.com`,)",
        ] {
            let mut l = basic();
            l.insert("traefik.http.routers.web.rule".into(), rule.into());
            assert!(
                parse_routers(&l).is_empty(),
                "rule {rule:?} must be skipped"
            );
        }
    }

    #[test]
    fn router_without_a_rule_is_skipped() {
        let l = labels(&[
            ("traefik.enable", "true"),
            ("traefik.http.routers.web.tls", "true"),
            ("traefik.http.services.web.loadbalancer.server.port", "3000"),
        ]);
        assert!(parse_routers(&l).is_empty());
    }

    // ── host normalization ───────────────────────────────────────────

    #[test]
    fn hosts_are_lowercased_and_trailing_dot_stripped() {
        let mut l = basic();
        l.insert(
            "traefik.http.routers.web.rule".into(),
            "Host(`  EXAMPLE.COM.  `)".into(),
        );
        assert_eq!(parse_routers(&l)[0].host, "example.com");
    }

    #[test]
    fn empty_and_whitespace_domains_are_rejected() {
        for host in ["``", "` `", "`   `", "`.`", "`..`"] {
            let mut l = basic();
            l.insert(
                "traefik.http.routers.web.rule".into(),
                format!("Host({host})"),
            );
            assert!(
                parse_routers(&l).is_empty(),
                "domain {host:?} must be rejected"
            );
        }
    }

    #[test]
    fn non_ascii_idn_hosts_are_rejected_punycode_is_required() {
        let mut l = basic();
        l.insert(
            "traefik.http.routers.web.rule".into(),
            "Host(`café.example.com`)".into(),
        );
        assert!(parse_routers(&l).is_empty());

        let mut punycode = basic();
        punycode.insert(
            "traefik.http.routers.web.rule".into(),
            "Host(`xn--calf-dma.example.com`)".into(),
        );
        assert_eq!(parse_routers(&punycode)[0].host, "xn--calf-dma.example.com");
    }

    #[test]
    fn wildcard_hosts_are_rejected() {
        let mut l = basic();
        l.insert(
            "traefik.http.routers.web.rule".into(),
            "Host(`*.example.com`)".into(),
        );
        assert!(parse_routers(&l).is_empty());
    }

    #[test]
    fn structurally_invalid_hosts_are_rejected() {
        for host in [
            "exa mple.com",
            "example..com",
            "-example.com",
            "example-.com",
            "exa_mple.com",
            "example.com/path",
            "http://example.com",
            "example.com:8080",
        ] {
            let mut l = basic();
            l.insert(
                "traefik.http.routers.web.rule".into(),
                format!("Host(`{host}`)"),
            );
            assert!(
                parse_routers(&l).is_empty(),
                "host {host:?} must be rejected"
            );
        }
    }

    #[test]
    fn overlong_hosts_and_labels_are_rejected() {
        let long_label = "a".repeat(64);
        let mut l = basic();
        l.insert(
            "traefik.http.routers.web.rule".into(),
            format!("Host(`{long_label}.com`)"),
        );
        assert!(parse_routers(&l).is_empty());

        let long_host = std::iter::repeat_n("abcdefgh", 40)
            .collect::<Vec<_>>()
            .join(".");
        let mut l2 = basic();
        l2.insert(
            "traefik.http.routers.web.rule".into(),
            format!("Host(`{long_host}`)"),
        );
        assert!(parse_routers(&l2).is_empty());
    }

    // ── service / port resolution ────────────────────────────────────

    #[test]
    fn service_defaults_to_the_router_name() {
        let l = labels(&[
            ("traefik.enable", "true"),
            ("traefik.http.routers.api.rule", "Host(`api.example.com`)"),
            ("traefik.http.services.api.loadbalancer.server.port", "9000"),
        ]);
        assert_eq!(parse_routers(&l)[0].service_port, Some(9000));
    }

    #[test]
    fn explicit_service_label_redirects_the_port_lookup() {
        let l = labels(&[
            ("traefik.enable", "true"),
            ("traefik.http.routers.api.rule", "Host(`api.example.com`)"),
            ("traefik.http.routers.api.service", "backend"),
            ("traefik.http.services.api.loadbalancer.server.port", "1111"),
            (
                "traefik.http.services.backend.loadbalancer.server.port",
                "2222",
            ),
        ]);
        assert_eq!(parse_routers(&l)[0].service_port, Some(2222));
    }

    #[test]
    fn camel_case_loadbalancer_key_is_accepted() {
        let l = labels(&[
            ("traefik.enable", "true"),
            ("traefik.http.routers.web.rule", "Host(`example.com`)"),
            ("traefik.http.services.web.loadBalancer.server.port", "4321"),
        ]);
        assert_eq!(parse_routers(&l)[0].service_port, Some(4321));
    }

    #[test]
    fn invalid_or_zero_port_labels_are_ignored() {
        for value in ["0", "-1", "70000", "abc", "", "80 80", "8080/tcp"] {
            let mut l = basic();
            l.insert(
                "traefik.http.services.web.loadbalancer.server.port".into(),
                value.into(),
            );
            assert_eq!(
                parse_routers(&l)[0].service_port,
                None,
                "port {value:?} must not parse"
            );
        }
    }

    #[test]
    fn resolves_explicit_port_over_exposed_ports() {
        let resolved = resolve_routers(&basic(), &[3000, 9090]);
        assert_eq!(resolved.len(), 1);
        assert_eq!(resolved[0].port, 3000);
    }

    #[test]
    fn labelled_port_the_container_does_not_expose_is_dropped() {
        // The container exposes 8080/9090 but claims 3000. Honouring that on a
        // baremetal install would build `127.0.0.1:3000` — an arbitrary port on
        // the Temps host, not the container. Never guess: drop the router.
        assert!(
            resolve_routers(&basic(), &[8080, 9090]).is_empty(),
            "a port label must not select a port the container never exposed"
        );
    }

    #[test]
    fn labelled_port_is_dropped_when_the_container_exposes_nothing() {
        assert!(
            resolve_routers(&basic(), &[]).is_empty(),
            "with no reported ports there is nothing to validate the label against"
        );
    }

    #[test]
    fn labelled_port_resolves_when_exposed_but_not_published() {
        // Docker-network-only backend: exposed, no host publication. Legitimate
        // in Docker mode (container_name:3000 resolves over the internal DNS),
        // so the label must still be honoured here. Whether the *deployment
        // mode* can reach it is decided later, at the route-table merge.
        let resolved = resolve_routers(&basic(), &[3000]);
        assert_eq!(resolved.len(), 1);
        assert_eq!(resolved[0].port, 3000);
    }

    #[test]
    fn labelled_port_resolves_when_it_matches_a_published_port() {
        // `ContainerView` folds host-published ports into `exposed_ports`, so a
        // published-only port (`-p 18080:3000`, no EXPOSE in the image) still
        // reaches `resolve_routers` as an exposed port and must be honoured.
        let mut l = basic();
        l.insert(
            "traefik.http.services.web.loadbalancer.server.port".into(),
            "8080".into(),
        );
        let resolved = resolve_routers(&l, &[3000, 8080]);
        assert_eq!(resolved.len(), 1);
        assert_eq!(resolved[0].port, 8080);
    }

    #[test]
    fn falls_back_to_the_single_exposed_port() {
        let mut l = basic();
        l.remove("traefik.http.services.web.loadbalancer.server.port");
        let resolved = resolve_routers(&l, &[8080]);
        assert_eq!(resolved.len(), 1);
        assert_eq!(resolved[0].port, 8080);
    }

    #[test]
    fn zero_exposed_ports_and_no_port_label_is_ambiguous_and_dropped() {
        let mut l = basic();
        l.remove("traefik.http.services.web.loadbalancer.server.port");
        assert!(resolve_routers(&l, &[]).is_empty());
    }

    #[test]
    fn multiple_exposed_ports_and_no_port_label_is_ambiguous_and_dropped() {
        let mut l = basic();
        l.remove("traefik.http.services.web.loadbalancer.server.port");
        assert!(
            resolve_routers(&l, &[8080, 5432]).is_empty(),
            "never guess between an app port and a database port"
        );
    }

    #[test]
    fn duplicate_hosts_across_routers_keep_only_the_first() {
        let l = labels(&[
            ("traefik.enable", "true"),
            ("traefik.http.routers.aaa.rule", "Host(`dup.example.com`)"),
            ("traefik.http.services.aaa.loadbalancer.server.port", "1000"),
            ("traefik.http.routers.zzz.rule", "Host(`dup.example.com`)"),
            ("traefik.http.services.zzz.loadbalancer.server.port", "2000"),
        ]);
        let resolved = resolve_routers(&l, &[1000, 2000]);
        assert_eq!(resolved.len(), 1);
        assert_eq!(resolved[0].router_name, "aaa");
        assert_eq!(resolved[0].port, 1000);
    }

    // ── TLS flag ─────────────────────────────────────────────────────

    #[test]
    fn tls_defaults_to_false() {
        assert!(!parse_routers(&basic())[0].tls);
    }

    #[test]
    fn tls_true_is_honoured() {
        let mut l = basic();
        l.insert("traefik.http.routers.web.tls".into(), "true".into());
        assert!(parse_routers(&l)[0].tls);
    }

    #[test]
    fn tls_certresolver_subkey_implies_tls() {
        let mut l = basic();
        l.insert(
            "traefik.http.routers.web.tls.certresolver".into(),
            "letsencrypt".into(),
        );
        assert!(parse_routers(&l)[0].tls);
    }

    #[test]
    fn explicit_tls_false_wins_over_subkeys() {
        let mut l = basic();
        l.insert("traefik.http.routers.web.tls".into(), "false".into());
        l.insert(
            "traefik.http.routers.web.tls.certresolver".into(),
            "letsencrypt".into(),
        );
        assert!(!parse_routers(&l)[0].tls);
    }

    #[test]
    fn unparsable_tls_value_without_subkeys_is_false() {
        let mut l = basic();
        l.insert("traefik.http.routers.web.tls".into(), "maybe".into());
        assert!(!parse_routers(&l)[0].tls);
    }

    #[test]
    fn tls_flag_is_per_router() {
        let l = labels(&[
            ("traefik.enable", "true"),
            ("traefik.http.routers.secure.rule", "Host(`s.example.com`)"),
            ("traefik.http.routers.secure.tls", "true"),
            (
                "traefik.http.services.secure.loadbalancer.server.port",
                "443",
            ),
            ("traefik.http.routers.plain.rule", "Host(`p.example.com`)"),
            ("traefik.http.services.plain.loadbalancer.server.port", "80"),
        ]);
        let routers = parse_routers(&l);
        assert_eq!(routers.len(), 2);
        assert_eq!(routers[0].router_name, "plain");
        assert!(!routers[0].tls);
        assert_eq!(routers[1].router_name, "secure");
        assert!(routers[1].tls);
    }

    // ── prefix strictness ────────────────────────────────────────────

    #[test]
    fn typo_prefixes_do_not_match() {
        for key in [
            "traefik.http.router.web.rule",
            "traefik.https.routers.web.rule",
            "traefik.htp.routers.web.rule",
            "traefk.http.routers.web.rule",
            "traefik.tcp.routers.web.rule",
            "traefik.udp.routers.web.rule",
            "traefik.http.routers.rule",
            "traefik.http.routers..rule",
            "traefik.http.routers.web.",
            "xtraefik.http.routers.web.rule",
        ] {
            let l = labels(&[("traefik.enable", "true"), (key, "Host(`example.com`)")]);
            assert!(
                parse_routers(&l).is_empty(),
                "key {key:?} must not register a router"
            );
        }
    }

    #[test]
    fn uppercase_structural_keys_are_matched_case_insensitively() {
        let l = labels(&[
            ("traefik.enable", "true"),
            ("Traefik.HTTP.Routers.Web.Rule", "Host(`example.com`)"),
            ("Traefik.HTTP.Services.Web.LoadBalancer.Server.Port", "3000"),
        ]);
        let routers = parse_routers(&l);
        assert_eq!(routers.len(), 1);
        assert_eq!(routers[0].service_port, Some(3000));
    }

    #[test]
    fn tcp_router_labels_are_out_of_scope() {
        let l = labels(&[
            ("traefik.enable", "true"),
            ("traefik.tcp.routers.db.rule", "HostSNI(`db.example.com`)"),
            ("traefik.tcp.services.db.loadbalancer.server.port", "5432"),
        ]);
        assert!(parse_routers(&l).is_empty());
    }

    #[test]
    fn empty_label_map_is_handled() {
        assert!(parse_routers(&HashMap::new()).is_empty());
        assert!(resolve_routers(&HashMap::new(), &[80]).is_empty());
    }

    #[test]
    fn multiple_routers_are_all_returned_in_deterministic_order() {
        let l = labels(&[
            ("traefik.enable", "true"),
            ("traefik.http.routers.zzz.rule", "Host(`z.example.com`)"),
            ("traefik.http.services.zzz.loadbalancer.server.port", "3000"),
            ("traefik.http.routers.aaa.rule", "Host(`a.example.com`)"),
            ("traefik.http.services.aaa.loadbalancer.server.port", "4000"),
        ]);
        let routers = parse_routers(&l);
        assert_eq!(
            routers
                .iter()
                .map(|r| r.router_name.as_str())
                .collect::<Vec<_>>(),
            vec!["aaa", "zzz"]
        );
    }
}
