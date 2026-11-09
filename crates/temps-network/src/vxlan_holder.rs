// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Who holds a VXLAN network ID on this host, and what to do about it
//! (ADR 062).
//!
//! The kernel allows one VXLAN device per network ID (VNI) and UDP port. When
//! another device already holds the pair this cluster's overlay needs, the
//! usual holder is another cluster's overlay left on the host: its
//! `vxlan-<name>` device, the `br-<name>` bridge it is enslaved to and the
//! Docker network `<name>` with containers still attached (ADR 059 naming).
//! [`inspect`] finds those, best-effort, and [`describe`] turns them into a
//! message that names them and gives the exact commands, plus the
//! alternative that needs no shell on the host: giving this cluster its own
//! network ID.
//!
//! Parsing and wording are pure and shared by the agent, the control plane
//! and `temps join`'s preflight; only [`inspect`] and [`list_devices`] touch
//! the host.

use std::collections::HashMap;
use std::process::Stdio;
use std::time::Duration;

use bollard::query_parameters::{InspectNetworkOptions, ListContainersOptions};
use ipnet::Ipv4Net;

/// The phrase every "network ID in use" message is built around. The control
/// plane looks for it in a node's reported overlay error to offer a new
/// network ID; see [`reports_id_in_use`].
pub const ID_IN_USE_PHRASE: &str = "is already used on this host by";

/// The phrase agents older than ADR 062 used for the same failure.
const LEGACY_ID_IN_USE_PHRASE: &str = "already holds vni=";

/// Containers listed by name before the message switches to a filter.
const MAX_LISTED_CONTAINERS: usize = 10;

/// Longest a Docker lookup may take; the message is still built without it.
const DOCKER_TIMEOUT: Duration = Duration::from_secs(5);

/// Whether an overlay error says the VXLAN network ID is held by another
/// device on the node, in this version's wording or an older agent's.
pub fn reports_id_in_use(message: &str) -> bool {
    message.contains(ID_IN_USE_PHRASE) || message.contains(LEGACY_ID_IN_USE_PHRASE)
}

/// One VXLAN device on the host.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VxlanDevice {
    pub name: String,
    pub vni: u32,
    pub port: u16,
}

/// The VXLAN devices in `ip -d -o link show type vxlan` output.
pub fn parse_devices(output: &str) -> Vec<VxlanDevice> {
    output
        .lines()
        .filter_map(|line| {
            let tokens: Vec<&str> = line.split_whitespace().collect();
            // "11: vxlan-temps0: <...>" or "11: vx0@eth0: <...>".
            let device = tokens.get(1)?.trim_end_matches(':');
            let device = device.split('@').next().unwrap_or(device);
            let topology = &tokens[tokens.iter().position(|token| *token == "vxlan")?..];
            let value = |key: &str| {
                topology
                    .windows(2)
                    .find(|pair| pair[0] == key)
                    .map(|pair| pair[1])
            };
            Some(VxlanDevice {
                name: device.to_string(),
                vni: value("id")?.parse().ok()?,
                // `ip` omits dstport for the kernel's historic default.
                port: value("dstport").map_or(Some(0), |port| port.parse().ok())?,
            })
        })
        .collect()
}

/// The device other than `except` that holds `vni` on `port`.
pub fn holder<'a>(
    devices: &'a [VxlanDevice],
    except: &str,
    vni: u32,
    port: u16,
) -> Option<&'a VxlanDevice> {
    devices
        .iter()
        .find(|device| device.name != except && device.vni == vni && device.port == port)
}

/// Every VXLAN device on this host. Linux only; elsewhere there are none.
pub async fn list_devices() -> Result<Vec<VxlanDevice>, String> {
    if !cfg!(target_os = "linux") {
        return Ok(Vec::new());
    }
    let output = tokio::process::Command::new("ip")
        .args(["-d", "-o", "link", "show", "type", "vxlan"])
        .stdin(Stdio::null())
        .output()
        .await
        .map_err(|error| format!("list VXLAN devices: {error}"))?;
    if !output.status.success() {
        return Err(format!(
            "list VXLAN devices: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        ));
    }
    Ok(parse_devices(&String::from_utf8_lossy(&output.stdout)))
}

/// The Docker network of the holder's overlay, as far as it could be read.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DockerLookup {
    /// Docker could not be asked (no socket, no permission, timed out).
    Unavailable(String),
    /// Docker answered: no network of that name.
    NoNetwork,
    /// The network, its subnets and every container attached (any state).
    Network {
        name: String,
        subnets: Vec<String>,
        containers: Vec<String>,
    },
}

/// What is known about the device holding the network ID.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HolderFacts {
    pub device: String,
    /// The overlay network name when the device is `vxlan-<name>`.
    pub overlay: Option<String>,
    /// The overlay's bridge (`br-<name>`) when it exists, and its IPv4
    /// subnets.
    pub bridge: Option<(String, Vec<String>)>,
    /// `None` when the device is not an overlay's, so there is no network to
    /// look up.
    pub docker: Option<DockerLookup>,
}

impl HolderFacts {
    /// Only the device is known.
    pub fn device_only(device: &str) -> Self {
        Self {
            device: device.to_string(),
            overlay: overlay_of(device),
            bridge: None,
            docker: None,
        }
    }
}

/// The overlay network name of a `vxlan-<name>` device.
fn overlay_of(device: &str) -> Option<String> {
    let name = device.strip_prefix("vxlan-")?;
    crate::overlay_name::validate_overlay_network_name(name)
        .is_ok()
        .then(|| name.to_string())
}

/// Names that can go into a copied shell command as they are.
fn shell_safe(name: &str) -> bool {
    !name.is_empty()
        && !name.starts_with('-')
        && name
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'-' | b'@'))
}

/// The message for `facts`: who holds `vni`/`port`, what is attached, the
/// commands that remove it when its cluster is gone, and the alternative.
/// `wanted` is the device this cluster's overlay needs.
pub fn describe(facts: &HolderFacts, wanted: &str, vni: u32, port: u16) -> String {
    let holder = &facts.device;
    let mut message = format!(
        "VXLAN network ID {vni} (UDP port {port}) {ID_IN_USE_PHRASE} '{holder}', so this \
         cluster's overlay ('{wanted}') cannot use it."
    );
    let alternative = "If it still runs, give this cluster its own network ID in Settings → \
                       Worker Nodes.";
    let Some(overlay) = facts.overlay.as_deref() else {
        let delete = if shell_safe(holder) {
            format!(" (`ip link delete {holder}`)")
        } else {
            String::new()
        };
        message.push_str(&format!(
            " '{holder}' is not a Temps overlay device. If nothing on this host needs it, delete \
             it{delete}. Otherwise give this cluster its own network ID in Settings → Worker \
             Nodes."
        ));
        return message;
    };

    let mut steps: Vec<String> = Vec::new();
    let bridge_subnets = facts
        .bridge
        .as_ref()
        .map(|(_, subnets)| subnets.clone())
        .unwrap_or_default();
    match &facts.docker {
        Some(DockerLookup::Network {
            name,
            subnets,
            containers,
        }) => {
            let subnets = if subnets.is_empty() {
                &bridge_subnets
            } else {
                subnets
            };
            let place = if subnets.is_empty() {
                format!("Docker network '{name}'")
            } else {
                format!("Docker network '{name}', {}", subnets.join(", "))
            };
            message.push_str(&format!(
                " '{holder}' belongs to another cluster's overlay ({place});"
            ));
            match containers.len() {
                0 => message.push_str(" no containers are attached to it."),
                count if count <= MAX_LISTED_CONTAINERS => message.push_str(&format!(
                    " {} {} still attached.",
                    if count == 1 {
                        "container"
                    } else {
                        "containers"
                    },
                    containers.join(", ") + if count == 1 { " is" } else { " are" },
                )),
                count => message.push_str(&format!(
                    " {count} containers are still attached ({}, ...).",
                    containers[..MAX_LISTED_CONTAINERS / 2].join(", ")
                )),
            }
            if !containers.is_empty() && shell_safe(name) {
                if containers.len() <= MAX_LISTED_CONTAINERS
                    && containers.iter().all(|container| shell_safe(container))
                {
                    steps.push(format!("docker rm -f {}", containers.join(" ")));
                } else {
                    steps.push(format!(
                        "docker rm -f $(docker ps -aq --filter network={name})"
                    ));
                }
            }
            if shell_safe(name) {
                steps.push(format!("docker network rm {name}"));
            }
        }
        lookup => {
            let place = if bridge_subnets.is_empty() {
                String::new()
            } else {
                format!(" (bridge subnets {})", bridge_subnets.join(", "))
            };
            message.push_str(&format!(
                " '{holder}' belongs to another cluster's overlay '{overlay}'{place}."
            ));
            match lookup {
                Some(DockerLookup::Unavailable(reason)) => message.push_str(&format!(
                    " Its Docker network '{overlay}' could not be checked ({reason}); remove its \
                     containers and the network first if it exists (`docker network inspect \
                     {overlay}`)."
                )),
                Some(DockerLookup::NoNetwork) => {
                    message.push_str(&format!(" No Docker network '{overlay}' is left."))
                }
                _ => {}
            }
        }
    }
    if shell_safe(holder) {
        steps.push(format!("ip link delete {holder}"));
    }
    if let Some((bridge, _)) = &facts.bridge {
        if shell_safe(bridge) {
            steps.push(format!("ip link delete {bridge}"));
        }
    }
    if steps.is_empty() {
        message.push_str(" If that cluster is gone, remove its devices.");
    } else {
        message.push_str(&format!(
            " If that cluster is gone, remove it: `{}`.",
            steps.join(" && ")
        ));
    }
    message.push(' ');
    message.push_str(alternative);
    message
}

/// Find out what `device` belongs to. Never fails: what cannot be read is
/// left out, and [`describe`] says so.
pub async fn inspect(device: &str) -> HolderFacts {
    let mut facts = HolderFacts::device_only(device);
    let Some(overlay) = facts.overlay.clone() else {
        return facts;
    };
    let bridge = format!("br-{overlay}");
    facts.bridge = bridge_subnets(&bridge)
        .await
        .map(|subnets| (bridge, subnets));
    facts.docker = Some(docker_lookup(&overlay).await);
    facts
}

/// The holder's message, inspected on this host.
pub async fn diagnose(device: &str, wanted: &str, vni: u32, port: u16) -> String {
    describe(&inspect(device).await, wanted, vni, port)
}

/// The IPv4 subnets on `bridge`, or `None` when it does not exist.
async fn bridge_subnets(bridge: &str) -> Option<Vec<String>> {
    if !cfg!(target_os = "linux") {
        return None;
    }
    let output = tokio::process::Command::new("ip")
        .args(["-o", "-4", "addr", "show", "dev", bridge])
        .stdin(Stdio::null())
        .output()
        .await
        .ok()?;
    if !output.status.success() {
        return None;
    }
    Some(parse_inet_subnets(&String::from_utf8_lossy(&output.stdout)))
}

/// The networks of the `inet` entries in `ip -o -4 addr show` output.
fn parse_inet_subnets(output: &str) -> Vec<String> {
    let mut subnets: Vec<String> = output
        .lines()
        .filter_map(|line| {
            let tokens: Vec<&str> = line.split_whitespace().collect();
            let address = tokens.windows(2).find(|pair| pair[0] == "inet")?[1];
            address
                .parse::<Ipv4Net>()
                .ok()
                .map(|net| net.trunc().to_string())
        })
        .collect();
    subnets.dedup();
    subnets
}

async fn docker_lookup(network: &str) -> DockerLookup {
    let docker = match bollard::Docker::connect_with_local_defaults() {
        Ok(docker) => docker,
        Err(error) => return DockerLookup::Unavailable(format!("no Docker connection: {error}")),
    };
    let inspected = tokio::time::timeout(
        DOCKER_TIMEOUT,
        docker.inspect_network(network, None::<InspectNetworkOptions>),
    )
    .await;
    let inspected = match inspected {
        Err(_) => return DockerLookup::Unavailable("Docker did not answer in time".into()),
        Ok(Err(bollard::errors::Error::DockerResponseServerError {
            status_code: 404, ..
        })) => return DockerLookup::NoNetwork,
        Ok(Err(error)) => return DockerLookup::Unavailable(error.to_string()),
        Ok(Ok(inspected)) => inspected,
    };
    let subnets = inspected
        .ipam
        .and_then(|ipam| ipam.config)
        .unwrap_or_default()
        .into_iter()
        .filter_map(|config| config.subnet)
        .collect();
    // Every container on it, stopped ones included: `docker network rm`
    // needs them gone, and the network's own listing shows running ones only.
    let listed = tokio::time::timeout(
        DOCKER_TIMEOUT,
        docker.list_containers(Some(ListContainersOptions {
            all: true,
            filters: Some(HashMap::from([(
                "network".to_string(),
                vec![network.to_string()],
            )])),
            ..Default::default()
        })),
    )
    .await;
    let containers = match listed {
        Ok(Ok(containers)) => {
            let mut names: Vec<String> = containers
                .into_iter()
                .filter_map(|container| {
                    container
                        .names
                        .and_then(|names| names.into_iter().next())
                        .map(|name| name.trim_start_matches('/').to_string())
                        .or(container.id)
                })
                .collect();
            names.sort();
            names
        }
        Ok(Err(error)) => return DockerLookup::Unavailable(error.to_string()),
        Err(_) => return DockerLookup::Unavailable("Docker did not answer in time".into()),
    };
    DockerLookup::Network {
        name: network.to_string(),
        subnets,
        containers,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn leftover(containers: &[&str]) -> HolderFacts {
        HolderFacts {
            device: "vxlan-temps0".into(),
            overlay: Some("temps0".into()),
            bridge: Some(("br-temps0".into(), vec!["10.202.0.0/24".into()])),
            docker: Some(DockerLookup::Network {
                name: "temps0".into(),
                subnets: vec!["10.202.0.0/24".into()],
                containers: containers.iter().map(|c| c.to_string()).collect(),
            }),
        }
    }

    #[test]
    fn a_leftover_overlay_is_named_with_its_containers_and_the_commands() {
        let message = describe(&leftover(&["web-1", "worker-1"]), "vxlan-temps1", 42, 4789);
        assert_eq!(
            message,
            "VXLAN network ID 42 (UDP port 4789) is already used on this host by \
             'vxlan-temps0', so this cluster's overlay ('vxlan-temps1') cannot use it. \
             'vxlan-temps0' belongs to another cluster's overlay (Docker network 'temps0', \
             10.202.0.0/24); containers web-1, worker-1 are still attached. If that cluster is \
             gone, remove it: `docker rm -f web-1 worker-1 && docker network rm temps0 && ip \
             link delete vxlan-temps0 && ip link delete br-temps0`. If it still runs, give this \
             cluster its own network ID in Settings → Worker Nodes."
        );
        assert!(reports_id_in_use(&message));
    }

    #[test]
    fn no_containers_and_many_containers_read_right() {
        let empty = describe(&leftover(&[]), "vxlan-temps1", 42, 4789);
        assert!(
            empty.contains("no containers are attached to it"),
            "{empty}"
        );
        assert!(
            empty.contains("`docker network rm temps0 && ip link delete vxlan-temps0"),
            "{empty}"
        );

        let one = describe(&leftover(&["web-1"]), "vxlan-temps1", 42, 4789);
        assert!(one.contains("container web-1 is still attached"), "{one}");

        let names: Vec<String> = (0..12).map(|i| format!("app-{i}")).collect();
        let refs: Vec<&str> = names.iter().map(String::as_str).collect();
        let many = describe(&leftover(&refs), "vxlan-temps1", 42, 4789);
        assert!(many.contains("12 containers are still attached"), "{many}");
        assert!(
            many.contains("docker rm -f $(docker ps -aq --filter network=temps0)"),
            "{many}"
        );
    }

    #[test]
    fn without_docker_the_device_facts_and_the_reason_are_given() {
        let facts = HolderFacts {
            docker: Some(DockerLookup::Unavailable("permission denied".into())),
            ..leftover(&[])
        };
        let message = describe(&facts, "vxlan-temps1", 42, 4789);
        assert!(
            message.contains("overlay 'temps0' (bridge subnets 10.202.0.0/24)"),
            "{message}"
        );
        assert!(
            message.contains("could not be checked (permission denied)"),
            "{message}"
        );
        assert!(
            message.contains("`ip link delete vxlan-temps0 && ip link delete br-temps0`"),
            "{message}"
        );
        assert!(message.contains("its own network ID"), "{message}");

        let bare = describe(
            &HolderFacts::device_only("vxlan-temps0"),
            "vxlan-temps1",
            42,
            4789,
        );
        assert!(bare.contains("`ip link delete vxlan-temps0`"), "{bare}");
    }

    #[test]
    fn a_foreign_device_is_not_called_an_overlay() {
        let message = describe(&HolderFacts::device_only("vx0"), "vxlan-temps0", 42, 4789);
        assert!(
            message.contains("'vx0' is not a Temps overlay device"),
            "{message}"
        );
        assert!(message.contains("`ip link delete vx0`"), "{message}");
        assert!(message.contains("its own network ID"), "{message}");
    }

    #[test]
    fn unsafe_names_never_reach_a_command() {
        let facts = HolderFacts {
            docker: Some(DockerLookup::Network {
                name: "temps0".into(),
                subnets: vec![],
                containers: vec!["bad;rm -rf".into()],
            }),
            ..leftover(&[])
        };
        let message = describe(&facts, "vxlan-temps1", 42, 4789);
        assert!(!message.contains("docker rm -f bad"), "{message}");
        assert!(message.contains("--filter network=temps0"), "{message}");
    }

    #[test]
    fn devices_are_parsed_from_ip_output() {
        let output = "11: vxlan-temps0: <UP> mtu 1450 master br-temps0 \\    vxlan id 42 \
                      dev eth0 srcport 0 0 dstport 4789 nolearning\n\
                      12: vx0@dummy0: <UP> mtu 1450 \\    vxlan id 7 dev dummy0 srcport 0 0 \
                      dstport 8472 nolearning\n\
                      13: lo: <LOOPBACK> mtu 65536\n";
        let devices = parse_devices(output);
        assert_eq!(
            devices,
            vec![
                VxlanDevice {
                    name: "vxlan-temps0".into(),
                    vni: 42,
                    port: 4789
                },
                VxlanDevice {
                    name: "vx0".into(),
                    vni: 7,
                    port: 8472
                },
            ]
        );
        assert_eq!(
            holder(&devices, "vxlan-temps1", 42, 4789).map(|d| d.name.as_str()),
            Some("vxlan-temps0")
        );
        assert_eq!(holder(&devices, "vxlan-temps0", 42, 4789), None);
        assert_eq!(holder(&devices, "vxlan-temps1", 43, 4789), None);
    }

    #[test]
    fn bridge_subnets_are_networks() {
        let output = "7: br-temps0    inet 10.202.0.1/24 brd 10.202.0.255 scope global br-temps0\\       valid_lft forever preferred_lft forever\n";
        assert_eq!(parse_inet_subnets(output), vec!["10.202.0.0/24"]);
    }

    #[test]
    fn the_old_wording_is_recognised_too() {
        assert!(reports_id_in_use(
            "bootstrap failed: vxlan transport error on 'vxlan-temps1': VXLAN device \
             'vxlan-temps0' already holds vni=42, port=4789 on this host"
        ));
        assert!(!reports_id_in_use("overlaps host route 10.0.0.0/8"));
    }
}
