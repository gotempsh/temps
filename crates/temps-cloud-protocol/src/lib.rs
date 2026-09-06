// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Wire protocol between a self-hosted Temps instance and an optional managed
//! backend.
//!
//! This crate is deliberately public and dependency-light. An operator running
//! a self-hosted instance must be able to read exactly what their instance
//! would send before deciding to connect anything.
//!
//! # Design constraints
//!
//! A released binary cannot be recalled, and instances are never force-upgraded.
//! Old versions will be talking to the backend for years. Two rules follow:
//!
//! 1. **Negotiate, never assume.** Every connection opens with [`Hello`],
//!    which carries the protocol version and a capability set. Neither side
//!    may use a capability the other did not advertise.
//! 2. **Additive changes only.** New fields are optional with defaults; new
//!    message kinds are ignored by peers that do not know them. Removing or
//!    repurposing a field requires a new [`PROTOCOL_VERSION`].
//!
//! # Boundaries
//!
//! This channel is a *control* plane: config, heartbeat, health, enrollment.
//! It never carries end-user application traffic, and the managed backend is
//! never in the request path of a deployed app. If the backend is unreachable,
//! the instance continues on its cached configuration.

#![forbid(unsafe_code)]

pub mod messages;

pub use messages::{
    BackupArtifact, BackupCompleted, BackupCompression, BackupEngine, BackupFormat,
    BackupLifecycleEventAccepted, BackupLifecycleEventRequest, BackupLifecycleStage, BackupTarget,
    BackupTargetRequest, EnrollRequest, EnrollResponse, Envelope, Heartbeat, HeartbeatAck,
    IngestAck, ManagedAiAnalysisRequest, ManagedAiAnalysisResponse, ManagedAiCapability,
    ManagedAiChatRequest, ManagedAiChatResponse, ManagedAiCitation, ManagedAiEvidence,
    ManagedAiTask, ManagedBackupCapability, ManagedNotificationAccepted,
    ManagedNotificationRequest, ManagedNotificationSeverity, NativeSnapshot,
    NativeSnapshotIdentity, NativeSnapshotObjectDeclaration, NativeSnapshotObjectKind,
    NativeSnapshotRequest, SpanRecord, TelemetryBatch, WalGObjectCompleted, WalGObjectDeclaration,
    WalGObjectKind, WalGObjectTarget, WalGObjectTargetRequest, WalGSnapshot, WalGSnapshotCompleted,
    WalGSnapshotRequest,
};

use serde::{Deserialize, Serialize};

/// Bumped only for a breaking change. Additive changes must not bump it.
pub const PROTOCOL_VERSION: u16 = 1;

/// A capability one side is willing to use. Absent = unsupported; the peer
/// must fall back rather than error.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum Capability {
    /// Instance may ship telemetry to the managed backend for longer retention
    /// than local storage provides. Local storage is unaffected either way.
    TelemetryShipping,
    /// Instance may have backups orchestrated centrally. Backup bytes always
    /// travel instance -> object storage directly, never through the backend.
    BackupOrchestration,
    /// Instance accepts managed DNS records and certificate material for a
    /// subdomain issued by the backend.
    ManagedSubdomain,
    /// Instance may submit an operator-approved, locally redacted manifest for
    /// credit-backed managed inference. Source telemetry never moves implicitly.
    ManagedAiInference,
    /// Instance may *read* previously mirrored telemetry back out of the
    /// managed backend to serve console queries (ADR-040).
    ///
    /// Strictly narrower than [`Capability::TelemetryShipping`] and negotiated
    /// separately: shipping is a write the instance chooses to make, reading is
    /// a query surface the backend chooses to expose. A backend that accepts
    /// telemetry but cannot serve it back declines this one, and the instance
    /// then reports "not supported" rather than "unavailable" — a distinction
    /// an operator debugging alone needs, because only one of those two is
    /// worth retrying.
    TelemetryQuery,
    /// A capability introduced by a newer peer. Older agents retain the
    /// connection and simply decline to negotiate the unknown feature.
    #[serde(other)]
    Unknown,
}

/// First frame on every connection, sent by both sides.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Hello {
    pub protocol_version: u16,
    /// Human-readable build identifier, for support and skew diagnostics.
    pub agent_version: String,
    /// What this side is willing to do. The effective set is the intersection.
    pub capabilities: Vec<Capability>,
}

impl Hello {
    /// Capabilities usable on this connection: the intersection of both sides.
    ///
    /// Returns an error only for an incompatible major protocol version --
    /// a capability the peer lacks is a normal, non-fatal outcome.
    pub fn negotiate(&self, peer: &Hello) -> Result<Vec<Capability>, ProtocolError> {
        if peer.protocol_version != PROTOCOL_VERSION {
            return Err(ProtocolError::VersionMismatch {
                ours: PROTOCOL_VERSION,
                theirs: peer.protocol_version,
            });
        }
        Ok(self
            .capabilities
            .iter()
            .copied()
            .filter(|capability| *capability != Capability::Unknown)
            .filter(|c| peer.capabilities.contains(c))
            .collect())
    }
}

/// Why a managed feature is unavailable, so the instance can say something
/// specific instead of failing silently or showing a generic error.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "reason", rename_all = "snake_case")]
#[non_exhaustive]
pub enum Unavailable {
    /// No account is connected. The instance should offer to connect one.
    NotEnrolled,
    /// Enrolled, but the plan does not include this capability.
    NotEntitled { required_plan: String },
    /// Included, but the period allowance is exhausted.
    QuotaExhausted {
        used_bytes: u64,
        limit_bytes: u64,
        resets_at: chrono::DateTime<chrono::Utc>,
    },
    /// Backend reachable but degraded. The instance keeps buffering locally.
    Degraded {
        retry_after_secs: u32,
        detail: String,
    },
    /// A reason introduced by a newer peer. The feature remains unavailable,
    /// but version skew must not make the entire response unreadable.
    #[serde(other)]
    Unknown,
}

#[derive(Debug, thiserror::Error)]
pub enum ProtocolError {
    #[error("protocol version mismatch: ours {ours}, peer {theirs}")]
    VersionMismatch { ours: u16, theirs: u16 },
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hello(caps: &[Capability]) -> Hello {
        Hello {
            protocol_version: PROTOCOL_VERSION,
            agent_version: "test".into(),
            capabilities: caps.to_vec(),
        }
    }

    #[test]
    fn negotiate_yields_the_intersection() {
        let ours = hello(&[
            Capability::TelemetryShipping,
            Capability::BackupOrchestration,
        ]);
        let theirs = hello(&[Capability::TelemetryShipping, Capability::ManagedSubdomain]);
        assert_eq!(
            ours.negotiate(&theirs).unwrap(),
            vec![Capability::TelemetryShipping]
        );
    }

    #[test]
    fn a_capability_the_peer_lacks_is_not_an_error() {
        let ours = hello(&[Capability::TelemetryShipping]);
        let theirs = hello(&[]);
        assert!(ours.negotiate(&theirs).unwrap().is_empty());
    }

    #[test]
    fn version_mismatch_is_fatal() {
        let ours = hello(&[]);
        let mut theirs = hello(&[]);
        theirs.protocol_version = PROTOCOL_VERSION + 1;
        assert!(matches!(
            ours.negotiate(&theirs),
            Err(ProtocolError::VersionMismatch { .. })
        ));
    }

    #[test]
    fn unknown_capabilities_are_tolerated_but_never_negotiated() {
        let peer: Hello = serde_json::from_str(
            r#"{"protocol_version":1,"agent_version":"future","capabilities":["telemetry_shipping","future_export"]}"#,
        )
        .unwrap();
        assert_eq!(
            peer.capabilities,
            vec![Capability::TelemetryShipping, Capability::Unknown]
        );
        assert_eq!(
            hello(&[Capability::TelemetryShipping, Capability::Unknown])
                .negotiate(&peer)
                .unwrap(),
            vec![Capability::TelemetryShipping]
        );
    }

    #[test]
    fn telemetry_query_is_negotiated_independently_of_shipping() {
        // ADR-040: an instance that ships telemetry to a backend which cannot
        // serve it back must end up with shipping enabled and reading absent,
        // not with reading assumed from shipping.
        let ours = hello(&[Capability::TelemetryShipping, Capability::TelemetryQuery]);
        let write_only_backend = hello(&[Capability::TelemetryShipping]);
        assert_eq!(
            ours.negotiate(&write_only_backend)
                .expect("capability absence is never fatal"),
            vec![Capability::TelemetryShipping]
        );
    }

    #[test]
    fn telemetry_query_survives_a_serde_round_trip_as_snake_case() {
        // The wire name is part of the contract with the backend; a rename
        // would silently read as `Unknown` on the peer.
        assert_eq!(
            serde_json::to_string(&Capability::TelemetryQuery).expect("capability must serialize"),
            r#""telemetry_query""#
        );
        assert_eq!(
            serde_json::from_str::<Capability>(r#""telemetry_query""#)
                .expect("capability must parse"),
            Capability::TelemetryQuery
        );
    }

    #[test]
    fn unknown_unavailable_reasons_are_tolerated() {
        let unavailable: Unavailable =
            serde_json::from_str(r#"{"reason":"future_maintenance","window":"soon"}"#).unwrap();
        assert_eq!(unavailable, Unavailable::Unknown);
    }
}
