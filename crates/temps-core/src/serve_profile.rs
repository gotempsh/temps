// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! What this process is allowed to run locally.
//!
//! A single-binary `temps serve` has always assumed it owns a Docker daemon:
//! it builds images, runs application containers, provisions managed
//! databases, and stores backups on its own disk. A control plane hosted in a
//! container has none of that — no Docker socket, no room for user workloads —
//! and it must not pretend otherwise: a scheduler that quietly places a
//! replica on "local" there produces a deployment that can never start.
//!
//! [`LocalWorkloadPolicy`] is the single boot-time answer to "may this process
//! run workloads itself?". It is registered in the plugin service registry by
//! the serve bootstrap, so any plugin can consult it (via `get_service`, with
//! a permissive default) instead of each one re-deriving the answer from the
//! environment.

use std::sync::Arc;

/// Stable identifier for the serve profile, as reported by the platform
/// capabilities endpoint and used in log lines.
pub const PROFILE_FULL: &str = "full";
/// See [`PROFILE_FULL`].
pub const PROFILE_CONTROL_PLANE: &str = "control-plane";

/// Boot-time decision about local workloads, shared with every plugin.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LocalWorkloadPolicy {
    profile: &'static str,
    local_workloads_enabled: bool,
    docker_available: bool,
}

impl LocalWorkloadPolicy {
    /// The historical single-binary behaviour: this process runs workloads,
    /// managed services and backups itself.
    pub fn full(docker_available: bool) -> Self {
        Self {
            profile: PROFILE_FULL,
            local_workloads_enabled: true,
            docker_available,
        }
    }

    /// Hosted control plane: console, observability and orchestration only.
    /// Applications run on remote worker nodes that joined with `temps join`.
    pub fn control_plane(docker_available: bool) -> Self {
        Self {
            profile: PROFILE_CONTROL_PLANE,
            local_workloads_enabled: false,
            docker_available,
        }
    }

    /// `"full"` or `"control-plane"`.
    pub fn profile(&self) -> &'static str {
        self.profile
    }

    /// Whether containers, builds and managed services may run on this host.
    pub fn local_workloads_enabled(&self) -> bool {
        self.local_workloads_enabled
    }

    /// Whether a Docker daemon answered a ping during startup. Reported to
    /// operators verbatim: "not set up" must be distinguishable from
    /// "not built" (see the capabilities endpoint).
    pub fn docker_available(&self) -> bool {
        self.docker_available
    }
}

impl Default for LocalWorkloadPolicy {
    /// Permissive by default so a context that never registers a policy (an
    /// embedded test harness, the standalone proxy bootstrap) keeps the
    /// pre-existing behaviour.
    fn default() -> Self {
        Self::full(true)
    }
}

/// Resolve the policy from a plugin service registry lookup result.
///
/// Every consumer does the same thing with the `Option` the registry returns,
/// so the "absent means full" rule lives here once rather than in each plugin.
pub fn policy_or_default(policy: Option<Arc<LocalWorkloadPolicy>>) -> Arc<LocalWorkloadPolicy> {
    policy.unwrap_or_else(|| Arc::new(LocalWorkloadPolicy::default()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn full_profile_allows_local_workloads() {
        let policy = LocalWorkloadPolicy::full(true);
        assert_eq!(policy.profile(), PROFILE_FULL);
        assert!(policy.local_workloads_enabled());
        assert!(policy.docker_available());
    }

    #[test]
    fn control_plane_profile_forbids_local_workloads() {
        let policy = LocalWorkloadPolicy::control_plane(false);
        assert_eq!(policy.profile(), PROFILE_CONTROL_PLANE);
        assert!(!policy.local_workloads_enabled());
        assert!(!policy.docker_available());
    }

    #[test]
    fn control_plane_with_a_reachable_daemon_still_forbids_local_workloads() {
        // Reachability and permission are different questions: an operator may
        // mount a socket into a control-plane container for diagnostics
        // without that turning it into a worker node.
        let policy = LocalWorkloadPolicy::control_plane(true);
        assert!(policy.docker_available());
        assert!(!policy.local_workloads_enabled());
    }

    #[test]
    fn missing_policy_defaults_to_full() {
        let resolved = policy_or_default(None);
        assert_eq!(resolved.profile(), PROFILE_FULL);
        assert!(resolved.local_workloads_enabled());
    }

    #[test]
    fn present_policy_is_returned_unchanged() {
        let resolved = policy_or_default(Some(Arc::new(LocalWorkloadPolicy::control_plane(false))));
        assert_eq!(resolved.profile(), PROFILE_CONTROL_PLANE);
        assert!(!resolved.local_workloads_enabled());
    }
}
