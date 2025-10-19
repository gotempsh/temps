use axum::Router;
use std::sync::Arc;

pub mod platform;
pub mod dns;

pub use platform::*;
pub use dns::*;

/// Configure all infrastructure routes (platform + DNS)
pub fn configure_routes<T>() -> Router<Arc<T>>
where
    T: InfraAppState + DnsAppState,
{
    Router::new()
        .merge(configure_platform_routes::<T>())
        .merge(configure_dns_routes::<T>())
}