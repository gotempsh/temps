//! Trust store for Cloudflare egress IP ranges.
//!
//! SECURITY: `CF-Connecting-IP` is honored ONLY when the immediate TCP peer is
//! inside Cloudflare's published egress ranges. The header itself must never
//! influence any decision (including starting the refresher) — anyone can send
//! it directly to the listener; only the peer address is trustworthy. When the
//! peer is not a Cloudflare address the header is ignored entirely and the
//! socket IP is used, exactly like the `X-Forwarded-For` handling on the
//! console side (`temps_core::resolve_client_ip`).
//!
//! The trust set is seeded with `BUILTIN_RANGES`, vendored from
//! <https://www.cloudflare.com/ips/> at build time, so the check works offline
//! and self-hosted instances never phone home unless Cloudflare-fronted
//! traffic is actually observed. Once a peer inside the (builtin) ranges is
//! seen, a background task refreshes the list daily from Cloudflare's public
//! endpoint; any fetch failure keeps the last known-good set, so the builtin
//! list is the permanent floor and the trust set never shrinks to empty.
//! A stale list degrades safely: traffic from a not-yet-known Cloudflare range
//! records the edge IP (today's behavior) rather than anything spoofable.

use arc_swap::ArcSwap;
use ipnetwork::IpNetwork;
use once_cell::sync::Lazy;
use std::net::IpAddr;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;
use tracing::{debug, info, warn};

/// Cloudflare's published egress ranges (<https://www.cloudflare.com/ips/>),
/// vendored 2026-07-16. These change on the order of years and are announced
/// in advance; the background refresher covers drift between releases.
const BUILTIN_RANGES: &[&str] = &[
    // IPv4
    "173.245.48.0/20",
    "103.21.244.0/22",
    "103.22.200.0/22",
    "103.31.4.0/22",
    "141.101.64.0/18",
    "108.162.192.0/18",
    "190.93.240.0/20",
    "188.114.96.0/20",
    "197.234.240.0/22",
    "198.41.128.0/17",
    "162.158.0.0/15",
    "104.16.0.0/13",
    "104.24.0.0/14",
    "172.64.0.0/13",
    "131.0.72.0/22",
    // IPv6
    "2400:cb00::/32",
    "2606:4700::/32",
    "2803:f800::/32",
    "2405:b500::/32",
    "2405:8100::/32",
    "2a06:98c0::/29",
    "2c0f:f248::/32",
];

const CLOUDFLARE_IPS_URL: &str = "https://api.cloudflare.com/client/v4/ips";
const REFRESH_INTERVAL: Duration = Duration::from_secs(24 * 60 * 60);
/// Retry sooner after a failed fetch so a transient outage doesn't leave the
/// list stale for a full day.
const RETRY_INTERVAL: Duration = Duration::from_secs(60 * 60);
const FETCH_TIMEOUT: Duration = Duration::from_secs(30);

/// Process-wide trust store, following the `crawler_detector` global pattern.
/// Seeded with the builtin ranges; the refresher starts lazily on first
/// Cloudflare-peer sighting.
pub static CLOUDFLARE_TRUST: Lazy<CloudflareIpTrust> = Lazy::new(CloudflareIpTrust::new);

pub struct CloudflareIpTrust {
    ranges: Arc<ArcSwap<Vec<IpNetwork>>>,
    refresher_started: AtomicBool,
}

impl Default for CloudflareIpTrust {
    fn default() -> Self {
        Self::new()
    }
}

impl CloudflareIpTrust {
    pub fn new() -> Self {
        let builtin: Vec<IpNetwork> = BUILTIN_RANGES
            .iter()
            .filter_map(|cidr| {
                cidr.parse()
                    .map_err(|e| warn!("builtin Cloudflare CIDR {cidr} failed to parse: {e}"))
                    .ok()
            })
            .collect();
        Self {
            ranges: Arc::new(ArcSwap::from_pointee(builtin)),
            refresher_started: AtomicBool::new(false),
        }
    }

    /// Whether `ip` is inside the currently known Cloudflare egress ranges.
    /// Lock-free snapshot read; linear scan over ~22 networks.
    pub fn is_cloudflare(&self, ip: IpAddr) -> bool {
        self.ranges.load().iter().any(|net| net.contains(ip))
    }

    /// Resolve the client IP for a connection: if the TCP peer is a verified
    /// Cloudflare address and it supplied a syntactically valid
    /// `CF-Connecting-IP`, that is the real client; otherwise the peer itself.
    ///
    /// The header value must parse as a bare `IpAddr` — anything else
    /// (ports, comma lists, garbage) falls back to the peer so no
    /// attacker-shaped string can reach logs or the `X-Forwarded-For` we set
    /// upstream.
    pub fn resolve_client_ip(&self, peer: IpAddr, cf_connecting_ip: Option<&str>) -> IpAddr {
        if !self.is_cloudflare(peer) {
            return peer;
        }
        // First confirmed Cloudflare-fronted connection: start keeping the
        // ranges fresh. Keyed on the peer, never on the header.
        self.ensure_refresher();
        match cf_connecting_ip.and_then(|v| v.trim().parse::<IpAddr>().ok()) {
            Some(client) => client,
            None => peer,
        }
    }

    /// Start the daily background refresh exactly once. No-op outside a tokio
    /// runtime (unit tests, tooling) — the builtin list still applies there.
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
        info!("Cloudflare-fronted traffic detected; starting IP-range refresher");
        let ranges = Arc::clone(&self.ranges);
        handle.spawn(refresh_loop(ranges));
    }
}

async fn refresh_loop(ranges: Arc<ArcSwap<Vec<IpNetwork>>>) {
    let Ok(client) = reqwest::Client::builder()
        .timeout(FETCH_TIMEOUT)
        .build()
        .map_err(|e| warn!("Cloudflare IP refresher disabled (HTTP client init failed): {e}"))
    else {
        // Builtin/vendored ranges remain in effect.
        return;
    };
    loop {
        let delay = match refresh_once(&client, &ranges).await {
            Ok(count) => {
                debug!("Cloudflare IP ranges refreshed: {count} networks");
                REFRESH_INTERVAL
            }
            Err(e) => {
                warn!("Cloudflare IP range refresh failed (keeping last known set): {e}");
                RETRY_INTERVAL
            }
        };
        tokio::time::sleep(delay).await;
    }
}

async fn refresh_once(
    client: &reqwest::Client,
    ranges: &ArcSwap<Vec<IpNetwork>>,
) -> Result<usize, String> {
    let body = client
        .get(CLOUDFLARE_IPS_URL)
        .send()
        .await
        .map_err(|e| format!("request failed: {e}"))?
        .error_for_status()
        .map_err(|e| format!("bad status: {e}"))?
        .text()
        .await
        .map_err(|e| format!("body read failed: {e}"))?;
    let parsed = parse_cloudflare_ips_response(&body)?;
    let count = parsed.len();
    ranges.store(Arc::new(parsed));
    Ok(count)
}

/// Parse the `/client/v4/ips` JSON body into networks. Errors (rather than
/// returning a partial set) if either address family is empty or anything
/// fails to parse — a truncated or reshaped response must never replace a
/// good trust set.
fn parse_cloudflare_ips_response(body: &str) -> Result<Vec<IpNetwork>, String> {
    #[derive(serde::Deserialize)]
    struct Response {
        success: bool,
        result: Option<ResponseResult>,
    }
    #[derive(serde::Deserialize)]
    struct ResponseResult {
        ipv4_cidrs: Vec<String>,
        ipv6_cidrs: Vec<String>,
    }

    let resp: Response = serde_json::from_str(body).map_err(|e| format!("invalid JSON: {e}"))?;
    if !resp.success {
        return Err("response marked success=false".to_string());
    }
    let result = resp.result.ok_or("response missing result")?;
    if result.ipv4_cidrs.is_empty() || result.ipv6_cidrs.is_empty() {
        return Err(format!(
            "suspiciously empty range list (v4: {}, v6: {})",
            result.ipv4_cidrs.len(),
            result.ipv6_cidrs.len()
        ));
    }
    result
        .ipv4_cidrs
        .iter()
        .chain(result.ipv6_cidrs.iter())
        .map(|cidr| {
            cidr.parse::<IpNetwork>()
                .map_err(|e| format!("CIDR {cidr} failed to parse: {e}"))
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn trust() -> CloudflareIpTrust {
        CloudflareIpTrust::new()
    }

    fn ip(s: &str) -> IpAddr {
        s.parse().unwrap()
    }

    /// Every vendored CIDR must parse — a typo in the builtin list would
    /// silently shrink the trust set.
    #[test]
    fn builtin_ranges_all_parse() {
        assert_eq!(trust().ranges.load().len(), BUILTIN_RANGES.len());
    }

    #[test]
    fn known_cloudflare_ips_match() {
        let t = trust();
        assert!(t.is_cloudflare(ip("104.16.1.1")));
        assert!(t.is_cloudflare(ip("172.64.0.1")));
        assert!(t.is_cloudflare(ip("2606:4700::1")));
        assert!(!t.is_cloudflare(ip("8.8.8.8")));
        assert!(!t.is_cloudflare(ip("127.0.0.1")));
        assert!(!t.is_cloudflare(ip("2001:db8::1")));
    }

    /// Non-Cloudflare peer → header ignored entirely (the spoofing case this
    /// module exists to prevent).
    #[test]
    fn untrusted_peer_ignores_header() {
        let t = trust();
        assert_eq!(
            t.resolve_client_ip(ip("203.0.113.5"), Some("1.2.3.4")),
            ip("203.0.113.5")
        );
    }

    /// Cloudflare peer with a valid header → the real client IP.
    #[test]
    fn cloudflare_peer_uses_header() {
        let t = trust();
        assert_eq!(
            t.resolve_client_ip(ip("104.16.1.1"), Some("198.51.100.7")),
            ip("198.51.100.7")
        );
        // IPv6 edge, IPv6 client
        assert_eq!(
            t.resolve_client_ip(ip("2606:4700::1"), Some("2001:db8::7")),
            ip("2001:db8::7")
        );
    }

    /// Cloudflare peer but malformed header → peer, never the raw string.
    #[test]
    fn cloudflare_peer_rejects_malformed_header() {
        let t = trust();
        let peer = ip("104.16.1.1");
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

    #[test]
    fn parses_cloudflare_ips_endpoint_shape() {
        let body = r#"{
            "result": {
                "ipv4_cidrs": ["173.245.48.0/20", "103.21.244.0/22"],
                "ipv6_cidrs": ["2400:cb00::/32"],
                "etag": "abc123"
            },
            "success": true,
            "errors": [],
            "messages": []
        }"#;
        let nets = parse_cloudflare_ips_response(body).unwrap();
        assert_eq!(nets.len(), 3);
    }

    /// A degenerate response must never replace the trust set.
    #[test]
    fn rejects_bad_refresh_payloads() {
        assert!(parse_cloudflare_ips_response("not json").is_err());
        assert!(parse_cloudflare_ips_response(r#"{"success": false, "result": null}"#).is_err());
        // Empty v6 family → rejected even though v4 is present.
        assert!(parse_cloudflare_ips_response(
            r#"{"success": true, "result": {"ipv4_cidrs": ["1.2.3.0/24"], "ipv6_cidrs": []}}"#
        )
        .is_err());
        assert!(parse_cloudflare_ips_response(
            r#"{"success": true, "result": {"ipv4_cidrs": ["bogus"], "ipv6_cidrs": ["2400:cb00::/32"]}}"#
        )
        .is_err());
    }
}
