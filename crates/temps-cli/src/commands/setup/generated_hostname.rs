// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

use std::net::Ipv4Addr;

/// True if `zone` is a loopback zone (`127.0.0.1.sslip.io`, `localhost`, …) —
/// local mode, where Let's Encrypt cannot reach the box for the HTTP-01
/// challenge, so on-demand TLS would never fire. This is the only condition
/// that gates ADR-018 §6 on-demand-TLS auto-enablement; any other (non-empty,
/// non-loopback) base domain qualifies, regardless of whether it is sslip.io.
/// Build the wildcard-DNS base domain for `ip`, using the **dashed** spelling.
///
/// Wildcard-DNS services (sslip.io and friends) locate the target address by
/// scanning the hostname for four numeric components joined by a *single*
/// separator style, taking the leftmost match — and that match is not anchored
/// to the base domain. With a dotted base, any generated label ending in a
/// number donates it to the front of the IP and the name resolves elsewhere:
///
/// ```text
/// observability-starter-1.127.0.0.1.sslip.io  ->  1.127.0.0
/// pr-42.127.0.0.1.sslip.io                    ->  42.127.0.0
/// ```
///
/// Deployment slugs are `{project}-{n}` and preview environments are
/// `pr-{number}`, so on a dotted base that is *every* generated hostname. The
/// dashed spelling makes the separator styles disagree (`1.127-0-0` mixes a dot
/// and dashes, so it is rejected) and the real `127-0-0-1` wins.
///
/// The published installer emits the same dashed form and passes it via
/// `--wildcard-domain`; this covers `temps setup --auto` generating its own.
///
/// Note: IPv4 only. sslip.io spells IPv6 with dashes for `:` and this does not
/// handle that — pre-existing, and `--wildcard-domain` remains the escape hatch.
pub(super) fn generated_domain_for(ip: &str, suffix: &str) -> anyhow::Result<String> {
    let address = ip.parse::<Ipv4Addr>().map_err(|error| {
        anyhow::anyhow!(
            "Cannot generate a hostname from server IP '{ip}': {error}. \
             Automatic domains currently require IPv4; provide --server-ip with a valid IPv4 \
             address or use an explicit --wildcard-domain."
        )
    })?;
    let suffix = suffix.trim_end_matches('.').to_ascii_lowercase();
    let encoded = address.to_string().replace('.', "-");
    if !suffix.contains('.')
        || encoded.len() + 1 + suffix.len() > 253
        || !suffix.split('.').all(|label| {
            !label.is_empty()
                && label.len() <= 63
                && !label.starts_with('-')
                && !label.ends_with('-')
                && label
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b == b'-')
        })
    {
        anyhow::bail!("Invalid generated hostname suffix '{suffix}' for server IP '{ip}'");
    }
    Ok(format!("{encoded}.{suffix}"))
}

pub(super) fn embedded_ipv4_from_zone(zone: &str, suffix: &str) -> Option<Ipv4Addr> {
    let encoded = zone
        .trim()
        .trim_end_matches('.')
        .to_ascii_lowercase()
        .strip_suffix(&format!(
            ".{}",
            suffix.trim_end_matches('.').to_ascii_lowercase()
        ))?
        .to_string();

    encoded
        .parse()
        .ok()
        .or_else(|| encoded.replace('-', ".").parse().ok())
}

pub(super) fn is_loopback_zone(zone: &str) -> bool {
    let z = zone.trim().trim_end_matches('.').to_ascii_lowercase();
    z == "localhost"
        || z.ends_with(".localhost")
        || z.starts_with("127.0.0.1")
        || z.contains("127-0-0-1")
        // `localho.st` is a real public DNS name that resolves to 127.0.0.1 --
        // used throughout this codebase as the local/loopback dev domain
        // (see doctor.rs's "default - configure a real domain for
        // production" warning and the proxy's SameSite-safe cookie tests)
        // specifically so browsers treat it as a normal internet host. It is
        // still loopback for ACME purposes: Let's Encrypt's HTTP-01/on-demand
        // validators cannot reach a box whose public DNS answer is its own
        // loopback address, so issuance can never succeed and on-demand TLS
        // must stay off here exactly as it does for `localhost`/`127.0.0.1`.
        || z == "localho.st"
        || z.ends_with(".localho.st")
}
