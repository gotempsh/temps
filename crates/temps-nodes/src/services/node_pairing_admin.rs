// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Starting and cancelling one-paste node pairings (ADR 048 D2b).
//!
//! A pairing is a pending node the control plane dials: this service mints
//! its single-use enrollment token, encrypts its secret, reserves its mesh
//! address and returns the pairing code, which holds the secret and is
//! handed only to the node. The dialing itself is
//! [`crate::services::node_pairing`].
//!
//! A worker that joins outbound only (ADR 055) starts its own pairing with a
//! join token ([`NodePairingAdminService::start_outbound`]): no code, no
//! dialing; it gets the same facts over HTTPS and dials the control plane.

use std::net::{AddrParseError, Ipv4Addr, SocketAddr};
use std::sync::Arc;

use sea_orm::DatabaseConnection;
use temps_config::cluster_ca::ClusterCaError;
use temps_config::{ConfigService, EnrollmentError, EnrollmentTokenService};
use temps_core::node_pki::PkiError;
use temps_core::EncryptionService;
use temps_entities::node_pairings;
use temps_network::mesh::MeshError;
use temps_wireguard::pairing::{PairingCode, PairingError, PairingId, PairingSecret};
use tracing::{error, info};
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
    /// A worker that joins outbound only dials the control plane, which
    /// publishes no WireGuard endpoint (ADR 055).
    #[error("the control plane publishes no WireGuard endpoint workers can dial")]
    ControlPlaneNotDialable,
    #[error("node name {name:?} is invalid: use 1-63 lowercase letters, digits and dashes")]
    InvalidName { name: String },
    #[error("node name {name:?} is invalid: use at most 63 letters, digits, '-', '.' or '_'")]
    InvalidRegistrationName { name: String },
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
    /// The worker's key belongs to a node of the same name, and it did not
    /// prove it is that node (ADR 060), or the node could not be read.
    #[error(transparent)]
    Rejoin(crate::services::node_service::NodeError),
    #[error("node {node_id} holds mesh key but its mesh address {address:?} is not an IPv4 address: {source}")]
    InvalidNodeMeshAddress {
        node_id: i32,
        address: Option<String>,
        #[source]
        source: AddrParseError,
    },
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

/// What a worker that joins outbound only (ADR 055) needs to bring its end
/// of the mesh up and register over it: a pairing code without the secret
/// and without the node's endpoint. The join token is single-use and bound
/// to the node's name; hand it only to that worker.
pub struct OutboundEnrollment {
    pub kind: OutboundEnrollmentKind,
    /// The name the join token is bound to.
    pub name: String,
    /// When the join token (and a new pairing) expires.
    pub expires_at: chrono::DateTime<chrono::Utc>,
    pub control_plane_public_key: String,
    pub control_plane_endpoint: String,
    pub control_plane_address: Ipv4Addr,
    pub node_address: Ipv4Addr,
    pub prefix_len: u8,
    pub listen_port: u16,
    pub node_api_port: u16,
    pub ca_fingerprint: String,
    pub join_token: Zeroizing<String>,
}

/// A freshly minted single-use enrollment token.
struct MintedToken {
    ca_fingerprint: String,
    join_token: String,
    token: temps_entities::node_enrollment_tokens::Model,
}

/// How an outbound enrollment reached the mesh.
pub enum OutboundEnrollmentKind {
    /// A new pairing reserved the worker a mesh address (ADR 055).
    Pairing(Box<node_pairings::Model>),
    /// The worker is node `node_id` joining the same control plane again
    /// (ADR 060): it proved it is that node, keeps its mesh key and address,
    /// and registers again to update the node in place. `ack` is
    /// [`temps_core::node_pki::rejoin_ack`] over its proof, so the worker
    /// can tell this is the control plane it belongs to.
    Rejoin { node_id: i32, ack: Option<String> },
}

/// The enrollment token and encrypted secret a new pairing is created with.
struct PairingCredentials {
    ca_fingerprint: String,
    join_token: String,
    token_id: i32,
    expires_at: chrono::DateTime<chrono::Utc>,
    secret_encrypted: String,
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

        let PairingCredentials {
            ca_fingerprint,
            join_token,
            token_id,
            expires_at,
            secret_encrypted,
        } = self
            .credentials(&name, &secret, Some(user_id), None)
            .await?;

        let created = temps_network::pairing::create(
            db,
            temps_network::pairing::NewPairing {
                pairing_id: pairing_id.to_base64url(),
                name: name.clone(),
                node_endpoint,
                secret_encrypted,
                enrollment_token_id: token_id,
                expires_at,
                created_by_user_id: Some(user_id),
            },
        )
        .await;
        let pairing = match created {
            Ok(pairing) => pairing,
            Err(source) => {
                self.revoke_unused_token(token_id).await;
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

    /// Create the pairing of a worker that joins outbound only (ADR 055) and
    /// sent its WireGuard key: it starts with the key, has no endpoint and is
    /// never dialed. Needs the mesh on and a control plane endpoint workers
    /// can dial. The pairing's token is bound to `name` and, like the token
    /// the worker authorized with, to `bound_labels`. The caller validated
    /// that token and claimed one use of it.
    ///
    /// When `public_key` is the mesh key of the node named `name` and
    /// `rejoin` proves the caller holds that node's current agent
    /// credential, nothing is reserved: the answer carries the node's own
    /// mesh address and a fresh single-use token bound to its name, so it
    /// registers again and the node is updated in place (ADR 060). Without
    /// that proof it is refused ([`NodePairingAdminError::Rejoin`]).
    pub async fn start_outbound(
        &self,
        name: &str,
        public_key: &str,
        created_by_user_id: Option<i32>,
        bound_labels: Option<serde_json::Value>,
        rejoin: Option<crate::services::node_service::RejoinClaim<'_>>,
    ) -> Result<OutboundEnrollment, NodePairingAdminError> {
        let db = self.db.as_ref();
        if !crate::services::node_service::is_valid_node_name(name) {
            return Err(NodePairingAdminError::InvalidRegistrationName {
                name: name.to_string(),
            });
        }
        let proof = rejoin.as_ref().map(|claim| claim.proof.to_string());
        let rejoining = crate::services::node_service::proven_rejoin(db, name, public_key, rejoin)
            .await
            .map_err(NodePairingAdminError::Rejoin)?;
        let settings = temps_network::mesh::load_settings(db)
            .await
            .map_err(mesh("read the mesh settings"))?
            .ok_or_else(|| mesh("enroll a node on the mesh")(MeshError::Disabled))?;
        let control_plane = temps_network::mesh::published_control_plane(db)
            .await
            .map_err(mesh("read the control plane's mesh key"))?
            .ok_or(NodePairingAdminError::MeshNotReady)?;
        let control_plane_endpoint = control_plane
            .endpoint
            .ok_or(NodePairingAdminError::ControlPlaneNotDialable)?;

        if let Some(node) = rejoining {
            let node_address = node
                .mesh_wg_address
                .as_deref()
                .unwrap_or_default()
                .parse()
                .map_err(|source| NodePairingAdminError::InvalidNodeMeshAddress {
                    node_id: node.id,
                    address: node.mesh_wg_address.clone(),
                    source,
                })?;
            let MintedToken {
                ca_fingerprint,
                join_token,
                token,
            } = self
                .mint_token(name, created_by_user_id, bound_labels)
                .await?;
            info!(
                node_id = node.id,
                name, "a node joins its control plane again; it keeps its mesh key and address"
            );
            return Ok(OutboundEnrollment {
                kind: OutboundEnrollmentKind::Rejoin {
                    node_id: node.id,
                    ack: proof.as_deref().and_then(|proof| {
                        temps_core::node_pki::rejoin_ack(&node.token_hash, proof)
                    }),
                },
                name: node.name,
                expires_at: token.expires_at,
                control_plane_public_key: control_plane.public_key,
                control_plane_endpoint,
                control_plane_address: settings.control_plane_address(),
                node_address,
                prefix_len: settings.cidr.prefix_len(),
                listen_port: settings.port,
                node_api_port: settings.node_api_port,
                ca_fingerprint,
                join_token: Zeroizing::new(join_token),
            });
        }

        let pairing_id = PairingId::generate()
            .map_err(|source| NodePairingAdminError::Randomness { what: "id", source })?;
        // Never handed out: nothing dials this pairing. The column is shared
        // with dialed pairings, so it still holds a random, encrypted value.
        let secret =
            PairingSecret::generate().map_err(|source| NodePairingAdminError::Randomness {
                what: "secret",
                source,
            })?;
        let PairingCredentials {
            ca_fingerprint,
            join_token,
            token_id,
            expires_at,
            secret_encrypted,
        } = self
            .credentials(name, &secret, created_by_user_id, bound_labels)
            .await?;
        let join_token = Zeroizing::new(join_token);

        // The same machine re-running `temps join` resumes its pending
        // enrollment (ADR 056); anything else creates a pairing, which
        // refuses a key in use.
        let resumed = temps_network::pairing::resume_outbound(
            db,
            temps_network::pairing::ResumedOutboundPairing {
                name: name.to_string(),
                public_key: public_key.to_string(),
                secret_encrypted: secret_encrypted.clone(),
                enrollment_token_id: token_id,
                expires_at,
                created_by_user_id,
            },
        )
        .await;
        let created = match resumed {
            Ok(Some(pairing)) => {
                info!(
                    pairing = pairing.id,
                    name, "resuming the pending outbound enrollment of the same machine"
                );
                Ok(pairing)
            }
            Ok(None) => {
                temps_network::pairing::create_outbound(
                    db,
                    temps_network::pairing::NewOutboundPairing {
                        pairing_id: pairing_id.to_base64url(),
                        name: name.to_string(),
                        public_key: public_key.to_string(),
                        secret_encrypted,
                        enrollment_token_id: token_id,
                        expires_at,
                        created_by_user_id,
                    },
                )
                .await
            }
            Err(error) => Err(error),
        };
        let pairing = match created {
            Ok(pairing) => pairing,
            Err(source) => {
                self.revoke_unused_token(token_id).await;
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
        Ok(OutboundEnrollment {
            name: pairing.name.clone(),
            expires_at: pairing.expires_at,
            kind: OutboundEnrollmentKind::Pairing(Box::new(pairing)),
            control_plane_public_key: control_plane.public_key,
            control_plane_endpoint,
            control_plane_address: settings.control_plane_address(),
            node_address,
            prefix_len: settings.cidr.prefix_len(),
            listen_port: settings.port,
            node_api_port: settings.node_api_port,
            ca_fingerprint,
            join_token,
        })
    }

    /// Mint a pairing's single-use enrollment token, bound to `name`, and
    /// encrypt its secret. A token whose secret could not be encrypted is
    /// revoked before returning.
    async fn credentials(
        &self,
        name: &str,
        secret: &PairingSecret,
        created_by_user_id: Option<i32>,
        bound_labels: Option<serde_json::Value>,
    ) -> Result<PairingCredentials, NodePairingAdminError> {
        let MintedToken {
            ca_fingerprint,
            join_token,
            token,
        } = self
            .mint_token(name, created_by_user_id, bound_labels)
            .await?;
        let secret_encrypted = match self
            .encryption_service
            .encrypt(secret.to_base64url().as_bytes())
        {
            Ok(encrypted) => encrypted,
            Err(error) => {
                self.revoke_unused_token(token.id).await;
                return Err(NodePairingAdminError::EncryptSecret {
                    name: name.to_string(),
                    reason: error.to_string(),
                });
            }
        };
        Ok(PairingCredentials {
            ca_fingerprint,
            join_token,
            token_id: token.id,
            expires_at: token.expires_at,
            secret_encrypted,
        })
    }

    /// Mint a single-use enrollment token bound to `name`, valid for
    /// [`PAIRING_TTL_SECS`], with the cluster CA's fingerprint.
    async fn mint_token(
        &self,
        name: &str,
        created_by_user_id: Option<i32>,
        bound_labels: Option<serde_json::Value>,
    ) -> Result<MintedToken, NodePairingAdminError> {
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
                bound_node_name: Some(name.to_string()),
                bound_labels,
                created_by_user_id,
                ca_fingerprint: Some(ca_fingerprint.clone()),
            })
            .await
            .map_err(|source| NodePairingAdminError::MintToken {
                name: name.to_string(),
                source,
            })?;
        Ok(MintedToken {
            ca_fingerprint,
            join_token,
            token,
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
