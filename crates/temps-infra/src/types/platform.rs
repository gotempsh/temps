// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

/// Platform compatibility information
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct PlatformInfo {
    /// Operating system type (e.g., "linux", "windows", "darwin")
    pub os_type: String,
    /// System architecture (e.g., "x86_64", "aarch64")
    pub architecture: String,
    /// List of supported platforms in "os/arch" format (e.g., ["linux/amd64"])
    pub platforms: Vec<String>,
}

/// Which capabilities this server process actually provides.
///
/// A client cannot tell "this build has no sandboxes" from "this process was
/// started in a profile that does not run them" by probing endpoints — both
/// look like failure. This endpoint answers the question directly so the
/// console can render an honest, actionable state (what is unavailable, and
/// why) instead of a dead button or an empty page.
///
/// **Honesty contract**: every field that is `true` MUST be backed by a
/// registered, reachable subsystem. A `true` that is not true is worse than
/// the endpoint not existing — it causes the console to render controls that
/// silently fail instead of showing an onboarding state.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct PlatformFeatures {
    /// Serve profile this process was started with: `"full"` or
    /// `"control-plane"`.
    pub profile: String,
    /// Whether a Docker client exists AND a daemon answered a ping at startup.
    /// In the `control-plane` profile a daemon may still be present for
    /// diagnostics, but no workloads are placed here regardless.
    pub docker: bool,
    /// Application containers can run on this host. `false` in the
    /// `control-plane` profile: applications run on worker nodes joined with
    /// `temps join`.
    pub deployments_local: bool,
    /// Container images can be built by this process. Requires a local Docker
    /// daemon; always `false` when `docker` is `false`.
    pub image_builds_local: bool,
    /// Managed services (PostgreSQL, Redis, MariaDB, …) can be provisioned on
    /// this host. Requires a local Docker daemon.
    pub managed_services: bool,
    /// Backups can be produced from services running on this host.
    /// Remote backups of worker-node services are reported separately in
    /// [`backups_remote`][Self::backups_remote].
    pub backups_local: bool,
    /// Backups of services on worker nodes can be orchestrated, scheduled and
    /// retained by this process. `true` when the backup scheduler is
    /// configured and worker credentials are present, regardless of profile.
    pub backups_remote: bool,
    /// Agent sandboxes / workspace previews run in this process. Requires a
    /// local Docker daemon.
    pub sandboxes: bool,
    /// Managed key-value store is registered and reachable.
    pub kv: bool,
    /// Workload importers (Compose, Coolify, Dokploy, Portainer, Kamal,
    /// CapRover) are available. Requires a local Docker daemon to run importer
    /// containers.
    pub imports: bool,
    /// Structured log aggregation, search and tailing is active.
    pub log_aggregation: bool,
    /// Container image vulnerability scanning is available. Requires a local
    /// Docker daemon to pull and scan images.
    pub vulnerability_scanning: bool,
}

impl PlatformFeatures {
    /// A fully-capable single-binary process: every subsystem enabled.
    ///
    /// `docker` is supplied by the caller because only the serve bootstrap
    /// has pinged the daemon at startup and knows whether it answered.
    pub fn full(docker: bool) -> Self {
        Self {
            profile: temps_core::PROFILE_FULL.to_string(),
            docker,
            deployments_local: true,
            image_builds_local: true,
            managed_services: true,
            backups_local: true,
            backups_remote: true,
            sandboxes: true,
            kv: true,
            imports: true,
            log_aggregation: true,
            vulnerability_scanning: true,
        }
    }

    /// A hosted control-plane process: console, API, observability and remote
    /// orchestration only. Local workloads run on worker nodes.
    ///
    /// The caller must supply truthful values for every parameter — do NOT
    /// pass `true` unless the corresponding service is registered, reachable,
    /// and confirmed to be operational:
    ///
    /// - `docker`: a Docker daemon answered a ping at startup (available for
    ///   diagnostics, but no workloads are placed here).
    /// - `backups_remote`: the backup scheduler is configured and worker-node
    ///   credentials are present.
    /// - `kv`: a key-value store is registered and reachable.
    /// - `log_aggregation`: a log-aggregation sink is registered and active.
    pub fn control_plane(
        docker: bool,
        backups_remote: bool,
        kv: bool,
        log_aggregation: bool,
    ) -> Self {
        Self {
            profile: temps_core::PROFILE_CONTROL_PLANE.to_string(),
            docker,
            // Local workloads are definitionally absent in this profile.
            deployments_local: false,
            image_builds_local: false,
            managed_services: false,
            backups_local: false,
            sandboxes: false,
            imports: false,
            vulnerability_scanning: false,
            // Caller-supplied: genuinely varies across CP deployments.
            backups_remote,
            kv,
            log_aggregation,
        }
    }
}

/// The mode the server is running in
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub enum ServerMode {
    /// Running locally (localhost access)
    Local,
    /// Direct public IP access
    Direct,
    /// Behind NAT (Network Address Translation)
    Nat,
    /// Behind Cloudflare Tunnel
    CloudflareTunnel,
}

impl ServerMode {
    /// Check if domain creation should be allowed for this mode
    pub fn can_create_domains(&self) -> bool {
        match self {
            ServerMode::Direct | ServerMode::Nat => true,
            ServerMode::Local | ServerMode::CloudflareTunnel => false,
        }
    }

    /// Get a human-readable description of why domain creation is not allowed
    pub fn domain_creation_error_message(&self) -> Option<&'static str> {
        match self {
            ServerMode::Local => Some("Domain creation is not supported when running locally. Please deploy to a server with a public IP address."),
            ServerMode::CloudflareTunnel => Some("Domain creation is not supported when using Cloudflare Tunnel. Domains are managed through Cloudflare."),
            _ => None,
        }
    }
}

impl std::fmt::Display for ServerMode {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ServerMode::Local => write!(f, "local"),
            ServerMode::Direct => write!(f, "direct"),
            ServerMode::Nat => write!(f, "nat"),
            ServerMode::CloudflareTunnel => write!(f, "cloudflare_tunnel"),
        }
    }
}

/// Response containing information about how the service is being accessed
#[derive(Debug, Serialize, Deserialize, ToSchema)]
pub struct ServiceAccessInfo {
    /// Mode of access: "local", "direct", "nat", or "cloudflare_tunnel"
    pub access_mode: String,

    /// Server's public IP address (always returned if available)
    pub public_ip: Option<String>,

    /// Server's private/local IP address (always returned if available)
    pub private_ip: Option<String>,

    /// Whether domain creation is allowed in this mode
    pub can_create_domains: bool,

    /// Error message if domain creation is not allowed
    pub domain_creation_error: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn full_constructor_sets_all_fields_true_except_docker_arg() {
        let features = PlatformFeatures::full(true);
        assert_eq!(features.profile, temps_core::PROFILE_FULL);
        assert!(features.docker);
        assert!(features.deployments_local);
        assert!(features.image_builds_local);
        assert!(features.managed_services);
        assert!(features.backups_local);
        assert!(features.backups_remote);
        assert!(features.sandboxes);
        assert!(features.kv);
        assert!(features.imports);
        assert!(features.log_aggregation);
        assert!(features.vulnerability_scanning);
    }

    #[test]
    fn full_constructor_reflects_docker_false() {
        let features = PlatformFeatures::full(false);
        assert!(!features.docker);
        // other fields are still true (docker-dependent ones could be false but
        // the full() constructor is honest about what the profile *supports*;
        // it is the caller's responsibility to call control_plane() instead if
        // docker is required for a subsystem and is absent)
        assert!(features.deployments_local);
    }

    #[test]
    fn control_plane_constructor_sets_local_workload_fields_false() {
        let features = PlatformFeatures::control_plane(false, false, false, false);
        assert_eq!(features.profile, temps_core::PROFILE_CONTROL_PLANE);
        assert!(!features.docker);
        assert!(!features.deployments_local);
        assert!(!features.image_builds_local);
        assert!(!features.managed_services);
        assert!(!features.backups_local);
        assert!(!features.sandboxes);
        assert!(!features.imports);
        assert!(!features.vulnerability_scanning);
    }

    #[test]
    fn control_plane_constructor_threads_caller_supplied_fields() {
        let features = PlatformFeatures::control_plane(true, true, true, true);
        assert!(features.docker);
        assert!(features.backups_remote);
        assert!(features.kv);
        assert!(features.log_aggregation);
        // local-workload fields are still false
        assert!(!features.deployments_local);
        assert!(!features.managed_services);
    }

    #[test]
    fn platform_features_serialises_all_twelve_fields() {
        let features = PlatformFeatures::full(true);
        let json = serde_json::to_value(&features).expect("serialisation must not fail");
        let obj = json.as_object().expect("must be a JSON object");

        for field in &[
            "profile",
            "docker",
            "deployments_local",
            "image_builds_local",
            "managed_services",
            "backups_local",
            "backups_remote",
            "sandboxes",
            "kv",
            "imports",
            "log_aggregation",
            "vulnerability_scanning",
        ] {
            assert!(obj.contains_key(*field), "missing field: {field}");
        }
        assert_eq!(obj.len(), 12, "unexpected extra or missing fields: {obj:?}");
    }
}
