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
use temps_wireguard::pairing::{self, PairingError, PairingId, PairingSecret, PairingSession};
use tracing::{info, warn};

/// How often the control plane looks for pairings to dial and expires old
/// ones.
const POLL_INTERVAL: Duration = Duration::from_secs(2);
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
            match temps_network::pairing::due(db.as_ref()).await {
                Ok(due) => {
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
                            dial(&db, &encryption, pairing).await;
                            if let Ok(mut set) = in_flight.lock() {
                                set.remove(&id);
                            }
                        });
                    }
                }
                Err(error) => warn!(%error, "could not load node pairings"),
            }
            tokio::time::sleep(POLL_INTERVAL).await;
        }
    });
}

/// One attempt at one pairing; the outcome is recorded on the pairing.
async fn dial(
    db: &DatabaseConnection,
    encryption: &EncryptionService,
    pairing: node_pairings::Model,
) {
    let outcome = attempt(db, encryption, &pairing).await;
    let recorded = match &outcome {
        Ok(public_key) => {
            info!(pairing = pairing.id, node = %pairing.name, "node pairing received the node's key");
            temps_network::pairing::record_key(db, pairing.id, public_key).await
        }
        Err(message) => temps_network::pairing::record_attempt(db, pairing.id, Some(message)).await,
    };
    if let Err(error) = recorded {
        warn!(pairing = pairing.id, %error, "could not record a node pairing attempt");
    }
}

/// The node's public key, or what the operator should know about why the
/// attempt failed.
async fn attempt(
    db: &DatabaseConnection,
    encryption: &EncryptionService,
    pairing: &node_pairings::Model,
) -> Result<String, String> {
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
    match pairing::initiate(endpoint, &session, &control_plane.public_key, deadline).await {
        Ok(public_key) => Ok(public_key),
        Err(PairingError::TimedOut) => Err(format!(
            "No answer from {endpoint} yet. Run the pairing command on that machine, and make \
             sure UDP port {port} on it accepts traffic from this control plane.",
            port = endpoint.port()
        )),
        Err(error) => {
            warn!(pairing = pairing.id, %error, "node pairing attempt failed");
            Err(format!("Could not reach {endpoint}: {error}"))
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
