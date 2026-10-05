// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! The control plane's side of one-paste node pairing (ADR 048 D2b): dial
//! every waiting pairing's node until it answers with its public key, then
//! record the key so the control plane peers with the node and it can
//! register over the mesh.

use std::collections::HashSet;
use std::net::SocketAddr;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use sea_orm::DatabaseConnection;
use temps_core::EncryptionService;
use temps_entities::node_pairings;
use temps_network::mesh::MeshError;
use temps_wireguard::pairing::{
    self, KeyRefusal, PairingError, PairingId, PairingSecret, PairingSession, RejectReason,
};
use tracing::{info, warn};

/// How often the control plane looks for pairings to dial and expires old
/// ones while any is pending, and when none is.
const POLL_INTERVAL: Duration = Duration::from_secs(2);
const IDLE_POLL_INTERVAL: Duration = Duration::from_secs(10);
/// How long one attempt holds a pairing against other control-plane
/// processes: the attempt plus its confirmation, with margin.
const DIAL_LEASE: Duration = Duration::from_secs(30);
/// How long one dialing attempt sends HELLOs before it is recorded as
/// unanswered and started again.
const ATTEMPT: Duration = Duration::from_secs(10);

/// Start dialing waiting pairings. Runs for the life of the process.
pub fn spawn_pairing_initiator(db: Arc<DatabaseConnection>, encryption: Arc<EncryptionService>) {
    let in_flight: Arc<Mutex<HashSet<i32>>> = Arc::default();
    tokio::spawn(async move {
        loop {
            if let Err(error) = temps_network::pairing::expire_stale(db.as_ref()).await {
                warn!(%error, "could not expire stale node pairings");
            }
            let mut pending = false;
            match temps_network::pairing::due(db.as_ref()).await {
                Ok(due) => {
                    pending = !due.is_empty();
                    for pairing in due {
                        let fresh = in_flight
                            .lock()
                            .map(|mut set| set.insert(pairing.id))
                            .unwrap_or(false);
                        if !fresh {
                            continue;
                        }
                        let db = db.clone();
                        let encryption = encryption.clone();
                        let in_flight = in_flight.clone();
                        tokio::spawn(async move {
                            let id = pairing.id;
                            // Another process sharing the database may be
                            // dialing it; one dialer per pairing.
                            match temps_network::pairing::claim(&db, id, DIAL_LEASE).await {
                                Ok(true) => {
                                    dial(&db, &encryption, pairing).await;
                                    if let Err(error) =
                                        temps_network::pairing::release(&db, id).await
                                    {
                                        warn!(pairing = id, %error, "could not release a node pairing");
                                    }
                                }
                                Ok(false) => {}
                                Err(error) => {
                                    warn!(pairing = id, %error, "could not claim a node pairing")
                                }
                            }
                            if let Ok(mut set) = in_flight.lock() {
                                set.remove(&id);
                            }
                        });
                    }
                }
                Err(error) => warn!(%error, "could not load node pairings"),
            }
            tokio::time::sleep(if pending {
                POLL_INTERVAL
            } else {
                IDLE_POLL_INTERVAL
            })
            .await;
        }
    });
}

/// One attempt at one pairing. The node's key is recorded during the
/// exchange, and the node is confirmed only once it is; a failure is recorded
/// on the pairing for the operator.
async fn dial(
    db: &DatabaseConnection,
    encryption: &EncryptionService,
    pairing: node_pairings::Model,
) {
    match attempt(db, encryption, &pairing).await {
        Ok(()) => {
            info!(pairing = pairing.id, node = %pairing.name, "node pairing received the node's key");
        }
        Err(failure) => {
            let recorded = match &failure {
                Failure::Attempt(message) => {
                    temps_network::pairing::record_attempt(db, pairing.id, Some(message)).await
                }
                Failure::Rejected(message) => {
                    temps_network::pairing::record_rejection(db, pairing.id, message).await
                }
            };
            if let Err(error) = recorded {
                warn!(pairing = pairing.id, %error, "could not record a node pairing attempt");
            }
        }
    }
}

/// Why an attempt did not pair the node, for the operator.
enum Failure {
    Attempt(String),
    /// The node answered and its key was refused.
    Rejected(String),
}

impl From<String> for Failure {
    fn from(message: String) -> Self {
        Self::Attempt(message)
    }
}

async fn attempt(
    db: &DatabaseConnection,
    encryption: &EncryptionService,
    pairing: &node_pairings::Model,
) -> Result<(), Failure> {
    let control_plane = temps_network::mesh::published_control_plane(db)
        .await
        .map_err(|error| {
            warn!(%error, "could not read the control plane's mesh key for pairing");
            "The control plane could not read its mesh state; see the server logs.".to_string()
        })?
        .ok_or_else(|| {
            "The control plane's end of the mesh is not up yet; pairing starts once it is."
                .to_string()
        })?;
    let session = session(encryption, pairing).map_err(|error| {
        warn!(pairing = pairing.id, %error, "node pairing record is unusable");
        "This pairing's stored secret is unreadable; cancel it and create a new one.".to_string()
    })?;
    let endpoint: SocketAddr = pairing.node_endpoint.parse().map_err(|_| {
        "This pairing's node address is invalid; cancel it and create a new one.".to_string()
    })?;
    let deadline = tokio::time::Instant::now() + ATTEMPT;
    let id = pairing.id;
    let accept = |public_key: String| async move {
        temps_network::pairing::record_key(db, id, &public_key)
            .await
            .map_err(|error| match error {
                MeshError::PublicKeyInUse => KeyRefusal::Reject(RejectReason::KeyInUse),
                MeshError::PairingClosed => KeyRefusal::Reject(RejectReason::PairingClosed),
                other => KeyRefusal::Defer(other.to_string()),
            })
    };
    match pairing::initiate(
        endpoint,
        &session,
        &control_plane.public_key,
        deadline,
        accept,
    )
    .await
    {
        Ok(_) => Ok(()),
        Err(PairingError::Rejected(RejectReason::KeyInUse)) => Err(Failure::Rejected(format!(
            "{endpoint} answered, but its WireGuard key already belongs to another node: the \
             key file was copied from another machine (a cloned disk or a shared home \
             directory). The pairing command on it stopped and printed which file to delete; \
             delete it and run the command again."
        ))),
        Err(PairingError::Rejected(reason)) => Err(Failure::Rejected(format!(
            "{endpoint} was refused: {reason}."
        ))),
        Err(PairingError::Deferred(why)) => {
            warn!(pairing = id, %why, "could not record a paired node's key; retrying");
            Err(format!(
                "{endpoint} answered, but the control plane could not record its key yet; \
                 retrying. See the server logs if this persists."
            )
            .into())
        }
        Err(PairingError::TimedOut) => Err(format!(
            "No answer from {endpoint} yet. Run the pairing command on that machine, and make \
             sure UDP port {port} on it accepts traffic from this control plane.",
            port = endpoint.port()
        )
        .into()),
        Err(error) => {
            warn!(pairing = id, %error, "node pairing attempt failed");
            Err(format!("Could not reach {endpoint}: {error}").into())
        }
    }
}

fn session(
    encryption: &EncryptionService,
    pairing: &node_pairings::Model,
) -> Result<PairingSession, String> {
    let secret = encryption
        .decrypt(&pairing.secret_encrypted)
        .map_err(|error| error.to_string())?;
    let secret = String::from_utf8(secret).map_err(|error| error.to_string())?;
    let secret = PairingSecret::from_base64url(&secret).map_err(|error| error.to_string())?;
    let id = PairingId::from_base64url(&pairing.pairing_id).map_err(|error| error.to_string())?;
    Ok(PairingSession::new(id, &secret))
}
