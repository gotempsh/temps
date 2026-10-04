// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Local credential identification and non-destructive, provider-independent verification.
//! No database, Temps authentication, scheduler, or notification dependencies.
//! Detection never performs network I/O; a caller explicitly chooses a verifier.

pub mod detection;
pub mod local;
pub mod presets;
pub mod verification;

pub use detection::{Candidate, CatalogDetector, CredentialDetector, DetectionError};
pub use local::{
    has_expiring_artifact, inspect, ArtifactKind, ExpiringArtifact, Inspection, LocalCheckSpec,
    LocalVerifier, LOCAL_PROVIDER, MAX_LOCAL_INPUT_BYTES,
};
pub use presets::{
    automatic_check, automatic_preset, provider_presets, AutomaticCheck, ProviderPreset,
};
pub use verification::*;
