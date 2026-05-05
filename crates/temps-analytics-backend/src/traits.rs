//! The [`AnalyticsBackend`] trait.
//!
//! Phase 1 starts deliberately small: only the contract surface is sketched, so
//! the crate compiles and the events crate can depend on it. As query methods
//! migrate out of `events_service.rs` into this trait, their signatures land
//! here and stop being implemented inline in the events service.
//!
//! The trait is `Send + Sync` and used behind `Arc<dyn AnalyticsBackend>` so
//! handlers stay backend-agnostic.

use async_trait::async_trait;

use crate::error::AnalyticsBackendError;

/// Common interface for any analytics storage backend.
///
/// Implementations:
/// - `TimescaleBackend` — Postgres + TimescaleDB hypertables (default)
/// - `ClickHouseBackend` — columnar replica, behind the `clickhouse` feature
#[async_trait]
pub trait AnalyticsBackend: Send + Sync {
    /// Short identifier used in logs and error messages
    /// (e.g. `"timescale"`, `"clickhouse"`).
    fn name(&self) -> &'static str;

    /// Lightweight health check. Backends should return quickly; expensive
    /// connectivity probes belong in a separate diagnostics path.
    async fn health_check(&self) -> Result<(), AnalyticsBackendError>;
}
