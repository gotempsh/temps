// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Atomic cloud-metadata egress protection for container traffic.
//!
//! A dedicated nftables base chain runs before Docker's forwarding rules and
//! rejects known metadata destinations for every routed container network,
//! including bridges created dynamically by Docker Compose. The complete table
//! is replaced in one nft transaction, so a failed refresh preserves the last
//! known-good policy instead of flushing a live chain.
//!
//! Some clouds put the host's DNS resolver inside a blocked range (Oracle
//! Cloud and Google Cloud answer DNS on 169.254.169.254, AWS on
//! 169.254.169.253). Docker hands containers the host's resolvers, so
//! rejecting the whole range cut every container off from DNS. Each install
//! therefore reads the host's resolver list and accepts DNS (UDP and TCP port
//! 53) to exactly those resolvers that fall in a blocked range, ahead of the
//! rejects. Every other port on those addresses, and every other address in
//! the blocked ranges, stays rejected.

use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};
use std::path::Path;
use std::process::Stdio;
use std::sync::Arc;
use std::time::Duration;

use bollard::Docker;
use thiserror::Error;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::sync::Notify;
use tracing::{debug, info, warn};

const TABLE: &str = "temps_metadata";
const FORWARD_PRIORITY: i32 = -200;

/// Alibaba Cloud's metadata endpoint.
const ALIBABA_METADATA: Ipv4Addr = Ipv4Addr::new(100, 100, 100, 200);
/// AWS's IPv6 metadata endpoint.
const AWS_METADATA_V6: Ipv6Addr = Ipv6Addr::new(0xfd00, 0x0ec2, 0, 0, 0, 0, 0, 0x0254);
/// Google Cloud's IPv6 metadata endpoint.
const GCP_METADATA_V6: Ipv6Addr = Ipv6Addr::new(0xfd20, 0x00ce, 0, 0, 0, 0, 0, 0x0254);

/// Where the host's resolvers are listed. Docker reads the second one instead
/// of the first when the first only names systemd-resolved's loopback stub, so
/// both are consulted.
const RESOLV_CONF_PATHS: [&str; 2] = ["/etc/resolv.conf", "/run/systemd/resolve/resolv.conf"];
/// resolv.conf is a few hundred bytes; never read more than this of it.
const RESOLV_CONF_MAX_BYTES: u64 = 64 * 1024;
/// glibc uses at most three nameservers; leave room for the union of both
/// files without letting a malformed file inflate the ruleset.
const MAX_DNS_EXCEPTIONS: usize = 8;

#[derive(Debug, Error)]
pub enum MetadataEgressError {
    #[error("could not start nft while installing cloud-metadata protection: {source}")]
    Spawn {
        #[source]
        source: std::io::Error,
    },
    #[error("nft did not expose stdin for the cloud-metadata ruleset")]
    MissingStdin,
    #[error("could not write the cloud-metadata ruleset to nft: {source}")]
    WriteRuleset {
        #[source]
        source: std::io::Error,
    },
    #[error("could not wait for nft to install cloud-metadata protection: {source}")]
    Wait {
        #[source]
        source: std::io::Error,
    },
    #[error("nft rejected the cloud-metadata ruleset (exit {exit_code}): {reason}")]
    ApplyFailed { exit_code: i32, reason: String },
}

/// Reconcile the global metadata guard. `docker` and `network_name` remain in
/// the signature because callers already have that deployment context; the
/// policy itself is intentionally network-independent so Compose bridges are
/// covered too.
pub async fn apply_metadata_egress_block(
    _docker: &Docker,
    network_name: &str,
) -> Result<(), MetadataEgressError> {
    if !cfg!(target_os = "linux") {
        return Ok(());
    }

    let dns_exceptions = install().await?;

    info!(
        network = network_name,
        table = TABLE,
        priority = FORWARD_PRIORITY,
        dns_exceptions = ?dns_exceptions,
        "Cloud-metadata egress protection reconciled"
    );
    Ok(())
}

/// Install the guard now and keep it installed until `shutdown` fires.
///
/// Deploys re-apply the guard, but nothing restores it after a reboot or a
/// host firewall flush, so a node that restarts and is not deployed to again
/// would run its containers without it. Each pass replaces the table in one
/// transaction, which also undoes any edit to it, and re-reads the host's
/// resolvers, so a resolver change from DHCP is picked up on the next pass.
/// Only the first success, a change of DNS exceptions, failures and the
/// recovery after a failure are logged, not every pass.
pub async fn maintain_metadata_egress_block(interval: Duration, shutdown: Arc<Notify>) {
    if !cfg!(target_os = "linux") {
        return;
    }

    let mut installed: Option<Vec<IpAddr>> = None;
    loop {
        match install().await {
            Ok(dns_exceptions) if installed.as_ref() != Some(&dns_exceptions) => {
                info!(
                    table = TABLE,
                    priority = FORWARD_PRIORITY,
                    dns_exceptions = ?dns_exceptions,
                    "Cloud-metadata egress protection installed"
                );
                installed = Some(dns_exceptions);
            }
            Ok(_) => {}
            Err(error) => {
                warn!(
                    %error,
                    retry_in_secs = interval.as_secs(),
                    "Cloud-metadata egress protection is not installed; containers on this node can reach the cloud metadata service until it is"
                );
                installed = None;
            }
        }
        tokio::select! {
            _ = tokio::time::sleep(interval) => {}
            _ = shutdown.notified() => return,
        }
    }
}

/// One nft transaction at a time, whichever caller asks. Returns the host
/// resolvers that DNS was let through to.
async fn install() -> Result<Vec<IpAddr>, MetadataEgressError> {
    static APPLY_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());
    let _guard = APPLY_LOCK.lock().await;
    let dns_exceptions = host_dns_exceptions().await;
    apply_nft(&render_ruleset(&dns_exceptions)).await?;
    Ok(dns_exceptions)
}

/// Whether the guard rejects traffic to `ip`.
fn is_blocked(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(v4) => v4.is_link_local() || v4 == ALIBABA_METADATA,
        IpAddr::V6(v6) => v6 == AWS_METADATA_V6 || v6 == GCP_METADATA_V6,
    }
}

/// Nameservers listed in resolv.conf text. Comments, other directives and
/// unparseable entries are skipped; an IPv6 zone (`%eth0`) is dropped.
fn parse_nameservers(resolv_conf: &str) -> Vec<IpAddr> {
    resolv_conf
        .lines()
        .filter_map(|line| {
            let mut fields = line.split_whitespace();
            if fields.next()? != "nameserver" {
                return None;
            }
            let address = fields.next()?;
            let address = address.split_once('%').map_or(address, |(ip, _)| ip);
            address.parse().ok()
        })
        .collect()
}

/// The host resolvers, among `nameservers`, that the guard would otherwise
/// reject: deduplicated, in order, at most [`MAX_DNS_EXCEPTIONS`].
fn dns_exceptions(nameservers: impl IntoIterator<Item = IpAddr>) -> Vec<IpAddr> {
    let mut exceptions = Vec::new();
    for ip in nameservers {
        if exceptions.len() == MAX_DNS_EXCEPTIONS {
            break;
        }
        if is_blocked(ip) && !exceptions.contains(&ip) {
            exceptions.push(ip);
        }
    }
    exceptions
}

/// Read at most [`RESOLV_CONF_MAX_BYTES`] of a resolv.conf. A missing file is
/// normal (no systemd-resolved) and yields `None`, as does an unreadable one:
/// without it the guard simply makes no DNS exception, the pre-existing policy.
async fn read_resolv_conf(path: &Path) -> Option<String> {
    let file = match tokio::fs::File::open(path).await {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return None,
        Err(error) => {
            warn!(path = %path.display(), %error, "Could not read the host resolver list; no DNS exception is made for resolvers it names");
            return None;
        }
    };
    let mut text = String::new();
    if let Err(error) = file
        .take(RESOLV_CONF_MAX_BYTES)
        .read_to_string(&mut text)
        .await
    {
        warn!(path = %path.display(), %error, "Could not read the host resolver list; no DNS exception is made for resolvers it names");
        return None;
    }
    Some(text)
}

/// The host's resolvers that sit inside a blocked range, from every
/// resolv.conf Docker may hand to containers.
async fn host_dns_exceptions() -> Vec<IpAddr> {
    let mut nameservers = Vec::new();
    for path in RESOLV_CONF_PATHS {
        if let Some(text) = read_resolv_conf(Path::new(path)).await {
            nameservers.extend(parse_nameservers(&text));
        }
    }
    let exceptions = dns_exceptions(nameservers);
    debug!(dns_exceptions = ?exceptions, "Host resolvers inside cloud-metadata ranges");
    exceptions
}

/// The complete nft batch. `dns_exceptions` are host resolvers inside a
/// blocked range; DNS to them is accepted before the rejects. An `accept` here
/// only ends this chain, so Docker's own forwarding rules still apply.
fn render_ruleset(dns_exceptions: &[IpAddr]) -> String {
    let mut out = String::from(
        "# `add` makes the following delete valid on both first install and refresh.\n\
         # nft applies the complete batch atomically at COMMIT.\n",
    );
    out.push_str(&format!("add table inet {TABLE}\n"));
    out.push_str(&format!("delete table inet {TABLE}\n"));
    out.push_str(&format!("add table inet {TABLE}\n"));
    out.push_str(&format!(
        "add chain inet {TABLE} forward {{ type filter hook forward priority {FORWARD_PRIORITY}; policy accept; }}\n"
    ));

    if !dns_exceptions.is_empty() {
        out.push_str(
            "\n# DNS to the host's own resolvers inside a metadata range (Oracle Cloud and\n\
             # Google Cloud answer DNS on 169.254.169.254, AWS on 169.254.169.253).\n\
             # Only port 53 to those exact addresses; the rejects below still apply\n\
             # to every other port, including the metadata HTTP service.\n",
        );
        for ip in dns_exceptions {
            let family = match ip {
                IpAddr::V4(_) => "ip",
                IpAddr::V6(_) => "ip6",
            };
            for proto in ["udp", "tcp"] {
                out.push_str(&format!(
                    "add rule inet {TABLE} forward {family} daddr {ip} {proto} dport 53 counter accept\n"
                ));
            }
        }
    }

    out.push_str(&format!(
        "\n# IPv4 link-local covers AWS/GCP/Azure/Hetzner/DO/Tencent/Oracle metadata endpoints.\n\
         add rule inet {TABLE} forward ip daddr 169.254.0.0/16 counter reject with icmp type port-unreachable\n\
         # Alibaba Cloud metadata.\n\
         add rule inet {TABLE} forward ip daddr {ALIBABA_METADATA} counter reject with icmp type port-unreachable\n\
         # AWS and Google IPv6 metadata endpoints.\n\
         add rule inet {TABLE} forward ip6 daddr {AWS_METADATA_V6} counter reject with icmpv6 type port-unreachable\n\
         add rule inet {TABLE} forward ip6 daddr {GCP_METADATA_V6} counter reject with icmpv6 type port-unreachable\n"
    ));
    out
}

async fn apply_nft(ruleset: &str) -> Result<(), MetadataEgressError> {
    let mut child = tokio::process::Command::new("nft")
        .arg("-f")
        .arg("-")
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|source| MetadataEgressError::Spawn { source })?;

    let mut stdin = child
        .stdin
        .take()
        .ok_or(MetadataEgressError::MissingStdin)?;
    stdin
        .write_all(ruleset.as_bytes())
        .await
        .map_err(|source| MetadataEgressError::WriteRuleset { source })?;
    drop(stdin);

    let output = child
        .wait_with_output()
        .await
        .map_err(|source| MetadataEgressError::Wait { source })?;
    if output.status.success() {
        return Ok(());
    }

    Err(MetadataEgressError::ApplyFailed {
        exit_code: output.status.code().unwrap_or(-1),
        reason: String::from_utf8_lossy(&output.stderr).trim().to_string(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    const NO_EXCEPTIONS_FIXTURE: &str = include_str!("../tests/fixtures/metadata-egress.nft");
    const LINK_LOCAL_DNS_FIXTURE: &str =
        include_str!("../tests/fixtures/metadata-egress-link-local-dns.nft");

    /// A golden fixture without its SPDX header and the blank line after it.
    fn golden(fixture: &str) -> String {
        fixture
            .lines()
            .skip_while(|line| line.starts_with("# SPDX-"))
            .skip_while(|line| line.is_empty())
            .map(|line| format!("{line}\n"))
            .collect()
    }

    fn ip(text: &str) -> IpAddr {
        text.parse().expect("test address parses")
    }

    /// Index of the first line containing `needle`.
    fn line_of(ruleset: &str, needle: &str) -> usize {
        ruleset
            .lines()
            .position(|line| line.contains(needle))
            .unwrap_or_else(|| panic!("ruleset has no line containing {needle:?}:\n{ruleset}"))
    }

    #[test]
    fn ruleset_without_dns_exceptions_matches_golden_fixture() {
        assert_eq!(render_ruleset(&[]), golden(NO_EXCEPTIONS_FIXTURE));
    }

    #[test]
    fn ruleset_with_link_local_resolver_matches_golden_fixture() {
        let ruleset = render_ruleset(&[ip("169.254.169.254"), ip("fd20:ce::254")]);
        assert_eq!(ruleset, golden(LINK_LOCAL_DNS_FIXTURE));
    }

    #[test]
    fn ruleset_is_atomic_and_precedes_docker_forwarding() {
        let ruleset = render_ruleset(&[ip("169.254.169.254")]);
        assert!(ruleset.contains("add table inet temps_metadata"));
        assert!(ruleset.contains("delete table inet temps_metadata"));
        assert!(ruleset.contains("hook forward priority -200"));
        assert!(!ruleset.contains("iifname"));
        assert!(!ruleset.contains("br-"));
        // The table is replaced before any rule is added to it.
        assert!(
            line_of(&ruleset, "add chain inet temps_metadata")
                < line_of(&ruleset, "add rule inet temps_metadata")
        );
    }

    #[test]
    fn ruleset_blocks_all_supported_ipv4_and_ipv6_metadata_ranges() {
        for exceptions in [vec![], vec![ip("169.254.169.254")]] {
            let ruleset = render_ruleset(&exceptions);
            for destination in [
                "ip daddr 169.254.0.0/16 counter reject",
                "ip daddr 100.100.100.200 counter reject",
                "ip6 daddr fd00:ec2::254 counter reject",
                "ip6 daddr fd20:ce::254 counter reject",
            ] {
                assert!(
                    ruleset.contains(destination),
                    "missing metadata rule {destination}"
                );
            }
            assert!(ruleset.contains("reject with icmp type port-unreachable"));
            assert!(ruleset.contains("reject with icmpv6 type port-unreachable"));
        }
    }

    #[test]
    fn dns_to_a_link_local_resolver_is_accepted_before_the_reject() {
        let ruleset = render_ruleset(&[ip("169.254.169.254")]);
        let reject = line_of(&ruleset, "ip daddr 169.254.0.0/16 counter reject");
        for proto in ["udp", "tcp"] {
            let accept = line_of(
                &ruleset,
                &format!("ip daddr 169.254.169.254 {proto} dport 53 counter accept"),
            );
            assert!(
                accept < reject,
                "{proto}/53 accept must precede the link-local reject"
            );
        }
    }

    #[test]
    fn dns_exception_is_port_53_only_and_never_a_range() {
        let ruleset = render_ruleset(&[ip("169.254.169.254"), ip("169.254.169.253")]);
        let accepts: Vec<&str> = ruleset
            .lines()
            .filter(|line| line.starts_with("add rule") && line.ends_with("accept"))
            .collect();
        assert_eq!(accepts.len(), 4, "one UDP and one TCP rule per resolver");
        for rule in accepts {
            assert!(
                rule.contains(" dport 53 "),
                "accept without port 53: {rule}"
            );
            assert!(!rule.contains('/'), "accept for a whole range: {rule}");
        }
        // The metadata HTTP service on the same address is still rejected.
        assert!(!ruleset.contains("dport 80"));
    }

    #[test]
    fn ruleset_does_not_block_linked_service_ranges() {
        let ruleset = render_ruleset(&[ip("169.254.169.254")]);
        for private_range in ["10.0.0.0", "172.16.0.0", "192.168.0.0"] {
            assert!(!ruleset.contains(private_range));
        }
    }

    #[test]
    fn parse_nameservers_reads_only_nameserver_lines() {
        let resolv_conf = "\
# This is /run/systemd/resolve/resolv.conf managed by man:systemd-resolved(8).
; legacy comment
nameserver 169.254.169.254
search example.internal
options edns0 trust-ad
nameserver   fe80::1%eth0
nameserver not-an-address
nameserver
nameserverx 10.0.0.1
";
        assert_eq!(
            parse_nameservers(resolv_conf),
            vec![ip("169.254.169.254"), ip("fe80::1")]
        );
        assert!(parse_nameservers("").is_empty());
    }

    #[test]
    fn dns_exceptions_keep_only_blocked_resolvers_deduplicated() {
        let nameservers = [
            ip("127.0.0.53"),      // systemd-resolved stub: not blocked
            ip("10.0.0.2"),        // private resolver: not blocked
            ip("169.254.169.254"), // Oracle Cloud / Google Cloud
            ip("169.254.169.253"), // AWS
            ip("169.254.169.254"), // listed in both resolv.conf files
            ip("100.100.2.136"),   // Alibaba Cloud's resolver: not blocked
            ip("fd00:ec2::253"),   // AWS IPv6 resolver: not blocked
            ip("fd20:ce::254"),    // Google Cloud IPv6 metadata host
        ];
        assert_eq!(
            dns_exceptions(nameservers),
            vec![
                ip("169.254.169.254"),
                ip("169.254.169.253"),
                ip("fd20:ce::254")
            ]
        );
    }

    #[test]
    fn dns_exceptions_are_capped() {
        let many = (1..=50).map(|n| IpAddr::V4(Ipv4Addr::new(169, 254, 0, n)));
        assert_eq!(dns_exceptions(many).len(), MAX_DNS_EXCEPTIONS);
    }

    #[test]
    fn no_link_local_resolver_means_the_unchanged_policy() {
        let exceptions = dns_exceptions(parse_nameservers("nameserver 1.1.1.1\n"));
        assert!(exceptions.is_empty());
        assert_eq!(render_ruleset(&exceptions), golden(NO_EXCEPTIONS_FIXTURE));
    }

    #[tokio::test]
    async fn read_resolv_conf_treats_a_missing_file_as_none() {
        let dir = tempfile::tempdir().expect("tempdir");
        assert!(read_resolv_conf(&dir.path().join("absent")).await.is_none());

        let path = dir.path().join("resolv.conf");
        std::fs::write(&path, "nameserver 169.254.169.254\n").expect("write resolv.conf");
        let text = read_resolv_conf(&path).await.expect("readable");
        assert_eq!(parse_nameservers(&text), vec![ip("169.254.169.254")]);
    }
}
