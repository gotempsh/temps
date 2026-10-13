// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Lazy per-cluster CA provisioning for multi-node mTLS (ADR-020 WS-2.1).
//!
//! The control plane mints ONE per-cluster CA the first time it needs to sign a
//! worker CSR, stores the CA cert (public) and the AES-256-GCM-encrypted CA key
//! in `settings.multi_node`, and reuses it thereafter. The CA cert is handed to
//! nodes at enrollment as their trust root; the CA private key never leaves the
//! control plane.

use temps_config::ConfigService;
use temps_core::EncryptionService;
use temps_deployer::remote::RemoteNodeDeployer;
use temps_deployer::DeployerError;

pub use temps_config::cluster_ca::{ensure_cluster_ca, ClusterCa};

#[derive(Debug, thiserror::Error)]
pub enum ClusterCaError {
    #[error(transparent)]
    Provisioning(#[from] temps_config::cluster_ca::ClusterCaError),
    #[error("Cluster certificate operation failed: {0}")]
    Pki(String),
    #[error("Failed to build node HTTP client: {0}")]
    Client(String),
}

/// Mint a control-plane CLIENT identity (cert + key) signed by the cluster CA,
/// returned as a single combined PEM suitable for `reqwest::Identity::from_pem`
/// (ADR-020 WS-2.1). The CP presents this when calling agents over mTLS; the
/// agent accepts it because it chains to the cluster CA. Generated on demand —
/// cheap (one keygen + one signature) and avoids persisting another secret.
pub fn cp_client_identity(ca: &ClusterCa) -> Result<String, ClusterCaError> {
    // A client cert needs no hostname SAN — the agent verifies only that it
    // chains to the cluster CA, not its name. Empty SANs also mean this cert can
    // never pass server-name verification, so it can't be reused to impersonate
    // a node's TLS server.
    let csr = temps_core::node_pki::generate_node_keypair_csr("temps-control-plane", &[])
        .map_err(|e| ClusterCaError::Pki(e.to_string()))?;
    let signed = temps_core::node_pki::sign_node_csr(&ca.cert_pem, &ca.key_pem, &csr.csr_pem, &[])
        .map_err(|e| ClusterCaError::Pki(e.to_string()))?;
    // reqwest's PEM Identity wants the cert chain followed by the private key.
    Ok(format!("{}\n{}", signed.cert_pem, csr.key_pem))
}

/// Build a rustls `ClientConfig` for mutual-TLS **WebSocket** connections to an
/// agent (ADR-020 WS-2.1). The terminal proxy dials the agent with
/// tokio-tungstenite rather than reqwest, so it needs a rustls config directly:
/// it presents the CP's cluster-CA-signed client identity and trusts ONLY the
/// cluster CA. Relies on the process-default crypto provider the CLI installs at
/// startup (same as every other `ClientConfig::builder()` in the workspace).
pub async fn cp_ws_client_config(
    config_service: &ConfigService,
    encryption_service: &EncryptionService,
) -> Result<rustls::ClientConfig, ClusterCaError> {
    use rustls::pki_types::{CertificateDer, PrivateKeyDer};
    use std::io::BufReader;

    let ca = ensure_cluster_ca(config_service, encryption_service).await?;
    // Combined PEM (cert chain followed by key) — each parser ignores the
    // other's blocks, so we feed the same buffer to both.
    let identity_pem = cp_client_identity(&ca)?;

    let cert_chain: Vec<CertificateDer<'static>> =
        rustls_pemfile::certs(&mut BufReader::new(identity_pem.as_bytes()))
            .collect::<Result<_, _>>()
            .map_err(|e| ClusterCaError::Client(format!("parse CP cert chain: {e}")))?;
    let key: PrivateKeyDer<'static> =
        rustls_pemfile::private_key(&mut BufReader::new(identity_pem.as_bytes()))
            .map_err(|e| ClusterCaError::Client(format!("parse CP private key: {e}")))?
            .ok_or_else(|| ClusterCaError::Client("control-plane identity has no key".into()))?;

    let mut roots = rustls::RootCertStore::empty();
    for cert in rustls_pemfile::certs(&mut BufReader::new(ca.cert_pem.as_bytes())) {
        let cert = cert.map_err(|e| ClusterCaError::Client(format!("parse cluster CA: {e}")))?;
        roots
            .add(cert)
            .map_err(|e| ClusterCaError::Client(format!("add cluster CA root: {e}")))?;
    }

    rustls::ClientConfig::builder()
        .with_root_certificates(roots)
        .with_client_auth_cert(cert_chain, key)
        .map_err(|e| ClusterCaError::Client(format!("build WS client config: {e}")))
}

/// TCP/TLS connect timeout for control-plane → node HTTP clients.
pub const NODE_CONNECT_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(10);

/// Build a raw `reqwest::Client` for talking to a node's agent over HTTP(S),
/// transparently using mutual TLS when `address` is `https://` (ADR-020
/// WS-2.1): the control plane presents a cluster-CA-signed client identity and
/// pins the agent's server cert to the cluster CA (built-in roots disabled).
/// Plain `http://` nodes fall back to the shared `insecure_tls` toggle. Pass
/// `timeout = None` for long-lived streams (e.g. log following), `Some(_)` for
/// bounded requests.
///
/// This is the streaming/raw-HTTP analogue of [`build_node_deployer`] for the
/// CP→agent paths that don't go through `RemoteNodeDeployer` (log streaming,
/// edge-analytics ingest) so they don't silently fall back to plaintext
/// against an mTLS-enforcing node.
pub async fn build_node_http_client(
    address: &str,
    config_service: &ConfigService,
    encryption_service: &EncryptionService,
    timeout: Option<std::time::Duration>,
) -> Result<reqwest::Client, ClusterCaError> {
    // A worker that accepts the TCP connection but never answers must not
    // hold a caller for the whole request timeout, and nothing on a node
    // legitimately redirects: following one would replay the bearer token.
    let mut builder = reqwest::Client::builder()
        .connect_timeout(NODE_CONNECT_TIMEOUT)
        .redirect(reqwest::redirect::Policy::none());
    if let Some(t) = timeout {
        builder = builder.timeout(t);
    }
    if is_https_address(address) {
        let ca = ensure_cluster_ca(config_service, encryption_service).await?;
        let identity_pem = cp_client_identity(&ca)?;
        let identity = reqwest::Identity::from_pem(identity_pem.as_bytes())
            .map_err(|e| ClusterCaError::Client(format!("invalid control-plane identity: {e}")))?;
        let ca_cert = reqwest::Certificate::from_pem(ca.cert_pem.as_bytes())
            .map_err(|e| ClusterCaError::Client(format!("invalid cluster CA certificate: {e}")))?;
        builder = builder
            .use_rustls_tls()
            .identity(identity)
            .add_root_certificate(ca_cert)
            .tls_built_in_root_certs(false);
    } else {
        builder = builder.danger_accept_invalid_certs(temps_core::tls::insecure_tls_enabled());
    }
    builder
        .build()
        .map_err(|e| ClusterCaError::Client(e.to_string()))
}

/// Whether a node agent address uses TLS (`https://`). The scheme is
/// case-insensitive (RFC 3986), so `HTTPS://` must get the cluster CA and the
/// control plane's client identity too: every caller deciding whether to talk
/// mTLS to a node goes through this one check, so they can never disagree.
pub fn is_https_address(address: &str) -> bool {
    address
        .trim_start()
        .get(..8)
        .is_some_and(|scheme| scheme.eq_ignore_ascii_case("https://"))
}

/// Build a `ContainerDeployer` for a remote node, transparently using mutual TLS
/// when the node's `address` is `https://` (ADR-020 WS-2.1) and plain HTTP
/// otherwise. This is the single place every CP→agent deployer is constructed so
/// no call site can accidentally fall back to plaintext against an mTLS node.
pub async fn build_node_deployer(
    address: &str,
    token: String,
    node_name: String,
    config_service: &ConfigService,
    encryption_service: &EncryptionService,
) -> Result<RemoteNodeDeployer, DeployerError> {
    if is_https_address(address) {
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
        RemoteNodeDeployer::new_mtls(
            address.to_string(),
            token,
            node_name,
            &identity,
            &ca.cert_pem,
        )
    } else {
        RemoteNodeDeployer::new(address.to_string(), token, node_name)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A node registered with an upper-case scheme must still get the
    /// cluster CA and client identity, never a plain client.
    #[test]
    fn https_detection_ignores_scheme_case_and_leading_space() {
        assert!(is_https_address("https://10.0.0.1:3100"));
        assert!(is_https_address("HTTPS://10.0.0.1:3100"));
        assert!(is_https_address("HttpS://node-1:3100"));
        assert!(is_https_address("  https://node-1:3100"));
    }

    #[test]
    fn non_https_addresses_are_not_tls() {
        assert!(!is_https_address("http://10.0.0.1:3100"));
        assert!(!is_https_address("HTTP://10.0.0.1:3100"));
        assert!(!is_https_address("10.0.0.1:3100"));
        assert!(!is_https_address("https:/10.0.0.1"));
        assert!(!is_https_address(""));
    }
}
