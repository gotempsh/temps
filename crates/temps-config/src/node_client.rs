// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! The control plane's mutual-TLS clients for node agents (ADR-020 WS-2.1).
//!
//! The control plane presents an identity signed by the per-cluster CA
//! ([`crate::cluster_ca`]) and trusts only that CA. Every client here is built
//! from one [`NodeAgentEndpoint`], so the HTTP and terminal paths agree on the
//! address and on the name the certificate is verified for.

use crate::cluster_ca::{ensure_cluster_ca, ClusterCa};
use crate::ConfigService;
use temps_core::node_address::NodeAgentEndpoint;
use temps_core::EncryptionService;

#[derive(Debug, thiserror::Error)]
pub enum ClusterCaError {
    #[error(transparent)]
    Provisioning(#[from] crate::cluster_ca::ClusterCaError),
    #[error("Cluster certificate operation failed: {reason}")]
    Pki { reason: String },
    #[error("Failed to build node HTTP client: {reason}")]
    Client { reason: String },
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
    let csr = temps_core::node_pki::generate_node_keypair_csr("temps-control-plane", &[]).map_err(
        |e| ClusterCaError::Pki {
            reason: e.to_string(),
        },
    )?;
    let signed = temps_core::node_pki::sign_node_csr(&ca.cert_pem, &ca.key_pem, &csr.csr_pem, &[])
        .map_err(|e| ClusterCaError::Pki {
            reason: e.to_string(),
        })?;
    // reqwest's PEM Identity wants the cert chain followed by the private key.
    Ok(format!("{}\n{}", signed.cert_pem, csr.key_pem))
}

/// Build a rustls `ClientConfig` for mutual-TLS **WebSocket** connections to an
/// agent (ADR-020 WS-2.1). The terminal proxy dials the agent with
/// tokio-tungstenite rather than reqwest, so it needs a rustls config directly:
/// it presents the CP's cluster-CA-signed client identity and trusts ONLY the
/// cluster CA. Relies on the process-default crypto provider the CLI installs at
/// startup (same as every other `ClientConfig::builder()` in the workspace).
///
/// `endpoint` is where the terminal connects ([`node_agent_endpoint`]): when it
/// dials the node's mesh address, the agent's certificate is verified for the
/// node's registered host instead of the dialed address, exactly as for the
/// node's HTTP clients.
pub async fn cp_ws_client_config(
    endpoint: &NodeAgentEndpoint,
    config_service: &ConfigService,
    encryption_service: &EncryptionService,
) -> Result<rustls::ClientConfig, ClusterCaError> {
    use rustls::pki_types::{CertificateDer, PrivateKeyDer};
    use std::io::BufReader;

    let ca = ensure_cluster_ca(config_service, encryption_service).await?;
    // Combined PEM (cert chain followed by key) — each parser ignores the
    // other's blocks, so we feed the same buffer to both.
    let identity_pem = cp_client_identity(&ca)?;
    if let Some(server_name) = endpoint.server_name.as_deref() {
        let mut config = temps_core::node_pki::node_agent_client_config(
            &identity_pem,
            &ca.cert_pem,
            server_name,
        )
        .map_err(|e| ClusterCaError::Client {
            reason: format!("build WS client config: {e}"),
        })?;
        // The terminal speaks HTTP/1.1 WebSocket upgrades only, like the
        // direct-dial config below (which offers no ALPN).
        config.alpn_protocols.clear();
        return Ok(config);
    }

    let cert_chain: Vec<CertificateDer<'static>> =
        rustls_pemfile::certs(&mut BufReader::new(identity_pem.as_bytes()))
            .collect::<Result<_, _>>()
            .map_err(|e| ClusterCaError::Client {
                reason: format!("parse CP cert chain: {e}"),
            })?;
    let key: PrivateKeyDer<'static> =
        rustls_pemfile::private_key(&mut BufReader::new(identity_pem.as_bytes()))
            .map_err(|e| ClusterCaError::Client {
                reason: format!("parse CP private key: {e}"),
            })?
            .ok_or_else(|| ClusterCaError::Client {
                reason: "control-plane identity has no key".into(),
            })?;

    let mut roots = rustls::RootCertStore::empty();
    for cert in rustls_pemfile::certs(&mut BufReader::new(ca.cert_pem.as_bytes())) {
        let cert = cert.map_err(|e| ClusterCaError::Client {
            reason: format!("parse cluster CA: {e}"),
        })?;
        roots.add(cert).map_err(|e| ClusterCaError::Client {
            reason: format!("add cluster CA root: {e}"),
        })?;
    }

    rustls::ClientConfig::builder()
        .with_root_certificates(roots)
        .with_client_auth_cert(cert_chain, key)
        .map_err(|e| ClusterCaError::Client {
            reason: format!("build WS client config: {e}"),
        })
}

/// Where the control plane reaches `node`'s agent: its registered address, or
/// its WireGuard mesh address for a node that joined with a public one (see
/// [`temps_core::node_address::node_agent_endpoint`]). Every CP→agent client
/// in this module is built from it, so the HTTP, deployer and terminal paths
/// always agree on the address and on the name the certificate is verified
/// for.
pub fn node_agent_endpoint(node: &temps_entities::nodes::Model) -> NodeAgentEndpoint {
    let endpoint = node.agent_endpoint();
    if endpoint.via_mesh() {
        tracing::debug!(
            node_id = node.id,
            node = %node.name,
            registered = %node.address,
            dial = %endpoint.dial_url,
            "reaching the node agent over the WireGuard mesh"
        );
    }
    endpoint
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
/// This is the streaming/raw-HTTP analogue of `build_node_deployer` for the
/// CP→agent paths that don't go through `RemoteNodeDeployer` (log streaming,
/// edge-analytics ingest) so they don't silently fall back to plaintext
/// against an mTLS-enforcing node.
///
/// The client is for `endpoint` ([`node_agent_endpoint`]); build request URLs
/// from `endpoint.dial_url`.
pub async fn build_node_http_client(
    endpoint: &NodeAgentEndpoint,
    config_service: &ConfigService,
    encryption_service: &EncryptionService,
    timeout: Option<std::time::Duration>,
) -> Result<reqwest::Client, ClusterCaError> {
    let address = endpoint.dial_url.as_str();
    // A worker that accepts the TCP connection but never answers must not
    // hold a caller for the whole request timeout, and nothing on a node
    // legitimately redirects: following one would replay the bearer token.
    let mut builder = reqwest::Client::builder()
        .connect_timeout(NODE_CONNECT_TIMEOUT)
        .redirect(reqwest::redirect::Policy::none());
    if let Some(t) = timeout {
        builder = builder.timeout(t);
    }
    if let (true, Some(server_name)) = (is_https_address(address), endpoint.server_name.as_deref())
    {
        let ca = ensure_cluster_ca(config_service, encryption_service).await?;
        let identity_pem = cp_client_identity(&ca)?;
        let tls = temps_core::node_pki::node_agent_client_config(
            &identity_pem,
            &ca.cert_pem,
            server_name,
        )
        .map_err(|e| ClusterCaError::Client {
            reason: e.to_string(),
        })?;
        builder = builder.use_preconfigured_tls(tls);
    } else if is_https_address(address) {
        let ca = ensure_cluster_ca(config_service, encryption_service).await?;
        let identity_pem = cp_client_identity(&ca)?;
        let identity = reqwest::Identity::from_pem(identity_pem.as_bytes()).map_err(|e| {
            ClusterCaError::Client {
                reason: format!("invalid control-plane identity: {e}"),
            }
        })?;
        let ca_cert = reqwest::Certificate::from_pem(ca.cert_pem.as_bytes()).map_err(|e| {
            ClusterCaError::Client {
                reason: format!("invalid cluster CA certificate: {e}"),
            }
        })?;
        builder = builder
            .use_rustls_tls()
            .identity(identity)
            .add_root_certificate(ca_cert)
            .tls_built_in_root_certs(false);
    } else {
        builder = builder.danger_accept_invalid_certs(temps_core::tls::insecure_tls_enabled());
    }
    builder.build().map_err(|e| ClusterCaError::Client {
        reason: e.to_string(),
    })
}

/// Whether a node agent address uses TLS (`https://`). The scheme is
/// case-insensitive (RFC 3986), so `HTTPS://` must get the cluster CA and the
/// control plane's client identity too: every caller deciding whether to talk
/// mTLS to a node goes through this one check, so they can never disagree.
pub fn is_https_address(address: &str) -> bool {
    temps_core::node_address::is_https_agent_address(address)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{AppSettings, ServerConfig};
    use sea_orm::{DatabaseBackend, DatabaseConnection, DbErr, MockDatabase};
    use std::sync::Arc;
    use temps_core::node_address::AGENT_MESH_LISTENER_KEY;
    use temps_entities::{nodes, settings};

    fn encryption() -> EncryptionService {
        EncryptionService::new_from_password("test")
    }

    fn server_config() -> Arc<ServerConfig> {
        Arc::new(
            ServerConfig::new(
                "127.0.0.1:0".into(),
                "postgres://test".into(),
                None,
                Some("127.0.0.1:0".into()),
            )
            .expect("test server config"),
        )
    }

    fn config_service(db: DatabaseConnection) -> ConfigService {
        ConfigService::new(server_config(), Arc::new(db))
    }

    /// A config service whose settings row holds a provisioned cluster CA.
    fn config_with_ca(ca: &temps_core::node_pki::ClusterCa) -> ConfigService {
        let mut app_settings = AppSettings::default();
        app_settings.multi_node.cluster_ca_cert_pem = Some(ca.cert_pem.clone());
        app_settings.multi_node.cluster_ca_key_encrypted = Some(
            encryption()
                .encrypt(ca.key_pem.as_bytes())
                .expect("encrypt CA key"),
        );
        let now = chrono::Utc::now();
        let row = settings::Model {
            id: 1,
            data: app_settings.to_json(),
            created_at: now,
            updated_at: now,
        };
        config_service(
            MockDatabase::new(DatabaseBackend::Postgres)
                .append_query_results([vec![row.clone()], vec![row]])
                .into_connection(),
        )
    }

    /// A config service whose database fails every read, so the cluster CA
    /// can never be resolved.
    fn config_without_database() -> ConfigService {
        config_service(
            MockDatabase::new(DatabaseBackend::Postgres)
                .append_query_errors([DbErr::Custom("settings table unavailable".into())])
                .into_connection(),
        )
    }

    fn generated_ca() -> temps_core::node_pki::ClusterCa {
        temps_core::node_pki::generate_cluster_ca().expect("generate test CA")
    }

    fn endpoint(dial_url: &str, server_name: Option<&str>) -> NodeAgentEndpoint {
        NodeAgentEndpoint {
            dial_url: dial_url.to_string(),
            server_name: server_name.map(str::to_string),
        }
    }

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

    /// The identity is a cert chain followed by a key, chains to the cluster
    /// CA, and has no SANs, so it can never impersonate a node's TLS server.
    #[test]
    fn client_identity_is_a_san_less_certificate_signed_by_the_cluster_ca() {
        let generated = generated_ca();
        let ca = ClusterCa {
            cert_pem: generated.cert_pem.clone(),
            key_pem: generated.key_pem.clone(),
        };

        let identity = cp_client_identity(&ca).expect("mint identity");

        reqwest::Identity::from_pem(identity.as_bytes()).expect("reqwest accepts the combined PEM");
        let cert_der = rustls_pemfile::certs(&mut identity.as_bytes())
            .next()
            .expect("identity has a certificate")
            .expect("certificate parses");
        let ca_der = rustls_pemfile::certs(&mut generated.cert_pem.as_bytes())
            .next()
            .expect("CA has a certificate")
            .expect("CA parses");
        let (_, cert) = x509_parser::parse_x509_certificate(&cert_der).expect("parse identity");
        let (_, ca_cert) = x509_parser::parse_x509_certificate(&ca_der).expect("parse CA");
        assert_eq!(cert.issuer(), ca_cert.subject());
        assert!(cert.verify_signature(Some(ca_cert.public_key())).is_ok());
        assert!(
            cert.subject_alternative_name()
                .expect("SAN extension parses")
                .is_none_or(|san| san.value.general_names.is_empty()),
            "a client identity must carry no SANs"
        );
    }

    /// Identity minting fails with a typed error on a CA key that is not one,
    /// rather than handing out an unsigned identity.
    #[test]
    fn client_identity_rejects_an_invalid_ca() {
        let ca = ClusterCa {
            cert_pem: "not a certificate".to_string(),
            key_pem: "not a key".to_string(),
        };
        assert!(matches!(
            cp_client_identity(&ca),
            Err(ClusterCaError::Pki { .. })
        ));
    }

    /// A plain `http://` node needs no cluster CA: the client builds without
    /// reading settings at all (the mock database would fail the read).
    #[tokio::test]
    async fn http_node_client_does_not_touch_the_cluster_ca() {
        let client = build_node_http_client(
            &endpoint("http://10.0.0.1:3100", None),
            &config_without_database(),
            &encryption(),
            Some(std::time::Duration::from_secs(1)),
        )
        .await;
        assert!(client.is_ok(), "got {:?}", client.err());
    }

    /// An `https://` node whose cluster CA cannot be resolved is an error,
    /// never a silent fallback to a client without the control plane's
    /// identity. Covers both the direct and the mesh-dialled address.
    #[tokio::test]
    async fn https_node_client_fails_closed_without_a_cluster_ca() {
        for server_name in [None, Some("203.0.113.10")] {
            let result = build_node_http_client(
                &endpoint("HTTPS://10.0.0.1:3100", server_name),
                &config_without_database(),
                &encryption(),
                None,
            )
            .await;
            assert!(
                matches!(result, Err(ClusterCaError::Provisioning(_))),
                "server_name {server_name:?}: got {:?}",
                result.map(|_| "a client")
            );
        }
    }

    /// With a provisioned CA both `https://` shapes build a client.
    #[tokio::test]
    async fn https_node_client_builds_with_a_cluster_ca() {
        let _ = rustls::crypto::ring::default_provider().install_default();
        let ca = generated_ca();
        for server_name in [None, Some("203.0.113.10")] {
            let client = build_node_http_client(
                &endpoint("https://10.0.0.1:3100", server_name),
                &config_with_ca(&ca),
                &encryption(),
                None,
            )
            .await;
            assert!(
                client.is_ok(),
                "server_name {server_name:?}: got {:?}",
                client.err()
            );
        }
    }

    /// The terminal's WebSocket config offers no ALPN in either shape: it only
    /// speaks HTTP/1.1 upgrades.
    #[tokio::test]
    async fn websocket_config_presents_an_identity_and_offers_no_alpn() {
        let _ = rustls::crypto::ring::default_provider().install_default();
        let ca = generated_ca();
        for server_name in [None, Some("203.0.113.10")] {
            let config = cp_ws_client_config(
                &endpoint("https://10.0.0.1:3100", server_name),
                &config_with_ca(&ca),
                &encryption(),
            )
            .await
            .expect("build WS config");
            assert!(config.alpn_protocols.is_empty());
            assert!(config.client_auth_cert_resolver.has_certs());
        }
    }

    #[tokio::test]
    async fn websocket_config_fails_closed_without_a_cluster_ca() {
        let result = cp_ws_client_config(
            &endpoint("https://10.0.0.1:3100", None),
            &config_without_database(),
            &encryption(),
        )
        .await;
        assert!(matches!(result, Err(ClusterCaError::Provisioning(_))));
    }

    /// A node whose agent listens on the mesh is dialled there, with the
    /// certificate still verified for its registered host.
    #[test]
    fn node_endpoint_follows_the_mesh_listener() {
        let now = chrono::Utc::now();
        let mut node = nodes::Model {
            architecture: None,
            id: 7,
            name: "worker-7".to_string(),
            token_hash: "hash".to_string(),
            token_encrypted: None,
            address: "https://203.0.113.10:3100".to_string(),
            private_address: "203.0.113.10".to_string(),
            public_endpoint: None,
            wg_public_key: None,
            mesh_wg_public_key: None,
            mesh_wg_endpoint: None,
            mesh_wg_address: Some("10.201.0.2".to_string()),
            role: "worker".to_string(),
            status: "ready".to_string(),
            labels: serde_json::json!({}),
            capacity: serde_json::json!({}),
            last_heartbeat: Some(now),
            edge_public_key: None,
            compute_cidr: None,
            underlay_address: None,
            failover_at: None,
            dns_resolver_running: None,
            dns_resolver_tasks_alive: None,
            dns_resolver_last_sync_at: None,
            dns_resolver_consecutive_failures: 0,
            dns_resolver_last_error: None,
            dns_resolver_record_count: None,
            deployments_without_resolver: None,
            build_limits: None,
            public_ingress_enabled: false,
            public_ingress_running: None,
            public_ingress_last_error: None,
            public_ingress_certificate_count: None,
            public_ingress_route_count: None,
            public_ingress_unsupported_route_count: None,
            public_ingress_unsupported_reasons: serde_json::json!([]),
            occupied_ranges: None,
            overlay_status: None,
            overlay_last_error: None,
            overlay_ready_cidr: None,
            overlay_remediation: None,
            overlay_network_name: None,
            created_at: now,
            updated_at: now,
        };
        assert_eq!(
            node_agent_endpoint(&node).dial_url,
            "https://203.0.113.10:3100"
        );

        node.capacity = serde_json::json!({ AGENT_MESH_LISTENER_KEY: "10.201.0.2:3100" });
        let mesh = node_agent_endpoint(&node);
        assert_eq!(mesh.dial_url, "https://10.201.0.2:3100");
        assert_eq!(mesh.server_name.as_deref(), Some("203.0.113.10"));
    }
}
