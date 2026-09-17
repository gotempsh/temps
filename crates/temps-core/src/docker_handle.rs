// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! The process-wide Docker client, which may legitimately not exist.
//!
//! Every plugin used to do `require_service::<bollard::Docker>()`, which
//! encodes an assumption that turned out to be wrong: that a `temps serve`
//! process always owns a Docker daemon. A control plane hosted in a container
//! with no socket mounted owns none, and must still serve the console, the
//! API, the proxy and every observability subsystem.
//!
//! Registering `Option<Arc<Docker>>` in the service registry does not work —
//! the registry is keyed by `TypeId` and the absence of a key is exactly what
//! makes `require_service` panic. So the *handle* is the registered service,
//! and it is always present:
//!
//! ```ignore
//! let docker = context.require_service::<DockerHandle>();
//! // ... later, on a path that genuinely needs the daemon:
//! let client = docker.require()?; // typed DockerUnavailable, never a panic
//! ```
//!
//! [`DockerHandle::require`] returns a typed error carrying the reason, so
//! every caller can map it into its own error enum and surface an honest,
//! actionable message instead of a raw bollard I/O error or a panic.

use std::sync::Arc;

use bollard::Docker;

/// A path that needs the local Docker daemon ran in a process that has none.
///
/// Carries the serve profile and the original reason so the message an
/// operator sees names both what was attempted and why it cannot work here.
#[derive(Debug, Clone, thiserror::Error)]
#[error(
    "This process has no local Docker daemon (serve profile '{profile}'): {reason}. \
     Operations that run containers, build images or manage local services are \
     unavailable here; they run on worker nodes joined with `temps join`"
)]
pub struct DockerUnavailable {
    /// `"full"` or `"control-plane"` — see [`crate::serve_profile`].
    pub profile: &'static str,
    /// Why no client exists: the profile forbids it, or construction failed.
    pub reason: String,
}

/// The process-wide Docker client, or an explanation of its absence.
///
/// Always registered in the service registry, in every profile, so consumers
/// use `require_service` (the handle is genuinely required) and then make the
/// *daemon* optional at the point of use.
#[derive(Debug, Clone)]
pub enum DockerHandle {
    /// A client was constructed. Says nothing about reachability — a bollard
    /// client is a lazy descriptor; see [`crate::LocalWorkloadPolicy::docker_available`].
    Available(Arc<Docker>),
    /// No client exists in this process, and this is why.
    Disabled {
        /// Serve profile this process was started with.
        profile: &'static str,
        /// Human-readable reason, rendered verbatim into errors and logs.
        reason: String,
    },
}

impl DockerHandle {
    /// Wrap a constructed client.
    pub fn available(docker: Arc<Docker>) -> Self {
        Self::Available(docker)
    }

    /// Record that this process deliberately has no client.
    pub fn disabled(profile: &'static str, reason: impl Into<String>) -> Self {
        Self::Disabled {
            profile,
            reason: reason.into(),
        }
    }

    /// The client, if there is one. Use on best-effort paths that degrade
    /// gracefully; use [`DockerHandle::require`] where absence is an error.
    pub fn get(&self) -> Option<&Arc<Docker>> {
        match self {
            Self::Available(docker) => Some(docker),
            Self::Disabled { .. } => None,
        }
    }

    /// Owned clone of the client, if there is one.
    pub fn cloned(&self) -> Option<Arc<Docker>> {
        self.get().cloned()
    }

    /// Whether a client exists in this process.
    pub fn is_available(&self) -> bool {
        matches!(self, Self::Available(_))
    }

    /// The client, or a typed error naming the profile and the reason.
    ///
    /// This is the one call every daemon-dependent code path should make, as
    /// late as possible: it converts "no Docker here" from a panic or an
    /// inscrutable connection error into something a caller can map onto a
    /// 409 or a job failure with a remedy in the text.
    pub fn require(&self) -> Result<Arc<Docker>, DockerUnavailable> {
        match self {
            Self::Available(docker) => Ok(docker.clone()),
            Self::Disabled { profile, reason } => Err(DockerUnavailable {
                profile,
                reason: reason.clone(),
            }),
        }
    }

    /// The same error [`DockerHandle::require`] would produce, without the
    /// client. For guards that must reject before they have anything to do
    /// with a client at all.
    pub fn unavailable_error(&self) -> Option<DockerUnavailable> {
        match self {
            Self::Available(_) => None,
            Self::Disabled { profile, reason } => Some(DockerUnavailable {
                profile,
                reason: reason.clone(),
            }),
        }
    }
}

/// The reason recorded for a control-plane-profile process, so the wording
/// exists once rather than at every construction site.
pub const CONTROL_PLANE_DOCKER_REASON: &str =
    "it was started with `--profile control-plane`, which runs no local workloads and \
     never connects to a Docker daemon";

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn disabled_handle_requires_into_a_typed_error() {
        let handle =
            DockerHandle::disabled(crate::PROFILE_CONTROL_PLANE, CONTROL_PLANE_DOCKER_REASON);

        assert!(!handle.is_available());
        assert!(handle.get().is_none());

        let error = handle.require().expect_err("no client exists");
        assert_eq!(error.profile, crate::PROFILE_CONTROL_PLANE);

        // The rendered message has to carry the remedy: for a self-hosted
        // operator this text is the only place the answer appears.
        let rendered = error.to_string();
        assert!(rendered.contains("control-plane"), "{rendered}");
        assert!(rendered.contains("temps join"), "{rendered}");
    }

    #[test]
    fn unavailable_error_matches_require() {
        let handle = DockerHandle::disabled(crate::PROFILE_CONTROL_PLANE, "no socket");
        let from_require = handle.require().expect_err("disabled").to_string();
        let standalone = handle.unavailable_error().expect("disabled").to_string();
        assert_eq!(from_require, standalone);
    }

    #[test]
    fn an_available_handle_reports_no_error() {
        // Constructing a real bollard client needs no daemon, only a socket
        // path, so this is a pure in-memory check of the enum's behaviour.
        let Ok(docker) = Docker::connect_with_local_defaults() else {
            // No socket path on this machine: the Disabled arm is covered by
            // the tests above, and there is nothing daemon-independent left
            // to assert here.
            return;
        };
        let handle = DockerHandle::available(Arc::new(docker));
        assert!(handle.is_available());
        assert!(handle.get().is_some());
        assert!(handle.unavailable_error().is_none());
        assert!(handle.require().is_ok());
    }
}
