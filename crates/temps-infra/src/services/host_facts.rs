// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! What Temps reads about the control plane's own machine before asking the
//! operator anything: the first screen of the welcome flow ("Check the box").
//!
//! Everything here is read-only and gathered from this process: processor,
//! memory, swap and disk through `sysinfo`, Docker through its API, whether
//! something already answers on ports 80 and 443 locally, the LAN and public
//! addresses, and Tailscale.
//!
//! Tailscale is read without opening tailscaled's socket: ADR 065 D2 has
//! Temps talk to tailscaled only with the owner's consent. The box is on a
//! tailnet when an interface holds a `100.64.0.0/10` address; its machine and
//! tailnet names come from MagicDNS answering a reverse lookup for that
//! address. What only the LocalAPI could say (HTTPS certificates) is reported
//! as not read, never guessed.
//!
//! Whether ports 80 and 443 answer *from the internet* cannot be told from
//! inside the box, so [`InboundCheck`] is always "not checked" here and says
//! why.

use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::sync::Arc;
use std::time::Duration;

use serde::{Deserialize, Serialize};
use temps_core::DockerHandle;
use tracing::debug;
use utoipa::ToSchema;

use super::PlatformInfoService;

/// How long a local port probe may take.
const PORT_PROBE_TIMEOUT: Duration = Duration::from_millis(400);
/// The most local addresses a port is probed on; a machine with more
/// interface addresses than this has the first ones probed.
const MAX_PROBED_ADDRESSES: usize = 64;
/// How long the MagicDNS reverse lookup may take.
const MAGIC_DNS_TIMEOUT: Duration = Duration::from_millis(800);
/// How long Docker may take to report its version.
const DOCKER_TIMEOUT: Duration = Duration::from_secs(3);
/// Tailscale's MagicDNS resolver.
const MAGIC_DNS_RESOLVER: IpAddr = IpAddr::V4(Ipv4Addr::new(100, 100, 100, 100));

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct MemoryFact {
    pub total_bytes: u64,
    /// Memory the OS reports as available for new work now.
    pub available_bytes: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct DiskFact {
    /// The mount point holding Temps' data directory.
    pub mount_point: String,
    pub total_bytes: u64,
    pub available_bytes: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct DockerFact {
    pub running: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub version: Option<String>,
    /// Why Docker could not be read, when it could not.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

/// Who holds a port on this machine.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum PortHolder {
    /// Nothing answers on it.
    Free,
    /// Temps' own proxy answers on it: one of its configured listeners is
    /// on the port, answers, and the proxy proved who it is. Configured but
    /// not answering (a separate proxy process stopped) is `Free`: nothing
    /// serves it; answering without the proof is `Other`.
    Temps,
    /// Something else answers on it.
    Other,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct PortFact {
    pub port: u16,
    pub holder: PortHolder,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct TailscaleFact {
    /// An interface holds a tailnet address.
    pub running: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ip: Option<String>,
    /// The machine's name on the tailnet, from MagicDNS.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub machine: Option<String>,
    /// The tailnet's MagicDNS suffix, such as `example.ts.net`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tailnet: Option<String>,
    /// MagicDNS answered for this machine.
    pub magic_dns: bool,
    /// What was not read, and why. HTTPS certificates are only known through
    /// tailscaled's LocalAPI, which Temps opens only with consent.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct InboundCheck {
    /// Whether ports 80 and 443 were checked from outside the box.
    pub checked: bool,
    /// The answer, when checked.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reachable: Option<bool>,
    /// Why it was not checked, or what was found.
    pub note: String,
}

/// The control plane's machine, as the welcome flow's first screen shows it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct HostFacts {
    pub hostname: Option<String>,
    pub cpu_cores: u32,
    pub memory: MemoryFact,
    /// Swap configured; `0` when there is none.
    pub swap_total_bytes: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub disk: Option<DiskFact>,
    pub docker: DockerFact,
    /// Ports 80 and 443 on this machine.
    pub ports: Vec<PortFact>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub lan_ip: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub public_ip: Option<String>,
    pub tailscale: TailscaleFact,
    pub inbound: InboundCheck,
}

/// Split a MagicDNS name (`box.example.ts.net.`) into the machine and the
/// tailnet. `None` when it is not a tailnet name.
pub fn split_tailnet_name(name: &str) -> Option<(String, String)> {
    let name = name.trim().trim_end_matches('.').to_ascii_lowercase();
    let (machine, tailnet) = name.split_once('.')?;
    if machine.is_empty() || !tailnet.ends_with(".ts.net") {
        return None;
    }
    Some((machine.to_string(), tailnet.to_string()))
}

/// The port the proxy listens on, from a configured `host:port` address.
pub fn listener_port(address: &str) -> Option<u16> {
    address
        .parse::<SocketAddr>()
        .map(|a| a.port())
        .ok()
        .or_else(|| address.rsplit_once(':').and_then(|(_, p)| p.parse().ok()))
}

/// Where to connect to reach a listener configured at `address`: the address
/// itself when it names one (a proxy bound only to a LAN or overlay address
/// answers nowhere else), the loopback of its family when it is unspecified
/// (`0.0.0.0`, `::`), and IPv4 loopback for a host name.
pub fn listener_probe_address(address: &str) -> Option<SocketAddr> {
    let parsed = address.parse::<SocketAddr>().ok().or_else(|| {
        listener_port(address).map(|port| SocketAddr::from((Ipv4Addr::LOCALHOST, port)))
    })?;
    Some(match parsed.ip() {
        IpAddr::V4(ip) if ip.is_unspecified() => {
            SocketAddr::from((Ipv4Addr::LOCALHOST, parsed.port()))
        }
        IpAddr::V6(ip) if ip.is_unspecified() => {
            SocketAddr::from((std::net::Ipv6Addr::LOCALHOST, parsed.port()))
        }
        _ => parsed,
    })
}

/// Who holds a port: Temps when one of its configured listeners on the port
/// answered at its own address and Temps' proxy proved it is running
/// (`proxy_identified`), someone else when something answered without that
/// proof (another server took the stopped proxy's address) or only on one of
/// this machine's other addresses, and nobody when nothing did (a configured
/// proxy that is stopped serves nothing).
pub fn port_holder(
    temps_answered: bool,
    proxy_identified: bool,
    local_answered: bool,
) -> PortHolder {
    match (temps_answered, proxy_identified, local_answered) {
        (true, true, _) => PortHolder::Temps,
        (true, false, _) | (false, _, true) => PortHolder::Other,
        (false, _, false) => PortHolder::Free,
    }
}

/// How host facts find Temps' own proxy on this machine.
#[derive(Debug, Clone)]
pub struct TempsProxy {
    /// Where its HTTP and HTTPS listeners answer (`listener_probe_address`).
    pub listeners: Vec<SocketAddr>,
    /// Where its HTTP listener answers, asked who it is.
    pub http: Option<SocketAddr>,
    /// The installation key the proxy proves it holds.
    pub key: String,
}

/// Whether the server answering at `http` is Temps' proxy: it answers a
/// fresh challenge with a MAC under the installation key, which nothing
/// else on the port can. A TCP connection alone proves nothing, as another
/// server may have taken the address of a proxy that stopped.
async fn proxy_identifies(http: SocketAddr, key: &str) -> bool {
    use temps_core::probe_auth;
    let nonce = probe_auth::identity_nonce();
    let Ok(client) = reqwest::Client::builder()
        .timeout(PORT_PROBE_TIMEOUT * 2)
        .no_proxy()
        .redirect(reqwest::redirect::Policy::none())
        .build()
    else {
        return false;
    };
    let Ok(response) = client
        .get(format!("http://{http}{}", probe_auth::PROXY_IDENTITY_PATH))
        .header(reqwest::header::HOST, probe_auth::PROXY_IDENTITY_HOST)
        .header(
            probe_auth::HEADER,
            probe_auth::sign(key, probe_auth::PROXY_IDENTITY_HOST, probe_auth::now()),
        )
        .header(probe_auth::PROXY_IDENTITY_NONCE_HEADER, &nonce)
        .send()
        .await
    else {
        return false;
    };
    response
        .headers()
        .get(probe_auth::PROXY_IDENTITY_HEADER)
        .and_then(|v| v.to_str().ok())
        .is_some_and(|answer| probe_auth::verify_identity(key, &nonce, answer))
}

/// Whether something accepts a TCP connection at `address`, within the
/// probe timeout. A connection, never a bind: binding even briefly could make
/// a real service fail to start.
async fn answers(address: SocketAddr) -> bool {
    tokio::time::timeout(PORT_PROBE_TIMEOUT, tokio::net::TcpStream::connect(address))
        .await
        .map(|r| r.is_ok())
        .unwrap_or(false)
}

/// Whether any of `addresses` accepts a connection. The probes run at once,
/// so the whole check takes one probe timeout however many there are, and
/// the rest stop once one answers.
async fn any_answers(addresses: impl IntoIterator<Item = SocketAddr>) -> bool {
    let mut probes = tokio::task::JoinSet::new();
    for address in addresses.into_iter().take(MAX_PROBED_ADDRESSES) {
        probes.spawn(answers(address));
    }
    while let Some(answered) = probes.join_next().await {
        if matches!(answered, Ok(true)) {
            return true;
        }
    }
    false
}

/// A probe target for `ip` (port `0`, set per probe), with its interface's
/// scope when `ip` is an IPv6 link-local address, which cannot be reached
/// without one.
fn probe_target(ip: IpAddr, scope: Option<u32>) -> SocketAddr {
    match ip {
        IpAddr::V6(v6) if v6.is_unicast_link_local() => {
            SocketAddr::V6(std::net::SocketAddrV6::new(v6, 0, 0, scope.unwrap_or(0)))
        }
        _ => SocketAddr::new(ip, 0),
    }
}

/// The IPv6 link-local addresses in a `/proc/net/if_inet6` listing, with
/// their interface index as the scope. Each line is the address in 32 hex
/// digits, then the interface index in hex, the prefix length, scope, flags
/// and name; a line that does not read so is skipped.
fn link_local_from_if_inet6(listing: &str) -> Vec<SocketAddr> {
    listing
        .lines()
        .filter_map(|line| {
            let mut fields = line.split_whitespace();
            let hex = fields.next()?;
            let index = u32::from_str_radix(fields.next()?, 16).ok()?;
            let ip = std::net::Ipv6Addr::from(u128::from_str_radix(hex, 16).ok()?);
            (hex.len() == 32 && ip.is_unicast_link_local())
                .then(|| probe_target(IpAddr::V6(ip), Some(index)))
        })
        .collect()
}

/// This machine's IPv6 link-local addresses, which `if_addrs` leaves out.
/// Read on Linux, where Temps' servers run; elsewhere there are none.
fn link_local_addresses() -> Vec<SocketAddr> {
    /// More than any real listing; a larger one is not read past this.
    const MAX_LISTING_BYTES: u64 = 256 * 1024;
    if !cfg!(target_os = "linux") {
        return Vec::new();
    }
    use std::io::Read;
    let Ok(file) = std::fs::File::open("/proc/net/if_inet6") else {
        return Vec::new();
    };
    let mut listing = String::new();
    if file
        .take(MAX_LISTING_BYTES)
        .read_to_string(&mut listing)
        .is_err()
    {
        return Vec::new();
    }
    link_local_from_if_inet6(&listing)
}

/// This machine's own addresses a server can listen on, as probe targets
/// (port `0`): both loopbacks and every interface address, IPv4 and IPv6,
/// link-local ones with their interface's scope. A server bound only to a
/// LAN, overlay or IPv6 address answers nowhere else, so loopback alone
/// would call its port free. At most [`MAX_PROBED_ADDRESSES`], each once.
#[cfg(test)]
fn local_addresses() -> Vec<SocketAddr> {
    local_probe_targets().0
}

/// [`local_addresses`], and whether they are all of this machine's: false
/// when its interfaces could not be listed or more were found than are
/// probed, so a port none of them answers on is not yet known to be free.
fn local_probe_targets() -> (Vec<SocketAddr>, bool) {
    let listed = if_addrs::get_if_addrs();
    let complete = listed.is_ok();
    let interfaces = listed.unwrap_or_default();
    let candidates = [
        probe_target(IpAddr::V4(Ipv4Addr::LOCALHOST), None),
        probe_target(IpAddr::V6(std::net::Ipv6Addr::LOCALHOST), None),
    ]
    .into_iter()
    .chain(interfaces.iter().map(|i| probe_target(i.ip(), i.index)))
    .chain(link_local_addresses());
    // One past the limit is enough to know some address is left out, so no
    // more are kept than that, however many the machine has.
    let mut seen = std::collections::HashSet::new();
    let mut distinct: Vec<SocketAddr> = candidates
        .filter(|target| seen.insert(*target))
        .take(MAX_PROBED_ADDRESSES + 1)
        .collect();
    let complete = complete && distinct.len() <= MAX_PROBED_ADDRESSES;
    distinct.truncate(MAX_PROBED_ADDRESSES);
    (distinct, complete)
}

/// Who holds a port no probed address answered on, when not every local
/// address was probed: Linux's socket tables list every socket listening on
/// it, whatever its address. Read, not tried with a bind, which a port under
/// 1024 refuses without the privilege, a connection in TIME_WAIT from a
/// stopped server blocks with nothing listening, and another probe doing
/// the same at once would take for a listener. Where the tables cannot be
/// read (not Linux), a bind on every address tells instead.
async fn unprobed_holder(port: u16) -> PortHolder {
    match tokio::task::spawn_blocking(move || listening_in_socket_tables(port)).await {
        Ok(Some(true)) => PortHolder::Other,
        Ok(Some(false)) => PortHolder::Free,
        _ => wildcard_bind_holder(port),
    }
}

/// Who holds `port` by binding it on every IPv4 and then every IPv6
/// address, without `SO_REUSEADDR`: the bind fails while a socket uses the
/// port on any address. Only where no socket table can be read; there a
/// connection lingering after a server stopped also counts as held.
fn wildcard_bind_holder(port: u16) -> PortHolder {
    let in_use = [
        tokio::net::TcpSocket::new_v4().map(|socket| (socket, IpAddr::V4(Ipv4Addr::UNSPECIFIED))),
        tokio::net::TcpSocket::new_v6()
            .map(|socket| (socket, IpAddr::V6(std::net::Ipv6Addr::UNSPECIFIED))),
    ]
    .into_iter()
    .filter_map(Result::ok)
    .any(|(socket, ip)| {
        // Each is dropped before the next, so it never blocks the other.
        matches!(
            socket.bind(SocketAddr::new(ip, port)),
            Err(error) if error.kind() == std::io::ErrorKind::AddrInUse
        )
    });
    if in_use {
        PortHolder::Other
    } else {
        PortHolder::Free
    }
}

/// Whether a socket listens on TCP `port` by Linux's `/proc/net/tcp` and
/// `tcp6`; `None` when neither can be read.
fn listening_in_socket_tables(port: u16) -> Option<bool> {
    let mut read_any = false;
    for table in ["/proc/net/tcp", "/proc/net/tcp6"] {
        let Ok(file) = std::fs::File::open(table) else {
            continue;
        };
        read_any = true;
        if socket_table_listens(std::io::BufReader::new(file), port) {
            return Some(true);
        }
    }
    read_any.then_some(false)
}

/// Whether a `/proc/net/tcp`-style table has a socket in LISTEN (`0A`) on
/// local `port`. Read line by line, stopping at the first one.
fn socket_table_listens(table: impl std::io::BufRead, port: u16) -> bool {
    table.lines().map_while(Result::ok).skip(1).any(|line| {
        let mut fields = line.split_whitespace();
        // `sl`, then `local_address`; then `rem_address`, then `st`.
        let local = fields.nth(1);
        let state = fields.nth(1);
        let local_port = local
            .and_then(|address| address.rsplit(':').next())
            .and_then(|hex| u16::from_str_radix(hex, 16).ok());
        state == Some("0A") && local_port == Some(port)
    })
}

/// Who holds `port`, given the probe addresses of Temps' own listeners,
/// whether its proxy proved it is running, and this machine's addresses
/// (`local_addresses`).
async fn probe_port(
    port: u16,
    temps_listeners: &[SocketAddr],
    proxy_identified: bool,
    local: &[SocketAddr],
) -> PortHolder {
    let temps_answered =
        any_answers(temps_listeners.iter().copied().filter(|l| l.port() == port)).await;
    let local_answered = !temps_answered
        && any_answers(local.iter().map(|target| {
            let mut target = *target;
            target.set_port(port);
            target
        }))
        .await;
    port_holder(temps_answered, proxy_identified, local_answered)
}

/// The first address on Tailscale's own interface among `interfaces`
/// (name, address, prefix length). Other overlays and carrier-grade NAT use
/// `100.64.0.0/10` too, so the range alone never says Tailscale runs.
pub fn tailnet_address<'a>(
    interfaces: impl IntoIterator<Item = (&'a str, IpAddr, u8)>,
) -> Option<IpAddr> {
    interfaces
        .into_iter()
        .find(|(name, ip, prefix)| {
            temps_core::private_network::is_tailscale_interface(name, *ip, *prefix)
        })
        .map(|(_, ip, _)| ip)
}

fn not_checked_inbound() -> InboundCheck {
    InboundCheck {
        checked: false,
        reachable: None,
        note: "Not checked: a machine cannot reliably test itself from outside. Open \
               http://<public address> from a phone off this network to check."
            .to_string(),
    }
}

/// Reads the facts. Holds its own clones of what it needs.
pub struct HostFactsService {
    platform_info: Arc<PlatformInfoService>,
    docker: Arc<DockerHandle>,
    /// Where Temps' proxy answers, and how it proves who it is.
    proxy: TempsProxy,
    /// Temps' data directory, whose disk is reported.
    data_dir: std::path::PathBuf,
}

impl HostFactsService {
    pub fn new(
        platform_info: Arc<PlatformInfoService>,
        docker: Arc<DockerHandle>,
        proxy: TempsProxy,
        data_dir: std::path::PathBuf,
    ) -> Self {
        Self {
            platform_info,
            docker,
            proxy,
            data_dir,
        }
    }

    pub async fn facts(&self) -> HostFacts {
        let (system, docker, ports, tailscale, public_ip, lan_ip) = tokio::join!(
            self.system(),
            self.docker(),
            self.ports(),
            self.tailscale(),
            self.platform_info.get_public_ip_with_fallback(),
            self.platform_info.get_private_ip_with_fallback(),
        );
        let (hostname, cpu_cores, memory, swap_total_bytes, disk) = system;
        HostFacts {
            hostname,
            cpu_cores,
            memory,
            swap_total_bytes,
            disk,
            docker,
            ports,
            lan_ip,
            public_ip,
            tailscale,
            inbound: not_checked_inbound(),
        }
    }

    async fn system(&self) -> (Option<String>, u32, MemoryFact, u64, Option<DiskFact>) {
        let data_dir = self.data_dir.clone();
        let read = tokio::task::spawn_blocking(move || {
            // Memory reads degrade to zero (shown as unknown) on a platform
            // sysinfo cannot read, rather than failing the whole screen.
            let (memory, swap) = match sysinfo::System::new() {
                Ok(mut system) => {
                    system.refresh_memory();
                    (
                        MemoryFact {
                            total_bytes: system.total_memory(),
                            available_bytes: system.available_memory(),
                        },
                        system.total_swap(),
                    )
                }
                Err(error) => {
                    debug!(%error, "Memory facts are unavailable on this platform");
                    (
                        MemoryFact {
                            total_bytes: 0,
                            available_bytes: 0,
                        },
                        0,
                    )
                }
            };
            let cores = std::thread::available_parallelism()
                .map(|n| n.get() as u32)
                .unwrap_or(1);
            let disk = temps_config::disk_status::disk_for_path(&data_dir).map(|d| DiskFact {
                mount_point: d.mount_point,
                total_bytes: d.total_bytes,
                available_bytes: d.available_bytes,
            });
            (sysinfo::System::host_name().ok(), cores, memory, swap, disk)
        })
        .await;
        match read {
            Ok(facts) => facts,
            Err(error) => {
                debug!(%error, "Reading system facts did not finish");
                (
                    None,
                    0,
                    MemoryFact {
                        total_bytes: 0,
                        available_bytes: 0,
                    },
                    0,
                    None,
                )
            }
        }
    }

    async fn docker(&self) -> DockerFact {
        let docker = match self.docker.require() {
            Ok(docker) => docker,
            Err(error) => {
                return DockerFact {
                    running: false,
                    version: None,
                    error: Some(error.to_string()),
                }
            }
        };
        match tokio::time::timeout(DOCKER_TIMEOUT, docker.version()).await {
            Ok(Ok(version)) => DockerFact {
                running: true,
                version: version.version,
                error: None,
            },
            Ok(Err(error)) => DockerFact {
                running: false,
                version: None,
                error: Some(format!("Docker did not answer: {error}")),
            },
            Err(_) => DockerFact {
                running: false,
                version: None,
                error: Some(format!(
                    "Docker did not answer within {}s",
                    DOCKER_TIMEOUT.as_secs()
                )),
            },
        }
    }

    async fn ports(&self) -> Vec<PortFact> {
        // Pingora binds every listener at startup or not at all, so a proxy
        // that proves itself on its HTTP listener holds its HTTPS one too.
        let identified = match self.proxy.http {
            Some(http) => proxy_identifies(http, &self.proxy.key).await,
            None => false,
        };
        let (local, complete) = tokio::task::spawn_blocking(local_probe_targets)
            .await
            .unwrap_or_else(|_| (vec![SocketAddr::from((Ipv4Addr::LOCALHOST, 0))], false));
        let listeners = &self.proxy.listeners;
        let (mut http, mut https) = tokio::join!(
            probe_port(80, listeners, identified, &local),
            probe_port(443, listeners, identified, &local),
        );
        if !complete {
            if http == PortHolder::Free {
                http = unprobed_holder(80).await;
            }
            if https == PortHolder::Free {
                https = unprobed_holder(443).await;
            }
        }
        vec![
            PortFact {
                port: 80,
                holder: http,
            },
            PortFact {
                port: 443,
                holder: https,
            },
        ]
    }

    async fn tailscale(&self) -> TailscaleFact {
        let interfaces: Vec<(String, IpAddr, u8)> = if_addrs::get_if_addrs()
            .map(|ifs| {
                ifs.into_iter()
                    .map(|i| {
                        let prefix = match &i.addr {
                            if_addrs::IfAddr::V4(v4) => u32::from(v4.netmask).count_ones(),
                            if_addrs::IfAddr::V6(v6) => u128::from(v6.netmask).count_ones(),
                        };
                        let ip = i.ip();
                        (i.name, ip, u8::try_from(prefix).unwrap_or(0))
                    })
                    .collect()
            })
            .unwrap_or_default();
        let Some(ip) = tailnet_address(
            interfaces
                .iter()
                .map(|(name, ip, prefix)| (name.as_str(), *ip, *prefix)),
        ) else {
            return TailscaleFact {
                running: false,
                ip: None,
                machine: None,
                tailnet: None,
                magic_dns: false,
                note: None,
            };
        };
        let name = magic_dns_name(ip).await;
        let (machine, tailnet) = name
            .as_deref()
            .and_then(split_tailnet_name)
            .map_or((None, None), |(m, t)| (Some(m), Some(t)));
        TailscaleFact {
            running: true,
            ip: Some(ip.to_string()),
            magic_dns: machine.is_some(),
            note: Some(if machine.is_some() {
                "HTTPS certificates are not read: that needs tailscaled's LocalAPI, which Temps \
                 opens only with your consent."
                    .to_string()
            } else {
                "MagicDNS did not answer for this machine, so its tailnet name is unknown. Turn \
                 on MagicDNS in the tailnet's DNS settings."
                    .to_string()
            }),
            machine,
            tailnet,
        }
    }
}

/// The MagicDNS name of `ip`, asked of Tailscale's own resolver.
async fn magic_dns_name(ip: IpAddr) -> Option<String> {
    use hickory_resolver::config::{NameServerConfig, ResolverConfig, ResolverOpts};
    use hickory_resolver::net::runtime::TokioRuntimeProvider;

    let mut config = ResolverConfig::default();
    config.add_name_server(NameServerConfig::udp_and_tcp(MAGIC_DNS_RESOLVER));
    let mut opts = ResolverOpts::default();
    opts.timeout = MAGIC_DNS_TIMEOUT;
    opts.attempts = 1;
    let resolver =
        hickory_resolver::Resolver::builder_with_config(config, TokioRuntimeProvider::default())
            .with_options(opts)
            .build()
            .ok()?;
    let lookup = tokio::time::timeout(MAGIC_DNS_TIMEOUT * 2, resolver.reverse_lookup(ip))
        .await
        .ok()?
        .ok()?;
    lookup
        .answers()
        .iter()
        .find_map(|record| match &record.data {
            hickory_resolver::proto::rr::RData::PTR(ptr) => Some(ptr.0.to_ascii()),
            _ => None,
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tailnet_names_split_into_machine_and_tailnet() {
        assert_eq!(
            split_tailnet_name("box.example.ts.net."),
            Some(("box".to_string(), "example.ts.net".to_string()))
        );
        assert_eq!(
            split_tailnet_name("Box.Example.TS.NET"),
            Some(("box".to_string(), "example.ts.net".to_string()))
        );
        assert_eq!(split_tailnet_name("box.example.com"), None);
        assert_eq!(split_tailnet_name("box"), None);
        assert_eq!(split_tailnet_name(".example.ts.net"), None);
    }

    #[test]
    fn listener_ports_are_read_from_configured_addresses() {
        assert_eq!(listener_port("0.0.0.0:80"), Some(80));
        assert_eq!(listener_port("[::]:443"), Some(443));
        assert_eq!(listener_port("localhost:8080"), Some(8080));
        assert_eq!(listener_port("not-an-address"), None);
    }

    #[test]
    fn a_listener_is_probed_at_its_own_address() {
        let probe = |a: &str| listener_probe_address(a).map(|a| a.to_string());
        assert_eq!(probe("0.0.0.0:443").as_deref(), Some("127.0.0.1:443"));
        assert_eq!(probe("[::]:443").as_deref(), Some("[::1]:443"));
        assert_eq!(
            probe("192.168.1.20:443").as_deref(),
            Some("192.168.1.20:443")
        );
        assert_eq!(probe("10.8.0.1:443").as_deref(), Some("10.8.0.1:443"));
        assert_eq!(probe("[fd00::1]:443").as_deref(), Some("[fd00::1]:443"));
        assert_eq!(probe("localhost:80").as_deref(), Some("127.0.0.1:80"));
        assert_eq!(probe("not-an-address"), None);
    }

    #[test]
    fn a_port_is_temps_only_while_temps_answers_on_it() {
        assert_eq!(port_holder(true, true, false), PortHolder::Temps);
        assert_eq!(port_holder(true, true, true), PortHolder::Temps);
        // Something answers at the proxy's address, but the proxy did not
        // prove it is running: another server took the port.
        assert_eq!(port_holder(true, false, false), PortHolder::Other);
        assert_eq!(port_holder(false, true, true), PortHolder::Other);
        // Configured, but its separate proxy process is stopped: nothing
        // serves the port, so it is not reported as Temps listening.
        assert_eq!(port_holder(false, false, false), PortHolder::Free);
    }

    /// Only a socket in LISTEN holds a port: one in TIME_WAIT (`06`) or an
    /// established connection (`01`) from a stopped server does not.
    #[test]
    fn the_socket_tables_name_only_listeners() {
        let table = "  sl  local_address rem_address   st tx_queue rx_queue tr tm->when retrnsmt   uid  timeout inode
   0: 0100007F:0050 00000000:0000 0A 00000000:00000000 00:00000000 00000000     0        0 1 1 0 100 0 0 10 0
   1: 0A00000F:01BB 0A000020:D431 06 00000000:00000000 03:00000F1A 00000000     0        0 0 3 0
   2: 0A00000F:01BB 0A000021:D432 01 00000000:00000000 00:00000000 00000000     0        0 2 1 0 20 4 30 10 -1
";
        // 0x50 is 80, 0x1BB is 443.
        assert!(socket_table_listens(table.as_bytes(), 80));
        assert!(!socket_table_listens(table.as_bytes(), 443));
        assert!(!socket_table_listens(table.as_bytes(), 8080));
    }

    /// A listener on any local address holds its port even where that
    /// address was not probed; once it stops, nothing does. Linux reads its
    /// socket tables, elsewhere a bind on every address tells.
    #[tokio::test]
    async fn a_listener_on_an_unprobed_address_holds_its_port() {
        let listener = std::net::TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
        let port = listener.local_addr().unwrap().port();
        assert_eq!(unprobed_holder(port).await, PortHolder::Other);
        drop(listener);
        assert_eq!(unprobed_holder(port).await, PortHolder::Free);
    }

    /// Without socket tables, a bind on every address finds a listener on
    /// one address only, and none once it stopped.
    #[test]
    fn a_bind_on_every_address_finds_a_listener_on_one() {
        let listener = std::net::TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
        let port = listener.local_addr().unwrap().port();
        assert_eq!(wildcard_bind_holder(port), PortHolder::Other);
        drop(listener);
        assert_eq!(wildcard_bind_holder(port), PortHolder::Free);
    }

    /// A minimal HTTP server on loopback that answers every request with
    /// `answer`, for the identity probe.
    async fn http_server(answer: &'static str) -> SocketAddr {
        let listener = tokio::net::TcpListener::bind((Ipv4Addr::LOCALHOST, 0))
            .await
            .unwrap();
        let address = listener.local_addr().unwrap();
        tokio::spawn(async move {
            use tokio::io::{AsyncReadExt, AsyncWriteExt};
            while let Ok((mut stream, _)) = listener.accept().await {
                let mut buffer = [0u8; 4096];
                let _ = stream.read(&mut buffer).await;
                let _ = stream.write_all(answer.as_bytes()).await;
            }
        });
        address
    }

    /// A server holding the installation key answers the challenge, as
    /// Temps' proxy does, so its port is Temps'. It checks the probe was
    /// signed for the identity host, as the proxy does.
    #[tokio::test]
    async fn the_proxy_that_answers_the_challenge_is_temps() {
        use temps_core::probe_auth;
        const KEY: &str = "test-installation-key";
        let listener = tokio::net::TcpListener::bind((Ipv4Addr::LOCALHOST, 0))
            .await
            .unwrap();
        let address = listener.local_addr().unwrap();
        tokio::spawn(async move {
            use tokio::io::{AsyncReadExt, AsyncWriteExt};
            while let Ok((mut stream, _)) = listener.accept().await {
                let mut buffer = vec![0u8; 8192];
                let read = stream.read(&mut buffer).await.unwrap_or(0);
                let request = String::from_utf8_lossy(&buffer[..read]).to_string();
                let header = |name: &str| {
                    request.lines().find_map(|line| {
                        let (n, v) = line.split_once(':')?;
                        n.trim()
                            .eq_ignore_ascii_case(name)
                            .then(|| v.trim().to_string())
                    })
                };
                let signed = header(probe_auth::HEADER).is_some_and(|token| {
                    probe_auth::verify(
                        KEY,
                        probe_auth::PROXY_IDENTITY_HOST,
                        &token,
                        probe_auth::now(),
                    )
                }) && header("host").as_deref()
                    == Some(probe_auth::PROXY_IDENTITY_HOST);
                let answer = header(probe_auth::PROXY_IDENTITY_NONCE_HEADER)
                    .filter(|_| signed)
                    .and_then(|nonce| probe_auth::identity_answer(KEY, &nonce));
                let reply = match answer {
                    Some(answer) => format!(
                        "HTTP/1.1 204 No Content\r\n{}: {answer}\r\n\r\n",
                        probe_auth::PROXY_IDENTITY_HEADER
                    ),
                    None => "HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\n\r\n".into(),
                };
                let _ = stream.write_all(reply.as_bytes()).await;
            }
        });
        assert!(proxy_identifies(address, KEY).await);
        // Asked with another installation's key, the answer does not verify.
        assert!(!proxy_identifies(address, "another-installation-key").await);
    }

    /// Another server on the stopped proxy's address answers the
    /// connection, and even a made-up identity header, but cannot answer the
    /// challenge: its port is someone else's.
    #[tokio::test]
    async fn a_server_on_the_proxys_address_is_not_taken_for_temps() {
        for answer in [
            "HTTP/1.1 200 OK\r\nContent-Length: 0\r\n\r\n",
            "HTTP/1.1 204 No Content\r\nx-temps-proxy-identity: 00ff\r\n\r\n",
        ] {
            let other = http_server(answer).await;
            assert!(!proxy_identifies(other, "test-installation-key").await);
            assert_eq!(
                probe_port(other.port(), &[other], false, &[]).await,
                PortHolder::Other
            );
        }
        // Nothing listening at all.
        let closed = {
            let listener = tokio::net::TcpListener::bind((Ipv4Addr::LOCALHOST, 0))
                .await
                .unwrap();
            listener.local_addr().unwrap()
        };
        assert!(!proxy_identifies(closed, "test-installation-key").await);
    }

    /// A proxy bound to one address answers there, not on loopback; probing
    /// loopback alone called its port free. Another server bound only to a
    /// LAN address holds the port too.
    #[tokio::test]
    async fn a_proxy_bound_to_one_address_holds_its_port() {
        let lan = if_addrs::get_if_addrs().ok().and_then(|ifs| {
            ifs.into_iter()
                .map(|i| i.ip())
                .find(|ip| ip.is_ipv4() && !ip.is_loopback())
        });
        let Some(lan) = lan else {
            println!("no non-loopback IPv4 address, skipping");
            return;
        };
        let listener = tokio::net::TcpListener::bind((lan, 0)).await.unwrap();
        let bound = listener.local_addr().unwrap();
        let port = bound.port();
        // Nothing on loopback answers for that port.
        assert!(!answers(SocketAddr::from((Ipv4Addr::LOCALHOST, port))).await);
        let local = local_addresses();
        assert!(local.iter().any(|t| t.ip() == lan), "{local:?} lists {lan}");
        let configured = listener_probe_address(&bound.to_string()).unwrap();
        assert_eq!(
            probe_port(port, &[configured], true, &local).await,
            PortHolder::Temps
        );
        // Someone else's listener on the LAN address only: the port is taken.
        assert_eq!(probe_port(port, &[], true, &local).await, PortHolder::Other);
        let unspecified = listener_probe_address(&format!("0.0.0.0:{port}")).unwrap();
        assert_eq!(
            probe_port(port, &[unspecified], true, &local).await,
            PortHolder::Other
        );
        // Loopback alone does not see it, which is why every address is probed.
        assert_eq!(
            probe_port(
                port,
                &[],
                true,
                &[SocketAddr::from((Ipv4Addr::LOCALHOST, 0))]
            )
            .await,
            PortHolder::Free
        );
        drop(listener);
        assert_eq!(
            probe_port(port, &[configured], true, &local).await,
            PortHolder::Free
        );
    }

    #[tokio::test]
    async fn a_loopback_listener_is_temps_when_configured_and_other_when_not() {
        let listener = tokio::net::TcpListener::bind((Ipv4Addr::LOCALHOST, 0))
            .await
            .unwrap();
        let port = listener.local_addr().unwrap().port();
        let all = listener_probe_address(&format!("0.0.0.0:{port}")).unwrap();
        let local = local_addresses();
        assert_eq!(
            probe_port(port, &[all], true, &local).await,
            PortHolder::Temps
        );
        assert_eq!(
            probe_port(port, &[all], false, &local).await,
            PortHolder::Other
        );
        assert_eq!(probe_port(port, &[], true, &local).await, PortHolder::Other);
    }

    /// The probe list holds each address once, never more than the cap,
    /// and IPv6 link-local addresses with their interface's scope.
    #[test]
    fn local_addresses_are_listed_once_with_their_scope() {
        let local = local_addresses();
        assert!(local.len() <= MAX_PROBED_ADDRESSES);
        let distinct: std::collections::HashSet<_> = local.iter().collect();
        assert_eq!(distinct.len(), local.len());
        assert!(local.contains(&SocketAddr::from((Ipv4Addr::LOCALHOST, 0))));
        let link_local: std::net::Ipv6Addr = "fe80::1".parse().unwrap();
        assert_eq!(
            probe_target(IpAddr::V6(link_local), Some(4)),
            SocketAddr::V6(std::net::SocketAddrV6::new(link_local, 0, 0, 4))
        );
    }

    #[test]
    fn link_local_addresses_are_read_with_their_interface_index() {
        let listing = "\
fe800000000000000a0027fffe4e1c2d 02 40 20 80     eth0
00000000000000000000000000000001 01 80 10 80       lo
fd7a115ca1e000000000000000000001 0a 80 00 80 tailscale0
not-an-address 03 40 20 80 eth1
";
        let ip: std::net::Ipv6Addr = "fe80::a00:27ff:fe4e:1c2d".parse().unwrap();
        assert_eq!(
            link_local_from_if_inet6(listing),
            vec![SocketAddr::V6(std::net::SocketAddrV6::new(ip, 0, 0, 2))]
        );
    }

    /// A server listening only on an interface's IPv6 link-local address
    /// holds its port: dropping those addresses called it free.
    #[tokio::test]
    async fn a_server_on_a_link_local_address_only_holds_its_port() {
        let Some(target) = local_addresses().into_iter().find(|t| {
            matches!(t, SocketAddr::V6(v6) if v6.ip().is_unicast_link_local() && v6.scope_id() != 0)
        }) else {
            println!("no scoped IPv6 link-local address, skipping");
            return;
        };
        let Ok(listener) = tokio::net::TcpListener::bind(target).await else {
            println!("cannot listen on {target}, skipping");
            return;
        };
        let port = listener.local_addr().unwrap().port();
        assert_eq!(
            probe_port(port, &[], true, &local_addresses()).await,
            PortHolder::Other
        );
    }

    /// A server listening only on IPv6 holds its port: probing IPv4 alone
    /// called it free.
    #[tokio::test]
    async fn a_server_on_ipv6_only_holds_its_port() {
        let Ok(listener) = tokio::net::TcpListener::bind((std::net::Ipv6Addr::LOCALHOST, 0)).await
        else {
            println!("no IPv6 loopback, skipping");
            return;
        };
        let port = listener.local_addr().unwrap().port();
        assert_eq!(
            probe_port(
                port,
                &[],
                true,
                &[SocketAddr::from((Ipv4Addr::LOCALHOST, 0))]
            )
            .await,
            PortHolder::Free
        );
        assert_eq!(
            probe_port(port, &[], true, &local_addresses()).await,
            PortHolder::Other
        );
    }

    #[test]
    fn only_tailscales_own_interface_says_tailscale_runs() {
        let lan: IpAddr = "192.168.1.20".parse().unwrap();
        let tailnet: IpAddr = "100.101.102.103".parse().unwrap();
        let cgnat_wan: IpAddr = "100.72.4.9".parse().unwrap();
        let netbird: IpAddr = "100.92.10.4".parse().unwrap();
        // A carrier-grade NAT WAN and NetBird use 100.64.0.0/10 as well.
        assert_eq!(
            tailnet_address([
                ("eth0", lan, 24),
                ("eth1", cgnat_wan, 10),
                ("wt0", netbird, 16),
                ("tailscale0", tailnet, 32),
            ]),
            Some(tailnet)
        );
        assert_eq!(
            tailnet_address([("eth1", cgnat_wan, 10), ("wt0", netbird, 16)]),
            None
        );
        // macOS: a utun host route in Tailscale's range.
        assert_eq!(tailnet_address([("utun4", tailnet, 32)]), Some(tailnet));
    }

    #[test]
    fn the_inbound_check_says_it_was_not_checked_and_how_to() {
        let inbound = not_checked_inbound();
        assert!(!inbound.checked);
        assert_eq!(inbound.reachable, None);
        assert!(inbound.note.contains("phone"));
    }

    #[tokio::test]
    async fn facts_without_docker_say_why() {
        let platform = Arc::new(PlatformInfoService::without_docker());
        let service = HostFactsService::new(
            platform,
            Arc::new(DockerHandle::disabled(
                "control-plane",
                "this process runs without Docker",
            )),
            TempsProxy {
                listeners: vec![SocketAddr::from((Ipv4Addr::LOCALHOST, 8080))],
                http: Some(SocketAddr::from((Ipv4Addr::LOCALHOST, 8080))),
                key: "test-installation-key".into(),
            },
            std::env::temp_dir(),
        );
        let docker = service.docker().await;
        assert!(!docker.running);
        assert!(docker.error.is_some());
        let ports = service.ports().await;
        assert_eq!(
            ports.iter().map(|p| p.port).collect::<Vec<_>>(),
            vec![80, 443]
        );
    }
}
