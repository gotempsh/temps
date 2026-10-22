// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Applying a ruleset where the Docker bridges live.
//!
//! On Linux the bridges are the host's: [`HostNft`] runs `nft` directly.
//! On macOS they are inside the Docker VM (colima, Docker Desktop), where
//! Temps cannot run a process. [`DockerVmNft`] runs `nft` in a short-lived
//! helper container on the VM's network namespace with `NET_ADMIN`, and
//! nothing else. Both apply the same bytes, so a policy tested on a laptop
//! is the policy a server enforces.
//!
//! Both refuse to apply when bridged traffic does not reach netfilter
//! (`net.bridge.bridge-nf-call-iptables` is not 1): traffic between two
//! containers on one bridge would then skip every rule, and a policy that
//! silently does not apply is worse than one that fails.

use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use async_trait::async_trait;
use bollard::models::{ContainerConfig, ContainerCreateBody, HostConfig};
use bollard::query_parameters::{
    CommitContainerOptionsBuilder, CreateContainerOptionsBuilder, CreateImageOptionsBuilder,
    LogsOptions, RemoveContainerOptions, UploadToContainerOptionsBuilder, WaitContainerOptions,
};
use bollard::Docker;
use futures::StreamExt;
use tokio::io::AsyncWriteExt;
use tokio::sync::Mutex;
use tracing::{debug, info};

use crate::nft::{removal_ruleset, TableName};

/// Where the kernel says whether bridged frames reach netfilter.
pub const BRIDGE_NF_SYSCTL: &str = "/proc/sys/net/bridge/bridge-nf-call-iptables";

/// Exit code the helper uses for "bridged traffic would bypass the rules".
const EXIT_BRIDGE_NF_DISABLED: i64 = 3;

#[derive(Debug, thiserror::Error)]
pub enum NftError {
    #[error(
        "bridged traffic does not reach netfilter on {location} ({BRIDGE_NF_SYSCTL} is {value}): \
         traffic between containers on one bridge would bypass every policy. \
         Load br_netfilter and set net.bridge.bridge-nf-call-iptables=1"
    )]
    BridgeNetfilterDisabled { location: String, value: String },
    #[error("nft on {location} rejected the ruleset (exit {status}): {output}")]
    Rejected {
        location: String,
        status: i64,
        output: String,
    },
    #[error("could not run '{program}' on {location}: {reason}")]
    Spawn {
        location: String,
        program: String,
        reason: String,
    },
    #[error("Docker {operation} for the nft helper failed: {reason}")]
    Docker { operation: String, reason: String },
    #[error("building the nft helper image {image} failed: {reason}")]
    HelperImage { image: String, reason: String },
    #[error("nft on {location} did not finish within {timeout_secs}s")]
    Timeout { location: String, timeout_secs: u64 },
}

#[async_trait]
pub trait NftExecutor: Send + Sync {
    /// Applies an `nft -f` script in one transaction.
    async fn apply(&self, ruleset: &str) -> Result<(), NftError>;

    /// Removes `table`. Succeeds when it is already absent.
    async fn remove(&self, table: &TableName) -> Result<(), NftError>;

    /// `nft list table inet <table>`, or `None` when it does not exist.
    async fn list(&self, table: &TableName) -> Result<Option<String>, NftError>;

    /// Where the rules land, for logs and errors.
    fn location(&self) -> String;
}

/// The executor for where the daemon runs (ADR-055 §17): the host's own
/// `nft` when this is Linux and the daemon is on this machine (an unset or
/// `unix://` `DOCKER_HOST`, the test `DockerRuntime` makes for host-level
/// facts), the daemon's VM through a helper container otherwise. On macOS a
/// `unix://` socket still leads into a VM, so it takes the helper path.
pub fn choose_executor(docker: Docker, daemon_is_local: bool) -> Box<dyn NftExecutor> {
    if cfg!(target_os = "linux") && daemon_is_local {
        Box::new(HostNft::new())
    } else {
        Box::new(DockerVmNft::new(docker))
    }
}

/// `nft` on this host.
#[derive(Debug, Clone)]
pub struct HostNft {
    program: String,
    timeout: Duration,
}

impl Default for HostNft {
    fn default() -> Self {
        Self::new()
    }
}

impl HostNft {
    pub fn new() -> Self {
        Self {
            program: "nft".to_string(),
            timeout: Duration::from_secs(30),
        }
    }

    async fn run(&self, args: &[&str], stdin: Option<&str>) -> Result<(i64, String), NftError> {
        let location = self.location();
        let mut command = tokio::process::Command::new(&self.program);
        command
            .args(args)
            .stdin(if stdin.is_some() {
                std::process::Stdio::piped()
            } else {
                std::process::Stdio::null()
            })
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .kill_on_drop(true);
        let spawn_error = |e: std::io::Error| NftError::Spawn {
            location: location.clone(),
            program: self.program.clone(),
            reason: e.to_string(),
        };
        let mut child = command.spawn().map_err(spawn_error)?;
        if let (Some(script), Some(mut pipe)) = (stdin, child.stdin.take()) {
            pipe.write_all(script.as_bytes())
                .await
                .map_err(spawn_error)?;
            drop(pipe);
        }
        let output = tokio::time::timeout(self.timeout, child.wait_with_output())
            .await
            .map_err(|_| NftError::Timeout {
                location: location.clone(),
                timeout_secs: self.timeout.as_secs(),
            })?
            .map_err(spawn_error)?;
        let mut text = String::from_utf8_lossy(&output.stdout).into_owned();
        text.push_str(&String::from_utf8_lossy(&output.stderr));
        Ok((i64::from(output.status.code().unwrap_or(-1)), text))
    }
}

#[async_trait]
impl NftExecutor for HostNft {
    async fn apply(&self, ruleset: &str) -> Result<(), NftError> {
        let value = tokio::fs::read_to_string(BRIDGE_NF_SYSCTL)
            .await
            .map(|v| v.trim().to_string())
            .unwrap_or_else(|_| "missing".to_string());
        if value != "1" {
            return Err(NftError::BridgeNetfilterDisabled {
                location: self.location(),
                value,
            });
        }
        let (status, output) = self.run(&["-f", "-"], Some(ruleset)).await?;
        rejected_unless_ok(self.location(), status, output)
    }

    async fn remove(&self, table: &TableName) -> Result<(), NftError> {
        let (status, output) = self
            .run(&["-f", "-"], Some(&removal_ruleset(table)))
            .await?;
        rejected_unless_ok(self.location(), status, output)
    }

    async fn list(&self, table: &TableName) -> Result<Option<String>, NftError> {
        let (status, output) = self
            .run(&["list", "table", "inet", table.as_str()], None)
            .await?;
        listing(self.location(), status, output)
    }

    fn location(&self) -> String {
        "this host".to_string()
    }
}

/// `nft` inside the Docker VM, through a helper container.
pub struct DockerVmNft {
    docker: Docker,
    image: String,
    base_image: String,
    timeout: Duration,
    /// One helper at a time: applies are rare and must not interleave.
    serial: Mutex<()>,
}

/// The helper image: Alpine with nftables, built once per VM.
pub const HELPER_IMAGE: &str = "temps-netpol-nft:v1";
const HELPER_BASE_IMAGE: &str = "alpine:3.20";

static HELPER_SEQ: AtomicU64 = AtomicU64::new(0);

fn helper_name(purpose: &str) -> String {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or_default();
    format!(
        "temps-netpol-{purpose}-{}-{nanos}-{}",
        std::process::id(),
        HELPER_SEQ.fetch_add(1, Ordering::Relaxed)
    )
}

impl DockerVmNft {
    pub fn new(docker: Docker) -> Self {
        Self {
            docker,
            image: HELPER_IMAGE.to_string(),
            base_image: HELPER_BASE_IMAGE.to_string(),
            timeout: Duration::from_secs(120),
            serial: Mutex::new(()),
        }
    }

    fn docker_error(operation: &str) -> impl Fn(bollard::errors::Error) -> NftError + '_ {
        move |e| NftError::Docker {
            operation: operation.to_string(),
            reason: e.to_string(),
        }
    }

    /// Builds the helper image if this VM does not have it yet.
    pub async fn ensure_helper_image(&self) -> Result<(), NftError> {
        if self.docker.inspect_image(&self.image).await.is_ok() {
            return Ok(());
        }
        let image_error = |reason: String| NftError::HelperImage {
            image: self.image.clone(),
            reason,
        };
        if self.docker.inspect_image(&self.base_image).await.is_err() {
            let options = CreateImageOptionsBuilder::new()
                .from_image(&self.base_image)
                .build();
            let mut pull = self.docker.create_image(Some(options), None, None);
            while let Some(progress) = pull.next().await {
                progress.map_err(|e| image_error(format!("pulling {}: {e}", self.base_image)))?;
            }
        }
        info!(image = %self.image, "building the nft helper image in the Docker VM");
        let name = helper_name("build");
        let (status, output) = self
            .run_container(
                &name,
                &self.base_image,
                &["apk", "add", "--no-cache", "nftables"],
                false,
                None,
            )
            .await
            .map_err(|e| image_error(e.to_string()))?;
        let result = if status == 0 {
            let (repo, tag) = self
                .image
                .split_once(':')
                .unwrap_or((&self.image, "latest"));
            let options = CommitContainerOptionsBuilder::new()
                .container(&name)
                .repo(repo)
                .tag(tag)
                .build();
            self.docker
                .commit_container(options, ContainerConfig::default())
                .await
                .map(|_| ())
                .map_err(|e| image_error(format!("committing {name}: {e}")))
        } else {
            Err(image_error(format!(
                "'apk add nftables' exited {status}: {output}"
            )))
        };
        self.remove_container(&name).await;
        result
    }

    async fn remove_container(&self, name: &str) {
        let options = RemoveContainerOptions {
            force: true,
            ..Default::default()
        };
        if let Err(e) = self.docker.remove_container(name, Some(options)).await {
            debug!(container = name, error = %e, "nft helper cleanup failed");
        }
    }

    /// Creates, optionally uploads `/netpol.nft`, starts and waits for a
    /// container. Leaves it in place: the caller removes it.
    async fn run_container(
        &self,
        name: &str,
        image: &str,
        cmd: &[&str],
        privileged_net: bool,
        upload: Option<&str>,
    ) -> Result<(i64, String), NftError> {
        let host_config = if privileged_net {
            HostConfig {
                network_mode: Some("host".to_string()),
                cap_drop: Some(vec!["ALL".to_string()]),
                cap_add: Some(vec!["NET_ADMIN".to_string()]),
                security_opt: Some(vec!["no-new-privileges".to_string()]),
                ..Default::default()
            }
        } else {
            HostConfig::default()
        };
        let body = ContainerCreateBody {
            image: Some(image.to_string()),
            cmd: Some(cmd.iter().map(|s| s.to_string()).collect()),
            labels: Some([("sh.temps.netpol.helper".to_string(), "1".to_string())].into()),
            host_config: Some(host_config),
            ..Default::default()
        };
        self.docker
            .create_container(
                Some(CreateContainerOptionsBuilder::new().name(name).build()),
                body,
            )
            .await
            .map_err(Self::docker_error("container create"))?;
        if let Some(ruleset) = upload {
            let archive = ruleset_archive(ruleset).map_err(|e| NftError::Docker {
                operation: "ruleset archive".to_string(),
                reason: e.to_string(),
            })?;
            let options = UploadToContainerOptionsBuilder::new().path("/").build();
            self.docker
                .upload_to_container(name, Some(options), bollard::body_full(archive.into()))
                .await
                .map_err(Self::docker_error("ruleset upload"))?;
        }
        self.docker
            .start_container(name, None)
            .await
            .map_err(Self::docker_error("container start"))?;
        let wait = async {
            let mut wait = self
                .docker
                .wait_container(name, None::<WaitContainerOptions>);
            match wait.next().await {
                Some(Ok(status)) => Ok(status.status_code),
                Some(Err(bollard::errors::Error::DockerContainerWaitError { code, .. })) => {
                    Ok(code)
                }
                Some(Err(e)) => Err(Self::docker_error("container wait")(e)),
                None => Ok(-1),
            }
        };
        let status = tokio::time::timeout(self.timeout, wait)
            .await
            .map_err(|_| NftError::Timeout {
                location: self.location(),
                timeout_secs: self.timeout.as_secs(),
            })??;
        let mut logs = self.docker.logs(
            name,
            Some(LogsOptions {
                stdout: true,
                stderr: true,
                ..Default::default()
            }),
        );
        let mut output = String::new();
        while let Some(chunk) = logs.next().await {
            let chunk = chunk.map_err(Self::docker_error("container logs"))?;
            output.push_str(&String::from_utf8_lossy(&chunk.into_bytes()));
        }
        Ok((status, output))
    }

    async fn run_nft(
        &self,
        script: &str,
        ruleset: Option<&str>,
    ) -> Result<(i64, String), NftError> {
        let _serial = self.serial.lock().await;
        self.ensure_helper_image().await?;
        let name = helper_name("nft");
        let result = self
            .run_container(
                &name,
                &self.image,
                &["/bin/sh", "-c", script],
                true,
                ruleset,
            )
            .await;
        self.remove_container(&name).await;
        result
    }
}

/// A tar archive holding `/netpol.nft`.
fn ruleset_archive(ruleset: &str) -> std::io::Result<Vec<u8>> {
    let mut builder = tar::Builder::new(Vec::new());
    let mut header = tar::Header::new_gnu();
    header.set_size(ruleset.len() as u64);
    header.set_mode(0o600);
    header.set_cksum();
    builder.append_data(&mut header, "netpol.nft", ruleset.as_bytes())?;
    builder.into_inner()
}

#[async_trait]
impl NftExecutor for DockerVmNft {
    async fn apply(&self, ruleset: &str) -> Result<(), NftError> {
        let script = format!(
            "v=$(cat {BRIDGE_NF_SYSCTL} 2>/dev/null || echo missing); \
             if [ \"$v\" != 1 ]; then echo \"$v\"; exit {EXIT_BRIDGE_NF_DISABLED}; fi; \
             exec nft -f /netpol.nft"
        );
        let (status, output) = self.run_nft(&script, Some(ruleset)).await?;
        if status == EXIT_BRIDGE_NF_DISABLED {
            return Err(NftError::BridgeNetfilterDisabled {
                location: self.location(),
                value: output.trim().to_string(),
            });
        }
        rejected_unless_ok(self.location(), status, output)
    }

    async fn remove(&self, table: &TableName) -> Result<(), NftError> {
        let (status, output) = self
            .run_nft("exec nft -f /netpol.nft", Some(&removal_ruleset(table)))
            .await?;
        rejected_unless_ok(self.location(), status, output)
    }

    async fn list(&self, table: &TableName) -> Result<Option<String>, NftError> {
        let script = format!("exec nft list table inet {table}");
        let (status, output) = self.run_nft(&script, None).await?;
        listing(self.location(), status, output)
    }

    fn location(&self) -> String {
        "the Docker VM".to_string()
    }
}

fn rejected_unless_ok(location: String, status: i64, output: String) -> Result<(), NftError> {
    if status == 0 {
        Ok(())
    } else {
        Err(NftError::Rejected {
            location,
            status,
            output: output.trim().to_string(),
        })
    }
}

fn listing(location: String, status: i64, output: String) -> Result<Option<String>, NftError> {
    if status == 0 {
        Ok(Some(output))
    } else if output.contains("No such file or directory") {
        Ok(None)
    } else {
        Err(NftError::Rejected {
            location,
            status,
            output: output.trim().to_string(),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn helper_names_are_unique() {
        assert_ne!(helper_name("nft"), helper_name("nft"));
        assert!(helper_name("nft").starts_with("temps-netpol-nft-"));
    }

    #[test]
    fn the_ruleset_archive_holds_one_private_file() {
        let bytes = ruleset_archive("table inet t {}\n").unwrap();
        let mut archive = tar::Archive::new(bytes.as_slice());
        let entries: Vec<_> = archive
            .entries()
            .unwrap()
            .map(|e| {
                let e = e.unwrap();
                (
                    e.path().unwrap().display().to_string(),
                    e.header().mode().unwrap(),
                    e.size(),
                )
            })
            .collect();
        assert_eq!(entries, [("netpol.nft".to_string(), 0o600, 16)]);
    }

    #[test]
    fn a_missing_table_lists_as_none() {
        let missing = "Error: No such file or directory\nlist table inet t\n".to_string();
        assert_eq!(listing("x".into(), 1, missing).unwrap(), None);
        assert!(matches!(
            listing("x".into(), 1, "syntax error".into()),
            Err(NftError::Rejected { status: 1, .. })
        ));
        assert_eq!(
            listing("x".into(), 0, "table".into()).unwrap(),
            Some("table".into())
        );
    }

    #[test]
    fn errors_say_where_and_how_to_fix() {
        let e = NftError::BridgeNetfilterDisabled {
            location: "the Docker VM".into(),
            value: "0".into(),
        };
        let text = e.to_string();
        assert!(
            text.contains("the Docker VM") && text.contains("br_netfilter"),
            "{text}"
        );
    }
}
