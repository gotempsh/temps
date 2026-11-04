// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! The names the Docker sandbox provider shares with the preview gateway
//! reconciler in `temps-ai-agents`.

use std::collections::BTreeSet;
use std::sync::Arc;

/// Container name for the singleton gateway on this host.
pub const PREVIEW_GATEWAY_CONTAINER: &str = "temps-preview-gateway";

pub const PREVIEW_GATEWAY_LABEL: &str = "sh.temps.preview-gateway";

/// This instance's gateway container name as the sandbox provider uses it,
/// shared with the gateway's settings endpoint so a rename reaches the
/// sandboxes created after it without a restart.
///
/// Every sandbox network is labelled with this name, and Docker cannot
/// relabel a network, so a rename waits until this instance has no sandbox
/// network on this host. A label does not say which instance created the
/// network (two instances may have used the same gateway name), so the
/// instance's own records decide: its sandbox rows, and the networks its
/// provider set up in this process. Only the second covers sandboxes created
/// without a row, such as credential checks.
///
/// The provider holds the read lock from labelling a sandbox's network until
/// the gateway is attached to it, and records the network in between. A
/// rename holds the write lock from checking those records until the new
/// name is set, so a network set up meanwhile waits and gets the new name.
#[derive(Debug, Clone)]
pub struct GatewayName {
    name: Arc<tokio::sync::RwLock<String>>,
    networks: Arc<tokio::sync::Mutex<BTreeSet<String>>>,
}

impl GatewayName {
    pub fn new(name: String) -> Self {
        Self {
            name: Arc::new(tokio::sync::RwLock::new(name)),
            networks: Arc::default(),
        }
    }

    /// Hold while labelling a sandbox network and attaching the gateway.
    pub async fn read(&self) -> tokio::sync::RwLockReadGuard<'_, String> {
        self.name.read().await
    }

    /// Hold from checking that this instance has no sandbox network until
    /// the new name is set.
    pub async fn write(&self) -> tokio::sync::RwLockWriteGuard<'_, String> {
        self.name.write().await
    }

    /// Record a sandbox network the provider labelled with the name.
    pub async fn network_set_up(&self, network: &str) {
        self.networks.lock().await.insert(network.to_string());
    }

    /// Forget a sandbox network the provider removed.
    pub async fn network_removed(&self, network: &str) {
        self.networks.lock().await.remove(network);
    }

    /// The sandbox networks the provider set up in this process and has not
    /// removed. Some may have been removed outside Temps since.
    pub async fn networks_set_up(&self) -> Vec<String> {
        self.networks.lock().await.iter().cloned().collect()
    }
}

impl Default for GatewayName {
    fn default() -> Self {
        Self::new(PREVIEW_GATEWAY_CONTAINER.to_string())
    }
}
