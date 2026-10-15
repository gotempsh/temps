// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Trust boundary for a build whose result is reported by a runner the
//! control plane does not control.
//!
//! A worker node that builds through the agent is part of the cluster: it was
//! enrolled, it holds a credential and the control plane streams the build to
//! it. A runner started by a CI provider, or a pull-mode runner on a machine
//! that also runs other things, is not. Whatever it says about a build is a
//! claim, and the claim decides which image a deployment starts.
//!
//! This module holds the rules for accepting that claim, deliberately free of
//! I/O so they can be tested exhaustively and cannot drift between executors:
//!
//! - A runner reports a **digest**, never an image reference. The reference is
//!   built here, from a repository the control plane chose before dispatch, so
//!   a runner cannot redirect a deployment to an image of its choosing
//!   ([`VerifiedBuild::image_reference`]).
//! - The reported digest, platforms and run handle are compared with what was
//!   dispatched and with a digest the control plane observed on its own
//!   ([`verify_runner_report`]).
//! - An integrity or identity failure is never eligible for fallback to a
//!   local build, whatever the policy says ([`RemoteBuildFailure::disposition`]):
//!   a failed check has to fail the deployment, not quietly build something
//!   else.
//!
//! Nothing here is wired into a deployment yet. See
//! `docs/architecture/multi-node-and-build-dispatch.md` for where it sits.

use std::collections::BTreeSet;
use std::fmt;

use thiserror::Error;

use crate::platform::canonicalize_platform;

/// Longest runner-supplied value echoed into an error message. Runner output
/// is untrusted and ends up in build logs the operator reads.
const MAX_ECHOED_VALUE_CHARS: usize = 80;

/// `sha256:` followed by 64 lowercase hexadecimal characters.
const SHA256_PREFIX: &str = "sha256:";
const SHA256_HEX_LEN: usize = 64;

/// Longest repository (registry host, port and path) accepted for a dispatch.
const MAX_REPOSITORY_LEN: usize = 255;

/// A content digest in the one spelling the control plane accepts.
///
/// Only `sha256` with lowercase hex is accepted. Registries and the OCI image
/// spec allow other algorithms and the hex alphabet is case-insensitive in
/// some tools, but two spellings of one digest is how string comparison turns
/// a match into a mismatch, or worse, a mismatch into a match.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ImageDigest(String);

impl ImageDigest {
    /// Parse `value`, which must already be in canonical form.
    pub fn parse(value: &str) -> Result<Self, BuildVerificationError> {
        let malformed = || BuildVerificationError::MalformedDigest {
            reported: clip(value),
        };
        let hex = value.strip_prefix(SHA256_PREFIX).ok_or_else(malformed)?;
        let canonical = hex.len() == SHA256_HEX_LEN
            && hex
                .bytes()
                .all(|byte| byte.is_ascii_digit() || matches!(byte, b'a'..=b'f'));
        if !canonical {
            return Err(malformed());
        }
        Ok(Self(value.to_string()))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for ImageDigest {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// What the control plane asked for, recorded before the build was handed to
/// an executor.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DispatchedBuild {
    /// Opaque identifier the runner received; the only thing a CI-side shim
    /// is given.
    pub build_id: String,
    pub deployment_id: i32,
    /// The executor's handle for this run (a provider run id, a lease id),
    /// persisted when the build was acquired. `None` when the executor has no
    /// such handle.
    pub run_id: Option<String>,
    /// Platforms the build was asked to produce.
    pub platforms: BTreeSet<String>,
    /// Repository the resulting image is deployed from, derived by the
    /// control plane from the project's pinned namespace. The reference that
    /// is deployed is always `<repository>@<digest>`.
    pub image_repository: String,
}

/// The claim a runner makes about a finished build.
///
/// There is no image-reference field, on purpose.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RunnerReport {
    pub build_id: String,
    pub run_id: Option<String>,
    pub digest: String,
    pub platforms: Vec<String>,
}

/// A report that passed every check, with the reference built server-side.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VerifiedBuild {
    pub digest: ImageDigest,
    /// `<repository>@<digest>`, never taken from the runner.
    pub image_reference: String,
    /// Canonical platforms, equal to what was dispatched.
    pub platforms: BTreeSet<String>,
}

/// Why a runner's report was refused. Every variant is an integrity failure:
/// none of them is retried or satisfied by building locally.
#[derive(Debug, Error, PartialEq, Eq)]
pub enum BuildVerificationError {
    #[error(
        "Dispatch record for deployment {deployment_id} is unusable: repository '{repository}' \
         is not a bare image repository ({reason})"
    )]
    InvalidRepository {
        deployment_id: i32,
        repository: String,
        reason: &'static str,
    },

    #[error("Dispatch record for deployment {deployment_id} requests no build platform")]
    NoPlatformsRequested { deployment_id: i32 },

    #[error(
        "Runner reported build '{reported}' for deployment {deployment_id}, \
         but build '{expected}' was dispatched"
    )]
    BuildIdMismatch {
        deployment_id: i32,
        expected: String,
        reported: String,
    },

    #[error(
        "Runner reported run {reported:?} for build '{build_id}' of deployment {deployment_id}, \
         but run {expected:?} was dispatched"
    )]
    RunIdMismatch {
        deployment_id: i32,
        build_id: String,
        expected: Option<String>,
        reported: Option<String>,
    },

    #[error(
        "Runner reported a malformed image digest '{reported}'; expected sha256:<64 lowercase hex>"
    )]
    MalformedDigest { reported: String },

    #[error(
        "Runner reported digest {reported} for build '{build_id}' of deployment {deployment_id}, \
         but the control plane observed {observed}"
    )]
    DigestMismatch {
        deployment_id: i32,
        build_id: String,
        reported: ImageDigest,
        observed: ImageDigest,
    },

    #[error(
        "Runner reported platforms [{reported}] for build '{build_id}' of deployment \
         {deployment_id}, but [{expected}] were requested"
    )]
    PlatformMismatch {
        deployment_id: i32,
        build_id: String,
        expected: String,
        reported: String,
    },
}

/// Check a runner's report against what was dispatched and against a digest
/// the control plane resolved itself.
///
/// `observed` must come from the control plane's own look at the artifact (a
/// registry resolve, or the id the daemon assigned on import), never from the
/// runner. A report that only agrees with itself proves nothing.
pub fn verify_runner_report(
    dispatched: &DispatchedBuild,
    report: &RunnerReport,
    observed: &ImageDigest,
) -> Result<VerifiedBuild, BuildVerificationError> {
    let deployment_id = dispatched.deployment_id;
    validate_repository(deployment_id, &dispatched.image_repository)?;

    let expected_platforms: BTreeSet<String> = dispatched
        .platforms
        .iter()
        .map(|platform| canonicalize_platform(platform))
        .collect();
    if expected_platforms.is_empty() {
        return Err(BuildVerificationError::NoPlatformsRequested { deployment_id });
    }

    if report.build_id != dispatched.build_id {
        return Err(BuildVerificationError::BuildIdMismatch {
            deployment_id,
            expected: dispatched.build_id.clone(),
            reported: clip(&report.build_id),
        });
    }

    // Strict on both sides: a runner that names a run the control plane never
    // recorded is as wrong as one that names a different run.
    if report.run_id != dispatched.run_id {
        return Err(BuildVerificationError::RunIdMismatch {
            deployment_id,
            build_id: dispatched.build_id.clone(),
            expected: dispatched.run_id.clone(),
            reported: report.run_id.as_deref().map(clip),
        });
    }

    let reported_digest = ImageDigest::parse(&report.digest)?;
    if &reported_digest != observed {
        return Err(BuildVerificationError::DigestMismatch {
            deployment_id,
            build_id: dispatched.build_id.clone(),
            reported: reported_digest,
            observed: observed.clone(),
        });
    }

    // Exact set equality: a platform nobody asked for is as suspect as one
    // that is missing.
    let reported_platforms: BTreeSet<String> = report
        .platforms
        .iter()
        .map(|platform| canonicalize_platform(platform))
        .collect();
    if reported_platforms != expected_platforms {
        return Err(BuildVerificationError::PlatformMismatch {
            deployment_id,
            build_id: dispatched.build_id.clone(),
            expected: expected_platforms
                .iter()
                .map(String::as_str)
                .collect::<Vec<_>>()
                .join(", "),
            reported: report
                .platforms
                .iter()
                .map(|platform| clip(platform))
                .collect::<Vec<_>>()
                .join(", "),
        });
    }

    Ok(VerifiedBuild {
        image_reference: format!("{}@{}", dispatched.image_repository, reported_digest),
        digest: reported_digest,
        platforms: expected_platforms,
    })
}

/// Why an executor could not deliver a build, grouped by what the control
/// plane may do next.
#[derive(Debug, Error, PartialEq, Eq)]
pub enum RemoteBuildFailure {
    #[error("No build executor is configured for deployment {deployment_id}")]
    NotConfigured { deployment_id: i32 },

    #[error("No eligible runner for deployment {deployment_id}: {reason}")]
    NoEligibleRunner { deployment_id: i32, reason: String },

    #[error(
        "Build for deployment {deployment_id} waited {waited_secs}s for a runner \
         (limit {limit_secs}s)"
    )]
    QueueTimeout {
        deployment_id: i32,
        waited_secs: u64,
        limit_secs: u64,
    },

    #[error("Runner cannot build for deployment {deployment_id}: {reason}")]
    CapabilityMismatch { deployment_id: i32, reason: String },

    #[error(
        "Build for deployment {deployment_id} lost its runner {attempts} time(s) \
         without finishing"
    )]
    LeaseExpired { deployment_id: i32, attempts: u32 },

    #[error("Could not start a runner for deployment {deployment_id}: {reason}")]
    DispatchFailed { deployment_id: i32, reason: String },

    #[error("Runner for deployment {deployment_id} reported a failed build: {reason}")]
    BuildFailed { deployment_id: i32, reason: String },

    /// The runner finished, but its report did not check out.
    #[error(transparent)]
    Integrity(#[from] BuildVerificationError),

    /// The runner's identity was not accepted by a trusted issuer.
    #[error("Runner identity rejected for deployment {deployment_id}: {reason}")]
    IdentityRejected { deployment_id: i32, reason: String },
}

/// What the control plane does with a [`RemoteBuildFailure`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FailureDisposition {
    /// Write a warning into the build log and build with the local daemon.
    FallBackToLocal,
    /// Mark the deployment failed.
    FailDeployment,
}

impl RemoteBuildFailure {
    /// Whether the control plane may build locally instead.
    ///
    /// `fallback_to_local` is the operator's policy. It applies to the
    /// failures of *not getting a build*: nothing configured, no runner,
    /// timeouts, a lost runner, a dispatch that did not start. It never
    /// applies to a runner that was reached and whose answer failed
    /// verification, or whose identity was refused. A runner that reports its
    /// own build as failed is a failed build, not missing infrastructure:
    /// building the same source again somewhere else would hide a Dockerfile
    /// error behind a slower deploy on the host the operator was protecting.
    pub fn disposition(&self, fallback_to_local: bool) -> FailureDisposition {
        match self {
            Self::Integrity(_) | Self::IdentityRejected { .. } | Self::BuildFailed { .. } => {
                FailureDisposition::FailDeployment
            }
            Self::NotConfigured { .. }
            | Self::NoEligibleRunner { .. }
            | Self::QueueTimeout { .. }
            | Self::CapabilityMismatch { .. }
            | Self::LeaseExpired { .. }
            | Self::DispatchFailed { .. } => {
                if fallback_to_local {
                    FailureDisposition::FallBackToLocal
                } else {
                    FailureDisposition::FailDeployment
                }
            }
        }
    }
}

fn validate_repository(deployment_id: i32, repository: &str) -> Result<(), BuildVerificationError> {
    let invalid = |reason: &'static str| BuildVerificationError::InvalidRepository {
        deployment_id,
        repository: clip(repository),
        reason,
    };
    if repository.is_empty() {
        return Err(invalid("empty"));
    }
    if repository.len() > MAX_REPOSITORY_LEN {
        return Err(invalid("longer than 255 bytes"));
    }
    if repository.contains('@') {
        return Err(invalid("already carries a digest"));
    }
    // A tag lives after the last slash; a registry port is before it.
    let last_segment = repository.rsplit('/').next().unwrap_or(repository);
    if last_segment.contains(':') {
        return Err(invalid("already carries a tag"));
    }
    let allowed = |byte: u8| {
        byte.is_ascii_lowercase()
            || byte.is_ascii_digit()
            || matches!(byte, b'.' | b'-' | b'_' | b'/' | b':')
    };
    if !repository.bytes().all(allowed) {
        return Err(invalid(
            "only lowercase letters, digits and . - _ / : are allowed",
        ));
    }
    if repository.starts_with('/') || repository.ends_with('/') || repository.contains("//") {
        return Err(invalid("has an empty path component"));
    }
    Ok(())
}

/// A runner-supplied value, bounded and with control characters escaped, so
/// it can be quoted in an error without flooding or forging a log line.
fn clip(value: &str) -> String {
    let mut clipped = String::new();
    for (index, character) in value.chars().enumerate() {
        if index == MAX_ECHOED_VALUE_CHARS {
            clipped.push('…');
            break;
        }
        clipped.extend(character.escape_debug());
    }
    clipped
}

#[cfg(test)]
mod tests {
    use super::*;

    const DIGEST_A: &str =
        "sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
    const DIGEST_B: &str =
        "sha256:bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";

    fn dispatched() -> DispatchedBuild {
        DispatchedBuild {
            build_id: "bld_01".into(),
            deployment_id: 42,
            run_id: Some("run-7".into()),
            platforms: BTreeSet::from(["linux/amd64".to_string()]),
            image_repository: "registry.example.test:5000/temps/project-9".into(),
        }
    }

    fn report() -> RunnerReport {
        RunnerReport {
            build_id: "bld_01".into(),
            run_id: Some("run-7".into()),
            digest: DIGEST_A.into(),
            platforms: vec!["linux/amd64".into()],
        }
    }

    fn observed() -> ImageDigest {
        ImageDigest::parse(DIGEST_A).expect("fixture digest is canonical")
    }

    #[test]
    fn accepts_a_report_that_matches_the_dispatch() {
        let verified = verify_runner_report(&dispatched(), &report(), &observed())
            .expect("a matching report is accepted");

        assert_eq!(verified.digest.as_str(), DIGEST_A);
        assert_eq!(
            verified.image_reference,
            format!("registry.example.test:5000/temps/project-9@{DIGEST_A}")
        );
        assert_eq!(
            verified.platforms,
            BTreeSet::from(["linux/amd64".to_string()])
        );
    }

    #[test]
    fn accepts_a_dispatch_with_no_run_handle_when_the_report_has_none() {
        let mut dispatch = dispatched();
        dispatch.run_id = None;
        let mut reported = report();
        reported.run_id = None;

        assert!(verify_runner_report(&dispatch, &reported, &observed()).is_ok());
    }

    #[test]
    fn platform_spellings_are_compared_canonically() {
        let mut reported = report();
        reported.platforms = vec!["Linux/AMD64".into(), "amd64".into()];

        assert!(verify_runner_report(&dispatched(), &reported, &observed()).is_ok());
    }

    #[test]
    fn rejects_another_build_id() {
        let mut reported = report();
        reported.build_id = "bld_02".into();

        let error = verify_runner_report(&dispatched(), &reported, &observed())
            .expect_err("a different build id is refused");

        assert!(matches!(
            error,
            BuildVerificationError::BuildIdMismatch {
                deployment_id: 42,
                ..
            }
        ));
        assert!(error.to_string().contains("deployment 42"));
    }

    #[test]
    fn rejects_a_different_run() {
        let mut reported = report();
        reported.run_id = Some("run-8".into());

        let error = verify_runner_report(&dispatched(), &reported, &observed())
            .expect_err("a different run is refused");

        assert!(matches!(
            error,
            BuildVerificationError::RunIdMismatch { .. }
        ));
    }

    #[test]
    fn rejects_a_run_the_control_plane_never_recorded() {
        let mut dispatch = dispatched();
        dispatch.run_id = None;

        let error = verify_runner_report(&dispatch, &report(), &observed())
            .expect_err("an unrecorded run is refused");

        assert!(matches!(
            error,
            BuildVerificationError::RunIdMismatch { .. }
        ));
    }

    #[test]
    fn rejects_a_report_that_omits_the_recorded_run() {
        let mut reported = report();
        reported.run_id = None;

        let error = verify_runner_report(&dispatched(), &reported, &observed())
            .expect_err("a missing run is refused");

        assert!(matches!(
            error,
            BuildVerificationError::RunIdMismatch { .. }
        ));
    }

    #[test]
    fn rejects_a_digest_the_control_plane_did_not_observe() {
        let mut reported = report();
        reported.digest = DIGEST_B.into();

        let error = verify_runner_report(&dispatched(), &reported, &observed())
            .expect_err("a digest the control plane did not see is refused");

        match error {
            BuildVerificationError::DigestMismatch {
                reported, observed, ..
            } => {
                assert_eq!(reported.as_str(), DIGEST_B);
                assert_eq!(observed.as_str(), DIGEST_A);
            }
            other => panic!("expected DigestMismatch, got {other:?}"),
        }
    }

    #[test]
    fn rejects_malformed_digests() {
        let uppercase = DIGEST_A.to_uppercase().replace("SHA256:", "sha256:");
        let cases = [
            "",
            "sha256:",
            "sha256:abc",
            "sha512:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
            "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
            "SHA256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
            "sha256:gggggggggggggggggggggggggggggggggggggggggggggggggggggggggggggggg",
            "sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
            "sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa\n",
            " sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
            uppercase.as_str(),
        ];

        for case in cases {
            assert!(
                matches!(
                    ImageDigest::parse(case),
                    Err(BuildVerificationError::MalformedDigest { .. })
                ),
                "digest {case:?} must be refused"
            );
        }

        let mut reported = report();
        reported.digest = "sha256:short".into();
        assert!(matches!(
            verify_runner_report(&dispatched(), &reported, &observed()),
            Err(BuildVerificationError::MalformedDigest { .. })
        ));
    }

    #[test]
    fn rejects_an_extra_platform() {
        let mut reported = report();
        reported.platforms = vec!["linux/amd64".into(), "linux/arm64".into()];

        let error = verify_runner_report(&dispatched(), &reported, &observed())
            .expect_err("an unrequested platform is refused");

        assert!(matches!(
            error,
            BuildVerificationError::PlatformMismatch { .. }
        ));
    }

    #[test]
    fn rejects_a_missing_platform() {
        let mut dispatch = dispatched();
        dispatch.platforms = BTreeSet::from(["linux/amd64".to_string(), "linux/arm64".to_string()]);

        let error = verify_runner_report(&dispatch, &report(), &observed())
            .expect_err("a missing platform is refused");

        assert!(matches!(
            error,
            BuildVerificationError::PlatformMismatch { .. }
        ));
    }

    #[test]
    fn rejects_an_empty_platform_report() {
        let mut reported = report();
        reported.platforms = Vec::new();

        assert!(matches!(
            verify_runner_report(&dispatched(), &reported, &observed()),
            Err(BuildVerificationError::PlatformMismatch { .. })
        ));
    }

    #[test]
    fn rejects_a_dispatch_that_requested_no_platform() {
        let mut dispatch = dispatched();
        dispatch.platforms.clear();

        assert_eq!(
            verify_runner_report(&dispatch, &report(), &observed()),
            Err(BuildVerificationError::NoPlatformsRequested { deployment_id: 42 })
        );
    }

    #[test]
    fn rejects_repositories_that_would_change_what_is_deployed() {
        let cases = [
            ("", "empty"),
            (
                "registry.example.test/app@sha256:abc",
                "already carries a digest",
            ),
            ("registry.example.test/app:latest", "already carries a tag"),
            ("registry.example.test/App", "only lowercase"),
            ("registry.example.test/app name", "only lowercase"),
            ("/app", "empty path component"),
            ("registry.example.test//app", "empty path component"),
            ("registry.example.test/app/", "empty path component"),
        ];

        for (repository, reason) in cases {
            let mut dispatch = dispatched();
            dispatch.image_repository = repository.into();

            let error = verify_runner_report(&dispatch, &report(), &observed())
                .expect_err("an unusable repository is refused");

            match error {
                BuildVerificationError::InvalidRepository {
                    deployment_id,
                    reason: found,
                    ..
                } => {
                    assert_eq!(deployment_id, 42);
                    assert!(
                        found.contains(reason),
                        "repository {repository:?}: expected reason containing {reason:?}, got {found:?}"
                    );
                }
                other => {
                    panic!("repository {repository:?}: expected InvalidRepository, got {other:?}")
                }
            }
        }
    }

    #[test]
    fn accepts_a_registry_with_a_port() {
        let mut dispatch = dispatched();
        dispatch.image_repository = "localhost:5000/temps/app".into();

        let verified = verify_runner_report(&dispatch, &report(), &observed())
            .expect("a registry port is not a tag");

        assert_eq!(
            verified.image_reference,
            format!("localhost:5000/temps/app@{DIGEST_A}")
        );
    }

    #[test]
    fn echoed_runner_values_are_bounded_and_escaped() {
        let mut reported = report();
        reported.build_id = format!("{}\n[error] forged line", "x".repeat(500));

        let error = verify_runner_report(&dispatched(), &reported, &observed())
            .expect_err("a different build id is refused");
        let message = error.to_string();

        assert!(
            !message.contains('\n'),
            "newline must be escaped: {message}"
        );
        assert!(message.len() < 400, "message must stay bounded: {message}");
        assert!(message.contains('…'));
    }

    fn not_configured() -> RemoteBuildFailure {
        RemoteBuildFailure::NotConfigured { deployment_id: 42 }
    }

    fn integrity() -> RemoteBuildFailure {
        RemoteBuildFailure::from(BuildVerificationError::DigestMismatch {
            deployment_id: 42,
            build_id: "bld_01".into(),
            reported: ImageDigest::parse(DIGEST_B).expect("canonical"),
            observed: ImageDigest::parse(DIGEST_A).expect("canonical"),
        })
    }

    #[test]
    fn missing_infrastructure_falls_back_only_when_policy_allows() {
        let failures = [
            not_configured(),
            RemoteBuildFailure::NoEligibleRunner {
                deployment_id: 42,
                reason: "no node advertises linux/arm64".into(),
            },
            RemoteBuildFailure::QueueTimeout {
                deployment_id: 42,
                waited_secs: 301,
                limit_secs: 300,
            },
            RemoteBuildFailure::CapabilityMismatch {
                deployment_id: 42,
                reason: "runner cannot build two platforms".into(),
            },
            RemoteBuildFailure::LeaseExpired {
                deployment_id: 42,
                attempts: 3,
            },
            RemoteBuildFailure::DispatchFailed {
                deployment_id: 42,
                reason: "workflow file not found".into(),
            },
        ];

        for failure in failures {
            assert_eq!(
                failure.disposition(true),
                FailureDisposition::FallBackToLocal,
                "{failure}"
            );
            assert_eq!(
                failure.disposition(false),
                FailureDisposition::FailDeployment,
                "{failure}"
            );
        }
    }

    #[test]
    fn integrity_and_identity_failures_never_fall_back() {
        let failures = [
            integrity(),
            RemoteBuildFailure::IdentityRejected {
                deployment_id: 42,
                reason: "issuer is not trusted".into(),
            },
        ];

        for failure in failures {
            assert_eq!(
                failure.disposition(true),
                FailureDisposition::FailDeployment,
                "{failure}"
            );
            assert_eq!(
                failure.disposition(false),
                FailureDisposition::FailDeployment,
                "{failure}"
            );
        }
    }

    #[test]
    fn a_failed_build_is_not_retried_on_the_control_plane() {
        let failure = RemoteBuildFailure::BuildFailed {
            deployment_id: 42,
            reason: "Dockerfile:12 command not found".into(),
        };

        assert_eq!(
            failure.disposition(true),
            FailureDisposition::FailDeployment
        );
    }

    #[test]
    fn verification_errors_convert_into_the_never_fallback_class() {
        let error = verify_runner_report(
            &dispatched(),
            &RunnerReport {
                digest: DIGEST_B.into(),
                ..report()
            },
            &observed(),
        )
        .expect_err("digest differs from the observed one");

        let failure: RemoteBuildFailure = error.into();

        assert_eq!(
            failure.disposition(true),
            FailureDisposition::FailDeployment
        );
        assert!(failure.to_string().contains("deployment 42"));
    }
}
