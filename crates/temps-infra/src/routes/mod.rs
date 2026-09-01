// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

use axum::Router;
use std::sync::Arc;

pub mod dns;
pub mod platform;

pub use dns::*;
pub use platform::*;

/// Configure all infrastructure routes (platform + DNS)
pub fn configure_routes<T>() -> Router<Arc<T>>
where
    T: InfraAppState + DnsAppState,
{
    Router::new()
        .merge(configure_platform_routes::<T>())
        .merge(configure_dns_routes::<T>())
}
