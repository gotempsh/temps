// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Shield on workers (ADR 066, D10, I3, I5 and I8): what the agent and the
//! control plane must agree on. The decision itself is `temps-shield`'s;
//! this is only the wire between the two.

use serde::{Deserialize, Serialize};

/// The capability an agent declares when its public ingress runs Shield
/// itself. Only a snapshot request that declares it gets routes whose
/// environment acts on verdicts (`selective` or `attack`).
pub const SHIELD_CAPABILITY: &str = "shield-v1";

/// HKDF info string the challenge keys are sealed to a node under, so the
/// sealed payload can never be opened as a certificate bundle, or the
/// reverse.
pub const SHIELD_KEYS_INFO: &[u8] = b"temps-shield-keys-v1";

/// Most events one upload may carry.
pub const MAX_EVENTS_PER_UPLOAD: usize = 5000;

/// Most bytes one upload may decompress to.
pub const MAX_EVENTS_DECOMPRESSED_BYTES: usize = 8 * 1024 * 1024;

/// Most bytes one compressed upload may be.
pub const MAX_EVENTS_COMPRESSED_BYTES: usize = 2 * 1024 * 1024;

/// One environment's Shield settings as a worker applies them to a route:
/// the effective mode, the exempt paths and the hashes of the bypass tokens
/// valid for it (its own and its project's). Never in the snapshot body,
/// which is the same for every environment.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ShieldRoutePolicy {
    pub project_id: i32,
    pub environment_id: i32,
    /// `off`, `log`, `selective` or `attack`.
    pub mode: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub exempt_paths: Vec<String>,
    /// Lowercase hex SHA-256 of each token.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub bypass_token_hashes: Vec<String>,
}

/// Which Shield snapshot is current, in a worker's route snapshot. A worker
/// fetches the body from the snapshot endpoint when the revision differs
/// from the one it has; revision 0 means none is stored yet, and the
/// worker evaluates against the built-in lists alone.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct ShieldRef {
    pub revision: u64,
}

/// The challenge keys, sealed to the node's ingress key with ECIES under
/// [`SHIELD_KEYS_INFO`]. They are opened only into memory.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SealedShieldKeys {
    pub ephemeral_public_key: String,
    pub ciphertext: String,
    pub nonce: String,
}

/// What [`SealedShieldKeys`] holds: each key the challenge gate derives,
/// under the label it derives it for, so a worker builds the same gate as
/// the control plane without the master key.
#[derive(Serialize, Deserialize)]
pub struct ShieldKeyMaterial {
    pub keys: Vec<ShieldKeyEntry>,
}

#[derive(Serialize, Deserialize)]
pub struct ShieldKeyEntry {
    pub label: String,
    /// Base64 of the 32-byte key.
    pub key: String,
}

/// `GET /internal/nodes/{id}/shield/snapshot?revision=N`, when the
/// control plane has something newer than `N` (else `304`).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ShieldSnapshotResponse {
    /// The newest stored revision; 0 when none is stored yet.
    pub revision: u64,
    /// Hex SHA-256 of `body`, as stored.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sha256: Option<String>,
    /// Base64 of the stored body (postcard, then zstd). Absent when no
    /// snapshot is stored yet.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub body: Option<String>,
    /// Absent when the node has no ingress key to seal them to: the worker
    /// then cannot issue challenges.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub keys: Option<SealedShieldKeys>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct ShieldSnapshotQuery {
    /// The revision the worker has; 0 for none.
    #[serde(default)]
    pub revision: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ShieldEventsAccepted {
    pub accepted: usize,
}
