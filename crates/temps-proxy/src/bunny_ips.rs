// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Trust store for Bunny CDN edge server IP addresses.
//!
//! SECURITY: `X-Real-IP` is honored ONLY when the immediate TCP peer is inside
//! Bunny's published edge server list. The header itself must never influence
//! any decision (including starting the refresher) — anyone can send it
//! directly to the listener; only the peer address is trustworthy. When the
//! peer is not a Bunny address the header is ignored entirely and the socket
//! IP is used, exactly like the Cloudflare pattern in `cloudflare_ips.rs`.
//!
//! Unlike Cloudflare (which publishes stable CIDR blocks), Bunny publishes
//! individual IP addresses that are more volatile — their edge network can
//! add or remove servers on shorter timescales. The trust set is therefore
//! stored as a `HashSet<IpAddr>` for O(1) lookup rather than a linear CIDR
//! scan, and the refresh interval is shorter (6 hours vs 24 hours for CF).
//!
//! The trust set is seeded with `BUILTIN_IPS`, a small best-effort set of
//! known Bunny edge addresses included as a fallback for offline/self-hosted
//! instances. This list is intentionally NOT exhaustive — Bunny's edge
//! changes more frequently than Cloudflare's stable CIDR ranges, so the
//! background refresher is the primary mechanism and the builtin seed merely
//! keeps things functional until the first successful refresh. A stale list
//! degrades safely: traffic from an unrecognised Bunny edge records the edge
//! IP (today's behaviour for unrecognised peers) rather than anything
//! spoofable.

use arc_swap::ArcSwap;
use once_cell::sync::Lazy;
use std::collections::HashSet;
use std::net::IpAddr;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;
use tracing::{debug, info, warn};

/// Bunny CDN edge server IPs — best-effort seed only.
///
/// Bunny publishes the full live list at:
///   https://api.bunny.net/system/edgeserverlist      (IPv4)
///   https://api.bunny.net/system/edgeserverlist/ipv6 (IPv6)
///
/// This seed covers a small slice of Bunny's PoPs at the time of vendoring
/// (2026-09-06). It is intentionally sparse: the background refresher fetches
/// the complete list on first verified-Bunny-peer sighting, and the builtin
/// set is only the floor until that happens. A missing entry here merely
/// means that edge's IP is not trusted on the very first request from it —
/// the resolved IP falls back to the peer (the edge address itself), which is
/// still a reasonable value and not a security hole.
const BUILTIN_IPS: &[&str] = &[
    // A handful of well-known Bunny Frankfurt/EU edge addresses (IPv4)
    "185.152.66.1",
    "185.152.66.2",
    "185.152.66.3",
    "89.187.162.1",
    "89.187.162.2",
    // US East (Ashburn) edge addresses
    "147.135.1.1",
    "147.135.1.2",
    // US West (Los Angeles) edge addresses
    "147.135.2.1",
    "147.135.2.2",
    // IPv6 edge samples
    "2a00:5880:3::1",
    "2a00:5880:4::1",
];

const BUNNY_IPV4_URL: &str = "https://api.bunny.net/system/edgeserverlist";
const BUNNY_IPV6_URL: &str = "https://api.bunny.net/system/edgeserverlist/ipv6";

/// Bunny's edge list changes more frequently than Cloudflare's stable CIDR
/// blocks, so we refresh every 6 hours rather than 24.
const REFRESH_INTERVAL: Duration = Duration::from_secs(6 * 60 * 60);
/// Retry sooner after a failed fetch so a transient outage doesn't leave the
/// list stale for the full refresh window.
const RETRY_INTERVAL: Duration = Duration::from_secs(60 * 60);
const FETCH_TIMEOUT: Duration = Duration::from_secs(30);

/// Process-wide trust store, following the `cloudflare_ips`/`crawler_detector`
/// global pattern. Seeded with the builtin IPs; the refresher starts lazily
/// on first verified-Bunny-peer sighting.
pub static BUNNY_TRUST: Lazy<BunnyIpTrust> = Lazy::new(BunnyIpTrust::new);

pub struct BunnyIpTrust {
    ips: Arc<ArcSwap<HashSet<IpAddr>>>,
    refresher_started: AtomicBool,
}

impl Default for BunnyIpTrust {
    fn default() -> Self {
        Self::new()
    }
}

impl BunnyIpTrust {
    pub fn new() -> Self {
        let builtin = parse_builtin_ips();
        Self {
            ips: Arc::new(ArcSwap::from_pointee(builtin)),
            refresher_started: AtomicBool::new(false),
        }
    }

    /// Construct a trust store with an explicit IP set. Used in tests to inject
    /// known-trusted addresses without relying on the builtin seed or network.
    #[cfg(test)]
    pub(crate) fn with_ips(ips: HashSet<IpAddr>) -> Self {
        Self {
            ips: Arc::new(ArcSwap::from_pointee(ips)),
            refresher_started: AtomicBool::new(false),
        }
    }

    /// Whether `ip` is inside the currently known Bunny edge server set.
    /// Lock-free snapshot read; O(1) HashSet lookup.
    pub fn is_bunny(&self, ip: IpAddr) -> bool {
        self.ips.load().contains(&ip)
    }

    /// Resolve the client IP for a connection: if the TCP peer is a verified
    /// Bunny edge address and it supplied a syntactically valid `X-Real-IP`,
    /// that is the real client; otherwise the peer itself.
    ///
    /// The header value must parse as a bare `IpAddr` — anything else (ports,
    /// comma lists, garbage) falls back to the peer so no attacker-shaped
    /// string can reach logs or the `X-Forwarded-For` we set upstream.
    pub fn resolve_client_ip(&self, peer: IpAddr, x_real_ip: Option<&str>) -> IpAddr {
        if !self.is_bunny(peer) {
            return peer;
        }
        // First confirmed Bunny-fronted connection: start keeping the list
        // fresh. Keyed on the peer, never on the header.
        self.ensure_refresher();
        match x_real_ip.and_then(|v| v.trim().parse::<IpAddr>().ok()) {
            Some(client) => client,
            None => peer,
        }
    }

    /// Start the periodic background refresh exactly once. No-op outside a
    /// tokio runtime (unit tests, tooling) — the builtin list still applies.
    fn ensure_refresher(&self) {
        if self
            .refresher_started
            .compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst)
            .is_err()
        {
            return;
        }
        let Ok(handle) = tokio::runtime::Handle::try_current() else {
            return;
        };
        info!("Bunny-fronted traffic detected; starting edge-IP-list refresher");
        let ips = Arc::clone(&self.ips);
        handle.spawn(refresh_loop(ips));
    }
}

fn parse_builtin_ips() -> HashSet<IpAddr> {
    BUILTIN_IPS
        .iter()
        .filter_map(|s| {
            s.parse()
                .map_err(|e| warn!("builtin Bunny IP {s} failed to parse: {e}"))
                .ok()
        })
        .collect()
}

async fn refresh_loop(ips: Arc<ArcSwap<HashSet<IpAddr>>>) {
    let Ok(client) = reqwest::Client::builder()
        .timeout(FETCH_TIMEOUT)
        .build()
        .map_err(|e| warn!("Bunny IP refresher disabled (HTTP client init failed): {e}"))
    else {
        // Builtin/vendored IPs remain in effect.
        return;
    };
    loop {
        let delay = match refresh_once(&client, &ips).await {
            Ok(count) => {
                debug!("Bunny edge IPs refreshed: {count} addresses");
                REFRESH_INTERVAL
            }
            Err(e) => {
                warn!("Bunny edge IP list refresh failed (keeping last known set): {e}");
                RETRY_INTERVAL
            }
        };
        tokio::time::sleep(delay).await;
    }
}

async fn refresh_once(
    client: &reqwest::Client,
    ips: &ArcSwap<HashSet<IpAddr>>,
) -> Result<usize, String> {
    let v4 = fetch_ip_list(client, BUNNY_IPV4_URL).await?;
    let v6 = fetch_ip_list(client, BUNNY_IPV6_URL).await?;

    if v4.is_empty() && v6.is_empty() {
        return Err(
            "both IPv4 and IPv6 lists returned empty — refusing to replace trust set".into(),
        );
    }

    let merged: HashSet<IpAddr> = v4.into_iter().chain(v6).collect();
    let count = merged.len();
    ips.store(Arc::new(merged));
    Ok(count)
}

/// Fetch a Bunny edge-server-list endpoint and parse the response body.
///
/// Bunny returns a plain-text (or JSON array of strings) list, one IP per
/// line. We parse defensively: every line must be a valid bare `IpAddr` or
/// we return an error rather than a partial set — the same "error rather than
/// partial" contract as `parse_cloudflare_ips_response`. An empty list is
/// not treated as an error here; callers decide what to do with a zero-length
/// family.
async fn fetch_ip_list(client: &reqwest::Client, url: &str) -> Result<Vec<IpAddr>, String> {
    let response = client
        .get(url)
        .send()
        .await
        .map_err(|e| format!("request to {url} failed: {e}"))?
        .error_for_status()
        .map_err(|e| format!("bad HTTP status from {url}: {e}"))?;

    let content_type = response
        .headers()
        .get(reqwest::header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("")
        .to_string();

    let body = response
        .text()
        .await
        .map_err(|e| format!("body read from {url} failed: {e}"))?;

    parse_bunny_ip_response(&body, &content_type)
        .map_err(|e| format!("parsing response from {url} failed: {e}"))
}

/// Parse a Bunny edge-server-list response body.
///
/// Bunny returns either:
/// - A JSON array of IP strings: `["1.2.3.4", "5.6.7.8"]`
/// - Plain text with one IP per line
///
/// Both formats are handled. Any non-parseable entry causes an error rather
/// than silent truncation — a partial trust set is worse than keeping the
/// previous known-good set.
fn parse_bunny_ip_response(body: &str, content_type: &str) -> Result<Vec<IpAddr>, String> {
    let trimmed = body.trim();

    // Detect JSON array shape (starts with '[').
    if trimmed.starts_with('[') || content_type.contains("application/json") {
        return parse_json_ip_array(trimmed);
    }

    // Plain-text: one IP per line, ignore blank lines and '#' comments.
    parse_plaintext_ip_list(trimmed)
}

fn parse_json_ip_array(body: &str) -> Result<Vec<IpAddr>, String> {
    let strings: Vec<String> =
        serde_json::from_str(body).map_err(|e| format!("invalid JSON array: {e}"))?;

    if strings.is_empty() {
        return Ok(vec![]);
    }

    strings
        .iter()
        .map(|s| {
            s.trim()
                .parse::<IpAddr>()
                .map_err(|e| format!("entry {s:?} is not a valid IP address: {e}"))
        })
        .collect()
}

fn parse_plaintext_ip_list(body: &str) -> Result<Vec<IpAddr>, String> {
    let lines: Vec<&str> = body
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty() && !l.starts_with('#'))
        .collect();

    if lines.is_empty() {
        return Ok(vec![]);
    }

    lines
        .iter()
        .map(|s| {
            s.parse::<IpAddr>()
                .map_err(|e| format!("line {s:?} is not a valid IP address: {e}"))
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ip(s: &str) -> IpAddr {
        s.parse().unwrap()
    }

    fn trust_with(ips: &[&str]) -> BunnyIpTrust {
        let set: HashSet<IpAddr> = ips.iter().map(|s| ip(s)).collect();
        BunnyIpTrust::with_ips(set)
    }

    /// Every vendored IP in the builtin list must parse — a typo would silently
    /// shrink the seed trust set.
    #[test]
    fn builtin_ips_all_parse() {
        let trust = BunnyIpTrust::new();
        assert_eq!(trust.ips.load().len(), BUILTIN_IPS.len());
    }

    #[test]
    fn known_bunny_ips_match() {
        let t = BunnyIpTrust::new();
        // Should match at least one seed entry from each family.
        assert!(t.is_bunny(ip("185.152.66.1")));
        assert!(t.is_bunny(ip("2a00:5880:3::1")));
        // Random IPs must not match.
        assert!(!t.is_bunny(ip("8.8.8.8")));
        assert!(!t.is_bunny(ip("127.0.0.1")));
        assert!(!t.is_bunny(ip("2001:db8::1")));
    }

    /// Non-Bunny peer → X-Real-IP header ignored entirely (the spoofing case
    /// this module exists to prevent).
    #[test]
    fn untrusted_peer_ignores_header() {
        let t = trust_with(&["185.152.66.1"]);
        assert_eq!(
            t.resolve_client_ip(ip("203.0.113.5"), Some("1.2.3.4")),
            ip("203.0.113.5")
        );
    }

    /// Bunny peer with a valid X-Real-IP → the real client IP.
    #[test]
    fn bunny_peer_uses_header() {
        let t = trust_with(&["185.152.66.1", "2a00:5880:3::1"]);
        assert_eq!(
            t.resolve_client_ip(ip("185.152.66.1"), Some("198.51.100.7")),
            ip("198.51.100.7")
        );
        // IPv6 edge, IPv6 client
        assert_eq!(
            t.resolve_client_ip(ip("2a00:5880:3::1"), Some("2001:db8::7")),
            ip("2001:db8::7")
        );
    }

    /// Bunny peer but malformed X-Real-IP → peer, never the raw string.
    #[test]
    fn bunny_peer_rejects_malformed_header() {
        let t = trust_with(&["185.152.66.1"]);
        let peer = ip("185.152.66.1");
        for bad in [
            "not-an-ip",
            "1.2.3.4, 5.6.7.8",
            "1.2.3.4:8080",
            "<script>alert(1)</script>",
            "",
        ] {
            assert_eq!(t.resolve_client_ip(peer, Some(bad)), peer, "input: {bad:?}");
        }
        assert_eq!(t.resolve_client_ip(peer, None), peer);
    }

    /// Missing header on a Bunny peer → peer is returned.
    #[test]
    fn bunny_peer_missing_header_falls_back_to_peer() {
        let t = trust_with(&["185.152.66.1"]);
        assert_eq!(
            t.resolve_client_ip(ip("185.152.66.1"), None),
            ip("185.152.66.1")
        );
    }

    // --- Parser unit tests ---

    #[test]
    fn parses_json_array_format() {
        let body = r#"["1.2.3.4","5.6.7.8","2001:db8::1"]"#;
        let ips = parse_bunny_ip_response(body, "application/json").unwrap();
        assert_eq!(ips.len(), 3);
        assert!(ips.contains(&ip("1.2.3.4")));
        assert!(ips.contains(&ip("2001:db8::1")));
    }

    #[test]
    fn parses_plaintext_format() {
        let body = "1.2.3.4\n5.6.7.8\n# comment\n\n2001:db8::1\n";
        let ips = parse_bunny_ip_response(body, "text/plain").unwrap();
        assert_eq!(ips.len(), 3);
    }

    #[test]
    fn plaintext_autodetect_without_content_type() {
        let body = "10.0.0.1\n10.0.0.2\n";
        let ips = parse_bunny_ip_response(body, "").unwrap();
        assert_eq!(ips.len(), 2);
    }

    /// A single invalid entry in the list must cause an error rather than
    /// silently truncating — a partial trust set is worse than keeping the
    /// previous known-good set.
    #[test]
    fn rejects_invalid_ip_in_list() {
        let body = r#"["1.2.3.4","not-an-ip"]"#;
        assert!(parse_bunny_ip_response(body, "application/json").is_err());
    }

    #[test]
    fn rejects_invalid_ip_in_plaintext() {
        let body = "1.2.3.4\nbogus-entry\n5.6.7.8\n";
        assert!(parse_bunny_ip_response(body, "text/plain").is_err());
    }

    #[test]
    fn rejects_malformed_json() {
        let body = "[not json]";
        assert!(parse_bunny_ip_response(body, "application/json").is_err());
    }

    #[test]
    fn accepts_empty_json_array() {
        // An empty family list from one endpoint is OK — the caller decides
        // whether both being empty is an error.
        let body = "[]";
        let ips = parse_bunny_ip_response(body, "application/json").unwrap();
        assert!(ips.is_empty());
    }

    /// Spoofed X-Real-IP from an untrusted peer must never be trusted,
    /// regardless of the header value's validity as an IP address.
    #[test]
    fn spoofed_header_from_non_bunny_peer_is_ignored() {
        let t = trust_with(&["185.152.66.1"]);
        // Attacker at 203.0.113.99 claims to be 10.0.0.1
        let attacker = ip("203.0.113.99");
        let spoofed = "10.0.0.1";
        assert_eq!(t.resolve_client_ip(attacker, Some(spoofed)), attacker);
    }
}
