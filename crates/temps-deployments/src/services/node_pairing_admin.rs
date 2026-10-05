// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Starting and cancelling one-paste node pairings (ADR 048 D2b).
//!
//! A pairing is a pending node the control plane dials: this service mints
//! its single-use enrollment token, encrypts its secret, reserves its mesh
//! address and returns the pairing code, which holds the secret and is
//! handed only to the node. The dialing itself is
//! [`crate::services::node_pairing`].

use std::net::{AddrParseError, SocketAddr};
use std::sync::Arc;

use sea_orm::DatabaseConnection;
use temps_config::cluster_ca::ClusterCaError;
use temps_config::{ConfigService, EnrollmentError, EnrollmentTokenService};
use temps_core::node_pki::PkiError;
use temps_core::EncryptionService;
use temps_entities::node_pairings;
use temps_network::mesh::MeshError;
use temps_wireguard::pairing::{PairingCode, PairingError, PairingId, PairingSecret};
use tracing::error;
use zeroize::Zeroizing;

/// How long a pairing code stays usable.
pub const PAIRING_TTL_SECS: i64 = 30 * 60;

#[derive(Debug, thiserror::Error)]
pub enum NodePairingAdminError {
    /// Reading or changing the mesh or pairing state failed, or the change
    /// was refused (see `source`).
    #[error("could not {action}: {source}")]
    Mesh {
        action: &'static str,
        #[source]
        source: MeshError,
    },
    #[error("the control plane has not brought up its end of the mesh yet")]
    MeshNotReady,
    #[error("node name {name:?} is invalid: use 1-63 lowercase letters, digits and dashes")]
    InvalidName { name: String },
    #[error("could not generate the pairing {what}: {source}")]
    Randomness {
        what: &'static str,
        #[source]
        source: PairingError,
    },
    #[error("could not initialize the cluster CA: {0}")]
    ClusterCa(#[source] ClusterCaError),
    #[error("could not fingerprint the cluster CA: {0}")]
    CaFingerprint(#[source] PkiError),
    #[error("could not mint the enrollment token for pairing {name:?}: {source}")]
    MintToken {
        name: String,
        #[source]
        source: EnrollmentError,
    },
    #[error("could not encrypt the secret of pairing {name:?}: {reason}")]
    EncryptSecret { name: String, reason: String },
    #[error("pairing {pairing_id} reserved mesh address {address:?}, which is not an IPv4 address: {source}")]
    InvalidMeshAddress {
        pairing_id: i32,
        address: String,
        #[source]
        source: AddrParseError,
    },
    #[error("node pairing {pairing_id} not found")]
    NotFound { pairing_id: i32 },
    #[error("node pairing {pairing_id} is {status}; nothing to cancel")]
    AlreadyFinished { pairing_id: i32, status: String },
}

fn mesh(action: &'static str) -> impl FnOnce(MeshError) -> NodePairingAdminError {
    move |source| NodePairingAdminError::Mesh { action, source }
}

/// A pairing that was just created, with its code. The code holds the
/// pairing secret and a join token: hand it only to the node.
pub struct StartedPairing {
    pub pairing: node_pairings::Model,
    pub code: Zeroizing<String>,
}

/// A node name: lowercase letters, digits and dashes, 1–63 characters.
pub fn valid_node_name(name: &str) -> bool {
    (1..=63).contains(&name.len())
        && name
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
        && !name.starts_with('-')
}

/// `worker-<up to 6 random characters>` from the pairing id.
fn default_node_name(pairing_id: &str) -> String {
    let suffix: String = pairing_id
        .chars()
        .filter(char::is_ascii_alphanumeric)
        .map(|c| c.to_ascii_lowercase())
        .take(6)
        .collect();
    if suffix.is_empty() {
        "worker".to_string()
    } else {
        format!("worker-{suffix}")
    }
}

/// Creates, lists and cancels node pairings.
pub struct NodePairingAdminService {
    db: Arc<DatabaseConnection>,
    config_service: Arc<ConfigService>,
    encryption_service: Arc<EncryptionService>,
    enrollment_tokens: Arc<EnrollmentTokenService>,
}

impl NodePairingAdminService {
    pub fn new(
        db: Arc<DatabaseConnection>,
        config_service: Arc<ConfigService>,
        encryption_service: Arc<EncryptionService>,
        enrollment_tokens: Arc<EnrollmentTokenService>,
    ) -> Self {
        Self {
            db,
            config_service,
            encryption_service,
            enrollment_tokens,
        }
    }

    /// The UDP port the control plane dials nodes on. Pairing needs the
    /// mesh, so this fails with [`MeshError::Disabled`] while it is off.
    pub async fn mesh_port(&self) -> Result<u16, NodePairingAdminError> {
        temps_network::mesh::load_settings(self.db.as_ref())
            .await
            .map_err(mesh("read the mesh settings"))?
            .map(|settings| settings.port)
            .ok_or_else(|| mesh("pair a node")(MeshError::Disabled))
    }

    /// Create a pairing for a node at `node_endpoint`, named `name` or
    /// `worker-<random>`.
    pub async fn start(
        &self,
        user_id: i32,
        node_endpoint: SocketAddr,
        name: Option<&str>,
    ) -> Result<StartedPairing, NodePairingAdminError> {
        let db = self.db.as_ref();
        let settings = temps_network::mesh::load_settings(db)
            .await
            .map_err(mesh("read the mesh settings"))?
            .ok_or_else(|| mesh("pair a node")(MeshError::Disabled))?;
        let control_plane = temps_network::mesh::published_control_plane(db)
            .await
            .map_err(mesh("read the control plane's mesh key"))?
            .ok_or(NodePairingAdminError::MeshNotReady)?;

        let pairing_id = PairingId::generate()
            .map_err(|source| NodePairingAdminError::Randomness { what: "id", source })?;
        let secret =
            PairingSecret::generate().map_err(|source| NodePairingAdminError::Randomness {
                what: "secret",
                source,
            })?;
        let name = match name.map(str::trim).filter(|n| !n.is_empty()) {
            Some(name) if valid_node_name(name) => name.to_string(),
            Some(name) => {
                return Err(NodePairingAdminError::InvalidName {
                    name: name.to_string(),
                })
            }
            None => default_node_name(&pairing_id.to_base64url()),
        };

        let ca = crate::cluster_ca::ensure_cluster_ca(
            self.config_service.as_ref(),
            self.encryption_service.as_ref(),
        )
        .await
        .map_err(NodePairingAdminError::ClusterCa)?;
        let ca_fingerprint = temps_core::node_pki::ca_fingerprint_sha256(&ca.cert_pem)
            .map_err(NodePairingAdminError::CaFingerprint)?;

        let (join_token, token) = self
            .enrollment_tokens
            .mint(temps_config::MintParams {
                max_uses: 1,
                ttl_secs: PAIRING_TTL_SECS,
                bound_node_name: Some(name.clone()),
                bound_labels: None,
                created_by_user_id: Some(user_id),
                ca_fingerprint: Some(ca_fingerprint.clone()),
            })
            .await
            .map_err(|source| NodePairingAdminError::MintToken {
                name: name.clone(),
                source,
            })?;
        let secret_encrypted = match self
            .encryption_service
            .encrypt(secret.to_base64url().as_bytes())
        {
            Ok(encrypted) => encrypted,
            Err(error) => {
                self.revoke_unused_token(token.id).await;
                return Err(NodePairingAdminError::EncryptSecret {
                    name,
                    reason: error.to_string(),
                });
            }
        };

        let created = temps_network::pairing::create(
            db,
            temps_network::pairing::NewPairing {
                pairing_id: pairing_id.to_base64url(),
                name: name.clone(),
                node_endpoint,
                secret_encrypted,
                enrollment_token_id: token.id,
                expires_at: token.expires_at,
                created_by_user_id: Some(user_id),
            },
        )
        .await;
        let pairing = match created {
            Ok(pairing) => pairing,
            Err(source) => {
                self.revoke_unused_token(token.id).await;
                return Err(NodePairingAdminError::Mesh {
                    action: "create the pairing",
                    source,
                });
            }
        };

        let node_address = pairing.mesh_address.parse().map_err(|source| {
            NodePairingAdminError::InvalidMeshAddress {
                pairing_id: pairing.id,
                address: pairing.mesh_address.clone(),
                source,
            }
        })?;
        let code = PairingCode {
            id: pairing_id.to_base64url(),
            secret: secret.to_base64url(),
            name: pairing.name.clone(),
            control_plane_public_key: control_plane.public_key,
            control_plane_endpoint: control_plane.endpoint,
            control_plane_address: settings.control_plane_address(),
            node_address,
            node_endpoint,
            prefix_len: settings.cidr.prefix_len(),
            listen_port: settings.port,
            node_api_port: settings.node_api_port,
            ca_fingerprint,
            join_token,
            expires_at: pairing.expires_at.timestamp(),
        };
        let code = code
            .encode()
            .map_err(|source| NodePairingAdminError::Randomness {
                what: "code",
                source,
            })?;
        Ok(StartedPairing {
            pairing,
            code: Zeroizing::new(code),
        })
    }

    /// Recent pairings, newest first.
    pub async fn list(&self) -> Result<Vec<node_pairings::Model>, NodePairingAdminError> {
        temps_network::pairing::list(self.db.as_ref())
            .await
            .map_err(mesh("list the node pairings"))
    }

    /// Cancel a pending pairing and revoke its enrollment token. Returns the
    /// pairing as it was before the cancel.
    pub async fn cancel(
        &self,
        pairing_id: i32,
    ) -> Result<node_pairings::Model, NodePairingAdminError> {
        let db = self.db.as_ref();
        let pairing = temps_network::pairing::get(db, pairing_id)
            .await
            .map_err(mesh("load the pairing"))?
            .ok_or(NodePairingAdminError::NotFound { pairing_id })?;
        if !temps_network::pairing::cancel(db, pairing_id)
            .await
            .map_err(mesh("cancel the pairing"))?
        {
            return Err(NodePairingAdminError::AlreadyFinished {
                pairing_id,
                status: pairing.status,
            });
        }
        // The pairing no longer accepts the node's key, so a token that
        // survives here cannot complete a pairing on its own.
        if let Err(error) = self
            .enrollment_tokens
            .revoke(pairing.enrollment_token_id)
            .await
        {
            error!(
                %error,
                pairing = pairing_id,
                token = pairing.enrollment_token_id,
                "pairing cancelled but its enrollment token could not be revoked"
            );
        }
        Ok(pairing)
    }

    /// A token minted for a pairing that was never created was never handed
    /// out; do not leave it usable.
    async fn revoke_unused_token(&self, token_id: i32) {
        if let Err(error) = self.enrollment_tokens.revoke(token_id).await {
            error!(
                %error,
                token = token_id,
                "could not revoke the enrollment token of a pairing that was not created"
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn node_names_are_dns_labels() {
        assert!(valid_node_name("worker-1"));
        assert!(!valid_node_name("Worker"));
        assert!(!valid_node_name("-worker"));
        assert!(!valid_node_name(""));
        assert!(!valid_node_name(&"a".repeat(64)));
    }

    #[test]
    fn default_names_take_up_to_six_characters_of_the_id() {
        assert_eq!(default_node_name("AbC-_dEfGhI"), "worker-abcdef");
        // Fewer than six usable characters must not panic.
        assert_eq!(default_node_name("a-_B"), "worker-ab");
        assert_eq!(default_node_name("-_"), "worker");
        assert_eq!(default_node_name(""), "worker");
        assert!(valid_node_name(&default_node_name("Zz9_-x")));
    }
}
