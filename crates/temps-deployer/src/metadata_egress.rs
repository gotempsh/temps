//! Atomic cloud-metadata egress protection for container traffic.
//!
//! A dedicated nftables base chain runs before Docker's forwarding rules and
//! rejects known metadata destinations for every routed container network,
//! including bridges created dynamically by Docker Compose. The complete table
//! is replaced in one nft transaction, so a failed refresh preserves the last
//! known-good policy instead of flushing a live chain.

use std::process::Stdio;

use bollard::Docker;
use thiserror::Error;
use tokio::io::AsyncWriteExt;
use tracing::info;

const TABLE: &str = "temps_metadata";
const FORWARD_PRIORITY: i32 = -200;

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

    static APPLY_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());
    let _guard = APPLY_LOCK.lock().await;
    let ruleset = render_ruleset();
    apply_nft(&ruleset).await?;

    info!(
        network = network_name,
        table = TABLE,
        priority = FORWARD_PRIORITY,
        "Cloud-metadata egress protection reconciled"
    );
    Ok(())
}

fn render_ruleset() -> String {
    include_str!("../tests/fixtures/metadata-egress.nft").to_string()
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

    #[test]
    fn ruleset_is_atomic_and_precedes_docker_forwarding() {
        let ruleset = render_ruleset();
        assert!(ruleset.contains("add table inet temps_metadata"));
        assert!(ruleset.contains("delete table inet temps_metadata"));
        assert!(ruleset.contains("hook forward priority -200"));
        assert!(!ruleset.contains("iifname"));
        assert!(!ruleset.contains("br-"));
    }

    #[test]
    fn ruleset_blocks_all_supported_ipv4_and_ipv6_metadata_ranges() {
        let ruleset = render_ruleset();
        for destination in [
            "ip daddr 169.254.0.0/16",
            "ip daddr 100.100.100.200",
            "ip6 daddr fd00:ec2::254",
            "ip6 daddr fd20:ce::254",
        ] {
            assert!(
                ruleset.contains(destination),
                "missing metadata destination {destination}"
            );
        }
        assert!(ruleset.contains("reject with icmp type port-unreachable"));
        assert!(ruleset.contains("reject with icmpv6 type port-unreachable"));
    }

    #[test]
    fn ruleset_does_not_block_linked_service_ranges() {
        let ruleset = render_ruleset();
        for private_range in ["10.0.0.0", "172.16.0.0", "192.168.0.0"] {
            assert!(!ruleset.contains(private_range));
        }
    }
}
