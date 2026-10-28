// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! The network policy boot guard.
//!
//! nftables tables and eBPF programs live in the kernel, so a reboot drops
//! them. Docker then restarts every container long before the agent has
//! fetched its policy and applied it again, which on a node in `enforce`
//! leaves restricted workloads reachable (and able to reach out) for that
//! whole window.
//!
//! While the node enforces, the agent keeps a guard ruleset on disk that
//! drops forwarded traffic into and out of the subnets its workloads use.
//! `temps-netpol-guard.service` loads it before `docker.service` at boot,
//! and the agent removes the guard table once its first pass has applied
//! the real policy. Outside `enforce` the file is removed, so a node that
//! does not enforce never boots behind a guard.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::Mutex;

use bollard::Docker;
use ipnet::IpNet;
use temps_workload_netpol::{NodeMode, PassOutcome};
use tracing::{info, warn};

/// The guard's nftables table. Not the policy table: lifting the guard must
/// never touch the policy, and loading it must never replace it.
pub const GUARD_TABLE: &str = "temps_netpol_boot_guard";

/// File name of the guard ruleset in the agent data directory.
pub const GUARD_FILE: &str = "netpol-boot-guard.nft";

/// Priority of the guard's chain: before the policy (-190), so nothing the
/// policy accepts gets past it while it is loaded.
const GUARD_PRIORITY: i32 = -195;
const _: () = assert!(
    GUARD_PRIORITY < temps_workload_netpol::HOOK_PRIORITY,
    "the boot guard must run before the policy chain"
);

/// Passes between re-reading the workload subnets (15s each).
const SUBNET_REFRESH_PASSES: u32 = 40;

/// The guard ruleset for `subnets`. Replaces any guard already loaded.
pub fn render(subnets: &[IpNet]) -> String {
    let v4: Vec<String> = subnets
        .iter()
        .filter(|net| matches!(net, IpNet::V4(_)))
        .map(ToString::to_string)
        .collect();
    let v6: Vec<String> = subnets
        .iter()
        .filter(|net| matches!(net, IpNet::V6(_)))
        .map(ToString::to_string)
        .collect();
    let mut rules = String::new();
    for (family, nets) in [("ip", &v4), ("ip6", &v6)] {
        if nets.is_empty() {
            continue;
        }
        let set = nets.join(", ");
        rules.push_str(&format!("\t\t{family} saddr {{ {set} }} counter drop\n"));
        rules.push_str(&format!("\t\t{family} daddr {{ {set} }} counter drop\n"));
    }
    format!(
        "# Managed by Temps: the network policy boot guard. Loaded before Docker at\n\
         # boot; removed by the agent once it has applied this node's policy.\n\
         table inet {GUARD_TABLE}\n\
         delete table inet {GUARD_TABLE}\n\
         table inet {GUARD_TABLE} {{\n\
         \tchain forward {{\n\
         \t\ttype filter hook forward priority {GUARD_PRIORITY}; policy accept;\n\
         {rules}\
         \t}}\n\
         }}\n"
    )
}

/// Whether a pass leaves the node enforcing a policy the kernel now holds.
fn enforcing(outcome: &PassOutcome) -> Option<bool> {
    match outcome {
        PassOutcome::Applied(_) => Some(true),
        PassOutcome::Unchanged { mode, .. } => Some(*mode == NodeMode::Enforce),
        PassOutcome::Legacy | PassOutcome::Removed { .. } | PassOutcome::Shadowed(_) => Some(false),
    }
}

/// Keeps the guard file in step with the node's mode and lifts the guard
/// table loaded at boot.
pub struct BootGuard {
    path: PathBuf,
    docker: Docker,
    lifted: AtomicBool,
    passes_since_refresh: AtomicU32,
    written: Mutex<Option<String>>,
}

impl BootGuard {
    pub fn new(data_dir: &Path, docker: Docker) -> Self {
        Self {
            path: data_dir.join(GUARD_FILE),
            docker,
            lifted: AtomicBool::new(false),
            passes_since_refresh: AtomicU32::new(0),
            written: Mutex::new(None),
        }
    }

    /// Called after every successful pass.
    pub async fn after_pass(&self, outcome: &PassOutcome) {
        let Some(enforcing) = enforcing(outcome) else {
            return;
        };
        if enforcing {
            self.keep_file().await;
        } else {
            self.remove_file();
        }
        // The real policy (or none, outside `enforce`) is in force now.
        if !self.lifted.swap(true, Ordering::SeqCst) {
            lift().await;
        }
    }

    async fn keep_file(&self) {
        let passes = self.passes_since_refresh.fetch_add(1, Ordering::Relaxed);
        let have_file = self
            .written
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .is_some();
        if have_file && passes < SUBNET_REFRESH_PASSES {
            return;
        }
        self.passes_since_refresh.store(0, Ordering::Relaxed);
        let subnets = workload_subnets(&self.docker).await;
        if subnets.is_empty() {
            warn!(
                path = %self.path.display(),
                "no workload subnet found; the network policy boot guard is not updated"
            );
            return;
        }
        let ruleset = render(&subnets);
        let mut written = self.written.lock().unwrap_or_else(|e| e.into_inner());
        if written.as_deref() == Some(ruleset.as_str()) {
            return;
        }
        match write_private(&self.path, &ruleset) {
            Ok(()) => {
                info!(
                    path = %self.path.display(),
                    subnets = ?subnets.iter().map(ToString::to_string).collect::<Vec<_>>(),
                    "network policy boot guard written; it is loaded before Docker at boot"
                );
                *written = Some(ruleset);
            }
            Err(error) => warn!(
                path = %self.path.display(),
                %error,
                "could not write the network policy boot guard; workloads are unguarded \
                 between a reboot and the agent's first pass"
            ),
        }
    }

    fn remove_file(&self) {
        let mut written = self.written.lock().unwrap_or_else(|e| e.into_inner());
        match std::fs::remove_file(&self.path) {
            Ok(()) => info!(path = %self.path.display(), "network policy boot guard removed"),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => warn!(
                path = %self.path.display(),
                %error,
                "could not remove the network policy boot guard; the next boot will block \
                 workload traffic until the agent's first pass"
            ),
        }
        *written = None;
    }
}

/// Remove the guard table if the boot loaded one.
async fn lift() {
    let output = tokio::process::Command::new("nft")
        .args(["delete", "table", "inet", GUARD_TABLE])
        .output()
        .await;
    match output {
        Ok(output) if output.status.success() => {
            info!("network policy boot guard lifted: this node's policy is applied")
        }
        // No guard loaded: the agent restarted without a reboot, or the node
        // does not enforce.
        Ok(_) => {}
        Err(error) => warn!(%error, "could not run nft to lift the network policy boot guard"),
    }
}

/// The subnets of the networks Temps workloads run on here: the app network
/// and the cross-node overlay.
async fn workload_subnets(docker: &Docker) -> Vec<IpNet> {
    let names = [
        temps_core::NETWORK_NAME.to_string(),
        temps_network::overlay_name::current().docker_network,
    ];
    let mut subnets = Vec::new();
    for name in names {
        match docker
            .inspect_network(
                &name,
                None::<bollard::query_parameters::InspectNetworkOptions>,
            )
            .await
        {
            Ok(network) => subnets.extend(
                network
                    .ipam
                    .and_then(|ipam| ipam.config)
                    .unwrap_or_default()
                    .into_iter()
                    .filter_map(|config| config.subnet?.parse::<IpNet>().ok()),
            ),
            // A single-host node has no overlay.
            Err(bollard::errors::Error::DockerResponseServerError {
                status_code: 404, ..
            }) => {}
            Err(error) => {
                warn!(network = %name, %error, "could not read a workload network's subnet")
            }
        }
    }
    subnets.sort();
    subnets.dedup();
    subnets
}

fn write_private(path: &Path, contents: &str) -> std::io::Result<()> {
    use std::io::Write as _;
    let temporary = path.with_extension("nft.tmp");
    {
        let mut options = std::fs::OpenOptions::new();
        options.write(true).create(true).truncate(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt as _;
            options.mode(0o600);
        }
        let mut file = options.open(&temporary)?;
        file.write_all(contents.as_bytes())?;
        file.sync_all()?;
    }
    std::fs::rename(&temporary, path)
}

#[cfg(test)]
mod tests {
    use super::*;
    use temps_workload_netpol::PassSummary;

    #[test]
    fn the_guard_drops_both_directions_of_every_workload_subnet() {
        let ruleset = render(&[
            "10.20.30.0/24".parse().unwrap(),
            "172.30.0.0/16".parse().unwrap(),
            "fd00:20::/64".parse().unwrap(),
        ]);
        assert!(ruleset.contains("table inet temps_netpol_boot_guard {"));
        assert!(ruleset.contains("delete table inet temps_netpol_boot_guard\n"));
        assert!(ruleset.contains("hook forward priority -195; policy accept;"));
        assert!(ruleset.contains("ip saddr { 10.20.30.0/24, 172.30.0.0/16 } counter drop"));
        assert!(ruleset.contains("ip daddr { 10.20.30.0/24, 172.30.0.0/16 } counter drop"));
        assert!(ruleset.contains("ip6 saddr { fd00:20::/64 } counter drop"));
        assert!(ruleset.contains("ip6 daddr { fd00:20::/64 } counter drop"));
    }

    #[test]
    fn only_an_enforcing_pass_keeps_the_guard() {
        let summary = PassSummary {
            digest: 1,
            local_workloads: 0,
            enforced_policies: 0,
            bindings: 0,
            warnings: vec![],
        };
        assert_eq!(
            enforcing(&PassOutcome::Applied(summary.clone())),
            Some(true)
        );
        assert_eq!(
            enforcing(&PassOutcome::Unchanged {
                mode: NodeMode::Enforce,
                digest: 1
            }),
            Some(true)
        );
        assert_eq!(
            enforcing(&PassOutcome::Unchanged {
                mode: NodeMode::Shadow,
                digest: 1
            }),
            Some(false)
        );
        assert_eq!(enforcing(&PassOutcome::Shadowed(summary)), Some(false));
        assert_eq!(enforcing(&PassOutcome::Legacy), Some(false));
        assert_eq!(
            enforcing(&PassOutcome::Removed {
                mode: NodeMode::Legacy
            }),
            Some(false)
        );
    }
}
