// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Route table refresh trait
//!
//! Decouples the route table implementation (in temps-routes) from consumers
//! (like the settings handler) that need to trigger a manual refresh.

use async_trait::async_trait;

/// Trait for refreshing the proxy route table from the database.
///
/// Implemented by `CachedPeerTable` in `temps-routes`. Injected via the plugin
/// system so handlers can trigger a manual reload without depending on
/// `temps-routes` directly.
#[async_trait]
pub trait RouteTableRefresher: Send + Sync {
    /// Reload all routes from the database into the in-memory cache.
    /// Returns the number of routes loaded.
    async fn refresh_routes(&self) -> Result<usize, Box<dyn std::error::Error + Send + Sync>>;

    /// Describe what serves `host` as an application right now, or `None`
    /// when nothing does.
    ///
    /// The answer comes from a fresh load of the route table, because that is
    /// the only place that knows every name the proxy serves: exact and
    /// wildcard routes, names generated from an environment's subdomain,
    /// deployments that are stopped or paused, and on-demand environments that
    /// are asleep. A lookup of stored domain rows alone misses most of those.
    ///
    /// Used before a hostname is reserved for the platform (ADR-051): the
    /// route table drops any project route on a reserved host, so reserving
    /// one that is in use would take that application offline.
    async fn application_serving_host(
        &self,
        host: &str,
    ) -> Result<Option<String>, Box<dyn std::error::Error + Send + Sync>>;
}
