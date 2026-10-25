// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Plugin builds on a joined worker node, for a control plane with no Docker.

use std::sync::Arc;

use async_trait::async_trait;
use temps_config::ConfigService;
use temps_core::plugin_builder::{PluginBuildError, PluginBuildRequest, RemotePluginBuilder};
use temps_core::EncryptionService;
use temps_deployer::remote::PluginBuildAttemptError;

use super::NodeScheduler;

/// Largest compiled plugin a worker may return.
const MAX_BINARY_BYTES: u64 = 256 * 1024 * 1024;

pub struct NodePluginBuilder {
    scheduler: Arc<NodeScheduler>,
    config_service: Arc<ConfigService>,
    encryption_service: Arc<EncryptionService>,
}

impl NodePluginBuilder {
    pub fn new(
        scheduler: Arc<NodeScheduler>,
        config_service: Arc<ConfigService>,
        encryption_service: Arc<EncryptionService>,
    ) -> Self {
        Self {
            scheduler,
            config_service,
            encryption_service,
        }
    }
}

#[async_trait]
impl RemotePluginBuilder for NodePluginBuilder {
    async fn build(&self, request: &PluginBuildRequest) -> Result<(), PluginBuildError> {
        let nodes = self.scheduler.plugin_build_nodes().await.map_err(|error| {
            tracing::warn!(%error, "could not list the nodes available to build a plugin");
            PluginBuildError::NoBuilder(
                "The list of worker nodes could not be read; try again shortly.".to_string(),
            )
        })?;
        if nodes.is_empty() {
            return Err(PluginBuildError::NoBuilder(
                "This server has no Docker or Podman and no worker node is connected. Join a worker node that has Docker (Worker Nodes → Add node), then install the plugin again."
                    .to_string(),
            ));
        }
        let mut refused = Vec::new();
        for node in nodes {
            let Some(encrypted_token) = node.token_encrypted.clone() else {
                refused.push(format!(
                    "worker '{}' has no agent token; re-register it",
                    node.name
                ));
                continue;
            };
            let token = match self
                .encryption_service
                .decrypt(&encrypted_token)
                .ok()
                .and_then(|bytes| String::from_utf8(bytes).ok())
            {
                Some(token) => token,
                None => {
                    refused.push(format!(
                        "worker '{}' has an unreadable agent token",
                        node.name
                    ));
                    continue;
                }
            };
            let deployer = match crate::cluster_ca::build_node_deployer(
                &node,
                token,
                self.config_service.as_ref(),
                self.encryption_service.as_ref(),
            )
            .await
            {
                Ok(deployer) => deployer,
                Err(error) => {
                    refused.push(format!("worker '{}': {error}", node.name));
                    continue;
                }
            };
            tracing::info!(node = %node.name, target = %request.bun_target, "building a plugin on a worker node");
            match deployer
                .build_plugin(
                    &request.source_archive,
                    &request.bun_target,
                    &request.output,
                    MAX_BINARY_BYTES,
                )
                .await
            {
                Ok(()) => return Ok(()),
                Err(PluginBuildAttemptError::Failed(reason)) => {
                    return Err(PluginBuildError::Failed(reason));
                }
                Err(PluginBuildAttemptError::NotAvailable(reason)) => {
                    tracing::info!(node = %node.name, %reason, "worker cannot build the plugin; trying the next one");
                    refused.push(reason);
                }
            }
        }
        Err(PluginBuildError::NoBuilder(format!(
            "No connected worker node could build the plugin ({}). A worker needs Docker or Podman and a current Temps agent.",
            refused.join("; ")
        )))
    }
}
