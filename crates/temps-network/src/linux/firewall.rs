//! nftables baseline rules.
//!
//! We install one dedicated nftables table named `temps_network` so we can
//! tear our rules down without touching anything else on the host. The
//! table has two chains:
//!
//! * `forward` (priority -100, type filter, hook forward) — accepts
//!   anything that ingresses from or egresses to our bridge. Sits *before*
//!   Docker's default-DROP `forward` chain so it takes effect even when
//!   Docker is installed alongside us.
//! * `postrouting` (priority 100, type nat, hook postrouting) — masquerades
//!   compute CIDR traffic that egresses on a non-bridge interface. This is what
//!   lets containers reach the internet.
//!
//! We shell out to `nft` because it is the canonical tool, every modern
//! distro ships it, and the rule set we need is small enough that an
//! embedded library (`rustables`) would add more complexity than value.

use crate::config::{NetworkConfig, NodeAlloc};
use crate::error::NetworkError;
use std::process::Stdio;
use tokio::io::AsyncWriteExt;
use tokio::process::Command;
use tracing::{debug, info};

const TABLE: &str = "temps_network";

/// Install the baseline rules. Idempotent: the script first deletes the
/// table (ignoring "not found"), then recreates it.
pub async fn install_baseline(config: &NetworkConfig, alloc: &NodeAlloc) -> crate::Result<()> {
    let script = render_baseline(config, alloc);
    apply_nft(&script)
        .await
        .map_err(|reason| NetworkError::Nftables {
            op: "install_baseline",
            table: TABLE.into(),
            reason,
        })?;
    info!(table = TABLE, bridge = %config.bridge_name, cidr = %alloc.compute_cidr, "nftables baseline installed");
    Ok(())
}

/// Remove the baseline rules. Idempotent.
pub async fn remove_baseline(_config: &NetworkConfig) -> crate::Result<()> {
    let script = format!("delete table inet {table}\n", table = TABLE);
    match apply_nft(&script).await {
        Ok(()) => Ok(()),
        Err(reason) if reason.contains("No such file") || reason.contains("does not exist") => {
            debug!(table = TABLE, "nftables table already absent");
            Ok(())
        }
        Err(reason) => Err(NetworkError::Nftables {
            op: "remove_baseline",
            table: TABLE.into(),
            reason,
        }),
    }
}

fn render_baseline(config: &NetworkConfig, alloc: &NodeAlloc) -> String {
    let bridge = &config.bridge_name;
    let cidr = alloc.compute_cidr;
    format!(
        "
# Idempotent install: drop the table if it exists, recreate from scratch.
add table inet {table}
delete table inet {table}
add table inet {table}

add chain inet {table} forward {{ type filter hook forward priority -100; policy accept; }}
add rule inet {table} forward iifname \"{bridge}\" accept
add rule inet {table} forward oifname \"{bridge}\" accept

add chain inet {table} postrouting {{ type nat hook postrouting priority 100; policy accept; }}
add rule inet {table} postrouting ip saddr {cidr} oifname != \"{bridge}\" masquerade
",
        table = TABLE,
        bridge = bridge,
        cidr = cidr,
    )
}

async fn apply_nft(script: &str) -> std::result::Result<(), String> {
    let mut child = Command::new("nft")
        .arg("-f")
        .arg("-")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| format!("spawn nft: {}", e))?;

    if let Some(mut stdin) = child.stdin.take() {
        stdin
            .write_all(script.as_bytes())
            .await
            .map_err(|e| format!("write nft script: {}", e))?;
        stdin
            .shutdown()
            .await
            .map_err(|e| format!("close nft stdin: {}", e))?;
    }

    let out = child
        .wait_with_output()
        .await
        .map_err(|e| format!("wait nft: {}", e))?;
    if out.status.success() {
        Ok(())
    } else {
        Err(String::from_utf8_lossy(&out.stderr).trim().to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ipnet::Ipv4Net;
    use std::net::{IpAddr, Ipv4Addr};
    use std::str::FromStr;
    use uuid::Uuid;

    #[test]
    fn baseline_script_includes_bridge_and_cidr() {
        let cfg = NetworkConfig::default();
        let alloc = NodeAlloc {
            node_id: Uuid::nil(),
            compute_cidr: Ipv4Net::from_str("172.20.5.0/24").unwrap(),
            bridge_address: IpAddr::V4(Ipv4Addr::new(172, 20, 5, 1)),
            underlay_address: IpAddr::V4(Ipv4Addr::new(10, 0, 0, 1)),
        };
        let s = render_baseline(&cfg, &alloc);
        assert!(s.contains("br-temps0"));
        assert!(s.contains("172.20.5.0/24"));
        assert!(s.contains("masquerade"));
        assert!(s.contains("delete table inet temps_network"));
    }
}
