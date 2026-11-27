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
    /// Temps' own proxy is configured to listen on it, and it answers.
    /// Configured but not answering (a separate proxy process stopped) is
    /// `Free`: nothing serves it.
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
/// answered at its own address, someone else when only local loopback
/// answered, and nobody when nothing did (a configured proxy that is stopped
/// serves nothing).
pub fn port_holder(temps_answered: bool, loopback_answered: bool) -> PortHolder {
    match (temps_answered, loopback_answered) {
        (true, _) => PortHolder::Temps,
        (false, true) => PortHolder::Other,
        (false, false) => PortHolder::Free,
    }
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

/// Who holds `port`, given the probe addresses of Temps' own listeners.
async fn probe_port(port: u16, temps_listeners: &[SocketAddr]) -> PortHolder {
    let mut temps_answered = false;
    for listener in temps_listeners.iter().filter(|l| l.port() == port) {
        if answers(*listener).await {
            temps_answered = true;
            break;
        }
    }
    let loopback_answered =
        !temps_answered && answers(SocketAddr::from((Ipv4Addr::LOCALHOST, port))).await;
    port_holder(temps_answered, loopback_answered)
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
    /// Where Temps' proxy listeners answer (`listener_probe_address`).
    temps_listeners: Vec<SocketAddr>,
    /// Temps' data directory, whose disk is reported.
    data_dir: std::path::PathBuf,
}

impl HostFactsService {
    pub fn new(
        platform_info: Arc<PlatformInfoService>,
        docker: Arc<DockerHandle>,
        temps_listeners: Vec<SocketAddr>,
        data_dir: std::path::PathBuf,
    ) -> Self {
        Self {
            platform_info,
            docker,
            temps_listeners,
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
        let mut facts = Vec::with_capacity(2);
        for port in [80u16, 443] {
            facts.push(PortFact {
                port,
                holder: probe_port(port, &self.temps_listeners).await,
            });
        }
        facts
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
        assert_eq!(port_holder(true, false), PortHolder::Temps);
        assert_eq!(port_holder(true, true), PortHolder::Temps);
        assert_eq!(port_holder(false, true), PortHolder::Other);
        // Configured, but its separate proxy process is stopped: nothing
        // serves the port, so it is not reported as Temps listening.
        assert_eq!(port_holder(false, false), PortHolder::Free);
    }

    /// A proxy bound to one address answers there, not on loopback; probing
    /// loopback alone called its port free.
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
        let configured = listener_probe_address(&bound.to_string()).unwrap();
        assert_eq!(probe_port(port, &[configured]).await, PortHolder::Temps);
        // Someone else's listener: not Temps'.
        assert_eq!(probe_port(port, &[]).await, PortHolder::Free);
        let unspecified = listener_probe_address(&format!("0.0.0.0:{port}")).unwrap();
        assert_eq!(probe_port(port, &[unspecified]).await, PortHolder::Free);
        drop(listener);
        assert_eq!(probe_port(port, &[configured]).await, PortHolder::Free);
    }

    #[tokio::test]
    async fn a_loopback_listener_is_temps_when_configured_and_other_when_not() {
        let listener = tokio::net::TcpListener::bind((Ipv4Addr::LOCALHOST, 0))
            .await
            .unwrap();
        let port = listener.local_addr().unwrap().port();
        let all = listener_probe_address(&format!("0.0.0.0:{port}")).unwrap();
        assert_eq!(probe_port(port, &[all]).await, PortHolder::Temps);
        assert_eq!(probe_port(port, &[]).await, PortHolder::Other);
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
            vec![SocketAddr::from((Ipv4Addr::LOCALHOST, 8080))],
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
