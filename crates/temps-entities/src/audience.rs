// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Who an address is for (ADR 064), as stored on `project_custom_domains` and
//! `environment_domains`. See `m20261024_000001_reachability`.

/// Everyone on the internet.
pub const PUBLIC: &str = "public";
/// Only devices on the operator's private network (a tailnet or a LAN),
/// served by a worker's private listener (ADR 064 D5).
pub const PRIVATE: &str = "private";
/// Anyone who signs in to Temps. Reserved for phase 4.
pub const SIGNED_IN: &str = "signed_in";
/// Private, and only for the tailnet logins and tags listed in
/// `address_tailnet_principals` (ADR 065 D6).
pub const TAILNET_USERS: &str = "tailnet_users";

/// Every value the database accepts.
pub const ALL: [&str; 4] = [PUBLIC, PRIVATE, SIGNED_IN, TAILNET_USERS];

/// Serde default for rows serialized before the column existed.
pub fn default_audience() -> String {
    PUBLIC.to_string()
}

/// How a private address is served (ADR 065 D1), the `private_via` column.
pub mod via {
    /// The worker's own private listener, with DNS at it (ADR 064 D5).
    pub const LISTENER: &str = "listener";
    /// The worker's machine name, `<machine>.<tailnet>.ts.net`, through
    /// Tailscale Serve.
    pub const TAILSCALE_SERVE: &str = "tailscale_serve";
    /// A Tailscale Service, `<service>.<tailnet>.ts.net`.
    pub const TAILSCALE_SERVICE: &str = "tailscale_service";

    /// Every value the database accepts.
    pub const ALL: [&str; 3] = [LISTENER, TAILSCALE_SERVE, TAILSCALE_SERVICE];
}
