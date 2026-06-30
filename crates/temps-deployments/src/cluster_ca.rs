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

/// A resolved cluster CA: the public cert PEM and the DECRYPTED key PEM.
pub struct ClusterCa {
    pub cert_pem: String,
    pub key_pem: String,
}

#[derive(Debug, thiserror::Error)]
pub enum ClusterCaError {
    #[error("Failed to read/write settings: {0}")]
    Settings(String),
    #[error("Cluster CA generation failed: {0}")]
    Pki(String),
    #[error("Encryption error: {0}")]
    Encryption(String),
    #[error("Failed to build node HTTP client: {0}")]
    Client(String),
}

fn decrypt_key(
    encryption_service: &EncryptionService,
    encrypted: &str,
) -> Result<String, ClusterCaError> {
    let bytes = encryption_service
        .decrypt(encrypted)
        .map_err(|e| ClusterCaError::Encryption(e.to_string()))?;
    String::from_utf8(bytes).map_err(|e| ClusterCaError::Encryption(e.to_string()))
}

/// Get the cluster CA (cert + decrypted key), minting and persisting it on first
/// use. Idempotent under concurrency: if a racing caller already stored a CA, we
/// re-read and return the winner's CA so every node pins the same root.
pub async fn ensure_cluster_ca(
    config_service: &ConfigService,
    encryption_service: &EncryptionService,
) -> Result<ClusterCa, ClusterCaError> {
    let settings = config_service
        .get_settings()
        .await
        .map_err(|e| ClusterCaError::Settings(e.to_string()))?;

    if let (Some(cert), Some(enc_key)) = (
        settings.multi_node.cluster_ca_cert_pem.clone(),
        settings.multi_node.cluster_ca_key_encrypted.clone(),
    ) {
        return Ok(ClusterCa {
            cert_pem: cert,
            key_pem: decrypt_key(encryption_service, &enc_key)?,
        });
    }

    // Mint a new CA and encrypt its private key for storage.
    let ca = temps_core::node_pki::generate_cluster_ca()
        .map_err(|e| ClusterCaError::Pki(e.to_string()))?;
    let enc_key = encryption_service
        .encrypt(ca.key_pem.as_bytes())
        .map_err(|e| ClusterCaError::Encryption(e.to_string()))?;
    let cert_pem = ca.cert_pem.clone();

    // Persist only if still absent — a concurrent minter may have won.
    config_service
        .update_setting_field(move |s| {
            if s.multi_node.cluster_ca_cert_pem.is_none() {
                s.multi_node.cluster_ca_cert_pem = Some(cert_pem.clone());
                s.multi_node.cluster_ca_key_encrypted = Some(enc_key.clone());
            }
        })
        .await
        .map_err(|e| ClusterCaError::Settings(e.to_string()))?;

    // Re-read to return whatever CA actually persisted (ours or the winner's).
    let settings = config_service
        .get_settings()
        .await
        .map_err(|e| ClusterCaError::Settings(e.to_string()))?;
    let cert = settings
        .multi_node
        .cluster_ca_cert_pem
        .ok_or_else(|| ClusterCaError::Settings("cluster CA cert missing after mint".into()))?;
    let enc_key = settings
        .multi_node
        .cluster_ca_key_encrypted
        .ok_or_else(|| ClusterCaError::Settings("cluster CA key missing after mint".into()))?;

    Ok(ClusterCa {
        cert_pem: cert,
        key_pem: decrypt_key(encryption_service, &enc_key)?,
    })
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
    let mut builder = reqwest::Client::builder();
    if let Some(t) = timeout {
        builder = builder.timeout(t);
    }
    if address.starts_with("https://") {
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
    if address.starts_with("https://") {
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
