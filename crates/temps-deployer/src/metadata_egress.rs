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
//! therefore reads the resolvers Docker hands to containers and accepts DNS
//! (UDP and TCP port 53) to exactly those that fall in a blocked range, ahead
//! of the rejects. A resolver the host stops using keeps its exception while
//! this process runs, because containers started earlier still query it.
//! Every other port on those addresses, and every other address in the
//! blocked ranges, stays rejected.

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

/// The host's resolver list.
const RESOLV_CONF: &str = "/etc/resolv.conf";
/// systemd-resolved's upstream resolver list. Docker and BuildKit hand
/// containers this file instead of [`RESOLV_CONF`] when that one names only
/// the resolved stub, which containers cannot reach.
const SYSTEMD_RESOLV_CONF: &str = "/run/systemd/resolve/resolv.conf";
/// systemd-resolved's loopback stub listener.
const SYSTEMD_RESOLVED_STUB: IpAddr = IpAddr::V4(Ipv4Addr::new(127, 0, 0, 53));
/// resolv.conf is a few hundred bytes; never read more than this of it.
const RESOLV_CONF_MAX_BYTES: usize = 64 * 1024;
/// glibc uses at most three nameservers; leave room for resolvers retained
/// across a change without letting a malformed file inflate the ruleset.
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

    let installed = install().await?;

    info!(
        network = network_name,
        table = TABLE,
        priority = FORWARD_PRIORITY,
        dns_exceptions = ?installed.dns_exceptions,
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
/// Only the first success, a change of DNS exceptions (by any caller), failures
/// and the recovery after a failure are logged, not every pass.
pub async fn maintain_metadata_egress_block(interval: Duration, shutdown: Arc<Notify>) {
    if !cfg!(target_os = "linux") {
        return;
    }

    let mut recovering = true;
    loop {
        match install().await {
            Ok(installed) => {
                if recovering || installed.changed {
                    info!(
                        table = TABLE,
                        priority = FORWARD_PRIORITY,
                        dns_exceptions = ?installed.dns_exceptions,
                        "Cloud-metadata egress protection installed"
                    );
                }
                recovering = false;
            }
            Err(error) => {
                warn!(
                    %error,
                    retry_in_secs = interval.as_secs(),
                    "Cloud-metadata egress protection is not installed; containers on this node can reach the cloud metadata service until it is"
                );
                recovering = true;
            }
        }
        tokio::select! {
            _ = tokio::time::sleep(interval) => {}
            _ = shutdown.notified() => return,
        }
    }
}

/// What one install committed.
#[derive(Debug)]
struct Installed {
    /// Resolvers DNS is let through to.
    dns_exceptions: Vec<IpAddr>,
    /// Whether they differ from the last ruleset any caller committed.
    changed: bool,
}

/// What installs remember between passes, whichever caller ran them.
struct GuardState {
    /// DNS exceptions in the ruleset that is live.
    committed: Vec<IpAddr>,
    /// Whether the systemd-resolved upstream file has been the resolver file
    /// containers get, at any point while this process runs (see
    /// [`container_nameservers`]).
    systemd_selected: bool,
}

/// One nft transaction at a time, whichever caller asks. The lock also holds
/// the DNS exceptions last committed, so every caller compares against the
/// ruleset that is actually live.
async fn install() -> Result<Installed, MetadataEgressError> {
    static STATE: tokio::sync::Mutex<GuardState> = tokio::sync::Mutex::const_new(GuardState {
        committed: Vec::new(),
        systemd_selected: false,
    });
    let mut state = STATE.lock().await;
    let current = host_dns_exceptions(
        Path::new(RESOLV_CONF),
        Path::new(SYSTEMD_RESOLV_CONF),
        &mut state.systemd_selected,
    )
    .await;
    let dns_exceptions = retain_previous_exceptions(current, &state.committed);
    apply_nft(&render_ruleset(&dns_exceptions)).await?;
    let changed = state.committed != dns_exceptions;
    state.committed.clone_from(&dns_exceptions);
    Ok(Installed {
        dns_exceptions,
        changed,
    })
}

/// `current` followed by every previously committed exception it no longer
/// names, at most [`MAX_DNS_EXCEPTIONS`]. Docker writes a container's
/// resolv.conf when the container (or build step) starts and never updates a
/// running one, so after the host's resolver changes, workloads started
/// earlier still query the old one. Keeping its exception while this process
/// runs keeps their DNS working; it is still port 53 only, to an address the
/// host itself used as its resolver.
fn retain_previous_exceptions(current: Vec<IpAddr>, previous: &[IpAddr]) -> Vec<IpAddr> {
    let mut exceptions = current;
    exceptions.truncate(MAX_DNS_EXCEPTIONS);
    for ip in previous {
        if exceptions.len() == MAX_DNS_EXCEPTIONS {
            break;
        }
        if !exceptions.contains(ip) {
            exceptions.push(*ip);
        }
    }
    exceptions
}

/// Whether the guard rejects traffic to `ip`.
fn is_blocked(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(v4) => v4.is_link_local() || v4 == ALIBABA_METADATA,
        IpAddr::V6(v6) => v6 == AWS_METADATA_V6 || v6 == GCP_METADATA_V6,
    }
}

/// One `nameserver` address. A zone (`%eth0`) is only valid on an IPv6
/// address; an IPv4-mapped IPv6 address is the IPv4 address it carries, which
/// is what its packets are sent to.
fn parse_nameserver(address: &str) -> Option<IpAddr> {
    let ip = match address.split_once('%') {
        Some((ip, zone)) if !zone.is_empty() => IpAddr::V6(ip.parse::<Ipv6Addr>().ok()?),
        Some(_) => return None,
        None => address.parse().ok()?,
    };
    Some(ip.to_canonical())
}

/// Nameservers listed in resolv.conf text. Comments, other directives and
/// unparseable entries are skipped.
fn parse_nameservers(resolv_conf: &str) -> Vec<IpAddr> {
    resolv_conf
        .lines()
        .filter_map(|line| {
            let mut fields = line.split_whitespace();
            if fields.next()? != "nameserver" {
                return None;
            }
            parse_nameserver(fields.next()?)
        })
        .collect()
}

/// resolv.conf text from at most [`RESOLV_CONF_MAX_BYTES`] of a file read with
/// one byte more: when the limit cut the file, the last, possibly cut line is
/// dropped so a truncated address is never read as a different one. Bytes
/// that are not UTF-8 (a comment in another encoding) only spoil their line.
fn resolv_conf_text(mut bytes: Vec<u8>) -> String {
    if bytes.len() > RESOLV_CONF_MAX_BYTES {
        bytes.truncate(RESOLV_CONF_MAX_BYTES);
        let complete = bytes.iter().rposition(|b| *b == b'\n').map_or(0, |i| i + 1);
        bytes.truncate(complete);
    }
    String::from_utf8_lossy(&bytes).into_owned()
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
    let limit = u64::try_from(RESOLV_CONF_MAX_BYTES)
        .unwrap_or(u64::MAX)
        .saturating_add(1);
    let file = match tokio::fs::File::open(path).await {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return None,
        Err(error) => {
            warn!(path = %path.display(), %error, "Could not read the host resolver list; no DNS exception is made for resolvers it names");
            return None;
        }
    };
    let mut bytes = Vec::new();
    if let Err(error) = file.take(limit).read_to_end(&mut bytes).await {
        warn!(path = %path.display(), %error, "Could not read the host resolver list; no DNS exception is made for resolvers it names");
        return None;
    }
    Some(resolv_conf_text(bytes))
}

/// The nameservers Docker and BuildKit hand to containers: those in `primary`,
/// unless its only nameserver is the systemd-resolved stub, in which case
/// those of `systemd` (the same choice Docker makes).
///
/// dockerd makes that choice once and keeps it for its lifetime, so after
/// `primary` stops naming only the stub it may still copy `systemd` into new
/// containers. Once the stub case has been seen (`systemd_selected`), both
/// files' resolvers are therefore returned, `primary` first. A daemon that
/// made the systemd choice before this process first looked cannot be
/// detected: dockerd does not expose it.
async fn container_nameservers(
    primary: &Path,
    systemd: &Path,
    systemd_selected: &mut bool,
) -> Vec<IpAddr> {
    let read = |path| async move {
        read_resolv_conf(path)
            .await
            .map(|text| parse_nameservers(&text))
            .unwrap_or_default()
    };
    let mut nameservers = read(primary).await;
    if nameservers == [SYSTEMD_RESOLVED_STUB] {
        *systemd_selected = true;
        return read(systemd).await;
    }
    if *systemd_selected {
        nameservers.extend(read(systemd).await);
    }
    nameservers
}

/// The resolvers Docker hands to containers that sit inside a blocked range.
async fn host_dns_exceptions(
    primary: &Path,
    systemd: &Path,
    systemd_selected: &mut bool,
) -> Vec<IpAddr> {
    let exceptions =
        dns_exceptions(container_nameservers(primary, systemd, systemd_selected).await);
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

    #[test]
    fn a_zone_is_only_accepted_on_ipv6_and_mapped_addresses_are_ipv4() {
        assert_eq!(parse_nameserver("169.254.169.254%eth0"), None);
        assert_eq!(parse_nameserver("fe80::1%"), None);
        assert_eq!(parse_nameserver("fe80::1%eth0"), Some(ip("fe80::1")));
        assert_eq!(
            parse_nameserver("::ffff:169.254.169.254"),
            Some(ip("169.254.169.254"))
        );
        // The mapped and bare forms are one exception and one `ip daddr` rule.
        let exceptions = dns_exceptions(parse_nameservers(
            "nameserver ::ffff:169.254.169.254\nnameserver 169.254.169.254\n",
        ));
        assert_eq!(exceptions, vec![ip("169.254.169.254")]);
        assert!(render_ruleset(&exceptions).contains("ip daddr 169.254.169.254 udp dport 53"));
    }

    #[test]
    fn a_line_cut_by_the_read_limit_is_dropped() {
        // The limit falls inside the address: `169.254.169.25|4`.
        let mut bytes = vec![b'#'; RESOLV_CONF_MAX_BYTES - 26];
        bytes.push(b'\n');
        bytes.extend_from_slice(b"nameserver 169.254.169.254\n");
        assert!(bytes.len() > RESOLV_CONF_MAX_BYTES);
        assert!(parse_nameservers(&resolv_conf_text(bytes)).is_empty());

        // Within the limit, a final line without a newline is complete.
        let text = resolv_conf_text(b"nameserver 169.254.169.254".to_vec());
        assert_eq!(parse_nameservers(&text), vec![ip("169.254.169.254")]);
    }

    #[test]
    fn a_comment_that_is_not_utf8_does_not_hide_a_nameserver() {
        let text =
            resolv_conf_text(b"# caf\xe9 r\xe9solveur\nnameserver 169.254.169.254\n".to_vec());
        assert_eq!(parse_nameservers(&text), vec![ip("169.254.169.254")]);
    }

    #[tokio::test]
    async fn the_systemd_file_is_used_only_behind_the_resolved_stub() {
        let dir = tempfile::tempdir().expect("tempdir");
        let primary = dir.path().join("resolv.conf");
        let systemd = dir.path().join("systemd-resolv.conf");
        std::fs::write(&systemd, "nameserver 169.254.169.254\n").expect("write systemd file");

        // Only the stub: Docker hands containers the upstream file.
        std::fs::write(&primary, "nameserver 127.0.0.53\noptions edns0\n").expect("write");
        let mut selected = false;
        assert_eq!(
            host_dns_exceptions(&primary, &systemd, &mut selected).await,
            vec![ip("169.254.169.254")]
        );
        assert!(selected);

        // A real resolver in the primary file: the systemd file is inactive,
        // so its link-local resolver gets no exception.
        std::fs::write(&primary, "nameserver 10.0.0.53\n").expect("write");
        let mut selected = false;
        assert!(host_dns_exceptions(&primary, &systemd, &mut selected)
            .await
            .is_empty());
        assert!(!selected);

        // The stub next to another resolver is not the systemd-resolved case.
        std::fs::write(&primary, "nameserver 127.0.0.53\nnameserver 10.0.0.53\n").expect("write");
        assert!(host_dns_exceptions(&primary, &systemd, &mut selected)
            .await
            .is_empty());

        // A link-local resolver in the primary file itself.
        std::fs::write(&primary, "nameserver 169.254.169.253\n").expect("write");
        assert_eq!(
            host_dns_exceptions(&primary, &systemd, &mut selected).await,
            vec![ip("169.254.169.253")]
        );
    }

    #[tokio::test]
    async fn the_systemd_choice_sticks_like_dockerds() {
        let dir = tempfile::tempdir().expect("tempdir");
        let primary = dir.path().join("resolv.conf");
        let systemd = dir.path().join("systemd-resolv.conf");
        std::fs::write(&primary, "nameserver 127.0.0.53\n").expect("write");
        std::fs::write(&systemd, "nameserver 169.254.169.254\n").expect("write");
        let mut selected = false;
        host_dns_exceptions(&primary, &systemd, &mut selected).await;
        assert!(selected);

        // The primary file stops naming only the stub and the upstream
        // resolver changes: dockerd may still copy the systemd file, so its
        // current resolver is covered, after the primary's.
        std::fs::write(&primary, "nameserver 169.254.169.250\n").expect("write");
        std::fs::write(&systemd, "nameserver 169.254.169.253\n").expect("write");
        assert_eq!(
            host_dns_exceptions(&primary, &systemd, &mut selected).await,
            vec![ip("169.254.169.250"), ip("169.254.169.253")]
        );
        assert!(selected);
    }

    #[test]
    fn resolvers_from_before_a_change_keep_their_exception() {
        let old = ip("169.254.169.254");
        let new = ip("169.254.169.253");
        assert_eq!(
            retain_previous_exceptions(vec![new], &[old]),
            vec![new, old]
        );
        assert_eq!(
            retain_previous_exceptions(vec![old], &[old, new]),
            vec![old, new]
        );
        assert_eq!(
            retain_previous_exceptions(vec![], &[]),
            Vec::<IpAddr>::new()
        );

        // Current resolvers always fit; older ones fill what is left.
        let current: Vec<IpAddr> = (1..=MAX_DNS_EXCEPTIONS as u8)
            .map(|n| IpAddr::V4(Ipv4Addr::new(169, 254, 1, n)))
            .collect();
        let retained = retain_previous_exceptions(current.clone(), &[old]);
        assert_eq!(retained, current);
    }
}
