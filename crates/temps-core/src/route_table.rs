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
}
