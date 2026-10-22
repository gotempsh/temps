// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Finding the bridge port of each instance, for [`crate::binding`].
//!
//! Docker does not report the host-side name of a container's veth pair.
//! It is found from both ends of the pair:
//!
//! - inside the container, `/proc/<pid>/root/sys/class/net/<if>/iflink` is
//!   the host-side peer's interface index, and `address` its MAC, which
//!   matches the MAC Docker reports for the endpoint on that network;
//! - on the host, `/sys/class/net/<name>/ifindex` maps that index to a name.
//!
//! [`ProcPortResolver`] reads both directly (Linux, the agent next to
//! `dockerd`); [`DockerVmPortResolver`] reads them in a helper container on
//! the VM's PID namespace with `SYS_PTRACE` and nothing else (macOS). Both
//! produce the same listing and share one parser, so the two paths cannot
//! disagree on how a port is found.

use std::collections::HashMap;

use async_trait::async_trait;

use crate::executor::{DockerVmNft, NftError};

/// One container endpoint to find the port of.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct PortQuery {
    /// The container's init PID in the namespace the resolver reads.
    pub pid: i64,
    /// The endpoint's MAC address, as Docker reports it (lower case).
    pub mac: String,
}

#[async_trait]
pub trait PortResolver: Send + Sync {
    /// The host-side interface of each queried endpoint that was found.
    /// Endpoints whose container is gone are left out, not errors: the next
    /// reconcile pass sees the container gone too.
    async fn resolve(&self, queries: &[PortQuery]) -> Result<HashMap<PortQuery, String>, NftError>;
}

/// The shell listing both resolvers produce:
/// `c <pid> <iflink> <mac>` per container interface and `h <ifindex> <name>`
/// per host interface.
fn listing_script(pids: &[i64]) -> String {
    let pids: Vec<String> = pids.iter().map(|p| p.to_string()).collect();
    format!(
        "for p in {pids}; do for d in /proc/$p/root/sys/class/net/*; do \
           [ -r \"$d/iflink\" ] && echo \"c $p $(cat $d/iflink) $(cat $d/address)\"; \
         done 2>/dev/null; done; \
         for d in /sys/class/net/*; do echo \"h $(cat $d/ifindex) ${{d##*/}}\"; done",
        pids = pids.join(" ")
    )
}

/// Parses a listing into the port of each query.
pub fn parse_listing(listing: &str, queries: &[PortQuery]) -> HashMap<PortQuery, String> {
    let mut by_endpoint: HashMap<(i64, String), u64> = HashMap::new();
    let mut names: HashMap<u64, String> = HashMap::new();
    for line in listing.lines() {
        let fields: Vec<&str> = line.split_whitespace().collect();
        match fields.as_slice() {
            ["c", pid, iflink, mac] => {
                if let (Ok(pid), Ok(iflink)) = (pid.parse::<i64>(), iflink.parse::<u64>()) {
                    by_endpoint.insert((pid, mac.to_ascii_lowercase()), iflink);
                }
            }
            ["h", index, name] => {
                if let Ok(index) = index.parse::<u64>() {
                    names.insert(index, (*name).to_string());
                }
            }
            _ => {}
        }
    }
    queries
        .iter()
        .filter_map(|query| {
            let index = by_endpoint.get(&(query.pid, query.mac.to_ascii_lowercase()))?;
            let name = names.get(index)?;
            Some((query.clone(), name.clone()))
        })
        .collect()
}

/// Reads `/proc` and `/sys` of this host (Linux, as root).
#[derive(Debug, Default, Clone)]
pub struct ProcPortResolver;

#[async_trait]
impl PortResolver for ProcPortResolver {
    async fn resolve(&self, queries: &[PortQuery]) -> Result<HashMap<PortQuery, String>, NftError> {
        if queries.is_empty() {
            return Ok(HashMap::new());
        }
        let mut pids: Vec<i64> = queries.iter().map(|q| q.pid).collect();
        pids.sort_unstable();
        pids.dedup();
        let script = listing_script(&pids);
        let output = tokio::process::Command::new("/bin/sh")
            .arg("-c")
            .arg(&script)
            .output()
            .await
            .map_err(|e| NftError::Spawn {
                location: "this host".to_string(),
                program: "/bin/sh".to_string(),
                reason: e.to_string(),
            })?;
        Ok(parse_listing(
            &String::from_utf8_lossy(&output.stdout),
            queries,
        ))
    }
}

/// Reads them inside the Docker VM through a helper container.
pub struct DockerVmPortResolver {
    vm: DockerVmNft,
}

impl DockerVmPortResolver {
    pub fn new(docker: bollard::Docker) -> Self {
        Self {
            vm: DockerVmNft::new(docker),
        }
    }
}

#[async_trait]
impl PortResolver for DockerVmPortResolver {
    async fn resolve(&self, queries: &[PortQuery]) -> Result<HashMap<PortQuery, String>, NftError> {
        if queries.is_empty() {
            return Ok(HashMap::new());
        }
        let mut pids: Vec<i64> = queries.iter().map(|q| q.pid).collect();
        pids.sort_unstable();
        pids.dedup();
        let (status, output) = self.vm.run_port_probe(&listing_script(&pids)).await?;
        if status != 0 {
            return Err(NftError::Rejected {
                location: "the Docker VM".to_string(),
                status,
                output: output.trim().to_string(),
            });
        }
        Ok(parse_listing(&output, queries))
    }
}

/// The resolver for where the daemon runs, the same choice as
/// [`crate::choose_executor`].
pub fn choose_port_resolver(
    docker: bollard::Docker,
    daemon_is_local: bool,
) -> Box<dyn PortResolver> {
    if cfg!(target_os = "linux") && daemon_is_local {
        Box::new(ProcPortResolver)
    } else {
        Box::new(DockerVmPortResolver::new(docker))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_listing_maps_each_endpoint_to_its_host_port() {
        let listing = "c 42 17 2A:ED:E8:11:17:CE\nc 42 1 00:00:00:00:00:00\nc 43 19 aa:bb:cc:dd:ee:ff\nh 1 lo\nh 17 veth6035663\nh 18 br-1\ngarbage line\n";
        let a = PortQuery {
            pid: 42,
            mac: "2a:ed:e8:11:17:ce".into(),
        };
        let gone = PortQuery {
            pid: 43,
            mac: "aa:bb:cc:dd:ee:ff".into(),
        };
        let found = parse_listing(listing, &[a.clone(), gone.clone()]);
        assert_eq!(found.get(&a).map(String::as_str), Some("veth6035663"));
        assert!(
            !found.contains_key(&gone),
            "an index with no host interface is left out"
        );
    }

    #[test]
    fn the_script_reads_every_pid_once() {
        let script = listing_script(&[7, 9]);
        assert!(script.contains("for p in 7 9;"), "{script}");
        assert!(script.contains("/sys/class/net/*"));
    }
}
