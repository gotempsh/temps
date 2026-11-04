// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Lazy per-cluster CA provisioning for multi-node mTLS (ADR-020 WS-2.1).
//!
//! The control plane mints ONE per-cluster CA the first time it needs to sign a
//! worker CSR, stores the CA cert (public) and the AES-256-GCM-encrypted CA key
//! in `settings.multi_node`, and reuses it thereafter. The CA cert is handed to
//! nodes at enrollment as their trust root; the CA private key never leaves the
//! control plane.
//!
//! The clients themselves live in [`temps_config::node_client`], below this
//! crate, so the sandbox node routing can use them; only the deployer needs
//! `temps-deployer`.

use temps_config::ConfigService;
use temps_core::EncryptionService;
use temps_deployer::remote::RemoteNodeDeployer;
use temps_deployer::DeployerError;

pub use temps_config::cluster_ca::{ensure_cluster_ca, ClusterCa};
pub use temps_config::node_client::{
    build_node_http_client, cp_client_identity, cp_ws_client_config, is_https_address,
    node_agent_endpoint, ClusterCaError, NODE_CONNECT_TIMEOUT,
};

/// Build a `ContainerDeployer` for a remote node, transparently using mutual TLS
/// when the node's `address` is `https://` (ADR-020 WS-2.1) and plain HTTP
/// otherwise. This is the single place every CP→agent deployer is constructed so
/// no call site can accidentally fall back to plaintext against an mTLS node.
/// The agent is dialed at [`node_agent_endpoint`].
pub async fn build_node_deployer(
    node: &temps_entities::nodes::Model,
    token: String,
    config_service: &ConfigService,
    encryption_service: &EncryptionService,
) -> Result<RemoteNodeDeployer, DeployerError> {
    let node_name = node.name.clone();
    let endpoint = node_agent_endpoint(node);
    let sealing = node.request_sealing();
    if is_https_address(&endpoint.dial_url) {
        let ca = ensure_cluster_ca(config_service, encryption_service)
            .await
            .map_err(|e| {
                DeployerError::NetworkError(format!(
                    "cluster CA unavailable for node {}: {}",
                    node_name, e
                ))
            })?;
        let identity = cp_client_identity(&ca).map_err(|e| {
            DeployerError::NetworkError(format!(
                "control-plane client identity unavailable for node {}: {}",
                node_name, e
            ))
        })?;
        RemoteNodeDeployer::new_mtls_at(&endpoint, token, node_name, &identity, &ca.cert_pem)
            .map(|deployer| deployer.with_sealing(sealing))
    } else {
        RemoteNodeDeployer::new(endpoint.dial_url, token, node_name)
            .map(|deployer| deployer.with_sealing(sealing))
    }
}
