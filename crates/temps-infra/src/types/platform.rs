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
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct PlatformFeatures {
    /// Serve profile this process was started with: `"full"` or
    /// `"control-plane"`.
    pub profile: String,
    /// Whether application containers can be deployed onto this host.
    /// `false` in the control-plane profile: applications run on worker nodes
    /// that joined the cluster with `temps join`.
    pub deployments_local: bool,
    /// Whether managed services (PostgreSQL, Redis, MariaDB, ...) can be
    /// provisioned on this host.
    pub managed_services: bool,
    /// Whether backups can be produced from services running on this host.
    /// Remote backups of worker-node services are unaffected.
    pub backups_local: bool,
    /// Whether agent sandboxes / workspace previews run in this process.
    pub sandboxes: bool,
    /// Whether a Docker daemon answered a ping during startup.
    pub docker: bool,
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
