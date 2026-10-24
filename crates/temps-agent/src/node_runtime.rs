// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! The node runtime on a worker (ADR-055 §7).
//!
//! A worker that advertises the `state_stream` capability reads its desired
//! state from `GET /api/internal/nodes/{id}/state` (a long poll), keeps a
//! 0600 snapshot of it under `<agent data dir>/node-runtime`, and reconciles
//! the instances assigned to it against the local Docker daemon. What it
//! observed rides on the heartbeat (see `server::spawn_heartbeat_loop`).
//!
//! The `/agent/*` API is unchanged: control planes that predate the stream
//! keep driving this agent through it, and the control plane still uses it
//! to move images onto the node.

use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use temps_workload::stream::{InstanceEnv, StateAck, StateResponse};
use temps_workload_node::{ControlPlaneLink, LinkError, NodeRuntime};

use crate::AgentConfig;

/// The capability a worker advertises in `capacity.node_capabilities`.
pub const STATE_STREAM_CAPABILITY: &str = "state_stream";

/// How long the control plane may hold a stream read open.
const STREAM_WAIT_SECS: u64 = 25;
/// The HTTP timeout of a stream read: the long poll plus headroom.
const STREAM_TIMEOUT: Duration = Duration::from_secs(STREAM_WAIT_SECS + 15);
/// The HTTP timeout of acks and environment reads.
const CALL_TIMEOUT: Duration = Duration::from_secs(15);
/// How often instances are reconciled when nothing else wakes the loop.
const RECONCILE_INTERVAL: Duration = Duration::from_secs(5);
const BACKOFF_START: Duration = Duration::from_secs(1);
const BACKOFF_MAX: Duration = Duration::from_secs(30);
/// The plane's scope label value on this node's containers.
const SCOPE: &str = "temps";

/// Errors starting the node runtime. The agent keeps running without it.
#[derive(Debug, thiserror::Error)]
pub enum NodeRuntimeStartError {
    #[error("node {node_id}: cannot resolve the node runtime directory: {reason}")]
    Directory { node_id: i32, reason: String },
    #[error(
        "node {node_id}: cannot build the control-plane client for the state stream: {reason}"
    )]
    Client { node_id: i32, reason: String },
    #[error("node {node_id}: cannot open the node runtime state: {reason}")]
    Open { node_id: i32, reason: String },
}

/// The control plane over HTTP, authenticated with the node's token.
pub struct HttpControlPlaneLink {
    client: reqwest::Client,
    base: String,
    token: String,
}

impl HttpControlPlaneLink {
    pub fn new(config: &AgentConfig) -> Result<Self, NodeRuntimeStartError> {
        let client = crate::control_plane_client_builder(config)
            .connect_timeout(Duration::from_secs(10))
            .build()
            .map_err(|e| NodeRuntimeStartError::Client {
                node_id: config.node_id,
                reason: e.to_string(),
            })?;
        Ok(Self {
            client,
            base: format!(
                "{}/api/internal/nodes/{}",
                config.control_plane_url.trim_end_matches('/'),
                config.node_id
            ),
            token: config.token.clone(),
        })
    }

    async fn refused(operation: &'static str, response: reqwest::Response) -> LinkError {
        let status = response.status().as_u16();
        // The body is a Problem document; it never carries a secret.
        let mut detail = response.text().await.unwrap_or_default();
        detail.truncate(512);
        LinkError::Refused {
            operation,
            status,
            detail,
        }
    }
}

fn unreachable(error: reqwest::Error) -> LinkError {
    LinkError::Unreachable {
        reason: error.to_string(),
    }
}

#[async_trait]
impl ControlPlaneLink for HttpControlPlaneLink {
    async fn fetch_state(&self, since: &str) -> Result<StateResponse, LinkError> {
        let response = self
            .client
            .get(format!("{}/state", self.base))
            .query(&[
                ("since", since.to_string()),
                ("wait_secs", STREAM_WAIT_SECS.to_string()),
            ])
            .bearer_auth(&self.token)
            .timeout(STREAM_TIMEOUT)
            .send()
            .await
            .map_err(unreachable)?;
        if !response.status().is_success() {
            return Err(Self::refused("state", response).await);
        }
        response.json().await.map_err(unreachable)
    }

    async fn ack(&self, ack: &StateAck) -> Result<(), LinkError> {
        let response = self
            .client
            .post(format!("{}/state/ack", self.base))
            .bearer_auth(&self.token)
            .timeout(CALL_TIMEOUT)
            .json(ack)
            .send()
            .await
            .map_err(unreachable)?;
        if !response.status().is_success() {
            return Err(Self::refused("ack", response).await);
        }
        Ok(())
    }

    async fn instance_env(&self, instance_id: i64) -> Result<InstanceEnv, LinkError> {
        let response = self
            .client
            .get(format!("{}/instances/{instance_id}/env", self.base))
            .bearer_auth(&self.token)
            .timeout(CALL_TIMEOUT)
            .send()
            .await
            .map_err(unreachable)?;
        if !response.status().is_success() {
            return Err(Self::refused("instance environment", response).await);
        }
        response.json().await.map_err(unreachable)
    }
}

/// `<agent data dir>/node-runtime`, next to `dns` and `sandboxes`.
pub fn node_runtime_dir(config: &AgentConfig) -> Result<std::path::PathBuf, NodeRuntimeStartError> {
    let sandboxes = crate::sandbox_work_root_for(&config.dns_data_dir).map_err(|e| {
        NodeRuntimeStartError::Directory {
            node_id: config.node_id,
            reason: e.to_string(),
        }
    })?;
    let data_dir = sandboxes
        .parent()
        .ok_or_else(|| NodeRuntimeStartError::Directory {
            node_id: config.node_id,
            reason: format!("'{}' has no parent directory", sandboxes.display()),
        })?;
    Ok(data_dir.join("node-runtime"))
}

/// Open the node runtime and start its stream and reconcile loops.
pub fn spawn(
    config: &AgentConfig,
    docker: bollard::Docker,
    bind_address: crate::network_sync::SharedBindAddress,
    overlay_bridge_address: Arc<std::sync::RwLock<Option<std::net::IpAddr>>>,
) -> Result<Arc<NodeRuntime>, NodeRuntimeStartError> {
    let dir = node_runtime_dir(config)?;
    let link: Arc<dyn ControlPlaneLink> = Arc::new(HttpControlPlaneLink::new(config)?);
    let driver: Arc<dyn temps_workload::RuntimeDriver> =
        // Secret files live beside the snapshot, under the agent's data
        // directory, so their bind mounts survive a reboot.
        Arc::new(temps_workload_docker::DockerDriver::new(docker).with_secrets_root(dir.join("secret-files")));
    let runtime = Arc::new(
        NodeRuntime::open(config.node_id, &dir, SCOPE, driver, link)
            .map_err(|e| NodeRuntimeStartError::Open {
                node_id: config.node_id,
                reason: e.to_string(),
            })?
            // The same slot the legacy deployer binds published ports to:
            // this node's private address, never every interface.
            .with_publish_host_ip(Arc::new(move || {
                bind_address
                    .read()
                    .map(|address| address.clone())
                    .unwrap_or_else(|poisoned| poisoned.into_inner().clone())
            }))
            // The resolver the legacy handlers give their containers: this
            // node's Hickory resolver once `network_sync` has the bridge up,
            // with public fallbacks so a stopped resolver never takes DNS
            // down for every instance on the node.
            .with_cluster_dns(temps_workload::ResolverSource::new(move || {
                overlay_bridge_address
                    .read()
                    .ok()
                    .and_then(|slot| slot.map(|ip| vec![ip.to_string()]))
                    .map(temps_deployer::docker::dns_with_fallback)
            })),
    );
    let reconcile_now = Arc::new(tokio::sync::Notify::new());
    tokio::spawn(stream_loop(runtime.clone(), reconcile_now.clone()));
    tokio::spawn(reconcile_loop(runtime.clone(), reconcile_now));
    tracing::info!(
        node_id = config.node_id,
        snapshot = %runtime.snapshot_path().display(),
        "node runtime started; reading desired state from the control plane's state stream"
    );
    Ok(runtime)
}

/// Read the stream for the life of the process, backing off while the
/// control plane is unreachable. Every applied answer, and every
/// reconnection, wakes the reconcile loop (an instance waiting for its
/// secrets is retried then).
async fn stream_loop(runtime: Arc<NodeRuntime>, reconcile_now: Arc<tokio::sync::Notify>) {
    let node_id = runtime.node_id();
    let mut backoff = BACKOFF_START;
    let mut failing = false;
    loop {
        match runtime.sync_once().await {
            Ok(_more) => {
                if failing {
                    tracing::info!(node_id, "the state stream reconnected");
                    failing = false;
                }
                backoff = BACKOFF_START;
                reconcile_now.notify_one();
            }
            Err(error) => {
                if !failing {
                    tracing::warn!(node_id, "reading the state stream failed: {error}");
                } else {
                    tracing::debug!(node_id, "reading the state stream failed: {error}");
                }
                failing = true;
                tokio::time::sleep(backoff).await;
                backoff = (backoff * 2).min(BACKOFF_MAX);
            }
        }
    }
}

async fn reconcile_loop(runtime: Arc<NodeRuntime>, reconcile_now: Arc<tokio::sync::Notify>) {
    let node_id = runtime.node_id();
    loop {
        let outcome = runtime.reconcile().await;
        if outcome.created + outcome.removed + outcome.stopped + outcome.errors > 0 {
            tracing::debug!(
                node_id,
                created = outcome.created,
                stopped = outcome.stopped,
                removed = outcome.removed,
                waiting_for_secrets = outcome.waiting_for_secrets,
                errors = outcome.errors,
                "node runtime reconcile pass"
            );
        }
        tokio::select! {
            _ = tokio::time::sleep(RECONCILE_INTERVAL) => {}
            _ = reconcile_now.notified() => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn config(dns_data_dir: &str) -> AgentConfig {
        serde_json::from_value(serde_json::json!({
            "listen_address": "127.0.0.1:3100",
            "token": "node-token",
            "node_name": "worker-a",
            "control_plane_url": "https://cp.example.test/",
            "node_id": 7,
            "dns_data_dir": dns_data_dir,
        }))
        .unwrap_or_else(|e| panic!("test config: {e}"))
    }

    #[test]
    fn the_runtime_lives_next_to_the_agents_other_state() {
        let dir =
            node_runtime_dir(&config("/var/lib/temps/dns")).unwrap_or_else(|e| panic!("dir: {e}"));
        assert_eq!(dir, std::path::PathBuf::from("/var/lib/temps/node-runtime"));
    }

    #[test]
    fn a_root_dns_dir_is_refused_rather_than_guessed() {
        assert!(matches!(
            node_runtime_dir(&config("/")),
            Err(NodeRuntimeStartError::Directory { node_id: 7, .. })
        ));
    }

    #[test]
    fn the_link_addresses_this_nodes_internal_routes() {
        let link = HttpControlPlaneLink::new(&config("/var/lib/temps/dns"))
            .unwrap_or_else(|e| panic!("link: {e}"));
        assert_eq!(link.base, "https://cp.example.test/api/internal/nodes/7");
    }
}
