// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Coordinating identical image builds (ADR 114).
//!
//! When several projects or environments build the same image (same
//! repository, commit, build path and build-time values), a project whose
//! `identical_build_mode` is `queue` or `share` stops running those builds
//! side by side:
//!
//! - [`fingerprint`] computes what "the same image" means, as a keyed hash.
//! - [`coordinator`] makes builds of one group take turns through the
//!   `identical_builds` table, and lets a `share` build deploy the image of an
//!   identical build that just succeeded.
//!
//! The build job (`jobs::identical_build`) is the only caller.

pub mod coordinator;
pub mod fingerprint;

pub use coordinator::{
    access_boundary, Admission, AdmissionEvent, AdmissionObserver, BuildLease, BuildTicket,
    CoordinationTiming, IdenticalBuildCoordinator, LeaderView, ReusedBuild, SharedImage,
};
pub use fingerprint::{
    identify, BuildIdentity, BuildInputs, BuilderKind, DockerfileSource, FingerprintKey,
    NotIdentifiable, SourceIdentity,
};

/// Why coordinating a build failed.
#[derive(Debug, thiserror::Error)]
pub enum IdenticalBuildError {
    #[error(
        "Deployment {deployment_id} waited {waited_secs}s for an identical build to finish and \
         stopped waiting (limit {limit_secs}s). Redeploy, or set this project's identical build \
         mode to 'parallel' to build without waiting"
    )]
    WaitTimedOut {
        deployment_id: i32,
        waited_secs: u64,
        limit_secs: u64,
    },
    #[error(
        "Identical build row {build_id} of deployment {deployment_id} could not be stored: the \
         image it reuses is not valid JSON: {source}"
    )]
    InvalidImage {
        build_id: i32,
        deployment_id: i32,
        #[source]
        source: serde_json::Error,
    },
    #[error("Failed to {operation} identical build coordination for deployment {deployment_id}: {source}")]
    Database {
        deployment_id: i32,
        operation: &'static str,
        #[source]
        source: sea_orm::DbErr,
    },
}

impl IdenticalBuildError {
    pub(crate) fn database(
        deployment_id: i32,
        operation: &'static str,
    ) -> impl FnOnce(sea_orm::DbErr) -> Self {
        move |source| IdenticalBuildError::Database {
            deployment_id,
            operation,
            source,
        }
    }
}
