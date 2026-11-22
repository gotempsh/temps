// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Host-network operations of a macOS worker, run in the Docker app's VM
//! through the mesh sidecar (ADR 090; see `temps_wireguard::vm`).
//!
//! The VM is where the mesh interface, the published ports and the
//! container bridges are, so the mesh lockdown, the route preflight and the
//! MTU are all the VM's, not the Mac's. The rules installed are exactly the
//! Linux ones ([`crate::mesh::render_lockdown`]).

use ipnet::Ipv4Net;

/// IPv4 unicast routes of `ip -4 route show table all`, with their device.
/// Typed routes (`local`, `broadcast`, `unreachable`, …) and the default
/// route are skipped, as the Linux walk skips non-unicast routes.
pub fn parse_routes(text: &str) -> Vec<(Ipv4Net, String)> {
    text.lines()
        .filter_map(|line| {
            let mut words = line.split_whitespace();
            let first = words.next()?;
            let net = first
                .parse::<Ipv4Net>()
                .ok()
                .or_else(|| first.parse::<std::net::Ipv4Addr>().ok().map(Ipv4Net::from))?;
            let mut rest = line.split_whitespace();
            rest.find(|word| *word == "dev")?;
            Some((net, rest.next()?.to_string()))
        })
        .collect()
}

/// The first route that overlaps `pool` and is not on one of `owned`.
pub fn colliding_route(
    routes: &[(Ipv4Net, String)],
    pool: Ipv4Net,
    owned: &[&str],
) -> Option<(Ipv4Net, String)> {
    routes
        .iter()
        .find(|(net, device)| {
            !owned.contains(&device.as_str())
                && (net.contains(&pool.network()) || pool.contains(&net.network()))
        })
        .cloned()
}

/// The device of `ip -4 route show default`.
pub fn default_route_device(text: &str) -> Option<String> {
    let line = text.lines().find(|line| line.starts_with("default"))?;
    let mut words = line.split_whitespace();
    words.find(|word| *word == "dev")?;
    words.next().map(str::to_string)
}

#[cfg(target_os = "macos")]
mod imp {
    use super::*;
    use crate::error::NetworkError;
    use crate::mesh::{LockdownState, MeshLockdown, MESH_INTERFACE, MESH_TABLE};
    use std::process::Stdio;
    use tokio::io::AsyncWriteExt;
    use tracing::info;

    fn failed(operation: &str, reason: String) -> NetworkError {
        NetworkError::VmExec {
            operation: operation.to_string(),
            reason,
        }
    }

    /// Run `argv` in the mesh sidecar; `Ok(None)` when it exited non-zero
    /// (the caller decides whether that is an answer or a failure).
    async fn exec(
        operation: &str,
        argv: &[&str],
        stdin: Option<&[u8]>,
    ) -> Result<Result<String, String>, NetworkError> {
        let endpoint = temps_wireguard::vm::docker_endpoint()
            .map_err(|error| failed(operation, error.to_string()))?;
        let mut child = tokio::process::Command::new(&endpoint.cli)
            .args(temps_wireguard::vm::exec_args(
                &endpoint.host,
                argv,
                stdin.is_some(),
            ))
            .stdin(if stdin.is_some() {
                Stdio::piped()
            } else {
                Stdio::null()
            })
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true)
            .spawn()
            .map_err(|error| failed(operation, format!("cannot run {}: {error}", endpoint.cli)))?;
        if let (Some(input), Some(mut pipe)) = (stdin, child.stdin.take()) {
            pipe.write_all(input).await.map_err(|error| {
                failed(operation, format!("cannot write to docker exec: {error}"))
            })?;
        }
        let output =
            tokio::time::timeout(temps_wireguard::vm::EXEC_TIMEOUT, child.wait_with_output())
                .await
                .map_err(|_| {
                    failed(
                        operation,
                        format!(
                            "no answer within {}s: the Docker app's VM may be paused (the Mac is \
                         asleep) or restarting",
                            temps_wireguard::vm::EXEC_TIMEOUT.as_secs()
                        ),
                    )
                })?
                .map_err(|error| failed(operation, format!("docker exec: {error}")))?;
        let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
        if stderr.contains("No such container") || stderr.contains("is not running") {
            return Err(failed(
                operation,
                format!("the mesh sidecar is not running in the Docker app's VM: {stderr}"),
            ));
        }
        Ok(if output.status.success() {
            Ok(String::from_utf8_lossy(&output.stdout).into_owned())
        } else {
            Err(stderr)
        })
    }

    pub async fn lockdown_state(lockdown: &MeshLockdown) -> Result<LockdownState, NetworkError> {
        let listed = exec(
            "inspect the mesh lockdown",
            &["nft", "list", "table", "inet", MESH_TABLE],
            None,
        )
        .await?;
        Ok(match listed {
            Err(_) => LockdownState::Missing,
            Ok(rules) if rules.contains(&crate::mesh::lockdown_marker(lockdown)) => {
                LockdownState::Current
            }
            Ok(_) => LockdownState::Outdated,
        })
    }

    pub async fn ensure_lockdown(lockdown: &MeshLockdown) -> Result<bool, NetworkError> {
        if lockdown_state(lockdown).await? == LockdownState::Current {
            return Ok(false);
        }
        let script = crate::mesh::render_lockdown(lockdown);
        exec(
            "install the mesh lockdown",
            &["nft", "-f", "-"],
            Some(script.as_bytes()),
        )
        .await?
        .map_err(|reason| NetworkError::Nftables {
            op: "install_mesh_lockdown",
            table: MESH_TABLE.into(),
            reason: format!("in the Docker app's VM: {reason}"),
        })?;
        info!(
            table = MESH_TABLE,
            "WireGuard mesh lockdown installed in the Docker app's VM"
        );
        Ok(true)
    }

    pub async fn preflight_mesh_routes(pool: Ipv4Net) -> Result<(), NetworkError> {
        let text = exec(
            "list the VM's routes",
            &["ip", "-4", "route", "show", "table", "all"],
            None,
        )
        .await?
        .map_err(|reason| failed("list the VM's routes", reason))?;
        match colliding_route(&parse_routes(&text), pool, &[MESH_INTERFACE]) {
            Some((existing_cidr, device)) => Err(NetworkError::MeshRouteCollision {
                pool,
                existing_cidr,
                device,
            }),
            None => Ok(()),
        }
    }

    pub async fn default_route_mtu() -> Result<u32, NetworkError> {
        let text = exec(
            "read the VM's default route",
            &["ip", "-4", "route", "show", "default"],
            None,
        )
        .await?
        .map_err(|reason| failed("read the VM's default route", reason))?;
        let device =
            default_route_device(&text).ok_or_else(|| NetworkError::UnderlayDetection {
                reason: "the Docker app's VM has no IPv4 default route".to_string(),
            })?;
        let path = format!("/sys/class/net/{device}/mtu");
        let mtu = exec("read the VM's MTU", &["cat", &path], None)
            .await?
            .map_err(|reason| NetworkError::UnderlayMtuDetection {
                device: device.clone(),
                reason,
            })?;
        mtu.trim()
            .parse()
            .map_err(|error| NetworkError::UnderlayMtuDetection {
                device,
                reason: format!("unreadable MTU {mtu:?}: {error}"),
            })
    }
}

#[cfg(target_os = "macos")]
pub(crate) use imp::{default_route_mtu, ensure_lockdown, lockdown_state, preflight_mesh_routes};

#[cfg(test)]
mod tests {
    use super::*;

    const ROUTES: &str = "default via 192.168.5.2 dev eth0 proto dhcp src 192.168.5.1 metric 100
10.210.0.0/16 dev temps-wg0 proto kernel scope link src 10.210.0.4
172.17.0.0/16 dev docker0 proto kernel scope link src 172.17.0.1 linkdown
192.168.5.0/24 dev eth0 proto kernel scope link src 192.168.5.1 metric 100
local 127.0.0.0/8 dev lo table local proto kernel scope host src 127.0.0.1
broadcast 172.17.255.255 dev docker0 table local proto kernel scope link src 172.17.0.1
";

    #[test]
    fn reads_unicast_routes_only() {
        let routes = parse_routes(ROUTES);
        assert_eq!(
            routes,
            vec![
                ("10.210.0.0/16".parse().unwrap(), "temps-wg0".to_string()),
                ("172.17.0.0/16".parse().unwrap(), "docker0".to_string()),
                ("192.168.5.0/24".parse().unwrap(), "eth0".to_string()),
            ]
        );
    }

    #[test]
    fn the_mesh_interface_own_route_is_not_a_collision() {
        let routes = parse_routes(ROUTES);
        assert_eq!(
            colliding_route(&routes, "10.210.0.0/16".parse().unwrap(), &["temps-wg0"]),
            None
        );
    }

    #[test]
    fn a_pool_shadowing_a_vm_network_is_reported_with_its_device() {
        let routes = parse_routes(ROUTES);
        assert_eq!(
            colliding_route(&routes, "172.17.128.0/20".parse().unwrap(), &["temps-wg0"]),
            Some(("172.17.0.0/16".parse().unwrap(), "docker0".to_string()))
        );
        assert_eq!(
            colliding_route(&routes, "192.168.0.0/16".parse().unwrap(), &["temps-wg0"]),
            Some(("192.168.5.0/24".parse().unwrap(), "eth0".to_string()))
        );
    }

    #[test]
    fn finds_the_default_route_device() {
        assert_eq!(default_route_device(ROUTES).as_deref(), Some("eth0"));
        assert_eq!(default_route_device("10.0.0.0/8 dev x\n"), None);
    }
}
